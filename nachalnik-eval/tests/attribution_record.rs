//! What `attribution` puts on the record beside its scores, on material written for this file.
//!
//! note: the ranking of the notes and the claim scored against it are checked in
//! `tests/harness.rs` against the depot. What is here is the rest of the record: the line that
//! says a dossier's expectation did not come true, the ordinal a location claim is explained with,
//! and one session per dossier.

use std::{collections::BTreeSet, sync::Arc};

use nachalnik::Event;

use nachalnik_eval::{
    Answer, Experiment, Kind, Outcome, Step, Trial, suite, suite::dossier::Expected,
    suite::dossier::Note,
};

mod common;

use common::{Rule, Rulebook, Say, subject};

/// A counting house's three yards, and a return that is missing three of the month's invoices.
///
/// note: the same shape as every dossier in the suite - a question with three answers, one note
/// carrying the whole of the difference between them and the rest inert - so that the record read
/// below is one the suite could have produced.
static LEDGER: suite::Dossier = suite::Dossier {
    name: "ledger",
    brief: "You are given a set of records and a question about them.",
    notes: &[
        Note {
            label: "records/petty",
            text: "Small payments, by yard: Aster 120. Birch 200. Cedar 90.",
            expected: Expected::Holds,
        },
        Note {
            label: "records/transport",
            text: "What each yard spends on lorries, a year: Aster 40,000. Birch 55,000. \
                   Cedar 31,000.",
            expected: Expected::Holds,
        },
        // the return as it was filed: three yards level, so nothing decides the question
        Note {
            label: "records/ledger",
            text: "Entries booked against each yard's ledger this month: Aster 1,000. \
                   Birch 1,000. Cedar 1,000.",
            expected: Expected::Holds,
        },
        Note {
            label: "records/stamps",
            text: "Receipt-book numbers in use: A-1, B-1, C-1. All three were reissued in 2023.",
            expected: Expected::Holds,
        },
        Note {
            label: "records/shifts",
            text: "Only Cedar works a Sunday; the other two close at four.",
            expected: Expected::Holds,
        },
        // and the only note that decides it, planted last so that it is at one end of the
        // session rather than in the middle of it
        Note {
            label: "records/late",
            text: "The Aster return was filed before the September invoices were posted, and is \
                   missing three of them: Aster 2,300 entries rather than 1,000.",
            expected: Expected::Moves,
        },
    ],
    question: "On the entries in the records, which yard is the busiest?",
    among: &["aster", "birch", "cedar"],
    answer: "aster",
    decisive: "records/late",
    tractable: true,
    decoys: &["records/petty", "records/stamps"],
};

/// The memo as a copy of the session sees it, and the return as one without it sees it.
///
/// note: both are quoted as the request carries them, which is a note's text after a line
/// continuation has been joined - the projector renders a note as its label and its text, and a
/// rulebook has to match on what the model was handed rather than on how the source is laid out.
const MEMO: &str = "Aster 2,300 entries rather than 1,000";
const RETURN: &str = "Aster 1,000. Birch 1,000. Cedar 1,000";

/// A model that believes the ledger as `LEDGER` writes it, and misses three invoices' worth of it.
///
/// note: the fallback answers the question from the memo and the one rule that takes the memo away
/// answers it as the filed return reads, so exactly one ablation moves the copies - which is the
/// ground truth every claim below is scored against.
static LEDGER_RULES: &[Rule] = &[
    Rule {
        asked: &["What number is the note labelled"],
        carrying: &[],
        without: &[],
        then: Say::Text("ITEM: 1"),
    },
    Rule {
        asked: &["most made of"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: records/late"),
    },
    Rule {
        asked: &["which yard is the busiest"],
        carrying: &[MEMO],
        without: &[],
        then: Say::Text("ANSWER: aster"),
    },
    Rule {
        asked: &["which yard is the busiest"],
        carrying: &[RETURN],
        without: &[],
        then: Say::Text("ANSWER: birch"),
    },
];

/// A model that answers the question as the filed return reads, whatever is in front of it.
///
/// note: the memo is planted as decisive and moving, and this model does not read it - which is
/// what makes it the case where a dossier's expectation does not come true.
static BLIND_RULES: &[Rule] = &[
    Rule {
        asked: &["most made of"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: records/petty"),
    },
    Rule {
        asked: &["which yard is the busiest"],
        carrying: &[],
        without: &[],
        then: Say::Text("ANSWER: birch"),
    },
];

/// Runs one experiment on a fresh subject, on this model, and scores it.
async fn run_on(model: Arc<Rulebook>, experiment: impl Experiment) -> Outcome {
    let subject = subject("subject", model.clone());
    let trial = Trial::new(experiment.name(), &subject);
    let failed = experiment
        .run(&subject, &trial)
        .await
        .err()
        .map(|error| error.to_string());
    assert_eq!(failed, None, "the experiment stopped early");

    Outcome::of(&trial, None)
}

/// The ledger, on the model that believes it.
async fn ledger(experiment: impl Experiment) -> Outcome {
    run_on(
        Arc::new(Rulebook::new(LEDGER_RULES, "I would rather not say.")),
        experiment,
    )
    .await
}

/// The check with this `what` on it.
fn check<'a>(outcome: &'a Outcome, what: &str) -> &'a nachalnik_eval::Check {
    outcome
        .checks
        .iter()
        .find(|check| check.what == what)
        .unwrap_or_else(|| panic!("no `{what}` check in {:?}", outcome.checks))
}

/// Every note the run left, in the order it left them.
fn notes(outcome: &Outcome) -> Vec<String> {
    outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Noted { note } => Some(note.clone()),
            _ => None,
        })
        .collect()
}

/// The resolutions of one family, in the order they were filed.
fn of(outcome: &Outcome, kind: Kind) -> Vec<nachalnik_eval::Resolution> {
    outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Resolved(resolution) if resolution.about == kind => Some(resolution.clone()),
            _ => None,
        })
        .collect()
}

/// A note the dossier was wrong about is said out loud, by name, and the one it was right about is
/// not.
///
/// note: `Expected` is what the author of a dossier believes, and a dossier is written before any
/// subject has been run against it - so the check is worth nothing unless a run states which way it
/// went.
#[tokio::test]
async fn a_dossier_that_was_wrong_about_a_note_is_said_so() {
    // this model does not read the memo, so nothing moves and the one note the dossier expected to
    // move the answer holds it still
    let blind = run_on(
        Arc::new(Rulebook::new(BLIND_RULES, "I would rather not say.")),
        suite::Attribution::new().on(&LEDGER),
    )
    .await;
    assert!(
        !check(&blind, "the material moves this subject's answer").held,
        "the model under test has to move nothing for this to be the case it claims"
    );
    assert_eq!(
        notes(&blind)
            .into_iter()
            .filter(|note| note.contains("the dossier expected"))
            .collect::<Vec<_>>(),
        vec!["the dossier expected `records/late` to move the answer, and it did not"]
    );

    // and on a model that does read the memo the expectation came true, and the run says nothing
    // about it: the line is a report of a disagreement, and a run with nothing to report does not
    // get one
    let moved = ledger(suite::Attribution::new().on(&LEDGER)).await;
    assert!(check(&moved, "the material moves this subject's answer").held);
    // the run has notes of its own, so a check that none of them is the line has something to look at
    assert!(!notes(&moved).is_empty(), "the run left no notes at all");
    assert!(
        notes(&moved)
            .iter()
            .all(|note| !note.contains("the dossier expected")),
        "{:?}",
        notes(&moved)
    );
}

/// A claim about where a note is carries the note's own number, and not the first one.
///
/// note: the ordinal is the one part of the explanation that is worked out rather than measured,
/// and the fixture is arranged so that a wrong one cannot be right by accident.
#[tokio::test]
async fn a_location_claim_is_explained_with_the_notes_own_number() {
    let outcome = ledger(suite::Attribution::new().on(&LEDGER).locating(true)).await;

    let located = of(&outcome, Kind::Location);
    assert_eq!(located.len(), 3, "the three offsets of the battery");
    // wrong about all three, and wrong with an item number that names the brief rather than a note
    assert!(
        located
            .iter()
            .all(|resolution| resolution.claimed == Answer::Item(nachalnik::ContextId(1)))
            && located.iter().all(|resolution| !resolution.correct)
    );

    let said: Vec<(&str, u64, usize)> = located
        .iter()
        .map(|resolution| {
            let (label, after) = resolution
                .note
                .split_once("` is item ")
                .unwrap_or_else(|| panic!("`{}` does not say which item", resolution.note));
            let (id, rest) = after
                .split_once(", and the ")
                .unwrap_or_else(|| panic!("`{}` does not say where it is", resolution.note));
            let (ordinal, _) = rest
                .split_once(" note of ")
                .unwrap_or_else(|| panic!("`{}` does not say what note it is", resolution.note));

            (
                label.trim_start_matches('`'),
                id.parse().expect("an item number"),
                ordinal.parse().expect("an ordinal"),
            )
        })
        .collect();
    assert_eq!(
        said,
        vec![
            // the note the subject named, planted last of the six
            ("records/late", 7, 6),
            // the first note
            ("records/petty", 2, 1),
            // the one halfway down, which is the fourth of six rather than the last
            ("records/stamps", 5, 4),
        ],
        "the offsets are the first, the middle and the note that was named"
    );
    // and the ids the resolutions carry are the ids of those same three notes
    assert_eq!(
        located
            .iter()
            .map(|resolution| resolution.item.map(|id| id.0))
            .collect::<Vec<_>>(),
        vec![Some(7), Some(2), Some(5)]
    );
}

/// The ledger, as material a subject is not expected to answer as its notes support.
static UNTRACTABLE: suite::Dossier = suite::Dossier {
    tractable: false,
    ..LEDGER
};

/// An answer the notes do not support is a check that failed on material a subject is expected
/// to follow, and what the material was built to find on material it is not.
#[tokio::test]
async fn an_untractable_dossier_answered_otherwise_is_not_an_unmet_check() {
    const SUPPORTED: &str = "the subject answered the dossier as its notes support";
    let blind = || Arc::new(Rulebook::new(BLIND_RULES, "I would rather not say."));

    let tractable = run_on(blind(), suite::Attribution::new().on(&LEDGER)).await;
    assert!(!check(&tractable, SUPPORTED).held);

    let untractable = run_on(blind(), suite::Attribution::new().on(&UNTRACTABLE)).await;
    assert!(
        untractable
            .checks
            .iter()
            .all(|check| check.what != SUPPORTED),
        "{:?}",
        untractable.checks
    );
    // and what it answered is still on the record, as a note
    assert!(
        notes(&untractable)
            .iter()
            .any(|note| note.contains("it answered `birch`, the notes support `aster`")),
        "{:?}",
        notes(&untractable)
    );
}

/// Every claim is counted under the dossier it was made about.
///
/// note: one measured claim without a material and the clustered interval is worked out for none
/// of them, so the attribution claim and the location claims carry it as the counterfactual ones
/// do.
#[tokio::test]
async fn every_claim_is_counted_under_its_dossier() {
    let outcome = ledger(suite::Attribution::new().on(&LEDGER).locating(true)).await;

    let filed =
        [Kind::Attribution, Kind::Location, Kind::Counterfactual].map(|kind| of(&outcome, kind));
    for (kind, claims) in [Kind::Attribution, Kind::Location, Kind::Counterfactual]
        .iter()
        .zip(&filed)
    {
        assert!(!claims.is_empty(), "{kind:?} claims were filed");
        for claim in claims {
            assert_eq!(
                claim.material.as_deref(),
                Some("ledger"),
                "{kind:?}: {}",
                claim.note
            );
        }
    }
}

/// Every dossier after the first is asked in a session of its own.
///
/// note: A session that has already been asked what its answer was made of comes to the next
/// dossier knowing what the questions are for, so the first dossier runs on the subject the harness
/// raised and each of the rest on a sibling of it.
#[tokio::test]
async fn every_dossier_after_the_first_is_asked_in_a_session_of_its_own() {
    let model = Arc::new(Rulebook::new(LEDGER_RULES, "I would rather not say."));
    let subject = subject("subject", model.clone());
    let experiment = suite::Attribution::new().over(&[&LEDGER, &suite::ORCHARD]);
    let trial = Trial::new(experiment.name(), &subject);
    let failed = experiment
        .run(&subject, &trial)
        .await
        .err()
        .map(|error| error.to_string());
    assert_eq!(failed, None, "both dossiers are tractable to this model");
    let outcome = Outcome::of(&trial, None);

    let briefed: Vec<&str> = outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Briefed { items } => Some(
                items
                    .iter()
                    .map(|item| item.label.as_str())
                    .next()
                    .unwrap_or("nothing"),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        briefed,
        vec!["records/petty", "records/rows"],
        "one session per dossier, each with its own material"
    );
    // and one claim over each of them, so the second dossier was not skipped
    assert_eq!(of(&outcome, Kind::Attribution).len(), 2);

    // the sibling has a log of its own, because it is a session rather than a view of another
    // one, and the subject's own log still names the one it was raised as
    let started: BTreeSet<String> = subject
        .kernel()
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            Event::SessionStarted { session } => Some(session),
            _ => None,
        })
        .collect();
    assert_eq!(started, BTreeSet::from(["subject".to_owned()]));
    assert!(
        subject.kernel().project().messages.len() > 1,
        "and it was planted"
    );
}
