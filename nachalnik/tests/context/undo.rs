//! Undo and redo: one operation, one checkpoint.
//!
//! note: what counts as an operation is the subject. `push_all` over eight files is one, a
//! `set_state` over eight ids is one, a recorded turn is one - and an operation that changes
//! nothing takes no checkpoint at all, which is what stops a `u` spending itself on a command
//! that did nothing. The replacement test is here rather than with the items because an
//! overwritten text is the one thing an undo stack is the only way back to.

use std::sync::Arc;

use nachalnik::{Config, ContextId, ContextItem, ContextState, Event, Kernel};

use crate::{drain, kernel, select};

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
