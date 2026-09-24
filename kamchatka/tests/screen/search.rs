//! The `/` filter over the trace and context panes.
//!
//! note: what is checked is that the rows the keys count and the rows on the screen are the same
//! rows. A pane filtered on its way to the terminal while the keys still worked against the whole
//! list would select the item above the one under the cursor, which is the bug this feature is
//! most likely to have.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kamchatka::app::Tab;
use nachalnik::{
    ModelResponse,
    test::{ConstTool, call},
};
use serde_json::json;

use crate::harness::Harness;

/// Types a run of characters at whatever has the keys.
async fn type_in(harness: &mut Harness, text: &str) {
    for c in text.chars() {
        harness
            .app
            .on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
            .await;
    }
}

async fn press(harness: &mut Harness, code: KeyCode) {
    harness
        .app
        .on_key(KeyEvent::new(code, KeyModifiers::NONE))
        .await;
}

#[tokio::test]
async fn slash_filters_the_trace_and_esc_puts_it_back() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    let all = harness.app.traced().len();
    assert!(all >= 4, "not enough events to filter: {all}");

    press(&mut harness, KeyCode::Char('/')).await;
    assert!(harness.app.search.is_some(), "`/` should open the box");
    type_in(&mut harness, "modreq").await;

    // fuzzy, not substring: `modreq` is not in `model.requested` as written
    let found = harness.app.traced();
    assert!(!found.is_empty(), "a fuzzy query should still match");
    assert!(
        found.len() < all,
        "the filter should drop rows: {} of {all}",
        found.len()
    );
    assert!(
        found.iter().all(|event| event.name.contains("model")),
        "kept: {:?}",
        found.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    let screen = harness.sized(120, 30);
    assert!(
        screen.contains("/modreq"),
        "the box should show it: {screen}"
    );

    press(&mut harness, KeyCode::Esc).await;
    assert!(harness.app.search.is_none(), "esc should close the box");
    assert_eq!(harness.app.traced().len(), all, "and put every row back");
}

/// The four tab shortcuts reach past an open search box, which is what `/help` promises of them.
///
/// note: the handler's own comment says a modifier means somebody reaching past the box for a key
/// that works everywhere - and it excluded `ctrl` only, so `alt+1` typed a `1` into the query.
/// Driven through `on_key` on purpose: that is where the search box takes its turn before the
/// shortcuts are reached, and a test that called `Harness::tab` would never see it.
#[tokio::test]
async fn a_tab_shortcut_works_while_a_search_is_open() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "model").await;

    harness.alt(KeyCode::Char('1')).await;
    assert_eq!(harness.app.tab, Tab::Chat, "`alt+1` went into the query");
    // and the box went with the tab, which is what leaving one does to a search of it
    assert!(
        harness.app.search.is_none(),
        "the box outlived the tab it was over"
    );
}

#[tokio::test]
async fn the_trace_can_be_searched_by_the_hour_it_happened() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    // the whole reason the stamp is built beside the filter rather than in the pane
    let hour = kamchatka::app::when::read_off(harness.app.trace[0].wall)
        .expect("the clock reads")
        .time[..2]
        .to_owned();

    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, &hour).await;

    assert!(
        !harness.app.traced().is_empty(),
        "searching the hour should find the events in it"
    );
}

#[tokio::test]
async fn the_keys_and_the_screen_agree_about_which_context_row_is_which() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Context);

    let all = harness.app.listed().len();
    assert!(all >= 2, "not enough items: {all}");

    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "assistant").await;

    let kept = harness.app.listed();
    assert!(!kept.is_empty(), "the assistant turn should match");
    assert!(kept.len() < all, "and the user turn should not");

    // the row the keys are on is a row that is still on the screen
    press(&mut harness, KeyCode::Down).await;
    assert!(
        harness.app.selected < kept.len(),
        "selection {} is off the end of {} filtered row(s)",
        harness.app.selected,
        kept.len()
    );
}

/// A context row can be filtered by the kind in its kind column.
///
/// note: `tool_result` rather than `assistant`, which is what the test above types and which
/// proves nothing about this: `assistant` is also an assistant turn's *label*, so it matched
/// before the kind was in the haystack at all. A tool result's label is the tool's name and its
/// content is the output, so `tool_result` appears nowhere but the column - and the pane answered
/// that there were none, on a session with one in it.
///
/// note: what is checked is the row that is kept as well as the count, because a fuzzy query over
/// a haystack this permissive can be right about how many and wrong about which.
#[tokio::test]
async fn a_context_row_can_be_filtered_by_its_kind() {
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

    let all = harness.app.listed().len();
    assert!(all >= 4, "a call, its result and two turns: {all}");

    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "tool_result").await;

    let kept = harness.app.listed();
    assert_eq!(kept.len(), 1, "{:?}", kept.iter().map(|i| &i.label));
    assert_eq!(kept[0].kind.name(), "tool_result");
    assert_eq!(
        kept[0].label, "dig",
        "the tool result, not a turn about one"
    );
    // and the box counts the rows the pane drew, out of everything there is
    let of = format!("1 of {}", harness.app.kernel.items().len());
    assert!(harness.flat().contains(&of), "{}", harness.flat());

    // and the column it was matched on is not what the row is matched *only* on: the label and
    // the content still work, or this would have traded one half of the haystack for the other
    press(&mut harness, KeyCode::Esc).await;
    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "bone").await;
    assert!(
        harness.app.listed().iter().any(|i| i.label == "dig"),
        "the content is still searched"
    );
}

#[tokio::test]
async fn a_filter_does_not_follow_you_to_another_tab() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "zzzz").await;
    assert!(harness.app.traced().is_empty(), "nothing matches `zzzz`");

    harness.tab(Tab::Context);
    assert!(
        harness.app.search.is_none(),
        "a query written for the trace should not filter the context"
    );
    assert!(!harness.app.listed().is_empty(), "the context is all there");
}

#[tokio::test]
async fn backspace_widens_the_search_again() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "zzzz").await;
    let none = harness.app.traced().len();

    press(&mut harness, KeyCode::Backspace).await;
    press(&mut harness, KeyCode::Backspace).await;
    press(&mut harness, KeyCode::Backspace).await;
    press(&mut harness, KeyCode::Backspace).await;

    assert_eq!(none, 0);
    assert_eq!(
        harness.app.search.as_ref().map(|s| s.query.as_str()),
        Some(""),
        "backspacing to empty leaves the box open"
    );
    assert!(
        !harness.app.traced().is_empty(),
        "an empty query hides nothing"
    );
}

/// A query can be fixed where the mistake is, rather than rubbed out back to it.
///
/// note: what is checked is the *match*, not only the string: the pattern is reparsed from the
/// query on every change, and an edit that wrote the text without reparsing would leave a box
/// showing one query while the pane answered another.
#[tokio::test]
async fn the_query_can_be_amended_in_the_middle_of_it() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "mopreq").await;
    assert!(
        harness.app.traced().is_empty(),
        "there is no `p` in `model.requested` for the fuzzy match to land on"
    );

    // back over `req` and take out the letter that does not belong, keeping what came after it
    for _ in 0..3 {
        press(&mut harness, KeyCode::Left).await;
    }
    press(&mut harness, KeyCode::Backspace).await;

    let found = harness.app.traced();
    assert!(
        !found.is_empty() && found.iter().all(|event| event.name.contains("model")),
        "the pane answers the amended query: {:?}",
        found.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    // and typing goes in where the cursor is, not at the end of the line
    type_in(&mut harness, "d").await;
    let search = harness.app.search.as_ref().expect("the box is still open");
    assert_eq!(search.query, "modreq");
    assert_eq!(search.parts(), ("mod", "req"));
    assert!(!harness.app.traced().is_empty(), "and it still matches");

    // the box draws the cursor where it is, between the two halves
    let screen = harness.sized(120, 30);
    assert!(screen.contains("/mod▏req"), "{screen}");
}

/// The two keys that take a character out take the one on their own side of the cursor.
#[tokio::test]
async fn backspace_and_delete_take_out_either_side_of_the_cursor() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);

    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "abcd").await;
    press(&mut harness, KeyCode::Left).await;
    press(&mut harness, KeyCode::Left).await;

    press(&mut harness, KeyCode::Backspace).await;
    assert_eq!(
        harness.app.search.as_ref().map(|s| s.parts()),
        Some(("a", "cd")),
        "backspace takes the character behind the cursor"
    );

    press(&mut harness, KeyCode::Delete).await;
    assert_eq!(
        harness.app.search.as_ref().map(|s| s.parts()),
        Some(("a", "d")),
        "delete takes the one in front of it"
    );

    // and neither runs off its end of the query
    press(&mut harness, KeyCode::Backspace).await;
    press(&mut harness, KeyCode::Backspace).await;
    press(&mut harness, KeyCode::Left).await;
    press(&mut harness, KeyCode::Delete).await;
    press(&mut harness, KeyCode::Delete).await;
    assert_eq!(
        harness.app.search.as_ref().map(|s| s.query.as_str()),
        Some(""),
        "an empty query stays empty and the box stays open"
    );
}

/// The keys the panes need while the box is open are still the panes'.
///
/// note: this is the half of the feature that is about what was *not* taken. `left` and `right`
/// were free; `up`, `down` and the paging are how somebody reads what a filter found, and `home`
/// and `end` are all that is left of `g` and `G` while every letter is going into the query.
#[tokio::test]
async fn the_box_leaves_the_rows_their_own_keys() {
    let mut harness = Harness::new([ModelResponse::text("done")]);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Context);

    press(&mut harness, KeyCode::Char('/')).await;
    let rows = harness.app.listed().len();
    assert!(rows >= 2, "not enough items: {rows}");

    press(&mut harness, KeyCode::Down).await;
    assert_eq!(harness.app.selected, 1, "down still moves between rows");
    press(&mut harness, KeyCode::Home).await;
    assert_eq!(
        harness.app.selected, 0,
        "and home is still the first of them"
    );
    press(&mut harness, KeyCode::End).await;
    assert_eq!(harness.app.selected, rows - 1, "and end the last");

    assert_eq!(
        harness.app.search.as_ref().map(|s| s.query.as_str()),
        Some(""),
        "none of which typed anything into the query"
    );
}

/// An empty pane and a filtered-empty pane are not the same empty, and only one of them has a
/// key that undoes it. Both panes used to give the search's answer unconditionally or never: the
/// trace told a session that had not done anything yet to press `esc` and clear a search nobody
/// had started, and the context blamed `f` for rows the query was what hid.
#[tokio::test]
async fn an_empty_pane_says_which_empty_it_is() {
    let mut harness = Harness::new([ModelResponse::text("done")]);

    // nothing has happened yet, so there is nothing to clear and it does not say there is
    harness.tab(Tab::Trace);
    let fresh = harness.sized(120, 30);
    assert!(fresh.contains("nothing here yet"), "{fresh}");
    assert!(!fresh.contains("esc clears"), "there is no search: {fresh}");

    // and once there is one that matches nothing, it says so, and says how to get out
    harness.tab(Tab::Chat);
    harness.send("go").await;
    harness.settle().await;
    harness.tab(Tab::Trace);
    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "zzzznotathing").await;
    let filtered = harness.sized(120, 30);
    assert!(filtered.contains("esc clears the search"), "{filtered}");

    // the context pane has three empties rather than two, and `f` is only one of them
    harness.tab(Tab::Context);
    assert!(harness.app.search.is_none(), "changing tabs clears it");
    press(&mut harness, KeyCode::Char('/')).await;
    type_in(&mut harness, "zzzznotathing").await;
    let context = harness.sized(120, 30);
    assert!(context.contains("esc clears the search"), "{context}");
    assert!(
        !context.contains("`f` lists them again"),
        "`f` is not what hid these: {context}"
    );
}
