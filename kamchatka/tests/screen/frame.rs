//! The window itself: the tabs, the borders, the scrollbars, the keys and the help.
//!
//! note: the part of the screen that is the same whatever is being drawn in it. A frame that
//! panics takes the session with it, and a tab drawn under another one is a tab nobody can read,
//! so the geometry is worth its own tests - and `tests/edges.rs` sweeps every window size that
//! these do not.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kamchatka::{
    app::{Focus, Speaker, Tab},
    tools::Limits,
    ui,
};
use nachalnik::{ContextItem, ModelResponse};
use ratatui::style::Color;

use crate::harness::Harness;

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
    //
    // note: from the COMMANDS section rather than from every line beginning with a slash, because
    // `/` is also a *key* - it opens the search box on two panes, and is listed under each of
    // them. Those are not commands and are supposed to appear twice; scanning the whole file for
    // a leading slash could not tell the two kinds apart and read a correctly documented key as a
    // command listed twice.
    //
    // the whole left column, not the first word: `/tools` and `/tools drop ID` are two entries
    // for one command and belong in here twice
    let commands: Vec<&str> = ui::HELP
        .lines()
        .skip_while(|line| !line.contains("COMMANDS"))
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
    let offered = kamchatka::introspect::install(&harness.app.kernel, Limits::default());
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

/// A tab with no prompt on it takes the keys, and `tab` is the way back to the one that has one.
///
/// note: this used to assert the opposite, and the reason it did was real: `a`, `n`, `r` and `d`
/// are bare letters on the permissions tab and every one of them changes something, so a command
/// typed at the prompt handing the next keystroke to the tab was a trap. What made it a trap was
/// that the prompt was still *drawn* there, looking exactly as typeable as it does anywhere else,
/// with only the focus - invisible except as a border colour - deciding which of the two a letter
/// meant. The box is gone from this tab now, so the question a person has to answer before
/// pressing a letter is one the screen answers on its own.
#[tokio::test]
async fn a_tab_with_no_prompt_on_it_takes_the_keys() {
    let mut harness = Harness::new([]);

    harness.send("/policy").await;
    assert_eq!(harness.app.tab, Tab::Permissions);
    assert_eq!(harness.app.focus, Focus::Body);
    assert!(!harness.app.prompted(), "and there is nowhere to type");
    assert!(
        !harness.screen().contains(" you "),
        "so the prompt is not drawn: {}",
        harness.screen()
    );

    // and the way back to typing is one key, from any of the three
    harness.press(KeyCode::Tab).await;
    assert_eq!(harness.app.tab, Tab::Chat);
    assert_eq!(harness.app.focus, Focus::Input);
    assert!(harness.app.prompted());
}

/// Every tab is framed the same, and the box with the keys in it is the one that says so.
///
/// note: the chat tab used to be the one screen in the program with no yellow on it anywhere. The
/// window border went yellow when the keys were on the tab's body, and on the chat tab they never
/// are - `Focus::Body` there is the pinned question, which has a box of its own - so the tab a
/// session is mostly spent on could not look open while the other three did. The prompt did not
/// make up for it in white: against grey that is a difference in brightness rather than in hue.
#[tokio::test]
async fn every_window_is_framed_the_same_and_the_keys_say_where_they_are() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file("notes.txt", "the wrong note"));

    // the open window is framed the same on the tab with a prompt and on the tab without one ...
    harness.tab(Tab::Chat);
    assert_eq!(harness.app.focus, Focus::Input);
    assert_eq!(
        harness.corners(),
        vec![Color::Yellow, Color::Yellow],
        "the window is framed, and the prompt under it has the keys"
    );

    harness.tab(Tab::Context);
    assert_eq!(harness.app.focus, Focus::Body);
    assert_eq!(harness.corners(), vec![Color::Yellow]);

    // ... and while an item is being edited the prompt is the one that answers "where do the keys
    // go?", which it could not do while an edit was yellow whether it had them or not
    harness.press(KeyCode::Char('e')).await;
    assert_eq!(harness.corners(), vec![Color::Yellow, Color::Yellow]);
    harness.press(KeyCode::Tab).await;
    assert_eq!(harness.app.focus, Focus::Body);
    assert_eq!(harness.corners(), vec![Color::Yellow, Color::Gray]);
    // and the box that has lost them says how to get back, the way the prompt's own title does
    assert!(
        harness.flat().contains("editing [1] · tab"),
        "{}",
        harness.flat()
    );
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
    let source = include_str!("../../src/app/command.rs").replace("\r\n", "\n");
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
