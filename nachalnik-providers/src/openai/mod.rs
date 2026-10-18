//! A [`Provider`](nachalnik::Provider) that speaks the OpenAI chat-completions dialect over HTTP,
//! streamed.
//!
//! note: nothing in here is kernel-specific or client-specific: it is the HTTP every one of these
//! APIs agrees on, and each oddity below is one a server really sent.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use nachalnik::{ModelRequest, async_trait};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::{
    Dialect, Endpoint, Keyed, Published, RETRIES, install_crypto, same_model, waiting::WHOLE_ANSWER,
};

mod responses;
mod wire;

/// Parameters that stop a stream being a stream, and what each one does instead.
///
/// note: a stream in this dialect is text arriving in order, and every client appends. Inception's
/// `diffusing` sends the whole answer again in `delta.content` at each denoising step. Appended,
/// the answer becomes every draft end to end: in the answer, the context, the token count, and
/// whatever the client wrote down.
///
/// note: a list to warn from, not a refusal or a reader that tries to cope. Parameters are the
/// caller's to set and are carried to the provider verbatim; that is the runtime's rule, and a
/// provider does not get to break it. Nothing on the wire marks a snapshot, so a reader coping
/// would be guessing. A client can say so before the request, which is what this is for.
pub const NOT_A_STREAM: [(&str, &str); 1] = [(
    "diffusing",
    "sends the whole answer again at each denoising step, and a stream is read here as text \
     arriving in order - so the answer, the context and the log would keep every draft",
)];

/// Any server that speaks the OpenAI chat-completions dialect, streamed.
pub struct OpenAiCompatible {
    client: reqwest::Client,
    /// Where the requests go. Somebody can change it mid-session.
    ///
    /// note: behind a lock for the same reason the model is. Comparing two models usually means one
    /// endpoint and two names. Comparing a hosted model with a local one means two endpoints, and
    /// restarting to do it makes the session part of what changed.
    base_url: Mutex<String>,
    api_key: String,
    model: Mutex<String>,
    context_limit: Mutex<Option<usize>>,
    /// What the listing said about this model beyond its context limit, learnt from the same entry
    /// the limit comes from.
    listed: Mutex<Entry>,
    /// The limit the caller set by hand, if any. Changing model or endpoint puts it back rather
    /// than dropping it.
    ///
    /// note: separate from [`Self::context_limit`], which is what [`Self::probe`] discovered. The
    /// limit is about the session, not whichever model the session asks. A `set_model` that quietly
    /// replaced it with whatever the endpoint advertises would overrule the caller.
    configured: Option<usize>,
    /// How many HTTP requests this has made in its life, retries counted separately. Never reset:
    /// it answers "what did this cost", which a run wants at the end.
    attempts: AtomicUsize,
    /// How many times a request is sent while the server is busy, the first included; see
    /// [`Dialect::set_tries`].
    tries: AtomicUsize,
    /// How long a stream may say nothing before the request is given up on; see
    /// [`Dialect::set_patience`].
    patience: Mutex<std::time::Duration>,
    /// What the last retry was about, taken by [`Endpoint::take_notice`], for a client with
    /// somewhere to put it and no stderr to spare.
    notice: Mutex<Option<String>>,
    /// Who these requests are on behalf of, where the endpoint asks. Set once at construction: a
    /// property of the program making the request, not of the session.
    ///
    /// note: one value rather than a field per header, so builder method call order does not decide
    /// what goes out. A category set before the app was named is kept, and sent once there is a URL
    /// to send it with. See [`Attribution`].
    app: Attribution,
    /// What this endpoint is called, as [`nachalnik::ModelInfo::provider`] reports it.
    ///
    /// note: worth setting when there are several. Without it, a panel comparing models through
    /// three endpoints has three providers whose `info()` all say `openai-compatible`, so the
    /// reader cannot tell which is which.
    label: String,
    /// Whether to ask for a streamed answer at all; see [`Self::streaming`].
    stream: bool,
    /// Whether to read thinking a model wrote into its own content back out of it; see
    /// [`Self::thinking_in_content`].
    thinking_in_content: bool,
    /// Whether an assistant turn's thinking goes back out with it; see [`Self::send_reasoning`].
    send_reasoning: bool,
    /// Whether to ask the Responses API rather than chat completions; see [`Self::responses`].
    responses: bool,
    /// Every request this was asked to send, in order, when [`Self::recording`] is on.
    requests: Mutex<Vec<ModelRequest>>,
    /// Whether to keep them.
    ///
    /// note: off by default: a session running all afternoon would hold every request it ever made,
    /// which is the context several times over. It is for a test or run that asserts on what
    /// actually went out, not on what it believes went out.
    recording: bool,
}

/// What used to be this module's own, and is now the crate's, because the System One client sends
/// it too.
pub use crate::Attribution;

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
            base_url: Mutex::new(crate::address(base_url)),
            api_key: api_key.into(),
            model: Mutex::new(model.into()),
            context_limit: Mutex::new(None),
            configured: None,
            listed: Mutex::new(Entry::default()),
            attempts: AtomicUsize::new(0),
            tries: AtomicUsize::new(RETRIES),
            patience: Mutex::new(crate::waiting::PATIENCE),
            notice: Mutex::new(None),
            app: Attribution::default(),
            label: "openai-compatible".to_owned(),
            stream: true,
            thinking_in_content: true,
            send_reasoning: false,
            responses: false,
            requests: Mutex::new(Vec::new()),
            recording: false,
        }
    }

    /// A client with the timeout a slow endpoint wants, which reqwest's own default is not:
    /// [`reqwest::ClientBuilder::timeout`] is unset unless somebody sets it, so the client
    /// [`OpenAiCompatible::new`] builds for itself has none at all.
    ///
    /// note: also where the cryptography `rustls` will use is installed, for a caller that builds
    /// its own client and would otherwise find out at the first `https://`. See
    /// [`crate::install_crypto`].
    ///
    /// note: worth choosing deliberately, because reqwest's timeout covers the *whole* request -
    /// connect, generate and body - so it is an upper bound on how long a model may think and not
    /// only on how long a dead socket may hang. Set it below what the work takes and every long
    /// answer arrives as `error decoding response body`, which looks like a network fault and is
    /// not one. The default here is ten minutes for that reason.
    ///
    /// note: a provider with no timeout is not a provider with no patience, which is why the
    /// default client is usable rather than a trap. What catches a socket with nobody on the other
    /// end is the silence watch - counted by this crate rather than told by the transport - and it
    /// ends a request that has said nothing whether or not anything else would have.
    pub fn client() -> reqwest::Client {
        Self::client_with(WHOLE_ANSWER)
    }

    /// The same, with a timeout of the caller's choosing.
    pub fn client_with(timeout: Duration) -> reqwest::Client {
        install_crypto();
        reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("a default client is buildable")
    }

    /// Sends through the caller's own client, so several models on one host share a connection pool
    /// and the timeout is the caller's to set.
    ///
    /// note: a caller's own client may be built before any constructor in this crate has run, so
    /// call [`crate::install_crypto`] first or `reqwest::ClientBuilder::build` panics, recommending
    /// `aws_lc_rs`, which is reqwest's suggestion and not this crate's provider.
    /// [`Self::client_with`] is the way round it when only a different timeout is wanted, because
    /// it installs the cryptography itself.
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Names this endpoint, as [`nachalnik::ModelInfo::provider`] will report it.
    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Whether to ask for a streamed answer. On unless turned off.
    ///
    /// note: a whole answer is worth having when nothing watches it arrive - a benchmark, a batch,
    /// a test - and it is the only way to reach some endpoints' non-streaming paths, which may
    /// differ from their streaming code. It costs every [`nachalnik::DeltaSink`] fragment and
    /// anything from a turn stopped partway: an interrupt still ends the wait, but comes back empty
    /// because a one-piece answer has no middle to keep.
    #[must_use]
    pub fn streaming(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Whether to take thinking a model wrote into its own content back out of it. On unless
    /// turned off.
    ///
    /// note: this dialect carries the thinking in `reasoning`, beside the content and not in it -
    /// but a model whose chat template ends the prompt *inside* a thinking block never writes the
    /// `<think>` that opened it, and an endpoint serving that model with no reasoning parser of
    /// its own passes the whole thing through as content. What arrives is an answer with its own
    /// thinking on the front and a bare `</think>` in the middle of it. From
    /// `poolside/laguna-xs-2.1:free`, left alone, the tag goes into the context, the transcript and
    /// the log, and `reasoning` is `None` while the reasoning sits in the text.
    ///
    /// So a `</think>` in the content is read as what it is, the thinking in front of it becomes
    /// [`nachalnik::ModelResponse::reasoning`], and what follows is the answer. Only where the
    /// endpoint reported no reasoning of its own: one that fills the field properly is one whose
    /// content is content, and is not touched. The whole response is on
    /// [`nachalnik::ModelResponse::raw`] either way, so nothing here is destroyed.
    ///
    /// note: worth turning off where a model may write `</think>` as text and mean it - a
    /// conversation about these tags is the obvious one, and it would lose whatever came before
    /// the first of them to the reasoning channel.
    #[must_use]
    pub fn thinking_in_content(mut self, on: bool) -> Self {
        self.thinking_in_content = on;
        self
    }

    /// Whether to send an assistant turn's thinking back with it, as `reasoning` beside its
    /// content. Off unless turned on.
    ///
    /// note: for a model that refuses a request whose tool-calling turns come back without the
    /// thinking that produced them, and answers `400` to every request after the first call. Off by
    /// default because most endpoints speaking this dialect reject a message carrying a field they
    /// do not know, and because the thinking costs its tokens on every request. `reasoning` is
    /// OpenRouter's name for it and the one this dialect reads first; an endpoint that wants
    /// `reasoning_content` back does not get it from this.
    ///
    /// note: [`Dialect::projection`] follows it, so the budget counts the thinking exactly when it
    /// is sent. A turn the projector elided goes without its thinking either way, and a turn whose
    /// thinking is empty goes without the field.
    ///
    /// note: does not apply with [`Self::responses`], which sends back the sealed thinking that API
    /// hands out and nothing else.
    #[must_use]
    pub fn send_reasoning(mut self, on: bool) -> Self {
        self.send_reasoning = on;
        self
    }

    /// Whether to ask OpenAI's Responses API, at `/responses`, rather than chat completions. Off
    /// unless turned on.
    ///
    /// note: the same endpoint, key and listing, and a different conversation. A turn comes back as
    /// ordered items - reasoning, a message, calls - and goes back out the same way, the reasoning
    /// sealed in the `encrypted_content` this API hands out for it. Chat completions has nowhere to
    /// put that, so a reasoning model asked through it starts every turn's thinking from nothing.
    /// OpenAI's own API answers it, and so does OpenRouter. Most of the rest of this dialect's
    /// servers do not, which is why it is not the default.
    ///
    /// note: the requests are made with `store: false`, so the server keeps nothing between them,
    /// and with `reasoning.encrypted_content` added to `include`, which is what makes the thinking
    /// something that can be sent back. Both are the caller's to change in the parameters; the
    /// second is added again to an `include` they set, unless they asked the server to `store` the
    /// conversation instead.
    ///
    /// note: [`Self::thinking_in_content`] does not apply. This API keeps the thinking in items of
    /// its own, and a `</think>` in what the model said is a word it said.
    #[must_use]
    pub fn responses(mut self, on: bool) -> Self {
        self.responses = on;
        if on && self.label == "openai-compatible" {
            self.label = "openai-responses".to_owned();
        }
        self
    }

    /// Whether this asks the Responses API; see [`Self::responses`].
    pub fn asks_responses(&self) -> bool {
        self.responses
    }

    /// Whether to keep a copy of every request sent, for [`Self::requests`] to hand back.
    #[must_use]
    pub fn recording(mut self, on: bool) -> Self {
        self.recording = on;
        self
    }

    /// Every request this was asked to send, in order; empty unless [`Self::recording`] is on.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().clone()
    }

    /// How many requests for an answer this has sent, retries counted separately; a model
    /// listing or a probe is not one.
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    /// How many times a request may be sent while the server is busy; see [`Dialect::set_tries`].
    fn tries(&self) -> usize {
        self.tries.load(Ordering::SeqCst)
    }

    /// How long a stream may say nothing; see [`Dialect::set_patience`].
    fn patience(&self) -> std::time::Duration {
        *self.patience.lock()
    }

    /// Says which app these requests are being made on behalf of, all of it at once.
    ///
    /// note: the way to hand over one [`Attribution`] built for several clients, which is what a
    /// program asking one service for a conversation and for advice wants: one value, and one
    /// switch to stop sending it. It replaces whatever the three methods below had set.
    #[must_use]
    pub fn attributed_to(mut self, app: Attribution) -> Self {
        self.app = app;
        self
    }

    /// Says which app these requests are being made on behalf of.
    ///
    /// note: off unless a caller asks for it, and sent only to the endpoint that reads it - see
    /// [`Attribution::new`].
    pub fn on_behalf_of(mut self, url: impl Into<String>, title: impl Into<String>) -> Self {
        self.app.url = url.into();
        self.app.title = title.into();
        self
    }

    /// Says what kind of program it is, in the categories the endpoint files apps under.
    ///
    /// note: does nothing on its own. It is sent beside [`Self::on_behalf_of`] or not at all, and
    /// to the same one endpoint. The page is built against the URL, so a category with no URL
    /// beside it describes nothing and buys the caller nothing. See [`Attribution::filed_under`]
    /// for what is and is not checked.
    #[must_use]
    pub fn filed_under<C: Into<String>>(mut self, categories: impl IntoIterator<Item = C>) -> Self {
        self.app = std::mem::take(&mut self.app).filed_under(categories);
        self
    }

    /// Keeps the app page off the public rankings and out of the marketplace; see
    /// [`Attribution::unlisted`] for the one request in which it means anything.
    #[must_use]
    pub fn unlisted(mut self, unlisted: bool) -> Self {
        self.app.unlisted = unlisted;
        self
    }

    /// Waits this long for a stream that says nothing before giving the request up, rather than
    /// the two and a half minutes it waits unless told; see [`Dialect::set_patience`].
    #[must_use]
    pub fn with_patience(self, patience: std::time::Duration) -> Self {
        *self.patience.lock() = patience;
        self
    }

    /// Measures against this limit rather than against whatever the endpoint advertises.
    ///
    /// note: for the two cases [`probe`](Self::probe) cannot settle: an endpoint that publishes no
    /// context length, and one whose published length is not what the model is actually served
    /// with. It is sticky: [`set_model`](Self::set_model) and [`set_endpoint`](Self::set_endpoint)
    /// put it back, because it is a decision about what to measure, not a fact about one model.
    ///
    /// note: `None` means "ask the endpoint", which is the default, not "no limit". A caller can
    /// pass the `Option` straight through from its own configuration.
    pub fn with_context_limit(mut self, limit: Option<usize>) -> Self {
        self.configured = limit;
        *self.context_limit.get_mut() = limit;
        self
    }

    /// Adds the app headers, if there are any and this is somewhere that reads them.
    fn attributed(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        self.app.sign(request, &self.host())
    }

    /// Where the requests are going.
    pub fn endpoint(&self) -> String {
        self.base_url.lock().clone()
    }

    /// Which model is being asked.
    pub fn model(&self) -> String {
        self.model.lock().clone()
    }

    /// Just the authority of [`Self::endpoint`] (`openrouter.ai`, `localhost:11434`) for the status
    /// line.
    ///
    /// note: the parsing lives on [`Endpoint::host`], where the other dialect can share it; this is
    /// here so a caller holding the concrete type does not have to import a trait to ask an obvious
    /// question.
    pub fn host(&self) -> String {
        Endpoint::host(self)
    }

    /// Sends the requests somewhere else from now on, and asks the new place what it can hold.
    ///
    /// note: the model goes with it, because a model belongs to the address it is served at. Given
    /// none, the old name is kept - a name right at both addresses is the ordinary case - and the
    /// new endpoint is then asked whether it has one by that name, a notice rather than a 404 on
    /// the next request.
    ///
    /// note: the key is not changed with it. It is given once, at construction; a key typed at the
    /// prompt would be in the transcript. This is for endpoints that need no key or the same one: a
    /// local model, a proxy, a second base URL on the same account.
    pub async fn set_endpoint(&self, url: impl Into<String>, model: Option<String>) {
        *self.base_url.lock() = crate::address(url);
        if let Some(model) = model {
            *self.model.lock() = model;
        }
        *self.context_limit.lock() = self.configured;
        // `probe` only overwrites the list where the new listing has one of its own. What the last
        // address said its model takes is not something this one said
        *self.listed.lock() = Entry::default();
        self.switched().await;
    }

    /// Every model this endpoint says it serves, if it will say.
    ///
    /// note: an empty answer means "it did not say", not "it has none". A gateway or proxy may
    /// serve no listing at all; treating silence as denial invents a restriction nobody stated.
    pub async fn models(&self) -> Vec<String> {
        let mut listings = self.listings().await;
        self.models_in(&mut listings).await
    }

    /// [`Self::models`], out of what has already been read.
    async fn models_in(&self, listings: &mut Listings) -> Vec<String> {
        let listed = listings
            .conventional
            .as_ref()
            .map(names_in)
            .unwrap_or_default();
        if !listed.is_empty() {
            return listed;
        }
        self.native(listings)
            .await
            .map(names_in)
            .unwrap_or_default()
    }

    /// A switch of model or address asks the new one its limit and whether it serves the model,
    /// from one read of each listing.
    async fn switched(&self) {
        let mut listings = self.listings().await;
        self.probe_in(&mut listings).await;
        let listed = self.models_in(&mut listings).await;
        self.say_if_the_model_is_not_there(&listed);
    }

    /// The conventional listing, read; the native one waits until something needs it.
    async fn listings(&self) -> Listings {
        Listings {
            conventional: self
                .listing(&format!("{}/models", self.endpoint()), true)
                .await,
            native: None,
        }
    }

    /// Google's native listing, one path up from a base ending in `/openai`, read the first time
    /// it is asked for and kept; `None` for any other base, or one that did not answer.
    async fn native<'a>(&self, listings: &'a mut Listings) -> Option<&'a Value> {
        if listings.native.is_none() {
            let native = match self.endpoint().strip_suffix("/openai") {
                Some(native) => self.listing(&format!("{native}/models"), false).await,
                None => None,
            };
            listings.native = Some(native);
        }

        listings.native.as_ref()?.as_ref()
    }

    /// Sets a notice if the model is not one the endpoint lists, and clears it if it is.
    ///
    /// note: the alternative is finding out on the next request, as a 404. Switching address and
    /// model are two commands, and it is easy to do one without the other.
    ///
    /// note: a switch that finds its model still clears the slot, because what is waiting there is
    /// older than the switch, and a client that reads it afterwards would blame the model it has
    /// just moved away from.
    fn say_if_the_model_is_not_there(&self, listed: &[String]) {
        let model = self.model.lock().clone();
        *self.notice.lock() = crate::unlisted(&model, listed);
    }

    /// A model listing, if the address answers with one.
    async fn listing(&self, url: &str, bearer: bool) -> Option<Value> {
        // note: a header rather than `?key=`, which is also what Google's native API takes. A key
        // in a URL is a key in every log a proxy keeps, and what sends it here is only a base
        // ending in `/openai` - which is not only Google's
        let request = match bearer {
            true => self.client.get(url).bearer(&self.api_key),
            false => self.client.get(url).google_key(&self.api_key),
        };

        request
            .timeout(crate::ASKING)
            .send()
            .await
            .ok()?
            .json::<Value>()
            .await
            .ok()
    }

    /// Switches models, and forgets the context limit that belonged to the old one.
    ///
    /// note: a limit the caller set is not the old model's, so it is put back rather than
    /// forgotten. Replacing an explicit [`with_context_limit`](Self::with_context_limit) with the
    /// endpoint's advertised limit would measure against a number the caller already decided not to
    /// use.
    pub async fn set_model(&self, model: impl Into<String>) {
        *self.model.lock() = model.into();
        *self.context_limit.lock() = self.configured;
        *self.listed.lock() = Entry::default();
        self.switched().await;
    }

    /// Takes whatever the provider last wanted to say for itself, if anything.
    pub fn take_notice(&self) -> Option<String> {
        self.notice.lock().take()
    }

    /// Asks the provider what it knows about the model, so that the context limit the kernel
    /// reports is the real one rather than a guess.
    ///
    /// note: The round trip is worth it because every figure the status line shows about how full
    /// the context is is measured against this number. An unknown limit is reported as unknown
    /// rather than guessed at.
    pub async fn probe(&self) {
        let mut listings = self.listings().await;
        self.probe_in(&mut listings).await;
    }

    /// [`Self::probe`], out of what has already been read.
    async fn probe_in(&self, listings: &mut Listings) {
        // a limit the caller set is a decision about what to measure against, not a statement about
        // which parameters the model takes, so the listing is still worth reading. Only the limit
        // is left alone
        let settled = self.context_limit.lock().is_some();

        let base = self.endpoint();
        let mut limit = (listings.conventional.as_ref()).and_then(|body| self.listed_limit(body));

        // an OpenAI-compatible listing does not have to carry a context length, and Google's does
        // not; its native one does, one path up
        if limit.is_none() {
            limit = (self.native(listings).await).and_then(|body| self.listed_limit(body));
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

    /// Asks ollama what context length the model is actually served with.
    ///
    /// note: `/api/show` advertises the architecture's maximum - 131,072 for a llama loaded with
    /// `num_ctx` of 4,096 - and a budget against that would be wrong in the direction that matters:
    /// nothing would look full, no compactor would fire, and the server would quietly drop the
    /// front of the conversation. `/api/ps` reports what a loaded model really serves; an unloaded
    /// model yields nothing, because "unknown" is better than a wrong number.
    ///
    /// note: the address is checked for ollama before either probe, because every `/v1` base is
    /// asked and most answer `/v1/models` while saying nothing about context length. A non-ollama
    /// address has no `/api/ps`, and one that takes rather than refuses the request waits both
    /// probes - a minute and a half of startup - to answer nothing.
    async fn loaded_limit(&self, root: &str) -> Option<usize> {
        if !self.answers_like_ollama(root).await {
            return None;
        }

        if let Some(limit) = self.running_limit(root).await {
            return Some(limit);
        }

        // only a loaded model reports one, and this one is cold. Asking for it with an empty prompt
        // loads it and generates nothing: it is the model this session is about to talk to.
        //
        // note: given longer than a listing because loading is what it is for and a large model
        // takes a while to load, but still bounded, for the reason `ASKING` has one
        self.client
            .post(format!("{root}/api/generate"))
            .json(&json!({ "model": *self.model.lock(), "prompt": "" }))
            .timeout(crate::ASKING * 8)
            .send()
            .await
            .ok()?;

        self.running_limit(root).await
    }

    /// Whether this address is an ollama, asked of the version it reports.
    ///
    /// note: ollama's `/api/version` answers `{"version":"…"}` and needs no key, and it is the one
    /// question the two probes can put to any `/v1` address without loading a model or waiting for
    /// a generation. What matters is only that something answers: a proxy in front of a model
    /// server will not say so, and is then measured against the limit it declares.
    async fn answers_like_ollama(&self, root: &str) -> bool {
        // note: bounded by a timeout on the request rather than the client's own, which is unset -
        // an address that takes the connection and says nothing is the case this whole question
        // is for
        let Ok(version) = self
            .client
            .get(format!("{root}/api/version"))
            .timeout(crate::ASKING)
            .send()
            .await
        else {
            return false;
        };

        // the body under the same bound, so a server that announced an answer it never sends ends
        // the wait here rather than a second time
        tokio::time::timeout(crate::ASKING, version.json::<Value>())
            .await
            .is_ok_and(|said| said.is_ok_and(|body| body["version"].is_string()))
    }

    /// The context length ollama has a model loaded with, if it has it loaded at all.
    async fn running_limit(&self, root: &str) -> Option<usize> {
        let model = self.model.lock().clone();
        let body = self
            .client
            .get(format!("{root}/api/ps"))
            .timeout(crate::ASKING)
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
    fn listed_limit(&self, body: &Value) -> Option<usize> {
        let model = self.model.lock().clone();
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

        // the same entry carries what the model will accept, and reading it costs nothing because
        // the round trip has already happened.
        //
        // note: two names for it because two dialects publish it differently. OpenRouter's
        // `supported_parameters` lists everything a request may carry: sampling knobs, `tools`, and
        // `response_format`. Inception's `supported_sampling_parameters` lists the sampling knobs
        // only and puts the rest under `supported_features`. Whichever is present is read, because
        // an unread list leaves `/params` with nothing to say about a `top_p` the model ignores.
        //
        // note: the narrower name is a narrower answer to the same question. A non-sampling
        // parameter absent from the sampling list may still be served - `response_format` on
        // `mercury-2.5` is - so `every_parameter` is set false for that shape, and a client reading
        // `lists_every_parameter` sees such a parameter as unchecked rather than ignored.
        let sampling_only = entry["supported_parameters"].is_null();
        let mut listed = self.listed.lock();
        if let Some(names) = entry["supported_parameters"]
            .as_array()
            .or_else(|| entry["supported_sampling_parameters"].as_array())
        {
            listed.every_parameter = !sampling_only;
            listed.parameters = names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect();
        }
        // note: OpenRouter's `default_parameters` names most of what it lists and gives most of
        // them as `null`, which is a default it does not know rather than one of nothing
        if let Some(defaults) = entry["default_parameters"].as_object() {
            listed.defaults = defaults
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
        }
        listed.max_output_tokens = entry["top_provider"]["max_completion_tokens"]
            .as_u64()
            .filter(|most| *most > 0)
            .map(|most| most as usize);
        drop(listed);

        // a limit of nothing is a listing that did not know, which the kernel already declines to
        // measure against - and reported as a limit it is a window of `0` on every screen
        [
            &entry["context_length"],
            &entry["top_provider"]["context_length"],
            &entry["inputTokenLimit"],
        ]
        .into_iter()
        .find_map(|limit| limit.as_u64().filter(|limit| *limit > 0))
        .map(|limit| limit as usize)
    }
}

/// The identifiers in a listing, however that listing spells them.
fn names_in(body: &Value) -> Vec<String> {
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

#[async_trait]
impl Endpoint for OpenAiCompatible {
    fn endpoint(&self) -> String {
        self.endpoint()
    }

    fn model(&self) -> String {
        self.model()
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

impl Dialect for OpenAiCompatible {
    fn set_patience(&self, patience: std::time::Duration) {
        *self.patience.lock() = patience;
    }

    fn set_tries(&self, tries: usize) {
        self.tries.store(tries.max(1), Ordering::SeqCst);
    }

    /// The default, for chat completions: see [`Dialect::projection`]. The Responses API takes a
    /// turn as the ordered items it was, so it is projected as blocks, as the Gemini and Anthropic
    /// dialects' are.
    ///
    /// note: as in the Anthropic dialect, the budget then counts thinking that has no sealed form -
    /// another provider's - which is left out of the request; see `Anthropic::projection`.
    fn projection(&self) -> nachalnik::LinearProjector {
        match self.responses {
            true => nachalnik::LinearProjector {
                send_blocks: true,
                ..Default::default()
            },
            false => nachalnik::LinearProjector {
                send_reasoning: self.send_reasoning,
                ..Default::default()
            },
        }
    }

    fn lists_every_parameter(&self) -> bool {
        self.listed.lock().every_parameter
    }

    fn published(&self, parameter: &str) -> Published {
        let listed = self.listed.lock();
        let published = Published::default().with_default(listed.defaults.get(parameter).cloned());
        match parameter {
            // the three names this dialect has for the length of an answer, the last the
            // Responses API's
            "max_tokens" | "max_completion_tokens" | "max_output_tokens" => {
                published.with_maximum(listed.max_output_tokens.map(Into::into))
            }
            _ => published,
        }
    }
}

/// What a listing's entry says about the model beyond its context limit.
///
/// note: one lock for all of it because it is learnt from one entry and forgotten on one switch; a
/// part a switch forgot to forget is the last model's answer given for this one.
struct Entry {
    /// The parameter names it said the model takes, where it said anything.
    parameters: Vec<String>,
    /// Whether `parameters` is the whole of what the model takes; see
    /// [`Dialect::lists_every_parameter`].
    every_parameter: bool,
    /// The defaults it published for them, without the ones it published as `null`.
    defaults: Map<String, Value>,
    /// The most the model will write in one answer.
    max_output_tokens: Option<usize>,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            parameters: Vec::new(),
            every_parameter: true,
            defaults: Map::new(),
            max_output_tokens: None,
        }
    }
}

/// What an address lists, read once for everything one question of it needs.
///
/// note: the native listing is kept because a limit and a list of names both fall back to it on
/// Google's compatible endpoint, and a switch asks for both; reading it separately each time was
/// the same request twice on every `/model` and `/endpoint`.
struct Listings {
    conventional: Option<Value>,
    /// `None` until asked for; then what the native listing answered, if anything.
    native: Option<Option<Value>>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nachalnik::Provider;

    use super::*;

    /// The Responses API is asked once it is turned on, and a client that was given no name of its
    /// own is named for it.
    #[test]
    fn the_responses_api_is_asked_once_turned_on_and_named_for() {
        let plain = OpenAiCompatible::new("m", "https://example.invalid/v1", "k");
        assert!(!plain.asks_responses());
        assert_eq!(plain.info().provider, "openai-compatible");

        let responses = plain.responses(true);
        assert!(responses.asks_responses());
        assert_eq!(responses.info().provider, "openai-responses");

        let named = OpenAiCompatible::new("m", "https://example.invalid/v1", "k")
            .labelled("ours")
            .responses(true);
        assert_eq!(named.info().provider, "ours");
    }

    /// The client gives up on a silent server when a whole answer's time is up, not before.
    ///
    /// note: reqwest's default waits for ever, and a caller sharing one client between several
    /// models takes this one's word for the bound. On a paused clock against a listener that never
    /// answers, only a timer can end the wait, and timers fire in order.
    #[tokio::test(start_paused = true)]
    async fn the_offered_client_gives_up_when_a_whole_answer_would() {
        let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let at = silent.local_addr().expect("its address");
        let mut asking = Box::pin(
            OpenAiCompatible::client()
                .get(format!("http://{at}/"))
                .send(),
        );

        let early = tokio::time::timeout(WHOLE_ANSWER - Duration::from_secs(1), &mut asking).await;
        assert!(early.is_err(), "still waiting just short of the bound");
        let late = tokio::time::timeout(Duration::from_secs(2), &mut asking)
            .await
            .expect("given up just past it")
            .expect_err("nothing was said");
        assert!(late.is_timeout(), "{late:?}");
    }

    /// Serves each path its own body and `{}` to any other, and keeps every path it was asked for,
    /// so the response says which address was read and what else was wanted.
    ///
    /// note: named against [`OpenAiCompatible::recording`], which keeps the requests it sent rather
    /// than the paths a server was asked for.
    async fn watching(
        routes: &[(&'static str, &'static str)],
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let routes = routes.to_vec();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        let asked = Arc::new(Mutex::new(Vec::new()));
        let keeping = asked.clone();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut socket, _)) = listener.accept().await {
                // to the blank line, so a head that arrived in two reads is not a path cut short
                let mut head = Vec::new();
                let mut chunk = [0u8; 4096];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&head);
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                let body = routes
                    .iter()
                    .find(|(route, _)| *route == path)
                    .map_or("{}", |(_, body)| *body);
                // kept before the answer goes out, so a client that has been answered has been
                // recorded
                keeping.lock().push(path.to_owned());
                // note: `Connection: close` because every connection is closed after one answer.
                // Otherwise the client pools the connection, and a request sent on it before the
                // close is read fails at once, so the probe comes back with nothing
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            }
        });

        (format!("http://{at}"), asked)
    }

    /// [`watching`], for a test that only wants the answers.
    async fn routed(routes: &[(&'static str, &'static str)]) -> String {
        watching(routes).await.0
    }

    /// note: the two shapes are quoted from the real endpoints, trimmed to the fields read. The
    /// Inception one publishes only the sampling list, and left unread it leaves `/params` with
    /// nothing to say about a parameter `mercury-2.5` ignores.
    #[tokio::test]
    async fn a_listing_is_read_under_either_name_for_what_the_model_takes() {
        let openrouter = r#"{"data":[{"id":"m","context_length":128000,
            "supported_parameters":["temperature","top_p","tools"]}]}"#;
        let inception = r#"{"data":[{"id":"m","context_length":260000,
            "supported_sampling_parameters":["temperature","stop"],
            "supported_features":["tools","json_mode"]}]}"#;

        for (shape, body, limit, takes) in [
            (
                "supported_parameters",
                openrouter,
                128_000,
                vec!["temperature", "top_p", "tools"],
            ),
            (
                "supported_sampling_parameters",
                inception,
                260_000,
                vec!["temperature", "stop"],
            ),
        ] {
            let provider =
                OpenAiCompatible::new("m", routed(&[("/models", body)]).await, "no key needed");
            provider.probe().await;

            let info = provider.info();
            assert_eq!(info.context_limit, Some(limit), "{shape}");
            assert_eq!(info.parameters, takes, "{shape}");
        }
    }

    /// A default published as `null` is unknown to the listing, and the cap on an answer bounds
    /// both of this dialect's names for it.
    #[tokio::test]
    async fn a_listing_says_what_it_publishes_about_each_parameter() {
        let body = r#"{"data":[{"id":"m","context_length":128000,
            "top_provider":{"context_length":128000,"max_completion_tokens":32768},
            "supported_parameters":["max_tokens","temperature","top_p"],
            "default_parameters":{"temperature":1,"top_p":null}}]}"#;
        let provider =
            OpenAiCompatible::new("m", routed(&[("/models", body)]).await, "no key needed");
        provider.probe().await;

        assert_eq!(provider.published("temperature").default, Some(json!(1)));
        assert_eq!(provider.published("top_p"), Published::default());
        for name in ["max_tokens", "max_completion_tokens"] {
            assert_eq!(
                provider.published(name).maximum,
                Some(32768.into()),
                "{name}"
            );
        }
        assert_eq!(provider.published("temperature").maximum, None);
        assert_eq!(provider.info().max_output_tokens, Some(32768));
    }

    /// An endpoint that publishes no parameter names lists none. Silence is not a claim that the
    /// model refuses everything; ollama and a bare proxy both answer this way.
    #[tokio::test]
    async fn a_listing_that_names_no_parameters_leaves_the_list_empty() {
        let bare = r#"{"data":[{"id":"m","context_length":4096}]}"#;
        let provider =
            OpenAiCompatible::new("m", routed(&[("/models", bare)]).await, "no key needed");
        provider.probe().await;

        assert_eq!(provider.info().context_limit, Some(4096));
        assert!(provider.info().parameters.is_empty());
    }

    /// A listing that gives a limit of nothing has given no limit.
    ///
    /// note: `0` was reported as the limit; clients measured a budget against it, but the kernel
    /// refused nothing by it.
    #[tokio::test]
    async fn a_listed_limit_of_nothing_is_no_limit() {
        for (body, limit) in [
            (r#"{"data":[{"id":"m","context_length":0}]}"#, None),
            (
                r#"{"data":[{"id":"m","context_length":0,"top_provider":{"context_length":8192}}]}"#,
                Some(8192),
            ),
        ] {
            let provider =
                OpenAiCompatible::new("m", routed(&[("/models", body)]).await, "no key needed");
            provider.probe().await;

            assert_eq!(provider.info().context_limit, limit, "{body}");
        }
    }

    /// An ollama is measured against the context length its running model is served with, read off
    /// `/api/ps`, because its listing publishes none. `/api/version` is asked first, and only an
    /// ollama is measured this way.
    ///
    /// note: `/api/show` advertises the architecture's maximum instead; measured against that,
    /// nothing would ever look full while the server dropped the front of the conversation.
    #[tokio::test]
    async fn an_ollama_is_measured_by_what_its_running_model_is_served_with() {
        let (at, asked) = watching(&[
            ("/v1/models", r#"{"data":[{"id":"m"}]}"#),
            ("/api/version", r#"{"version":"0.12.0"}"#),
            (
                "/api/ps",
                r#"{"models":[{"name":"m:latest","context_length":4096}]}"#,
            ),
        ])
        .await;
        let provider = OpenAiCompatible::new("m", format!("{at}/v1"), "no key needed");
        provider.probe().await;

        assert_eq!(provider.info().context_limit, Some(4096));
        assert!(asked.lock().contains(&"/api/ps".to_owned()));
    }

    /// An ollama holding no model is asked to load it, and a load that takes its time is waited
    /// out: the limit the loaded model is served with is what the session is measured against.
    ///
    /// note: on the real clock, since what is held is a bound on a socket. The load takes four
    /// seconds, against the two minutes loading is given and the under two seconds a bound cut
    /// down to a listing's would be, so the margin is wide either way.
    #[tokio::test]
    async fn an_ollama_is_asked_to_load_a_model_that_takes_its_time() {
        // the load: longer than a listing is given, shorter than loading a model is given
        const LOADING: Duration = Duration::from_secs(4);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        // cold until the empty prompt has been answered, which is what makes `/api/ps` report
        // nothing the first time it is asked
        let loaded = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let warming = loaded.clone();
        tokio::spawn(async move {
            use std::sync::atomic::Ordering;
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut head = Vec::new();
                let mut chunk = [0u8; 4096];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let path = String::from_utf8_lossy(&head)
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();

                let body = match path.as_str() {
                    "/v1/models" => r#"{"data":[{"id":"m"}]}"#,
                    "/api/version" => r#"{"version":"0.12.0"}"#,
                    // the model is loaded here, and only says so afterwards
                    "/api/generate" => {
                        tokio::time::sleep(LOADING).await;
                        warming.store(true, Ordering::SeqCst);
                        r#"{"done":true}"#
                    }
                    // nothing while the model is cold, which is what sends `loaded_limit` to the
                    // empty prompt in the first place
                    "/api/ps" => match loaded.load(Ordering::SeqCst) {
                        true => r#"{"models":[{"name":"m:latest","context_length":4096}]}"#,
                        false => r#"{"models":[]}"#,
                    },
                    _ => "{}",
                };

                // `Connection: close`, as `watching` says, for the same reason
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            }
        });

        let provider = OpenAiCompatible::new("m", format!("http://{at}/v1"), "no key needed");
        provider.probe().await;

        assert_eq!(
            provider.info().context_limit,
            Some(4096),
            "the limit the loaded model is served with, or none where the load was given up on"
        );
    }

    /// A `/v1` address whose listing names no context length is asked whether it is an ollama, and
    /// one that does not answer like one is left alone: nothing asks it for `/api/ps`, and nothing
    /// sends it the empty prompt that loads a model - the second given two minutes to load a model
    /// an address that is not ollama does not have, and both to answer nothing.
    #[tokio::test]
    async fn an_address_that_does_not_say_it_is_ollama_is_asked_for_nothing_else() {
        let (at, asked) = watching(&[("/v1/models", r#"{"data":[{"id":"m"}]}"#)]).await;
        let provider = OpenAiCompatible::new("m", format!("{at}/v1"), "no key needed");
        provider.probe().await;

        let asked = asked.lock().clone();
        assert_eq!(
            asked,
            ["/v1/models", "/api/version"],
            "what else it was asked for"
        );
        assert_eq!(provider.info().context_limit, None);
    }

    /// A switch to an address whose conventional listing says nothing asks the native listing once,
    /// for the limit and the names.
    #[tokio::test]
    async fn a_switch_asks_for_the_native_listing_once() {
        let (at, asked) = watching(&[(
            "/models",
            r#"{"models":[{"name":"models/gemini-x","inputTokenLimit":1048576}]}"#,
        )])
        .await;
        let provider = OpenAiCompatible::new("gemini-x", "http://127.0.0.1:1", "k");
        provider.set_endpoint(format!("{at}/openai"), None).await;

        let native = asked
            .lock()
            .iter()
            .filter(|path| *path == "/models")
            .count();
        assert_eq!(native, 1, "{:?}", asked.lock());
        // and the one answer is read for the limit, not only asked for
        assert_eq!(provider.info().context_limit, Some(1_048_576));
    }

    /// Where the conventional listing says nothing, a base ending in `/openai` is asked one path
    /// up, where Google keeps its native listing.
    #[tokio::test]
    async fn a_listing_that_says_nothing_is_asked_for_one_path_up() {
        let at = routed(&[("/models", r#"{"models":[{"name":"models/gemini-x"}]}"#)]).await;
        let provider = OpenAiCompatible::new("gemini-x", format!("{at}/openai"), "k");

        assert_eq!(provider.models().await, ["gemini-x"]);
    }
}
