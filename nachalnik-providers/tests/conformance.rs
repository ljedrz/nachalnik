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

#[cfg(feature = "openai")]
use nachalnik::{
    BoxError, DeltaSink, ModelInfo, ModelRequest, ModelResponse, Provider, async_trait,
};
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

/// A provider that decodes every read off the wire on its own fails the suite, on the case that
/// splits a character between two reads.
///
/// note: the suite held to its own verdict. A case that stopped splitting the body would still
/// pass every provider that is right, so only a provider that is wrong can show that it splits.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_provider_that_decodes_each_read_on_its_own_does_not_conform() {
    nachalnik_providers::install_crypto();
    let failed = failures(Conformance::openai("a lossy provider", |url| {
        Arc::new(Lossy {
            url,
            client: reqwest::Client::new(),
        })
    }))
    .await;

    assert!(
        failed.contains("a character split between two reads survives"),
        "{failed}"
    );
}

/// A provider that keeps the summaries of its thinking last-first fails the suite, and is told
/// that the order is what is wrong.
///
/// note: both summaries are still on the turn and the answer is still `9`, so the order is the
/// only thing that stands between this provider and a pass.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_provider_that_keeps_its_summaries_last_first_does_not_conform() {
    let failed = failures(Conformance::openai("a last-first provider", |url| {
        Arc::new(LastFirst(nachalnik_providers::OpenAiCompatible::new(
            "conformance",
            url,
            "no key needed",
        )))
    }))
    .await;

    assert!(
        failed.contains("the two summaries were kept in the wrong order"),
        "{failed}"
    );
}

/// What the suite said about a provider it failed.
#[cfg(feature = "openai")]
async fn failures(conformance: Conformance) -> String {
    let checked = tokio::spawn(async move { conformance.check().await }).await;
    let panic = checked.expect_err("the provider passed").into_panic();

    panic
        .downcast_ref::<String>()
        .cloned()
        .expect("the suite names what failed")
}

/// Reads the body of an OpenAI stream, decoding every read as it arrives.
#[cfg(feature = "openai")]
struct Lossy {
    url: String,
    client: reqwest::Client,
}

#[cfg(feature = "openai")]
#[async_trait]
impl Provider for Lossy {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("lossy", "m")
    }

    async fn respond(&self, _: ModelRequest, _: DeltaSink) -> Result<ModelResponse, BoxError> {
        let mut response = self.client.post(&self.url).send().await?;
        let mut read = String::new();
        while let Some(chunk) = response.chunk().await? {
            read.push_str(&String::from_utf8_lossy(&chunk));
        }

        let said: String = read
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
            .filter_map(|event| {
                event["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();

        Ok(ModelResponse::text(said))
    }
}

/// The OpenAI-compatible provider, with the summaries of its thinking turned around.
#[cfg(feature = "openai")]
struct LastFirst(nachalnik_providers::OpenAiCompatible);

#[cfg(feature = "openai")]
#[async_trait]
impl Provider for LastFirst {
    fn info(&self) -> ModelInfo {
        self.0.info()
    }

    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        let mut response = self.0.respond(request, deltas).await?;
        if let Some(reasoning) = &response.reasoning {
            let text = reasoning.to_text();
            let mut summaries: Vec<&str> = text.split("\n\n").collect();
            summaries.reverse();
            response.reasoning = Some(summaries.join("\n\n").into());
        }

        Ok(response)
    }
}
