//! Tests for the context: the state the user is supposed to be able to control.

use std::sync::Arc;

use nachalnik::{
    BytesPerToken, Config, ContextId, ContextItem, ContextState, Event, Kernel, TokenCounter,
    selectors::Selector,
    test::{ConstTool, EchoTool},
};
use serde_json::json;

/// Returns the events received so far.
fn drain(events: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<Event> {
    let mut received = Vec::new();
    while let Ok(event) = events.try_recv() {
        received.push(event);
    }

    received
}

fn sel(input: &str) -> Selector {
    input.parse().unwrap()
}

fn kernel() -> Kernel {
    Kernel::new(Config::default())
}

/// Resolves a selector the way a client would.
fn select(kernel: &Kernel, input: &str) -> Vec<ContextId> {
    sel(input).matches(&kernel.items())
}

#[test]
fn items_are_identified_and_counted() {
    let kernel = kernel();
    let system = kernel.push(ContextItem::system("be terse"));
    let file = kernel.push(ContextItem::file("src/parser.rs", "fn parse() {}"));

    assert_eq!(system.0, 1);
    assert_eq!(file.0, 2);
    assert_eq!(kernel.items().len(), 2);
    // the default counter is bytes/4, rounded up
    assert_eq!(
        kernel.item(file).unwrap().tokens,
        "fn parse() {}".len().div_ceil(4)
    );
    // the budget is counted over what the projector produced, not over what the context holds,
    // and for a labelled reference those are different numbers: `src/parser.rs:\n` goes on the
    // wire and somebody pays for it. Summing the items would leave that off the bill
    let items = kernel.items().iter().map(|i| i.tokens).sum::<usize>();
    let projected = kernel.budget().context_tokens;
    assert!(
        projected > items,
        "the label a reference is projected with costs something: {projected} vs {items}"
    );
    assert_eq!(
        projected,
        "be terse".len().div_ceil(4) + "src/parser.rs:\nfn parse() {}".len().div_ceil(4)
    );
}

#[test]
fn excluding_hides_items_without_destroying_them() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a".repeat(400)));
    let b = kernel.push(ContextItem::file("src/b.rs", "b".repeat(40)));
    let before = kernel.budget().context_tokens;

    let changed = kernel.set_state([a], ContextState::Excluded, Some("too big".into()));
    assert_eq!(changed.changed, vec![a]);

    let item = kernel.item(a).unwrap();
    assert_eq!(item.state, ContextState::Excluded);
    assert_eq!(item.note.as_deref(), Some("too big"));
    assert_eq!(
        item.content.to_text().len(),
        400,
        "the content is still there"
    );
    let b_text = kernel.item(b).unwrap().content.to_text().into_owned();
    assert_eq!(
        kernel.budget().context_tokens,
        format!("src/b.rs:\n{b_text}").len().div_ceil(4),
        "only `b` is left, and it is projected with its label"
    );
    assert_eq!(
        kernel.with_context(|c| c.tokens_withheld()),
        kernel.item(a).unwrap().tokens,
        "what was excluded is what is being withheld"
    );

    // and it comes back
    let restored = kernel.set_state([a], ContextState::Active, None);
    assert_eq!(restored.changed, vec![a]);
    assert_eq!(kernel.budget().context_tokens, before);
    assert!(
        kernel.set_state([a], ContextState::Active, None).is_empty(),
        "a state it is already in is not a change"
    );
}

#[test]
fn undo_reverts_a_whole_operation() {
    let kernel = kernel();
    kernel.push(ContextItem::file("src/a.rs", "a"));
    kernel.push(ContextItem::file("src/b.rs", "b"));
    kernel.push(ContextItem::file("src/c.rs", "c"));

    let files = select(&kernel, "files");
    assert_eq!(
        kernel
            .set_state(files, ContextState::Excluded, None)
            .changed
            .len(),
        3
    );
    assert_eq!(kernel.with_context(|c| c.projected().count()), 0);

    assert!(kernel.undo());
    assert_eq!(
        kernel.with_context(|c| c.projected().count()),
        3,
        "one undo puts all three back"
    );

    // undo walks back the additions, too
    assert!(kernel.undo());
    assert_eq!(kernel.items().len(), 2);
}

#[test]
fn undo_depth_is_configurable() {
    let kernel = Kernel::new(Config {
        context_undo_depth: 0,
        ..Default::default()
    });
    kernel.push(ContextItem::user("hi"));

    assert!(!kernel.undo());
}

#[test]
fn pinning_is_just_another_state() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));

    assert_eq!(
        kernel.set_state([a], ContextState::Pinned, None).changed,
        vec![a]
    );
    assert_eq!(kernel.item(a).unwrap().state, ContextState::Pinned);
    assert!(kernel.item(a).unwrap().is_projected());

    assert_eq!(
        kernel.set_state([a], ContextState::Active, None).changed,
        vec![a]
    );
    assert_eq!(kernel.item(a).unwrap().state, ContextState::Active);
}

#[test]
fn content_can_be_replaced_in_place() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a".repeat(100)));

    kernel.replace(a, "a").unwrap();
    assert_eq!(kernel.item(a).unwrap().tokens, 1);
    assert_eq!(kernel.item(a).unwrap().content.to_text(), "a");
    assert!(kernel.undo());
    assert_eq!(kernel.item(a).unwrap().tokens, 25);

    assert!(kernel.replace(ContextId(999), "x").is_err());
}

#[test]
fn selectors_resolve_against_real_items() {
    let kernel = kernel();
    let system = kernel.push(ContextItem::system("be terse"));
    let user = kernel.push(ContextItem::user("hello"));
    let file = kernel.push(ContextItem::file("src/a.rs", "a"));
    let memory = kernel.push(ContextItem::memory("recalled", "something"));
    let ext = kernel.push(ContextItem::new(
        nachalnik::ContextKind::Reference,
        "helix",
        "buffer",
        "text",
    ));
    let first = kernel.push(ContextItem::tool_result("c1".into(), "grep", "one", false));
    let second = kernel.push(ContextItem::tool_result("c2".into(), "grep", "two", false));
    let other = kernel.push(ContextItem::tool_result(
        "c3".into(),
        "shell",
        "three",
        false,
    ));

    assert_eq!(select(&kernel, "1"), vec![system]);
    assert_eq!(select(&kernel, "user"), vec![user]);
    assert_eq!(select(&kernel, "files"), vec![file]);
    assert_eq!(select(&kernel, "file:src/a.rs"), vec![file]);
    assert_eq!(select(&kernel, "memories"), vec![memory]);
    assert_eq!(select(&kernel, "source:helix"), vec![ext]);
    assert_eq!(select(&kernel, "tool:grep"), vec![first, second]);
    assert_eq!(select(&kernel, "tool:grep:latest"), vec![second]);
    assert_eq!(select(&kernel, "tool:grep:first"), vec![first]);
    assert_eq!(
        select(&kernel, "all:tool_results"),
        vec![first, second, other]
    );
    assert_eq!(select(&kernel, "kind:tool_result").len(), 3);
    assert_eq!(select(&kernel, "all").len(), 8);
    assert!(select(&kernel, "file:nope.rs").is_empty());
}

#[test]
fn what_a_client_needs_to_render_a_breakdown_is_all_there() {
    let kernel = kernel();
    kernel.add_tool(Arc::new(EchoTool::new("echo", [])));
    kernel.push(ContextItem::system("be terse"));
    let big = kernel.push(ContextItem::file("src/big.rs", "x".repeat(8_000)));
    kernel.set_state([big], ContextState::Excluded, Some("garbage".into()));

    let budget = kernel.budget();
    assert!(budget.tool_tokens > 0, "tool definitions cost tokens too");
    assert_eq!(budget.used(), budget.context_tokens + budget.tool_tokens);
    assert_eq!(budget.limit, None, "no provider, no limit to know");
    assert_eq!(kernel.with_context(|c| c.tokens_withheld()), 2_000);

    // ... per item: an identity, a label, a size, a state and a reason
    let item = kernel.item(big).unwrap();
    assert_eq!(
        (
            item.id,
            item.label.as_str(),
            item.tokens,
            item.state,
            item.note.as_deref()
        ),
        (
            big,
            "src/big.rs",
            2_000,
            ContextState::Excluded,
            Some("garbage")
        ),
    );
    assert_eq!(item.source, "file");
    assert_eq!(item.kind.name(), "reference");
}

#[test]
fn counting_is_replaceable() {
    struct OneEach;

    impl TokenCounter for OneEach {
        fn count(&self, _content: &nachalnik::Content) -> usize {
            1
        }
    }

    let kernel = kernel();
    kernel.push(ContextItem::file("src/a.rs", "a".repeat(400)));
    // 400 bytes of content plus the `src/a.rs:\n` the projector labels a reference with, which
    // is what the request will actually carry
    let projected = "src/a.rs:\n".len() + 400;
    assert_eq!(kernel.budget().context_tokens, projected.div_ceil(4));

    kernel.set_counter(Arc::new(OneEach));
    assert_eq!(kernel.budget().context_tokens, 1);

    kernel.set_counter(Arc::new(BytesPerToken { bytes_per_token: 2 }));
    assert_eq!(kernel.budget().context_tokens, projected.div_ceil(2));
}

#[tokio::test]
async fn the_projection_says_what_is_being_sent_and_what_is_not() {
    let kernel = kernel();
    kernel.add_tool(Arc::new(ConstTool::new("grep", "hits")));
    kernel.push(ContextItem::system("be terse"));
    kernel.push(ContextItem::user("look for foo"));
    let file = kernel.push(ContextItem::file("src/a.rs", "fn main() {}"));

    let projection = kernel.project();
    assert_eq!(projection.messages.len(), 3);
    assert_eq!(projection.included, select(&kernel, "all"));
    assert_eq!(
        projection.messages[2].content.as_ref().unwrap().to_text(),
        "src/a.rs:\nfn main() {}",
        "references are labelled, so the model knows what it is looking at"
    );

    kernel.set_state([file], ContextState::Excluded, Some("not relevant".into()));
    let projection = kernel.project();
    assert_eq!(projection.messages.len(), 2);
    assert_eq!(projection.skipped.len(), 1);
    assert_eq!(projection.skipped[0].id, file);
    assert_eq!(projection.skipped[0].reason, "excluded: not relevant");
}

#[test]
fn json_content_is_counted_and_kept_structured() {
    let kernel = kernel();
    let id = kernel.push(ContextItem::diagnostic(
        "src/a.rs:1",
        json!({ "severity": "error", "message": "mismatched types" }),
    ));

    let item = kernel.item(id).unwrap();
    assert_eq!(item.tokens, item.content.byte_len().div_ceil(4));
    assert!(item.content.as_text().is_none());
}

#[test]
fn metadata_is_carried_but_never_interpreted() {
    let kernel = kernel();
    let id = kernel.push(
        ContextItem::file("src/a.rs", "a").with_meta(json!({ "priority": "low", "buffer": 3 })),
    );

    assert_eq!(kernel.item(id).unwrap().meta["priority"], "low");
}

#[test]
fn a_reason_is_never_rewritten_in_silence() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));
    kernel.set_state([a], ContextState::Excluded, Some("too big".into()));

    let mut events = kernel.subscribe();

    // the same state with a different reason is a different fact about the item, so it is a
    // change, and it is announced like one
    assert_eq!(
        kernel
            .set_state([a], ContextState::Excluded, Some("the user said so".into()))
            .changed,
        vec![a]
    );
    assert_eq!(
        kernel.item(a).unwrap().note.as_deref(),
        Some("the user said so")
    );
    assert_eq!(
        events.try_recv().map(|e| e.name().to_owned()).ok(),
        Some("context.changed".to_owned())
    );

    // and asking for what is already true changes nothing, and says nothing
    assert!(
        kernel
            .set_state([a], ContextState::Excluded, Some("the user said so".into()))
            .is_empty()
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn an_operation_that_does_nothing_does_not_spend_an_undo() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));
    kernel.set_state([a], ContextState::Excluded, Some("too big".into()));

    let depth = kernel.with_context(|c| c.undo_len());

    // a replace of something that is not there, and a state change that is already true
    assert!(kernel.replace(ContextId(999), "nope").is_err());
    assert!(
        kernel
            .set_state([a], ContextState::Excluded, Some("too big".into()))
            .is_empty()
    );
    assert!(
        kernel
            .set_state([ContextId(999)], ContextState::Active, None)
            .is_empty()
    );
    assert_eq!(
        kernel.with_context(|c| c.undo_len()),
        depth,
        "a failed operation that spends an undo makes the next one walk back somebody else's work"
    );

    // so the one undo available still reverts the exclusion, as the user would expect
    assert!(kernel.undo());
    assert_eq!(kernel.item(a).unwrap().state, ContextState::Active);
}

#[test]
fn the_budget_quotes_for_the_request_that_would_actually_be_sent() {
    let kernel = kernel();
    kernel.push(ContextItem::user("hi"));

    // a tool result with no turn asking for it is one the projector drops, so it is not part of
    // what the next request costs - a budget that counted it would be quoting for a request that
    // is never going to exist
    let orphan = kernel.push(ContextItem::tool_result(
        "nobody-asked".into(),
        "grep",
        "x".repeat(4_000),
        false,
    ));
    assert!(kernel.item(orphan).unwrap().is_projected());
    assert_eq!(kernel.project().skipped.len(), 1);

    let budget = kernel.budget();
    let request = kernel.preview_request().unwrap();
    assert_eq!(request.messages.len(), 1);
    assert_eq!(
        budget.context_tokens,
        kernel.item(kernel.items()[0].id).unwrap().tokens,
        "the orphan is listed and inspectable, but it is not part of the quote"
    );

    // and once the turn that asked for it is there, it counts
    kernel.push(ContextItem::assistant(
        "",
        vec![nachalnik::ToolCall::new("nobody-asked", "grep", json!({}))],
    ));
    assert!(kernel.budget().context_tokens > budget.context_tokens + 900);
}

#[test]
fn a_kernel_says_what_it_is_without_being_asked_twice() {
    let kernel = kernel();
    kernel.push(ContextItem::file("src/a.rs", "fn a() {}"));
    kernel.add_tool(Arc::new(ConstTool::new("peek", "ok")));

    let rendered = format!("{kernel:?}");
    assert!(rendered.contains("state: \"idle\""), "{rendered}");
    assert!(rendered.contains("items: 1"), "{rendered}");
    assert!(rendered.contains("peek"), "{rendered}");
}

#[test]
fn an_undo_says_what_it_did() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));
    let b = kernel.push(ContextItem::file("src/b.rs", "b"));
    kernel.set_state([a, b], ContextState::Excluded, Some("too big".into()));

    let mut events = kernel.subscribe();

    // undoing the exclusion reverts two items and removes none
    assert!(kernel.undo());
    let Some(Event::ContextUndone {
        items,
        removed,
        changed,
    }) = events.try_recv().ok()
    else {
        panic!("an undo is a context change like any other")
    };
    assert_eq!(items, 2);
    assert!(removed.is_empty());
    assert_eq!(
        changed,
        vec![a, b],
        "a client can render exactly what came back"
    );

    // undoing the addition takes an item back out of existence, and says so
    assert!(kernel.undo());
    let Some(Event::ContextUndone {
        items,
        removed,
        changed,
    }) = events.try_recv().ok()
    else {
        panic!()
    };
    assert_eq!(items, 1);
    assert_eq!(removed, vec![b]);
    assert!(changed.is_empty());

    // and an undo with nothing to undo is not an event
    while kernel.undo() {
        let _ = events.try_recv();
    }
    assert!(events.try_recv().is_err());
}

#[test]
fn naming_nothing_is_not_the_same_as_changing_nothing() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));

    let done = kernel.set_state([a, ContextId(999)], ContextState::Excluded, None);
    assert_eq!(done.changed, vec![a]);
    assert_eq!(done.unknown, vec![ContextId(999)], "so a client can say so");
    assert!(done.unchanged.is_empty());

    // asking again: the item is where it was asked to be, and 999 still does not exist
    let again = kernel.set_state([a, ContextId(999)], ContextState::Excluded, None);
    assert!(again.changed.is_empty());
    assert_eq!(again.unchanged, vec![a]);
    assert_eq!(again.unknown, vec![ContextId(999)]);
}

#[test]
fn a_redo_puts_back_what_an_undo_took() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));
    let b = kernel.push(ContextItem::file("src/b.rs", "b"));
    kernel.set_state([a, b], ContextState::Excluded, Some("too big".into()));

    let mut events = kernel.subscribe();

    assert!(kernel.undo());
    assert!(
        kernel.item(a).unwrap().is_projected(),
        "the exclusion is off"
    );
    let _ = events.try_recv();

    assert!(
        kernel.redo(),
        "and losing work to a mis-click is not control"
    );
    let Some(Event::ContextRedone {
        items,
        restored,
        changed,
    }) = events.try_recv().ok()
    else {
        panic!("a redo is announced like anything else")
    };
    assert_eq!(items, 2);
    assert!(restored.is_empty());
    assert_eq!(changed, vec![a, b]);
    assert_eq!(kernel.item(a).unwrap().state, ContextState::Excluded);
    assert_eq!(kernel.item(a).unwrap().note.as_deref(), Some("too big"));

    // undoing the addition and redoing it brings the item itself back
    assert!(kernel.undo());
    assert!(kernel.undo());
    assert_eq!(kernel.items().len(), 1);
    let _ = drain(&mut events);

    assert!(kernel.redo());
    let Some(Event::ContextRedone { restored, .. }) = events.try_recv().ok() else {
        panic!()
    };
    assert_eq!(restored, vec![b]);
    assert_eq!(kernel.items().len(), 2);
}

#[test]
fn doing_something_new_makes_the_undone_future_unreachable() {
    let kernel = kernel();
    kernel.push(ContextItem::file("src/a.rs", "a"));
    kernel.push(ContextItem::file("src/b.rs", "b"));

    assert!(kernel.undo());
    assert_eq!(kernel.with_context(|c| c.redo_len()), 1);

    // a redo that reached across this would be overwriting it, not restoring anything
    kernel.push(ContextItem::file("src/c.rs", "c"));
    assert_eq!(kernel.with_context(|c| c.redo_len()), 0);
    assert!(!kernel.redo());
}

#[test]
fn a_set_of_files_is_one_thing_the_user_did() {
    let kernel = kernel();
    let ids = kernel.push_all([
        ContextItem::file("src/a.rs", "a"),
        ContextItem::file("src/b.rs", "b"),
        ContextItem::file("src/c.rs", "c"),
    ]);

    assert_eq!(ids.len(), 3);
    assert_eq!(
        kernel.with_context(|c| c.undo_len()),
        1,
        "three pushes would have spent three of the sixteen the user has"
    );
    assert!(kernel.undo());
    assert!(kernel.items().is_empty());

    assert!(kernel.push_all([]).is_empty());
}

#[test]
fn superseding_is_explicit_and_reversible() {
    let kernel = kernel();
    let old = kernel.push(ContextItem::file("src/a.rs", "fn a() {}"));

    // the kernel does not guess that a second read replaces the first; it is told
    let new = kernel
        .supersede(old, ContextItem::file("src/a.rs", "fn a() -> u8 { 1 }"))
        .unwrap();

    assert_eq!(kernel.item(old).unwrap().state, ContextState::Superseded);
    assert!(!kernel.item(old).unwrap().is_projected());
    assert_eq!(
        kernel.item(old).unwrap().note.as_deref(),
        Some(&*format!("superseded by item {new}"))
    );
    assert_eq!(
        kernel.item(new).unwrap().content.to_text(),
        "fn a() -> u8 { 1 }"
    );
    assert_eq!(
        kernel.item(old).unwrap().content.to_text(),
        "fn a() {}",
        "the old one keeps its contents and its identifier"
    );

    // and it is one operation
    assert!(kernel.undo());
    assert!(kernel.item(old).unwrap().is_projected());
    assert!(kernel.item(new).is_none());

    assert!(
        kernel
            .supersede(ContextId(999), ContextItem::user("x"))
            .is_err()
    );
}

#[test]
fn a_hint_can_be_attached_after_the_fact() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));
    assert!(kernel.item(a).unwrap().meta.is_null());

    let mut events = kernel.subscribe();
    kernel.annotate(a, json!({ "expendable": true })).unwrap();

    assert_eq!(kernel.item(a).unwrap().meta, json!({ "expendable": true }));
    assert_eq!(
        events.try_recv().map(|e| e.name().to_owned()).ok(),
        Some("context.annotated".to_owned()),
        "a hint that decides what a compactor drops is not a quiet change"
    );

    // and setting it to what it already says is not a change
    kernel.annotate(a, json!({ "expendable": true })).unwrap();
    assert!(events.try_recv().is_err());

    assert!(kernel.annotate(ContextId(999), json!(1)).is_err());
}

#[test]
fn a_request_does_not_copy_the_context_to_build_itself() {
    let kernel = kernel();
    let id = kernel.push(ContextItem::user("x".repeat(1 << 20)));

    let item = kernel.item(id).unwrap();
    let projected = kernel.project();
    let sent = projected.messages[0].content.clone().unwrap();

    let (nachalnik::Content::Text(held), nachalnik::Content::Text(going)) = (&item.content, &sent)
    else {
        unreachable!()
    };
    assert!(
        Arc::ptr_eq(held, going),
        "every request re-projects the whole context; copying it each time would make a large \
         context cost more to look at than to think about"
    );

    // the same holds across the state changes an undo snapshot pins the old version for
    kernel.set_state([id], ContextState::Excluded, Some("too big".into()));
    let after = kernel.item(id).unwrap();
    let (nachalnik::Content::Text(before), nachalnik::Content::Text(after)) =
        (&item.content, &after.content)
    else {
        unreachable!()
    };
    assert!(
        Arc::ptr_eq(before, after),
        "pruning moved a pointer, not a megabyte"
    );
}

#[test]
fn a_replacement_is_the_one_thing_that_would_otherwise_be_lost() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "fn a() -> u8 { 1 }"));

    let mut events = kernel.subscribe();
    kernel.replace(a, "fn a() -> u8 { 2 }").unwrap();

    // every other event names its item and lets the context hold the contents; this one carries
    // them, because after a replacement they are nowhere else
    let Some(Event::ContextReplaced { was, id, .. }) = events.try_recv().ok() else {
        panic!("a replacement is announced like anything else")
    };
    assert_eq!(id, a);
    assert_eq!(was.to_text(), "fn a() -> u8 { 1 }");

    // which is what keeps `model.requested` worth anything: it names the items a request was
    // built from rather than copying them, and a name is only as good as what it points at
    let log = serde_json::to_string(&kernel.history()).unwrap();
    assert!(
        log.contains("fn a() -> u8 { 1 }"),
        "a request replayed from its item ids would reconstruct the wrong bytes"
    );

    // and it costs a pointer rather than a copy
    let item = kernel.item(a).unwrap();
    let (nachalnik::Content::Text(now), nachalnik::Content::Text(before)) = (&item.content, &was)
    else {
        unreachable!()
    };
    assert!(!Arc::ptr_eq(now, before));
    assert_eq!(before.len(), 18);
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

#[test]
fn a_result_follows_the_call_it_answers_whatever_lands_between_them() {
    // the failure this closes: a tool that writes something into the context - `amend note`, in
    // kamchatka - pushes its item while the turn that called it is still collecting results. The
    // item lands between the assistant message and the results, and every OpenAI-compatible API
    // refuses the request: "an assistant message with 'tool_calls' must be followed by tool
    // messages responding to each 'tool_call_id'". Five of seven live runs died this way, each
    // one immediately after the model had written down all ten of its correct findings
    let kernel = kernel();
    kernel.push(ContextItem::user("go"));
    kernel.push(ContextItem::assistant(
        "",
        vec![
            nachalnik::ToolCall::new("c1", "amend", json!({})),
            nachalnik::ToolCall::new("c2", "shell", json!({})),
        ],
    ));
    // what the tool wrote, pushed the moment it ran and so before the second result exists
    kernel.push(ContextItem::memory("q1", "Cargo.lock is 3593 lines"));
    kernel.push(ContextItem::tool_result(
        "c1".into(),
        "amend",
        "written down",
        false,
    ));
    kernel.push(ContextItem::tool_result(
        "c2".into(),
        "shell",
        "3593",
        false,
    ));

    let roles: Vec<&str> = kernel
        .project()
        .messages
        .iter()
        .map(|message| message.role.as_str())
        .collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "tool", "tool", "user"],
        "the results have to reach the wire before anything else the turn produced"
    );

    // and it is a repair like any other: moving somebody's item is not something to do quietly
    let projection = kernel.project();
    assert_eq!(
        projection.included.len(),
        5,
        "nothing was dropped to achieve it"
    );
    assert!(
        projection
            .repairs
            .iter()
            .any(|said| said.contains("held item")),
        "the move is on the record: {:?}",
        projection.repairs
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
