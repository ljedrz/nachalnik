//! What holds of a context and its projection whatever has just been done to them.
//!
//! note: invariants rather than a reference model, deliberately. A model faithful enough to
//! compare against is a second implementation of the context that has to be kept honest, and a
//! wrong model reads exactly like a broken kernel. What is here instead are sentences this
//! workspace has already written down - in `Kernel::set_state`'s doc, in `ContextState`'s, in the
//! note on `Kernel::undo` about granularity - checked after every operation of a generated
//! sequence rather than after the one sequence somebody thought of.
//!
//! note: it earned its place on its fourth generated sequence, with a request in which a tool
//! result sat four messages away from the call it answered - the shape every OpenAI-compatible
//! API refuses. `projection.rs` counted what a turn was waiting for and reset the count on the
//! next turn, so a result belonging to an older turn had nothing anchoring it. The fix is in that
//! file; the assertion that found it is `a_context_and_its_projection_agree_after_every_operation`
//! below, and it is stated at full strength rather than narrowed to what held at the time.
//!
//! note: the family worth the most is the one where two things must agree about one request. Every
//! bug worth fixing in the recent releases was a member of it: the chat drawn from one account and
//! the request built from another, a figure in a corner that counted an item the projection had
//! repaired away, an elided item subtracted as though it had been sent. A projection and a budget
//! taken from the same kernel a moment apart are two accounts of one request, and they are
//! checked against each other here rather than against a number written down by hand.

use std::collections::{BTreeMap, BTreeSet};

use nachalnik::{
    Config, Content, ContextId, ContextItem, ContextKind, ContextState, Kernel, Snapshot,
    ToolCallId, test::call,
};
use proptest::{
    prelude::*,
    strategy::ValueTree,
    test_runner::{Config as RunnerConfig, TestRunner},
};
use serde_json::json;

/// One thing somebody can do to a context, in the kernel's own vocabulary.
#[derive(Debug, Clone)]
enum Op {
    /// One item.
    Push(Pushed),
    /// Several at once, which is one operation and therefore one undo.
    PushAll(usize),
    /// The moves, over one item or several: this is the whole of context control.
    SetState(Vec<usize>, ContextState),
    /// New content under the same identifier.
    Replace(usize, String),
    /// New content beside an item that keeps its own, marked superseded.
    Supersede(usize, String),
    Undo,
    Redo,
    /// Identifiers claimed without an item to go with them, the way a client importing turns it
    /// did not issue claims them.
    ///
    /// note: not a context operation, and here because `Snapshot::used_calls` is. Nothing else in
    /// this alphabet puts anything in that field - no turn runs, so no call is ever issued - and
    /// the snapshot property below would carry an always-empty one without it.
    Reserve(usize),
}

/// What kind of item to add, chosen so that a sequence can build the shape a repair is for.
#[derive(Debug, Clone, Copy)]
enum Pushed {
    User,
    System,
    File,
    /// A turn that asks for a tool.
    Call,
    /// A turn that asks for two at once.
    ///
    /// note: in the alphabet because `Call` makes turns with exactly one call, and with only
    /// those, two branches never run: the arithmetic that decides whether a turn's results were
    /// already next to it, and the half of the adjacency rule that says nothing else may arrive
    /// until *every* call in a turn has an answer. Found by asking what the reachability test
    /// below should count, which is the argument for having written it.
    Calls,
    /// A result answering the oldest call that has none yet.
    Answer,
    /// A result answering a call nobody made.
    Orphan,
    /// A result answering the call the *next* `Call` will ask for, so that a result can precede
    /// the turn that asked for it.
    ///
    /// note: in the alphabet because without it the ordering pass's deferral cannot be tested at
    /// all. `Answer` only ever answers a call that already exists, so every result it makes is
    /// already after its call, and the branch that holds a result back until its turn has been
    /// emitted is never reached - measured, by taking the branch out and watching nothing fail.
    Early,
    /// Bytes that are not text, which the default counter will not price.
    ///
    /// note: in the alphabet because without it the invariant about `uncounted` cannot fail.
    /// Measured: with nothing unpriced anywhere in a generated context, a kernel changed to
    /// report every request as fully counted broke no property here at all. An invariant that
    /// cannot fire reads exactly like one that holds.
    Picture,
}

/// A kernel, and what the sequence so far entitles us to assume about it.
struct World {
    kernel: Kernel,
    /// Every identifier seen, and the source and label it was seen with. An identifier is not
    /// reused, so neither is an identity.
    identity: BTreeMap<ContextId, (String, String)>,
    /// The calls asked for so far, and how many have been answered.
    calls: Vec<ToolCallId>,
    answered: usize,
}

impl World {
    fn new() -> Self {
        Self {
            kernel: Kernel::new(Config::default()),
            identity: BTreeMap::new(),
            calls: Vec::new(),
            answered: 0,
        }
    }

    /// The identifiers as the context has them, in order.
    fn ids(&self) -> Vec<ContextId> {
        self.kernel.items().iter().map(|item| item.id).collect()
    }

    fn item(&mut self, what: Pushed) -> ContextItem {
        match what {
            Pushed::User => ContextItem::user("a question"),
            Pushed::System => ContextItem::system("be brief"),
            Pushed::File => ContextItem::file("src/a.rs", "fn main() {}"),
            Pushed::Call => {
                let id = format!("c{}", self.calls.len());
                self.calls.push(ToolCallId::from(id.as_str()));

                ContextItem::assistant("running it", vec![call(&id, "peek", json!({}))])
            }
            Pushed::Calls => {
                let (first, second) = (
                    format!("c{}", self.calls.len()),
                    format!("c{}", self.calls.len() + 1),
                );
                self.calls.push(ToolCallId::from(first.as_str()));
                self.calls.push(ToolCallId::from(second.as_str()));

                ContextItem::assistant(
                    "running both",
                    vec![
                        call(&first, "peek", json!({})),
                        call(&second, "peek", json!({})),
                    ],
                )
            }
            Pushed::Answer => match self.calls.get(self.answered).cloned() {
                Some(id) => {
                    self.answered += 1;

                    ContextItem::tool_result(id, "peek", "it says so", false)
                }
                // nothing to answer, so this is a message instead - the sequence is a sequence of
                // what a client did, and a client cannot answer a call that was never asked
                None => ContextItem::user("nothing to answer"),
            },
            Pushed::Picture => ContextItem::user(Content::blob("image/png", "aGVsbG8=")),
            Pushed::Early => {
                let next = format!("c{}", self.calls.len());

                ContextItem::tool_result(ToolCallId::from(next.as_str()), "peek", "early", false)
            }
            Pushed::Orphan => ContextItem::tool_result(
                ToolCallId::from("never-asked"),
                "peek",
                "answering nobody",
                false,
            ),
        }
    }

    /// Does one thing, and says nothing about whether it worked: an operation that cannot apply
    /// is a legitimate thing for a client to attempt and the invariants have to hold after it.
    fn apply(&mut self, op: Op) {
        let ids = self.ids();
        let pick = |n: usize| ids.get(n % ids.len().max(1)).copied();

        match op {
            Op::Push(what) => {
                let item = self.item(what);
                self.kernel.push(item);
            }
            Op::PushAll(n) => {
                let items: Vec<_> = (0..n % 5 + 1).map(|_| self.item(Pushed::User)).collect();
                self.kernel.push_all(items);
            }
            Op::SetState(picks, state) => {
                let targets: Vec<_> = picks.iter().filter_map(|n| pick(*n)).collect();
                self.kernel.set_state(targets, state, None);
            }
            Op::Replace(n, text) => {
                if let Some(id) = pick(n) {
                    let _ = self.kernel.replace(id, text);
                }
            }
            Op::Supersede(n, text) => {
                if let Some(id) = pick(n) {
                    let _ = self.kernel.supersede(id, ContextItem::user(text));
                }
            }
            Op::Undo => {
                self.kernel.undo();
            }
            Op::Redo => {
                self.kernel.redo();
            }
            Op::Reserve(n) => {
                let claimed = (0..n % 3 + 1).map(|k| ToolCallId::from(format!("r{k}").as_str()));
                self.kernel.reserve_calls(claimed);
            }
        }
    }
}

/// Everything that must be true of a context and the request it projects to.
fn holds(world: &mut World) -> Result<(), TestCaseError> {
    let items = world.kernel.items();
    let ids: Vec<ContextId> = items.iter().map(|item| item.id).collect();

    // an identifier is handed out once. `Snapshot::next_item` is stored rather than derived for
    // this reason, and a resumed session that reissued one would be the bug
    // `Event::ToolCallRepaired` exists to prevent, one layer down
    let next = world.kernel.with_context(|context| context.next_id());
    for item in &items {
        prop_assert!(item.id.0 < next, "{} is not below next_id {next}", item.id);

        let identity = (item.source.clone(), item.label.clone());
        match world.identity.get(&item.id) {
            Some(known) => {
                prop_assert_eq!(known, &identity, "item {} is a different item now", item.id)
            }
            None => {
                world.identity.insert(item.id, identity);
            }
        }
    }
    let unique: BTreeSet<&ContextId> = ids.iter().collect();
    prop_assert_eq!(unique.len(), ids.len(), "an identifier appears twice");

    let projection = world.kernel.project();
    let included: BTreeSet<ContextId> = projection.included.iter().copied().collect();
    let skipped: BTreeSet<ContextId> = projection.skipped.iter().map(|one| one.id).collect();
    let present: BTreeSet<ContextId> = ids.iter().copied().collect();

    // the projection is an account of items that exist, and each of them is in exactly one of its
    // two columns - a client showing what is being sent and what is being held reads both
    prop_assert!(
        included.is_subset(&present),
        "the projection included something absent"
    );
    prop_assert!(
        skipped.is_subset(&present),
        "the projection skipped something absent"
    );
    prop_assert!(included.is_disjoint(&skipped), "an item both sent and held");

    // the context's order is not the request's, and the difference is never silent. A tool result
    // has to follow the call it answers, so an item that landed between the two is held back -
    // which is a reordering, and one this projector names in `repairs`. What must hold is that
    // nothing is moved *quietly*: if the request is not in the order the context is, something
    // says why.
    let order: Vec<ContextId> = ids
        .iter()
        .copied()
        .filter(|id| included.contains(id))
        .collect();
    if projection.included != order {
        prop_assert!(
            !projection.repairs.is_empty(),
            "the projection reordered the context and said nothing: {:?} against {:?}",
            projection.included,
            order
        );
    }

    // a state that does not project does not contribute. The converse is deliberately not
    // asserted: an item the projector repaired away says it is sending and is not being sent
    for item in &items {
        if !item.state.is_projected() {
            prop_assert!(
                !included.contains(&item.id),
                "item {} is {} and was included",
                item.id,
                item.state
            );
        }
    }

    // every call in the request is answered in the request, and every answer answers a call that
    // is in it. This is what `repair_orphans` is for, and most providers reject a request where
    // it does not hold
    let asked: BTreeSet<ToolCallId> = projection
        .messages
        .iter()
        .flat_map(|message| message.tool_calls.iter().map(|one| one.id.clone()))
        .collect();
    let answered: BTreeSet<ToolCallId> = projection
        .messages
        .iter()
        .filter_map(|message| message.tool_call_id.clone())
        .collect();
    prop_assert_eq!(&asked, &answered, "an orphan survived the projection");

    // and the reason the order gives way: a result reaches the wire immediately after the call it
    // answers, with nothing in between and nothing else until every call in that turn has one.
    // This is the shape the conventional dialect requires and refuses the whole request over,
    // naming the identifier that went unanswered.
    //
    // note: this is the assertion the file was written for. It failed on its fourth generated
    // sequence - two assistant turns in a row, the first one's result recorded after the second -
    // because the projector tracked what a turn was waiting for as a count and reset it on the
    // next turn, leaving a result belonging to an older turn with nothing anchoring it. The
    // ordering is a pass over the whole list now, and this says the strong thing rather than the
    // weaker one that held while the bug did.
    let mut outstanding: BTreeSet<ToolCallId> = BTreeSet::new();
    for message in &projection.messages {
        match message.tool_call_id.as_ref() {
            Some(answers) => prop_assert!(
                outstanding.remove(answers),
                "a result for {answers} that no call immediately before it was waiting for"
            ),
            None => {
                prop_assert!(
                    outstanding.is_empty(),
                    "a {:?} message arrived with {} call(s) of the turn before still unanswered",
                    message.role,
                    outstanding.len()
                );
                outstanding = message
                    .tool_calls
                    .iter()
                    .map(|one| one.id.clone())
                    .collect();
            }
        }
    }
    prop_assert!(
        outstanding.is_empty(),
        "the request ends with {} call(s) nobody answered",
        outstanding.len()
    );

    // the two accounts of one request. A budget is counted over the projected messages, so a
    // count taken from the projection a moment later has to be the same figure - and the one that
    // matters is `uncounted`, because an elided payload is not a hole in a request that no longer
    // carries it
    let budget = world.kernel.budget();
    let counter = world.kernel.counter();
    let counted: usize = projection
        .messages
        .iter()
        .map(|m| counter.count_message(m))
        .sum();
    let unpriced: usize = projection
        .messages
        .iter()
        .map(|m| counter.uncounted_message(m))
        .sum();
    prop_assert_eq!(
        budget.context_tokens,
        counted,
        "two accounts of what the request costs"
    );
    prop_assert_eq!(
        budget.uncounted,
        unpriced,
        "two accounts of what nobody priced"
    );
    prop_assert_eq!(
        budget.fully_counted(),
        unpriced == 0,
        "abstaining out loud disagrees with itself"
    );

    Ok(())
}

/// Sequence lengths that reach past the undo depth, which is sixteen by default: an undo stack is
/// a bounded thing and the interesting arithmetic is at and past its bound.
fn ops() -> impl Strategy<Value = Vec<Op>> {
    let pushed = prop_oneof![
        1 => Just(Pushed::User),
        1 => Just(Pushed::System),
        1 => Just(Pushed::File),
        2 => Just(Pushed::Call),
        2 => Just(Pushed::Calls),
        2 => Just(Pushed::Answer),
        1 => Just(Pushed::Orphan),
        2 => Just(Pushed::Picture),
        2 => Just(Pushed::Early),
    ];
    let state = prop_oneof![
        Just(ContextState::Active),
        Just(ContextState::Excluded),
        Just(ContextState::Pinned),
        Just(ContextState::Elided),
        Just(ContextState::Archived),
        Just(ContextState::Superseded),
    ];
    let op = prop_oneof![
        4 => pushed.prop_map(Op::Push),
        1 => (0usize..8).prop_map(Op::PushAll),
        3 => (prop::collection::vec(0usize..12, 1..4), state).prop_map(|(ids, s)| Op::SetState(ids, s)),
        1 => (0usize..12, "[a-z ]{0,20}").prop_map(|(n, t)| Op::Replace(n, t)),
        1 => (0usize..12, "[a-z ]{0,20}").prop_map(|(n, t)| Op::Supersede(n, t)),
        2 => Just(Op::Undo),
        2 => Just(Op::Redo),
        1 => (0usize..3).prop_map(Op::Reserve),
    ];

    prop_oneof![
        1 => prop::collection::vec(op.clone(), 1..6),
        3 => prop::collection::vec(op, 14..40),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// note: checked after *every* operation rather than at the end of the sequence, so that a
    /// counterexample is the shortest prefix that breaks something rather than a sequence with
    /// the interesting part somewhere in it.
    #[test]
    fn a_context_and_its_projection_agree_after_every_operation(ops in ops()) {
        let mut world = World::new();
        holds(&mut world)?;

        for op in ops {
            world.apply(op);
            holds(&mut world)?;
        }
    }

    /// note: what a resume is for is a conversation that carries on, so the assertion is that the
    /// resumed session projects to the same request rather than merely holding the same items -
    /// the items being equal and the request differing is exactly the kind of disagreement this
    /// file is about. Through serde on the way, because that is the form a snapshot survives in
    /// and a field that serialises but does not come back reads as an empty one.
    ///
    /// note: `Snapshot` derives `PartialEq`, which makes the whole of it one assertion. Worth
    /// saying because resuming *recounts* - the docs on `Kernel::resume` are explicit that a
    /// resumed context comes back with corrected figures rather than the ones it was saved with -
    /// so a snapshot equal to its own round trip is a claim that nothing in these sequences makes
    /// the saved figures stale. A counter that had learned something would; none here has, since
    /// no turn runs.
    #[test]
    fn a_snapshot_resumes_into_the_session_it_was_taken_from(ops in ops()) {
        let mut world = World::new();
        for op in ops {
            world.apply(op);
        }

        let taken = world.kernel.snapshot();
        let json = serde_json::to_string(&taken).expect("a snapshot serialises");
        let read: Snapshot = serde_json::from_str(&json).expect("and reads back");
        prop_assert_eq!(&read, &taken, "the snapshot did not survive its own serde");

        let resumed = Kernel::resume(Config::default(), read);
        prop_assert_eq!(resumed.items(), world.kernel.items(), "the items came back");
        prop_assert_eq!(resumed.project(), world.kernel.project(), "the request came back");
        prop_assert_eq!(&resumed.snapshot(), &taken, "and it can be saved again unchanged");
    }

    /// note: the granularity `Kernel::undo` documents - one operation, not one item - is what
    /// makes this checkable at all. `push_all` and a `set_state` over several identifiers are in
    /// the alphabet precisely because they are the operations whose undo covers more than one
    /// item, and they are where a stack that counted items would come apart.
    #[test]
    fn undoing_to_the_beginning_and_redoing_puts_everything_back(ops in ops()) {
        let mut world = World::new();
        for op in ops {
            world.apply(op);
        }

        let before = world.kernel.items();
        let mut depth = 0;
        while world.kernel.undo() {
            depth += 1;
        }
        for _ in 0..depth {
            prop_assert!(world.kernel.redo(), "the stack ran out before it was refilled");
        }

        prop_assert_eq!(world.kernel.items(), before, "a round trip through the undo stack");
        holds(&mut world)?;
    }
}

/// An operation that changes nothing takes no checkpoint, so the next undo walks back the last
/// operation that *did* something rather than doing nothing at all.
///
/// note: a case rather than a property, because the state to set is the state the item is already
/// in and a generator asked for that would mostly be asked for something else. The cost of
/// getting it wrong is the whole undo stack becoming unreliable in exactly the situation where
/// somebody reaches for it - a client whose keybinding sets a state unconditionally would spend
/// every checkpoint on nothing.
#[test]
fn a_change_that_changes_nothing_is_not_undoable() {
    let kernel = Kernel::new(Config::default());
    let first = kernel.push(ContextItem::user("a question"));
    kernel.set_state([first], ContextState::Excluded, None);

    let after_real_work = kernel.items();

    // the same state again, and again: nothing changes, so nothing is checkpointed
    for _ in 0..3 {
        let outcome = kernel.set_state([first], ContextState::Excluded, None);
        assert!(outcome.changed.is_empty(), "{outcome:?}");
        assert_eq!(outcome.unchanged, vec![first]);
    }
    assert_eq!(kernel.items(), after_real_work, "a no-op moved the context");

    // one undo, and it reaches past all three of them to the exclusion
    assert!(kernel.undo());
    assert_eq!(
        kernel.item(first).unwrap().state,
        ContextState::Active,
        "the undo spent itself on a no-op instead of the exclusion"
    );
}

/// One operation is one undo, whatever it touched.
///
/// note: the property above cannot see this. Undoing everything and redoing everything restores
/// the context whether a checkpoint was spent per operation or per item, so the round trip holds
/// either way and the granularity - the thing `Kernel::undo` actually documents - goes unchecked.
/// This is the assertion that fails when a bulk operation quietly becomes several.
#[test]
fn one_operation_is_one_undo_however_many_items_it_touched() {
    let kernel = Kernel::new(Config::default());

    // several items in, in one operation, and one undo takes all of them away
    let pushed = kernel.push_all((0..5).map(|n| ContextItem::user(format!("question {n}"))));
    assert_eq!(pushed.len(), 5);
    assert!(kernel.undo());
    assert!(
        kernel.items().is_empty(),
        "one push_all took {} undo(s)",
        kernel.items().len()
    );
    assert!(kernel.redo());
    assert_eq!(kernel.items().len(), 5);

    // and the same for a state change over several of them at once
    let outcome = kernel.set_state(pushed.iter().copied(), ContextState::Excluded, None);
    assert_eq!(outcome.changed.len(), 5);
    assert!(kernel.undo());
    for id in &pushed {
        assert_eq!(
            kernel.item(*id).unwrap().state,
            ContextState::Active,
            "item {id} needed an undo of its own"
        );
    }
}

/// What a run of the generators above actually reaches.
///
/// note: this exists because three of the properties in this workspace were measurably weaker
/// than they read, and each was found the slow way - by breaking the implementation on purpose
/// and noticing that nothing failed. A name strategy topping out below the length limit it was
/// written to test; an alphabet with nothing unpriced in it, so the invariant about abstaining
/// could not fail; an alphabet where a result never preceded its call, so the branch that holds
/// one back was never entered. In every case the test read like a thorough one.
///
/// So the states the properties are *about* are counted here and asserted to occur. It is not a
/// coverage measurement and does not try to be one: it is a short list of the specific
/// configurations that the assertions above have a branch for, and it fails when a reweighted
/// `prop_oneof!` quietly stops producing one of them. That is the failure mode this cannot
/// otherwise see, because a property that never reaches its case passes.
#[test]
fn the_generators_reach_what_the_properties_are_about() {
    #[derive(Default, Debug)]
    struct Reached {
        /// A sequence longer than the undo depth, which is where the stack's arithmetic is.
        past_the_undo_depth: usize,
        /// A projection carrying something the counter would not price.
        something_unpriced: usize,
        /// A result recorded before the call it answers, which is the deferral.
        a_result_before_its_call: usize,
        /// A turn with more than one call, both of them answered.
        a_turn_with_two_answers: usize,
        /// The ordering pass actually moving a result.
        a_result_moved: usize,
        /// An orphan actually dropped, which is the other repair.
        an_orphan_dropped: usize,
        /// An undo that undid something, and a redo that put it back.
        an_undo_that_did_something: usize,
        a_redo_that_did_something: usize,
        /// A superseded item, since `supersede` is the only thing that makes that state.
        something_superseded: usize,
        /// A snapshot with claimed identifiers in it.
        a_reserved_identifier: usize,
    }

    let mut runner = TestRunner::new(RunnerConfig {
        cases: 512,
        ..RunnerConfig::default()
    });
    let strategy = ops();
    let mut reached = Reached::default();

    for _ in 0..512 {
        let ops = strategy
            .new_tree(&mut runner)
            .expect("the strategy produces a sequence")
            .current();
        if ops.len() > Config::default().context_undo_depth {
            reached.past_the_undo_depth += 1;
        }

        let mut world = World::new();
        for op in ops {
            let undoing = matches!(op, Op::Undo);
            let redoing = matches!(op, Op::Redo);
            let before = world.ids();
            world.apply(op);

            if undoing && world.ids() != before {
                reached.an_undo_that_did_something += 1;
            }
            if redoing && world.ids() != before {
                reached.a_redo_that_did_something += 1;
            }

            let items = world.kernel.items();
            if items
                .iter()
                .any(|item| item.state == ContextState::Superseded)
            {
                reached.something_superseded += 1;
            }

            // a result whose call is somewhere after it in the context: the ordering pass has to
            // hold it back rather than emit it where it sits
            let asked_at = |wanted: &ToolCallId| {
                items
                    .iter()
                    .position(|item| item.calls().any(|call| &call.id == wanted))
            };
            for (at, item) in items.iter().enumerate() {
                if let ContextKind::ToolResult { call, .. } = &item.kind
                    && asked_at(call).is_some_and(|asked| asked > at)
                {
                    reached.a_result_before_its_call += 1;
                }
            }
            if items
                .iter()
                .any(|item| item.calls().count() > 1 && item.state.is_projected())
            {
                let projection = world.kernel.project();
                let answers = projection
                    .messages
                    .iter()
                    .filter(|message| message.tool_call_id.is_some())
                    .count();
                if answers > 1 {
                    reached.a_turn_with_two_answers += 1;
                }
            }

            if !world.kernel.snapshot().used_calls.is_empty() {
                reached.a_reserved_identifier += 1;
            }

            let projection = world.kernel.project();
            if world.kernel.budget().uncounted > 0 {
                reached.something_unpriced += 1;
            }
            for repair in &projection.repairs {
                if repair.contains("moved item") {
                    reached.a_result_moved += 1;
                }
                if repair.contains("dropped the call") {
                    reached.an_orphan_dropped += 1;
                }
            }
        }
    }

    // every field is a branch one of the assertions above has, so a zero is a property that
    // cannot fail rather than one that holds
    let counts = [
        ("past_the_undo_depth", reached.past_the_undo_depth),
        ("something_unpriced", reached.something_unpriced),
        ("a_result_before_its_call", reached.a_result_before_its_call),
        ("a_turn_with_two_answers", reached.a_turn_with_two_answers),
        ("a_result_moved", reached.a_result_moved),
        ("an_orphan_dropped", reached.an_orphan_dropped),
        (
            "an_undo_that_did_something",
            reached.an_undo_that_did_something,
        ),
        (
            "a_redo_that_did_something",
            reached.a_redo_that_did_something,
        ),
        ("something_superseded", reached.something_superseded),
        ("a_reserved_identifier", reached.a_reserved_identifier),
    ];
    for (what, count) in counts {
        assert!(count > 0, "nothing generated reached {what}: {reached:#?}");
    }
}
