//! Deciding on a model's tool calls, running them, and recording what they produced.
//!
//! note: the private half of [`super::Kernel`]'s execution path; see the sibling `request`
//! module for why it is not in `mod.rs`. The division is the state machine's own: everything
//! here happens between the model asking for tools and the machine coming back to
//! [`super::State::Idle`].

use std::sync::{Arc, atomic::Ordering::SeqCst};

use crate::{
    context::{ContextId, ContextItem, ContextState},
    error::Result,
    event::{Event, OutputSink},
    model::ToolCall,
    permissions::{Grant, GrantSource, PermissionId, PermissionRequest, Verdict},
    tool::{Tool, ToolOutput},
};

use super::{Batch, Kernel, PreparedCall, Restore, State};

impl Kernel {
    /// Matches the model's calls to tools, asks the policy about each, and queues them.
    ///
    /// note: Nothing about a permission is announced until every call is queued, and the
    /// announcements and the move of the state machine happen under one hold of the machine lock,
    /// so a client can see a question only once there is one to answer. Its whole job here is to
    /// answer the question it is handed, and it would not be much of a runtime if
    /// [`Kernel::decide`] could fail purely because the client was quick about it - which it
    /// would, if a request were announced while the policy was still being consulted about the
    /// *next* call in the batch.
    ///
    /// `turn` is the batch the turn asking for them was recorded in, which an answer to a call
    /// nobody can run joins.
    pub(super) async fn prepare_calls(&self, calls: &[ToolCall], mut turn: Batch) -> State {
        let mut prepared = Vec::with_capacity(calls.len());
        let mut announcements = Vec::with_capacity(calls.len());

        for call in calls {
            self.emit(Event::ToolRequested {
                call: call.id.clone(),
                tool: call.tool.clone(),
                args: call.args.clone(),
            });

            let Some(tool) = self.tool(&call.tool) else {
                self.emit(Event::ToolUnknown {
                    call: call.id.clone(),
                    tool: call.tool.clone(),
                });
                // joining the checkpoint the turn was recorded under, a moment ago: the turn and
                // the kernel's answer to a call nobody can run are one thing that happened, and a
                // checkpoint each would let one `undo` leave a call answered and its neighbour not
                self.record_tool_result(
                    call,
                    ToolOutput::error(unknown_tool(&call.tool, &self.tool_ids())),
                    None,
                    None,
                    &mut turn,
                );
                continue;
            };

            let id = PermissionId(self.0.next_permission.fetch_add(1, SeqCst));
            // what the call needs rather than what the tool might: see `Tool::needs`
            let request = PermissionRequest::new(id, call, tool.needs(call));

            let grant = match self.policy().evaluate(&request).await {
                Verdict::Allow => Some((Grant::Allow, GrantSource::Policy)),
                Verdict::Deny => Some((Grant::Deny, GrantSource::Policy)),
                Verdict::Ask => None,
            };

            announcements.push(match grant {
                Some((grant, source)) => Event::PermissionDecided {
                    id,
                    call: call.id.clone(),
                    tool: call.tool.clone(),
                    grant,
                    source,
                },
                None => Event::PermissionRequested {
                    request: request.clone(),
                },
            });

            prepared.push(PreparedCall {
                call: call.clone(),
                tool,
                request,
                grant,
            });
        }

        // the calls are queued, announced and the machine moved without letting go of the lock,
        // so that by the time anybody can see a `permission.requested` there is a request to
        // answer, and a `Kernel::decide` racing this simply waits its turn
        let mut machine = self.0.machine.lock();
        let to = Self::state_for(&prepared);
        machine.pending = prepared;
        for announcement in announcements {
            self.emit(announcement);
        }
        self.transition(&mut machine, to.clone());

        to
    }

    /// Runs the claimed calls, in the order the model asked for them.
    pub(super) async fn execute(&self, prepared: Vec<PreparedCall>) -> Result<State> {
        // note: if this future is dropped, the calls it had claimed are gone with it; their
        // results are simply never recorded, and the projector drops the orphaned calls from
        // the next request
        let mut restore = Restore::new(self, State::Idle);

        if self.0.config.parallel_tool_calls {
            // whatever order they finished in, they are recorded in the order the model asked
            // for them, so that a context does not depend on which tool happened to be quick
            let outputs = self.invoke_together(&prepared).await;
            let mut batch = Batch::default();
            for (prepared, output) in prepared.iter().zip(outputs) {
                self.record_output(prepared, output, &mut batch);
            }
        } else {
            // one at a time, and each one recorded before the next begins, so that a client
            // watching the stream sees a call finish rather than a batch of them
            let mut batch = Batch::default();
            for prepared in &prepared {
                // an interrupt stops the ones that have not started. They are still recorded,
                // and recorded as not having run, because a call with no result at all would
                // leave the model looking at a question nobody answered
                let output = match self.is_interrupted() {
                    true => ToolOutput::error("interrupted before this call was made"),
                    false => {
                        self.invoke(
                            prepared.tool.clone(),
                            prepared.call.clone(),
                            prepared.request.clone(),
                            prepared.grant,
                        )
                        .await
                    }
                };
                self.record_output(prepared, output, &mut batch);
            }
        }

        self.transition(&mut self.0.machine.lock(), State::Idle);
        restore.disarm();

        Ok(State::Idle)
    }

    /// Runs the calls at the same time; see
    /// [`Config::parallel_tool_calls`](crate::Config::parallel_tool_calls).
    async fn invoke_together(&self, prepared: &[PreparedCall]) -> Vec<ToolOutput> {
        let mut running = tokio::task::JoinSet::new();
        for (index, call) in prepared.iter().enumerate() {
            let (kernel, tool, grant) = (self.clone(), call.tool.clone(), call.grant);
            let request = call.request.clone();
            let call = call.call.clone();
            running.spawn(async move { (index, kernel.invoke(tool, call, request, grant).await) });
        }

        let mut outputs: Vec<Option<ToolOutput>> = (0..prepared.len()).map(|_| None).collect();
        while let Some(finished) = running.join_next().await {
            match finished {
                Ok((index, output)) => outputs[index] = Some(output),
                // a tool that panics unwinds through `step` exactly as it does when the calls
                // run one at a time; being run beside another one does not make it survivable
                Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                Err(_) => {}
            }
        }

        outputs
            .into_iter()
            .map(|output| {
                output.unwrap_or_else(|| ToolOutput::error("the call did not run to completion"))
            })
            .collect()
    }

    /// Runs one call. A refusal and a failure are both outputs, because the model is told about
    /// them either way.
    async fn invoke(
        &self,
        tool: Arc<dyn Tool>,
        call: ToolCall,
        request: PermissionRequest,
        grant: Option<(Grant, GrantSource)>,
    ) -> ToolOutput {
        let (grant, source) = grant.expect("every claimed call has been decided");
        if grant == Grant::Deny {
            // asked only where the policy is what refused it. `refusal` puts a reason into the
            // standing-rule wording and into no other, so asking anywhere else computes an
            // explanation of somebody else's decision and drops it
            let why = match source {
                GrantSource::Policy => self.policy().why(&request),
                _ => None,
            };

            return ToolOutput::error(refusal(source, why));
        }

        self.emit(Event::ToolStarted {
            call: call.id.clone(),
            tool: call.tool.clone(),
        });

        // a tool that fails is not a kernel failure: the model is told, and the loop goes on
        let sink = OutputSink::new(self.clone(), call.id.clone(), call.tool.clone());
        match tool.invoke(&call, sink).await {
            Ok(output) => output,
            Err(e) => ToolOutput::error(e.to_string()),
        }
    }

    /// Records what a call produced, keeping the whole of it when a limit shortened it.
    ///
    /// note: every call of a batch is recorded into one [`Batch`] - the shape
    /// [`Kernel::cancel_pending_calls`] uses, for the reason it gives. Running the calls a turn
    /// asked for is one thing that happened, and a checkpoint each would let one `undo` leave
    /// some of them answered and the last one never mentioned - which is also what a checkpoint
    /// of somebody else's, landing between two of them, would do if it were not folded in.
    fn record_output(&self, prepared: &PreparedCall, mut output: ToolOutput, batch: &mut Batch) {
        // an output limit decides what the *model* is shown. It is not permission to throw the
        // rest away, so unless the user has said otherwise the whole of it goes into the context
        // too - archived, listed, inspectable, and restorable like anything else
        let limit = prepared
            .tool
            .limit(&prepared.call)
            .or(self.0.config.default_tool_output_limit);
        let over = limit.is_some_and(|limit| output.content.byte_len() > limit);

        let whole = (over && self.0.config.keep_truncated_output).then(|| {
            let mut item = ContextItem::tool_result(
                prepared.call.id.clone(),
                prepared.call.tool.clone(),
                output.content.clone(),
                output.is_error,
            );
            item.state = ContextState::Archived;
            // the same label its short copy gets, because the two are one call and the row
            // somebody opens to read what was cut is this one. Labelled `fs` beside an `fs:read`
            // copy, it would read as a different call by a tool that did not say what it did
            if let Some(label) = self.operation_label(&prepared.call) {
                item.label = label;
            }
            // the note says why it is archived, which is what a note is for and which stops being
            // true the moment somebody activates it. The `because` is the half that does not:
            // this item is the whole of an output that was shortened, whatever state it ends up in
            item.note = Some("the whole output; the model was shown a truncated copy".to_owned());
            item.included_because =
                Some("the whole of a tool output an output limit shortened".to_owned());

            // the pair is one thing that happened, so it goes into the batch first
            self.add_in(item, batch)
        });

        let truncated = limit.and_then(|limit| output.content.truncate_to(limit));
        self.record_tool_result(&prepared.call, output, truncated, whole, batch);
    }

    /// What a result of this call is called: the tool's name and the operation the call named,
    /// for a tool that does more than one thing.
    ///
    /// note: a context listing row after row labelled `context` says nothing about what any of
    /// them did, and the label is the only place the row carries a name at all - the column beside
    /// it is the *kind*, which reads `tool_result` for every one of them. [`Tool::needs`] is how a
    /// call says which operation it is, and it is asked for every call anyway, to consult the
    /// policy.
    ///
    /// note: the tool's id and not the capability's domain. For every multi-operation tool in this
    /// workspace the two are the same word, and the label comes out as the subject exactly. Where
    /// they differ the tool's name is the one worth keeping: a tool called `shell` acting in
    /// `exec` would be labelled `exec:run`, and every tool from an MCP server declares `mcp:call`,
    /// so a context full of them would say `mcp:call` on every row and name none of them.
    ///
    /// note: `None` for a tool that declares one operation, and for a call that named none of the
    /// ones it declares. A tool that does one thing is described by its own name, and appending the
    /// one operation it has would be noise on every row; a call naming no operation declares all of
    /// them, and the tool's name is the honest label for one nobody can place.
    /// [`ContextKind::ToolResult`](crate::ContextKind::ToolResult) keeps the tool id either way, so
    /// `tool:<name>` selects what it always did.
    ///
    /// note: asked here rather than written at the one place a result is recorded, because a
    /// shortened output is recorded as *two* items and both of them are that call. Labelled only
    /// where a result is recorded, the whole would carry the bare tool name - and the row a person
    /// opens to read the part that was cut would not say which read it came from.
    fn operation_label(&self, call: &ToolCall) -> Option<String> {
        let tool = self.tool(&call.tool)?;
        if tool.spec().capabilities.len() < 2 {
            return None;
        }
        match &tool.needs(call)[..] {
            [needed] => Some(format!("{}:{}", call.tool, needed.op)),
            _ => None,
        }
    }

    /// Records a tool result in the context and broadcasts [`Event::ToolFinished`].
    ///
    /// note: into a [`Batch`] the caller holds - the turn it answers, the calls of one turn, or the
    /// whole of a truncated output, which is recorded as a second item - so that one
    /// [`Kernel::undo`] takes back the whole of what happened rather than leaving half a tool call
    /// behind.
    pub(super) fn record_tool_result(
        &self,
        call: &ToolCall,
        output: ToolOutput,
        truncated: Option<usize>,
        whole: Option<ContextId>,
        batch: &mut Batch,
    ) -> ContextId {
        let is_error = output.is_error;
        let mut item =
            ContextItem::tool_result(call.id.clone(), call.tool.clone(), output.content, is_error);

        if let Some(label) = self.operation_label(call) {
            item.label = label;
        }
        // note: `included_because` and not `note`, which is documented as why an item is in its
        // *current state* and is replaced whenever that changes. This item is `Active`, so it has
        // no state to explain - and being a shortened copy is a fact about what it holds, which
        // outlives every state it will ever be in. Kept in the note, it would be lost the first
        // time anybody changed the item's state, and with it the only sentence saying which item
        // holds the whole.
        item.included_because = match (truncated, whole) {
            (Some(bytes), Some(whole)) => Some(format!(
                "{bytes} bytes were truncated by the output limit; the whole output is item {whole}"
            )),
            (Some(bytes), None) => {
                Some(format!("{bytes} bytes were truncated by the output limit"))
            }
            (None, _) => None,
        };

        let id = self.add_in(item, batch);
        let tokens = self.item(id).map(|i| i.tokens).unwrap_or(0);
        self.emit(Event::ToolFinished {
            call: call.id.clone(),
            tool: call.tool.clone(),
            is_error,
            truncated,
            tokens,
            item: id,
            whole,
        });

        id
    }
}

/// What a refused call is told, which is the policy's reason and the kernel's own account of
/// what kind of refusal it was.
///
/// note: `the call was not permitted` on its own is true and close to useless. It leaves the one
/// question a refused agent has to answer - is trying again worth anything? - entirely open, so
/// a model refused by a standing rule will keep asking, and a model refused once by a person
/// will give up on an approach that was never the problem. The kernel cannot know *why*; it does
/// know which of those two happened, because it is the thing that resolved the grant.
///
/// note: written for a reader who has never heard of this runtime. No `at the terminal`, no
/// `verdict`, no `grant`: a tool result is read by a model, and a sentence in a codebase's own
/// idiom reads to one like a state it is supposed to recognise.
fn refusal(source: GrantSource, why: Option<String>) -> String {
    match source {
        // a standing rule: the same call will meet the same answer, and so will a paraphrase of
        // it, so the useful move is a different approach or a question to whoever set the rule
        GrantSource::Policy => {
            let reason = why.unwrap_or_else(|| "the permission policy refused it".to_owned());

            format!(
                "the call was not permitted: {reason}. That is a standing rule rather than an \
                 answer to this one call, so making the same call again will be refused the same \
                 way."
            )
        }
        // asked and answered: this call was refused, and nothing was said about the next one
        GrantSource::User => "the call was not permitted: this call was refused when it was \
                              asked about. That is an answer to this call rather than a standing \
                              rule, so a different approach may well be allowed."
            .to_owned(),
        other => format!("the call was not permitted: {other:?}"),
    }
}

/// What a call to a tool nobody registered is told, which is what it asked for and what is here.
///
/// note: the tools *are* named, because a model told only that its word was wrong does not stop -
/// it guesses again, and a model that spells every call `<tool>.<operation>` can read the
/// definitions the whole time without seeing which part of what it wrote is the wrong part. A list
/// to compare its own word against is the shortest thing that says so.
///
/// note: the ones registered rather than the ones a spelling is close to, because a suggestion is
/// a guess about what was meant and this runtime does not know. The list is short by construction:
/// these are the tools of one session.
fn unknown_tool(asked: &str, here: &[String]) -> String {
    let named = match here.is_empty() {
        true => "there are no tools in this session at all".to_owned(),
        false => format!(
            "the tools here are {}",
            here.iter()
                .map(|id| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };

    format!("there is no tool named `{asked}`; {named}")
}
