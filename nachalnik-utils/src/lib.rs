//! The environment `nachalnik`'s examples and its live suites read. Not published, not API.
//!
//! note: what this crate held until now was a whole OpenAI-compatible provider, because the
//! examples and the live tests each had a copy of one and the two had grown apart. That provider
//! is [`nachalnik_providers`] now - published, so that `kamchatka`'s copy could be merged into it
//! as well - and what is left here is the part that was never a provider's to do: which endpoint
//! to talk to, which key pays for it, and which models to ask.
//!
//! note: it stays a crate rather than a module in either place because four places need it -
//! `nachalnik`'s examples and live suite, `nachalnik-eval`'s, and `nachalnik-mcp`'s one live test -
//! and a second copy of it is how the first duplication started. It is a dev-dependency of those
//! and of nothing else, which is what lets it stay at `0.0.0` and unpublished: cargo strips
//! dev-dependencies from a published manifest, so a crate that is only ever dev-depended on never
//! has to exist on the registry.

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
/// note: and `OPENROUTER_API_KEY` only for OpenRouter. It used to win wherever the base URL
/// pointed, so somebody with one exported who followed the recipe for Google or a local ollama
/// sent their OpenRouter key there as the bearer, and the key they had set for it was never read.
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
/// note: both halves or neither. A `NACHALNIK_APP_URL` with no title would be attributed to a
/// bare address, which is a worse entry in somebody's rankings than no entry at all.
///
/// note: off unless asked for, like the header it feeds. Naming an app is a claim about which
/// program is spending the tokens, and the honest default for a benchmark is to make no claim -
/// the run is the example, not whatever harness happens to share its repository.
pub fn attribution() -> Option<(String, String)> {
    let url = env::var("NACHALNIK_APP_URL").ok()?;
    let title = env::var("NACHALNIK_APP_TITLE").ok()?;

    Some((url, title))
}

/// The one model a live suite asks, named by `NACHALNIK_TEST_MODEL` or by whatever the caller
/// falls back to.
///
/// note: here rather than in each suite because the *name of the variable* is the shared fact, and
/// spelling it by hand is a mistake that costs a run without failing one. `NACHALNIK_MODEL` is not
/// it - nothing reads that - and a suite that asked for it would quietly ask the default model
/// instead, which the endpoint then refuses or answers as somebody else, so the failures are about
/// whatever the tests assert rather than about the variable.
///
/// note: the default stays the caller's. What each suite is known to pass against is a fact about
/// that suite - a small free model with tool support here, a larger one where the questions are
/// about introspection - and one shared default would be wrong for at least one of them.
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

/// One provider per model, sharing a connection pool, each asked what its context limit is.
///
/// note: one client between them rather than one each, so that a panel comparing four models on
/// the same host opens one set of connections rather than four.
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
