//! A suite any provider can be held to, whichever of the two dialects it speaks.
//!
//! note: this was written when there were three providers in this workspace that were one
//! provider written three times, and it is the reason there is now one per dialect. The cost of
//! the copies was paid three times in a row, each time the same way: a bug found in one copy,
//! fixed in one copy. The stream that decoded each chunk lossily was in all three. The tool-call
//! fragments filed by a missing index were in two. Arguments that would not parse were handled one
//! way in a file's streamed path and another in its whole-answer path.
//!
//! note: it outlives the duplication because what it holds is not agreement between copies but a
//! list of shapes some server really sent. Every case below is a bug that actually happened, asked
//! through a real socket rather than of a parser, because what is under test lives inside
//! `respond` - between reading bytes off a response and handing back a [`ModelResponse`] - and a
//! test that reached in beside it would be testing a copy of the code under test.
//!
//! note: behind the `conformance` feature, and off by default. It is a dev tool, it stands up
//! `TcpListener`s, and a caller writing a provider of its own is who it is for.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use nachalnik_providers::conformance::Conformance;
//! # async fn go() {
//! Conformance::openai("my provider", |url| Arc::new(my_provider(url)))
//!     .check()
//!     .await;
//! # }
//! # fn my_provider(_: String) -> nachalnik_providers::OpenAiCompatible { unimplemented!() }
//! ```

use std::{sync::Arc, time::Duration};

use nachalnik::{Config, ContextItem, Kernel, ModelResponse, Provider, StopReason};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// What a provider calls a turn whose stream stopped arriving partway.
///
/// note: a [`StopReason::Other`] rather than a variant of its own, because `Other` is the
/// runtime's own place for "anything else the provider reported" and a word providers have to
/// agree on does not need the core to hold it. What makes them agree is this case: the literal
/// lives in each provider, and one that spells it differently fails here.
const CUT_OFF: &str = "cut off";

/// The wire format a provider under test speaks.
///
/// note: the dialects disagree about the *bodies*, not about the questions. Two calls in one turn
/// are two calls in both; what differs is how a server says so, which is why each case below
/// supplies a body per dialect rather than one body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Dialect {
    /// OpenAI chat-completions: `choices[].delta`, tool calls assembled from fragments.
    OpenAi,
    /// Google's `generateContent`: `candidates[].content.parts`, whole calls.
    Gemini,
}

/// What one case came to.
enum Outcome {
    Passed,
    /// The dialect has no way to express the question; see each case for why.
    Skipped(&'static str),
    Failed(String),
}

/// A provider under test, and the suite it has to pass.
pub struct Conformance {
    what: String,
    dialect: Dialect,
    build: Box<dyn Fn(String) -> Arc<dyn Provider> + Send + Sync>,
}

impl Conformance {
    /// A provider speaking the OpenAI chat-completions dialect.
    pub fn openai<P: Provider + 'static>(
        what: impl Into<String>,
        build: impl Fn(String) -> Arc<P> + Send + Sync + 'static,
    ) -> Self {
        Self {
            what: what.into(),
            dialect: Dialect::OpenAi,
            build: Box::new(move |url| build(url) as Arc<dyn Provider>),
        }
    }

    /// A provider speaking Google's own.
    pub fn gemini<P: Provider + 'static>(
        what: impl Into<String>,
        build: impl Fn(String) -> Arc<P> + Send + Sync + 'static,
    ) -> Self {
        Self {
            what: what.into(),
            dialect: Dialect::Gemini,
            build: Box::new(move |url| build(url) as Arc<dyn Provider>),
        }
    }

    /// Runs every case, and panics naming all of the ones that failed.
    ///
    /// note: all of them, rather than stopping at the first. A provider that has drifted has
    /// usually drifted in more than one place, and finding that out one `cargo test` at a time is
    /// how a sweep turns into an afternoon.
    pub async fn check(&self) {
        // reqwest is built with `rustls-no-provider`, so a client cannot reach `https://` until
        // one is named; a caller who built their client by hand has not necessarily done it
        let _ = rustls::crypto::ring::default_provider().install_default();

        let cases: Vec<(&str, Outcome)> = vec![
            (
                "a character split between two reads survives",
                self.split_character().await,
            ),
            (
                "two calls in one turn are two calls",
                self.two_calls().await,
            ),
            (
                "a call numbered from one leaves nothing empty",
                self.numbered_from_one().await,
            ),
            (
                "arguments streamed in fragments are assembled",
                self.fragmented_arguments().await,
            ),
            (
                "arguments that are not JSON are handed over as written",
                self.broken_arguments().await,
            ),
            (
                "text arriving in fragments assembles in order",
                self.fragments().await,
            ),
            (
                "an error inside a successful response is an error",
                self.error_in_a_200().await,
            ),
            (
                "a body that is not a stream is an error",
                self.not_a_stream().await,
            ),
            (
                "a body that stops arriving keeps what arrived",
                self.cut_off_midstream().await,
            ),
            (
                "a stream cut while the model was still thinking keeps the thinking",
                self.cut_off_while_thinking().await,
            ),
            (
                "what the server said a request cost is carried through",
                self.usage().await,
            ),
            (
                "a turn that thought reports the thinking inside what it generated",
                self.reasoning_is_inside_what_was_generated().await,
            ),
            (
                "thinking that arrives as a finished summary is thinking",
                self.a_summary_is_read_as_thinking().await,
            ),
        ];

        let (mut failed, mut skipped) = (Vec::new(), Vec::new());
        for (name, outcome) in cases {
            match outcome {
                Outcome::Passed => {}
                Outcome::Skipped(why) => skipped.push(format!("  {name}: {why}")),
                Outcome::Failed(why) => failed.push(format!("  {name}\n    {why}")),
            }
        }

        assert!(
            failed.is_empty(),
            "{} failed {} case(s) of the provider conformance suite:\n{}\n\nnot asked of {:?}:\n{}",
            self.what,
            failed.len(),
            failed.join("\n"),
            self.dialect,
            match skipped.is_empty() {
                true => "  (nothing)".to_owned(),
                false => skipped.join("\n"),
            },
        );
    }

    // ------------------------------------------------------------------------------- the cases

    /// A chunk boundary is not a character boundary.
    ///
    /// note: the bytes off the socket were decoded as they arrived, lossily, in all three of this
    /// workspace's providers. A character split between two reads was decoded twice - once with
    /// its tail missing and once with its head - and became two replacement characters, which then
    /// went into the context and into whatever record was kept. Every split point through the text
    /// is tried, because one fixed split point proves only that one worked.
    async fn split_character(&self) -> Outcome {
        const SAID: &str = "zażółć — 大";
        let body = match self.dialect {
            Dialect::OpenAi => concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"zażółć — 大\"},\"index\":0}]}\n\n",
                "data: [DONE]\n\n",
            ),
            Dialect::Gemini => concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"zażółć — 大\"}],",
                "\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n",
            ),
        };

        let from = body.find("za").expect("the text is in the body");
        for at in from..from + SAID.len() + 1 {
            match self.ask(body, Delivery::Split(at)).await {
                Ok(response) => {
                    let said = text_of(&response);
                    if said != SAID {
                        return Outcome::Failed(format!(
                            "broken after {at} bytes, the answer came back {said:?} rather than \
                             {SAID:?}"
                        ));
                    }
                }
                Err(e) => return Outcome::Failed(format!("broken after {at} bytes: {e}")),
            }
        }

        Outcome::Passed
    }

    /// A turn that thought reports the thinking inside what it generated.
    ///
    /// note: the question none of the three was ever asked, and all three answered differently.
    /// `Usage::output_tokens` is everything the model generated and is charged for, reasoning
    /// included, so that `input + output` is the whole bill whichever endpoint answered. The
    /// dialects do not hand it over that way: OpenAI's `completion_tokens` already contains the
    /// reasoning, Google reports thoughts and candidates side by side, and a Google endpoint
    /// speaking the OpenAI dialect omits the details object and leaves the thinking to be found in
    /// the difference between the total and its parts. Three shapes, one figure, and until this
    /// case existed each provider settled it privately and two settled it wrongly.
    ///
    /// note: what is asserted is the *relationship* rather than a number pulled from a fixture -
    /// the answer's own cost has to come back by subtracting the reasoning from the output. A
    /// provider that reports the two side by side passes the sum and fails this.
    async fn reasoning_is_inside_what_was_generated(&self) -> Outcome {
        let body = match self.dialect {
            Dialect::OpenAi => concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"index\":0,",
                "\"finish_reason\":\"stop\"}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,",
                "\"completion_tokens\":30,\"total_tokens\":41,",
                "\"completion_tokens_details\":{\"reasoning_tokens\":20}}}\n\n",
                "data: [DONE]\n\n",
            ),
            Dialect::Gemini => concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}],",
                "\"role\":\"model\"},\"finishReason\":\"STOP\"}],",
                "\"usageMetadata\":{\"promptTokenCount\":11,\"candidatesTokenCount\":10,",
                "\"thoughtsTokenCount\":20,\"totalTokenCount\":41}}\n\n",
            ),
        };

        let usage = match self.ask(body, Delivery::Whole).await {
            Ok(response) => match response.usage {
                Some(usage) => usage,
                None => return Outcome::Failed("nothing was reported".to_owned()),
            },
            Err(e) => return Outcome::Failed(e),
        };

        match (usage.output_tokens, usage.reasoning_tokens) {
            (Some(30), Some(20)) => Outcome::Passed,
            (Some(10), Some(20)) => Outcome::Failed(
                "the thinking is reported beside what was generated rather than inside it, so a \
                 bill of 30 reads as 10"
                    .to_owned(),
            ),
            other => Outcome::Failed(format!(
                "30 generated, of which 20 was reasoning, came back as {other:?}"
            )),
        }
    }

    /// Thinking that arrives as a finished summary is thinking.
    ///
    /// note: the shape `delta.reasoning` is not. A dialect may hand the thinking over as fragments
    /// while it is generated, or as one finished summary of it afterwards - `{"content": ...,
    /// "status": "complete"}`, on the chunk rather than in the delta - and an endpoint that does
    /// the second had its thinking read by nothing here. Measured: `mercury-2` asked with
    /// `reasoning_summary: true` returns eleven hundred reasoning tokens and a summary of them
    /// in that field, and both providers put the turn on the context with nothing in it where the
    /// thinking was.
    ///
    /// note: two summaries in one turn, because that is what the endpoint sends - one per stretch
    /// of reasoning it finishes - and the question is whether both are kept. Replacing rather than
    /// appending loses the first, which is the half a reader wants: it is the one about the
    /// working, and the second is usually the tidy paragraph. The same endpoint's non-streamed
    /// field is these joined, so a provider that keeps one is answering a question its own dialect
    /// has already settled.
    async fn a_summary_is_read_as_thinking(&self) -> Outcome {
        let body = match self.dialect {
            Dialect::OpenAi => concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"9\"},\"index\":0}],",
                "\"reasoning_summary\":null}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"index\":0}],\"reasoning_summary\":",
                "{\"content\":\"all but 9 means 9 stay\",\"status\":\"complete\"}}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"index\":0,\"finish_reason\":\"stop\"}],",
                "\"reasoning_summary\":{\"content\":\"so the answer is 9\",",
                "\"status\":\"complete\"}}\n\n",
                "data: [DONE]\n\n",
            ),
            // Google's dialect has no summary field: the thinking is parts of the turn, marked
            // `thought`, and `cut_off_while_thinking` above is where that shape is asked about
            Dialect::Gemini => {
                return Outcome::Skipped(
                    "this dialect sends the thinking as parts of the turn, not as a summary",
                );
            }
        };

        let response = match self.ask(body, Delivery::Whole).await {
            Ok(response) => response,
            Err(e) => return Outcome::Failed(e),
        };

        // `thinking()` rather than the field, for the reason the case above uses it
        let thought = response
            .thinking()
            .map(|content| content.to_text())
            .collect::<Vec<_>>()
            .join("\n");
        match (
            thought.contains("all but 9 means 9 stay"),
            thought.contains("so the answer is 9"),
        ) {
            (true, true) => match text_of(&response).trim() {
                "9" => Outcome::Passed,
                said => Outcome::Failed(format!(
                    "the summary went into the answer as well: the turn said {said:?}"
                )),
            },
            (false, false) => Outcome::Failed(format!(
                "a summary of the thinking was sent and none of it is on the turn: {thought:?}"
            )),
            _ => Outcome::Failed(format!(
                "one of the two summaries the turn carried was dropped rather than kept: \
                 {thought:?}"
            )),
        }
    }

    /// A body that stops arriving in the middle of an answer keeps what arrived.
    ///
    /// note: the failure that prompted this ran for 148 seconds against a reasoning model and
    /// came back `error decoding response body` - reqwest's words for a body that ended early -
    /// with every token it had produced thrown away and the turn failed. All three providers here
    /// did that, and one of them carried a note saying it did the opposite.
    ///
    /// note: the answer is the one the interrupt path eleven lines above it already gives: keep
    /// whatever was parsed, abandon the socket, and mark the turn. A dropped connection is that
    /// case without the consent, so it gets the same handling under a name of its own. The two
    /// alternatives are both worse. Failing throws away work the provider has *already billed*;
    /// retrying bills it again, up to the retry budget, so an answer that reliably outruns an
    /// upstream's patience is paid for four times and fails anyway - and the loop that retries a
    /// busy server is careful to do it only where nothing was generated.
    ///
    /// note: what a *complete* call caught by the cut does is nothing special on purpose. It is a
    /// call the model really made, it is recorded, and the permission policy is still the thing
    /// that decides whether it runs. A provider dropping it would be deciding that on the
    /// caller's behalf, which is the one thing none of this is allowed to do.
    async fn cut_off_midstream(&self) -> Outcome {
        let body = match self.dialect {
            Dialect::OpenAi => concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"the first half \"},\"index\":0}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_1\",",
                "\"type\":\"function\",\"function\":{\"name\":\"write\",",
                "\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}]},\"index\":0}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"NEVER-ARRIVED\"},\"index\":0}]}\n\n",
                "data: [DONE]\n\n",
            ),
            Dialect::Gemini => concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"the first half \"}],",
                "\"role\":\"model\"}}]}\n\n",
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{",
                "\"name\":\"write\",\"args\":{\"path\":\"a.txt\"},\"id\":\"call_1\"}}],",
                "\"role\":\"model\"}}]}\n\n",
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"NEVER-ARRIVED\"}],",
                "\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n",
            ),
        };

        // the connection goes away just inside the third chunk, so the two before it are whole
        // lines that arrived and the third is a line that never finished. Found rather than
        // counted, because the prefix is a different length in each dialect
        let marker = body
            .find("NEVER-ARRIVED")
            .expect("the third chunk is in the body");
        let at = body[..marker]
            .rfind("data: ")
            .expect("it is a chunk of its own")
            + 6;
        let response = match self.ask(body, Delivery::Cut(at)).await {
            Ok(response) => response,
            Err(e) => {
                return Outcome::Failed(format!(
                    "a body that stopped arriving took the whole turn with it: {e}"
                ));
            }
        };

        let said = text_of(&response);
        if said != "the first half " {
            return Outcome::Failed(format!(
                "what arrived before the cut came back {said:?} rather than {:?}",
                "the first half "
            ));
        }
        // `calls()` rather than the field, because one dialect records a turn as ordered blocks
        // and keeps its calls in them
        let calls: Vec<_> = response.calls().collect();
        if calls.len() != 1 {
            return Outcome::Failed(format!(
                "the call that arrived whole was not kept: {:?}",
                calls.iter().map(|call| &call.tool).collect::<Vec<_>>()
            ));
        }
        match &response.stop {
            StopReason::Other(why) if why == CUT_OFF => Outcome::Passed,
            other => Outcome::Failed(format!(
                "the turn came back as {other:?}, which does not say it was cut off"
            )),
        }
    }

    /// And a cut with nothing on the wire yet but thinking is still a turn.
    ///
    /// note: the case above cuts after some content, which is the easy half. On a reasoning model
    /// the answer starts late - replaying a real OpenRouter stream, the first non-empty `content`
    /// was 36% of the way in, behind eleven thousand bytes of `reasoning` - so a cut is more likely
    /// to land here than anywhere else. What decides it is whether a thinking delta counts as
    /// something having arrived: counted, the turn comes back with the thinking in it, and the next
    /// request carries what the model had worked out; not counted, the whole thing is an error and
    /// the most expensive part of the answer is the part thrown away.
    async fn cut_off_while_thinking(&self) -> Outcome {
        let body = match self.dialect {
            Dialect::OpenAi => concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning\":\"working it out\"},\"index\":0}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"NEVER-ARRIVED\"},\"index\":0}]}\n\n",
                "data: [DONE]\n\n",
            ),
            Dialect::Gemini => concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"working it out\",",
                "\"thought\":true}],\"role\":\"model\"}}]}\n\n",
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"NEVER-ARRIVED\"}],",
                "\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n",
            ),
        };

        let marker = body
            .find("NEVER-ARRIVED")
            .expect("the second chunk is in the body");
        let at = body[..marker]
            .rfind("data: ")
            .expect("it is a chunk of its own")
            + 6;
        let response = match self.ask(body, Delivery::Cut(at)).await {
            Ok(response) => response,
            Err(e) => {
                return Outcome::Failed(format!(
                    "a turn that had only got as far as thinking was thrown away whole: {e}"
                ));
            }
        };

        // `thinking()` rather than the field, for the reason `calls()` is used above: one dialect
        // records a turn as ordered blocks and keeps its thinking in them
        let thought = response
            .thinking()
            .map(|content| content.to_text())
            .collect::<Vec<_>>()
            .join("\n");
        if !thought.contains("working it out") {
            return Outcome::Failed(format!(
                "the thinking that had arrived is not on the turn: {thought:?}"
            ));
        }
        if text_of(&response).contains("NEVER-ARRIVED") {
            return Outcome::Failed("what never arrived is on the turn anyway".to_owned());
        }
        match &response.stop {
            StopReason::Other(why) if why == CUT_OFF => Outcome::Passed,
            other => Outcome::Failed(format!(
                "the turn came back as {other:?}, which does not say it was cut off"
            )),
        }
    }

    /// Two calls in one turn are two calls.
    ///
    /// note: the OpenAI body here is the shape Google's *compatible* endpoint sends - one whole
    /// call per chunk, each with an identifier of its own and no `index` anywhere. Read as index
    /// zero, every call in a turn lands on the first: two `write`s become one call named
    /// `writewrite` whose arguments are two JSON objects run together, and the model is told there
    /// is no such tool.
    async fn two_calls(&self) -> Outcome {
        let body = match self.dialect {
            Dialect::OpenAi => concat!(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_1\",",
                "\"type\":\"function\",\"function\":{\"name\":\"write\",",
                "\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}]},\"index\":0}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_2\",",
                "\"type\":\"function\",\"function\":{\"name\":\"write\",",
                "\"arguments\":\"{\\\"path\\\":\\\"b.txt\\\"}\"}}]},\"index\":0,",
                "\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
            ),
            Dialect::Gemini => concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{",
                "\"name\":\"write\",\"args\":{\"path\":\"a.txt\"},\"id\":\"call_1\"}}],",
                "\"role\":\"model\"}}]}\n\n",
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{",
                "\"name\":\"write\",\"args\":{\"path\":\"b.txt\"},\"id\":\"call_2\"}}],",
                "\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n",
            ),
        };

        let response = match self.ask(body, Delivery::Whole).await {
            Ok(response) => response,
            Err(e) => return Outcome::Failed(e),
        };
        let calls: Vec<_> = response.calls().collect();
        if calls.len() != 2 {
            return Outcome::Failed(format!(
                "the turn asked for two and {} came back: {:?}",
                calls.len(),
                calls.iter().map(|c| &c.tool).collect::<Vec<_>>()
            ));
        }
        for (call, wanted) in calls.iter().zip(["a.txt", "b.txt"]) {
            if call.tool != "write" {
                return Outcome::Failed(format!("a call came back named `{}`", call.tool));
            }
            if call.args["path"] != wanted {
                return Outcome::Failed(format!(
                    "a call meant for {wanted} arrived with {}",
                    call.args
                ));
            }
        }

        Outcome::Passed
    }

    /// An index that starts at one leaves no unfilled call at zero.
    ///
    /// note: minimax numbers its calls from one. Used as a position in a list, that leaves an
    /// empty call at zero - no name, no identifier - which the kernel then repairs and hands to
    /// the model as a tool that does not exist. Google's dialect has no index at all, so there is
    /// nothing here to ask it.
    async fn numbered_from_one(&self) -> Outcome {
        let Dialect::OpenAi = self.dialect else {
            return Outcome::Skipped("this dialect files no calls by index");
        };
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_a\",",
            "\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"index\":0,",
            "\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
        );

        let response = match self.ask(body, Delivery::Whole).await {
            Ok(response) => response,
            Err(e) => return Outcome::Failed(e),
        };
        let calls: Vec<_> = response.calls().collect();
        match calls.as_slice() {
            [one] if one.tool == "read" && one.id.0 == "call_a" => Outcome::Passed,
            other => Outcome::Failed(format!(
                "one call was asked for and {} came back: {:?}",
                other.len(),
                other.iter().map(|c| (&c.id.0, &c.tool)).collect::<Vec<_>>()
            )),
        }
    }

    /// A call's arguments arrive a few characters at a time, against an index.
    ///
    /// note: the ordinary OpenAI shape, and the other half of what an index is for: the first
    /// fragment carries the name and the identifier, the rest carry nothing but more of the
    /// arguments. A provider that resolved calls by identifier alone would drop every fragment
    /// after the first. Google's dialect sends whole calls, so there is nothing here to ask it.
    async fn fragmented_arguments(&self) -> Outcome {
        let Dialect::OpenAi = self.dialect else {
            return Outcome::Skipped("this dialect sends whole calls");
        };
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
            "\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}}]},\"index\":0}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,",
            "\"function\":{\"arguments\":\"th\\\":\\\"note\"}}]},\"index\":0}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,",
            "\"function\":{\"arguments\":\"s.md\\\"}\"}}]},\"index\":0,",
            "\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
        );

        match self.ask(body, Delivery::Whole).await {
            Ok(response) => match response.calls().next() {
                Some(call) if call.args["path"] == "notes.md" => Outcome::Passed,
                Some(call) => Outcome::Failed(format!("the fragments came back as {}", call.args)),
                None => Outcome::Failed("no call survived the fragments".to_owned()),
            },
            Err(e) => Outcome::Failed(e),
        }
    }

    /// A model that writes arguments which are not JSON is shown that it did.
    ///
    /// note: handing back `{}` instead runs the tool with no arguments and tells nobody why.
    /// Google's dialect sends arguments as an object rather than as a string, so it cannot express
    /// the question.
    async fn broken_arguments(&self) -> Outcome {
        let Dialect::OpenAi = self.dialect else {
            return Outcome::Skipped("this dialect sends arguments as an object");
        };
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
            "\"function\":{\"name\":\"read\",\"arguments\":\"{path: notes\"}}]},\"index\":0,",
            "\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
        );

        let response = match self.ask(body, Delivery::Whole).await {
            Ok(response) => response,
            Err(e) => return Outcome::Failed(e),
        };
        match response.calls().next() {
            Some(call) if call.args["_unparsed"] == "{path: notes" => Outcome::Passed,
            Some(call) => Outcome::Failed(format!(
                "what the model wrote is not in the arguments it was given: {}",
                call.args
            )),
            None => Outcome::Failed("the call did not survive at all".to_owned()),
        }
    }

    /// A sentence arriving in pieces is one sentence, in the order the pieces came.
    async fn fragments(&self) -> Outcome {
        let body = match self.dialect {
            Dialect::OpenAi => concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"one \"},\"index\":0}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"two \"},\"index\":0}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"three\"},\"index\":0,",
                "\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            ),
            Dialect::Gemini => concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"one \"}],",
                "\"role\":\"model\"}}]}\n\n",
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"two \"}],",
                "\"role\":\"model\"}}]}\n\n",
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"three\"}],",
                "\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n",
            ),
        };

        match self.ask(body, Delivery::Whole).await {
            Ok(response) if text_of(&response) == "one two three" => Outcome::Passed,
            Ok(response) => {
                Outcome::Failed(format!("the pieces came back as {:?}", text_of(&response)))
            }
            Err(e) => Outcome::Failed(e),
        }
    }

    /// These APIs report an upstream failure as an object inside an otherwise fine 200.
    ///
    /// note: a provider that only reads the status code records the model as having said nothing,
    /// and the kernel faithfully writes that down. The runtime's own `Provider` documentation
    /// warns about it: "Both of this crate's example providers had to learn that the hard way."
    async fn error_in_a_200(&self) -> Outcome {
        let body = "data: {\"error\":{\"message\":\"the upstream is on fire\",\"code\":502}}\n\n";

        match self.ask(body, Delivery::Whole).await {
            Ok(response) => Outcome::Failed(format!(
                "a failure inside a 200 was read as an answer: {:?}",
                text_of(&response)
            )),
            Err(e) if e.contains("the upstream is on fire") => Outcome::Passed,
            Err(e) => Outcome::Failed(format!("the server's own words are not in the error: {e}")),
        }
    }

    /// A body that is not a stream at all is an error rather than an empty answer.
    async fn not_a_stream(&self) -> Outcome {
        match self
            .ask("<html>502 Bad Gateway</html>", Delivery::Whole)
            .await
        {
            Ok(response) => Outcome::Failed(format!(
                "a page of HTML was read as an answer: {:?}",
                text_of(&response)
            )),
            Err(_) => Outcome::Passed,
        }
    }

    /// What the provider said a request cost reaches the kernel.
    ///
    /// note: the figure everything downstream reports as what a run *actually* cost, as against
    /// the kernel's own estimate. A provider that dropped it would leave a caller with two
    /// estimates and no measurement.
    async fn usage(&self) -> Outcome {
        let body = match self.dialect {
            Dialect::OpenAi => concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"index\":0,",
                "\"finish_reason\":\"stop\"}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,",
                "\"completion_tokens\":3,\"total_tokens\":14}}\n\ndata: [DONE]\n\n",
            ),
            Dialect::Gemini => concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}],",
                "\"role\":\"model\"},\"finishReason\":\"STOP\"}],",
                "\"usageMetadata\":{\"promptTokenCount\":11,\"candidatesTokenCount\":3}}\n\n",
            ),
        };

        match self.ask(body, Delivery::Whole).await {
            Ok(response) => match response.usage.and_then(|usage| usage.input_tokens) {
                Some(11) => Outcome::Passed,
                other => Outcome::Failed(format!("the request was reported as costing {other:?}")),
            },
            Err(e) => Outcome::Failed(e),
        }
    }

    // ---------------------------------------------------------------------------- the machinery

    /// Puts one question to the provider, through a socket, and hands back what it made of the
    /// answer.
    ///
    /// note: through a `Kernel` rather than by calling `respond` directly, because that is the
    /// path a caller has: the request is built by the projector, the answer is recorded, and what
    /// is asserted on is what a session would have ended up holding.
    async fn ask(
        &self,
        body: &'static str,
        delivery: Delivery,
    ) -> Result<Arc<ModelResponse>, String> {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider((self.build)(server(body, delivery).await));
        kernel.push(ContextItem::user("go"));
        kernel.step().await.map_err(|e| e.to_string())?;

        kernel
            .last_response()
            .ok_or_else(|| "the provider answered with nothing at all".to_owned())
    }
}

/// What the turn said, whichever shape it came back in.
fn text_of(response: &ModelResponse) -> String {
    response
        .content
        .as_ref()
        .map(|content| content.to_text().into_owned())
        .unwrap_or_default()
}

/// How much of a body the server delivers, and in how many writes.
///
/// note: named rather than an `Option<usize>`, because there are three intents here and two of
/// them are the same number meaning different things: `Split` delivers the whole answer awkwardly,
/// `Cut` delivers half an answer and goes away.
#[derive(Debug, Clone, Copy)]
enum Delivery {
    /// All of it, in one write.
    Whole,
    /// All of it, in two writes broken at this byte.
    Split(usize),
    /// The first this many bytes and then the connection, with `Content-Length` still promising
    /// the whole thing - an upstream that went away in the middle of an answer.
    Cut(usize),
}

/// Answers every request with `body`, delivered however `delivery` says.
///
/// note: it answers more than one connection, because a provider that retries a request is a
/// provider doing what it is supposed to do and should not deadlock a test for it.
///
/// note: `Content-Length` rather than a chunked body, so that the server says nothing about
/// framing that the provider might lean on. What is being tested is what it does with the bytes.
async fn server(body: &'static str, delivery: Delivery) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut discard = [0u8; 16384];
            let _ = socket.read(&mut discard).await;
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                         Content-Length: {}\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;

            let bytes = body.as_bytes();
            match delivery {
                Delivery::Split(at) if at < bytes.len() => {
                    let _ = socket.write_all(&bytes[..at]).await;
                    let _ = socket.flush().await;
                    // long enough for the first half to be read on its own, which is the point
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = socket.write_all(&bytes[at..]).await;
                }
                // the promised length is the whole body and this is not it, so the client sees a
                // body that stops arriving rather than a short answer
                Delivery::Cut(at) if at < bytes.len() => {
                    let _ = socket.write_all(&bytes[..at]).await;
                }
                _ => {
                    let _ = socket.write_all(bytes).await;
                }
            }
            let _ = socket.flush().await;
            let _ = socket.shutdown().await;
        }
    });

    format!("http://{address}")
}
