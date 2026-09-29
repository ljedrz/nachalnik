//! The intervals a score is reported with.

use nachalnik_eval::{Answer, Kind, Resolution, Scores};

use crate::resolution;

#[test]
fn an_accuracy_comes_with_an_interval_and_a_p_value() {
    let claims = vec![
        resolution(true, 0.9),
        resolution(true, 0.9),
        resolution(true, 0.9),
        resolution(false, 0.9),
    ];
    let scores = Scores::over(&claims);

    // three of four, Wilson at 95%: wide, which is the point of reporting it
    let interval = scores.interval.expect("four claims have an interval");
    assert!((interval.low - 0.3006).abs() < 1e-3, "{interval:?}");
    assert!((interval.high - 0.9544).abs() < 1e-3, "{interval:?}");

    // three of the four outcomes were `yes`, so the commonest answer *is* right three times in
    // four, and getting three or more that way happens 74% of the time. The accuracy and the
    // baseline are the same number here, and the p-value is what says so out loud
    assert_eq!(scores.majority, 0.75);
    assert_eq!(scores.p_value, Some(0.738281));
}

#[test]
fn the_binomial_tail_is_the_exact_one() {
    // four of four against a coin: 1/16
    let perfect: Vec<_> =
        (0..4)
            .map(|_| Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(true)))
            .chain((0..4).map(|_| {
                Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(false))
            }))
            .collect();
    // half the outcomes are yes and half no, and it said yes every time: 4 of 8, p = 0.64
    let scores = Scores::over(&perfect);
    assert_eq!((scores.n, scores.correct), (8, 4));
    assert_eq!(scores.majority, 0.5);
    assert_eq!(scores.p_value, Some(0.636719));
}

#[test]
fn an_interval_is_not_a_claim_of_certainty_at_the_edges() {
    let claims: Vec<_> = (0..4)
        .map(|_| Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(true)))
        .collect();
    let scores = Scores::over(&claims);

    assert_eq!(scores.accuracy, 1.0);
    // every outcome was the same, so there is no baseline to beat and no skill to report
    assert_eq!(scores.skill, None);
    let interval = scores.interval.unwrap();
    assert!(
        interval.low < 1.0,
        "four of four is not certainty: {interval:?}"
    );
    assert_eq!(interval.high, 1.0);
}
