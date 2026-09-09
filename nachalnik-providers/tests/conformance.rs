//! Both dialects, against the suite every provider in this workspace has to pass.
//!
//! note: gated on the `conformance` feature, because the suite it runs is behind one - it is a
//! dev tool for whoever is writing a third provider, not something a caller of this crate should
//! be made to compile - and on the dialects, a test each, so that a build with one of them turned
//! off still runs the other's. CI turns every feature on.
//!
//! note: what the two dialects share is the *questions*. Every case in the suite is a bug that
//! actually happened, and every one of them was fixed in one copy at a time back when there were
//! three copies of this code: the stream decoded lossily was in all three, the tool-call
//! fragments filed by a missing index in two. A case added there applies to both of these at
//! once, with nothing edited here.

#![cfg(all(feature = "conformance", any(feature = "openai", feature = "gemini")))]

use std::sync::Arc;

use nachalnik_providers::conformance::Conformance;

#[cfg(feature = "openai")]
#[tokio::test]
async fn the_openai_compatible_provider_conforms() {
    Conformance::openai("the OpenAI-compatible provider", |url| {
        Arc::new(nachalnik_providers::OpenAiCompatible::new(
            "conformance",
            url,
            "no key needed",
        ))
    })
    .check()
    .await;
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn the_gemini_provider_conforms() {
    Conformance::gemini("the Gemini provider", |url| {
        Arc::new(nachalnik_providers::Gemini::new(
            "conformance",
            url,
            "no key needed",
        ))
    })
    .check()
    .await;
}
