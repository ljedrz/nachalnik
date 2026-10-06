//! Anthropic's own dialect, the Messages API: an assistant turn is an ordered list of typed blocks.
//!
//! note: the third wire format rather than a setting on either of the others, for the reasons the
//! Gemini dialect gives: a different path, a different auth header, the instructions outside the
//! conversation, tool results as blocks inside a user turn, and a stream of named events - a block
//! opened, fragments of it, the block closed - rather than chunks of a message.
//!
//! note: what makes it worth having is the thinking. A thinking block carries a `signature`, and a
//! turn that used tools has to send its thinking back *with* that signature or the next request is
//! refused; the OpenAI dialect has nowhere to keep one. Here a block's own fields ride on the block
//! - [`Part::extra`], [`ToolCall::extra`] - and go back out on the same block.
//!
//! note: what goes back out is only what this API defines for each kind of block, and not whatever
//! happens to be in `extra`. A session that started against Gemini carries `thoughtSignature` on
//! its parts, and a field this API does not know is a 400; thinking with no signature at all - any
//! other provider's - cannot be sent either, so it is left out rather than sent and refused.
//!
//! note: OpenRouter answers this dialect too, at `/api/v1/messages`, and adds to it: a `provider`
//! and a `cost`, ids of its own, and a `[DONE]` after `message_stop`. All of it is read past.

use std::sync::atomic::{AtomicUsize, Ordering};

use nachalnik::{
    Block, BoxError, Content, DeltaSink, LinearProjector, Message, ModelInfo, ModelRequest,
    ModelResponse, Part, Provider, Role, StopReason, ToolCall, ToolCallId, Usage, async_trait,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::{
    Dialect, Endpoint, Keyed, install_crypto,
    reading::{Events, Read, Stopped, not_a_stream},
    refused,
    waiting::{Asking, Sent, interrupted, sent},
};

/// Where Anthropic's own API lives, unless told otherwise.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// The version of the API this dialect is written against, sent with every request.
pub const VERSION: &str = "2023-06-01";

/// How much a turn may generate, where the endpoint has not said what the model takes.
///
/// note: this API requires `max_tokens` on every request and has no default to fall back on, so
/// one has to be chosen. `max_tokens` in the parameters replaces it.
const DEFAULT_MAX_TOKENS: u64 = 16_384;

/// The most a turn is allowed by default, whatever the model takes.
///
/// note: a model that takes 128K of output would otherwise be asked for all of it on every
/// request, and an endpoint that counts `max_tokens` against the context window refuses a long
/// conversation for room it was never going to use. The parameter is still there for a turn that
/// needs more.
const MOST_BY_DEFAULT: u64 = 32_000;

/// Anthropic's Messages API, streamed, with the order of a turn and its signatures kept.
pub struct Anthropic {
    client: reqwest::Client,
    base_url: Mutex<String>,
    api_key: String,
    model: Mutex<String>,
    context_limit: Mutex<Option<usize>>,
    /// The most the model generates in one turn, where the endpoint said.
    output_limit: Mutex<Option<u64>>,
    /// The limit the caller set by hand, if it set one, kept so that changing model or endpoint
    /// puts it back rather than dropping it.
    configured: Option<usize>,
    /// Every request for an answer this has sent, retries included, never reset.
    attempts: AtomicUsize,
    notice: Mutex<Option<String>>,
}

/// One block of the turn, being assembled from the stream.
///
/// note: the text is accumulated in a `String` and made into a [`Content`] once, because appending
/// to an `Arc<str>` rebuilds it every time and a streamed paragraph would be quadratic.
enum Partial {
    Text(String, Map<String, Value>),
    Thinking(String, Map<String, Value>),
    /// Thinking this API will not show, carried back as it came.
    Redacted(Map<String, Value>),
    Call {
        id: String,
        name: String,
        /// The arguments the block opened with: `{}` in a stream, all of them in a whole answer.
        input: Value,
        /// The fragments that replace them, as they arrived.
        json: String,
        extra: Map<String, Value>,
    },
}

impl Anthropic {
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
            output_limit: Mutex::new(None),
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
    /// note: sticky across [`Endpoint::set_model`] and [`Endpoint::set_endpoint`], as the other
    /// dialects' is. `None` means "ask the endpoint", which is the default.
    pub fn with_context_limit(mut self, limit: Option<usize>) -> Self {
        self.configured = limit;
        *self.context_limit.get_mut() = limit;
        self
    }

    /// Asks the endpoint what the model takes in and how much it may generate.
    ///
    /// note: two questions, because two kinds of endpoint speak this dialect. Anthropic's
    /// describes one model at `/models/{id}`; OpenRouter has no such path for an id with a `/` in
    /// it, and answers only the listing - in this dialect's shape when asked with its headers,
    /// twenty to a page unless asked for more. The first is asked first, and the listing only
    /// where it said nothing. OpenRouter's own shape, `context_length` and
    /// `top_provider.max_completion_tokens`, is read as well, for a proxy that answers with it.
    pub async fn probe(&self) {
        let (base, model) = (self.endpoint(), self.model());
        let described = self.get(format!("{base}/models/{model}")).await;
        let mut found = described.as_ref().map(limits).unwrap_or_default();

        if found == (None, None)
            && let Some(listed) = self.get(format!("{base}/models?limit=1000")).await
        {
            found = listed["data"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| entry["id"] == model.as_str())
                .map(limits)
                .unwrap_or_default();
        }

        let (context, output) = found;
        if self.context_limit.lock().is_none() {
            *self.context_limit.lock() = context.map(|limit| limit as usize);
        }
        *self.output_limit.lock() = output;
    }

    /// One question about the endpoint, answered as JSON or not at all.
    async fn get(&self, url: String) -> Option<Value> {
        let response = self
            .client
            .get(url)
            .anthropic_key(&self.api_key)
            .header("anthropic-version", VERSION)
            .timeout(crate::ASKING)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json::<Value>().await.ok()
    }

    /// Puts a notice up if the model is not one the endpoint lists; see the Gemini dialect's.
    async fn say_if_the_model_is_not_there(&self) {
        let model = self.model();
        let listed = self.models().await;
        *self.notice.lock() = crate::unlisted(&model, &listed);
    }

    /// The parts of a turn that go out as a user turn: what the person said, and what they
    /// attached.
    fn said(content: Option<&Content>) -> Vec<Value> {
        match content {
            Some(Content::Blocks(blocks)) => blocks
                .iter()
                .filter_map(Block::said)
                .filter_map(|said| Self::block_of(&said.content))
                .collect(),
            Some(said) => Self::block_of(said).into_iter().collect(),
            None => Vec::new(),
        }
    }

    /// One piece of content as the block this API carries it in, or nothing for empty text,
    /// which this API refuses.
    ///
    /// note: a picture is an `image` and anything else is a `document`, by its media type. This
    /// API reads PDFs that way and refuses the rest in its own words, which say more than a
    /// placeholder made up here would.
    fn block_of(content: &Content) -> Option<Value> {
        match content.as_blob() {
            Some(blob) => {
                let kind = match blob.media_type.starts_with("image/") {
                    true => "image",
                    false => "document",
                };
                Some(json!({
                    "type": kind,
                    "source": {
                        "type": "base64",
                        "media_type": blob.media_type,
                        "data": blob.data,
                    },
                }))
            }
            None => {
                let text = content.to_text();
                (!text.is_empty()).then(|| json!({ "type": "text", "text": text }))
            }
        }
    }

    /// The blocks an assistant turn goes back out as.
    ///
    /// note: a turn recorded as blocks goes back in the order it arrived in. One recorded the
    /// conventional way - by a session that started against another provider - is assembled as
    /// what was said and then the calls; its thinking has no signature and is not sent.
    fn turn(message: &Message) -> Vec<Value> {
        let Some(blocks) = message.blocks() else {
            let mut turn: Vec<Value> = message
                .content
                .as_ref()
                .and_then(Self::block_of)
                .into_iter()
                .collect();
            turn.extend(message.tool_calls.iter().map(Self::asking));
            return turn;
        };

        blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text(part) => Self::block_of(&part.content)
                    .map(|text| Self::carrying(text, &part.extra, &["citations"])),
                Block::Reasoning(part) => Self::thought(part),
                Block::Call(call) => Some(Self::asking(call)),
                // `Block` is `#[non_exhaustive]`: a variant this provider has never heard of is
                // dropped rather than guessed at, and the turn is still valid without it
                _ => None,
            })
            .collect()
    }

    /// A block with the fields of `extra` this API defines for it put back on it.
    fn carrying(mut block: Value, extra: &Value, defined: &[&str]) -> Value {
        if let (Some(block), Some(extra)) = (block.as_object_mut(), extra.as_object()) {
            for key in defined {
                if let Some(value) = extra.get(*key).filter(|value| !is_empty(value)) {
                    block.insert((*key).to_owned(), value.clone());
                }
            }
        }

        block
    }

    /// A turn's thinking as this API takes it back: signed, or redacted as it came, or not at all.
    fn thought(part: &Part) -> Option<Value> {
        let extra = part.extra.as_object()?;
        if extra.get("type").and_then(Value::as_str) == Some("redacted_thinking") {
            return Some(Value::Object(extra.clone()));
        }

        // note: an empty text is still a thought. The newer models omit what they thought by
        // default and send the signature over nothing, and the signature is what has to go back
        let signature = extra.get("signature").filter(|value| !is_empty(value))?;
        Some(json!({
            "type": "thinking",
            "thinking": part.content.to_text(),
            "signature": signature,
        }))
    }

    /// One tool call, as a `tool_use` block.
    fn asking(call: &ToolCall) -> Value {
        let input = match &*call.args {
            Value::Object(_) => (*call.args).clone(),
            // this API takes an object and nothing else; arguments that never were one are handed
            // over inside one, as the dialect that received them would have recorded them
            Value::Null => json!({}),
            other => json!({ "_unparsed": other }),
        };
        let block = json!({
            "type": "tool_use",
            "id": wire_id(&call.id),
            "name": call.tool,
            "input": input,
        });

        Self::carrying(block, &call.extra, &["caller"])
    }

    /// One tool result, as a `tool_result` block of the user turn that answers the model's.
    fn answering(message: &Message) -> Value {
        let mut answered = json!({
            "type": "tool_result",
            "tool_use_id": message.tool_call_id.as_ref().map(wire_id).unwrap_or_default(),
        });

        // text as a string, and anything that is more than text - a screenshot - as the blocks
        // it is made of; nothing at all is an empty result, which this API takes with no content
        let content = match message.content.as_ref() {
            Some(Content::Json(value)) => Some(json!(value.to_string())),
            Some(said @ Content::Blocks(_)) => Some(json!(Self::said(Some(said)))),
            Some(said) if said.as_blob().is_some() => Some(json!(Self::said(Some(said)))),
            Some(said) => Some(json!(said.to_text())).filter(|text| text != ""),
            None => None,
        };
        if let Some(content) = content.filter(|content| !is_empty(content)) {
            answered["content"] = content;
        }

        answered
    }

    /// Reads a block that has just opened.
    fn opened(block: &Value, deltas: &DeltaSink) -> Option<Partial> {
        let fields = block.as_object()?;
        let rest = |taken: &[&str]| -> Map<String, Value> {
            fields
                .iter()
                .filter(|(key, _)| key.as_str() != "type" && !taken.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        };

        match block["type"].as_str()? {
            "text" => {
                let text = block["text"].as_str().unwrap_or_default();
                if !text.is_empty() {
                    deltas.text(text);
                }
                Some(Partial::Text(text.to_owned(), rest(&["text"])))
            }
            "thinking" => {
                let text = block["thinking"].as_str().unwrap_or_default();
                if !text.is_empty() {
                    deltas.reasoning(text);
                }
                Some(Partial::Thinking(text.to_owned(), rest(&["thinking"])))
            }
            "redacted_thinking" => Some(Partial::Redacted(fields.clone())),
            "tool_use" => Some(Partial::Call {
                id: block["id"].as_str().unwrap_or_default().to_owned(),
                name: block["name"].as_str().unwrap_or_default().to_owned(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
                json: String::new(),
                extra: rest(&["id", "name", "input"]),
            }),
            // a server tool's call and its result, and whatever comes next. Dropped rather than
            // guessed at, as the Gemini dialect drops a part it has no variant for; what is
            // dropped is still in `ModelResponse::raw`
            _ => None,
        }
    }

    /// Takes a fragment of a block that is already open.
    fn grown(partial: &mut Partial, delta: &Value, deltas: &DeltaSink) {
        match (partial, delta["type"].as_str()) {
            (Partial::Text(text, _), Some("text_delta")) => {
                let more = delta["text"].as_str().unwrap_or_default();
                deltas.text(more);
                text.push_str(more);
            }
            (Partial::Thinking(text, _), Some("thinking_delta")) => {
                let more = delta["thinking"].as_str().unwrap_or_default();
                deltas.reasoning(more);
                text.push_str(more);
            }
            (Partial::Thinking(_, extra), Some("signature_delta")) => {
                extra.insert("signature".to_owned(), delta["signature"].clone());
            }
            (Partial::Call { id, json, .. }, Some("input_json_delta")) => {
                let more = delta["partial_json"].as_str().unwrap_or_default();
                if !more.is_empty() {
                    deltas.tool_args(ToolCallId(id.clone()), more);
                    json.push_str(more);
                }
            }
            (Partial::Text(_, extra), Some("citations_delta")) => {
                let cited = extra
                    .entry("citations")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(cited) = cited.as_array_mut() {
                    cited.push(delta["citation"].clone());
                }
            }
            _ => {}
        }
    }
}

/// A call's identifier as this API takes one: letters, digits, `_` and `-`, and nothing else.
///
/// note: the identifiers in a session are whoever made them - another provider, or the kernel
/// repairing a call that came without one - and this API refuses a `tool_use` whose id has
/// anything else in it. The same rewriting is applied to the call and to its result, so the pair
/// still matches.
fn wire_id(id: &ToolCallId) -> String {
    let rewritten: String =
        id.0.chars()
            .map(
                |c| match c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    true => c,
                    false => '_',
                },
            )
            .collect();

    match rewritten.is_empty() {
        true => "call".to_owned(),
        false => rewritten,
    }
}

/// Whether a value says nothing: null, or an empty string, list or object.
fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(listed) => listed.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        _ => false,
    }
}

/// What an endpoint's description of a model says it takes in, and what it may generate.
fn limits(entry: &Value) -> (Option<u64>, Option<u64>) {
    let context = entry["max_input_tokens"]
        .as_u64()
        .or_else(|| entry["context_length"].as_u64());
    let output = entry["max_tokens"]
        .as_u64()
        .or_else(|| entry["top_provider"]["max_completion_tokens"].as_u64());
    (context, output)
}

#[async_trait]
impl Provider for Anthropic {
    fn info(&self) -> ModelInfo {
        // note: one lock to a statement, for the reason the other dialects' `info` gives
        let context_limit = *self.context_limit.lock();
        let model = self.model();
        let endpoint = crate::recorded(&self.base_url.lock());

        ModelInfo::new("anthropic", model)
            .with_context_limit(context_limit)
            .with_tool_calling(true)
            .with_reasoning(true)
            .with_endpoint(endpoint)
    }

    /// The payload, rendered once. `respond` sends exactly this.
    fn render(&self, request: &ModelRequest) -> Option<Value> {
        let mut instructions: Vec<String> = Vec::new();
        let mut messages: Vec<Value> = Vec::new();

        for message in &request.messages {
            let (role, blocks) = match message.role {
                Role::System => {
                    // this API keeps its instructions out of the conversation, as Google's does
                    instructions.extend(
                        message
                            .content
                            .as_ref()
                            .map(|said| said.to_text().into_owned())
                            .filter(|said| !said.is_empty()),
                    );
                    continue;
                }
                Role::Assistant => ("assistant", Self::turn(message)),
                Role::Tool => ("user", vec![Self::answering(message)]),
                _ => ("user", Self::said(message.content.as_ref())),
            };
            if blocks.is_empty() {
                continue;
            }

            // turns alternate here, so three tool results are three blocks of one turn rather
            // than three turns
            match messages.last_mut() {
                Some(last) if last["role"] == role => {
                    if let Some(existing) = last["content"].as_array_mut() {
                        existing.extend(blocks);
                    }
                }
                _ => messages.push(json!({ "role": role, "content": blocks })),
            }
        }

        // note: in a user turn the results come first. This API refuses one that answers a call
        // after something else has been said, and a reference or a note the kernel put between
        // the call and its result is exactly that something; moved behind the results, it is
        // still said, in the same turn
        for message in &mut messages {
            if message["role"] == "user"
                && let Some(blocks) = message["content"].as_array_mut()
            {
                blocks.sort_by_key(|block| block["type"] != "tool_result");
            }
        }

        let most = self
            .output_limit
            .lock()
            .map_or(DEFAULT_MAX_TOKENS, |limit| limit.min(MOST_BY_DEFAULT));
        let mut body = json!({
            "model": self.model(),
            "max_tokens": most,
            "messages": messages,
            "stream": true,
        });
        if !instructions.is_empty() {
            body["system"] = json!(instructions.join("\n\n"));
        }
        if !request.tools.is_empty() {
            body["tools"] = json!(
                request
                    .tools
                    .iter()
                    .map(|spec| json!({
                        "name": spec.id,
                        "description": spec.description,
                        "input_schema": spec.schema,
                    }))
                    .collect::<Vec<_>>()
            );
        }

        // note: the fields built from the request itself are not replaced, for the reason the
        // other dialects give: a parameter is carried beside the conversation, not in place of
        // it. `stream` is not one of them: `false` is a whole message, and a whole message is
        // read as the stream it would have been
        for (key, value) in &request.params {
            match key.as_str() {
                "model" | "messages" | "system" | "tools" => {}
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
        let model = self.model();
        let limit = *self.context_limit.lock();
        let asking = Asking {
            model: &model,
            deltas: &deltas,
            notice: &self.notice,
        };

        let url = format!("{base}/messages");
        let sending = || {
            self.client
                .post(&url)
                .anthropic_key(&self.api_key)
                .header("anthropic-version", VERSION)
                .json(&body)
        };
        let mut streamed = Streamed::default();
        match sent(&asking, &self.attempts, limit, true, sending, &mut streamed).await? {
            Sent::Interrupted => Ok(interrupted()),
            // never asked for here, and read as what it would be if it came
            Sent::Whole(payload) => unstreamed(payload.to_string(), streamed, &deltas),
            Sent::Streamed(Read::Events(events, stopped)) => Ok(answer(streamed, events, stopped)),
            Sent::Streamed(Read::Interrupted) => Ok(interrupted()),
            Sent::Streamed(Read::Unstreamed(body)) => unstreamed(body, streamed, &deltas),
            Sent::Streamed(Read::Refused { said, .. }) => Err(refused(said, limit)),
        }
    }
}

/// What the request cost, as the stream reported it, field by field.
///
/// note: kept apart until the end, because the figures arrive in two events - the prompt with
/// `message_start`, what was generated with `message_delta` - and a later one carries only some
/// of them.
#[derive(Default)]
struct Spent {
    input: Option<u64>,
    cache_written: Option<u64>,
    cache_read: Option<u64>,
    output: Option<u64>,
    thinking: Option<u64>,
}

impl Spent {
    fn read(&mut self, reported: &Value) {
        let field = |name: &str| reported[name].as_u64();
        self.input = field("input_tokens").or(self.input);
        self.cache_written = field("cache_creation_input_tokens").or(self.cache_written);
        self.cache_read = field("cache_read_input_tokens").or(self.cache_read);
        self.output = field("output_tokens").or(self.output);
        self.thinking = reported["output_tokens_details"]["thinking_tokens"]
            .as_u64()
            .or(self.thinking);
    }

    /// The figures as `Usage` defines them.
    ///
    /// note: this API's `input_tokens` is only the part of the prompt that was neither read from
    /// the cache nor written to it, and `Usage::input_tokens` is the whole prompt with the cached
    /// part inside it - so the three are added. Reported as they stand, a conversation served from
    /// the cache would read as a few tokens long, and every figure calibrated on it would be wrong.
    /// `output_tokens` already counts the thinking, which is what `Usage` asks.
    fn usage(&self) -> Option<Usage> {
        let input = match (self.input, self.cache_written, self.cache_read) {
            (None, None, None) => None,
            (input, written, read) => Some(
                input.unwrap_or_default() + written.unwrap_or_default() + read.unwrap_or_default(),
            ),
        };
        if input.is_none() && self.output.is_none() {
            return None;
        }

        Some(Usage {
            input_tokens: input,
            output_tokens: self.output,
            reasoning_tokens: self.thinking,
            cached_input_tokens: self.cache_read,
        })
    }
}

/// A stream, read: the turn as its blocks left it.
#[derive(Default)]
struct Streamed {
    /// The blocks so far, by the index the stream gave each one.
    blocks: Vec<(u64, Partial)>,
    /// Why the turn ended, once something has said.
    finish: Option<String>,
    /// Whether the stream's own end has arrived.
    stopped: bool,
    spent: Spent,
}

impl Events for Streamed {
    fn event(&mut self, event: &Value, deltas: &DeltaSink) {
        match event["type"].as_str() {
            Some("message_start") => self.spent.read(&event["message"]["usage"]),
            Some("content_block_start") => {
                let index = event["index"].as_u64().unwrap_or(self.blocks.len() as u64);
                if let Some(partial) = Anthropic::opened(&event["content_block"], deltas) {
                    self.blocks.push((index, partial));
                }
            }
            Some("content_block_delta") => {
                let index = event["index"].as_u64();
                if let Some((_, partial)) = self
                    .blocks
                    .iter_mut()
                    .rev()
                    .find(|(at, _)| index.is_none_or(|index| *at == index))
                {
                    Anthropic::grown(partial, &event["delta"], deltas);
                }
            }
            Some("message_delta") => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.finish = Some(reason.to_owned());
                }
                self.spent.read(&event["usage"]);
            }
            Some("message_stop") => self.stopped = true,
            // `ping`, `content_block_stop`, and whatever is added next
            _ => {}
        }
    }

    fn finished(&self) -> bool {
        self.finish.is_some() || self.stopped
    }
}

/// A body that was not a stream, read as the events it would have been streamed as - or, where it
/// is not a message, the error that says so.
///
/// note: what comes back where `stream` did not reach the server, which a proxy that rewrote the
/// body is enough for. A whole message's blocks are the blocks a stream would have opened, with
/// everything already in them.
fn unstreamed(
    body: String,
    mut streamed: Streamed,
    deltas: &DeltaSink,
) -> Result<ModelResponse, BoxError> {
    let message = match serde_json::from_str::<Value>(&body) {
        Ok(message) if message["type"] == "message" && message["content"].is_array() => message,
        _ => return Err(not_a_stream(&body)),
    };

    let mut events =
        vec![json!({ "type": "message_start", "message": { "usage": message["usage"] } })];
    for (index, block) in message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        events
            .push(json!({ "type": "content_block_start", "index": index, "content_block": block }));
        events.push(json!({ "type": "content_block_stop", "index": index }));
    }
    events.push(json!({
        "type": "message_delta",
        "delta": { "stop_reason": message["stop_reason"] },
        "usage": message["usage"],
    }));
    events.push(json!({ "type": "message_stop" }));

    for event in &events {
        streamed.event(event, deltas);
    }
    Ok(answer(streamed, vec![message], Stopped::Done))
}

/// The turn the stream came to: the blocks in the order they were opened, and what ended it.
fn answer(streamed: Streamed, events: Vec<Value>, stopped: Stopped) -> ModelResponse {
    let Streamed {
        mut blocks,
        mut finish,
        spent,
        ..
    } = streamed;
    match stopped {
        Stopped::Done => {}
        Stopped::Interrupted => finish = Some("interrupted".to_owned()),
        Stopped::CutOff => finish = Some("cut off".to_owned()),
    }

    blocks.sort_by_key(|(index, _)| *index);
    let blocks: Vec<Block> = blocks
        .into_iter()
        .map(|(_, partial)| match partial {
            Partial::Text(said, extra) => {
                Block::Text(Part::new(said).with_extra(Value::Object(extra)))
            }
            Partial::Thinking(said, extra) => {
                Block::Reasoning(Part::new(said).with_extra(Value::Object(extra)))
            }
            Partial::Redacted(fields) => {
                Block::Reasoning(Part::new("").with_extra(Value::Object(fields)))
            }
            Partial::Call {
                id,
                name,
                input,
                json,
                extra,
            } => {
                // the fragments are the arguments where any came, and what the block opened with
                // where none did; fragments that are not JSON are handed over as written, so the
                // model is shown what it wrote rather than a call run on nothing
                let args = match json.trim() {
                    "" if input.is_object() => input,
                    "" => json!({}),
                    written => serde_json::from_str::<Value>(written)
                        .unwrap_or_else(|_| json!({ "_unparsed": written })),
                };
                Block::Call(ToolCall::new(id, name, args).with_extra(Value::Object(extra)))
            }
        })
        .collect();
    let asked = blocks.iter().any(|block| block.call().is_some());

    ModelResponse {
        // the whole turn in one slot, in the order it was produced; see the Gemini dialect
        content: (!blocks.is_empty()).then(|| Content::blocks(blocks)),
        reasoning: None,
        tool_calls: Vec::new(),
        stop: match finish.as_deref() {
            Some("interrupted") => StopReason::Other("interrupted".to_owned()),
            // ahead of `asked`, for the reason the Gemini dialect gives
            Some("cut off") => StopReason::Other("cut off".to_owned()),
            Some("refusal") => StopReason::Refusal,
            // running out of room is said nowhere else, whether or not the turn asked for anything
            Some("max_tokens" | "model_context_window_exceeded") => StopReason::Length,
            _ if asked => StopReason::ToolUse,
            Some("end_turn" | "stop_sequence") => StopReason::EndTurn,
            Some(other) => StopReason::Other(other.to_owned()),
            None => StopReason::Other("unreported".to_owned()),
        },
        usage: spent.usage(),
        raw: Some(json!({ "stream": events })),
    }
}

#[async_trait]
impl Endpoint for Anthropic {
    fn endpoint(&self) -> String {
        self.endpoint()
    }

    fn model(&self) -> String {
        self.model()
    }

    /// note: `limit` for Anthropic's own listing, which pages twenty at a time unless asked for
    /// more; OpenRouter lists everything either way and reads past the query.
    async fn models(&self) -> Vec<String> {
        let base = self.endpoint();
        let Some(body) = self.get(format!("{base}/models?limit=1000")).await else {
            return Vec::new();
        };

        body["data"]
            .as_array()
            .map(|listed| {
                listed
                    .iter()
                    .filter_map(|model| model["id"].as_str())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn set_model(&self, model: String) {
        *self.model.lock() = model;
        *self.context_limit.lock() = self.configured;
        *self.output_limit.lock() = None;
        self.probe().await;
        self.say_if_the_model_is_not_there().await;
    }

    /// note: the check happens whether or not a model was named, for the reason the Gemini
    /// dialect's gives.
    async fn set_endpoint(&self, url: String, model: Option<String>) {
        *self.base_url.lock() = crate::address(url);
        *self.context_limit.lock() = self.configured;
        *self.output_limit.lock() = None;
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

impl Dialect for Anthropic {
    /// Both of the things the conventional dialect cannot take, for the reasons the Gemini
    /// dialect gives: the order of a turn, and its thinking going back as blocks - which, signed,
    /// it has to.
    ///
    /// note: one mismatch is left, and it is in the projector's terms rather than this dialect's.
    /// Thinking another provider recorded - in a session that started against one - has no
    /// signature, so `render` leaves it out, and the budget still counts it: the projector has a
    /// switch for all reasoning and none for unsigned reasoning. Turning it off would drop the
    /// signed blocks too, which is the one thing this dialect cannot afford, so the cost is an
    /// estimate that is high by that thinking until it is pruned or the session moves on.
    fn projection(&self) -> LinearProjector {
        LinearProjector {
            send_blocks: true,
            ..Default::default()
        }
    }
}
