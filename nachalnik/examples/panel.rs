//! Several models argue about one question, in rounds, and end with a ruling you can audit.
//!
//! ```text
//! export NACHALNIK_API_KEY=...
//! cargo run --example panel -- -m openai/gpt-4o-mini -m qwen/qwen3-coder "is this API sound?"
//! ```
//!
//! Or entirely on your own machine, which is also the honest way to find out how small a model
//! can be and still hold a position:
//!
//! ```text
//! NACHALNIK_API_KEY=ollama NACHALNIK_BASE_URL=http://localhost:11434/v1 \
//!   cargo run --example panel -- -m llama3.2 -m granite4.2:3b -r 2 "is this API sound?"
//! ```
//!
//! Round one is independent: nobody has read anybody. From round two on, every panelist gets the
//! others' positions as [`ContextItem`]s of its own - labelled, attributed, and counted - and
//! each new round *supersedes* the last one's, so a five-round panel carries one item per peer
//! rather than five. Every panelist also states a position through a tool each round, so what
//! comes out at the end is not a vibe but a tally, with the movement between rounds visible.
//!
//! ```text
//! panel [-m MODEL].. [-r ROUNDS] [-f FILE].. [--chair MODEL] [--words N] [--seq] [--save DIR] question...
//! ```
//!
//! What this is really showing is that "what did each participant know, and when?" is a question
//! with an exact answer here. `CONTEXTS`, at the end, prints it: every peer statement each model
//! read, in the order it read them, with the superseded ones still listed. A multi-model
//! evaluation whose inputs you cannot reconstruct is an anecdote.

use std::{
    collections::HashMap,
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use nachalnik::{
    BoxError, Config, ContextId, ContextItem, ContextKind, ContextState, Kernel, ModelResponse,
    OutputSink, PermissionPolicy, PermissionRequest, State, Tool, ToolCall, ToolOutput, ToolSpec,
    Verdict, async_trait,
};
use parking_lot::Mutex;
use serde_json::json;

// the OpenAI-compatible HTTP provider, shared with the `compare` example
#[path = "common/mod.rs"]
mod common;

use common::{thousands, wrap};

const WIDTH: usize = 78;

const USAGE: &str = "\
usage: panel [-m MODEL].. [-r ROUNDS] [options] question...

  -m, --model MODEL    a panelist; repeat it, or set NACHALNIK_MODELS=a,b,c
  -r, --rounds N       how many rounds (default 3; the first one is independent)
  -f, --file FILE      put a file in every panelist's context, pinned
      --chair MODEL    who writes the ruling (default: the first panelist)
      --words N        how long an answer may be, in words (default 120)
      --seq            one panelist at a time instead of all at once
      --save DIR       write every panelist's log and snapshot there

environment:
  NACHALNIK_API_KEY / OPENROUTER_API_KEY / OPENAI_API_KEY
  NACHALNIK_BASE_URL   e.g. https://generativelanguage.googleapis.com/v1beta/openai
                       or   http://localhost:11434/v1  (ollama; any key will do)";

/// Complains and stops, the way a command-line tool should.
fn bail(message: &str) -> ! {
    eprintln!("{message}\n\n{USAGE}");
    std::process::exit(2);
}

// ---------------------------------------------------------------------------------- the ballot

/// A position a panelist took in one round.
#[derive(Clone, Debug)]
struct Position {
    verdict: String,
    confidence: f64,
    because: String,
}

/// The panel's only tool: it records the caller's current position.
///
/// note: One instance per panelist, each closing over its own slot, which is why the tool never
/// has to ask who is calling it. A `Tool` is an object you construct, not a name in a registry.
struct Ballot {
    slot: Arc<Mutex<Option<Position>>>,
}

#[async_trait]
impl Tool for Ballot {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "state_position",
            "records your current position on the question; call it exactly once per round, \
             after you have said what you think",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "verdict": {
                    "type": "string",
                    "description": "your answer in as few words as possible, and in the same \
                                    words each round unless you have changed your mind, so that \
                                    it can be compared with the others",
                },
                "confidence": {
                    "type": "number",
                    "description": "how sure you are, from 0 to 1",
                },
                "because": {
                    "type": "string",
                    "description": "the one reason that matters most",
                },
            },
            "required": ["verdict", "confidence", "because"],
        }))
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let Some(verdict) = call.args["verdict"]
            .as_str()
            .filter(|v| !v.trim().is_empty())
        else {
            // the panelist reads this at the start of the next round, and the tally records an
            // abstention for this one; nothing is invented on its behalf
            return Ok(ToolOutput::error("`verdict` is required, and was missing"));
        };

        *self.slot.lock() = Some(Position {
            verdict: verdict.trim().to_owned(),
            confidence: call.args["confidence"].as_f64().unwrap_or(f64::NAN),
            because: call.args["because"].as_str().unwrap_or_default().to_owned(),
        });

        Ok(ToolOutput::new("position recorded"))
    }
}

/// The ballot needs nothing from the world, so it is allowed. Anything else would have to be
/// asked about, and there is nobody at the terminal to ask.
struct AllowTheBallot;

#[async_trait]
impl PermissionPolicy for AllowTheBallot {
    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        match request.capabilities.is_empty() {
            true => Verdict::Allow,
            false => Verdict::Deny,
        }
    }
}

// --------------------------------------------------------------------------------- the panelist

/// What one panelist did in one round.
#[derive(Default)]
struct Turn {
    said: Option<String>,
    position: Option<Position>,
    trouble: Option<String>,
}

/// One model, its session, and everything it has said.
struct Panelist {
    model: String,
    kernel: Kernel,
    slot: Arc<Mutex<Option<Position>>>,
    rounds: Mutex<Vec<Turn>>,
    /// The item currently carrying each peer's opinion, so the next round can supersede it.
    peers: Mutex<HashMap<String, ContextId>>,
}

impl Panelist {
    /// Takes one turn: one request, in which the model answers and casts its ballot.
    ///
    /// note: `step` rather than `turn`, because a turn would go back to the model with the
    /// ballot's result, and there is nothing for it to do with that. The tool result stays in the
    /// context and the next round's request carries it, which is exactly right - the panelist can
    /// see that its ballot was accepted, or that it was rejected for want of a verdict.
    async fn speak(&self) -> Turn {
        let state = match self.kernel.step().await {
            Ok(state) => state,
            Err(e) => {
                return Turn {
                    trouble: Some(e.to_string()),
                    ..Turn::default()
                };
            }
        };

        let said = self
            .kernel
            .last_response()
            .and_then(|r: Arc<ModelResponse>| r.content.clone())
            .map(|c| c.to_text().trim().to_owned())
            .filter(|text| !text.is_empty());

        let mut trouble = None;
        match state {
            State::Ready { .. } => {
                if let Err(e) = self.kernel.step().await {
                    trouble = Some(e.to_string());
                }
            }
            State::Deciding { .. } => {
                self.kernel
                    .cancel_pending_calls("the panel allows only the ballot");
                trouble = Some("it asked for a tool the panel does not allow".to_owned());
            }
            _ => {}
        }

        Turn {
            said,
            position: self.slot.lock().take(),
            trouble,
        }
    }

    /// What the others should be shown about a round: the prose, plus where it left the panelist
    /// standing. Rounds are numbered from one, the way they are printed.
    fn opinion(&self, round: usize) -> Option<String> {
        let rounds = self.rounds.lock();
        let turn = rounds.get(round.checked_sub(1)?)?;

        match (&turn.said, &turn.position) {
            (Some(said), Some(p)) => Some(format!(
                "{said}\n\n(position: {} - confidence {})",
                p.verdict,
                confidence(p.confidence)
            )),
            (Some(said), None) => Some(said.clone()),
            (None, Some(p)) => Some(format!("(position: {} - {})", p.verdict, p.because)),
            (None, None) => None,
        }
    }

    /// The position it last managed to state, if it ever did.
    fn latest(&self) -> Option<Position> {
        self.rounds
            .lock()
            .iter()
            .rev()
            .find_map(|turn| turn.position.clone())
    }
}

fn confidence(value: f64) -> String {
    match value.is_finite() {
        true => format!("{:.0}%", value.clamp(0.0, 1.0) * 100.0),
        false => "unstated".to_owned(),
    }
}

/// The first line of an error, for a table cell.
fn brief(trouble: &str) -> String {
    trouble.lines().next().unwrap_or_default().to_owned()
}

// -------------------------------------------------------------------------------- the reporting

fn heading(text: &str) {
    println!("\n\x1b[1m{text}\x1b[0m");
}

/// Where every panelist stood in every round, side by side, so that movement is visible.
fn report_positions(panel: &[Panelist], rounds: usize) {
    heading("POSITIONS · where everyone stood, round by round");

    print!("\n  {:<26}", "PANELIST");
    for round in 1..=rounds {
        print!("{:<22}", format!("round {round}"));
    }
    println!();

    for panelist in panel {
        print!("  {:<26}", panelist.model);
        for round in 0..rounds {
            let cell = match panelist.rounds.lock().get(round) {
                Some(Turn {
                    position: Some(p), ..
                }) => format!(
                    "{} {}",
                    p.verdict.chars().take(15).collect::<String>(),
                    confidence(p.confidence)
                ),
                Some(Turn {
                    trouble: Some(_), ..
                }) => "(failed)".to_owned(),
                _ => "(no position)".to_owned(),
            };
            print!("{cell:<22}");
        }
        println!();
    }
}

/// Groups the final positions, keeping abstentions apart from disagreement.
///
/// note: The grouping is by exact text, deliberately: a tally that decides for itself that two
/// differently-worded verdicts meant the same thing is a tally that can be wrong without anyone
/// noticing. The ballot's schema asks for the same words each round for the same reason.
fn tally(panel: &[Panelist]) -> (Vec<(String, Vec<String>)>, Vec<String>) {
    let mut counted: Vec<(String, Vec<String>)> = Vec::new();
    let mut abstained = Vec::new();

    for panelist in panel {
        let Some(position) = panelist.latest() else {
            abstained.push(panelist.model.clone());
            continue;
        };
        let key = position.verdict.to_lowercase();

        match counted.iter_mut().find(|(v, _)| v.to_lowercase() == key) {
            Some((_, voters)) => voters.push(panelist.model.clone()),
            None => counted.push((position.verdict, vec![panelist.model.clone()])),
        }
    }
    counted.sort_by_key(|(_, voters)| std::cmp::Reverse(voters.len()));

    (counted, abstained)
}

/// Every context item each panelist read, with the superseded ones still in place.
fn report_contexts(panel: &[Panelist]) {
    heading("CONTEXTS · what each panelist actually read");

    for panelist in panel {
        let items = panelist.kernel.items();
        let budget = panelist.kernel.budget();
        let superseded = items
            .iter()
            .filter(|i| i.state == ContextState::Superseded)
            .count();

        println!(
            "\n  \x1b[1m{}\x1b[0m — {} items, {superseded} superseded, ~{} tokens",
            panelist.model,
            items.len(),
            thousands(budget.context_tokens),
        );

        for item in &items {
            let kind = match &item.kind {
                ContextKind::AssistantMessage { tool_calls, .. } if !tool_calls.is_empty() => {
                    "ballot"
                }
                ContextKind::AssistantMessage { .. } => "said",
                ContextKind::ToolResult { .. } => "recorded",
                _ => &item.source,
            };
            let note = match (&item.note, item.is_projected()) {
                (Some(note), _) => format!("  ({note})"),
                (None, false) => format!("  ({})", item.state),
                (None, true) => String::new(),
            };
            // `ContextId`'s `Display` writes straight to the formatter, so it is rendered into a
            // string before it is padded rather than being handed a width it would ignore
            println!(
                "    {:<5}{:<12}{:<32}{:>7}{note}",
                format!("[{}]", item.id),
                kind,
                item.label.chars().take(31).collect::<String>(),
                thousands(item.tokens),
            );
        }
    }
}

/// Writes every panelist's session where it can be read back.
fn save(panel: &[Panelist], dir: &str) -> Result<(), BoxError> {
    std::fs::create_dir_all(dir)?;

    for panelist in panel {
        let slug: String = panelist
            .model
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();

        let log: Vec<String> = panelist
            .kernel
            .history()
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        std::fs::write(format!("{dir}/{slug}.jsonl"), log.join("\n"))?;
        std::fs::write(
            format!("{dir}/{slug}.json"),
            serde_json::to_vec_pretty(&panelist.kernel.snapshot())?,
        )?;

        println!("  wrote {dir}/{slug}.jsonl and {dir}/{slug}.json");
    }

    Ok(())
}

// ------------------------------------------------------------------------------------- the run

/// Runs one round: everybody answers, nobody waits for anybody.
async fn round(panel: &Arc<Vec<Panelist>>, sequential: bool) -> Vec<Duration> {
    async fn one(panel: &[Panelist], index: usize) -> Duration {
        let started = Instant::now();
        let panelist = &panel[index];

        let turn = panelist.speak().await;
        panelist.rounds.lock().push(turn);

        started.elapsed()
    }

    if sequential {
        let mut times = Vec::with_capacity(panel.len());
        for index in 0..panel.len() {
            times.push(one(panel, index).await);
        }

        return times;
    }

    // one task per panelist: the kernels share nothing, so there is nothing to coordinate. Note
    // that this is the *example* spawning tasks, not the kernel - a kernel spawns none unless
    // `Config::parallel_tool_calls` says so
    let mut running = tokio::task::JoinSet::new();
    for index in 0..panel.len() {
        let panel = panel.clone();
        running.spawn(async move { (index, one(&panel, index).await) });
    }

    let mut times = vec![Duration::ZERO; panel.len()];
    while let Some(Ok((index, elapsed))) = running.join_next().await {
        times[index] = elapsed;
    }

    times
}

/// Gives every panelist the others' opinions from the round just finished, superseding the ones
/// they replace.
///
/// note: This is the point of the example. A peer's opinion is an item like any other - it has a
/// source, a label, a size and a state - and when it is replaced, the kernel is told that it was
/// replaced rather than being handed a longer list. The old one is still there, still listed,
/// still restorable; it is simply not in the next request. Without it, the last round of a
/// five-model panel would carry twenty stale opinions.
fn circulate(panel: &[Panelist], previous: usize) -> usize {
    let mut carried = 0;

    for (index, panelist) in panel.iter().enumerate() {
        for (peer_index, peer) in panel.iter().enumerate() {
            if peer_index == index {
                continue;
            }
            let Some(opinion) = peer.opinion(previous) else {
                continue;
            };

            let item = ContextItem::new(
                ContextKind::Reference,
                "panel",
                format!("{} · round {previous}", peer.model),
                opinion,
            )
            .because("what a peer said in the round before this one");

            let replacing = panelist.peers.lock().get(&peer.model).copied();
            let id = match replacing {
                Some(old) => panelist.kernel.supersede(old, item),
                None => Ok(panelist.kernel.push(item)),
            };
            if let Ok(id) = id {
                panelist.peers.lock().insert(peer.model.clone(), id);
                carried += 1;
            }
        }
    }

    carried
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let (mut models, mut files, mut chair, mut save_to) = (Vec::new(), Vec::new(), None, None);
    let (mut rounds, mut words, mut sequential) = (3_usize, 120_usize, false);
    let mut question = Vec::new();

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
            "-r" | "--rounds" => rounds = next("-r")?.parse()?,
            "--chair" => chair = Some(next("--chair")?),
            "--words" => words = next("--words")?.parse()?,
            "--save" => save_to = Some(next("--save")?),
            "--seq" => sequential = true,
            _ => question.push(arg),
        }
    }

    let models = common::models(models);
    if models.len() < 2 {
        bail("a panel needs at least two models");
    }
    if question.is_empty() {
        bail("give the panel something to rule on");
    }
    let question = question.join(" ");
    let rounds = rounds.max(1);

    // this is the example's opinion, not the runtime's: the crate ships no system prompt, so
    // every word the panel is told to obey is right here, in the open
    let rules = format!(
        "You are one of {} models on a panel answering a single question together.\n\
         Each round: say what you think in at most {words} words, then call `state_position` \
         exactly once with where you stand.\n\
         From round two you will be shown what the others said, attributed. Disagreeing is fine \
         and changing your mind is fine - say which one you are doing, and why.",
        models.len()
    );

    let mut panel = Vec::with_capacity(models.len());
    for provider in common::providers(&models).await? {
        let model = provider.model.lock().clone();
        let kernel = Kernel::new(Config {
            session_name: Some(model.clone()),
            ..Default::default()
        });
        let slot = Arc::new(Mutex::new(None));

        kernel.set_provider(provider.clone());
        kernel.set_policy(Arc::new(AllowTheBallot));
        kernel.add_tool(Arc::new(Ballot { slot: slot.clone() }));

        kernel.push(ContextItem::instruction("panel rules", rules.clone()).pinned());
        for path in &files {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    kernel.push(
                        ContextItem::file(path, content)
                            .because("the user named it on the command line")
                            .pinned(),
                    );
                }
                Err(e) => eprintln!("could not read {path}: {e}"),
            }
        }
        kernel.push(ContextItem::user(format!(
            "Round 1 of {rounds}. Nobody has read anybody yet. Answer, then call \
             `state_position`.\n\n{question}"
        )));

        panel.push(Panelist {
            model,
            kernel,
            slot,
            rounds: Mutex::new(Vec::new()),
            peers: Mutex::new(HashMap::new()),
        });
    }
    let panel = Arc::new(panel);

    println!(
        "\x1b[1mnachalnik\x1b[0m · panel · {} panelists · {rounds} rounds\n\n{}",
        panel.len(),
        wrap(&question, WIDTH - 2, "  ")
    );

    for number in 1..=rounds {
        let note = match number {
            1 => "independent - nobody has read anybody".to_owned(),
            _ => {
                let carried = circulate(&panel, number - 1);
                for panelist in panel.iter() {
                    panelist.kernel.push(ContextItem::user(format!(
                        "Round {number} of {rounds}. You have now read the others. Answer again: \
                         keep your position or change it, say which, and call `state_position`."
                    )));
                }
                format!("{carried} peer opinions circulated, superseding the last ones")
            }
        };

        heading(&format!("ROUND {number} · {note}"));
        println!();
        let times = round(&panel, sequential).await;

        for (panelist, elapsed) in panel.iter().zip(times) {
            let rounds = panelist.rounds.lock();
            let cell = match rounds.last() {
                Some(Turn {
                    trouble: Some(e), ..
                }) => format!("\x1b[31m! {}\x1b[0m", brief(e)),
                Some(Turn {
                    position: Some(p), ..
                }) => format!("{} ({})", p.verdict, confidence(p.confidence)),
                _ => "\x1b[31mno position recorded\x1b[0m".to_owned(),
            };
            println!(
                "  {:<26}{:>7.2}s   {cell}",
                panelist.model,
                elapsed.as_secs_f64()
            );
        }
    }

    report_positions(&panel, rounds);

    // the tally is arithmetic done here, out in the open, and the chair is asked to write the
    // ruling *given* it - rather than being asked to guess what everyone thought
    let (counted, abstained) = tally(&panel);
    heading("TALLY");
    println!();
    for (verdict, voters) in &counted {
        println!(
            "  {:>2} · {:<34}{}",
            voters.len(),
            verdict.chars().take(33).collect::<String>(),
            voters.join(", ")
        );
    }
    if !abstained.is_empty() {
        println!(
            "  {:>2} · {:<34}{}",
            abstained.len(),
            "(no position stated)",
            abstained.join(", ")
        );
    }

    let chair = chair
        .and_then(|wanted| panel.iter().find(|p| p.model == wanted))
        .unwrap_or(&panel[0]);
    let summary: Vec<String> = panel
        .iter()
        .map(|panelist| match panelist.latest() {
            Some(p) => format!(
                "{}: {} ({}) - {}",
                panelist.model,
                p.verdict,
                confidence(p.confidence),
                p.because
            ),
            None => format!("{}: stated no position at all", panelist.model),
        })
        .collect();

    chair.kernel.push(
        ContextItem::new(
            ContextKind::Reference,
            "panel",
            "final positions",
            summary.join("\n"),
        )
        .because("the tally the chair has to rule on"),
    );
    chair.kernel.push(ContextItem::user(
        "You are the chair. Write the panel's ruling in one paragraph: the collective answer, \
         and, where anyone dissented, what they dissented about. A panelist who stated no \
         position did not agree with you - say so rather than counting them in. Do not report \
         more agreement than there was. Do not call any tool.",
    ));

    heading(&format!("RULING · by {}", chair.model));
    println!();
    let ruling = chair.speak().await;
    match (&ruling.said, &ruling.trouble) {
        (Some(text), _) => println!("{}", wrap(text, WIDTH - 2, "  ")),
        (None, Some(e)) => println!("  \x1b[31m! {e}\x1b[0m"),
        (None, None) => println!("  \x1b[31mthe chair said nothing\x1b[0m"),
    }

    let split = match (counted.len(), abstained.len()) {
        (1, 0) => "unanimous".to_owned(),
        (1, n) => format!(
            "unanimous among the {} who stated a position; {n} did not ({})",
            counted[0].1.len(),
            abstained.join(", ")
        ),
        (_, _) => format!(
            "not unanimous; {} took the minority view",
            counted[1..]
                .iter()
                .flat_map(|(_, voters)| voters.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    println!(
        "\n  {} · {split}",
        counted
            .iter()
            .map(|(verdict, voters)| format!("{verdict} {}", voters.len()))
            .collect::<Vec<_>>()
            .join(" · ")
    );

    report_contexts(&panel);

    if let Some(dir) = save_to {
        heading("SAVED");
        save(&panel, &dir)?;
        println!(
            "\n  each is a session of its own: `kamchatka -r {dir}/<model>.json` carries any of them \
             on."
        );
    }

    for panelist in panel.iter() {
        panelist.kernel.finish();
    }

    Ok(())
}
