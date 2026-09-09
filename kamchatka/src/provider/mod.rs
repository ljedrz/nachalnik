//! A [`Provider`] that speaks the OpenAI chat-completions dialect over HTTP, streamed.
//!
//! note: There is nothing terminal-specific in here, and nothing kernel-specific either - it is
//! the HTTP that every one of these APIs happens to agree on. It reports fragments through the
//! [`nachalnik::DeltaSink`] and prints nothing: the screen belongs to the terminal, and a
//! provider that wrote to it would be drawing over the frame.

use std::{
    env,
    sync::{Arc, atomic::AtomicUsize},
};

use nachalnik::{BoxError, LinearProjector, Provider, async_trait};
use parking_lot::Mutex;
use serde_json::{Value, json};

pub(crate) mod waiting;
mod wire;

/// Parameters that stop a stream being a stream, and what each one does instead.
///
/// note: a stream in this dialect is text arriving in order, and every client of it appends. One
/// parameter breaks that, and it is not a fringe one: Inception's `diffusing`, documented as
/// "show the diffusion effect in the streamed response", sends the *whole answer again* in
/// `delta.content` at each denoising step. Measured against `mercury-2.5`: four fragments of 339,
/// 339, 427 and 435 characters, the first three noise - `ThR rMmchatka tYS>rf5Ta in Russia` - and
/// the last the finished paragraph. Appended, as this program and every other OpenAI-dialect
/// client appends them, a 435-character answer became 1,540 characters of drafts, in the
/// transcript, in the context, in the token count and in the session log.
///
/// note: a warning rather than a refusal, and rather than a reader that tries to cope. Parameters
/// are the person's to set and are carried to the provider verbatim - that is the runtime's rule
/// and this program does not get to break it - and there is nothing on the wire that marks a
/// snapshot as one, so a reader coping would be a reader guessing. What this program owes is to
/// not be quietly wrong about its own record, which is the whole of its case.
pub(crate) const NOT_A_STREAM: [(&str, &str); 1] = [(
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

/// Whether an endpoint is one that keeps a ranking of the apps calling it.
///
/// note: on the authority rather than on the whole address, so a self-hosted path or a regional
/// subdomain still counts, and `openrouter.ai.example.com` does not.
fn ranks_apps(host: &str) -> bool {
    let host = host.split(':').next().unwrap_or(host);
    host == "openrouter.ai" || host.ends_with(".openrouter.ai")
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
            every_parameter: Mutex::new(true),
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

    /// Whether [`nachalnik::ModelInfo::parameters`] is the whole of what the model takes, or only
    /// the part of it this endpoint publishes.
    ///
    /// note: it decides what may be said about a parameter that is *not* on the list, and the two
    /// answers are different claims. An exhaustive list settles it - the parameter is sent, the
    /// model does not take it, and it is ignored, which is the thing `/params` exists to say. A
    /// list of the sampling knobs alone settles nothing: `reasoning_effort` is absent from
    /// `mercury-2.5`'s and is read all the same, validated hard enough that a bad value comes back
    /// a 400. Reporting that one as ignored would be inventing a restriction out of a list that
    /// never claimed to be complete, which is the failure this crate takes seriously everywhere
    /// else it reads somebody's metadata.
    ///
    /// note: defaulted to `true` because a dialect that publishes one list of everything is the
    /// ordinary case and the one this was written against. An endpoint that knows better says so.
    fn lists_every_parameter(&self) -> bool {
        true
    }

    /// The projection this dialect can carry.
    ///
    /// note: it is answered here, beside the `to_wire` that has to honour it, because the two
    /// were decided in different places and drifted. The budget is counted over the messages the
    /// projector produced - which is what makes it the size of the request rather than the size
    /// of the context - so a projector that hands over something the wire format then drops does
    /// not merely waste the effort. It charges the person for bytes that never leave the process,
    /// and goes on doing it for as long as those messages are in the context.
    fn projection(&self) -> LinearProjector {
        LinearProjector {
            // note: `to_wire` has never put an assistant turn's thinking on the wire, and cannot:
            // most endpoints speaking this dialect reject a message carrying a field they do not
            // know, and there is no agreed name for that one. So sending it was never on offer -
            // only paying for it was. The turn keeps its reasoning either way: it is in the
            // record, on the context tab, and prunable like everything else.
            send_reasoning: false,
            ..Default::default()
        }
    }

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
