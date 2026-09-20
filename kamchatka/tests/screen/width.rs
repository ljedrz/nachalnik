//! Text a terminal draws two cells wide, in the four places that decide what fits.
//!
//! note: a character is not a column, and everything in here is about the consequence. Measured in
//! characters, a row of CJK is thought to hold twice what it holds - so the line built for it is
//! twice the width of the pane, and a `Paragraph` that does not wrap drops the right-hand end of
//! it. The lost end is not reachable by scrolling either: it was never put in a row. So the
//! assertion each of these makes is the same one, and it is the strongest one available - every
//! character that went in comes back off the screen.
//!
//! note: `packed` rather than `screen` or `flat`. Wide prose has no spaces to wrap at, so it is
//! broken mid-phrase and lands on two rows; running the rows together *and* taking the spaces out
//! is the only view in which what went in is a substring of what came out.

use std::sync::Arc;

use kamchatka::app::Tab;
use nachalnik::{
    Capability, ContextItem, ModelResponse,
    test::{ConstTool, call},
};
use serde_json::json;

use crate::harness::Harness;

/// `n` distinct characters a terminal gives two cells each, so `n` of them is `2n` columns.
///
/// note: distinct rather than one repeated, because the failure this is about loses a *run* of
/// them - and a needle of one character repeated is still found in a haystack that dropped half
/// of it. These are the CJK unified ideographs from the start of the block, every one of them
/// East Asian Wide.
fn wide(n: usize) -> String {
    ('一'..).take(n).collect()
}

/// Which cell of a drawn row `needle` starts in.
///
/// note: not the byte offset, which is what `str::find` answers and is a different number the
/// moment anything before it is not ASCII. A wide character reaches the buffer as its own cell
/// and a blank one after it, so a row read back off the screen has one character per cell and the
/// count of them is the column.
fn cell_of(line: &str, needle: &str) -> usize {
    let at = line
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is not on `{line}`"));

    line[..at].chars().count()
}

/// An answer wider than the pane arrives whole, wrapped onto as many rows as its columns need.
#[tokio::test]
async fn wide_prose_is_wrapped_by_column_and_none_of_it_is_lost() {
    let said = wide(60);
    let mut harness = Harness::new([ModelResponse::text(said.clone())]);

    harness.send("say something long").await;
    harness.settle().await;

    let packed = harness.packed();
    assert!(
        packed.contains(&said),
        "a hundred and twenty columns of answer in a pane a hundred wide: {packed}"
    );
}

/// And so does a fenced block, which is cut where the room runs out rather than reflowed.
///
/// note: the same measurement one layer along. Prose goes through `refit` and a block goes
/// through `markdown::fit`, and each was counting characters where the pane counts cells - so
/// this passing says nothing about the one above it.
#[tokio::test]
async fn a_wide_code_block_is_cut_by_column_and_none_of_it_is_lost() {
    let code = wide(60);
    let mut harness = Harness::new([ModelResponse::text(format!("here:\n\n```\n{code}\n```\n"))]);

    harness.send("show me").await;
    harness.settle().await;

    let packed = harness.packed();
    assert!(packed.contains(&code), "{packed}");
}

/// A permission question shows the whole of the arguments it is about.
///
/// note: the one of these with something at stake beyond reading. The panel is where somebody
/// decides whether to let a call run, and an argument whose end was dropped for being measured
/// wrong is a decision taken on half of what was asked for.
#[tokio::test]
async fn a_question_shows_every_column_of_a_wide_argument() {
    let path = wide(50);
    let mut harness = Harness::new([
        ModelResponse::tool_calls(vec![call("c1", "dig", json!({ "where": path.clone() }))]),
        ModelResponse::text("I did not"),
    ]);
    harness.app.kernel.add_tool(Arc::new(
        ConstTool::new("dig", "a bone").with_capabilities([Capability::exec("run")]),
    ));

    harness.send("dig there").await;
    harness.settle().await;

    let packed = harness.packed();
    assert!(packed.contains("digwants:exec:run"), "{packed}");
    assert!(
        packed.contains(&path),
        "the panel is where somebody decides, and it is showing part of the argument: {packed}"
    );
}

/// A wide label does not push the columns to its right off the end of the row.
///
/// note: the other half of measuring, and the one no amount of wrapping fixes. `{:<width$}` pads
/// by characters, so a label of ten wide characters in a column ten wide is given no padding at
/// all and takes twenty cells - and every column after it starts ten cells late and the last one
/// falls off the edge. The token count is the column at the far right, so it is the one that
/// disappears, and it is the reason the pane exists.
#[tokio::test]
async fn a_wide_label_keeps_the_columns_after_it_where_they_belong() {
    let mut harness = Harness::new([]);
    harness
        .app
        .kernel
        .push(ContextItem::file(wide(8), "a short thing to hold"));
    harness.drain();
    harness.tab(Tab::Context);

    let screen = harness.screen();
    let header = screen
        .lines()
        .find(|line| line.contains("kind"))
        .unwrap_or_else(|| panic!("the pane's own headings: {screen}"));
    let row = screen
        .lines()
        .find(|line| line.contains("reference"))
        .unwrap_or_else(|| panic!("the item's row: {screen}"));

    // the headings are ASCII and are laid out from the same numbers the row is, so a column that
    // does not start under its own heading is a column that has been pushed
    assert_eq!(
        cell_of(row, "reference"),
        cell_of(header, "kind"),
        "the kind column is not under its heading:\n{header}\n{row}"
    );
    // and the label that pushed it is drawn whole rather than cut back to fit
    assert!(harness.packed().contains(&wide(8)), "{screen}");
}
