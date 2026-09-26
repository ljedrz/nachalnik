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
    BoxError, DeltaSink, ModelInfo, ModelRequest, ModelResponse, Provider, StopReason, async_trait,
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
    let failed = failures(rewritten(|answer| {
        let mut response = answer?;
        if let Some(reasoning) = &response.reasoning {
            let text = reasoning.to_text();
            let mut summaries: Vec<&str> = text.split("\n\n").collect();
            summaries.reverse();
            response.reasoning = Some(summaries.join("\n\n").into());
        }
        Ok(response)
    }))
    .await;

    assert!(
        failed.contains("the two summaries were kept in the wrong order"),
        "{failed}"
    );
}

/// A provider that drops its thinking fails the summary case, and is told that none of the
/// thinking arrived rather than that one summary of two was lost.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_provider_that_drops_its_thinking_is_told_none_of_it_arrived() {
    let failed = failures(rewritten(|answer| {
        let mut response = answer?;
        response.reasoning = None;
        Ok(response)
    }))
    .await;

    assert!(
        failed.contains("a summary of the thinking was sent and none of it is on the turn"),
        "{failed}"
    );
}

/// A provider that empties the arguments of the calls it reads fails every case whose call
/// carries some.
///
/// note: the name and the identifier survive, so these cases fail on the arguments or not at
/// all. A call that kept its name and lost its arguments is one a tool would run on nothing.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_provider_that_empties_a_calls_arguments_does_not_conform() {
    let failed = failures(rewritten(|answer| {
        let mut response = answer?;
        for call in &mut response.tool_calls {
            call.args = Arc::new(serde_json::json!({}));
        }
        Ok(response)
    }))
    .await;

    for case in [
        "arguments streamed in fragments are assembled",
        "arguments that are not JSON are handed over as written",
        "a body that stops arriving keeps what arrived",
    ] {
        assert!(failed.contains(case), "{case}: {failed}");
    }
}

/// A provider that files a call under an identifier the model did not give it fails the case
/// that numbers a call from one.
///
/// note: the name is left right on purpose: that case asks for both, and a provider that gets
/// one of the two is a turn the kernel cannot match a tool result to.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_provider_that_renames_a_call_does_not_conform() {
    let failed = failures(rewritten(|answer| {
        let mut response = answer?;
        for call in &mut response.tool_calls {
            call.id = "renamed".into();
        }
        Ok(response)
    }))
    .await;

    assert!(
        failed.contains("a call numbered from one leaves nothing empty"),
        "{failed}"
    );
}

/// A provider that loses what the model said fails the case that sends it in fragments.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_provider_that_loses_the_text_does_not_conform() {
    let failed = failures(rewritten(|answer| {
        let mut response = answer?;
        response.content = None;
        Ok(response)
    }))
    .await;

    assert!(
        failed.contains("text arriving in fragments assembles in order"),
        "{failed}"
    );
}

/// A provider that marks a cut turn in words of its own fails both cases that cut one.
///
/// note: a stop reason of `Other`, because the word is the thing both cases hold every provider
/// to: a caller that looks for a cut looks for the one word.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_provider_that_calls_a_cut_something_else_does_not_conform() {
    let failed = failures(rewritten(|answer| {
        let mut response = answer?;
        if let StopReason::Other(why) = &mut response.stop {
            *why = "interrupted".to_owned();
        }
        Ok(response)
    }))
    .await;

    for case in [
        "a body that stops arriving keeps what arrived",
        "a stream cut while the model was still thinking keeps the thinking",
    ] {
        assert!(failed.contains(case), "{case}: {failed}");
    }
}

/// A provider that fails without the server's words fails both cases that send an error.
///
/// note: any error at all is the easy half of those cases; what they hold a provider to is that
/// whoever reads the error is told what came back.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_provider_that_loses_what_the_server_said_does_not_conform() {
    let failed = failures(rewritten(|answer| {
        answer.map_err(|_| "the request failed".into())
    }))
    .await;

    for case in [
        "an error inside a successful response is an error",
        "a body that is not a stream is an error",
    ] {
        assert!(failed.contains(case), "{case}: {failed}");
    }
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

/// The OpenAI-compatible provider, with every answer rewritten on its way out.
#[cfg(feature = "openai")]
struct Rewritten(nachalnik_providers::OpenAiCompatible, Rewrite);

#[cfg(feature = "openai")]
type Rewrite = fn(Result<ModelResponse, BoxError>) -> Result<ModelResponse, BoxError>;

/// The suite, held to the OpenAI-compatible provider rewritten by `rewrite`.
#[cfg(feature = "openai")]
fn rewritten(rewrite: Rewrite) -> Conformance {
    Conformance::openai("a rewritten provider", move |url| {
        Arc::new(Rewritten(
            nachalnik_providers::OpenAiCompatible::new("conformance", url, "no key needed"),
            rewrite,
        ))
    })
}

#[cfg(feature = "openai")]
#[async_trait]
impl Provider for Rewritten {
    fn info(&self) -> ModelInfo {
        self.0.info()
    }

    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        (self.1)(self.0.respond(request, deltas).await)
    }
}
