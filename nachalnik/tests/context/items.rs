//! Items: what one is, what state it is in, and what changing it does to the rest.
//!
//! note: the invariant every one of these is a version of - nothing is destroyed. An excluded,
//! archived or superseded item keeps its identifier, is still listed, is still inspectable, and
//! comes back with a `set_state`. Removal is a state change, which is why `pinning_is_just_
//! another_state` is not the joke the name makes it sound like.

use std::sync::Arc;

use nachalnik::{ContextId, ContextItem, ContextState, test::ConstTool};
use serde_json::json;

use crate::{kernel, select};

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
        Some(&*format!("replaced by item {new}"))
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
