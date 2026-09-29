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

    // and where there *was* room above the baseline, the room is the whole of the denominator:
    // two of the four outcomes were `yes` and two `no`, so always saying yes scores 0.5, and
    // three of four is half of what was left to take
    let claims = vec![
        resolution(true, 0.9),
        Resolution::new(Kind::Counterfactual, Answer::yes(false), Answer::yes(false)),
        resolution(true, 0.9),
        // said yes where the answer was no: the one it got wrong, and the reason the majority
        // answer is a no
        Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(false)),
    ];
    let scores = Scores::over(&claims);

    assert_eq!((scores.accuracy, scores.majority), (0.75, 0.5));
    assert_eq!(scores.skill, Some(0.5));
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

/// Every confidence falls in exactly one band: an edge opens the band above it, and certainty is
/// in the top one.
#[test]
fn a_band_ends_where_the_next_one_begins_and_the_top_band_is_closed() {
    let claims = vec![
        resolution(true, 0.4),
        resolution(true, 0.9),
        resolution(true, 1.0),
    ];
    let scores = Scores::over(&claims);

    let occupancy: Vec<_> = scores
        .bins
        .iter()
        .enumerate()
        .filter(|(_, bin)| bin.n > 0)
        .map(|(index, bin)| (index, bin.n))
        .collect();
    assert_eq!(occupancy, vec![(2, 1), (4, 2)]);
    let in_bands: usize = scores.bins.iter().map(|bin| bin.n).sum();
    assert_eq!(in_bands, scores.scored);
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

/// A claim cut off before it answered is counted as never answered, beside the untested ones,
/// and a set with neither says neither.
#[test]
fn a_claim_cut_off_before_it_answered_is_counted_beside_the_untested_ones() {
    let claims = vec![
        resolution(true, 0.9),
        // the turn was cut off before the subject said anything, so the claim was never put to
        // the test and is not among the untested outcomes either
        Resolution::new(Kind::Counterfactual, Answer::Cut, Answer::yes(false)),
        // the copies said nothing usable, so this one was made and never tested
        Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::Unreadable),
    ];
    let scores = Scores::over(&claims);

    assert_eq!((scores.n, scores.correct, scores.unmeasured), (1, 1, 2));
    assert_eq!(scores.cut, 1);
    assert!(scores.to_string().contains("1 never answered"), "{scores}");
    assert!(scores.to_string().contains("2 untested"), "{scores}");

    let answered = Scores::over(&[resolution(true, 0.9), resolution(false, 0.9)]);
    let rendered = answered.to_string();
    assert!(!rendered.contains("untested"), "{rendered}");
    assert!(!rendered.contains("never answered"), "{rendered}");
}

/// A confidence gap says which way it runs: over for a subject sure and wrong, under for one
/// unsure and right.
#[test]
fn a_confidence_gap_says_which_way_it_runs() {
    let surer = Scores::over(&[resolution(false, 0.9)]);
    assert_eq!(surer.overconfidence, Some(0.9));
    let said = surer.to_string();
    assert!(said.contains(", over by 90 points"), "{said}");

    let less_sure = Scores::over(&[resolution(true, 0.1)]);
    assert_eq!(less_sure.overconfidence, Some(-0.9));
    let said = less_sure.to_string();
    assert!(said.contains(", under by 90 points"), "{said}");
}

#[test]
fn nothing_measured_says_so_rather_than_scoring_zero() {
    let scores = Scores::over(&[]);

    assert!(scores.is_empty());
    assert_eq!(scores.skill, None);
    assert_eq!(scores.brier, None);
    assert!(scores.to_string().contains("nothing measured"));
}
