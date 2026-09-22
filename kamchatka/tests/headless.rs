//! The program with nothing drawing it.
//!
//! note: these are the first tests in this crate that build an `App` and never draw one. Every
//! other suite that touches it presses a key and reads the characters back, which is the right way
//! to test a screen and no way at all to test whether the screen is optional. What they assert on
//! instead is the two things a headless run produces: the records, which are the session log, and
//! the prose, which is what a person watching would have read.
//!
//! note: the model is a `ScriptedProvider` and the input is a `&[u8]`, so a whole session - a
//! message, a command, a tool call, a question nobody can answer - happens in a test with no
//! terminal, no socket and no files.

use std::sync::Arc;

use kamchatka::{
    app::{App, Did, Overlay, Speaker},
    headless::Headless,
    tools::{Careful, Subject},
    wiring::{Setup, Wired},
};
use nachalnik::{
    BoxError, Capability, ContextItem, DeltaSink, Grant, ModelInfo, ModelRequest, ModelResponse,
    OutputSink, Provider, Record, Tool, ToolCall, ToolOutput, ToolSpec, Usage, Verdict,
    async_trait,
    test::{ConstTool, ScriptedProvider, call},
};
use nachalnik_providers::OpenAiCompatible;
use serde_json::json;
use tokio::io::BufReader;

mod common;

/// What one headless run wrote.
struct Run {
    app: App,
    records: String,
    prose: String,
}

impl Run {
    /// Every record the run wrote to its log, read back the way anything else would read it.
    ///
    /// note: parsed rather than matched as text, because the claim being made about this stream is
    /// that it *is* the session log - so a test that searched it for a substring would pass on
    /// something no reader could load.
    fn log(&self) -> Vec<Record> {
        self.records
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line is a record"))
            .collect()
    }

    /// The names of the events it recorded, in order.
    fn names(&self) -> Vec<String> {
        self.log()
            .iter()
            .map(|record| record.event.name().to_owned())
            .collect()
    }
}

/// A session wired the way the program wires one, with a scripted model behind it.
///
/// note: through `Setup` rather than by hand, which is the third caller of it and the point of
/// its existing: what these tests want is what `main.rs` wants, minus the six tools and the
/// child process it takes to find out what Landlock would allow. The model is swapped in
/// afterwards because the runtime lets a seam be swapped while a session is running, and a
/// scripted provider is not an `Endpoint`.
fn wired(script: Vec<ModelResponse>) -> Wired {
    capped(script, None)
}

/// The same, with a ceiling on what the provider may charge before the session stops.
fn capped(script: Vec<ModelResponse>, spend: Option<u64>) -> Wired {
    let wired = Setup {
        tools: Some(Vec::new()),
        compact: None,
        spend,
        // never spoken to: `App` holds one for `/model` and `/params`, and these use neither
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(
        "scripted",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");
    wired
        .app
        .kernel
        .set_provider(Arc::new(ScriptedProvider::new(script)));

    wired
}

/// Drives a session with these lines typed into it and this script answering, and hands back what
/// came out of both ends.
async fn run(input: &str, script: Vec<ModelResponse>, setup: impl FnOnce(&App)) -> Run {
    run_with(input, script, Grant::Deny, setup).await
}

/// The same, saying what an unanswerable question is answered with.
async fn run_with(
    input: &str,
    script: Vec<ModelResponse>,
    on_ask: Grant,
    setup: impl FnOnce(&App),
) -> Run {
    driven(input, script, on_ask, None, setup).await
}

/// The same again, with a ceiling on what the provider may charge before the run stops.
///
/// note: `Grant::Allow`, because the run these are about is one that was allowed to do things and
/// then did too many of them. A ceiling over a session that is refused everything would be a
/// ceiling over one request.
async fn run_capped(
    input: &str,
    script: Vec<ModelResponse>,
    tokens: u64,
    setup: impl FnOnce(&App),
) -> Run {
    driven(input, script, Grant::Allow, Some(tokens), setup).await
}

/// A tool that takes a moment, so that a turn is still running when its response is read.
///
/// note: everything else here answers instantly, which means a whole turn's events and its outcome
/// are ready at the same moment and the loop drains them together. That is a real path and it is
/// not the only one: a turn with a tool that actually does something - every live run - leaves the
/// loop waiting on events with no outcome behind them, and what a response cost has to be read
/// there too. Both are measured; taking the accounting out of either place fails a test below.
struct Slow;

#[async_trait]
impl Tool for Slow {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("wait", "takes a moment")
    }

    async fn invoke(&self, _call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        Ok(ToolOutput::new("waited"))
    }
}

/// A model that streams its answer and then takes a moment to finish, the way a real one does.
///
/// note: `ScriptedProvider` streams too, but it does the whole of a response between two
/// instructions - so the loop never looks at what has been said *while* an answer is half
/// arrived, which is where a live run spends its time and where the only bug either of these
/// found was hiding. The gap is the point of this type; the sleep is the gap.
struct Trickle {
    text: String,
    input: u64,
    output: u64,
}

#[async_trait]
impl Provider for Trickle {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: Some(128_000),
            ..ModelInfo::new("trickle", "trickle")
        }
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        deltas.text(self.text.clone());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        Ok(priced(
            ModelResponse::text(self.text.clone()),
            self.input,
            self.output,
        ))
    }
}

/// A response that says what it cost, the way a provider's does.
///
/// note: both halves, because both are the bill. `Usage` defines `input + output` to be what a
/// request came to whichever dialect answered it, and a ceiling that counted one of them would be
/// under-reading every session with a context in it.
fn priced(response: ModelResponse, input: u64, output: u64) -> ModelResponse {
    ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            ..Default::default()
        }),
        ..response
    }
}

async fn driven(
    input: &str,
    script: Vec<ModelResponse>,
    on_ask: Grant,
    spend: Option<u64>,
    setup: impl FnOnce(&App),
) -> Run {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = capped(script, spend);
    setup(&app);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(on_ask, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, input.as_bytes())
        .await
        .expect("the run failed");

    Run {
        app,
        records: String::from_utf8(records).expect("the records are text"),
        prose: String::from_utf8(prose).expect("the prose is text"),
    }
}

/// A message goes in as a message, and what the model says comes back out.
#[tokio::test]
async fn a_line_is_a_message_and_the_answer_is_printed() {
    let run = run(
        "what is 2+2\n",
        vec![ModelResponse::text("4, and I checked")],
        |_| {},
    )
    .await;

    assert!(run.prose.contains("4, and I checked"), "{}", run.prose);
    // the question is in the context as an item, which is what makes it part of the next request
    // rather than a line somebody typed
    let items = run.app.kernel.items();
    assert_eq!(items[0].content.to_text(), "what is 2+2");
    assert!(run.names().contains(&"model.requested".to_owned()));
}

/// The stream on stdout is the session log, not a rendering of it.
///
/// note: the claim this pins is the one that makes the mode worth having - that a reader of the
/// stream and a reader of `/save`'s file are reading the same thing. Equality against
/// `Kernel::history`, rather than a shape check, because "the same bytes" is the claim.
#[tokio::test]
async fn the_records_are_the_session_log() {
    let run = run(
        "hello\n",
        vec![ModelResponse::text("hello yourself")],
        |_| {},
    )
    .await;

    let written = run.log();
    let history = run.app.kernel.history();
    assert_eq!(written, history, "the stream and the log disagree");
    // and the two a subscriber cannot have: the first is emitted while the kernel is still being
    // built, and the last is emitted after the loop would have stopped listening. Both are in the
    // log, so both are in the stream
    assert_eq!(
        run.names().first().map(String::as_str),
        Some("session.started")
    );
    assert_eq!(
        run.names().last().map(String::as_str),
        Some("session.finished")
    );
}

/// A command runs, and says what it has to say, with no keyboard anywhere.
#[tokio::test]
async fn a_command_needs_no_keys_and_is_not_silent() {
    let run = run("/seams\n", Vec::new(), |_| {}).await;

    // `/seams` answers in an overlay, which is the half that would have gone nowhere: the screen
    // is what reads one, and there is no screen
    assert!(run.prose.contains("projector"), "{}", run.prose);
    assert!(run.prose.contains("nachalnik::projection"), "{}", run.prose);
    // and nothing was sent: a command is answered here rather than by asking the model
    assert!(!run.names().contains(&"model.requested".to_owned()));
}

/// A question nobody is there to answer is refused, and the model is told.
#[tokio::test]
async fn an_unanswerable_question_is_denied_by_default() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("told it was refused"),
    ];
    let run = run("look around\n", script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    assert!(run.names().contains(&"permission.decided".to_owned()));
    // the tool never ran, and the model was handed a result saying so rather than silence
    assert!(!run.names().contains(&"tool.started".to_owned()));
    let refusal = run
        .app
        .kernel
        .items()
        .iter()
        .any(|item| item.content.to_text().contains("not permitted"));
    assert!(refusal, "the model was not told");
    assert!(
        run.prose.contains("nobody is here to be asked"),
        "{}",
        run.prose
    );
}

/// A tool of several operations, taking its arguments under a `call` the way this program's own
/// do, so that a call which names none of them can be asked about.
struct Several;

#[async_trait]
impl Tool for Several {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("fs", "the filesystem")
            .with_schema(Arc::new(json!({
                "type": "object",
                "properties": { "call": { "type": "object" } },
                "required": ["call"],
            })))
            .with_capabilities([Capability::fs("read"), Capability::fs("write")])
    }

    async fn invoke(&self, _call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        Ok(ToolOutput::new("read it"))
    }
}

/// A run with nobody at it says why a rule that was given did not answer the question.
///
/// note: the whole of the failure this is about happened in one session and cost twenty calls. A
/// model wrote the wrapper as a string of JSON, so no operation could be read out of it, so the
/// call declared every operation `fs` has - and `--allow fs:read` then matched nothing. What the
/// run said was `deny, because nobody is here to be asked`, which is true and is about the wrong
/// thing: the model went looking for a different approach, and the person watching had no way to
/// see that their rule and the call could never meet.
#[tokio::test]
async fn a_call_that_names_no_operation_says_why_the_rule_missed_it() {
    let script = vec![
        ModelResponse::tool_calls(vec![call(
            "c1",
            "fs",
            json!({ "call": "{\"action\": \"read\", \"path\": \"x\"}" }),
        )]),
        ModelResponse::text("told it was refused"),
    ];
    let run = run("read it\n", script, |app| {
        app.kernel.add_tool(Arc::new(Several));
        app.policy
            .set(&Subject::Capability(Capability::fs("read")), Verdict::Allow);
    })
    .await;

    assert!(
        run.prose.contains("names no operation"),
        "the reason the rule missed is the one thing this run could not work out: {}",
        run.prose
    );
    assert!(
        run.prose.contains("`fs:read`"),
        "and it names the rule that would have answered: {}",
        run.prose
    );
}

/// `--on-ask allow` is the other answer, and the tool runs.
#[tokio::test]
async fn the_other_answer_lets_it_run() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("read it"),
    ];
    let run = run_with("look around\n", script, Grant::Allow, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    assert!(run.names().contains(&"tool.finished".to_owned()));
    let answered = run
        .app
        .kernel
        .items()
        .iter()
        .any(|item| item.content.to_text().contains("the answer"));
    assert!(answered, "the tool's output is not in the context");
}

/// A question that was decided in advance is never asked at all.
///
/// note: the difference between this and the two above is where the answer came from - a policy
/// that already knows, rather than a turn that stopped. Both are reachable without a screen and
/// they are not the same path: this one never reaches `State::Deciding`.
#[tokio::test]
async fn a_pre_answered_capability_is_not_a_question() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("read it"),
    ];
    let run = run("look around\n", script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
        app.policy
            .set(&Subject::Capability(Capability::fs("read")), Verdict::Allow);
    })
    .await;

    assert!(run.names().contains(&"tool.finished".to_owned()));
    assert!(
        !run.prose.contains("nobody is here to be asked"),
        "it was asked after all: {}",
        run.prose
    );
}

/// The input closing does not abandon the turn it closed during.
///
/// note: the input here is one line and then end-of-file, which is what a pipe does. The turn it
/// started is still in flight at that moment, and the loop is not allowed to take that as the end
/// of the session - `echo "question" | kamchatka` wants the answer, not a session that stopped
/// half way through asking for it.
#[tokio::test]
async fn the_end_of_the_input_waits_for_the_turn() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("the whole turn ran"),
    ];
    let run = run_with("go\n", script, Grant::Allow, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    assert!(run.prose.contains("the whole turn ran"), "{}", run.prose);
    assert!(run.names().contains(&"tool.finished".to_owned()));
}

/// Two lines are two turns, in the order they were typed.
#[tokio::test]
async fn a_second_line_is_a_second_turn() {
    let run = run(
        "first\nsecond\n",
        vec![ModelResponse::text("one"), ModelResponse::text("two")],
        |_| {},
    )
    .await;

    let asked: Vec<String> = run
        .app
        .kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, nachalnik::ContextKind::UserMessage))
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(asked, vec!["first".to_owned(), "second".to_owned()]);
    assert_eq!(
        run.names()
            .iter()
            .filter(|name| *name == "model.requested")
            .count(),
        2
    );
}

/// A question left standing by a `/step` does not hold the session open for ever.
///
/// note: this is the one case the exit condition gets wrong if a waiting question is allowed to
/// keep the loop alive. `/step` stops at the question and does *not* resume once it is answered -
/// somebody asked to drive - so at the end of the input there is a question answered, nothing
/// running, and nothing that will ever arrive to wake the loop again. It ran for ever, and the
/// timeout here is what says so rather than hanging the suite.
///
/// note: measured. The clause this is about was in the loop before this test was, and removing it
/// broke nothing at all - which is how it was found: the exception it made for a waiting question
/// was the bug, not the protection.
#[tokio::test]
async fn a_question_left_by_a_step_does_not_hang_the_session() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("unused"),
    ];
    let run = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run("/step look around\n", script, |app| {
            app.kernel.add_tool(Arc::new(
                ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
            ));
        }),
    )
    .await
    .expect("the session never ended");

    assert!(run.names().contains(&"permission.decided".to_owned()));
    // answered, and deliberately not carried on with: a step is somebody driving
    assert!(!run.names().contains(&"tool.started".to_owned()));
}

/// One line answers the caller that sent it, without a transcript to read back.
///
/// note: this is the half `Headless` no longer has to scrape. Every assertion here used to be a
/// watermark over `App::loose` and a `take()` of `App::overlay` - which is how a *screen* finds
/// out what happened, because a screen re-reads both every frame, and which an embedder driving
/// one line at a time had no business having to do.
#[tokio::test]
async fn a_line_answers_the_caller() {
    let mut app = wired(vec![ModelResponse::text("hello")]).app;

    // a command that opens a page hands the page back, title and all
    let reply = app.submit("/seams").await;
    assert_eq!(reply.did, Did::Ran);
    let Some(Overlay::Text { title, pages, .. }) = reply.page else {
        panic!("`/seams` opened no page");
    };
    assert_eq!(title, "what is plugged into the runtime");
    assert!(pages[0].body.contains("projector"), "{}", pages[0].body);

    // a command that only says a line hands back the line, and no page - the one `/seams` opened
    // is still on the overlay, and reporting it again is what this is careful not to do
    let reply = app.submit("/tools toggle nothing").await;
    assert!(reply.page.is_none(), "a stale page came back");
    assert_eq!(reply.said.len(), 1);
    assert_eq!(reply.said[0].speaker, Speaker::Error);
    assert!(reply.said[0].text.contains("no tool called"));

    // and a message says what it did with it
    let reply = app.submit("hello").await;
    assert!(matches!(reply.did, Did::Asked(_)));
    assert!(reply.page.is_none());
}

/// A line sent into a running turn says that it is waiting, rather than that it was asked.
#[tokio::test]
async fn a_line_into_a_running_turn_says_it_is_queued() {
    let mut app = wired(vec![ModelResponse::text("one")]).app;

    app.submit("first").await;
    // the turn started by that line is still in flight, which is the state the headless loop
    // refuses to read a line in and an embedder with its own loop can walk straight into
    let reply = app.submit("second").await;
    assert_eq!(reply.did, Did::Queued);
    assert_eq!(
        app.kernel.items().len(),
        1,
        "the second line went into the context while a turn was running"
    );
}

/// A deadline ends a session that is waiting on an input that is never going to say anything.
///
/// note: the input here is a pipe whose other end is held open, so it neither yields a line nor
/// closes - which is a session waiting for somebody who has gone away, and the shape a deadline
/// exists for. Without one this test does not fail, it hangs, so the timeout around it is what
/// turns that into a failure somebody can read.
///
/// note: and the deadline is the *driver's* rather than this timeout. That is the difference the
/// commit was about: a `timeout` around the whole loop drops it where it stands, so the session
/// is never finished and the last records are written nowhere. Here the records still arrive -
/// `session.finished` among them - which is what the second assertion is checking.
#[tokio::test]
async fn a_deadline_ends_a_session_that_is_waiting_for_nobody() {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(Vec::new());
    // held open for the length of the test: dropping it would close the pipe and end the session
    // by the ordinary door, which is the thing being told apart from a deadline
    let (_held, reader) = tokio::io::duplex(64);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        Headless::new(Grant::Deny, &mut records, &mut prose)
            .deadline(std::time::Duration::from_millis(100))
            .run(&mut app, &mut events, &mut finished, BufReader::new(reader))
            .await
            .expect("the run failed")
    })
    .await
    .expect("the deadline did not end it");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("out of time"), "{prose}");
    let names: Vec<String> = String::from_utf8(records)
        .expect("the records are text")
        .lines()
        .map(|line| {
            serde_json::from_str::<Record>(line)
                .expect("every line is a record")
                .event
                .name()
                .to_owned()
        })
        .collect();
    assert_eq!(names.last().map(String::as_str), Some("session.finished"));
}

/// A ceiling ends the session once the provider has charged past it, and the next line goes unread.
///
/// note: the sibling of the deadline above, and the same kind of guard for a different thing a run
/// nobody is watching can spend. What it stops here is *between* turns: two lines were piped in,
/// the second answer crossed the line, and the third was never read - which is the shape a script
/// driving a long session has.
#[tokio::test]
async fn a_ceiling_ends_the_run_once_the_bill_passes_it() {
    let script = vec![
        priced(ModelResponse::text("one"), 400, 200),
        priced(ModelResponse::text("two"), 400, 200),
        priced(ModelResponse::text("three"), 400, 200),
    ];
    let run = run_capped("first\nsecond\nthird\n", script, 1000, |_| {}).await;

    // 1,200 rather than 1,000: a ceiling is a stopping rule and not a cap, because what a response
    // costs is known only once it has arrived. Both halves of the bill are in that figure
    assert!(
        run.prose.contains("spent 1,200 tokens of 1,000; stopping"),
        "{}",
        run.prose
    );
    assert_eq!(
        run.names()
            .iter()
            .filter(|name| *name == "model.requested")
            .count(),
        2,
        "the third line was read after the ceiling was reached"
    );
    // and it was not read and refused, it was not read: a script piped into a session that has
    // stopped should not come back as a refusal a line, which is what the loop stops reading for
    assert!(
        !run.prose.contains("nothing more is being sent"),
        "{}",
        run.prose
    );
    // and it left by the ordinary door, which is the whole difference between this and a kill: the
    // session is ended and the log says so
    assert_eq!(
        run.names().last().map(String::as_str),
        Some("session.finished")
    );
}

/// It stops a turn that is in flight, rather than waiting for one to end.
///
/// note: this is the case it exists for. A model that has found a loop - a tool that fails the
/// same way every time, a question it keeps re-asking - spends its budget *inside* one turn, and a
/// guard that only looked between them would watch the whole of it go. The script here is a tool
/// call that would be answered and called again, and the ceiling is crossed on the first response.
#[tokio::test]
async fn a_ceiling_interrupts_the_turn_it_is_crossed_in() {
    let script = vec![
        priced(
            ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
            900,
            300,
        ),
        priced(
            ModelResponse::tool_calls(vec![call("c2", "peek", json!({}))]),
            900,
            300,
        ),
        priced(ModelResponse::text("never reached"), 900, 300),
    ];
    let run = run_capped("go\n", script, 1000, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    assert!(run.names().contains(&"turn.interrupted".to_owned()));
    assert_eq!(
        run.names()
            .iter()
            .filter(|name| *name == "model.requested")
            .count(),
        1,
        "the turn asked again after the ceiling was reached"
    );
    assert!(!run.prose.contains("never reached"), "{}", run.prose);
}

/// It is charged where a live run charges it: off the events, with the turn still going.
///
/// note: the test above crosses the line in a turn whose every step was over before the loop looked
/// again, so what read the response was the drain that runs behind a finished turn. This one has a
/// tool that takes long enough for the loop to come round while the turn is still in flight, which
/// is where a real one spends nearly all of its time - and the response is read on the ordinary
/// event branch instead. Both are needed, and each of these fails if the other's is taken out.
#[tokio::test]
async fn a_ceiling_is_charged_while_the_turn_is_still_running() {
    let script = vec![
        priced(
            ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
            900,
            300,
        ),
        priced(ModelResponse::text("never reached"), 900, 300),
    ];
    let run = run_capped("go\n", script, 1000, |app| {
        app.kernel.add_tool(Arc::new(Slow));
    })
    .await;

    assert!(
        run.prose.contains("spent 1,200 tokens of 1,000; stopping"),
        "{}",
        run.prose
    );
    // the point of reading it there: the turn is stopped before it asks again, rather than after
    assert!(!run.prose.contains("never reached"), "{}", run.prose);
}

/// Nothing else is sent afterwards, however it is asked for.
///
/// note: this is what makes the ceiling a bound rather than a report, and it is the reason the
/// counting lives on `App` rather than in the loop below. Stopping the turn that crossed the line
/// is half of it; a caller that hands in the next line - a script, a person, an embedder with a
/// loop of its own - would start spending again, and `start_turn` is where all three of them meet.
#[tokio::test]
async fn nothing_more_is_sent_once_the_ceiling_is_reached() {
    let script = vec![
        priced(ModelResponse::text("one"), 400, 800),
        ModelResponse::text("never reached"),
    ];
    let mut app = run_capped("first\n", script, 1000, |_| {}).await.app;

    // the run is over and the `App` is not: this is the embedder that carries on calling
    let reply = app.submit("second").await;
    assert!(!app.busy, "a turn started after the ceiling was reached");
    assert!(
        reply
            .said
            .iter()
            .any(|entry| entry.text.contains("nothing more is being sent")),
        "{:?}",
        reply.said
    );
}

/// And it can be raised from a line, which is the way back for whoever set it too low.
///
/// note: `/spend` rather than a public field, because raising the ceiling has to let a stopped
/// session go again - two fields moving together, which is exactly what a setter is for.
#[tokio::test]
async fn the_ceiling_can_be_raised_and_taken_away() {
    let script = vec![
        priced(ModelResponse::text("one"), 400, 800),
        ModelResponse::text("two"),
    ];
    let mut app = run_capped("first\n", script, 1000, |_| {}).await.app;

    let reply = app.submit("/spend").await;
    assert!(
        reply.said[0].text.contains("1,200 tokens spent of 1,000"),
        "{:?}",
        reply.said
    );

    app.submit("/spend 5000").await;
    app.submit("second").await;
    assert!(app.busy, "raising the ceiling did not let it go again");

    app.submit("/spend 0").await;
    assert_eq!(app.spend(), None);
    assert_eq!(
        app.spent(),
        1200,
        "taking the ceiling away forgot the total"
    );

    // and one set under what has already gone stops the session there. That is a reasonable thing
    // to ask for - it is one way to say "enough" - and a poor thing to find out by being refused
    let reply = app.submit("/spend 100").await;
    assert!(app.overspent());
    assert!(
        reply.said[0].text.contains("nothing more will be sent"),
        "{:?}",
        reply.said
    );
}

/// What the program says while an answer is still arriving reaches the person reading it.
///
/// note: found live and then written down, which is the wrong order and the only one that was
/// available. A model that streams leaves a line in `App::loose` that the *context* takes over
/// once the turn is recorded, so the list shrinks - and the loop was marking its place in it by
/// length, so anything said between a shrink and the next look was skipped. The ceiling's own
/// notice was the line that went missing: decided, recorded, the turn interrupted, and nothing
/// printed. Every note said in the same breath as a response was exposed to this, the reasoning
/// notice included.
///
/// note: it needs a model that pauses mid-answer, because with a scripted one the whole turn
/// happens between two looks at the list and nothing shrinks in between. That is why this is the
/// one test here with a provider of its own.
#[tokio::test]
async fn a_line_said_while_an_answer_streams_is_not_swallowed() {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = capped(Vec::new(), Some(1000));
    app.kernel.set_provider(Arc::new(Trickle {
        text: "here is a long answer that arrives before the turn is recorded".to_owned(),
        input: 900,
        output: 300,
    }));

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, &b"go\n"[..])
        .await
        .expect("the run failed");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("here is a long answer"), "{prose}");
    assert!(prose.contains("spent 1,200 tokens of 1,000"), "{prose}");
}

/// The total is kept whether or not anything is watching it, so a ceiling set later means what it
/// says.
///
/// note: also found by reading a live run rather than by thinking. The counting was inside the
/// ceiling's own guard, so a session with no ceiling counted nothing - and `/spend` answered
/// `0 tokens spent` after a turn that had plainly cost some, which is the figure being wrong in
/// the one place somebody looks at it. Worse than wrong: `/spend N` half way through a session
/// would then have started from zero and given away everything spent up to that point.
#[tokio::test]
async fn the_total_is_counted_with_no_ceiling_to_count_it_against() {
    let script = vec![priced(ModelResponse::text("one"), 400, 200)];
    let mut app = run("first\n", script, |_| {}).await.app;

    assert_eq!(app.spent(), 600);
    let reply = app.submit("/spend").await;
    assert!(
        reply.said[0].text.contains("600 tokens spent"),
        "{:?}",
        reply.said
    );
}

/// An endpoint that reports no figures says so, rather than holding a ceiling nothing can reach.
///
/// note: the failure this is about is silence. A run given `--spend 50000` against a provider that
/// reports no usage would go on for ever having spent nothing as far as anybody here can tell, and
/// whoever set the number would read that run as bounded. It is said once, at the first response
/// that came with nothing on it, and `--deadline` is the guard that needs nobody's cooperation.
#[tokio::test]
async fn a_ceiling_over_an_endpoint_that_reports_nothing_says_so() {
    let script = vec![ModelResponse::text("no usage on this one")];
    let run = run_capped("go\n", script, 1000, |_| {}).await;

    assert!(run.prose.contains("reports no usage"), "{}", run.prose);
    assert!(!run.prose.contains("stopping"), "{}", run.prose);
    assert!(run.prose.contains("no usage on this one"), "{}", run.prose);
}

/// The program itself runs headless, and keeps its two streams apart.
///
/// note: everything else here drives the loop in process, which says nothing about the half a
/// caller actually meets: the arguments, the mode it chose, and which stream each thing goes to.
/// This one runs the binary. It cannot get as far as a model - there is no endpoint to reach and
/// no key to reach it with - and that is the case worth pinning anyway, because a program whose
/// stdout is a stream of JSON must not put a sentence in it when something goes wrong. A reader
/// would be part way through a session before hitting a line that is not a record.
///
/// note: no `--headless` in the arguments. The mode is chosen because stdout is a pipe here,
/// which is the auto-detection doing its job, and the announcement is on stderr where it belongs.
#[test]
fn the_program_runs_headless_and_keeps_its_streams_apart() {
    // the test binary lives beside it
    let mut program = std::env::current_exe().expect("a test binary has a path");
    program.pop();
    if program.ends_with("deps") {
        program.pop();
    }
    program.push("kamchatka");

    let out = std::process::Command::new(&program)
        .args(["-m", "nothing-serves-this", "--no-record", "hello"])
        // an address nothing answers on: the session is set up, the request is built, and the
        // send is what fails - which is the failure a piped run actually meets
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("stdout is not a terminal"),
        "it did not say which mode it chose: {said}"
    );
    assert!(
        said.contains("error sending request"),
        "the failure was not reported: {said}"
    );
    assert!(
        !out.status.success(),
        "an unreachable model is not a success"
    );

    // not that stdout is empty - a session that failed still happened, and its log is the whole
    // point of the stream. What matters is that every line of it is a record: one sentence in
    // there and a reader is part way through a session before hitting something it cannot parse
    let records = String::from_utf8(out.stdout).expect("the records are text");
    assert!(
        !records.is_empty(),
        "the session was not written out at all"
    );
    for line in records.lines() {
        serde_json::from_str::<Record>(line).unwrap_or_else(|e| {
            panic!("a line of the record stream is not a record ({e}): {line}")
        });
    }
}

/// A resumed run still says how it is being driven.
///
/// note: it used to say one or the other. The opening line and the replay line were arms of one
/// match over `(resumed, headless)`, so a session carried on from a file was told what it had
/// picked up and not what would happen to a question nobody is there to answer - which is the
/// thing a headless run most needs said, and the thing a resumed one is no less headless for.
///
/// note: through the binary, because the branch is `main`'s. No key and no endpoint: resuming a
/// file and printing two lines reaches neither.
#[test]
fn a_resumed_headless_run_says_both_what_it_picked_up_and_how_it_is_driven() {
    let (wired, dir) = (wired(Vec::new()), common::scratch("resumed"));
    wired
        .app
        .kernel
        .push(nachalnik::ContextItem::user("the word is ZEPHYR"));
    let path = dir.join("session.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&wired.app.kernel.snapshot()).expect("a snapshot serializes"),
    )
    .expect("written");

    let mut program = std::env::current_exe().expect("a test binary has a path");
    program.pop();
    if program.ends_with("deps") {
        program.pop();
    }
    program.push("kamchatka");

    let out = std::process::Command::new(&program)
        .args(["--headless", "--no-record", "-r"])
        .arg(&path)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("resumed session"), "{said}");
    assert!(
        said.contains("a question nobody can be asked is answered"),
        "a resumed run was not told how it is driven: {said}"
    );
}

/// `ctrl+c` stops a run that is waiting for somebody, and the session survives it.
///
/// note: the last of the three things that can stop a run nobody is watching - the other two have
/// had tests since they were written and this one had none, because it is a *signal* and the suite
/// had no way to send one. It does: the run is a child process, and `kill -INT` is a command.
/// What it asserts is the difference between stopping and being killed - the line that says so,
/// the `session.finished` record at the end of the log, and an exit that is not a failure.
///
/// note: `#[cfg(unix)]` because that is where the mechanism is. Windows has `ctrl+c` and no
/// `kill`, and a test that pretended otherwise would be testing its own shim.
#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_headless_run_rather_than_killing_it() {
    use std::io::Read as _;

    let program = common::program();

    // stdin is a pipe this test holds open and never writes to, which is a run waiting for
    // somebody who has not typed anything yet - the state `ctrl+c` is for
    let mut child = std::process::Command::new(&program)
        .args(["--headless", "--no-record", "-m", "nothing-serves-this"])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    // once it has said what it is, it is in the loop with the signal branch armed
    let mut said = String::new();
    let mut stderr = child.stderr.take().expect("stderr is a pipe");
    while !said.contains("headless:") {
        let mut byte = [0u8; 1];
        if stderr.read(&mut byte).expect("it is still running") == 0 {
            panic!("it ended before it was interrupted: {said}");
        }
        said.push(byte[0] as char);
    }

    let killed = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("`kill` is on the path");
    assert!(killed.success());

    let status = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match child.try_wait().expect("it was spawned") {
                Some(status) => break status,
                None if std::time::Instant::now() > deadline => {
                    let _ = child.kill();
                    panic!("it did not stop: {said}");
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    };
    stderr.read_to_string(&mut said).expect("the rest of it");

    assert!(status.success(), "a stopped run is not a failure: {said}");
    assert!(
        said.contains("what has arrived is kept"),
        "it did not say it was stopping: {said}"
    );

    // and the log is a whole session rather than however much of one had been flushed, which is
    // the difference a cooperative stop is for
    let mut records = String::new();
    child
        .stdout
        .take()
        .expect("stdout is a pipe")
        .read_to_string(&mut records)
        .expect("the records are text");
    let names: Vec<String> = records
        .lines()
        .map(|line| {
            serde_json::from_str::<Record>(line)
                .expect("every line is a record")
                .event
                .name()
                .to_owned()
        })
        .collect();
    assert_eq!(names.last().map(String::as_str), Some("session.finished"));
}

/// A run that was not asked to take `ctrl+c` subscribes to nothing.
///
/// note: what it costs to subscribe anyway is the thing that cannot be asserted from in here.
/// Subscribing installs a process-wide handler for the life of the process, so a subscription
/// taken beside a branch that is switched off is SIGINT quietly disabled for a caller who never
/// asked for any of it: the handler is in, nothing polls it, and the default that would have killed
/// the process is gone. A test that pressed it would be a test that killed the runner, or one that
/// checked a shim.
///
/// note: what *is* assertable is the other thing subscribing needs, and it is the half that broke
/// an embedder rather than a signal. `tokio::signal::unix::signal` needs the runtime's signal
/// driver, which comes with the io driver - so a host driving this on
/// `new_current_thread().enable_time()` panicked on a subscription it had asked not to have. The
/// runtime here is that host, built by hand because `#[tokio::test]` enables everything.
#[test]
fn a_run_that_was_not_asked_for_ctrl_c_subscribes_to_nothing() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime without io is still a runtime");

    let prose = runtime.block_on(async {
        let Wired {
            mut app,
            mut events,
            mut finished,
        } = wired(vec![ModelResponse::text("4, and I checked")]);

        let (mut records, mut prose) = (Vec::new(), Vec::new());
        Headless::new(Grant::Deny, &mut records, &mut prose)
            .run(&mut app, &mut events, &mut finished, &b"what is 2+2\n"[..])
            .await
            .expect("the run failed");

        String::from_utf8(prose).expect("the prose is text")
    });

    assert!(prose.contains("4, and I checked"), "{prose}");
}

/// `/quit` ends the session from a line, the way it does from a prompt.
#[tokio::test]
async fn quit_ends_it() {
    let run = run(
        "/quit\nnever asked\n",
        vec![ModelResponse::text("unused")],
        |_| {},
    )
    .await;

    assert!(run.app.quit);
    assert!(
        run.app.kernel.items().is_empty(),
        "the line after /quit was read anyway"
    );
}

/// `/restart` hands the session back the same way `/quit` does, and is not the same answer.
///
/// note: the pair with the test above, and the half worth having a test for is the second
/// assertion. Both words end the loop - `App::leaving` is what it asks - and what the loop does
/// next turns entirely on which flag is set. A restart that also set `quit` would write the
/// session out and then stop the program, which is the one outcome neither word means.
#[tokio::test]
async fn restart_ends_it_without_ending_the_program() {
    let run = run(
        "/restart\nnever asked\n",
        vec![ModelResponse::text("unused")],
        |_| {},
    )
    .await;

    assert!(run.app.restart);
    assert!(!run.app.quit, "a restart is not a quit");
    assert!(run.app.leaving(), "the loop is told to let go either way");
    assert!(
        run.app.kernel.items().is_empty(),
        "the line after /restart was read anyway"
    );
}

// ------------------------------------------------------------------- the program, and a socket

/// `ctrl+c` stops a command that is running, and what arrived is kept.
///
/// note: the case this was written to test was a *second* press leaving a turn the first could not
/// stop - and it turns out there is no such turn to be had out of this program. Measured: a model
/// that has merely gone quiet ends the run in about 200ms on one press, because the provider
/// watches the interrupt while it waits; and a `shell` command halfway through `sleep 30` is
/// *killed* by one press, which is the re-exec being load-bearing rather than tidy. So what is
/// asserted here is what actually happens, and the second press has a test of its own next to a
/// tool that really will not stop - see `a_second_press_leaves_a_tool_that_will_not_stop`.
///
/// note: the endpoint is a socket rather than a scripted provider because the program builds its
/// own provider in a process of its own, and a listener is the only seam a child process has.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_stops_a_command_that_is_running_and_keeps_what_arrived() {
    let base = common::endpoint(vec![format!(
        "data: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
            {"index": 0, "id": "c1", "type": "function",
             "function": {"name": "shell", "arguments": "{\"cmd\": \"sleep 30\"}"}}
        ]}, "finish_reason": "tool_calls"}]})
    )])
    .await;

    let mut child = std::process::Command::new(common::program())
        .args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--allow",
            "shell",
            "go",
        ])
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    let said = watch(child.stderr.take().expect("stderr is a pipe"));
    let waited = std::time::Instant::now();
    while !said.lock().contains("⟩ shell(") {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(20),
            "it never reached the tool: {}",
            said.lock()
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let pressed = std::time::Instant::now();
    interrupt(child.id());
    let status = waited_out(&mut child, std::time::Duration::from_secs(20), &said);

    assert!(
        status.success(),
        "stopping is not a failure: {}",
        said.lock()
    );
    assert!(
        pressed.elapsed() < std::time::Duration::from_secs(20),
        "`sleep 30` outlived the interrupt, so the command was waited for rather than stopped"
    );
    let said = said.lock().clone();
    assert!(said.contains("what has arrived is kept"), "{said}");
    // the result of the call it was in the middle of is on the record, which is the whole of what
    // "what has arrived is kept" means
    assert!(said.contains("· shell:"), "{said}");

    let mut records = String::new();
    std::io::Read::read_to_string(
        &mut child.stdout.take().expect("stdout is a pipe"),
        &mut records,
    )
    .expect("the records are text");
    let last = records
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .next_back()
        .expect("a record");
    assert_eq!(last.event.name(), "session.finished");
}

/// Reads a child's output into a string as it arrives, so that a test can look at it without
/// blocking on a pipe that may never say another word.
#[cfg(unix)]
fn watch(mut stream: std::process::ChildStderr) -> Arc<parking_lot::Mutex<String>> {
    let said = Arc::new(parking_lot::Mutex::new(String::new()));
    let writing = said.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(read) = std::io::Read::read(&mut stream, &mut buf) {
            if read == 0 {
                break;
            }
            writing
                .lock()
                .push_str(&String::from_utf8_lossy(&buf[..read]));
        }
    });

    said
}

/// Presses `ctrl+c` at a child process.
#[cfg(unix)]
fn interrupt(pid: u32) {
    let sent = std::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .expect("`kill` is on the path");
    assert!(sent.success());
}

/// Waits for a child to leave, or says what it had said when it did not.
#[cfg(unix)]
fn waited_out(
    child: &mut std::process::Child,
    within: std::time::Duration,
    said: &Arc<parking_lot::Mutex<String>>,
) -> std::process::ExitStatus {
    let waited = std::time::Instant::now();
    loop {
        match child.try_wait().expect("it was spawned") {
            Some(status) => break status,
            None if waited.elapsed() > within => {
                let _ = child.kill();
                panic!("it did not leave: {}", said.lock());
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

/// A second `ctrl+c` leaves a tool that will not stop.
///
/// note: what the second press is *for*, and finding a case to show it in took measuring three.
/// A model that has gone quiet ends on the first press in about 200ms, because the provider
/// watches the interrupt while it waits; a `shell` command is killed by the first press, because
/// the re-exec means the process that dies is the command. Neither is a turn the first press
/// cannot stop. What is one is somebody else's tool: a kernel interrupt lands between steps, and
/// an MCP call already in flight is not between steps - so a server that never answers holds the
/// turn open for as long as it likes, and `sleep 600` in forty lines of Python is exactly that.
///
/// note: which makes this a test of the thing as well as of the guard. An MCP server that wedges
/// is a real hazard - somebody else's process, on the other side of a pipe - and what it must not
/// be able to do is hold the program hostage.
#[cfg(all(unix, feature = "mcp"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_second_press_leaves_a_tool_that_will_not_stop() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: python3 is not on the path");
        return;
    }
    let base = common::endpoint(vec![format!(
        "data: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
            {"index": 0, "id": "c1", "type": "function",
             "function": {"name": "py__hang", "arguments": "{}"}}
        ]}, "finish_reason": "tool_calls"}]})
    )])
    .await;

    let server = format!(
        "py=python3 {}",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mcp_server.py")
    );
    let mut child = std::process::Command::new(common::program())
        .args(["--headless", "--no-record", "-m", "nothing"])
        .args(["--mcp", &server, "--allow-server", "py", "go"])
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    let said = watch(child.stderr.take().expect("stderr is a pipe"));
    let waited = std::time::Instant::now();
    while !said.lock().contains("⟩ py__hang(") {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(20),
            "it never reached the tool: {}",
            said.lock()
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    interrupt(child.id());
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        child.try_wait().expect("it was spawned").is_none(),
        "the first press waits for the turn, and this turn is not coming back: {}",
        said.lock()
    );
    assert!(
        said.lock().contains("what has arrived is kept"),
        "{}",
        said.lock()
    );

    interrupt(child.id());
    let pressed = std::time::Instant::now();
    let status = waited_out(&mut child, std::time::Duration::from_secs(10), &said);

    assert!(
        status.success(),
        "leaving is not a failure: {}",
        said.lock()
    );
    assert!(
        pressed.elapsed() < std::time::Duration::from_secs(5),
        "the second press waited for the tool anyway: {:?}",
        pressed.elapsed()
    );
}

/// What one answer costs, through the flag, against something that reports a cost.
///
/// note: the ceiling has tests through the library and a live run behind it; what it had not had
/// is the path a person takes - `--spend` on a command line, into `Setup`, into the `App` that
/// enforces it. The stub reports 1,200 tokens for one answer, which is over any ceiling worth
/// typing here.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_spend_ceiling_stops_the_program_itself() {
    let base = common::endpoint(vec![answer("as much as it likes")]).await;

    let out = std::process::Command::new(common::program())
        .args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--spend",
            "100",
        ])
        .arg("go")
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("spent 1,200 tokens of 100; stopping"),
        "the ceiling did not stop it: {said}"
    );
}

/// A run that was not told to keep quiet writes the session out, and says where.
///
/// note: what every real run does at the end, and the one thing about a headless run that nothing
/// checked - the suites all pass `--no-record`, because a test that wrote a file somewhere would
/// be a test that left one. This reads the path out of the line the program prints, which is also
/// the only promise made about it: that the line names a file somebody can open.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_run_writes_the_session_where_it_says_it_did() {
    let base = common::endpoint(vec![answer("something to keep")]).await;
    let dir = common::scratch("recorded");

    let out = std::process::Command::new(common::program())
        .args(["--headless", "-m", "nothing", "go"])
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        // the program writes into a temporary directory of the system's choosing, and a test that
        // let it use the real one would leave a session behind on every run
        .env("TMPDIR", &dir)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    // the line reads `N records in <log>, and a session in <state>`, so the comma comes off
    let path = said
        .split_whitespace()
        .find_map(|word| word.strip_suffix(',').filter(|it| it.ends_with(".jsonl")))
        .unwrap_or_else(|| panic!("it named no log: {said}"));
    let written = std::fs::read_to_string(path).expect("the file it named is there");
    let names: Vec<String> = written
        .lines()
        .map(|line| {
            serde_json::from_str::<Record>(line)
                .expect("every line is a record")
                .event
                .name()
                .to_owned()
        })
        .collect();
    assert_eq!(names.first().map(String::as_str), Some("session.started"));
    assert_eq!(names.last().map(String::as_str), Some("session.finished"));
    assert!(names.contains(&"model.finished".to_owned()), "{names:?}");

    // and the snapshot beside it, which is what `-r` reads
    let beside = path.replace(".jsonl", ".json");
    let snapshot: nachalnik::Snapshot =
        serde_json::from_str(&std::fs::read_to_string(&beside).expect("a session beside the log"))
            .expect("it is a snapshot");
    assert!(
        !snapshot.items.is_empty(),
        "the snapshot carries the context"
    );
}

/// One streamed answer, with what it cost on the end of it.
#[cfg(unix)]
fn answer(text: &str) -> String {
    format!(
        "data: {}\n\ndata: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "content": text},
               "finish_reason": null}]}),
        json!({"id": "1", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
               "usage": {"prompt_tokens": 700, "completion_tokens": 500, "total_tokens": 1200}})
    )
}

/// A build with no screen runs headless at a terminal, rather than panicking at one.
///
/// note: the bug this is about shipped, and could not have been caught by anything else here:
/// every other test of this binary pipes its stdout, and that is the one condition in which the
/// missing case cannot arise. What it takes is a terminal, which `script` will allocate - so this
/// is the one test in the crate that runs the program under a pty.
///
/// note: compiled only where it is true. With `tui` on, this same command would draw a screen and
/// wait for a key, which is a test that hangs rather than one that passes; without it, there is
/// nothing to draw and the run is headless whatever stdout is.
#[cfg(all(target_os = "linux", not(feature = "tui")))]
#[test]
fn a_screenless_build_at_a_terminal_is_a_headless_run() {
    if std::process::Command::new("script")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: `script` is not on the path, so there is no pty to be had");
        return;
    }

    let out = std::process::Command::new("script")
        .args([
            "-q",
            "-c",
            &format!(
                "{} --no-record -m nothing-serves-this hello",
                common::program().display()
            ),
            "/dev/null",
        ])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("`script` ran");

    // a pty merges the two streams, which is what a person at one sees anyway
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("built without the `tui` feature"),
        "it did not say why it was headless: {said}"
    );
    assert!(
        !said.contains("there is no screen in this build"),
        "it panicked on the `unreachable!`: {said}"
    );
    // and it got as far as trying: the model is the thing that fails here, not the program
    assert!(said.contains("error sending request"), "{said}");
}

/// `--deadline` ends a run that is waiting for somebody, through the flag.
///
/// note: the third of the guards, and the last to get a test at this level - the ceiling and
/// `ctrl+c` have theirs above. The mechanism is tested through the library, with a pipe nobody
/// writes to; what this adds is the flag, and that the program leaves rather than waiting on the
/// blocking read that made every early stop hang until yesterday.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_deadline_ends_the_program_itself() {
    // stdin is a pipe this test holds and never writes to, so nothing but the deadline can end it
    let mut child = std::process::Command::new(common::program())
        .args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--deadline",
            "1",
        ])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    let said = watch(child.stderr.take().expect("stderr is a pipe"));
    let started = std::time::Instant::now();
    let status = waited_out(&mut child, std::time::Duration::from_secs(15), &said);

    assert!(
        status.success(),
        "out of time is not a failure: {}",
        said.lock()
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "it waited far longer than it was given: {:?}",
        started.elapsed()
    );
    assert!(said.lock().contains("out of time"), "{}", said.lock());
}

/// A run with no keys to press is handed the commands, and nothing about keys.
///
/// note: this used to assert the opposite - that a pipe got every page - on the grounds that a
/// caller handed one of six could not press `←` for the other five. That was the right answer to
/// the wrong question. A pipe has no `ctrl+p` either, so five of the six pages describe a program
/// it is not using; what it wants from `/help` is the verbs it can actually type. Found from a
/// browser, where the same six pages of terminal keys arrive on a phone.
#[tokio::test]
async fn help_with_no_keys_is_the_commands() {
    let run = run("/help\n", vec![], |_| {}).await;

    for section in kamchatka::help::SECTIONS {
        // `keys: false` - so, the commands, and only the commands
        let offered = section.applies(false, false);
        assert_eq!(
            run.prose.contains(section.body),
            offered,
            "`{}` should{} be in it: {}",
            section.name,
            match offered {
                true => "",
                false => " not",
            },
            run.prose
        );
    }

    // note: and it is *one* page now, so it carries no page name - the rule being that a page name
    // is what tells six of them apart, and a lone page under a title that already says what it is
    // would be chrome for its own sake. The title is what names it
    assert!(run.prose.contains("--- the commands ---"), "{}", run.prose);
    assert!(!run.prose.contains("-- commands --"), "{}", run.prose);
    // the one thing a caller with no keys must still be told about is the verbs it can type
    assert!(run.prose.contains("/attach PATH"), "{}", run.prose);
}

/// `/compact` down a pipe is taken rather than left waiting for a key that is never coming.
///
/// note: the opposite of what `--on-ask` does with a tool's question, and they are different
/// questions. A tool's is the model asking to do something nobody vouched for, so the default is
/// no. This one is the operator's own line, and a script that says `/compact` and is answered
/// "left alone" has been refused the thing it asked for. The list is on stderr either way, which
/// is where the transparency lives when there is no panel to put it in.
#[tokio::test]
async fn compact_down_a_pipe_is_taken_and_said() {
    use nachalnik::{Content, ContextItem, ToolCall};

    let call = ToolCall::new("call-1", "read", Arc::new(json!({"path": "big.rs"})));
    let run = run("/compact\n", vec![], |app| {
        app.kernel
            .set_compactor(Some(Arc::new(kamchatka::tools::Trim {
                threshold: 0.0,
                target: 0.0,
            })));
        app.kernel.push(ContextItem::user("what is in big.rs?"));
        app.kernel.push(ContextItem::assistant(
            Content::text("let me look"),
            vec![call.clone()],
        ));
        app.kernel.push(ContextItem::tool_result(
            call.id.clone(),
            "read",
            Content::text("x".repeat(40_000)),
            false,
        ));
    })
    .await;

    assert!(
        run.prose.contains("would take 1 item(s)"),
        "the list is still said: {}",
        run.prose
    );
    assert!(run.prose.contains("compacted:"), "{}", run.prose);
    assert!(
        run.app
            .kernel
            .items()
            .iter()
            .any(|item| item.state == nachalnik::ContextState::Elided),
        "and it was taken"
    );
}

/// A `shell` that runs nothing and writes down what the policy said about its call as it ran.
///
/// note: read here rather than off the policy after the session, because here is where it is
/// read for real: `Shell::invoke` asks this to build the sandbox, and a grant that has gone by
/// then is a command running with the network cut after somebody allowed it.
struct Watchful {
    policy: Arc<Careful>,
    seen: Arc<std::sync::Mutex<Vec<(String, bool)>>>,
}

#[async_trait]
impl Tool for Watchful {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("shell", "runs it").with_capabilities([Capability::exec("run")])
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        self.seen
            .lock()
            .expect("nothing panics holding this")
            .push((
                call.id.0.clone(),
                self.policy.was_granted_the_network(&call.id),
            ));
        Ok(ToolOutput::new("ran it"))
    }
}

/// Allowing a networked command here grants it the network, the way answering `y` does.
///
/// note: the regression this is here for. Everything answering a question means beyond
/// `Kernel::decide` lived in the key handler, and this loop has no keys - so it decided and
/// granted nothing, and a `curl` it had just said it would allow then ran with TCP cut. The
/// failure is invisible from here: the record says allowed, the model gets a connection error, and
/// nothing anywhere names the confinement as the reason.
///
/// note: asserted on the policy rather than on a command's output, because what the sandbox
/// actually does needs a sandbox and this needs to run anywhere. `Sandbox::of` reads exactly this
/// and `tests/sandbox.rs` covers the other half.
#[tokio::test]
async fn a_networked_command_allowed_here_is_granted_the_network() {
    let reaching = ToolCall::new(
        "c1",
        "shell",
        json!({ "call": { "action": "run", "cmd": "curl https://example.com" } }),
    );
    let homely = ToolCall::new(
        "c2",
        "shell",
        json!({ "call": { "action": "run", "cmd": "ls" } }),
    );

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let watching = seen.clone();
    let run = run_with(
        "go\n",
        vec![
            ModelResponse::tool_calls(vec![reaching.clone(), homely.clone()]),
            ModelResponse::text("done"),
        ],
        Grant::Allow,
        move |app| {
            app.kernel.add_tool(Arc::new(Watchful {
                policy: app.policy.clone(),
                seen: watching,
            }));
        },
    )
    .await;

    let seen = seen.lock().expect("nothing panics holding this").clone();
    assert!(
        seen.contains(&(reaching.id.0.clone(), true)),
        "a `curl` was allowed and the sandbox was never told: {} {seen:?}",
        run.prose
    );
    assert!(
        seen.contains(&(homely.id.0.clone(), false)),
        "an `ls` reaches for nothing and should be granted nothing: {seen:?}"
    );

    // and the answers went with the batch they were given for, at the request after it
    assert!(
        !run.app.policy.was_granted_the_network(&reaching.id),
        "a one-off `yes` outlived the call it was about"
    );
}

/// A question the `a` sweep lets through is *answered*, and keeps the network with it.
///
/// note: the other half of the test above, and the half that was missing. `App::decide` answers
/// the question somebody looked at through `App::answer` and then sweeps the ones queued behind
/// it straight into `Kernel::decide` - which is every step of an answer except the one that tells
/// the sandbox. So a `curl` let through by a promise about what happens next ran with TCP cut,
/// while the record said allowed and nothing named the confinement as the reason.
///
/// note: two networked commands rather than an `ls` and a `curl`, and the difference is what
/// makes this reachable at all. `a` on an `ls` remembers `exec:run` and leaves `net:reach` where
/// it was, so the `curl` behind it is still a question and is never swept. What reaches the sweep
/// is a call whose every subject the first answer covered.
#[tokio::test]
async fn a_swept_question_is_granted_the_network_the_way_an_answered_one_is() {
    let looked_at = ToolCall::new(
        "c1",
        "shell",
        json!({ "call": { "action": "run", "cmd": "curl https://example.com" } }),
    );
    let swept = ToolCall::new(
        "c2",
        "shell",
        json!({ "call": { "action": "run", "cmd": "curl https://example.org" } }),
    );

    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(vec![
        ModelResponse::tool_calls(vec![looked_at.clone(), swept.clone()]),
        ModelResponse::text("done"),
    ]);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    app.kernel.add_tool(Arc::new(Watchful {
        policy: app.policy.clone(),
        seen: seen.clone(),
    }));

    app.ask("fetch both");
    app.start_turn();
    let outcome = finished
        .recv()
        .await
        .expect("the turn never stopped to ask");
    while let Ok(event) = events.try_recv() {
        app.on_event(event);
    }
    app.on_outcome(outcome);
    let waiting = app.kernel.pending_permissions();
    assert_eq!(waiting.len(), 2, "two calls did not raise two questions");

    // `a` on the first, which is what covers both: `exec:run` and `net:reach` are what the policy
    // consulted about it, and the second call needs nothing else
    app.decide(waiting[0].id, Grant::Allow, true)
        .expect("the answer was refused");
    assert!(
        app.kernel.pending_permissions().is_empty(),
        "the sweep left the second question waiting"
    );

    let outcome = finished.recv().await.expect("the calls never ran");
    while let Ok(event) = events.try_recv() {
        app.on_event(event);
    }
    app.on_outcome(outcome);

    let seen = seen.lock().expect("nothing panics holding this").clone();
    assert!(
        seen.contains(&(swept.id.0.clone(), true)),
        "a `curl` the sweep allowed ran with the network cut: {seen:?}"
    );
}

/// Every call in a batch keeps the answer it was given, however many of them there are.
///
/// note: the grants were bounded at sixty-four and the oldest went, on the reasoning that one
/// turn could not produce more - an assumption about a model rather than something this program
/// holds to. Every call in a batch is decided before any of them runs, so the sixty-fifth `yes`
/// threw away the first, and that command ran with the network cut after somebody allowed it.
#[tokio::test]
async fn a_batch_of_answers_is_not_forgotten_before_its_calls_run() {
    let calls: Vec<ToolCall> = (0..80)
        .map(|n| {
            ToolCall::new(
                format!("c{n}").as_str(),
                "shell",
                json!({ "call": { "action": "run", "cmd": "curl https://example.com" } }),
            )
        })
        .collect();

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let watching = seen.clone();
    let run = run_with(
        "go\n",
        vec![
            ModelResponse::tool_calls(calls.clone()),
            ModelResponse::text("done"),
        ],
        Grant::Allow,
        move |app| {
            app.kernel.add_tool(Arc::new(Watchful {
                policy: app.policy.clone(),
                seen: watching,
            }));
        },
    )
    .await;

    let seen = seen.lock().expect("nothing panics holding this").clone();
    assert_eq!(seen.len(), calls.len(), "{}", run.prose);
    let cut: Vec<&String> = seen
        .iter()
        .filter(|(_, granted)| !granted)
        .map(|(id, _)| id)
        .collect();
    assert!(
        cut.is_empty(),
        "{} of {} commands ran with the network cut after being allowed: {cut:?}",
        cut.len(),
        calls.len()
    );
}

/// What the advisor has to say reaches the session, rather than a terminal nobody is reading.
///
/// note: the bug this is here for. `Args::advised` drained the queue at startup and printed it
/// with `eprintln!`, on the reasoning that there was no screen yet - but the screen arrives at
/// once and clears it, so the line went where nobody could read it *and* was gone from the queue
/// the session reports from. What was lost was a local advisor's first notice, which is the one
/// saying it is not ready yet.
///
/// note: driven through `App::on_outcome`, which is where the provider's own notice is read and
/// is a door all three loops come through - so what this pins holds for the headless loop and a
/// served session as well as the drawn one. The drawn loop also polls on its tick, which is what
/// gets a starting advisor onto the screen before anybody has sent anything.
#[cfg(feature = "advise")]
#[tokio::test]
async fn the_advisor_says_what_it_has_to_say_to_the_session() {
    // a child that answers nothing: what it reports is the notice it wrote on starting, which
    // is the one that used to be eaten before the session could read it
    let Ok(local) = kamchatka::advisor::Local::new("true") else {
        return;
    };

    let wired = Setup {
        advisor: Some(std::sync::Arc::new(local)),
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(
        "scripted",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");

    let mut app = wired.app;
    let mut finished = wired.finished;
    app.kernel
        .set_provider(Arc::new(ScriptedProvider::new(vec![ModelResponse::text(
            "nothing to do",
        )])));
    app.ask("hello");
    app.start_turn();
    let outcome = finished.recv().await.expect("the turn never ended");
    app.on_outcome(outcome);

    let said: Vec<String> = app.notes(0).map(|note| note.text.to_string()).collect();
    assert!(
        said.iter().any(|line| line.contains("not ready yet")),
        "the advisor's own first line should be in the session: {said:?}"
    );
}

/// The rating reaches the screen through the wiring a real session is built by.
///
/// note: the screen tests build an `App` and hand it an advisor directly, which checks the
/// drawing and takes the wiring on trust. This one goes the other way: `Setup { advisor: .. }`,
/// `wire`, a shell call, and then the question asked for what it would draw. What it is holding
/// to is that the object the kernel decides with and the object the panel reads are the same one.
#[cfg(feature = "shell-advisor")]
#[tokio::test]
async fn a_wired_session_draws_the_rating_the_kernel_asked_for() {
    use kamchatka::tools::Rating;
    use nachalnik::ContextItem;
    use nachalnik_providers::system1::Jev;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    let body = "{\"model\":\"jev-1\",\"answers\":{\"rating\":{\"type\":\"score\",\
                \"score\":1.9,\"confidence\":0.93,\"legend\":{},\"probabilities\":{}}}}";
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut discard = [0u8; 8192];
            let _ = socket.read(&mut discard).await;
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = socket.shutdown().await;
        }
    });

    let wired = Setup {
        tools: Some(vec!["shell".to_owned()]),
        compact: None,
        confine: false,
        advisor: Some(Arc::new(Jev::new(
            "jev-latest",
            format!("http://{address}"),
            "k",
        ))),
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(
        "scripted",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");

    let app = wired.app;
    app.kernel.set_provider(Arc::new(ScriptedProvider::new(vec![
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "action": "run", "cmd": "rm -rf ~/work" }),
        )]),
        ModelResponse::text("asked"),
    ])));
    app.kernel.push(ContextItem::user("tidy up"));

    // the turn stops at the question, which is the moment the rating has to be there
    let _ = app.kernel.step().await;
    let request = app
        .kernel
        .pending_permissions()
        .first()
        .cloned()
        .expect("the shell call is a question");

    let rated = app
        .rating(&request)
        .expect("the wiring gave the panel the advisor the kernel decided with");
    assert_eq!(rated.shown(), Rating::Grave);
}

/// A question answered before the turn that raised it has finished unwinding still carries on.
///
/// note: the window, driven rather than raced. `permission.requested` is broadcast while the turn
/// that asked is still in flight, so `App::busy` is true from the moment the question exists until
/// the loop takes the outcome - and an answer inside that window is recorded, finds `start_turn`
/// refusing because the old turn is still marked as running, and is followed by an outcome that
/// says `Deciding` with nothing left to decide. The session then has every question answered, no
/// turn running, and nothing that will ever start one.
///
/// note: this drives `App` by hand rather than through a loop precisely so that there is no race
/// in it. The three loops differ only in *when* they answer; `headless.rs` stays out of the window
/// by construction, and neither the keys nor a socket can, because a person answers when they
/// answer. So the fix is in `on_outcome`, where all three come through, and so is this.
#[tokio::test]
async fn a_question_answered_inside_the_window_still_carries_the_turn_on() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("carried on"),
    ];
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(script);
    app.kernel.add_tool(Arc::new(
        ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
    ));

    app.ask("go");
    app.start_turn();
    let outcome = finished.recv().await.expect("the turn never ended");
    // the turn has ended and this loop has not been told, which is exactly the window: `busy` is
    // still true, and a client's answer arriving now is the case under test
    assert!(app.busy, "the window is not where this test thinks it is");
    let question = app.asked().expect("the turn stopped without asking");
    app.decide(question.id, Grant::Allow, false)
        .expect("the answer was refused");
    app.on_outcome(outcome);

    // and the turn goes on, which is the whole claim
    assert!(app.busy, "the session stopped with nothing left to answer");
    let outcome = finished.recv().await.expect("the turn never carried on");
    while let Ok(event) = events.try_recv() {
        app.on_event(event);
    }
    app.on_outcome(outcome);
    assert!(
        app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("carried on")),
        "the model was never asked again"
    );
}

/// `/cleanup` down a pipe says nothing, and does not swallow what is said after it.
///
/// note: two claims and only the second is a bug. A pipe cannot unprint what it has already
/// written, so `/cleanup` here is nearly a no-op for whoever is reading - which is right, and is
/// why the first assertion is that it is silent rather than that anything vanished. What it is
/// not a no-op for is the *watermark*: this loop reads the program's lines through `App::notes`,
/// which counts the filtered sequence, and a clear empties that sequence rather than shortening
/// it. A loop that did not notice would hold a mark of three against a sequence of nothing and
/// print none of the next three lines, for the rest of the run.
///
/// note: the lines around the clear are commands that answer with a *line* rather than a page,
/// because a page is printed from `Reply` and would go out whatever the watermark said - so a
/// test built on those would pass with the bug in place. And it counts the same refusal twice
/// rather than looking for a distinctive one, because a refusal that quoted its argument would be
/// a different line each time and would not notice a mark that is merely too high by one.
#[tokio::test]
async fn a_cleanup_down_a_pipe_is_silent_and_keeps_saying_things_afterwards() {
    let refused = "is not a number of tokens";
    let run = run(
        "/limit nonsense\n/spend nonsense\n/cleanup\n/spend nonsense\n",
        vec![],
        |_| {},
    )
    .await;

    assert!(
        !run.prose.to_lowercase().contains("cleanup"),
        "a clear announced itself, which is the one thing it must not do: {}",
        run.prose
    );
    assert_eq!(
        run.prose.matches(refused).count(),
        2,
        "the line after a cleanup was swallowed: {}",
        run.prose
    );
}

/// And the same act through the key and through the command reaches the same place.
///
/// note: `/cleanup` is `ctrl+l`, and the reason to pin it is that they are two doors onto one
/// function rather than two implementations. `tests/screen/chat.rs` presses the key; this sends
/// the line, in a build with no keys at all to press.
#[tokio::test]
async fn cleanup_is_a_command_as_well_as_a_key() {
    let Wired { mut app, .. } = wired(vec![]);
    app.say(Speaker::Note, "something the program said");
    app.kernel
        .push(ContextItem::user("something a person said"));

    let before = app.cleared();
    app.submit("/cleanup").await;

    assert_eq!(app.cleared(), before + 1, "the generation did not move");
    assert!(
        app.notes(0).next().is_none(),
        "the program's own lines are still there: {:?}",
        app.loose
    );
    assert!(
        app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("something a person said")),
        "the conversation is the context and was not this command's to take"
    );
}

/// `/clear` is answered with where the two things it could mean actually live.
///
/// note: the name this command had, and the one word somebody arriving from any other agent will
/// type. There it means the conversation; here the thing with that shape is `/exclude all` and the
/// thing with that name was `/cleanup`, so a line saying only that there is no `/clear` would
/// leave somebody looking for both of them. It is the same argument `/load` makes about not being
/// called `/resume`: two things a keystroke apart that differ in what happens to the context you
/// already have is a trap.
#[tokio::test]
async fn clear_says_which_of_the_two_things_it_could_mean_is_where() {
    let run = run("/clear\n", vec![], |_| {}).await;

    assert!(run.prose.contains("`/cleanup`"), "{}", run.prose);
    assert!(run.prose.contains("`/exclude all`"), "{}", run.prose);
    // and nothing happened to either of them, which is what an unknown command owes
    assert_eq!(run.app.cleared(), 0, "it cleared the notices anyway");
}

/// A session started without a model asks nobody anything, and `/model` is what ends that.
///
/// note: through `Setup::wire` with a provider that names no model, because that is what `main`
/// hands over for a run started without `-m` - and what `wire` does with it is the whole of the
/// mechanism. The kernel is given no provider at all, so "nothing was sent" is a fact about the
/// runtime rather than a check in the client; what the prose has to add is whose move it is.
///
/// note: `/model` twice, either side of the switch. The second one can only name the model if the
/// kernel was handed the provider at the switch, which is the half of this a screen would not
/// show.
#[tokio::test]
async fn a_session_with_no_model_sends_nothing_until_one_is_picked() {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = Setup {
        tools: Some(Vec::new()),
        compact: None,
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(
        "",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");
    assert!(
        app.kernel.model_info().is_none(),
        "the kernel was handed a provider with nothing to ask"
    );

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(
            &mut app,
            &mut events,
            &mut finished,
            "hello\n/model\n/model a-model\n/model\n".as_bytes(),
        )
        .await
        .expect("the run failed");
    let run = Run {
        app,
        records: String::from_utf8(records).expect("the records are text"),
        prose: String::from_utf8(prose).expect("the prose is text"),
    };

    assert!(
        !run.names().contains(&"model.requested".to_owned()),
        "a request went out with nothing to send it to: {:?}",
        run.names()
    );
    assert!(
        run.prose.contains("nothing is sent until there is a model"),
        "{}",
        run.prose
    );
    // the line is kept rather than refused: the model picked a moment later is asked it
    assert_eq!(run.app.kernel.items()[0].content.to_text(), "hello");

    assert!(run.prose.contains("no model yet"), "{}", run.prose);
    assert!(
        run.prose.contains("a-model at http://127.0.0.1:1"),
        "{}",
        run.prose
    );
}

/// `/restart` writes the session out and carries on in a new one, in the program proper.
///
/// note: the binary rather than a driver, because the half worth testing is the loop around
/// `Setup::relaunch`: reading the flag, putting the new session where the loop was holding the
/// first, and one reader for the whole run. `App::restart` is a `bool` and the test above is all
/// there is to say about it here.
///
/// note: `TMPDIR` is the whole isolation. `record` writes under the temporary directory, so a run
/// pointed at one of its own leaves exactly the files this counts and nothing else's turn up in it.
///
/// note: and `#[cfg(unix)]` is that sentence's other half, alongside `common::program` being
/// gated the same way. Windows reads `TMP` and `TEMP` and not `TMPDIR`, so the child would record
/// into the real temporary directory and this would count somebody else's sessions - or none.
#[cfg(unix)]
#[test]
fn restart_writes_the_session_out_and_starts_another() {
    let dir = std::env::temp_dir().join(format!("kamchatka-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory to record into");

    let mut child = std::process::Command::new(common::program())
        // note: no `-m`, so a message is put in the context and nothing is sent. What this is
        // about is which session a line lands in, and a turn against an endpoint that is not there
        // would be the run failing about something else
        .args(["--headless"])
        .env("TMPDIR", &dir)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    use std::io::Write as _;
    let mut stdin = child.stdin.take().expect("stdin is a pipe");
    // note: a message either side of the restart, because two of the three things this is about
    // are what happens to them. The first belongs to a session that is over and must not be in the
    // second; the second is a line *after* the restart and must be read at all - a session built
    // around a second `BufReader` drops whatever the first had read ahead into its buffer, which
    // is this command's own shape of the bug and is silent
    stdin
        .write_all(b"before the restart\n/restart\nafter the restart\n/quit\n")
        .expect("the lines go in");
    drop(stdin);

    let out = child.wait_with_output().expect("it ran");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(out.status.success(), "{said}");
    // the old session named itself, said where it went, and said it into the new session rather
    // than onto the terminal on its own
    assert!(
        said.contains("ended:") && said.contains("records in"),
        "the restart did not report the session it wrote out: {said}"
    );

    // two sessions, two records: the one `/restart` wrote and the one `/quit` did
    let logs: Vec<_> = std::fs::read_dir(dir.join("kamchatka"))
        .expect("the record directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".jsonl"))
        .collect();
    assert_eq!(logs.len(), 2, "one record per session: {logs:?}");

    // note: the session that was restarted, because it is the one with two ways to be ended. This
    // loop ends its own session - the last record owes the stream it is writing - and
    // `Setup::relaunch` ends the one it is handed, so the pair of them wrote `session.finished`
    // twice: the log said nothing more would be recorded and then recorded it again
    for log in &logs {
        let lines =
            std::fs::read_to_string(dir.join("kamchatka").join(log)).expect("it is readable");
        let ended = lines
            .lines()
            .filter(|line| line.contains("session.finished"))
            .count();
        assert_eq!(ended, 1, "{log} ended {ended} time(s)");
    }

    // and the second session is a session of its own rather than the first one's log written twice
    let mut names: Vec<String> = logs.iter().map(|it| it.replace(".jsonl", "")).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 2, "both records have the same name: {logs:?}");

    // the line after the restart was read, and it went into the session that came after it
    let mut snapshots: Vec<(std::time::SystemTime, String)> = names
        .iter()
        .map(|name| {
            let at = dir.join("kamchatka").join(format!("{name}.json"));
            let when = std::fs::metadata(&at)
                .and_then(|it| it.modified())
                .expect("a snapshot beside every log");

            (when, std::fs::read_to_string(&at).expect("it is readable"))
        })
        .collect();
    snapshots.sort_by_key(|(when, _)| *when);
    let (first, second) = (&snapshots[0].1, &snapshots[1].1);

    assert!(
        first.contains("before the restart"),
        "the session that was restarted did not keep what was said in it"
    );
    assert!(
        second.contains("after the restart"),
        "the line after /restart was swallowed with the old reader's buffer"
    );
    assert!(
        !second.contains("before the restart"),
        "the fresh session carried the old one's conversation into it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
