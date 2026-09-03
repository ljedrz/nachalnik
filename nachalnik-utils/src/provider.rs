//! A [`Provider`] that speaks the OpenAI chat-completions dialect over HTTP.
//!
//! note: The union of what the examples needed and what the live tests needed, which had grown
//! apart: the examples had ollama's `/api/ps` probing and printed fragments as they arrived; the
//! tests had `Retry-After` handling, quota detection, a non-streamed path and a record of every
//! request they made. Each of those is useful to the other, so this has all of them, and each is
//! off or empty unless asked for.

use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};

use nachalnik::{
    BoxError, Content, DeltaSink, Message, ModelInfo, ModelRequest, ModelResponse, Provider,
    StopReason, ToolCall, ToolCallId, Usage, async_trait,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// How many HTTP attempts one request gets before it gives up.
const ATTEMPTS: u32 = 5;

/// How long a stream may say nothing before the provider looks up to check whether it has been
/// asked to stop.
const HEARTBEAT: Duration = Duration::from_millis(120);

/// Any server that speaks the OpenAI chat-completions dialect.
pub struct OpenAiCompatible {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    label: String,
    model: Mutex<String>,
    context_limit: Mutex<Option<usize>>,
    /// Whether to ask for a streamed answer. `params` can still override it per request.
    stream: bool,
    /// Every request this was asked to send, in order, for a test that wants to assert on what
    /// actually went out rather than on what it thinks went out.
    requests: Mutex<Vec<ModelRequest>>,
    /// How many HTTP attempts it has made, retries included.
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

impl OpenAiCompatible {
    /// Builds a provider for one model.
    ///
    /// note: The client is passed in rather than made here, so that several models on the same
    /// host share one connection pool.
    pub fn new(
        client: reqwest::Client,
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into(),
            api_key: api_key.into(),
            label: "openai-compatible".to_owned(),
            model: Mutex::new(model.into()),
            context_limit: Mutex::new(crate::context_limit()),
            stream: true,
            requests: Mutex::new(Vec::new()),
            attempts: AtomicUsize::new(0),
        }
    }

    /// The same, reading the endpoint and the key from the environment.
    pub fn from_env(client: reqwest::Client, model: impl Into<String>) -> Result<Self, BoxError> {
        Ok(Self::new(
            client,
            model,
            crate::base_url(),
            crate::api_key()?,
        ))
    }

    /// A client with the timeout a real API wants, which is longer than reqwest's default.
    ///
    /// note: this is also where the cryptography `rustls` will use is installed. reqwest is built
    /// with `rustls-no-provider`, so there is no default waiting behind it - a client built
    /// without one fails at the first `https://` with "no process-level CryptoProvider
    /// available". Every client in here comes from this function, so this is the one place it has
    /// to happen; the second call loses the race and says so, which is not an error.
    pub fn client() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .expect("a default client is buildable")
    }

    /// Names the provider, as [`ModelInfo::provider`] will report it.
    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Whether to ask for a streamed answer at all.
    #[must_use]
    pub fn streaming(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Switches models, and forgets the context limit that belonged to the old one.
    pub async fn set_model(&self, model: impl Into<String>) {
        *self.model.lock() = model.into();
        *self.context_limit.lock() = None;
        self.probe().await;
    }

    /// Which model it is set to talk to.
    pub fn model(&self) -> String {
        self.model.lock().clone()
    }

    /// Every request this has been asked to send, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().clone()
    }

    /// How many HTTP attempts it has made, retries included.
    pub fn attempts(&self) -> usize {
        self.attempts.load(SeqCst)
    }

    /// Asks the provider what it knows about the model, so that the context limit the kernel
    /// reports is the real one rather than a guess.
    ///
    /// note: Worth the round trip, because everything the kernel says about how full a context is
    /// is measured against this number. An unknown limit is reported as unknown rather than
    /// guessed at, which is the honest answer but not a useful one.
    ///
    /// note: Two shapes, because two real APIs. OpenRouter lists `context_length` beside an `id`;
    /// Google's OpenAI-compatible listing carries no limit at all, so the number has to come from
    /// its native one - which calls the field `inputTokenLimit`, prefixes the identifier with
    /// `models/`, and wants the key in the query string rather than a header.
    pub async fn probe(&self) {
        if self.context_limit.lock().is_some() {
            return;
        }

        let mut limit = self
            .listed_limit(&format!("{}/models", self.base_url), true)
            .await;

        if limit.is_none()
            && let Some(native) = self.base_url.strip_suffix("/openai")
        {
            limit = self.listed_limit(&format!("{native}/models"), false).await;
        }
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
    /// be wrong in the one direction that matters. Nothing would ever look full, no compactor
    /// would fire, and the server would quietly drop the front of the conversation instead.
    /// `/api/ps` reports what a loaded model is really serving, and a model that is not loaded
    /// yields nothing at all, because "unknown" is a better answer than a number that is wrong.
    async fn loaded_limit(&self, root: &str) -> Option<usize> {
        if let Some(limit) = self.running_limit(root).await {
            return Some(limit);
        }

        // only a *loaded* model reports one, and this one is cold. Asking for it with an empty
        // prompt loads it and generates nothing, which is a side effect worth having: it is the
        // model this process is about to talk to anyway
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

    /// Parses a whole (non-streamed) answer.
    fn parse(&self, body: &Value) -> ModelResponse {
        let choice = &body["choices"][0];
        let message = &choice["message"];

        ModelResponse {
            content: message["content"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(Content::text),
            reasoning: message["reasoning"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(Content::text),
            tool_calls: message["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|call| {
                    let args: Value = serde_json::from_str(
                        call["function"]["arguments"].as_str().unwrap_or("{}"),
                    )
                    .unwrap_or_else(|_| json!({}));

                    ToolCall::new(
                        call["id"].as_str().unwrap_or_default(),
                        call["function"]["name"].as_str().unwrap_or_default(),
                        args,
                    )
                    .with_extra(call["extra_content"].clone())
                })
                .collect(),
            stop: stop_reason(choice["finish_reason"].as_str()),
            usage: body.get("usage").filter(|u| !u.is_null()).map(usage_of),
            raw: Some(body.clone()),
        }
    }

    /// Reads a server-sent-event stream into one answer, stopping early if asked to.
    async fn parse_stream(
        &self,
        mut response: reqwest::Response,
        deltas: &DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        let mut buffer = String::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls: Vec<PartialCall> = Vec::new();
        let mut finish = None;
        let mut usage = None;
        // every payload the server sent, verbatim
        let mut chunks = Vec::new();

        loop {
            // the timeout is what makes a model that says nothing at all interruptible; without
            // it this sits in `chunk` until the server feels like talking, and a request that
            // stalls before its first byte cannot be stopped at all
            let bytes = match tokio::time::timeout(HEARTBEAT, response.chunk()).await {
                Ok(Ok(Some(bytes))) => bytes,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => {
                    if deltas.is_interrupted() {
                        finish = Some("interrupted".to_owned());
                        break;
                    }
                    continue;
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(end) = buffer.find('\n') {
                // somebody asked to stop. Whatever has been parsed is kept and the rest of the
                // socket is abandoned; the check is here, before the next fragment, so that a
                // fragment is never read and then thrown away
                if deltas.is_interrupted() {
                    finish = Some("interrupted".to_owned());
                    break;
                }

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
                    usage = Some(usage_of(reported));
                }

                let choice = &chunk["choices"][0];
                if let Some(reason) = choice["finish_reason"].as_str() {
                    finish = Some(reason.to_owned());
                }

                let delta = &choice["delta"];
                if let Some(fragment) = delta["content"].as_str().filter(|f| !f.is_empty()) {
                    deltas.text(fragment);
                    text.push_str(fragment);
                }
                if let Some(fragment) = delta["reasoning"].as_str().filter(|f| !f.is_empty()) {
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
        if chunks.is_empty() {
            // a request stopped before the server had said anything is not a broken response,
            // and reporting it as one would be an error message for doing as it was told
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

                    // an empty or repeated identifier is repaired by the kernel, which says so on
                    // the event stream; a provider does not have to paper over it
                    ToolCall::new(call.id, call.name, args).with_extra(call.extra)
                })
                .collect(),
            stop: stop_reason(finish.as_deref()),
            usage,
            raw: Some(json!({ "stream": chunks })),
        })
    }
}

#[async_trait]
impl Provider for OpenAiCompatible {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: *self.context_limit.lock(),
            tool_calling: true,
            reasoning: true,
            ..ModelInfo::new(self.label.clone(), self.model.lock().clone())
        }
    }

    /// The payload, rendered once. `respond` sends exactly this, so previewing it is not a second
    /// opinion about what goes out - it is the thing that goes out.
    fn render(&self, request: &ModelRequest) -> Option<Value> {
        let mut body = json!({
            "model": *self.model.lock(),
            "messages": request.messages.iter().map(to_wire).collect::<Vec<_>>(),
        });
        if self.stream {
            body["stream"] = json!(true);
            body["stream_options"] = json!({ "include_usage": true });
        }

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
        // only what the user set, and nothing else: the kernel invents no parameters, and neither
        // does this provider. Last, so that `stream` is one of the things it can decide
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
        self.requests.lock().push(request.clone());
        // exactly what `render` shows, because it is the same call
        let body = self
            .render(&request)
            .expect("this provider always renders something");
        let streaming = body["stream"] == json!(true);

        // a free tier answers "busy" often enough that not retrying makes the whole thing look
        // broken when it is not. Waiting and trying again is the *provider's* business: the
        // kernel must not silently send a request twice behind a caller's back
        let mut last = String::new();
        for attempt in 0..ATTEMPTS {
            self.attempts.fetch_add(1, SeqCst);

            let response = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await?;

            let status = response.status();
            if streaming && status.is_success() {
                return self.parse_stream(response, &deltas).await;
            }

            let headers = response.headers().clone();
            let text = response.text().await?;
            let payload: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let error = body_error(&payload);

            // an upstream rate limit arrives either as a 429 or as an error inside a fine 200.
            // Waiting out an upstream limit is worth it; waiting out a daily one is not
            let code = |wanted: u64| {
                u64::from(status.as_u16()) == wanted
                    || error.as_ref().is_some_and(|(code, _)| *code == wanted)
            };
            let limited = code(429) && !out_of_quota(&text);
            // and a model that is simply busy is not what anybody is here for; it says so as a
            // 503, which is as transient as a rate limit and just as uninteresting
            let overloaded = code(503) || status.is_server_error();

            if (limited || overloaded) && attempt + 1 < ATTEMPTS {
                let wait = retry_after(&headers, &payload)
                    .unwrap_or_else(|| Duration::from_secs(2u64.pow(attempt)));
                let why = match limited {
                    true => "rate limited",
                    false => "busy",
                };
                // on stderr, so that it never lands in output somebody is piping somewhere
                eprintln!(
                    "  {} is {why}; waiting {}s",
                    self.model.lock(),
                    wait.as_secs()
                );
                last = text;
                tokio::time::sleep(wait).await;
                continue;
            }

            return match error {
                Some((code, error)) => Err(format!("{code}: {error}").into()),
                None if !status.is_success() => Err(format!("{status}: {text}").into()),
                None => Ok(self.parse(&payload)),
            };
        }

        Err(format!("gave up after {ATTEMPTS} attempts: {last}").into())
    }
}

// ------------------------------------------------------------------------------- the wire format

/// Translates a kernel message into the wire format.
fn to_wire(message: &Message) -> Value {
    let mut wire = json!({ "role": message.role.as_str() });

    if let Some(content) = &message.content {
        wire["content"] = json!(content.to_text());
    }
    // `calls()` rather than the field: a turn projected as ordered blocks keeps its calls in
    // its content, and reading the field would send the words of a turn with none of the calls
    // in it - which this API rejects, and which is very hard to see afterwards
    let calls: Vec<_> = message.calls().collect();
    if !calls.is_empty() {
        wire["tool_calls"] = Value::Array(
            calls
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

/// What the provider said a request cost.
///
/// note: `reasoning_tokens` is read from `completion_tokens_details` where a server sends it, and
/// otherwise *inferred* from the residual: `total_tokens` minus the prompt and completion counts.
/// Google's OpenAI-compatible endpoint omits the details object altogether and reports a total
/// that is larger than its parts - measured, 19 in and 5 out against a total of 151 - so the
/// thinking a reasoning model is billed for exists in that dialect only as the difference. Read
/// literally, its usage says a request cost 24 tokens when it cost 151, and anything reporting
/// what a run cost would be out by a factor of twenty.
///
/// note: it is an inference, and it is a safe one in exactly one direction. A server whose total
/// is the sum of its parts yields zero and changes nothing, which is every non-reasoning
/// dialect; a negative residual is discarded rather than trusted. What it must not be read as is
/// a *reported* figure - it is this crate's arithmetic about somebody else's numbers, which is
/// why it says so here.
fn usage_of(reported: &Value) -> Usage {
    let input = reported["prompt_tokens"].as_u64();
    let output = reported["completion_tokens"].as_u64();
    let residual = || {
        let total = reported["total_tokens"].as_u64()?;
        total.checked_sub(input.unwrap_or_default() + output.unwrap_or_default())
    };

    Usage {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: reported["completion_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .or_else(residual)
            .filter(|tokens| *tokens > 0),
        cached_input_tokens: reported["prompt_tokens_details"]["cached_tokens"].as_u64(),
    }
}

/// Why the model stopped, in the kernel's vocabulary.
fn stop_reason(finish: Option<&str>) -> StopReason {
    match finish {
        Some("stop") => StopReason::EndTurn,
        Some("interrupted") => StopReason::Other("interrupted".to_owned()),
        Some("tool_calls" | "function_call") => StopReason::ToolUse,
        Some("length") => StopReason::Length,
        Some("content_filter") => StopReason::Refusal,
        Some(other) => StopReason::Other(other.to_owned()),
        None => StopReason::Other("unreported".to_owned()),
    }
}

/// Whether a listed identifier names the model being asked about, allowing for the decorations
/// listings put on them: Google's `models/` prefix, ollama's implicit `:latest` tag.
fn same_model(listed: &str, model: &str) -> bool {
    let listed = listed.trim_start_matches("models/");

    listed == model || listed.trim_end_matches(":latest") == model.trim_end_matches(":latest")
}

/// Whether an error means the account is out of free requests for the day, rather than having hit
/// a momentary upstream limit. The first is worth skipping over; the second is worth waiting out.
pub fn out_of_quota(error: &str) -> bool {
    error.contains("per-day") || error.contains("daily")
}

/// How long the server asked us to wait, from the header or from the error it embedded.
fn retry_after(headers: &reqwest::header::HeaderMap, body: &Value) -> Option<Duration> {
    let from_header = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let from_body = body["error"]["metadata"]["retry_after_seconds"].as_u64();

    from_header
        .or(from_body)
        // long enough to be worth honouring, short enough not to hang a test run
        .map(|seconds| Duration::from_secs(seconds.clamp(1, 90)))
}

/// Turns the `error` object these APIs like to return *inside a 200* into a real error.
fn body_error(body: &Value) -> Option<(u64, String)> {
    let error = body.get("error").filter(|e| !e.is_null())?;
    let code = error["code"].as_u64().unwrap_or(0);

    Some((code, error.to_string()))
}

/// One provider per model, sharing a connection pool, each asked what its context limit is.
pub async fn providers(models: &[String]) -> Result<Vec<Arc<OpenAiCompatible>>, BoxError> {
    let client = OpenAiCompatible::client();

    let mut built = Vec::new();
    for model in models {
        let provider = OpenAiCompatible::from_env(client.clone(), model)?;
        provider.probe().await;
        built.push(Arc::new(provider));
    }

    Ok(built)
}

/// The models to use, from repeated flags or from `NACHALNIK_MODELS`.
pub fn models(flags: Vec<String>) -> Vec<String> {
    if !flags.is_empty() {
        return flags;
    }

    env::var("NACHALNIK_MODELS")
        .map(|listed| {
            listed
                .split(',')
                .map(|model| model.trim().to_owned())
                .filter(|model| !model.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
