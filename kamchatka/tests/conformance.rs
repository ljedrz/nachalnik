//! Both of this crate's providers, against the suite every provider in the workspace has to pass.
//!
//! note: this file is the answer to a structural problem rather than to a bug. `provider.rs` and
//! `gemini.rs` share 286 identical lines with each other and 418 with `nachalnik-utils`'s, and
//! they cannot be merged: this crate is published and that one is permanently unpublished, so
//! nothing here may depend on it except like this - as a dev-dependency, which cargo strips from
//! a published manifest.
//!
//! note: so what is shared is the *questions*. Every case in the suite is a bug that actually
//! happened to one of the three, and every one of them was fixed in one copy at a time - the
//! stream decoded lossily was in all three, the tool-call fragments filed by a missing index in
//! two. A case added there now applies to every provider at once, with nothing edited here.

use std::sync::Arc;

use kamchatka::{gemini::Gemini, provider::OpenAiCompatible};
use nachalnik_utils::conformance::Conformance;

#[tokio::test]
async fn the_openai_compatible_provider_conforms() {
    Conformance::openai("kamchatka's OpenAI-compatible provider", |url| {
        Arc::new(OpenAiCompatible::new("conformance", url, "no key needed"))
    })
    .check()
    .await;
}

#[tokio::test]
async fn the_gemini_provider_conforms() {
    Conformance::gemini("kamchatka's Gemini provider", |url| {
        Arc::new(Gemini::new("conformance", url, "no key needed"))
    })
    .check()
    .await;
}
