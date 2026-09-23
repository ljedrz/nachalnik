//! `fork`: standing up a copy of this session and asking it something.
//!
//! note: its own tool rather than two more actions on the context, because it is the one thing in
//! here that neither reads nor changes a context - it makes a second session and pays a provider
//! for an answer. Letting something read its own items should not be letting it buy another
//! request, which is what allowing `context` would mean if these were actions of it. The subjects
//! say so too: `fork:draft` and `fork:ask` are a domain of their own, and a tool that declares two
//! domains is a tool that is two things.
//!
//! note: nothing in the runtime knows what a fork is. It is `snapshot` and `resume`, which the
//! runtime documents as the way to carry a session on somewhere else, pointed at a copy that is
//! thrown away - with the provider and the projector of the session it came from, no tools, no
//! compactor and one request.

use nachalnik::{
    BoxError, Capability, Config, ContextId, ContextItem, ContextState, Delta, Event, Kernel,
    OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

use crate::{
    app::text::thousands,
    tools::{
        Limits, domains,
        ops::{Arg, Op, action_of, actions, inner, schema},
    },
};

use super::{Reach, action, ids, unknown};

/// How long a fork may think before this looks up to see whether somebody has pressed escape.
const HEARTBEAT: Duration = Duration::from_millis(120);

/// The two ways to ask a copy something: carry on, or put a question.
fn ops() -> Vec<Op> {
    vec![
        Op::new(
            "draft",
            "answers the conversation as it stands, so you can read what you would say *before* \
             you say it and fix either the answer or the context",
            vec![],
        ),
        Op::new(
            "ask",
            "puts a question of your own to the copy, rather than letting it answer the \
             conversation - for weighing an approach, or, with `without`, for finding out whether \
             a piece of your context is what is leading you astray",
            vec![
                Arg::text("question", "what to put to the copy").needed(),
                Arg::list(
                    "without",
                    "integer",
                    "items the copy does not get to see, by number. Taking one away is what makes \
                     this an experiment rather than the same context answering twice",
                ),
            ],
        ),
    ]
}

/// Asks a throwaway copy of this session a question, or lets it answer the conversation.
pub struct Fork {
    reach: Reach,
    limits: Limits,
    ops: Vec<Op>,
    schema: Arc<Value>,
}

impl Fork {
    /// Builds one; see [`super::install`], which is the only caller.
    pub(super) fn new(reach: Reach, limits: Limits) -> Self {
        let ops = ops();
        Self {
            reach,
            limits,
            schema: Arc::new(schema(&ops)),
            ops,
        }
    }
}

#[async_trait]
impl Tool for Fork {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "fork",
            "asks a copy of you, on a copy of your context, and costs a request. A fork has no \
             tools: it can think, not act, and it answers once. Nothing it does reaches your \
             context, and nobody has read what it said.",
        )
        .with_schema(self.schema.clone())
        .with_capabilities(
            actions(&self.ops)
                .into_iter()
                .map(domains::fork)
                .collect::<Vec<_>>(),
        )
    }

    /// note: per operation, because they cost the same and mean different things. `draft` asks
    /// what this session would say next, which is a question about this context; `ask` puts words
    /// of the model's own to a copy, which is not.
    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        match action_of(call, &self.ops) {
            Some(op) => vec![domains::fork(op)],
            // an operation this tool does not have is refused by `invoke` with a list of the ones
            // it does; what it must not be is a call that needed nothing and was therefore allowed
            None => self.spec().capabilities,
        }
    }

    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits.for_call(&self.needs(call))
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        let args = match inner(&call.args) {
            Ok(args) => args,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        let args = &*args;

        let action = action(args)?;
        if let Some(refusal) = crate::tools::ops::unread(action, args, &self.ops) {
            return Ok(ToolOutput::error(refusal));
        }
        match action {
            "draft" => branch(&kernel, None, &[], &output).await,
            "ask" => {
                let Some(question) = args["question"].as_str() else {
                    return Ok(ToolOutput::error(
                        "`ask` needs a `question` to put to the copy; `draft` is the one that \
                         just carries on the conversation",
                    ));
                };
                let without = match ids(args, "without") {
                    Ok(without) => without,
                    Err(why) => return Ok(ToolOutput::error(why)),
                };
                branch(&kernel, Some(question), &without, &output).await
            }
            other => Ok(ToolOutput::error(unknown(other, &actions(&self.ops)))),
        }
    }
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
    // whose items the count is about: what follows adds a system instruction of this tool's own,
    // and `ask` adds the question - neither of them the caller's, so neither is counted. A fork is
    // worth having because two runs are comparable, and a number that moves with which of the two
    // operations asked for it is not
    let theirs: std::collections::BTreeSet<ContextId> =
        fork.items().iter().map(|item| item.id).collect();
    // note: said out loud, because the copy cannot work it out. It inherits a conversation full of
    // tool calls and their results and no tool definitions at all, and a model reading that asks
    // for a tool - which nothing here can run, so the answer comes back as a call and no words.
    // Without being told, a model does this as a rule rather than as a corner case
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
    let items = fork
        .project()
        .included
        .iter()
        .filter(|id| theirs.contains(id))
        .count();

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
    // note: said either way, because "on 9 of your items" does not say whether that is all of them,
    // and a fork's worth is which items the copy did not get. A model can ask a copy to disregard
    // something rather than take it away with `without`, and report the matching answer as an
    // ablation - the copy was asked to pretend, and nothing in the reply would distinguish the
    // two. Where the copy really did see everything, the reply says so.
    match left_out.is_empty() {
        true => out.push_str(
            ". Nothing of yours was taken away, so this is the same context answering again \
             rather than a test of what any of it was doing. `without` takes items away from the \
             copy, and a question that asks it to disregard something is not the same thing - it \
             is still reading it.",
        ),
        false => {
            let numbers: Vec<String> = left_out.iter().map(|id| id.to_string()).collect();
            out.push_str(&format!(
                ", without {}, which the copy could not read at all.",
                numbers.join(", ")
            ));
        }
    }
    out.push_str(
        " None of this is in your context and nobody has read it; it is yours to use or drop.\n",
    );
    if let Some(usage) = response.usage {
        out.push_str(&format!(
            "it cost {} in / {}.\n",
            thousands(usage.input_tokens.unwrap_or_default() as usize),
            crate::app::text::charged(&usage),
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
    // cuts from the end. On a reasoning model the thinking is the bulk of a fork, so with the
    // thinking first the limit eats the answer and leaves the deliberation about how to answer.
    // Nothing about this order is a claim about what the copy did; the two sections are labelled
    // and the reasoning says it is reasoning
    if let Some(reasoning) = &response.reasoning {
        out.push_str(&format!(
            "\n--- its reasoning, which it produced before the answer above ---\n{}\n",
            reasoning.to_text()
        ));
    }

    Ok(ToolOutput::new(out))
}
