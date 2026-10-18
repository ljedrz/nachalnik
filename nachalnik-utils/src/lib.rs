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

/// The API key, under whichever of the documented names it is set.
///
/// note: Deliberately not `OPENAI_API_KEY`, which plenty of people have exported for other
/// reasons. Spending somebody's credits as a side effect of `cargo test` would be a poor way to
/// demonstrate a crate about not doing things behind the user's back.
///
/// note: and `OPENROUTER_API_KEY` only for OpenRouter. Pointed anywhere else - Google, a local
/// ollama - only `NACHALNIK_API_KEY` is read, so an OpenRouter key somebody happens to have
/// exported is never sent to another endpoint as the bearer.
pub fn api_key() -> Result<String, BoxError> {
    let base = base_url();
    match nachalnik_providers::is_openrouter(&base) {
        true => env::var("OPENROUTER_API_KEY")
            .or_else(|_| env::var("NACHALNIK_API_KEY"))
            .map_err(|_| "set OPENROUTER_API_KEY or NACHALNIK_API_KEY".into()),
        false => env::var("NACHALNIK_API_KEY")
            .map_err(|_| format!("set NACHALNIK_API_KEY for {base}").into()),
    }
}

/// The endpoint to talk to; OpenRouter unless told otherwise.
pub fn base_url() -> String {
    env::var("NACHALNIK_BASE_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_owned())
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

/// The one model a live suite asks, from `NACHALNIK_TEST_MODEL` or the caller's fallback.
///
/// note: here rather than in each suite because the variable name is the shared fact; spelling it
/// by hand costs a run without failing. `NACHALNIK_MODEL` is not read. A suite reading a misspelt
/// name quietly uses the default model, so failures concern the test assertions, not the variable.
///
/// note: the default stays the caller's. Each suite's model is a fact about that suite; one shared
/// default would be wrong for at least one of them.
pub fn test_model(default: &str) -> String {
    env::var("NACHALNIK_TEST_MODEL").unwrap_or_else(|_| default.to_owned())
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
    let mut provider = OpenAiCompatible::new(model, base_url(), api_key()?)
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
