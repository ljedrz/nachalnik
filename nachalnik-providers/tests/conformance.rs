//! Both dialects, against the suite every provider in this workspace has to pass.
//!
//! note: `#![cfg(feature = "conformance")]`, because the suite it runs is behind that feature -
//! it is a dev tool for whoever is writing a third provider, not something a caller of this crate
//! should be made to compile. CI turns every feature on.
//!
//! note: what the two dialects share is the *questions*. Every case in the suite is a bug that
//! actually happened, and every one of them was fixed in one copy at a time back when there were
//! three copies of this code: the stream decoded lossily was in all three, the tool-call
//! fragments filed by a missing index in two. A case added there applies to both of these at
//! once, with nothing edited here.

#![cfg(feature = "conformance")]

use std::sync::Arc;

use nachalnik_providers::conformance::Conformance;
use nachalnik_providers::{Gemini, OpenAiCompatible};

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
