//! Tests that draw the screen and read it back.
//!
//! note: A terminal program whose tests only checked its state would be testing the half that
//! nobody looks at. These render into a `TestBackend` and assert on the characters that come out,
//! which is the same thing a person sitting in front of it would be doing.
//!
//! note: The model is a `ScriptedProvider` and the tools do nothing, so none of this touches a
//! network or a file. What is under test is the wiring: that a key press reaches the kernel, that
//! the kernel's answer reaches the screen, and that what the screen says about the next request
//! is what the next request would actually contain.

use std::{sync::Arc, time::Duration};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kamchatka::{
    app::{App, Outcome, Speaker, Tab},
    provider::OpenAiCompatible,
    sandbox::Confinement,
    tools::{Careful, Subject},
    ui,
};
use nachalnik::{
    Capability, Config, ContextItem, ContextState, Delta, Event, Kernel, ModelInfo, ModelResponse,
    State, StopReason, Usage, Verdict,
    test::{ConstTool, ScriptedProvider, call},
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    style::{Color, Modifier},
};
use serde_json::json;
use tokio::sync::{broadcast::Receiver, mpsc::UnboundedReceiver};

/// A terminal, and everything needed to pretend somebody is sitting at it.
struct Harness {
    app: App,
    events: Receiver<Event>,
    finished: UnboundedReceiver<Outcome>,
}

impl Harness {
    /// Builds one around a model that will answer with exactly these.
    fn new(script: impl IntoIterator<Item = ModelResponse>) -> Self {
        Self::configured(script, Config::default())
    }

    /// The same, for a runtime configured some other way.
    fn configured(script: impl IntoIterator<Item = ModelResponse>, config: Config) -> Self {
        let kernel = Kernel::new(config);
        // subscribed first, the way the program does it: plugging things in is itself a handful of
        // events, and they belong on the trace with the rest
        let events = kernel.subscribe();

        let policy = Arc::new(Careful::new());
        kernel.set_provider(Arc::new(ScriptedProvider::new(script)));
        kernel.set_policy(policy.clone());
        let (outcomes, finished) = tokio::sync::mpsc::unbounded_channel();
        // the screen never talks to this one; it is here for `/model`, which the tests do not use
        let provider = Arc::new(OpenAiCompatible::new("scripted", "http://127.0.0.1:1", ""));

        Self {
            app: App::new(kernel, policy, provider, outcomes),
            events,
            finished,
        }
    }

    /// Answers the question on the screen, after the pause a question waits for.
    ///
    /// note: a permission question does not take a key as an answer while somebody is still
    /// typing - see `kamchatka::app::SETTLING`, and the live session that granted `shell` for good
    /// with the `a` of "what" - and a test presses its keys with nothing at all in between.
    async fn answer(&mut self, code: KeyCode) {
        tokio::time::sleep(kamchatka::app::SETTLING).await;
        self.press(code).await;
    }

    /// Presses a key.
    async fn press(&mut self, code: KeyCode) {
        self.app
            .on_key(KeyEvent::new(code, KeyModifiers::NONE))
            .await;
    }

    /// Presses a key with control held.
    async fn chord(&mut self, code: KeyCode) {
        self.app
            .on_key(KeyEvent::new(code, KeyModifiers::CONTROL))
            .await;
    }

    /// Types a line and sends it.
    async fn send(&mut self, line: &str) {
        for c in line.chars() {
            self.press(KeyCode::Char(c)).await;
        }
        self.press(KeyCode::Enter).await;
    }

    /// Feeds the app whatever the kernel has broadcast since last time.
    fn drain(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.app.on_event(event);
        }
    }

    /// Opens a tab, the way `alt+2` would.
    fn tab(&mut self, tab: Tab) {
        self.app.show(tab);
    }

    /// Waits for the running turn to stop, the way the real loop does.
    ///
    /// note: With a deadline, because the alternative is a test that hangs for ever when a key
    /// press went somewhere other than where it was expected to - which is exactly the mistake
    /// this file exists to catch.
    async fn settle(&mut self) {
        let outcome = tokio::time::timeout(Duration::from_secs(5), self.finished.recv())
            .await
            .expect("a turn should have been started, and should have finished")
            .expect("the channel outlives the turn");

        self.drain();
        self.app.on_outcome(outcome);
    }

    /// Draws, and returns what is on the screen.
    fn screen(&mut self) -> String {
        self.sized(100, 30)
    }

    /// Draws, and reports how the first character of `needle` is styled.
    fn style_of(&mut self, needle: &str) -> (Color, Modifier) {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &mut self.app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        let first = needle.chars().next().expect("a needle to look for");
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            if let Some(at) = row.find(needle) {
                // `find` gives bytes and the buffer is indexed in cells; every character this is
                // used on is one cell wide
                let x = row[..at].chars().count() as u16;
                let cell = &buffer[(x, y)];
                assert_eq!(
                    cell.symbol(),
                    first.to_string(),
                    "the cell under the needle"
                );

                return (cell.fg, cell.modifier);
            }
        }

        panic!("`{needle}` is not on the screen");
    }

    /// The screen with its line breaks undone: borders dropped, rows run together, runs of
    /// spaces collapsed to one.
    ///
    /// note: for asserting on a *sentence* rather than on a line. A phrase that sits comfortably
    /// on one row here wraps onto two on a machine whose temp directory is
    /// `/var/folders/df/djsxfhc17x95674wsm_g8s980000gn/T` - which is macOS, and which is how
    /// `is not a session` became `is not a` and then `session:` and a green test went red on
    /// somebody else's CI. Anything whose text can contain a path belongs here rather than in
    /// `screen`.
    fn flat(&mut self) -> String {
        let screen = self.screen();

        screen
            .replace(['│', '┌', '┐', '└', '┘', '─'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The same again, with the spaces taken out too.
    ///
    /// note: for finding a *token* rather than a sentence. `wrapped` splits a chunk that will not
    /// fit on a row of its own, so a path longer than the window is wide arrives as `…/jun` and
    /// `k.json` and no amount of running the rows together puts it back. This is the view that
    /// can still find it.
    fn packed(&mut self) -> String {
        self.flat().replace(' ', "")
    }

    /// Which of the three working dots is the lit one.
    fn dots(&mut self) -> usize {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &mut self.app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        let row = buffer.area.height - 1;
        let dots: Vec<u16> = (0..buffer.area.width)
            .filter(|x| buffer[(*x, row)].symbol() == "•")
            .collect();
        assert_eq!(dots.len(), 3, "three dots, or none at all");

        dots.iter()
            .position(|x| buffer[(*x, row)].fg == Color::Yellow)
            .expect("one of them is lit")
    }

    /// Draws at a given size.
    fn sized(&mut self, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| ui::draw(frame, &mut self.app))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[tokio::test]
async fn every_item_in_the_context_is_on_the_screen_with_what_it_costs() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file("src/parser.rs", "fn parse() {}").pinned());
    harness.app.kernel.push(ContextItem::user("what is this?"));
    harness.tab(Tab::Context);

    let screen = harness.screen();

    // the identifier, the label and the size, for each of them
    assert!(screen.contains("src/parser.rs"), "{screen}");
    assert!(screen.contains("user"), "{screen}");
    // the pin is visible as a pin, rather than only being true somewhere
    let pinned = screen
        .lines()
        .find(|line| line.contains("src/parser.rs"))
        .expect("the file is listed");
    assert!(pinned.contains('▪'), "{pinned}");
}

#[tokio::test]
async fn taking_an_item_out_of_the_next_request_takes_it_out_of_the_next_request() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file("secrets.txt", "hunter2"));
    harness.app.kernel.push(ContextItem::user("hello"));

    let before = harness.app.kernel.preview_request().unwrap();
    assert!(format!("{:?}", before.messages).contains("hunter2"));

    // to the context tab, pick the file, and walk it out one press at a time
    harness.tab(Tab::Context);
    harness.press(KeyCode::Home).await;

    // once: elided. What it says is out of the request, and a marker stands where it was
    harness.press(KeyCode::Char(' ')).await;
    let sent = format!(
        "{:?}",
        harness.app.kernel.preview_request().unwrap().messages
    );
    assert!(!sent.contains("hunter2"), "the request still carries it");
    assert!(
        sent.contains("removed from view by the user"),
        "and says where it went: {sent}"
    );
    assert_eq!(harness.app.kernel.items()[0].state, ContextState::Elided);

    // and the screen says so, rather than the item simply disappearing
    let screen = harness.screen();
    let row = screen
        .lines()
        .find(|line| line.contains("secrets.txt"))
        .expect("an elided item is still listed");
    assert!(row.contains('…'), "{row}");

    // twice: excluded, and now there is nothing of it in the request at all
    harness.press(KeyCode::Char(' ')).await;
    let sent = format!(
        "{:?}",
        harness.app.kernel.preview_request().unwrap().messages
    );
    assert!(
        !sent.contains("hunter2") && !sent.contains("removed from view"),
        "{sent}"
    );
    assert_eq!(
        harness.app.kernel.items()[0].state,
        ContextState::Excluded,
        "and the item is still there, in a state that says why"
    );
    let screen = harness.screen();
    let row = screen
        .lines()
        .find(|line| line.contains("secrets.txt"))
        .expect("an excluded item is still listed");
    assert!(row.contains('-'), "{row}");

    // three times: back where it started, on the same key
    harness.press(KeyCode::Char(' ')).await;
    let again = harness.app.kernel.preview_request().unwrap();
    assert!(format!("{:?}", again.messages).contains("hunter2"));
    assert_eq!(harness.app.kernel.items()[0].state, ContextState::Active);
}

#[tokio::test]
async fn an_answer_that_arrives_in_fragments_is_one_paragraph_on_the_screen() {
    let mut harness = Harness::new([ModelResponse::text("a whole sentence, eventually")]);

    harness.send("say something").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(screen.contains("> say something"), "{screen}");
    assert!(screen.contains("a whole sentence, eventually"), "{screen}");
}

#[tokio::test]
async fn a_tool_that_needs_permission_stops_the_turn_and_puts_the_question_on_the_screen() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({ "where": "here" }))]),
        ModelResponse::text("I could not, so I did not"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
    ));

    harness.send("dig somewhere").await;
    harness.settle().await;

    // the question, with the arguments it would run with
    let screen = harness.screen();
    assert!(screen.contains("a tool wants to run"), "{screen}");
    assert!(screen.contains("dig wants: shell"), "{screen}");
    assert!(screen.contains("\"where\""), "{screen}");

    // looking closer, and then coming back: the tool is still waiting, so the question has to
    // still be there. It was not, and there was no way left to answer it
    harness.answer(KeyCode::Char('i')).await;
    let inspected = harness.screen();
    assert!(inspected.contains("what it was asked to do"), "{inspected}");
    assert!(
        inspected.contains("\"capabilities\""),
        "the tool itself: {inspected}"
    );

    harness.press(KeyCode::Esc).await;
    let back = harness.screen();
    assert!(back.contains("dig wants: shell"), "{back}");
    assert!(back.contains("[y] once"), "{back}");

    // saying no answers the call rather than abandoning it: the model is told
    harness.answer(KeyCode::Char('n')).await;
    harness.settle().await;

    let denied = harness
        .app
        .kernel
        .items()
        .into_iter()
        .find(|item| item.label == "dig")
        .expect("the refusal is a tool result like any other");
    assert!(
        denied.content.to_text().contains("not permitted"),
        "{denied:?}"
    );
    assert!(harness.app.overlay.is_none());
}

#[tokio::test]
async fn saying_always_stops_the_question_being_asked_again() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::tool_calls(vec![call("c2", "dig", json!({}))]),
        ModelResponse::text("twice"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
    ));

    harness.send("dig twice").await;
    harness.settle().await;
    assert!(harness.app.overlay.is_some(), "the first one is a question");

    harness.answer(KeyCode::Char('a')).await;
    harness.settle().await;

    // the second call went through without stopping, and the policy says why - the prompt's
    // "always" and the permissions tab are the same table
    assert!(harness.app.overlay.is_none());
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::Shell)),
        nachalnik::Verdict::Allow
    );
    let results = harness
        .app
        .kernel
        .items()
        .into_iter()
        .filter(|item| item.label == "dig")
        .count();
    assert_eq!(results, 2);
}

#[tokio::test]
async fn an_answer_stopped_partway_is_shown_once_rather_than_twice() {
    let mut harness = Harness::new([]);

    // exactly what the loop sees when a streamed answer is stopped mid-sentence: fragments, then
    // the interrupt, then the shortened turn being recorded. The note in the middle used to make
    // the transcript look as though nothing had streamed, and the answer was printed again
    harness.app.on_event(Event::ModelRequested {
        model: ModelInfo::new("scripted", "scripted"),
        messages: 1,
        tools: 0,
        tokens: 4,
        items: Vec::new(),
        skipped: Vec::new(),
        repairs: Vec::new(),
    });
    harness.app.on_event(Event::ModelDelta {
        delta: Delta::Text("the beginning of a sentence".to_owned()),
    });
    harness.app.on_event(Event::Interrupted);

    let item = harness.app.kernel.push(ContextItem::assistant(
        "the beginning of a sentence",
        Vec::new(),
    ));
    harness.app.on_event(Event::ModelFinished {
        stop: StopReason::Other("interrupted".to_owned()),
        usage: None,
        tool_calls: Vec::new(),
        item,
    });

    let screen = harness.screen();
    assert_eq!(
        screen.matches("the beginning of a sentence").count(),
        1,
        "{screen}"
    );
    assert!(screen.contains("stopped"), "{screen}");
}

#[tokio::test]
async fn an_answer_from_a_provider_that_does_not_stream_still_appears() {
    let mut harness = Harness::new([]);

    // the same events, minus the fragments: nothing was drawn while it was arriving, so the
    // finished turn has to be read back off the item the kernel recorded
    harness.app.on_event(Event::ModelRequested {
        model: ModelInfo::new("scripted", "scripted"),
        messages: 1,
        tools: 0,
        tokens: 4,
        items: Vec::new(),
        skipped: Vec::new(),
        repairs: Vec::new(),
    });
    let item = harness.app.kernel.push(ContextItem::assistant(
        "all at once, at the end",
        Vec::new(),
    ));
    harness.app.on_event(Event::ModelFinished {
        stop: StopReason::EndTurn,
        usage: None,
        tool_calls: Vec::new(),
        item,
    });

    assert!(harness.screen().contains("all at once, at the end"));
}

#[tokio::test]
async fn the_budget_puts_the_estimate_beside_what_was_really_charged() {
    let mut harness = Harness::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(1_234),
            ..Default::default()
        }),
        ..ModelResponse::text("done")
    }]);
    harness.app.kernel.push(ContextItem::file(
        "haystack.txt",
        "a needle in it. ".repeat(200),
    ));

    harness.send("go").await;
    harness.settle().await;
    harness.send("/budget").await;

    let screen = harness.screen();
    // the estimate is not the truth, and the screen is not allowed to imply that it is
    assert!(screen.contains("the next request: ~"), "{screen}");
    assert!(
        screen.contains("really cost 1,234"),
        "the provider's own figure is missing: {screen}"
    );
    assert!(
        screen.contains("learned from 1 request"),
        "the correction it drew from the difference is missing: {screen}"
    );
}

#[tokio::test]
async fn what_a_tool_wants_to_write_is_shown_as_the_lines_it_would_write() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "scribble",
        json!({ "path": "greet.py", "content": "def main():\n    print(\"hi\")\n" }),
    )])]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("scribble", "done").with_capabilities([Capability::Write]),
    ));

    harness.send("write something").await;
    harness.settle().await;

    // this is the moment somebody decides, so the argument they have to read is put back into
    // the lines it is, rather than left as `\n` in the middle of a JSON string. The one-line
    // summary in the transcript is still a one-line summary, which is why this looks at how the
    // *question* renders rather than at the whole screen
    let screen = harness.screen();
    assert!(screen.contains("  def main():"), "{screen}");
    assert!(screen.contains("      print(\"hi\")"), "{screen}");
    assert!(
        screen.contains("path: greet.py"),
        "a short argument should read as a plain line, not as JSON: {screen}"
    );
    // and nothing is hidden by making it readable
    assert!(screen.contains("the exact JSON"), "{screen}");
}

#[tokio::test]
async fn several_repairs_at_once_are_one_line_rather_than_a_wall_of_them() {
    let mut harness = Harness::new([]);

    // one compaction pass can orphan a handful of calls, and a notice each would push the answer
    // off the screen to say one thing
    harness.app.on_event(Event::ModelRequested {
        model: ModelInfo::new("scripted", "scripted"),
        messages: 1,
        tools: 0,
        tokens: 4,
        items: Vec::new(),
        skipped: Vec::new(),
        repairs: vec![
            "dropped the call `a`".to_owned(),
            "dropped the call `b`".to_owned(),
            "dropped the call `c`".to_owned(),
        ],
    });

    let screen = harness.screen();
    assert_eq!(screen.matches("dropped the call").count(), 0, "{screen}");
    assert!(screen.contains("repaired in 3 places"), "{screen}");
    assert!(screen.contains("ctrl+p says where"), "{screen}");
}

#[tokio::test]
async fn a_models_markdown_is_shown_as_formatting_rather_than_as_punctuation() {
    // a raw literal, because what is under test is markdown and the escapes would be the first
    // thing to get it wrong
    const ANSWER: &str = r#"## What I found

The `median` function is **wrong** for even lengths:

```rust
let mid = sorted.len() / 2;
```

---

- `mean` is fine.
"#;

    let mut harness = Harness::new([ModelResponse::text(ANSWER)]);

    harness.send("look").await;
    harness.settle().await;

    let screen = harness.screen();
    // the punctuation of the format is not the message
    assert!(!screen.contains("##"), "{screen}");
    assert!(!screen.contains("**"), "{screen}");
    assert!(!screen.contains("```"), "{screen}");
    assert!(!screen.contains('`'), "{screen}");

    // ... but everything it was marking up is still there, and marked up
    assert!(screen.contains("What I found"), "{screen}");
    assert!(screen.contains("let mid = sorted.len() / 2;"), "{screen}");
    assert!(screen.contains("- mean is fine."), "{screen}");

    let (_, heading) = harness.style_of("What I found");
    assert!(heading.contains(Modifier::BOLD), "a heading should be bold");

    let (_, emphasis) = harness.style_of("wrong");
    assert!(emphasis.contains(Modifier::BOLD), "**bold** should be bold");

    let (code, _) = harness.style_of("median");
    assert_eq!(code, Color::Cyan, "`code` should be told apart from prose");

    // a horizontal rule is drawn rather than spelled
    assert!(!screen.contains("---"), "{screen}");
    assert!(screen.contains("────"), "{screen}");

    // a fenced block gets a rule down its left rather than a slab of background
    let fenced = screen
        .lines()
        .find(|line| line.contains("let mid"))
        .expect("the block is on the screen");
    assert!(fenced.trim_start().starts_with('│'), "{fenced}");
}

#[tokio::test]
async fn what_a_tool_said_is_shown_as_the_tool_said_it() {
    let mut harness = Harness::new([]);

    // markdown is what the *model* writes. A tool's output is bytes, and running it through a
    // renderer would be inventing structure that the tool did not put there
    harness.app.say(
        kamchatka::app::Speaker::Result,
        "**not bold** and `not code` and # not a heading",
    );

    let screen = harness.screen();
    assert!(screen.contains("**not bold**"), "{screen}");
    assert!(screen.contains("`not code`"), "{screen}");
    assert!(screen.contains("# not a heading"), "{screen}");
}

#[tokio::test]
async fn a_chatty_tool_does_not_wipe_out_the_trace() {
    let mut harness = Harness::new([]);

    harness.app.on_event(Event::ToolStarted {
        call: nachalnik::ToolCallId("c1".to_owned()),
        tool: "shell".to_owned(),
    });
    // `cat` of a thousand lines really did erase the whole trace, one `tool.output` at a time
    for _ in 0..900 {
        harness.app.on_event(Event::ToolOutput {
            call: nachalnik::ToolCallId("c1".to_owned()),
            tool: "shell".to_owned(),
            chunk: "a line of it\n".to_owned(),
        });
    }

    assert_eq!(
        harness.app.trace.len(),
        2,
        "the started, and one line counting the output up"
    );
    harness.tab(Tab::Trace);
    let screen = harness.screen();
    assert!(screen.contains("tool.started"), "{screen}");
    assert!(screen.contains("11,700 bytes so far"), "{screen}");
}

#[tokio::test]
async fn a_slash_is_a_command_and_everything_else_is_a_message() {
    let mut harness = Harness::new([]);

    harness.send("/policy").await;
    assert!(
        harness.app.kernel.items().is_empty(),
        "a command is not something the model is told about"
    );
    assert_eq!(harness.app.tab, Tab::Permissions);

    // it moved the focus to the tab it opened, so the prompt needs it back
    harness.tab(Tab::Chat);
    harness.send("/nonsense").await;
    assert!(harness.screen().contains("there is no `/nonsense`"));
}

#[tokio::test]
async fn what_the_screen_shows_of_the_next_request_is_the_next_request() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("the only thing in here"));

    harness
        .app
        .on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))
        .await;

    let screen = harness.screen();
    assert!(screen.contains("the only thing in here"), "{screen}");
    // it is the kernel's own rendering, not a description of it
    assert!(screen.contains("\"role\""), "{screen}");
}

#[tokio::test]
async fn a_panel_that_promises_the_bytes_does_not_reflow_the_spaces_out_of_them() {
    let mut harness = Harness::new([]);
    // a run of spaces on a line long enough that the panel has to fold it - which is exactly
    // where a wrapper that split on whitespace used to collapse every run into one. `/payload`
    // and `/raw` say they show the bytes, and a file's indentation is spaces in a row
    let padded = format!(
        "def f():{}return 1, and then {}",
        " ".repeat(8),
        "x ".repeat(40)
    );
    harness
        .app
        .kernel
        .push(ContextItem::file("indented.py", padded));

    harness.tab(Tab::Context);
    harness.press(KeyCode::Home).await;
    harness.press(KeyCode::Enter).await;

    let screen = harness.sized(60, 30);
    assert!(
        screen.contains("def f():        return 1,"),
        "the indentation was re-flowed away: {screen}"
    );
}

#[tokio::test]
async fn the_status_line_says_what_the_budget_is_measured_against() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::user("hello"));

    let screen = harness.screen();
    let status = screen.lines().last().expect("a status line");

    assert!(status.contains("idle"), "{status}");
    assert!(status.contains("scripted"), "{status}");
    // the estimate, marked as one, and the limit it is a percentage *of* - a bare percentage is
    // not something anybody can act on
    assert!(status.contains("~"), "{status}");
    assert!(status.contains("% (128k)"), "{status}");
}

#[tokio::test]
async fn the_trace_shows_the_events_the_session_log_is_made_of() {
    let mut harness = Harness::new([ModelResponse::text("done")]);

    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    let screen = harness.sized(120, 30);
    assert!(screen.contains("model.requested"), "{screen}");
    assert!(screen.contains("model.finished"), "{screen}");
    assert!(screen.contains("state.changed"), "{screen}");

    // nothing is cut off with an ellipsis in the middle of the part worth reading, which is what
    // the pane did while it was sharing forty columns with the context
    assert!(
        screen.contains("requesting → finished"),
        "a state change is truncated: {screen}"
    );
    assert!(
        screen.contains("/save keeps them all"),
        "the pane should say where the rest of them are: {screen}"
    );
}

#[tokio::test]
async fn each_tab_takes_the_whole_window_and_the_others_are_not_under_it() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.app.kernel.push(ContextItem::file("a.rs", "one"));
    harness.send("hello there").await;
    harness.settle().await;

    // the strip names all three wherever you are, so the others are findable
    for tab in Tab::ALL {
        harness.tab(tab);
        let screen = harness.screen();
        for name in Tab::ALL.map(Tab::name) {
            assert!(
                screen.contains(name),
                "{name} is missing from the strip: {screen}"
            );
        }
    }

    harness.tab(Tab::Chat);
    let chat = harness.screen();
    assert!(chat.contains("hello there"), "{chat}");
    assert!(!chat.contains("state.changed"), "{chat}");
    assert!(!chat.contains("user_message"), "{chat}");

    harness.tab(Tab::Context);
    let context = harness.screen();
    assert!(context.contains("a.rs"), "{context}");
    assert!(
        context.contains("user_message"),
        "the kinds are a column: {context}"
    );
    assert!(!context.contains("state.changed"), "{context}");

    harness.tab(Tab::Trace);
    let trace = harness.screen();
    assert!(trace.contains("state.changed"), "{trace}");
    assert!(!trace.contains("user_message"), "{trace}");
}

#[tokio::test]
async fn the_next_tab_is_one_keystroke_and_each_of_them_is_two() {
    let mut harness = Harness::new([]);

    harness
        .app
        .on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
        .await;
    assert_eq!(harness.app.tab, Tab::Context);

    harness
        .app
        .on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT))
        .await;
    assert_eq!(harness.app.tab, Tab::Chat);

    harness
        .app
        .on_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT))
        .await;
    assert_eq!(harness.app.tab, Tab::Trace);
}

#[tokio::test]
async fn an_item_that_is_not_going_into_the_request_says_why_where_it_is_listed() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::file("a.rs", "one"));

    harness.send("/prune files").await;
    harness.tab(Tab::Context);

    // "why is that out?" is a question about the thing you are looking at, so it is answered
    // on its row rather than only in the request preview
    let screen = harness.screen();
    assert!(screen.contains("excluded: pruned by `files`"), "{screen}");
}

#[tokio::test]
async fn a_command_that_names_items_by_selector_reports_what_it_matched() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::file("a.rs", "one"));
    harness.app.kernel.push(ContextItem::file("b.rs", "two"));

    harness.send("/prune files").await;

    assert!(harness.screen().contains("2 item(s) are now excluded"));
    assert!(
        harness
            .app
            .kernel
            .items()
            .iter()
            .all(|item| item.state == ContextState::Excluded)
    );

    // and the undo the runtime keeps is one keystroke away, from the context tab
    harness.tab(Tab::Context);
    harness.press(KeyCode::Char('u')).await;
    assert!(
        harness
            .app
            .kernel
            .items()
            .iter()
            .all(|item| item.state == ContextState::Active)
    );
}

#[tokio::test]
async fn the_help_lists_the_keys_that_exist() {
    let mut harness = Harness::new([]);

    harness.press(KeyCode::F(1)).await;
    let top = harness.sized(110, 40);
    assert!(top.contains("alt+1 / 2 / 3"), "{top}");
    assert!(top.contains("alt+enter"), "{top}");

    // every heading starts at the same column: the first one used to sit flush against the
    // border, because a `\` continuation after the opening quote had eaten its indent
    let column = |heading: &str| {
        top.lines()
            .find(|line| line.contains(heading))
            .unwrap_or_else(|| panic!("`{heading}` is in the help: {top}"))
            .find(heading)
            .expect("just found it")
    };
    assert_eq!(column("THE TABS"), column("ANYWHERE"), "{top}");

    // it is longer than a screenful, and says so, and scrolls
    assert!(
        top.contains(" of "),
        "the panel should count its own lines: {top}"
    );
    // ... and the commands are reachable from the top by scrolling, however long the help grows
    let mut rest = String::new();
    for _ in 0..8 {
        harness.press(KeyCode::PageDown).await;
        rest = harness.sized(110, 40);
        if rest.contains("/prune") {
            break;
        }
    }
    assert!(rest.contains("/prune"), "{rest}");

    // ... and every command is listed once. `/seams` was in there twice, and a test that could
    // only see one screenful at a time had no way to notice
    // the whole left column, not the first word: `/tools` and `/tools drop ID` are two entries
    // for one command and belong in here twice
    let commands: Vec<&str> = ui::HELP
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with('/'))
        .map(|line| line.split("  ").next().unwrap_or(line).trim_end())
        .collect();
    let mut seen = commands.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), commands.len(), "listed twice: {commands:?}");
    assert!(commands.contains(&"/seams"), "{commands:?}");

    // and every form `amend`'s schema names to a model is one the selector language really takes.
    // A schema that offered a form the parser refuses would be teaching a model to make a call
    // that comes back as an error, which is the one thing a description is there to prevent
    let offered = kamchatka::introspect::install(&harness.app.kernel);
    let amend = harness.app.kernel.tool("amend").expect("installed");
    let select = amend.spec().schema["properties"]["select"]["description"]
        .as_str()
        .expect("it says what it takes")
        .to_owned();
    drop(offered);

    for form in [
        "17",
        "all",
        "all:tool_results",
        "kind:assistant_message",
        "state:excluded",
        "tool:grep",
        "tool:grep:first",
        "tool:grep:latest",
        "source:mcp",
        "file:src/x.rs",
        "label:cargo test",
    ] {
        assert!(
            form.parse::<nachalnik::selectors::Selector>().is_ok(),
            "the schema offers `{form}` and the language refuses it"
        );
    }
    // named as forms rather than as examples: a model that read `tool:shell` as a literal asked
    // to prune it in a session with no shell in it
    for prefix in [
        "kind:<kind>",
        "state:<state>",
        "tool:<name>",
        "source:<name>",
        "file:<path>",
    ] {
        assert!(
            select.contains(prefix),
            "the schema does not name `{prefix}`: {select}"
        );
    }

    // any other key closes it
    harness.press(KeyCode::Esc).await;
    assert!(harness.app.overlay.is_none());
}

#[tokio::test]
async fn introspect_offers_the_two_tools_and_takes_them_away_again() {
    let mut harness = Harness::new([]);
    assert!(harness.app.kernel.tool_ids().is_empty());
    let before = harness.app.undecided();

    harness.send("/introspect").await;
    assert_eq!(harness.app.kernel.tool_ids(), ["amend", "introspect"]);
    assert!(harness.app.introspect.is_some());
    // the policy has two more subjects to ask about without being told anything, because the tab
    // reads what the registered tools declare. Two, not one: looking at your own context and
    // rewriting it are different questions, which is the whole reason there are two tools
    harness.tab(Tab::Permissions);
    assert_eq!(harness.app.undecided(), before + 2);
    let screen = harness.screen();
    assert!(
        screen.contains(&format!("{} more it will ask about", before + 2)),
        "{screen}"
    );

    harness.tab(Tab::Chat);
    harness.send("/introspect").await;
    assert!(harness.app.kernel.tool_ids().is_empty());
    // and the handle they reached the kernel through has gone with them
    assert!(harness.app.introspect.is_none());
}

#[tokio::test]
async fn a_turn_that_runs_out_of_requests_says_so_instead_of_looking_finished() {
    let mut harness = Harness::configured(
        [
            ModelResponse::tool_calls(vec![call("c1", "look", json!({}))]),
            ModelResponse::text("and there it was"),
        ],
        Config {
            // one request per turn, so that carrying on is somebody's decision rather than
            // something that happens
            max_requests_per_turn: Some(1),
            ..Default::default()
        },
    );
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("look", "nothing")));

    harness.send("look around").await;
    harness.settle().await;

    // the tool ran, and then the turn stopped without an answer; a screen that said nothing here
    // would look exactly like one where the model had finished
    let screen = harness.screen();
    assert!(screen.contains("paused after 1 requests"), "{screen}");
    assert!(screen.contains("/continue"), "{screen}");

    harness.send("/continue").await;
    harness.settle().await;
    assert!(harness.screen().contains("and there it was"));
}

#[tokio::test]
async fn stepping_stops_where_a_turn_walks_straight_through() {
    let mut harness = Harness::new([ModelResponse::tool_calls(vec![call(
        "c1",
        "look",
        json!({"at": "the state machine"}),
    )])]);
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("look", "nothing to see")));

    // one transition: the request goes, the model asks for a tool, and the kernel comes to rest
    // in `Ready` - which a whole turn passes through without ever being visible. The message has
    // to come with it, because sending one on its own runs the turn and leaves nothing to step
    harness.send("/step look around").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(screen.contains("step \u{2192} ready"), "{screen}");
    // and it says what is about to happen, which is the only reason to stand here
    assert!(screen.contains("look"), "{screen}");
    assert!(screen.contains("the state machine"), "{screen}");
    assert_eq!(
        harness.app.kernel.pending_calls().len(),
        1,
        "the call is decided and waiting, not run"
    );
    assert!(
        harness
            .app
            .kernel
            .items()
            .iter()
            .all(|item| !matches!(item.kind, nachalnik::ContextKind::ToolResult { .. })),
        "nothing has run yet, so there is no result"
    );

    // the next transition runs it
    harness.app.start_step();
    harness.settle().await;
    // a fresh snapshot: printing the one captured before the step would show `step -> ready` at
    // exactly the moment somebody needs to see why the tool did not run
    let after = harness.screen();
    assert!(after.contains("nothing to see"), "{after}");
}

#[tokio::test]
async fn editing_an_item_changes_what_the_model_reads_and_keeps_the_old_one() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file("notes.txt", "the wrong note"));
    harness.tab(Tab::Context);
    harness.press(KeyCode::Home).await;

    harness.press(KeyCode::Char('e')).await;
    // the prompt now holds what the item says, and says so
    assert!(
        harness.screen().contains("editing [1]"),
        "{}",
        harness.screen()
    );
    assert_eq!(harness.app.input.lines(), ["the wrong note"]);

    harness.send("s").await;

    // the request carries the edit, and only the edit: the item it replaced is superseded, so
    // the projector leaves it out
    let request = harness.app.kernel.preview_request().unwrap();
    let sent = format!("{:?}", request.messages);
    assert!(sent.contains("the wrong notes"), "{sent}");
    assert_eq!(
        sent.matches("the wrong note").count(),
        1,
        "both copies went into the request: {sent}"
    );

    // and the one it replaced is still there, in a state that says why
    let items = harness.app.kernel.items();
    assert_eq!(items[0].state, ContextState::Superseded);
    assert!(
        items[0].note.as_deref().unwrap_or_default().contains("2"),
        "the old item should name the one that replaced it: {:?}",
        items[0].note
    );
    let screen = harness.screen();
    assert!(
        screen.contains('~'),
        "superseded should be marked: {screen}"
    );
}

#[tokio::test]
async fn dropping_the_pending_calls_tells_the_model_rather_than_losing_them() {
    let mut harness = Harness::configured(
        [ModelResponse::tool_calls(vec![
            call("c1", "danger", json!({})),
            call("c2", "danger", json!({})),
        ])],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("danger", "ran").with_capabilities([Capability::Shell]),
    ));

    harness.send("do something rash").await;
    harness.settle().await;
    let asked = harness.screen();
    assert!(asked.contains("[d] drop them all"), "{asked}");

    harness.answer(KeyCode::Char('d')).await;
    harness.drain();

    assert!(
        harness.app.kernel.pending_permissions().is_empty(),
        "nothing should still be waiting"
    );
    // the model is told, rather than left waiting for calls that silently vanished
    let request = harness.app.kernel.preview_request().unwrap();
    let sent = format!("{:?}", request.messages);
    assert!(sent.contains("cancelled"), "{sent}");
    assert!(harness.screen().contains("2 call(s) dropped"));
}

#[tokio::test]
async fn a_tool_can_stop_being_offered_without_restarting() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("shell", "ran")));
    harness.app.kernel.push(ContextItem::user("hello"));

    assert!(
        harness
            .app
            .kernel
            .preview_request()
            .unwrap()
            .tools
            .iter()
            .any(|spec| spec.id == "shell")
    );

    harness.send("/tools drop shell").await;

    assert!(
        harness
            .app
            .kernel
            .preview_request()
            .unwrap()
            .tools
            .is_empty(),
        "the next request should not offer it"
    );
    assert!(harness.screen().contains("no longer offered"));
}

#[tokio::test]
async fn a_selector_with_nothing_to_select_teaches_the_language() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::user("hello"));

    harness.send("/prune").await;

    // an error saying the empty string is not a selector is true and useless; the grammar has
    // ten forms and this is where somebody goes looking for them
    let screen = harness.sized(110, 40);
    assert!(screen.contains("tool:grep:latest"), "{screen}");
    assert!(screen.contains("state:excluded"), "{screen}");
    // and the forms it advertises really are forms
    for form in ["all", "state:excluded", "tool:grep:latest", "17"] {
        assert!(
            form.parse::<nachalnik::selectors::Selector>().is_ok(),
            "the help offers `{form}`, which does not parse"
        );
    }
}

#[tokio::test]
async fn an_item_can_be_reached_by_the_number_it_is_shown_under() {
    let mut harness = Harness::new([]);
    for i in 0..12 {
        harness
            .app
            .kernel
            .push(ContextItem::user(format!("message {i}")));
    }
    harness.tab(Tab::Context);
    harness.press(KeyCode::Home).await;

    // the number in the first column is the one every note names and `/prune` takes, so it is
    // the one that should get you there
    harness.press(KeyCode::Char('9')).await;
    harness.press(KeyCode::Char('G')).await;

    assert_eq!(harness.app.kernel.items()[harness.app.selected].id.0, 9);
    // and the digits do not linger to derail the next key
    assert!(harness.app.count.is_empty());
}

#[tokio::test]
async fn the_permissions_tab_shows_every_answer_the_policy_would_give() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("grep", "found it").with_capabilities([Capability::Read]),
    ));
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::Shell]),
    ));
    harness.tab(Tab::Permissions);

    // nothing has been decided, so there is nothing to list: `ask` is what this policy does about
    // everything nobody has answered for, and a screenful of it would bury the line that says what
    // can happen without stopping. The rest are counted along the bottom instead
    let screen = harness.sized(110, 30);
    assert_eq!(
        harness.app.permissions().len(),
        0,
        "`ask` is not a decision, and nobody has made one"
    );
    assert!(screen.contains("nothing has been decided"), "{screen}");
    assert!(screen.contains("more it will ask about"), "{screen}");

    // ... and deciding one puts it on the tab, naming what it covers
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Shell), Verdict::Deny);
    let screen = harness.sized(110, 30);
    let shell = screen
        .lines()
        .find(|line| line.contains("shell"))
        .expect("a decided capability is listed");
    assert!(shell.contains("deny"), "{shell}");
    assert!(
        shell.contains("rm"),
        "the row names what it covers: {shell}"
    );

    // no tool declares `network` - but the shell is judged against it anyway, on what the command
    // says, so its row names the tool the answer actually reaches rather than claiming that
    // nothing needs it
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Network), Verdict::Deny);
    let screen = harness.sized(110, 30);
    let network = screen
        .lines()
        .find(|line| line.contains("network"))
        .expect("the decision is listed");
    assert!(network.contains("deny"), "{network}");
    assert!(
        network.contains("rm, when the command reaches for it"),
        "{network}"
    );

    // and a capability nothing needs at all still reads as a fact rather than a gap
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Write), Verdict::Deny);
    let screen = harness.sized(110, 30);
    let write = screen
        .lines()
        .find(|line| line.contains("write"))
        .expect("what the policy has been told about is listed");
    assert!(write.contains("nothing registered needs it"), "{write}");
}

#[tokio::test]
async fn a_shell_reaching_for_the_network_is_judged_as_reaching_for_it() {
    use kamchatka::tools::reaches_the_network as reaches;

    // what a model writes when it wants the network
    assert!(reaches("curl https://example.com"));
    assert!(reaches("pip install requests"));
    assert!(reaches("git push origin master"));
    assert!(reaches("wc -l x.py && curl -s https://example.com"));
    assert!(reaches(
        "HTTPS_PROXY=http://p:8080 curl https://example.com"
    ));
    assert!(reaches("/usr/bin/curl https://example.com"));

    // and what it writes the rest of the time
    assert!(!reaches("wc -l ledger.py"));
    assert!(!reaches("cat curl.txt"));
    assert!(!reaches("echo 'curl is a program'"));
    assert!(!reaches(""));
}

#[tokio::test]
async fn a_denied_network_reaches_the_shell_that_would_have_used_it() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "cmd": "curl https://example.com" }),
        )]),
        ModelResponse::text("refused, then"),
    ]);
    // a stand-in for the shell: what is under test is the policy reading the command, not the
    // program that would run it
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::Shell]),
    ));
    // the default is a question, not a refusal: reaching the network is a thing somebody may
    // perfectly well want, and the sandbox is what makes either answer mean something
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::Network)),
        Verdict::Ask
    );
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Network), Verdict::Deny);

    harness.send("fetch it").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(
        !screen.contains("allow this?"),
        "a refusal is not a question: {screen}"
    );
    let refused = harness
        .app
        .kernel
        .items()
        .iter()
        .any(|item| item.content.to_text().contains("permitted"));
    assert!(refused, "the model is told, rather than left waiting");

    // and the screen says which stance did it. Without this the tab reads `shell: ask` beside a
    // refused shell call and nothing anywhere accounts for the refusal
    assert!(
        screen.contains("network"),
        "a refusal nobody can account for is the thing this program is not for: {screen}"
    );
}

#[tokio::test]
async fn the_permissions_tab_admits_what_a_shell_can_do() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("grep", "found it").with_capabilities([Capability::Read]),
    ));
    harness.tab(Tab::Permissions);

    // nothing here runs commands, so the five verdicts are the whole story
    let screen = harness.sized(120, 30);
    assert!(!screen.contains("shell:"), "{screen}");

    // ... and once something does, the tab has to account for it: `shell` subsumes every other
    // row unless something is confining it, and nothing is here
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("sh", "output").with_capabilities([Capability::Shell]),
    ));
    let screen = harness.sized(120, 30);
    assert!(
        screen.contains("shell: a command can do any of these"),
        "{screen}"
    );

    // ... and when something is, it says what a command can reach instead of what it cannot
    harness.app.confinement = Confinement::Full;
    let screen = harness.sized(120, 30);
    assert!(screen.contains("shell: confined"), "{screen}");
    assert!(
        !screen.contains("can do any of these"),
        "a confined shell cannot: {screen}"
    );

    // refusing it outright puts the other rows back in charge either way
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Shell), Verdict::Deny);
    let screen = harness.sized(120, 30);
    assert!(!screen.contains("shell:"), "{screen}");
}

#[tokio::test]
async fn a_path_rule_is_finer_than_the_capability_above_it() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "read", json!({ "path": "src/main.rs" }))]),
        ModelResponse::tool_calls(vec![call("c2", "read", json!({ "path": ".env" }))]),
        ModelResponse::text("as you wish"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::Read]),
    ));

    // answered for the capability and nothing else, which is what `always` on an ordinary read
    // does. The ordinary file is no longer a question and `.env` still is, because the strictest
    // of what is consulted wins and a rule about the *file* is finer than a verdict about the tool
    // that opened it. One turn, both reads, one question
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Read), Verdict::Allow);
    harness.send("read both").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(
        screen.contains("src/main.rs") && screen.contains("read: 2 tokens"),
        "the ordinary one ran without being asked about: {screen}"
    );
    let asked = screen
        .lines()
        .find(|line| line.contains("wants:"))
        .expect("this one is a question");
    assert!(asked.contains(".env*"), "the rule is named: {asked}");
    assert!(asked.contains("read"), "and so is the capability: {asked}");
}

#[tokio::test]
async fn saying_always_answers_for_everything_the_question_named() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "read", json!({ "path": ".env" }))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::Read]),
    ));
    harness.send("read it").await;
    harness.settle().await;
    assert!(harness.app.overlay.is_some(), "the rule made it a question");

    // `a` has to answer the path rule as well as the capability. Answering for `read` alone would
    // leave the question exactly where it was, since the rule is the stricter of the two
    harness.answer(KeyCode::Char('a')).await;
    harness.settle().await;

    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Path(".env*".to_owned())),
        Verdict::Allow,
        "the rule that raised the question is the one that was answered"
    );
}

#[tokio::test]
async fn the_permissions_tab_draws_the_path_rules_too() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("read", "contents").with_capabilities([Capability::Read]),
    ));
    harness.tab(Tab::Permissions);

    // nobody has decided anything about `.env*`, so it is not a row - it is one of the things the
    // footer counts, and it arrives here the moment somebody answers a question about it
    let screen = harness.sized(120, 40);
    assert!(!screen.contains(".env*"), "{screen}");
    assert!(
        screen.contains("more it will ask about"),
        "what is not listed is still counted: {screen}"
    );

    harness
        .app
        .policy
        .set(&Subject::Path(".env*".to_owned()), Verdict::Deny);

    let screen = harness.sized(120, 40);
    let rule = screen
        .lines()
        .find(|line| line.contains(".env*"))
        .expect("a decision about a path is a decision, and is drawn like any other");
    assert!(rule.contains("deny"), "{rule}");
    assert!(rule.contains("read"), "it names the tools it binds: {rule}");

    // ... and it cycles like any other row
    pick(&mut harness, &Subject::Path(".env*".to_owned())).await;
    harness.press(KeyCode::Char(' ')).await;
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Path(".env*".to_owned())),
        Verdict::Ask
    );
}

#[tokio::test]
async fn a_refusal_says_which_stance_made_it() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "shell", json!({ "cmd": "rm -rf /tmp/x" }))]),
        ModelResponse::text("no, then"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "output").with_capabilities([Capability::Shell]),
    ));
    // this time it is the tool's own capability that is refused, not something the command reached
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Shell), Verdict::Deny);

    harness.send("clean up").await;
    harness.settle().await;

    let screen = harness.screen();
    let note = screen
        .lines()
        .find(|line| line.contains("refused by"))
        .expect("the refusal is accounted for");
    assert!(note.contains("shell"), "{note}");
    assert!(
        !note.contains("network"),
        "the command never reached for it: {note}"
    );
}

/// Puts the cursor on the row for `subject`, which has to be a decision for it to be listed.
async fn pick(harness: &mut Harness, subject: &Subject) {
    let at = harness
        .app
        .permissions()
        .iter()
        .position(|row| row.subject == *subject)
        .unwrap_or_else(|| {
            panic!(
                "the tab lists decisions, and nobody has decided about `{subject}`: {:?}",
                harness
                    .app
                    .permissions()
                    .iter()
                    .map(|row| row.subject.to_string())
                    .collect::<Vec<_>>()
            )
        });
    harness.app.chosen = at;
}

#[tokio::test]
async fn changing_a_permission_changes_what_happens_next() {
    let mut harness = Harness::configured(
        [
            ModelResponse::tool_calls(vec![call("c1", "rm", json!({}))]),
            ModelResponse::text("as you wish"),
            ModelResponse::tool_calls(vec![call("c2", "rm", json!({}))]),
            ModelResponse::text("done"),
        ],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::Shell]),
    ));

    // shell is a question by default, so this turn stops and asks
    harness.send("tidy up").await;
    harness.settle().await;
    assert!(matches!(
        harness.app.overlay,
        Some(kamchatka::app::Overlay::Permission { .. })
    ));
    // answering resumes the turn, so let it finish before starting another
    harness.answer(KeyCode::Char('n')).await;
    harness.settle().await;

    // `n` at the prompt refuses that call and decides nothing, so the tab still has nothing to
    // say about the shell. It lists decisions
    assert!(
        !harness
            .app
            .permissions()
            .iter()
            .any(|row| row.subject == Subject::Capability(Capability::Shell)),
        "a refusal of one call is not a decision about the capability"
    );

    // deciding it *is* what puts it there, and changes what the same call does next time
    harness.app.policy.set(
        &Subject::Capability(Capability::Shell),
        nachalnik::Verdict::Deny,
    );
    harness.tab(Tab::Permissions);
    pick(&mut harness, &Subject::Capability(Capability::Shell)).await;
    harness.press(KeyCode::Char('a')).await;

    assert_eq!(
        harness.app.permissions()[harness.app.chosen].verdict,
        nachalnik::Verdict::Allow
    );
    harness.tab(Tab::Chat);
    assert!(harness.screen().contains("runs without asking"));

    harness.send("tidy up again").await;
    harness.settle().await;

    assert!(
        harness.app.overlay.is_none(),
        "it was decided in advance, so there is nothing to ask"
    );
    let screen = harness.screen();
    assert!(screen.contains("gone"), "the tool ran: {screen}");
}

#[tokio::test]
async fn a_capability_can_be_refused_outright_rather_than_asked_about() {
    let mut harness = Harness::configured(
        [
            ModelResponse::tool_calls(vec![call("c1", "rm", json!({}))]),
            ModelResponse::text("fine, I will not"),
        ],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::Shell]),
    ));

    // the tab lists decisions, so there is one to make first: `a` at a question is what puts a
    // subject on it, and `n` there is what changes its mind
    harness.app.policy.set(
        &Subject::Capability(Capability::Shell),
        nachalnik::Verdict::Allow,
    );
    harness.tab(Tab::Permissions);
    pick(&mut harness, &Subject::Capability(Capability::Shell)).await;
    harness.press(KeyCode::Char('n')).await;

    harness.tab(Tab::Chat);
    harness.send("tidy up").await;
    harness.settle().await;

    // no question, no run, and the model is told rather than left guessing
    assert!(harness.app.overlay.is_none(), "a refusal is not a question");
    let request = harness.app.kernel.preview_request().unwrap();
    let sent = format!("{:?}", request.messages);
    assert!(!sent.contains("gone"), "it should not have run: {sent}");
    assert!(sent.contains("not permitted"), "{sent}");
}

#[tokio::test]
async fn cycling_a_permission_goes_round_rather_than_getting_stuck() {
    let mut harness = Harness::new([]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("rm", "gone").with_capabilities([Capability::Shell]),
    ));
    harness.app.policy.set(
        &Subject::Capability(Capability::Shell),
        nachalnik::Verdict::Allow,
    );
    harness.tab(Tab::Permissions);
    pick(&mut harness, &Subject::Capability(Capability::Shell)).await;

    use nachalnik::Verdict::{Allow, Ask, Deny};
    let shell = Subject::Capability(Capability::Shell);
    let mut seen = vec![harness.app.policy.stance(&shell)];
    for _ in 0..2 {
        harness.press(KeyCode::Char(' ')).await;
        seen.push(harness.app.policy.stance(&shell));
    }

    assert_eq!(seen, vec![Allow, Deny, Ask], "space goes allow, deny, ask");

    // ... and arriving back at `ask` takes the row off the tab, because `ask` is not a decision -
    // it is what the policy does when nobody has made one. That is what taking a decision back
    // looks like here, and the subject returns the next time somebody answers a question about it
    assert!(
        !harness
            .app
            .permissions()
            .iter()
            .any(|row| row.subject == shell),
        "cycling back to `ask` is undeciding it"
    );
    harness.app.policy.set(&shell, Allow);
    assert!(
        harness
            .app
            .permissions()
            .iter()
            .any(|row| row.subject == shell)
    );
}

#[tokio::test]
async fn ctrl_d_leaves_even_when_a_tool_is_waiting_to_run() {
    let mut harness = Harness::configured(
        [ModelResponse::tool_calls(vec![call(
            "c1",
            "danger",
            json!({}),
        )])],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("danger", "ran").with_capabilities([Capability::Shell]),
    ));

    harness.send("do something rash").await;
    harness.settle().await;
    assert!(harness.app.overlay.is_some(), "a question is up");

    // `d` is a key at this prompt, and the overlay used to be dispatched before anything looked
    // at the modifiers - so the key people press to leave dropped every pending call instead
    harness
        .app
        .on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .await;

    assert!(
        harness.app.quit,
        "ctrl+d means leave, wherever it is pressed"
    );
    assert_eq!(
        harness.app.kernel.pending_permissions().len(),
        1,
        "and it does not answer the question on the way out"
    );
}

#[tokio::test]
async fn dropping_the_calls_hands_the_turn_back_to_the_model() {
    let mut harness = Harness::configured(
        [
            ModelResponse::tool_calls(vec![call("c1", "danger", json!({}))]),
            ModelResponse::text("all right, something else then"),
        ],
        Config::default(),
    );
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("danger", "ran").with_capabilities([Capability::Shell]),
    ));

    harness.send("do something rash").await;
    harness.settle().await;
    harness.answer(KeyCode::Char('d')).await;

    // answering `n` to each resumes the turn; dropping them all used to leave the kernel idle
    // with the refusals recorded and nobody driving, until somebody noticed and typed /continue
    harness.settle().await;
    let screen = harness.screen();
    assert!(
        screen.contains("all right, something else then"),
        "{screen}"
    );
}

#[tokio::test]
async fn editing_an_item_leaves_it_doing_whatever_it_was_doing() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file("big.log", "a wall of output"));
    harness.app.kernel.push(ContextItem::user("hello"));
    harness.tab(Tab::Context);
    harness.press(KeyCode::Home).await;

    // take it out - twice, past the marker - then change what it says
    harness.press(KeyCode::Char(' ')).await;
    harness.press(KeyCode::Char(' ')).await;
    harness.press(KeyCode::Char('e')).await;
    harness.send("a shorter wall").await;

    // editing decides what an item says, not whether it is sent: a pruned item that came back
    // Active would quietly put itself in the next request, and an archived one would promote the
    // whole of an oversized tool output into it
    let items = harness.app.kernel.items();
    let edited = items.last().expect("the replacement");
    assert_eq!(edited.state, ContextState::Excluded);
    let sent = format!(
        "{:?}",
        harness.app.kernel.preview_request().unwrap().messages
    );
    assert!(!sent.contains("shorter wall"), "{sent}");
}

#[tokio::test]
async fn a_count_does_not_outlive_the_key_after_it() {
    let mut harness = Harness::new([]);
    for i in 0..12 {
        harness
            .app
            .kernel
            .push(ContextItem::user(format!("message {i}")));
    }
    harness.tab(Tab::Context);
    harness.press(KeyCode::End).await;

    // typed, then abandoned by a key that never reaches the context tab at all
    harness.press(KeyCode::Char('4')).await;
    harness.press(KeyCode::F(1)).await;
    harness.press(KeyCode::Esc).await;

    // so `G` is the last item, not item 4
    harness.press(KeyCode::Char('G')).await;
    assert_eq!(harness.app.kernel.items()[harness.app.selected].id.0, 12);
}

#[tokio::test]
async fn an_abandoned_edit_does_not_swallow_the_next_message() {
    let mut harness = Harness::new([ModelResponse::text("hello back")]);
    harness
        .app
        .kernel
        .push(ContextItem::file("notes.txt", "the original"));
    harness.tab(Tab::Context);
    harness.press(KeyCode::Home).await;
    harness.press(KeyCode::Char('e')).await;
    assert!(harness.app.editing.is_some());

    // walking away from the tab abandons the edit; it used to stay armed, with the item's text
    // still in the prompt, so the next message was committed into the context instead of sent
    harness.tab(Tab::Chat);
    assert!(harness.app.editing.is_none(), "the edit was abandoned");
    assert_eq!(harness.app.input.lines(), [""], "and the prompt is empty");

    harness.send("hello").await;
    harness.settle().await;

    assert!(harness.screen().contains("hello back"), "it was sent");
    assert_eq!(
        harness.app.kernel.items()[0].content.to_text(),
        "the original",
        "and the item is untouched"
    );
}

#[tokio::test]
async fn a_command_that_opens_a_tab_leaves_the_keys_on_the_prompt() {
    let mut harness = Harness::new([]);

    harness.send("/policy").await;

    // `a`, `n`, `r` and `d` are all bare letters on this tab and all of them change something, so
    // a command typed at the prompt must not hand the next keystroke to it
    assert_eq!(harness.app.tab, Tab::Permissions);
    assert_eq!(harness.app.focus, kamchatka::app::Focus::Input);

    let before = harness
        .app
        .policy
        .stance(&Subject::Capability(Capability::Read));
    harness.send("are we ok?").await;
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::Read)),
        before,
        "typing at the prompt should not have rewritten the policy"
    );
}

#[tokio::test]
async fn a_fenced_block_is_coloured_by_what_the_tokens_are() {
    // note: a real string with real newlines; a `\` continuation would eat them, and the block
    // would arrive as one line of prose
    let answer = r#"how it works:

```rust
// the loop
fn step() { let x = 1; }
```
"#;
    let mut harness = Harness::new([ModelResponse::text(answer)]);
    harness.send("go on").await;
    harness.settle().await;

    let screen = harness.screen();

    // the fences themselves are punctuation and are not shown; the rule down the left is
    assert!(!screen.contains("```"), "{screen}");
    let code = screen
        .lines()
        .find(|line| line.contains("fn step()"))
        .expect("the block is on screen");
    assert!(code.contains("│ fn step()"), "{code}");

    // and the tokens are told apart: a keyword, a number and a comment are three colours
    let (keyword, _) = harness.style_of("fn step");
    let (digit, _) = harness.style_of("1;");
    let (comment, _) = harness.style_of("// the loop");
    assert_eq!(keyword, Color::Magenta);
    assert_eq!(digit, Color::Yellow);
    assert_eq!(comment, Color::Gray);
    assert_ne!(keyword, digit);
}

#[tokio::test]
async fn a_block_still_arriving_is_a_block_rather_than_prose() {
    // every code block is unterminated for as long as it is streaming in, and one read as prose
    // would jump from unstyled text to a coloured block the moment the closing fence landed
    let mut harness = Harness::new([]);
    harness
        .app
        .say(Speaker::Model, "here:\n\n```rust\nfn half(");

    let screen = harness.screen();

    let code = screen
        .lines()
        .find(|line| line.contains("fn half("))
        .expect("what there is of it is on screen");
    assert!(code.contains("│ fn half("), "{code}");
    assert!(!screen.contains("```"), "{screen}");
}

#[tokio::test]
async fn a_language_nothing_can_colour_is_still_a_block() {
    let mut harness = Harness::new([]);
    harness.app.say(
        Speaker::Model,
        "look:\n\n```brainfuck\n+[----->+++<]>+.\n```\n",
    );

    let screen = harness.screen();

    let code = screen
        .lines()
        .find(|line| line.contains("+[----->+++<]>+."))
        .expect("the block is on screen");
    assert!(
        code.contains("│ +[----->+++<]>+."),
        "it gets the rule whether or not anybody can colour it: {code}"
    );
}

#[tokio::test]
async fn what_a_person_reads_is_lighter_than_what_holds_it_together() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file("src/parser.rs", "fn parse() {}"));
    harness.app.kernel.set_state(
        [nachalnik::ContextId(1)],
        ContextState::Excluded,
        Some("too big".into()),
    );
    harness.tab(Tab::Context);

    // the reason an item is not being sent is the column this tab exists for, and it is read
    let (reason, _) = harness.style_of("excluded: too big");
    assert_eq!(reason, Color::Gray, "not the terminal's bright black");

    // the same for the header above it, and for the count along the bottom
    let (header, _) = harness.style_of("sending");
    assert_eq!(header, Color::Gray);

    // the rule down the left of a code block is not read, and stays out of the way
    harness.app.say(Speaker::Model, "```rust\nfn f() {}\n```\n");
    harness.tab(Tab::Chat);
    let (bar, _) = harness.style_of("│ fn f()");
    assert_eq!(bar, Color::DarkGray, "chrome, not words");
}

#[tokio::test]
async fn a_tab_with_more_than_fits_says_so_down_its_border() {
    let mut harness = Harness::new([]);
    for i in 0..80 {
        harness
            .app
            .kernel
            .push(ContextItem::file(format!("src/f{i}.rs"), "fn f() {}"));
    }
    harness.tab(Tab::Context);

    let screen = harness.sized(100, 30);
    let thumb: Vec<_> = screen
        .lines()
        .enumerate()
        .filter(|(_, line)| line.ends_with('█'))
        .map(|(y, _)| y)
        .collect();

    assert!(
        !thumb.is_empty(),
        "eighty items in twenty-odd rows: {screen}"
    );
    assert!(
        thumb.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "the thumb is one run, not scattered: {thumb:?}"
    );

    // it sits at the top, because that is where the list is
    let top = screen
        .lines()
        .position(|line| line.contains("sending"))
        .expect("the header row");
    assert_eq!(
        thumb[0],
        top + 1,
        "the bar starts under the header: {screen}"
    );

    // ... and moves when the list does
    for _ in 0..60 {
        harness.press(KeyCode::Down).await;
    }
    let scrolled = harness.sized(100, 30);
    let moved = scrolled
        .lines()
        .position(|line| line.ends_with('█'))
        .expect("still a thumb");
    assert!(moved > thumb[0], "{scrolled}");

    // and at the end of the list it is at the end of the track: a bar that stopped short would be
    // saying there is more below when there is not
    harness.press(KeyCode::End).await;
    let bottom = harness.sized(100, 30);
    let rows: Vec<_> = bottom.lines().collect();
    let last = rows
        .iter()
        .rposition(|line| line.contains("src/f79.rs"))
        .expect("the last item is on screen");
    assert!(
        rows[last].ends_with('█'),
        "the thumb reaches the row the content does:\n{bottom}"
    );

    // ... and back at the top it starts at the top
    harness.press(KeyCode::Home).await;
    let top = harness.sized(100, 30);
    let rows: Vec<_> = top.lines().collect();
    let first = rows
        .iter()
        .position(|line| line.contains("src/f0.rs"))
        .expect("the first item is on screen");
    assert!(
        rows[first].ends_with('█'),
        "and the row the content starts on:\n{top}"
    );
}

#[tokio::test]
async fn a_tab_that_fits_draws_no_bar_at_all() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file("src/parser.rs", "fn parse() {}"));
    harness.tab(Tab::Context);

    let screen = harness.sized(100, 30);

    assert!(
        !screen.contains('█'),
        "one item in thirty rows needs no scrollbar: {screen}"
    );
}

#[tokio::test]
async fn a_session_is_saved_to_a_path_and_comes_back_from_it() {
    let dir = std::env::temp_dir().join(format!("kamchatka-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a place to write");
    // named `.jsonl` on purpose: the stem used to keep it, so this wrote `notes.jsonl.jsonl`
    let asked = dir.join("notes.jsonl");

    let mut harness = Harness::new([ModelResponse::text("4817, noted")]);
    harness.send("remember 4817").await;
    harness.settle().await;

    harness.send(&format!("/save {}", asked.display())).await;

    let log = dir.join("notes.jsonl");
    let state = dir.join("notes.json");
    assert!(log.exists(), "the log is at the path that was asked for");
    assert!(state.exists(), "and so is the snapshot");
    assert!(
        !dir.join("notes.jsonl.jsonl").exists(),
        "the extension should not have been doubled"
    );
    // packed, because the note carries the path it wrote to and a long enough temp directory
    // leaves the renderer no choice but to break the name across two rows
    assert!(
        harness.packed().contains("notes.json"),
        "{}",
        harness.screen()
    );

    // the log is one record per line, and every line is a record
    let written = std::fs::read_to_string(&log).unwrap();
    let records: Vec<&str> = written.lines().filter(|line| !line.is_empty()).collect();
    assert!(!records.is_empty());
    for line in &records {
        serde_json::from_str::<nachalnik::Record>(line).expect("every line is a record");
    }

    // and the snapshot rebuilds the context in a kernel that never saw any of it happen
    let snapshot: nachalnik::Snapshot =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).expect("a session");
    let carried = Kernel::resume(Config::default(), snapshot);

    let said: Vec<String> = carried
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert!(said.iter().any(|text| text.contains("remember 4817")));
    assert!(said.iter().any(|text| text.contains("4817, noted")));

    // saving again over the same files says so rather than replacing them in silence
    harness.send(&format!("/save {}", asked.display())).await;
    assert!(harness.flat().contains("replaced"), "{}", harness.screen());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_saved_session_comes_back_into_a_running_one_without_losing_what_was_there() {
    let dir = std::env::temp_dir().join(format!("kamchatka-load-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a place to write");
    let saved = dir.join("before.json");

    let mut first = Harness::new([ModelResponse::text("4817, noted")]);
    first.send("remember 4817").await;
    first.settle().await;
    first
        .send(&format!("/save {}", dir.join("before").display()))
        .await;
    assert!(saved.exists());

    // a second session, with a conversation of its own already in it, and something pinned
    let mut second = Harness::new([ModelResponse::text("nothing so far")]);
    let pinned = second
        .app
        .kernel
        .push(ContextItem::system("answer in Polish").pinned());
    second.send("what do you remember").await;
    second.settle().await;
    let mine: Vec<_> = second.app.kernel.items().iter().map(|i| i.id).collect();

    second.send(&format!("/load {}", saved.display())).await;

    // what was here is set aside rather than dropped: same numbers, same contents, not going
    for id in mine.iter().filter(|id| **id != pinned) {
        let item = second.app.kernel.item(*id).expect("still there");
        assert_eq!(item.state, ContextState::Archived, "[{id}] was dropped");
        assert!(!item.content.to_text().is_empty());
    }

    // a pin is the person saying this stays, and `--system` is pinned: a load that archived it
    // would answer a question about a saved conversation by revoking the session's instructions
    let kept = second.app.kernel.item(pinned).expect("still there");
    assert_eq!(kept.state, ContextState::Pinned, "the pin was overruled");

    // and the loaded conversation is the one the next request would carry
    let projected: Vec<String> = second
        .app
        .kernel
        .project()
        .messages
        .iter()
        .map(|message| {
            message
                .content
                .as_ref()
                .map(|c| c.to_text().into_owned())
                .unwrap_or_default()
        })
        .collect();
    assert!(
        projected.iter().any(|text| text.contains("remember 4817")),
        "{projected:?}"
    );
    assert!(
        !projected
            .iter()
            .any(|text| text.contains("what do you remember")),
        "{projected:?}"
    );
    assert!(
        projected
            .iter()
            .any(|text| text.contains("answer in Polish")),
        "{projected:?}"
    );

    // it is on the screen as the conversation it was, and it says what it did
    let screen = second.flat();
    assert!(screen.contains("remember 4817"), "{screen}");
    assert!(screen.contains("loaded"), "{screen}");

    // and nothing was destroyed to get here: two undos and it is as it was
    assert!(second.app.kernel.undo());
    assert!(second.app.kernel.undo());
    for id in mine.iter().filter(|id| **id != pinned) {
        let item = second.app.kernel.item(*id).expect("still there");
        assert_eq!(item.state, ContextState::Active, "[{id}] did not come back");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn loading_something_that_is_not_a_session_says_which_file_and_why() {
    let dir = std::env::temp_dir().join(format!("kamchatka-notasession-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a place to write");
    let junk = dir.join("junk.json");
    std::fs::write(&junk, "{\"nope\": true}").expect("written");

    let mut harness = Harness::new([]);
    harness.send(&format!("/load {}", junk.display())).await;
    // flattened, because the message carries a path and a long enough one wraps the sentence
    let screen = harness.flat();
    assert!(screen.contains("is not a session"), "{screen}");
    // the name, from the view that survives a path the renderer had to break mid-token
    assert!(harness.packed().contains("junk.json"), "{screen}");

    harness
        .send(&format!("/load {}", dir.join("absent.json").display()))
        .await;
    let screen = harness.flat();
    assert!(screen.contains("could not read"), "{screen}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_resumed_session_is_read_back_as_the_conversation_it_was() {
    let mut first = Harness::new([ModelResponse::text("of course")]);
    first.send("hello there").await;
    first.settle().await;

    // what `-r` does: a fresh kernel from the snapshot, and the terminal reads the conversation
    // back off the context, because a resume arrives as one event rather than a thousand
    let carried = Kernel::resume(Config::default(), first.app.kernel.snapshot());
    let mut second = Harness::new([]);
    second.app.kernel = carried;
    second.app.replay();

    let screen = second.screen();
    assert!(screen.contains("hello there"), "{screen}");
    assert!(screen.contains("of course"), "{screen}");
    assert!(screen.contains("resumed session"), "{screen}");
}

#[tokio::test]
async fn saying_always_answers_for_the_calls_already_waiting() {
    let mut harness = Harness::new([
        // one answer, three calls: the model asked for all of them before anybody was asked
        // about any of them, and all three questions exist before the first is drawn
        ModelResponse::tool_calls(vec![
            call("c1", "dig", json!({ "where": "one" })),
            call("c2", "dig", json!({ "where": "two" })),
            call("c3", "dig", json!({ "where": "three" })),
        ]),
        ModelResponse::text("all three"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
    ));

    harness.send("dig three times").await;
    harness.settle().await;
    assert_eq!(
        harness.app.kernel.pending_permissions().len(),
        3,
        "one question each"
    );

    harness.answer(KeyCode::Char('a')).await;
    harness.settle().await;

    // "always" said what happens from now on, and the other two were already in the queue when it
    // was said. Asking about them again would be the prompt going back on it one keystroke later
    assert!(
        harness.app.kernel.pending_permissions().is_empty(),
        "the rest of the batch was answered by the same `always`"
    );
    assert!(harness.app.overlay.is_none());
    let results = harness
        .app
        .kernel
        .items()
        .into_iter()
        .filter(|item| item.label == "dig")
        .count();
    assert_eq!(results, 3, "and all three of them ran");
}

#[tokio::test]
async fn saying_always_leaves_a_waiting_call_that_needs_something_else_a_question() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![
            call("c1", "dig", json!({ "path": "src/main.rs" })),
            call("c2", "dig", json!({ "path": ".env" })),
        ]),
        ModelResponse::text("both"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
    ));

    harness.send("dig twice").await;
    harness.settle().await;

    // no `settle` after this one: there is still a question, so no turn was started to wait for
    harness.answer(KeyCode::Char('a')).await;

    // `shell` is allowed from now on and the second call is still a question, because the rule
    // about `.env` is not what anybody just answered
    let waiting = harness.app.kernel.pending_permissions();
    assert_eq!(waiting.len(), 1, "the one with a rule of its own");
    assert_eq!(waiting[0].args["path"], ".env");
    assert!(harness.app.overlay.is_some());
}

#[tokio::test]
async fn a_pasted_block_arrives_as_the_lines_it_was_pasted_as() {
    let mut harness = Harness::new([]);

    // what a terminal sends: the breaks inside a paste are carriage returns, because a paste is
    // spelled as though it had been typed and that is what enter sends
    harness.app.paste("first line\rsecond line\r\nthird line");

    assert_eq!(
        harness.app.input.lines(),
        ["first line", "second line", "third line"],
        "a paste is the lines it was, not one line with invisible characters in it"
    );
    let screen = harness.screen();
    for line in ["first line", "second line", "third line"] {
        assert!(
            screen.contains(line),
            "{line} is not in the prompt: {screen}"
        );
    }
}

#[tokio::test]
async fn a_message_sent_into_a_running_turn_is_answered_when_it_ends() {
    let mut harness = Harness::new([
        ModelResponse::text("the first answer"),
        ModelResponse::text("the second answer"),
    ]);

    harness.send("the first question").await;
    // ... and this one goes in while that turn is still running. Nothing used to come back to it:
    // the turn was already going, so nothing started, and it sat in the context unanswered
    harness.app.busy = true;
    harness.send("the second question").await;
    harness.app.busy = false;
    harness.app.on_outcome(Outcome::Stopped(State::Idle));

    assert!(harness.app.busy, "a turn was started for it");
    harness.settle().await;
    let screen = harness.screen();
    assert!(screen.contains("the second answer"), "{screen}");
}

#[tokio::test]
async fn a_message_sent_into_a_running_turn_goes_in_after_the_answer_it_interrupted() {
    let mut harness = Harness::new([ModelResponse::text("the first answer")]);

    harness.send("the first question").await;
    harness.app.busy = true;
    harness.send("the second question").await;

    // ... which is not in the context yet, because the answer being written is not in it either
    let during: Vec<String> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert!(
        !during.iter().any(|text| text.contains("the second")),
        "a message pushed here lands in front of the answer the model is still writing: {during:?}"
    );

    harness.app.busy = false;
    harness.settle().await;

    // the conversation reads in the order it happened, and ends with the person - which is the
    // one shape a request is allowed to have
    let after: Vec<String> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    let question = after
        .iter()
        .position(|text| text.contains("the second question"))
        .expect("it went in when the turn ended");
    let answer = after
        .iter()
        .position(|text| text.contains("the first answer"))
        .expect("the answer it waited for");
    assert!(answer < question, "{after:?}");
}

#[tokio::test]
async fn a_question_that_arrives_under_somebody_s_fingers_is_not_answered_by_them() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
    ));

    harness.send("dig").await;
    harness.settle().await;
    assert!(harness.app.overlay.is_some(), "the question is up");

    // the next thing typed is a message, not an answer - and `a` is the third letter of it. It
    // used to grant `shell` for the rest of the session, which is what a live run did
    for c in "what is the capital of Peru".chars() {
        harness.press(KeyCode::Char(c)).await;
    }

    assert!(
        harness.app.overlay.is_some(),
        "the question is still waiting to be read"
    );
    assert_eq!(
        harness
            .app
            .policy
            .stance(&Subject::Capability(Capability::Shell)),
        Verdict::Ask,
        "nothing was granted by somebody typing a sentence"
    );
    // ... and the sentence is where it was aimed, whole - including its first letter, which
    // arrives after a pause and is therefore not part of any typing this could have waited out
    assert_eq!(harness.app.input.lines(), ["what is the capital of Peru"]);

    // ... and it can be sent from there, which puts it in the queue a message typed into a
    // running turn goes in: the question is a pause in a turn, not the end of one
    harness.press(KeyCode::Enter).await;
    assert!(harness.app.input.lines() == [""], "the prompt was sent");
    assert!(
        harness.app.overlay.is_some(),
        "and the question is still the question"
    );

    // and once the typing stops, one key answers it
    harness.answer(KeyCode::Char('y')).await;
    harness.settle().await;
    assert!(harness.app.overlay.is_none(), "answered");
}

#[tokio::test]
async fn a_message_sent_into_a_turn_that_stops_to_ask_waits_for_the_answer_too() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("dug, and Lima"),
        ModelResponse::text("Lima"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
    ));

    harness.send("dig").await;
    // typed while that turn is running, and the turn then stops to ask about the call
    harness.send("what is the capital of Peru").await;
    harness.settle().await;
    assert!(harness.app.overlay.is_some(), "the turn stopped to ask");

    // it must not have gone in yet: the call it stopped at has a result still to come, and a user
    // message between an assistant's call and that call's result is a shape a request cannot have
    let waiting: Vec<String> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert!(
        !waiting.iter().any(|text| text.contains("capital of Peru")),
        "{waiting:?}"
    );

    harness.answer(KeyCode::Char('y')).await;
    harness.settle().await;

    let after: Vec<String> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .collect();
    let question = after
        .iter()
        .position(|text| text.contains("capital of Peru"))
        .expect("it went in once the turn was done");
    let result = after
        .iter()
        .position(|text| text.contains("a bone"))
        .expect("the call's result");
    assert!(result < question, "{after:?}");
}

/// Every command the prompt answers to is in the help.
///
/// note: read out of the source rather than listed here. A list in a test is one more copy for
/// somebody to forget, updated by the same person who forgot the help - and the thing worth
/// catching is a command that works and cannot be found, which is the shape `/help` itself was in.
#[test]
fn every_command_that_exists_is_in_the_help() {
    // note: line endings normalised first. `.gitattributes` pins the checkout to LF, and this is
    // the belt to that pair of braces: a test that reads source to see what it says should not be
    // the thing that notices how the source was checked out. It was, on Windows, and nowhere else.
    let source = include_str!("../src/app.rs").replace("\r\n", "\n");
    let handler = source
        .split_once("async fn command(")
        .expect("the slash commands are answered in one place")
        .1
        .split_once("\n    }\n")
        .expect("and that place ends")
        .0;

    let mut listed = 0;
    for line in handler.lines() {
        // a match arm whose pattern is one or more quoted names: `"prune" | "keep" | "restore" =>`
        let Some((arms, _)) = line.split_once("=>") else {
            continue;
        };
        if !arms.trim_start().starts_with('"') {
            continue;
        }
        for name in arms.split('|') {
            let Some((name, _)) = name.trim().trim_start_matches('"').split_once('"') else {
                continue;
            };
            assert!(
                ui::HELP.contains(&format!("/{name}")),
                "`/{name}` works and F1 does not mention it"
            );
            listed += 1;
        }
    }

    assert!(
        listed > 15,
        "only found {listed} commands; the scan is broken"
    );
}

#[tokio::test]
async fn a_message_that_has_to_wait_says_that_it_is_waiting() {
    let mut harness = Harness::new([ModelResponse::text("the first answer")]);

    harness.send("the first question").await;
    harness.app.busy = true;
    harness.send("the second question").await;

    // it is on the screen and not yet in the context, which is the one moment those two disagree.
    // Saying so is the difference between a message waiting and a message that went nowhere
    let screen = harness.screen();
    assert!(
        screen.contains("goes in when the turn stops"),
        "a queued message says it is queued: {screen}"
    );
}

#[tokio::test]
async fn the_address_the_requests_go_to_is_visible_and_can_be_changed() {
    let mut harness = Harness::new([]);

    // where they are going, before anything is switched. A model name means a different model at a
    // different address, so a comparison that cannot see the address is a comparison of names
    harness.send("/provider").await;
    let screen = harness.screen();
    assert!(screen.contains("http://127.0.0.1:1"), "{screen}");

    harness
        .send("/provider http://127.0.0.1:2/v1 a-model-served-there")
        .await;
    // the probe it starts is a round trip to a port with nothing on it; the address itself changes
    // here, and that is what the next request would use
    for _ in 0..50 {
        if harness.app.provider.endpoint() == "http://127.0.0.1:2/v1" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(harness.app.provider.endpoint(), "http://127.0.0.1:2/v1");
    // the model went with the address, because a model belongs to the address that serves it
    assert_eq!(
        nachalnik::Provider::info(&*harness.app.provider).model,
        "a-model-served-there"
    );

    harness.send("/seams").await;
    let seams = harness.screen();
    assert!(
        seams.contains("http://127.0.0.1:2/v1"),
        "the seam that names the provider says where it is: {seams}"
    );
}

#[tokio::test]
async fn any_one_item_can_be_taken_out_of_the_request_or_pinned_against_compaction() {
    let mut harness = Harness::new([]);
    for text in ["the first", "the second", "the third"] {
        harness.app.kernel.push(ContextItem::user(text));
    }
    harness.tab(Tab::Context);

    // one at a time, by picking it: the second out of the next request - two presses, since the
    // first stops at the marker - and the third pinned. Neither is a sweep over everything, which
    // is the operation that is never what anybody wants
    harness.press(KeyCode::Home).await;
    harness.press(KeyCode::Down).await;
    harness.press(KeyCode::Char(' ')).await;
    harness.press(KeyCode::Char(' ')).await;
    harness.press(KeyCode::Down).await;
    harness.press(KeyCode::Char('p')).await;

    let states: Vec<ContextState> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.state)
        .collect();
    assert_eq!(
        states,
        [
            ContextState::Active,
            ContextState::Excluded,
            ContextState::Pinned
        ]
    );

    // ... and what that means is on the wire: the excluded one is not in the request, and the
    // pinned one is something the kernel will refuse a compactor
    let request = harness.app.kernel.preview_request().expect("a request");
    let sent = format!("{:?}", request.messages);
    assert!(
        sent.contains("the first") && sent.contains("the third"),
        "{sent}"
    );
    assert!(!sent.contains("the second"), "{sent}");

    // and every change is one keystroke from being undone
    harness.press(KeyCode::Char('u')).await;
    harness.press(KeyCode::Char('u')).await;
    harness.press(KeyCode::Char('u')).await;
    let restored: Vec<ContextState> = harness
        .app
        .kernel
        .items()
        .iter()
        .map(|item| item.state)
        .collect();
    assert_eq!(restored, [ContextState::Active; 3]);
}

#[tokio::test]
async fn every_event_the_session_recorded_is_on_the_trace_tab() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "look", json!({}))]),
        ModelResponse::text("and there it was"),
    ]);
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("look", "a thing")));

    harness.send("look").await;
    harness.settle().await;
    // the recount at the end of a turn is itself an event, and it is emitted by the outcome the
    // line above has just handed over
    harness.drain();
    harness.tab(Tab::Trace);
    let screen = harness.sized(120, 40);

    // the claim is the whole log, not a selection of it: whatever the runtime recorded, this tab
    // draws under the same name. Two exceptions, both of them nameable. The streaming fragments
    // are coalesced into one counting line and are not in the log either by default
    // (`Config::record_progress`), and the chat tab is where they are read; and `session.started`
    // is emitted by the kernel's constructor, before a subscriber to it can exist at all
    let recorded: std::collections::BTreeSet<String> = harness.app.kernel.with_history(|session| {
        session
            .records()
            .map(|record| record.event.name().to_owned())
            .collect()
    });
    assert!(recorded.len() > 8, "a turn records more than {recorded:?}");
    for name in recorded.iter().filter(|name| *name != "session.started") {
        assert!(
            screen.contains(name.as_str()),
            "`{name}` is not on the trace: {screen}"
        );
    }
}

/// The status line pairs the model with where it is being served from, without being asked for it.
///
/// note: `/model`, `/provider` and `/seams` have always named the address, but only when asked,
/// so a session pointed at a local ollama drew exactly like one talking to OpenRouter - and the
/// reason for naming the address at all is that those are two different models.
#[tokio::test]
async fn the_status_line_says_where_the_requests_go() {
    let mut harness = Harness::new([ModelResponse::text("done")]);

    harness.send("go").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(
        screen.contains("@ 127.0.0.1:1"),
        "the status line does not say where the requests go: {screen}"
    );
}

/// A compaction pass leaves the conversation coherent: the calls the model made are still on the
/// record, each with an answer saying its result was compacted.
///
/// note: the reason this program elides rather than removes. Removing a tool result forces the
/// projector to drop the call that asked for it - a call with no result is a request most
/// providers reject - so the model was left reading a history in which it never asked for any of
/// this, immediately above a summary saying the results had been dropped.
#[tokio::test]
async fn compaction_shortens_a_result_without_unasking_the_question() {
    use kamchatka::tools::Trim;
    use nachalnik::{Compactor, Content, ToolCall};

    let harness = Harness::new([]);
    let kernel = &harness.app.kernel;

    let call = ToolCall::new(
        "call-1",
        "read",
        std::sync::Arc::new(json!({"path": "big.rs"})),
    );
    kernel.push(ContextItem::user("what is in big.rs?"));
    kernel.push(ContextItem::assistant(
        Content::text("let me look"),
        vec![call.clone()],
    ));
    let result = kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "read",
        Content::text("x".repeat(40_000)),
        false,
    ));

    let trim = Trim {
        threshold: 0.0,
        target: 0.0,
    };
    let plan = trim
        .plan(&kernel.items(), &kernel.budget())
        .await
        .expect("something to compact");
    assert_eq!(plan.elide, vec![result], "it elides rather than removes");
    assert!(plan.remove.is_empty());

    let report = kernel.apply_compaction(plan);
    assert_eq!(report.elided.len(), 1);
    assert!(
        report.tokens_after < report.tokens_before / 10,
        "{} -> {}",
        report.tokens_before,
        report.tokens_after
    );

    let request = kernel.preview_request().expect("a request");
    let sent = format!("{:?}", request.messages);
    assert!(!sent.contains("xxxx"), "the content is gone");
    assert!(
        sent.contains("compacted to make room"),
        "and says so where it was: {sent}"
    );
    assert_eq!(
        request
            .messages
            .iter()
            .filter(|m| !m.tool_calls.is_empty())
            .count(),
        1,
        "the call it made is still on the record: {sent}"
    );
    assert!(
        kernel.project().repairs.is_empty(),
        "so nothing had to be repaired"
    );

    // and it is still on the tab, marked, restorable, with what it holds counted as held back
    assert_eq!(kernel.item(result).unwrap().state, ContextState::Elided);
    assert_eq!(
        kernel.with_context(|c| c.tokens_withheld()),
        kernel.item(result).unwrap().tokens
    );
}

/// Editing an elided item leaves it elided: the row says a marker is being sent, and an edit that
/// came back `Active` would be sending the new text against what the screen says.
#[tokio::test]
async fn editing_an_elided_item_does_not_quietly_send_the_edit() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file("big.log", "a wall of output"));
    harness.app.kernel.push(ContextItem::user("hello"));
    harness.tab(Tab::Context);
    harness.press(KeyCode::Home).await;

    // one press: elided. Then rewrite what it says
    harness.press(KeyCode::Char(' ')).await;
    assert_eq!(harness.app.kernel.items()[0].state, ContextState::Elided);
    harness.press(KeyCode::Char('e')).await;
    harness.send("a shorter wall").await;

    let items = harness.app.kernel.items();
    let edited = items.last().expect("the replacement");
    assert_eq!(edited.state, ContextState::Elided);
    let sent = format!(
        "{:?}",
        harness.app.kernel.preview_request().unwrap().messages
    );
    assert!(!sent.contains("shorter wall"), "{sent}");

    // and `space` round the rest of the cycle - past excluded - is how you say you meant the
    // model to read it
    harness.press(KeyCode::End).await;
    harness.press(KeyCode::Char(' ')).await;
    harness.press(KeyCode::Char(' ')).await;
    let items = harness.app.kernel.items();
    assert_eq!(items.last().unwrap().state, ContextState::Active);
    let sent = format!(
        "{:?}",
        harness.app.kernel.preview_request().unwrap().messages
    );
    assert!(sent.contains("shorter wall"), "{sent}");
}

#[tokio::test]
async fn a_question_about_a_long_argument_can_be_read_and_still_be_answered() {
    // an `amend` that rewrites a tool result carries the replacement in its arguments, and the
    // replacement is as long as the result was. Sized as one block, the box grew past the bottom
    // of the screen and `centred` cut what was last in it - the answers
    let long: String = (0..80)
        .map(|i| format!("line {i} of a very long replacement"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "amend",
            json!({ "action": "revise", "content": long }),
        )]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("amend", "revised").with_capabilities([Capability::Custom("amend".into())]),
    ));

    harness.send("fix it").await;
    harness.settle().await;

    // the question, and every way of answering it, on a screen the arguments cannot fit on
    let screen = harness.screen();
    assert!(screen.contains("amend wants: amend"), "{screen}");
    assert!(screen.contains("[y] once"), "{screen}");
    assert!(screen.contains("[i] the exact JSON"), "{screen}");
    // and it says how to see the part that did not fit
    assert!(screen.contains("pgup / pgdn for the rest"), "{screen}");
    assert!(screen.contains("line 0 of"), "{screen}");
    assert!(!screen.contains("line 79 of"), "{screen}");

    // pgdn moves the arguments rather than the conversation hidden behind them
    harness.press(KeyCode::PageDown).await;
    let screen = harness.screen();
    assert!(!screen.contains("line 0 of"), "{screen}");
    assert!(screen.contains("line 20 of"), "{screen}");
    // the answers do not move with them
    assert!(screen.contains("[y] once"), "{screen}");

    // it stops at the end rather than counting presses: without the clamp, four pages down past
    // the bottom is four pages back up before anything moves
    for _ in 0..8 {
        harness.press(KeyCode::PageDown).await;
    }
    let bottom = harness.screen();
    assert!(bottom.contains("line 79 of"), "{bottom}");
    harness.press(KeyCode::PageUp).await;
    let screen = harness.screen();
    assert_ne!(screen, bottom, "one page up moves");

    // a box too small for the question is still a box somebody has to answer: the arguments go
    // first, then the header, and the answers are the last thing standing
    let tiny = harness.sized(30, 6);
    assert!(tiny.contains("[y] once"), "{tiny}");

    // and the question is still answerable after all that reading
    harness.answer(KeyCode::Char('y')).await;
    harness.settle().await;
    assert!(harness.app.kernel.pending_permissions().is_empty());
}

#[tokio::test]
async fn an_item_that_was_rewritten_can_still_be_read_as_it_was() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("the tool said 500"));
    let id = harness.app.kernel.items()[0].id;
    harness.drain();

    // what `amend revise` does: replaced in place, keeping the number, so the old text exists
    // nowhere except the event that announced it going
    for said in ["the tool said 400", "the tool said 412"] {
        harness
            .app
            .kernel
            .replace(id, said)
            .expect("the item is there");
        harness.drain();
    }

    harness.tab(Tab::Context);
    harness.press(KeyCode::Enter).await;

    // it opens on what the item says now, which is what pressing enter always meant
    let screen = harness.screen();
    assert!(
        screen.contains("to the model │ as stored │ v2 │ v1"),
        "{screen}"
    );
    assert!(screen.contains("the tool said 412"), "{screen}");

    // and the versions are one key away, newest first
    harness.press(KeyCode::Right).await;
    let screen = harness.screen();
    assert!(screen.contains("version 2 of 3"), "{screen}");
    assert!(screen.contains("the tool said 400"), "{screen}");

    harness.press(KeyCode::Right).await;
    let screen = harness.screen();
    assert!(screen.contains("version 1 of 3"), "{screen}");
    assert!(screen.contains("the tool said 500"), "{screen}");

    // left goes back the way it came, and neither key closes the box
    harness.press(KeyCode::Left).await;
    let screen = harness.screen();
    assert!(screen.contains("the tool said 400"), "{screen}");

    // anything else still closes it
    harness.press(KeyCode::Esc).await;
    assert!(harness.app.overlay.is_none());
}

#[tokio::test]
async fn an_item_the_model_does_not_read_in_full_is_shown_as_the_model_gets_it() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("a long and expensive thing"));
    harness.app.kernel.push(ContextItem::user("and another"));
    let items = harness.app.kernel.items();
    let (elided, excluded) = (items[0].id, items[1].id);
    harness.app.kernel.set_state(
        [elided],
        ContextState::Elided,
        Some("summarised elsewhere".into()),
    );
    harness
        .app
        .kernel
        .set_state([excluded], ContextState::Excluded, Some("stale".into()));
    harness.drain();

    // an elided item goes in as a marker: the screen and the request say different things about
    // it, and the request's answer is the one somebody opened this to find
    harness.tab(Tab::Context);
    harness.press(KeyCode::Enter).await;
    let screen = harness.screen();
    assert!(
        screen.contains("[... summarised elsewhere ...]"),
        "{screen}"
    );
    assert!(!screen.contains("a long and expensive thing"), "{screen}");

    // what it still says is on the next page along
    harness.press(KeyCode::Left).await;
    let screen = harness.screen();
    assert!(screen.contains("a long and expensive thing"), "{screen}");

    // an excluded one is not in the request at all, and says so with the reason
    harness.press(KeyCode::Esc).await;
    harness.press(KeyCode::Down).await;
    harness.press(KeyCode::Enter).await;
    let screen = harness.screen();
    assert!(
        screen.contains("this item is not in the request"),
        "{screen}"
    );
    assert!(screen.contains("excluded: stale"), "{screen}");
}

#[tokio::test]
async fn an_edit_carries_what_the_item_used_to_say_to_the_item_that_replaced_it() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("the first draft"));
    harness.drain();

    // `e` supersedes rather than replaces, so the old item keeps its own row - but the new one is
    // where somebody will be looking
    harness.tab(Tab::Context);
    harness.press(KeyCode::Char('e')).await;
    harness.press(KeyCode::Char('!')).await;
    harness.press(KeyCode::Enter).await;
    harness.drain();

    let new = harness.app.kernel.items().last().expect("the edit").id;
    harness.tab(Tab::Context);
    harness.press(KeyCode::End).await;
    harness.press(KeyCode::Enter).await;
    let screen = harness.screen();
    assert!(screen.contains(&format!("[{new}]")), "{screen}");
    assert!(screen.contains("│ v1"), "{screen}");
    harness.press(KeyCode::Right).await;
    let screen = harness.screen();
    assert!(screen.contains("the first draft"), "{screen}");
}

#[tokio::test]
async fn an_undone_rewrite_does_not_leave_the_same_text_on_two_pages() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::user("what it says"));
    let id = harness.app.kernel.items()[0].id;
    harness.drain();
    harness
        .app
        .kernel
        .replace(id, "what it says instead")
        .expect("the item is there");
    harness.drain();
    assert!(harness.app.kernel.undo());
    harness.drain();

    // the version that was put back is now the current one, and a page saying so twice says
    // nothing
    harness.tab(Tab::Context);
    harness.press(KeyCode::Enter).await;
    let screen = harness.screen();
    assert!(screen.contains("what it says"), "{screen}");
    assert!(!screen.contains("v1"), "{screen}");
}

#[tokio::test]
async fn an_item_the_projector_drops_says_so_even_though_its_row_looks_healthy() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({ "where": "here" }))]),
        ModelResponse::text("a bone"),
    ]);
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("dig", "a bone")));
    harness.send("dig").await;
    harness.settle().await;
    harness.tab(Tab::Context);

    // a turn recorded in the conventional three slots keeps its calls in its kind rather than in
    // its content, and reading only the content showed an empty box for a turn that was nothing
    // but calls
    harness.press(KeyCode::Home).await;
    harness.press(KeyCode::Down).await;
    harness.press(KeyCode::Enter).await;
    let screen = harness.screen();
    assert!(screen.contains(r#"dig({"where":"here"})"#), "{screen}");
    harness.press(KeyCode::Esc).await;

    // taking the turn out orphans its result, which the projector then has to drop - and nothing
    // about the result's own row says so, because as far as its state goes it is going
    let turn = harness.app.kernel.items()[1].id;
    harness
        .app
        .kernel
        .set_state([turn], ContextState::Excluded, Some("gone".into()));
    harness.drain();

    harness.press(KeyCode::Home).await;
    harness.press(KeyCode::Down).await;
    harness.press(KeyCode::Down).await;
    let row = harness.screen();
    assert!(
        row.lines()
            .any(|line| line.contains("tool_result") && line.contains(" · ")),
        "the row still reads as active: {row}"
    );

    harness.press(KeyCode::Enter).await;
    let screen = harness.screen();
    assert!(
        screen.contains("this item is not in the request"),
        "{screen}"
    );
    assert!(screen.contains("orphaned tool result"), "{screen}");
    // and what it holds is still one key away
    harness.press(KeyCode::Right).await;
    let screen = harness.screen();
    assert!(screen.contains("a bone"), "{screen}");
}

#[tokio::test]
async fn a_conversation_stays_where_somebody_scrolled_it_while_the_model_keeps_writing() {
    let mut harness = Harness::new([]);
    let write = |harness: &mut Harness, from: usize, to: usize| {
        for n in from..to {
            harness.app.on_event(Event::ModelDelta {
                delta: Delta::Text(format!("line {n}\n\n")),
            });
        }
    };

    write(&mut harness, 0, 80);
    let screen = harness.screen();
    assert!(screen.contains("line 79"), "{screen}");

    // scroll back to read something from further up
    harness.press(KeyCode::PageUp).await;
    harness.press(KeyCode::PageUp).await;
    let read = harness.screen();
    let anchor = read
        .match_indices("line ")
        .next()
        .map(|(at, _)| {
            read[at..]
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .expect("something is on the screen");
    assert!(!read.contains("line 79"), "{read}");

    // the model writes another eighty lines under it. Every fragment used to yank the window back
    // to the newest of them, which made reading anything it had said earlier impossible until the
    // turn was over
    write(&mut harness, 80, 160);
    let after = harness.screen();
    assert!(after.contains(&anchor), "the view moved: {after}");
    assert!(!after.contains("line 159"), "{after}");
    // and the window says there is more underneath, rather than just stopping
    assert!(after.contains("ctrl+e follows"), "{after}");

    // ctrl+e is the way back down without paging through what arrived in between
    harness.chord(KeyCode::Char('e')).await;
    let back = harness.screen();
    assert!(back.contains("line 159"), "{back}");

    // and it keeps following from there
    write(&mut harness, 160, 200);
    let screen = harness.screen();
    assert!(screen.contains("line 199"), "{screen}");
}

#[tokio::test]
async fn a_message_of_your_own_takes_you_back_to_the_bottom() {
    let mut harness = Harness::new([]);
    for n in 0..80 {
        harness.app.on_event(Event::ModelDelta {
            delta: Delta::Text(format!("line {n}\n\n")),
        });
    }
    harness.screen();
    harness.press(KeyCode::PageUp).await;
    harness.press(KeyCode::PageUp).await;
    assert!(!harness.screen().contains("line 79"));

    // typing something and sending it is somebody saying they are done reading back
    harness.send("what about this").await;
    let screen = harness.screen();
    assert!(screen.contains("> what about this"), "{screen}");
}

#[tokio::test]
async fn the_next_request_says_why_there_is_none_rather_than_reporting_a_fault() {
    let mut harness = Harness::new([]);

    // nothing has been said yet, and `the context projects to an empty request` is the runtime's
    // sentence for a rule it is enforcing correctly - it reads as a malfunction to somebody who
    // has simply not typed anything
    harness.send("/request").await;
    let screen = harness.screen();
    assert!(screen.contains("nothing yet"), "{screen}");
    assert!(!screen.contains("projects to an empty request"), "{screen}");
    harness.press(KeyCode::Esc).await;

    // a context that is not empty and still sends nothing is a different answer, and it is the
    // one moment the list of what was left out is worth most - which is when it used to be
    // dropped, because the error returned before the list was built
    harness
        .app
        .kernel
        .push(ContextItem::user("what about this"));
    let id = harness.app.kernel.items()[0].id;
    harness.app.kernel.set_state(
        [id],
        ContextState::Excluded,
        Some("thought better of it".into()),
    );
    harness.drain();

    harness.send("/request").await;
    let screen = harness.screen();
    assert!(
        screen.contains("excluded: thought better of it"),
        "{screen}"
    );
    assert!(screen.contains("not one of the 1 item(s)"), "{screen}");
}

#[tokio::test]
async fn the_first_line_of_a_session_names_keys_that_do_what_it_says() {
    let mut harness = Harness::new([]);
    harness.app.say(Speaker::Note, ui::GREETING);
    let screen = harness.screen();
    assert!(screen.contains("ctrl+t"), "{screen}");

    // it opened with `tab moves to the context`, which tab has never done: on the chat tab there
    // is nothing to move the focus to
    harness.press(KeyCode::Tab).await;
    assert_eq!(harness.app.tab, Tab::Chat, "tab does not open a tab");

    // what it names now is what happens, in the order it names it
    harness.chord(KeyCode::Char('t')).await;
    assert_eq!(harness.app.tab, Tab::Context);
    harness.chord(KeyCode::Char('t')).await;
    assert_eq!(harness.app.tab, Tab::Trace);
    harness
        .app
        .on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT))
        .await;
    assert_eq!(harness.app.tab, Tab::Chat);

    // and every key it names is one F1 lists, which is itself checked against the code
    for key in ["ctrl+t", "alt+1", "ctrl+p", "F1"] {
        assert!(
            ui::HELP.contains(key) || ui::HELP.contains(&key.to_lowercase()),
            "the greeting offers `{key}` and the help does not mention it"
        );
    }
}

#[tokio::test]
async fn a_session_that_is_working_says_so_with_something_that_moves() {
    let mut harness = Harness::new([]);

    // nothing is happening, so nothing moves: the marker is not decoration on a resting line
    assert!(!harness.screen().contains('•'), "{}", harness.screen());

    harness.app.busy = true;
    let lit = |harness: &mut Harness, ms: u64| -> usize {
        harness.app.since = std::time::Instant::now()
            .checked_sub(Duration::from_millis(ms))
            .expect("a clock with some road behind it");
        harness.dots()
    };

    // three dots, and which one is lit comes from the clock rather than from a frame counter -
    // so it moves at the same rate whatever the screen is doing, and stops where it is if the
    // screen stops being drawn at all, which is the one thing it is there to make visible
    let first = lit(&mut harness, 0);
    let second = lit(&mut harness, 300);
    let third = lit(&mut harness, 600);
    assert_ne!(first, second, "the lit dot did not move");
    assert_ne!(second, third, "the lit dot did not move on");
    assert_eq!(lit(&mut harness, 840), first, "and it goes round");

    // a short turn stays clean; a long one says how long, because "is it hung?" is the question
    // the marker raises and cannot answer on its own
    harness.app.since = std::time::Instant::now();
    assert!(!harness.screen().contains("0s ·"), "{}", harness.screen());
    harness.app.since = std::time::Instant::now()
        .checked_sub(Duration::from_secs(42))
        .expect("a clock");
    assert!(harness.screen().contains("42s"), "{}", harness.screen());

    // and it goes when the work does
    harness.app.busy = false;
    assert!(!harness.screen().contains('•'), "{}", harness.screen());
}

#[tokio::test]
async fn a_refused_model_is_told_which_kind_of_refusal_it_was() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "cmd": "cat /etc/shadow" }),
        )]),
        ModelResponse::text("understood"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("shell", "it ran!").with_capabilities([Capability::Shell]),
    ));
    // a standing rule rather than a moment's hesitation
    harness
        .app
        .policy
        .set(&Subject::Capability(Capability::Shell), Verdict::Deny);

    harness.send("read the shadow file").await;
    harness.settle().await;

    // the result the model reads names the rule that did it, and says the rule is standing - so
    // it can stop asking rather than rephrasing the same call. Before this the whole of what
    // reached the model was `the call was not permitted`, and the reason was on the screen only
    let result = harness
        .app
        .kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .expect("a refusal is a result like any other");
    let said = result.content.to_text().into_owned();
    assert!(said.contains("`shell`"), "it names what refused it: {said}");
    assert!(
        said.contains("a standing rule rather than an answer to this one call"),
        "{said}"
    );

    // and the person is told too: handing the reason out once meant whichever asked first got it
    let screen = harness.screen();
    assert!(screen.contains("refused by"), "{screen}");
}

#[tokio::test]
async fn a_call_refused_once_at_the_prompt_says_so_rather_than_naming_a_rule() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({ "where": "here" }))]),
        ModelResponse::text("understood"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::Shell]),
    ));

    harness.send("dig").await;
    harness.settle().await;
    harness.answer(KeyCode::Char('n')).await;
    harness.settle().await;

    // nothing standing was decided, so the model is told the opposite thing: this call was
    // refused, and a different approach may well be allowed
    let result = harness
        .app
        .kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .expect("a refusal is a result like any other");
    let said = result.content.to_text().into_owned();
    assert!(
        said.contains("an answer to this call rather than a standing rule"),
        "{said}"
    );
}

#[tokio::test]
async fn every_tool_says_what_it_is_and_what_each_argument_is_for() {
    let harness = Harness::new([]);
    for tool in kamchatka::tools::builtin(
        kamchatka::tools::Shell {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            policy: harness.app.policy.clone(),
            confiner: Some(std::path::PathBuf::from("/self")),
        },
        kamchatka::sandbox::Reach {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            confined: true,
        },
    ) {
        harness.app.kernel.add_tool(tool);
    }
    let _offered = kamchatka::introspect::install(&harness.app.kernel);

    for spec in harness.app.kernel.tool_specs() {
        assert!(
            !spec.description.trim().is_empty(),
            "{} says nothing",
            spec.id
        );
        // long enough to be useful, short enough to be read: the two that manage a context are
        // five actions each and earn their length; a file tool that needed this much would be
        // describing something it should not be doing
        assert!(
            spec.description.len() < 1_500,
            "{} is {} chars",
            spec.id,
            spec.description.len()
        );

        // every argument says what it is for. A bare `{"type": "string"}` leaves a model to
        // guess whether a path is absolute, what `old` has to match, what a `select` accepts -
        // and a guess costs a turn each time
        let properties = spec.schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{} has no object schema", spec.id));
        for (name, field) in properties {
            let said = field
                .get("description")
                .and_then(|text| text.as_str())
                .is_some_and(|text| !text.trim().is_empty());
            assert!(
                said || field.get("enum").is_some(),
                "`{}`'s `{name}` has nothing to say for itself",
                spec.id
            );
        }

        // and none of it is written in this program's own vocabulary. What reads these has never
        // heard of the program, the crates it is built from, or a terminal somebody is sitting
        // at; a word like that reads to a model as a thing it is supposed to recognise
        let text = format!("{} {}", spec.description, spec.schema);
        for insider in ["at the terminal", "nachalnik", "kamchatka", "the kernel"] {
            assert!(
                !text.contains(insider),
                "`{}` says `{insider}` to a reader who has never heard of it",
                spec.id
            );
        }
    }
}

#[tokio::test]
async fn the_pane_says_what_an_item_costs_now_and_what_it_is_holding_back() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("what does it do?"));
    harness.app.kernel.push(ContextItem::file(
        "src/parser.rs",
        "fn parse() {}\n".repeat(20),
    ));
    let items = harness.app.kernel.items();
    harness.app.kernel.set_state(
        [items[1].id],
        ContextState::Elided,
        Some("compacted to make room".into()),
    );
    harness.drain();
    harness.tab(Tab::Context);

    let screen = harness.screen();
    let elided = screen
        .lines()
        .find(|line| line.contains("src/parser.rs"))
        .expect("the item is listed");
    let held = harness
        .app
        .kernel
        .item(items[1].id)
        .expect("still there")
        .tokens;

    // an elided item holds what it always held and costs what its marker costs, and the column
    // headed `sending` used to show the first of those - answering a question nobody asked while
    // the status line beside it answered the right one
    let sending: Vec<usize> = elided
        .split_whitespace()
        .filter_map(|word| word.replace(',', "").parse().ok())
        .collect();
    assert!(
        sending.contains(&held),
        "the held column reports what it holds: {elided}"
    );
    assert!(
        sending.iter().any(|n| *n > 0 && *n < held),
        "and the sending column reports the marker, which is smaller: {elided}"
    );

    // the two columns are what the status line is made of: everything sending, and everything held
    let sent: usize = harness.app.costs().values().sum();
    let status = harness.screen();
    assert!(status.contains(&format!("~{sent} tokens")), "{status}");
    assert!(status.contains(&format!("{held} held back")), "{status}");

    // and the label column is as wide as the widest label, not a fixed twenty-six
    let header = screen.lines().nth(1).expect("the header row");
    let gap = header.find("kind").expect("the kind column") - header.find("label").expect("label");
    assert!(gap < 20, "twenty columns of nothing: {header}");
}

#[tokio::test]
async fn the_trace_says_what_each_event_carries_and_which_step_was_slow() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({ "where": "here" }))]),
        ModelResponse::text("a bone"),
    ]);
    harness
        .app
        .kernel
        .add_tool(Arc::new(ConstTool::new("dig", "a bone")));
    harness.send("dig").await;
    harness.settle().await;

    // the one event that carries content, and the only place the old text survives once the undo
    // window closes. It printed its own name against an empty line
    let id = harness.app.kernel.items()[0].id;
    harness
        .app
        .kernel
        .replace(id, "dig, please")
        .expect("the item is there");
    assert!(harness.app.kernel.undo());
    harness.drain();

    harness.tab(Tab::Trace);
    let screen = harness.sized(110, 30);
    assert!(screen.contains("context.replaced"), "{screen}");
    assert!(screen.contains("it said: dig"), "{screen}");
    assert!(screen.contains("context.undone"), "{screen}");
    assert!(screen.contains("put back as they were"), "{screen}");

    // and nothing in the pane is a name with nothing beside it
    for line in screen.lines().filter(|line| line.contains('.')) {
        let Some(name) = line.split_whitespace().find(|word| {
            word.contains('.') && word.chars().all(|c| c.is_ascii_lowercase() || c == '.')
        }) else {
            continue;
        };
        let after = line.split_once(name).map(|(_, rest)| rest).unwrap_or("");
        assert!(
            after.trim_end_matches(['│', ' ', '█']).trim().len() > 1,
            "`{name}` says nothing: {line}"
        );
    }

    // the gap column: blank for the frame-to-frame majority, and there for the few that waited
    harness.tab(Tab::Trace);
    let last = harness.app.trace.len() - 1;
    harness.app.trace[last].at = std::time::Instant::now()
        .checked_add(Duration::from_secs(4))
        .expect("a clock");
    let screen = harness.sized(110, 30);
    assert!(screen.contains("+4.0s"), "{screen}");

    // and a window with no room for it spends its columns on what happened instead
    let narrow = harness.sized(46, 30);
    assert!(!narrow.contains("+4.0s"), "{narrow}");
}

#[tokio::test]
async fn a_table_too_wide_for_the_window_keeps_its_shape() {
    let mut harness = Harness::new([]);
    harness.app.say(
        Speaker::Model,
        "Here is a comparison:\n\n\
         | seam | you provide | the kernel provides |\n\
         | --- | --- | --- |\n\
         | `Provider` | a model | the request, verbatim |\n\
         | `Tool` | what it can do | the schema and the gating |\n\n\
         and some text after it.\n",
    );
    harness.tab(Tab::Chat);

    // wide enough: the box is drawn at the width its contents want, and the backticks are gone
    let roomy = harness.sized(90, 20);
    assert!(roomy.contains("│ seam     │ you provide    │"), "{roomy}");
    assert!(
        !roomy.contains('`'),
        "inline code is styled, not spelled: {roomy}"
    );

    // too narrow: the columns give, not the borders. Every line of the table is the same width
    // and has a border at each end - the markdown renderer's own table was wrapped like a
    // sentence, so half of a border arrived on the next line and the whole thing came apart
    let narrow = harness.sized(46, 24);
    // the window's own frame taken off, so that what is left is the table's
    let rows: Vec<String> = narrow
        .lines()
        .map(|line| {
            let mut cells: Vec<char> = line.chars().collect();
            cells.resize(46, ' ');

            cells[1..45]
                .iter()
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    let table: Vec<&String> = rows
        .iter()
        .filter(|line| line.starts_with(['┌', '│', '├', '└']))
        .collect();
    assert!(table.len() > 6, "the table is drawn: {narrow}");
    for line in &table {
        assert!(
            line.ends_with(['┐', '│', '┤', '┘']),
            "a row that does not close: {line:?} in {narrow}"
        );
        assert_eq!(
            line.chars().count(),
            table[0].chars().count(),
            "a row of a different width: {line:?} in {narrow}"
        );
    }
    // and nothing was lost to make it fit
    assert!(narrow.contains("verbatim"), "{narrow}");
    assert!(narrow.contains("gating"), "{narrow}");
}

#[tokio::test]
async fn a_table_is_only_a_table_where_one_was_written() {
    let mut harness = Harness::new([]);
    harness.app.say(
        Speaker::Model,
        "```md\n| not | a | table |\n| --- | --- | --- |\n| it is | in | a fence |\n```\n\n\
         | left | middle | right |\n| :--- | :----: | ----: |\n| a | b |\n| c | d | e | f |\n",
    );
    harness.tab(Tab::Chat);
    let screen = harness.sized(80, 24);

    // a table inside a fence is a code block, pipes and all
    assert!(screen.contains("| not | a | table |"), "{screen}");

    // the colons say which way a column reads, and a short row is squared up rather than left
    // ragged; a long one is cut to the columns the header declared
    assert!(screen.contains("│ a    │   b    │       │"), "{screen}");
    assert!(screen.contains("│ c    │   d    │     e │"), "{screen}");
}

#[tokio::test]
async fn a_stopped_command_says_so_where_a_limit_cannot_cut_it() {
    // an output limit cuts from the end, so a notice appended after the standard error is the
    // first thing a long-running command loses - and it is the line that explains why the output
    // stops mid-sentence. The first line survives anything
    let interrupted = "exit: stopped before it finished, at the request of the person you are \
                       working with; what is below is what it had said by then\n--- stdout ---\n";
    let mut content = nachalnik::Content::text(format!("{interrupted}{}", "x".repeat(4_000)));
    content.truncate_to(200).expect("it is over the limit");

    let said = content.to_text();
    assert!(said.contains("stopped before it finished"), "{said}");
    assert!(said.contains("truncated by an output limit"), "{said}");
}
