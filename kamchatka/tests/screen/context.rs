//! The context tab: what it lists, what each row says, and what changing a row does.
//!
//! note: the tab this program exists for, so it gets the most of these. What is being checked
//! throughout is that the screen and the next request agree - a row that says an item is going
//! is a row whose item is in `preview_request`, and one that says it is not names the reason in
//! the projector's own words rather than in the screen's.

use std::sync::Arc;

use crossterm::event::KeyCode;
use kamchatka::app::{Focus, Tab};
use nachalnik::{
    Calibration, ContextItem, ContextState, ModelResponse,
    test::{ConstTool, call},
};
use serde_json::json;

use crate::harness::Harness;

/// A figure with its thousands separated, the way the pane writes one.
///
/// note: `thousands` is the pane's own and not something an integration test can reach, so this
/// is that rule written out again - once, here, rather than inline in each test that needs it.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }

    out
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
async fn an_item_that_is_not_going_into_the_request_says_why_where_it_is_listed() {
    let mut harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::file("a.rs", "one"));

    harness.send("/prune files").await;
    harness.tab(Tab::Context);

    // "why is that out?" is a question about the thing you are looking at, so it is answered
    // on its row rather than only in the request preview
    let screen = harness.screen();
    assert!(
        screen.contains("excluded: at the terminal, by `files`"),
        "{screen}"
    );
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

/// An edit changes what the model reads, in place, and what it said is still readable.
///
/// note: it used to supersede - a second item, with the old one left marked `~` - and what
/// this asserted was that second row. The row said what the `v1` page under `enter` already
/// said, and buying it cost an identifier the model may be holding, a state to carry over by
/// hand and a `replaces` hint for the conversation to read. So: one item, one identifier, and
/// the words it used to say a page rather than a row.
#[tokio::test]
async fn editing_an_item_changes_what_the_model_reads_and_keeps_what_it_said() {
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

    // the request carries the edit, and only the edit
    let request = harness.app.kernel.preview_request().unwrap();
    let sent = format!("{:?}", request.messages);
    assert!(sent.contains("the wrong notes"), "{sent}");
    assert_eq!(
        sent.matches("the wrong note").count(),
        1,
        "both versions went into the request: {sent}"
    );

    // one item, with the number it had: nothing was appended, and nothing the model is holding
    // a reference to has moved
    let items = harness.app.kernel.items();
    assert_eq!(items.len(), 1, "an edit is not a second item: {items:#?}");
    assert_eq!(items[0].id.0, 1);
    assert_eq!(items[0].state, ContextState::Active);
    // whose hand it was, on the item itself - `amend` writes the same key saying `amend`, and a
    // model reading this should never find its own tool credited with a sentence a person wrote
    assert_eq!(items[0].meta["revised"]["by"], "user");

    // and what it said before is a page under `enter` rather than a row of its own, on an item
    // whose `as stored` page opens saying who rewrote it
    harness.drain();
    harness.press(KeyCode::Enter).await;
    let screen = harness.screen();
    assert!(screen.contains("│ v1"), "{screen}");
    assert!(screen.contains("rewritten by `user`"), "{screen}");
}

/// `e` on a turn that is nothing but a tool call says why it cannot, rather than opening an
/// empty prompt over it.
///
/// note: the call is on the item's *kind*, beside the content, and `Kernel::replace` writes
/// content - so `e` could never have reached the thing on the screen. What it did instead was
/// open a box titled `editing [2]` holding the turn's content, which for a call-only turn is the
/// empty string; somebody who pressed `e` on a row reading `dig({"where":"here"})` got a blank
/// prompt and no account of why. Committing into it was worse than useless: it wrote a sentence
/// onto a turn whose call it had not touched, so the turn then said one thing and did another.
///
/// note: it is answered on this tab, in a panel over the row it is about, and not as a line on
/// the conversation where every other note the pane raises goes. The chat is not the tab somebody
/// is looking at when they press `e` on a context row, so a note there read as a key that did
/// nothing at all - which is the same failure as the empty box, one tab along.
///
/// note: what is asserted is that the keys did not move and that the answer is on the screen the
/// key was pressed on, because that is the half a wording change cannot break. The panel is
/// checked for the row it names rather than for its prose.
#[tokio::test]
async fn a_turn_that_is_only_a_tool_call_cannot_be_edited_and_says_so() {
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

    // the turn is the second item, and its row is the one showing the call
    harness.tab(Tab::Context);
    harness.press(KeyCode::Home).await;
    harness.press(KeyCode::Down).await;
    harness.press(KeyCode::Char('e')).await;

    assert!(
        harness.app.editing.is_none(),
        "the prompt should not have been armed over an item it cannot put back"
    );
    assert_eq!(
        harness.app.focus,
        Focus::Body,
        "and the keys should still be on the pane"
    );
    // the answer is on this tab, over the row it is about, rather than on the conversation
    let panel = harness.flat();
    assert!(panel.contains("[2] cannot be edited"), "{panel}");
    assert!(panel.contains("tool call and nothing else"), "{panel}");
    assert!(
        panel.contains("space") && panel.contains("enter"),
        "the keys that do work: {panel}"
    );
    assert!(
        harness
            .app
            .loose
            .iter()
            .all(|entry| !entry.text.contains("cannot be edited")),
        "the conversation is not where this belongs"
    );

    // the item is untouched: an `e` that refuses credits no hand and rewrites nothing
    let turn = &harness.app.kernel.items()[1];
    assert!(turn.meta.get("revised").is_none(), "{:?}", turn.meta);
    assert!(
        turn.content.to_text().trim().is_empty(),
        "{:?}",
        turn.content
    );
    assert_eq!(turn.calls().count(), 1, "the call is still there");

    // any key closes it, as it does every panel
    harness.press(KeyCode::Esc).await;
    assert!(harness.app.overlay.is_none(), "{}", harness.flat());

    // and the result of that call is still editable, because its content is the whole of it
    harness.press(KeyCode::Down).await;
    harness.press(KeyCode::Char('e')).await;
    assert!(harness.app.editing.is_some(), "a tool result is text");
    assert_eq!(harness.app.input.lines(), ["a bone"]);
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

    // editing decides what an item says, not whether it is sent. A supersession had to carry
    // this over by hand and twice did not - a pruned item came back Active and quietly put
    // itself in the next request, an archived one promoted the whole of an oversized output
    // into it - and a replacement never touches the state at all
    let items = harness.app.kernel.items();
    let edited = items.first().expect("the item that was edited");
    assert_eq!(edited.state, ContextState::Excluded);
    let sent = format!(
        "{:?}",
        harness.app.kernel.preview_request().unwrap().messages
    );
    assert!(!sent.contains("shorter wall"), "{sent}");
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
    let edited = items.first().expect("the item that was edited");
    assert_eq!(edited.state, ContextState::Elided);
    let sent = format!(
        "{:?}",
        harness.app.kernel.preview_request().unwrap().messages
    );
    assert!(!sent.contains("shorter wall"), "{sent}");

    // and `space` round the rest of the cycle - past excluded - is how you say you meant the
    // model to read it. On the same row: the edit did not move anywhere
    harness.press(KeyCode::Home).await;
    harness.press(KeyCode::Char(' ')).await;
    harness.press(KeyCode::Char(' ')).await;
    let items = harness.app.kernel.items();
    assert_eq!(items.first().unwrap().state, ContextState::Active);
    let sent = format!(
        "{:?}",
        harness.app.kernel.preview_request().unwrap().messages
    );
    assert!(sent.contains("shorter wall"), "{sent}");
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
async fn an_edit_keeps_what_the_item_used_to_say_on_the_item() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("the first draft"));
    harness.drain();

    // `e` replaces in place, so there is one row, one identifier, and one place to look for
    // what it used to say
    harness.tab(Tab::Context);
    harness.press(KeyCode::Char('e')).await;
    harness.press(KeyCode::Char('!')).await;
    harness.press(KeyCode::Enter).await;
    harness.drain();

    let items = harness.app.kernel.items();
    assert_eq!(items.len(), 1, "an edit is not a second item: {items:#?}");
    let edited = items[0].id;
    harness.tab(Tab::Context);
    harness.press(KeyCode::Home).await;
    harness.press(KeyCode::Enter).await;
    let screen = harness.screen();
    assert!(screen.contains(&format!("[{edited}]")), "{screen}");
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
async fn f_lists_only_what_the_next_request_carries_and_keeps_the_item_it_was_on() {
    // after a compaction most of the pane is items the model will never read again, and reading
    // past them to find the conversation is the thing this tab is for
    let mut harness = Harness::new([]);
    for text in ["first question", "second question", "third question"] {
        harness.app.kernel.push(ContextItem::user(text));
    }
    harness
        .app
        .kernel
        .push(ContextItem::file("dropped.rs", "fn gone() {}\n"));
    harness
        .app
        .kernel
        .push(ContextItem::file("kept.rs", "fn here() {}\n"));
    let items = harness.app.kernel.items();
    let (second, dropped) = (items[1].id, items[3].id);
    harness.app.kernel.set_state(
        [items[0].id, dropped],
        ContextState::Elided,
        Some("compacted to make room".into()),
    );
    harness.drain();
    harness.tab(Tab::Context);

    let all = harness.screen();
    assert_eq!(
        harness.app.listed().len(),
        5,
        "everything is listed to begin with"
    );
    assert!(all.contains("dropped.rs"), "{all}");
    assert_eq!(
        all.matches("compacted to make room").count(),
        2,
        "both pruned rows say why they are not being sent: {all}"
    );

    // stand on an item that survives the filter, so the selection has somewhere to stay
    harness.app.selected = 1;
    assert_eq!(harness.app.listed()[1].id, second);

    harness.press(KeyCode::Char('f')).await;
    let sending = harness.screen();
    assert!(
        !sending.contains("dropped.rs") && !sending.contains("compacted to make room"),
        "the pruned rows are gone: {sending}"
    );
    assert!(
        sending.contains("kept.rs") && sending.contains("second question"),
        "and everything still being sent is there: {sending}"
    );
    assert!(
        sending.contains("2 not being sent"),
        "the header says how many it is holding back: {sending}"
    );

    // the rows underneath moved, so the selection follows the item rather than the row number
    assert_eq!(
        harness.app.listed()[harness.app.selected].id,
        second,
        "still on the item it was on"
    );

    // and asking for one of the hidden ones by number says so, rather than "there is no item"
    harness.press(KeyCode::Char('4')).await;
    harness.press(KeyCode::Char('G')).await;
    let note = harness
        .app
        .loose
        .last()
        .map(|entry| entry.text.clone())
        .unwrap_or_default();
    assert!(
        note.contains(&format!("item [{}] is not being sent", dropped.0)),
        "a hidden item is not a missing one: {note:?}"
    );

    harness.press(KeyCode::Char('f')).await;
    let back = harness.screen();
    assert!(back.contains("dropped.rs"), "f puts them back: {back}");
    assert!(
        !back.contains("not being sent, hidden by f"),
        "and the header goes back to what it was: {back}"
    );
}

#[tokio::test]
async fn a_figure_too_wide_for_its_column_does_not_run_into_the_one_beside_it() {
    // `keep_truncated_output` archives the whole of what a tool produced, and what a tool can
    // produce has no ceiling: one `grep` that wandered into a build directory archived 11MB, and
    // the pane put `3,370,258` into a column budgeted at seven. `{:>7}` pads and never truncates,
    // so the figure took nine columns, the `0` beside it lost its gap and read as `01,400,000`,
    // and the two characters came off the far end of the row.
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("what does it do?"));
    harness.app.kernel.push(
        ContextItem::file("noise.log", "the command said something.\n".repeat(200_000))
            .because("a grep that went into ./target"),
    );
    let items = harness.app.kernel.items();
    let big = items[1].id;
    harness
        .app
        .kernel
        .set_state([big], ContextState::Archived, None);
    harness.drain();
    harness.tab(Tab::Context);

    let held = harness.app.kernel.item(big).expect("still there").tokens;
    assert!(held >= 1_000_000, "the item should hold millions: {held}");

    let screen = harness.screen();
    let row = screen
        .lines()
        .find(|line| line.contains("noise.log"))
        .expect("the item is listed")
        .to_owned();

    // the figure gives way, not the layout
    for line in screen.lines() {
        assert!(
            line.chars().count() <= 100,
            "a row ran past the screen: {line:?}"
        );
    }
    let exact = grouped(held);
    assert!(
        !row.contains(&exact),
        "the exact figure does not fit seven columns and should not be printed in full: {row}"
    );
    assert!(
        row.contains("archived"),
        "the column past the figures is still on the same row as the item: {row}"
    );
    assert!(
        row.contains('M'),
        "an abbreviated figure says how much is held: {row}"
    );

    // and what it is holding is still reported in full down on the status line, which has room
    let status = harness.screen();
    assert!(
        status.contains(&format!("{exact} held back")),
        "the status line still says all of it: {status}"
    );
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
    let item = harness
        .app
        .kernel
        .item(items[1].id)
        .expect("still there")
        .clone();
    let going = harness.app.going();
    let marker = going
        .costs
        .get(&item.id)
        .copied()
        .expect("an elided item is in the request, as a marker");
    let held = going.held_back(&item);

    // an elided item costs what its marker costs and keeps the rest of itself out, and the column
    // headed `sending` used to show the whole of what it held - answering a question nobody asked
    // while the status line beside it answered the right one
    assert_eq!(
        held,
        item.tokens - marker,
        "what it is keeping out is what it holds, less the marker that went in its place"
    );
    let figures: Vec<usize> = elided
        .split_whitespace()
        .filter_map(|word| word.replace(',', "").parse().ok())
        .collect();
    assert!(
        figures.contains(&held),
        "the held column reports what the request is not carrying: {elided}"
    );
    assert!(
        figures.contains(&marker) && marker < held,
        "and the sending column reports the marker, which is smaller: {elided}"
    );

    // the two columns are what the status line is made of: everything sending, and everything held
    let sent: usize = harness.app.going().costs.values().sum();
    let status = harness.screen();
    assert!(status.contains(&format!("~{sent} tokens")), "{status}");
    assert!(status.contains(&format!("{held} held back")), "{status}");

    // and the label column is as wide as the widest label, not a fixed twenty-six
    let header = screen.lines().nth(1).expect("the header row");
    let gap = header.find("kind").expect("the kind column") - header.find("label").expect("label");
    assert!(gap < 20, "twenty columns of nothing: {header}");
}

/// A turn whose thinking the endpoint will not take back is holding it, and the pane has to say so.
///
/// note: the bug this closes, from a real session. A reasoning model had put 25,903 tokens of
/// thinking on one turn, and every row on the pane read under 2k - because the turn is `Active`,
/// its words and its calls are going, so `sends_content` was true and the `held` column was left
/// blank. What it was holding was almost the whole of the session. The same conflation for the
/// fourth time: whether an item is going is not a yes or a no, and only the projection knows.
#[tokio::test]
async fn a_turn_whose_thinking_is_not_sent_says_what_it_is_holding() {
    let mut harness = Harness::new([]);
    // the dialect every OpenAI-compatible endpoint speaks: there is no agreed field for an
    // assistant turn's thinking, so `OpenAiCompatible::projection` does not carry it back
    harness
        .app
        .kernel
        .set_projector(Arc::new(nachalnik::LinearProjector {
            send_reasoning: false,
            ..Default::default()
        }));
    harness
        .app
        .kernel
        .push(ContextItem::user("rewrite the page"));
    let turn = harness.app.kernel.push(
        ContextItem::assistant("done - the deck reads better now", Vec::new())
            .with_reasoning(Some("weighing the two openings. ".repeat(300).into())),
    );
    harness.tab(Tab::Context);

    let item = harness.app.kernel.item(turn).expect("still there").clone();
    let going = harness.app.going();
    let sending = going
        .costs
        .get(&item.id)
        .copied()
        .expect("the turn is going");
    let held = going.held_back(&item);
    assert!(
        held > sending * 10,
        "the fixture must hold far more than it sends: {held} against {sending}"
    );

    let screen = harness.screen();
    let row = screen
        .lines()
        .find(|line| line.contains("the deck reads better"))
        .expect("the turn is listed");
    assert!(
        row.contains(&grouped(held)),
        "the thinking is on no row: {row}"
    );

    // and the status line adds it up, because the corner and the pane are one answer
    assert!(
        screen.contains(&format!("{} held back", grouped(held))),
        "{screen}"
    );

    // back to the chat tab before typing a command: on the context tab the letters are keys
    harness.tab(Tab::Chat);
    harness.send("/budget").await;
    let budget = harness.flat();
    assert!(
        budget.contains(&format!("held back: {} tokens", grouped(held))),
        "`/budget` disagrees with the pane: {budget}"
    );
    assert!(
        budget.contains("thinking the endpoint will not take back"),
        "and it does not say which of the four this is: {budget}"
    );
}

/// The same turn, under a projector that does carry thinking back, is holding nothing.
///
/// note: the pair to the test above, and the half that says what the figure means. It is not
/// "reasoning is expensive" - it is *this endpoint does not take it*, which is a property of the
/// projector and can change with the provider under a context that has not moved.
#[tokio::test]
async fn the_same_turn_holds_nothing_where_the_thinking_is_sent() {
    let harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("rewrite the page"));
    let turn = harness.app.kernel.push(
        ContextItem::assistant("done", Vec::new())
            .with_reasoning(Some("weighing the two openings. ".repeat(300).into())),
    );

    let item = harness.app.kernel.item(turn).expect("still there").clone();
    let going = harness.app.going();
    assert_eq!(
        going.held_back(&item),
        0,
        "the whole of it is in the request, so nothing is being held"
    );
}

/// A counter that has just learnt something does not make every row look as though it is holding.
///
/// note: what a live run against Gemini found, whose projector carries thinking and ordered
/// blocks back in full - so the context was holding nothing, and the pane reported 1,264 tokens
/// held across eighteen rows. `ContextItem::tokens` is counted when the item arrives, a projected
/// message is counted now, and a `Calibrating` counter moves the scale under them between one
/// response and the next: two rulers, and the few percent between them read as tokens held back.
///
/// note: the window is not a corner case. `App` recounts when a *turn* ends, so every reading
/// taken while one is running is inside it - and the `context` tool is only ever called from
/// inside a turn, which is to say always.
#[tokio::test]
async fn a_counter_that_has_learnt_something_invents_nothing_held_back() {
    let harness = Harness::new([]);
    harness.app.kernel.push(ContextItem::user("go"));
    harness
        .app
        .kernel
        .push(ContextItem::file("big.rs", "fn parse() {}\n".repeat(400)));
    let items = harness.app.kernel.items();

    let going = harness.app.going();
    for item in &items {
        assert_eq!(going.held_back(item), 0, "nothing is held yet: {}", item.id);
    }

    // the provider reports that a request really cost less than the counter guessed, and the
    // counter takes the correction. Nothing recounts what is already here until the turn ends
    let counter = harness.app.kernel.counter();
    let learned = counter
        .calibration()
        .expect("the default counter is a calibrating one");
    counter.recalibrate(Calibration {
        scale: learned.scale * 0.9,
        ..learned
    });

    let going = harness.app.going();
    for item in &items {
        assert_eq!(
            going.held_back(item),
            0,
            "a scale that moved is not an item holding something back: {}",
            item.id
        );
    }
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

/// `/limit` changes the number the tool will really declare on its next call.
///
/// note: the answer to a result the model has just reported as cut off. It has to reach the tools
/// rather than a table nobody reads - `Tool::spec` is called afresh for every request, which is
/// what makes a change here land without a restart - and it has to say that it applies to the
/// next call, because the one already shortened is recovered a different way: its whole is
/// archived beside it and one `space` sends that instead.
#[tokio::test]
async fn the_output_limit_can_be_raised_without_restarting() {
    let mut harness = Harness::new([]);
    let limits = harness.app.limits.clone();
    for tool in kamchatka::tools::builtin(
        kamchatka::tools::Shell {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            readable: Vec::new(),
            policy: harness.app.policy.clone(),
            confiner: None,
            limits: limits.clone(),
        },
        kamchatka::sandbox::Reach {
            workdir: std::path::PathBuf::from("/w"),
            extra: Vec::new(),
            readable: Vec::new(),
            confined: true,
        },
        limits,
    ) {
        harness.app.kernel.add_tool(tool);
    }

    // note: asked of the *call* rather than of the spec, because a tool that does several things
    // has a limit per thing - `fs:read` and `fs:grep` are one tool and two reasonable answer
    // sizes. See `Tool::limit`
    let declared = |harness: &Harness, tool: &str, action: &str| {
        let call = nachalnik::ToolCall::new("c", tool, json!({ "action": action }));
        harness.app.kernel.tool(tool).and_then(|it| it.limit(&call))
    };
    assert_eq!(declared(&harness, "fs", "read"), Some(32_000));

    harness.send("/limit fs:read 64000").await;
    assert_eq!(
        declared(&harness, "fs", "read"),
        Some(64_000),
        "the tool has to declare the new one, or the command changed nothing"
    );
    // and the rest of `fs` is untouched: one operation was named, one operation moved
    assert_eq!(declared(&harness, "fs", "grep"), Some(32_000));
    // the one place a tool and its domain are spelled differently: `shell` runs `exec:run`
    assert_eq!(declared(&harness, "shell", "run"), Some(32_000));

    let screen = harness.flat();
    assert!(screen.contains("was cut at 32,000"), "{screen}");
    assert!(
        screen.contains("from its next call"),
        "it has to say which call it applies to: {screen}"
    );

    // the number the listing prints is a handle the command takes, or it is decoration: naming
    // the row and naming the subject are the same instruction. `fs:read` is the twentieth of the
    // sorted subjects
    harness.send("/limit 20 48000").await;
    assert_eq!(declared(&harness, "fs", "read"), Some(48_000));
    assert_eq!(
        declared(&harness, "fs", "grep"),
        Some(32_000),
        "one row moved"
    );
    assert!(
        harness.flat().contains("`fs:read` was cut at 64,000"),
        "the answer names the tool, not the row: {}",
        harness.flat()
    );

    // a tool nothing here limits says so, and lists what is limited rather than failing silently
    harness.send("/limit write 1000").await;
    let screen = harness.flat();
    assert!(screen.contains("nothing here limits `write`"), "{screen}");

    // a row out of range is answered the same way, by a listing that says what the range is
    harness.send("/limit 99 1000").await;
    let screen = harness.flat();
    assert!(screen.contains("nothing here limits `99`"), "{screen}");
    // and the listing is where the range is; every subject is in it, `exec:run` among them
    harness.send("/limit").await;
    let screen = harness.sized(120, 40).replace('\n', " ");
    assert!(screen.contains("[14] exec:run"), "{screen}");
    harness.press(KeyCode::Esc).await;

    // and nought is not a limit, it is a tool that answers with a marker
    harness.send("/limit fs:read 0").await;
    assert_eq!(declared(&harness, "fs", "read"), Some(48_000), "unchanged");
    assert!(
        harness.flat().contains("/tools toggle ID"),
        "{}",
        harness.flat()
    );

    // the listing last, because it opens a preview and the next keystroke closes it again
    harness.send("/limit").await;
    let screen = harness.sized(120, 40).replace('\n', " ");
    assert!(
        screen.contains("48,000 bytes"),
        "it lists the new one: {screen}"
    );
    assert!(
        screen.contains("context:note"),
        "and every other subject: {screen}"
    );
    // numbered, so that the number the command takes is one somebody can read off the screen
    assert!(screen.contains("[20] fs:read"), "{screen}");
}

/// A limit nothing in this session declares says so, in the listing and on the change.
///
/// note: found by reading a headless run. `/tools` said "4 offered: edit, read, shell, write" and
/// `/limit`, two lines later, listed six under "how much of each tool's output the model is
/// shown" - `amend`, `context`, `log` and `setup` among them, none of them installed, because the
/// table is the `Limits` map and the map holds a row for everything this program ships. Setting
/// one took, silently, and answered "from its next call onwards" about a tool that has no calls.
/// The rows are right to be there - a limit set before the tool arrives is in force the moment it
/// does, which this checks - and it is the sentence over them that was claiming too much.
#[tokio::test]
async fn a_limit_for_a_tool_nobody_is_offering_says_which_ones_those_are() {
    let mut harness = Harness::new([]);

    // the row, not the listing: the sentence under the table contains the words `not offered` too,
    // so a check of the whole screen passes whether or not anything is actually marked
    let row = |harness: &mut Harness, tool: &str| {
        harness
            .screen()
            .lines()
            .find(|line| line.contains(tool) && line.contains("bytes"))
            .unwrap_or_else(|| panic!("no row for `{tool}`"))
            .to_owned()
    };

    harness.send("/limit").await;
    let marked = row(&mut harness, "context:look");
    assert!(
        marked.contains("not offered"),
        "a row nothing here declares is marked as one: {marked}"
    );
    // a screen tall enough for the whole table and the sentence under it: there is a row per
    // subject now, and the default viewport shows the first twenty-five of thirty-three
    let whole = harness
        .sized(120, 60)
        .replace(['│', '┌', '┐', '└', '┘', '─'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        whole.contains("a limit held for something no tool here declares"),
        "and the marking says what it is: {whole}"
    );
    // the listing is a preview, and the next keystroke closes it - so it is closed deliberately
    // rather than with the first character of the next command
    harness.press(KeyCode::Esc).await;

    // setting one still takes, because that is the point of the row being there at all
    harness.send("/limit context:look 8000").await;
    let screen = harness.flat();
    assert!(screen.contains("is now cut at 8,000"), "{screen}");
    assert!(
        screen.contains("has no next call until something does"),
        "\"from its next call onwards\" is a promise about a tool that is not here: {screen}"
    );

    // and it is in force the moment the tool arrives, which is what the row is for
    harness.app.introspect = Some(kamchatka::introspect::install(
        &harness.app.kernel,
        harness.app.policy.clone(),
        harness.app.limits.clone(),
    ));
    let looking = nachalnik::ToolCall::new("c", "context", json!({ "action": "look" }));
    assert_eq!(
        harness
            .app
            .kernel
            .tool("context")
            .and_then(|tool| tool.limit(&looking)),
        Some(8_000),
        "the limit set before the tool existed is the one it reads"
    );

    // and its row is no longer marked. This harness installs no builtin tools, so `fs` and
    // `shell` are legitimately still unoffered here - which is why the check is of the row rather
    // than of the whole listing, and why the sentence under the table does not claim to account
    // for every mark
    harness.send("/limit").await;
    let marked = row(&mut harness, "context:look");
    assert!(
        !marked.contains("not offered"),
        "the tool is installed now: {marked}"
    );
}

/// An item the projector repaired away is holding what it holds, and the pane has to say so.
///
/// note: the bug this closes, from a real session. Somebody put the whole of a truncated tool
/// result back beside the copy the model had been shown - which is the intended way to send the
/// whole - and the row *below* it dropped to `0`. The pair answer one call, so the whole takes the
/// call and the short copy is dropped: correct, and the request was right throughout. What was
/// wrong is that three of the four places reporting on it disagreed. The row said it was sending
/// its content, showed `0` for what that cost, and accounted for none of the 8,583 tokens it was
/// holding; `/budget` said nothing was held back at all.
///
/// note: the cause is one conflation, for the third time: whether an item is going cannot be read
/// off its *state*. A repaired-away item is `Active`. Only the projection knows.
#[tokio::test]
async fn an_item_the_projector_drops_says_what_it_is_holding() {
    let mut harness = Harness::new([]);
    let kernel = harness.app.kernel.clone();

    let asked = call("c1", "context", json!({}));
    kernel.push(ContextItem::user("read the whole of it"));
    kernel.push(ContextItem::assistant("reading", vec![asked.clone()]));
    // the pair an output limit leaves behind
    let whole = kernel.push(ContextItem::tool_result(
        asked.id.clone(),
        "context",
        "the fork's whole answer ".repeat(60),
        false,
    ));
    kernel.push(ContextItem::tool_result(
        asked.id.clone(),
        "context",
        "the fork's whole answer [... 900 bytes truncated by an output limit ...]",
        false,
    ));
    kernel.set_state([whole], ContextState::Archived, None);

    // `space` on the archived whole, which is how a person asks for all of it
    kernel.set_state([whole], ContextState::Active, None);

    harness.tab(Tab::Context);
    let screen = harness.flat();
    assert!(
        screen.contains("a second result for one call"),
        "the dropped row does not say why it is not going: {screen}"
    );
    assert!(
        !screen.contains("truncated by an output limit ..."),
        "and it must not show content it is not sending: {screen}"
    );

    // back to the chat tab before typing a command: on the context tab the letters are keys
    harness.tab(Tab::Chat);

    // and `/budget` counts it too, where it used to say nothing was held back at all. Counting
    // the item is the half that was wrong: the figure came from the states, which have no way to
    // say that an `Active` item is not in the request
    harness.send("/budget").await;
    let budget = harness.flat();
    assert!(
        budget.contains("held back:") && budget.contains("in 1 item"),
        "`/budget` disagrees with the pane about what is held back: {budget}"
    );
    assert!(
        !budget.contains("held back: 0 "),
        "the short copy is holding its whole content: {budget}"
    );
}

/// And a tool result spends its six rows on what the tool said.
///
/// note: `head` takes the first six *lines*, so two blank ones in front cost a third of the
/// preview and truncate it two lines early - the padding does not just look untidy, it eats the
/// evidence.
#[tokio::test]
async fn a_padded_tool_result_does_not_spend_its_preview_on_nothing() {
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({}))]),
        ModelResponse::text("done"),
    ]);
    harness.app.kernel.add_tool(Arc::new(ConstTool::new(
        "dig",
        "\n\nfirst\nsecond\nthird\nfourth\nfifth\nsixth",
    )));

    harness.send("dig").await;
    harness.settle().await;

    let screen = harness.screen();
    assert!(screen.contains("first"), "{screen}");
    assert!(
        screen.contains("sixth"),
        "the blank rows should not have cost it the last two lines: {screen}"
    );
}

/// A row the counter would not price says so, rather than reading as the cheapest thing here.
///
/// note: the two meanings of `0` in that column. One is "measured, and free"; the other is
/// "there is a picture here and nothing priced it", and until the `+` they were the same cell -
/// which invites exactly the wrong conclusion from a pane somebody opens to decide what to get
/// rid of. The picture is the most expensive thing in the request and was reading as the
/// cheapest row in the list.
#[tokio::test]
async fn a_row_nobody_priced_does_not_read_as_a_free_one() {
    use nachalnik::Content;

    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::user("what is on the screen?"));
    let shot = harness.app.kernel.push(ContextItem::user(Content::blob(
        "image/png",
        "A".repeat(4_000),
    )));

    harness.tab(Tab::Context);
    let screen = harness.screen();

    assert_eq!(
        harness.app.kernel.item(shot).unwrap().tokens,
        0,
        "the counter declines it, which is the trap this is about"
    );
    let row = screen
        .lines()
        .find(|line| line.contains("image/png"))
        .unwrap_or_else(|| panic!("the picture is listed: {screen}"));
    assert!(
        row.contains("0+"),
        "its figure should say it is a floor: {row:?}"
    );
    // and a row that really is fully counted carries no `+`
    let prose = screen
        .lines()
        .find(|line| line.contains("what is on the screen"))
        .expect("the question is listed");
    assert!(!prose.contains('+'), "{prose:?}");
}
