//! A walk through everything the kernel promises, with no network and no API key involved.
//!
//! It ends up in the situation the design document calls the canonical one: a tool produces an
//! enormous, useless output, and instead of silently summarizing it, the runtime shows what it
//! costs and lets the user throw it out.
//!
//! Note what is *not* imported from the library: the permission policy and the `/context`
//! renderer are both written here, in about forty lines, because neither belongs in a kernel.
//!
//! ```text
//! cargo run --example transparency --features selectors
//! ```

use std::{collections::VecDeque, sync::Arc};

use nachalnik::{
    BoxError, Capability, Config, ContextItem, ContextState, Delta, DeltaSink, Event, Grant,
    Kernel, ModelInfo, ModelRequest, ModelResponse, OutputSink, PermissionPolicy,
    PermissionRequest, Provider, State, Tool, ToolCall, ToolOutput, ToolSpec, Verdict, async_trait,
    selectors::Selector,
};
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::broadcast::Receiver;

const WIDTH: usize = 78;

/// A provider that answers from a prepared script; a real one would speak HTTP instead.
struct Script(Mutex<VecDeque<ModelResponse>>);

#[async_trait]
impl Provider for Script {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: Some(128_000),
            tool_calling: true,
            ..ModelInfo::new("scripted", "as-if/gpt")
        }
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        let response = self.0.lock().pop_front().ok_or("the script ran out")?;
        if let Some(content) = &response.content {
            // a streaming provider reports fragments as they arrive
            deltas.text(content.to_text().into_owned());
        }

        Ok(response)
    }
}

/// Reading is cheap and reversible; anything with a side effect gets a question. This is the
/// entire permission story, and it is here rather than in the library on purpose.
struct AskAboutSideEffects;

#[async_trait]
impl PermissionPolicy for AskAboutSideEffects {
    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        if request
            .capabilities
            .iter()
            .all(|capability| *capability == Capability::Read)
        {
            Verdict::Allow
        } else {
            Verdict::Ask
        }
    }
}

/// A tool that hands back a canned file, and needs permission to read.
struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("read_file", "returns the contents of a file")
            .with_schema(json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }))
            .with_capabilities([Capability::Read])
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let path = call.args["path"].as_str().unwrap_or_default();

        Ok(ToolOutput::new(format!(
            "// {path}\nfn parse(input: &str) -> Result<Ast> {{\n    todo!()\n}}\n"
        )))
    }
}

/// A tool that runs a command - here, one that produces a wall of useless output.
struct CargoTest;

#[async_trait]
impl Tool for CargoTest {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("shell", "runs a shell command and returns its output")
            .with_schema(json!({
                "type": "object",
                "properties": { "cmd": { "type": "string" } },
                "required": ["cmd"],
            }))
            .with_capabilities([Capability::Shell])
    }

    async fn invoke(&self, _call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let mut all = String::from("   Compiling nachalnik v0.1.0\n");
        for i in 0..600 {
            let line = format!(
                "test parser::tests::case_{i:04} ... ok\nwarning: unused variable `x` at src/parser.rs:{i}\n"
            );
            // long-running tools report as they go, so they are not invisible until they finish
            if i % 200 == 0 {
                output.push(line.clone());
            }
            all.push_str(&line);
        }
        all.push_str("test result: ok. 600 passed; 0 failed\n");

        Ok(ToolOutput::new(all))
    }
}

/// Renders `/context` from the items themselves - the kernel has no opinion about how this
/// should look, and no code for it.
///
/// note: Every column is named, because a column of numbers whose meaning the reader has to
/// infer is not transparency, it is decoration.
fn render_context(kernel: &Kernel) {
    println!(
        "   {:<4}{:<13}{:<30}{:>8}",
        "ID", "SOURCE", "LABEL", "TOKENS"
    );
    println!("   {}", "─".repeat(WIDTH - 6));

    for item in kernel.items() {
        let state = match (item.is_projected(), &item.note) {
            (true, _) => String::new(),
            (false, Some(note)) => format!("  {} · {note}", item.state),
            (false, None) => format!("  {}", item.state),
        };
        println!(
            "   {:<4}{:<13}{:<30}{:>8}{}",
            item.id.to_string(),
            item.source.replace('_', " "),
            item.label.chars().take(29).collect::<String>(),
            thousands(item.tokens),
            clip(&state, WIDTH - 61),
        );
    }

    let budget = kernel.budget();
    let withheld = kernel.with_context(|c| c.tokens_withheld());
    println!("   {}", "─".repeat(WIDTH - 6));
    println!(
        "   {:<47}{:>8}",
        "the items above, as they project",
        thousands(budget.context_tokens)
    );
    println!(
        "   {:<47}{:>8}",
        "the tool definitions",
        thousands(budget.tool_tokens)
    );
    println!(
        "   {:<47}{:>8}   \x1b[1m← the request\x1b[0m",
        "together",
        thousands(budget.used())
    );
    if withheld != 0 {
        println!(
            "   {:<47}{:>8}",
            "withheld, still in the context",
            thousands(withheld)
        );
    }
    match budget.limit {
        Some(limit) => println!("   {:<47}{:>8}", "the model's limit", thousands(limit)),
        None => println!("   {:<47}{:>8}", "the model's limit", "unknown"),
    }
}

/// Clips a string so that nothing wraps in an eighty-column terminal.
fn clip(text: &str, room: usize) -> String {
    match text.chars().count() > room {
        true => text
            .chars()
            .take(room.saturating_sub(1))
            .chain(['…'])
            .collect(),
        false => text.to_owned(),
    }
}

/// Formats a number with `,` as the thousands separator.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }

    out
}

/// Prints whatever has happened since the last time it was called.
fn events(receiver: &mut Receiver<Event>) {
    while let Ok(event) = receiver.try_recv() {
        let detail = match &event {
            Event::StateChanged { from, to } => format!("{} -> {}", from.name(), to.name()),
            Event::SessionStarted { session } => format!("session {session}"),
            Event::ModelChanged { to, .. } => match to {
                Some(info) => format!("{} / {}", info.provider, info.model),
                None => "none".to_owned(),
            },
            Event::ToolsChanged { tools } => format!("now offering: {}", tools.join(", ")),
            Event::PolicyChanged => "AskAboutSideEffects".to_owned(),
            Event::ContextAdded {
                id, label, tokens, ..
            } => {
                format!("[{id}] {label}, {} tokens", thousands(*tokens))
            }
            Event::ContextChanged { id, from, to, .. } => format!("[{id}] {from} -> {to}"),
            Event::ContextUndone {
                items,
                removed,
                changed,
            } => format!(
                "{} reverted, {} taken back out of existence; the context holds {items}",
                changed.len(),
                removed.len()
            ),
            Event::ModelRequested {
                messages,
                tools,
                tokens,
                ..
            } => {
                format!(
                    "{messages} messages, {tools} tools, ~{} tokens",
                    thousands(*tokens)
                )
            }
            Event::ModelDelta { delta } => match delta {
                Delta::Text(text) => format!("text, {} bytes", text.len()),
                Delta::Reasoning(text) => format!("reasoning, {} bytes", text.len()),
                Delta::ToolArgs { call, fragment } => {
                    format!("arguments for {call}, {} bytes", fragment.len())
                }
                // `Delta` is `#[non_exhaustive]`, so a new kind of fragment is a recompile
                other => format!("{other:?}"),
            },
            Event::ModelFinished { stop, .. } => format!("{stop:?}"),
            Event::PermissionRequested { request } => format!("{} needs an answer", request.tool),
            Event::PermissionDecided {
                tool,
                grant,
                source,
                ..
            } => {
                format!("{tool}: {grant} (by the {source:?})")
            }
            Event::ToolRequested { tool, args, .. } => format!("{tool}({args})"),
            Event::ToolStarted { tool, .. } => tool.clone(),
            Event::ToolOutput { tool, chunk, .. } => {
                format!("{tool}: {} bytes so far", chunk.len())
            }
            Event::ToolFinished { tool, tokens, .. } => {
                format!("{tool}, {} tokens", thousands(*tokens))
            }
            _ => String::new(),
        };
        println!(
            "   \x1b[2m· {:<22}{}\x1b[0m",
            event.name(),
            clip(&detail, WIDTH - 28)
        );
    }
}

/// A numbered stage, with a sentence saying what it is there to show.
fn stage(number: usize, title: &str, note: &str) {
    println!("\n\x1b[1m{number} · {title}\x1b[0m");
    for line in textwrap(note, WIDTH - 4) {
        println!("   \x1b[2m{line}\x1b[0m");
    }
    println!();
}

fn textwrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let line = lines.last_mut().expect("there is always one");
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(word.to_owned());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }

    lines
}

/// A remark under a stage, in the same voice as the stage headings.
fn note(text: &str) {
    for (i, line) in textwrap(text, WIDTH - 6).into_iter().enumerate() {
        let lead = if i == 0 { "→" } else { " " };
        println!("   \x1b[2m{lead} {line}\x1b[0m");
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let kernel = Kernel::new(Config::default());
    let mut stream = kernel.subscribe();

    println!("\x1b[1mnachalnik · transparency\x1b[0m");
    println!("{}", "═".repeat(WIDTH));
    for line in textwrap(
        "A scripted model, two tools and a permission policy - no network and no API key. \
         Nothing below is a decision the kernel made: the policy, the prompt and the rendering \
         are all in this file, in about four hundred lines, because none of them belong in a \
         runtime.",
        WIDTH,
    ) {
        println!("{line}");
    }

    stage(
        1,
        "THE PIECES ARE YOURS",
        "A provider, two tools and a policy are installed. Even that is on the event stream, so \
         there is no moment in a session that the log cannot account for.",
    );
    kernel.set_provider(Arc::new(Script(Mutex::new(VecDeque::from([
        ModelResponse::tool_calls(vec![ToolCall::new(
            "call_01",
            "read_file",
            json!({ "path": "src/parser.rs" }),
        )]),
        ModelResponse::tool_calls(vec![ToolCall::new(
            "call_02",
            "shell",
            json!({ "cmd": "cargo test" }),
        )]),
        ModelResponse::text("`parse` is unimplemented - it hits the `todo!()` on the first call."),
    ])))));
    kernel.add_tool(Arc::new(ReadFile));
    kernel.add_tool(Arc::new(CargoTest));
    kernel.set_policy(Arc::new(AskAboutSideEffects));
    events(&mut stream);

    let info = kernel.model_info().unwrap();
    note(&format!(
        "the model is {} / {}, and it says its context holds {} tokens",
        info.provider,
        info.model,
        thousands(info.context_limit.unwrap_or_default())
    ));

    stage(
        2,
        "THE CONTEXT, ITEM BY ITEM",
        "Every item has an identity, a source, a label, a size and a state. The kernel put none \
         of them there - this file did, one call each - and the first one is pinned, which means \
         no compactor may drop it.",
    );
    kernel.push(
        ContextItem::instruction("AGENTS.md", "This project uses no unsafe code.")
            .because("the user asked for it in their config")
            .pinned(),
    );
    kernel.push(ContextItem::selection(
        "src/parser.rs:12-14",
        "fn parse(input: &str) -> Result<Ast> { todo!() }",
    ));
    kernel.push(ContextItem::diagnostic(
        "src/parser.rs:13",
        "warning: unreachable code after `todo!()`",
    ));
    kernel.push(ContextItem::user("why is this failing?"));
    events(&mut stream);
    println!();
    render_context(&kernel);

    stage(
        3,
        "WHAT WILL BE SENT, EXACTLY",
        "This is `preview_request()`. It is not a description of the request or a reconstruction \
         of it - it is the request, available before anything has been sent, and there is no \
         step between it and the wire where the kernel adds something of its own.",
    );
    println!("   {:<10}{:>7}   CONTENT", "ROLE", "BYTES");
    println!("   {}", "─".repeat(WIDTH - 6));
    for message in kernel.preview_request()?.messages {
        let content = message
            .content
            .unwrap_or_default()
            .to_text()
            .replace('\n', " ⏎ ");
        println!(
            "   {:<10}{:>7}   {}",
            message.role.as_str(),
            content.len(),
            clip(&content, WIDTH - 23)
        );
    }

    stage(
        4,
        "STEP ONE · ASK THE MODEL",
        "One `step` is one transition. The model asks to read a file, the policy allows reading \
         outright, and the loop stops in `ready` - the calls are decided, nothing has run yet, \
         and this is where a client can look before anything happens.",
    );
    let state = kernel.step().await?;
    events(&mut stream);
    println!("\n   \x1b[1mstate: {}\x1b[0m", state.name());

    stage(
        5,
        "STEP TWO · RUN WHAT IT ASKED FOR",
        "The tool runs, its result becomes a context item like any other, and the loop is back \
         at rest.",
    );
    let state = kernel.step().await?;
    events(&mut stream);
    println!("\n   \x1b[1mstate: {}\x1b[0m", state.name());

    stage(
        6,
        "STEP THREE · ASK AGAIN, AND THIS TIME SOMEBODY HAS TO DECIDE",
        "The model wants a shell. The policy in this file allows reads and asks about everything \
         else, so the loop stops and waits rather than proceeding and reporting afterwards.",
    );
    let state = kernel.step().await?;
    events(&mut stream);
    println!("\n   \x1b[1mstate: {}\x1b[0m", state.name());

    let State::Deciding { .. } = state else {
        unreachable!("the policy asks about shell access")
    };
    for request in kernel.pending_permissions() {
        println!("\n   ┌─ PERMISSION {}", "─".repeat(WIDTH - 20));
        println!("   │ tool:         {}", request.tool);
        println!(
            "   │ capabilities: {}",
            request
                .capabilities
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("   │ arguments:    {}", clip(&request.args.to_string(), 55));
        println!("   └─ [y] [n] [inspect] {}", "─".repeat(WIDTH - 28));
        // a client would ask a person here; this one says yes
        let state = kernel.decide(request.id, Grant::Allow)?;
        events(&mut stream);
        println!("\n   \x1b[1mstate: {}\x1b[0m", state.name());
    }

    stage(
        7,
        "STEP FOUR · RUN IT, AND GET A WALL OF NOISE BACK",
        "The shell tool returns six hundred lines of passing tests. This is the situation every \
         agent runtime ends up in, and the only question that matters is what happens next.",
    );
    let state = kernel.step().await?;
    events(&mut stream);
    println!("\n   \x1b[1mstate: {}\x1b[0m\n", state.name());
    render_context(&kernel);

    stage(
        8,
        "NOTHING IS SACRED",
        "That tool result is 13,000 tokens of nothing. A client offers to remove it; the user \
         says yes; it goes. There is no negotiation with the agent about whether it is relevant.",
    );
    let noisy = "tool:shell:latest"
        .parse::<Selector>()?
        .matches(&kernel.items());
    let item = kernel.item(noisy[0]).unwrap();
    println!(
        "   > item {} ({}) is {} tokens.\n   > Remove it from the next request?  [y] [n] [inspect]\n   > y\n",
        item.id,
        item.label,
        thousands(item.tokens)
    );

    let before = kernel.budget().used();
    kernel.set_state(
        noisy.clone(),
        ContextState::Excluded,
        Some("an enormous test output with nothing in it".into()),
    );
    events(&mut stream);
    note(&format!(
        "the next request went from {} tokens to {}",
        thousands(before),
        thousands(kernel.budget().used())
    ));
    for repair in &kernel.project().repairs {
        note(&format!(
            "and so that the request stays valid, the projection {repair}"
        ));
    }

    println!("\n   > \"wait, no, I needed that\"\n");
    kernel.undo();
    events(&mut stream);
    note(&format!(
        "back to {} tokens, in one call - the item was never destroyed, only withheld",
        thousands(kernel.budget().used())
    ));

    println!("\n   > \"no, it really is garbage\"\n");
    kernel.set_state(
        noisy,
        ContextState::Excluded,
        Some("no, it really is garbage".into()),
    );
    events(&mut stream);

    stage(
        9,
        "STEP FIVE · CARRYING ON, WITH THE NOISE GONE",
        "Whatever you change while the kernel rests is what the next request contains. Nothing \
         had to be told that the context moved.",
    );
    let state = kernel.step().await?;
    events(&mut stream);
    println!("\n   \x1b[1mstate: {}\x1b[0m", state.name());
    if let Some(response) = kernel.last_response() {
        println!();
        for line in textwrap(
            &response.content.clone().unwrap_or_default().to_text(),
            WIDTH - 6,
        ) {
            println!("   \x1b[1m\"{line}\"\x1b[0m");
        }
    }

    stage(
        10,
        "THE SESSION, FROM THE OUTSIDE",
        "Every one of those events is also a record in an append-only log of plain serde types. \
         A client is `subscribe()` plus a renderer, and changing the renderer does not \
         invalidate a single past session.",
    );
    let history = kernel.history();
    println!(
        "   {} records, {} of them state transitions",
        history.len(),
        history
            .iter()
            .filter(|r| r.event.name() == "state.changed")
            .count()
    );
    println!("\n   the last three, as they would be persisted:\n");
    for record in history.iter().rev().take(3).rev() {
        let json = serde_json::to_string(record)?;
        println!("   \x1b[2m{}\x1b[0m", clip(&json, WIDTH - 6));
    }

    kernel.finish();

    Ok(())
}
