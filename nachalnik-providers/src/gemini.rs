//! Google's dialect: an assistant turn is an ordered list of parts.
//!
//! note: the OpenAI-compatible shim in [`openai`](crate::openai) cannot say what the model did:
//! Gemini answers with `content.parts[]` (a thinking part, a sentence, a `functionCall`, in the
//! order produced), and the shim flattens that into a `content` string beside a `tool_calls` array,
//! so everything downstream reads a rearranged turn. This dialect reports the order as
//! [`Content::Blocks`] and sends it back the same way.
//!
//! note: a second provider rather than a flag, because these are two wire formats: different paths,
//! auth header and field names, and `functionResponse` parts inside a user turn instead of a `tool`
//! role. They share the trait the kernel talks through.
//!
//! note: signatures make it necessary. Gemini signs the parts of a turn and answers
//! `400 Function call is missing a thought_signature` when one comes back without its signature,
//! and the shim keeps a call's signature but drops a text part's, having nowhere to put it. Here
//! each part's extra fields ride on its block ([`Part::extra`], [`ToolCall::extra`]) and go back
//! out on the same part, uninterpreted.

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
    Dialect, Endpoint, Keyed, Published, RETRIES, install_crypto,
    reading::{Events, Read, Stopped, not_a_stream},
    refused,
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
    /// How many times a request is sent while the server is busy, the first included; see
    /// [`Dialect::set_tries`].
    tries: AtomicUsize,
    /// How long a stream may say nothing before the request is given up on; see
    /// [`Dialect::set_patience`].
    patience: Mutex<std::time::Duration>,
    notice: Mutex<Option<String>>,
    /// What the model's listing published about the parameters it takes.
    listed: Mutex<Listed>,
}

/// What a model's entry in the listing says about its parameters.
///
/// note: the figures are about fields of `generationConfig` - `temperature`, `topP`, `topK` and
/// `outputTokenLimit`, which bounds `maxOutputTokens` - so they are named by their path into it, as
/// a caller setting one writes. One lock for all of it, learnt from one entry and forgotten on one
/// switch, for the reason the other dialect's `Entry` gives.
#[derive(Default)]
struct Listed {
    /// The paths of the parameters the entry published a figure for, in a fixed order.
    parameters: Vec<String>,
    /// The defaults it published, by path.
    defaults: Map<String, Value>,
    /// The maxima it published, by path.
    maxima: Map<String, Value>,
}

impl Listed {
    /// Reads a model's entry, as `GET /models/{model}` answers it.
    fn read(entry: &Value) -> Self {
        let mut listed = Self::default();
        // what the listing calls it, the `generationConfig` field it is about, and whether the
        // figure is the field's default or its most
        let figures = [
            ("outputTokenLimit", "maxOutputTokens", false),
            ("temperature", "temperature", true),
            ("maxTemperature", "temperature", false),
            ("topP", "topP", true),
            ("topK", "topK", true),
        ];
        for (published, field, default) in figures {
            let Some(figure) = entry.get(published).filter(|it| it.is_number()) else {
                continue;
            };
            let path = format!("generationConfig.{field}");
            match default {
                true => listed.defaults.insert(path.clone(), figure.clone()),
                false => listed.maxima.insert(path.clone(), figure.clone()),
            };
            if !listed.parameters.contains(&path) {
                listed.parameters.push(path);
            }
        }

        listed
    }
}

/// A run of parts of one kind, assembled from the stream.
///
/// note: a long answer arrives as a part per chunk and becomes one block rather than forty. The
/// text is accumulated in a `String` and made into a [`Content`] once because appending to an
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
            base_url: Mutex::new(crate::address(base_url)),
            api_key: api_key.into(),
            model: Mutex::new(model.into()),
            context_limit: Mutex::new(None),
            configured: None,
            attempts: AtomicUsize::new(0),
            tries: AtomicUsize::new(RETRIES),
            patience: Mutex::new(crate::waiting::PATIENCE),
            notice: Mutex::new(None),
            listed: Mutex::new(Listed::default()),
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

    /// Waits this long for a silent stream before giving up on the request, rather than the default
    /// two and a half minutes; see [`Dialect::set_patience`].
    #[must_use]
    pub fn with_patience(self, patience: std::time::Duration) -> Self {
        *self.patience.lock() = patience;
        self
    }

    /// Measures against this limit rather than against whatever the endpoint advertises.
    ///
    /// note: sticky across [`Endpoint::set_model`] and [`Endpoint::set_endpoint`] because it is a
    /// decision about what to measure, not a fact about one model. `None` means "ask the endpoint",
    /// the default.
    pub fn with_context_limit(mut self, limit: Option<usize>) -> Self {
        self.configured = limit;
        *self.context_limit.get_mut() = limit;
        self
    }

    /// Asks the endpoint what the model's context limit is and what it publishes about the
    /// parameters it takes.
    ///
    /// note: the native listing carries the limit, `inputTokenLimit`, which the OpenAI-compatible
    /// one does not, so this is one round trip rather than two. It is asked even where the limit
    /// was set by hand because the parameters are not.
    pub async fn probe(&self) {
        let (base, model) = (self.endpoint(), self.model.lock().clone());
        let Ok(response) = self
            .client
            .get(format!("{base}/models/{model}"))
            .google_key(&self.api_key)
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
            self.context_limit.lock().get_or_insert(limit as usize);
        }
        *self.listed.lock() = Listed::read(&body);
    }

    /// Sets a notice if the model is not listed by the endpoint.
    ///
    /// note: otherwise the first bad request returns a 404 with the API's prose in it. Address and
    /// model are separate commands, so this runs after either changes, as in the other dialect.
    ///
    /// note: an empty listing means "it did not say", not "it has none", so it sets no notice, and
    /// clears one still waiting, for the reason the other dialect's gives.
    async fn say_if_the_model_is_not_there(&self) {
        let model = self.model.lock().clone();
        let listed = self.models().await;
        *self.notice.lock() = crate::unlisted(&model, &listed);
    }

    /// All of a part except the fields this provider understands.
    ///
    /// note: not just `thoughtSignature`, though that is the one that matters. Whatever Google puts
    /// beside the text is carried back unread, the same bargain [`ToolCall::extra`] makes and the
    /// only honest one for a field a provider has never heard of.
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
            // a part with no text and no call is either a signature arriving alone, which belongs
            // to whatever came before it, or a kind of part this provider does not speak
            // (`inlineData`, `executableCode`, `fileData`). The second is dropped rather than
            // reattached, since attaching an image to the text before it would send it back as a
            // field of that text and corrupt the turn; it is still in `ModelResponse::raw`
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
    /// what was attached to it. A turn recorded the conventional way is assembled into the order
    /// this API expects: thinking, then what was said, then the calls.
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
                // `Block` is `#[non_exhaustive]`: an unknown variant is dropped rather than guessed
                // at, and the turn remains valid without it
                _ => Value::Null,
            })
            .filter(|part| !part.is_null())
            .collect()
    }

    /// One piece of content, as the part this API carries it in.
    ///
    /// note: text goes in a `text` part and bytes in an `inline_data` part. The payload is already
    /// base64, the form this field takes, so nothing is encoded on the way out.
    fn part_of(content: &Content) -> Value {
        match content.as_blob() {
            Some(blob) => json!({
                "inline_data": { "mime_type": blob.media_type, "data": blob.data },
            }),
            None => json!({ "text": content.to_text() }),
        }
    }

    /// A blob, as the `inlineData` a `functionResponse` carries it in.
    fn inline(blob: &nachalnik::Blob) -> Value {
        json!({ "inlineData": { "mimeType": blob.media_type, "data": blob.data } })
    }

    /// One tool call, as a part.
    fn asking(call: &ToolCall) -> Value {
        let mut asked = json!({ "name": call.tool, "args": *call.args });
        // the identifier pairs the call with its answer, and this API supplies its own. Where the
        // kernel repaired one, the repaired identifier is what both halves of the pair carry
        if !call.id.0.is_empty() {
            asked["id"] = json!(call.id.0);
        }

        Self::reattach(json!({ "functionCall": asked }), &call.extra)
    }

    /// One tool result, as a part of the user turn that answers the model's.
    ///
    /// note: a picture the tool returned goes in the `functionResponse`'s own `parts` as
    /// `inlineData`, and the text around it in `response` - the one place this API takes a picture
    /// as part of a result rather than as something the person said after it. Named instead, as the
    /// other dialect has to, the model is left to guess.
    fn answering(message: &Message) -> Value {
        let said = message.content.clone().unwrap_or_default();
        let pictures: Vec<Value> = match &said {
            Content::Blob(blob) => vec![Self::inline(blob)],
            Content::Blocks(blocks) => blocks
                .iter()
                .filter_map(Block::said)
                .filter_map(|part| part.content.as_blob())
                .map(Self::inline)
                .collect(),
            _ => Vec::new(),
        };
        // a tool that produced JSON hands it over as it is; anything else is a string, and this
        // API wants an object either way. With its pictures in `parts`, the string is the text
        // that came with them, without a line naming each
        let answer = match &said {
            Content::Json(value) if value.is_object() => (**value).clone(),
            Content::Blob(_) => json!({ "result": "" }),
            Content::Blocks(blocks) if !pictures.is_empty() => json!({
                "result": blocks
                    .iter()
                    .filter_map(Block::said)
                    .filter(|part| part.content.as_blob().is_none())
                    .map(|part| part.content.to_text())
                    .collect::<Vec<_>>()
                    .join("\n"),
            }),
            _ => json!({ "result": said.to_text() }),
        };

        let mut answered = json!({
            "name": message.name.clone().unwrap_or_default(),
            "response": answer,
        });
        if let Some(id) = &message.tool_call_id {
            answered["id"] = json!(id.0);
        }
        if !pictures.is_empty() {
            answered["parts"] = json!(pictures);
        }

        json!({ "functionResponse": answered })
    }
}

#[async_trait]
impl Provider for Gemini {
    fn info(&self) -> ModelInfo {
        // note: one lock to a statement: a struct literal holds every guard it takes until it is
        // done, and `respond` takes the same two in the other order on another thread
        let context_limit = *self.context_limit.lock();
        let model = self.model.lock().clone();
        let endpoint = crate::recorded(&self.base_url.lock());
        let (parameters, most) = {
            let listed = self.listed.lock();
            let most = listed
                .maxima
                .get("generationConfig.maxOutputTokens")
                .and_then(Value::as_u64);
            (listed.parameters.clone(), most)
        };

        ModelInfo::new("google", model)
            .with_context_limit(context_limit)
            .with_max_output_tokens(most.map(|most| most as usize))
            .with_parameters(parameters)
            .with_tool_calling(true)
            .with_reasoning(true)
            .with_endpoint(endpoint)
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
            // three turns. The API is least forgiving of sending them separately
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
                        // note: `parametersJsonSchema` rather than `parameters`, which is
                        // Google's own `Schema` - a closed set of fields with no `$ref`,
                        // `$defs`, `additionalProperties`, `const` or list of types, so an MCP
                        // server's schema using any of them made every request a 400. This one
                        // takes JSON Schema as it is written
                        "parametersJsonSchema": spec.schema,
                    }))
                    .collect::<Vec<_>>()
            }]);
        }

        // note: asked for by default, since this dialect exists for the order of a turn's thinking.
        // `generationConfig` in the parameters is merged over it, so
        // `{"thinkingConfig": {"includeThoughts": false}}` turns it off, visibly on the `/params`
        // line
        //
        // note: merged all the way down, so `{"thinkingConfig": {"thinkingBudget": 1024}}` sets a
        // budget without taking away the thoughts it budgets
        body["generationConfig"] = json!({ "thinkingConfig": { "includeThoughts": true } });
        //
        // note: the three fields built from the request are not replaced: a parameter is carried
        // beside the conversation, not in place of it. The model is not put in the body: it is in
        // the address, and `generateContent` does not define it as a body field
        for (key, value) in &request.params {
            match key.as_str() {
                "generationConfig" => merge(&mut body["generationConfig"], value),
                "contents" | "systemInstruction" | "tools" | "model" => {}
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
        // one lock to a statement; see `info`
        let base = self.endpoint();
        let model = self.model.lock().clone();
        let limit = *self.context_limit.lock();
        let asking = Asking {
            model: &model,
            deltas: &deltas,
            notice: &self.notice,
            tries: self.tries.load(Ordering::SeqCst),
            patience: *self.patience.lock(),
        };

        let url = format!("{base}/models/{model}:streamGenerateContent?alt=sse");
        let sending = || self.client.post(&url).google_key(&self.api_key).json(&body);
        let mut streamed = Streamed::default();
        match sent(&asking, &self.attempts, limit, true, sending, &mut streamed).await? {
            Sent::Interrupted => Ok(interrupted()),
            // never asked for here, and read as what it would be if it came
            Sent::Whole(payload) => unstreamed(payload.to_string(), streamed, &deltas),
            // note: a stream that carried neither a candidate nor a refusal of the prompt has not
            // answered, as Chat Completions holds a stream with no choice in it. Read as a turn
            // it finished with nothing said and `EndTurn`, which hides an endpoint's fault as a
            // model with nothing to say. A body that came whole is read as before
            Sent::Streamed(Read::Events(events, stopped)) => {
                match !streamed.answered && !events.is_empty() {
                    true => Err(no_answer(&events[0])),
                    false => Ok(answer(streamed, events, stopped)),
                }
            }
            Sent::Streamed(Read::Interrupted) => Ok(interrupted()),
            Sent::Streamed(Read::Unstreamed(body)) => unstreamed(body, streamed, &deltas),
            Sent::Streamed(Read::Refused { said, .. }) => Err(refused(said, limit)),
        }
    }
}

/// A stream, read: the turn as the parts left it.
///
/// note: `partial` is the turn in the order it was produced, so this dialect gathers parts where
/// the other one gathers three slots.
#[derive(Default)]
struct Streamed {
    /// The turn so far, in the order the model produced it.
    partial: Vec<Partial>,
    /// Why the turn ended, once something has said.
    finish: Option<String>,
    /// What the request cost, where the server reported it.
    usage: Option<Usage>,
    /// Whether anything carried a candidate, or a refusal of the prompt, which is an answer too.
    answered: bool,
}

impl Events for Streamed {
    fn event(&mut self, chunk: &Value, deltas: &DeltaSink) {
        self.answered |= chunk["candidates"]
            .as_array()
            .is_some_and(|candidates| !candidates.is_empty())
            || chunk["promptFeedback"]["blockReason"].is_string();
        if let Some(reported) = chunk.get("usageMetadata").filter(|u| !u.is_null()) {
            // note: thoughts are added to `Usage::output_tokens` because that field is everything
            // generated and this dialect reports thoughts and candidates apart. Google defines
            // `totalTokenCount` as the prompt plus the thoughts plus the candidates, so the sum
            // matches that definition. Without it, a thinking turn's cost is the answer alone.
            // `thoughtsTokenCount` still reports how much of the output was thinking
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

        // a prompt the API will not answer comes back with no candidate and the reason beside it;
        // that is a refusal whatever the reason says
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

/// What a stream that never answered is refused with, quoting the first thing it carried.
fn no_answer(event: &Value) -> BoxError {
    let short = crate::markup::quoted(&event.to_string());
    format!("the stream carried no answer: {short}").into()
}

/// Reads a non-streamed body as the events it would have been streamed as, or returns the error.
///
/// note: the unstreamed answer is exactly those events in a list, which is what
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
    Ok(answer(streamed, events, Stopped::Done))
}

/// The turn the stream came to: the parts in the order they were produced, and what ended it.
fn answer(streamed: Streamed, events: Vec<Value>, stopped: Stopped) -> ModelResponse {
    let Streamed {
        partial,
        mut finish,
        usage,
        ..
    } = streamed;
    match stopped {
        // note: this dialect's own end of the stream is an end of the turn. A stream the server
        // ended on purpose with no `finishReason` is not a turn whose reason nobody gave; the
        // end-of-stream marker is the only thing that said so
        Stopped::Done if finish.is_none() => finish = Some("STOP".to_owned()),
        Stopped::Done => {}
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
        // note: `stop` is derived from the parts, not from `finishReason`, which says `STOP` for a
        // turn that asked for three tools. A turn ends by running out of things to say, and a call
        // is not that
        stop: match finish.as_deref() {
            Some("interrupted") => StopReason::Other("interrupted".to_owned()),
            // note: ahead of `asked`, for the reason `interrupted` is ahead of it. That the
            // turn asked for a tool is visible in the blocks it is carrying; that the stream
            // stopped partway through is visible nowhere else. The kernel decides what to run
            // from the calls rather than from this, so saying so costs the turn nothing
            Some("cut off") => StopReason::Other("cut off".to_owned()),
            Some(blocked) if blocked.starts_with("blocked: ") => StopReason::Refusal,
            // ahead of `asked` too: running out of room is reported nowhere else either, and the
            // OpenAI dialect reports it as `Length` whether or not the turn asked for anything
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
            .google_key(&self.api_key)
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
        *self.listed.lock() = Listed::default();
        self.probe().await;
        self.say_if_the_model_is_not_there().await;
    }

    /// note: the check happens whether or not a model was named. With none, the old name is kept. A
    /// name that was right at the last address is the one worth asking about at this one. Skipping
    /// it means the next request gets a 404.
    async fn set_endpoint(&self, url: String, model: Option<String>) {
        *self.base_url.lock() = crate::address(url);
        *self.context_limit.lock() = self.configured;
        *self.listed.lock() = Listed::default();
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
    fn set_patience(&self, patience: std::time::Duration) {
        *self.patience.lock() = patience;
    }

    fn set_tries(&self, tries: usize) {
        self.tries.store(tries.max(1), Ordering::SeqCst);
    }

    /// The listing publishes figures for a handful of `generationConfig`'s fields, and the model
    /// takes many more - `seed`, `stopSequences`, `thinkingConfig`, `safetySettings` - so a
    /// parameter missing from it is unchecked rather than ignored.
    fn lists_every_parameter(&self) -> bool {
        false
    }

    fn published(&self, parameter: &str) -> Published {
        let listed = self.listed.lock();
        Published::default()
            .with_default(listed.defaults.get(parameter).cloned())
            .with_maximum(
                listed
                    .maxima
                    .get(parameter)
                    .and_then(Value::as_number)
                    .cloned(),
            )
    }

    /// The conventional dialect cannot take either of these. In this dialect the shape of a turn is
    /// an order; flattening it into three slots on the way out would undo, one request later, the
    /// ordering recorded on the way in. It takes a turn's thinking back as a part marked `thought`,
    /// which a signed thought requires.
    fn projection(&self) -> LinearProjector {
        LinearProjector {
            send_blocks: true,
            ..Default::default()
        }
    }
}

/// Puts a boundary between two text parts that are about to become one turn.
///
/// note: this API joins a turn's text parts with nothing between them, so two merged items run
/// together: a note ending `…the codename is kotelnaya` followed by `what is the codename?` reaches
/// the model as `kotelnayawhat is the codename?`. Every reference `kamchatka` sends (a file from
/// `-f`, an `/attach`, a `/note`) is followed by the question about it.
///
/// note: the other dialect keeps one message per item and needs none of this. Here parts are
/// merged, so the separator goes inside the text, and this is the last place that knows there were
/// two items.
///
/// note: text against text only: a `functionResponse` is a field of its own and a blob is
/// `inline_data`, so neither runs into a neighbour. The boundary is normalised (trailing newlines
/// trimmed, then exactly one blank line), so a file ending in three newlines or in none is
/// separated the same way and one request always renders to the same bytes.
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
