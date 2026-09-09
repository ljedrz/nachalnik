//! Both dialects, against the suite every provider in this workspace has to pass.
//!
//! note: the suite is in `nachalnik-utils`, which is permanently unpublished, so it is reached
//! the only way a published crate may reach one - as a dev-dependency, which cargo strips from
//! the manifest it uploads.
//!
//! note: what the two dialects share is the *questions*. Every case in the suite is a bug that
//! actually happened, and every one of them was fixed in one copy at a time back when there were
//! three copies of this code: the stream decoded lossily was in all three, the tool-call
//! fragments filed by a missing index in two. A case added there applies to both of these at
//! once, with nothing edited here.

use std::sync::Arc;

use nachalnik_providers::{Gemini, OpenAiCompatible};
use nachalnik_utils::conformance::Conformance;

#[tokio::test]
async fn the_openai_compatible_provider_conforms() {
    Conformance::openai("the OpenAI-compatible provider", |url| {
        Arc::new(OpenAiCompatible::new("conformance", url, "no key needed"))
    })
    .check()
    .await;
}

#[tokio::test]
async fn the_gemini_provider_conforms() {
    Conformance::gemini("the Gemini provider", |url| {
        Arc::new(Gemini::new("conformance", url, "no key needed"))
    })
    .check()
    .await;
}
