//! Google's own dialect, in which an assistant turn is an ordered list of parts.
//!
//! note: this exists because the OpenAI-compatible shim in [`openai`](crate::openai) cannot
//! say what the model actually did. Gemini answers with `content.parts[]` - a thinking part, a
//! sentence, a `functionCall`, in the order they were produced - and the shim flattens that into
//! a `content` string beside a `tool_calls` array, because the dialect it is imitating has no
//! order to report. Everything downstream then reads a turn that has been rearranged. This one
//! reports the order, as [`Content::Blocks`], and sends it back the same way.
//!
//! note: it is a second provider rather than a flag on the first because they are two wire
//! formats, not two settings: different paths, different auth header, different names for every
//! field, `functionResponse` parts inside a user turn instead of a `tool` role. What they share is
//! the trait the kernel talks through.
//!
//! note: signatures are the reason to bother beyond tidiness. Gemini signs the parts of a turn and
//! answers `400 Function call is missing a thought_signature` when one comes back without its
//! signature; the shim reports a call's signature and drops the one on a text part, because a
//! message has nowhere to keep it. Here every part's own extra fields ride on the block they
//! belong to - [`Part::extra`] and [`ToolCall::extra`] - and go back out attached to the same
//! part, uninterpreted.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use nachalnik::{
    Block, BoxError, Content, DeltaSink, LinearProjector, Message, ModelInfo, ModelRequest,
    ModelResponse, Part, Provider, Role, StopReason, ToolCall, ToolCallId, Usage, async_trait,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::{
    Dialect, Endpoint, install_crypto,
    reading::{Events, Read, Stopped, not_a_stream, read},
    same_model,
    waiting::{Asking, Sent, interrupted, sent},
};

/// Where Google's own API lives, unless told otherwise.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google's `generateContent`, streamed, with the order of a turn kept.
pub struct Gemini {
    client: reqwest::Client,
    base_url: Mutex<String>,
    api_key: String,
    model: Mutex<String>,
    context_limit: Mutex<Option<usize>>,
    /// The limit the caller set by hand, if it set one, kept so that changing model or endpoint
    /// puts it back rather than dropping it.
    configured: Option<usize>,
    /// Every request for an answer this has sent, retries included, never reset.
    attempts: AtomicUsize,
    notice: Mutex<Option<String>>,
}

/// A run of parts of one kind, being assembled from the stream.
///
/// note: a long answer arrives as a part per chunk, and they are one block rather than forty. The
/// text is accumulated in a `String` and made into a [`Content`] once, because appending to an
/// `Arc<str>` rebuilds it every time and a streamed paragraph would be quadratic.
enum Partial {
    Text(String, Value),
    Reasoning(String, Value),
    Call(ToolCall),
}

impl Gemini {
    /// Builds a provider for one model.
    pub fn new(
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        install_crypto();
        Self {
            client: reqwest::Client::new(),
            base_url: Mutex::new(base_url.into()),
            api_key: api_key.into(),
            model: Mutex::new(model.into()),
            context_limit: Mutex::new(None),
            configured: None,
            attempts: AtomicUsize::new(0),
            notice: Mutex::new(None),
        }
    }

    /// Where the requests are going.
    pub fn endpoint(&self) -> String {
        self.base_url.lock().clone()
    }

    /// Which model is being asked.
    pub fn model(&self) -> String {
        self.model.lock().clone()
    }

    /// How many requests for an answer this has sent, retries counted separately; a model
    /// listing or a probe is not one.
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    /// Measures against this limit rather than against whatever the endpoint advertises.
    ///
    /// note: sticky across [`Endpoint::set_model`] and [`Endpoint::set_endpoint`], because it is
    /// a decision about what to measure and not a fact about one model. `None` means "ask the
    /// endpoint", which is the default.
    pub fn with_context_limit(mut self, limit: Option<usize>) -> Self {
        self.configured = limit;
        *self.context_limit.get_mut() = limit;
        self
    }

    /// Asks the endpoint what the model's context limit is.
    ///
    /// note: the native listing carries it - `inputTokenLimit` - which the OpenAI-compatible one
    /// does not, so this is one round trip rather than the two that one needs.
    pub async fn probe(&self) {
        if self.context_limit.lock().is_some() {
            return;
        }

        let (base, model) = (self.endpoint(), self.model.lock().clone());
        let Ok(response) = self
            .client
            .get(format!("{base}/models/{model}"))
            .header("x-goog-api-key", &self.api_key)
            .timeout(crate::ASKING)
            .send()
            .await
        else {
            return;
        };
        let Ok(body) = response.json::<Value>().await else {
            return;
        };

        if let Some(limit) = body["inputTokenLimit"].as_u64() {
            *self.context_limit.lock() = Some(limit as usize);
        }
    }

    /// Puts a notice up if the model is not one the endpoint lists.
    ///
    /// note: the alternative is finding out on the next request, as a 404 with a paragraph of
    /// somebody's API prose in it. Switching address and model are two commands and it is easy to
    /// do one of them - which is why this is asked after an address changes as well as after a
    /// name does, the same as the other dialect.
    ///
    /// note: an empty listing is "it did not say" rather than "it has none", so it buys silence.
    async fn say_if_the_model_is_not_there(&self) {
        let model = self.model.lock().clone();
        let listed = self.models().await;
        if listed.is_empty() || listed.iter().any(|name| same_model(name, &model)) {
            return;
        }

        *self.notice.lock() = Some(format!(
            "{model} is not one of the {} models this address lists",
            listed.len()
        ));
    }

    /// Everything in a part except the fields this provider understands.
    ///
    /// note: not just `thoughtSignature`, though that is the one that matters. Whatever Google
    /// puts beside the text next is carried back unread, which is the same bargain
    /// [`ToolCall::extra`] makes and the only one a provider can make honestly about a field it
    /// has never heard of.
    fn attached(part: &Value) -> Value {
        let Some(fields) = part.as_object() else {
            return Value::Null;
        };
        let kept: Map<String, Value> = fields
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "text" | "thought" | "functionCall"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        match kept.is_empty() {
            true => Value::Null,
            false => Value::Object(kept),
        }
    }

    /// Puts back whatever was attached to a part, beside the part.
    fn reattach(mut part: Value, extra: &Value) -> Value {
        if let (Some(part), Some(extra)) = (part.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                part.insert(key.clone(), value.clone());
            }
        }

        part
    }

    /// Takes one streamed part into the run being assembled.
    fn absorb(part: &Value, partial: &mut Vec<Partial>, deltas: &DeltaSink) {
        let extra = Self::attached(part);

        if let Some(asked) = part.get("functionCall").filter(|call| !call.is_null()) {
            let id = asked["id"].as_str().unwrap_or_default();
            let name = asked["name"].as_str().unwrap_or_default();
            let args = asked
                .get("args")
                .cloned()
                .filter(|args| !args.is_null())
                .unwrap_or_else(|| json!({}));

            deltas.tool_args(ToolCallId(id.to_owned()), args.to_string());
            partial.push(Partial::Call(
                ToolCall::new(id, name, args).with_extra(extra),
            ));

            return;
        }

        let text = part["text"].as_str().unwrap_or_default();
        if text.is_empty() {
            // a part with no text and no call is either a signature arriving on its own - which
            // belongs to whatever it came after rather than to nothing - or a kind of part this
            // provider does not speak: `inlineData`, `executableCode`, `fileData`. The second is
            // dropped rather than reattached, because attaching an image to the text before it
            // would send it back out as a field of that text and quietly corrupt the turn. What
            // is dropped is still in `ModelResponse::raw`, which is as much as a provider can
            // honestly offer about a shape it has no variant for
            let metadata = extra
                .as_object()
                .is_some_and(|fields| fields.keys().all(|key| key == "thoughtSignature"));
            if metadata {
                match partial.last_mut() {
                    Some(Partial::Text(_, at) | Partial::Reasoning(_, at)) => *at = extra,
                    Some(Partial::Call(call)) => call.extra = Arc::new(extra),
                    None => {}
                }
            }

            return;
        }

        let thinking = part["thought"].as_bool().unwrap_or(false);
        match thinking {
            true => deltas.reasoning(text),
            false => deltas.text(text),
        }

        // a run of parts of one kind is one block: the model said one thing, in forty chunks
        match partial.last_mut() {
            Some(Partial::Reasoning(said, at)) if thinking => {
                said.push_str(text);
                if !extra.is_null() {
                    *at = extra;
                }
            }
            Some(Partial::Text(said, at)) if !thinking => {
                said.push_str(text);
                if !extra.is_null() {
                    *at = extra;
                }
            }
            _ => partial.push(match thinking {
                true => Partial::Reasoning(text.to_owned(), extra),
                false => Partial::Text(text.to_owned(), extra),
            }),
        }
    }

    /// The parts an assistant turn goes back out as.
    ///
    /// note: a turn recorded as blocks goes back in the order it arrived in, each part carrying
    /// what was attached to it. One recorded the conventional way - by a session that started
    /// against another provider, or by a projector told not to send blocks - is assembled into
    /// the order this API expects: the thinking, then what was said, then the calls.
    fn turn(message: &Message) -> Vec<Value> {
        let Some(blocks) = message.blocks() else {
            let mut parts = Vec::new();
            if let Some(thinking) = &message.reasoning {
                parts.push(json!({ "text": thinking.to_text(), "thought": true }));
            }
            if let Some(said) = message
                .content
                .as_ref()
                .filter(|said| said.as_blob().is_some() || !said.to_text().is_empty())
            {
                parts.push(Self::part_of(said));
            }
            parts.extend(message.tool_calls.iter().map(Self::asking));

            return parts;
        };

        blocks
            .iter()
            .map(|block| match block {
                Block::Text(part) => Self::reattach(Self::part_of(&part.content), &part.extra),
                Block::Reasoning(part) => Self::reattach(
                    json!({ "text": part.content.to_text(), "thought": true }),
                    &part.extra,
                ),
                Block::Call(call) => Self::asking(call),
                // `Block` is `#[non_exhaustive]`: a variant this provider has never heard of is
                // dropped rather than guessed at, and the turn is still valid without it
                _ => Value::Null,
            })
            .filter(|part| !part.is_null())
            .collect()
    }

    /// One piece of content, as the part this API carries it in.
    ///
    /// note: text in a `text` part and bytes in an `inline_data` one. The payload is already
    /// base64, which is the form this field takes, so nothing is encoded on the way out.
    fn part_of(content: &Content) -> Value {
        match content.as_blob() {
            Some(blob) => json!({
                "inline_data": { "mime_type": blob.media_type, "data": blob.data },
            }),
            None => json!({ "text": content.to_text() }),
        }
    }

    /// One tool call, as a part.
    fn asking(call: &ToolCall) -> Value {
        let mut asked = json!({ "name": call.tool, "args": *call.args });
        // the identifier is what pairs the call with its answer, and this API takes one of its
        // own making; where the kernel had to repair one, the repaired identifier is what both
        // halves of the pair carry
        if !call.id.0.is_empty() {
            asked["id"] = json!(call.id.0);
        }

        Self::reattach(json!({ "functionCall": asked }), &call.extra)
    }

    /// One tool result, as a part of the user turn that answers the model's.
    fn answering(message: &Message) -> Value {
        let said = message.content.clone().unwrap_or_default();
        // a tool that produced JSON hands it over as it is; anything else is a string, and this
        // API wants an object either way
        let answer = match &said {
            Content::Json(value) if value.is_object() => (**value).clone(),
            _ => json!({ "result": said.to_text() }),
        };

        let mut answered = json!({
            "name": message.name.clone().unwrap_or_default(),
            "response": answer,
        });
        if let Some(id) = &message.tool_call_id {
            answered["id"] = json!(id.0);
        }

        json!({ "functionResponse": answered })
    }
}

#[async_trait]
impl Provider for Gemini {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: *self.context_limit.lock(),
            tool_calling: true,
            reasoning: true,
            ..ModelInfo::new("google", self.model.lock().clone())
        }
    }

    /// The payload, rendered once. `respond` sends exactly this.
    fn render(&self, request: &ModelRequest) -> Option<Value> {
        let mut instructions: Vec<String> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();

        for message in &request.messages {
            if message.role == Role::System {
                // this API keeps its instructions out of the conversation rather than at the top
                // of it, which is the one place the shape of a request really differs
                instructions.extend(
                    message
                        .content
                        .as_ref()
                        .map(|said| said.to_text().into_owned()),
                );
                continue;
            }

            let (role, parts) = match message.role {
                Role::Assistant => ("model", Self::turn(message)),
                Role::Tool => ("user", vec![Self::answering(message)]),
                // note: a turn recorded as blocks is a turn that is more than one thing - a
                // sentence and the screenshot it is about - so it goes out as one part each
                // rather than through `to_text`, which would name the picture instead of
                // carrying it
                _ => (
                    "user",
                    match message.content.as_ref() {
                        Some(Content::Blocks(blocks)) => blocks
                            .iter()
                            .filter_map(Block::said)
                            .map(|said| Self::part_of(&said.content))
                            .collect(),
                        Some(said) => vec![Self::part_of(said)],
                        None => vec![json!({ "text": "" })],
                    },
                ),
            };
            if parts.is_empty() {
                continue;
            }

            // turns alternate here, so three tool results are three parts of one turn rather than
            // three turns. Sending them separately is the shape this API is least forgiving about
            match contents.last_mut() {
                Some(last) if last["role"] == role => {
                    if let Some(existing) = last["parts"].as_array_mut() {
                        separate(existing, &parts);
                        existing.extend(parts);
                    }
                }
                _ => contents.push(json!({ "role": role, "parts": parts })),
            }
        }

        let mut body = json!({ "contents": contents });
        if !instructions.is_empty() {
            body["systemInstruction"] = json!({ "parts": [{ "text": instructions.join("\n\n") }] });
        }
        if !request.tools.is_empty() {
            body["tools"] = json!([{
                "functionDeclarations": request
                    .tools
                    .iter()
                    .map(|spec| json!({
                        "name": spec.id,
                        "description": spec.description,
                        "parameters": spec.schema,
                    }))
                    .collect::<Vec<_>>()
            }]);
        }

        // note: asked for, because a provider whose reason to exist is the order of a turn's
        // thinking would be a poor one if it never asked to be told any. It is a default and not
        // a decision: `generationConfig` in the parameters is merged over this, so
        // `{"thinkingConfig": {"includeThoughts": false}}` turns it off and says so on the
        // `/params` line where somebody can read it back
        //
        // note: merged all the way down, so that `{"thinkingConfig": {"thinkingBudget": 1024}}`
        // sets a budget without also taking away the thoughts it is a budget for
        body["generationConfig"] = json!({ "thinkingConfig": { "includeThoughts": true } });
        for (key, value) in &request.params {
            match key.as_str() {
                "generationConfig" => merge(&mut body["generationConfig"], value),
                _ => body[key] = value.clone(),
            }
        }

        Some(body)
    }

    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        let body = self.render(&request).expect("this provider always renders");
        let (base, model, limit) = (
            self.endpoint(),
            self.model.lock().clone(),
            *self.context_limit.lock(),
        );
        let asking = Asking {
            model: &model,
            deltas: &deltas,
            notice: &self.notice,
        };

        let url = format!("{base}/models/{model}:streamGenerateContent?alt=sse");
        let sending = || {
            self.client
                .post(&url)
                .header("x-goog-api-key", &self.api_key)
                .json(&body)
        };
        let mut streamed = Streamed::default();
        let mut response = match sent(&asking, &self.attempts, limit, true, sending).await? {
            Sent::Interrupted => return Ok(interrupted()),
            Sent::Streaming(response) => response,
            // never asked for here, and read as what it would be if it came
            Sent::Whole(payload) => return unstreamed(payload.to_string(), streamed, &deltas),
        };

        match read(&mut response, &asking, limit, &mut streamed).await? {
            Read::Events(events, stopped) => Ok(answer(streamed, events, stopped)),
            Read::Interrupted => Ok(interrupted()),
            Read::Unstreamed(body) => unstreamed(body, streamed, &deltas),
        }
    }
}

/// A stream, read: the turn as the parts left it.
///
/// note: `partial` is the turn in the order it was produced, which is why this dialect gathers
/// parts where the other one gathers three slots.
#[derive(Default)]
struct Streamed {
    /// The turn so far, in the order the model produced it.
    partial: Vec<Partial>,
    /// Why the turn ended, once something has said.
    finish: Option<String>,
    /// What the request cost, where the server reported it.
    usage: Option<Usage>,
}

impl Events for Streamed {
    fn event(&mut self, chunk: &Value, deltas: &DeltaSink) {
        if let Some(reported) = chunk.get("usageMetadata").filter(|u| !u.is_null()) {
            // note: the thoughts are added in, because `Usage::output_tokens` is everything
            // generated and this dialect reports the two apart. Google's own `totalTokenCount` is
            // defined as the prompt plus the thoughts plus the candidates, so the sum is that
            // definition rather than this crate's invention - and without it a thinking turn's
            // cost is the answer alone. `thoughtsTokenCount` still says how much of it was
            // thinking, one field along
            let thoughts = reported["thoughtsTokenCount"].as_u64();
            let candidates = reported["candidatesTokenCount"].as_u64();
            self.usage = Some(Usage {
                input_tokens: reported["promptTokenCount"].as_u64(),
                // note: `None` only where the dialect said neither, so that "it did not say" stays
                // distinguishable from "it generated nothing"
                output_tokens: match (candidates, thoughts) {
                    (None, None) => None,
                    (candidates, thoughts) => {
                        Some(candidates.unwrap_or_default() + thoughts.unwrap_or_default())
                    }
                },
                reasoning_tokens: thoughts,
                cached_input_tokens: reported["cachedContentTokenCount"].as_u64(),
            });
        }

        // a prompt the API will not answer at all comes back with no candidate and the reason
        // beside it - which is a refusal whatever it names
        if let Some(reason) = chunk["promptFeedback"]["blockReason"].as_str() {
            self.finish = Some(format!("blocked: {reason}"));
        }
        let candidate = &chunk["candidates"][0];
        if let Some(reason) = candidate["finishReason"].as_str() {
            self.finish = Some(reason.to_owned());
        }
        for part in candidate["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            Gemini::absorb(part, &mut self.partial, deltas);
        }
    }

    fn finished(&self) -> bool {
        self.finish.is_some()
    }
}

/// A body that was not a stream, read as the events it would have been streamed as - or, where
/// it is not those, the error that says so.
///
/// note: this API's unstreamed answer is exactly those events, in a list, which is what
/// `streamGenerateContent` sends where `alt=sse` did not reach it - a proxy that dropped the query
/// string is enough. A single event is a list of one.
fn unstreamed(
    body: String,
    mut streamed: Streamed,
    deltas: &DeltaSink,
) -> Result<ModelResponse, BoxError> {
    let events = match serde_json::from_str::<Value>(&body) {
        Ok(Value::Array(events)) => events,
        Ok(event @ Value::Object(_)) => vec![event],
        _ => Vec::new(),
    };
    let answers = !events.is_empty()
        && events.iter().all(|event| {
            ["candidates", "promptFeedback", "usageMetadata"]
                .iter()
                .any(|field| event.get(field).is_some())
        });
    if !answers {
        return Err(not_a_stream(&body));
    }

    for event in &events {
        streamed.event(event, deltas);
    }
    Ok(answer(streamed, events, Stopped::Ended))
}

/// The turn the stream came to: the parts in the order they were produced, and what ended it.
fn answer(streamed: Streamed, events: Vec<Value>, stopped: Stopped) -> ModelResponse {
    let Streamed {
        partial,
        mut finish,
        usage,
    } = streamed;
    match stopped {
        Stopped::Ended => {}
        Stopped::Interrupted => finish = Some("interrupted".to_owned()),
        Stopped::CutOff => finish = Some("cut off".to_owned()),
    }

    let blocks: Vec<Block> = partial
        .into_iter()
        .map(|part| match part {
            Partial::Text(said, extra) => Block::Text(Part::new(said).with_extra(extra)),
            Partial::Reasoning(said, extra) => Block::Reasoning(Part::new(said).with_extra(extra)),
            Partial::Call(call) => Block::Call(call),
        })
        .collect();
    let asked = blocks.iter().any(|block| block.call().is_some());

    ModelResponse {
        // the whole turn in one slot, in the order it was produced. `reasoning` and
        // `tool_calls` stay empty: they are the other way of recording the same turn, and a
        // response carrying both would be two accounts of it
        content: (!blocks.is_empty()).then(|| Content::blocks(blocks)),
        reasoning: None,
        tool_calls: Vec::new(),
        // note: derived from the parts, not from `finishReason`, which says `STOP` for a turn
        // that asked for three tools. What ends a turn here is running out of things to say,
        // and a call is not that
        stop: match finish.as_deref() {
            Some("interrupted") => StopReason::Other("interrupted".to_owned()),
            // note: ahead of `asked`, for the reason `interrupted` is ahead of it. That the
            // turn asked for a tool is visible in the blocks it is carrying; that the stream
            // stopped partway through is visible nowhere else. The kernel decides what to run
            // from the calls rather than from this, so saying so costs the turn nothing
            Some("cut off") => StopReason::Other("cut off".to_owned()),
            Some(blocked) if blocked.starts_with("blocked: ") => StopReason::Refusal,
            // and so is running out of room, which is said nowhere else either, and which the
            // OpenAI dialect reports as `Length` whether or not the turn asked for anything
            Some("MAX_TOKENS") => StopReason::Length,
            _ if asked => StopReason::ToolUse,
            Some("STOP") => StopReason::EndTurn,
            Some("SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII") => StopReason::Refusal,
            Some(other) => StopReason::Other(other.to_lowercase()),
            None => StopReason::Other("unreported".to_owned()),
        },
        usage,
        raw: Some(json!({ "stream": events })),
    }
}

/// Merges one JSON value over another: objects key by key, all the way down, and anything else by
/// replacing it.
fn merge(into: &mut Value, over: &Value) {
    match (into.as_object_mut(), over.as_object()) {
        (Some(into), Some(over)) => {
            for (key, value) in over {
                merge(into.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        _ => *into = over.clone(),
    }
}

#[async_trait]
impl Endpoint for Gemini {
    fn endpoint(&self) -> String {
        self.endpoint()
    }

    fn model(&self) -> String {
        self.model()
    }

    async fn models(&self) -> Vec<String> {
        let base = self.endpoint();
        let Ok(response) = self
            .client
            .get(format!("{base}/models?pageSize=200"))
            .header("x-goog-api-key", &self.api_key)
            .timeout(crate::ASKING)
            .send()
            .await
        else {
            return Vec::new();
        };
        let Ok(body) = response.json::<Value>().await else {
            return Vec::new();
        };

        body["models"]
            .as_array()
            .map(|listed| {
                listed
                    .iter()
                    .filter_map(|model| model["name"].as_str())
                    .map(|name| name.strip_prefix("models/").unwrap_or(name).to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn set_model(&self, model: String) {
        *self.model.lock() = model;
        *self.context_limit.lock() = self.configured;
        self.probe().await;
        self.say_if_the_model_is_not_there().await;
    }

    /// note: the check happens whether or not a model was named. Given none the old name is kept,
    /// and a name that was right at the last address is exactly the one worth asking about at this
    /// one. Skipping it leaves the 404 on the next request to say so.
    async fn set_endpoint(&self, url: String, model: Option<String>) {
        *self.base_url.lock() = url;
        *self.context_limit.lock() = self.configured;
        match model {
            Some(model) => self.set_model(model).await,
            None => {
                self.probe().await;
                self.say_if_the_model_is_not_there().await;
            }
        }
    }

    fn take_notice(&self) -> Option<String> {
        self.notice.lock().take()
    }
}

impl Dialect for Gemini {
    /// Both of the things the conventional dialect cannot take. In this one the shape of a turn
    /// is an order, and flattening it into three slots on the way out would undo, one request
    /// later, the ordering that was recorded on the way in; and it takes a turn's thinking back as
    /// a part marked `thought`, which for a signed one it has to.
    fn projection(&self) -> LinearProjector {
        LinearProjector {
            send_blocks: true,
            ..Default::default()
        }
    }
}

/// Puts a boundary between two text parts that are about to become one turn.
///
/// note: this API concatenates the text parts of a turn with *nothing* between them, so two items
/// merged above are read as one run of words: a note ending `…the codename is kotelnaya` followed
/// by a question beginning `what is the codename?` reaches the model as `kotelnayawhat is the
/// codename?`, and the model reads the codename as `kotelnayawhat`. Every reference `kamchatka`
/// sends is one of these - a file put in with `-f`, an `/attach`, a `/note` - and the message
/// after it is the question about it, which is the commonest shape there is.
///
/// note: the other dialect gets this for free by keeping one message per item. Here the parts of
/// a turn are the merge, so the separator has to live inside the text, and this is the last place
/// that knows there were two of them.
///
/// note: text against text only. A `functionResponse` is a field of its own and reads as one
/// whatever precedes it, and a blob is `inline_data`; neither runs into a neighbour. The boundary
/// is normalised rather than appended to - trailing newlines trimmed, then exactly one blank
/// line - so that a file ending in three of them and one ending in none are separated the same
/// way, and rendering the same request twice produces the same bytes.
fn separate(existing: &mut [Value], coming: &[Value]) {
    let (Some(last), Some(next)) = (existing.last_mut(), coming.first()) else {
        return;
    };
    let both_text = last.get("text").is_some()
        && next.get("text").is_some()
        && last.get("thought").is_none()
        && next.get("thought").is_none();
    if !both_text {
        return;
    }

    if let Some(text) = last["text"].as_str() {
        last["text"] = json!(format!("{}\n\n", text.trim_end_matches('\n')));
    }
}
