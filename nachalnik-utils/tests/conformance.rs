//! This crate's provider, against the suite every provider here has to pass.

use std::sync::Arc;

use nachalnik_utils::{OpenAiCompatible, conformance::Conformance};

#[tokio::test]
async fn the_openai_compatible_provider_conforms() {
    Conformance::openai("nachalnik-utils", |url| {
        Arc::new(OpenAiCompatible::new(
            reqwest::Client::new(),
            "conformance",
            url,
            "no key needed",
        ))
    })
    .check()
    .await;
}
