//! Automatic context management that cannot happen behind your back.
//!
//! The compactor here is ordinary user code: it decides when the context is too big, asks the
//! model to summarize what it is about to drop, and hands the kernel a plan. The kernel refuses
//! to remove anything pinned, applies the rest, and reports exactly what happened - which the
//! user is then free to undo.
//!
//! ```text
//! cargo run --example compaction
//! ```

use std::{collections::VecDeque, sync::Arc};

use nachalnik::{
    BoxError, Budget, CompactionPlan, CompactionReport, Compactor, Config, ContextItem,
    ContextKind, ContextState, DeltaSink, Event, Kernel, ModelInfo, ModelRequest, ModelResponse,
    Params, Provider, ToolCall, async_trait,
};
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::broadcast::Receiver;

const WIDTH: usize = 78;

/// A provider that answers from a script; a real one would speak HTTP.
struct Script(Mutex<VecDeque<ModelResponse>>);

#[async_trait]
impl Provider for Script {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            // deliberately tiny, so that a couple of tool results is already too much
            context_limit: Some(2_000),
            tool_calling: true,
            ..ModelInfo::new("scripted", "as-if/gpt")
        }
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        _deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        self.0.lock().pop_front().ok_or("the script ran out".into())
    }
}

/// Summarizes the largest tool results once the context passes a threshold.
struct Summarizer {
    provider: Arc<dyn Provider>,
    /// How full the context has to be before this bothers.
    threshold: f64,
    /// How empty it is trying to get it.
    target: f64,
}

#[async_trait]
impl Compactor for Summarizer {
    fn should_compact(&self, budget: &Budget) -> bool {
        budget
            .fraction_used()
            .is_some_and(|used| used >= self.threshold)
    }

    async fn plan(&self, items: &[Arc<ContextItem>], budget: &Budget) -> Option<CompactionPlan> {
        let target = (budget.limit? as f64 * self.target) as usize;

        // note: this deliberately does *not* filter out pinned items. It could - they arrive with
        // their states - but a promise you keep only because everyone remembers to is not a
        // promise. The kernel refuses them, and says which ones it refused
        let mut candidates: Vec<_> = items
            .iter()
            .filter(|item| {
                item.is_projected() && matches!(item.kind, ContextKind::ToolResult { .. })
            })
            .collect();
        candidates.sort_by_key(|item| std::cmp::Reverse(item.tokens));

        let mut used = budget.used();
        let mut remove = Vec::new();
        let mut text = String::new();
        for item in candidates {
            if used <= target {
                break;
            }
            used -= item.tokens.min(used);
            remove.push(item.id);
            text.push_str(&format!("{}:\n{}\n", item.label, item.content.to_text()));
        }
        if remove.is_empty() {
            return None;
        }

        // a compactor is just user code: it can ask a model for help like anything else
        let request = ModelRequest {
            messages: vec![nachalnik::Message::user(format!(
                "summarize the following tool output in one sentence:\n\n{text}"
            ))],
            tools: Vec::new(),
            params: Params::new(),
        };
        let summary = self
            .provider
            .respond(request, DeltaSink::disconnected())
            .await
            .ok()?
            .content?;

        Some(CompactionPlan {
            summary: Some(ContextItem::summary(summary)),
            reason: format!(
                "the context reached {}% of the {}-token limit",
                (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize,
                budget.limit.unwrap_or_default(),
            ),
            remove,
            elide: Vec::new(),
        })
    }
}

// -------------------------------------------------------------------------------- the rendering

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

/// A numbered stage, with a sentence saying what it is for.
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

/// Lists the context. Every column is labelled, because a number whose meaning you have to guess
/// at is not transparency.
fn render_context(kernel: &Kernel, caption: &str) {
    println!(
        "   {:<4}{:<11}{:<28}{:>8}",
        "ID", "STATE", "LABEL", "TOKENS"
    );

    for item in kernel.items() {
        let sent = match item.is_projected() {
            true => "",
            false => "   not sent",
        };
        println!(
            "   {:<4}{:<11}{:<28}{:>8}{sent}",
            item.id.to_string(),
            item.state.to_string(),
            item.label.chars().take(27).collect::<String>(),
            thousands(item.tokens),
        );
    }

    let budget = kernel.budget();
    let withheld = kernel.with_context(|context| context.tokens_withheld());
    println!("   {}", "─".repeat(51));
    println!(
        "   {:<43}{:>8}",
        "the next request",
        thousands(budget.used())
    );
    if withheld != 0 {
        println!(
            "   {:<43}{:>8}",
            "withheld, still in the context",
            thousands(withheld)
        );
    }
    println!(
        "   {:<43}{:>8}",
        "the model's limit",
        thousands(budget.limit.unwrap_or_default()),
    );
    note(caption);
}

/// A remark under a table, in the same voice as the stage headings.
fn note(text: &str) {
    for (i, line) in textwrap(text, WIDTH - 6).into_iter().enumerate() {
        let lead = if i == 0 { "→" } else { " " };
        println!("   \x1b[2m{lead} {line}\x1b[0m");
    }
}

/// Prints one event as a name and a detail, clipped so that nothing wraps.
fn line(name: &str, detail: &str) {
    let room = WIDTH - 26;
    let detail: String = match detail.chars().count() > room {
        true => detail.chars().take(room - 1).chain(['…']).collect(),
        false => detail.to_owned(),
    };

    println!("   \x1b[2m· {name:<20}{detail}\x1b[0m");
}

/// Renders a compaction report; the kernel supplies the facts, not the formatting.
fn render_report(report: &CompactionReport) {
    println!("   ┌─ CONTEXT COMPACTION {}", "─".repeat(WIDTH - 25));
    println!("   │ why:  {}", report.reason);
    println!("   │");

    if !report.removed.is_empty() {
        println!("   │ removed, and can be brought back with one `set_state`:");
        for item in &report.removed {
            println!(
                "   │   [{}] {:<28}{:>8} tokens",
                item.id,
                item.label,
                thousands(item.tokens)
            );
        }
    }
    if !report.refused.is_empty() {
        println!("   │");
        println!("   │ asked for, and refused by the kernel because it is pinned:");
        for item in &report.refused {
            println!(
                "   │   [{}] {:<28}{:>8} tokens",
                item.id,
                item.label,
                thousands(item.tokens)
            );
        }
    }
    if let Some(summary) = &report.summary {
        println!("   │");
        println!("   │ added in their place:");
        println!(
            "   │   [{}] {:<28}{:>8} tokens",
            summary.id,
            summary.label,
            thousands(summary.tokens)
        );
    }

    println!("   │");
    println!(
        "   │ the next request: {} tokens -> {}",
        thousands(report.tokens_before),
        thousands(report.tokens_after)
    );
    println!("   └{}", "─".repeat(WIDTH - 4));
}

/// Prints whatever has happened since the last time it was called.
fn events(receiver: &mut Receiver<Event>) {
    while let Ok(event) = receiver.try_recv() {
        match event {
            Event::Compacted { report } => render_report(&report),
            Event::ContextChanged { id, from, to, note } => line(
                "context.changed",
                &format!(
                    "[{id}] {from} -> {to}{}",
                    note.map(|note| format!(" ({note})")).unwrap_or_default()
                ),
            ),
            Event::ContextAdded {
                id, label, tokens, ..
            } => line("context.added", &format!("[{id}] {label}, {tokens} tokens")),
            Event::StateChanged { from, to } => line(
                "state.changed",
                &format!("{} -> {}", from.name(), to.name()),
            ),
            Event::ModelRequested {
                messages,
                tokens,
                repairs,
                ..
            } => {
                line(
                    "model.requested",
                    &format!("{messages} messages, ~{tokens} tokens"),
                );
                for repair in repairs {
                    line("", &format!("↳ {repair}"));
                }
            }
            Event::ModelFinished { stop, .. } => line("model.finished", &format!("{stop:?}")),
            other => line(other.name(), ""),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let provider = Arc::new(Script(Mutex::new(VecDeque::from([
        ModelResponse::text("the grep found nothing in the parser; the failure is in the lexer"),
        ModelResponse::text("look at src/lexer.rs, not the parser"),
    ]))));

    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider.clone());
    kernel.set_compactor(Some(Arc::new(Summarizer {
        provider,
        threshold: 0.75,
        target: 0.15,
    })));

    println!("\x1b[1mnachalnik · compaction\x1b[0m");
    println!("{}", "═".repeat(WIDTH));
    for line in textwrap(
        "A compactor is ordinary user code. It decides when the context is too full and what \
         should go; the kernel refuses to touch anything pinned, applies the rest, and reports \
         every single thing it did. None of what follows is a decision the kernel made.",
        WIDTH,
    ) {
        println!("{line}");
    }

    // a session that has been running for a while: the model asked for two tools, both answered
    // at length, and one of the answers has been pinned by the user
    stage(
        1,
        "THE SESSION SO FAR",
        "Two large tool results, and a model with a deliberately small limit. The user pinned \
         one of the results, which makes it undroppable rather than merely important.",
    );
    kernel.push(ContextItem::assistant(
        "",
        vec![
            ToolCall::new("call_01", "cargo test", json!({})),
            ToolCall::new("call_02", "grep", json!({ "pattern": "lex" })),
        ],
    ));
    let pinned = kernel.push(
        ContextItem::tool_result("call_01".into(), "cargo test", "x".repeat(2_000), false)
            .because("the user pinned it")
            .pinned(),
    );
    let doomed = kernel.push(ContextItem::tool_result(
        "call_02".into(),
        "grep",
        "y".repeat(6_000),
        false,
    ));
    kernel.push(ContextItem::user("so where is the bug?"));

    render_context(
        &kernel,
        "already over the limit, so the next request cannot go out as it stands",
    );

    let mut stream = kernel.subscribe();
    events(&mut stream);

    stage(
        2,
        "THE NEXT TURN, WHICH COMPACTS FIRST",
        "The compactor runs at the start of the request, and everything it does arrives on the \
         event stream before the request goes out. It asked to remove both tool results; one of \
         them was pinned.",
    );
    kernel.turn().await?;
    events(&mut stream);

    stage(
        3,
        "THE CONTEXT AFTERWARDS",
        "Nothing was destroyed. The dropped result is still an item with an id and a size - it \
         is simply not being sent - and the summary that replaced it is an ordinary item too.",
    );
    render_context(&kernel, "comfortably under the limit again");
    println!();
    note(&format!(
        "The pinned item is still {}. The dropped one still holds every one of its {} bytes, \
         and putting it back is a `set_state` like any other.",
        kernel.item(pinned).unwrap().state,
        thousands(kernel.item(doomed).unwrap().content.byte_len()),
    ));

    stage(
        4,
        "\"NO. PUT IT BACK.\"",
        "Disagreeing with the compactor is a state change like any other, and it is one call. \
         The summary stays, because the user did not ask for that to go.",
    );
    kernel.set_state([doomed], ContextState::Active, Some("I wanted that".into()));
    events(&mut stream);
    render_context(
        &kernel,
        "and over the limit again, which is the user's business and nobody else's",
    );

    println!("\n\x1b[2m   turning it off entirely is one call: kernel.set_compactor(None)\x1b[0m");

    Ok(())
}
