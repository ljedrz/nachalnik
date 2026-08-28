//! Tests against a real model, over the real wire.
//!
//! They are skipped unless an API key is in the environment, the same variable the `agent`
//! example reads:
//!
//! ```text
//! OPENROUTER_API_KEY=sk-or-... cargo test --test live -- --nocapture
//! ```
//!
//! ```text
//! NACHALNIK_TEST_MODEL=liquid/lfm-2.5-2.6b:free   # the default; anything with tool support
//! NACHALNIK_TEST_MODEL_B=...                      # a second model, for the swap test
//! NACHALNIK_BASE_URL=...                          # any OpenAI-compatible server
//! ```
//!
//! Google AI Studio speaks the same dialect, and has a free tier of its own:
//!
//! ```text
//! NACHALNIK_API_KEY=... \
//! NACHALNIK_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai \
//! NACHALNIK_TEST_MODEL=gemini-3.5-flash-lite \
//! NACHALNIK_TEST_MODEL_B=gemini-3.5-flash \
//!   cargo test --test live -- --test-threads=1 --nocapture
//! ```
//!
//! A local server works as well, and costs nothing:
//!
//! ```text
//! NACHALNIK_API_KEY=ollama \
//! NACHALNIK_BASE_URL=http://localhost:11434/v1 \
//! NACHALNIK_TEST_MODEL=granite4.2:3b \
//! NACHALNIK_TEST_MODEL_B=llama3.2 \
//!   cargo test --test live -- --test-threads=1
//! ```
//!
//! note: These tests need a model that can read its own tool results, which is a real bar and
//! not every small model clears it. Measured on the same machine, `granite4.2:3b` passes all
//! eighteen; `llama3.2` passes fifteen and fails three, because it calls the tool, is handed the
//! answer in the request, and then reports a different one it made up. That is a fair result for
//! a test suite whose subject is whether a real model's answers survive the round trip - the
//! failure is in the model, and the way to tell is that the projected messages in the panic
//! output contain the tool result the model claims it never saw.
//!
//! note: What these check is what a scripted provider cannot: that the requests the kernel
//! builds are accepted by a real API, that a real model's answers survive the round trip
//! through the context, and that the loop still works when a tool result is pruned, truncated
//! or compacted out from under it. They are deliberately assertion-light about *prose* and
//! assertion-heavy about structure.
//!
//! note: They run one at a time and retry on rate limits and on a busy upstream, so that neither
//! a free-tier limit nor somebody else's traffic is ever the thing under test. A whole run costs about twenty requests, and a key that has run out of
//! free requests for the day makes them skip rather than fail - the difference between the two
//! kinds of rate limit is in [`out_of_quota`].

use std::{
    env,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};

use nachalnik::{
    BoxError, BytesPerToken, Calibrating, Capability, Config, Content, ContextItem, ContextKind,
    ContextState, Delta, DeltaSink, Event, Grant, Kernel, Message, ModelInfo, ModelRequest,
    ModelResponse, OutputSink, Params, Provider, Record, Role, State, StopReason, Tool, ToolCall,
    ToolCallId, ToolOutput, ToolSpec, async_trait,
    selectors::Selector,
    test::{AllowAll, DenyAll, LargestFirstCompactor},
};
use serde_json::{Value, json};
use tokio::sync::broadcast::Receiver;

/// A small, free, tool-capable model.
const DEFAULT_MODEL: &str = "liquid/lfm-2.5-2.6b:free";

/// Whether a listed identifier names the model under test, allowing for the decorations listings
/// put on them: Google's `models/` prefix, ollama's implicit `:latest` tag.
fn names_model(listed: &str, model: &str) -> bool {
    listed == model
        || listed.strip_prefix("models/") == Some(model)
        || listed.strip_suffix(":latest") == Some(model)
}

/// Live tests take turns, so that the free tier's rate limit is not what is being measured.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn serialize() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

// ------------------------------------------------------------------------------- the provider

/// An OpenAI-compatible provider that remembers what it was asked and what it was told.
struct Live {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    info: ModelInfo,
    requests: Mutex<Vec<ModelRequest>>,
    attempts: AtomicUsize,
}

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
                    // whatever the provider attached to this call goes straight back
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

fn usage_of(reported: &Value) -> nachalnik::Usage {
    nachalnik::Usage {
        input_tokens: reported["prompt_tokens"].as_u64(),
        output_tokens: reported["completion_tokens"].as_u64(),
        reasoning_tokens: reported["completion_tokens_details"]["reasoning_tokens"].as_u64(),
        cached_input_tokens: reported["prompt_tokens_details"]["cached_tokens"].as_u64(),
    }
}

/// Whether an error means the account is out of free requests for the day, rather than having
/// hit a momentary upstream limit. The first is worth skipping over; the second is worth waiting
/// out.
fn out_of_quota(error: &str) -> bool {
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

impl Live {
    fn new(model: &str) -> Option<Arc<Self>> {
        // note: deliberately not `OPENAI_API_KEY`, which plenty of people have exported for
        // other reasons; spending someone's credits as a side effect of `cargo test` would be a
        // poor way to demonstrate a crate about not doing things behind the user's back
        let api_key = env::var("OPENROUTER_API_KEY")
            .or_else(|_| env::var("NACHALNIK_API_KEY"))
            .ok()?;

        Some(Arc::new(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .expect("a client"),
            base_url: env::var("NACHALNIK_BASE_URL")
                .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_owned()),
            api_key,
            info: ModelInfo {
                context_limit: None,
                tool_calling: true,
                reasoning: true,
                ..ModelInfo::new("openrouter", model)
            },
            requests: Mutex::new(Vec::new()),
            attempts: AtomicUsize::new(0),
        }))
    }

    /// Fills in the context limit from the provider's own model listing.
    ///
    /// note: Two shapes, because two real APIs. OpenRouter lists `context_length` beside an
    /// `id`; Google's OpenAI-compatible listing carries no limit at all, so the number has to
    /// come from its native one - which calls the field `inputTokenLimit`, prefixes the
    /// identifier with `models/`, and wants the key in the query string rather than a header.
    async fn probe(self: &Arc<Self>) -> Arc<Self> {
        let mut provider = Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            info: self.info.clone(),
            requests: Mutex::new(Vec::new()),
            attempts: AtomicUsize::new(0),
        };

        let mut limit = self
            .listed_limit(&format!("{}/models", self.base_url), true)
            .await;
        // Google's OpenAI-compatible listing carries no context length; its native one does
        if limit.is_none()
            && let Some(native) = self.base_url.strip_suffix("/openai")
        {
            limit = self.listed_limit(&format!("{native}/models"), false).await;
        }
        // ollama's carries none either, and what it advertises elsewhere is the architecture's
        // maximum rather than the `num_ctx` it is actually serving
        if limit.is_none()
            && let Some(root) = self.base_url.strip_suffix("/v1")
        {
            limit = self.loaded_limit(root).await;
        }
        provider.info.context_limit = limit;

        Arc::new(provider)
    }

    /// Asks ollama what context length the model is actually loaded with, loading it first if it
    /// has to. Its listings advertise the architecture's maximum, which is not what it will
    /// serve, and a limit that is wrong in that direction is worse than none.
    async fn loaded_limit(&self, root: &str) -> Option<usize> {
        for attempt in 0..2 {
            if attempt == 1 {
                // an empty prompt loads the model and generates nothing; only a loaded model
                // reports the context it was given
                self.client
                    .post(format!("{root}/api/generate"))
                    .json(&json!({ "model": &self.info.model, "prompt": "" }))
                    .send()
                    .await
                    .ok()?;
            }

            let body = self
                .client
                .get(format!("{root}/api/ps"))
                .send()
                .await
                .ok()?
                .json::<Value>()
                .await
                .ok()?;
            let found = body["models"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| {
                    names_model(entry["name"].as_str().unwrap_or_default(), &self.info.model)
                })
                .and_then(|entry| entry["context_length"].as_u64());

            if let Some(limit) = found {
                return Some(limit as usize);
            }
        }

        None
    }

    /// Looks the model up in a listing and returns whatever context limit it advertises.
    async fn listed_limit(&self, url: &str, bearer: bool) -> Option<usize> {
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

            names_model(listed, &self.info.model)
        })?;

        entry["context_length"]
            .as_u64()
            .or_else(|| entry["top_provider"]["context_length"].as_u64())
            .or_else(|| entry["inputTokenLimit"].as_u64())
            .map(|limit| limit as usize)
    }

    /// The requests this provider was handed, in order.
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// How many HTTP attempts it took, retries included.
    fn attempts(&self) -> usize {
        self.attempts.load(SeqCst)
    }

    /// Renders the request; [`Provider::render`] and [`Provider::respond`] both come here, so
    /// what is previewed is what is sent.
    fn body(&self, request: &ModelRequest) -> Value {
        let mut body = json!({
            "model": self.info.model,
            "messages": request.messages.iter().map(to_wire).collect::<Vec<_>>(),
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
        // whatever the user set, and nothing else
        for (key, value) in &request.params {
            body[key] = value.clone();
        }

        body
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

    /// Parses a streamed answer, reporting fragments as they arrive.
    async fn parse_stream(
        &self,
        mut response: reqwest::Response,
        deltas: &DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        let mut buffer = String::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls: Vec<(String, String, String, Value)> = Vec::new();
        let mut finish = None;
        let mut usage = None;
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
                if let Some((code, error)) = body_error(&chunk) {
                    return Err(format!("{code} mid-stream: {error}").into());
                }

                if let Some(reported) = chunk.get("usage").filter(|u| !u.is_null()) {
                    usage = Some(usage_of(reported));
                }

                // somebody asked to stop; the rest of the stream is abandoned and what has
                // arrived is handed back as a short answer
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
                        calls.push(Default::default());
                    }
                    let call = &mut calls[index];
                    if let Some(id) = requested["id"].as_str() {
                        call.0 = id.to_owned();
                    }
                    if let Some(name) = requested["function"]["name"].as_str() {
                        call.1.push_str(name);
                    }
                    // the provider's own state for this call, which it will want back
                    if !requested["extra_content"].is_null() {
                        call.3 = requested["extra_content"].clone();
                    }
                    if let Some(fragment) = requested["function"]["arguments"].as_str() {
                        call.2.push_str(fragment);
                        deltas.tool_args(ToolCallId(call.0.clone()), fragment);
                    }
                }

                chunks.push(chunk);
            }
            if finish.as_deref() == Some("interrupted") {
                break;
            }
        }

        if chunks.is_empty() {
            // not a stream at all: an error body, most likely
            let payload: Value = serde_json::from_str(&buffer).unwrap_or(Value::Null);
            return match body_error(&payload) {
                Some((code, error)) => Err(format!("{code}: {error}").into()),
                None => Err(format!("the stream carried no data: {buffer}").into()),
            };
        }

        Ok(ModelResponse {
            content: (!text.is_empty()).then_some(Content::text(text)),
            reasoning: (!reasoning.is_empty()).then_some(Content::text(reasoning)),
            tool_calls: calls
                .into_iter()
                .map(|(id, tool, args, extra)| {
                    let args: Value = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));

                    ToolCall::new(id, tool, args).with_extra(extra)
                })
                .collect(),
            stop: stop_reason(finish.as_deref()),
            usage,
            raw: Some(json!({ "stream": chunks })),
        })
    }
}

fn stop_reason(finish: Option<&str>) -> StopReason {
    match finish {
        Some("stop") => StopReason::EndTurn,
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        Some("length") => StopReason::Length,
        Some("content_filter") => StopReason::Refusal,
        Some(other) => StopReason::Other(other.to_owned()),
        None => StopReason::Other("unreported".to_owned()),
    }
}

#[async_trait]
impl Provider for Live {
    fn info(&self) -> ModelInfo {
        self.info.clone()
    }

    fn render(&self, request: &ModelRequest) -> Option<Value> {
        Some(self.body(request))
    }

    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        self.requests.lock().unwrap().push(request.clone());
        // exactly what `render` shows, because it is the same call
        let body = self.render(&request).expect("this provider always renders");
        let streaming = body["stream"] == json!(true);

        const ATTEMPTS: u32 = 5;
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

            // an upstream rate limit arrives either as a 429 or as an error inside a fine 200
            // waiting out an upstream limit is worth it; waiting out a daily one is not
            let code = |wanted: u64| {
                status.as_u16() as u64 == wanted
                    || error.as_ref().is_some_and(|(code, _)| *code == wanted)
            };
            let limited = code(429) && !out_of_quota(&text);
            // and a model that is simply busy is not what any of these tests is about; it says
            // so as a 503, which is as transient as a rate limit and just as uninteresting
            let overloaded = code(503) || status.is_server_error();

            if (limited || overloaded) && attempt + 1 < ATTEMPTS {
                let wait = retry_after(&headers, &payload)
                    .unwrap_or_else(|| Duration::from_secs(2u64.pow(attempt)));
                let why = if limited { "rate limited" } else { "busy" };
                eprintln!("  {why}; waiting {}s", wait.as_secs());
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

// ---------------------------------------------------------------------------------- the tools

/// A tool whose output cannot be guessed by a model, so seeing it in an answer proves the
/// result made the round trip.
struct Secret {
    output: String,
    limit: Option<usize>,
    ran: AtomicUsize,
}

impl Secret {
    fn new(output: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            output: output.into(),
            limit: None,
            ran: AtomicUsize::new(0),
        })
    }

    fn with_limit(output: impl Into<String>, limit: usize) -> Arc<Self> {
        Arc::new(Self {
            output: output.into(),
            limit: Some(limit),
            ran: AtomicUsize::new(0),
        })
    }

    fn ran(&self) -> usize {
        self.ran.load(SeqCst)
    }
}

#[async_trait]
impl Tool for Secret {
    fn spec(&self) -> ToolSpec {
        let spec = ToolSpec::new("secret", "returns today's secret code word")
            .with_capabilities([Capability::Read]);

        match self.limit {
            Some(limit) => spec.with_output_limit(limit),
            None => spec,
        }
    }

    async fn invoke(&self, _call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        self.ran.fetch_add(1, SeqCst);
        output.push("looking it up");

        Ok(ToolOutput::new(self.output.clone()))
    }
}

// -------------------------------------------------------------------------------- the fixtures

/// A kernel wired to a live provider, or `None` when there is no API key to use.
async fn live() -> Option<(Kernel, Arc<Live>)> {
    live_with(Config::default()).await
}

/// The same, for a test that needs the kernel configured differently.
async fn live_with(config: Config) -> Option<(Kernel, Arc<Live>)> {
    let model = env::var("NACHALNIK_TEST_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let provider = Live::new(&model)?.probe().await;

    let kernel = Kernel::new(config);
    kernel.set_provider(provider.clone());
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_params(params(500));

    Some((kernel, provider))
}

/// Runs a turn; skips the test if the key has no free requests left today, and fails on any
/// other provider error.
macro_rules! turn {
    ($kernel:expr) => {
        match $kernel.turn().await {
            Ok(state) => state,
            Err(e) if out_of_quota(&e.to_string()) => {
                eprintln!("skipped: {e}");
                return;
            }
            Err(e) => panic!("{e}"),
        }
    };
}

/// Returns the fixtures, or skips the test.
macro_rules! live {
    () => {
        match live().await {
            Some(fixtures) => fixtures,
            None => {
                eprintln!("skipped: set OPENROUTER_API_KEY to run the live tests");
                return;
            }
        }
    };
}

/// Returns the fixtures with a configured kernel, or skips the test.
macro_rules! live_with {
    ($config:expr) => {
        match live_with($config).await {
            Some(fixtures) => fixtures,
            None => {
                eprintln!("skipped: set OPENROUTER_API_KEY to run the live tests");
                return;
            }
        }
    };
}

/// A reasoning model needs room to think before it says anything at all.
fn params(max_tokens: u64) -> Params {
    let mut params = Params::new();
    params.insert("max_tokens".into(), json!(max_tokens));
    params.insert("temperature".into(), json!(0));

    params
}

fn drain(events: &mut Receiver<Event>) -> Vec<Event> {
    let mut received = Vec::new();
    while let Ok(event) = events.try_recv() {
        received.push(event);
    }

    received
}

/// The text of the most recent answer, lowercased.
fn answer(kernel: &Kernel) -> String {
    kernel
        .last_response()
        .and_then(|response| response.content.clone())
        .map(|content| content.to_text().to_lowercase())
        .unwrap_or_default()
}

fn results(kernel: &Kernel) -> Vec<Arc<ContextItem>> {
    "kind:tool_result"
        .parse::<Selector>()
        .unwrap()
        .matches(&kernel.items())
        .into_iter()
        .filter_map(|id| kernel.item(id))
        .collect()
}

// ----------------------------------------------------------------------------------- the tests

#[tokio::test]
async fn a_turn_reaches_a_real_model_and_comes_back() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    kernel.push(ContextItem::user("Reply with the single word: pong"));

    // the request that is about to be sent, captured before it is
    let previewed = kernel.preview_request().unwrap();
    let state = turn!(kernel);

    let State::Finished { item, stop } = state else {
        panic!("no tools were offered, so there was nothing to decide: {state:?}")
    };

    // what was previewed is exactly what went out
    assert_eq!(provider.requests(), vec![previewed]);
    assert!(provider.attempts() >= 1, "the provider was really called");

    let response = kernel.last_response().unwrap();
    assert_eq!(response.stop, StopReason::EndTurn);
    assert_eq!(
        stop, response.stop,
        "the state carries the model's own reason"
    );
    assert!(!answer(&kernel).is_empty(), "{response:?}");
    assert!(response.raw.is_some(), "the provider's own payload is kept");

    // real numbers, reported by the provider, next to the kernel's own estimate
    let usage = response.usage.expect("usage");
    assert!(usage.input_tokens.unwrap_or(0) > 0, "{usage:?}");
    assert!(usage.output_tokens.unwrap_or(0) > 0, "{usage:?}");
    assert!(kernel.budget().context_tokens > 0);

    // and the answer is in the context, attributed to the model
    let recorded = kernel.item(item).unwrap();
    assert_eq!(recorded.source, "model");
    assert_eq!(recorded.kind.name(), "assistant_message");
    assert_eq!(
        recorded.content.to_text().to_lowercase(),
        answer(&kernel),
        "the recorded turn is the answer, not a summary of it"
    );
}

#[tokio::test]
async fn the_context_limit_is_the_providers_own_number() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    let info = kernel.model_info().unwrap();
    let limit = info
        .context_limit
        .expect("the provider reports a context length");
    assert!(limit >= 4_096, "{limit} is implausibly small");
    assert_eq!(kernel.budget().limit, Some(limit));
}

#[tokio::test]
async fn a_system_instruction_is_obeyed() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    kernel.push(ContextItem::system(
        "You always answer with exactly one lowercase word, and nothing else.",
    ));
    kernel.push(ContextItem::user("What colour is a ripe banana?"));

    turn!(kernel);

    let sent = &provider.requests()[0];
    assert_eq!(sent.messages[0].role, Role::System, "the role is mapped");
    assert_eq!(sent.messages[1].role, Role::User);
    assert!(
        answer(&kernel).contains("yellow"),
        "the instruction and the question both arrived: {:?}",
        answer(&kernel)
    );
}

#[tokio::test]
async fn a_reference_is_labelled_so_the_model_knows_what_it_is_reading() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    kernel.push(ContextItem::file(
        "recipe.txt",
        "The secret ingredient is tarragon.",
    ));
    kernel.push(ContextItem::user(
        "Which file did I give you? Answer with the file name only.",
    ));

    turn!(kernel);

    assert_eq!(
        provider.requests()[0].messages[0]
            .content
            .as_ref()
            .unwrap()
            .to_text(),
        "recipe.txt:\nThe secret ingredient is tarragon.",
    );
    assert!(
        answer(&kernel).contains("recipe"),
        "the label reached the model: {:?}",
        answer(&kernel)
    );
}

#[tokio::test]
async fn streaming_fragments_add_up_to_the_answer() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    let mut params = params(500);
    params.insert("stream".into(), json!(true));
    params.insert("stream_options".into(), json!({ "include_usage": true }));
    kernel.set_params(params);
    kernel.push(ContextItem::user("Count from one to five, in words."));

    let mut events = kernel.subscribe();
    turn!(kernel);

    let mut streamed = String::new();
    let mut reasoned = 0;
    for event in drain(&mut events) {
        match event {
            Event::ModelDelta {
                delta: Delta::Text(fragment),
            } => streamed.push_str(&fragment),
            Event::ModelDelta {
                delta: Delta::Reasoning(_),
            } => reasoned += 1,
            _ => {}
        }
    }

    let response = kernel.last_response().unwrap();
    assert!(!streamed.is_empty(), "fragments arrived as events");
    assert_eq!(
        Some(Content::text(streamed)),
        response.content,
        "the fragments are the answer, in order"
    );
    assert!(response.usage.is_some(), "usage survives a stream");
    eprintln!("  ({reasoned} reasoning fragments)");
}

#[tokio::test]
async fn a_model_calls_a_tool_and_sees_its_result() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    let secret = Secret::new("The secret code word is APRICOT.");
    kernel.add_tool(secret.clone());
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));

    let state = turn!(kernel);
    assert!(
        matches!(state, State::Finished { .. }),
        "the policy allowed everything: {state:?}"
    );

    assert_eq!(secret.ran(), 1, "the tool ran exactly once");
    assert_eq!(provider.requests().len(), 2, "call, then result");

    // the second request carries the assistant's call and the tool's answer
    let second = &provider.requests()[1];
    let assistant = second
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("the model's turn");
    assert_eq!(assistant.tool_calls.len(), 1);
    let result = second
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the tool's result");
    assert_eq!(
        result.tool_call_id,
        Some(assistant.tool_calls[0].id.clone())
    );
    assert_eq!(result.name.as_deref(), Some("secret"));

    assert!(
        answer(&kernel).contains("apricot"),
        "the model read the result: {:?}",
        answer(&kernel)
    );

    // and the exchange is in the context, in order
    let kinds: Vec<_> = kernel.items().iter().map(|i| i.kind.name()).collect();
    assert_eq!(
        kinds,
        [
            "user_message",
            "assistant_message",
            "tool_result",
            "assistant_message"
        ]
    );
}

#[tokio::test]
async fn asking_pauses_the_loop_and_the_answer_resumes_it() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();
    // the default policy asks about everything
    kernel.set_policy(Arc::new(nachalnik::AskAlways));

    let secret = Secret::new("The secret code word is APRICOT.");
    kernel.add_tool(secret.clone());
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));

    let State::Deciding { calls } = turn!(kernel) else {
        panic!("a real model asked for a tool, so a real decision is needed")
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(
        secret.ran(),
        0,
        "nothing ran while it was being asked about"
    );
    assert_eq!(provider.requests().len(), 1);

    let request = &kernel.pending_permissions()[0];
    assert_eq!(request.tool, "secret");
    assert_eq!(request.capabilities, vec![Capability::Read]);

    assert!(matches!(
        kernel.decide(request.id, Grant::Allow).unwrap(),
        State::Ready { .. }
    ));
    assert!(matches!(turn!(kernel), State::Finished { .. }));
    assert_eq!(secret.ran(), 1);
    assert!(answer(&kernel).contains("apricot"), "{:?}", answer(&kernel));
}

#[tokio::test]
async fn a_refused_call_never_runs_and_the_model_carries_on() {
    let _serial = serialize().await;
    let (kernel, _) = live!();
    kernel.set_policy(Arc::new(DenyAll));

    let secret = Secret::new("The secret code word is APRICOT.");
    kernel.add_tool(secret.clone());
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));

    let state = turn!(kernel);

    assert_eq!(
        secret.ran(),
        0,
        "a model cannot talk its way past the policy"
    );
    let refusal = results(&kernel);
    assert_eq!(refusal.len(), 1);
    assert!(
        refusal[0].content.to_text().contains("not permitted"),
        "{:?}",
        refusal[0]
    );
    assert!(
        matches!(
            refusal[0].kind,
            ContextKind::ToolResult { is_error: true, .. }
        ),
        "the model is told, as an error"
    );
    assert!(
        matches!(state, State::Finished { .. }),
        "and it answered anyway: {state:?}"
    );
    assert!(!answer(&kernel).contains("apricot"));
}

#[tokio::test]
async fn pruning_a_tool_exchange_still_produces_a_request_the_api_accepts() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    kernel.add_tool(Secret::new("The secret code word is APRICOT."));
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));
    turn!(kernel);

    // out goes the tool result, leaving the model's call without an answer
    let noisy: Vec<_> = results(&kernel).iter().map(|i| i.id).collect();
    assert_eq!(
        kernel
            .set_state(noisy, ContextState::Excluded, Some("noise".into()))
            .len(),
        1
    );
    let repairs = kernel.project().repairs;
    assert_eq!(repairs.len(), 1, "{repairs:?}");

    kernel.push(ContextItem::user("What did I just ask you about?"));
    let state = turn!(kernel);

    // the API accepted the repaired projection - this is the assertion a mock cannot make
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    let last = provider.requests().pop().unwrap();
    assert!(
        !last.messages.iter().any(|m| m.role == Role::Tool),
        "the pruned result is gone: {:?}",
        last.messages
    );
    assert!(
        last.messages.iter().all(|m| m.tool_calls.is_empty()),
        "and so is the call it answered: {:?}",
        last.messages
    );
    assert!(!answer(&kernel).is_empty());
}

#[tokio::test]
async fn a_truncated_tool_result_is_still_a_valid_request() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    let noisy = format!(
        "{}\nThe secret code word is APRICOT.",
        "noise ".repeat(4_000)
    );
    kernel.add_tool(Secret::with_limit(noisy, 200));
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me what it returned.",
    ));

    let state = turn!(kernel);
    assert!(matches!(state, State::Finished { .. }), "{state:?}");

    // the pair: the whole of what the tool said, archived, and the copy the model was shown
    let recorded = results(&kernel);
    assert_eq!(recorded.len(), 2, "a limit shortens; it does not destroy");

    let (whole, shown) = (&recorded[0], &recorded[1]);
    assert_eq!(whole.state, ContextState::Archived);
    assert!(whole.content.to_text().contains("APRICOT"));
    assert!(whole.content.to_text().len() > 20_000);

    assert!(shown.content.to_text().len() < 400, "it was truncated");
    assert!(shown.content.to_text().contains("bytes truncated"));
    assert!(
        shown
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("truncated"),
        "and the item says so: {:?}",
        shown.note
    );

    // and only the short one crossed the wire, once
    let sent = provider.requests().pop().unwrap();
    let tool_messages: Vec<_> = sent
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .collect();
    assert_eq!(tool_messages.len(), 1, "the archived copy is not sent");
    assert!(tool_messages[0].content.as_ref().unwrap().to_text().len() < 400);
}

#[tokio::test]
async fn compaction_before_a_request_produces_a_request_the_api_accepts() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    // a compactor that always fires, so the real context limit does not have to be reached
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.0,
        target: 0.0,
    })));
    kernel.push(ContextItem::tool_result(
        "call_0".into(),
        "cargo",
        format!("{}\n", "warning: unused variable\n".repeat(200)),
        false,
    ));
    kernel.push(ContextItem::user("Say the word: ready"));

    let mut events = kernel.subscribe();
    let state = turn!(kernel);
    assert!(matches!(state, State::Finished { .. }), "{state:?}");

    let report = drain(&mut events)
        .into_iter()
        .find_map(|event| match event {
            Event::Compacted { report } => Some(report),
            _ => None,
        })
        .expect("it compacted");
    assert_eq!(report.removed.len(), 1);
    assert!(report.summary.is_some());
    assert!(report.tokens_after < report.tokens_before);

    // the request went out without the removed item, and with the summary in its place
    let sent = &provider.requests()[0];
    assert!(
        !sent.messages.iter().any(|m| m
            .content
            .as_ref()
            .unwrap()
            .to_text()
            .contains("unused variable")),
        "{:?}",
        sent.messages
    );
    assert!(!answer(&kernel).is_empty());
}

#[tokio::test]
async fn a_parameter_the_user_set_reaches_the_model() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    // one token is not enough to answer anything, and the server is the one that says so
    kernel.set_params(params(1));
    kernel.push(ContextItem::user("Write a paragraph about tarragon."));

    turn!(kernel);

    assert_eq!(provider.requests()[0].params["max_tokens"], json!(1));
    assert_eq!(
        kernel.last_response().unwrap().stop,
        StopReason::Length,
        "the limit was applied by the provider, not by the kernel"
    );
    assert!(
        matches!(
            kernel.state(),
            State::Finished {
                stop: StopReason::Length,
                ..
            }
        ),
        "and a truncated turn does not look like a clean one: {:?}",
        kernel.state()
    );
}

#[tokio::test]
async fn the_model_can_be_swapped_mid_session() {
    let _serial = serialize().await;
    let (kernel, first) = live!();

    kernel.push(ContextItem::user("Reply with the single word: one"));
    turn!(kernel);
    let before = answer(&kernel);

    // a second model if one was named, otherwise a second provider for the same one
    let model = env::var("NACHALNIK_TEST_MODEL_B")
        .or_else(|_| env::var("NACHALNIK_TEST_MODEL"))
        .unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let second = Live::new(&model).unwrap().probe().await;

    let mut events = kernel.subscribe();
    let previous = kernel.set_provider(second.clone()).unwrap();
    assert_eq!(previous.info(), first.info(), "the old one is handed back");
    assert!(drain(&mut events).iter().any(|event| matches!(
        event,
        Event::ModelChanged { to: Some(info), .. } if info.model == model
    )));

    kernel.push(ContextItem::user("Now reply with the single word: two"));
    turn!(kernel);

    assert_eq!(
        first.requests().len(),
        1,
        "the first one was not used again"
    );
    assert_eq!(second.requests().len(), 1, "the second one was");
    assert!(
        second.requests()[0].messages.len() >= 3,
        "and it saw the earlier turn: {:?}",
        second.requests()[0].messages
    );
    assert!(!answer(&kernel).is_empty());
    assert_ne!(answer(&kernel), before);
}

#[tokio::test]
async fn a_real_session_survives_a_round_trip() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    kernel.add_tool(Secret::new("The secret code word is APRICOT."));
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));
    turn!(kernel);
    kernel.finish();

    let history = kernel.history();
    assert!(history.len() > 12, "{} records", history.len());
    assert_eq!(history[0].event.name(), "session.started");
    assert!(history.windows(2).all(|w| w[0].seq < w[1].seq));

    // the transitions a real tool round makes
    let states: Vec<_> = history
        .iter()
        .filter_map(|record| match &record.event {
            Event::StateChanged { to, .. } => Some(to.name()),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        [
            "requesting",
            "ready",
            "executing",
            "idle",
            "requesting",
            "finished"
        ]
    );

    // and every record of it is exportable
    for record in &history {
        let json = serde_json::to_string(record).unwrap();
        let restored: Record = serde_json::from_str(&json).unwrap();
        assert_eq!(&restored, record, "{json}");
    }
}

#[tokio::test]
async fn the_payload_that_was_recorded_is_the_one_that_went_out() {
    let _serial = serialize().await;
    let (kernel, provider) = live_with!(Config {
        record_payloads: true,
        ..Default::default()
    });

    kernel.push(ContextItem::user("Reply with the single word: pong"));

    // what a client would show somebody before letting the request go
    let previewed = kernel
        .preview_payload()
        .unwrap()
        .expect("this provider renders its own payload");

    turn!(kernel);

    let recorded: Vec<Value> = kernel
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            Event::ModelPayload { payload } => Some(payload),
            _ => None,
        })
        .collect();

    assert_eq!(recorded.len(), 1, "one request, one payload");
    assert_eq!(
        recorded[0], previewed,
        "a preview that has quietly stopped matching is worse than none"
    );

    // and the payload really is an account of the request the provider was handed
    let sent = &provider.requests()[0];
    assert_eq!(
        recorded[0]["messages"].as_array().map(Vec::len),
        Some(sent.messages.len())
    );
    assert_eq!(
        recorded[0]["messages"][0]["content"].as_str(),
        sent.messages[0]
            .content
            .as_ref()
            .map(|c| c.to_text())
            .as_deref()
    );
}

#[tokio::test]
async fn a_step_abandoned_mid_request_leaves_the_kernel_usable() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    kernel.push(ContextItem::user(
        "Count from one to twenty, one number per line.",
    ));

    // a real request, dropped while the bytes are still moving. This is the case a scripted
    // provider cannot produce: there is no way to mock a socket that is genuinely half-read
    let abandoned = tokio::time::timeout(Duration::from_millis(100), kernel.step()).await;
    if abandoned.is_ok() {
        eprintln!("skipped: the request finished before it could be abandoned");
        return;
    }

    assert!(
        !kernel.state().is_busy(),
        "a dropped step must not leave the kernel stuck in {}",
        kernel.state()
    );
    assert_eq!(kernel.state(), State::Idle);
    assert!(
        kernel.last_response().is_none(),
        "nothing is recorded from a request that never came back"
    );
    assert_eq!(
        kernel.items().len(),
        1,
        "and the context is where it was: just the question"
    );

    // the point of not wedging is that the next one works
    let state = turn!(kernel);
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    assert!(!answer(&kernel).is_empty());
}

#[tokio::test]
async fn an_interrupted_turn_stops_between_requests() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();
    let mut events = kernel.subscribe();

    kernel.push(ContextItem::user("Reply with the single word: pong"));
    kernel.interrupt();

    // interrupting is checked before each request a turn would make, so a turn interrupted
    // before it starts makes none at all
    let state = turn!(kernel);

    assert_eq!(state, State::Idle);
    assert!(
        provider.requests().is_empty(),
        "an interrupted turn does not talk to the model"
    );
    assert!(
        drain(&mut events)
            .iter()
            .any(|event| event.name() == "turn.interrupted"),
        "and it says so rather than looking like a turn that did nothing"
    );
    assert!(!kernel.is_interrupted(), "the flag is spent, not sticky");

    // the context is untouched, so carrying on is one call
    let state = turn!(kernel);
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
async fn a_calibrating_counter_is_told_what_a_real_request_cost() {
    let _serial = serialize().await;
    let (kernel, _) = live!();
    let counter = Arc::new(Calibrating::new(BytesPerToken::default()));
    kernel.set_counter(counter.clone());

    // big enough to have a systematic error in it: a counter that learned from a ten-token
    // request would be learning from noise, and this one declines to
    kernel.push(ContextItem::file(
        "haystack.txt",
        "the quick brown fox jumps over the lazy dog. ".repeat(400),
    ));
    kernel.push(ContextItem::user("Reply with the single word: pong"));

    let guessed = kernel.budget().used();
    turn!(kernel);
    let charged = kernel
        .last_response()
        .unwrap()
        .usage
        .and_then(|usage| usage.input_tokens)
        .expect("the provider reports what it charged for") as usize;
    assert!(charged > 256, "the request has to be worth learning from");

    // it was told exactly what the kernel estimated and exactly what that cost - no rounding, no
    // averaging, nothing interpreted on the way. This is the part a scripted provider cannot
    // check: that the second number is a real one
    let learned = counter.calibration();
    assert_eq!(learned.observations, 1);
    assert_eq!(
        learned.estimated, guessed as u64,
        "the estimate it hears about is the one it made"
    );
    assert_eq!(learned.reported, charged as u64);

    // note: this deliberately does not assert that the estimate got *closer*. Whether it does
    // depends on how wrong the underlying counter happened to be for that one request - against a
    // short prompt `bytes / 4` can be exactly right, and a correction cannot improve on exact.
    // That the ratio converges is arithmetic, and it is pinned in `tests/tokens.rs`; what is
    // being checked here is that the arithmetic is fed real numbers.
    kernel.recount();
    kernel.push(ContextItem::user("And again, one word: pong"));
    let corrected = kernel.budget().used();
    turn!(kernel);

    let learned = counter.calibration();
    assert_eq!(learned.observations, 2);
    assert!(
        (0.1..=10.0).contains(&learned.scale),
        "a correction drawn from real requests should be a sane one, not {}",
        learned.scale
    );
    assert!(
        corrected > 0,
        "and it still produces a budget rather than zeroing one"
    );
}

#[tokio::test]
async fn an_interrupt_stops_a_stream_that_is_watching() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    // this provider streams when it is asked to, and an answer that arrives in one piece has no
    // middle to be interrupted in
    let mut params = params(500);
    params.insert("stream".into(), json!(true));
    params.insert("stream_options".into(), json!({ "include_usage": true }));
    kernel.set_params(params);

    let mut events = kernel.subscribe();
    kernel.push(ContextItem::user(
        "Count from one to two hundred, one number per line, and write nothing else.",
    ));

    // press the button the moment the first fragment arrives, from a task that is not the one
    // driving the loop - which is the only interesting case, and the reason `interrupt` exists
    let watcher = {
        let kernel = kernel.clone();
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                match event {
                    Event::ModelDelta { .. } => {
                        kernel.interrupt();
                        return true;
                    }
                    Event::ModelFinished { .. } | Event::ModelFailed { .. } => return false,
                    _ => {}
                }
            }

            false
        })
    };

    let state = turn!(kernel);
    if !watcher.await.unwrap_or(false) {
        eprintln!("skipped: the whole answer arrived before a fragment could be seen");
        return;
    }

    let State::Finished { stop, .. } = &state else {
        panic!("no tools were offered: {state:?}")
    };
    assert_eq!(
        stop,
        &StopReason::Other("interrupted".to_owned()),
        "the provider was watching, so it stopped rather than reading to the end"
    );

    // what had arrived is an ordinary item: partial, but real, and there to keep or to prune
    let answer = answer(&kernel);
    assert!(!answer.is_empty(), "it kept what it had");
    assert!(
        !answer.contains("200"),
        "and it really did stop early: {answer:?}"
    );
    assert_eq!(kernel.items().len(), 2);

    // the loop is at rest, and one transition attempt clears the outstanding interrupt
    assert!(!kernel.state().is_busy());
    kernel.step().await.unwrap();
    assert!(!kernel.is_interrupted());
}
