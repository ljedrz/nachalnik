//! Deciding on a model's tool calls, running them, and recording what they produced.
//!
//! note: the private half of [`super::Kernel`]'s execution path; see the sibling `request`
//! module for why it is not in `mod.rs`. The division is the state machine's own: everything
//! here happens between [`super::State::Ready`] and [`super::State::Idle`].

use std::sync::{Arc, atomic::Ordering::SeqCst};

use crate::{
    context::{ContextId, ContextItem, ContextState},
    error::Result,
    event::{Event, OutputSink},
    model::ToolCall,
    permissions::{Grant, GrantSource, PermissionId, PermissionRequest, Verdict},
    tool::{Tool, ToolOutput},
};

use super::{Kernel, PreparedCall, Restore, State};

impl Kernel {
    /// Matches the model's calls to tools, asks the policy about each, and queues them.
    ///
    /// note: Nothing about a permission is announced until every call is queued and the state
    /// machine has moved. A client's whole job here is to answer the question it is handed, and
    /// it would not be much of a runtime if [`Kernel::decide`] could fail purely because the
    /// client was quick about it - which it did, for as long as the policy was still being
    /// consulted about the *next* call in the batch.
    pub(super) async fn prepare_calls(&self, calls: &[ToolCall]) -> State {
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
                self.record_tool_result(
                    call,
                    ToolOutput::error(format!("there is no tool named `{}`", call.tool)),
                    None,
                    None,
                    true,
                );
                continue;
            };

            let spec = tool.spec();
            let id = PermissionId(self.0.next_permission.fetch_add(1, SeqCst));
            let request = PermissionRequest::new(id, call, spec.capabilities.clone());

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
                spec,
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
            for (prepared, output) in prepared.iter().zip(outputs) {
                self.record_output(prepared, output);
            }
        } else {
            // one at a time, and each one recorded before the next begins, so that a client
            // watching the stream sees a call finish rather than a batch of them
            for prepared in &prepared {
                // an interrupt stops the ones that have not started. They are still recorded,
                // and recorded as not having run, because a call with no result at all would
                // leave the model looking at a question nobody answered
                let output = match self.is_interrupted() {
                    true => ToolOutput::error("interrupted before this call was made"),
                    false => {
                        self.invoke(prepared.tool.clone(), prepared.call.clone(), prepared.grant)
                            .await
                    }
                };
                self.record_output(prepared, output);
            }
        }

        self.transition(&mut self.0.machine.lock(), State::Idle);
        restore.disarm();

        Ok(State::Idle)
    }

    /// Runs the calls at the same time; see [`Config::parallel_tool_calls`].
    async fn invoke_together(&self, prepared: &[PreparedCall]) -> Vec<ToolOutput> {
        let mut running = tokio::task::JoinSet::new();
        for (index, call) in prepared.iter().enumerate() {
            let (kernel, tool, grant) = (self.clone(), call.tool.clone(), call.grant);
            let call = call.call.clone();
            running.spawn(async move { (index, kernel.invoke(tool, call, grant).await) });
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
        grant: Option<(Grant, GrantSource)>,
    ) -> ToolOutput {
        let (grant, source) = grant.expect("every claimed call has been decided");
        if grant == Grant::Deny {
            return ToolOutput::error(refusal(source, self.policy().why(&call.id)));
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
    fn record_output(&self, prepared: &PreparedCall, mut output: ToolOutput) {
        // an output limit decides what the *model* is shown. It is not permission to throw the
        // rest away, so unless the user has said otherwise the whole of it goes into the context
        // too - archived, listed, inspectable, and restorable like anything else
        let limit = prepared
            .spec
            .output_limit
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
            // the note says why it is archived, which is what a note is for and which stops being
            // true the moment somebody activates it. The `because` is the half that does not:
            // this item is the whole of an output that was shortened, whatever state it ends up in
            item.note = Some("the whole output; the model was shown a truncated copy".to_owned());
            item.included_because =
                Some("the whole of a tool output an output limit shortened".to_owned());

            // the pair is one thing that happened, so it gets one checkpoint, taken here
            self.add_item(item, true)
        });

        let truncated = limit.and_then(|limit| output.content.truncate_to(limit));
        self.record_tool_result(&prepared.call, output, truncated, whole, whole.is_none());
    }

    /// Records a tool result in the context and broadcasts [`Event::ToolFinished`].
    ///
    /// note: `checkpoint` is false only when the caller has already taken one for this result -
    /// a truncated output is recorded as two items, and one [`Kernel::undo`] should take back
    /// both of them rather than leaving half a tool call behind.
    pub(super) fn record_tool_result(
        &self,
        call: &ToolCall,
        output: ToolOutput,
        truncated: Option<usize>,
        whole: Option<ContextId>,
        checkpoint: bool,
    ) -> ContextId {
        let is_error = output.is_error;
        let mut item =
            ContextItem::tool_result(call.id.clone(), call.tool.clone(), output.content, is_error);
        // note: `included_because` and not `note`, which is documented as why an item is in its
        // *current state* and is replaced whenever that changes. This item is `Active`, so it has
        // no state to explain - and being a shortened copy is a fact about what it holds, which
        // outlives every state it will ever be in. Kept in the note, it was destroyed the first
        // time anybody pressed `space` on the row: a live session cycled the pair looking at it
        // and lost the only sentence saying which item held the whole.
        item.included_because = match (truncated, whole) {
            (Some(bytes), Some(whole)) => Some(format!(
                "{bytes} bytes were truncated by the output limit; the whole output is item {whole}"
            )),
            (Some(bytes), None) => {
                Some(format!("{bytes} bytes were truncated by the output limit"))
            }
            (None, _) => None,
        };

        let id = self.add_item(item, checkpoint);
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
    let reason = why.unwrap_or_else(|| "the permission policy refused it".to_owned());

    match source {
        // a standing rule: the same call will meet the same answer, and so will a paraphrase of
        // it, so the useful move is a different approach or a question to whoever set the rule
        GrantSource::Policy => format!(
            "the call was not permitted: {reason}. That is a standing rule rather than an \
             answer to this one call, so making the same call again will be refused the same way."
        ),
        // asked and answered: this call was refused, and nothing was said about the next one
        GrantSource::User => "the call was not permitted: this call was refused when it was \
                              asked about. That is an answer to this call rather than a standing \
                              rule, so a different approach may well be allowed."
            .to_owned(),
        other => format!("the call was not permitted: {other:?}"),
    }
}
