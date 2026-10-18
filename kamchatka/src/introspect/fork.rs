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
    BoxError, Capability, Config, ContextId, ContextItem, ContextKind, ContextState, Delta, Event,
    Kernel, OutputSink, Tool, ToolCall, ToolCallId, ToolOutput, ToolSpec, async_trait,
};
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::{
    app::text::thousands,
    tools::{
        Limits, domains,
        ops::{Arg, Layout, Op, action_of, actions, inner, schema},
    },
};

use super::{Reach, action, ids, if_reachable, unknown};

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

/// What the last fork of a turn read, and which turn that was.
///
/// note: one entry rather than a per-turn map, because a turn is over before the next one starts
/// and the next one overwrites it - so a fork asked alone is always the first of its turn, and a
/// fork beside any other call knows whether the call before it read the same items.
#[derive(Default)]
struct Read {
    /// The assistant turn whose calls these are.
    turn: Option<ContextId>,
    /// The caller's item identifiers the last fork of that turn actually read.
    items: Vec<ContextId>,
}

/// Asks a throwaway copy of this session a question, or lets it answer the conversation.
pub struct Fork {
    reach: Reach,
    limits: Limits,
    forked: Arc<AtomicU64>,
    read: Arc<std::sync::Mutex<Read>>,
    ops: Vec<Op>,
    schema: Arc<Value>,
}

impl Fork {
    /// Builds one; see [`super::install`], which is the only caller, and which holds `forked`.
    pub(super) fn new(
        reach: Reach,
        limits: Limits,
        forked: Arc<AtomicU64>,
        layout: Layout,
    ) -> Self {
        let ops = ops();
        Self {
            reach,
            limits,
            forked,
            read: Arc::default(),
            schema: Arc::new(schema(&ops, layout)),
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
             context: what it says comes back to you alone, to use or drop. Forks asked in one \
             turn do not read one another's answers, and a call of yours that writes to the \
             context is said when it gave one fork something the other did not have.",
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
            // `invoke` refuses an operation this tool does not have, with a list of the ones it
            // does. Such a call must not be one that needed nothing and was therefore allowed
            None => self.spec().capabilities,
        }
    }

    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits.for_call(&self.needs(call))
    }

    fn unparsed(&self, written: &str) -> Option<String> {
        crate::tools::ops::advice(written, Layout::of(&self.schema))
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let kernel = self.reach.kernel()?;

        let args = match inner(&call.args) {
            Ok(args) => args,
            Err(misread) => return Ok(ToolOutput::error(misread.said(Layout::of(&self.schema)))),
        };
        let args = &*args;

        let action = action(args)?;
        if let Some(refusal) = crate::tools::ops::unread(action, args, &self.ops) {
            return Ok(ToolOutput::error(refusal));
        }
        match action {
            "draft" => {
                branch(
                    &kernel,
                    Asked {
                        call: &call.id,
                        question: None,
                        without: &[],
                    },
                    &output,
                    &self.forked,
                    &self.read,
                    || {},
                )
                .await
            }
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
                // before the request rather than after it: a copy asked without an item that is
                // not there is the whole context answering again, paid for as an ablation
                if let Some(missing) = without.iter().find(|id| kernel.item(**id).is_none()) {
                    let numbers =
                        if_reachable(&kernel, self.reach.policy(), "context:look", || {
                            " `context` prints".to_owned()
                        });
                    return Ok(ToolOutput::error(format!(
                        "there is no item {missing} to leave out, so the copy was not asked. \
                         `without` takes the numbers{numbers}."
                    )));
                }
                branch(
                    &kernel,
                    Asked {
                        call: &call.id,
                        question: Some(question),
                        without: &without,
                    },
                    &output,
                    &self.forked,
                    &self.read,
                    || {},
                )
                .await
            }
            other => Ok(ToolOutput::error(unknown(other, &actions(&self.ops)))),
        }
    }
}

/// What one call asked a fork: which call it was, the question if it put one, and what to leave out.
struct Asked<'a> {
    call: &'a ToolCallId,
    question: Option<&'a str>,
    without: &'a [ContextId],
}

/// Answers on a copy of the context, and hands back only what was said.
///
/// note: the copy is a whole second [`Kernel`] resumed from a [`Snapshot`](nachalnik::Snapshot) of
/// this one. Forking is what `snapshot` and `resume` already are, so nothing was added to the
/// runtime. It gets this session's provider and projector, answering the same model in the same
/// dialect, and gets **no tools**, no compactor and a limit of one request. A fork can think; it
/// cannot act, and it cannot go on thinking after it has answered once.
///
/// note: nothing the fork does reaches this session's context or event log; it has a log of its own
/// that goes when it does. What it is visible as is the text it streams, relayed into this tool's
/// own [`OutputSink`], so a person watching the terminal sees a fork thinking rather than a tool
/// that has gone quiet.
///
/// note: the copy is taken as the context stood when the turn asked for it, not as it stands when
/// this call runs. A turn's calls run one after another, so two forks asked together would
/// otherwise differ by the first one's answer: the second would be handed what the first said,
/// spoiling the comparison. And `without` cannot keep out a result that does not exist yet. So the
/// results of the other calls in the same turn are left out - excluded rather than deleted like a
/// `without` - and not counted among the items asked about.
///
/// note: a call that writes to the context rather than answering one is not covered by that: a note
/// is not a result, and a snapshot does not say which call put an item there. The copy is taken as
/// the context stands and compared with the copy the fork beside it was given, so two forks of a
/// turn are either alike or the reply says how they are not.
///
/// note: `looked` is called once the copy is taken, and is nothing but a place for a test to stand
/// between the copy and what is made of it.
///
/// note: what it was charged is added to `forked` for the session's ceiling. Read off the fork's
/// own log once its turn is over, rather than off the stream the relay reads: the relay is stopped
/// as soon as the turn ends, and a figure it had not reached yet would be spent and never counted.
async fn branch(
    kernel: &Kernel,
    asked: Asked<'_>,
    output: &OutputSink,
    forked: &AtomicU64,
    last_read: &std::sync::Mutex<Read>,
    looked: impl FnOnce(),
) -> Result<ToolOutput, BoxError> {
    let Asked {
        call: asking,
        question,
        without,
    } = asked;
    let Some(provider) = kernel.provider() else {
        return Ok(ToolOutput::error("there is no provider to ask"));
    };

    // note: held from before the copy is taken until what it read is written down, so that two
    // forks of a turn compare their copies in the order they took them. Taken only to compare, two
    // forks under `--parallel-tool-calls` could copy the context either side of a call writing to
    // it and then lock the other way round - the later copy first, finding nobody before it, and
    // the earlier one second, finding nothing the later did not have - and both said they were
    // the same context. Held in a block of its own, because nothing in it awaits and the future
    // that held it across an await could not be sent between threads
    let (fork, items, left_out, unanswered, extra) = {
        // a panic here - the projector is somebody else's code - leaves what was read last as it
        // was, since that is written only at the end, so the lock is taken back rather than every
        // later fork failing on it
        let mut last = last_read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut snapshot = kernel.snapshot();
        looked();
        let asking_in = snapshot
            .items
            .iter()
            .find(|item| item.calls().any(|call| &call.id == asking))
            .map(|turn| {
                (
                    turn.id,
                    turn.calls().map(|call| call.id.clone()).collect::<Vec<_>>(),
                )
            });
        let siblings: std::collections::HashSet<ToolCallId> = asking_in
            .as_ref()
            .map(|(_, calls)| {
                calls
                    .iter()
                    .filter(|call| *call != asking)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let mut left_out = Vec::new();
        let mut unanswered = Vec::new();
        for item in &mut snapshot.items {
            // named first: a sibling's result the caller also named is one it asked to leave out
            if without.contains(&item.id) {
                // excluded rather than deleted, so the fork can still name the item by the number
                // this session knows it by
                item.state = ContextState::Excluded;
                item.note = Some("left out of this fork".into());
                left_out.push(item.id);
            } else if let ContextKind::ToolResult { call, .. } = &item.kind
                && siblings.contains(call)
            {
                item.state = ContextState::Excluded;
                item.note = Some("answered in the same turn this fork was asked in".into());
                unanswered.push(item.id);
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
        // The count is about the fork's items. What follows adds a system instruction of this
        // tool's own, and `ask` adds the question; neither is the caller's, so neither is counted.
        // A fork is worth having because two runs are comparable, and a count that moved with
        // which of the two operations asked for it would not be
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
        let read: Vec<ContextId> = fork
            .project()
            .included
            .iter()
            .filter(|id| theirs.contains(id))
            .copied()
            .collect();
        let items = read.len();
        // note: what the previous fork read, where it was another fork of this turn. Two forks in a
        // turn are the comparison `fork` exists for. A call that writes to the context, such as
        // `context` with a `note`, adds an item the previous fork did not have, so the two copies
        // differ even when siblings are excluded. Nothing in the snapshot says which call wrote an
        // item, so this compares the two rather than guessing, and the sentence below is weakened
        // by what it finds
        let extra: Vec<ContextId> = match asking_in.as_ref().map(|(turn, _)| *turn) {
            Some(turn) => {
                let earlier = (last.turn == Some(turn)).then(|| last.items.clone());
                last.turn = Some(turn);
                last.items = read.clone();
                // in the order the copy has them, so the sentence reads like a context
                earlier.map_or_else(Vec::new, |earlier| {
                    let earlier: std::collections::BTreeSet<ContextId> =
                        earlier.into_iter().collect();
                    read.into_iter()
                        .filter(|id| !earlier.contains(id))
                        .collect()
                })
            }
            None => Vec::new(),
        };

        (fork, items, left_out, unanswered, extra)
    };

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

    // It uses the same heartbeat as the `shell` tool, for the same reason: the fork is a whole
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
    let charged: u64 = fork
        .history()
        .iter()
        .filter_map(|record| match &record.event {
            Event::ModelFinished {
                usage: Some(usage), ..
            } => Some(usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0)),
            _ => None,
        })
        .sum();
    forked.fetch_add(charged, Ordering::Relaxed);

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
    // note: said either way, because "on 9 of your items" does not say whether that is all of them.
    // A fork's worth is which items the copy did not get. A model can ask a copy to disregard
    // something rather than take it away with `without`, and report the matching answer as an
    // ablation: the copy was asked to pretend, and nothing in the reply distinguishes the two.
    // Where the copy really did see everything, the reply says so.
    //
    // note: weakened to what was named, because something else may have been kept away. Two forks
    // in one turn, or a fork beside another call whose result already landed, leaves the copy an
    // item short of the caller's context. "Nothing of yours was taken away" beside a count that
    // came up short is read as "these two runs saw the same context", which is the inference two
    // forks in a turn exist to support. What the other call kept out is a clause of its own below.
    //
    // note: the sentence stands or falls on the copy being the caller's context when the turn
    // began. A call that writes to the context makes that untrue for every fork after it. Siblings
    // above are excluded by the tool that answered them, and a note is nobody's result, so the two
    // copies are compared rather than the items attributed. Where they differ, the reply says what
    // the later copy has that the earlier one did not.
    match (left_out.is_empty(), extra.is_empty()) {
        // note: the claim compares this copy to the caller's context and is true only if no earlier
        // call in the turn added to the context. A sibling that writes to the context (`context`
        // with a `note`) does add to it, so the copy holds something the fork beside it did not;
        // the claim says nothing about that
        (true, false) => {
            out.push_str(". Nothing you named with `without` was taken away.");
            out.push_str(&written_beside(&extra));
        }
        (true, true) => out.push_str(
            ". Nothing you named with `without` was taken away, so this is the same context \
             answering again rather than a test of what any of it was doing. `without` takes items \
             away from the copy, and a question that asks it to disregard something is not the same \
             thing - it is still reading it.",
        ),
        (false, _) => {
            let numbers: Vec<String> = left_out.iter().map(|id| id.to_string()).collect();
            out.push_str(&format!(
                ", without {}, which the copy could not read at all.",
                numbers.join(", ")
            ));
            // The two copies differ by what a sibling wrote as well as by what was named; taking
            // items away does not make that any less true
            if !extra.is_empty() {
                out.push_str(&written_beside(&extra));
            }
        }
    }
    // The calls beside this one kept items out of the copy; that is a fact about the context it was
    // asked on, not about the question it was asked
    if !unanswered.is_empty() {
        let numbers: Vec<String> = unanswered.iter().map(|id| id.to_string()).collect();
        out.push_str(&match numbers.as_slice() {
            // note: answered by the time the copy was taken, or there would be no item to leave
            // out - what keeps it out is that it came after the question
            [one] => format!(
                " Your other call in this turn was answered after you asked, so its result \
                 ({one}) is left out of this copy."
            ),
            _ => format!(
                " Your other calls in this turn were answered after you asked, so their results \
                 ({}) are left out of this copy.",
                numbers.join(", ")
            ),
        });
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
        // it asked for a tool instead of answering, and a fork has nothing to run one. Saying so
        // beats a blank: a caller reading an empty draft cannot tell a copy that had nothing to say
        // from one that tried to do something
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

    // note: the answer comes before the thinking, the opposite of the order it was produced in. An
    // output limit cuts from the end, and on a reasoning model the thinking is the bulk of a fork,
    // so thinking first would make the limit eat the answer and leave the deliberation about how to
    // answer. This order is not a claim about what the copy did; the sections are labelled, and the
    // reasoning says it is reasoning
    if let Some(reasoning) = &response.reasoning {
        out.push_str(&format!(
            "\n--- its reasoning, which it produced before the answer above ---\n{}\n",
            reasoning.to_text()
        ));
    }

    Ok(ToolOutput::new(out))
}

/// What a copy has that the fork beside it in the turn did not, because another call wrote to the
/// context between the two; a sentence to follow the rest of the reply.
fn written_beside(extra: &[ContextId]) -> String {
    let numbers: Vec<String> = extra.iter().map(|id| id.to_string()).collect();
    format!(
        " Another call in this turn wrote to your context while this copy was being taken, so the \
         copy also has {} that the fork beside it in this turn did not - the two are not the same \
         context and their answers are not a comparison.",
        numbers.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex, atomic::AtomicU64, mpsc},
        time::Duration,
    };

    use nachalnik::{
        Config, ContextItem, Kernel, ModelResponse, OutputSink, ToolCallId,
        test::{ScriptedProvider, call},
    };
    use serde_json::json;

    use super::{Asked, Read, branch};

    /// Two forks of a turn compare their copies in the order they took them, whichever reaches the
    /// comparison first.
    ///
    /// note: the order the kernel runs a turn's calls in under `--parallel-tool-calls` is a
    /// `JoinSet`'s, which no test can choose, so this stands between the first fork's copy and its
    /// comparison and runs the second there: something is written to the context, and the second
    /// fork is let go for as long as it takes to copy it. Compared in the order they reached the
    /// lock, the second fork found nobody before it and the first found nothing the second did not
    /// have, and both said they were the same context.
    #[test]
    fn forks_compare_their_copies_in_the_order_they_took_them() {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(Arc::new(ScriptedProvider::new([
            ModelResponse::text("one way"),
            ModelResponse::text("the other way"),
        ])));
        kernel.push(ContextItem::user("go"));
        kernel.push(ContextItem::assistant(
            "",
            vec![
                call("c1", "fork", json!({ "action": "ask" })),
                call("c2", "fork", json!({ "action": "ask" })),
            ],
        ));
        let last_read = Arc::new(Mutex::new(Read::default()));
        let forked = Arc::new(AtomicU64::new(0));
        let ask = |kernel: Kernel,
                   last_read: Arc<Mutex<Read>>,
                   forked: Arc<AtomicU64>,
                   asking: &'static str,
                   looked: Box<dyn FnOnce() + Send>| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime");
            let output = runtime.block_on(branch(
                &kernel,
                Asked {
                    call: &ToolCallId::from(asking),
                    question: Some("which way?"),
                    without: &[],
                },
                &OutputSink::disconnected(),
                &forked,
                &last_read,
                looked,
            ));
            output
                .expect("the fork answered")
                .content
                .to_text()
                .into_owned()
        };

        let (copied, taken) = mpsc::channel();
        let second = {
            let (kernel, last_read, forked) = (kernel.clone(), last_read.clone(), forked.clone());
            move || {
                ask(
                    kernel,
                    last_read,
                    forked,
                    "c2",
                    Box::new(move || {
                        let _ = copied.send(());
                    }),
                )
            }
        };
        let (written, running) = (Arc::new(Mutex::new(None)), Arc::new(Mutex::new(None)));
        let first = ask(
            kernel.clone(),
            last_read,
            forked,
            "c1",
            Box::new({
                let (kernel, written, running) = (kernel.clone(), written.clone(), running.clone());
                move || {
                    *written.lock().unwrap() =
                        Some(kernel.push(ContextItem::user("written between the two copies")));
                    *running.lock().unwrap() = Some(std::thread::spawn(second));
                    // the second fork takes a copy before this fork compares if it can. It cannot
                    // when the lock is held from before the copy, so this waits
                    let _ = taken.recv_timeout(Duration::from_millis(300));
                }
            }),
        );
        let second = running
            .lock()
            .unwrap()
            .take()
            .expect("the second fork was started")
            .join()
            .expect("the second fork answered");
        let written = written.lock().unwrap().expect("the item was written");

        assert!(
            first.contains("the same context answering again"),
            "the first copy has nobody before it: {first}"
        );
        assert!(
            second.contains("wrote to your context") && second.contains(&written.to_string()),
            "the second copy has an item the first did not, and says which: {second}"
        );
    }
}
