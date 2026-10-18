//! Runs the suite against a real model and prints what it found.
//!
//! ```text
//! export NACHALNIK_API_KEY=...
//! cargo run -p nachalnik-eval --example bench -- -m google/gemini-3.5-flash -r 2 --json out.json
//! ```
//!
//! A local model works too, and costs nothing:
//!
//! ```text
//! NACHALNIK_API_KEY=ollama NACHALNIK_BASE_URL=http://localhost:11434/v1 \
//!   cargo run -p nachalnik-eval --example bench -- -m granite4.2:3b
//! ```
//!
//! note: The provider is not in the crate, and cannot be: `nachalnik-eval` ships no HTTP client
//! for the same reason `nachalnik` does not. This example talks through `nachalnik-providers`'
//! OpenAI-compatible provider, built from the environment by `nachalnik-utils`, and both are
//! dev-dependencies. Any [`Provider`] does, which is what makes the suite model-agnostic.
//!
//! note: `--temperature 0` is the default and does not make anything deterministic. It narrows
//! the sampling and nothing more, which is why replicates exist. At `-r 1` the instability
//! reported is zero because nothing was measured, not because the answers were stable.

use std::{
    env,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use nachalnik::{Config, Kernel, Params, Provider};
use nachalnik_eval::{Outcome, Pace, Report, Subject, evaluate_with, suite};
use nachalnik_utils::base_url;
use serde_json::json;

const USAGE: &str = "\
usage: bench [-m MODEL] [-r N] [-l N] [-j N] [--per-minute N] [--swap] [-e NAME]..
             [--temperature T] [--max-tokens N] [--json FILE | --no-json]

  -m, --model MODEL      the model to measure; or NACHALNIK_TEST_MODEL
  -r, --replicates N     how many copies each condition gets (default 1)
  -l, --ladders N        how many times `repair` runs its ladder per dossier
  -j, --at-once N        how many requests may be in flight at once (default 1)
      --per-minute N     and how many may be started in any sixty seconds (default none)
      --swap             run only `privilege`, with its two dossiers the other way round
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
  NACHALNIK_APP_TITLE    apps; no claim is made unless both are set";

/// A small, free, widely available model.
const DEFAULT_MODEL: &str = "google/gemini-3.5-flash-lite";

/// The output budget every request is sent with.
///
/// note: generous on purpose, and it costs nothing to be: a cap is a ceiling rather than a
/// reservation, so a model that answers in forty tokens is billed for forty. What it buys is that
/// a reasoning model cannot quietly truncate mid-thought and return an answer with no last line
/// for the reading to find.
///
/// note: the figure has to clear the *thinking*, not the answer, and on a reasoning model thinking
/// is where nearly all the tokens go. A cap a quarter of this size is enough for one to spend on a
/// single question and return an empty message.
const MAX_TOKENS: u32 = 32_768;

#[tokio::main]
async fn main() -> Result<(), nachalnik::BoxError> {
    let mut model = nachalnik_utils::test_model(DEFAULT_MODEL);
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
    // note: set rather than left to the provider, because a reasoning model that runs out of output
    // budget mid-thought returns no content: `finish_reason: length` and a null message. Every
    // probe is read off the last line of an answer, so an empty one arrives as `Answer::Unreadable`
    // and is scored as untested. The failure is quiet: a report of untested claims with no
    // indication that a token cap caused it.
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

    // note: one `Pace` for the whole run, because a rate is only obeyed if its window is shared. A
    // limit of twenty a minute applied afresh per experiment is nine times the limit. The window is
    // built inside `evaluate_with`, so at `-j 1` each experiment starts a fresh window and the
    // boundary between two can see up to twice the rate.
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

    // note: on by default, and named after the model and start time unless a path is given. A run
    // is hours long and the console output is a summary: the scores, not the questions and answers.
    // Without the file, a run whose terminal is closed leaves nothing that can be scored again. Opt
    // out with `--no-json`.
    let started = SystemTime::now();
    let json = match (json, no_json) {
        (_, true) => None,
        (Some(path), _) => Some(path),
        (None, _) => Some(default_path(&model, &started)),
    };
    if let Some(path) = &json {
        println!("the record of this run is being written to {path}, after every experiment\n");
    }
    let at = started
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default();

    // every finished experiment, in the order it finished. A checkpoint is written from this. The
    // report at the end is the ordered one; this is only the record that survives a killed run.
    //
    // note: printing is this example's job, not the library's. `evaluate_with` prints nothing,
    // because a library should not write to somebody's terminal. `tell` lets a run that fans its
    // experiments out show progress as they finish; a run of hundreds of requests that shows
    // nothing until the last one cannot be told from a hung one
    let landed: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());
    let tell = |outcome: &Outcome| {
        println!("{outcome}\n");

        let mut landed = landed
            .lock()
            .expect("nothing here panics while holding this");
        landed.push(outcome.clone());

        // written after each experiment finishes, not once at the end. A suite runs for hours and
        // any part can hang. If a run is killed, it keeps every experiment that already finished.
        if let Some(path) = &json {
            let so_far = Report {
                at,
                outcomes: landed.clone(),
            };
            // warned rather than returned: losing a checkpoint is a reason to say so loudly, not to
            // throw away the hours of run still to come
            if let Err(e) = checkpoint(path, &so_far) {
                eprintln!("warning: could not write {path}: {e}");
            }
        }
    };

    // note: fanned out only when the pace leaves room for it. At one request in flight there is
    // no room by definition, and starting them all anyway would interleave every session through
    // a single-file queue - the same total time, but every experiment finishing near the end
    // instead of one after another, which is the progress a long run is read by and the partial
    // record a killed one is left with. Above one, they overlap and the ceiling decides how much.
    let report = match at_once {
        1 => {
            let mut report = Report {
                at,
                outcomes: Vec::new(),
            };
            for experiment in experiments {
                let one = evaluate_with([experiment], subject, pace, &tell).await;
                report.outcomes.extend(one.outcomes);
            }
            report
        }
        _ => evaluate_with(experiments, subject, pace, &tell).await,
    };

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

/// Where a run writes itself when none was given.
///
/// note: the model is in the name because `report.json` does not say which model was measured. The
/// time is in the name because the same model is measured more than once, and a second run silently
/// overwriting the first loses a day's work. Seconds since the epoch are used to avoid a
/// date-formatting dependency.
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
/// note: it writes beside the target and renames onto it, rather than writing over it. A rename
/// within a directory is atomic, so a reader, or a run killed mid-write, sees either the whole
/// previous checkpoint or the whole new one, never a half-flushed one. Writing in place would leave
/// a window in which the file on disk is truncated JSON.
///
/// note: the temporary is in the same directory, because a rename across filesystems is not a
/// rename. `/tmp` would be the usual place for it, and for anyone whose working directory is on a
/// different mount every checkpoint would fail to land.
fn checkpoint(path: &str, report: &Report) -> Result<(), nachalnik::BoxError> {
    let partial = format!("{path}.partial");
    std::fs::write(&partial, serde_json::to_string_pretty(report)?)?;
    std::fs::rename(&partial, path)?;

    Ok(())
}

/// The last two lines of a [`Report`]'s rendering. The outcomes above were already printed one by
/// one as they landed.
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
/// note: printed rather than plotted, and printed with the counts, because with forty claims the
/// shape of the curve is mostly where the claims happened to land.
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
