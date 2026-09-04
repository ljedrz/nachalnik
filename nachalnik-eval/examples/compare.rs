//! Puts saved runs side by side, and refuses to pretend that runs asked different questions are
//! comparable.
//!
//! ```text
//! cargo run -p nachalnik-eval --example compare -- eval-runs/*/*/report.json
//! ```
//!
//! note: The refusal is the feature. Everything else here is arithmetic anybody could do in a
//! spreadsheet; what a spreadsheet will not do is notice that one of the files was produced by an
//! instrument whose questions had a word changed in them. Runs are grouped by
//! [`Instrument::digest`], the groups are reported separately, and a comparison across groups is
//! printed only under a heading saying what is wrong with it.
//!
//! note: It reads the crate's own `Report` back through `serde`, which is the point of the record
//! being a value rather than a log: a run can be re-read, re-scored and re-tabulated months later
//! without the model that produced it being asked anything again.

use std::{collections::BTreeMap, env, fs};

use nachalnik_eval::{Kind, Outcome, Report, Scores};

const USAGE: &str = "usage: compare FILE...\n\n  FILE   a report.json written by the `bench` \
                     example";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let files: Vec<String> = env::args().skip(1).collect();
    if files.is_empty() || files.iter().any(|f| f == "-h" || f == "--help") {
        println!("{USAGE}");
        return Ok(());
    }

    let mut runs: Vec<(String, Report)> = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file)?;
        let report: Report = serde_json::from_str(&text)?;
        runs.push((file.clone(), report));
    }

    // one group per instrument: two runs in different groups were not asked the same thing, and
    // no amount of arithmetic afterwards can put that right
    let mut groups: BTreeMap<String, Vec<&Outcome>> = BTreeMap::new();
    for (_, report) in &runs {
        for outcome in &report.outcomes {
            groups
                .entry(format!("{}:{}", outcome.experiment, outcome.instrument))
                .or_default()
                .push(outcome);
        }
    }

    let mut experiments: BTreeMap<&str, Vec<&Instrumented>> = BTreeMap::new();
    let instrumented: Vec<Instrumented> = groups
        .iter()
        .flat_map(|(key, outcomes)| {
            outcomes.iter().map(move |outcome| Instrumented {
                key: key.clone(),
                outcome,
            })
        })
        .collect();
    for one in &instrumented {
        experiments
            .entry(&one.outcome.experiment)
            .or_default()
            .push(one);
    }

    for (experiment, runs) in &experiments {
        let asked: Vec<&str> = {
            let mut seen: Vec<&str> = runs
                .iter()
                .map(|one| one.outcome.instrument.digest.as_str())
                .collect();
            seen.sort_unstable();
            seen.dedup();
            seen
        };

        println!("\n=== {experiment}");
        if asked.len() > 1 {
            println!(
                "  !! {} different instruments below. These rows are NOT comparable with each \
                 other; a question, a reading or a dossier differs between them.",
                asked.len()
            );
        }
        println!(
            "  {:<26} {:<22} {:>16} {:>7} {:>7} {:>7} {:>7} {:>6}",
            "model", "instrument", "right", "guess", "skill", "brier", "ece", "over"
        );
        for one in runs {
            let outcome = one.outcome;
            let scores = &outcome.scores;
            println!(
                "  {:<26} {:<22} {:>16} {:>7} {:>7} {:>7} {:>7} {:>6}",
                outcome
                    .model
                    .as_ref()
                    .map(|model| model.model.clone())
                    .unwrap_or_else(|| "?".to_owned()),
                outcome.instrument.digest,
                right(scores),
                percent(Some(scores.majority)),
                figure(scores.skill),
                figure(scores.brier),
                figure(scores.ece),
                percent(scores.overconfidence),
            );
            for check in outcome.checks.iter().filter(|check| !check.held) {
                println!("      unmet: {}: {}", check.what, check.detail);
            }
            if let Some(failed) = &outcome.failed {
                println!("      stopped: {failed}");
            }
        }
    }

    dissociation(&instrumented);
    depths(&instrumented);

    Ok(())
}

/// An outcome and the instrument group it belongs to.
struct Instrumented<'a> {
    key: String,
    outcome: &'a Outcome,
}

/// The claim families that come apart: saying *what* an answer rests on, against saying *where*
/// the thing is.
///
/// note: worth its own table because it is the one place two families measured on the same
/// subject, in the same session, about the same item, can be read against each other - and the
/// order they come out in is the opposite of what anybody expects.
fn dissociation(runs: &[Instrumented]) {
    println!("\n=== what against where, pooled per model");
    println!(
        "  {:<26} {:>16} {:>16} {:>16}",
        "model", "attribution", "location", "counterfactual"
    );

    let mut per_model: BTreeMap<String, Vec<&Outcome>> = BTreeMap::new();
    for one in runs {
        let name = one
            .outcome
            .model
            .as_ref()
            .map(|model| model.model.clone())
            .unwrap_or_else(|| "?".to_owned());
        per_model.entry(name).or_default().push(one.outcome);
    }

    for (model, outcomes) in per_model {
        let of = |kind: Kind| -> String {
            let (mut n, mut correct) = (0, 0);
            for outcome in &outcomes {
                for family in &outcome.families {
                    if family.kind == kind {
                        n += family.scores.n;
                        correct += family.scores.correct;
                    }
                }
            }
            match n {
                0 => "-".to_owned(),
                _ => format!("{correct}/{n}"),
            }
        };
        println!(
            "  {:<26} {:>16} {:>16} {:>16}",
            model,
            of(Kind::Attribution),
            of(Kind::Location),
            of(Kind::Counterfactual)
        );
    }
}

/// Accuracy at each remove of self-reference, per model.
fn depths(runs: &[Instrumented]) {
    let rows: Vec<&Instrumented> = runs
        .iter()
        .filter(|one| one.outcome.depths.is_recursive())
        .collect();
    if rows.is_empty() {
        return;
    }

    println!("\n=== self-reference, one remove at a time");
    for one in rows {
        let model = one
            .outcome
            .model
            .as_ref()
            .map(|model| model.model.clone())
            .unwrap_or_else(|| "?".to_owned());
        let curve: Vec<String> = one
            .outcome
            .depths
            .0
            .iter()
            .map(|depth| {
                format!(
                    "{}:{}/{}",
                    depth.depth, depth.scores.correct, depth.scores.n
                )
            })
            .collect();
        println!("  {:<26} {}  [{}]", model, curve.join("  "), one.key);
    }
}

/// `3/4 (30-95)`, or a dash where nothing was measured.
fn right(scores: &Scores) -> String {
    if scores.is_empty() {
        return "-".to_owned();
    }
    match scores.interval {
        Some(interval) => format!(
            "{}/{} ({:.0}-{:.0})",
            scores.correct,
            scores.n,
            interval.low * 100.0,
            interval.high * 100.0
        ),
        None => format!("{}/{}", scores.correct, scores.n),
    }
}

/// A figure to three places, or a dash.
fn figure(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{value:+.3}"))
}

/// A fraction as whole percent, or a dash.
fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{:+.0}", value * 100.0))
}
