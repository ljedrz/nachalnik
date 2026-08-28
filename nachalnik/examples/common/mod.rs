//! The boilerplate the networked examples share: a [`Provider`] that speaks the OpenAI
//! chat-completions dialect over HTTP, streamed.
//!
//! This is not part of the library and it is not what the examples are about - it is a hundred
//! and fifty lines of server-sent events that `agent`, `compare` and `panel` would otherwise say
//! three times each. The interesting code is in the examples themselves.
//!
//! It is pulled in with `#[path = "common/mod.rs"] mod common;`, because a directory under
//! `examples/` with no `main.rs` is not built as an example of its own.

// each example uses a different part of this
#![allow(dead_code)]

use std::{
    env,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use nachalnik::{
    BoxError, Content, DeltaSink, Message, ModelInfo, ModelRequest, ModelResponse, Provider,
    StopReason, ToolCall, ToolCallId, Usage, async_trait,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// How many times a request is retried when the server says it is busy.
const RETRIES: usize = 4;

/// Any server that speaks the OpenAI chat-completions dialect, streamed.
pub struct OpenAiCompatible {
    pub client: reqwest::Client,
    pub base_url: String,
    pub api_key: String,
    pub model: Mutex<String>,
    pub context_limit: Mutex<Option<usize>>,
    /// Whether fragments are printed as they arrive.
    ///
    /// note: For a client that drives one loop on the terminal's own task this is the simplest
    /// thing that works. For one that runs several models at once it is exactly wrong - their
    /// output would interleave into nonsense - so those subscribe to
    /// [`Event::ModelDelta`](nachalnik::Event::ModelDelta) instead, or wait.
    pub echo: bool,
    /// How many times this provider has backed off, so that a busy server cannot be retried
    /// forever by a session that keeps making new requests.
    attempts: AtomicUsize,
}

/// A tool call being assembled from streamed fragments.
#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
    /// Whatever the provider attached to the call, which it will want back verbatim.
    extra: Value,
}

/// Translates a kernel message into the wire format.
fn to_wire(message: &Message) -> Value {
    let mut wire = json!({
        "role": message.role.as_str()
    });

    if let Some(content) = &message.content {
        wire["content"] = json!(content.to_text());
    }
    if !message.tool_calls.is_empty() {
        wire["tool_calls"] = Value::Array(
            message
                .tool_calls
                .iter()
                .map(|call| {
                    let mut wire = json!({
                        "id": call.id.0,
                        "type": "function",
                        "function": { "name": call.tool, "arguments": call.args.to_string() },
                    });
                    // some APIs hand back a signature per call and reject the next request
                    // without it, so it goes back exactly as it arrived
                    if !call.extra.is_null() {
                        wire["extra_content"] = (*call.extra).clone();
                    }

                    wire
                })
                .collect(),
        );
    }
    if let Some(id) = &message.tool_call_id {
        wire["tool_call_id"] = json!(id.0);
    }
    if let Some(name) = &message.name {
        wire["name"] = json!(name);
    }

    wire
}

impl OpenAiCompatible {
    /// Builds a provider for one model, reading the endpoint and the key from the environment.
    ///
    /// note: The client is passed in rather than made here, so that several models on the same
    /// host share one connection pool.
    pub fn new(client: reqwest::Client, model: impl Into<String>) -> Result<Self, BoxError> {
        Ok(Self {
            client,
            base_url: base_url(),
            api_key: api_key()?,
            model: Mutex::new(model.into()),
            context_limit: Mutex::new(
                env::var("NACHALNIK_CONTEXT_LIMIT")
                    .ok()
                    .and_then(|v| v.parse().ok()),
            ),
            echo: false,
            attempts: AtomicUsize::new(0),
        })
    }

    /// Prints the model's output as it streams in.
    pub fn echoing(mut self) -> Self {
        self.echo = true;
        self
    }

    /// Asks the provider what it knows about the model, so that the context limit the kernel
    /// reports is the real one rather than a guess.
    ///
    /// note: Worth the round trip, because everything the kernel says about how full a context is
    /// is measured against this number. An unknown limit is reported as unknown rather than
    /// guessed at, which is the honest answer but not a useful one.
    pub async fn probe(&self) {
        if self.context_limit.lock().is_some() {
            return;
        }

        let mut limit = self
            .listed_limit(&format!("{}/models", self.base_url), true)
            .await;

        // an OpenAI-compatible listing does not have to carry a context length, and Google's does
        // not; its native one does, one path up
        if limit.is_none()
            && let Some(native) = self.base_url.strip_suffix("/openai")
        {
            limit = self.listed_limit(&format!("{native}/models"), false).await;
        }
        // ollama's does not either, and the number its `/api/show` advertises is the wrong one
        if limit.is_none()
            && let Some(root) = self.base_url.strip_suffix("/v1")
        {
            limit = self.loaded_limit(root).await;
        }

        *self.context_limit.lock() = limit;
    }

    /// Asks ollama what context length the model is actually being served with.
    ///
    /// note: Its `/api/show` advertises the architecture's maximum - 131,072 for a llama that is
    /// in fact loaded with a `num_ctx` of 4,096 - and a budget measured against that number would
    /// be wrong in the one direction that matters. A compactor would never fire, nothing would
    /// ever look full, and the server would quietly drop the front of the conversation instead.
    /// `/api/ps` reports what a loaded model is really serving, and a model that is not loaded
    /// yields nothing at all, because "unknown" is a better answer than a number that is wrong.
    async fn loaded_limit(&self, root: &str) -> Option<usize> {
        if let Some(limit) = self.running_limit(root).await {
            return Some(limit);
        }

        // only a *loaded* model reports one, and this one is cold. Asking for it with an empty
        // prompt loads it and generates nothing, which is a side effect worth having: it is the
        // model the examples are about to talk to anyway, and the alternative is a budget with
        // no denominator
        self.client
            .post(format!("{root}/api/generate"))
            .json(&json!({ "model": *self.model.lock(), "prompt": "" }))
            .send()
            .await
            .ok()?;

        self.running_limit(root).await
    }

    /// The context length ollama has a model loaded with, if it has it loaded at all.
    async fn running_limit(&self, root: &str) -> Option<usize> {
        let model = self.model.lock().clone();
        let body = self
            .client
            .get(format!("{root}/api/ps"))
            .send()
            .await
            .ok()?
            .json::<Value>()
            .await
            .ok()?;

        body["models"]
            .as_array()?
            .iter()
            .find(|entry| same_model(entry["name"].as_str().unwrap_or_default(), &model))
            .and_then(|entry| entry["context_length"].as_u64())
            .map(|limit| limit as usize)
    }

    /// Looks the model up in a listing and returns whatever context limit it advertises.
    async fn listed_limit(&self, url: &str, bearer: bool) -> Option<usize> {
        let model = self.model.lock().clone();
        let request = match bearer {
            true => self.client.get(url).bearer_auth(&self.api_key),
            false => self.client.get(format!("{url}?key={}", self.api_key)),
        };
        let body = request.send().await.ok()?.json::<Value>().await.ok()?;

        // `data` is the OpenAI shape, `models` the native Google one
        let entries = body["data"]
            .as_array()
            .or_else(|| body["models"].as_array())?;
        let entry = entries.iter().find(|entry| {
            let listed = entry["id"]
                .as_str()
                .or_else(|| entry["name"].as_str())
                .unwrap_or_default();

            same_model(listed, &model)
        })?;

        entry["context_length"]
            .as_u64()
            .or_else(|| entry["top_provider"]["context_length"].as_u64())
            .or_else(|| entry["inputTokenLimit"].as_u64())
            .map(|limit| limit as usize)
    }
}

#[async_trait]
impl Provider for OpenAiCompatible {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: *self.context_limit.lock(),
            tool_calling: true,
            reasoning: true,
            ..ModelInfo::new("openai-compatible", self.model.lock().clone())
        }
    }

    /// The payload, rendered once. `respond` sends exactly this, so previewing it is not a second
    /// opinion about what goes out - it is the thing that goes out.
    fn render(&self, request: &ModelRequest) -> Option<Value> {
        let mut body = json!({
            "model": *self.model.lock(),
            "messages": request.messages.iter().map(to_wire).collect::<Vec<_>>(),
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        if !request.tools.is_empty() {
            body["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|spec| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": spec.id,
                                "description": spec.description,
                                "parameters": spec.schema,
                            },
                        })
                    })
                    .collect(),
            );
        }
        // only what the user set, and nothing else: the kernel invents no parameters, and
        // neither does this provider
        for (key, value) in &request.params {
            body[key] = value.clone();
        }

        Some(body)
    }

    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        let body = self.render(&request).expect("this provider always renders");

        // a free tier answers "busy" often enough that not retrying makes the examples look
        // broken when they are not. It is worth being clear about where this belongs: waiting
        // and trying again is the *provider's* business, because the kernel must not silently
        // send a request twice behind a caller's back
        let mut response = loop {
            let response = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await?;

            let status = response.status();
            if status.is_success() {
                break response;
            }

            let transient = status.as_u16() == 429 || status.is_server_error();
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if !transient || attempt >= RETRIES {
                let body = response.text().await.unwrap_or_default();
                return Err(format!("{status}: {body}").into());
            }

            // on stderr, so that it never lands in output somebody is piping somewhere
            let wait = Duration::from_secs(1 << attempt);
            eprintln!(
                "\x1b[2m  {} answered {}; trying again in {}s\x1b[0m",
                self.model.lock(),
                status.as_u16(),
                wait.as_secs()
            );
            tokio::time::sleep(wait).await;
        };

        let mut buffer = String::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls: Vec<PartialCall> = Vec::new();
        let mut finish = None;
        let mut usage = None;
        // every payload the server sent, verbatim
        let mut chunks = Vec::new();

        while let Some(bytes) = response.chunk().await? {
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(end) = buffer.find('\n') {
                let line = buffer[..end].trim().to_owned();
                buffer.drain(..=end);

                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                // these APIs report an upstream failure - a rate limit, a dead provider - as an
                // error object rather than an HTTP status, sometimes mid-stream
                if let Some(error) = chunk.get("error").filter(|e| !e.is_null()) {
                    return Err(format!("{error}").into());
                }

                if let Some(reported) = chunk.get("usage").filter(|u| !u.is_null()) {
                    usage = Some(Usage {
                        input_tokens: reported["prompt_tokens"].as_u64(),
                        output_tokens: reported["completion_tokens"].as_u64(),
                        reasoning_tokens: reported["completion_tokens_details"]["reasoning_tokens"]
                            .as_u64(),
                        cached_input_tokens: reported["prompt_tokens_details"]["cached_tokens"]
                            .as_u64(),
                    });
                }

                // somebody asked to stop. The bytes still on the socket are abandoned, and
                // what has arrived is handed back as an ordinary answer - a short one
                if deltas.is_interrupted() {
                    finish = Some("interrupted".to_owned());
                    break;
                }

                let choice = &chunk["choices"][0];
                if let Some(reason) = choice["finish_reason"].as_str() {
                    finish = Some(reason.to_owned());
                }

                let delta = &choice["delta"];
                if let Some(fragment) = delta["content"].as_str().filter(|f| !f.is_empty()) {
                    if self.echo {
                        print!("{fragment}");
                        let _ = std::io::stdout().flush();
                    }
                    deltas.text(fragment);
                    text.push_str(fragment);
                }
                if let Some(fragment) = delta["reasoning"].as_str().filter(|f| !f.is_empty()) {
                    if self.echo {
                        print!("\x1b[2m{fragment}\x1b[0m");
                        let _ = std::io::stdout().flush();
                    }
                    deltas.reasoning(fragment);
                    reasoning.push_str(fragment);
                }

                for requested in delta["tool_calls"].as_array().into_iter().flatten() {
                    let index = requested["index"].as_u64().unwrap_or(0) as usize;
                    while calls.len() <= index {
                        calls.push(PartialCall::default());
                    }
                    let call = &mut calls[index];

                    if let Some(id) = requested["id"].as_str() {
                        call.id = id.to_owned();
                    }
                    if let Some(name) = requested["function"]["name"].as_str() {
                        call.name.push_str(name);
                    }
                    if !requested["extra_content"].is_null() {
                        call.extra = requested["extra_content"].clone();
                    }
                    if let Some(fragment) = requested["function"]["arguments"].as_str() {
                        call.args.push_str(fragment);
                        deltas.tool_args(ToolCallId(call.id.clone()), fragment);
                    }
                }

                chunks.push(chunk);
            }
            if finish.as_deref() == Some("interrupted") {
                break;
            }
        }
        if self.echo && (!text.is_empty() || !reasoning.is_empty()) {
            println!();
        }
        if chunks.is_empty() {
            // the response was not a stream at all; an error body is the usual reason
            let payload: Value = serde_json::from_str(&buffer).unwrap_or(Value::Null);
            return match payload.get("error").filter(|e| !e.is_null()) {
                Some(error) => Err(format!("{error}").into()),
                None => Err(format!("the stream carried no data: {buffer}").into()),
            };
        }

        Ok(ModelResponse {
            content: (!text.is_empty()).then_some(Content::text(text)),
            reasoning: (!reasoning.is_empty()).then_some(Content::text(reasoning)),
            tool_calls: calls
                .into_iter()
                .map(|call| {
                    // a model that produces invalid JSON gets to see that it did
                    let args: Value = serde_json::from_str(&call.args)
                        .unwrap_or_else(|_| json!({ "_unparsed": call.args }));

                    // an empty or repeated identifier is repaired by the kernel, which says so
                    // on the event stream; a provider does not have to paper over it
                    ToolCall::new(call.id, call.name, args).with_extra(call.extra)
                })
                .collect(),
            stop: match finish.as_deref() {
                Some("stop") => StopReason::EndTurn,
                Some("interrupted") => StopReason::Other("interrupted".to_owned()),
                Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
                Some("length") => StopReason::Length,
                Some("content_filter") => StopReason::Refusal,
                Some(other) => StopReason::Other(other.to_owned()),
                None => StopReason::Other("unreported".to_owned()),
            },
            usage,
            raw: Some(json!({ "stream": chunks })),
        })
    }
}

/// Whether a listed identifier names the model being asked about, allowing for the decorations
/// listings put on them: Google's `models/` prefix, ollama's implicit `:latest` tag.
fn same_model(listed: &str, model: &str) -> bool {
    listed == model
        || listed.strip_prefix("models/") == Some(model)
        || listed.strip_suffix(":latest") == Some(model)
}

// -------------------------------------------------------------------------------- the trimmings

/// The API key, under whichever of the documented names it is set.
pub fn api_key() -> Result<String, BoxError> {
    env::var("OPENROUTER_API_KEY")
        .or_else(|_| env::var("NACHALNIK_API_KEY"))
        .or_else(|_| env::var("OPENAI_API_KEY"))
        .map_err(|_| "set OPENROUTER_API_KEY (or NACHALNIK_API_KEY / OPENAI_API_KEY)".into())
}

/// The endpoint to talk to; OpenRouter unless told otherwise.
pub fn base_url() -> String {
    env::var("NACHALNIK_BASE_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_owned())
}

/// The models to use, from repeated `-m` flags or from `NACHALNIK_MODELS`.
pub fn models(flags: Vec<String>) -> Vec<String> {
    if !flags.is_empty() {
        return flags;
    }

    env::var("NACHALNIK_MODELS")
        .unwrap_or_default()
        .split(',')
        .map(|model| model.trim().to_owned())
        .filter(|model| !model.is_empty())
        .collect()
}

/// Builds one kernel-ready provider per model, sharing a connection pool, and asks each what its
/// context limit is.
pub async fn providers(models: &[String]) -> Result<Vec<Arc<OpenAiCompatible>>, BoxError> {
    let client = reqwest::Client::new();

    let mut providers = Vec::with_capacity(models.len());
    for model in models {
        let provider = Arc::new(OpenAiCompatible::new(client.clone(), model.clone())?);
        provider.probe().await;
        providers.push(provider);
    }

    Ok(providers)
}

/// Formats a number with `,` as the thousands separator.
pub fn thousands(n: impl TryInto<u64>) -> String {
    let digits = n.try_into().unwrap_or(0).to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }

    out
}

/// Wraps text to a width, indenting every line.
pub fn wrap(text: &str, width: usize, indent: &str) -> String {
    let mut out = String::new();

    for paragraph in text.split('\n') {
        let mut column = 0;
        for word in paragraph.split_whitespace() {
            if column == 0 {
                out.push_str(indent);
            } else if column + 1 + word.chars().count() > width {
                out.push('\n');
                out.push_str(indent);
                column = 0;
            } else {
                out.push(' ');
                column += 1;
            }
            out.push_str(word);
            column += word.chars().count();
        }
        out.push('\n');
    }

    out.trim_end().to_owned()
}
