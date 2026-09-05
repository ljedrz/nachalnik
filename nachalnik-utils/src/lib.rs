//! Scaffolding shared by `nachalnik`'s examples and its live test suite. Not published, not API.
//!
//! note: This crate exists for one reason: the examples and the live tests each had their own
//! copy of the same OpenAI-compatible provider - five hundred lines apiece, grown apart in
//! different directions, and fixed twice whenever they needed fixing at all. Neither of them is
//! what this workspace is *about*, so neither of them should be written twice.
//!
//! note: It is a dev-dependency of `nachalnik` and of `nachalnik-eval`, and of nothing else,
//! which is what lets it stay at `0.0.0` and unpublished: cargo strips dev-dependencies from a
//! published manifest, so a crate that is only ever dev-depended on never has to exist on the
//! registry. A *normal* dependency could not do this - `cargo package` refuses a dependency with
//! no version - which is why `kamchatka`, being published and being a binary, still carries a
//! provider of its own, and why `nachalnik-eval`'s runner is an example rather than one.

#![deny(unsafe_code)]
#![deny(missing_docs)]

pub mod conformance;
pub mod provider;

pub use provider::{OpenAiCompatible, models, out_of_quota, providers};

use std::env;

use nachalnik::BoxError;

/// The API key, under whichever of the documented names it is set.
///
/// note: Deliberately not `OPENAI_API_KEY`, which plenty of people have exported for other
/// reasons. Spending somebody's credits as a side effect of `cargo test` would be a poor way to
/// demonstrate a crate about not doing things behind the user's back.
pub fn api_key() -> Result<String, BoxError> {
    env::var("OPENROUTER_API_KEY")
        .or_else(|_| env::var("NACHALNIK_API_KEY"))
        .map_err(|_| "set OPENROUTER_API_KEY or NACHALNIK_API_KEY".into())
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
