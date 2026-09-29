//! What an intervention does to a copy of a session.

use nachalnik::{Config, ContextId, ContextItem, ContextState, Kernel};
use nachalnik_eval::{
    Intervention, Subject,
    suite::dossier::DEPOT,
    suite::{ERRANDS, LISTING},
};

/// A kernel holding the depot dossier, and the items its notes became.
fn planted() -> (Kernel, Vec<ContextId>) {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::system(DEPOT.brief).pinned());
    let ids = DEPOT
        .notes
        .iter()
        .map(|note| kernel.push(ContextItem::memory(note.label, note.text)))
        .collect();

    (kernel, ids)
}

#[test]
fn an_exclusion_is_a_state_change_and_nothing_is_destroyed() {
    let (kernel, ids) = planted();
    let mut snapshot = kernel.snapshot();
    let before = snapshot.items.len();

    // the annex memo, which is the fifth note since the numeric red herring was planted second
    let annex = ids[4];
    let applied = Intervention::without([annex]).apply(&mut snapshot);

    assert_eq!(applied.touched, vec![annex]);
    // something moved, and it was pointed at something that was there
    assert!(!applied.is_empty());
    assert!(applied.missing.is_empty());
    assert!(applied.is_complete());
    assert!(applied.unpinned.is_empty());
    // still there, still itself, still nameable by the number the session knows it by
    assert_eq!(snapshot.items.len(), before);
    let moved = snapshot.items.iter().find(|i| i.id == annex).unwrap();
    assert_eq!(moved.state, ContextState::Excluded);
    assert!(moved.content.to_text().contains("annex"));
    assert!(moved.note.is_some());
    // and the live session never heard about it
    assert_eq!(kernel.item(annex).unwrap().state, ContextState::Active);
}

#[test]
fn an_errand_answers_out_of_its_own_result() {
    // note: two invariants, and neither is decoration. `Errand::args` reads the arguments out of
    // a string constant and falls back to an empty object rather than panicking, so a malformed
    // constant would hand every copy a call that names a tool and says nothing about what it was
    // called with - a quieter and worse failure than a parse error.
    //
    // note: the second is the design. An answer that could have been given without the result is
    // an answer whose support nothing is measuring, so the declared figure has to be in the
    // result, has to be restated in the answer, and must *not* be in the question - a figure the
    // asking supplied would be readable in every arm.
    // a loop over nothing passes, so there has to be something to loop over
    assert!(!ERRANDS.is_empty());
    for errand in ERRANDS {
        // the constant itself, because `args()` falls back to an empty object and so is an object
        // whatever the constant says
        assert!(
            serde_json::from_str::<serde_json::Value>(errand.args)
                .is_ok_and(|args| args.is_object()),
            "`{}` has arguments that are not a JSON object: {}",
            errand.label,
            errand.args
        );

        let digits = |text: &str| {
            text.chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
        };
        assert!(
            digits(errand.result).contains(&digits(errand.quotes)),
            "`{}` quotes {} and its result does not state it",
            errand.label,
            errand.quotes
        );
        assert!(
            errand.answered.contains(errand.quotes),
            "`{}` answers without restating {}",
            errand.label,
            errand.quotes
        );
        assert!(
            !digits(errand.asked).contains(&digits(errand.quotes)),
            "`{}` is asked in terms of {}, so the answer does not need the result",
            errand.label,
            errand.quotes
        );
    }
}

#[test]
fn excluding_a_tool_result_takes_its_call_down_and_eliding_one_does_not() {
    // note: the whole of what `Provenance` measures, settled offline. Every other experiment here
    // reaches for `Intervention::Without` because it leaves no marker in the request; what it does
    // leave is this - a conversation in which the call was never made - and the crate said so in a
    // doc comment for a long time before anything checked it.
    let subject = Subject::new(Kernel::new(Config::default()));
    let items = LISTING.install(&subject);
    let (called, result) = (items[1].id, items[2].id);
    let origin = subject.kernel().snapshot();

    // as it stands: the ask, the call, the result, the answer
    let whole = Kernel::resume(Config::default(), origin.clone()).project();
    assert_eq!(whole.included.len(), 4);
    assert_eq!(whole.messages.len(), 4);
    assert!(whole.repairs.is_empty());
    assert_eq!(
        whole
            .messages
            .iter()
            .filter(|m| !m.tool_calls.is_empty())
            .count(),
        1
    );

    // elided: the result keeps its place and keeps answering its call, so the turn keeps its shape
    // and there is nothing to repair
    let mut snapshot = origin.clone();
    Intervention::elided([result]).apply(&mut snapshot);
    let elided = Kernel::resume(Config::default(), snapshot).project();
    assert_eq!(elided.included.len(), 4);
    assert_eq!(elided.messages.len(), 4);
    assert!(elided.repairs.is_empty(), "{:?}", elided.repairs);
    assert_eq!(
        elided
            .messages
            .iter()
            .filter(|m| !m.tool_calls.is_empty())
            .count(),
        1
    );

    // excluded: the orphaned call is repaired away, which leaves an assistant turn with no content
    // and no answered calls, which is a turn the projector drops - so *two* items leave, and the
    // copy reads a conversation in which nothing was ever run
    let mut snapshot = origin;
    Intervention::without([result]).apply(&mut snapshot);
    let gone = Kernel::resume(Config::default(), snapshot).project();
    assert_eq!(gone.included.len(), 2);
    assert_eq!(gone.messages.len(), 2);
    assert_eq!(gone.repairs.len(), 1);
    assert!(gone.repairs[0].contains("its result is not in the projection"));
    assert!(gone.messages.iter().all(|m| m.tool_calls.is_empty()));
    assert_eq!(
        gone.skipped.iter().map(|left| left.id).collect::<Vec<_>>(),
        vec![called, result],
        "the call was named as well as the result"
    );

    // and the answer that quoted the figure is still there, with nothing behind it
    assert!(
        gone.messages.iter().any(|m| m
            .content
            .as_ref()
            .is_some_and(|said| said.to_text().contains("68,402"))),
        "the conclusion outlives its evidence, which is the situation being measured"
    );
}

#[test]
fn moving_a_pinned_item_is_allowed_and_recorded() {
    let (kernel, _) = planted();
    let mut snapshot = kernel.snapshot();
    let brief = snapshot.items[0].id;

    let applied = Intervention::without([brief]).apply(&mut snapshot);

    assert_eq!(applied.touched, vec![brief]);
    assert_eq!(applied.unpinned, vec![brief]);
}

#[test]
fn naming_an_item_that_is_not_there_is_reported() {
    let (kernel, _) = planted();
    let mut snapshot = kernel.snapshot();

    let applied = Intervention::without([ContextId(9_999)]).apply(&mut snapshot);

    assert!(applied.is_empty());
    assert!(!applied.is_complete());
    assert_eq!(applied.missing, vec![ContextId(9_999)]);
}

#[test]
fn only_keeps_what_it_names() {
    let (kernel, ids) = planted();
    let mut snapshot = kernel.snapshot();

    let applied = Intervention::only([ids[0], ids[1]]).apply(&mut snapshot);

    let projected: Vec<_> = snapshot
        .items
        .iter()
        .filter(|item| item.state.is_projected())
        .map(|item| item.id)
        .collect();
    assert_eq!(projected, vec![ids[0], ids[1]]);
    // both named items were in the copy, so neither is reported missing
    assert!(applied.missing.is_empty(), "{:?}", applied.missing);
}

/// `only` over a copy that holds just what it names excludes nothing and misses nothing.
#[test]
fn only_keeps_a_lone_item_and_reports_nothing_missing() {
    let kernel = Kernel::new(Config::default());
    let only_item = kernel.push(ContextItem::memory("notes/one", "the only note"));
    let mut snapshot = kernel.snapshot();

    let applied = Intervention::only([only_item]).apply(&mut snapshot);

    assert!(applied.missing.is_empty(), "{:?}", applied.missing);
    assert!(applied.is_complete());
}

/// A revision reports the item it moved, and names it as unpinned only when it was pinned.
///
/// note: `unpinned` records a pin being moved anyway; an ordinary item listed there would be a
/// claim about a pin nobody made.
#[test]
fn a_revision_reports_an_item_it_moved_and_names_the_pinned_one() {
    let (kernel, ids) = planted();
    let mut snapshot = kernel.snapshot();
    let pinned = snapshot.items[0].id;

    let revised = Intervention::revised(ids[3], "the annex was never built").apply(&mut snapshot);
    assert_eq!(revised.touched, vec![ids[3]]);
    assert!(revised.unpinned.is_empty(), "{:?}", revised.unpinned);

    let over_pin =
        Intervention::revised(pinned, "the brief is something else").apply(&mut snapshot);
    assert_eq!(over_pin.touched, vec![pinned]);
    assert_eq!(over_pin.unpinned, vec![pinned]);
}

#[test]
fn a_revision_replaces_what_an_item_says_and_a_plant_takes_a_fresh_number() {
    let (kernel, ids) = planted();
    let mut snapshot = kernel.snapshot();
    let next = snapshot.next_item;

    Intervention::Compound(vec![
        Intervention::revised(ids[3], "the annex was never built"),
        Intervention::planted(ContextItem::memory(
            "notes/late",
            "and nor was anything else",
        )),
    ])
    .apply(&mut snapshot);

    let revised = snapshot.items.iter().find(|i| i.id == ids[3]).unwrap();
    assert_eq!(revised.content.to_text(), "the annex was never built");
    // never an identifier the session has handed out before
    assert_eq!(snapshot.items.last().unwrap().id, ContextId(next));
    assert_eq!(snapshot.next_item, next + 1);
}

#[test]
fn an_intervention_says_what_it_is() {
    assert_eq!(Intervention::Nothing.describe(), "nothing moved");
    assert_eq!(
        Intervention::without([ContextId(4), ContextId(7)]).describe(),
        "without 4, 7"
    );
    assert_eq!(
        Intervention::revised(ContextId(4), "x").describe(),
        "with 4 saying something else"
    );
    // in the word the result is read back in, which is the variant's own
    assert_eq!(
        Intervention::elided([ContextId(4)]).describe(),
        "with 4 elided"
    );
}
