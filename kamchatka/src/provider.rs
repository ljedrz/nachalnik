//! A [`Provider`] that speaks the OpenAI chat-completions dialect over HTTP, streamed.
//!
//! note: There is nothing terminal-specific in here, and nothing kernel-specific either - it is
//! the HTTP that every one of these APIs happens to agree on. It reports fragments through the
//! [`DeltaSink`] and prints nothing: the screen belongs to the terminal, and a provider that
//! wrote to it would be drawing over the frame.

use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use nachalnik::{
    BoxError, Content, DeltaSink, Message, ModelInfo, ModelRequest, ModelResponse, Provider,
    StopReason, ToolCall, ToolCallId, Usage, async_trait,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// How many times a request is retried when the server says it is busy.
pub(crate) const RETRIES: usize = 4;

/// How long a stream may say nothing before the provider looks up to check whether it has been
/// asked to stop.
const HEARTBEAT: Duration = Duration::from_millis(120);

/// How long a stream may say nothing before the person watching is told about it.
pub(crate) const QUIET: Duration = Duration::from_secs(10);

/// How much longer it has to keep saying nothing before being mentioned again.
pub(crate) const AGAIN: Duration = Duration::from_secs(30);

/// How long it may say nothing before the request is given up on.
pub(crate) const PATIENCE: Duration = Duration::from_secs(150);

/// What a stream's silence has come to mean.
pub(crate) enum Silence {
    /// Long enough to be worth saying out loud, this many whole seconds in.
    Worth(u64),
    /// Long enough to stop waiting.
    Enough,
    /// Not long enough to be either.
    Ordinary,
}

/// Watches the gap since the last byte of a stream.
///
/// note: [`HEARTBEAT`] only makes a stalled request *interruptible*. It wakes up, checks whether
/// escape was pressed, and goes back to waiting - so a server that answers the connection and
/// then goes quiet, which is exactly what an overloaded one does, left the status line reading
/// `asking` for ever with nothing to tell it apart from a model that was simply thinking hard.
/// This is the part that says so, and eventually the part that stops.
pub(crate) struct Vigil {
    /// When something last arrived.
    last: Instant,
    /// The silence already mentioned, in whole seconds; zero when there is nothing to mention.
    said: u64,
}

impl Vigil {
    /// Starts watching, now.
    pub(crate) fn new() -> Self {
        Self {
            last: Instant::now(),
            said: 0,
        }
    }

    /// Something arrived; returns whether the quiet before it had been mentioned.
    pub(crate) fn heard(&mut self) -> bool {
        let mentioned = self.said > 0;
        self.last = Instant::now();
        self.said = 0;

        mentioned
    }

    /// Nothing has arrived; what that has come to mean.
    pub(crate) fn waited(&mut self) -> Silence {
        self.judge(self.last.elapsed())
    }

    /// The same, for a silence of a given length.
    ///
    /// note: split out so that the rule can be tested without a socket and a wall clock. What is
    /// left in `waited` is the clock reading, which has nothing in it to get wrong.
    fn judge(&mut self, silent: Duration) -> Silence {
        if silent >= PATIENCE {
            return Silence::Enough;
        }

        // once, and then at intervals: a line a second for two and a half minutes would bury the
        // conversation it was reporting on
        let due = match self.said {
            0 => QUIET.as_secs(),
            said => said + AGAIN.as_secs(),
        };
        let seconds = silent.as_secs();
        if seconds >= due {
            self.said = seconds;
            return Silence::Worth(seconds);
        }

        Silence::Ordinary
    }
}

/// Any server that speaks the OpenAI chat-completions dialect, streamed.
pub struct OpenAiCompatible {
    client: reqwest::Client,
    /// Where the requests go, which is a thing somebody changes mid-session.
    ///
    /// note: behind a lock for the same reason the model is. Comparing two models usually means
    /// one endpoint and two names, but comparing a hosted model with the one on the machine in
    /// front of you means two endpoints - and having to restart to do it makes the session, which
    /// is the thing being compared, part of what changed.
    base_url: Mutex<String>,
    api_key: String,
    model: Mutex<String>,
    context_limit: Mutex<Option<usize>>,
    /// How many times this provider has backed off, so that a busy server cannot be retried
    /// forever by a session that keeps making new requests.
    attempts: AtomicUsize,
    /// What the last retry was about, for the status line; the terminal has no stderr to spare.
    notice: Mutex<Option<String>>,
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

/// Installs the cryptography `rustls` will use, and says nothing if it is already installed.
///
/// note: reqwest is built with `rustls-no-provider`, so there is no default waiting behind this -
/// a client built without it fails at the first `https://` with "no process-level CryptoProvider
/// available". It lives beside the constructor rather than in `main` so that a test or an example
/// that builds a provider and never goes near `main` is not the one that finds out.
pub(crate) fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

impl OpenAiCompatible {
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

    /// Where the requests are going.
    pub fn endpoint(&self) -> String {
        self.base_url.lock().clone()
    }

    /// Just the authority of [`Self::endpoint`] - `openrouter.ai`, `localhost:11434` - for the
    /// status line, which has no room for the rest of it.
    ///
    /// note: the parsing lives on [`Endpoint::host`], where the other dialect can share it; this
    /// is here so that a caller holding the concrete type does not have to import a trait to ask
    /// an obvious question.
    pub fn host(&self) -> String {
        Endpoint::host(self)
    }

    /// Sends the requests somewhere else from now on, and asks the new place what it can hold.
    ///
    /// note: the model goes with it, because a model belongs to the address it is served at and
    /// carrying the old name over is how a session ends up asking a local ollama for
    /// `gemini-3.6-flash`. Given none, the old name is kept - a name that is right at both
    /// addresses is the ordinary case, and refusing to keep it would be a nuisance - and the new
    /// endpoint is then asked whether it has one by that name, which is a notice rather than a
    /// 404 on the next request.
    ///
    /// note: the key is not changed with it. It is read from the environment once, at startup, and
    /// a key typed at the prompt would be a key in the transcript - so what this is for is the
    /// endpoints that need no key or the same one: a local model, a proxy, a second base URL on
    /// the same account.
    pub async fn set_endpoint(&self, url: impl Into<String>, model: Option<String>) {
        *self.base_url.lock() = url.into();
        if let Some(model) = model {
            *self.model.lock() = model;
        }
        *self.context_limit.lock() = configured_limit();
        self.probe().await;
        self.say_if_the_model_is_not_there().await;
    }

    /// Every model this endpoint says it serves, if it will say.
    ///
    /// note: an empty answer means "it did not say" rather than "it has none". A gateway or a
    /// proxy may serve no listing at all, and treating a silence as a denial would be inventing a
    /// restriction nobody stated.
    pub async fn models(&self) -> Vec<String> {
        let base = self.endpoint();
        let listed = self.listed_names(&format!("{base}/models"), true).await;
        if !listed.is_empty() {
            return listed;
        }
        match base.strip_suffix("/openai") {
            Some(native) => self.listed_names(&format!("{native}/models"), false).await,
            None => listed,
        }
    }

    /// Puts a notice up if the model is not one the endpoint lists.
    ///
    /// note: the alternative is finding out on the next request, as a 404 with a paragraph of
    /// somebody's API prose in it. Switching address and model are two commands and it is easy to
    /// do one of them.
    async fn say_if_the_model_is_not_there(&self) {
        let model = self.model.lock().clone();
        let listed = self.models().await;
        if listed.is_empty() || listed.iter().any(|name| same_model(name, &model)) {
            return;
        }

        let some: Vec<&str> = listed.iter().take(3).map(String::as_str).collect();
        *self.notice.lock() = Some(format!(
            "{model} is not one of the {} models at this address ({}{}); /model to pick one",
            listed.len(),
            some.join(", "),
            match listed.len() > some.len() {
                true => ", …",
                false => "",
            }
        ));
    }

    /// The identifiers in a listing, however that listing spells them.
    async fn listed_names(&self, url: &str, bearer: bool) -> Vec<String> {
        let request = match bearer {
            true => self.client.get(url).bearer_auth(&self.api_key),
            false => self.client.get(format!("{url}?key={}", self.api_key)),
        };
        let Ok(response) = request.send().await else {
            return Vec::new();
        };
        let Ok(body) = response.json::<Value>().await else {
            return Vec::new();
        };

        body["data"]
            .as_array()
            .or_else(|| body["models"].as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry["id"].as_str().or_else(|| entry["name"].as_str()))
                    .map(|name| name.strip_prefix("models/").unwrap_or(name).to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Switches models, and forgets the context limit that belonged to the old one.
    ///
    /// note: the limit somebody set for themselves is not the old model's, and is put back rather
    /// than forgotten. Without this, `/model` quietly replaced an explicit
    /// `KAMCHATKA_CONTEXT_LIMIT` with whatever the endpoint advertises - which is the number the
    /// person had already decided not to measure against.
    pub async fn set_model(&self, model: impl Into<String>) {
        *self.model.lock() = model.into();
        *self.context_limit.lock() = configured_limit();
        self.probe().await;
        self.say_if_the_model_is_not_there().await;
    }

    /// Takes whatever the provider last wanted to say for itself, if anything.
    pub fn take_notice(&self) -> Option<String> {
        self.notice.lock().take()
    }

    /// Asks the provider what it knows about the model, so that the context limit the kernel
    /// reports is the real one rather than a guess.
    ///
    /// note: Worth the round trip, because every figure the status line shows about how full the
    /// context is is measured against this number. An unknown limit is reported as unknown rather
    /// than guessed at, which is the honest answer but not a useful one.
    pub async fn probe(&self) {
        if self.context_limit.lock().is_some() {
            return;
        }

        let base = self.endpoint();
        let mut limit = self.listed_limit(&format!("{base}/models"), true).await;

        // an OpenAI-compatible listing does not have to carry a context length, and Google's does
        // not; its native one does, one path up
        if limit.is_none()
            && let Some(native) = base.strip_suffix("/openai")
        {
            limit = self.listed_limit(&format!("{native}/models"), false).await;
        }
        // ollama's does not either, and the number its `/api/show` advertises is the wrong one
        if limit.is_none()
            && let Some(root) = base.strip_suffix("/v1")
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
        // model this session is about to talk to anyway
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
        // only what the user set, and nothing else: the kernel invents no parameters, and neither
        // does this provider
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

        // a free tier answers "busy" often enough that not retrying makes the whole thing look
        // broken when it is not. Waiting and trying again is the *provider's* business: the
        // kernel must not silently send a request twice behind a caller's back
        let mut response = loop {
            let response = self
                .client
                .post(format!("{}/chat/completions", self.endpoint()))
                .bearer_auth(&self.api_key)
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
                "{} answered {}; trying again in {}s",
                self.model.lock(),
                status.as_u16(),
                wait.as_secs()
            ));
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
        let mut vigil = Vigil::new();

        loop {
            // the timeout is what makes a model that says nothing at all interruptible; without
            // it this sits in `chunk` until the server feels like talking, and a request that
            // stalls before its first byte leaves `esc` doing nothing whatever. The same reason
            // the shell tool has one
            let bytes = match tokio::time::timeout(HEARTBEAT, response.chunk()).await {
                Ok(Ok(Some(bytes))) => {
                    if vigil.heard() {
                        *self.notice.lock() =
                            Some(format!("{} is answering again", self.model.lock()));
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
                    match vigil.waited() {
                        Silence::Enough => {
                            return Err(format!(
                                "{} answered and then said nothing for {}s; giving up",
                                self.model.lock(),
                                PATIENCE.as_secs()
                            )
                            .into());
                        }
                        Silence::Worth(seconds) => {
                            *self.notice.lock() = Some(format!(
                                "{} has said nothing for {seconds}s; esc gives up on it",
                                self.model.lock()
                            ));
                        }
                        Silence::Ordinary => {}
                    }
                    continue;
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(end) = buffer.find('\n') {
                // somebody pressed escape. Whatever has been parsed is kept and the rest of the
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
                    usage = Some(Usage {
                        input_tokens: reported["prompt_tokens"].as_u64(),
                        output_tokens: reported["completion_tokens"].as_u64(),
                        reasoning_tokens: reported["completion_tokens_details"]["reasoning_tokens"]
                            .as_u64(),
                        cached_input_tokens: reported["prompt_tokens_details"]["cached_tokens"]
                            .as_u64(),
                    });
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
                    // note: OpenAI numbers the calls in a message and streams each one's arguments
                    // in fragments, so the index is what says which call a fragment belongs to.
                    // Google's compatible endpoint sends no index at all - one whole call per
                    // chunk, each with an identifier of its own - and taking that for index zero
                    // folded three parallel calls into one: the names ran together into
                    // `writewritewrite` and the model was told there was no such tool. So the
                    // identifier decides when there is no index, and a fragment with neither
                    // continues whatever came last
                    let at = match requested["index"].as_u64() {
                        Some(index) => index as usize,
                        None => match requested["id"].as_str().filter(|id| !id.is_empty()) {
                            Some(id) => calls
                                .iter()
                                .position(|call| call.id == id)
                                .unwrap_or(calls.len()),
                            None => calls.len().saturating_sub(1),
                        },
                    };
                    while calls.len() <= at {
                        calls.push(PartialCall::default());
                    }
                    let call = &mut calls[at];

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
            // a request stopped before the server had said anything is not a broken response, and
            // reporting it as one would put a red line on the screen for doing what was asked
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

/// The half of a provider the person at the terminal drives.
///
/// note: [`Provider`] is the kernel's half, and it is one method: the kernel asks for an answer
/// and has no opinion about where the answer comes from. Where the requests go, what the endpoint
/// serves, which model is being asked and what the last retry was about are this program's
/// business - `/model`, `/models`, `/provider` and the status line - and they are the same
/// questions whichever dialect is in use. So they are a trait of this crate's own rather than
/// something the runtime was made to carry.
///
/// note: [`Provider`] is a supertrait, so one `Arc` answers both. That is what lets `App` hold a
/// provider without knowing which wire format is behind it, which is the claim `/seams` makes
/// about every other part of the runtime and had not been true of this one.
#[async_trait]
pub trait Endpoint: Provider {
    /// Where the requests are going.
    fn endpoint(&self) -> String;

    /// Which model is being asked.
    fn model(&self) -> String;

    /// Just the authority of [`Endpoint::endpoint`] - `openrouter.ai`, `localhost:11434` - for
    /// the status line, which has no room for the rest of it.
    ///
    /// note: hand-parsed rather than through a URL crate, because the whole use of it is a string
    /// to draw. Anything this does not recognise as a URL is handed back as it came, since a
    /// status line showing nothing would be worse than one showing something odd.
    fn host(&self) -> String {
        let endpoint = self.endpoint();
        let after_scheme = endpoint
            .split_once("://")
            .map_or(endpoint.as_str(), |(_, rest)| rest);

        after_scheme
            .split('/')
            .next()
            .filter(|host| !host.is_empty())
            .unwrap_or(&endpoint)
            .to_owned()
    }

    /// What this endpoint serves, which is what [`Endpoint::set_model`] takes.
    async fn models(&self) -> Vec<String>;

    /// Switches models.
    async fn set_model(&self, model: String);

    /// Switches the address the requests go to, and optionally the model with it.
    async fn set_endpoint(&self, url: String, model: Option<String>);

    /// Takes whatever the provider last wanted to say for itself, if anything.
    fn take_notice(&self) -> Option<String>;
}

#[async_trait]
impl Endpoint for OpenAiCompatible {
    fn endpoint(&self) -> String {
        self.endpoint()
    }

    fn model(&self) -> String {
        self.model.lock().clone()
    }

    async fn models(&self) -> Vec<String> {
        self.models().await
    }

    async fn set_model(&self, model: String) {
        self.set_model(model).await;
    }

    async fn set_endpoint(&self, url: String, model: Option<String>) {
        self.set_endpoint(url, model).await;
    }

    fn take_notice(&self) -> Option<String> {
        self.take_notice()
    }
}

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

/// Whether a listed identifier names the model being asked about, allowing for the decorations
/// listings put on them: Google's `models/` prefix, ollama's implicit `:latest` tag.
pub fn same_model(listed: &str, model: &str) -> bool {
    listed == model
        || listed.strip_prefix("models/") == Some(model)
        || listed.strip_suffix(":latest") == Some(model)
}

/// The context limit somebody set by hand, if they set one.
pub(crate) fn configured_limit() -> Option<usize> {
    env::var("KAMCHATKA_CONTEXT_LIMIT")
        .ok()
        .and_then(|limit| limit.parse().ok())
}

/// The API key, under whichever of the documented names it is set.
pub fn api_key() -> Result<String, BoxError> {
    env::var("KAMCHATKA_API_KEY")
        .or_else(|_| env::var("OPENROUTER_API_KEY"))
        .or_else(|_| env::var("OPENAI_API_KEY"))
        .map_err(|_| "set KAMCHATKA_API_KEY (or OPENROUTER_API_KEY / OPENAI_API_KEY)".into())
}

/// The endpoint to talk to; OpenRouter unless told otherwise.
pub fn base_url() -> String {
    env::var("KAMCHATKA_BASE_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_owned())
}

/// Builds a provider from the environment, asking the endpoint what the model's limit is.
pub async fn connect(model: impl Into<String>) -> Result<Arc<OpenAiCompatible>, BoxError> {
    let provider = Arc::new(OpenAiCompatible::new(model, base_url(), api_key()?));
    provider.probe().await;

    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the rule says about a silence of a given length, as a word.
    fn judged(vigil: &mut Vigil, seconds: u64) -> &'static str {
        match vigil.judge(Duration::from_secs(seconds)) {
            Silence::Worth(_) => "said",
            Silence::Enough => "gave up",
            Silence::Ordinary => "waited",
        }
    }

    #[test]
    fn a_stream_that_goes_quiet_is_mentioned_once_and_then_occasionally() {
        let mut vigil = Vigil::new();

        // a gap short enough to be a model thinking is not news
        assert_eq!(judged(&mut vigil, 0), "waited");
        assert_eq!(judged(&mut vigil, QUIET.as_secs() - 1), "waited");

        // the first one that is
        assert_eq!(judged(&mut vigil, QUIET.as_secs()), "said");
        // and not again a second later, or the report would bury the conversation it is about
        assert_eq!(judged(&mut vigil, QUIET.as_secs() + 1), "waited");
        assert_eq!(
            judged(&mut vigil, QUIET.as_secs() + AGAIN.as_secs() - 1),
            "waited"
        );
        assert_eq!(
            judged(&mut vigil, QUIET.as_secs() + AGAIN.as_secs()),
            "said"
        );

        // and eventually it stops waiting, which is the whole point: `asking` for ever was
        // indistinguishable from a model that was still coming
        assert_eq!(judged(&mut vigil, PATIENCE.as_secs()), "gave up");
        assert_eq!(judged(&mut vigil, PATIENCE.as_secs() + 60), "gave up");
    }

    #[test]
    fn a_stream_that_starts_talking_again_starts_the_count_over() {
        let mut vigil = Vigil::new();
        assert_eq!(judged(&mut vigil, QUIET.as_secs()), "said");

        // it came back, and the quiet before it had been mentioned - so the next one is news
        assert!(vigil.heard(), "the silence was reported, so its end is too");
        assert!(!vigil.heard(), "an ordinary byte is not");
        assert_eq!(judged(&mut vigil, QUIET.as_secs()), "said");
    }
}
