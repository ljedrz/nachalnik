//! Reads the reports a sweep produced and computes the figures that are about *models*.
//!
//! ```text
//! cargo run --example pool -- eval-runs/*/*/report.json
//! ```
//!
//! note: separate from `bench` because it answers a different question. `bench` measures one
//! model and every figure it prints is computed over items, where the items share a dossier and
//! are not independent. This pools *runs*, one per model, and the only test it applies is the
//! sign test - which is honest here and nowhere else, because models are independent of each
//! other in a way that items never are.
//!
//! note: it reads the saved reports rather than re-running anything, so the analysis of a sweep
//! costs nothing and can be repeated after the fact. That is the point of `--json` holding every
//! question and every answer: a figure in a paper should be recomputable from the record by
//! somebody who was not there.

use std::{env, fs};

use nachalnik_eval::{Cohort, Kind, Report, Step, Surface, per_model};

/// The default effect size the sign test counts against, in points.
///
/// note: a study's registered effect and not zero, because "the difference was positive" is a much
/// weaker claim than any preregistration makes and the two must not be reported in the same
/// column. Thirty is what the first study registered; `--at-least` is how a later one says what it
/// registered instead.
const REGISTERED: f64 = 0.30;

const USAGE: &str = "\
usage: pool [--at-least POINTS] REPORT.json..

  --at-least POINTS   the registered effect size for the sign test (default 30)

Prints the primary endpoint per model, then the sign test across them.";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut at_least = REGISTERED;
    let mut paths: Vec<String> = Vec::new();

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--at-least" => {
                let points: f64 = args.next().ok_or("--at-least wants a number")?.parse()?;
                at_least = points / 100.0;
            }
            other => paths.push(other.to_owned()),
        }
    }
    if paths.is_empty() {
        return Err(format!("nothing to pool\n\n{USAGE}").into());
    }

    // one report per model. The rule - a report that measured nothing never displaces one that
    // did - lives in the crate rather than here, because it silently cost a model once and a rule
    // that can do that belongs somewhere with a test on it
    let mut loaded: Vec<(Report, String)> = Vec::new();
    for path in &paths {
        let text = fs::read_to_string(path)?;
        let report: Report = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        loaded.push((report, path.clone()));
    }
    let runs = per_model(loaded);

    let surfaces: Vec<Surface> = runs.iter().map(|(_, report, _)| report.surface()).collect();

    println!("the primary endpoint, one row per model\n");
    println!(
        "  {:<38}{:>9}{:>9}{:>10}  run",
        "model", "figures", "plain", "diff"
    );
    let mut figures: Vec<Option<f64>> = Vec::new();
    for ((model, _, path), surface) in runs.iter().zip(&surfaces) {
        figures.push(surface.difference.filter(|_| surface.is_measurable()));
        let cell = |claimed: usize, total: usize| match total {
            0 => "     -".to_owned(),
            _ => format!("{claimed:>3}/{total:<3}"),
        };
        println!(
            "  {:<38}{:>9}{:>9}{:>10}  {}",
            model,
            cell(surface.claimed_numeric, surface.numeric),
            cell(surface.claimed_plain, surface.plain),
            match surface.difference {
                Some(d) if surface.is_measurable() => format!("{:+.0}", d * 100.0),
                _ => "-".to_owned(),
            },
            run_of(path),
        );
    }

    // a report from before the claims carried their material cannot be read for this endpoint,
    // and saying so beats printing a dash. Such runs are non-comparable for other reasons too, but
    // a reader looking at a column of dashes deserves to know whether the model said nothing or
    // the file cannot answer
    let stale: Vec<&str> = runs
        .iter()
        .zip(&surfaces)
        .filter(|((_, report, _), surface)| !surface.is_measurable() && asks_it(report))
        .map(|((model, ..), _)| model.as_str())
        .collect();
    if !stale.is_empty() {
        println!(
            "\n  {} run(s) predate the endpoint and carry no material on their claims: {}",
            stale.len(),
            stale.join(", ")
        );
    }

    let cohort = Cohort::over(figures.clone(), at_least);
    println!("\n  at least {:+.0} points: {cohort}", at_least * 100.0);
    if !cohort.is_unanimous() && cohort.is_measurable() {
        println!(
            "  note: not unanimous, so this is {} per-model results and no model-level claim - \
             see P1",
            cohort.agreed
        );
    }

    // and the same figures pooled over every item of every model, which is *not* the endpoint
    // and is printed last so that nobody reads it first
    let pooled = Surface::of(
        surfaces.iter().map(|s| s.claimed_numeric).sum(),
        surfaces.iter().map(|s| s.numeric).sum(),
        surfaces.iter().map(|s| s.claimed_plain).sum(),
        surfaces.iter().map(|s| s.plain).sum(),
    );
    println!("\n  pooled over every item of every model (descriptive only): {pooled}");

    Ok(())
}

/// The run directory a report came from, which is what names it in `eval-runs/`.
fn run_of(path: &str) -> &str {
    path.trim_end_matches("/report.json")
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

/// Whether a report asked the question the endpoint is read from at all.
///
/// note: it did if it resolved a counterfactual claim; whether that claim says which note it was
/// about is a different matter, and the difference between "this model was not asked" and "this
/// file cannot be read for it".
fn asks_it(report: &Report) -> bool {
    report.outcomes.iter().any(|outcome| {
        outcome
            .steps
            .iter()
            .any(|step| matches!(step, Step::Resolved(r) if r.about == Kind::Counterfactual))
    })
}
