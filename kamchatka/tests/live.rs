//! The terminal against a real model, over the real wire.
//!
//! Skipped unless a key is in the environment:
//!
//! ```text
//! KAMCHATKA_API_KEY=... \
//! KAMCHATKA_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai \
//! KAMCHATKA_TEST_MODEL=gemini-3.5-flash-lite \
//!   cargo test -p kamchatka --test live -- --test-threads=1 --nocapture
//! ```
//!
//! note: `screen.rs` drives the same keys against a scripted model and asserts what is drawn.
//! This file exists for the one thing that cannot answer: whether the request the keys produced
//! is a request a real API accepts. The state cycle on the context tab rewrites the messages -
//! eliding a tool result replaces its content, pruning one takes the call down as well - and
//! "the projection is well formed" is a claim only a server can settle.

use std::{sync::Arc, time::Duration};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use kamchatka::{
    app::{App, Focus, Tab},
    provider::OpenAiCompatible,
    tools::{Careful, Subject},
    ui,
};
use nachalnik::{
    BoxError, Capability, Config, Content, ContextKind, ContextState, Kernel, OutputSink, Role,
    State, Tool, ToolCall, ToolOutput, ToolSpec, Verdict, async_trait,
};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

/// A model small enough to be free and able to call a tool.
const DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";

/// One at a time, so a free tier's rate limit is not what is under test.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A tool whose answer no model could guess, so seeing it proves the round trip.
struct Secret;

#[async_trait]
impl Tool for Secret {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            id: "secret".to_owned(),
            description: "Returns today's code word.".to_owned(),
            schema: Arc::new(json!({"type": "object", "properties": {}})),
            capabilities: vec![Capability::Read],
            output_limit: None,
        }
    }

    async fn invoke(&self, _call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        // long on purpose: elision is only interesting when there is something to hide, and a
        // request under 256 reported tokens is one the calibrating counter refuses to learn from
        let mut out = String::from("The code word is APRICOT. Repeat it exactly when asked.\n");
        for i in 0..400 {
            out.push_str(&format!(
                "line {i}: routine diagnostic output, of no interest.\n"
            ));
        }
        Ok(ToolOutput::new(Content::text(out)))
    }
}

/// The terminal, wired to a real endpoint, or `None` when there is no key to reach it with.
fn live() -> Option<(
    App,
    tokio::sync::mpsc::UnboundedReceiver<kamchatka::app::Outcome>,
)> {
    let key = std::env::var("KAMCHATKA_API_KEY")
        .or_else(|_| std::env::var("NACHALNIK_API_KEY"))
        .ok()?;
    let base = std::env::var("KAMCHATKA_BASE_URL")
        .unwrap_or_else(|_| "https://generativelanguage.googleapis.com/v1beta/openai".to_owned());
    let model = std::env::var("KAMCHATKA_TEST_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());

    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(OpenAiCompatible::new(model, base, key));
    kernel.set_provider(provider.clone());

    // the tool is allowed outright: what is under test is the shape of the request, and a
    // permission prompt in the middle of it would only be testing the prompt
    let policy = Arc::new(Careful::new());
    policy.set(&Subject::Capability(Capability::Read), Verdict::Allow);
    kernel.set_policy(policy.clone());
    kernel.add_tool(Arc::new(Secret));

    let (outcomes, finished) = tokio::sync::mpsc::unbounded_channel();

    Some((App::new(kernel, policy, provider, outcomes), finished))
}

macro_rules! live {
    () => {
        match live() {
            Some(pair) => pair,
            None => {
                eprintln!("no key in the environment; skipping");
                return;
            }
        }
    };
}

/// Types a line into the prompt, sends it, and waits for the turn to finish.
///
/// note: `tab` first, because the keys go wherever the focus is and a test that has just been
/// driving the context tab still has them there - where `enter` opens an item rather than sending
/// a message, and the turn this is waiting for never starts.
async fn send(
    app: &mut App,
    finished: &mut tokio::sync::mpsc::UnboundedReceiver<kamchatka::app::Outcome>,
    line: &str,
) {
    if app.focus != Focus::Input {
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .await;
    }
    for c in line.chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
            .await;
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await;

    // longer than the scripted suite's deadline, because a real model is on the other end
    let outcome = tokio::time::timeout(Duration::from_secs(90), finished.recv())
        .await
        .expect("the turn should have finished")
        .expect("the channel outlives the turn");
    app.on_outcome(outcome);
}

/// What the model last said, as text.
fn answer(app: &App) -> String {
    app.kernel
        .last_response()
        .and_then(|r| r.content.clone())
        .map(|c| c.to_text().into_owned())
        .unwrap_or_default()
}

/// Presses a key at the context tab, asking for the keys back first.
///
/// note: the mirror of `send`. Sending a message leaves the focus on the prompt, so a `space`
/// after one goes into the text being typed rather than onto the picked row - which looks exactly
/// like the cycle not advancing.
async fn press(app: &mut App, code: KeyCode) {
    if app.focus != Focus::Body {
        app.show(Tab::Context);
    }
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE)).await;
}

/// Draws at a given width and hands back just the status line.
fn status_line(app: &mut App, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 12)).expect("a backend");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("a frame");
    let buffer = terminal.backend().buffer().clone();
    (0..width)
        .map(|x| buffer[(x, 11)].symbol())
        .collect::<String>()
}

/// Draws, so that a frame that panics fails the test rather than going unnoticed.
fn draw(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("a backend");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("a frame");
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<Vec<_>>()
        .chunks(100)
        .map(|row| format!("{}\n", row.concat()))
        .collect()
}

/// The whole point of the three-way cycle, end to end: each step has to produce a request a real
/// API accepts, and the model has to read each one for what it is.
#[tokio::test]
async fn the_space_cycle_produces_requests_a_real_api_accepts() {
    let _serial = SERIAL.lock().await;
    let (mut app, mut finished) = live!();

    send(
        &mut app,
        &mut finished,
        "Call the secret tool, then tell me the code word.",
    )
    .await;
    let said = answer(&app).to_lowercase();
    assert!(
        said.contains("apricot"),
        "the model read the result: {said}"
    );

    // to the context tab, onto the tool result
    app.show(Tab::Context);
    let result = app
        .kernel
        .items()
        .iter()
        .position(|i| matches!(i.kind, ContextKind::ToolResult { .. }))
        .expect("a tool result");
    press(&mut app, KeyCode::Home).await;
    for _ in 0..result {
        press(&mut app, KeyCode::Down).await;
    }

    // ---- one press: elided. The call keeps its answer, and the answer is a marker
    press(&mut app, KeyCode::Char(' ')).await;
    let items = app.kernel.items();
    assert_eq!(
        items[result].state,
        ContextState::Elided,
        "{:?}",
        items[result].state
    );
    let projection = app.kernel.project();
    assert!(projection.repairs.is_empty(), "{:?}", projection.repairs);
    assert!(draw(&mut app).contains('…'), "the row is marked");

    // the shape of the request is asserted *before* sending: after it, the model may have called
    // the tool again, which is a fair thing for it to do and not this test's business
    let request = app.kernel.preview_request().expect("a request");
    let tools: Vec<_> = request
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .collect();
    assert_eq!(tools.len(), 1, "the result is still a message");
    assert!(
        tools[0]
            .content
            .as_ref()
            .is_some_and(|c| c.to_text().contains("removed from view by the user")),
        "carrying the marker: {:?}",
        tools[0].content
    );
    assert!(
        !tools[0]
            .content
            .as_ref()
            .is_some_and(|c| c.to_text().contains("APRICOT")),
        "and not the content it replaced"
    );
    // note: `APRICOT` is still in the request, in the model's *own* answer from the first turn.
    // That is the whole shape of the thing: eliding takes the evidence, not the conversation
    // about it, and the model is left to decide what it can still see
    assert!(
        request.messages.iter().any(|m| !m.tool_calls.is_empty()),
        "and the call that asked for it survived"
    );

    send(
        &mut app,
        &mut finished,
        "What is the code word now? Answer from this conversation only, do not call any tool. \
         If you cannot see it, say NOT VISIBLE.",
    )
    .await;
    let said = answer(&app);
    assert!(
        !said.is_empty(),
        "the API accepted an elided result and the model answered"
    );
    println!("  elided  -> {}", said.trim().replace('\n', " "));

    // ---- two presses: excluded. Now the projector has to take the call down too
    press(&mut app, KeyCode::Char(' ')).await;
    assert_eq!(app.kernel.items()[result].state, ContextState::Excluded);
    let projection = app.kernel.project();
    assert_eq!(projection.repairs.len(), 1, "{:?}", projection.repairs);

    // again, before sending: the result is gone and so is the call, which is the difference
    // between this and the step above
    let request = app.kernel.preview_request().expect("a request");
    assert!(
        !request.messages.iter().any(|m| m.role == Role::Tool),
        "the pruned result is gone"
    );
    assert!(
        request.messages.iter().all(|m| m.tool_calls.is_empty()),
        "and so is the call it answered"
    );

    send(
        &mut app,
        &mut finished,
        "And now? Answer from this conversation only, do not call any tool. One short sentence.",
    )
    .await;
    let said = answer(&app);
    assert!(!said.is_empty(), "the API accepted the repaired projection");
    println!("  excluded -> {}", said.trim().replace('\n', " "));

    // ---- three presses: back where it started, and the content is on the wire again
    press(&mut app, KeyCode::Char(' ')).await;
    assert_eq!(app.kernel.items()[result].state, ContextState::Active);
    let request = app.kernel.preview_request().expect("a request");
    assert!(
        format!("{:?}", request.messages).contains("APRICOT"),
        "the content came back"
    );
}

/// The status line names the address, and the budget it reports is the projection's, corrected
/// by what the provider actually charged.
#[tokio::test]
async fn the_status_line_reports_a_real_endpoint_and_a_corrected_budget() {
    let _serial = SERIAL.lock().await;
    let (mut app, mut finished) = live!();

    // something substantial in the context: under 256 reported tokens the calibrating counter
    // refuses to learn, on the grounds that the percentage error of a tiny request is noise
    app.kernel.push(nachalnik::ContextItem::file(
        "notes.txt",
        "a line of perfectly ordinary notes. ".repeat(120),
    ));
    send(&mut app, &mut finished, "Say the single word: ready.").await;

    // wide enough for all of it: the address is there
    let status = status_line(&mut app, 140);
    assert!(
        status.contains("@ generativelanguage.googleapis.com"),
        "the address is on the screen: {status}"
    );
    assert!(
        status.contains("really"),
        "and so is the real figure: {status}"
    );

    // and where it does not fit, the address is what gives way rather than the figures
    let narrow = status_line(&mut app, 100);
    assert!(!narrow.contains('@'), "the address gave way: {narrow}");
    assert!(
        narrow.contains("really") && narrow.contains("F1"),
        "and the figures did not: {narrow}"
    );

    // the provider reported what the request really cost, and the counter has been told
    let budget = app.kernel.budget();
    let reported = budget
        .reported
        .and_then(|u| u.input_tokens)
        .expect("the provider reports usage");
    let calibration = app.kernel.counter().calibration().expect("it calibrates");
    assert!(calibration.observations >= 1, "{calibration:?}");
    println!(
        "  estimated {} · really {reported} · scale {:.3} after {} observation(s)",
        budget.used(),
        calibration.scale,
        calibration.observations
    );
    assert!(
        status.contains("really"),
        "and the screen says so: {status}"
    );
}

/// A long message wraps in the prompt and is sent whole - the wrapping is a drawing decision and
/// must not touch what goes on the wire.
#[tokio::test]
async fn a_wrapped_message_is_sent_as_one_line() {
    let _serial = SERIAL.lock().await;
    let (mut app, mut finished) = live!();

    let long = "Reply with exactly the word FINE and nothing else, no matter how long this \
                sentence turns out to be or how many times it wraps in the box it was typed into.";
    send(&mut app, &mut finished, long).await;

    let sent = app
        .kernel
        .items()
        .iter()
        .find(|i| matches!(i.kind, ContextKind::UserMessage))
        .expect("the message")
        .content
        .to_text()
        .into_owned();
    assert_eq!(sent, long, "wrapping is drawn, not typed into the message");
    assert!(
        matches!(app.kernel.state(), State::Idle | State::Finished { .. }),
        "and the turn ran"
    );
}

/// Isolates the provider from the terminal: a bare kernel, this crate's own provider, one tool.
#[tokio::test]
async fn this_crates_provider_can_do_a_tool_call() {
    let _serial = SERIAL.lock().await;
    let Some((app, _finished)) = live() else {
        eprintln!("no key; skipping");
        return;
    };
    let kernel = app.kernel.clone();
    kernel.push(nachalnik::ContextItem::user(
        "Call the secret tool, then tell me the code word.",
    ));

    let state = tokio::time::timeout(Duration::from_secs(45), kernel.turn())
        .await
        .expect("the turn should not hang")
        .expect("the turn should not error");
    println!("  state: {state:?}");
    println!("  said:  {}", answer(&app).trim().replace('\n', " "));
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
}

/// `/models` asks the endpoint what it serves, because the ids are the endpoint's own and
/// `/model` is otherwise a command you can only use if you already knew the answer.
#[tokio::test]
async fn models_lists_what_the_endpoint_actually_serves() {
    let _serial = SERIAL.lock().await;
    let (mut app, _finished) = live!();

    for c in "/models flash-lite".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
            .await;
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await;

    let screen = draw(&mut app);
    assert!(
        screen.contains("/model ID switches"),
        "it says what to do with the list: {screen}"
    );
    assert!(
        screen.contains("gemini-3.5-flash-lite"),
        "and the list has real ids on it: {screen}"
    );
    assert!(
        screen.contains("▸ gemini-3.5-flash-lite"),
        "with the one in use marked: {screen}"
    );
    assert!(
        !screen.contains("gemini-2.5-pro"),
        "and the filter kept the rest out: {screen}"
    );
}
