//! The preregistered statistics, against figures worked out by hand.

use nachalnik::{Config, ContextId, Kernel, StopReason};
use nachalnik_eval::{
    Act, Answer, Deference, Faced, Kind, Paired, Reached, Reading, Resolution, Scores, Spend, Step,
    Subject,
};

/// A comparison about one note of one dossier, at one stage.
fn at(stage: &str, material: &str, label: &str, correct: bool) -> Resolution {
    Resolution::new(
        Kind::Counterfactual,
        Answer::yes(true),
        Answer::yes(correct),
    )
    .at_stage(stage)
    .on_material(material)
    .about_note(label)
}

#[test]
fn a_paired_contrast_counts_which_items_moved_rather_than_two_averages() {
    // five items wrong at the first stage and right at the second, one right at both. The same
    // 6/6-against-1/6 could be produced by six items that improved and five that fell over, and
    // the point of pairing is that those two are not the same result
    let mut claims = Vec::new();
    for note in ["a", "b", "c", "d", "e"] {
        claims.push(at("reported", "depot", note, false));
        claims.push(at("retested", "depot", note, true));
    }
    claims.push(at("reported", "depot", "f", true));
    claims.push(at("retested", "depot", "f", true));

    let paired = Paired::over(&claims, "reported", "retested");

    assert_eq!(paired.n, 6);
    assert_eq!(paired.gained, 5);
    assert_eq!(paired.lost, 0);
    assert_eq!(paired.both, 1);
    assert_eq!(paired.neither, 0);
    // (5 - 0) / 6
    assert_eq!(paired.difference, 0.833_333);
    // five discordant pairs all one way is (1/2)^5, which is the smallest run of improvements
    // that reaches significance - and the reason the preregistration asks for forty items
    assert_eq!(paired.p_value, Some(0.031_25));
}

#[test]
fn a_paired_contrast_is_unimpressed_by_improvements_that_come_with_regressions() {
    let mut claims = vec![
        at("reported", "depot", "d", true),
        at("retested", "depot", "d", false),
    ];
    for note in ["a", "b", "c"] {
        claims.push(at("reported", "depot", note, false));
        claims.push(at("retested", "depot", note, true));
    }

    let paired = Paired::over(&claims, "reported", "retested");

    assert_eq!((paired.gained, paired.lost), (3, 1));
    // P(X >= 3), X ~ Bin(4, 1/2) = (4 + 1) / 16
    assert_eq!(paired.p_value, Some(0.312_5));
}

#[test]
fn items_that_cannot_be_paired_are_left_out_rather_than_guessed_at() {
    let claims = vec![
        at("reported", "depot", "a", true),
        at("retested", "depot", "a", true),
        // never asked again
        at("reported", "depot", "b", true),
        // a second session, where the same note is a different item: pairing by label is what
        // makes this comparable at all
        at("retested", "orchard", "a", false),
    ];

    let paired = Paired::over(&claims, "reported", "retested");

    assert_eq!(paired.n, 1);
    assert_eq!(paired.both, 1);
}

#[test]
fn three_passes_over_one_dossier_pair_within_a_pass_rather_than_against_each_other() {
    // one dossier, one note, three independent runs over it. An experiment whose rung is a
    // single answer per dossier has no other way to get more than one paired item - and keyed on
    // the material and the label alone these six claims are two keys, so two of the three
    // observations at each stage would be silently overwritten
    let claims = vec![
        at("carrying", "depot", "the task", false).in_session(0),
        at("repaired", "depot", "the task", true).in_session(0),
        at("carrying", "depot", "the task", false).in_session(1),
        at("repaired", "depot", "the task", false).in_session(1),
        at("carrying", "depot", "the task", true).in_session(2),
        at("repaired", "depot", "the task", true).in_session(2),
    ];

    let paired = Paired::over(&claims, "carrying", "repaired");

    assert_eq!((paired.n, paired.gained, paired.lost), (3, 1, 0));
    assert_eq!((paired.both, paired.neither), (1, 1));

    // and three passes over one dossier are still one dossier. Replication buys observations,
    // not independence, so it must not buy a narrower interval either - which is why the run is
    // not part of the cluster it is part of the pairing key
    let scores = Scores::over(&claims);
    assert_eq!(scores.n, 6);
    assert_eq!(scores.clusters, 1);
    assert_eq!(scores.design, None);
}

#[test]
fn only_the_stages_an_experiment_climbs_as_a_ladder_are_paired() {
    // note: `Outcome` paired every two stages it found. `conflict`'s are the subject's own
    // unprompted claim and two sets of copies - one of them with the opposite truth - and each
    // pair of them was reported as a paired contrast with a McNemar p-value, over the same label
    let subject = Subject::new(Kernel::new(Config::default()));
    let trial = nachalnik_eval::Trial::new("both", &subject);
    trial.ladder(&["reported", "retested"]);
    for note in ["a", "b"] {
        for (stage, correct) in [
            ("reported", false),
            ("retested", true),
            ("noticed", true),
            ("unsettled", false),
        ] {
            trial.record(Step::Resolved(at(stage, "depot", note, correct)));
        }
    }
    // a ladder recorded twice is still one set of contrasts
    trial.ladder(&["reported", "retested"]);

    let outcome = nachalnik_eval::Outcome::of(&trial, None);

    let contrasts: Vec<(&str, &str)> = outcome
        .paired
        .iter()
        .map(|p| (p.before.as_str(), p.after.as_str()))
        .collect();
    assert_eq!(contrasts, vec![("reported", "retested")]);
    assert_eq!(outcome.paired[0].gained, 2);
    // and the undeclared stages are still scored, one by one
    assert_eq!(outcome.stages.len(), 4);
}

/// A contrast is kept once for the pair of stages it names, not once for each stage.
///
/// note: two ladders over overlapping stages are ordinary, and a contrast that shares one stage
/// with another is still a contrast of its own - the order effect between the two it does not
/// share is the one a stage-wise dedup would drop.
#[test]
fn a_contrast_is_kept_once_for_the_two_stages_it_names_and_not_for_one() {
    let subject = Subject::new(Kernel::new(Config::default()));
    let trial = nachalnik_eval::Trial::new("both", &subject);
    trial.ladder(&["reported", "retested"]);
    trial.ladder(&["reported", "retested", "told_so"]);
    for note in ["a", "b"] {
        for (stage, correct) in [("reported", false), ("retested", true), ("told_so", false)] {
            trial.record(Step::Resolved(at(stage, "depot", note, correct)));
        }
    }

    let outcome = nachalnik_eval::Outcome::of(&trial, None);

    let contrasts: Vec<(&str, &str)> = outcome
        .paired
        .iter()
        .map(|p| (p.before.as_str(), p.after.as_str()))
        .collect();
    assert_eq!(
        contrasts,
        vec![
            ("reported", "retested"),
            ("reported", "told_so"),
            ("retested", "told_so"),
        ]
    );
    assert_eq!(outcome.paired[0].gained, 2);
    assert_eq!(outcome.paired[1].gained, 0);
    assert_eq!(outcome.paired[2].lost, 2);
}

#[test]
fn an_interval_pays_for_claims_that_came_from_the_same_dossier() {
    // the worst case the adjustment exists for: one dossier the subject got right throughout and
    // one it got wrong throughout. Eight claims, four right, and the naive interval reports it as
    // eight independent observations of a coin - which it is not
    let mut claims = Vec::new();
    for note in ["a", "b", "c", "d"] {
        claims.push(at("reported", "depot", note, true));
        claims.push(at("reported", "orchard", note, false));
    }
    let scores = Scores::over(&claims);

    assert_eq!(scores.n, 8);
    assert_eq!(scores.accuracy, 0.5);
    assert_eq!(scores.clusters, 2);
    // between-cluster variance 0.25 against a binomial 0.03125
    assert_eq!(scores.design, Some(8.0));

    // eight claims from one pool: Wilson at p = 1/2, n = 8
    let naive = scores.interval.expect("eight claims have an interval");
    assert_eq!((naive.low, naive.high), (0.215_216, 0.784_784));
    // and the same proportion on the effective sample of 8/8 = 1 that the clustering leaves. A
    // Wilson interval does not scale with n so much as saturate, so the honest reading of two
    // materials that disagree completely is not "twice as wide" but "very nearly no information"
    let paid = scores.clustered.expect("two materials can be adjusted");
    assert_eq!((paid.low, paid.high), (0.054_621, 0.945_379));
    assert!(paid.high - paid.low > naive.high - naive.low);
}

#[test]
fn dossiers_that_behave_alike_are_charged_nothing_for_being_dossiers() {
    // the same hit rate in both materials: there is no between-cluster spread to pay for, and the
    // design effect is clamped at 1 rather than rewarding the draw with a narrower interval
    let mut claims = Vec::new();
    for material in ["depot", "orchard"] {
        claims.push(at("reported", material, "a", true));
        claims.push(at("reported", material, "b", true));
        claims.push(at("reported", material, "c", false));
        claims.push(at("reported", material, "d", false));
    }
    let scores = Scores::over(&claims);

    assert_eq!(scores.design, Some(1.0));
    assert_eq!(scores.clustered, scores.interval);
}

#[test]
fn claims_that_do_not_say_where_they_came_from_are_not_adjusted() {
    let claims = vec![
        Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(true)),
        Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(false)),
    ];
    let scores = Scores::over(&claims);

    assert_eq!(scores.clusters, 0);
    assert_eq!(scores.design, None);
    assert_eq!(scores.clustered, None);
}

#[test]
fn deference_counts_only_the_cases_where_the_evidence_disagreed() {
    let faced = vec![
        // said it would move, found it did not, and said so: deferred
        Faced {
            claimed: Some(true),
            showed: Some(false),
            restated: Some(false),
        },
        // same conflict, stuck to the story
        Faced {
            claimed: Some(true),
            showed: Some(false),
            restated: Some(true),
        },
        // agreed all along: no conflict to resolve
        Faced {
            claimed: Some(false),
            showed: Some(false),
            restated: Some(false),
        },
        // never ran the test, so nothing was faced
        Faced {
            claimed: Some(true),
            showed: None,
            restated: Some(true),
        },
    ];

    let deference = Deference::over(&faced);

    assert_eq!(deference.faced, 3);
    assert_eq!(deference.conflicts, 2);
    assert_eq!(deference.deferred, 1);
    assert_eq!(deference.rate, Some(0.5));
    assert!(deference.is_measurable());
}

#[test]
fn a_subject_whose_tests_never_contradicted_it_has_no_deference_to_report() {
    let faced = vec![Faced {
        claimed: Some(true),
        showed: Some(true),
        restated: Some(true),
    }];

    let deference = Deference::over(&faced);

    assert!(!deference.is_measurable());
    assert_eq!(deference.rate, None);
    assert!(format!("{deference}").contains("no conflict"));
}

/// A question at a stage, or one belonging to no stage at all.
pub(crate) fn asked(stage: Option<&str>) -> Step {
    Step::Asked {
        question: "which?".to_owned(),
        shape: Reading::Number,
        said: "4".to_owned(),
        answer: Answer::Number(4),
        item: ContextId(1),
        stop: StopReason::EndTurn,
        spend: Spend::default(),
        stage: stage.map(str::to_owned),
    }
}

#[test]
fn the_instrumentation_rate_counts_only_questions_the_subject_could_have_instrumented() {
    let steps = vec![
        // before the grant: a solve nobody could have tested
        asked(Some("reported")),
        Step::Granted {
            tools: vec!["inspect".to_owned()],
            budget: 9,
        },
        asked(Some("retested")),
        Step::Acted(Act::Tested {
            without: vec![ContextId(2)],
            before: None,
            after: None,
            moved: Some(true),
            spend: Spend::default(),
            failed: None,
        }),
        // offered and ignored
        asked(Some("retested")),
        // a second session's solve: after the grant, but no rung of the ladder
        asked(None),
        Step::Acted(Act::Looked { items: 7 }),
        asked(Some("tested")),
        Step::Acted(Act::Looked { items: 7 }),
        // twice on one question still counts once
        Step::Acted(Act::Tested {
            without: vec![ContextId(2)],
            before: None,
            after: None,
            moved: Some(false),
            spend: Spend::default(),
            failed: None,
        }),
    ];

    let reached = Reached::over(&steps);

    assert_eq!(
        reached.offered, 3,
        "the solve before the grant and the untagged one do not count"
    );
    assert_eq!(reached.instrumented, 2);
    assert_eq!(reached.rate, Some(0.666_667));
    assert_eq!(reached.tests, 2);
    assert_eq!(reached.looks, 2);
    assert!(reached.clears_the_gate());
}

#[test]
fn a_subject_that_never_reached_for_the_handles_fails_the_gate_rather_than_scoring_badly() {
    let steps = vec![
        Step::Granted {
            tools: vec!["inspect".to_owned()],
            budget: 4,
        },
        asked(Some("retested")),
        asked(Some("retested")),
        Step::Acted(Act::Refused {
            what: "amend".to_owned(),
            why: "not granted".to_owned(),
        }),
    ];

    let reached = Reached::over(&steps);

    assert_eq!((reached.offered, reached.instrumented), (2, 0));
    assert_eq!(reached.refusals, 1);
    assert!(!reached.clears_the_gate());
}

#[test]
fn a_grant_does_not_outlive_the_session_it_was_made_in() {
    // two passes over the same ladder, in two sessions, with the handles arriving partway up
    // each. The rungs below the handles were asked of a subject that had nothing to reach for,
    // in the second session exactly as in the first
    let ladder = || {
        vec![
            Step::Briefed { items: Vec::new() },
            asked(Some("carrying")),
            asked(Some("again")),
            Step::Granted {
                tools: vec!["inspect".to_owned(), "amend".to_owned()],
                budget: 4,
            },
            asked(Some("unprompted")),
            Step::Acted(Act::Looked { items: 7 }),
        ]
    };
    let steps: Vec<Step> = ladder().into_iter().chain(ladder()).collect();

    let reached = Reached::over(&steps);

    assert_eq!(
        reached.offered, 2,
        "one handled question per session; a grant that carried over would count six"
    );
    assert_eq!(reached.instrumented, 2);
    assert!(reached.clears_the_gate());
}
