//! Two tools that let an agent inspect and manage its own context: one that reads it, one that
//! changes it.
//!
//! note: Everything here is ordinary user code, like the rest of `tools.rs`, and none of it
//! needed a line added to the runtime. What the runtime has is a context that is a list of public
//! values, a request that can be built without being sent, and a session that can be snapshotted
//! and resumed - and a tool is allowed to call all of it. That is the whole trick: introspection
//! is not a feature of the kernel, it is what a tool can already do with the kernel's ordinary
//! surface. What these two add is the part the kernel has no opinion about: which of it a *model*
//! may do.
//!
//! note: Two tools rather than one with an `action` argument, because a [`ToolSpec`] declares its
//! capabilities once for every call it will ever receive. One tool would mean that answering
//! *always* to "may it look at its own context?" also answered "may it rewrite a tool result?" -
//! a grant that delivers considerably more than it implies, which is the shape of thing this
//! program exists not to do. So [`Introspect`] looks and [`Amend`] changes, they declare different
//! capabilities, and the permissions tab has a row for each.
//!
//! note: What [`Amend`] will not do is undo a person's decisions. A pinned item, a system
//! instruction, and the assistant turn carrying the call being executed are all refused, with the
//! reason handed back to the model. The agent is not the boss.

use std::{
    collections::BTreeSet,
    sync::{Arc, Weak},
    time::Duration,
};

use nachalnik::{
    Block, BoxError, Capability, Config, Content, ContextId, ContextItem, ContextKind,
    ContextState, Delta, Event, Kernel, OutputSink, Tool, ToolCall, ToolCallId, ToolOutput,
    ToolSpec, async_trait, selectors::Selector,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::ui::thousands;

/// How long a fork may think before this looks up to see whether somebody has pressed escape.
const HEARTBEAT: Duration = Duration::from_millis(120);

/// How much of an item's text the listing shows on its row.
const GLIMPSE: usize = 48;

/// Registers both tools, and returns the handle that keeps their reach into the kernel alive.
///
/// note: the return value is load-bearing rather than informational, and dropping it is how the
/// tools are switched off: they hold a [`Weak`] to it, and a tool that cannot upgrade its weak
/// handle refuses the call and says why. That indirection is not decoration either. A `Kernel`
/// stored inside a `Tool` the same kernel holds is a reference cycle that keeps the whole session
/// alive after the last handle to it is gone, which the runtime's own documentation warns about;
/// an [`Arc`] somebody *else* owns, pointed at weakly from in here, is the shape that has an end.
pub fn install(kernel: &Kernel) -> Arc<Kernel> {
    let anchor = Arc::new(kernel.clone());
    let reach = Reach(Arc::downgrade(&anchor));
    // shared, because the two tools are one agent's hands: what `amend` pinned is what
    // `introspect` should report as the agent's own to unpin, and a second set would have them
    // disagreeing about a promise
    let pinned = Arc::new(Mutex::new(BTreeSet::new()));

    kernel.add_tool(Arc::new(Introspect {
        reach: reach.clone(),
        pinned: pinned.clone(),
    }));
    kernel.add_tool(Arc::new(Amend {
        reach,
        pinned,
        journal: Mutex::new(Journal::default()),
    }));

    anchor
}

/// The way back to the kernel a tool is registered on.
#[derive(Clone)]
struct Reach(Weak<Kernel>);

impl Reach {
    /// The kernel, or the reason there is not one any more.
    fn kernel(&self) -> Result<Arc<Kernel>, BoxError> {
        self.0.upgrade().ok_or_else(|| {
            "this session is over; there is nothing left to look at or change".into()
        })
    }
}

// ------------------------------------------------------------------------------------ looking

/// Reads the context, the budget, the request about to be sent, and the answer that would follow.
///
/// note: none of the five actions changes anything, which is why they are together and why the
/// capability they declare is its own. `draft` and `fork` do spend tokens - they ask the model -
/// so this is not free, only harmless.
pub struct Introspect {
    reach: Reach,
    pinned: Pinned,
}

/// The items the agent pinned itself, shared between the tool that sets them and the one that
/// reports them.
type Pinned = Arc<Mutex<BTreeSet<ContextId>>>;

#[async_trait]
impl Tool for Introspect {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
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
        .with_capabilities([Capability::Custom("introspect".into())])
        .with_output_limit(32_000)
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
            "the last request really cost {} in / {} out, as the provider counted it\n",
            thousands(usage.input_tokens.unwrap_or_default() as usize),
            thousands(usage.output_tokens.unwrap_or_default() as usize),
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
            "it cost {} in / {} out.\n",
            thousands(usage.input_tokens.unwrap_or_default() as usize),
            thousands(usage.output_tokens.unwrap_or_default() as usize),
        ));
    }
    if let Some(reasoning) = &response.reasoning {
        out.push_str(&format!(
            "\n--- its reasoning ---\n{}\n",
            reasoning.to_text()
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

    Ok(ToolOutput::new(out))
}

// ----------------------------------------------------------------------------------- changing

/// Manages the context: prunes it, rewrites an item, writes something down, walks its own
/// changes back.
///
/// note: it keeps the set of items it pinned itself, which is the whole of the mechanism that
/// stops a model quietly unpinning what a person pinned. A pin is a promise, and the promise was
/// not made to the model.
///
/// note: it also keeps a journal of what it has done, which is what `undo` walks - deliberately
/// *not* [`Kernel::undo`]. Two reasons, and either would be enough. The kernel's undo stack is the
/// person's, bound to the `u` key in the terminal, and a model walking it back would be undoing
/// their work rather than its own. And the top of that stack, at the moment a tool is running, is
/// always the assistant turn that asked for the call: one step would erase the model's own
/// question, orphan the answer it is waiting for, and leave the loop rebuilding a request from
/// before it asked. A journal of this tool's own amendments has neither problem, and it is the
/// honest scope of "undo my mistakes" - the mistakes being the ones it made.
pub struct Amend {
    reach: Reach,
    pinned: Pinned,
    journal: Mutex<Journal>,
}

/// What [`Amend`] has done, and what it has walked back.
#[derive(Default)]
struct Journal {
    done: Vec<Undoing>,
    undone: Vec<Undoing>,
}

/// One amendment, recorded as the way back from it.
///
/// note: the way back rather than the change itself, because applying one returns the way back
/// from *that* - so undo and redo are the same operation run against two stacks, and there is no
/// second representation to keep in step with the first.
enum Undoing {
    /// Put these items into these states, with these notes.
    States(Vec<(ContextId, ContextState, Option<String>)>),
    /// Put this text and this metadata back on this item.
    Said(ContextId, Content, Value),
}

impl Undoing {
    /// Applies it, and hands back the way from where that leaves things to where they were.
    fn apply(self, kernel: &Kernel) -> Option<Self> {
        match self {
            Self::States(states) => {
                let mut back = Vec::new();
                for (id, state, note) in states {
                    let Some(item) = kernel.item(id) else {
                        continue;
                    };
                    back.push((id, item.state, item.note.clone()));
                    kernel.set_state([id], state, note);
                }

                (!back.is_empty()).then_some(Self::States(back))
            }
            Self::Said(id, content, meta) => {
                let item = kernel.item(id)?;
                let back = Self::Said(id, item.content.clone(), item.meta.clone());
                kernel.replace(id, content).ok()?;
                let _ = kernel.annotate(id, meta);

                Some(back)
            }
        }
    }

    /// What it will put back, for a report somebody has to read.
    fn about(&self) -> String {
        match self {
            Self::States(states) => format!(
                "{} back to {}",
                numbers(&states.iter().map(|(id, ..)| *id).collect::<Vec<_>>()),
                match states.first() {
                    Some((_, state, _)) => state.to_string(),
                    None => "nothing".to_owned(),
                }
            ),
            Self::Said(id, ..) => format!("what [{id}] said"),
        }
    }
}

#[async_trait]
impl Tool for Amend {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "amend",
            "manages your own context, so that what you carry into the next request is what you \
             decided to carry. `prune` moves items: `elide` leaves a short marker in place of one, \
             which is what to reach for when a tool result has served its purpose - the call it \
             answers stays answered and stops costing what it holds; `exclude` takes one out \
             altogether, which also takes down the call that asked for it; `archive` puts one away \
             for good; `pin` protects one from being compacted away; `restore` puts one back. Name \
             the items with `ids`, or with `select` for a whole class of them at once. `revise` \
             rewrites what one item says, for when you wrote something down wrong. `note` writes \
             something into your own context - a plan, a conclusion, a thing not to try again - \
             which you can pin so that compaction cannot take it. `undo` and `redo` walk back and \
             forward through the changes *you* made with this tool. Nothing here destroys \
             anything: every item keeps its number and can be restored. A pinned item, a system \
             instruction and the turn you are speaking in are refused - they are not yours. A \
             reason is required, and it is what the person you are working with reads. Use \
             `introspect` to look first, `budget` especially.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["prune", "revise", "note", "undo", "redo"],
                },
                "ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "prune: the items to move. revise: exactly one item",
                },
                // note: the forms, with the variable part written as a placeholder. It listed
                // examples - `tool:shell`, `kind:assistant_message` - and a model reading them as
                // literals rather than as instances asked to prune `tool:shell` in a session with
                // no shell. The closed sets are not spelled out here because `look` prints them
                // in its own columns, which is a shorter way to learn them than a schema is
                "select": {
                    "type": "string",
                    "description": "prune: a class of items instead of `ids`. One of: an item \
                                    number; `all`; `all:tool_results` (or files, diagnostics, \
                                    selections, memories, instructions, system, user, model, \
                                    compaction); `kind:<kind>` or `state:<state>`, taking the \
                                    words `look` prints in those columns; `tool:<name>`, \
                                    optionally `:first` or `:latest`; `source:<name>`; \
                                    `file:<path>`; `label:<text>`. Anything else is read as a \
                                    label.",
                },
                "state": {
                    "type": "string",
                    // note: the five worth naming. `unelide`, `unpin`, `include` and the rest
                    // are accepted and deliberately not listed - an enum of eleven words, six of
                    // them the same word, is a harder thing to read than one of five
                    "enum": ["elide", "exclude", "archive", "pin", "restore"],
                    "description": "prune: where they go. `restore` is the way back from any of \
                                    the others, including a pin you set yourself",
                },
                "content": {
                    "type": "string",
                    "description": "revise: what the item should say instead. note: what to write down",
                },
                "label": {
                    "type": "string",
                    "description": "note: a short name for it, so it is findable later",
                },
                "pin": {
                    "type": "boolean",
                    "description": "note: protect it from compaction",
                },
                "reason": {
                    "type": "string",
                    "description": "why, in your own words; the person you work with reads this",
                },
                "steps": {
                    "type": "integer",
                    "description": "undo, redo: how many of your own changes to walk; 1 by default",
                },
            },
            "required": ["action", "reason"],
        }))
        .with_capabilities([Capability::Custom("amend".into())])
        .with_output_limit(8_000)
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        let Some(reason) = call.args["reason"]
            .as_str()
            .filter(|r| !r.trim().is_empty())
        else {
            return Ok(ToolOutput::error(
                "`reason` is required: it becomes the item's note, and it is what the person at \
                 the terminal reads when they ask why something is not in the request",
            ));
        };

        match action(&call.args)? {
            "prune" => Ok(self.prune(&kernel, call, reason)),
            "revise" => Ok(self.revise(&kernel, call, reason)),
            "note" => Ok(self.note(&kernel, call, reason)),
            "undo" => Ok(self.walk(&kernel, call, reason, true)),
            "redo" => Ok(self.walk(&kernel, call, reason, false)),
            // a word that names a *state* is not a typo, it is somebody looking in the right
            // tool at the wrong level: `restore` is what `prune` puts an item back to, and two
            // models in a row spent a call each being told it does not exist before giving up
            other if state_of(other).is_some() => Ok(ToolOutput::error(format!(
                "`{other}` is a `state`, not an `action`: \
                 {{\"action\":\"prune\",\"ids\":[…],\"state\":\"{other}\",\"reason\":\"…\"}}"
            ))),
            other => Ok(ToolOutput::error(unknown(
                other,
                &["prune", "revise", "note", "undo", "redo"],
            ))),
        }
    }
}

/// Whether anything the agent wrote down for itself is still going into the request.
///
/// A note is the one thing in a context that is there because the agent decided a finding was
/// worth keeping. If none is, then everything the agent knows is in items it did not choose, and
/// hiding those is the whole of what it knew.
fn wrote_anything_down(kernel: &Kernel) -> bool {
    kernel.items().iter().any(|item| {
        matches!(item.kind, ContextKind::Reference)
            && item.source == "agent"
            && item.state.sends_content()
    })
}

impl Amend {
    /// Moves items to a state, refusing the ones that are not the model's to move.
    fn prune(&self, kernel: &Kernel, call: &ToolCall, reason: &str) -> ToolOutput {
        // a selector, or a list of numbers. Naming a class of items is what makes this usable for
        // the job it is mostly for - "the tool results I am done with" is one thought, and
        // reading twelve numbers off a listing to say it is not
        let selected = call.args["select"].as_str();
        let ids = match selected {
            Some(input) => match input.parse::<Selector>() {
                Ok(selector) => selector.matches(&kernel.items()),
                Err(e) => {
                    return ToolOutput::error(format!(
                        "`{input}` is not a selector: {e}\n\n{}",
                        crate::ui::SELECTORS
                    ));
                }
            },
            None => ids(&call.args, "ids"),
        };
        if ids.is_empty() {
            // a selector that parsed and matched nothing is a different mistake from naming no
            // items at all, and telling them apart is the difference between trying again with a
            // better selector and trying again with the same one
            return ToolOutput::error(match selected {
                Some(input) => format!(
                    "`{input}` is a selector, and nothing in your context matches it; \
                     `introspect` with `look` lists what there is"
                ),
                None => "`prune` needs `ids`, or a `select` naming a class of them".to_owned(),
            });
        }
        let Some(state) = state_of(call.args["state"].as_str().unwrap_or_default()) else {
            // what each one does rather than only what it is called: the choice between `elide`
            // and `exclude` is the one that decides whether a tool call keeps its answer, and a
            // list of five words does not help anybody make it
            return ToolOutput::error(
                "`state` must be one of:\n  \
                 elide    - replace what it says with a marker; a tool call keeps its answer\n  \
                 exclude  - take it out of the request; a tool call loses its answer too\n  \
                 archive  - keep it, do not send it, and stop counting it against the budget\n  \
                 pin      - protect it from compaction\n  \
                 restore  - put it back the way it was",
            );
        };

        let before = kernel.budget().used();
        let mine = self.pinned.lock().clone();
        let own = own_turn(kernel, &call.id);

        let (mut allowed, mut refused) = (Vec::new(), Vec::new());
        for id in ids {
            match kernel.item(id) {
                None => refused.push(format!("[{id}] there is no such item")),
                Some(item) => match protected(&item, &mine, own) {
                    Some(why) => refused.push(format!("[{id}] {why}")),
                    None => allowed.push(id),
                },
            }
        }

        // read before the change, because it is the way back from it
        let was: Vec<_> = allowed
            .iter()
            .filter_map(|id| kernel.item(*id))
            .map(|item| (item.id, item.state, item.note.clone()))
            .collect();
        let changed = kernel.set_state(allowed, state, Some(reason.to_owned()));
        for id in &changed.changed {
            self.note_pin(*id, state);
        }
        if !changed.changed.is_empty() {
            let moved = was
                .into_iter()
                .filter(|(id, ..)| changed.changed.contains(id));
            self.record(Undoing::States(moved.collect()));
        }

        let mut out = format!("{} item(s) are now {state}", changed.changed.len());
        if !changed.changed.is_empty() {
            out.push_str(&format!(": {}", numbers(&changed.changed)));
        }
        out.push('\n');
        // the way back, at the moment it becomes worth knowing. A session that elided twenty-two
        // items spent its next two calls guessing at an `action` called `restore` and then gave
        // up; the reversal is a `state`, and six words here are cheaper than that
        if !changed.changed.is_empty() && !state.sends_content() {
            out.push_str("back: the same ids with `state: \"restore\"`, or `undo` for all of it\n");
            // the failure this closes: a run gathered nineteen thousand tokens of evidence across
            // seventeen tool results, said nothing in its own turns, elided all seventeen at once,
            // and then answered all ten questions from nothing - confidently, and wrong on every
            // one. The tool told it what it had saved and nothing about what it had just spent
            if !wrote_anything_down(kernel) {
                out.push_str(
                    "you have no notes: what those items said survives only in what you have \
                     already said. `note` writes a finding down where pruning cannot reach it.\n",
                );
            }
        }
        if !changed.unchanged.is_empty() {
            out.push_str(&format!(
                "{} were already: {}\n",
                changed.unchanged.len(),
                numbers(&changed.unchanged)
            ));
        }
        for refusal in &refused {
            out.push_str(&format!("refused: {refusal}\n"));
        }
        out.push_str(&cost(kernel, before));

        ToolOutput::new(out)
    }

    /// Rewrites what one item says.
    fn revise(&self, kernel: &Kernel, call: &ToolCall, reason: &str) -> ToolOutput {
        let ids = ids(&call.args, "ids");
        let [id] = ids[..] else {
            return ToolOutput::error("`revise` takes exactly one id in `ids`");
        };
        let Some(content) = call.args["content"].as_str() else {
            return ToolOutput::error("`revise` needs the `content` to put there instead");
        };
        let Some(item) = kernel.item(id) else {
            return ToolOutput::error(format!("there is no context item {id}"));
        };
        if let Some(why) = protected(&item, &self.pinned.lock(), own_turn(kernel, &call.id)) {
            return ToolOutput::error(format!("[{id}] {why}"));
        }

        let before = kernel.budget().used();
        let was = item.tokens;
        if let Err(e) = kernel.replace(id, Content::text(content.to_owned())) {
            return ToolOutput::error(e.to_string());
        }

        // the note cannot be set without a second state change, and a second state change would
        // be a second thing to undo; the metadata slot exists for exactly this and costs no
        // checkpoint, so who rewrote this and why travels with the item either way
        let mut meta = match item.meta.is_object() {
            true => item.meta.clone(),
            false => json!({}),
        };
        meta["revised"] = json!({ "by": "amend", "reason": reason, "call": call.id.to_string() });
        let _ = kernel.annotate(id, meta);
        self.record(Undoing::Said(id, item.content.clone(), item.meta.clone()));

        let now = kernel.item(id).map(|item| item.tokens).unwrap_or_default();
        ToolOutput::new(format!(
            "[{id}] {} now says something else: ~{} tokens instead of ~{}. What it said before is \
             on the trace as `context.replaced`, is on the context pane under `enter`, and one \
             undo brings it back.\n{}",
            item.label,
            thousands(now),
            thousands(was),
            cost(kernel, before),
        ))
    }

    /// Writes something into the context that will still be there later.
    ///
    /// note: the one thing here that *adds*, and it earns its place because everything else an
    /// agent knows is in a turn - and a turn is the first thing a compactor comes for. A plan, a
    /// conclusion, a thing that did not work: written down as an item of its own it can be pinned,
    /// and a pin is a promise the kernel keeps even against a `Compactor`. Saying the same thing
    /// out loud in a turn is not a promise about anything.
    ///
    /// note: the source is `agent`, not `memory` or `user`, so that "who put these 12,000 tokens
    /// in here?" has an answer on the context pane. It is the item's own field for exactly this,
    /// and a tool that attributed its writing to somebody else would be the one dishonest thing
    /// in a program built to show where everything came from.
    fn note(&self, kernel: &Kernel, call: &ToolCall, reason: &str) -> ToolOutput {
        let Some(content) = call.args["content"]
            .as_str()
            .filter(|c| !c.trim().is_empty())
        else {
            return ToolOutput::error("`note` needs the `content` to write down");
        };
        let label = call.args["label"].as_str().unwrap_or("note");
        let pin = call.args["pin"].as_bool().unwrap_or(false);

        let before = kernel.budget().used();
        let id = kernel.push(
            ContextItem::new(ContextKind::Reference, "agent", label, content.to_owned())
                .because(reason.to_owned()),
        );
        if pin {
            kernel.set_state([id], ContextState::Pinned, Some(reason.to_owned()));
            self.note_pin(id, ContextState::Pinned);
        }
        // the way back from having written it is to put it away; nothing here destroys anything,
        // so an undone note is archived and still listed rather than gone
        self.record(Undoing::States(vec![(
            id,
            ContextState::Archived,
            Some("a note this tool wrote, and then walked back".to_owned()),
        )]));

        ToolOutput::new(format!(
            "[{id}] {label} is in your context now, and goes into every request from here on. {}\n{}",
            match pin {
                true => "It is pinned, so compaction cannot take it.",
                false =>
                    "It is not pinned, so compaction may take it; say `pin` if it has to last.",
            },
            cost(kernel, before),
        ))
    }

    /// Walks this tool's own amendments back, or forward again.
    ///
    /// note: the two directions are one loop over two stacks, because an [`Undoing`] applied hands
    /// back the way from where that left things to where they were. There is no separate "redo"
    /// representation to be written, or to fall out of step with the first one.
    fn walk(&self, kernel: &Kernel, call: &ToolCall, reason: &str, back: bool) -> ToolOutput {
        let steps = call.args["steps"].as_u64().unwrap_or(1).clamp(1, 64) as usize;
        let before = kernel.budget().used();

        let mut put_back = Vec::new();
        let mut touched = Vec::new();
        {
            let mut journal = self.journal.lock();
            for _ in 0..steps {
                let taken = match back {
                    true => journal.done.pop(),
                    false => journal.undone.pop(),
                };
                let Some(change) = taken else {
                    break;
                };
                put_back.push(change.about());

                // an item that has since gone gives nothing to walk to, and the entry is spent
                // either way rather than left to be retried against a context it no longer
                // describes
                let Some(inverse) = change.apply(kernel) else {
                    continue;
                };
                if let Undoing::States(states) = &inverse {
                    touched.extend(states.iter().map(|(id, ..)| *id));
                }
                match back {
                    true => journal.undone.push(inverse),
                    false => journal.done.push(inverse),
                }
            }
        }
        for id in touched {
            if let Some(item) = kernel.item(id) {
                self.note_pin(id, item.state);
            }
        }

        let (done, undone) = {
            let journal = self.journal.lock();
            (journal.done.len(), journal.undone.len())
        };
        let direction = match back {
            true => "back",
            false => "forward again",
        };

        let mut out = match put_back.is_empty() {
            true => format!(
                "there was nothing of yours to walk {direction}. `undo` and `redo` only move the \
                 changes this tool made; the person you work with has an undo of their own, and \
                 it is not this one.\n"
            ),
            false => format!(
                "walked {} of your own change(s) {direction}, because: {reason}\n  {}\n",
                put_back.len(),
                put_back.join("\n  "),
            ),
        };
        out.push_str(&format!(
            "{done} change(s) of yours can still be undone, {undone} redone.\n"
        ));
        out.push_str(&cost(kernel, before));

        ToolOutput::new(out)
    }

    /// Records an amendment, and makes whatever had been walked back unreachable.
    ///
    /// note: the same rule the kernel's own redo stack follows, and for the same reason: a redo
    /// that reached across work done since would be overwriting it rather than restoring anything.
    fn record(&self, undoing: Undoing) {
        let mut journal = self.journal.lock();
        journal.undone.clear();
        journal.done.push(undoing);
    }

    /// Remembers whether this tool is the one holding an item pinned.
    fn note_pin(&self, id: ContextId, state: ContextState) {
        let mut mine = self.pinned.lock();
        match state {
            ContextState::Pinned => mine.insert(id),
            _ => mine.remove(&id),
        };
    }
}

// ------------------------------------------------------------------------------------ helpers

/// The action the call names, or the fact that it names none.
fn action(args: &Value) -> Result<&str, BoxError> {
    args["action"]
        .as_str()
        .ok_or_else(|| "the `action` argument is required".into())
}

/// What to say about an action nobody implements.
fn unknown(action: &str, known: &[&str]) -> String {
    format!(
        "there is no `{action}`; this tool does {}",
        known.join(", ")
    )
}

/// The context ids in a named array argument.
fn ids(args: &Value, name: &str) -> Vec<ContextId> {
    args[name]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_u64())
                .map(ContextId)
                .collect()
        })
        .unwrap_or_default()
}

/// The state a word names.
fn state_of(word: &str) -> Option<ContextState> {
    Some(match word {
        "exclude" | "excluded" => ContextState::Excluded,
        "elide" | "elided" => ContextState::Elided,
        "archive" | "archived" => ContextState::Archived,
        "pin" | "pinned" => ContextState::Pinned,
        // there is one way back and a great many words for it. Four states hide an item or hold
        // it, and undoing any of them is the same move - so `unpin` and `unelide` are not
        // separate operations to be refused for not existing, they are this one spelled the way
        // somebody thought of it
        "restore" | "active" | "include" | "unelide" | "unexclude" | "unarchive" | "unpin" => {
            ContextState::Active
        }
        _ => return None,
    })
}

/// Why this item is not the model's to change, if it is not.
fn protected(
    item: &ContextItem,
    mine: &BTreeSet<ContextId>,
    own_turn: Option<ContextId>,
) -> Option<String> {
    if matches!(item.kind, ContextKind::System) {
        return Some("a system instruction, which belongs to whoever started this session".into());
    }
    if item.state == ContextState::Pinned && !mine.contains(&item.id) {
        return Some("pinned by the person you are working with, and a pin is a promise".into());
    }
    if own_turn == Some(item.id) {
        return Some("the assistant turn this very call is part of".into());
    }

    None
}

/// The item holding the assistant turn that asked for this call, if it is still there.
fn own_turn(kernel: &Kernel, call: &ToolCallId) -> Option<ContextId> {
    kernel.with_context(|context| {
        context
            .items()
            .iter()
            .rev()
            // `calls()`, or an ordered turn would look like a turn that asked for nothing and the
            // guard below it - that a call cannot excise the turn it is speaking in - would
            // quietly stop holding
            .find(|item| item.calls().any(|asked| &asked.id == call))
            .map(|item| item.id)
    })
}

/// What the next request costs now, beside what it cost before the change.
///
/// note: it says so when the change made the request *bigger*, which is not a rare accident. An
/// elided item is replaced by a marker carrying the reason somebody gave for eliding it, and on a
/// short item that reason is the larger of the two - a live session elided twenty-two items and
/// added 162 tokens doing it. Both numbers were already here and a model reading them carefully
/// could work it out; none of them did, and one went on to elide everything it had.
fn cost(kernel: &Kernel, before: usize) -> String {
    let budget = kernel.budget();
    let now = budget.used();
    format!(
        "the next request is now ~{} tokens{}, from ~{}.{}\n",
        thousands(now),
        budget
            .limit
            .map(|limit| format!(" of {}", thousands(limit)))
            .unwrap_or_default(),
        thousands(before),
        match now > before {
            true => format!(
                " That is {} more than before, not less: what an elided item leaves behind is a \
                 marker carrying your reason for eliding it, and on a short item that costs more \
                 than the content did.",
                thousands(now - before)
            ),
            false => String::new(),
        },
    )
}

/// A list of item numbers, as somebody would read them out.
fn numbers(ids: &[ContextId]) -> String {
    ids.iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(", ")
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
