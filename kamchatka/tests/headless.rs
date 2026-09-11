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
    BoxError, Capability, Grant, ModelResponse, OutputSink, Record, Tool, ToolCall, ToolOutput,
    ToolSpec, Usage, Verdict, async_trait,
    test::{ConstTool, ScriptedProvider, call},
};
use nachalnik_providers::OpenAiCompatible;
use serde_json::json;
use tokio::io::BufReader;

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
    let wired = Setup {
        builtin_tools: false,
        compact: None,
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
    } = wired(script);
    setup(&app);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    let mut driver = Headless::new(on_ask, &mut records, &mut prose);
    if let Some(tokens) = spend {
        driver = driver.spend(tokens);
    }
    driver
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
