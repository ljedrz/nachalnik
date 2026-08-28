//! Ask several models the same thing at once, and prove that it *was* the same thing.
//!
//! ```text
//! export NACHALNIK_API_KEY=...
//! cargo run --example compare -- -m openai/gpt-4o-mini -m qwen/qwen3-coder "is this sound?"
//! ```
//!
//! Local models work as well as hosted ones, and are the cheapest way to try this:
//!
//! ```text
//! NACHALNIK_API_KEY=ollama NACHALNIK_BASE_URL=http://localhost:11434/v1 \
//!   cargo run --example compare -- -m llama3.2 -m granite4.2:3b "is this sound?"
//! ```
//!
//! A comparison is only worth anything if the only thing that differed was the model, and that
//! is normally taken on trust: the harness assembles a prompt somewhere inside itself, per
//! model, and you get the answers. Here each model gets a [`Kernel`] of its own, the *same*
//! [`ContextItem`]s are pushed into every one of them, and the tool prints the fingerprint of
//! the exact request each is about to receive. When they match, they match byte for byte, and
//! you can see that they do before anything is sent.
//!
//! ```text
//! compare [-m MODEL].. [-f FILE].. [-s SYSTEM] [--seq] [--payload] [--save DIR] [-i] [prompt...]
//! ```
//!
//! With no prompt on the command line it asks for one, and keeps asking - which makes the second
//! round the interesting one, because from then on each context also holds that model's own
//! answers, and the tool says so rather than quietly comparing different things.

use std::{
    collections::HashMap,
    env,
    io::{Write, stdin, stdout},
    sync::Arc,
    time::{Duration, Instant},
};

use nachalnik::{
    BoxError, Config, ContextItem, Kernel, ModelResponse, StopReason, Usage, selectors::Selector,
};

// the OpenAI-compatible HTTP provider, shared with the `panel` example
#[path = "common/mod.rs"]
mod common;

use common::{thousands, wrap};

const USAGE: &str = "\
usage: compare [-m MODEL].. [-f FILE].. [-s SYSTEM] [options] [prompt...]

  -m, --model MODEL    a model to ask; repeat it, or set NACHALNIK_MODELS=a,b,c
  -f, --file FILE      put a file in every context, pinned
  -s, --system TEXT    a system instruction for every model
  -i, --interactive    keep asking after the first prompt
      --seq            ask one model at a time instead of all at once
      --payload        print the exact payload each provider will send
      --save DIR       write every session's log and snapshot there

environment:
  NACHALNIK_API_KEY / OPENROUTER_API_KEY / OPENAI_API_KEY
  NACHALNIK_BASE_URL   e.g. https://generativelanguage.googleapis.com/v1beta/openai
                       or   http://localhost:11434/v1  (ollama; any key will do)";

/// One model, and the session it is having.
struct Contender {
    model: String,
    kernel: Kernel,
}

/// What one model did with one prompt.
struct Answer {
    elapsed: Duration,
    outcome: Result<Arc<ModelResponse>, String>,
}

/// A 64-bit FNV-1a digest of a request.
///
/// note: This is here so that "they were sent the same thing" can be *checked* rather than
/// asserted. It is not a cryptographic hash and does not need to be - nobody is attacking their
/// own prompt - it only has to notice a difference, and the bytes it runs over are the serialized
/// messages themselves.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }

    hash
}

/// Complains and stops, the way a command-line tool should.
fn bail(message: &str) -> ! {
    eprintln!("{message}\n\n{USAGE}");
    std::process::exit(2);
}

fn stop_name(stop: &StopReason) -> String {
    match stop {
        StopReason::EndTurn => "end_turn".to_owned(),
        StopReason::ToolUse => "tool_use".to_owned(),
        StopReason::Length => "length".to_owned(),
        StopReason::Refusal => "refusal".to_owned(),
        StopReason::Other(other) => other.clone(),
        // `StopReason` is `#[non_exhaustive]`, so a new one arriving in a later version is a
        // recompile rather than a break
        other => format!("{other:?}"),
    }
}

fn count(n: Option<u64>) -> String {
    n.map(thousands).unwrap_or_else(|| "-".to_owned())
}

fn heading(text: &str) {
    println!("\n\x1b[1m{text}\x1b[0m");
}

fn prompt(text: &str) -> Option<String> {
    print!("{text}");
    let _ = stdout().flush();

    let mut line = String::new();
    match stdin().read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => Some(line.trim().to_owned()),
        Err(_) => None,
    }
}

/// Reports what each model is about to be sent, and whether it is the same thing.
///
/// note: The claim is checked twice over, and neither check is the runtime's word for it: the
/// fingerprint runs over the serialized messages of [`Kernel::preview_request`], which is the
/// projection the next request is built from, and the second one runs over only the items the
/// user put there. The first stops matching as soon as the models have said anything, because by
/// then the contexts genuinely differ - and that is worth being told rather than hidden.
fn report_inputs(contenders: &[Contender], payloads: bool) -> Result<(), BoxError> {
    heading("INPUTS · what each model is about to be sent");
    println!(
        "\n  {:<30}{:>6}{:>10}{:>14}   REQUEST",
        "MODEL", "MSGS", "~TOKENS", "LIMIT"
    );

    let mut requests = Vec::new();
    let mut common = Vec::new();
    for contender in contenders {
        let request = contender.kernel.preview_request()?;
        let budget = contender.kernel.budget();

        // the items the user put there, which stay identical however the answers diverge
        let mine: Vec<_> = contender
            .kernel
            .items()
            .into_iter()
            .filter(|item| item.is_projected() && item.source != "model")
            .map(|item| (item.label.clone(), item.content.to_text().into_owned()))
            .collect();

        println!(
            "  {:<30}{:>6}{:>10}{:>14}   {:016x}",
            contender.model,
            request.messages.len(),
            thousands(budget.context_tokens),
            budget
                .limit
                .map(thousands)
                .unwrap_or_else(|| "?".to_owned()),
            fingerprint(&serde_json::to_vec(&request.messages)?),
        );

        requests.push(fingerprint(&serde_json::to_vec(&request.messages)?));
        common.push(fingerprint(&serde_json::to_vec(&mine)?));
    }

    let same_request = requests.windows(2).all(|pair| pair[0] == pair[1]);
    let same_items = common.windows(2).all(|pair| pair[0] == pair[1]);
    println!();
    match (same_request, same_items) {
        (true, _) => println!("  identical: every model is sent the same request, byte for byte."),
        (false, true) => println!(
            "  diverged: each context now also holds that model's own answers. What the user\n  \
             put there is still identical in all of them ({:016x}).",
            common[0]
        ),
        (false, false) => println!(
            "  \x1b[31mnot comparable: the contexts differ in what the user put there.\x1b[0m"
        ),
    }

    if payloads {
        for contender in contenders {
            if let Some(payload) = contender.kernel.preview_payload()? {
                let label = format!("── {} · payload ", contender.model);
                println!(
                    "\n  {label}{}",
                    "─".repeat(76_usize.saturating_sub(label.chars().count()))
                );
                println!("{}", serde_json::to_string_pretty(&payload)?);
            }
        }
    }

    Ok(())
}

/// Asks every model, either all at once or one at a time.
async fn ask(contenders: &[Contender], sequential: bool) -> Vec<Answer> {
    async fn one(kernel: Kernel) -> Answer {
        let started = Instant::now();
        let outcome = match kernel.turn().await {
            Ok(_) => match kernel.last_response() {
                Some(response) => Ok(response),
                None => Err("the turn ended without a response".to_owned()),
            },
            Err(e) => Err(e.to_string()),
        };

        Answer {
            elapsed: started.elapsed(),
            outcome,
        }
    }

    if sequential {
        let mut answers = Vec::with_capacity(contenders.len());
        for contender in contenders {
            answers.push(one(contender.kernel.clone()).await);
        }

        return answers;
    }

    // one task per kernel: the kernels share nothing, so there is nothing to coordinate. Note
    // that this is the *example* spawning tasks, not the kernel - a kernel spawns none unless
    // `Config::parallel_tool_calls` says so
    let mut running = tokio::task::JoinSet::new();
    for (index, contender) in contenders.iter().enumerate() {
        let kernel = contender.kernel.clone();
        running.spawn(async move { (index, one(kernel).await) });
    }

    let mut answers: HashMap<usize, Answer> = HashMap::new();
    while let Some(Ok((index, answer))) = running.join_next().await {
        answers.insert(index, answer);
    }

    (0..contenders.len())
        .map(|index| {
            answers.remove(&index).unwrap_or(Answer {
                elapsed: Duration::ZERO,
                outcome: Err("the task did not finish".to_owned()),
            })
        })
        .collect()
}

/// Prints the table, then the answers in full.
///
/// note: `EST` is the kernel's own estimate, taken before the request went out, and `IN` is what
/// the provider then charged for. Showing both is the only way to find out how wrong a token
/// counter that does not have the model's tokenizer is - and the models disagree with each other
/// about the very same bytes, which is worth seeing once.
fn report(contenders: &[Contender], answers: &[Answer], estimates: &[usize], width: usize) {
    heading("ANSWERS");
    println!(
        "\n  {:<30}{:>8}{:>9}{:>9}{:>9}{:>9}   STOP",
        "MODEL", "TIME", "EST", "IN", "OUT", "THINK"
    );

    for ((contender, answer), estimate) in contenders.iter().zip(answers).zip(estimates) {
        let seconds = format!("{:.2}s", answer.elapsed.as_secs_f64());
        match &answer.outcome {
            Ok(response) => {
                let usage = response.usage.unwrap_or_default();
                println!(
                    "  {:<30}{seconds:>8}{:>9}{:>9}{:>9}{:>9}   {}",
                    contender.model,
                    thousands(*estimate),
                    count(usage.input_tokens),
                    count(usage.output_tokens),
                    count(usage.reasoning_tokens),
                    stop_name(&response.stop),
                );
            }
            Err(e) => println!(
                "  {:<30}{seconds:>8}{:>9}{:>9}{:>9}{:>9}   \x1b[31m{}\x1b[0m",
                contender.model,
                thousands(*estimate),
                "-",
                "-",
                "-",
                e.lines().next().unwrap_or_default(),
            ),
        }
    }

    let charged: Vec<_> = answers
        .iter()
        .filter_map(|a| a.outcome.as_ref().ok())
        .filter_map(|r| r.usage.and_then(|u: Usage| u.input_tokens))
        .collect();
    if charged.len() > 1 && charged.windows(2).any(|pair| pair[0] != pair[1]) {
        println!(
            "\n  note that the providers charged different numbers of input tokens for the same\n  \
             bytes: {}. Tokenizers differ, so a budget is an estimate whichever end it\n  \
             comes from.",
            charged
                .iter()
                .map(|n| thousands(*n))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    for (contender, answer) in contenders.iter().zip(answers) {
        println!(
            "\n\x1b[1m── {} {}\x1b[0m\n",
            contender.model,
            "─".repeat(width.saturating_sub(contender.model.chars().count() + 4))
        );
        match &answer.outcome {
            Ok(response) => {
                let text = response.content.clone().unwrap_or_default();
                println!("{}", wrap(&text.to_text(), width - 2, "  "));
            }
            Err(e) => println!("  \x1b[31m! {e}\x1b[0m"),
        }
    }
}

/// Writes every session where it can be read back: the log as it happened, the snapshot as it
/// ended up.
fn save(contenders: &[Contender], dir: &str) -> Result<(), BoxError> {
    std::fs::create_dir_all(dir)?;

    for contender in contenders {
        let slug: String = contender
            .model
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();

        let log: Vec<String> = contender
            .kernel
            .history()
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        std::fs::write(format!("{dir}/{slug}.jsonl"), log.join("\n"))?;
        std::fs::write(
            format!("{dir}/{slug}.json"),
            serde_json::to_vec_pretty(&contender.kernel.snapshot())?,
        )?;

        println!("  wrote {dir}/{slug}.jsonl and {dir}/{slug}.json");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let (mut models, mut files, mut system, mut save_to) = (Vec::new(), Vec::new(), None, None);
    let (mut sequential, mut payloads, mut interactive) = (false, false, false);
    let mut words = Vec::new();

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut next = |flag: &str| args.next().ok_or(format!("{flag} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-m" | "--model" => models.push(next("-m")?),
            "-f" | "--file" => files.push(next("-f")?),
            "-s" | "--system" => system = Some(next("-s")?),
            "--save" => save_to = Some(next("--save")?),
            "-i" | "--interactive" => interactive = true,
            "--seq" => sequential = true,
            "--payload" => payloads = true,
            _ => words.push(arg),
        }
    }

    let models = common::models(models);
    if models.len() < 2 {
        bail("name at least two models with -m");
    }

    // no tools are offered here, so one request is the whole turn; the cap is a backstop against
    // a model that invents a tool call anyway
    let config = Config {
        max_requests_per_turn: Some(2),
        ..Default::default()
    };

    let mut contenders = Vec::with_capacity(models.len());
    for provider in common::providers(&models).await? {
        let kernel = Kernel::new(Config {
            session_name: Some(provider.model()),
            ..config.clone()
        });
        kernel.set_provider(provider.clone());
        contenders.push(Contender {
            model: provider.model(),
            kernel,
        });
    }

    // the same items, pushed into every context. This is the whole trick, and it is worth
    // noticing that it is not a feature of the runtime: a context is a list the caller owns, so
    // "give them all the same one" is a `for` loop
    let mut shared = Vec::new();
    if let Some(text) = system {
        shared.push(ContextItem::instruction("system", text));
    }
    for path in &files {
        match std::fs::read_to_string(path) {
            Ok(content) => shared.push(
                ContextItem::file(path, content)
                    .because("the user named it on the command line")
                    .pinned(),
            ),
            Err(e) => eprintln!("could not read {path}: {e}"),
        }
    }
    for contender in &contenders {
        contender.kernel.push_all(shared.iter().cloned());
    }

    println!(
        "\x1b[1mnachalnik\x1b[0m · compare · {} models · {}",
        contenders.len(),
        common::base_url()
    );

    let mut pending = (!words.is_empty()).then(|| words.join(" "));
    if pending.is_none() {
        interactive = true;
    }

    loop {
        let line = match pending.take() {
            Some(line) => line,
            None => match prompt("\n\x1b[1m> \x1b[0m") {
                Some(line) if !line.is_empty() => line,
                _ => break,
            },
        };

        for contender in &contenders {
            contender.kernel.push(ContextItem::user(line.clone()));
        }

        report_inputs(&contenders, payloads)?;

        // taken before the request, because afterwards the context holds the answer too
        let estimates: Vec<usize> = contenders
            .iter()
            .map(|c| c.kernel.budget().context_tokens)
            .collect();

        let answers = ask(&contenders, sequential).await;
        report(&contenders, &answers, &estimates, 78);

        if !interactive {
            break;
        }
    }

    if let Some(dir) = save_to {
        heading("SAVED");
        save(&contenders, &dir)?;
        println!(
            "\n  each is a session of its own: `kamchatka -r {dir}/<model>.json` picks any of them up."
        );
    }

    // one selector, run against every context: `all:model` is every turn a model produced
    if let Ok(selector) = "all:model".parse::<Selector>() {
        let turns: usize = contenders
            .iter()
            .map(|c| selector.matches(&c.kernel.items()).len())
            .sum();
        let records: usize = contenders.iter().map(|c| c.kernel.history().len()).sum();
        println!(
            "\n{turns} answers across {} sessions, {records} events recorded",
            models.len()
        );
    }

    for contender in &contenders {
        contender.kernel.finish();
    }

    Ok(())
}
