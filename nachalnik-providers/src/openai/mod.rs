//! A [`Provider`](nachalnik::Provider) that speaks the OpenAI chat-completions dialect over
//! HTTP, streamed.
//!
//! note: there is nothing kernel-specific in here and nothing client-specific either - it is the
//! HTTP that every one of these APIs happens to agree on, and each oddity below is one a server
//! really sent.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use nachalnik::{ModelRequest, async_trait};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::{Endpoint, install_crypto, same_model, waiting::WHOLE_ANSWER};

mod wire;

/// Parameters that stop a stream being a stream, and what each one does instead.
///
/// note: a stream in this dialect is text arriving in order, and every client of it appends. One
/// parameter breaks that, and it is not a fringe one: Inception's `diffusing`, documented as
/// "show the diffusion effect in the streamed response", sends the *whole answer again* in
/// `delta.content` at each denoising step. Measured against `mercury-2.5`: four fragments of 339,
/// 339, 427 and 435 characters, the first three noise - `ThR rMmchatka tYS>rf5Ta in Russia` - and
/// the last the finished paragraph. Appended, as this crate and every other OpenAI-dialect client
/// appends them, a 435-character answer became 1,540 characters of drafts - in the answer, in the
/// context, in the token count and in whatever the client wrote down.
///
/// note: a list to warn from rather than a refusal, and rather than a reader that tries to cope.
/// Parameters are the caller's to set and are carried to the provider verbatim - that is the
/// runtime's rule and a provider does not get to break it - and there is nothing on the wire that
/// marks a snapshot as one, so a reader coping would be a reader guessing. What a client can do
/// is say so before the request, which is what this is for.
pub const NOT_A_STREAM: [(&str, &str); 1] = [(
    "diffusing",
    "sends the whole answer again at each denoising step, and a stream is read here as text \
     arriving in order - so the answer, the context and the log would keep every draft",
)];

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
    /// Whether [`Self::parameters`] is the whole of what the model takes; see
    /// [`Endpoint::lists_every_parameter`].
    every_parameter: Mutex<bool>,
    /// The limit the caller set by hand, if it set one, kept so that changing model or endpoint
    /// puts it back rather than dropping it.
    ///
    /// note: separate from [`Self::context_limit`], which is what [`Self::probe`] discovered.
    /// Somebody who has decided what to measure against has decided it about the session, not
    /// about whichever model the session happens to be asking, and a `set_model` that quietly
    /// replaced it with whatever the endpoint advertises would be overruling them.
    configured: Option<usize>,
    /// How many times this provider has backed off *since the last answer*, so that a busy
    /// server cannot be retried forever by a session that keeps making new requests. Reset by
    /// every request that succeeds.
    backoff: AtomicUsize,
    /// How many HTTP requests this has made in its life, retries counted separately. Never reset:
    /// what it answers is "what did this cost", which a run wants at the end of it.
    attempts: AtomicUsize,
    /// What the last retry was about, taken by [`Endpoint::take_notice`], for a client that has
    /// somewhere to put it and no stderr to spare.
    notice: Mutex<Option<String>>,
    /// Who to say these requests are on behalf of, where the endpoint asks. Set once, at
    /// construction: it is a property of the program making the request, not of the session.
    attribution: Option<Attribution>,
    /// What this endpoint is called, as [`nachalnik::ModelInfo::provider`] reports it.
    ///
    /// note: worth setting when there are several. A panel comparing four models through three
    /// endpoints has three providers whose `info()` all said `openai-compatible`, which told the
    /// reader nothing about which was which.
    label: String,
    /// Whether to ask for a streamed answer at all; see [`Self::streaming`].
    stream: bool,
    /// Every request this was asked to send, in order, when [`Self::recording`] is on.
    requests: Mutex<Vec<ModelRequest>>,
    /// Whether to keep them.
    ///
    /// note: off by default, and it has to be: a session that runs all afternoon would hold every
    /// request it ever made, which is the context several times over. What it is for is a test or
    /// a run that wants to assert on what actually went out rather than on what it believes went
    /// out, and those know they want it.
    recording: bool,
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

/// Whether an endpoint is one that keeps a ranking of the apps calling it.
///
/// note: on the authority rather than on the whole address, so a self-hosted path or a regional
/// subdomain still counts, and `openrouter.ai.example.com` does not.
fn ranks_apps(host: &str) -> bool {
    let host = host.split(':').next().unwrap_or(host);
    host == "openrouter.ai" || host.ends_with(".openrouter.ai")
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
            context_limit: Mutex::new(None),
            configured: None,
            parameters: Mutex::new(Vec::new()),
            every_parameter: Mutex::new(true),
            backoff: AtomicUsize::new(0),
            attempts: AtomicUsize::new(0),
            notice: Mutex::new(None),
            attribution: None,
            label: "openai-compatible".to_owned(),
            stream: true,
            requests: Mutex::new(Vec::new()),
            recording: false,
        }
    }

    /// A client with the timeout a slow endpoint wants, which is longer than reqwest's default.
    ///
    /// note: also where the cryptography `rustls` will use is installed, for a caller that builds
    /// its own client and would otherwise find out at the first `https://`. See
    /// [`crate::install_crypto`].
    ///
    /// note: worth choosing deliberately, because reqwest's timeout covers the *whole* request -
    /// connect, generate and body - so it is an upper bound on how long a model may think and not
    /// only on how long a dead socket may hang. Set it below what the work takes and every long
    /// answer arrives as `error decoding response body`, which looks like a network fault and is
    /// not one. The default here is ten minutes for that reason; what catches a socket with
    /// nobody on the other end is the silence watch, which is counted rather than told.
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

    /// Sends through a client of the caller's own, so that several models on one host share a
    /// connection pool - and so that the timeout is the caller's to set.
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

    /// Whether to ask for a streamed answer at all. On unless turned off.
    ///
    /// note: a whole answer is worth having where nothing is watching one arrive - a benchmark
    /// run, a batch, a test - and it is the only way to reach some endpoints' non-streaming
    /// paths, which are not always the same code as their streaming ones. What it costs is every
    /// [`nachalnik::DeltaSink`] fragment and, with them, the ability to stop a turn partway: an
    /// answer that arrives in one piece has no middle to interrupt.
    #[must_use]
    pub fn streaming(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
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

    /// How many HTTP requests this has made, retries counted separately.
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    /// Says which app these requests are being made on behalf of.
    ///
    /// note: off unless a caller asks for it, and sent only to the endpoint that reads it. The
    /// headers name the program, never the person running it or what they asked - but a
    /// `HTTP-Referer` volunteered to whatever address the caller has pointed this at is still
    /// something the person running it did not ask to send, and the address is a setting.
    pub fn on_behalf_of(mut self, url: impl Into<String>, title: impl Into<String>) -> Self {
        self.attribution = Some(Attribution {
            url: url.into(),
            title: title.into(),
        });
        self
    }

    /// Measures against this limit rather than against whatever the endpoint advertises.
    ///
    /// note: for the two cases [`probe`](Self::probe) cannot settle - an endpoint that publishes
    /// no context length at all, and one whose published length is not what the model is actually
    /// being served with. It is sticky: [`set_model`](Self::set_model) and
    /// [`set_endpoint`](Self::set_endpoint) put it back, because it is a decision about what to
    /// measure, not a fact about one model.
    ///
    /// note: `None` is not "no limit", it is "ask the endpoint", which is the default. A caller
    /// reading this out of its own configuration can pass the `Option` straight through.
    pub fn with_context_limit(mut self, limit: Option<usize>) -> Self {
        self.configured = limit;
        *self.context_limit.get_mut() = limit;
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

    /// Which model is being asked.
    pub fn model(&self) -> String {
        self.model.lock().clone()
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
        *self.context_limit.lock() = self.configured;
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
            "{model} is not one of the {} models this address lists ({}{})",
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
    /// than forgotten. Without this, switching model quietly replaced an explicit
    /// [`with_context_limit`](Self::with_context_limit) with whatever the endpoint advertises -
    /// which is the number the caller had already decided not to measure against.
    pub async fn set_model(&self, model: impl Into<String>) {
        *self.model.lock() = model.into();
        *self.context_limit.lock() = self.configured;
        self.parameters.lock().clear();
        *self.every_parameter.lock() = true;
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
        //
        // note: two names for it, because two dialects publish it differently and neither is
        // wrong. OpenRouter's `supported_parameters` lists everything a request may carry, sampling
        // knobs and `tools` and `response_format` together; Inception's
        // `supported_sampling_parameters` lists the sampling knobs only and puts the rest under
        // `supported_features`. Reading whichever is present beats reading one and calling the
        // other silence: an endpoint that published its list and had it go unread is an endpoint
        // where `/params` says nothing about a `top_p` the model ignores, which is the single thing
        // that check exists to say.
        //
        // note: what the narrower name costs is a narrower answer to the same question. Set a
        // *non*-sampling parameter against an endpoint that publishes only the sampling ones and it
        // is reported as unlisted - `response_format` on `mercury-2.5` is exactly that, absent from
        // the sampling list and served all the same. The message is worded "does not list", which
        // stays true either way; it is the "and ignored" beside it that is guessing, and only for
        // that class. The trade is one class of parameter over-reported against every class going
        // unchecked, which is the position this was in.
        let sampling_only = entry["supported_parameters"].is_null();
        if let Some(listed) = entry["supported_parameters"]
            .as_array()
            .or_else(|| entry["supported_sampling_parameters"].as_array())
        {
            *self.every_parameter.lock() = !sampling_only;
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
impl Endpoint for OpenAiCompatible {
    fn endpoint(&self) -> String {
        self.endpoint()
    }

    fn model(&self) -> String {
        self.model()
    }

    fn lists_every_parameter(&self) -> bool {
        *self.every_parameter.lock()
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

#[cfg(test)]
mod tests {
    use nachalnik::Provider;

    use super::*;

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

    /// Serves one model listing and closes, so a `probe` reads a real HTTP response off a real
    /// socket rather than a parsed literal.
    async fn listing(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut discard = [0u8; 4096];
                let _ = socket.read(&mut discard).await;
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            }
        });

        format!("http://{at}")
    }

    /// note: the two shapes are quoted from what the two endpoints really answer, trimmed to the
    /// fields being read. The Inception one is the case that prompted this: it published a list,
    /// nothing read it, and `/params` had nothing to say about a parameter `mercury-2.5` ignores.
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
            let provider = OpenAiCompatible::new("m", listing(body).await, "no key needed");
            provider.probe().await;

            let info = provider.info();
            assert_eq!(info.context_limit, Some(limit), "{shape}");
            assert_eq!(info.parameters, takes, "{shape}");
        }
    }

    /// An endpoint that publishes neither name says nothing, and silence is not a claim that the
    /// model refuses everything - ollama and a bare proxy both answer this way.
    #[tokio::test]
    async fn a_listing_that_names_no_parameters_leaves_the_list_empty() {
        let bare = r#"{"data":[{"id":"m","context_length":4096}]}"#;
        let provider = OpenAiCompatible::new("m", listing(bare).await, "no key needed");
        provider.probe().await;

        assert_eq!(provider.info().context_limit, Some(4096));
        assert!(provider.info().parameters.is_empty());
    }
}
