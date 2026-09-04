//! Runs the suite against a real model and prints what it found.
//!
//! ```text
//! export NACHALNIK_API_KEY=...
//! cargo run --example bench -- -m google/gemini-3.5-flash -r 2 --json out.json
//! ```
//!
//! A local model works too, and costs nothing:
//!
//! ```text
//! NACHALNIK_API_KEY=ollama NACHALNIK_BASE_URL=http://localhost:11434/v1 \
//!   cargo run --example bench -- -m granite4.2:3b
//! ```
//!
//! note: The provider is not in the crate, and cannot be: `nachalnik-eval` ships no HTTP client
//! for the same reason `nachalnik` does not, and the one this example uses lives in
//! `nachalnik-utils`, which is a dev-dependency and never published. Any [`Provider`] does -
//! that is the whole of what makes the suite model-agnostic.
//!
//! note: `--temperature 0` is the default and does not make anything deterministic. It narrows
//! the sampling and nothing more, which is exactly why replicates exist and why a run at `-r 1`
//! reports an instability of zero rather than a stability of one.

use std::{
    env,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use nachalnik::{Config, Kernel, Params, Provider};
use nachalnik_eval::{Outcome, Report, Subject, evaluate, suite};
use nachalnik_utils::{OpenAiCompatible, base_url};
use serde_json::json;

const USAGE: &str = "\
usage: bench [-m MODEL] [-r N] [-l N] [-e NAME].. [--temperature T] [--max-tokens N]
             [--json FILE]

  -m, --model MODEL      the model to measure; or NACHALNIK_TEST_MODEL
  -r, --replicates N     how many copies each condition gets (default 1)
  -l, --ladders N        how many times `repair` runs its ladder per dossier
      --swap             run `privilege` with its two dossiers the other way round
  -e, --experiment NAME  only these: attribution, recursion, lie, privilege,
                         instrumented, repair, feedback
      --temperature T    sent verbatim as a model parameter (default 0)
      --max-tokens N     output budget per request (default 32768; 0 omits it)
      --json FILE        write the whole report, steps and all, here

environment:
  NACHALNIK_API_KEY / OPENROUTER_API_KEY
  NACHALNIK_BASE_URL     e.g. http://localhost:11434/v1  (ollama; any key will do)
  NACHALNIK_TEST_MODEL   the model, if -m is not given";

/// A small, free, widely available model.
const DEFAULT_MODEL: &str = "google/gemini-3.5-flash-lite";

/// The output budget every request is sent with.
///
/// note: generous on purpose, and it costs nothing to be: a cap is a ceiling rather than a
/// reservation, so a model that answers in forty tokens is billed for forty. What it buys is that
/// a reasoning model cannot quietly truncate mid-thought and return an answer with no last line
/// for the reading to find.
///
/// note: 8,192 was the first guess and it was not enough. `deepseek/deepseek-v4-flash-0731` spent
/// 15,374 reasoning tokens on one question about repairing its own context and returned an empty
/// message, twice. The figure has to clear the *thinking*, not the answer, and on these models
/// thinking is where nearly all the tokens go.
const MAX_TOKENS: u32 = 32_768;

#[tokio::main]
async fn main() -> Result<(), nachalnik::BoxError> {
    let mut model = env::var("NACHALNIK_TEST_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let mut replicates = 1usize;
    let mut ladders = suite::LADDERS;
    let mut wanted: Vec<String> = Vec::new();
    let mut temperature = 0.0f64;
    let mut max_tokens = MAX_TOKENS;
    let mut swap = false;
    let mut json: Option<String> = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-m" | "--model" => model = args.next().ok_or("-m wants a model")?,
            "-r" | "--replicates" => {
                replicates = args.next().ok_or("-r wants a number")?.parse()?
            }
            "-l" | "--ladders" => ladders = args.next().ok_or("-l wants a number")?.parse()?,
            "-e" | "--experiment" => wanted.push(args.next().ok_or("-e wants a name")?),
            "--swap" => swap = true,
            "--temperature" => {
                temperature = args.next().ok_or("--temperature wants a number")?.parse()?
            }
            "--max-tokens" => {
                max_tokens = args.next().ok_or("--max-tokens wants a number")?.parse()?
            }
            "--json" => json = Some(args.next().ok_or("--json wants a path")?),
            other => return Err(format!("{other}: not an option\n\n{USAGE}").into()),
        }
    }

    let client = OpenAiCompatible::client();
    let provider: Arc<dyn Provider> = Arc::new(
        OpenAiCompatible::from_env(client, &model)?
            .labelled(base_url())
            // there is nothing to stream to: every answer here is read, not watched
            .streaming(false),
    );

    let mut params = Params::new();
    params.insert("temperature".to_owned(), json!(temperature));
    // note: set rather than left to the provider, because a reasoning model that runs out of
    // output budget mid-thought returns no content at all - `finish_reason: length` and a null
    // message - and every probe here is read off the *last line* of an answer. That arrives as
    // `Answer::Unreadable`, which is scored honestly as untested, so the failure is quiet: a
    // report full of untested claims and no indication that the cause was a token cap. Measured
    // on `inception/mercury-2.5-preview`, which spent 802 reasoning tokens on one division.
    if max_tokens > 0 {
        params.insert("max_tokens".to_owned(), json!(max_tokens));
    }

    // note: the counterbalance for the one experiment that has a presentation confound. The
    // foreign arm's material arrives quoted while the subject's own arrives as its context, so a
    // difference between the arms of a single run is confounded with a difference between the two
    // dossiers; running both orders and pooling is what separates them
    let counterbalanced: Vec<Arc<dyn nachalnik_eval::Experiment>> = vec![Arc::new(
        suite::Privilege::new().swapped().replicates(replicates),
    )];
    let experiments = match swap {
        true => counterbalanced,
        false => suite::all_with(replicates, ladders),
    }
    .into_iter()
    .filter(|experiment| wanted.is_empty() || wanted.iter().any(|name| name == experiment.name()))
    .collect::<Vec<_>>();
    if experiments.is_empty() {
        return Err(format!("no experiment called {}\n\n{USAGE}", wanted.join(", ")).into());
    }
    println!(
        "measuring {model} at {} over {} experiment(s), {replicates} copy/copies per condition, \
         {ladders} ladder(s) per dossier",
        base_url(),
        experiments.len(),
    );
    for experiment in &experiments {
        println!("  {:<14}{}", experiment.name(), experiment.instrument());
    }
    println!();

    let subject = |name: &str| {
        let kernel = Kernel::new(Config {
            session_name: Some(format!("{model}/{name}")),
            ..Config::default()
        });
        kernel.set_provider(provider.clone());
        kernel.set_params(params.clone());

        Ok(Subject::new(kernel))
    };

    // one at a time, and printed as it lands. `evaluate` prints nothing - a library has no
    // business writing to somebody's terminal - and a run of a hundred requests that shows
    // nothing until the last one is a run nobody can tell from a hung one
    let started = SystemTime::now();
    let mut outcomes: Vec<Outcome> = Vec::new();
    for experiment in experiments {
        let one = evaluate([experiment], subject).await;
        for outcome in &one.outcomes {
            println!("{outcome}\n");
        }
        outcomes.extend(one.outcomes);
    }
    let report = Report {
        at: started
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or_default(),
        outcomes,
    };

    println!("{}", summary(&report));
    print_curve(&report);

    if let Some(path) = json {
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
        println!("\nthe whole record, every question and every answer, is in {path}");
    }

    Ok(())
}

/// The last two lines of a [`Report`]'s own rendering: the outcomes above have already been
/// printed one by one as they landed.
fn summary(report: &Report) -> String {
    let spend = report.spend();

    format!(
        "pooled: {}\ntotal:  {} requests, {} in / {} out",
        report.scores(),
        spend.requests,
        spend.input,
        spend.output
    )
}

/// The calibration curve, as far as there is one to draw.
///
/// note: printed rather than plotted, and printed with the counts, because the shape of a curve
/// over forty claims is mostly the shape of where the claims happened to land.
fn print_curve(report: &Report) {
    let scores = report.scores();
    if scores.bins.iter().all(|bin| bin.n == 0) {
        return;
    }

    println!("\ncalibration, pooled:");
    for bin in &scores.bins {
        if bin.n == 0 {
            continue;
        }
        let bar = "#".repeat((bin.accuracy * 20.0).round() as usize);
        println!(
            "  {:.0}-{:.0}%  said {:>4.0}%, right {:>4.0}%  {bar:<20} ({} claim(s))",
            bin.from * 100.0,
            bin.to * 100.0,
            bin.confidence * 100.0,
            bin.accuracy * 100.0,
            bin.n,
        );
    }
}
