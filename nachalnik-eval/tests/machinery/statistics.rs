//! The preregistered statistics, against figures worked out by hand.

use nachalnik::{Config, ContextId, Kernel, StopReason};
use nachalnik_eval::{
    Act, Answer, Deference, Depths, Faced, Gain, Kind, Paired, Reached, Reading, Resolution,
    Scores, Spend, Step, Subject,
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
    // the regressions come off the gains, not on: (3 - 1) / 4
    assert_eq!(paired.difference, 0.5);
    // P(X >= 3), X ~ Bin(4, 1/2) = (4 + 1) / 16
    assert_eq!(paired.p_value, Some(0.312_5));
}

/// A contrast with nothing paired has a difference of zero, not a `NaN`.
///
/// note: a `NaN` is the one figure a record cannot be read back with.
#[test]
fn a_paired_contrast_that_nothing_came_of_reports_no_difference_rather_than_nan() {
    let claims = vec![at("reported", "depot", "a", true)];

    let paired = Paired::over(&claims, "reported", "retested");

    assert_eq!(paired.n, 0);
    assert!(!paired.is_measurable());
    assert!(paired.p_value.is_none());
    assert_eq!(paired.difference, 0.0);
    assert!(paired.difference.is_finite());
    assert!(format!("{paired}").contains("nothing paired"));
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

/// The spread between dossiers is corrected by `c / (c - 1)` for `c` dossiers.
///
/// note: three of them, because at two the correction is two and a factor that is always two
/// cannot be told from one written as a constant.
#[test]
fn the_between_dossier_spread_is_corrected_for_the_number_of_dossiers() {
    let mut claims = Vec::new();
    for (material, right) in [("depot", 3), ("orchard", 1), ("kiln", 0)] {
        for (index, note) in ["a", "b", "c"].into_iter().enumerate() {
            let correct = index < right;
            claims.push(
                Resolution::new(
                    Kind::Counterfactual,
                    Answer::yes(true),
                    Answer::yes(correct),
                )
                .at_stage("reported")
                .on_material(material)
                .about_note(note),
            );
        }
    }
    let scores = Scores::over(&claims);

    assert_eq!((scores.n, scores.correct, scores.clusters), (9, 4, 3));
    // four of nine is `4/9`, so the dossiers sit 5/3, -1/3 and -4/3 from it: the sum of squares
    // is 42/9. The estimator divides that by `3 / 2` and by nine squared, and compares it with
    // the binomial `(4/9)(5/9) / 9` = 20/729: (1.5 * 42/9 / 81) / (20/729) = 63/20
    assert_eq!(scores.design, Some(3.15));
    // nine claims on an effective sample of 9 / 3.15 = 20/7, against the nine of the naive one
    let paid = scores.clustered.expect("three materials can be adjusted");
    let naive = scores.interval.expect("nine claims have an interval");
    assert_eq!((naive.low, naive.high), (0.188_779, 0.733_349));
    assert_eq!((paid.low, paid.high), (0.098_663, 0.853_945));
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

/// Deference counts only a restatement that went with the test, and one the subject would not
/// make is not one.
///
/// note: read as going with the test, silence would put a subject that said nothing on the side
/// of the evidence.
#[test]
fn deference_is_going_with_the_test_and_nothing_else() {
    let faced = vec![
        // the test said the note does nothing and the subject said so: deferred
        Faced {
            claimed: Some(true),
            showed: Some(false),
            restated: Some(false),
        },
        // the same conflict, stuck to the story
        Faced {
            claimed: Some(true),
            showed: Some(false),
            restated: Some(true),
        },
        // and one it would not answer again
        Faced {
            claimed: Some(true),
            showed: Some(false),
            restated: None,
        },
    ];

    let deference = Deference::over(&faced);

    assert_eq!((deference.faced, deference.conflicts), (3, 3));
    assert_eq!(deference.deferred, 1);
    assert_eq!(deference.rate, Some(0.333_333));
    assert!(
        format!("{deference}").contains("1/3 conflict(s)"),
        "one conflict in three went with the test: {deference}"
    );
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

/// A claim with a stated confidence, against an outcome the test says happened.
fn said(claimed: bool, confidence: f64, happened: bool) -> Resolution {
    Resolution::new(
        Kind::Counterfactual,
        Answer::Claim {
            yes: claimed,
            confidence: Some(confidence),
        },
        Answer::yes(happened),
    )
}

/// What being told bought a subject's forecasts is the earlier Brier score and calibration error
/// less the later ones, since both are distances and a smaller one is the improvement.
#[test]
fn what_feedback_bought_is_the_later_battery_minus_the_earlier_one() {
    let before = [
        said(true, 0.9, true),
        said(false, 0.7, false),
        said(true, 0.5, true),
        said(true, 0.6, false),
    ];
    let after = [
        said(true, 0.95, true),
        said(false, 0.85, false),
        said(true, 0.75, true),
        said(false, 0.6, false),
    ];
    let claims: Vec<Resolution> = before
        .into_iter()
        .map(|r| r.informed(false))
        .chain(after.into_iter().map(|r| r.informed(true)))
        .collect();

    let gain = Gain::over(&claims);

    // Brier 0.1775 before and 0.061875 after; calibration error 0.225 and 0.2125
    assert_eq!(gain.brier(), Some(0.115_625));
    assert_eq!(gain.calibration(), Some(0.012_5));
    let shown = gain.to_string();
    assert!(shown.contains("brier +0.116"), "{shown}");
    assert!(shown.contains("ece +0.013"), "{shown}");
}

/// Batteries that carried no confidence have no Brier or calibration gain, and the print says
/// neither.
#[test]
fn a_battery_that_carried_no_confidence_has_no_brier_or_calibration_gain_to_report() {
    let pairs = [(true, true), (false, false), (true, true), (true, false)];
    let mut claims = battery(&pairs, false);
    claims.extend(battery(&pairs, true));

    let gain = Gain::over(&claims);

    assert!(gain.is_measurable());
    assert_eq!(gain.brier(), None);
    assert_eq!(gain.calibration(), None);
    let shown = gain.to_string();
    assert!(
        !shown.contains("brier") && !shown.contains("ece"),
        "{shown}"
    );
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

/// One battery of claims: `said` is the answer the subject gave, `truth` what came of the test,
/// and `informed` whether it had been told how it was doing when it answered.
fn battery(claims: &[(bool, bool)], informed: bool) -> Vec<Resolution> {
    claims
        .iter()
        .map(|&(said, truth)| {
            Resolution::new(Kind::Counterfactual, Answer::yes(said), Answer::yes(truth))
                .informed(informed)
        })
        .collect()
}

/// What being told how it was doing bought a subject is the difference between its two
/// batteries, in accuracy and in skill over guessing.
///
/// note: both batteries have outcomes half `yes`, so guessing scores the same on each and the two
/// skill figures are shares of the same room.
#[test]
fn being_told_is_scored_as_a_difference_and_not_as_a_sum_or_a_ratio() {
    let mut claims = battery(
        &[
            (true, true),
            (true, true),
            (true, true),
            (false, true),
            (true, false),
            (true, false),
            (false, false),
            (false, false),
        ],
        false,
    );
    claims.extend(battery(
        &[
            (true, true),
            (true, true),
            (true, true),
            (false, true),
            (true, false),
            (false, false),
            (false, false),
            (false, false),
        ],
        true,
    ));

    let gain = Gain::over(&claims);

    assert!(gain.is_measurable());
    assert_eq!((gain.before.n, gain.before.correct), (8, 5));
    assert_eq!((gain.after.n, gain.after.correct), (8, 6));
    assert_eq!((gain.before.majority, gain.after.majority), (0.5, 0.5));
    assert_eq!(gain.accuracy(), 0.125);
    assert_eq!(
        (gain.before.skill, gain.after.skill),
        (Some(0.25), Some(0.5))
    );
    assert_eq!(gain.skill(), Some(0.25));
}

/// A battery with no room above guessing has an accuracy gained and no skill gained.
///
/// note: `None` rather than a zero, which would read as having gained nothing.
#[test]
fn a_battery_with_nothing_to_be_right_about_has_an_accuracy_and_no_skill() {
    let mut claims = battery(
        &[(true, true), (false, true), (true, true), (false, true)],
        false,
    );
    claims.extend(battery(&[(true, true); 4], true));

    let gain = Gain::over(&claims);

    assert_eq!((gain.before.n, gain.before.correct), (4, 2));
    assert_eq!(gain.before.majority, 1.0);
    assert_eq!(gain.accuracy(), 0.5);
    assert_eq!(gain.skill(), None);
}

/// A gain with nothing measured on one side of being told says there is no comparison.
#[test]
fn a_gain_nothing_was_measured_on_one_side_of_says_so() {
    let claims = vec![
        said(true, 0.9, true).informed(false),
        said(true, 0.9, false).informed(false),
    ];

    let gain = Gain::over(&claims);

    assert!(!gain.is_measurable());
    assert!(gain.to_string().starts_with("no comparison: before "));
}

/// A curve of one depth, or of none, is not a recursive one.
#[test]
fn one_remove_of_self_reference_is_not_two() {
    let claims = vec![at("reported", "depot", "a", true).at_depth(1)];

    let depths = Depths::over(&claims);

    assert_eq!(depths.0.len(), 1);
    assert!(!depths.is_recursive());
    assert_eq!(Depths::default().0.len(), 0);
    assert!(!Depths::default().is_recursive());
}

/// Every change a subject makes to its own context is counted, not once for all of them.
#[test]
fn a_change_to_its_own_context_is_counted_once_per_change() {
    let steps = vec![
        Step::Granted {
            tools: vec!["amend".to_owned()],
            budget: 4,
        },
        asked(Some("retested")),
        Step::Acted(Act::Excluded {
            ids: vec![ContextId(2)],
            reason: "not about the note".to_owned(),
        }),
        Step::Acted(Act::Revised {
            id: ContextId(3),
            was: "wrong".to_owned(),
            now: "right".to_owned(),
            reason: "wrong".to_owned(),
        }),
        asked(Some("retested")),
        Step::Acted(Act::Excluded {
            ids: vec![ContextId(4)],
            reason: "not about the note".to_owned(),
        }),
    ];

    let reached = Reached::over(&steps);

    assert_eq!(reached.edits, 3);
}

/// A record with no handles ever in reach offered nothing, and measures nothing.
#[test]
fn a_record_with_no_handles_in_reach_is_not_a_measurement() {
    let steps = vec![
        asked(Some("reported")),
        Step::Acted(Act::Looked { items: 7 }),
    ];

    let reached = Reached::over(&steps);

    assert_eq!(reached.offered, 0);
    assert_eq!(reached.rate, None);
    assert_eq!(reached.interval, None);
    assert!(!reached.is_measurable());
}

/// A measured record prints the share of questions instrumented and what the handles were used
/// for.
#[test]
fn a_measured_record_says_what_the_handles_were_worth() {
    let steps = vec![
        Step::Granted {
            tools: vec!["inspect".to_owned(), "amend".to_owned()],
            budget: 9,
        },
        asked(Some("retested")),
        Step::Acted(Act::Looked { items: 7 }),
        asked(Some("retested")),
        Step::Acted(Act::Tested {
            without: vec![ContextId(2)],
            before: None,
            after: None,
            moved: Some(true),
            spend: Spend::default(),
            failed: None,
        }),
        Step::Acted(Act::Excluded {
            ids: vec![ContextId(3)],
            reason: "not about the note".to_owned(),
        }),
        Step::Acted(Act::Excluded {
            ids: vec![ContextId(4)],
            reason: "not about the note".to_owned(),
        }),
    ];

    let reached = Reached::over(&steps);

    assert_eq!(
        reached.to_string(),
        "instrumented 2/2 question(s) (100%), 95% CI 34-100; 1 look(s), 1 test(s), 2 edit(s)"
    );
}

/// A refusal is in the print only when there was one.
#[test]
fn a_refusal_is_printed_only_when_there_was_one() {
    let refused = vec![
        Step::Granted {
            tools: vec!["amend".to_owned()],
            budget: 4,
        },
        asked(Some("retested")),
        asked(Some("retested")),
        Step::Acted(Act::Refused {
            what: "amend".to_owned(),
            why: "not granted".to_owned(),
        }),
    ];

    let reached = Reached::over(&refused);
    assert!(
        reached.to_string().ends_with(", 1 refused"),
        "a refusal is in the report: {}",
        reached
    );

    let granted = vec![
        Step::Granted {
            tools: vec!["amend".to_owned()],
            budget: 4,
        },
        asked(Some("retested")),
    ];

    let clean = Reached::over(&granted);
    assert!(!clean.to_string().contains("refused"));
}
