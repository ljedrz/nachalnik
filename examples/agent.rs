//! An agent you can actually talk to: an OpenRouter (or any OpenAI-compatible) provider with
//! streaming, four tools, a permission prompt, and slash commands for everything the kernel
//! exposes.
//!
//! ```text
//! export OPENROUTER_API_KEY=sk-or-...
//! cargo run --example agent
//! ```
//!
//! Then type a message at the `>` prompt and press enter. One can also be given up front, along
//! with any files to put in the context:
//!
//! ```text
//! cargo run --example agent -- -f src/lib.rs "what does this crate do?"
//! ```
//!
//! `/save` writes the session log *and* a snapshot; `-r` picks the session back up:
//!
//! ```text
//! cargo run --example agent -- -r session.json
//! ```
//!
//! Optional environment:
//!
//! ```text
//! NACHALNIK_MODEL=qwen/qwen3-coder                    # or /model <id> at the prompt
//! NACHALNIK_BASE_URL=http://localhost:8080/v1         # llama.cpp, ollama, vLLM, OpenAI, ...
//! NACHALNIK_API_KEY=...                               # or OPENAI_API_KEY
//! ```
//!
//! Everything here is user code - the provider, the tools, the policy, the prompting, the
//! rendering. The kernel supplies the state machine, the context and the paper trail; this file
//! supplies the opinions. It drives [`Kernel::step`] rather than [`Kernel::turn`] so that it can
//! render between transitions, which is what a real client wants.
//!
//! In order: the four tools, the permission policy, the rendering, and the read-eval-print loop
//! with its slash commands. The provider is the one in `examples/common`, shared with the
//! `compare` and `panel` examples, because an HTTP client is not what any of them is about.

use std::{
    collections::HashSet,
    env,
    io::{Write, stdin, stdout},
    sync::Arc,
};

use nachalnik::{
    BoxError, BytesPerToken, Calibrating, Capability, Config, ContextItem, ContextState, Event,
    Grant, Kernel, OutputSink, PermissionPolicy, PermissionRequest, State, Tool, ToolCall,
    ToolOutput, ToolSpec, Verdict, async_trait, selectors::Selector,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::broadcast::Receiver;

/// How many requests one user message may cost before the agent stops to ask.
const REQUEST_BUDGET: usize = 12;

// ------------------------------------------------------------------------------ the provider

// the OpenAI-compatible HTTP provider, shared with the `compare` and `panel` examples; a
// directory under `examples/` with no `main.rs` is not built as an example of its own
#[path = "common/mod.rs"]
mod common;

use common::OpenAiCompatible;

// --------------------------------------------------------------------------------- the tools

/// A tool that is a closure plus a declaration.
struct Simple<F> {
    spec: ToolSpec,
    run: F,
}

#[async_trait]
impl<F> Tool for Simple<F>
where
    F: Fn(&Value) -> Result<String, BoxError> + Send + Sync,
{
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        match (self.run)(&call.args) {
            Ok(output) => Ok(ToolOutput::new(output)),
            // an expected failure the model should read and react to
            Err(e) => Ok(ToolOutput::error(e.to_string())),
        }
    }
}

fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, BoxError> {
    args[name]
        .as_str()
        .ok_or_else(|| format!("the `{name}` argument is required").into())
}

fn tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Simple {
            spec: ToolSpec::new("read", "reads a file and returns its contents")
                .with_schema(json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                }))
                .with_capabilities([Capability::Read])
                .with_output_limit(32_000),
            run: |args: &Value| Ok(std::fs::read_to_string(arg(args, "path")?)?),
        }),
        Arc::new(Simple {
            spec: ToolSpec::new("write", "creates or replaces a file")
                .with_schema(json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                    },
                    "required": ["path", "content"],
                }))
                .with_capabilities([Capability::Write]),
            run: |args: &Value| {
                let (path, content) = (arg(args, "path")?, arg(args, "content")?);
                std::fs::write(path, content)?;
                Ok(format!("wrote {} bytes to {path}", content.len()))
            },
        }),
        Arc::new(Simple {
            spec: ToolSpec::new(
                "edit",
                "replaces the first occurrence of `old` with `new` in a file",
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old": { "type": "string" },
                    "new": { "type": "string" },
                },
                "required": ["path", "old", "new"],
            }))
            .with_capabilities([Capability::Edit]),
            run: |args: &Value| {
                let (path, old, new) = (arg(args, "path")?, arg(args, "old")?, arg(args, "new")?);
                let before = std::fs::read_to_string(path)?;
                let Some(at) = before.find(old) else {
                    return Err(format!("`old` does not occur in {path}").into());
                };
                let after = format!("{}{new}{}", &before[..at], &before[at + old.len()..]);
                std::fs::write(path, after)?;
                Ok(format!("replaced one occurrence in {path}"))
            },
        }),
        Arc::new(Simple {
            spec: ToolSpec::new(
                "shell",
                "runs a command with `sh -c` and returns its output",
            )
            .with_schema(json!({
                "type": "object",
                "properties": { "cmd": { "type": "string" } },
                "required": ["cmd"],
            }))
            .with_capabilities([Capability::Shell])
            .with_output_limit(32_000),
            run: |args: &Value| {
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(arg(args, "cmd")?)
                    .output()?;

                Ok(format!(
                    "exit: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                ))
            },
        }),
    ]
}

// -------------------------------------------------------------------------------- the policy

/// Reading is allowed, the network is refused, and everything else is a question - unless the
/// user has already said "always" for that capability.
struct Careful {
    allowed: Mutex<HashSet<Capability>>,
}

impl Careful {
    fn new() -> Self {
        Self {
            allowed: Mutex::new(HashSet::from([Capability::Read])),
        }
    }

    /// Remembers that these capabilities may be used without asking again.
    fn always(&self, capabilities: &[Capability]) {
        self.allowed.lock().extend(capabilities.iter().cloned());
    }

    fn listing(&self) -> String {
        let mut names: Vec<_> = self.allowed.lock().iter().map(|c| c.to_string()).collect();
        names.sort();

        names.join(", ")
    }
}

#[async_trait]
impl PermissionPolicy for Careful {
    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        if request.capabilities.contains(&Capability::Network) {
            return Verdict::Deny;
        }

        let allowed = self.allowed.lock();
        if request.capabilities.iter().all(|c| allowed.contains(c)) {
            Verdict::Allow
        } else {
            Verdict::Ask
        }
    }
}

// ------------------------------------------------------------------------------- the rendering

/// Prints whatever has happened since the last call, dimmed.
fn render(events: &mut Receiver<Event>) {
    while let Ok(event) = events.try_recv() {
        let line = match &event {
            Event::ContextAdded {
                id,
                source,
                label,
                tokens,
                ..
            } if source != "model" => Some(format!("+ [{id}] {label} ({source}), {tokens} tokens")),
            Event::ContextChanged { id, to, note, .. } => Some(format!(
                "~ [{id}] {to}{}",
                note.as_ref().map(|n| format!(": {n}")).unwrap_or_default()
            )),
            Event::ModelRequested {
                messages,
                tools,
                tokens,
                ..
            } => Some(format!(
                "→ {messages} messages, {tools} tools, ~{tokens} tokens"
            )),
            Event::ModelFinished { stop, usage, .. } => Some(match usage {
                Some(usage) => format!(
                    "← {stop:?}; {} in / {} out (reported)",
                    usage.input_tokens.unwrap_or(0),
                    usage.output_tokens.unwrap_or(0)
                ),
                None => format!("← {stop:?}"),
            }),
            Event::ModelFailed { error } => Some(format!("! {error}")),
            Event::ToolRequested { tool, args, .. } => Some(format!("⟩ {tool}({args})")),
            Event::ToolOutput { tool, chunk, .. } => {
                Some(format!("⟩ {tool}: {} bytes so far", chunk.len()))
            }
            Event::ToolFinished {
                tool,
                tokens,
                truncated,
                is_error,
                ..
            } => Some(format!(
                "⟨ {tool}: {tokens} tokens{}{}",
                if *is_error { " (error)" } else { "" },
                truncated
                    .map(|b| format!(", {b} bytes truncated"))
                    .unwrap_or_default()
            )),
            Event::PermissionDecided {
                tool,
                grant,
                source,
                ..
            } => Some(format!("· {tool}: {grant} (by the {source:?})")),
            Event::Compacted { report } => Some(format!(
                "· compacted: {} items out, {} -> {} tokens ({})",
                report.removed.len(),
                report.tokens_before,
                report.tokens_after,
                report.reason
            )),
            _ => None,
        };

        if let Some(line) = line {
            println!("\x1b[2m  {line}\x1b[0m");
        }
    }
}

/// Prints the context as a tree - the kernel has no opinion about how this should look, and no
/// code for it.
fn show_context(kernel: &Kernel) {
    let mut groups: Vec<(String, Vec<_>)> = Vec::new();
    for item in kernel.items() {
        match groups.iter_mut().find(|(source, _)| *source == item.source) {
            Some((_, items)) => items.push(item),
            None => groups.push((item.source.clone(), vec![item])),
        }
    }

    for (source, items) in groups {
        let sent: usize = items
            .iter()
            .filter(|i| i.is_projected())
            .map(|i| i.tokens)
            .sum();
        println!("{:<44}{sent:>9}", source.to_uppercase().replace('_', " "));

        let last = items.len() - 1;
        for (i, item) in items.iter().enumerate() {
            let branch = if i == last { "└─" } else { "├─" };
            let label: String = item.label.chars().take(30).collect();
            let left = format!("  {branch} [{}] {label}", item.id);
            let suffix = if item.is_projected() {
                String::new()
            } else {
                format!("  ({})", item.state)
            };
            println!("{left:<44}{:>9}{suffix}", item.tokens);
        }
    }

    let budget = kernel.budget();
    let withheld = kernel.with_context(|c| c.tokens_withheld());
    println!("\n{:<44}{:>9}", "TOOLS", budget.tool_tokens);
    println!("{:<44}{:>9}", "TOTAL", budget.used());
    if withheld != 0 {
        println!("{:<44}{:>9}  (not sent)", "WITHHELD", withheld);
    }
    match budget.limit {
        Some(limit) => println!("{:<44}{:>9}", "LIMIT", limit),
        None => println!("{:<44}{:>9}", "LIMIT", "unknown"),
    }
    if let Some(usage) = kernel.last_response().and_then(|r| r.usage) {
        println!(
            "{:<44}{:>9}  (reported by the provider)",
            "LAST REQUEST",
            usage.input_tokens.unwrap_or(0)
        );
    }
}

const USAGE: &str = "\
usage: agent [-f FILE]... [-r SESSION] [message...]

  -f, --file FILE      put a file in the context, pinned
  -r, --resume FILE    carry on from a session written by /save
  message              sent as soon as the agent starts; otherwise type at the prompt

environment:
  OPENROUTER_API_KEY / NACHALNIK_API_KEY / OPENAI_API_KEY
  NACHALNIK_MODEL     e.g. qwen/qwen3-coder
  NACHALNIK_BASE_URL  e.g. http://localhost:8080/v1";

const HELP: &str = "\
  /context [selector]   what is in the context, or what a selector matches
  /request              the exact request that would be sent next
  /payload              the provider's own payload for it, byte for byte
  /raw                  the provider's own last payload
  /state                what the runtime is doing
  /events [n]           the last n session records
  /prune <selector>     take items out of the next request
  /keep <selector>      pin items, so compaction cannot touch them
  /restore <selector>   put items back
  /undo                 revert the last context operation
  /tools                the tool definitions the model is offered
  /policy               which capabilities are allowed without asking
  /model [id]           show or switch the model (https://openrouter.ai/models)
  /params [key json]    show or set a model parameter
  /save [path]          write the session, resumable with -r
  /help /quit           (ctrl-c stops a turn in flight; again to leave)";

// ---------------------------------------------------------------------------------- the client

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

/// Asks the person at the terminal. This is the only place a decision is made.
fn ask(kernel: &Kernel, policy: &Careful, request: &PermissionRequest) -> Result<(), BoxError> {
    let capabilities: Vec<_> = request.capabilities.iter().map(|c| c.to_string()).collect();

    loop {
        println!("\n  ┌ {} wants: {}", request.tool, capabilities.join(", "));
        println!("  │ {}", request.args);
        let answer = prompt("  └ [y]es / [n]o / [a]lways / [i]nspect? ").unwrap_or_default();

        match answer.chars().next() {
            Some('y') => {
                kernel.decide(request.id, Grant::Allow)?;
                return Ok(());
            }
            Some('a') => {
                policy.always(&request.capabilities);
                kernel.decide(request.id, Grant::Allow)?;
                return Ok(());
            }
            Some('i') => {
                println!("{}", serde_json::to_string_pretty(&request.args)?);
                if let Some(tool) = kernel.tool(&request.tool) {
                    println!("{}", serde_json::to_string_pretty(&tool.spec())?);
                }
            }
            _ => {
                kernel.decide(request.id, Grant::Deny)?;
                return Ok(());
            }
        }
    }
}

/// Drives the state machine until the model is done, somebody has to decide something, or the
/// request budget runs out. The rendering happens between transitions, which is why this uses
/// `step` rather than `turn`.
async fn run(kernel: &Kernel, policy: &Careful, events: &mut Receiver<Event>) {
    let mut requests = 0;

    loop {
        if matches!(kernel.state(), State::Idle | State::Finished { .. }) {
            if requests >= REQUEST_BUDGET {
                let answer = prompt(&format!("\n  {requests} requests so far. continue? [y/N] "))
                    .unwrap_or_default();
                if !answer.starts_with('y') {
                    return;
                }
                requests = 0;
            }
            requests += 1;
        }

        let state = match kernel.step().await {
            Ok(state) => state,
            Err(e) => {
                render(events);
                println!("\x1b[31m  ! {e}\x1b[0m");
                return;
            }
        };
        render(events);

        match state {
            State::Finished { .. } => return,
            State::Deciding { .. } => {
                for request in kernel.pending_permissions() {
                    if let Err(e) = ask(kernel, policy, &request) {
                        println!("  ! {e}");
                        return;
                    }
                }
                render(events);
            }
            _ => {}
        }
    }
}

/// Handles a slash command; returns `false` when it is time to stop.
async fn command(
    kernel: &Kernel,
    policy: &Careful,
    provider: &OpenAiCompatible,
    line: &str,
) -> bool {
    let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();

    let resolve = |input: &str| -> Result<Vec<nachalnik::ContextId>, BoxError> {
        Ok(input.parse::<Selector>()?.matches(&kernel.items()))
    };

    match command {
        "/quit" | "/exit" => return false,
        "/help" => println!("{HELP}"),
        "/context" => {
            if rest.is_empty() {
                show_context(kernel);
            } else {
                match resolve(rest) {
                    Ok(ids) => {
                        for id in ids {
                            if let Some(item) = kernel.item(id) {
                                println!(
                                    "  [{}] {:<28}{:>8} tokens  {}",
                                    item.id, item.label, item.tokens, item.state
                                );
                            }
                        }
                    }
                    Err(e) => println!("  ! {e}"),
                }
            }
        }
        "/request" => match kernel.preview_request() {
            Ok(request) => println!("{}", serde_json::to_string_pretty(&request).unwrap()),
            Err(e) => println!("  ! {e}"),
        },
        // what `/request` turns into on the wire; this provider renders it once and sends what
        // it rendered, so this is the thing itself rather than a second opinion about it
        "/payload" => match kernel.preview_payload() {
            Ok(Some(payload)) => println!("{}", serde_json::to_string_pretty(&payload).unwrap()),
            Ok(None) => println!("  this provider cannot render a request without sending it"),
            Err(e) => println!("  ! {e}"),
        },
        "/raw" => match kernel.last_response().and_then(|r| r.raw.clone()) {
            Some(raw) => println!("{}", serde_json::to_string_pretty(&raw).unwrap()),
            None => println!("  nothing has been answered yet"),
        },
        "/state" => {
            println!("  {:?}", kernel.state());
            for call in kernel.pending_calls() {
                println!("  pending: {}({})", call.tool, call.args);
            }
        }
        "/events" => {
            let history = kernel.history();
            let count: usize = rest.parse().unwrap_or(10);
            for record in history.iter().rev().take(count).rev() {
                println!("\x1b[2m  {}\x1b[0m", serde_json::to_string(record).unwrap());
            }
        }
        "/prune" | "/keep" | "/restore" => match resolve(rest) {
            Ok(ids) if ids.is_empty() => println!("  nothing matches `{rest}`"),
            Ok(ids) => {
                let (state, note) = match command {
                    "/prune" => (ContextState::Excluded, Some(format!("pruned by `{rest}`"))),
                    "/keep" => (ContextState::Pinned, None),
                    _ => (ContextState::Active, None),
                };
                let changed = kernel.set_state(ids, state, note);
                println!("  {} item(s) are now {state}", changed.len());
            }
            Err(e) => println!("  ! {e}"),
        },
        "/undo" => {
            if kernel.undo() {
                println!("  reverted");
            } else {
                println!("  nothing to undo");
            }
        }
        "/tools" => {
            for spec in kernel.tool_specs() {
                let capabilities: Vec<_> =
                    spec.capabilities.iter().map(|c| c.to_string()).collect();
                println!(
                    "  {:<8}{:<52}[{}]",
                    spec.id,
                    spec.description,
                    capabilities.join(", ")
                );
            }
        }
        "/policy" => println!("  allowed without asking: {}", policy.listing()),
        "/model" => {
            if !rest.is_empty() {
                *provider.model.lock() = rest.to_owned();
                *provider.context_limit.lock() = None;
                // the new model has a limit of its own, and a budget measured against the old
                // one - or against none at all - is not a budget
                provider.probe().await;
            }
            match kernel.model_info() {
                Some(info) => println!(
                    "  {} / {} ({} tokens of context)",
                    info.provider,
                    info.model,
                    info.context_limit
                        .map(|l| l.to_string())
                        .unwrap_or_else(|| "an unknown number of".into())
                ),
                None => println!("  no provider"),
            }
        }
        "/params" => {
            if let Some((key, value)) = rest.split_once(' ') {
                match serde_json::from_str::<Value>(value.trim()) {
                    Ok(value) => {
                        let mut params = kernel.params();
                        params.insert(key.trim().to_owned(), value);
                        kernel.set_params(params);
                    }
                    Err(e) => println!("  ! {key} needs a JSON value: {e}"),
                }
            }
            println!(
                "  {}",
                serde_json::to_string(&kernel.params()).unwrap_or_default()
            );
        }
        "/save" => {
            // the log says what happened; the snapshot is what can be resumed from. Keeping only
            // one of them means either losing the story or losing the context
            let stem = match rest {
                "" => "session",
                given => given.strip_suffix(".json").unwrap_or(given),
            };
            let log = format!("{stem}.jsonl");
            let state = format!("{stem}.json");

            let jsonl: Vec<String> = kernel
                .history()
                .iter()
                .map(|record| serde_json::to_string(record).unwrap())
                .collect();
            let written = std::fs::write(&log, jsonl.join("\n") + "\n").and_then(|()| {
                let snapshot = serde_json::to_vec_pretty(&kernel.snapshot())?;
                std::fs::write(&state, snapshot)
            });
            match written {
                Ok(()) => println!(
                    "  {} records in {log}, and a session in {state} (agent -r {state})",
                    jsonl.len()
                ),
                Err(e) => println!("  ! {e}"),
            }
        }
        other => println!("  unknown command `{other}`; try /help"),
    }

    true
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    // `-f path` puts a file in the context, pinned; everything else is the first message
    let mut files = Vec::new();
    let mut resume = None;
    let mut words = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-f" | "--file" => match args.next() {
                Some(path) => files.push(path),
                None => return Err("-f needs a path".into()),
            },
            "-r" | "--resume" => match args.next() {
                Some(path) => resume = Some(path),
                None => return Err("-r needs a path".into()),
            },
            _ => words.push(arg),
        }
    }

    let model = env::var("NACHALNIK_MODEL").unwrap_or_else(|_| "openai/gpt-4o-mini".to_owned());

    // this one prints as it streams, because it runs on the same task the terminal does
    let provider = Arc::new(OpenAiCompatible::new(reqwest::Client::new(), model)?.echoing());
    provider.probe().await;

    // the kernel enforces no turn budget of its own here: this client counts requests itself,
    // because how long an agent may keep going is a decision for whoever is watching it
    let config = Config {
        max_requests_per_turn: None,
        ..Default::default()
    };
    let kernel = match &resume {
        Some(path) => {
            let snapshot = serde_json::from_slice(&std::fs::read(path)?)?;
            let kernel = Kernel::resume(config, snapshot);
            println!(
                "resumed {} - {} items, ~{} tokens",
                kernel.session_name(),
                kernel.items().len(),
                kernel.budget().context_tokens
            );
            kernel
        }
        None => Kernel::new(config),
    };
    let mut events = kernel.subscribe();
    let policy = Arc::new(Careful::new());

    // ctrl-c asks the loop to stop rather than killing the process: the provider is watching, so
    // a long answer stops where it is and what had arrived stays in the context. Pressing it
    // while nothing is running means what it usually means
    {
        let kernel = kernel.clone();
        tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                if !kernel.state().is_busy() {
                    println!();
                    std::process::exit(0);
                }
                kernel.interrupt();
                println!("\n\x1b[2m  stopping...\x1b[0m");
            }
        });
    }

    kernel.set_provider(provider.clone());
    kernel.set_policy(policy.clone());
    // the default counter runs about a sixth low against this API; this one is told what each
    // request actually cost and corrects itself, so `/context` converges on the truth
    kernel.set_counter(Arc::new(Calibrating::new(BytesPerToken::default())));
    for tool in tools() {
        kernel.add_tool(tool);
    }

    for path in files {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                kernel.push(
                    ContextItem::file(&path, content)
                        .because("the user named it on the command line")
                        .pinned(),
                );
            }
            Err(e) => eprintln!("could not read {path}: {e}"),
        }
    }

    let info = kernel.model_info().unwrap();
    println!(
        "\x1b[1mnachalnik\x1b[0m · {} · {} tokens of context",
        info.model,
        info.context_limit
            .map(|l| l.to_string())
            .unwrap_or_else(|| "?".into())
    );
    println!(
        "type a message and press enter · /help for the commands · /model <id> to switch models\n"
    );
    render(&mut events);

    // a message given on the command line is sent straight away
    let mut pending = (!words.is_empty()).then(|| words.join(" "));

    loop {
        let line = match pending.take() {
            Some(line) => {
                println!("\x1b[1m> \x1b[0m{line}");
                line
            }
            None => match prompt("\x1b[1m> \x1b[0m") {
                Some(line) => line,
                None => break,
            },
        };

        if line.is_empty() {
            continue;
        }
        if line.starts_with('/') {
            if !command(&kernel, &policy, &provider, &line).await {
                break;
            }
            continue;
        }

        // this is all "sending a message" is: one context item, then the loop
        kernel.push(ContextItem::user(line));
        render(&mut events);
        run(&kernel, &policy, &mut events).await;

        // the counter has just been told what the last request really cost, so the figures on
        // the older items are out of date. Bringing them into line is a decision, not a
        // side effect - it is one call, and it says so on the event stream
        kernel.recount();
        render(&mut events);
    }

    kernel.finish();
    println!("\n{} events recorded", kernel.history().len());

    Ok(())
}
