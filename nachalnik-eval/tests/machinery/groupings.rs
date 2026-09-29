//! How the claims of a run are grouped for a report: by remove of self-reference, and by family.

use nachalnik_eval::{Answer, Depths, Family, Kind, Resolution};

/// A recursive claim at `depth`, right or wrong.
fn at_recursion(depth: usize, correct: bool) -> Resolution {
    Resolution::new(Kind::Recursive, Answer::yes(true), Answer::yes(correct)).at_depth(depth)
}

/// A claim of one family, right or wrong.
fn about(about: Kind, correct: bool) -> Resolution {
    Resolution::new(about, Answer::yes(true), Answer::yes(correct))
}

/// The depth curve prints one line for each remove, in order, and no blank line before them.
#[test]
fn the_depth_curve_puts_one_line_on_each_remove_and_no_blank_first() {
    let curve = Depths::over(&[at_recursion(1, true), at_recursion(2, false)]).to_string();
    let lines: Vec<&str> = curve.lines().collect();
    assert_eq!(lines.len(), 2, "{curve:?}");
    assert!(lines[0].starts_with("  depth 1:"), "{curve:?}");
    assert!(lines[1].starts_with("  depth 2:"), "{curve:?}");

    let one = Depths::over(&[at_recursion(1, true)]).to_string();
    assert_eq!(one.lines().count(), 1, "{one:?}");
    assert!(!one.starts_with('\n'), "{one:?}");
}

/// Each family is scored over its own claims only.
#[test]
fn a_claim_is_scored_against_its_own_family_and_not_against_the_rest() {
    let claims = vec![
        about(Kind::Counterfactual, true),
        about(Kind::Recursive, true),
        about(Kind::Recursive, true),
        about(Kind::Recursive, false),
    ];

    let families = Family::over(&claims);

    let kinds: Vec<Kind> = families.iter().map(|family| family.kind).collect();
    assert_eq!(kinds, vec![Kind::Counterfactual, Kind::Recursive]);
    assert_eq!((families[0].scores.n, families[0].scores.correct), (1, 1));
    assert_eq!((families[1].scores.n, families[1].scores.correct), (3, 2));
}
