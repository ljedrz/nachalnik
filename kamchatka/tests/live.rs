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
//! This file exists for the two things that cannot answer. The first is whether the request the
//! keys produced is a request a real API accepts: the state cycle on the context tab rewrites the
//! messages - eliding a tool result replaces its content, excluding one takes the call down as
//! well - and "the projection is well formed" is a claim only a server can settle. The second is
//! whether the sentences these tools write are ones a model can act on, which is the last section
//! below; a scripted provider agrees with every refusal it is handed.
//!
//! note: three fixtures, three sections, and the environment says which endpoint each is pointed
//! at. `KAMCHATKA_BASE_URL` and `KAMCHATKA_TEST_MODEL` move the first and the last; the middle one
//! has `KAMCHATKA_GEMINI_*` of its own and skips without them, so a run pointed at some other
//! provider does not send that provider's key to Google. `KAMCHATKA_TEST_DEADLINE` is how long a
//! turn gets, and `KAMCHATKA_CONTEXT_LIMIT` has to be small for the compaction case to have
//! anything to do.

use std::{sync::Arc, time::Duration};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use kamchatka::{
    app::{App, Focus, Tab},
    provider::OpenAiCompatible,
    tools::{Careful, Limits, Subject},
    ui,
};
use nachalnik::{
    Block, BoxError, Capability, Config, Content, ContextItem, ContextKind, ContextState, Kernel,
    LinearProjector, OutputSink, Role, State, Tool, ToolCall, ToolOutput, ToolSpec, Verdict,
    async_trait,
};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

mod common;

/// A model small enough to be free and able to call a tool.
const DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";

/// The model this run is actually pointed at.
///
/// note: `/models` marks the one in use, so a test that filters the list has to filter for
/// whatever that is. Naming [`DEFAULT_MODEL`] instead passed only while nobody overrode it, and
/// then reported a different model on the list as a broken list.
fn model_in_use() -> String {
    std::env::var("KAMCHATKA_TEST_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned())
}

/// Where the requests go, which is Google's OpenAI-compatible shim unless told otherwise.
fn base_url() -> String {
    std::env::var("KAMCHATKA_BASE_URL")
        .unwrap_or_else(|_| "https://generativelanguage.googleapis.com/v1beta/openai".to_owned())
}

/// Just the host of it, which is what the status line has room to draw.
fn host() -> String {
    let url = base_url();
    let after_scheme = url.split_once("://").map_or(&*url, |(_, rest)| rest);

    after_scheme
        .split('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

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
    let base = base_url();
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

    Some((
        App::new(kernel, policy, provider, Limits::default(), outcomes),
        finished,
    ))
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
    // a turn that stopped to ask something is still open, and the app swallows anything typed
    // into it on purpose - the message would land between a call and its result. Waiting on an
    // outcome that cannot come reports the model as slow ninety seconds later, which is a
    // description of neither the cause nor the fix
    assert!(
        !matches!(app.kernel.state(), State::Deciding { .. }),
        "a permission prompt is open, so {line:?} would go nowhere: allow what the tool needs"
    );

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

    let outcome = tokio::time::timeout(deadline(), finished.recv())
        .await
        .expect("the turn should have finished")
        .expect("the channel outlives the turn");
    app.on_outcome(outcome);
}

/// How long a turn is given before the test calls the model too slow.
///
/// note: longer than the scripted suite's, because a real model is on the other end, and settable
/// because how much longer is a property of the endpoint rather than of anything here. Ninety
/// seconds is ample against a paid provider and not always enough against a free tier: one run
/// took eight minutes for the suite and reported two of its own timeouts as failures of the
/// program.
fn deadline() -> Duration {
    Duration::from_secs(
        std::env::var("KAMCHATKA_TEST_DEADLINE")
            .ok()
            .and_then(|it| it.parse().ok())
            .unwrap_or(90),
    )
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
    //
    // note: the host is read back out of the environment rather than named here. Naming Google's
    // passed only while nobody set `KAMCHATKA_BASE_URL` - which the doc at the top of this file
    // tells them to - and then reported a correctly drawn OpenRouter address as a missing one
    let status = status_line(&mut app, 140);
    assert!(
        status.contains(&format!("@ {}", host())),
        "the address is on the screen: {status}"
    );
    assert!(
        status.contains("really"),
        "and so is the real figure: {status}"
    );

    // and where it does not fit, the address is what gives way rather than the figures
    //
    // note: "gave way" rather than "went", because how far it has to give depends on how long
    // this endpoint's host is. `generativelanguage.googleapis.com` is dropped outright at this
    // width and `openrouter.ai` is merely shortened, and a test that demanded the first reported
    // the correct handling of the second as a failure. What must hold either way is the ladder's
    // whole point: the figures and the key hint are still on the line. `edges.rs` pins the rungs
    // themselves, at widths it chooses rather than widths a provider's name decides
    let narrow = status_line(&mut app, 100);
    assert!(
        !narrow.contains(&format!("@ {}", host())),
        "the address gave way: {narrow}"
    );
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

    let model = model_in_use();
    for c in format!("/models {model}").chars() {
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
        screen.contains(&model),
        "and the list has real ids on it: {screen}"
    );
    assert!(
        screen.contains(&format!("▸ {model}")),
        "with the one in use marked: {screen}"
    );
    assert!(
        !screen.contains("gemini-2.5-pro"),
        "and the filter kept the rest out: {screen}"
    );
}

// --------------------------------------------------------------- the dialect that keeps an order

/// The same terminal, talking to Google's own API rather than the OpenAI-compatible shim.
///
/// note: its own environment variables, because the suite above is usually pointed at the shim
/// through `KAMCHATKA_BASE_URL` and this one must not follow it there - the whole subject is what
/// the shim cannot say. `KAMCHATKA_GEMINI_API_KEY` is one of them, so that a run pointed at some
/// other provider skips these rather than sending that provider's key to Google.
fn gemini() -> Option<(
    App,
    Arc<Kernel>,
    tokio::sync::mpsc::UnboundedReceiver<kamchatka::app::Outcome>,
)> {
    let base = std::env::var("KAMCHATKA_GEMINI_BASE_URL")
        .unwrap_or_else(|_| kamchatka::gemini::DEFAULT_BASE_URL.to_owned());
    // a key belongs to the endpoint it was given for. The one above is borrowed only when it was
    // plausibly Google's - an unset base URL, or one pointing there - because otherwise this sent
    // an OpenRouter key to `generativelanguage.googleapis.com` and reported the 400 that came back
    // as three broken tests about turn order
    let key = std::env::var("KAMCHATKA_GEMINI_API_KEY")
        .or_else(|_| match base_url().contains("googleapis.com") {
            true => {
                std::env::var("KAMCHATKA_API_KEY").or_else(|_| std::env::var("NACHALNIK_API_KEY"))
            }
            false => Err(std::env::VarError::NotPresent),
        })
        .ok()?;
    let model =
        std::env::var("KAMCHATKA_GEMINI_MODEL").unwrap_or_else(|_| "gemini-3.6-flash".to_owned());

    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(kamchatka::gemini::Gemini::new(model, base, key));
    kernel.set_provider(provider.clone());
    // what `--gemini` wires up: this dialect's turn *is* an order, so the projector sends one
    kernel.set_projector(Arc::new(LinearProjector {
        send_blocks: true,
        ..Default::default()
    }));

    let policy = Arc::new(Careful::new());
    policy.set(&Subject::Capability(Capability::Read), Verdict::Allow);
    // both of them: `install` offers `introspect` and `amend`, and a model that takes the second
    // offer used to stop the turn to ask. The turn then sat in `Deciding` with the prompt open,
    // the next message was swallowed the way the app swallows anything typed at one, and the
    // wire format this file exists to check never got its second request
    for capability in ["introspect", "amend"] {
        policy.set(
            &Subject::Capability(Capability::Custom(capability.into())),
            Verdict::Allow,
        );
    }
    kernel.set_policy(policy.clone());
    kernel.add_tool(Arc::new(Secret));
    let introspect = kamchatka::introspect::install(&kernel, Limits::default());

    let (outcomes, finished) = tokio::sync::mpsc::unbounded_channel();

    Some((
        App::new(kernel, policy, provider, Limits::default(), outcomes),
        introspect,
        finished,
    ))
}

macro_rules! gemini {
    () => {
        match gemini() {
            Some(fixtures) => fixtures,
            None => {
                eprintln!("no key in the environment; skipping");
                return;
            }
        }
    };
}

/// The assistant turn a run produced.
fn turn(app: &App) -> Arc<nachalnik::ContextItem> {
    app.kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
        .expect("a turn was recorded")
}

#[tokio::test]
async fn a_real_turn_is_recorded_in_the_order_it_was_produced() {
    let _serial = SERIAL.lock().await;
    let (mut app, _introspect, mut finished) = gemini!();

    send(
        &mut app,
        &mut finished,
        "Use the secret tool, then tell me the code word. Say a short sentence before you call it.",
    )
    .await;

    let turn = turn(&app);
    let blocks = turn
        .content
        .as_blocks()
        .unwrap_or_else(|| panic!("recorded as an order, not as {:?}", turn.content));

    // whatever the model chose to do, it is on the record in the order it did it, and the parts
    // are the parts this API actually reports rather than three slots something rearranged it into
    let names: Vec<_> = blocks.iter().map(Block::name).collect();
    assert!(!names.is_empty(), "{names:?}");
    assert!(
        names.contains(&"call"),
        "it was asked to use the tool: {names:?}"
    );
    // the conventional slots are empty, so nothing holds a second account of the same turn
    let ContextKind::AssistantMessage {
        tool_calls,
        reasoning,
    } = &turn.kind
    else {
        unreachable!()
    };
    assert!(tool_calls.is_empty() && reasoning.is_none());

    // the calls were found, gated, run and answered - the ordinary loop, over a turn whose calls
    // live somewhere it has never looked before
    assert_eq!(turn.calls().count(), 1, "{names:?}");
    assert!(
        answer(&app).to_lowercase().contains("apricot"),
        "it read the result: {}",
        answer(&app)
    );

    // and the signature really did arrive on the part it belongs to, which is the thing the next
    // request is rejected over
    assert!(
        blocks.iter().any(|block| !block.extra().is_null()),
        "this model signs what it produces: {blocks:?}"
    );

    // note: whether the turn also *thought* out loud is not asserted here, and deliberately.
    // Thought summaries are asked for and not promised: measured against this model, the same
    // prompt answers `[thought, functionCall]` on one run and `[functionCall]` on the next. That
    // the provider reports them when they come is `tests/gemini.rs`, over a recorded stream,
    // where it is a fact rather than a coin toss - and the test below is the live half of it
    eprintln!("recorded as {names:?}");
}

#[tokio::test]
async fn a_real_model_thinks_out_loud_between_what_it_says_and_what_it_asks_for() {
    let _serial = SERIAL.lock().await;
    let (mut app, _introspect, mut finished) = gemini!();

    // the claim the whole exercise rests on: one turn holding thinking *and* speech *and* a call,
    // in the order the model produced them. Whether any one turn thinks aloud is the model's
    // business, so this asks a few times and skips rather than failing - what is under test is
    // whether the order survives when it happens, not whether it happens
    for attempt in 1..=3 {
        send(
            &mut app,
            &mut finished,
            "A farmer has 17 sheep and all but 9 die. Work it out, then call the secret tool, \
             then give me both answers.",
        )
        .await;

        let ordered = app.kernel.items().into_iter().rev().find(|item| {
            item.content
                .as_blocks()
                .is_some_and(|blocks| blocks.len() > 1)
        });
        let Some(turn) = ordered else {
            continue;
        };
        let blocks = turn.content.as_blocks().unwrap_or_default();
        let names: Vec<_> = blocks.iter().map(Block::name).collect();
        eprintln!("attempt {attempt}: {names:?}");

        if names.contains(&"reasoning") {
            // more than one kind of thing in one turn, which is the shape three slots cannot hold
            assert!(names.len() > 1, "{names:?}");
            assert!(
                turn.thinking().next().is_some(),
                "and `thinking()` finds it where it is actually kept"
            );
            return;
        }
    }

    eprintln!("skipped: the model returned no thought summary in three turns");
}

#[tokio::test]
async fn an_ordered_turn_goes_back_and_is_accepted() {
    let _serial = SERIAL.lock().await;
    let (mut app, _introspect, mut finished) = gemini!();

    send(&mut app, &mut finished, "Use the secret tool.").await;
    // the second request carries the first turn back, signatures and all. This API answers
    // `400 Function call is missing a thought_signature` when one is dropped, so a turn that
    // survives being recorded and reprojected is the only way to find out that it did
    send(&mut app, &mut finished, "Say the code word once more.").await;

    assert!(
        answer(&app).to_lowercase().contains("apricot"),
        "{}",
        answer(&app)
    );
    assert!(matches!(
        app.kernel.state(),
        State::Idle | State::Finished { .. }
    ));
}

#[tokio::test]
async fn the_introspection_tools_read_an_ordered_turn() {
    let _serial = SERIAL.lock().await;
    let (mut app, _introspect, mut finished) = gemini!();

    send(
        &mut app,
        &mut finished,
        "Use the secret tool, then tell me the code word.",
    )
    .await;
    let turn = turn(&app);

    // `introspect` is the reason the two of these were built together: an agent that can read its
    // own context is only worth having if what it reads is what really happened, and until there
    // was a provider that reported an order there was no order in there to read
    let tool = app.kernel.tool("introspect").expect("installed");
    let read = tool
        .invoke(
            &ToolCall::new(
                "c1",
                "introspect",
                json!({ "action": "look", "ids": [turn.id.0] }),
            ),
            OutputSink::disconnected(),
        )
        .await
        .expect("the tool answered");
    let said = read.content.to_text();

    assert!(said.contains("block(s), in order"), "{said}");
    // read out by kind, with the ones that came signed marked - none of which a turn flattened
    // into three slots could have shown, because by then the order is gone and the signature with
    // it
    assert!(said.contains("call"), "{said}");
    assert!(said.contains("(signed)"), "{said}");

    // and the listing marks it as a turn with an order, rather than reporting a reasoning model
    // as having thought nothing and asked for nothing
    let listed = tool
        .invoke(
            &ToolCall::new("c2", "introspect", json!({ "action": "look" })),
            OutputSink::disconnected(),
        )
        .await
        .expect("the tool answered")
        .content
        .to_text()
        .into_owned();
    assert!(listed.contains("ordered block(s)"), "{listed}");
    assert!(listed.contains("call(s)]"), "{listed}");

    // and the figures it would budget against are the provider's own, not a guess nobody checked
    let budget = tool
        .invoke(
            &ToolCall::new("c3", "introspect", json!({ "action": "budget" })),
            OutputSink::disconnected(),
        )
        .await
        .expect("the tool answered")
        .content
        .to_text()
        .into_owned();
    assert!(budget.contains("the next request is"), "{budget}");
    assert!(
        budget.contains("as the provider counted it"),
        "a real request has been charged for by now: {budget}"
    );
    assert!(budget.contains("most expensive item(s)"), "{budget}");
}

// ------------------------------------------- the tools, and what a model does with what they say

/// The same terminal with the real file tools behind it, in a directory of its own.
///
/// note: a third fixture, because a third question. The suite at the top asks whether the request
/// a keystroke produced is one a server accepts; this one asks whether the *sentences these tools
/// write* are ones a model can act on. A refusal, a shortened result and a permission question are
/// all text aimed at a reader nobody here can interview, and the only way to find out that the
/// reader loops on it is to watch one do so - which is how four of the notes in this section got
/// written.
fn agent(
    dir: &std::path::Path,
) -> Option<(
    App,
    Limits,
    tokio::sync::mpsc::UnboundedReceiver<kamchatka::app::Outcome>,
)> {
    let key = std::env::var("KAMCHATKA_API_KEY")
        .or_else(|_| std::env::var("NACHALNIK_API_KEY"))
        .ok()?;

    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(OpenAiCompatible::new(model_in_use(), base_url(), key));
    kernel.set_provider(provider.clone());

    let policy = Arc::new(Careful::new());
    for capability in [
        Capability::Read,
        Capability::Write,
        Capability::Edit,
        Capability::Shell,
    ] {
        policy.set(&Subject::Capability(capability), Verdict::Allow);
    }
    kernel.set_policy(policy.clone());

    let limits = Limits::new();
    let reach = kamchatka::sandbox::Reach {
        workdir: dir.to_path_buf(),
        extra: Vec::new(),
        readable: Vec::new(),
        // on, so that a relative path is resolved against `workdir` the way it is in the program,
        // where `workdir` *is* the process directory. Off, `Reach` hands back what it was given
        // and a relative path goes to the process directory instead - which in a test run is the
        // repository, so every file the test had just written came back absent
        confined: true,
    };
    for tool in kamchatka::tools::builtin(
        kamchatka::tools::Shell {
            policy: policy.clone(),
            workdir: reach.workdir.clone(),
            extra: Vec::new(),
            readable: Vec::new(),
            // no Landlock: what is under test is what the tools say, and a session that cannot
            // start a confined child would report that as every one of these failing
            confiner: None,
            limits: limits.clone(),
        },
        reach,
        limits.clone(),
    ) {
        kernel.add_tool(tool);
    }

    let (outcomes, finished) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(kernel, policy, provider, limits.clone(), outcomes);
    app.confinement = kamchatka::sandbox::Confinement::Unsupported;

    Some((app, limits, finished))
}

/// The same, with `introspect` and `amend` offered as well.
fn introspecting(
    dir: &std::path::Path,
    ask_about_amend: bool,
) -> Option<(
    App,
    Limits,
    tokio::sync::mpsc::UnboundedReceiver<kamchatka::app::Outcome>,
)> {
    let (mut app, limits, finished) = agent(dir)?;
    for capability in ["introspect", "amend"] {
        let verdict = match ask_about_amend && capability == "amend" {
            true => Verdict::Ask,
            false => Verdict::Allow,
        };
        app.policy.set(
            &Subject::Capability(Capability::Custom(capability.into())),
            verdict,
        );
    }
    app.introspect = Some(kamchatka::introspect::install(&app.kernel, limits.clone()));

    Some((app, limits, finished))
}

macro_rules! agent {
    ($dir:expr) => {
        match agent($dir) {
            Some(it) => it,
            None => {
                eprintln!("no key in the environment; skipping");
                return;
            }
        }
    };
}

macro_rules! introspecting {
    ($dir:expr, $ask:expr) => {
        match introspecting($dir, $ask) {
            Some(it) => it,
            None => {
                eprintln!("no key in the environment; skipping");
                return;
            }
        }
    };
}

/// A directory of its own, so that a `read` cannot wander into the repository.
fn workdir(name: &str) -> std::path::PathBuf {
    // the process id in the name, so that two models being run against this at once do not each
    // clear the other's files out from under it
    let dir = common::scratch(&format!("live-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory");

    // resolved, because `Reach` canonicalises what it is handed and compares: on a machine whose
    // temporary directory is a symlink, an uncanonicalised `workdir` reaches nothing
    dir.canonicalize().expect("it exists")
}

/// Types a line and presses enter, without waiting for anything.
async fn type_line(app: &mut App, line: &str) {
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
}

/// Sends a slash command, which produces no turn to wait for.
async fn command(app: &mut App, line: &str) {
    app.show(Tab::Chat);
    type_line(app, line).await;
}

/// Every tool result in the context, as text.
fn results(app: &App) -> Vec<String> {
    app.kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .collect()
}

/// The screen with the line breaks taken out, for asserting on text that wrapped.
fn flat(app: &mut App) -> String {
    draw(app).replace('\n', " ")
}

// -------------------------------------------------------------- what the model is told about `~`

/// A `~` comes back as a sentence about `~`, never as a missing file - which is the whole point
/// of refusing it in words rather than letting it join onto the working directory and fail.
///
/// note: what a given model then *does* is printed rather than asserted. It is worth watching -
/// three of the four wording changes in this area were written after reading it - but a test that
/// failed when a model chose to explain the refusal to the user instead of acting on it would be
/// a test of the model, and this suite cannot fix one of those.
#[tokio::test]
async fn a_tilde_comes_back_as_a_sentence_about_tildes() {
    let _serial = SERIAL.lock().await;
    let dir = workdir("tilde");
    std::fs::write(dir.join("notes.txt"), "the first line says PELICAN\n").expect("a file");
    let (mut app, _limits, mut finished) = agent!(&dir);

    send(
        &mut app,
        &mut finished,
        "Read the file `~/notes.txt`. Use exactly that path, with the tilde, for your first \
         read call. The file is in your working directory. Then tell me the word the first \
         line says.",
    )
    .await;

    let seen = results(&app);
    println!("  tool results:");
    for result in &seen {
        println!("    {}", result.lines().next().unwrap_or_default());
    }
    let refused: Vec<_> = seen
        .iter()
        .filter(|r| r.contains("`~` is not expanded here"))
        .collect();
    assert!(
        !refused.is_empty(),
        "the model was told to use one and something answered it: {seen:?}"
    );
    // the `access(2)` trap in its second form, and the thing this must never do again
    assert!(
        !seen
            .iter()
            .any(|r| r.starts_with('~') && r.contains("No such file")),
        "a tilde came back as a missing file: {seen:?}"
    );

    println!(
        "  refused {} time(s); recovered: {}",
        refused.len(),
        answer(&app).to_uppercase().contains("PELICAN")
    );
}

// ----------------------------------------------------------------------- a limit, and raising it

/// A shortened result says so where the model reads it, the whole is kept, and `/limit` changes
/// what the next call is cut at.
#[tokio::test]
async fn a_shortened_read_is_reported_and_the_limit_can_be_raised() {
    let _serial = SERIAL.lock().await;
    let dir = workdir("limit");
    let mut big = String::from("FIRST LINE: OSPREY\n");
    for i in 0..200 {
        big.push_str(&format!(
            "line {i}: routine filler of no interest at all.\n"
        ));
    }
    big.push_str("LAST LINE: PELICAN\n");
    std::fs::write(dir.join("big.txt"), &big).expect("a file");

    let (mut app, limits, mut finished) = agent!(&dir);
    limits.set("read", 600);

    send(
        &mut app,
        &mut finished,
        "Read the file big.txt with the read tool. Tell me the very last line of the file, and \
         say plainly whether you received the whole file or only part of it.",
    )
    .await;

    let said = answer(&app);
    println!("  with read cut at 600 bytes, it said: {said}");
    let short = app
        .kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .expect("a result");
    println!("  because: {:?}", short.included_because);
    assert!(
        short
            .included_because
            .as_deref()
            .unwrap_or_default()
            .contains("output limit"),
        "the fact is on the item the model was shown: {short:?}"
    );
    assert!(
        app.kernel
            .items()
            .iter()
            .any(|item| matches!(item.state, ContextState::Archived)),
        "and the whole of it was kept"
    );

    // raising it does not recover that result, and does not need to: this is for the next call
    command(&mut app, "/limit read 64000").await;
    assert!(
        flat(&mut app).contains("read"),
        "the command said something: {}",
        flat(&mut app)
    );
    assert_eq!(limits.of("read"), Some(64_000));

    send(
        &mut app,
        &mut finished,
        "Read big.txt again now, and tell me the very last line.",
    )
    .await;
    println!("  with read cut at 64,000 bytes, it said: {}", answer(&app));
    // asserted on the result rather than on the answer, because whether the model bothers to read
    // it again is the model's business - one asked the person for a way to get the tail instead,
    // which is a fair thing to do and not a fact about the limit
    let whole = results(&app)
        .into_iter()
        .rev()
        .find(|result| result.contains("LAST LINE: PELICAN"));
    match whole {
        Some(_) => println!("  and the read that followed came back whole"),
        None => println!("  the model did not read it again, so nothing was cut this time either"),
    }
    assert!(
        !results(&app)
            .iter()
            .skip_while(|r| !r.contains("truncated"))
            .skip(1)
            .any(|r| r.contains("truncated")),
        "nothing was cut after the limit was raised: {:?}",
        results(&app)
    );
}

// ------------------------------------------------- an item the projector repairs away, on screen

/// The whole of a shortened output, put back beside the copy the model was shown, is `Active` and
/// not in the request. The pane has to say that rather than charge it `0`, and the request that
/// comes out of it has to be one a real API still accepts.
#[tokio::test]
async fn the_whole_of_a_cut_output_put_back_is_shown_as_left_out() {
    let _serial = SERIAL.lock().await;
    let dir = workdir("going");
    std::fs::write(
        dir.join("big.txt"),
        "OSPREY\n".repeat(400) + "LAST LINE: PELICAN\n",
    )
    .expect("a file");

    let (mut app, limits, mut finished) = agent!(&dir);
    limits.set("read", 600);
    send(
        &mut app,
        &mut finished,
        "Read big.txt with the read tool and say how many lines you got.",
    )
    .await;

    let archived = app
        .kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.state, ContextState::Archived))
        .expect("the whole was kept");
    app.kernel
        .set_state([archived.id], ContextState::Active, None);

    // two results now answer one call, and the projector drops one of them
    let going = app.going();
    let dropped: Vec<_> = app
        .kernel
        .items()
        .into_iter()
        .filter(|item| item.state.sends_content() && !going.sends_content(item))
        .collect();
    println!("  repaired away: {} item(s)", dropped.len());
    for item in &dropped {
        println!(
            "    [{}] {} · {:?}",
            item.id,
            item.state,
            going.left_out.get(&item.id)
        );
    }
    assert_eq!(dropped.len(), 1, "exactly one of the pair is dropped");
    assert!(
        going.left_out.contains_key(&dropped[0].id),
        "and the pane has the projector's reason for it"
    );

    app.show(Tab::Context);
    let screen = flat(&mut app);
    assert!(
        screen.contains("already has a result") || screen.contains("second result"),
        "which is what the row says: {screen}"
    );

    // and the request that shape produces is still one the endpoint accepts
    send(&mut app, &mut finished, "Say the single word: ready.").await;
    assert!(
        matches!(app.kernel.state(), State::Finished { .. }),
        "{:?}",
        app.kernel.state()
    );
}

// ----------------------------------------------------------- the question names what it is about

/// The permission question for `amend` says which item it would change.
#[tokio::test]
async fn the_question_about_a_real_amend_names_the_item() {
    let _serial = SERIAL.lock().await;
    let dir = workdir("about");
    let (mut app, _limits, mut finished) = introspecting!(&dir, true);
    app.kernel.push(
        ContextItem::file(
            "secrets.txt",
            "a file of no interest whatsoever.\n".repeat(20),
        )
        .because("named on the command line"),
    );

    // the turn stops at the question rather than finishing, so this cannot wait on an outcome
    type_line(
        &mut app,
        "First use the introspect tool with action `look` to see your context. Then use the \
         amend tool once to elide the item called secrets.txt, naming it by its number in \
         `ids`, with any reason you like. Do nothing else.",
    )
    .await;
    let asked = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if let Some(request) = app.kernel.pending_permissions().into_iter().next() {
                return Some(request);
            }
            if let Ok(outcome) = finished.try_recv() {
                app.on_outcome(outcome);
                if app.kernel.pending_permissions().is_empty() {
                    return None;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("something happened");

    let Some(request) = asked else {
        panic!(
            "the model never called amend; it said: {}",
            answer(&app).chars().take(400).collect::<String>()
        );
    };
    let about = app.about(&request);
    println!("  args: {}", request.args);
    println!("  about: {about:?}");
    assert!(!about.is_empty(), "the question names the items");
    assert!(
        about[0].contains("secrets.txt"),
        "by what the row calls them: {about:?}"
    );

    let screen = flat(&mut app);
    assert!(
        screen.contains("secrets.txt"),
        "and it is on the screen with the question: {screen}"
    );
}

// ----------------------------------------------------------------- five moves, five action names

/// Each move is an action named for what it leaves behind, so a model that reads the schema can
/// ask for one without being told twice that `restore` is not a thing.
#[tokio::test]
async fn a_real_model_asks_for_the_moves_by_name() {
    let _serial = SERIAL.lock().await;
    let dir = workdir("actions");
    let (mut app, _limits, mut finished) = introspecting!(&dir, false);
    app.kernel.push(ContextItem::file(
        "notes.txt",
        "PELICAN is the code word.\n",
    ));

    send(
        &mut app,
        &mut finished,
        "Using the amend tool, do exactly two things: first pin the context item called \
         notes.txt, then write a note that says the code word is PELICAN. Then stop.",
    )
    .await;

    for result in results(&app) {
        println!("    amend said: {}", result.replace('\n', " · "));
    }
    let refused: Vec<String> = results(&app)
        .into_iter()
        .filter(|r| r.contains("is not one of") || r.contains("unknown"))
        .collect();
    assert!(
        refused.is_empty(),
        "nothing was refused as unknown: {refused:?}"
    );

    let items = app.kernel.items();
    let pinned = items
        .iter()
        .any(|i| i.label.contains("notes.txt") && matches!(i.state, ContextState::Pinned));
    let noted = items
        .iter()
        .find(|i| i.content.to_text().to_uppercase().contains("PELICAN") && i.id.0 > 1);
    println!("  notes.txt pinned: {pinned}");
    println!("  a note was written: {}", noted.is_some());
    assert!(pinned || noted.is_some(), "at least one of the two landed");
}

// --------------------------------------------------------------- compaction against a real limit

/// The compactor fires against a real budget, and what it leaves is a request the endpoint takes.
///
/// note: needs `KAMCHATKA_CONTEXT_LIMIT` set small, because a provider that reports 262,144 will
/// not reach a threshold inside a test.
#[tokio::test]
async fn compaction_under_a_real_limit_leaves_a_request_the_endpoint_accepts() {
    let _serial = SERIAL.lock().await;
    if std::env::var("KAMCHATKA_CONTEXT_LIMIT").is_err() {
        eprintln!("no context limit set; skipping");
        return;
    }
    let dir = workdir("trim");
    std::fs::write(dir.join("big.txt"), "OSPREY is a bird.\n".repeat(600)).expect("a file");

    let (mut app, _limits, mut finished) = agent!(&dir);
    app.kernel
        .set_compactor(Some(Arc::new(kamchatka::tools::Trim {
            threshold: 0.5,
            target: 0.3,
        })));

    send(
        &mut app,
        &mut finished,
        "Read big.txt with the read tool, then say the single word: ready.",
    )
    .await;
    send(
        &mut app,
        &mut finished,
        "Read big.txt again with the read tool, then say the single word: ready.",
    )
    .await;

    let budget = app.kernel.budget();
    println!(
        "  {} of {:?} used, {:.0}%",
        budget.used(),
        budget.limit,
        budget.fraction_used().unwrap_or_default() * 100.0
    );
    let elided: Vec<_> = app
        .kernel
        .items()
        .into_iter()
        .filter(|item| item.state.is_elided())
        .collect();
    for item in &elided {
        println!("    elided [{}] {} · {:?}", item.id, item.label, item.note);
    }
    assert!(!elided.is_empty(), "the compactor fired");
    assert!(
        elided
            .iter()
            .all(|item| item.note.as_deref().unwrap_or_default().contains("%")),
        "and each marker carries the reason it gave"
    );

    // the shape it left has to be one a real endpoint still accepts
    send(&mut app, &mut finished, "Say the single word: ready.").await;
    assert!(
        matches!(app.kernel.state(), State::Finished { .. }),
        "{:?}",
        app.kernel.state()
    );

    // and `/budget` has to account for the same request the compactor just changed, with the
    // figures a real provider reported rather than the counter's guess
    command(&mut app, "/budget").await;
    let panel = flat(&mut app);
    println!(
        "  /budget: {}",
        panel
            .split("  ")
            .filter(|piece| piece.contains("token") || piece.contains("cach"))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    assert!(
        !panel.contains("0 items out"),
        "the report counts what it moved: {panel}"
    );
    assert!(
        panel.contains("really") || panel.contains("charged"),
        "and says what was really charged: {panel}"
    );
}
