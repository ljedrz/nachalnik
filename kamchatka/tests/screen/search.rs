//! The `/` filter over the trace and context panes.
//!
//! note: what is checked is that the rows the keys count and the rows on the screen are the same
//! rows. A pane filtered on its way to the terminal while the keys still worked against the whole
//! list would select the item above the one under the cursor, which is the bug this feature is
//! most likely to have.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kamchatka::app::Tab;
use nachalnik::ModelResponse;

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
