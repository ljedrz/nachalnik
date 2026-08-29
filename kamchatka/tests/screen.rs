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

    // any other key closes it
    harness.press(KeyCode::Esc).await;
    assert!(harness.app.overlay.is_none());
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
        Some(kamchatka::app::Overlay::Permission)
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
    let (header, _) = harness.style_of("tokens");
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
        .position(|line| line.contains("tokens"))
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
    let screen = harness.screen();
    assert!(screen.contains("notes.json"), "{screen}");

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
    assert!(
        harness.screen().contains("replaced"),
        "{}",
        harness.screen()
    );

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
    let source = include_str!("../src/app.rs");
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
