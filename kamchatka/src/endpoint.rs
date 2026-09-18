//! Where this program's requests go, which key pays for them, and what it says about itself.
//!
//! note: the providers themselves are [`nachalnik_providers`], a crate of their own, and they
//! read no environment at all - where to send a request and what to measure it against are the
//! caller's to decide. This is that caller: four variables, documented in `--help` and in the
//! README, and one place that turns them into a provider. A dialect each, because the address
//! they default to is the one thing the two do not share.
//!
//! note: named for what it settles rather than for what it hands back. It was `provider`, which
//! was the truth while the dialects were files in this crate and stopped being it the day they
//! became [`nachalnik_providers`] - and a module called `provider` next to a crate of providers
//! reads as the place one is implemented rather than the place one is addressed.

use std::{env, sync::Arc};

use nachalnik::BoxError;
use nachalnik_providers::{Gemini, OpenAiCompatible, gemini::DEFAULT_BASE_URL};

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
/// note: what goes out is the name of the program and what kind of program it is - not the key,
/// not the model, not a word of what was asked - and it goes only to OpenRouter.
/// `KAMCHATKA_NO_ATTRIBUTION` turns it off, because a program that names its user's tooling to a
/// third party should say so and let them stop it.
const APP_URL: &str = "https://github.com/ljedrz/nachalnik/tree/HEAD/kamchatka";
const APP_TITLE: &str = "kamchatka";

/// Which of OpenRouter's categories the app page is filed under, which is what puts it in the
/// marketplace rather than only in the rankings.
///
/// note: two, because two per request is the documented limit, and these are the two that are
/// simply true - `cli-agent` is "terminal-based coding assistants" in their own words, and
/// `programming-app` is the wider one it also is. The rest of the coding group is somebody else:
/// this is not an editor plugin, it does not run in anybody's cloud, and it builds no apps.
///
/// note: nothing checks the spelling, here or in the provider, because OpenRouter drops a category
/// it does not recognise without an error. What catches a typo is opening the page the attribution
/// built and seeing what it says.
const APP_CATEGORIES: [&str; 2] = ["cli-agent", "programming-app"];

/// The context limit somebody set by hand, if they set one.
pub fn configured_limit() -> Option<usize> {
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
    let mut provider =
        OpenAiCompatible::new(model, base_url(), api_key()?).with_context_limit(configured_limit());
    if env::var_os("KAMCHATKA_NO_ATTRIBUTION").is_none() {
        provider = provider
            .on_behalf_of(APP_URL, APP_TITLE)
            .filed_under(APP_CATEGORIES);
    }

    let provider = Arc::new(provider);
    provider.probe().await;

    Ok(provider)
}

/// The advisor: a second model, asked about tool calls rather than about turns.
///
/// note: its own key under its own name, and no fallback to the three above. Those are one
/// account paying for a conversation; this is a different service with a different key, and a
/// program that reached for `KAMCHATKA_API_KEY` here would send somebody's OpenRouter key to
/// TypeSafe the first time they turned this on.
///
/// note: nothing in here is read unless `--advise` was asked for. The flag is what decides
/// whether a tool's arguments leave the machine at all - see `tools::advice` - and a key sitting
/// in the environment is not a decision to send them.
#[cfg(feature = "advise")]
pub mod advise {
    use nachalnik_providers::typesafe::{self, Jev};

    use super::*;

    /// The advisor's key, under either of the documented names.
    pub fn api_key() -> Result<String, BoxError> {
        env::var("KAMCHATKA_TYPESAFE_API_KEY")
            .or_else(|_| env::var("TYPESAFE_API_KEY"))
            .map_err(|_| {
                "--advise needs a key: set KAMCHATKA_TYPESAFE_API_KEY (or TYPESAFE_API_KEY)".into()
            })
    }

    /// Which model answers; TypeSafe's own default unless told otherwise.
    pub fn model() -> String {
        env::var("KAMCHATKA_TYPESAFE_MODEL").unwrap_or_else(|_| typesafe::DEFAULT_MODEL.to_owned())
    }

    /// The endpoint to talk to; TypeSafe's own unless told otherwise.
    pub fn base_url() -> String {
        env::var("KAMCHATKA_TYPESAFE_BASE_URL")
            .unwrap_or_else(|_| typesafe::DEFAULT_BASE_URL.to_owned())
    }

    /// Builds the advisor from the environment, checking that the model it names is served.
    ///
    /// note: the listing is asked for here rather than on the first refusal, for the reason
    /// `Gemini::probe` is called at startup: a model name that is not served comes back a 400,
    /// and the moment to find that out is before a session is running rather than the first time
    /// a permission question depends on it.
    pub async fn connect() -> Result<Arc<Jev>, BoxError> {
        let jev = Arc::new(Jev::new(model(), base_url(), api_key()?));
        jev.probe().await;

        Ok(jev)
    }
}

/// The same four variables, pointed at Google's own API.
pub mod gemini {
    use super::*;

    /// The endpoint to talk to; Google's own unless told otherwise.
    pub fn base_url() -> String {
        env::var("KAMCHATKA_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned())
    }

    /// Builds a provider from the environment, asking the endpoint what the model's limit is.
    ///
    /// note: no attribution. It is OpenRouter that keeps a ranking of the apps calling it, and
    /// there is nothing to tell Google that it has asked for.
    pub async fn connect(model: impl Into<String>) -> Result<Arc<Gemini>, BoxError> {
        let provider = Arc::new(
            Gemini::new(model, base_url(), api_key()?).with_context_limit(configured_limit()),
        );
        provider.probe().await;

        Ok(provider)
    }
}
