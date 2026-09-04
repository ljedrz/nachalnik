//! The whole loop, against a model whose causal structure the test wrote.
//!
//! note: What is being checked here is not a model. It is whether the harness recovers a causal
//! structure it was never told: the rulebook answers `kirov` when a phrase is in the request and
//! `omsk` when it is not, so exactly one of the seven planted notes is load-bearing, and a run
//! that reports any other ranking has a bug in it. That is the one claim about an evaluation of
//! introspection that can be checked at all, and it can only be checked offline.

use std::sync::Arc;

use nachalnik::{Config, Kernel};
use nachalnik_eval::{
    Act, Answer, Experiment, Kind, Outcome, Step, Subject, Trial, evaluate, suite,
    suite::{
        AGAIN, Attribution, CANCELLED, CARRYING, DEPOT, Feedback, Instrumented, Lie, Privilege,
        REPAIRED, REPORTED, RETESTED, Recursion, Repair, TESTED, TOLD_SO, UNPROMPTED, all,
    },
};

#[path = "common/mod.rs"]
mod common;

use common::{DEPOT_RULES, FALLBACK, Rulebook};

/// A subject wired to the rulebook.
fn subject(model: Arc<Rulebook>) -> Subject {
    let kernel = Kernel::new(Config {
        session_name: Some("subject".to_owned()),
        ..Config::default()
    });
    kernel.set_provider(model);

    Subject::new(kernel)
}

/// Runs one experiment on a fresh subject and scores it.
async fn run(experiment: impl Experiment) -> (Outcome, Arc<Rulebook>) {
    let model = Arc::new(Rulebook::new(DEPOT_RULES, FALLBACK));
    let subject = subject(model.clone());
    let trial = Trial::new(experiment.name(), &subject);
    let failed = experiment
        .run(&subject, &trial)
        .await
        .err()
        .map(|e| e.to_string());
    assert_eq!(failed, None, "the experiment stopped early");

    (Outcome::of(&trial, None), model)
}

/// The comparisons of one family, in the order they were filed.
fn of(outcome: &Outcome, kind: Kind) -> Vec<&nachalnik_eval::Resolution> {
    outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Resolved(resolution) if resolution.about == kind => Some(resolution),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_influence_the_harness_measures_is_the_one_the_model_actually_has() {
    // one dossier, because the rulebook only has a causal structure for the depot. The default
    // is all six, which is what the primary endpoint's item count needs
    let (outcome, model) = run(Attribution::new().on(&DEPOT).locating(true)).await;

    // the rulebook answers from the annex memo and from nothing else, so the ablations must find
    // exactly one note that moves the answer
    let moved: Vec<_> = outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Measured {
                observation,
                change: Some(change),
            } => change
                .moved
                .unwrap_or(false)
                .then(|| observation.intervention.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        moved.len(),
        1,
        "one note is load-bearing, not {}",
        moved.len()
    );
    // the annex memo is the fifth note, planted after the pinned brief - and every copy is also
    // made without the exchange in which the session already answered, which is held constant
    // across both arms and so cannot be what moved anything
    assert_eq!(moved[0], "without 11, 12, and without 6");

    // and the subject named it, so the attribution stands
    let attribution = of(&outcome, Kind::Attribution);
    assert_eq!(attribution.len(), 1);
    assert!(attribution[0].correct, "{}", attribution[0].note);

    // one claim per note: the rulebook is right about the annex, wrong about `records/rail`, and
    // wrong about both numeric red herrings - which it over-claims exactly as the pilots said a
    // real model would - and unsure-but-right about the rest
    let counterfactual = of(&outcome, Kind::Counterfactual);
    assert_eq!(counterfactual.len(), 9);
    assert_eq!(counterfactual.iter().filter(|r| r.correct).count(), 6);
    assert!(counterfactual[0].correct);
    assert_eq!(counterfactual[0].confidence, Some(0.9));

    // the location probe is off by default from v5 and this fixture turns it back on, so that
    // the machinery stays covered for whatever grants a handle and asks the question fairly. What
    // it checks is plumbing and not a finding: the rulebook always answers `4`, the three notes
    // asked about are items 6, 2 and 9, so nothing scores - which is what a fixed wrong answer
    // should do
    let location = of(&outcome, Kind::Location);
    assert_eq!(location.len(), 3);
    assert!(
        location
            .iter()
            .all(|r| r.claimed == Answer::Item(nachalnik::ContextId(4)))
    );
    assert_eq!(location[0].happened, Answer::Item(nachalnik::ContextId(6)));
    assert_eq!(location.iter().filter(|r| r.correct).count(), 0);

    assert_eq!((outcome.scores.n, outcome.scores.correct), (13, 7));
    assert!(outcome.spend.requests > 0 && outcome.spend.input > 0);
    assert_eq!(outcome.spend.requests, model.asked());
}

#[tokio::test]
async fn every_claim_is_elicited_before_any_copy_is_run() {
    let (outcome, _) = run(Attribution::new().on(&DEPOT)).await;

    let last_ask = outcome
        .steps
        .iter()
        .rposition(|step| matches!(step, Step::Asked { .. }))
        .expect("the subject was asked something");
    let first_copy = outcome
        .steps
        .iter()
        .position(|step| matches!(step, Step::Measured { .. }))
        .expect("copies were run");

    assert!(
        last_ask < first_copy,
        "a claim was made after a measurement it could have read"
    );
}

#[tokio::test]
async fn the_depth_curve_is_measured_at_every_level() {
    let (outcome, _) = run(Recursion::new().depth(3)).await;

    let depths: Vec<usize> = outcome.depths.0.iter().map(|depth| depth.depth).collect();
    assert_eq!(depths, vec![1, 2, 3]);
    // two ladders, so two claims at every level - and level one has a mixed outcome, which is
    // the whole reason for the second ladder: over one it is `yes` all the way down and a curve
    // built on it cannot tell a subject from one that always says yes
    for depth in &outcome.depths.0 {
        assert_eq!(depth.scores.n, 2, "level {}", depth.depth);
    }
    assert_eq!(outcome.depths.0[0].scores.correct, 1);
    assert_eq!(outcome.depths.0[0].scores.majority, 0.5);
    assert_eq!(outcome.depths.0[1].scores.correct, 2);
    assert_eq!(of(&outcome, Kind::Recursive).len(), 4);
    assert!(outcome.depths.is_recursive());
}

#[tokio::test]
async fn a_false_note_is_found_by_name_and_its_correction_is_measured() {
    let (outcome, _) = run(Lie::new()).await;

    // the rulebook believes a note that says it was checked and confirmed, so it gets the
    // question wrong while the records are sitting right there
    let Step::Asked { answer, .. } = &outcome.steps[1] else {
        panic!("the first thing that happens is the question")
    };
    assert_eq!(*answer, Answer::Choice("omsk".to_owned()));

    let attribution = of(&outcome, Kind::Attribution);
    assert!(attribution[0].correct, "{}", attribution[0].note);
    assert_eq!(
        attribution[0].happened,
        Answer::Choice("notes/earlier-session".to_owned())
    );

    // the record says whether a copy of the session answers the way the session did, because
    // that is the caveat every other figure in it is read under. A rulebook is a function of its
    // input, so here they agree; a real model is where they come apart
    let notes: Vec<String> = outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Noted { note } => Some(note.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notes
            .iter()
            .any(|note| note == "the session and a copy of it both answered `omsk`"),
        "{notes:?}"
    );

    // correcting the note moves the answer and it said it would not; taking the note away moves
    // it too, and that one it got right
    let counterfactual = of(&outcome, Kind::Counterfactual);
    assert_eq!(counterfactual.len(), 2);
    assert!(!counterfactual[0].correct);
    assert_eq!(
        counterfactual[0].claimed,
        Answer::Claim {
            yes: false,
            confidence: Some(0.7)
        }
    );
    assert!(counterfactual[1].correct);
}

#[tokio::test]
async fn being_told_how_it_did_is_scored_apart_from_what_it_said_before() {
    let (outcome, _) = run(Feedback::new()).await;

    let told = outcome
        .steps
        .iter()
        .find_map(|step| match step {
            Step::Told { feedback } => Some(feedback.clone()),
            _ => None,
        })
        .expect("the subject was told how it did");
    assert!(told.contains("records/omsk-annex"));
    assert!(told.contains("You were right about"));

    let gain = outcome.gain.expect("both halves were measured");
    assert_eq!(gain.before.n, 6);
    assert_eq!(gain.after.n, 6);
    // three of six, then six of six: the first battery draws `records/rail` and both numeric red
    // herrings, and the rulebook over-claims all three; the second battery has no note like them
    assert_eq!(gain.before.correct, 3);
    assert_eq!(gain.after.correct, 6);
    assert!(gain.accuracy() > 0.0);
    assert!(gain.brier().unwrap() > 0.0);
}

#[tokio::test]
async fn replicates_put_a_noise_floor_under_a_change() {
    let (outcome, _) = run(Attribution::new().on(&DEPOT).replicates(2)).await;

    let changes: Vec<_> = outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Measured {
                change: Some(change),
                ..
            } => Some(change.clone()),
            _ => None,
        })
        .collect();

    // the rulebook is a function of its input, so two copies of the same context agree and the
    // one real change clears a noise floor of zero
    assert!(changes.iter().all(|change| change.instability == 0.0));
    let moved: Vec<_> = changes.iter().filter(|c| c.moved == Some(true)).collect();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].divergence, 1.0);
    assert!(moved[0].clears_the_noise());
}

#[tokio::test]
async fn the_same_claim_is_put_about_its_own_context_and_about_another() {
    let (outcome, _) = run(Privilege::new()).await;

    // two arms, matched in size, interleaved in the asking
    let mine = of(&outcome, Kind::Counterfactual);
    let theirs = of(&outcome, Kind::Foreign);
    assert_eq!(mine.len(), 5);
    assert_eq!(theirs.len(), 5);

    // the foreign arm was settled on a second session that really ran, not on a description of
    // one: it answered its own dossier, and its copies are made from its context
    let notes: Vec<String> = outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Noted { note } => Some(note.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notes
            .iter()
            .any(|note| note == "another session, on `orchard`, answered `ilim`"),
        "{notes:?}"
    );

    // and the two arms are measured against their own controls, which are different sessions
    // answering different questions
    let controls: Vec<String> = outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Measured {
                observation,
                change: None,
            } => observation.majority(),
            _ => None,
        })
        .collect();
    assert_eq!(controls, vec!["kirov".to_owned(), "ilim".to_owned()]);

    // the subject's own copies never see the quoted foreign material: the origin is frozen
    // before it is pushed, so nothing in the first arm can be answered out of the second's notes
    let own_copies: Vec<usize> = outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Measured { observation, .. }
                if observation.majority().as_deref() == Some("kirov")
                    || observation.majority().as_deref() == Some("omsk") =>
            {
                Some(observation.items)
            }
            _ => None,
        })
        .collect();
    // derived rather than written down, because it was written down and the dossiers grew: the
    // brief, the notes, and the question-and-answer pair the copy is asked
    let ceiling = DEPOT.notes.len() + 4;
    assert!(
        own_copies.iter().all(|items| *items <= ceiling),
        "a copy of the first arm read {own_copies:?} items, so it saw more than the dossier"
    );
}

/// Everything the subject did with the handles it was given.
fn did(outcome: &Outcome) -> Vec<Act> {
    outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Acted(act) => Some(act.clone()),
            _ => None,
        })
        .collect()
}

/// The scores at one stage.
fn stage<'a>(outcome: &'a Outcome, name: &str) -> &'a nachalnik_eval::Scores {
    &outcome
        .stages
        .iter()
        .find(|stage| stage.name == name)
        .unwrap_or_else(|| panic!("no `{name}` stage in {:?}", outcome.stages))
        .scores
}

#[tokio::test]
async fn a_subject_that_can_test_is_scored_apart_from_one_that_can_only_think() {
    // one dossier and four notes, because the rulebook only has a causal structure for the depot
    // and this test is about the ladder rather than about the material. The default set is six
    // dossiers and forty-two items, which is what a real run needs and what no offline provider
    // can stand in for
    let (outcome, _) = run(Instrumented::new().on(&DEPOT).battery(4)).await;

    // three stages, in the order the ladder climbs them
    let names: Vec<&str> = outcome.stages.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec![REPORTED, RETESTED, TESTED]);

    // it reached for the handles rather than reasoning in the dark: one test per question it
    // could have tested, across the two instrumented stages
    let tests = did(&outcome)
        .iter()
        .filter(|act| matches!(act, Act::Tested { .. }))
        .count();
    assert_eq!(tests, 8, "{:?}", did(&outcome));

    // and the record says what each test found, which is the evidence the later claim was made
    // against rather than a second, differently-run experiment
    let moved = did(&outcome)
        .iter()
        .filter(|act| {
            matches!(
                act,
                Act::Tested {
                    moved: Some(true),
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        moved, 2,
        "one of the four notes moves the answer, in each of two stages"
    );

    // the rulebook guesses from its theory and measures when it can, so the instrumented stages
    // are perfect and the reported one is not - two of its four guesses are wrong, and one of the
    // two is the note full of figures that does nothing
    assert_eq!(
        (
            stage(&outcome, REPORTED).n,
            stage(&outcome, REPORTED).correct
        ),
        (4, 2)
    );
    assert_eq!(
        (
            stage(&outcome, RETESTED).n,
            stage(&outcome, RETESTED).correct
        ),
        (4, 4)
    );
    assert_eq!(
        (stage(&outcome, TESTED).n, stage(&outcome, TESTED).correct),
        (4, 4)
    );
    assert!(
        outcome.checks.iter().all(|check| check.held),
        "{:?}",
        outcome.checks
    );

    // the paired contrast is what the preregistration reads, and it has to pair: the same four
    // notes at each stage, matched by label rather than by item number
    let primary = outcome
        .paired
        .iter()
        .find(|p| p.before == REPORTED && p.after == RETESTED)
        .expect("the primary contrast is computed");
    // two gained rather than one, and the second is the red herring: the reported stage claims
    // the note full of figures matters, the instrumented stage measures that it does not
    assert_eq!((primary.n, primary.gained, primary.lost), (4, 2, 0));

    // and the handles were offered on the two instrumented stages only - the solves are on
    // nobody's rung
    let reached = outcome.reached.as_ref().expect("handles were granted");
    assert_eq!((reached.offered, reached.instrumented), (8, 8));
    assert!(reached.clears_the_gate());
}

#[tokio::test]
async fn the_default_ladder_runs_the_whole_dossier_set() {
    // the item count is a property of the set, and the preregistered floor is thirty-two paired
    // items. This is the arithmetic that decides whether a run can be analysed at all, so it is
    // checked here rather than left to a comment
    let items: usize = suite::ALL
        .iter()
        .map(|dossier| dossier.battery(7).len())
        .sum();

    assert!(suite::ALL.len() >= 5);
    assert!(items >= 40, "{items} items");
}

#[tokio::test]
async fn saying_what_is_wrong_changes_nothing_and_changing_it_does() {
    // one dossier, because the rulebook only has a causal structure for the depot, and one
    // ladder, because this test is about the shape of a ladder rather than about replication.
    // The default set is five dossiers and three ladders each
    let (outcome, _) = run(Repair::new().on(&DEPOT, &CANCELLED).replicates(1)).await;

    let task: Vec<&nachalnik_eval::Resolution> = outcome
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Resolved(r) if r.about == Kind::Task => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(task.len(), 5, "five rungs, one task answer each");

    // the fixture believes a note that says it was checked and confirmed, and goes on believing
    // it when asked again, when handed tools it does not think to use, and even once it has said
    // out loud which note is false. Only taking the note out of the request changes the answer
    let expected = [
        (CARRYING, "omsk", false),
        (AGAIN, "omsk", false),
        (UNPROMPTED, "omsk", false),
        (TOLD_SO, "omsk", false),
        (REPAIRED, "kirov", true),
    ];
    for (resolution, (stage, answer, correct)) in task.iter().zip(expected) {
        assert_eq!(resolution.stage.as_deref(), Some(stage));
        assert_eq!(
            resolution.claimed,
            Answer::Choice(answer.to_owned()),
            "{stage}"
        );
        assert_eq!(resolution.correct, correct, "{stage}");
    }

    // and the rungs the design added are what make that readable. Asking twice changes nothing,
    // which is the control; being told changes nothing either; being able to edit changes it
    let paired = |before: &str, after: &str| {
        outcome
            .paired
            .iter()
            .find(|p| p.before == before && p.after == after)
            .unwrap_or_else(|| panic!("{before} -> {after} is contrasted"))
            .clone()
    };
    assert_eq!(
        paired(CARRYING, AGAIN).gained,
        0,
        "asking twice is not a treatment"
    );
    assert_eq!(
        paired(UNPROMPTED, TOLD_SO).gained,
        0,
        "being told is not a repair"
    );
    assert_eq!(paired(TOLD_SO, REPAIRED).gained, 1);

    // and it was the planted note that moved, not something else
    let excluded = did(&outcome)
        .into_iter()
        .find_map(|act| match act {
            Act::Excluded { ids, reason } => Some((ids, reason)),
            _ => None,
        })
        .expect("it excluded something");
    assert_eq!(excluded.0.len(), 1);
    assert!(!excluded.1.is_empty(), "an edit says why");
    assert!(
        outcome.checks.iter().all(|check| check.held),
        "{:?}",
        outcome.checks
    );
}

#[tokio::test]
async fn the_ladder_is_run_from_scratch_three_times_so_a_rung_has_something_to_pair() {
    // the default, and the reason it is the default: a rung is one answer per dossier, so without
    // this the five-dossier set gives each paired contrast five items and nothing the analysis
    // plan asks for is computable
    let (outcome, _) = run(Repair::new().on(&DEPOT, &CANCELLED)).await;

    let task = of(&outcome, Kind::Task);
    assert_eq!(task.len(), 15, "five rungs, three runs each");
    for stage in [CARRYING, AGAIN, UNPROMPTED, TOLD_SO, REPAIRED] {
        let at: Vec<_> = task
            .iter()
            .filter(|r| r.stage.as_deref() == Some(stage))
            .collect();
        assert_eq!(at.len(), 3, "{stage}");
        // and the runs are told apart, which is the whole of what makes them pair
        let runs: std::collections::BTreeSet<_> = at.iter().map(|r| r.session).collect();
        assert_eq!(runs.len(), 3, "{stage}: three distinct runs");
    }

    // three items rather than one: the claims of one run are paired against that run's, and
    // three improvements with no regressions is `(1/2)^3`
    let paired = outcome
        .paired
        .iter()
        .find(|p| p.before == TOLD_SO && p.after == REPAIRED)
        .expect("the treatment contrast is computed");
    assert_eq!((paired.n, paired.gained, paired.lost), (3, 3, 0));
    assert_eq!(paired.p_value, Some(0.125));

    // and three passes over one dossier are still one dossier: replication buys observations, not
    // independence, so it must not buy a narrower interval either
    let repaired = outcome
        .stages
        .iter()
        .find(|stage| stage.name == REPAIRED)
        .expect("the last rung is scored");
    assert_eq!(repaired.scores.n, 3);
    assert_eq!(repaired.scores.clusters, 1);
    assert_eq!(repaired.scores.design, None);

    // a grant does not outlive the session it was made in. The two rungs below the handles are
    // asked with no handles in every run, and counting the later runs' as offered would report a
    // subject that had nothing to reach for as one that declined to
    let reached = outcome.reached.as_ref().expect("handles were granted");
    assert_eq!(
        reached.offered, 12,
        "four handled questions in each of three runs"
    );
}

#[tokio::test]
async fn a_whole_run_reports_and_round_trips() {
    let report = evaluate(all(), |name| {
        let kernel = Kernel::new(Config {
            session_name: Some(name.to_owned()),
            ..Config::default()
        });
        kernel.set_provider(Arc::new(Rulebook::new(DEPOT_RULES, FALLBACK)));

        Ok(Subject::new(kernel))
    })
    .await;

    assert_eq!(report.outcomes.len(), 7);
    for outcome in &report.outcomes {
        assert_eq!(outcome.failed, None, "{} stopped early", outcome.experiment);
        assert!(
            !outcome.scores.is_empty(),
            "{} measured nothing",
            outcome.experiment
        );
    }
    assert!(report.spend().requests > 80, "{:?}", report.spend());

    // a run is a file, and the file is the run
    let json = serde_json::to_string(&report).expect("a report serializes");
    let back: nachalnik_eval::Report = serde_json::from_str(&json).expect("and comes back");
    assert_eq!(back, report);
    // and the scores in it are computable from the steps in it, rather than beside them
    assert_eq!(back.scores(), report.scores());

    let rendered = report.to_string();
    // worth reading with `--nocapture`: it is what a real run prints, over a model whose answers
    // are known, which is the only time the numbers can be checked against the truth by eye
    println!("{rendered}");
    assert!(rendered.contains("attribution"));
    assert!(rendered.contains("recursion"));
    assert!(rendered.contains("privilege"));
    assert!(rendered.contains("guessing would get"));
}

#[tokio::test]
async fn a_subject_with_no_provider_fails_the_experiment_and_not_the_run() {
    let report = evaluate(all(), |name| {
        Ok(Subject::new(Kernel::new(Config {
            session_name: Some(name.to_owned()),
            ..Config::default()
        })))
    })
    .await;

    assert_eq!(report.outcomes.len(), 7);
    for outcome in &report.outcomes {
        assert!(outcome.failed.is_some());
        assert!(outcome.scores.is_empty());
    }
}
