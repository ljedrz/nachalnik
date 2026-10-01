//! `kamchatka reconcile`: forks of one session folded back into one.
//!
//! note: the forks are real ones - an ancestor kernel's snapshot resumed twice and carried on
//! differently - because what is being claimed is about what a fork of this runtime looks like:
//! the same items under the same numbers up to where they part, and each fork numbering its own
//! next item the same.

use std::sync::Arc;

use kamchatka::reconcile::{Fork, Reconciled, reconcile};
use nachalnik::{
    Calibration, Config, ContextId, ContextItem, ContextKind, ContextState, Event, Kernel,
    ModelInfo, Snapshot, test::ScriptedProvider,
};
use serde_json::json;

mod common;

/// A session of three items: an instruction, a question and an answer.
fn ancestor() -> Snapshot {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::system("be brief"));
    kernel.push(ContextItem::user("where should the annex go?"));
    kernel.push(ContextItem::assistant(
        "I will look at both sides first",
        vec![],
    ));

    kernel.snapshot()
}

/// The ancestor carried on somewhere else.
fn fork_of(ancestor: &Snapshot) -> Kernel {
    Kernel::resume(Config::default(), ancestor.clone())
}

/// What the `context` tool's `note` writes.
fn note(label: &str, content: &str) -> ContextItem {
    ContextItem::new(ContextKind::Reference, "agent", label, content).because("worth keeping")
}

/// A fork as `Fork::read` would hand it over: its snapshot, and its log beside it.
fn read(name: &str, kernel: &Kernel) -> Fork {
    Fork::new(name, kernel.snapshot(), kernel.history())
}

/// Two forks that each went on for a while and wrote down where the annex should go.
fn two_forks() -> (Kernel, Kernel) {
    two_forks_of(&ancestor())
}

/// [`two_forks`], of an ancestor the caller holds.
fn two_forks_of(ancestor: &Snapshot) -> (Kernel, Kernel) {
    let (a, b) = (fork_of(ancestor), fork_of(ancestor));
    a.push(ContextItem::user("try the north side"));
    a.push(ContextItem::assistant("the north side floods", vec![]));
    a.push(note("plan", "build on the south side"));
    a.push(note("soil", "the north side is clay"));
    b.push(ContextItem::user("try the south side"));
    b.push(note("plan", "build on the north side"));
    // a reference, and not the agent's: a fork's reading stays with the fork
    b.push(ContextItem::file("survey.txt", "the south side is rock"));

    (a, b)
}

fn merged(forks: &[Fork]) -> Reconciled {
    reconcile(forks, "merged").expect("they are forks of one session")
}

fn item(reconciled: &Reconciled, id: u64) -> &ContextItem {
    reconciled
        .snapshot
        .items
        .iter()
        .find(|item| item.id == ContextId(id))
        .unwrap_or_else(|| panic!("no item {id}: {:?}", reconciled.snapshot.items))
}

/// The shared part comes through whole, each fork brings only its notes, and two notes under one
/// label are both there and named.
#[test]
fn the_shared_part_is_kept_and_only_the_notes_come_from_each_fork() {
    let (a, b) = two_forks();
    let reconciled = merged(&[read("a", &a), read("b", &b)]);

    assert_eq!(reconciled.shared_through, ContextId(3));
    // past every number either fork handed out: a went to 7, so the manifest is 8
    let ids: Vec<u64> = reconciled.snapshot.items.iter().map(|i| i.id.0).collect();
    assert_eq!(ids, [1, 2, 3, 8, 9, 10, 11]);
    assert!(
        reconciled.snapshot.items.iter().all(|item| {
            let said = item.content.to_text();
            !said.contains("try the") && !said.contains("is rock")
        }),
        "a fork's own turns came through: {:?}",
        reconciled.snapshot.items
    );

    let manifest = item(&reconciled, 8);
    assert!(matches!(manifest.kind, ContextKind::System));
    assert_eq!(manifest.state, ContextState::Pinned);
    let said = manifest.content.to_text();
    for phrase in [
        "They agree up to item 3",
        "a: 4 items, b: 3 items",
        "- a: [9] `plan`, [10] `soil`",
        "- b: [11] `plan`",
        "`plan` is the label of more than one note here - [9] from a, [11] from b",
    ] {
        assert!(said.contains(phrase), "{phrase:?} is not in: {said}");
    }

    // the labels are the forks' own, and which fork each came from is in the metadata
    let (south, north) = (item(&reconciled, 9), item(&reconciled, 11));
    assert_eq!(
        (south.label.as_str(), north.label.as_str()),
        ("plan", "plan")
    );
    assert_eq!(south.content.to_text(), "build on the south side");
    assert_eq!(north.meta["reconciled"], json!({ "from": "b", "id": 5 }));
    assert_eq!(south.meta["reconciled"], json!({ "from": "a", "id": 6 }));
    assert_eq!(south.included_because.as_deref(), Some("worth keeping"));

    assert!(
        reconciled
            .said
            .iter()
            .any(|line| line == "a: 2 notes carried, and 2 items left behind"),
        "{:?}",
        reconciled.said
    );
    assert_eq!(reconciled.snapshot.session, "merged");
}

/// The fresh log begins with the resume of the shared part, names each item taken and where it
/// came from, and checks clean against the snapshot beside it.
#[test]
fn the_log_is_a_fresh_one_that_names_what_was_taken_from_where() {
    let (a, b) = two_forks();
    let reconciled = merged(&[read("a", &a), read("b", &b)]);

    match &reconciled.records[0].event {
        Event::SessionResumed { session, items, .. } => {
            assert_eq!((session.as_str(), *items), ("merged", 3));
        }
        other => panic!("the log begins with {other:?}"),
    }
    let added: Vec<(u64, String)> = reconciled
        .records
        .iter()
        .filter_map(|record| match &record.event {
            Event::ContextAdded { id, meta, .. } => Some((
                id.0,
                meta["reconciled"]["from"].as_str().unwrap_or("").to_owned(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        added,
        [
            (8, String::new()),
            (9, "a".to_owned()),
            (10, "a".to_owned()),
            (11, "b".to_owned())
        ]
    );
    // numbered on from where the forks' logs left off, so no record here shares a number with one
    // of theirs
    let past = a.snapshot().last_seq.max(b.snapshot().last_seq);
    assert_eq!(reconciled.records[0].seq, past + 1);

    let log: String = reconciled
        .records
        .iter()
        .map(|record| serde_json::to_string(record).unwrap() + "\n")
        .collect();
    let snapshot = serde_json::to_string(&reconciled.snapshot).unwrap();
    let checked = kamchatka::check::check(Some(&log), Some(&snapshot));
    assert!(checked.findings.is_empty(), "{:?}", checked.findings);
    assert!(
        reconciled.snapshot.problems().is_empty(),
        "{:?}",
        reconciled.snapshot.problems()
    );
}

/// Two sessions that are not forks of each other are refused rather than put end to end.
#[test]
fn two_sessions_that_share_nothing_are_refused() {
    let a = fork_of(&ancestor());
    let b = Kernel::new(Config::default());
    b.push(ContextItem::system("be thorough"));

    let refused = reconcile(&[read("a", &a), read("b", &b)], "merged").unwrap_err();
    assert!(
        refused.contains("a and b are not forks of one session"),
        "{refused}"
    );
}

/// A shared item each fork left in a different state is given the most included of them, and
/// says what each fork had.
#[test]
fn a_shared_item_takes_the_most_included_state_any_fork_left_it_in() {
    let (a, b) = two_forks();
    a.set_state([ContextId(2)], ContextState::Pinned, None);
    b.set_state(
        [ContextId(2)],
        ContextState::Excluded,
        Some("not needed".into()),
    );
    b.set_state(
        [ContextId(3)],
        ContextState::Elided,
        Some("compacted".into()),
    );
    let reconciled = merged(&[read("a", &a), read("b", &b)]);

    let question = item(&reconciled, 2);
    assert_eq!(question.state, ContextState::Pinned);
    assert_eq!(
        question.note, None,
        "the note goes with the state it explains"
    );
    assert_eq!(
        question.meta["reconciled"]["states"],
        json!({ "a": "pinned", "b": "excluded" })
    );
    let answer = item(&reconciled, 3);
    assert_eq!(answer.state, ContextState::Active);
    assert_eq!(item(&reconciled, 1).meta, serde_json::Value::Null);
    assert!(
        reconciled
            .said
            .iter()
            .any(|line| line.starts_with("2 shared items are in a different state")),
        "{:?}",
        reconciled.said
    );
}

/// Revises an item the way a person's `e` does: the content replaced, and the metadata saying so.
fn revise(kernel: &Kernel, id: u64, content: &str) {
    kernel.replace(ContextId(id), content).expect("there");
    kernel
        .annotate(
            ContextId(id),
            json!({ "revised": { "by": "user", "reason": "a correction" } }),
        )
        .expect("there");
}

/// One fork's revision of a shared item is kept, and two forks revising it differently is
/// refused, naming both.
#[test]
fn a_shared_item_one_fork_revised_is_kept_as_revised_and_two_revisions_are_refused() {
    let (a, b) = two_forks();
    revise(&a, 2, "where should the annex and the shed go?");
    let reconciled = merged(&[read("a", &a), read("b", &b)]);
    assert_eq!(reconciled.shared_through, ContextId(3));
    assert_eq!(
        item(&reconciled, 2).content.to_text(),
        "where should the annex and the shed go?"
    );
    assert_eq!(item(&reconciled, 2).meta["reconciled"]["revised_in"], "a");
    assert!(
        item(&reconciled, 8)
            .content
            .to_text()
            .contains("Item 2 is as a revised it after the forks parted."),
        "{}",
        item(&reconciled, 8).content.to_text()
    );

    revise(&b, 2, "where should the annex go, if anywhere?");
    let refused = reconcile(&[read("a", &a), read("b", &b)], "merged").unwrap_err();
    assert!(
        refused.contains("item 2 is shared by the forks and says something different in a and b"),
        "{refused}"
    );
}

/// The last shared item revised in one fork is taken for shared only where the fork's log holds
/// the overwrite, since nothing after it agrees.
///
/// note: without the log, a revised item and two forks' different next items look alike, so the
/// forks are taken to part before it.
#[test]
fn the_last_shared_item_revised_needs_the_log_to_say_it_is_one_item() {
    let (a, b) = two_forks();
    revise(&a, 3, "I will look at the north side first");

    let logged = merged(&[read("a", &a), read("b", &b)]);
    assert_eq!(logged.shared_through, ContextId(3));
    assert_eq!(
        item(&logged, 3).content.to_text(),
        "I will look at the north side first"
    );

    let unlogged = merged(&[
        Fork::new("a", a.snapshot(), Vec::new()),
        Fork::new("b", b.snapshot(), Vec::new()),
    ]);
    assert_eq!(unlogged.shared_through, ContextId(2));
    assert!(
        unlogged.said.iter().any(|line| line
            .starts_with("item 3 is revised in a fork, and taken for where the forks part")),
        "{:?}",
        unlogged.said
    );
}

/// The token counter's correction is summed across forks talking to one model, and dropped
/// across two.
#[test]
fn the_calibration_is_summed_for_one_model_and_dropped_for_two() {
    let talking_to = |kernel: &Kernel, model: &str| {
        kernel.set_provider(Arc::new(
            ScriptedProvider::new([]).with_info(ModelInfo::new("test", model)),
        ));
    };
    let learned = |kernel: &Kernel, estimated: u64, reported: u64| {
        let mut snapshot = kernel.snapshot();
        snapshot.calibration = Some(Calibration {
            scale: reported as f64 / estimated as f64,
            observations: 2,
            estimated,
            reported,
        });
        snapshot
    };

    let (a, b) = two_forks();
    talking_to(&a, "m1");
    talking_to(&b, "m1");
    let forks = [
        Fork::new("a", learned(&a, 100, 150), a.history()),
        Fork::new("b", learned(&b, 300, 450), b.history()),
    ];
    let one = merged(&forks);
    let calibration = one.snapshot.calibration.expect("one model");
    assert_eq!(
        (
            calibration.estimated,
            calibration.reported,
            calibration.observations
        ),
        (400, 600, 4)
    );
    assert_eq!(calibration.scale, 1.5);
    assert_eq!(one.model.as_deref(), Some("m1"));

    talking_to(&b, "m2");
    let forks = [
        Fork::new("a", learned(&a, 100, 150), a.history()),
        Fork::new("b", learned(&b, 300, 450), b.history()),
    ];
    let two = merged(&forks);
    // the counter says what it has learned, which here is nothing
    assert_eq!(two.snapshot.calibration, Some(Calibration::default()));
    assert_eq!(two.model, None);
    assert!(
        two.said.iter().any(|line| line
            == "the token counter's correction is dropped: a talked to m1 and b talked to m2"),
        "{:?}",
        two.said
    );
}

/// A parameter the forks disagree on is left out, and the ones they agree on are kept.
#[test]
fn a_parameter_the_forks_disagree_on_is_left_out() {
    let (a, b) = two_forks();
    let with = |kernel: &Kernel, temperature: f64| {
        let mut snapshot = kernel.snapshot();
        snapshot
            .params
            .insert("temperature".into(), json!(temperature));
        snapshot.params.insert("top_p".into(), json!(0.9));
        Fork::new("fork", snapshot, kernel.history())
    };
    let (mut first, mut second) = (with(&a, 0.2), with(&b, 0.7));
    first.name = "a".into();
    second.name = "b".into();
    let reconciled = merged(&[first, second]);

    assert_eq!(reconciled.snapshot.params.get("temperature"), None);
    assert_eq!(reconciled.snapshot.params["top_p"], json!(0.9));
    assert!(
        reconciled
            .said
            .iter()
            .any(|line| line.starts_with("`temperature` left out of the parameters")),
        "{:?}",
        reconciled.said
    );
}

/// The program writes the pair, will not write over it a second time, and what it wrote checks
/// clean.
#[test]
fn the_program_writes_a_pair_that_checks_clean_and_writes_over_nothing() {
    let dir = common::scratch("reconcile");
    let (a, b) = two_forks();
    for (name, kernel) in [("a", &a), ("b", &b)] {
        let log: String = kernel
            .history()
            .iter()
            .map(|record| serde_json::to_string(record).unwrap() + "\n")
            .collect();
        std::fs::write(dir.join(format!("{name}.jsonl")), log).expect("written");
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec(&kernel.snapshot()).unwrap(),
        )
        .expect("written");
    }
    let run = |args: &[&str]| {
        common::command()
            .current_dir(&dir)
            .args(args)
            .output()
            .expect("the binary under test is built")
    };

    let out = run(&["reconcile", "a.json", "b", "-o", "merged"]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(said.contains("the forks agree up to item 3"), "{said}");
    assert!(
        said.contains("`kamchatka -r merged.json` carries on"),
        "{said}"
    );
    let snapshot: Snapshot =
        serde_json::from_slice(&std::fs::read(dir.join("merged.json")).expect("written")).unwrap();
    assert_eq!(snapshot.items.len(), 7);

    let again = run(&["reconcile", "a.json", "b.json", "-o", "merged.json"]);
    assert!(!again.status.success());
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("is already there"),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );

    let twice = run(&["reconcile", "a.json", "a", "-o", "elsewhere"]);
    assert!(!twice.status.success());
    assert!(
        String::from_utf8_lossy(&twice.stderr).contains("a is named twice"),
        "{}",
        String::from_utf8_lossy(&twice.stderr)
    );

    let checked = run(&["--check", "merged"]);
    assert!(
        checked.status.success(),
        "{}{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
}

/// Two sessions that begin alike - the same instruction, from the same `-s` - are not forks of one
/// session, and are refused rather than reconciled on the instruction they share.
#[test]
fn sessions_that_only_begin_alike_are_refused() {
    let named = |name: &str| {
        let kernel = Kernel::new(Config {
            session_name: Some(name.to_owned()),
            ..Config::default()
        });
        kernel.push(ContextItem::system("be brief"));
        kernel.push(ContextItem::user(format!("a question for {name}")));
        kernel.push(note("plan", name));
        kernel
    };
    let (one, two) = (named("one"), named("two"));

    let refused = reconcile(&[read("a", &one), read("b", &two)], "merged").unwrap_err();
    assert_eq!(
        refused,
        "a and b are not forks of one session: one is session one and the other two"
    );
}

/// The model every fork was last talking to is said in the fresh log, which is where `-r` reads
/// one from, with the address where every fork's log names the same one.
#[test]
fn the_model_the_forks_talked_to_is_in_the_fresh_log() {
    let talking_to = |kernel: &Kernel, model: &str, endpoint: &str| {
        let mut info = ModelInfo::new("test", model);
        info.endpoint = Some(endpoint.to_owned());
        kernel.set_provider(Arc::new(ScriptedProvider::new([]).with_info(info)));
    };
    let said_model = |reconciled: &Reconciled| {
        reconciled
            .records
            .iter()
            .filter_map(|record| match &record.event {
                Event::ModelChanged { to, .. } => Some(to.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    let (a, b) = two_forks();
    talking_to(&a, "m1", "http://here/v1");
    talking_to(&b, "m1", "http://here/v1");
    let same = merged(&[read("a", &a), read("b", &b)]);
    let changes = said_model(&same);
    assert_eq!(changes.len(), 1, "{changes:?}");
    let to = changes[0].as_ref().expect("a model");
    assert_eq!(
        (to.model.as_str(), to.endpoint.as_deref()),
        ("m1", Some("http://here/v1"))
    );
    // and before anything that came from a fork, right after the resume
    assert!(matches!(same.records[1].event, Event::ModelChanged { .. }));

    talking_to(&b, "m1", "http://there/v1");
    let elsewhere = merged(&[read("a", &a), read("b", &b)]);
    let to = said_model(&elsewhere)[0].clone().expect("a model");
    assert_eq!((to.model.as_str(), to.endpoint), ("m1", None));

    talking_to(&b, "m2", "http://here/v1");
    let two = merged(&[read("a", &a), read("b", &b)]);
    assert!(said_model(&two).is_empty(), "{:?}", said_model(&two));
}

/// A fork that never went past the ancestor is a fork with nothing to bring, and three forks are
/// numbered in the order they were named.
#[test]
fn three_forks_and_one_that_never_went_on() {
    let ancestor = ancestor();
    let (a, b) = two_forks_of(&ancestor);
    let still = fork_of(&ancestor);
    let reconciled = merged(&[read("still", &still), read("a", &a), read("b", &b)]);

    assert_eq!(reconciled.shared_through, ContextId(3));
    let carried: Vec<(u64, &str)> = reconciled
        .snapshot
        .items
        .iter()
        .filter(|item| item.source == "agent")
        .map(|item| (item.id.0, item.meta["reconciled"]["from"].as_str().unwrap()))
        .collect();
    assert_eq!(carried, [(9, "a"), (10, "a"), (11, "b")]);
    let manifest = item(&reconciled, 8).content.to_text();
    assert!(manifest.contains("still: 0 items"), "{manifest}");
    assert!(
        manifest.contains("- still: nothing written down"),
        "{manifest}"
    );
    assert!(
        manifest.starts_with(
            "This session is 3 forks of one session put back together: still, a and b."
        ),
        "{manifest}"
    );
}

/// A note keeps the state its fork left it in: a pin is still a pin, and a note taken out of the
/// request is carried out of it - and is not counted among the labels the model reads twice.
#[test]
fn a_carried_note_keeps_its_state() {
    let (a, b) = two_forks();
    // a's `plan` is 6, b's is 5
    a.set_state(
        [ContextId(6)],
        ContextState::Excluded,
        Some("wrong after all".into()),
    );
    b.set_state([ContextId(5)], ContextState::Pinned, None);
    let reconciled = merged(&[read("a", &a), read("b", &b)]);

    assert_eq!(item(&reconciled, 9).state, ContextState::Excluded);
    assert_eq!(
        item(&reconciled, 9).note.as_deref(),
        Some("wrong after all")
    );
    assert_eq!(item(&reconciled, 11).state, ContextState::Pinned);
    assert!(
        !item(&reconciled, 8)
            .content
            .to_text()
            .contains("more than one note"),
        "{}",
        item(&reconciled, 8).content.to_text()
    );
}

/// A reconciled session is a session like any other: carried on twice, it reconciles again, and
/// what the first reconcile brought is shared by both.
#[test]
fn a_reconciled_session_reconciles_again() {
    let (a, b) = two_forks();
    let first = merged(&[read("a", &a), read("b", &b)]);
    let (c, d) = (
        Kernel::resume(Config::default(), first.snapshot.clone()),
        Kernel::resume(Config::default(), first.snapshot.clone()),
    );
    c.push(ContextItem::user("and the shed?"));
    c.push(note("shed", "beside the annex"));
    d.push(ContextItem::user("and the roof?"));
    let again = merged(&[read("c", &c), read("d", &d)]);

    assert_eq!(again.shared_through, ContextId(11));
    let ids: Vec<u64> = again.snapshot.items.iter().map(|i| i.id.0).collect();
    assert_eq!(ids, [1, 2, 3, 8, 9, 10, 11, 14, 15]);
    assert_eq!(item(&again, 15).meta["reconciled"]["from"], "c");
    assert_eq!(item(&again, 9).meta["reconciled"]["from"], "a");
}

/// What a reconcile makes is a session the runtime takes as any other, whatever the forks were.
///
/// note: forks generated from one ancestor - turns and notes, each fork's own words its own, items
/// moved to every state - and the claims are the ones that must hold of all of them: the shared
/// part is the ancestor whole, with the most included state any fork left an item in; every note a
/// fork wrote is carried and nothing else of its own is; numbers are past every fork's; and the
/// pair it writes reads back clean and resumes.
#[test]
fn whatever_the_forks_the_session_made_is_sound() {
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Added {
        Said,
        Answered,
        Noted(u8),
    }
    let added = prop_oneof![
        Just(Added::Said),
        Just(Added::Answered),
        (0u8..3).prop_map(Added::Noted)
    ];
    let state = prop_oneof![
        Just(ContextState::Active),
        Just(ContextState::Pinned),
        Just(ContextState::Elided),
        Just(ContextState::Excluded),
        Just(ContextState::Superseded),
    ];
    let fork = (
        prop::collection::vec(added.clone(), 0..6),
        prop::collection::vec((0usize..8, state.clone()), 0..4),
        prop::option::weighted(0.3, 0usize..6),
    );

    proptest!(
        ProptestConfig {
            cases: 96,
            failure_persistence: None,
            ..ProptestConfig::default()
        },
        |(shared in prop::collection::vec(added, 1..6),
          forks in prop::collection::vec(fork, 2..5))| {
            let push = |kernel: &Kernel, added: &Added, whose: &str, nth: usize| {
                kernel.push(match added {
                    Added::Said => ContextItem::user(format!("{whose} says {nth}")),
                    Added::Answered => ContextItem::assistant(format!("{whose} answers {nth}"), vec![]),
                    Added::Noted(label) => note(&format!("label{label}"), &format!("{whose} notes {nth}")),
                });
            };
            let root = Kernel::new(Config::default());
            for (nth, added) in shared.iter().enumerate() {
                push(&root, added, "the ancestor", nth);
            }
            let ancestor = root.snapshot();

            let mut read_back = Vec::new();
            let shared_len = ancestor.items.len();
            let mut revised_by: Vec<Vec<usize>> = vec![Vec::new(); shared_len];
            for (index, (tail, moves, revises)) in forks.iter().enumerate() {
                let kernel = fork_of(&ancestor);
                let whose = format!("fork {index}");
                if let Some(at) = revises {
                    let at = at % shared_len;
                    revise(&kernel, ancestor.items[at].id.0, &format!("{whose} revised this"));
                    revised_by[at].push(index);
                }
                for (nth, added) in tail.iter().enumerate() {
                    push(&kernel, added, &whose, nth);
                }
                for (at, state) in moves {
                    let items = kernel.items();
                    let id = items[at % items.len()].id;
                    kernel.set_state([id], *state, None);
                }
                read_back.push(read(&whose, &kernel));
            }

            // the first item two forks revised is refused, naming it; nothing is made
            let outcome = reconcile(&read_back, "merged");
            if let Some(at) = revised_by.iter().position(|forks| forks.len() > 1) {
                let refused = outcome.expect_err("two revisions of one item");
                let named = format!("item {} is shared by the forks", ancestor.items[at].id);
                prop_assert!(refused.starts_with(&named), "{}", refused);
                return Ok(());
            }
            let reconciled = outcome.expect("forks of one session");
            let items = &reconciled.snapshot.items;

            // the ancestor whole, each item in the most included state any fork left it in
            prop_assert_eq!(reconciled.shared_through, ancestor.items.last().unwrap().id);
            for (at, original) in ancestor.items.iter().enumerate() {
                let kept = &items[at];
                prop_assert_eq!(kept.id, original.id);
                match revised_by[at][..] {
                    [fork] => prop_assert_eq!(
                        kept.content.to_text(),
                        format!("fork {fork} revised this")
                    ),
                    _ => prop_assert_eq!(&kept.content, &original.content),
                }
                let rank = |state: ContextState| match state {
                    ContextState::Pinned => 3,
                    ContextState::Active => 2,
                    ContextState::Elided => 1,
                    _ => 0,
                };
                let best = read_back
                    .iter()
                    .map(|fork| rank(fork.snapshot.items[at].state))
                    .max()
                    .unwrap();
                prop_assert_eq!(rank(kept.state), best);
            }

            // then the manifest, and then every fork's notes and nothing else of theirs
            let start = read_back.iter().map(|f| f.snapshot.next_item).max().unwrap();
            let after = &items[ancestor.items.len()..];
            prop_assert_eq!(after[0].id, ContextId(start));
            prop_assert_eq!(after[0].state, ContextState::Pinned);
            let expected: Vec<String> = read_back
                .iter()
                .flat_map(|fork| fork.snapshot.items[ancestor.items.len()..].iter())
                .filter(|item| item.source == "agent")
                .map(|item| item.content.to_text().into_owned())
                .collect();
            let carried: Vec<String> = after[1..].iter().map(|item| item.content.to_text().into_owned()).collect();
            prop_assert_eq!(carried, expected);
            for (nth, item) in after.iter().enumerate() {
                prop_assert_eq!(item.id, ContextId(start + nth as u64));
            }

            // and the pair reads back clean and resumes
            prop_assert!(reconciled.snapshot.problems().is_empty(), "{:?}", reconciled.snapshot.problems());
            let log: String = reconciled
                .records
                .iter()
                .map(|record| serde_json::to_string(record).unwrap() + "\n")
                .collect();
            let snapshot = serde_json::to_string(&reconciled.snapshot).unwrap();
            let checked = kamchatka::check::check(Some(&log), Some(&snapshot));
            prop_assert!(checked.findings.is_empty(), "{:?}", checked.findings);
            let resumed = Kernel::resume(Config::default(), serde_json::from_str(&snapshot).unwrap());
            let next = resumed.push(ContextItem::user("carry on"));
            prop_assert_eq!(next, ContextId(start + after.len() as u64));
        }
    );
}

/// The program end to end: a session, two forks of it carried on by the program itself with the
/// model writing notes through `context`, the reconcile, and a resume of what it wrote - which
/// goes on with the forks' model without being told it.
#[tokio::test(flavor = "multi_thread")]
async fn the_program_reconciles_forks_it_made_and_carries_on_from_them() {
    use serde_json::json;

    let dir = common::scratch("reconcile-program");
    let noting = |id: &str, label: &str, content: &str| {
        format!(
            "data: {}\n\ndata: {}",
            json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
                {"index": 0, "id": id, "type": "function", "function": {"name": "context",
                 "arguments": json!({"action": "note", "label": label, "content": content,
                                     "reason": "worth keeping"}).to_string()}}]},
                "finish_reason": null}]}),
            json!({"id": "1", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})
        )
    };
    let base = common::endpoint(vec![
        common::answer("I will look at both sides"),
        noting("a1", "plan", "build on the south side"),
        common::answer("south it is"),
        noting("b1", "plan", "build on the north side"),
        common::answer("north it is"),
        common::answer("they disagree"),
    ])
    .await;
    let run = |args: &[&str]| {
        let out = common::command()
            .args(["--headless", "--deadline", "20", "--on-ask", "allow"])
            .args(args)
            .current_dir(&dir)
            .env("TMPDIR", &dir)
            .env("KAMCHATKA_BASE_URL", &base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .env_remove("KAMCHATKA_MODEL")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the binary under test is built");
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(0), "{args:?}: {said}");
        let state = said
            .lines()
            .find_map(|line| {
                line.strip_prefix("`kamchatka -r ")?
                    .strip_suffix("` carries on from it")
            })
            .map(str::to_owned);
        (
            said,
            state,
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    let (_, ancestor, _) = run(&["-m", "nothing", "where should the annex go?"]);
    let ancestor = ancestor.expect("the parting line says where the session went");
    let (_, a, _) = run(&["-r", &ancestor, "try the north side"]);
    let (_, b, _) = run(&["-r", &ancestor, "try the south side"]);
    let (a, b) = (a.expect("a went somewhere"), b.expect("b went somewhere"));
    assert_ne!(a, b, "each fork has a record of its own");

    let out = common::command()
        .current_dir(&dir)
        .args(["reconcile", &a, &b, "-o", "merged"])
        .output()
        .expect("the binary under test is built");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(said.contains("the forks agree up to item 2"), "{said}");
    assert!(
        said.contains("`kamchatka -r merged.json` carries on from it, talking to nothing"),
        "{said}"
    );

    let snapshot: Snapshot =
        serde_json::from_slice(&std::fs::read(dir.join("merged.json")).expect("written")).unwrap();
    let notes: Vec<String> = snapshot
        .items
        .iter()
        .filter(|item| item.source == "agent")
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(
        notes,
        ["build on the south side", "build on the north side"]
    );
    assert!(
        snapshot
            .items
            .iter()
            .all(|item| !item.content.to_text().contains("it is")),
        "a fork's answer came through: {:?}",
        snapshot.items
    );

    // no `-m`: the model is in the record the reconcile wrote
    let (said, _, records) = run(&["-r", "merged.json", "what did they conclude?"]);
    assert!(!said.contains("no model yet"), "{said}");
    assert!(
        said.contains("they disagree"),
        "the message was sent: {said}"
    );
    assert!(
        records.contains(
            r#""event":"model.requested","model":{"provider":"openai-compatible","model":"nothing""#
        ),
        "{records}"
    );
}

/// Without the logs, a revised item is still taken for shared where the item after it agrees in
/// every fork.
#[test]
fn a_revision_the_next_item_vouches_for_needs_no_log() {
    let (a, b) = two_forks();
    revise(&a, 2, "where should the annex and the shed go?");
    let reconciled = merged(&[
        Fork::new("a", a.snapshot(), Vec::new()),
        Fork::new("b", b.snapshot(), Vec::new()),
    ]);

    assert_eq!(reconciled.shared_through, ContextId(3));
    assert_eq!(
        item(&reconciled, 2).content.to_text(),
        "where should the annex and the shed go?"
    );
}

/// An item one fork revised and another moved keeps both: the words from the one, and the state
/// with the note that explains it from the other.
#[test]
fn a_revision_and_a_state_from_two_forks_are_both_kept() {
    let (a, b) = two_forks();
    revise(&a, 2, "where should the annex and the shed go?");
    b.set_state(
        [ContextId(2)],
        ContextState::Pinned,
        Some("the question everything answers".into()),
    );
    let reconciled = merged(&[read("a", &a), read("b", &b)]);

    let question = item(&reconciled, 2);
    assert_eq!(
        question.content.to_text(),
        "where should the annex and the shed go?"
    );
    assert_eq!(question.state, ContextState::Pinned);
    assert_eq!(
        question.note.as_deref(),
        Some("the question everything answers")
    );
    assert_eq!(question.meta["revised"]["by"], "user");
    assert_eq!(question.meta["reconciled"]["revised_in"], "a");
}
