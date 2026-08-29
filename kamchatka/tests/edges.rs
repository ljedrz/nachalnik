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
