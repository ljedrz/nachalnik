//! Google's own dialect, in which an assistant turn is an ordered list of parts.
//!
//! note: this exists because the OpenAI-compatible shim in [`provider`](crate::provider) cannot
//! say what the model actually did. Gemini answers with `content.parts[]` - a thinking part, a
//! sentence, a `functionCall`, in the order they were produced - and the shim flattens that into
//! a `content` string beside a `tool_calls` array, because the dialect it is imitating has no
//! order to report. Everything downstream then reads a turn that has been rearranged. This one
//! reports the order, as [`Content::Blocks`], and sends it back the same way.
//!
//! note: it is a second provider rather than a flag on the first because they are two wire
//! formats, not two settings: different paths, different auth header, different names for every
//! field, `functionResponse` parts inside a user turn instead of a `tool` role. What they share is
//! the trait the kernel talks through, which is the point being demonstrated.
//!
//! note: signatures are the reason to bother beyond tidiness. Gemini signs the parts of a turn and
//! answers `400 Function call is missing a thought_signature` when one comes back without its
//! signature; the shim reports a call's signature and drops the one on a text part, because a
//! message has nowhere to keep it. Here every part's own extra fields ride on the block they
//! belong to - [`Part::extra`] and [`ToolCall::extra`] - and go back out attached to the same
//! part, uninterpreted.

use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use nachalnik::{
    Block, BoxError, Content, DeltaSink, Message, ModelInfo, ModelRequest, ModelResponse, Part,
    Provider, Role, StopReason, ToolCall, ToolCallId, Usage, async_trait,
};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::provider::{
    Endpoint, PATIENCE, RETRIES, Silence, Vigil, api_key, configured_limit, install_crypto,
    same_model,
};

/// How long a stream may say nothing before the provider looks up to check whether it has been
/// asked to stop.
const HEARTBEAT: Duration = Duration::from_millis(200);

/// Where Google's own API lives, unless told otherwise.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google's `generateContent`, streamed, with the order of a turn kept.
pub struct Gemini {
    client: reqwest::Client,
    base_url: Mutex<String>,
    api_key: String,
    model: Mutex<String>,
    context_limit: Mutex<Option<usize>>,
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
            context_limit: Mutex::new(configured_limit()),
            attempts: AtomicUsize::new(0),
            notice: Mutex::new(None),
        }
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

    /// Everything in a part except the fields this provider understands.
    ///
    /// note: not just `thoughtSignature`, though that is the one that matters today. Whatever
    /// Google puts beside the text next is carried back unread, which is the same bargain
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
            // is dropped is still in `ModelResponse::raw`, which is the whole of what a provider
            // can honestly offer about a shape it has no variant for
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
    /// the order this API expects, which is the reassembly that used to be all anyone could do.
    fn turn(message: &Message) -> Vec<Value> {
        let Some(blocks) = message.blocks() else {
            let mut parts = Vec::new();
            if let Some(thinking) = &message.reasoning {
                parts.push(json!({ "text": thinking.to_text(), "thought": true }));
            }
            if let Some(said) = message
                .content
                .as_ref()
                .map(Content::to_text)
                .filter(|said| !said.is_empty())
            {
                parts.push(json!({ "text": said }));
            }
            parts.extend(message.tool_calls.iter().map(Self::asking));

            return parts;
        };

        blocks
            .iter()
            .map(|block| match block {
                Block::Text(part) => {
                    Self::reattach(json!({ "text": part.content.to_text() }), &part.extra)
                }
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
                _ => (
                    "user",
                    vec![json!({
                        "text": message.content.as_ref().map(Content::to_text).unwrap_or_default()
                    })],
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
        body["generationConfig"] = json!({ "thinkingConfig": { "includeThoughts": true } });
        for (key, value) in &request.params {
            match (key.as_str(), value.as_object()) {
                ("generationConfig", Some(set)) => {
                    for (key, value) in set {
                        body["generationConfig"][key] = value.clone();
                    }
                }
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
        let (base, model) = (self.endpoint(), self.model.lock().clone());

        // the same bargain the other provider makes: waiting and trying again is the provider's
        // business, because the kernel must not send a request twice behind a caller's back
        let mut response = loop {
            let response = self
                .client
                .post(format!(
                    "{base}/models/{model}:streamGenerateContent?alt=sse"
                ))
                .header("x-goog-api-key", &self.api_key)
                .json(&body)
                .send()
                .await?;

            let status = response.status();
            if status.is_success() {
                // the budget belongs to a request, not to a session: without this an afternoon
                // that had already ridden out four busy servers answered the fifth by giving up
                // on the first try
                self.attempts.store(0, Ordering::SeqCst);
                break response;
            }

            let transient = status.as_u16() == 429 || status.is_server_error();
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if !transient || attempt >= RETRIES {
                self.attempts.store(0, Ordering::SeqCst);
                let body = response.text().await.unwrap_or_default();
                return Err(format!("{status}: {body}").into());
            }

            let wait = Duration::from_secs(1 << attempt);
            *self.notice.lock() = Some(format!(
                "{model} answered {}; trying again in {}s",
                status.as_u16(),
                wait.as_secs()
            ));
            tokio::time::sleep(wait).await;
        };

        let mut buffer = String::new();
        let mut partial: Vec<Partial> = Vec::new();
        let mut finish = None;
        let mut usage = None;
        let mut chunks = Vec::new();
        let mut vigil = Vigil::new();

        loop {
            // without the timeout this sits in `chunk` until the server feels like talking, and a
            // request that stalls before its first byte leaves `esc` doing nothing whatever
            let bytes = match tokio::time::timeout(HEARTBEAT, response.chunk()).await {
                Ok(Ok(Some(bytes))) => {
                    if vigil.heard() {
                        *self.notice.lock() = Some(format!("{model} is answering again"));
                    }
                    bytes
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => {
                    if deltas.is_interrupted() {
                        finish = Some("interrupted".to_owned());
                        break;
                    }
                    // note: a turn that asks for a tool makes two requests rather than one, which
                    // is twice the chance of meeting a server in this state - and it is the shape
                    // "it hangs whenever it uses a tool" really has
                    match vigil.waited() {
                        Silence::Enough => {
                            return Err(format!(
                                "{model} answered and then said nothing for {}s; giving up",
                                PATIENCE.as_secs()
                            )
                            .into());
                        }
                        Silence::Worth(seconds) => {
                            *self.notice.lock() = Some(format!(
                                "{model} has said nothing for {seconds}s; esc gives up on it"
                            ));
                        }
                        Silence::Ordinary => {}
                    }
                    continue;
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(end) = buffer.find('\n') {
                if deltas.is_interrupted() {
                    finish = Some("interrupted".to_owned());
                    break;
                }

                let line = buffer[..end].trim().to_owned();
                buffer.drain(..=end);

                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(chunk) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                if let Some(error) = chunk.get("error").filter(|e| !e.is_null()) {
                    return Err(format!("{error}").into());
                }

                if let Some(reported) = chunk.get("usageMetadata").filter(|u| !u.is_null()) {
                    usage = Some(Usage {
                        input_tokens: reported["promptTokenCount"].as_u64(),
                        output_tokens: reported["candidatesTokenCount"].as_u64(),
                        reasoning_tokens: reported["thoughtsTokenCount"].as_u64(),
                        cached_input_tokens: reported["cachedContentTokenCount"].as_u64(),
                    });
                }

                let candidate = &chunk["candidates"][0];
                if let Some(reason) = candidate["finishReason"].as_str() {
                    finish = Some(reason.to_owned());
                }
                for part in candidate["content"]["parts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    Self::absorb(part, &mut partial, &deltas);
                }

                chunks.push(chunk);
            }
            if finish.as_deref() == Some("interrupted") {
                break;
            }
        }

        if chunks.is_empty() {
            if finish.as_deref() == Some("interrupted") || deltas.is_interrupted() {
                return Ok(ModelResponse {
                    content: None,
                    reasoning: None,
                    tool_calls: Vec::new(),
                    stop: StopReason::Other("interrupted".to_owned()),
                    usage: None,
                    raw: None,
                });
            }

            let payload: Value = serde_json::from_str(&buffer).unwrap_or(Value::Null);
            return match payload.get("error").filter(|e| !e.is_null()) {
                Some(error) => Err(format!("{error}").into()),
                None => Err(format!("the stream carried no data: {buffer}").into()),
            };
        }

        let blocks: Vec<Block> = partial
            .into_iter()
            .map(|part| match part {
                Partial::Text(said, extra) => Block::Text(Part::new(said).with_extra(extra)),
                Partial::Reasoning(said, extra) => {
                    Block::Reasoning(Part::new(said).with_extra(extra))
                }
                Partial::Call(call) => Block::Call(call),
            })
            .collect();
        let asked = blocks.iter().any(|block| block.call().is_some());

        Ok(ModelResponse {
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
                _ if asked => StopReason::ToolUse,
                Some("STOP") => StopReason::EndTurn,
                Some("MAX_TOKENS") => StopReason::Length,
                Some("SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII") => StopReason::Refusal,
                Some(other) => StopReason::Other(other.to_lowercase()),
                None => StopReason::Other("unreported".to_owned()),
            },
            usage,
            raw: Some(json!({ "stream": chunks })),
        })
    }
}

#[async_trait]
impl Endpoint for Gemini {
    fn endpoint(&self) -> String {
        self.base_url.lock().clone()
    }

    fn model(&self) -> String {
        self.model.lock().clone()
    }

    async fn models(&self) -> Vec<String> {
        let base = self.endpoint();
        let Ok(response) = self
            .client
            .get(format!("{base}/models?pageSize=200"))
            .header("x-goog-api-key", &self.api_key)
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
        *self.context_limit.lock() = configured_limit();
        self.probe().await;

        let model = self.model.lock().clone();
        let listed = self.models().await;
        if !listed.is_empty() && !listed.iter().any(|name| same_model(name, &model)) {
            *self.notice.lock() = Some(format!(
                "{model} is not one of the {} models at this address; /models lists them",
                listed.len()
            ));
        }
    }

    async fn set_endpoint(&self, url: String, model: Option<String>) {
        *self.base_url.lock() = url;
        *self.context_limit.lock() = configured_limit();
        match model {
            Some(model) => self.set_model(model).await,
            None => self.probe().await,
        }
    }

    fn take_notice(&self) -> Option<String> {
        self.notice.lock().take()
    }
}

/// The endpoint to talk to; Google's own unless told otherwise.
pub fn base_url() -> String {
    env::var("KAMCHATKA_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned())
}

/// Builds a provider from the environment, asking the endpoint what the model's limit is.
pub async fn connect(model: impl Into<String>) -> Result<Arc<Gemini>, BoxError> {
    let provider = Arc::new(Gemini::new(model, base_url(), api_key()?));
    provider.probe().await;

    Ok(provider)
}
