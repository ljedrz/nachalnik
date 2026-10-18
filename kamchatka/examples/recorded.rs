//! A recorded session, run headless so the whole exchange can be read afterwards.
//!
//! A scratch harness for watching the introspection tools work against a real model, with a
//! context limit small enough that managing it is not optional.
//!
//! ```console
//! KAMCHATKA_API_KEY=... KAMCHATKA_CONTEXT_LIMIT=10000 DIALECT=openai \
//!   KAMCHATKA_BASE_URL=https://openrouter.ai/api/v1 KAMCHATKA_MODEL=... \
//!   cargo run -p kamchatka --example recorded
//! ```
//!
//! `TASK` sets the question. `TASK2` adds a second line. `BRIEF` replaces the system instruction.
//! `PLANT` puts memories in the context before the question. `OUT` names the recording directory.
//! `INTROSPECT=off` removes the four introspection tools. `KAMCHATKA_MODEL` names the model; unlike
//! the program, this example reads the variable rather than taking `-m`.
//!
//! note: `DIALECT` defaults to Google's native dialect, which the ordered-blocks path wants and is
//! wrong for every other endpoint. Without it against an OpenAI-compatible base URL the first
//! request returns `404 Not Found`, recorded as `model.failed` in `events.jsonl` and reported as
//! `the last turn failed`, which names neither the endpoint nor the dialect. A non-Google base URL
//! needs `DIALECT=openai` beside it, as in the command above.

use std::{fs, io::Write, path::PathBuf, sync::Arc, time::Duration};

use kamchatka::{
    endpoint,
    headless::Headless,
    sandbox,
    tools::Subject,
    wiring::{Setup, Wired},
};
use nachalnik::{
    Block, Capability, ContextItem, ContextKind, Event, Grant, Kernel, LinearProjector,
};
use nachalnik_providers::Dialect;

/// What it is being asked to work out.
fn task() -> String {
    std::env::var("TASK").unwrap_or_else(|_| {
        "Which file in this repository is the longest, and how many lines is it? \
         Sanity-check your answer before you give it."
            .to_owned()
    })
}

/// The brief tells the model the budget it really has.
///
/// note: the budget is read from `KAMCHATKA_CONTEXT_LIMIT`, which is what the provider is held to.
/// A brief with a different number than the session limit tells the model one number and shows
/// another through its `budget` call.
fn brief(budget: usize) -> String {
    format!(
        "You are working in a Rust workspace on this machine, through a shell.

You have a hard context budget of {budget} tokens for this whole task, and tool output here is \
large: one careless command will spend most of it. Four tools let you do something about that.

`context` reads your own state and changes it: `budget` says where you stand and which items are \
costing you the most, `look` lists what you are carrying, `search` finds text anywhere in it \
without putting the item back, `elide` replaces an item you are done with by a short marker and \
gives you its tokens back - or a whole class of them at once, named with `select` - and `note` \
writes something down where compaction cannot reach it.

`fork` asks a copy of you something without spending your context on the reply: `draft` shows you \
your own answer before you give it, and `ask` puts a question of your own to the copy.

`log` reads the record kept beside your context - what was added, replaced or elided, and what \
was asked permission for. `setup` says what this session is running with: which model, which \
tools, and what the policy will refuse before you ask.

Check your budget before and after anything expensive. Answer only when you are sure."
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // the shell tool re-executes this binary with `--confine-and-run`, and it restricts itself
    // before exec'ing the command
    if let Some(code) = sandbox::run_if_asked() {
        std::process::exit(code);
    }

    // the provider reads `KAMCHATKA_CONTEXT_LIMIT`, so it is required rather than set. Setting an
    // environment variable costs one `unsafe` in edition 2024, which this example avoids
    let budget: usize = std::env::var("KAMCHATKA_CONTEXT_LIMIT")
        .ok()
        .and_then(|limit| limit.parse().ok())
        .ok_or(
            "KAMCHATKA_CONTEXT_LIMIT has to be a number of tokens: the context budget the model \
             is held to",
        )?;

    // `DIALECT=openai` points this at OpenAI-compatible endpoints such as OpenRouter, ollama, or a
    // proxy. The only downstream change is whether the projector is asked for ordered blocks,
    // because that shape is Google's
    let ordered = std::env::var("DIALECT").as_deref() != Ok("openai");
    let model = std::env::var("KAMCHATKA_MODEL").unwrap_or_else(|_| {
        match ordered {
            true => "gemini-3.6-flash",
            false => "liquid/lfm-2.5-2.6b:free",
        }
        .to_owned()
    });
    let provider: Arc<dyn Dialect> = match ordered {
        true => endpoint::gemini::connect(Some(model.as_str()))
            .await
            .map(|it| it as Arc<dyn Dialect>),
        false => endpoint::connect(Some(model.as_str()))
            .await
            .map(|it| it as Arc<dyn Dialect>),
    }
    .map_err(|e| format!("could not reach {model}: {e}"))?;

    // the control arm uses the same model on the same task with no way to see or change its
    // context: `INTROSPECT=off` removes the introspection tools
    let introspecting = std::env::var("INTROSPECT").as_deref() != Ok("off");
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = Setup {
        // forty requests, because managing a context takes turns of its own, and eight is the
        // program's number for a user to say `/continue`
        requests: Some(40),
        // no compactor: what is interesting is whether it manages the budget itself
        compact: None,
        // a ceiling of 200,000 tokens, because forty requests against a model in a loop is an
        // unwanted recording and a bill. The deadline bounds the time; this bounds what the time
        // can be spent on
        spend: Some(200_000),
        allow: [
            Capability::fs("read"),
            Capability::exec("run"),
            kamchatka::tools::domains::context("look"),
            kamchatka::tools::domains::log("read"),
            kamchatka::tools::domains::setup("permissions"),
            kamchatka::tools::domains::context("elide"),
        ]
        .into_iter()
        .map(Subject::Capability)
        .collect(),
        // Write and edit are refused outright rather than left to be asked about, and the refusal
        // reaches the shell: `Sandbox::of` leaves the working directory writable unless they are
        // refused, and a recorded demo is no reason to let a model edit the repository it is
        // reading
        deny: [Capability::fs("write"), Capability::fs("edit")]
            .into_iter()
            .map(Subject::Capability)
            .collect(),
        // the two that are about something other than this session, named rather than a flag
        // turned off: which tools a session offers is one list, and this is what the control
        // arm's looks like
        tools: (!introspecting).then(|| vec!["fs".to_owned(), "shell".to_owned()]),
        system: Some(std::env::var("BRIEF").unwrap_or_else(|_| brief(budget))),
        ..Default::default()
    }
    .wire(provider)?;

    if ordered {
        app.kernel.set_projector(Arc::new(LinearProjector {
            send_blocks: true,
            ..Default::default()
        }));
    }
    eprintln!(
        "shell: {}, model: {model}, budget: {budget}, ordered: {ordered}, introspect: \
         {introspecting}",
        app.confinement
    );

    // `PLANT=label::content` puts something in the context before the question is asked, so a run
    // about inspecting a context has one worth inspecting
    if let Ok(planted) = std::env::var("PLANT") {
        for item in planted.split("||") {
            let (label, content) = item.split_once("::").unwrap_or(("notes", item));
            app.kernel.push(
                ContextItem::memory(label, content.to_owned())
                    .because("carried over from an earlier session"),
            );
        }
    }

    // the loop is `kamchatka --headless`, driven from a string instead of a pipe. `TASK2` is the
    // second line a user types.
    //
    // note: what stays here rather than moving into the program is what this example is for: a
    // planted context, a budget small enough that managing it is not optional, and `session.md`,
    // which is a reading of the context rather than a log of the session
    let started = std::time::Instant::now();
    let mut lines = task();
    if let Ok(next) = std::env::var("TASK2") {
        lines.push('\n');
        lines.push_str(&next);
    }
    lines.push('\n');

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    // every question is granted, which is what a recording someone is watching wants and the
    // opposite of the program default; see `OnAsk` in `args.rs`.
    //
    // note: the deadline is the driver's rather than a `timeout` around it. Wrapped in a timeout,
    // the future is dropped where it stands, so the session is never finished and the records of
    // the turn it was stopped in go nowhere. A run that hit its deadline is worth reading
    // afterwards
    let mut driver =
        Headless::new(Grant::Allow, &mut records, &mut prose).deadline(Duration::from_secs(600));
    // Do not use `?`, which would skip the write below: a failed run is worth reading afterwards.
    // The error is still the exit code, and it waits until the record is on disk
    let failed = driver
        .run(&mut app, &mut events, &mut finished, lines.as_bytes())
        .await
        .err();
    if let Some(e) = &failed {
        eprintln!("the run failed: {e}");
    }

    write(&app.kernel, budget, &prose, started.elapsed())?;
    match failed {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

/// Writes the exchange out four ways: to read, to check, to replay, and as it streamed.
fn write(
    kernel: &Kernel,
    budget: usize,
    prose: &[u8],
    took: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
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
    // note: what the driver printed, rather than the deltas drained off the broadcast at the end.
    // A subscriber nobody reads from until the session is over keeps only the last
    // `Config::event_queue_depth` events and drops the rest; this is what a person watching the
    // run saw
    fs::write(out.join("streamed.txt"), prose)?;

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
