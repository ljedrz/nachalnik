//! Where this program's requests go, which key pays for them, and what it says about itself.
//!
//! note: the providers themselves are [`nachalnik_providers`], a crate of their own, and they
//! read no environment at all - where to send a request and what to measure it against are the
//! caller's to decide. This is that caller: four variables, documented in `--help` and in the
//! README, and one place that turns them into a provider. A dialect each, because the address
//! they default to is the one thing the two do not share.

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
/// note: what goes out is the name of the program and nothing else - not the key, not the model,
/// not a word of what was asked - and it goes only to OpenRouter. `KAMCHATKA_NO_ATTRIBUTION`
/// turns it off, because a program that names its user's tooling to a third party should say so
/// and let them stop it.
const APP_URL: &str = "https://github.com/ljedrz/nachalnik/tree/HEAD/kamchatka";
const APP_TITLE: &str = "kamchatka";

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
        provider = provider.on_behalf_of(APP_URL, APP_TITLE);
    }

    let provider = Arc::new(provider);
    provider.probe().await;

    Ok(provider)
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
