//! A suite every provider in this workspace has to pass, whichever dialect it speaks.
//!
//! note: this exists because three providers here are one provider written three times.
//! `kamchatka`'s OpenAI-compatible one and this crate's share 418 identical lines; `kamchatka`'s
//! two share 286. They cannot simply be merged - `kamchatka` is published and this crate is
//! permanently unpublished, so nothing published may depend on it - and the cost of that has been
//! paid three times in a row, each time the same way: a bug found in one copy, fixed in one copy.
//! The stream that decoded each chunk lossily was in all three. The tool-call fragments filed by a
//! missing index were in two. Arguments that would not parse were handled one way in a file's
//! streamed path and another in its whole-answer path.
//!
//! note: so the answer here is not deduplication but agreement. Each provider is asked the same
//! questions through a real socket, and a case added to this module applies to every provider at
//! once without any of them being edited. Every case below is a bug that actually happened.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use nachalnik_utils::conformance::Conformance;
//! # async fn go() {
//! Conformance::openai("my provider", |url| Arc::new(my_provider(url)))
//!     .check()
//!     .await;
//! # }
//! # fn my_provider(_: String) -> nachalnik_utils::OpenAiCompatible { unimplemented!() }
//! ```

use std::{sync::Arc, time::Duration};

use nachalnik::{Config, ContextItem, Kernel, ModelResponse, Provider};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

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
                "what the server said a request cost is carried through",
                self.usage().await,
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
            match self.ask(body, Some(at)).await {
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

        let response = match self.ask(body, None).await {
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

        let response = match self.ask(body, None).await {
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

        match self.ask(body, None).await {
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

        let response = match self.ask(body, None).await {
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

        match self.ask(body, None).await {
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

        match self.ask(body, None).await {
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
        match self.ask("<html>502 Bad Gateway</html>", None).await {
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

        match self.ask(body, None).await {
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
        split_at: Option<usize>,
    ) -> Result<Arc<ModelResponse>, String> {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider((self.build)(server(body, split_at).await));
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

/// Answers every request with `body`, optionally in two writes split at `at` bytes.
///
/// note: it answers more than one connection, because a provider that retries a request is a
/// provider doing what it is supposed to do and should not deadlock a test for it.
///
/// note: `Content-Length` rather than a chunked body, so that the server says nothing about
/// framing that the provider might lean on. What is being tested is what it does with the bytes.
async fn server(body: &'static str, split_at: Option<usize>) -> String {
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
            match split_at {
                Some(at) if at < bytes.len() => {
                    let _ = socket.write_all(&bytes[..at]).await;
                    let _ = socket.flush().await;
                    // long enough for the first half to be read on its own, which is the point
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = socket.write_all(&bytes[at..]).await;
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
