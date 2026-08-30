//! A recorded session, run headless so the whole exchange can be read afterwards.
//!
//! Not part of the crate's story - a scratch harness for watching the introspection tools work
//! against a real model, with a context limit small enough that managing it is not optional.
//!
//! ```console
//! KAMCHATKA_API_KEY=... KAMCHATKA_CONTEXT_LIMIT=10000 \
//!   cargo run -p kamchatka --example recorded
//! ```
//!
//! `TASK` sets the question and `OUT` the directory the recording is written to.

use std::{fs, io::Write, path::PathBuf, sync::Arc, time::Duration};

use kamchatka::{
    gemini, introspect,
    provider::{self, Endpoint},
    sandbox, tools,
};
use nachalnik::{
    Block, Capability, Config, Content, ContextItem, ContextKind, Delta, Event, Grant, Kernel,
    LinearProjector, State, Verdict,
};

/// What it is being asked to work out.
fn task() -> String {
    std::env::var("TASK").unwrap_or_else(|_| {
        "Which file in this repository is the longest, and how many lines is it? \
         Sanity-check your answer before you give it."
            .to_owned()
    })
}

const BRIEF: &str = "You are working in a Rust workspace on this machine, through a shell.

You have a hard context budget of 10,000 tokens for this whole task, and tool output here is \
large: one careless command will spend most of it. Two tools let you do something about that.

`introspect` reads your own state: `budget` says where you stand and which items are costing you \
the most, `look` lists what you are carrying, `draft` shows you your own answer before you give \
it, and `fork` asks a copy of you a question without spending your context on the reply.

`amend` manages it: `prune` with `elide` replaces an item you are done with by a short marker and \
gives you its tokens back, `select` names a whole class of them at once, and `note` writes \
something down where compaction cannot reach it.

Check your budget before and after anything expensive. Answer only when you are sure.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // this binary is its own confiner, the way the program is: the shell tool re-executes it with
    // `--confine-and-run` and it restricts itself before exec'ing the command
    if let Some(code) = sandbox::run_if_asked() {
        std::process::exit(code);
    }

    // the budget is the whole point, and the provider already reads it from here - so it is
    // required rather than set, which keeps this example free of the one `unsafe` that setting an
    // environment variable now costs
    let budget: usize = std::env::var("KAMCHATKA_CONTEXT_LIMIT")
        .ok()
        .and_then(|limit| limit.parse().ok())
        .ok_or("set KAMCHATKA_CONTEXT_LIMIT (10000 is what the recorded sessions used)")?;

    // two dialects, one trait. `DIALECT=openai` points this at anything OpenAI-compatible -
    // OpenRouter, ollama, a proxy - and the only thing downstream that changes is whether the
    // projector is asked for ordered blocks, because that shape is Google's and not theirs
    let ordered = std::env::var("DIALECT").as_deref() != Ok("openai");
    let model = std::env::var("KAMCHATKA_MODEL").unwrap_or_else(|_| {
        match ordered {
            true => "gemini-3.6-flash",
            false => "liquid/lfm-2.5-2.6b:free",
        }
        .to_owned()
    });
    let provider: Arc<dyn Endpoint> = match ordered {
        true => gemini::connect(&model)
            .await
            .map(|it| it as Arc<dyn Endpoint>),
        false => provider::connect(&model)
            .await
            .map(|it| it as Arc<dyn Endpoint>),
    }
    .map_err(|e| format!("could not reach {model}: {e}"))?;

    let kernel = Kernel::new(Config {
        max_requests_per_turn: Some(40),
        ..Default::default()
    });
    let mut events = kernel.subscribe();
    kernel.set_provider(provider.clone());
    if ordered {
        kernel.set_projector(Arc::new(LinearProjector {
            send_blocks: true,
            ..Default::default()
        }));
    }

    // no compactor: what is interesting is whether it manages the budget itself
    let policy = Arc::new(tools::Careful::new());
    for capability in [
        Capability::Read,
        Capability::Shell,
        Capability::Custom("introspect".into()),
        Capability::Custom("amend".into()),
    ] {
        policy.set(&tools::Subject::Capability(capability), Verdict::Allow);
    }
    // refused outright rather than left to be asked about, and it reaches the shell: `Sandbox::of`
    // leaves the working directory writable for anything short of a refusal, and a recorded demo
    // is no reason to let a model edit the repository it is reading
    for capability in [Capability::Write, Capability::Edit] {
        policy.set(&tools::Subject::Capability(capability), Verdict::Deny);
    }
    kernel.set_policy(policy.clone());

    let program = std::env::current_exe()?;
    let confinement = sandbox::available(&program);
    let reach = sandbox::Reach {
        workdir: std::env::current_dir()?,
        extra: Vec::new(),
        confined: true,
    };
    for tool in tools::builtin(
        tools::Shell {
            policy: policy.clone(),
            workdir: reach.workdir.clone(),
            extra: reach.extra.clone(),
            confiner: confinement.is_confined().then(|| program.clone()),
        },
        reach,
    ) {
        kernel.add_tool(tool);
    }
    let _introspect = introspect::install(&kernel);
    eprintln!("shell: {confinement}, model: {model}, budget: {budget}, ordered: {ordered}");

    let brief = std::env::var("BRIEF").unwrap_or_else(|_| BRIEF.to_owned());
    kernel.push(ContextItem::system(brief).pinned());
    // `PLANT=label::content` puts something in the context before the question is asked, which is
    // how a run about *inspecting* a context gets one worth inspecting
    if let Ok(planted) = std::env::var("PLANT") {
        for item in planted.split("||") {
            let (label, content) = item.split_once("::").unwrap_or(("notes", item));
            kernel.push(
                ContextItem::memory(label, content.to_owned())
                    .because("carried over from an earlier session"),
            );
        }
    }
    kernel.push(ContextItem::user(task()));

    // the loop, with nobody at the terminal: anything the policy still wants asked about is
    // granted once and recorded as such
    let started = std::time::Instant::now();
    let mut follow_up = std::env::var("TASK2").ok();
    loop {
        match kernel.turn().await? {
            State::Deciding { .. } => {
                for pending in kernel.pending_permissions() {
                    kernel.decide(pending.id, Grant::Allow)?;
                }
            }
            // a second question, asked once the first is answered, so a session can be about
            // something that only exists after a turn has happened
            State::Finished { .. } | State::Idle => match follow_up.take() {
                Some(next) => {
                    kernel.push(ContextItem::user(next));
                    continue;
                }
                None => break,
            },
            other => {
                eprintln!("stopped at {other}");
                break;
            }
        }
        if started.elapsed() > Duration::from_secs(600) {
            eprintln!("out of time");
            break;
        }
    }

    write(&kernel, budget, &mut events, started.elapsed())
}

/// Writes the exchange out three ways: to read, to check, and to replay.
fn write(
    kernel: &Kernel,
    budget: usize,
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    took: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut deltas = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let Event::ModelDelta { delta } = event {
            deltas.push(delta);
        }
    }

    let out = PathBuf::from(std::env::var("OUT").unwrap_or_else(|_| "recorded".into()));
    fs::create_dir_all(&out)?;

    // the readable one
    let mut script = String::new();
    script.push_str(&format!(
        "# a recorded session\n\n- model: {}\n- context limit: {budget} tokens\n- took: {:.0}s, \
         {} requests\n- ended at ~{} tokens of {budget}\n\n---\n\n",
        kernel.model_info().map(|i| i.model).unwrap_or_default(),
        took.as_secs_f64(),
        kernel
            .history()
            .iter()
            .filter(|r| matches!(r.event, Event::ModelRequested { .. }))
            .count(),
        kernel.budget().used(),
    ));

    for item in kernel.items() {
        let head = format!(
            "### [{}] {} · {} · {} tokens{}\n\n",
            item.id,
            item.kind.name(),
            item.state,
            item.tokens,
            item.note
                .as_deref()
                .map(|note| format!(" · {note}"))
                .unwrap_or_default(),
        );
        script.push_str(&head);

        match item.content.as_blocks() {
            Some(blocks) => {
                for block in blocks {
                    match block {
                        Block::Call(call) => script.push_str(&format!(
                            "**calls** `{}({})`\n\n",
                            call.tool,
                            trim(&call.args.to_string(), 600)
                        )),
                        _ => {
                            let said = block
                                .part()
                                .map(|part| part.content.to_text().into_owned())
                                .unwrap_or_default();
                            script.push_str(&format!(
                                "**{}**\n\n{}\n\n",
                                block.name(),
                                trim(&said, 4_000)
                            ));
                        }
                    }
                }
            }
            None => {
                let said = item.content.to_text();
                script.push_str(&format!("```\n{}\n```\n\n", trim(&said, 4_000)));
            }
        }

        if let ContextKind::ToolResult { tool, is_error, .. } = &item.kind {
            script.push_str(&format!(
                "*(from `{tool}`{})*\n\n",
                match is_error {
                    true => ", an error",
                    false => "",
                }
            ));
        }
    }

    fs::write(out.join("session.md"), &script)?;

    // the trace, and a snapshot to resume from
    let mut log = fs::File::create(out.join("events.jsonl"))?;
    for record in kernel.history() {
        writeln!(log, "{}", serde_json::to_string(&record)?)?;
    }
    fs::write(
        out.join("snapshot.json"),
        serde_json::to_string_pretty(&kernel.snapshot())?,
    )?;
    fs::write(
        out.join("streamed.txt"),
        deltas
            .iter()
            .map(|delta| match delta {
                Delta::Text(text) => text.clone(),
                Delta::Reasoning(text) => format!("[thinking] {text}"),
                _ => String::new(),
            })
            .collect::<String>(),
    )?;

    eprintln!(
        "wrote {}/session.md ({} items, {} events)",
        out.display(),
        kernel.items().len(),
        kernel.history().len()
    );

    Ok(())
}

/// The first `limit` bytes of something, on a character boundary.
fn trim(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }

    format!(
        "{}\n[... {} more bytes ...]",
        &text[..cut],
        text.len() - cut
    )
}

/// Silences the unused-import warning for `Content` when the shape of this changes.
#[allow(dead_code)]
fn _unused(_: Content) {}
