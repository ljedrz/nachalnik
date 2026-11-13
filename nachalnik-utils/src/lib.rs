//! Environment variables read by `nachalnik`'s examples and live suites. Not published; not API.
//!
//! note: the provider is [`nachalnik_providers`], which is published and used by `kamchatka`. This
//! crate handles endpoint, API key, and models; a provider does not move back here.
//!
//! note: it is a crate rather than a module because `nachalnik`'s examples and live suite,
//! `nachalnik-eval`'s, and `nachalnik-mcp`'s live test all need it. Separate copies made the
//! providers diverge. It is a dev-dependency of those crates only, so it stays at `0.0.0` and
//! unpublished: cargo omits versionless dev-dependencies from published manifests.

#![deny(unsafe_code)]
#![deny(missing_docs)]

use std::{env, sync::Arc};

use nachalnik::BoxError;
use nachalnik_providers::OpenAiCompatible;

/// The API key, under whichever of the documented names it is set, or none.
///
/// note: Deliberately not `OPENAI_API_KEY`, which plenty of people have exported for other
/// reasons. Spending somebody's credits as a side effect of `cargo test` would be a poor way to
/// demonstrate a crate about not doing things behind the user's back.
///
/// note: and `OPENROUTER_API_KEY` only for OpenRouter. Pointed anywhere else - Google, a local
/// llama-server - only `NACHALNIK_API_KEY` is read, so an OpenRouter key somebody happens to have
/// exported is never sent to another endpoint as the bearer.
///
/// note: no key is an empty one, which sends no header, except at OpenRouter, which refuses every
/// request without one. A server on this machine checks none unless it was started with one, and
/// a hosted service that does check refuses the request, which spends nothing.
pub fn api_key() -> Result<String, BoxError> {
    let base = base_url()?;
    match nachalnik_providers::is_openrouter(&base) {
        true => env::var("OPENROUTER_API_KEY")
            .or_else(|_| env::var("NACHALNIK_API_KEY"))
            .map_err(|_| "set OPENROUTER_API_KEY or NACHALNIK_API_KEY".into()),
        false => Ok(env::var("NACHALNIK_API_KEY").unwrap_or_default()),
    }
}

/// The endpoint to talk to, which `NACHALNIK_BASE_URL` names.
///
/// note: no default. Any address chosen here would be a service picked for everybody who runs an
/// example or a live suite, so without one they refuse or skip, saying which variable to set.
pub fn base_url() -> Result<String, BoxError> {
    env::var("NACHALNIK_BASE_URL").map_err(|_| {
        "set NACHALNIK_BASE_URL to the address of the server to talk to, e.g. \
         http://127.0.0.1:8080/v1 for a local llama-server"
            .into()
    })
}

/// The context limit named in the environment, for a provider that will not say.
pub fn context_limit() -> Option<usize> {
    env::var("NACHALNIK_CONTEXT_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
}

/// The app these requests are made on behalf of, if the environment names one.
///
/// note: both halves or neither. A `NACHALNIK_APP_URL` with no title attributes the app to a bare
/// address, a worse entry in somebody's rankings than no entry at all.
///
/// note: off unless asked for, like the header it feeds. Naming an app claims which program spends
/// the tokens; the honest default for a benchmark is no claim, not crediting whatever program
/// shares its repository.
pub fn attribution() -> Option<(String, String)> {
    let url = env::var("NACHALNIK_APP_URL").ok()?;
    let title = env::var("NACHALNIK_APP_TITLE").ok()?;

    Some((url, title))
}

/// The one model a live suite asks, which `NACHALNIK_TEST_MODEL` names.
///
/// note: here rather than in each suite because the variable name is the shared fact; spelling it
/// by hand costs a run without failing. `NACHALNIK_MODEL` is not read.
///
/// note: no default, for the reason the address has none: a model name belongs to one service,
/// so any default picks that service's model for everybody. A llama-server answers with whatever
/// it loaded, whatever the name, so any name does there.
pub fn test_model() -> Result<String, BoxError> {
    env::var("NACHALNIK_TEST_MODEL").map_err(|_| {
        "set NACHALNIK_TEST_MODEL to a model the endpoint serves; a llama-server takes any name"
            .into()
    })
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

/// A provider built from the environment, on a client with the timeout a slow endpoint wants.
///
/// note: not labelled and not probed. What to call it is the caller's - a suite comparing two
/// endpoints wants different names for them than a benchmark writing one into a report - and
/// probing is a round trip a caller may not want to pay for.
pub fn provider(model: &str) -> Result<OpenAiCompatible, BoxError> {
    let mut provider = OpenAiCompatible::new(model, base_url()?, api_key()?)
        .with_client(OpenAiCompatible::client())
        .with_context_limit(context_limit());
    if let Some((url, title)) = attribution() {
        provider = provider.on_behalf_of(url, title);
    }

    Ok(provider)
}

/// One provider per model, sharing a connection pool; each is asked its context limit.
///
/// note: one client shared between them rather than one each, so a panel comparing models on one
/// host opens one set of connections rather than one per model.
pub async fn providers(models: &[String]) -> Result<Vec<Arc<OpenAiCompatible>>, BoxError> {
    let client = OpenAiCompatible::client();

    let mut built = Vec::new();
    for model in models {
        let provider = provider(model)?.with_client(client.clone());
        provider.probe().await;
        built.push(Arc::new(provider));
    }

    Ok(built)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Models named on the command line are the ones used, as given, and the environment is not
    /// read for any.
    ///
    /// note: the flags rather than `NACHALNIK_MODELS`, because setting a variable in a test is a
    /// race with every other test reading one; the variable's half is what a run with no flags
    /// gets, and a flag given is a run that asked for exactly these.
    #[test]
    fn the_models_named_by_flags_are_the_ones_used() {
        let named = vec!["a/model".to_owned(), "b/model".to_owned()];
        assert_eq!(models(named.clone()), named);
    }
}
