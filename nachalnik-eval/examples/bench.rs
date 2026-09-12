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
use nachalnik_eval::{Pace, Report, Subject, evaluate_with, suite};
use nachalnik_utils::base_url;
use serde_json::json;

const USAGE: &str = "\
usage: bench [-m MODEL] [-r N] [-l N] [-e NAME].. [--temperature T] [--max-tokens N]
             [--json FILE]

  -m, --model MODEL      the model to measure; or NACHALNIK_TEST_MODEL
  -r, --replicates N     how many copies each condition gets (default 1)
  -l, --ladders N        how many times `repair` runs its ladder per dossier
  -j, --at-once N        how many requests may be in flight at once (default 1)
      --per-minute N     and how many may be started in any sixty seconds (default none)
      --swap             run `privilege` with its two dossiers the other way round
  -e, --experiment NAME  only these: attribution, recursion, lie, conflict,
                         provenance, privilege, instrumented, repair, feedback
      --temperature T    sent verbatim as a model parameter (default 0)
      --max-tokens N     output budget per request (default 32768; 0 omits it)
      --json FILE        write the whole report, steps and all, here; defaults to
                         bench-<model>-<when>.json in the working directory
      --no-json          do not write one

environment:
  NACHALNIK_API_KEY / OPENROUTER_API_KEY
  NACHALNIK_BASE_URL     e.g. http://localhost:11434/v1  (ollama; any key will do)
  NACHALNIK_TEST_MODEL   the model, if -m is not given
  NACHALNIK_APP_URL      name the app the run is on behalf of, where the endpoint ranks
  NACHALNIK_APP_TITLE    them; both or neither, and no claim is made unless both are set";

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
    let mut at_once = 1usize;
    let mut per_minute = 0usize;
    let mut wanted: Vec<String> = Vec::new();
    let mut temperature = 0.0f64;
    let mut max_tokens = MAX_TOKENS;
    let mut swap = false;
    let mut json: Option<String> = None;
    let mut no_json = false;

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
            "-j" | "--at-once" => at_once = args.next().ok_or("-j wants a number")?.parse()?,
            "--per-minute" => {
                per_minute = args.next().ok_or("--per-minute wants a number")?.parse()?
            }
            "-e" | "--experiment" => wanted.push(args.next().ok_or("-e wants a name")?),
            "--swap" => swap = true,
            "--temperature" => {
                temperature = args.next().ok_or("--temperature wants a number")?.parse()?
            }
            "--max-tokens" => {
                max_tokens = args.next().ok_or("--max-tokens wants a number")?.parse()?
            }
            "--json" => json = Some(args.next().ok_or("--json wants a path")?),
            "--no-json" => no_json = true,
            other => return Err(format!("{other}: not an option\n\n{USAGE}").into()),
        }
    }

    let provider: Arc<dyn Provider> = Arc::new(
        nachalnik_utils::provider(&model)?
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
         {ladders} ladder(s) per dossier, {at_once} request(s) at once{}",
        base_url(),
        experiments.len(),
        match per_minute {
            0 => String::new(),
            n => format!(", {n} per minute"),
        },
    );
    for experiment in &experiments {
        println!("  {:<14}{}", experiment.name(), experiment.instrument());
    }
    println!();

    // note: one `Pace` for the whole run, built once and handed to every experiment, because a
    // rate is only obeyed if the window is shared - a limit of twenty a minute applied afresh per
    // experiment is nine times the limit.
    let pace = match per_minute {
        0 => Pace::at_once(at_once),
        n => Pace::at_once(at_once).per_minute(n),
    };

    let subject = |name: &str| {
        let kernel = Kernel::new(Config {
            session_name: Some(format!("{model}/{name}")),
            ..Config::default()
        });
        kernel.set_provider(provider.clone());
        kernel.set_params(params.clone());

        Ok(Subject::new(kernel))
    };

    // one experiment at a time, and printed as it lands. `evaluate_with` prints nothing - a
    // library has no business writing to somebody's terminal - and a run of a hundred requests
    // that shows nothing until the last one is a run nobody can tell from a hung one.
    //
    // note: the concurrency is kept *inside* an experiment rather than across them, which is
    // where nearly all of it is anyway: `attribution` ablates every note in every dossier, and
    // that sweep is most of the requests a whole suite makes. Fanning the experiments out as well
    // would finish sooner and would print nothing until it did, which is the trade this loop
    // exists to refuse.
    // note: on by default, and named after the model and the hour if nobody said where. A run is
    // hours long and the console output is a summary - the scores, not the questions and answers
    // they were computed from - so a run whose terminal is closed used to leave nothing that
    // could be scored again. Opting out is `--no-json`, which is the rarer thing to want.
    let started = SystemTime::now();
    let json = match (json, no_json) {
        (_, true) => None,
        (Some(path), _) => Some(path),
        (None, _) => Some(default_path(&model, &started)),
    };
    if let Some(path) = &json {
        println!("the record of this run is being written to {path}, after every experiment\n");
    }
    // the accumulator is the report itself rather than a `Vec` turned into one at the end, so
    // that what gets written after each experiment is the same value, and writing it costs no
    // copy of everything that has landed so far
    let mut report = Report {
        at: started
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or_default(),
        outcomes: Vec::new(),
    };

    for experiment in experiments {
        let one = evaluate_with([experiment], subject, pace).await;
        for outcome in &one.outcomes {
            println!("{outcome}\n");
        }
        report.outcomes.extend(one.outcomes);

        // written after every experiment rather than once at the end. A suite is hours long and
        // any of it can hang - measured, one experiment against a free endpoint sat on a single
        // probe for over an hour - and a run that is killed at that point used to leave nothing
        // at all, however many experiments had already finished.
        if let Some(path) = &json
            && let Err(e) = checkpoint(path, &report)
        {
            // warned rather than returned: losing the checkpoint is a reason to say so loudly,
            // not a reason to throw away the hours of run still to come
            eprintln!("warning: could not write {path}: {e}");
        }
    }

    println!("{}", summary(&report));
    print_curve(&report);

    if let Some(path) = json {
        // the last checkpoint already holds this, and it is written again anyway: the loop's
        // warning is not fatal, so the only way to know the file on disk is complete is to write
        // it once more where a failure *is*
        checkpoint(&path, &report)?;
        println!("\nthe whole record, every question and every answer, is in {path}");
    }

    Ok(())
}

/// Where a run writes itself when nobody said.
///
/// note: the model in the name, because a directory of `report.json` tells nobody which model any
/// of them measured; the time, because the same model is measured more than once and a second run
/// silently overwriting the first is how a day's work disappears. Seconds since the epoch rather
/// than anything friendlier, to keep this free of a date-formatting dependency.
fn default_path(model: &str, started: &SystemTime) -> String {
    let model = model
        .chars()
        .map(|c| match c.is_ascii_alphanumeric() {
            true => c,
            false => '-',
        })
        .collect::<String>();
    let when = started
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();

    format!("bench-{model}-{when}.json")
}

/// Writes the report to `path`, atomically.
///
/// note: written beside the target and renamed onto it, rather than written over it. A rename
/// within a directory is atomic, so a reader - or a run killed mid-write - sees either the whole
/// previous checkpoint or the whole new one, and never the half of one that had been flushed when
/// the process died. Writing in place would make every checkpoint a window in which the file on
/// disk is truncated JSON, which is a worse record than the one this exists to replace.
///
/// note: the temporary is in the same directory, because a rename across filesystems is not a
/// rename. `/tmp` would be the usual place for it and would silently turn this back into a
/// copy-and-truncate on anyone whose working directory is a different mount.
fn checkpoint(path: &str, report: &Report) -> Result<(), nachalnik::BoxError> {
    let partial = format!("{path}.partial");
    std::fs::write(&partial, serde_json::to_string_pretty(report)?)?;
    std::fs::rename(&partial, path)?;

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
