//! Compaction, from the context's side: what a plan may move, what it costs, and what it leaves
//! behind.
//!
//! note: an elided item leaves a marker carrying the reason for eliding it, and the marker is
//! counted like anything else - so eliding something smaller than its own marker makes the
//! request bigger, and a compactor that did not check would go round again. The other half is
//! that a call and the result answering it stay together: eliding the result must not orphan the
//! call, or the request stops being one an API will take.

use std::sync::Arc;

use nachalnik::{CompactionPlan, ContextItem, ContextState};
use serde_json::json;

use crate::kernel;

/// The same rule, for the one operation that used to be exempt from it. A `Compactor` is asked
/// before every request, so a pass that moves nothing is not an odd hand-written plan - it is what
/// a compactor whose every candidate is pinned or already elided answers with, every time.
#[test]
fn a_compaction_that_moves_nothing_does_not_spend_an_undo() {
    let kernel = kernel();
    let pinned = kernel.push(ContextItem::file("src/a.rs", "a".repeat(400)).pinned());
    let elided = kernel.push(ContextItem::file("src/b.rs", "b".repeat(400)));
    kernel.set_state([elided], ContextState::Elided, Some("already gone".into()));

    let depth = kernel.with_context(|c| c.undo_len());

    // a plan naming nothing at all
    let report = kernel.apply_compaction(CompactionPlan {
        reason: "nothing to do".into(),
        ..CompactionPlan::default()
    });
    assert!(report.removed.is_empty() && report.elided.is_empty());
    assert_eq!(kernel.with_context(|c| c.undo_len()), depth);

    // one whose every candidate is refused, and one naming only what is already a marker
    let report = kernel.apply_compaction(CompactionPlan {
        remove: vec![pinned],
        elide: vec![pinned, elided],
        reason: "an overzealous compactor".into(),
        ..CompactionPlan::default()
    });
    assert_eq!(report.refused.len(), 2, "the pin is refused for both");
    assert!(report.elided.is_empty(), "and the marker is already one");
    assert_eq!(
        kernel.with_context(|c| c.undo_len()),
        depth,
        "a pass that moved nothing spent an undo"
    );

    // the same pass with a summary on it, which is the shape every real one has. A summary stands
    // in the place of what was taken, so a pass that took nothing has nowhere to put one - and
    // banking it anyway is the whole of the failure this is about, because the next request asks
    // again and the context is no smaller. Measured against a live endpoint: a summary and an
    // undo per turn, and the request growing 53 tokens each time
    let report = kernel.apply_compaction(CompactionPlan {
        elide: vec![pinned],
        summary: Some(ContextItem::summary("1 earlier tool result(s) were elided")),
        reason: "an overzealous compactor, with something to say about it".into(),
        ..CompactionPlan::default()
    });
    assert!(report.summary.is_none(), "the summary went in anyway");
    assert_eq!(kernel.items().len(), 2, "and is in the context");
    assert_eq!(
        kernel.with_context(|c| c.undo_len()),
        depth,
        "and was worth a checkpoint"
    );

    // and the redo the person still had is still theirs. This is the sharper half: `checkpoint`
    // discards the redo stack, so a pass that did nothing used to make an undone change
    // unreachable - before every request, for the rest of the session
    assert!(kernel.undo());
    assert_eq!(kernel.with_context(|c| c.redo_len()), 1);
    kernel.apply_compaction(CompactionPlan {
        reason: "still nothing to do".into(),
        ..CompactionPlan::default()
    });
    assert_eq!(
        kernel.with_context(|c| c.redo_len()),
        1,
        "an empty pass threw away the redo"
    );
    assert!(kernel.redo());
}

/// A pass may only take what the request is carrying, and the projection is what knows. An item
/// the projector repaired away is `Active`, holds everything it holds, and is costing nothing -
/// so a pass that takes it recovers nothing while reporting the whole of it as recovered, and
/// moves something the model was never being shown.
#[test]
fn a_compaction_leaves_alone_what_the_request_was_not_carrying() {
    let kernel = kernel();
    let call = nachalnik::ToolCall::new("c1", "read", Arc::new(json!({ "path": "big.rs" })));
    kernel.push(ContextItem::assistant(
        nachalnik::Content::text("let me look"),
        vec![call.clone()],
    ));
    let answered = kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "read",
        "x".repeat(400),
        false,
    ));
    // the whole of a shortened output, put back beside the copy the model was shown: two results
    // answer one call, and the projector carries one of them
    let second = kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "read",
        "x".repeat(4_000),
        false,
    ));

    let carried = kernel.project().included.to_vec();
    assert!(
        carried.contains(&answered) != carried.contains(&second),
        "exactly one of the pair is in the request: {carried:?}"
    );
    let ghost = match carried.contains(&second) {
        true => answered,
        false => second,
    };
    let held = kernel.item(ghost).unwrap().tokens;

    let depth = kernel.with_context(|c| c.undo_len());
    let report = kernel.apply_compaction(CompactionPlan {
        elide: vec![ghost],
        remove: vec![ghost],
        summary: Some(ContextItem::summary("something was elided")),
        reason: "the context was full".into(),
    });
    assert!(
        report.elided.is_empty() && report.removed.is_empty(),
        "it was costing nothing, so there was nothing to recover: it was credited with {held}"
    );
    assert_eq!(
        kernel.item(ghost).unwrap().state,
        ContextState::Active,
        "and it is in the state the person left it in"
    );
    assert!(
        report.summary.is_none(),
        "with no summary for work not done"
    );
    assert_eq!(kernel.with_context(|c| c.undo_len()), depth);
}

/// And a pass that *does* move something is still one operation, whichever of the three it did.
#[test]
fn a_compaction_that_moves_something_is_one_undo() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a".repeat(400)));
    let b = kernel.push(ContextItem::file("src/b.rs", "b".repeat(400)));

    let depth = kernel.with_context(|c| c.undo_len());
    let report = kernel.apply_compaction(CompactionPlan {
        remove: vec![a],
        elide: vec![b],
        summary: Some(ContextItem::summary("two items went")),
        reason: "the context was full".into(),
    });
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.elided.len(), 1);
    assert!(report.summary.is_some());
    assert_eq!(kernel.with_context(|c| c.undo_len()), depth + 1);

    assert!(kernel.undo());
    assert_eq!(kernel.item(a).unwrap().state, ContextState::Active);
    assert_eq!(kernel.item(b).unwrap().state, ContextState::Active);
    assert_eq!(kernel.items().len(), 2, "the summary went with them");
}

/// An elided item is still in the request, as a marker, and its own size is on the withheld side
/// of the ledger rather than the spent one.
#[test]
fn eliding_leaves_a_marker_where_the_content_was() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a".repeat(4000)));
    let fat = kernel.budget().context_tokens;

    kernel.set_state(
        [a],
        ContextState::Elided,
        Some("compacted to make room".into()),
    );

    let item = kernel.item(a).unwrap();
    assert_eq!(item.state, ContextState::Elided);
    assert!(item.state.is_projected(), "it is still in the request");
    assert!(!item.state.sends_content(), "but not as what it says");
    assert_eq!(
        item.content.to_text().len(),
        4000,
        "and nothing was destroyed to get here"
    );

    let projection = kernel.project();
    assert_eq!(projection.included, vec![a]);
    assert!(
        projection.skipped.is_empty(),
        "an elided item is not a skipped one: {:?}",
        projection.skipped
    );
    let sent = projection.messages[0].content.as_ref().unwrap().to_text();
    assert!(
        sent.contains("[... compacted to make room ...]"),
        "the model is told, in the words of whoever elided it: {sent}"
    );
    assert!(!sent.contains("aaaa"), "and not told the content: {sent}");

    // the budget follows the marker, not the item
    assert!(
        kernel.budget().context_tokens < fat / 10,
        "the request got smaller: {} vs {fat}",
        kernel.budget().context_tokens
    );
    assert_eq!(
        kernel.with_context(|c| c.tokens_withheld()),
        item.tokens,
        "what it holds is being withheld, not spent"
    );

    // and it comes back, because the content never went anywhere
    kernel.set_state([a], ContextState::Active, None);
    assert_eq!(kernel.budget().context_tokens, fat);
}

/// The reason eliding exists: an excluded tool result takes its call down with it, and an elided
/// one does not.
#[tokio::test]
async fn eliding_a_tool_result_keeps_the_call_that_asked_for_it() {
    use nachalnik::{Content, ToolCall};

    let kernel = kernel();
    let call = ToolCall::new("call-1", "read", Arc::new(json!({"path": "src/a.rs"})));
    kernel.push(ContextItem::user("what is in a.rs?"));
    kernel.push(ContextItem::assistant(
        Content::text("let me look"),
        vec![call.clone()],
    ));
    let result = kernel.push(ContextItem::tool_result(
        call.id.clone(),
        "read",
        Content::text("a".repeat(4000)),
        false,
    ));

    // excluded: the projector has to take the call down too, so the model reads a conversation in
    // which nobody ever asked for the file
    kernel.set_state([result], ContextState::Excluded, Some("too big".into()));
    let projection = kernel.project();
    assert_eq!(projection.repairs.len(), 1, "{:?}", projection.repairs);
    let assistant = projection
        .messages
        .iter()
        .find(|m| !m.tool_calls.is_empty() || m.role == nachalnik::Role::Assistant)
        .expect("the assistant turn is there");
    assert!(
        assistant.tool_calls.is_empty(),
        "excluding the result took the call with it"
    );

    // elided: the call keeps its answer, so the turn keeps its shape
    kernel.set_state([result], ContextState::Elided, Some("compacted".into()));
    let projection = kernel.project();
    assert!(
        projection.repairs.is_empty(),
        "nothing had to be repaired: {:?}",
        projection.repairs
    );
    let assistant = projection
        .messages
        .iter()
        .find(|m| m.role == nachalnik::Role::Assistant)
        .expect("the assistant turn is there");
    assert_eq!(
        assistant.tool_calls.len(),
        1,
        "the call it made is still on the record"
    );
    let answer = projection
        .messages
        .iter()
        .find(|m| m.role == nachalnik::Role::Tool)
        .expect("and it still has an answer");
    assert!(
        answer
            .content
            .as_ref()
            .unwrap()
            .to_text()
            .contains("[... compacted ...]"),
        "which says it was compacted"
    );
}

/// An elided turn stops costing what it thought. Without that, eliding an assistant turn frees
/// nothing - the marker replacing the words is a line, and the reasoning behind it can be
/// thousands of tokens - so a compactor would watch the budget refuse to move and elide again,
/// while `tokens_withheld` claimed those tokens were being kept from the model.
#[tokio::test]
async fn eliding_a_turn_stops_it_costing_what_it_thought() {
    let kernel = kernel();
    kernel.push(ContextItem::user("why?"));
    let turn = kernel.push(
        ContextItem::assistant("a short answer", Vec::new())
            .with_reasoning(Some(nachalnik::Content::text("X".repeat(4_000)))),
    );

    let before = kernel.budget().context_tokens;
    kernel.set_state(
        [turn],
        ContextState::Elided,
        Some("compacted to make room".into()),
    );
    let after = kernel.budget().context_tokens;

    assert!(
        after * 10 < before,
        "the request costs {after} where it cost {before}: a marker, not a marker plus the \
         thinking behind it"
    );
    // and the two ledgers agree about it: what the item is holding is on the withheld side
    let withheld = kernel.with_context(|context| context.tokens_withheld());
    assert!(withheld >= before - after, "{withheld} withheld");
}
