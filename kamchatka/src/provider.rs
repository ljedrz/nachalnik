//! A [`Provider`] that speaks the OpenAI chat-completions dialect over HTTP, streamed.
//!
//! note: There is nothing terminal-specific in here, and nothing kernel-specific either - it is
//! the HTTP that every one of these APIs happens to agree on. It reports fragments through the
//! [`DeltaSink`] and prints nothing: the screen belongs to the terminal, and a provider that
//! wrote to it would be drawing over the frame.

use std::{
    env,
    future::Future,
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

/// The longest a server may ask to be left alone before this stops waiting and says so.
///
/// note: for the difference between a busy server and one that has said no until tomorrow. A
/// per-minute limit answers `Retry-After: 5`; a spent daily quota answers with the seconds until
/// midnight, and sitting through four doublings to discover that wastes the turn and the wait.
const LINGER: Duration = Duration::from_secs(60);

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
    /// The parameter names the listing said this model takes, where it said anything. Learnt from
    /// the same entry the context limit comes from, which is already being fetched and read.
    parameters: Mutex<Vec<String>>,
    /// How many times this provider has backed off, so that a busy server cannot be retried
    /// forever by a session that keeps making new requests.
    attempts: AtomicUsize,
    /// What the last retry was about, for the status line; the terminal has no stderr to spare.
    notice: Mutex<Option<String>>,
    /// Who to say these requests are on behalf of, where the endpoint asks. Set once, at
    /// construction: it is a property of the program making the request, not of the session.
    attribution: Option<Attribution>,
}

/// The app a request is being made on behalf of, for an endpoint that keeps a ranking of them.
///
/// note: the URL is an identifier rather than a link anybody follows - OpenRouter keeps the app's
/// page against it - so it wants to be the project's own address and to stay the same.
#[derive(Clone, Debug)]
pub struct Attribution {
    /// The app's own URL, which is what the ranking is kept against.
    pub url: String,
    /// What to call it on the page.
    pub title: String,
}

/// A tool call being assembled from streamed fragments.
#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
    /// Whatever the provider attached to the call, which it will want back verbatim.
    extra: Value,
    /// The number the provider filed this call under, if it used one. Not a position: see the
    /// note where the fragments are gathered.
    slot: Option<u64>,
}

/// Whether an endpoint is one that keeps a ranking of the apps calling it.
///
/// note: on the authority rather than on the whole address, so a self-hosted path or a regional
/// subdomain still counts, and `openrouter.ai.example.com` does not.
fn ranks_apps(host: &str) -> bool {
    let host = host.split(':').next().unwrap_or(host);
    host == "openrouter.ai" || host.ends_with(".openrouter.ai")
}

/// What to say about a request the server refused: its own sentence, rather than its envelope.
///
/// note: a spent quota came back as six hundred characters of JSON - the message, the remedy, the
/// rate-limit headers, and the account's `user_id` - and all of it went into the transcript and
/// into the session log, which is a file people send each other. What a reader needs is the
/// sentence. Nobody needs an identifier for their account written into it.
fn complaint(status: reqwest::StatusCode, body: &str) -> String {
    match serde_json::from_str::<Value>(body)
        .ok()
        .as_ref()
        .and_then(said)
    {
        Some(said) => format!("{status}: {said}"),
        None => {
            let short: String = body.trim().chars().take(300).collect();
            match short.is_empty() {
                true => format!("{status}"),
                false => format!("{status}: {short}"),
            }
        }
    }
}

/// The sentence inside an error object, wherever the server put it.
///
/// note: two shapes, both seen on the same endpoint in the same afternoon. A refused request
/// nests it under `error`; a stream that fails halfway sends the object on its own, with `message`
/// at the top. Reading only the first left the second one printing its whole envelope, which is
/// the thing this function exists to stop.
fn said(value: &Value) -> Option<String> {
    let error = match value.get("error").filter(|error| !error.is_null()) {
        Some(nested) => nested,
        None => value,
    };
    let message = error["message"].as_str()?.trim();

    let mut said = message.chars().take(400).collect::<String>();
    // the upstream's own words, where the wrapper is only reporting that something upstream
    // failed - "Provider returned error" on its own names neither the provider nor the problem
    if let Some(raw) = error["metadata"]["raw"]
        .as_str()
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && !message.contains(*raw))
    {
        said.push_str(" - ");
        said.push_str(&raw.chars().take(300).collect::<String>());
    }
    Some(said)
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
            parameters: Mutex::new(Vec::new()),
            attempts: AtomicUsize::new(0),
            notice: Mutex::new(None),
            attribution: None,
        }
    }

    /// Says which app these requests are being made on behalf of.
    ///
    /// note: off unless a caller asks for it, and sent only to the endpoint that reads it. The
    /// headers name the program, never the person running it or what they asked - but a
    /// `HTTP-Referer` volunteered to whatever address somebody has pointed this at is still
    /// something they did not ask to send, and `KAMCHATKA_BASE_URL` points it at anything.
    pub fn on_behalf_of(mut self, url: impl Into<String>, title: impl Into<String>) -> Self {
        self.attribution = Some(Attribution {
            url: url.into(),
            title: title.into(),
        });
        self
    }

    /// Adds the app headers, if there are any and this is somewhere that reads them.
    fn attributed(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let Some(app) = self
            .attribution
            .as_ref()
            .filter(|_| ranks_apps(&self.host()))
        else {
            return request;
        };
        // `X-OpenRouter-Title` is the current name; `X-Title` is the one it replaced and is still
        // accepted. Neither the categories nor the visibility header is sent: an unrecognised
        // category is refused, and the default visibility is the point of attribution
        request
            .header(reqwest::header::REFERER, &app.url)
            .header("X-OpenRouter-Title", &app.title)
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
        self.parameters.lock().clear();
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
        // a limit somebody set for themselves is a decision about what to measure against; it is
        // not a statement about which parameters the model takes, so the listing is still worth
        // reading. Only the limit is left alone
        let settled = self.context_limit.lock().is_some();

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

        if !settled {
            *self.context_limit.lock() = limit;
        }
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

        // the same entry carries what the model will accept, and reading it here costs nothing:
        // the round trip has already happened
        if let Some(listed) = entry["supported_parameters"].as_array() {
            *self.parameters.lock() = listed
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect();
        }

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
            parameters: self.parameters.lock().clone(),
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

        // read once, and used for every line said about this request: a name that changed halfway
        // through would make one wait look like two
        let model = self.model.lock().clone();

        // a free tier answers "busy" often enough that not retrying makes the whole thing look
        // broken when it is not. Waiting and trying again is the *provider's* business: the
        // kernel must not silently send a request twice behind a caller's back
        let mut response = loop {
            let response = match watched(
                self.attributed(
                    self.client
                        .post(format!("{}/chat/completions", self.endpoint()))
                        .bearer_auth(&self.api_key),
                )
                .json(&body)
                .send(),
                &deltas,
                &model,
                &self.notice,
            )
            .await
            {
                Ok(response) => response,
                // a connection that timed out is a busy server wearing different clothes, and it
                // used to be the one thing here that was not waited out: a 429 got four tries and
                // a doubling, a stall got none. Eleven of fourteen runs against one upstream died
                // this way while the same model answered a single request in six seconds. A
                // refused connection is *not* this - it is a definite answer, usually an address
                // with nothing behind it, and making a typo take four doublings to report helps
                // nobody
                Err(reason) if reason.worth_waiting_out() => {
                    let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    let wait = Duration::from_secs(1 << attempt);
                    if attempt >= RETRIES {
                        self.attempts.store(0, Ordering::SeqCst);
                        return Err(reason.giving_up(&model));
                    }

                    *self.notice.lock() = Some(format!(
                        "{model} {}; trying again in {}s",
                        reason.what_happened(),
                        wait.as_secs()
                    ));
                    tokio::time::sleep(wait).await;
                    continue;
                }
                // nobody is owed an error for being obeyed
                Err(Unsent::Interrupted) => return Ok(interrupted()),
                Err(reason) => return Err(reason.giving_up(&model)),
            };

            let status = response.status();
            if status.is_success() {
                // the budget belongs to a request, not to a session: without this an afternoon
                // that had already ridden out four busy servers answered the fifth by giving up
                // on the first try
                self.attempts.store(0, Ordering::SeqCst);
                break response;
            }

            // the server's own answer to "when?", where it gives one. Guessing at a doubling is
            // for a server that did not say
            let asked = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);

            let transient = status.as_u16() == 429 || status.is_server_error();
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let wait = asked.unwrap_or(Duration::from_secs(1 << attempt));
            if !transient || attempt >= RETRIES || wait > LINGER {
                self.attempts.store(0, Ordering::SeqCst);
                let body = response.text().await.unwrap_or_default();
                let mut said = complaint(status, &body);
                if transient && wait > LINGER {
                    said.push_str(&format!(
                        " - it asked to be left for {}s, which is longer than this waits",
                        wait.as_secs()
                    ));
                }
                return Err(said.into());
            }

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
                    // the same treatment the refused-request path gets: this one arrives as a
                    // bare object with `message` at the top rather than nested under `error`, and
                    // printing it whole put the provider's entire envelope on the screen
                    return Err(match said(error) {
                        Some(said) => said,
                        None => format!("{error}").chars().take(300).collect(),
                    }
                    .into());
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
                        // note: the index says which call a fragment belongs to. It is *not* a
                        // position in the list: minimax numbers its calls from one, and using it
                        // as a slot left an unfilled call at zero, which the kernel then reported
                        // as a repaired identifier and a tool with no name - a wasted round trip
                        // and an error the model had to read. So an index is looked up, and a
                        // number never seen before starts a new call at the end
                        Some(index) => match calls.iter().position(|call| call.slot == Some(index))
                        {
                            Some(at) => at,
                            None => {
                                calls.push(PartialCall {
                                    slot: Some(index),
                                    ..PartialCall::default()
                                });
                                calls.len() - 1
                            }
                        },
                        None => match requested["id"].as_str().filter(|id| !id.is_empty()) {
                            Some(id) => match calls.iter().position(|call| call.id == id) {
                                Some(at) => at,
                                None => {
                                    calls.push(PartialCall::default());
                                    calls.len() - 1
                                }
                            },
                            None => match calls.is_empty() {
                                true => {
                                    calls.push(PartialCall::default());
                                    0
                                }
                                false => calls.len() - 1,
                            },
                        },
                    };
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
                return Ok(interrupted());
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

/// Why a request never became a response.
pub(crate) enum Unsent {
    /// The transport gave up on it.
    Transport(reqwest::Error),
    /// Nothing came back at all, for [`PATIENCE`].
    Silent,
    /// Somebody pressed escape before the answer had started.
    Interrupted,
}

impl Unsent {
    /// Whether sending it again is worth anything.
    pub(crate) fn worth_waiting_out(&self) -> bool {
        match self {
            Self::Transport(e) => worth_waiting_out(e),
            // the same thing the transport's own timeout means, arrived at by counting rather
            // than by being told: a server that took the connection and went quiet is busy
            Self::Silent => true,
            Self::Interrupted => false,
        }
    }

    /// What happened, as the middle of a sentence whose subject is the model.
    pub(crate) fn what_happened(&self) -> &'static str {
        match self {
            // worth telling apart: one of them hung up, the other never spoke
            Self::Transport(_) => "did not answer in time",
            Self::Silent => "has not answered at all",
            Self::Interrupted => "was interrupted",
        }
    }

    /// The error to end the turn with, once there is no patience left to spend.
    pub(crate) fn giving_up(self, model: &str) -> BoxError {
        match self {
            Self::Transport(e) => e.into(),
            Self::Silent => format!(
                "{model} never answered; giving up after {}s",
                PATIENCE.as_secs()
            )
            .into(),
            Self::Interrupted => "interrupted".into(),
        }
    }
}

/// The answer to a request that was stopped before it had one.
///
/// note: not an error. Somebody asked for this, and a red line on the screen for doing as asked
/// reads as a bug in the program rather than as an answer to the key that was pressed.
pub(crate) fn interrupted() -> ModelResponse {
    ModelResponse {
        content: None,
        reasoning: None,
        tool_calls: Vec::new(),
        stop: StopReason::Other("interrupted".to_owned()),
        usage: None,
        raw: None,
    }
}

/// Waits for a request to be answered, watching the wait the way the stream itself is watched.
///
/// note: `&mut sending` rather than `sending`. Handing `timeout` the future itself would drop it
/// 120ms later and cancel the request that had just been made; borrowing it stops polling for
/// that round and leaves the connection standing. The loop is the one the stream runs, for the
/// same three reasons - `esc` is heard, the silence is said out loud, and it ends - and it is
/// here because everything before the first byte had none of them. A server that accepted the
/// connection and then went away held the terminal for eighteen minutes with `asking` on the
/// status line and no way to take it back.
///
/// note: free rather than a method, because [`Gemini`](crate::gemini::Gemini) sends its requests
/// down a different URL with a different header and needs exactly this in front of them.
pub(crate) async fn watched(
    sending: impl Future<Output = reqwest::Result<reqwest::Response>>,
    deltas: &DeltaSink,
    model: &str,
    notice: &Mutex<Option<String>>,
) -> Result<reqwest::Response, Unsent> {
    let mut sending = std::pin::pin!(sending);
    let mut vigil = Vigil::new();

    loop {
        if let Ok(sent) = tokio::time::timeout(HEARTBEAT, &mut sending).await {
            return sent.map_err(Unsent::Transport);
        }

        if deltas.is_interrupted() {
            return Err(Unsent::Interrupted);
        }

        match vigil.waited() {
            Silence::Enough => return Err(Unsent::Silent),
            Silence::Worth(seconds) => {
                *notice.lock() = Some(format!(
                    "{model} has not answered for {seconds}s; esc gives up on it"
                ));
            }
            Silence::Ordinary => {}
        }
    }
}

/// Whether a request that never got an answer is worth sending again.
///
/// note: a timeout only. Everything else a transport can fail with is either a decision - nothing
/// listening on that address, a name that does not resolve - or a bug in what was built, and
/// neither improves by being repeated. `is_timeout` walks the source chain, so a stall reported
/// as hyper's `Io(TimedOut)` several layers down still counts.
fn worth_waiting_out(e: &reqwest::Error) -> bool {
    e.is_timeout()
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

/// The project these requests are made on behalf of, where the endpoint keeps a ranking of apps.
///
/// note: this crate's own directory rather than the repository root, because the identifier
/// should name the thing making the requests. `nachalnik` is the runtime and makes none - it owns
/// no client and no provider - and a page called `kamchatka` that resolved to the library would
/// leave a visitor to find the program inside it. It also keeps the identifier free for anything
/// else in this workspace that ever calls out.
///
/// note: `HEAD` rather than a branch name. This is a primary key: OpenRouter builds the app's
/// page against it, so changing it later does not rename the app, it starts a new one and orphans
/// what the old one had. `tree/master/...` would have been that change waiting to happen the day
/// the default branch is renamed.
///
/// note: what goes out is the name of the program and nothing else - not the key, not the model,
/// not a word of what was asked - and it goes only to OpenRouter. `KAMCHATKA_NO_ATTRIBUTION`
/// turns it off, because a program that names its user's tooling to a third party should say so
/// and let them stop it.
const APP_URL: &str = "https://github.com/ljedrz/nachalnik/tree/HEAD/kamchatka";
const APP_TITLE: &str = "kamchatka";

/// Builds a provider from the environment, asking the endpoint what the model's limit is.
pub async fn connect(model: impl Into<String>) -> Result<Arc<OpenAiCompatible>, BoxError> {
    let mut provider = OpenAiCompatible::new(model, base_url(), api_key()?);
    if env::var_os("KAMCHATKA_NO_ATTRIBUTION").is_none() {
        provider = provider.on_behalf_of(APP_URL, APP_TITLE);
    }

    let provider = Arc::new(provider);
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
    fn only_the_endpoint_that_reads_the_app_headers_is_sent_them() {
        // the ones that are
        assert!(ranks_apps("openrouter.ai"));
        assert!(ranks_apps("openrouter.ai:443"));
        assert!(ranks_apps("api.openrouter.ai"));

        // and the ones that are not. The last is the reason this matches on the authority rather
        // than looking for the name anywhere in the address: a suffix test on the whole URL would
        // have sent an unrelated host the name of the program calling it
        assert!(!ranks_apps("localhost:11434"));
        assert!(!ranks_apps("generativelanguage.googleapis.com"));
        assert!(!ranks_apps("openrouter.ai.example.com"));
        assert!(!ranks_apps("notopenrouter.ai"));
        assert!(!ranks_apps(""));
    }

    /// The two shapes a rate limit actually arrived in, copied out of a real session.
    ///
    /// note: `concat!` rather than a raw string over several lines. A raw string keeps the
    /// backslash *and* the newline, so a fixture written that way is not JSON, `complaint` falls
    /// through to clipping it, and the test passes without ever reaching the code it is about.
    #[test]
    fn a_refused_request_is_reported_in_the_server_s_own_words_and_no_more() {
        let status = reqwest::StatusCode::TOO_MANY_REQUESTS;

        // a spent daily quota: one useful sentence, wrapped in the rate-limit headers and the
        // account's identifier, neither of which belongs in a file somebody will send on
        let daily = concat!(
            r#"{"error":{"message":"Rate limit exceeded: free-models-per-day. Add 10 credits"#,
            r#" to unlock 1000 free model requests per day","code":429,"metadata":{"headers":"#,
            r#"{"X-RateLimit-Remaining":"0"},"limit_source":"openrouter_free_tier_daily"}},"#,
            r#""user_id":"user_3GBJq3JdBGGCK0OiVeXg1v8GYfW"}"#,
        );
        assert!(
            serde_json::from_str::<Value>(daily).is_ok(),
            "the fixture has to be the shape the server actually sends"
        );
        let said = complaint(status, daily);
        assert!(said.contains("free-models-per-day"), "{said}");
        assert!(
            !said.contains("user_"),
            "the account is nobody's business: {said}"
        );
        assert!(!said.contains("X-RateLimit"), "{said}");
        assert!(said.len() < daily.len() / 2, "and it is shorter: {said}");

        // an upstream one, where the wrapper's own message names neither the provider nor the
        // problem, and the sentence worth reading is underneath it
        let upstream = concat!(
            r#"{"error":{"message":"Provider returned error","code":429,"metadata":{"raw":"#,
            r#""z-ai/glm-5.2:free is temporarily rate-limited upstream.","provider_name":"#,
            r#""Decart"}}}"#,
        );
        assert!(serde_json::from_str::<Value>(upstream).is_ok());
        let said = complaint(status, upstream);
        assert!(said.contains("Provider returned error"), "{said}");
        assert!(said.contains("temporarily rate-limited upstream"), "{said}");

        // something that is not JSON at all still says what happened
        let plain = complaint(status, "<html>gateway timeout</html>");
        assert!(
            plain.contains("429") && plain.contains("gateway timeout"),
            "{plain}"
        );

        // and so does nothing at all
        assert!(complaint(status, "").contains("429"));
    }

    /// The shape a stream that fails halfway sends, which is not the shape a refused request
    /// sends. Copied out of a live session against `inception/mercury-2.5-preview`.
    #[test]
    fn an_error_that_arrives_mid_stream_is_read_the_same_way() {
        let midstream = concat!(
            r#"{"code":502,"message":"Upstream error from Inception: I'm sorry, but I can't"#,
            r#" share details of my architecture or training process.","metadata":"#,
            r#"{"error_type":"provider_unavailable"}}"#,
        );
        let value: Value =
            serde_json::from_str(midstream).expect("the shape it actually arrives in");

        // `message` at the top, with no `error` around it - read only the nested one and this
        // whole envelope went to the screen
        let sentence = said(&value).expect("there is a sentence in there");
        assert!(
            sentence.starts_with("Upstream error from Inception"),
            "{sentence}"
        );
        assert!(!sentence.contains("error_type"), "{sentence}");
        assert!(!sentence.contains('{'), "no envelope: {sentence}");

        // and the nested shape still reads, so one function serves both paths
        let nested: Value = serde_json::from_str(r#"{"error":{"message":"nested"}}"#).unwrap();
        assert_eq!(said(&nested).as_deref(), Some("nested"));

        // something with no sentence in it at all has nothing to hand back
        let empty: Value = serde_json::from_str(r#"{"code":502}"#).unwrap();
        assert_eq!(said(&empty), None);
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

    /// A socket that accepts and then says nothing, so a request to it stalls the way a loaded
    /// upstream does rather than being refused.
    async fn black_hole() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        tokio::spawn(async move {
            // held open, never answered: accepting and dropping would be a reset, which is a
            // different thing entirely
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });

        at
    }

    /// An address on which nothing is listening, and which says so.
    ///
    /// note: bound and then dropped rather than a well-known port assumed to be free. This was
    /// `127.0.0.1:9` - discard - and it failed on a CI host whose firewall *drops* packets to a
    /// reserved port instead of refusing them, which turns "nothing is listening" into a timeout
    /// and so into the one answer this half of the test needs it not to give. A port the OS has
    /// just handed out and taken back is refused rather than filtered.
    async fn nobody_home() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        drop(listener);

        at
    }

    #[tokio::test]
    async fn a_stalled_request_is_waited_out_and_a_refused_one_is_not() {
        // the failure this closes: a 429 got four tries and a doubling; a connection that stalled
        // got none, and took the session with it. Eleven of fourteen runs against one upstream
        // died this way while the same model answered a single request in six seconds
        install_crypto();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(250))
            .build()
            .expect("a client");

        let stalled = client
            .get(format!("http://{}/", black_hole().await))
            .send()
            .await
            .expect_err("nothing ever answers there");
        assert!(
            worth_waiting_out(&stalled),
            "a stall is a busy server, and this is the error a busy one produces: {stalled:?}"
        );

        // note: its own client, with room to spare. A refusal comes back in microseconds, so the
        // only thing a long timeout changes is whether a loaded machine can turn one into a
        // timeout before it arrives - and a timeout is precisely the answer that would make this
        // assertion mean the opposite of what it says
        let patient = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("a client");
        let refused = patient
            .get(format!("http://{}/", nobody_home().await))
            .send()
            .await
            .expect_err("nothing is listening");
        assert!(
            !refused.is_timeout(),
            "this host neither answered nor refused, so there is nothing here to tell apart; \
             a firewall that drops rather than refuses will do this: {refused:?}"
        );
        assert!(
            !worth_waiting_out(&refused),
            "an address with nothing behind it is an answer, not a delay: {refused:?}"
        );

        // counting the silence ourselves has to mean what the transport's own timeout means,
        // because it is now the thing that usually notices first
        assert!(
            Unsent::Silent.worth_waiting_out(),
            "a server that took the request and said nothing is a busy one"
        );

        // and the one failure that must never be retried: resending a request somebody cancelled
        // spends their money on an answer they asked not to have
        assert!(
            !Unsent::Interrupted.worth_waiting_out(),
            "esc is a decision, not a delay"
        );
    }
}
