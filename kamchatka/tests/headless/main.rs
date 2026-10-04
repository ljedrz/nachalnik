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
//! terminal, no socket and no files. `program.rs` is the exception: what only the binary does, the
//! arguments, the signals and the records it leaves, is tested by spawning it. `spend.rs` is the
//! ceiling, which is one mechanism with enough cases to be read on its own.

use std::sync::Arc;

use kamchatka::{
    app::{App, Did, Outcome, Overlay, Speaker},
    headless::Headless,
    tools::{Careful, Subject},
    wiring::{Setup, Wired},
};
use nachalnik::{
    BoxError, Capability, ContextItem, ContextKind, DeltaSink, Grant, ModelInfo, ModelRequest,
    ModelResponse, OutputSink, Provider, Record, StopReason, Tool, ToolCall, ToolOutput, ToolSpec,
    Usage, Verdict, async_trait,
    test::{ConstTool, ScriptedProvider, call},
};
use nachalnik_providers::OpenAiCompatible;
use serde_json::json;
use tokio::io::BufReader;

// the path is the price of the directory, as in `tests/remote/main.rs`: `common` is shared with
// every other suite and stays where all of them can reach it
#[path = "../common/mod.rs"]
mod common;

mod program;
mod spend;

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
        ModelInfo::new("trickle", "trickle").with_context_limit(128_000)
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

/// A provider's retry is said while it waits, above the answer it held up.
///
/// note: nothing the kernel emits carries the notice, so a loop that read it only when the turn
/// ended printed `trying again` under an answer somebody had already read.
#[tokio::test]
async fn a_retry_is_said_before_the_answer_it_held_up() {
    let base = common::endpoint(vec![
        common::BUSY.to_owned(),
        common::answer("held up, then answered"),
    ])
    .await;

    let Wired {
        mut app,
        mut events,
        mut finished,
    } = Setup {
        tools: Some(Vec::new()),
        compact: None,
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new("nothing", base, "")))
    .expect("the wiring failed");
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, "ask\n".as_bytes())
        .await
        .expect("the run failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    let retried = prose
        .find("trying again")
        .expect("the retry was never said");
    let answered = prose.find("held up, then answered").expect("no answer");
    assert!(
        retried < answered,
        "the retry was said after the answer:\n{prose}"
    );
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

/// A run with no screen still says what `/budget` is about, and counts no draft.
///
/// note: `App::drafted` is a figure rather than a `#[cfg]` away in a build with no prompt: `/budget`
/// reports it and the status line adds it in, so the empty answer is what both of them print.
/// A run with nothing half-typed saying that a message is waiting to be sent is a figure for a
/// message nobody can type.
#[tokio::test]
async fn a_run_with_no_screen_counts_no_draft() {
    let run = run("/budget\n", Vec::new(), |_| {}).await;

    assert!(
        run.prose.contains("the next request:"),
        "`/budget` said nothing about the request: {}",
        run.prose
    );
    assert!(
        !run.prose.contains("typed but not sent"),
        "a run with no prompt has nothing half-typed: {}",
        run.prose
    );
    assert_eq!(run.app.drafted(), 0, "and nothing is counted for one");
}

/// A name no command answers to is cut before it is quoted back, as every other line is.
///
/// note: the dispatch splits on the first space, so a name given with arguments was short by
/// accident and a bare one was not: a hundred thousand characters typed after the slash came back
/// as a hundred thousand characters of refusal, on the stream a script is reading. `one_line` is
/// what the neighbouring notices use and what cuts it now.
#[tokio::test]
async fn a_name_no_command_answers_to_is_cut_before_it_is_quoted_back() {
    let typed = format!("/{}", "a".repeat(100_000));
    let run = run(&format!("{typed}\n"), Vec::new(), |_| {}).await;

    assert!(run.prose.contains("there is no `/aaa"), "{}", run.prose);
    assert!(
        run.prose.contains('…'),
        "the whole name was written back: {}",
        run.prose
    );
    // and it is a line like any other, not a hundred thousand bytes of one
    let refusal = run
        .prose
        .lines()
        .find(|line| line.contains("there is no"))
        .expect("the name was refused");
    assert!(
        refusal.chars().count() < 200,
        "the refusal is {} characters: {refusal}",
        refusal.chars().count()
    );
    // and nothing was sent to the model to find out what it meant
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
/// note: the whole of the failure this is about happened in one session and cost twenty calls. No
/// operation could be read out of the arguments, so the call declared every operation `fs` has -
/// and `--allow fs:read` then matched nothing. What the run said was `deny, because nobody is here
/// to be asked`, which is true and is about the wrong thing: the model went looking for a
/// different approach, and the person watching had no way to see that their rule and the call
/// could never meet.
///
/// note: the call here leaves `action` out, where the session that bought this wrote the whole
/// wrapper as a string of JSON. That shape is read through now - see `ops::inner` - and the
/// widening it caused is reached by any call whose operation cannot be read, so the test asks for
/// the one that is still a call nobody can place.
#[tokio::test]
async fn a_call_that_names_no_operation_says_why_the_rule_missed_it() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "fs", json!({ "call": { "path": "x" } }))]),
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

/// A call a standing rule refused because it named no operation is told that, not only the rule.
///
/// note: the refusal reached the model as `refused by \`fs:write\` and \`fs:edit\`. That is a
/// standing rule`, for a call whose operation could not be read, and a model told that retries the
/// same call: the rule is the wrong thing to blame, and the arguments are the thing to fix. The real
/// `fs`, wired, because what is being checked is that the policy knows what the session's own
/// tools declare.
#[tokio::test]
async fn a_rule_that_refused_a_call_naming_no_operation_says_the_call_named_none() {
    let wired = Setup {
        tools: Some(vec!["fs".to_owned()]),
        compact: None,
        deny: vec![Subject::Capability(Capability::fs("write"))],
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(
        "scripted",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired;
    // a path and no operation, so nothing here can tell which of five the call meant
    app.kernel.set_provider(Arc::new(ScriptedProvider::new(vec![
        ModelResponse::tool_calls(vec![call(
            "c1",
            "fs",
            json!({ "call": { "path": "Cargo.toml" } }),
        )]),
        ModelResponse::text("told why"),
    ])));

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, "read it\n".as_bytes())
        .await
        .expect("the run failed");

    let told: Vec<String> = app
        .kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("names no operation"), "{}", told[0]);
    assert!(
        told[0].contains("holds `path` and no `action`"),
        "and says what the call held instead: {}",
        told[0]
    );
}

/// A call a level too deep is read as the call, by the rule and by the tool alike.
///
/// note: two sweeps with one model wrote nearly every call as `{"call": {"item": {...}}}`, were
/// judged against everything each tool does, and spent their sessions refused. Read through, the
/// call is the `read` it meant, and `--allow fs:read` answers it.
#[tokio::test]
async fn a_call_one_level_too_deep_is_allowed_by_the_rule_for_what_it_asks() {
    let wired = Setup {
        tools: Some(vec!["fs".to_owned()]),
        compact: None,
        allow: vec![Subject::Capability(Capability::fs("read"))],
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(
        "scripted",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired;
    app.kernel.set_provider(Arc::new(ScriptedProvider::new(vec![
        ModelResponse::tool_calls(vec![call(
            "c1",
            "fs",
            json!({ "call": { "item": { "action": "read", "path": "Cargo.toml" } } }),
        )]),
        ModelResponse::text("read it"),
    ])));

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, "read it\n".as_bytes())
        .await
        .expect("the run failed");

    let told: Vec<String> = app
        .kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("[package]"), "the file, read: {}", told[0]);
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

/// A blank line is nothing, the way enter on an empty prompt is, and spaces round a line do not
/// make a command into a message.
#[tokio::test]
async fn a_blank_line_is_not_a_message() {
    let run = run(
        "\n   \n  /budget\nfirst\n",
        vec![ModelResponse::text("one")],
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
    assert_eq!(asked, vec!["first".to_owned()], "a blank line was sent");
    assert_eq!(
        run.names()
            .iter()
            .filter(|name| *name == "model.requested")
            .count(),
        1
    );
}

/// A line that is not UTF-8 is read and said to be, rather than ending the session.
///
/// note: the input was read as a stream of text, and a stream that was not text at one line was an
/// error for the whole run: the session ended there, every later line went unread, and nothing said
/// which line it had choked on. `\r\n` is here because the reader that replaced it is written by
/// hand, and `lines` took both endings off.
#[tokio::test]
async fn a_line_that_is_not_utf8_is_read_and_named() {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(vec![ModelResponse::text("read it")]);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(
            &mut app,
            &mut events,
            &mut finished,
            &b"/note caf\xe9\r\nand after it\n"[..],
        )
        .await
        .expect("the run failed");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        prose.contains("not UTF-8") && prose.contains("/note caf\u{FFFD}"),
        "{prose}"
    );
    assert!(
        prose.contains("read it"),
        "the next line went unread: {prose}"
    );
    let said: Vec<String> = app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert!(said.contains(&"caf\u{FFFD}".to_owned()), "{said:?}");
    assert!(said.contains(&"and after it".to_owned()), "{said:?}");
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

/// `/stop` in `ready` drops the calls decided and not run, and the model is told.
///
/// note: found live: `/stop` there said nothing was running, and the calls ran at the next line
/// whatever had been done first - the turn that asked for them excluded, or their tool taken out of
/// the registry.
#[tokio::test]
async fn stop_in_ready_drops_the_calls_waiting_to_run() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("told"),
    ];
    let run = run_with(
        "/step look around\n/load\n/stop\n/continue\n",
        script,
        Grant::Allow,
        |app| {
            app.kernel.add_tool(Arc::new(
                ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
            ));
        },
    )
    .await;

    // what `/load` says there is about calls waiting to run, not about a question
    assert!(run.prose.contains("`/stop` drops them"), "{}", run.prose);
    assert!(
        run.prose.contains("1 call(s) dropped without running"),
        "{}",
        run.prose
    );
    assert!(!run.names().contains(&"tool.started".to_owned()));
    assert!(run.app.kernel.items().iter().any(|item| {
        matches!(item.kind, ContextKind::ToolResult { .. })
            && item.content.to_text().contains("cancelled")
    }));
    assert!(run.prose.contains("told"), "{}", run.prose);
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

/// The user messages in a session's context, in order.
fn said(app: &App) -> Vec<String> {
    app.kernel
        .items()
        .iter()
        .filter(|item| item.label == "user")
        .map(|item| item.content.to_text().into_owned())
        .collect()
}

/// A line queued into a turn that fails goes in when it fails, and is not overtaken by the next.
#[tokio::test]
async fn a_line_queued_into_a_failed_turn_goes_in_before_the_next_one() {
    // nothing scripted, so every request fails
    let Wired {
        mut app,
        mut finished,
        ..
    } = wired(Vec::new());

    app.submit("first").await;
    assert_eq!(app.submit("second").await.did, Did::Queued);
    let outcome = finished.recv().await.expect("the turn reported nothing");
    assert!(
        matches!(outcome, Outcome::Failed(_)),
        "the turn did not fail"
    );
    app.on_outcome(outcome);
    assert!(!app.busy, "a failure started a turn for the waiting line");

    app.submit("third").await;
    assert_eq!(said(&app), ["first", "second", "third"]);
}

/// And the same after a step, which rests without a turn to wait for.
#[tokio::test]
async fn a_line_queued_into_a_step_goes_in_before_the_next_one() {
    let Wired {
        mut app,
        mut finished,
        ..
    } = wired(vec![ModelResponse::text("one")]);

    app.submit("/step first").await;
    assert_eq!(app.submit("second").await.did, Did::Queued);
    let outcome = finished.recv().await.expect("the step reported nothing");
    assert!(matches!(outcome, Outcome::Stepped(_)), "it was not a step");
    app.on_outcome(outcome);
    assert!(!app.busy, "a step started a turn for the waiting line");

    app.submit("third").await;
    assert_eq!(said(&app), ["first", "second", "third"]);
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

/// A deadline reached while a command waits on an endpoint stops the wait, and the run.
///
/// note: `/models` was awaited inside the branch that read its line, so neither the deadline nor
/// `ctrl+c` could be reached until the endpoint answered - and one that had gone quiet held a
/// `--deadline` run for as long as the provider's own patience. The listing is sent out now, and
/// the deadline stops it as it stops a turn. A switch would be waited for instead: half an
/// `/endpoint` is worse than a late exit.
#[tokio::test]
async fn a_deadline_stops_a_command_waiting_on_an_endpoint() {
    use tokio::io::AsyncWriteExt as _;

    // takes every connection and says nothing, which is an endpoint that has gone quiet
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
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
        "quiet",
        format!("http://{at}/v1"),
        "",
    )))
    .expect("the wiring failed");
    // held open, so that only the deadline can end the run
    let (mut typing, reader) = tokio::io::duplex(64);
    typing
        .write_all(b"/models\n")
        .await
        .expect("could not type");

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        Headless::new(Grant::Deny, &mut records, &mut prose)
            .deadline(std::time::Duration::from_millis(200))
            .run(&mut app, &mut events, &mut finished, BufReader::new(reader))
            .await
            .expect("the run failed")
    })
    .await
    .expect("the deadline waited for the endpoint");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("out of time"), "{prose}");
    assert!(
        prose.contains("stopped waiting for the list of models"),
        "{prose}"
    );
    drop(typing);
}

/// A deadline too far off to be an instant is no deadline, rather than a panic.
///
/// note: `--deadline 18446744073709551615` added the seconds to the clock with `+`, which panics
/// on overflow - so the run died with exit 101 before reading a line, and wrote no record.
#[tokio::test]
async fn a_deadline_past_the_end_of_time_is_none() {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(vec![ModelResponse::text("in time")]);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .deadline(std::time::Duration::from_secs(u64::MAX))
        .run(&mut app, &mut events, &mut finished, &b"go\n"[..])
        .await
        .expect("the run failed");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("in time"), "{prose}");
    assert!(!prose.contains("out of time"), "{prose}");
}

/// A session refused for a request longer than the model takes passes the next messages over
/// until something makes room, and then sends again.
///
/// note: a message refused there still went into the context, where it made the request it could
/// not get into longer, and every one a script sent after it went out together once there was
/// room. The ceiling above passes messages over for the same reason.
#[tokio::test]
async fn a_message_that_cannot_fit_is_passed_over_until_there_is_room() {
    let run = run(
        "first\nsecond\nthird\n/exclude file:big.rs\nfourth\n",
        vec![],
        |app| {
            app.kernel.set_provider(Arc::new(
                ScriptedProvider::new(vec![ModelResponse::text("there is room now")])
                    .with_info(ModelInfo::new("scripted", "scripted").with_context_limit(400)),
            ));
            app.kernel
                .push(ContextItem::file("big.rs", "x".repeat(4_000)).pinned());
        },
    )
    .await;

    let asked: Vec<String> = run
        .app
        .kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, ContextKind::UserMessage))
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(
        asked,
        ["first", "fourth"],
        "only the refused one and the one after the room was made"
    );
    assert_eq!(
        run.prose.matches("passed over unsent").count(),
        1,
        "{}",
        run.prose
    );
    assert!(run.prose.contains("there is room now"), "{}", run.prose);
}

/// Prose that fails part-way through a turn still ends the session after the turn, not in it.
///
/// note: `… | head` is this: the reader goes, the next line written fails, and the loop left with
/// the error while the turn was still running - no `session.finished` and a tool still going, for
/// whoever wrote the record afterwards. `Breaks` fails every write after the first tool call.
#[tokio::test]
async fn a_run_whose_prose_breaks_mid_turn_still_ends_after_the_turn() {
    struct Breaks(bool);
    impl std::io::Write for Breaks {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.0 = String::from_utf8_lossy(buf).contains("wait");
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
        ModelResponse::text("never reached"),
    ];
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = capped(script, None);
    app.kernel.add_tool(Arc::new(Slow));

    let (mut records, mut prose) = (Vec::new(), Breaks(false));
    let ran = Headless::new(Grant::Allow, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, &b"go\n"[..])
        .await;

    assert!(ran.is_err(), "the broken prose is still the answer");
    assert!(!app.busy, "the turn was left running");
    let names: Vec<_> = app
        .kernel
        .history()
        .into_iter()
        .map(|record| record.event.name().to_owned())
        .collect();
    assert_eq!(
        names.last().map(String::as_str),
        Some("session.finished"),
        "{names:?}"
    );
    assert!(names.contains(&"tool.finished".to_owned()), "{names:?}");
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

/// A record kept as the session goes holds every record the kernel does, and a snapshot that
/// names the last of them.
///
/// note: the same `Recorder` the program starts, on a session with no screen and no socket, so
/// that what it writes is checked against the kernel's own log rather than against what a
/// process left behind. The file is read the way anything else would read it, a record per line.
#[tokio::test]
async fn a_record_kept_as_it_goes_holds_the_whole_log() {
    let dir = common::scratch("kept");
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
        ModelResponse::text("done"),
    ];
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = capped(script, None);
    app.kernel.add_tool(Arc::new(Slow));
    let recorder = kamchatka::wiring::Recorder::start_under(&app, &dir).expect("a record");
    let (log, state) = (recorder.log().to_owned(), recorder.state().to_owned());
    app.recorder = Some(recorder);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Allow, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, &b"go\n"[..])
        .await
        .expect("the run failed");
    let written = kamchatka::wiring::record(&app).expect("finished");
    assert_eq!(
        (written.log.as_str(), written.state.as_str()),
        (log.as_str(), state.as_str())
    );

    let kept: Vec<Record> = std::fs::read_to_string(&log)
        .expect("readable")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line is a record"))
        .collect();
    let history = app.kernel.history();
    assert_eq!(kept.len(), history.len(), "{kept:?}");
    assert!(
        kept.iter()
            .zip(&history)
            .all(|(kept, held)| kept.seq == held.seq && kept.event == held.event),
        "the file is the log, in order"
    );
    assert_eq!(written.records, kept.len());
    assert_eq!(
        kept.last().map(|record| record.event.name()),
        Some("session.finished")
    );
    let snapshot: nachalnik::Snapshot =
        serde_json::from_slice(&std::fs::read(&state).expect("readable")).expect("a session");
    assert_eq!(
        snapshot.last_seq,
        kept.last().map(|r| r.seq).unwrap_or_default()
    );

    // note: both halves are the whole conversation, so both are their owner's alone. The
    // snapshot was, and the log beside it came out `0644` under an ordinary umask - kept private
    // only by the directory it happens to be in
    for path in [&log, &state] {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{path} is {mode:o}");
    }
}

/// A session at rest writes its snapshot again when something was logged since, and not otherwise.
///
/// note: the snapshot is replaced by a rename, so a file that was written again is a different
/// file. What this stands for is a person's one command announcing many items - each
/// announcement asks for a snapshot, and every one after the first would be the same file
/// rendered, synced and renamed again.
#[tokio::test]
async fn a_snapshot_is_written_again_only_when_the_log_has_moved() {
    use std::os::unix::fs::MetadataExt as _;

    let dir = common::scratch("resting");
    let Wired { app, .. } = capped(vec![], None);
    let recorder = kamchatka::wiring::Recorder::start_under(&app, &dir).expect("a record");
    let file = || std::fs::metadata(recorder.state()).expect("written").ino();
    let first = file();

    recorder.checkpoint(&app.kernel).expect("kept");
    assert_eq!(file(), first, "nothing was logged, so nothing was written");

    app.kernel.push(ContextItem::user("a"));
    recorder.checkpoint(&app.kernel).expect("kept");
    assert_ne!(
        file(),
        first,
        "an item was added, and the snapshot shows it"
    );
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

/// And what a command answers names the command for what a key would do, rather than the key.
///
/// note: `/limit`, `/note`, `/copy`, `/request` and a bare `/exclude` each ended on a key of the
/// context tab - the way back to an excluded copy, a pin, a row to copy, an item to put back, a
/// change to undo - down a pipe that has neither the tab nor the key. `/restore`, `/pin` and
/// `/undo` are the same acts.
#[tokio::test]
async fn a_command_with_no_keys_names_no_key() {
    let run = run(
        "hello\n/limit\n/note\n/copy x\n/exclude\n/exclude all\n/request\n",
        vec![ModelResponse::text("hi")],
        |_| {},
    )
    .await;

    assert!(!run.prose.contains("context tab"), "{}", run.prose);
    assert!(run.prose.contains("`/undo` away"), "{}", run.prose);
    assert!(
        run.prose.contains("`/restore` with its number"),
        "{}",
        run.prose
    );
    assert!(
        run.prose.contains("`/pin` with its number"),
        "{}",
        run.prose
    );
}

/// A parameter named without a value is refused, rather than answered with the list.
///
/// note: found live. `/params temperature` was read as `/params`, so it printed what was set as
/// though it had done something, which reads as the parameter having been set or taken away.
#[tokio::test]
async fn a_parameter_named_without_a_value_is_refused() {
    let run = run(
        "/params temperature 0.2\n/params temperature\n",
        vec![],
        |_| {},
    )
    .await;

    assert!(
        run.prose
            .contains("`/params temperature` needs a JSON value"),
        "{}",
        run.prose
    );
    assert_eq!(
        run.app.kernel.params().get("temperature"),
        Some(&json!(0.2))
    );
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
            .set_compactor(Some(Arc::new(kamchatka::tools::Shedder {
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
        // read, so the pass may take it: a result the model has not been shown yet is kept
        app.kernel.push(ContextItem::assistant(
            Content::text("it is forty thousand x"),
            vec![],
        ));
        // and asked something since, so that turn is over: a pass leaves the turn in progress
        // alone
        app.kernel.push(ContextItem::user("and now?"));
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
/// actually does needs a sandbox and this runs on a kernel without one too. `Sandbox::of` reads
/// exactly this and `tests/sandbox.rs` covers the other half.
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

/// A refusal is not remembered, and asking for one does not write the rules that allow the call.
///
/// note: `always` remembers every subject the policy consulted, as allowed, and it did so whatever
/// the answer was. No client here sends a refusal with `remember` on, and the protocol takes one:
/// a client of its own asking never to allow this wrote the rules that allow it, and the sweep
/// behind it let the next such call through.
#[tokio::test]
async fn a_refusal_asked_to_be_remembered_is_refused_rather_than_allowed() {
    let curl = |id: &str, host: &str| {
        ToolCall::new(
            id,
            "shell",
            json!({ "call": { "action": "run", "cmd": format!("curl https://{host}") } }),
        )
    };
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(vec![
        ModelResponse::tool_calls(vec![curl("c1", "example.com"), curl("c2", "example.org")]),
        ModelResponse::text("done"),
    ]);
    app.kernel.add_tool(Arc::new(Watchful {
        policy: app.policy.clone(),
        seen: Arc::new(std::sync::Mutex::new(Vec::new())),
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

    assert!(app.decide(waiting[0].id, Grant::Deny, true).is_err());
    // nothing was allowed on the strength of it, and nothing was swept
    assert_ne!(app.policy.verdict(&waiting[1]), Verdict::Allow);
    assert_eq!(app.kernel.pending_permissions().len(), 2);
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
/// the session reports from. What was lost was the advisor's first notice, which is the one
/// saying something is wrong before a question depends on it.
///
/// note: driven through `App::on_outcome`, which is where the provider's own notice is read and
/// is a door all three loops come through - so what this pins holds for the headless loop and a
/// served session as well as the drawn one. The drawn loop also polls on its tick, which is what
/// gets a starting advisor onto the screen before anybody has sent anything.
#[cfg(feature = "shell-advisor")]
#[tokio::test]
async fn the_advisor_says_what_it_has_to_say_to_the_session() {
    use nachalnik_providers::system1::Client;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    // an endpoint that lists a model other than the one asked for: what the advisor reports is
    // the notice its probe left at startup, before the session could read it
    let body = "{\"data\":[{\"id\":\"vendor/decider\"}]}";
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
    let engine = Client::new("vendor/nope", format!("http://{address}"), "k");
    engine.probe().await;

    let wired = Setup {
        advisor: Some(Arc::new(engine)),
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
        said.iter()
            .any(|line| line.contains("does not list vendor/nope")),
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
    use nachalnik_providers::system1::Client;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    let body = "{\"model\":\"vendor/decider\",\"answers\":{\"rating\":{\"type\":\"score\",\
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
        advisor: Some(Arc::new(Client::new(
            "vendor/decider",
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

/// And undo and redo are commands too, which is how a run with no keys takes a change back.
///
/// note: `u` and `U` on the context tab are the same two calls into the same function, for the
/// reason `/cleanup` above is: one act and two ways in. Everything this program says about undoing
/// something names the key, and a pipe, a `--connect` client and a browser have none of them, so
/// before this there was a run with no way to take an exclusion or an edit back at all.
///
/// note: the item read back at the end rather than after the first `/undo` as well, so that the
/// test is about both commands reaching the stack rather than about one of them.
#[tokio::test]
async fn undo_and_redo_are_commands_as_well_as_keys() {
    let run = run(
        "/note something worth undoing\n/undo\n/redo\n",
        vec![],
        |_| {},
    )
    .await;

    assert!(run.prose.contains("undone"), "{}", run.prose);
    assert!(run.prose.contains("redone"), "{}", run.prose);
    assert!(
        run.app
            .kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("something worth undoing")),
        "the redo did not put it back: {:?}",
        run.app.kernel.items()
    );
    // and both took a checkpoint, so a second `/undo` has something to reach for
    assert_eq!(run.app.kernel.with_context(|context| context.undo_len()), 1);
}

/// And nothing is claimed where there is nothing on the stack, whatever the caller asked for.
///
/// note: the same two lines `u` and `U` answer with, and the reason they are worth having here is
/// that a script reads them: a run that says `undone` when it undid nothing will be trusted by
/// whatever is driving it.
#[tokio::test]
async fn an_undo_with_nothing_to_undo_says_so() {
    let run = run("/undo\n/redo\n", vec![], |_| {}).await;

    assert!(
        run.prose.contains("there is nothing to undo"),
        "{}",
        run.prose
    );
    assert!(
        run.prose.contains("there is nothing to redo"),
        "{}",
        run.prose
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

/// A command has one name, and the second spellings it used to take are not commands.
///
/// note: `/prune` and `/keep` were aliases of `/exclude` and `/pin`, and `/policy` and `/provider`
/// the other names of `/permissions` and `/endpoint` - so every place a session is read back in,
/// the help, the trace and a script, had two words for one act. They are refused as any word that
/// is not a command is, with no pointer: a pointer is kept for a name somebody arrives with from
/// another agent, as `/clear` is, and none of these is one. Nothing happens to the context, which
/// is what an unknown command owes.
#[tokio::test]
async fn a_command_has_one_name() {
    let run = run(
        "/prune files\n/keep files\n/policy\n/provider\n",
        vec![],
        |app| {
            app.kernel.push(ContextItem::file("a.rs", "one"));
        },
    )
    .await;

    for old in ["prune", "keep", "policy", "provider"] {
        assert!(
            run.prose.contains(&format!(
                "there is no `/{old}`; `/help` lists what there is"
            )),
            "{}",
            run.prose
        );
    }
    assert!(
        run.app
            .kernel
            .items()
            .iter()
            .all(|item| item.state == nachalnik::ContextState::Active),
        "an old name moved something anyway"
    );
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

/// A `/model` or `/endpoint` switch is in the record, from what was in use to what is now.
///
/// note: both switch the provider the kernel already holds in place, so the kernel's slot never
/// changed and `model.changed` was never emitted: the session talked to another model and the
/// log, a `/save` and a resume from it could not say when. Setting the same provider again would
/// have asked it after the switch and recorded the change as from the new model to itself.
#[tokio::test]
async fn a_switch_of_model_is_in_the_record() {
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
        "first",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(
            &mut app,
            &mut events,
            &mut finished,
            "/model second\n/endpoint http://127.0.0.1:2 third\n/model\n".as_bytes(),
        )
        .await
        .expect("the run failed");

    let changes: Vec<(Option<String>, Option<String>)> = app
        .kernel
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            nachalnik::Event::ModelChanged { from, to } => {
                Some((from.map(|it| it.model), to.map(|it| it.model)))
            }
            _ => None,
        })
        .collect();
    let named = |from: &str, to: &str| (Some(from.to_owned()), Some(to.to_owned()));
    assert!(changes.contains(&named("first", "second")), "{changes:?}");
    assert!(changes.contains(&named("second", "third")), "{changes:?}");
}

/// `/endpoint` with something that is not an address refuses it and keeps the one it had.
///
/// note: it took any word, so `/endpoint not a url at all` announced `a url at all at not` and the
/// next request failed as a `builder error`; and `/endpoint localhost:11434/v1`, the scheme left
/// off, is a URL whose scheme is `localhost`. And one with a query string, which every path is
/// appended after: `…/v1?token=…/chat/completions` never worked, and its failure wrote the token
/// into the record.
#[tokio::test]
async fn provider_refuses_what_is_not_an_address() {
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
        "first",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(
            &mut app,
            &mut events,
            &mut finished,
            "/endpoint not a url at all\n/endpoint localhost:11434/v1\n/endpoint http:///v1\n\
             /endpoint http://127.0.0.1:2/v1?token=t\n/endpoint http://127.0.0.1:2/v1#here\n"
                .as_bytes(),
        )
        .await
        .expect("the run failed");

    let prose = String::from_utf8(prose).expect("utf-8");
    for typed in [
        "`not`",
        "`localhost:11434/v1`",
        "`http:///v1`",
        "`http://127.0.0.1:2/v1?token=t`",
        "`http://127.0.0.1:2/v1#here`",
    ] {
        assert!(
            prose.contains(&format!("{typed} is not an address")),
            "{typed}: {prose}"
        );
    }
    assert!(
        !prose.contains("from now on"),
        "nothing was announced: {prose}"
    );
    assert_eq!(app.provider.endpoint(), "http://127.0.0.1:1");
    assert_eq!(app.kernel.model_info().expect("a model").model, "first");
}

/// Serves `answer` - a status line, its headers and a body - to every request, for as long as anybody
/// asks.
async fn answering(answer: &'static str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut discard = vec![0u8; 65536];
            let _ = socket.read(&mut discard).await;
            let _ = socket.write_all(answer.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });

    format!("http://{at}")
}

/// Drives `input` against the OpenAI dialect at `address`, asking for `model`, and hands back the
/// prose.
async fn prose_at(address: String, model: &str, input: &str) -> String {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = Setup {
        tools: Some(Vec::new()),
        compact: None,
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(model, address, "")))
    .expect("the wiring failed");

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    // a failed last turn is an `Err`, and some of these are about what was said on the way to one
    let _ = Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, input.as_bytes())
        .await;

    String::from_utf8(prose).expect("the prose is text")
}

/// A switch's notice is printed when the switch is done: before the next line, and on the last.
///
/// note: the run looked for a notice only when a turn ended, so `/model` with a name the address
/// does not list said so after the *next* answer - blaming whichever model had just given it - and
/// a script ending on the switch never said so at all.
#[tokio::test]
async fn a_switchs_notice_is_printed_when_the_switch_is_done() {
    const LISTS: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: 28\r\n\r\n{\"data\":[{\"id\":\"resident\"}]}";
    let missing = "bogus/nope is not one of";

    let prose = prose_at(
        answering(LISTS).await,
        "resident",
        "/model bogus/nope\n/model resident\n",
    )
    .await;
    let said = prose.find(missing).expect(&prose);
    let next = prose.find("switching to resident").expect(&prose);
    assert!(said < next, "before the next line: {prose}");
    assert_eq!(prose.matches(missing).count(), 1, "{prose}");

    let prose = prose_at(answering(LISTS).await, "resident", "/model bogus/nope\n").await;
    assert!(prose.contains(missing), "on the last line: {prose}");
}

/// A switch on a script's last line is in the record before the session ends.
///
/// note: the line after a switch is what waits for it, and at the end of the input there is none.
/// The endpoint here takes a moment to fail, so the switch is still settling when the input
/// closes; without waiting for it, `session.finished` was written first and the change landed in
/// a record already ended - or never, once the program had gone.
#[tokio::test]
async fn a_switch_on_the_last_line_is_recorded_before_the_session_ends() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                drop(socket);
            });
        }
    });
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
        "first",
        format!("http://{address}"),
        "",
    )))
    .expect("the wiring failed");

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(
            &mut app,
            &mut events,
            &mut finished,
            &b"/model second\n"[..],
        )
        .await
        .expect("the run failed");

    let names: Vec<&str> = app
        .kernel
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            nachalnik::Event::ModelChanged { to: Some(to), .. } if to.model == "second" => {
                Some("switched")
            }
            nachalnik::Event::SessionFinished => Some("finished"),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["switched", "finished"]);
}

/// A rule given at the start - a flag or a settings file - is at the start of the record.
#[tokio::test]
async fn a_rule_given_at_the_start_is_in_the_record() {
    let Wired { app, .. } = Setup {
        tools: Some(Vec::new()),
        compact: None,
        allow: vec![Subject::parse("exec:run")],
        deny: vec![Subject::parse("*.pem")],
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(
        "",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");

    let ruled: Vec<_> = app
        .kernel
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            nachalnik::Event::PolicyRuled {
                subject, verdict, ..
            } => Some((subject, verdict)),
            _ => None,
        })
        .collect();
    assert_eq!(
        ruled,
        [
            ("exec:run".to_owned(), Verdict::Allow),
            ("*.pem".to_owned(), Verdict::Deny)
        ]
    );
}

/// What a person reads is text: an escape sequence in the model's words is not handed to their
/// terminal.
///
/// note: the prose is somebody's terminal and most of what reaches it is not this program's to
/// vouch for. The screen drops control characters because its drawing library does; this wrote
/// them through, so a model that had read a file carrying a sequence could set the clipboard,
/// clear the screen, or overwrite a line with one that looks like this program's.
#[tokio::test]
async fn an_escape_sequence_in_the_answer_does_not_reach_the_terminal() {
    let script = vec![ModelResponse::text(
        "first\x1b]52;c;aGk=\x07 then\x1b[2J\r\u{9b}31m last\tcolumn\nnext line",
    )];
    let run = run("go\n", script, |_| {}).await;

    // the sequences' printable remains stay, as they do on the screen: inert without the byte
    // that made them a sequence
    assert!(
        run.prose
            .contains("first]52;c;aGk= then[2J31m last\tcolumn\nnext line"),
        "{:?}",
        run.prose
    );
    assert!(
        !run.prose.contains(['\x1b', '\x07', '\r', '\u{9b}']),
        "{:?}",
        run.prose
    );
}

/// `/continue` after a turn the model ended itself says so, rather than asking again.
///
/// note: found live. `/continue` started a turn from wherever the kernel rested, and after a clean
/// `end_turn` that is a request carrying nothing new - the model answered the same question a
/// second time, at the price of a request.
#[tokio::test]
async fn continue_after_a_finished_turn_asks_nothing() {
    let script = vec![
        ModelResponse::text("forty-two"),
        ModelResponse::text("asked again"),
    ];
    let run = run("what is six times seven?\n/continue\n", script, |_| {}).await;

    assert!(run.prose.contains("forty-two"), "{}", run.prose);
    assert!(!run.prose.contains("asked again"), "{}", run.prose);
    assert!(run.prose.contains("nothing to continue"), "{}", run.prose);
}

/// A bare `/step` after a finished turn is declined as `/continue` is.
///
/// note: found live: `/step` twice after an answer asked the same question twice more.
#[tokio::test]
async fn a_bare_step_after_a_finished_turn_asks_nothing() {
    let script = vec![
        ModelResponse::text("forty-two"),
        ModelResponse::text("asked again"),
    ];
    let run = run("what is six times seven?\n/step\n", script, |_| {}).await;

    assert!(!run.prose.contains("asked again"), "{}", run.prose);
    assert!(run.prose.contains("nothing to continue"), "{}", run.prose);
}

/// A context that ends on an answer with the machine idle - what `/load` and `-r` leave - is not
/// asked about again either.
#[tokio::test]
async fn continue_over_a_loaded_answer_asks_nothing() {
    let script = vec![ModelResponse::text("asked again")];
    let run = run("/continue\n", script, |app| {
        app.kernel
            .push(ContextItem::user("what is six times seven?"));
        app.kernel
            .push(ContextItem::assistant("forty-two", Vec::new()));
    })
    .await;

    assert!(!run.prose.contains("asked again"), "{}", run.prose);
    assert!(run.prose.contains("nothing to continue"), "{}", run.prose);
}

/// `/continue` with nothing to send says so, and is not a failed turn.
///
/// note: found live: it reached the kernel's empty-projection error, and a headless run ended on
/// it exited `1` for a request that was never sent.
#[tokio::test]
async fn continue_with_nothing_to_send_is_not_a_failure() {
    // `run` refuses a run that ended in a failed turn, which is the exit code
    let run = run("/continue\n/step\n", Vec::new(), |_| {}).await;

    assert_eq!(
        run.prose.matches("nothing to answer").count(),
        2,
        "{}",
        run.prose
    );
}

/// An answer that was cut short is carried on from, which is what `/continue` is for.
#[tokio::test]
async fn continue_after_a_cut_short_answer_carries_on() {
    let script = vec![
        ModelResponse {
            stop: StopReason::Length,
            ..ModelResponse::text("forty")
        },
        ModelResponse::text("-two"),
    ];
    let run = run("what is six times seven?\n/continue\n", script, |_| {}).await;

    assert!(run.prose.contains("-two"), "{}", run.prose);
    assert!(!run.prose.contains("nothing to continue"), "{}", run.prose);
}

/// A model that stopped short says why, and one that finished or was stopped says nothing.
#[tokio::test]
async fn a_turn_that_stopped_short_says_why() {
    let stopped = |said: &str, stop| ModelResponse {
        stop,
        ..ModelResponse::text(said)
    };
    let script = vec![
        stopped("Rivers are among", StopReason::Length),
        stopped("", StopReason::Refusal),
        stopped("a sentence", StopReason::Other("eos_token".to_owned())),
        stopped("done", StopReason::EndTurn),
        stopped("half", StopReason::Other("interrupted".to_owned())),
    ];
    let run = run("one\ntwo\nthree\nfour\nfive\n", script, |_| {}).await;

    assert!(
        run.prose
            .contains("Rivers are among\n· the answer stopped at the model's length limit"),
        "{}",
        run.prose
    );
    assert!(run.prose.contains("reported a refusal"), "{}", run.prose);
    assert!(run.prose.contains("`eos_token`"), "{}", run.prose);
    assert!(!run.prose.contains("interrupted"), "{}", run.prose);
    // and nothing after the turn that finished or the one that was stopped
    assert_eq!(run.prose.matches("\n· ").count(), 3, "{}", run.prose);
}

/// `/note` and `/attach` say what went in, which a screen draws from the item and a pipe has no
/// other sight of.
#[tokio::test]
async fn what_a_command_put_in_is_said() {
    let dir = common::scratch("attached-headless");
    let file = dir.join("notes.txt");
    std::fs::write(&file, "PLUM").expect("a file to attach");

    let run = run(
        &format!(
            "/note the runner has no network\n/attach {}\n",
            file.display()
        ),
        Vec::new(),
        |_| {},
    )
    .await;

    assert!(
        run.prose.contains("note (memory) went into the context"),
        "{}",
        run.prose
    );
    assert!(
        run.prose.contains("(file) went into the context"),
        "{}",
        run.prose
    );
}

/// One answer does not run into the next on a person's half of the output.
///
/// note: found live: a `/step` answered `42` straight onto the end of the previous answer's last
/// sentence, which had no newline after it.
#[tokio::test]
async fn one_answer_does_not_run_into_the_next() {
    let script = vec![ModelResponse::text("first"), ModelResponse::text("second")];
    let run = run("one\ntwo\n", script, |_| {}).await;

    assert!(!run.prose.contains("firstsecond"), "{:?}", run.prose);
    assert!(run.prose.contains("first\nsecond"), "{:?}", run.prose);
}

/// A model that answers in one piece, with no fragment ahead of it, the way a provider that does
/// not stream does - and an endpoint that ignores being asked to.
struct Whole(&'static str);

#[async_trait]
impl Provider for Whole {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("whole", "whole")
    }

    async fn respond(&self, _: ModelRequest, _: DeltaSink) -> Result<ModelResponse, BoxError> {
        Ok(ModelResponse::text(self.0))
    }
}

/// An answer that arrived whole is printed, once.
#[tokio::test]
async fn an_answer_that_was_not_streamed_is_printed() {
    let run = run("ask\n", Vec::new(), |app| {
        app.kernel
            .set_provider(Arc::new(Whole("said in one piece")));
    })
    .await;

    assert_eq!(
        run.prose.matches("said in one piece").count(),
        1,
        "{}",
        run.prose
    );
}

/// A run that fails still ends the last answer's line, so the caller's parting line is one of its
/// own.
///
/// note: an input that breaks is what fails it here, read after an answer with no newline at its
/// end: the loop leaves with an error, and it used to leave before ending that line.
#[tokio::test]
async fn a_run_that_fails_still_ends_the_answer_s_line() {
    /// One line, and then a read that fails.
    struct Breaks(Option<&'static [u8]>);

    impl tokio::io::AsyncRead for Breaks {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(match self.0.take() {
                Some(line) => {
                    buf.put_slice(line);
                    Ok(())
                }
                None => Err(std::io::Error::other("the input broke")),
            })
        }
    }

    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(vec![ModelResponse::text("no newline")]);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    let ran = Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(
            &mut app,
            &mut events,
            &mut finished,
            tokio::io::BufReader::new(Breaks(Some(b"say it\n"))),
        )
        .await;

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(ran.is_err(), "{prose}");
    assert!(prose.ends_with("no newline\n"), "{prose:?}");
}

/// A stop asked for inside that window is honoured by the turn it carries on, and a line waiting
/// for the turn does not start another.
#[tokio::test]
async fn a_stop_inside_the_window_is_not_undone_by_carrying_the_turn_on() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("never asked"),
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
    assert!(app.busy, "the window is not where this test thinks it is");
    let question = app.asked().expect("the turn stopped without asking");
    app.decide(question.id, Grant::Allow, false)
        .expect("the answer was refused");
    app.submit("and then").await;
    app.submit("/stop").await;
    app.on_outcome(outcome);

    while app.busy {
        let outcome = finished.recv().await.expect("the turn never ended");
        while let Ok(event) = events.try_recv() {
            app.on_event(event);
        }
        app.on_outcome(outcome);
    }
    assert!(
        !app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("never asked")),
        "the stop was undone"
    );
}

/// A tool that talks a great deal, fast enough that the loop cannot keep up with it.
///
/// note: one fragment per line of what it is reporting, which is what `shell` does as a command's
/// output arrives - and the reason the notice this is about is false. A headless run prints what a
/// tool was asked to do and what it cost, never the output between, so a subscriber that fell
/// behind on these has missed nothing it was going to be shown.
struct Chatty {
    /// How many lines to report.
    lines: usize,
}

#[async_trait]
impl Tool for Chatty {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("cat", "prints a file")
    }

    async fn invoke(&self, _call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        for line in 0..self.lines {
            output.push(format!("line {line}\n"));
        }

        Ok(ToolOutput::new("printed"))
    }
}

/// A tool that floods its output is not reported as a flood of fragments gone by.
///
/// note: what the notice used to say, and both halves of it were false. It counted `tool.output`
/// fragments, which this loop has never printed - a headless run reports a tool's result by its
/// size and leaves the output to the model - and it promised the records had them, which nothing
/// holds: a fragment is not an event anybody recorded, so there is nothing to go back for.
///
/// note: what is asserted is what the prose does *not* say, and nothing about which events came
/// through. A flood of twenty thousand fragments is past what any subscription keeps, so the
/// events around it are themselves among what went by - which is the thing being lost here, and
/// not something a test about the prose should depend on.
#[tokio::test]
async fn output_nothing_prints_is_not_reported_as_fragments_gone_by() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "cat", json!({}))]),
        ModelResponse::text("it is all yours"),
    ];
    let run = run("cat huge.txt\n", script, |app| {
        app.kernel.add_tool(Arc::new(Chatty { lines: 20_000 }));
    })
    .await;

    assert!(
        !run.prose.contains("went by too fast"),
        "fragments nothing prints were reported as lost: {}",
        run.prose
    );
    // and the claim that the records hold them goes with it: a fragment is in no record, so a
    // reader told to go to the log for one is sent after something that is not there
    assert!(
        !run.prose.contains("the records have"),
        "the prose points at a log that has none of it: {}",
        run.prose
    );
}

/// A copy with no terminal to go to says so in the one line, rather than a line saying it went to
/// the clipboard and a second saying it could not.
///
/// note: `App::not_copied` rather than `/copy` down a pipe, because whether the test's own stderr
/// is a terminal depends on who runs it. What is being checked is the one line a loop gets when
/// the hand-over has nothing to hand it to, which is the whole of the contradiction.
#[tokio::test]
async fn a_copy_that_went_nowhere_says_so_once() {
    let Wired { mut app, .. } = wired(vec![]);
    let id = app.kernel.push(ContextItem::user("copy me"));

    app.copy(id);
    app.not_copied("there is no terminal here for it to go to");

    let notes: Vec<_> = app.notes(0).map(|entry| entry.text.clone()).collect();
    assert_eq!(
        notes,
        [format!(
            "[{id}] was not copied: there is no terminal here for it to go to"
        )],
    );
}

/// `/copy` down a pipe leaves one line about the copy, and it is the true one.
///
/// note: end to end where the one above is not, because the two lines are two halves of a loop
/// and only driving the loop shows whether it puts them together. `hand_over` writes to this
/// process's own standard error, which under a test runner is a pipe, so the hand-over has nothing
/// to go to - the same thing a session under `kamchatka --headless` is in when its caller captured
/// the output. What must not appear is the receipt, because the loop knows it is false by the time
/// it could be read.
#[tokio::test]
async fn a_copy_down_a_pipe_does_not_say_it_reached_the_clipboard() {
    let run = run("/copy 1\n", vec![], |app| {
        app.kernel.push(ContextItem::user("what does it say?"));
    })
    .await;

    assert!(
        !run.prose.contains("to the clipboard"),
        "the receipt is still there after the hand-over had nowhere to go: {}",
        run.prose
    );
    assert!(
        run.prose
            .contains("was not copied: there is no terminal here for it to go to"),
        "the copy was not accounted for: {}",
        run.prose
    );
}

/// A copy that did reach the terminal keeps its receipt, and the note is not there beside it.
#[tokio::test]
async fn a_copy_that_went_to_the_terminal_is_not_taken_back() {
    let Wired { mut app, .. } = wired(vec![]);
    let id = app.kernel.push(ContextItem::user("copy me"));

    app.copy(id);
    let notes: Vec<_> = app.notes(0).map(|entry| entry.text.clone()).collect();
    assert_eq!(notes, [format!("[{id}] to the clipboard: 7 bytes")]);
}

/// `/permissions` says what the policy is, down a pipe as well as at a screen.
///
/// note: it was a tab and nothing else, so a caller with no screen got no rules at all - and the
/// rules are the answer the command is for: what is allowed, what is refused, and what each
/// covers. It was a page rather than a line for the reason `/budget` is one: a policy holds more
/// rows than a line holds, and this is the page a caller without a tab reads instead.
#[tokio::test]
async fn the_policy_is_a_page_and_not_only_a_tab() {
    let run = run("/permissions\n", vec![], |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("grep", "found it").with_capabilities([Capability::fs("read")]),
        ));
        app.policy
            .set(&Subject::Capability(Capability::exec("run")), Verdict::Deny);
    })
    .await;

    // the rows the tab draws, and the one thing above them the tab says about the whole table
    assert!(run.prose.contains("--- the policy ---"), "{}", run.prose);
    assert!(run.prose.contains("exec:run"), "{}", run.prose);
    assert!(run.prose.contains("deny"), "{}", run.prose);
    // the screen still gets its tab, since that is what somebody at a desk is asking for
    assert_eq!(run.app.tab, kamchatka::app::Tab::Permissions);
    // and nothing was sent to the model: a command is answered here rather than by asking
    assert!(!run.names().contains(&"model.requested".to_owned()));
}

/// A policy with nothing decided says so, rather than printing a table of nobody's answers.
///
/// note: the tab does not list a row for a subject nobody has answered about, and a page that did
/// would bury the one line that says what this agent can do without stopping.
#[tokio::test]
async fn a_policy_nobody_has_decided_anything_about_says_so() {
    let run = run("/permissions\n", vec![], |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("grep", "found it").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    assert!(
        run.prose.contains("nothing has been decided"),
        "{}",
        run.prose
    );
}

/// A model that panics on its first request and answers the second.
struct PanicsOnce(std::sync::atomic::AtomicBool);

#[async_trait]
impl Provider for PanicsOnce {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("panics", "panics")
    }

    async fn respond(&self, _: ModelRequest, _: DeltaSink) -> Result<ModelResponse, BoxError> {
        if !self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
            panic!("the provider fell over");
        }
        Ok(ModelResponse::text("second answer"))
    }
}

/// A turn that panics is a failed turn: it is said, and the next line is still read.
///
/// note: the turn runs on a task of its own, and a panic there used to send no outcome at all,
/// so `busy` stayed set and the run waited for good. A provider is the seam that can still do it
/// - a tool's panic is answered by the kernel as a failed call - and a timeout is what turns a
/// hang into a failure here rather than into a suite that never ends.
#[tokio::test]
async fn a_turn_that_panics_fails_and_the_session_carries_on() {
    let run = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run("first\nsecond\n", Vec::new(), |app| {
            app.kernel
                .set_provider(Arc::new(PanicsOnce(Default::default())));
        }),
    )
    .await
    .expect("the run hung on the panicked turn");

    assert!(
        run.prose
            .contains("the turn panicked: the provider fell over"),
        "{}",
        run.prose
    );
    assert!(run.prose.contains("second answer"), "{}", run.prose);
    assert!(!run.app.busy);
}

/// A tool that panics when it is run.
struct Falls;

#[async_trait]
impl Tool for Falls {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("falls", "panics").with_capabilities([Capability::fs("read")])
    }

    async fn invoke(&self, _: &ToolCall, _: OutputSink) -> Result<ToolOutput, BoxError> {
        panic!("the tool fell over")
    }
}

/// A tool that panics is a call that failed, and the record says which and how.
#[tokio::test]
async fn a_tool_that_panics_is_a_failed_call_in_the_record() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "falls", json!({}))]),
        ModelResponse::text("it fell over"),
    ];
    let run = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_with("try it\n", script, Grant::Allow, |app| {
            app.kernel.add_tool(Arc::new(Falls));
        }),
    )
    .await
    .expect("the run hung on the panicked tool");

    let tools: Vec<String> = run
        .names()
        .into_iter()
        .filter(|name| name.starts_with("tool.") && name != "tool.requested")
        .collect();
    assert_eq!(tools, ["tool.started", "tool.panicked", "tool.finished"]);
    assert!(run.prose.contains("it fell over"), "{}", run.prose);
}

/// The record a run leaves checks clean, and each way of spoiling it is named.
///
/// note: a real run rather than a written log, because the claim `--check` makes is about the
/// records this program writes: that a record it left is one nothing is wrong with. A check that
/// found something in an ordinary session would be a check nobody could use.
#[tokio::test]
async fn the_record_a_run_leaves_checks_clean_and_a_spoiled_one_does_not() {
    use kamchatka::check::check;

    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("read it"),
    ];
    // `/undo` then `/redo` on purpose, and the exclusion after them: the log's own `context.undone`
    // takes an item back out and its `context.redone` puts it in again, and nothing after that
    // names the item again - so the `context.redone` is the only record that leaves it in the
    // context, and a reader that did not read it would say the snapshot holds an item the log
    // never added
    let run = run_with(
        "look around\n/undo\n/redo\n/exclude 1\n",
        script,
        Grant::Allow,
        |app| {
            app.kernel.add_tool(Arc::new(
                ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
            ));
        },
    )
    .await;
    let snapshot = serde_json::to_string(&run.app.kernel.snapshot()).unwrap();
    let log = &run.records;

    let clean = check(Some(log), Some(&snapshot));
    assert!(clean.findings.is_empty(), "{:?}", clean.findings);
    assert_eq!(clean.records, log.lines().count());

    let lines: Vec<&str> = log.lines().collect();
    let without = |pick: &dyn Fn(&str) -> bool| {
        lines
            .iter()
            .filter(|line| !pick(line))
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    };
    let found = |log: &str, snapshot: Option<&str>| check(Some(log), snapshot).findings;

    // a log drained part way through, which begins in the middle of the session, is not taken
    // for a whole one: what came before its first record is not the log's to account for
    let asked = lines
        .iter()
        .position(|line| line.contains("\"tool.requested\""))
        .expect("the run asked for a call");
    let drained = lines[asked + 1..].join("\n");
    let findings = found(&drained, Some(&snapshot));
    assert!(findings.is_empty(), "{findings:?}");

    // a record taken out is a gap
    let gap = without(&|line| line.contains("\"model.requested\""));
    let findings = found(&gap, None);
    assert!(
        findings.iter().any(|it| it.contains("missing")),
        "{findings:?}"
    );

    // a finished call taken out is a call that never finished
    let open = without(&|line| line.contains("\"tool.finished\""));
    let findings = found(&open, None);
    assert!(
        findings
            .iter()
            .any(|it| it.contains("`c1`") && it.contains("never finished")),
        "{findings:?}"
    );

    // a line cut short, a record twice and an event from a later version are each named
    let spoiled = format!(
        "{log}{}\n{}\n{{\"seq\": 9998, \"at\": 1, \"event\": {{\"event\": \"tool.teleported\"}}}}\n\
         {{\"seq\": 9999, \"at\"",
        lines[lines.len() - 1],
        lines[lines.len() - 1],
    );
    let findings = found(&spoiled, None);
    for said in ["numbered the same", "`tool.teleported`", "is not a record"] {
        assert!(
            findings.iter().any(|it| it.contains(said)),
            "{said}: {findings:?}"
        );
    }

    // a log whose own records disagree with each other: a change from a state the log did not
    // leave the item in, and a change to an item the log never added
    let joined = lines.join("\n") + "\n";
    let changed = lines
        .iter()
        .find(|line| line.contains("\"context.changed\""))
        .expect("the run changed an item");
    let mut value: serde_json::Value = serde_json::from_str(changed).unwrap();
    value["event"]["from"] = json!("pinned");
    let findings = found(
        &joined.replacen(changed, &value.to_string(), 1),
        Some(&snapshot),
    );
    assert!(
        findings
            .iter()
            .any(|it| it.contains("where the log had it")),
        "{findings:?}"
    );

    // and a change to an item whose `context.added` is not in the log at all, which is what a
    // record taken out of the middle of a session leaves behind: the log says it changed item 1,
    // and nothing in it added item 1. The snapshot is the real one here, which still holds
    // item 1, so the finding is about the record that is gone rather than about the item
    let without_one =
        without(&|line| line.contains("\"context.added\"") && line.contains("\"id\":1,"));
    let findings = found(&without_one, Some(&snapshot));
    assert!(
        findings
            .iter()
            .any(|it| it.contains("changes item 1") && it.contains("never added")),
        "{findings:?}"
    );

    // and a snapshot edited to say something the log does not
    let mut edited: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    edited["items"][0]["state"] = json!("pinned");
    let findings = found(log, Some(&edited.to_string()));
    assert!(
        findings.iter().any(|it| it.contains("the log leaves it")),
        "{findings:?}"
    );
    let mut dropped: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    dropped["items"].as_array_mut().unwrap().remove(0);
    let findings = found(log, Some(&dropped.to_string()));
    assert!(
        findings
            .iter()
            .any(|it| it.contains("the snapshot does not have it")),
        "{findings:?}"
    );
}

/// A session carried on from a snapshot checks clean too, against the items it brought back and
/// through an undo, and a count the log cannot account for is named.
#[test]
fn a_resumed_record_checks_clean_and_a_miscounted_one_does_not() {
    use kamchatka::check::check;
    use nachalnik::{Config, ContextState, Kernel};

    let first = Kernel::new(Config::default());
    first.push(ContextItem::user("one"));
    first.push(ContextItem::user("two"));

    let resumed = Kernel::resume(Config::default(), first.snapshot());
    let three = resumed.push(ContextItem::user("three"));
    resumed.set_state([nachalnik::ContextId(1)], ContextState::Excluded, None);
    resumed.push(ContextItem::user("four"));
    resumed.undo().expect("the push is undone");
    resumed.set_state([three], ContextState::Pinned, None);

    let log: String = resumed
        .history()
        .iter()
        .map(|record| serde_json::to_string(record).unwrap() + "\n")
        .collect();
    let snapshot = resumed.snapshot();
    let clean = check(Some(&log), Some(&serde_json::to_string(&snapshot).unwrap()));
    assert!(clean.findings.is_empty(), "{:?}", clean.findings);

    let mut short = serde_json::to_value(&snapshot).unwrap();
    short["items"].as_array_mut().unwrap().remove(1);
    let findings = check(Some(&log), Some(&short.to_string())).findings;
    assert!(
        findings
            .iter()
            .any(|it| it.contains("the log leaves 3 items") && it.contains("the snapshot has 2")),
        "{findings:?}"
    );
}

/// A page a line opened while a command was out is said to whoever has no screen to open it on.
///
/// note: `/help` answered `queued` because `/models` was still at the endpoint, and the caller was
/// handed it by `App::release` rather than by the screen redrawing. Somebody with keys presses
/// another key and reads the page; down a pipe and over a socket there is no key to press and
/// nothing would ever show it, so `release` says it in the chat instead - which is the only place
/// a caller with no keys is shown what a command answered.
#[tokio::test]
async fn a_page_from_a_line_that_waited_for_a_command_is_said_rather_than_opened() {
    let Wired {
        mut app,
        mut finished,
        ..
    } = wired(Vec::new());
    // a caller with no keys: a pipe, or a client
    app.keys = false;

    let first = app.submit("/models").await;
    assert_eq!(first.did, Did::Ran, "the listing went out to the endpoint");
    assert!(
        app.in_flight(),
        "and it is still out, so the next line waits for it"
    );

    // the page the waiting line opens: nothing on the screen can open it here, so it is said or it
    // is nowhere
    let queued = app.submit("/help").await;
    assert_eq!(queued.did, Did::Queued, "the line did not wait");

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), finished.recv())
        .await
        .expect("the listing never came back")
        .expect("the channel outlives the session");
    app.on_outcome(outcome);
    assert!(
        !app.in_flight(),
        "the listing is finished, so the waiting line can run"
    );

    assert!(app.release().await, "the waiting line was not handed in");

    let said: Vec<String> = app.notes(0).map(|entry| entry.text.clone()).collect();
    let page = said
        .iter()
        .find(|text| text.starts_with("--- "))
        .unwrap_or_else(|| panic!("the page was opened and never said: {said:?}"));
    assert!(
        page.contains("the commands"),
        "and it is the page `/help` answers with: {page}"
    );
    assert!(
        said.iter().any(|text| text.contains("/attach PATH")),
        "the page's own words: {said:?}"
    );
}

/// A listing that was stopped does not finish the listing that came after it.
///
/// note: `/models` reaches the endpoint, and the loop is a moment behind reading the answer when
/// somebody stops the session and asks for the list again. The answer to the first is on the
/// channel with nothing marking which listing it belongs to except the number it was given when it
/// went out, and that number is what tells the two apart - without it, the answer to a listing
/// nobody is waiting for finishes the one that is, and the second is never answered at all while
/// every line after it is held behind it.
#[tokio::test]
async fn a_listing_that_was_stopped_does_not_finish_the_listing_after_it() {
    let Wired {
        mut app,
        mut finished,
        ..
    } = wired(Vec::new());

    app.submit("/models").await;
    assert!(app.in_flight(), "the first listing should be out");

    // its answer has arrived, and the loop has not read it yet
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), finished.recv())
        .await
        .expect("the first listing never came back")
        .expect("the channel outlives the session");

    // and the session is stopped, which is what lets the next listing go out while that answer is
    // still unread
    app.interrupt();
    assert!(!app.in_flight(), "the stop took the listing");

    app.submit("/models").await;
    assert!(app.in_flight(), "the second listing is out");

    // now the loop reads what the first one answered
    app.on_outcome(first);

    assert!(
        app.in_flight(),
        "the answer to a stopped listing finished the one that was sent after it"
    );
}

/// A compaction pass that falls over is said and the session carries on, as a turn's is.
///
/// note: `/compact` hands the pass to a task and waits for the answer, so the pass is the one
/// thing in the program that can fail where a turn cannot be watched failing - and it is what
/// holds every line after it. The task watching it sends what came back however the work ended,
/// including that it never came back at all; without that, a pass that panicked left the session
/// waiting for a command that is never coming, and every line after it queued for ever.
#[tokio::test]
async fn a_compaction_pass_that_fell_over_is_said_and_the_session_carries_on() {
    use nachalnik::{Budget, CompactionPlan, Compactor, ContextItem};

    /// A pass that falls over, which is what a compactor reaching for the model and finding
    /// something broken underneath it looks like from here.
    struct Falls;

    #[nachalnik::async_trait]
    impl Compactor for Falls {
        fn should_compact(&self, _budget: &Budget) -> bool {
            false
        }

        async fn plan(
            &self,
            _items: &[std::sync::Arc<ContextItem>],
            _budget: &Budget,
        ) -> Option<CompactionPlan> {
            panic!("the compactor fell over")
        }
    }

    let Wired {
        mut app,
        mut finished,
        ..
    } = wired(Vec::new());
    app.keys = false;
    app.kernel.set_compactor(Some(Arc::new(Falls)));

    let reply = app.submit("/compact").await;
    assert_eq!(reply.did, Did::Ran);
    assert!(app.in_flight(), "the pass is out");

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), finished.recv())
        .await
        .expect("a pass that fell over reported nothing, so the session waits for it for ever")
        .expect("the channel outlives the session");
    app.on_outcome(outcome);

    assert!(
        !app.in_flight(),
        "the pass is finished, whatever it finished with"
    );
    let said: Vec<String> = app.notes(0).map(|entry| entry.text.clone()).collect();
    assert!(
        said.iter()
            .any(|text| text == "the compaction pass failed before it came back"),
        "the session is not told the pass failed: {said:?}"
    );

    // and a line after it is read at all, rather than queued behind a pass that is never coming
    let reply = app.submit("/help").await;
    assert_eq!(reply.did, Did::Ran, "the next line waited for ever");
}

/// A `/model` switch still settling is waited for when the session is leaving.
///
/// note: the line after a switch is held for it, so a session whose last line is the switch has
/// nothing left to hand it in - and the switch's own notice would be taken by a look that never
/// comes, and its change written to a record that has already been ended, or written by a process
/// that has already gone. So the way out waits for the switch first, under its own bound, which is
/// what takes the handle: a switch left standing is a command still out, and every line after it
/// waits for one that is never coming.
#[tokio::test]
async fn a_switch_still_settling_is_waited_for_when_the_session_leaves() {
    let Wired {
        mut app,
        mut events,
        mut finished,
        ..
    } = wired(Vec::new());

    let reply = app.submit("/model another").await;
    assert_eq!(reply.did, Did::Ran);
    assert!(
        app.settling.is_some(),
        "the switch should still be settling"
    );

    app.wait_for_turn(&mut events, &mut finished, |_| {}).await;

    assert!(
        app.settling.is_none(),
        "the switch was left standing, so the session is still waiting on it"
    );
    assert!(
        !app.in_flight(),
        "and nothing is out at the endpoint once it has been waited for"
    );
}

/// A log of two sessions in one file is read as one, and the items the later session was not
/// resumed from are said rather than counted against it.
///
/// note: two sessions in one file is what a session leaves when it is started afresh in the
/// middle of a record: the new `session.started` follows the old one's records and numbers on
/// beside them. What came before it belongs to a session that is over, so the items it left are
/// not items this one has, and the count the two halves have to agree on is this one's alone.
#[test]
fn two_sessions_in_one_log_are_read_as_two_and_the_count_is_the_later_one() {
    use kamchatka::check::check;
    use nachalnik::{Config, ContextId, Kernel};

    let first = Kernel::new(Config::default());
    first.push(ContextItem::user("one"));
    first.push(ContextItem::user("two"));

    let second = Kernel::new(Config::default());
    second.push(ContextItem::user("three"));

    // one file, numbered on from where the first session ended, and this session's snapshot
    // beside it renumbered to where the records it names are
    let mut lines: Vec<String> = first
        .history()
        .iter()
        .map(|record| serde_json::to_string(record).unwrap())
        .collect();
    let mut seq = lines.len() as u64;
    for record in second.history() {
        seq += 1;
        let mut value = serde_json::to_value(&record).unwrap();
        value["seq"] = json!(seq);
        lines.push(value.to_string());
    }
    let log = lines.join("\n") + "\n";
    let mut snapshot = serde_json::to_value(second.snapshot()).unwrap();
    snapshot["last_seq"] = json!(seq);

    let clean = check(Some(&log), Some(&snapshot.to_string()));
    assert!(clean.findings.is_empty(), "{:?}", clean.findings);

    // an item this session never added is still named, the items the session before it left
    // being none of this one's: both sessions handed out identifier 1, and this one is the
    // only half of the log that says what it did with it
    let mut extra = snapshot.clone();
    extra["items"][0]["id"] = json!(ContextId(2));
    let findings = check(Some(&log), Some(&extra.to_string())).findings;
    assert!(
        findings
            .iter()
            .any(|it| it.contains("the snapshot has item 2, which the log never added")),
        "{findings:?}"
    );
}
