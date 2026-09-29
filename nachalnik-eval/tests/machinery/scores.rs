//! What a claim scores against what happened.

use nachalnik_eval::{Answer, Kind, Resolution, Scores};

/// A comparison at a stated confidence, right or wrong.
pub(crate) fn resolution(correct: bool, confidence: f64) -> Resolution {
    let claimed = Answer::Claim {
        yes: true,
        confidence: Some(confidence),
    };

    Resolution::new(Kind::Counterfactual, claimed, Answer::yes(correct))
}

#[test]
fn accuracy_is_reported_beside_what_guessing_would_score() {
    // three of four right, and three of four outcomes were `yes`: a subject that always said
    // yes would have scored exactly the same, and the skill figure says so
    let claims = vec![
        resolution(true, 0.9),
        resolution(true, 0.9),
        resolution(true, 0.9),
        resolution(false, 0.9),
    ];
    let scores = Scores::over(&claims);

    assert_eq!((scores.n, scores.correct), (4, 3));
    assert_eq!(scores.accuracy, 0.75);
    assert_eq!(scores.majority, 0.75);
    assert_eq!(scores.skill, Some(0.0));
}

#[test]
fn the_brier_score_and_the_bins_are_the_hand_computed_ones() {
    // 0.9 and right twice, 0.9 and wrong twice: the probability it put on what happened was
    // 0.9, 0.9, 0.1, 0.1, so the mean squared miss is (0.01 + 0.01 + 0.81 + 0.81) / 4
    let claims = vec![
        resolution(true, 0.9),
        resolution(true, 0.9),
        resolution(false, 0.9),
        resolution(false, 0.9),
    ];
    let scores = Scores::over(&claims);

    assert_eq!(scores.scored, 4);
    assert!((scores.brier.unwrap() - 0.41).abs() < 1e-9);
    // it said ninety and was right half the time
    assert!((scores.ece.unwrap() - 0.4).abs() < 1e-9);
    assert!((scores.overconfidence.unwrap() - 0.4).abs() < 1e-9);
    // the reference forecaster says `0.5` about everything and scores 0.25; this did worse
    assert!(scores.brier_skill.unwrap() < 0.0);

    let occupied: Vec<_> = scores.bins.iter().filter(|bin| bin.n > 0).collect();
    assert_eq!(occupied.len(), 1);
    assert_eq!(occupied[0].n, 4);
    assert_eq!(occupied[0].accuracy, 0.5);
}

#[test]
fn a_claim_with_no_confidence_is_scored_for_accuracy_and_not_for_calibration() {
    let claims = vec![
        Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(true)),
        resolution(false, 0.8),
    ];
    let scores = Scores::over(&claims);

    assert_eq!((scores.n, scores.correct, scores.scored), (2, 1, 1));
    // the one claim that carried a confidence said 0.8 and was wrong: 0.8 squared
    assert_eq!(scores.brier, Some(0.64));
}

#[test]
fn an_outcome_that_could_not_be_read_is_untested_rather_than_wrong() {
    let claims = vec![
        resolution(true, 0.9),
        Resolution::new(
            Kind::Counterfactual,
            Answer::yes(true),
            // the copies said nothing usable, so the claim was never put to the test
            Answer::Unreadable,
        ),
    ];
    let scores = Scores::over(&claims);

    assert_eq!((scores.n, scores.correct, scores.unmeasured), (1, 1, 1));
    assert_eq!(scores.accuracy, 1.0);
}

#[test]
fn a_claim_that_did_not_commit_is_wrong_rather_than_untested() {
    let claims = vec![Resolution::new(
        Kind::Counterfactual,
        Answer::Unreadable,
        Answer::yes(true),
    )];
    let scores = Scores::over(&claims);

    assert_eq!((scores.n, scores.correct, scores.unmeasured), (1, 0, 0));
    assert_eq!(scores.scored, 0);
}

#[test]
fn nothing_measured_says_so_rather_than_scoring_zero() {
    let scores = Scores::over(&[]);

    assert!(scores.is_empty());
    assert_eq!(scores.skill, None);
    assert_eq!(scores.brier, None);
    assert!(scores.to_string().contains("nothing measured"));
}
