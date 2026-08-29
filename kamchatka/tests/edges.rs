//! The program at the edges of its own state: every window size, every tab, every key, and the
//! states nobody drives it into on purpose.
//!
//! note: a terminal program is drawn at whatever size the window happens to be, including the
//! sizes it passes through while somebody drags an edge, and it is driven by whatever key somebody
//! presses next rather than by the one the screen is expecting. Neither of those is a scenario
//! worth writing out one at a time, and both are cheap to sweep: a frame that panics takes the
//! session with it, which is the one failure this program cannot report.

use std::{sync::Arc, time::Duration};

use kamchatka::{
    app::{App, Overlay, Speaker, Tab},
    provider::OpenAiCompatible,
    tools::Careful,
    ui,
};
use nachalnik::{
    Config, ContextItem, Kernel,
    test::{ConstTool, ScriptedProvider},
};
use ratatui::{Terminal, backend::TestBackend};

fn app() -> App {
    let kernel = Kernel::new(Config::default());
    let policy = Arc::new(Careful::new());
    kernel.set_provider(Arc::new(ScriptedProvider::new([])));
    kernel.set_policy(policy.clone());
    kernel.add_tool(Arc::new(ConstTool::new("read", "hello")));
    let (outcomes, _keep) = tokio::sync::mpsc::unbounded_channel();
    std::mem::forget(_keep);

    let provider = Arc::new(OpenAiCompatible::new("scripted", "http://127.0.0.1:1", ""));
    let mut app = App::new(kernel, policy, provider, outcomes);

    app.say(Speaker::User, "what does this do?");
    app.say(
        Speaker::Model,
        "# a heading\n\nsome **bold** prose that runs on for a while so that it has to be \
         wrapped somewhere, and then a block:\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n\n\
         and a list:\n\n- one\n- two\n",
    );
    app.say(Speaker::Result, "a line\nanother line\n");
    // the shapes a model's answer actually arrives in, including the ones it never finishes: a
    // block still streaming in has no closing fence, and one in a language nothing has heard of
    // still has to be drawn
    app.say(
        Speaker::Model,
        "```klingon\nwhat is this\n```\n\n```rust\nfn unfinished() {\n",
    );
    app.say(Speaker::Result, format!("{}\r\n", "x".repeat(5_000)));
    app.kernel.push(ContextItem::user("what does this do?"));
    app.kernel
        .push(ContextItem::file("src/some/deep/path.rs", "fn parse() {}").pinned());

    app
}

fn draw(app: &mut App, width: u16, height: u16) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a backend");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("a frame");
}

#[test]
fn every_tab_draws_at_every_size() {
    let mut app = app();

    for tab in Tab::ALL {
        app.show(tab);
        for width in 1..=60u16 {
            for height in 1..=20u16 {
                draw(&mut app, width, height);
            }
        }
    }
}

#[test]
fn a_session_with_nothing_in_it_draws_at_every_size() {
    let kernel = Kernel::new(Config::default());
    let policy = Arc::new(Careful::new());
    kernel.set_policy(policy.clone());
    let (outcomes, keep) = tokio::sync::mpsc::unbounded_channel();
    std::mem::forget(keep);
    let provider = Arc::new(OpenAiCompatible::new("scripted", "http://127.0.0.1:1", ""));
    let mut app = App::new(kernel, policy, provider, outcomes);

    for tab in Tab::ALL {
        app.show(tab);
        for width in 1..=60u16 {
            for height in 1..=20u16 {
                draw(&mut app, width, height);
            }
        }
    }
}

/// Text that is not one byte to the character, in every place this program puts text.
///
/// note: the transcript, the context table and a tool's output all go through this program's own
/// clipping and wrapping rather than through a widget's, and every one of those is written in
/// characters where a terminal cell is not one. A byte index into the middle of one of these is a
/// panic, and the shortest way to find out is to draw them.
#[test]
fn text_that_is_not_ascii_draws_at_every_size() {
    let mut app = app();

    app.say(Speaker::User, "把这个仓库讲清楚 🌋 — czy to działa?");
    app.say(
        Speaker::Model,
        "**大丈夫**: `фн main()` 🚀🚀🚀\n\n```rust\nlet s = \"日本語のコメント\";\n```\n",
    );
    app.say(Speaker::Result, "🌋".repeat(400));
    app.kernel.push(ContextItem::file(
        "файлы/日本語/very-deep.rs",
        "🌋".repeat(200),
    ));
    app.kernel.push(ContextItem::user("🌋"));
    app.input
        .insert_str("🌋 半角 and a very long line ".repeat(6));

    for tab in Tab::ALL {
        app.show(tab);
        for width in 1..=60u16 {
            for height in 1..=20u16 {
                draw(&mut app, width, height);
            }
        }
    }
}

/// Scrolled past both ends of everything, which is where a subtraction goes round.
#[test]
fn every_tab_draws_scrolled_past_its_own_end() {
    let mut app = app();

    for scroll in [0, 1, usize::MAX] {
        app.scroll = scroll;
        app.trace_scroll = scroll;
        app.selected = scroll;
        app.chosen = scroll;
        app.overlay = Some(Overlay::Text {
            title: "the keys".to_owned(),
            body: ui::HELP.to_owned(),
            scroll,
        });
        for tab in Tab::ALL {
            app.show(tab);
            for (width, height) in [(1, 1), (20, 3), (40, 8), (80, 24), (200, 60)] {
                draw(&mut app, width, height);
            }
        }
    }
}

#[test]
fn an_overlay_draws_at_every_size() {
    let mut app = app();

    app.overlay = Some(Overlay::Text {
        title: "the keys".to_owned(),
        body: ui::HELP.to_owned(),
        scroll: 0,
    });
    for width in 1..=60u16 {
        for height in 1..=20u16 {
            draw(&mut app, width, height);
        }
    }
}

#[tokio::test]
async fn a_permission_question_draws_at_every_size() {
    use nachalnik::{ModelResponse, test::call};
    use serde_json::json;

    let kernel = Kernel::new(Config::default());
    let policy = Arc::new(Careful::new());
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("1", "read", json!({ "path": ".env" }))]),
    ])));
    kernel.set_policy(policy.clone());
    kernel.add_tool(Arc::new(ConstTool::new("read", "hello")));
    let (outcomes, _keep) = tokio::sync::mpsc::unbounded_channel();
    std::mem::forget(_keep);
    let provider = Arc::new(OpenAiCompatible::new("scripted", "http://127.0.0.1:1", ""));
    let mut app = App::new(kernel, policy, provider, outcomes);

    app.kernel.push(ContextItem::user("read the env"));
    let _ = tokio::time::timeout(Duration::from_secs(5), app.kernel.turn()).await;
    app.overlay = Some(Overlay::Permission);

    for width in 1..=60u16 {
        for height in 1..=20u16 {
            draw(&mut app, width, height);
        }
    }
}

/// Every key this program looks at, pressed at every tab, in a session with nothing in it.
///
/// note: an empty context is the state a program is in for the first few seconds of its life and
/// the one nobody drives it in, because the way to get to the context tab is to have something to
/// look at. Every key there acts on a selected row, and there is none.
#[tokio::test]
async fn every_key_at_every_tab_with_nothing_to_act_on() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let keys: Vec<KeyCode> = ('a'..='z')
        .chain('A'..='Z')
        .chain('0'..='9')
        .chain([' ', '?', '/', '.'])
        .map(KeyCode::Char)
        .chain([
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Tab,
            KeyCode::F(1),
        ])
        .collect();

    for empty in [true, false] {
        let mut app = match empty {
            true => {
                let kernel = Kernel::new(Config::default());
                let policy = Arc::new(Careful::new());
                kernel.set_provider(Arc::new(ScriptedProvider::new([])));
                kernel.set_policy(policy.clone());
                let (outcomes, keep) = tokio::sync::mpsc::unbounded_channel();
                std::mem::forget(keep);
                let provider = Arc::new(OpenAiCompatible::new("none", "http://127.0.0.1:1", ""));
                App::new(kernel, policy, provider, outcomes)
            }
            false => app(),
        };

        for tab in Tab::ALL {
            for focus in [false, true] {
                for key in &keys {
                    app.show(tab);
                    if focus {
                        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                            .await;
                    }
                    app.on_key(KeyEvent::new(*key, KeyModifiers::NONE)).await;
                    draw(&mut app, 80, 24);
                    draw(&mut app, 20, 5);
                }
            }
        }
    }
}

/// The status line names the address as well as the model, and the address it names is the
/// authority rather than the whole URL, which would not fit beside the rest of the line.
///
/// note: hand-parsed in `provider::host`, so the shapes worth pinning are the ones a person
/// actually types at `/provider`: a bare host, a path to strip, a port to keep, and the ones that
/// are not URLs at all - which come back as they came, a status line showing something odd being
/// better than one showing nothing.
#[test]
fn the_address_on_the_status_line_is_the_host() {
    let cases = [
        ("https://openrouter.ai/api/v1", "openrouter.ai"),
        ("http://localhost:11434/v1", "localhost:11434"),
        ("https://openrouter.ai", "openrouter.ai"),
        ("http://127.0.0.1:1", "127.0.0.1:1"),
        ("localhost:8080/v1", "localhost:8080"),
        ("", ""),
        ("https://", "https://"),
    ];

    for (endpoint, expected) in cases {
        let provider = OpenAiCompatible::new("m", endpoint, "");
        assert_eq!(provider.host(), expected, "for endpoint {endpoint:?}");
    }
}

/// The prompt wraps a long message instead of sliding it sideways under the border, and the box
/// grows to hold every row of it.
///
/// note: `ui::wrapped_rows` counts the rows and `TextArea` draws them, and the two are separate
/// pieces of code that have to agree - so this asserts against the drawn screen rather than
/// against the count. Every word typed has to be readable somewhere on it.
#[test]
fn a_long_message_wraps_in_the_prompt_rather_than_scrolling_out_of_it() {
    let mut app = app();
    let typed = "explain why the projector drops a tool call whose result was pruned, \
                 and what the repair list says about it afterwards";
    app.input.insert_str(typed);

    let mut terminal = Terminal::new(TestBackend::new(60, 24)).expect("a backend");
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .expect("a frame");
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<Vec<_>>()
        .chunks(60)
        .map(|row| format!("{}\n", row.concat()))
        .collect();

    // nothing typed has gone off the side
    for word in typed.split_whitespace() {
        assert!(
            screen.contains(word),
            "`{word}` is not on the screen:\n{screen}"
        );
    }
    // and it really wrapped: at 60 columns that text cannot be on one row
    assert!(
        ui::wrapped_rows(app.input.lines(), 58) > 1,
        "it should have taken several rows"
    );
}

/// The row count is what the box is sized from, so the arithmetic is worth pinning on its own.
#[test]
fn rows_are_counted_the_way_the_widget_wraps_them() {
    let row = |s: &str, w: usize| ui::wrapped_rows(&[s.to_owned()], w);

    assert_eq!(row("", 10), 1, "an empty line is still a row");
    assert_eq!(row("short", 10), 1);
    assert_eq!(row("exactly-10", 10), 1, "a line that just fits is one row");
    assert_eq!(row("aaaa bbbb cccc", 10), 2, "broken at the space");
    // a word with nowhere to break is split rather than left to overflow
    assert_eq!(row(&"x".repeat(25), 10), 3);
    // several lines are the sum of their own rows
    assert_eq!(
        ui::wrapped_rows(&["aaaa bbbb cccc".to_owned(), "short".to_owned()], 10),
        3
    );
    // a width of nothing is not a division by zero
    assert_eq!(ui::wrapped_rows(&["anything".to_owned()], 0), 1);
}

/// `ui::wrapped_rows` and `TextArea`'s own wrapping are two pieces of code that have to agree
/// about how many rows a message takes: the first sizes the box, the second fills it. If they
/// drift - a version bump changing where the widget breaks - the box is the wrong height and
/// either text is hidden or a blank row sits under it. So this asks the widget.
#[test]
fn the_row_count_agrees_with_what_the_widget_actually_draws() {
    use ratatui::{Terminal, backend::TestBackend};
    use ratatui_textarea::{TextArea, WrapMode};

    let texts = [
        "short",
        "a line that is comfortably longer than the box it is being typed into",
        // the case a search for spaces gets wrong: `/` is a word bound, so the widget breaks
        // inside the path and packs more onto the row than whitespace alone would allow
        "and what /a/very/long/path/that/cannot/be/broken/at/a/space/anywhere.rs has to do",
        "https://example.com/an/extremely/long/url/with/no/spaces/in/it/at/all/whatsoever",
        &"x".repeat(200),
        "réservé naïve façade — em dashes and accents, which are not one byte each",
        "日本語のテキストは一文字が二列ぶんの幅になる",
    ];

    for text in texts {
        for width in [12usize, 20, 37, 60, 79] {
            let mut area = TextArea::from([text.to_owned()]);
            area.set_wrap_mode(WrapMode::WordOrGlyph);

            // tall enough that nothing is cut off, so the last row with anything on it is the
            // last row the widget used
            let mut terminal =
                Terminal::new(TestBackend::new(width as u16, 64)).expect("a backend");
            terminal
                .draw(|frame| frame.render_widget(&area, frame.area()))
                .expect("a frame");

            let buffer = terminal.backend().buffer().clone();
            let drawn = (0..64)
                .filter(|y| (0..width).any(|x| buffer[(x as u16, *y as u16)].symbol().trim() != ""))
                .count()
                .max(1);

            assert_eq!(
                ui::wrapped_rows(area.lines(), width),
                drawn,
                "at width {width} for {text:?}"
            );
        }
    }
}
