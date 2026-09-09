//! The tool that reads: what is being carried, what it costs, what the next request holds, and
//! what the answer to it would be.
//!
//! note: five actions, none of which changes anything, which is why they are one tool and why the
//! capability they declare is its own. `draft` and `fork` do ask the model, so this is not free -
//! only harmless. The tool that changes things is next door in `amend`, and the two are separately
//! grantable on purpose.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use nachalnik::{
    Block, BoxError, Capability, Config, ContextId, ContextItem, ContextKind, ContextState, Delta,
    Event, Kernel, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};
use serde_json::json;

use crate::{tools::Limits, ui::thousands};

use super::{Pinned, Reach, action, ids, protected, unknown};

/// How long a fork may think before this looks up to see whether somebody has pressed escape.
const HEARTBEAT: Duration = Duration::from_millis(120);

/// How much of an item's text the listing shows on its row.
const GLIMPSE: usize = 48;

/// Reads the context, the budget, the request about to be sent, and the answer that would follow.
///
/// note: none of the five actions changes anything, which is why they are together and why the
/// capability they declare is its own. `draft` and `fork` do spend tokens - they ask the model -
/// so this is not free, only harmless.
pub struct Introspect {
    reach: Reach,
    pinned: Pinned,
    limits: Limits,
}

impl Introspect {
    /// Builds one; see [`super::install`], which is the only caller.
    pub(super) fn new(reach: Reach, pinned: Pinned, limits: Limits) -> Self {
        Self {
            reach,
            pinned,
            limits,
        }
    }
}

#[async_trait]
impl Tool for Introspect {
    fn spec(&self) -> ToolSpec {
        let spec = ToolSpec::new(
            "introspect",
            "reads your own state, so you can check it before you act on it. `look` lists every \
             item in your context - what it is, what it costs, whether it is going into the next \
             request and why not if it is not - and with `ids` reads any of them back, block by \
             block, including what you were thinking when you produced them. A long one comes \
             back as its start and its end, because reading an item copies it into your context; \
             `whole` if you need all of it anyway. `budget` is what \
             the next request costs against what there is, what the last one really cost, and \
             which items are the expensive ones - read it before deciding what to give up. \
             `request` shows the request you are about to send, message by message, with what the \
             projector left out and what it repaired. `draft` answers the conversation on a \
             throwaway copy and shows you what you would say *before* you say it, so you can \
             check your answer against your context and fix either. `fork` puts a question to a \
             copy of yourself on a copy of your context, optionally with some items left out - \
             for weighing an approach, or asking whether a piece of context is what is leading \
             you astray. A fork has no tools: it can think, not act. `amend` is the tool that \
             changes any of this.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["look", "budget", "request", "draft", "fork"],
                },
                "ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "look: read these items in full instead of listing all of them",
                },
                // note: declared, because the tool reads it, the description tells the model to
                // use it, and `look`'s own last line and the marker in a sampled item both end by
                // telling it to ask for the `whole` of one. An argument named in three places and
                // absent from the schema is one a model following the schema cannot pass, and one
                // an endpoint validating against the schema will refuse outright
                "whole": {
                    "type": "boolean",
                    "description": "look: read the named items entire, rather than as a start and \
                                    an end. It costs what carrying them costs",
                },
                "question": {
                    "type": "string",
                    "description": "fork: what to ask the copy",
                },
                "without": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "fork: item ids the copy does not get to see",
                },
            },
            "required": ["action"],
        }))
        .with_capabilities([Capability::Custom("introspect".into())]);

        self.limits.apply(spec)
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        match action(&call.args)? {
            "look" => Ok(ToolOutput::new(look(
                &kernel,
                &ids(&call.args, "ids"),
                call.args["whole"].as_bool().unwrap_or(false),
            ))),
            "budget" => Ok(ToolOutput::new(budget(&kernel, &self.pinned.lock()))),
            "request" => Ok(ToolOutput::new(request(&kernel))),
            "draft" => branch(&kernel, None, &[], &output).await,
            "fork" => {
                let Some(question) = call.args["question"].as_str() else {
                    return Ok(ToolOutput::error(
                        "`fork` needs a `question` to put to the copy; `draft` is the one that \
                         just carries on the conversation",
                    ));
                };
                branch(
                    &kernel,
                    Some(question),
                    &ids(&call.args, "without"),
                    &output,
                )
                .await
            }
            other => Ok(ToolOutput::error(unknown(
                other,
                &["look", "budget", "request", "draft", "fork"],
            ))),
        }
    }
}

/// The context, item by item, or the whole of the named ones.
fn look(kernel: &Kernel, ids: &[ContextId], whole: bool) -> String {
    let items = kernel.items();
    if !ids.is_empty() {
        return ids
            .iter()
            .map(|id| full(&items, *id, whole))
            .collect::<Vec<_>>()
            .join("\n");
    }

    let budget = kernel.budget();
    // note: the undo depth is reported and named as somebody else's on purpose. It is the stack
    // behind the `u` key in the terminal, it holds everything that has ever happened to this
    // context, and `amend undo` does not touch it - a figure that big, sitting unlabelled next to
    // a tool called `undo`, would be an invitation to try to walk back the person's work
    let (withheld, theirs) =
        kernel.with_context(|context| (context.tokens_withheld(), context.undo_len()));

    let mut out = format!(
        "{} items · {} of them go into the next request\n\
         ~{} tokens going{}, ~{} withheld\n\
         {} change(s) in the person's own undo stack, which is theirs; `amend undo` walks back \
         what you did\n\n\
         {:>4}  {:<10}  {:<18}  {:>8}  what it is\n",
        items.len(),
        items.iter().filter(|item| item.is_projected()).count(),
        thousands(budget.used()),
        budget
            .limit
            .map(|limit| format!(
                " of {} ({}%)",
                thousands(limit),
                (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize
            ))
            .unwrap_or_default(),
        thousands(withheld),
        theirs,
        "id",
        "state",
        "kind",
        "tokens",
    );

    for item in &items {
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>8}  {}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            thousands(item.tokens),
            row(item),
        ));
    }

    out.push_str(
        "\n`look` with `ids` reads any of these back, including the reasoning recorded on an \
         assistant turn; a long one arrives as its start and its end unless you ask for the \
         `whole` of it.\n",
    );

    out
}

/// One item's row: its label, then whatever else is worth knowing on one line.
fn row(item: &ContextItem) -> String {
    let glimpsed = glimpse(&item.content.to_text());
    let mut said = match glimpsed.is_empty() {
        true => item.label.clone(),
        false => format!("{}: {glimpsed}", item.label),
    };
    if matches!(item.kind, ContextKind::AssistantMessage { .. }) {
        // `calls()` and `thinking()` rather than the kind's own slots: a turn a provider recorded
        // in the order it was produced keeps both inside its content, and a row that read the
        // slots would report a reasoning model as having thought nothing and asked for nothing
        let calls = item.calls().count();
        if calls != 0 {
            said.push_str(&format!(" [{calls} call(s)]"));
        }
        if item.thinking().next().is_some() {
            said.push_str(" [+reasoning]");
        }
        if let Some(blocks) = item.content.as_blocks() {
            said.push_str(&format!(" [{} ordered block(s)]", blocks.len()));
        }
    }
    if let Some(note) = &item.note {
        said.push_str(&format!(" · {note}"));
    }

    said
}

/// The whole of one item, or the fact that there is no such item.
/// How much of an item's content `look` shows either side of the gap before it is asked for the
/// whole thing.
///
/// note: reading an item copies that item into the context. So asking to see a 9,000-token tool
/// result in order to decide whether to keep it costs very nearly what keeping it costs - a live
/// session did exactly that, twice, and finished an honest clean-up 7,688 tokens heavier than it
/// started. A head and a tail is enough to tell build noise from something worth keeping, and the
/// whole thing is still one argument away for the times it is really wanted.
const SAMPLE: usize = 1_500;

/// An item's content: whole if it is small or if it was asked for, a head and a tail otherwise.
fn sampled(text: &str, whole: bool) -> String {
    if whole || text.len() <= SAMPLE * 2 {
        return text.to_owned();
    }

    // on a character boundary, so that a cut through a multi-byte character does not panic
    let mut head = SAMPLE;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - SAMPLE;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }

    format!(
        "{}\n[... {} bytes not shown. Asking for an item copies it into your context, so reading \
         all of this costs about what carrying it costs; `whole: true` if you need it anyway ...]\n{}",
        &text[..head],
        thousands(tail - head),
        &text[tail..],
    )
}

fn full(items: &[Arc<ContextItem>], id: ContextId, whole: bool) -> String {
    let Some(item) = items.iter().find(|item| item.id == id) else {
        return format!("[{id}] there is no such item\n");
    };

    let mut out = format!(
        "[{}] {} · {} · from {} · {} · {} tokens\n",
        item.id,
        item.label,
        item.kind.name(),
        item.source,
        item.state,
        thousands(item.tokens),
    );
    if let Some(because) = &item.included_because {
        out.push_str(&format!("  it is here because: {because}\n"));
    }
    if let Some(note) = &item.note {
        out.push_str(&format!("  it is {} because: {note}\n", item.state));
    }
    if !item.meta.is_null() {
        out.push_str(&format!("  attached: {}\n", item.meta));
    }
    // a turn that was recorded as an order is read back as one, block by block. This is the
    // thing `introspect` exists for and the one view of it that is not available anywhere else: the
    // request the model will be sent has the same parts in the same order, but by then the
    // thinking looks like a field rather than something that happened between two calls
    if let Some(blocks) = item.content.as_blocks() {
        out.push_str(&format!("  --- {} block(s), in order ---\n", blocks.len()));
        for (at, block) in blocks.iter().enumerate() {
            let said = match block {
                Block::Call(call) => format!("{}({})", call.tool, call.args),
                _ => block
                    .part()
                    .map(|part| part.content.to_text().into_owned())
                    .unwrap_or_default(),
            };
            let signed = match block.extra().is_null() {
                true => "",
                false => " (signed)",
            };
            out.push_str(&format!("  [{at}] {}{signed}: {said}\n", block.name()));
        }

        return out;
    }

    // the reasoning first, because on the turn that carries it it is the part that explains the
    // rest, and because it is the one thing here the model cannot see in the request itself
    if let Some(reasoning) = item.reasoning() {
        out.push_str(&format!("  --- reasoning ---\n{}\n", reasoning.to_text()));
    }
    out.push_str(&format!(
        "  --- content ---\n{}\n",
        sampled(&item.content.to_text(), whole)
    ));

    out
}

/// What the next request costs, what there is, and what giving something up would buy.
///
/// note: the action a compaction decision is actually made from, which `look` was being asked to
/// be and is not: a table of every item in insertion order answers "what am I carrying?" and not
/// "what is it costing me and what should go?". The expensive items are the ones that decide that,
/// so they are sorted and totalled here rather than left to be found by reading.
///
/// note: it reports the estimate beside what the provider charged for the last request, and the
/// correction the counter has worked out from the difference, because the estimate is made without
/// the model's tokenizer and is usually low. An agent budgeting against a number nobody has
/// checked is the thing this crate exists not to do quietly.
fn budget(kernel: &Kernel, mine: &BTreeSet<ContextId>) -> String {
    let budget = kernel.budget();
    let withheld = kernel.with_context(|context| context.tokens_withheld());

    let room = match budget.limit {
        Some(limit) => format!(
            " of {} ({}% full, ~{} left)",
            thousands(limit),
            (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize,
            thousands(limit.saturating_sub(budget.used())),
        ),
        None => ", against a limit this provider does not report".to_owned(),
    };

    let mut out = format!(
        "the next request is ~{} tokens{room}\n  {} in the context, {} in the tool definitions\n\
         ~{} tokens are being held back - excluded, archived, or elided to a marker\n",
        thousands(budget.used()),
        thousands(budget.context_tokens),
        thousands(budget.tool_tokens),
        thousands(withheld),
    );

    match budget.reported {
        Some(usage) => out.push_str(&format!(
            "the last request really cost {} in / {}, as the provider counted it\n",
            thousands(usage.input_tokens.unwrap_or_default() as usize),
            crate::ui::charged(&usage),
        )),
        None => out.push_str(
            "nothing has been charged for yet, so the figures above are only an estimate\n",
        ),
    }
    if let Some(learned) = kernel.counter().calibration() {
        out.push_str(&match learned.observations {
            0 => "the estimate has not been checked against a real request yet; treat it as a floor\n"
                .to_owned(),
            seen => format!(
                "the estimate is corrected by x{:.2}, learned from {seen} request(s)\n",
                learned.scale
            ),
        });
    }

    // the expensive ones, biggest first: what a decision about compaction is made from.
    //
    // note: what the *projection* included, not what the states say. They are not the same list -
    // a tool result whose call is not in the request is repaired out of it by the projector, and
    // is costing nothing however active it looks. Offering it as something to save tokens by
    // eliding would be advice that buys nothing
    let going: BTreeSet<ContextId> = kernel.project().included.into_iter().collect();
    let mut costly: Vec<_> = kernel
        .items()
        .into_iter()
        .filter(|item| going.contains(&item.id) && item.state.sends_content())
        .collect();
    costly.sort_by_key(|item| std::cmp::Reverse(item.tokens));
    costly.truncate(10);

    if costly.is_empty() {
        return out;
    }

    out.push_str(&format!(
        "\nthe {} most expensive item(s) actually going into it:\n{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  what it is\n",
        costly.len(),
        "id",
        "state",
        "kind",
        "tokens",
        "if all go",
    ));
    let mut running = 0;
    for item in &costly {
        running += item.tokens;
        // saying so here saves a call that would only be refused, and the reason is the same one
        // `amend` would give: it is not the model's to move
        let whose = match protected(item, mine, None) {
            Some(_) => " · not yours",
            None => "",
        };
        out.push_str(&format!(
            "{:>4}  {:<10}  {:<18}  {:>8}  {:>8}  {}{whose}\n",
            item.id.0,
            item.state.to_string(),
            item.kind.name(),
            thousands(item.tokens),
            thousands(running),
            glimpse(&format!("{}: {}", item.label, item.content.to_text())),
        ));
    }
    out.push_str(
        "\nthe fifth column is what eliding everything down to that row would save, give or take \
         what the markers cost. Eliding leaves a marker in place, so a tool result still answers \
         the call that asked for it; excluding one takes that call down with it, and the model \
         then reads a conversation in which it never asked. `amend` does either.\n",
    );

    out
}

/// The request that would go next, as a shape rather than as its bytes.
///
/// note: a summary and not the request itself, which is the one thing this could print and must
/// not: the request *is* the context, so answering with it would double every token the agent was
/// asking about. Roles, sizes and first lines are what the question "what am I about to send?"
/// actually wants, and `/request` in the terminal has the verbatim JSON for whoever wants that.
fn request(kernel: &Kernel) -> String {
    let request = match kernel.preview_request() {
        Ok(request) => request,
        Err(e) => return format!("there is no request to preview: {e}\n"),
    };
    let projection = kernel.project();
    let budget = kernel.budget();

    let mut out = format!(
        "{} message(s), {} tool(s), ~{} tokens{}\n\n{:>4}  {:<10}  {:>8}  first line\n",
        request.messages.len(),
        request.tools.len(),
        thousands(budget.used()),
        budget
            .limit
            .map(|limit| format!(" of {}", thousands(limit)))
            .unwrap_or_default(),
        "#",
        "role",
        "bytes",
    );

    for (index, message) in request.messages.iter().enumerate() {
        let said = message
            .content
            .as_ref()
            .map(|c| c.to_text())
            .unwrap_or_default();
        out.push_str(&format!(
            "{:>4}  {:<10}  {:>8}  {}\n",
            index + 1,
            message.role.as_str(),
            thousands(said.len()),
            glimpse(&said),
        ));
    }

    if !projection.skipped.is_empty() {
        out.push_str("\nleft out:\n");
        for left_out in &projection.skipped {
            out.push_str(&format!("  [{}] {}\n", left_out.id, left_out.reason));
        }
    }
    if !projection.repairs.is_empty() {
        out.push_str("\nrepaired, to keep the request valid:\n");
        for repair in &projection.repairs {
            out.push_str(&format!("  {repair}\n"));
        }
    }

    out
}

/// Answers on a copy of the context, and hands back only what was said.
///
/// note: the copy is a whole second [`Kernel`] resumed from a [`Snapshot`](nachalnik::Snapshot) of
/// this one, which is why this needed nothing added to the runtime: forking a session is what
/// `snapshot` and `resume` already are, and the documentation for `resume` says as much. It gets
/// this session's provider and projector so that it is answering the same model in the same
/// dialect, and it gets **no tools**, no compactor and a limit of one request. A fork can think;
/// it cannot act, and it cannot go on thinking after it has answered once.
///
/// note: nothing the fork does reaches this session's context, and nothing it does reaches this
/// session's event log either - it has a log of its own that goes when it does. What it *is*
/// visible as is the text it streams, relayed into this tool's own [`OutputSink`], so a person
/// watching the terminal sees a fork thinking rather than a tool that has gone quiet.
async fn branch(
    kernel: &Kernel,
    question: Option<&str>,
    without: &[ContextId],
    output: &OutputSink,
) -> Result<ToolOutput, BoxError> {
    let Some(provider) = kernel.provider() else {
        return Ok(ToolOutput::error("there is no provider to ask"));
    };

    let mut snapshot = kernel.snapshot();
    let mut left_out = Vec::new();
    for item in &mut snapshot.items {
        if without.contains(&item.id) {
            // excluded rather than deleted, so the fork's own account of itself can still name
            // the item by the number this session knows it by
            item.state = ContextState::Excluded;
            item.note = Some("left out of this fork".into());
            left_out.push(item.id);
        }
    }
    let fork = Kernel::resume(
        Config {
            session_name: Some(format!("{}#fork", kernel.session_name())),
            // it answers once and is thrown away: there is nothing for an undo stack to be for,
            // and nothing after the first request for a second one to build on
            context_undo_depth: 0,
            max_requests_per_turn: Some(1),
            ..Config::default()
        },
        snapshot,
    );
    fork.set_provider(provider);
    fork.set_projector(kernel.projector());
    // note: said out loud, because the copy cannot work it out. It inherits a conversation full of
    // tool calls and their results and no tool definitions at all, and a model reading that asks
    // for a tool - which nothing here can run, so the answer comes back as a call and no words.
    // Measured against a real model that is not a corner case, it is what happens every time
    fork.push(
        ContextItem::system(
            "You are a copy of this session, made to think and not to act. You have no tools \
             here, and nothing you ask for can be run: answer in words, from what is already in \
             front of you.",
        )
        .pinned(),
    );
    if let Some(question) = question {
        fork.push(ContextItem::user(question).because("put to a fork of this context"));
    }
    // what the fork will actually read, rather than what it was handed: the projector still has
    // to repair the call this very tool is answering out of the copy, and a count taken before it
    // did would be one the fork never saw
    let items = fork.project().included.len();

    let mut events = fork.subscribe();
    let sink = output.clone();
    let relay = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            if let Event::ModelDelta {
                delta: Delta::Text(text),
            } = event
            {
                sink.push(text);
            }
        }
    });

    // the same heartbeat the `shell` tool runs on, and for the same reason: the fork is a whole
    // request that could take a minute, and escape has to reach it
    let outcome = {
        let turn = fork.turn();
        tokio::pin!(turn);
        loop {
            tokio::select! {
                outcome = &mut turn => break outcome,
                _ = tokio::time::sleep(HEARTBEAT) => {
                    if output.is_interrupted() {
                        fork.interrupt();
                    }
                }
            }
        }
    };
    relay.abort();

    if let Err(e) = outcome {
        return Ok(ToolOutput::error(format!("the fork got no answer: {e}")));
    }
    let Some(response) = fork.last_response() else {
        return Ok(ToolOutput::error("the fork got no answer at all"));
    };

    let mut out = match question {
        Some(question) => format!("a copy of you, asked `{question}`, on {items} of your items"),
        None => format!("what you would say if you answered now, drafted on {items} of your items"),
    };
    if !left_out.is_empty() {
        let numbers: Vec<String> = left_out.iter().map(|id| id.to_string()).collect();
        out.push_str(&format!(", without {}", numbers.join(", ")));
    }
    out.push_str(
        ". None of this is in your context and nobody has read it; it is yours to use or drop.\n",
    );
    if let Some(usage) = response.usage {
        out.push_str(&format!(
            "it cost {} in / {}.\n",
            thousands(usage.input_tokens.unwrap_or_default() as usize),
            crate::ui::charged(&usage),
        ));
    }
    let said = response
        .content
        .as_ref()
        .map(|content| content.to_text())
        .unwrap_or_default();
    match said.trim().is_empty() {
        // it asked for a tool instead of answering, and there is nothing in a fork to run one.
        // Saying so beats handing back a blank: a caller reading an empty draft has no way to
        // tell a copy that had nothing to say from one that tried to do something
        true => out.push_str(&format!(
            "\n--- it said nothing ({:?}) ---\nit asked for {} instead of answering, and a fork \
             has no tools; ask it something it can answer from what it already has.\n",
            response.stop,
            match response.calls().next() {
                Some(call) => format!("`{}`", call.tool),
                None => "nothing at all".to_owned(),
            },
        )),
        false => out.push_str(&format!(
            "\n--- what it said ({:?}) ---\n{said}\n",
            response.stop
        )),
    }

    // note: the answer before the thinking, which is the opposite of the order it was produced
    // in and the right way round for the one thing that happens to this output: an output limit
    // cuts from the end. On a reasoning model the thinking is the bulk of a fork - measured on one
    // real fork, 68% of 34,287 bytes against the answer's 30% - so with the thinking first the
    // limit ate the answer and left the deliberation about how to answer. That session lost
    // exactly the three paragraphs it had asked for. Nothing about this order is a claim about
    // what the copy did; the two sections are labelled and the reasoning says it is reasoning
    if let Some(reasoning) = &response.reasoning {
        out.push_str(&format!(
            "\n--- its reasoning, which it produced before the answer above ---\n{}\n",
            reasoning.to_text()
        ));
    }

    Ok(ToolOutput::new(out))
}

/// The first line of something, shortened to fit a column.
fn glimpse(text: &str) -> String {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    match first.chars().count() > GLIMPSE {
        true => format!("{}…", first.chars().take(GLIMPSE - 1).collect::<String>()),
        false => first.to_owned(),
    }
}
