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
    tools::Subject,
    wiring::{Setup, Wired},
};
use nachalnik::{
    BoxError, Capability, DeltaSink, Grant, ModelInfo, ModelRequest, ModelResponse, OutputSink,
    Provider, Record, Tool, ToolCall, ToolOutput, ToolSpec, Usage, Verdict, async_trait,
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
/// its existing: what these tests want is what `main.rs` wants, minus the four tools and the
/// child process it takes to find out what Landlock would allow. The model is swapped in
/// afterwards because the runtime lets a seam be swapped while a session is running, and a
/// scripted provider is not an `Endpoint`.
fn wired(script: Vec<ModelResponse>) -> Wired {
    capped(script, None)
}

/// The same, with a ceiling on what the provider may charge before the session stops.
fn capped(script: Vec<ModelResponse>, spend: Option<u64>) -> Wired {
    let wired = Setup {
        builtin_tools: false,
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
            ConstTool::new("peek", "the answer").with_capabilities([Capability::Read]),
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

/// `--on-ask allow` is the other answer, and the tool runs.
#[tokio::test]
async fn the_other_answer_lets_it_run() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("read it"),
    ];
    let run = run_with("look around\n", script, Grant::Allow, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::Read]),
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
            ConstTool::new("peek", "the answer").with_capabilities([Capability::Read]),
        ));
        app.policy
            .set(&Subject::Capability(Capability::Read), Verdict::Allow);
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
            ConstTool::new("peek", "the answer").with_capabilities([Capability::Read]),
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
                ConstTool::new("peek", "the answer").with_capabilities([Capability::Read]),
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
    let reply = app.submit("/tools drop nothing").await;
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
            ConstTool::new("peek", "the answer").with_capabilities([Capability::Read]),
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

    let mut program = std::env::current_exe().expect("a test binary has a path");
    program.pop();
    if program.ends_with("deps") {
        program.pop();
    }
    program.push("kamchatka");

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

// ------------------------------------------------------------------- the program, and a socket

/// An endpoint the program can be pointed at, which answers the model listing and then hands out
/// these bodies, one per request, as a stream.
///
/// note: a socket rather than a scripted provider, because what is under test here is the
/// *program*: it builds its own provider out of two environment variables, in a process of its
/// own, and nothing this test holds can be swapped into that. A listener is the only seam a child
/// process has.
///
/// note: the bodies are SSE because the provider asks for a stream unless told not to, and the
/// point of these tests is the path the program actually takes. `[DONE]` is appended here so that
/// a case reads as what the model said rather than as protocol.
#[cfg(unix)]
async fn endpoint(answers: Vec<String>) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    let answers = Arc::new(answers);
    let nth = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (answers, nth) = (answers.clone(), nth.clone());
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

                let mut buf = vec![0u8; 65536];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..read]).into_owned();

                let (kind, body) = match head.contains("/models") {
                    true => (
                        "application/json",
                        r#"{"data":[{"id":"nothing","context_length":128000}]}"#.to_owned(),
                    ),
                    false => match answers.get(nth.fetch_add(1, SeqCst)) {
                        Some(sse) => ("text/event-stream", format!("{sse}\n\ndata: [DONE]\n\n")),
                        // a request nobody wrote an answer for is held open rather than refused,
                        // which is a model that has gone quiet - and the one thing a test must not
                        // do here is end the turn by accident
                        None => return std::future::pending().await,
                    },
                };
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });

    format!("http://{at}/v1")
}

/// The binary under test.
#[cfg(unix)]
fn program() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("a test binary has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }

    path.join("kamchatka")
}

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
    let base = endpoint(vec![format!(
        "data: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
            {"index": 0, "id": "c1", "type": "function",
             "function": {"name": "shell", "arguments": "{\"cmd\": \"sleep 30\"}"}}
        ]}, "finish_reason": "tool_calls"}]})
    )])
    .await;

    let mut child = std::process::Command::new(program())
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
    let base = endpoint(vec![format!(
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
    let mut child = std::process::Command::new(program())
        .args(["--headless", "--no-record", "-m", "nothing"])
        .args(["--mcp", &server, "--allow", "mcp:py", "go"])
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
    let base = endpoint(vec![answer("as much as it likes")]).await;

    let out = std::process::Command::new(program())
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
    let base = endpoint(vec![answer("something to keep")]).await;
    let dir = common::scratch("recorded");

    let out = std::process::Command::new(program())
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
                program().display()
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
    let mut child = std::process::Command::new(program())
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

/// A run with no keys to press is handed every page of the key reference rather than the one the
/// panel would have opened at. On a screen `/help` is six pages and `←` turns them; down a pipe
/// there is no key to press, so a caller given one of six would be reading a reference whose
/// other five it has no way to ask for.
#[tokio::test]
async fn help_down_a_pipe_is_the_whole_of_it() {
    let run = run("/help\n", vec![], |_| {}).await;

    for section in kamchatka::help::SECTIONS {
        // the one that is not offered is the one whose keys do not exist: nothing is waiting on a
        // permission here, so there is nothing for `y` or `a` to answer
        let offered = section.applies(false);
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

    // and each page is named, because six of them run together with nothing between is a wall
    assert!(run.prose.contains("-- commands --"), "{}", run.prose);
}
