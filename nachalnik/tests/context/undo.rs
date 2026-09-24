//! Undo and redo: one operation, one checkpoint.
//!
//! note: what counts as an operation is the subject. `push_all` over eight files is one, a
//! `set_state` over eight ids is one, a recorded turn is one - and an operation that changes
//! nothing takes no checkpoint at all, which is what stops a `u` spending itself on a command
//! that did nothing. The replacement test is here rather than with the items because an
//! overwritten text is the one thing an undo stack is the only way back to.

use std::sync::Arc;

use nachalnik::{
    Config, ContextId, ContextItem, ContextState, Event, Kernel, ModelResponse,
    test::{AllowAll, ConstTool, ScriptedProvider, call},
};
use serde_json::json;

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

    assert!(kernel.undo().unwrap());
    assert_eq!(
        kernel.with_context(|c| c.projected().count()),
        3,
        "one undo puts all three back"
    );

    // undo walks back the additions, too
    assert!(kernel.undo().unwrap());
    assert_eq!(kernel.items().len(), 2);
}

#[test]
fn undo_depth_is_configurable() {
    let kernel = Kernel::new(Config {
        context_undo_depth: 0,
        ..Default::default()
    });
    kernel.push(ContextItem::user("hi"));

    assert!(!kernel.undo().unwrap());
}

#[test]
fn an_operation_that_does_nothing_does_not_spend_an_undo() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));
    kernel.set_state([a], ContextState::Excluded, Some("too big".into()));

    let depth = kernel.with_context(|c| c.undo_len());

    // a replace of something that is not there, a replace with what the item already says, and a
    // state change that is already true
    assert!(kernel.replace(ContextId(999), "nope").is_err());
    assert!(kernel.replace(a, "a").is_ok());
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
    assert!(kernel.undo().unwrap());
    assert_eq!(kernel.item(a).unwrap().state, ContextState::Active);
}

/// A recount that moves no figure leaves the items as they were, to an undo as much as anybody.
///
/// note: `kamchatka` recounts after every turn, so an item a recount copied without changing
/// read as changed on every undo after it: undoing one push named the whole context.
#[test]
fn a_recount_that_moves_no_figure_changes_nothing_an_undo_reports() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));
    let b = kernel.push(ContextItem::file("src/b.rs", "b"));
    kernel.recount();

    let mut events = kernel.subscribe();
    assert!(kernel.undo().unwrap());
    let Some(Event::ContextUndone {
        removed, changed, ..
    }) = events.try_recv().ok()
    else {
        panic!("an undo is a context change like any other")
    };
    assert_eq!(removed, vec![b]);
    assert!(
        changed.is_empty(),
        "{a} was recounted to the figure it had, which is not a change: {changed:?}"
    );
}

#[test]
fn an_undo_says_what_it_did() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));
    let b = kernel.push(ContextItem::file("src/b.rs", "b"));
    kernel.set_state([a, b], ContextState::Excluded, Some("too big".into()));

    let mut events = kernel.subscribe();

    // undoing the exclusion reverts two items and removes none
    assert!(kernel.undo().unwrap());
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
    assert!(kernel.undo().unwrap());
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
    while kernel.undo().unwrap() {
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

    assert!(kernel.undo().unwrap());
    assert!(
        kernel.item(a).unwrap().is_projected(),
        "the exclusion is off"
    );
    let _ = events.try_recv();

    assert!(
        kernel.redo().unwrap(),
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
    assert!(kernel.undo().unwrap());
    assert!(kernel.undo().unwrap());
    assert_eq!(kernel.items().len(), 1);
    let _ = drain(&mut events);

    assert!(kernel.redo().unwrap());
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

    assert!(kernel.undo().unwrap());
    assert_eq!(kernel.with_context(|c| c.redo_len()), 1);

    // a redo that reached across this would be overwriting it, not restoring anything
    kernel.push(ContextItem::file("src/c.rs", "c"));
    assert_eq!(kernel.with_context(|c| c.redo_len()), 0);
    assert!(!kernel.redo().unwrap());
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
    assert!(kernel.undo().unwrap());
    assert!(kernel.items().is_empty());

    assert!(kernel.push_all([]).is_empty());
}

/// And so is dropping a turn's calls. The model asked for three tools and somebody said no, which
/// is one thing that happened: a checkpoint each would make a single `undo` take back one refusal
/// and leave the other two, so the model would be looking at a turn where two of its calls were
/// answered and one was never mentioned. It is also what the depth is measured against - a model
/// asking for sixteen tools would otherwise spend the whole undo history on one keystroke.
#[tokio::test]
async fn cancelling_a_turn_s_calls_is_one_thing_the_user_did() {
    let kernel = kernel();
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![
            call("c1", "shell", json!({ "cmd": "a" })),
            call("c2", "shell", json!({ "cmd": "b" })),
            call("c3", "shell", json!({ "cmd": "c" })),
        ]),
    ])));
    kernel.add_tool(Arc::new(ConstTool::new("shell", "it ran!")));
    kernel.push(ContextItem::user("do three things"));

    // one step: the calls are prepared and nothing has run
    kernel.step().await.unwrap();
    let depth = kernel.with_context(|c| c.undo_len());

    assert_eq!(kernel.cancel_pending_calls("changed my mind"), 3);
    assert_eq!(
        kernel.with_context(|c| c.undo_len()),
        depth + 1,
        "three refusals would have spent three of the sixteen the user has"
    );

    assert!(kernel.undo().unwrap());
    assert!(
        !kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("changed my mind")),
        "one undo left part of the cancellation behind"
    );
}

/// And so is running them. The calls that run are one checkpoint, taken by the first result and
/// joined by the rest; the kernel's answer to a call nobody can run joins the turn it answers,
/// which is recorded in the same step. A checkpoint a result would let one `undo` leave some of a
/// turn's calls answered and one never mentioned - the shape the test above is about.
#[tokio::test]
async fn running_a_turn_s_calls_is_one_thing_that_happened() {
    let kernel = kernel();
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![
            call("c1", "shell", json!({ "cmd": "a" })),
            call("c2", "nothing", json!({})),
            call("c3", "shell", json!({ "cmd": "c" })),
        ]),
    ])));
    kernel.add_tool(Arc::new(ConstTool::new("shell", "it ran!")));
    kernel.push(ContextItem::user("do three things"));
    let depth = kernel.with_context(|c| c.undo_len());
    let results = |kernel: &nachalnik::Kernel| {
        kernel
            .items()
            .iter()
            .filter(|item| item.kind.name() == "tool_result")
            .count()
    };

    kernel.step().await.unwrap();
    assert_eq!(
        kernel.with_context(|c| c.undo_len()),
        depth + 1,
        "the turn and the answer to the call nobody can run are one checkpoint"
    );
    kernel.step().await.unwrap();
    assert_eq!(results(&kernel), 3);
    assert_eq!(
        kernel.with_context(|c| c.undo_len()),
        depth + 2,
        "two results would have spent two of the sixteen the user has"
    );

    assert!(kernel.undo().unwrap());
    assert_eq!(
        results(&kernel),
        1,
        "one undo left part of the batch behind"
    );
    assert!(kernel.undo().unwrap());
    assert_eq!(
        kernel.items().len(),
        1,
        "the turn went without the answer to its unknown call"
    );
}

/// Metadata rides with the operation it describes and takes no checkpoint of its own, but it is
/// new work: a redo that reached across it would put the old metadata back.
#[test]
fn an_annotation_is_not_overwritten_by_a_redo() {
    let kernel = kernel();
    let a = kernel.push(ContextItem::file("src/a.rs", "a"));
    kernel.push(ContextItem::file("src/b.rs", "b"));
    assert!(kernel.undo().unwrap());

    kernel.annotate(a, json!({ "expendable": true })).unwrap();
    assert!(
        !kernel.redo().unwrap(),
        "the redone future outlived an annotation"
    );
    assert_eq!(kernel.item(a).unwrap().meta, json!({ "expendable": true }));
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

/// An undo is refused while a turn holds calls, and the calls run against the turn that asked.
///
/// note: the undo took the turn back and left the calls: the tool ran anyway, its result was
/// recorded against a turn no longer there - so the projector dropped it and the model never
/// learned the command had run - and recording it took a checkpoint, so `redo` could not bring
/// the turn back either. Deciding and ready are the resting states that hold calls, and the
/// refusal covers both.
#[tokio::test]
async fn an_undo_is_refused_while_a_turn_holds_calls() {
    let kernel = kernel();
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("done"),
    ])));
    kernel.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    kernel.push(ContextItem::user("look"));

    let asking = kernel.step().await.unwrap();
    assert!(
        matches!(asking, nachalnik::State::Deciding { .. }),
        "{asking:?}"
    );
    let before = kernel.items();
    assert!(matches!(kernel.undo(), Err(nachalnik::Error::Busy)));
    assert!(matches!(kernel.redo(), Err(nachalnik::Error::Busy)));
    assert_eq!(kernel.items(), before, "nothing was undone");

    let request = kernel.pending_permissions()[0].id;
    let ready = kernel.decide(request, nachalnik::Grant::Allow).unwrap();
    assert!(matches!(ready, nachalnik::State::Ready { .. }), "{ready:?}");
    assert!(matches!(kernel.undo(), Err(nachalnik::Error::Busy)));

    kernel.step().await.unwrap();
    let projected = kernel.project();
    assert!(
        projected.skipped.is_empty(),
        "the result went out with the turn that asked for it: {:?}",
        projected.skipped
    );
    assert!(
        kernel.undo().unwrap(),
        "and at rest an undo is an undo again"
    );
}

/// A tool that writes into the context while it runs, the way `kamchatka`'s `context` tool does.
struct Notes(std::sync::OnceLock<Kernel>);

#[nachalnik::async_trait]
impl nachalnik::Tool for Notes {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec::new("note", "writes a note into the context")
    }

    async fn invoke(
        &self,
        _call: &nachalnik::ToolCall,
        _output: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
        self.0
            .get()
            .expect("the kernel was handed over")
            .push(ContextItem::file("notes.md", "noted"));

        Ok(nachalnik::ToolOutput::new("noted"))
    }
}

/// A batch of results is one undo even when something changed the context between two of them.
///
/// note: each result joined the checkpoint the first took, which assumed nothing else took one in
/// between - and a tool that edits the context while it runs does exactly that. One undo then took
/// back the note and the results recorded after it, and kept the one before: a turn with one call
/// answered and two not, repaired out of the next request.
#[tokio::test]
async fn a_batch_is_one_undo_whatever_happened_between_its_results() {
    let kernel = kernel();
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![
            call("c1", "echo", json!({})),
            call("c2", "note", json!({})),
            call("c3", "echo", json!({})),
        ]),
        ModelResponse::text("done"),
    ])));
    kernel.add_tool(Arc::new(ConstTool::new("echo", "said")));
    let notes = Arc::new(Notes(std::sync::OnceLock::new()));
    let _ = notes.0.set(kernel.clone());
    kernel.add_tool(notes);
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.unwrap();

    // the closing answer, and then the batch
    assert!(kernel.undo().unwrap());
    assert!(kernel.undo().unwrap());
    let left: Vec<_> = kernel.items().iter().map(|item| item.kind.name()).collect();
    assert_eq!(
        left,
        ["user_message", "assistant_message"],
        "the batch went whole, the note it wrote included"
    );
    // and it comes back whole
    assert!(kernel.redo().unwrap());
    let results = kernel
        .items()
        .iter()
        .filter(|item| item.kind.name() == "tool_result")
        .count();
    assert_eq!(results, 3);
}
