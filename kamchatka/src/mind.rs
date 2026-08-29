//! Two tools that hand the agent its own context: one that reads it, one that changes it.
//!
//! note: Everything here is ordinary user code, like the rest of `tools.rs`, and none of it
//! needed a line added to the runtime. What the runtime has is a context that is a list of public
//! values, a request that can be built without being sent, and a session that can be snapshotted
//! and resumed - and a tool is allowed to call all of it. That is the whole trick: metacognition
//! is not a feature of the kernel, it is what a tool can already do with the kernel's ordinary
//! surface. What these two add is the part the kernel has no opinion about: which of it a *model*
//! may do.
//!
//! note: Two tools rather than one with an `action` argument, because a [`ToolSpec`] declares its
//! capabilities once for every call it will ever receive. One tool would mean that answering
//! *always* to "may it look at its own context?" also answered "may it rewrite a tool result?" -
//! a grant that delivers considerably more than it implies, which is the shape of thing this
//! program exists not to do. So [`Mind`] looks and [`Amend`] changes, they declare different
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
    BoxError, Capability, Config, Content, ContextId, ContextItem, ContextKind, ContextState,
    Delta, Event, Kernel, OutputSink, Tool, ToolCall, ToolCallId, ToolOutput, ToolSpec,
    async_trait,
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

    kernel.add_tool(Arc::new(Mind {
        reach: reach.clone(),
    }));
    kernel.add_tool(Arc::new(Amend {
        reach,
        pinned: Mutex::new(BTreeSet::new()),
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

/// Reads the agent's own context, the request it is about to send, and what it would say next.
///
/// note: none of the four actions changes anything, which is why they are together and why the
/// capability they declare is its own. `draft` and `fork` do spend tokens - they ask the model -
/// so this is not free, only harmless.
pub struct Mind {
    reach: Reach,
}

#[async_trait]
impl Tool for Mind {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "mind",
            "looks at your own state. `look` lists every item in your context - what it is, what \
             it costs, whether it is going into the next request, and why not if it is not - and \
             with `ids` reads any of them in full, your own recorded reasoning included. \
             `request` shows the request you are about to send, message by message, with what the \
             projector left out and what it repaired. `draft` answers the conversation on a \
             throwaway copy of your context and shows you what you would say, without saying it \
             or recording it. `fork` puts a question to a copy of yourself on a copy of your \
             context, optionally with some items left out, and returns its answer only. A fork \
             has no tools: it can think, not act. Use `amend` to change any of this.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["look", "request", "draft", "fork"],
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
            "look" => Ok(ToolOutput::new(look(&kernel, &ids(&call.args, "ids")))),
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
                &["look", "request", "draft", "fork"],
            ))),
        }
    }
}

/// The context, item by item, or the whole of the named ones.
fn look(kernel: &Kernel, ids: &[ContextId]) -> String {
    let items = kernel.items();
    if !ids.is_empty() {
        return ids
            .iter()
            .map(|id| full(&items, *id))
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
        "\n`look` with `ids` reads any of these in full, including the reasoning recorded on an \
         assistant turn.\n",
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
    if let ContextKind::AssistantMessage {
        tool_calls,
        reasoning,
    } = &item.kind
    {
        if !tool_calls.is_empty() {
            said.push_str(&format!(" [{} call(s)]", tool_calls.len()));
        }
        if reasoning.is_some() {
            said.push_str(" [+reasoning]");
        }
    }
    if let Some(note) = &item.note {
        said.push_str(&format!(" · {note}"));
    }

    said
}

/// The whole of one item, or the fact that there is no such item.
fn full(items: &[Arc<ContextItem>], id: ContextId) -> String {
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
    // the reasoning first, because on the turn that carries it it is the part that explains the
    // rest, and because it is the one thing here the model cannot see in the request itself
    if let Some(reasoning) = item.reasoning() {
        out.push_str(&format!("  --- reasoning ---\n{}\n", reasoning.to_text()));
    }
    out.push_str(&format!("  --- content ---\n{}\n", item.content.to_text()));

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
    out.push_str(&format!(
        "\n--- what it said ({:?}) ---\n{}\n",
        response.stop,
        response
            .content
            .as_ref()
            .map(|c| c.to_text())
            .unwrap_or_default(),
    ));

    Ok(ToolOutput::new(out))
}

// ----------------------------------------------------------------------------------- changing

/// Changes the agent's own context: prunes it, rewrites an item, walks its own changes back.
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
    pinned: Mutex<BTreeSet<ContextId>>,
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
            "changes your own context. `prune` moves items to a state: `exclude` takes one out of \
             the request entirely, `elide` leaves a marker in its place so the turn keeps its \
             shape, `archive` puts it away for good, `pin` protects it from compaction, `restore` \
             puts it back. `revise` rewrites what one item says. `undo` and `redo` walk back and \
             forward through the changes *you* made with this tool, newest first; they do not \
             touch anything anybody else did. Nothing here destroys anything: every item keeps \
             its number, everything can be restored, and every change is recorded where the \
             person at the terminal can see it. Some things are refused: a pinned item, a system \
             instruction, and the assistant turn you are speaking in are not yours to change. A \
             reason is required, and it is what the person will read. Use `mind` to look first.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["prune", "revise", "undo", "redo"],
                },
                "ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "prune: the items to move. revise: exactly one item",
                },
                "state": {
                    "type": "string",
                    "enum": ["exclude", "elide", "archive", "pin", "restore"],
                    "description": "prune: where they go",
                },
                "content": {
                    "type": "string",
                    "description": "revise: what the item should say instead",
                },
                "reason": {
                    "type": "string",
                    "description": "why, in your own words; the person at the terminal reads this",
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
            "undo" => Ok(self.walk(&kernel, call, reason, true)),
            "redo" => Ok(self.walk(&kernel, call, reason, false)),
            other => Ok(ToolOutput::error(unknown(
                other,
                &["prune", "revise", "undo", "redo"],
            ))),
        }
    }
}

impl Amend {
    /// Moves items to a state, refusing the ones that are not the model's to move.
    fn prune(&self, kernel: &Kernel, call: &ToolCall, reason: &str) -> ToolOutput {
        let ids = ids(&call.args, "ids");
        if ids.is_empty() {
            return ToolOutput::error("`prune` needs `ids`");
        }
        let Some(state) = state_of(call.args["state"].as_str().unwrap_or_default()) else {
            return ToolOutput::error(
                "`state` must be one of exclude, elide, archive, pin, restore",
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
             on the trace as `context.replaced`, and one undo brings it back.\n{}",
            item.label,
            thousands(now),
            thousands(was),
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
                 changes this tool made; the person at the terminal has an undo of their own, and \
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
        "restore" | "active" => ContextState::Active,
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
        return Some("pinned by the person at the terminal, and a pin is a promise".into());
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
            .find(|item| match &item.kind {
                ContextKind::AssistantMessage { tool_calls, .. } => {
                    tool_calls.iter().any(|asked| &asked.id == call)
                }
                _ => false,
            })
            .map(|item| item.id)
    })
}

/// What the next request costs now, beside what it cost before the change.
fn cost(kernel: &Kernel, before: usize) -> String {
    let budget = kernel.budget();
    format!(
        "the next request is now ~{} tokens{}, from ~{}.\n",
        thousands(budget.used()),
        budget
            .limit
            .map(|limit| format!(" of {}", thousands(limit)))
            .unwrap_or_default(),
        thousands(before),
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
