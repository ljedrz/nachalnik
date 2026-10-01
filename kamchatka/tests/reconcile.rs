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
    let ancestor = ancestor();
    let (a, b) = (fork_of(&ancestor), fork_of(&ancestor));
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
            == "the token counter's correction is dropped: a was talking to m1, b was talking to m2"),
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
