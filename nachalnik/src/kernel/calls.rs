//! Decides on a model's tool calls, runs them, and records what they produced.
//!
//! note: the private half of [`super::Kernel`]'s execution path, out of `mod.rs` for the reason the
//! sibling `request` module gives: everything between the model asking for tools and the machine
//! returning to [`super::State::Idle`].

use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, atomic::Ordering::SeqCst},
    task::Poll,
};

use crate::{
    context::{ContextId, ContextItem, ContextState, WHOLE_OUTPUT},
    error::Result,
    event::{Event, OutputSink},
    model::ToolCall,
    permissions::{Grant, GrantSource, PermissionId, PermissionPolicy, PermissionRequest, Verdict},
    tool::{Tool, ToolOutput},
};

use super::{Batch, Kernel, PreparedCall, Restore, State};

impl Kernel {
    /// Matches the model's calls to tools, asks the policy about each, and queues them.
    ///
    /// note: no permission is announced until every call is queued; [`Kernel::settle`] then
    /// announces them and moves the machine together, so a client sees a question only when it can
    /// answer it. Announcing earlier, while the policy is still being consulted about the next call
    /// in the batch, could make a quick client's [`Kernel::decide`] fail.
    ///
    /// `turn` is the batch the turn asking for them was recorded in; an answer to a call nobody can
    /// run joins it.
    pub(super) async fn prepare_calls(&self, calls: &[ToolCall], mut turn: Batch) -> State {
        let mut prepared = Vec::with_capacity(calls.len());
        let mut announcements = Vec::with_capacity(calls.len());
        let ceiling = self.call_ceiling();

        for (asked, call) in calls.iter().enumerate() {
            self.emit(Event::ToolRequested {
                call: call.id.clone(),
                tool: call.tool.clone(),
                args: call.args.clone(),
            });

            // past the ceiling, nothing is asked about or run. Each call is still answered and
            // joins the turn's checkpoint, as a call nobody can run does. See
            // `Kernel::set_call_ceiling`
            if let Some(ceiling) = ceiling.filter(|ceiling| asked >= *ceiling) {
                self.refuse(
                    call,
                    None,
                    over_the_ceiling(calls.len(), ceiling),
                    &mut turn,
                );
                continue;
            }

            let Some(tool) = self.tool(&call.tool) else {
                self.emit(Event::ToolUnknown {
                    call: call.id.clone(),
                    tool: call.tool.clone(),
                });
                // joins the checkpoint the turn was recorded under: the turn and the kernel's
                // answer to a call nobody can run are one event, and separate checkpoints would let
                // one `undo` leave a call answered and its neighbour not
                self.refuse(
                    call,
                    None,
                    unknown_tool(&call.tool, &self.tool_ids()),
                    &mut turn,
                );
                continue;
            };

            // arguments that are not an object are not asked about or run with. The call is
            // answered here and joins the turn's checkpoint, as one nobody can run does. See
            // `ToolCall::unparsed`
            if let Some(written) = &call.unparsed {
                let error = misparsed(written);
                self.emit(Event::ToolUnparsed {
                    call: call.id.clone(),
                    tool: call.tool.clone(),
                    written: written.clone(),
                    error: error.said.clone(),
                });
                let advice = guarded(|| tool.unparsed(written)).unwrap_or_else(|_| {
                    self.emit(Event::ToolPanicked {
                        call: call.id.clone(),
                        tool: call.tool.clone(),
                    });
                    None
                });
                let said = unparsed_answer(call, written, &error, advice);
                self.refuse(call, Some(&*tool), said, &mut turn);
                continue;
            }

            // a call the tool would refuse whatever the answer is refused before anybody is asked,
            // and joins the turn's checkpoint as an unparsed one does. See `Tool::malformed`
            //
            // note: under the same catch as `needs`, and a panic here fails the call the same way
            match guarded(|| tool.malformed(call)) {
                Ok(None) => {}
                Ok(Some(said)) => {
                    self.refuse(call, Some(&*tool), said, &mut turn);
                    continue;
                }
                Err(panic) => {
                    self.emit(Event::ToolPanicked {
                        call: call.id.clone(),
                        tool: call.tool.clone(),
                    });
                    self.refuse(call, None, panicked_needing(&*panic), &mut turn);
                    continue;
                }
            }

            // what the call needs rather than what the tool might: see `Tool::needs`
            //
            // note: under the same catch as `invoke`, since it reads the model's arguments too. A
            // tool that panics here has failed the call: nothing is asked or run, and the call is
            // answered as one to a missing tool, joining the turn's checkpoint
            let needs = match guarded(|| tool.needs(call)) {
                Ok(needs) => needs,
                Err(panic) => {
                    self.emit(Event::ToolPanicked {
                        call: call.id.clone(),
                        tool: call.tool.clone(),
                    });
                    self.refuse(call, None, panicked_needing(&*panic), &mut turn);
                    continue;
                }
            };
            let id = PermissionId(self.0.next_permission.fetch_add(1, SeqCst));
            let request = PermissionRequest::new(id, call, needs);

            let policy = self.policy();
            let grant = match policy.evaluate(&request).await {
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
                policy,
            });
        }

        // queued under the same hold that announces them, so a `Kernel::decide` racing this waits
        // for the request it answers to be there
        let mut machine = self.0.machine.lock();
        let to = Self::state_for(&prepared);
        machine.pending = prepared;
        self.settle(&mut machine, announcements, to.clone());

        to
    }

    /// Answers a call that is not run with `said`, as an error joining the turn's checkpoint.
    fn refuse(&self, call: &ToolCall, tool: Option<&dyn Tool>, said: String, turn: &mut Batch) {
        self.record_tool_result(call, tool, ToolOutput::error(said), None, None, turn);
    }

    /// Runs the claimed calls, and records their results in the order the model asked for them.
    pub(super) async fn execute(&self, prepared: Vec<PreparedCall>) -> Result<State> {
        // note: if this future is dropped, the calls it had claimed are gone with it. Their results
        // are never recorded, and the projector drops the orphaned calls from the next request
        let mut restore = Restore::new(self);

        if self.0.config.parallel_tool_calls {
            // They are recorded in the order the model asked for them, whatever order they finished
            // in, so that a context does not depend on which tool happened to be quick
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
                let output = self.invoke(prepared, true).await;
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
            let (kernel, call) = (self.clone(), call.clone());
            running.spawn(async move { (index, kernel.invoke(&call, false).await) });
        }

        let mut outputs: Vec<Option<ToolOutput>> = (0..prepared.len()).map(|_| None).collect();
        while let Some(finished) = running.join_next().await {
            match finished {
                Ok((index, output)) => outputs[index] = Some(output),
                // a tool's panic is caught inside `invoke`, so one that reaches here is the
                // kernel's own, and unwinds through `step` as it would with the calls run one at
                // a time
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
    /// either. `stoppable` means an outstanding interrupt keeps the call from starting; only calls
    /// run one at a time are stoppable, and a batch run together has been started whole.
    ///
    /// note: an interrupt stops calls that have not started. They are still recorded, as not having
    /// run, because a call with no result would leave the model looking at a question nobody
    /// answered.
    ///
    /// note: the flag is read and the start announced under one hold of the machine lock, where
    /// `interrupt` sets and announces it. Read outside it, an interrupt landing between the two
    /// puts `turn.interrupted` in the record ahead of a call that then ran.
    async fn invoke(&self, prepared: &PreparedCall, stoppable: bool) -> ToolOutput {
        // asked before the lock below is taken, since it is the policy's code
        let refused = self.refusal(prepared.grant, &prepared.request, &*prepared.policy);
        {
            let _machine = self.0.machine.lock();
            if stoppable && self.is_interrupted() {
                return ToolOutput::error("interrupted before this call was made");
            }
            if let Some(refusal) = refused {
                return refusal;
            }
            self.emit(Event::ToolStarted {
                call: prepared.call.id.clone(),
                tool: prepared.call.tool.clone(),
            });
        }

        self.run(prepared.tool.clone(), prepared.call.clone()).await
    }

    /// What a refused call is answered with, or `None` for one that may run.
    fn refusal(
        &self,
        grant: Option<(Grant, GrantSource)>,
        request: &PermissionRequest,
        policy: &dyn PermissionPolicy,
    ) -> Option<ToolOutput> {
        let (grant, source) = grant.expect("every claimed call has been decided");
        if grant != Grant::Deny {
            return None;
        }

        // asked only where the policy refused it. `refusal` puts a reason into the standing-rule
        // wording and into no other; asking elsewhere computes an explanation and drops it
        let why = match source {
            GrantSource::Policy => policy.why(request),
            _ => None,
        };

        Some(ToolOutput::error(refusal(source, why)))
    }

    /// Runs a call that may run and has been announced as started.
    async fn run(&self, tool: Arc<dyn Tool>, call: ToolCall) -> ToolOutput {
        // a tool that fails is not a kernel failure: the model is told, and the loop goes on.
        //
        // note: a panic is a failure too, so it is caught at each poll rather than unwinding
        // through `step`, which took the turn with it, answered nothing, and handed a client a
        // panic instead of a state. `AssertUnwindSafe` because what the tool leaves behind is its
        // own; the kernel changes its state under its locks and holds none across a poll
        let sink = OutputSink::new(self.clone(), call.id.clone(), call.tool.clone());
        let mut running = tool.invoke(&call, sink);
        let caught = std::future::poll_fn(|cx| {
            match std::panic::catch_unwind(AssertUnwindSafe(|| running.as_mut().poll(cx))) {
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(answered)) => Poll::Ready(Ok(answered)),
                Err(panic) => Poll::Ready(Err(panic)),
            }
        })
        .await;

        match caught {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => ToolOutput::error(described(&*e)),
            Err(panic) => {
                self.emit(Event::ToolPanicked {
                    call: call.id.clone(),
                    tool: call.tool.clone(),
                });
                ToolOutput::error(panicked(&*panic))
            }
        }
    }

    /// Records what a call produced, keeping the whole of it when a limit shortened it.
    ///
    /// note: every call of a batch is recorded into one [`Batch`], the shape
    /// [`Kernel::cancel_pending_calls`] uses. A checkpoint taken between two calls is folded in,
    /// because it would split the batch.
    fn record_output(&self, prepared: &PreparedCall, mut output: ToolOutput, batch: &mut Batch) {
        // an output limit decides what the model is shown; it is not permission to throw the rest
        // away. Unless the user said otherwise, the whole output goes into the context too,
        // excluded and restorable like anything else.
        //
        // note: a tool that panics saying what a call may return, or says nothing, has its result
        // held to the session's own limit; the call has run and its result is still its answer
        let limit = guarded(|| prepared.tool.limit(&prepared.call))
            .unwrap_or_else(|_| {
                self.emit(Event::ToolPanicked {
                    call: prepared.call.id.clone(),
                    tool: prepared.call.tool.clone(),
                });
                None
            })
            .or(self.default_tool_output_limit());
        // note: cut on a copy first, because whether it is over is whether anything is cut - which
        // a picture kept whole makes a different question from whether it is longer than the limit
        let pictures = self.picture_limit();
        let mut shown = output.content.clone();
        let truncated = limit.and_then(|limit| match pictures {
            Some(pictures) => shown.truncate_around_pictures(limit, pictures),
            None => shown.truncate_to(limit),
        });
        let over = truncated.is_some();

        let whole = (over && self.0.config.keep_truncated_output).then(|| {
            let mut item = ContextItem::tool_result(
                prepared.call.id.clone(),
                prepared.call.tool.clone(),
                output.content.clone(),
                output.is_error,
            );
            item.state = ContextState::Excluded;
            // the same label its short copy gets: the two are one call, and this is the row
            // somebody opens to read what was cut. Labelled plain `fs` beside an `fs:read` copy, it
            // would look like a different call
            if let Some(label) = Self::operation_label(&*prepared.tool, &prepared.call) {
                item.label = label;
            }
            // the note says why it is out, which stops being true once somebody activates it; the
            // `because` stays true in any state
            item.note = Some("the whole output; the model was shown a truncated copy".to_owned());
            item.included_because = Some(WHOLE_OUTPUT.to_owned());

            // the pair is one thing that happened, so it goes into the batch first
            self.add_in(item, batch)
        });

        output.content = shown;
        self.record_tool_result(
            &prepared.call,
            Some(&*prepared.tool),
            output,
            truncated,
            whole,
            batch,
        );
    }

    /// What a result of this call is called: the tool's name and the operation the call named, for
    /// a tool that does more than one thing.
    ///
    /// note: rows labelled `context` say nothing about what they did, and the label is a row's only
    /// name (the kind column reads `tool_result` for all of them). [`Tool::needs`] says which
    /// operation a call is, and is asked for every call anyway.
    ///
    /// note: the tool's id, not the capability's domain. For this workspace's multi-operation tools
    /// they are the same word. Where they differ the tool's name wins: `shell` acts as `exec:run`,
    /// and every MCP tool declares `mcp:call`, which would name none of them.
    ///
    /// note: `None` for a tool declaring one operation, where the operation would be noise on every
    /// row, and for a call naming none of the tool's operations, which declares all of them.
    /// [`ContextKind::ToolResult`](crate::ContextKind::ToolResult) keeps the tool id either way, so
    /// `tool:<name>` selects what it always did.
    ///
    /// note: asked here rather than where a result is recorded, because a shortened output is
    /// recorded as two items and both should name the operation.
    ///
    /// note: the tool the call was matched to, not the registry's: a tool removed after its call
    /// was decided still runs that call.
    fn operation_label(tool: &dyn Tool, call: &ToolCall) -> Option<String> {
        if tool.spec().capabilities.len() < 2 {
            return None;
        }
        // a tool that panics saying what a call needs was answered for it already, and is
        // labelled by its name here
        match &guarded(|| tool.needs(call)).ok()?[..] {
            [needed] => Some(format!("{}:{}", call.tool, needed.op)),
            _ => None,
        }
    }

    /// Records a tool result in the context and broadcasts [`Event::ToolFinished`]. `tool` is the
    /// one the call was matched to, or `None` for a call naming no tool.
    ///
    /// note: into a [`Batch`] the caller holds (the turn it answers, the calls of one turn, or a
    /// truncated output's whole, recorded as a second item), so one [`Kernel::undo`] takes back
    /// everything that happened rather than half a tool call.
    pub(super) fn record_tool_result(
        &self,
        call: &ToolCall,
        tool: Option<&dyn Tool>,
        output: ToolOutput,
        truncated: Option<usize>,
        whole: Option<ContextId>,
        batch: &mut Batch,
    ) -> ContextId {
        let is_error = output.is_error;
        let mut item =
            ContextItem::tool_result(call.id.clone(), call.tool.clone(), output.content, is_error);

        if let Some(label) = tool.and_then(|tool| Self::operation_label(tool, call)) {
            item.label = label;
        }
        // note: `included_because`, not `note`: a note explains the current state and is replaced
        // when it changes, and this item is `Active`. Being a shortened copy is a fact about its
        // content that outlives every state; kept in the note, it would be lost at the first state
        // change, and with it the only sentence saying which item holds the whole.
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

/// What a refused call is told: the policy's reason and the kernel's own account of what kind of
/// refusal it was.
///
/// note: `the call was not permitted` alone leaves open whether trying again is worth anything, so
/// a model refused by a standing rule keeps asking, and one refused by a person abandons an
/// approach that was never the problem. The kernel cannot know why, but it knows which of the two
/// happened, because it resolved the grant.
///
/// note: written for a reader who has never heard of this runtime: no `at the terminal`, `verdict`
/// or `grant`. A model reads a codebase's idiom as a state it should recognise.
fn refusal(source: GrantSource, why: Option<String>) -> String {
    match source {
        // a standing rule: the same call and any paraphrase of it get the same answer. The useful
        // move is a different approach or a question to whoever set the rule
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

/// What a call whose tool failed is told: the error, and each cause beneath it.
///
/// note: the causes, because the top of a wrapped error is often only what was being done ("reading
/// the config") and the reason is the error it wraps, which `to_string` leaves out. A cause already
/// in the message is not repeated, for errors that print their cause as well as returning it.
fn described(error: &(dyn std::error::Error + 'static)) -> String {
    let mut said = error.to_string();
    let mut cause = error.source();
    while let Some(error) = cause {
        let more = error.to_string();
        if !said.contains(&more) {
            said = format!("{said}: {more}");
        }
        cause = error.source();
    }
    said
}

/// What the parser made of arguments that are not a JSON object.
struct Misparsed {
    /// What it said was wrong, with where.
    said: String,
    /// The byte the fault is at, or `0` where the text parsed as something other than an object.
    at: usize,
}

/// Parses what a model wrote again, for what is wrong with it and where; the provider that
/// found it unreadable kept the text and not the error.
fn misparsed(written: &str) -> Misparsed {
    match serde_json::from_str::<serde_json::Value>(written) {
        Err(error) => {
            // the parser counts lines and columns from one, and a column is a byte in its line
            let line = written
                .split_inclusive('\n')
                .take(error.line().saturating_sub(1))
                .map(str::len)
                .sum::<usize>();
            Misparsed {
                said: error.to_string(),
                at: (line + error.column().saturating_sub(1)).min(written.len()),
            }
        }
        Ok(parsed) => Misparsed {
            said: format!(
                "it is JSON, but {} rather than an object",
                match parsed {
                    serde_json::Value::Array(_) => "a list",
                    serde_json::Value::String(_) => "a string",
                    serde_json::Value::Number(_) => "a number",
                    serde_json::Value::Bool(_) => "true or false",
                    _ => "null",
                }
            ),
            at: 0,
        },
    }
}

/// How many bytes of a model's writing are quoted back: enough to see the fault, not so many that a
/// long call is copied into the context twice.
const QUOTED: usize = 200;

/// What a call whose arguments are not a JSON object is answered with: which call, that the
/// arguments did not parse, what the parser said and where, the text around that, and whatever the
/// tool adds; see [`Tool::unparsed`].
///
/// note: the text around the fault, not the start, because faults past any cut are common and the
/// start is the part that is fine.
fn unparsed_answer(
    call: &ToolCall,
    written: &str,
    error: &Misparsed,
    advice: Option<String>,
) -> String {
    let mut from = error.at.saturating_sub(QUOTED / 2);
    while !written.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (from + QUOTED).min(written.len());
    while !written.is_char_boundary(to) {
        to += 1;
    }
    let quoted = format!(
        "{}{}{}",
        if from > 0 { "..." } else { "" },
        &written[from..to],
        if to < written.len() { "..." } else { "" },
    );

    format!(
        "the arguments of this call to `{}` (`{}`) are not a JSON object, so it was not run and \
         nothing was asked about it: {}. What arrived there was `{quoted}`. Send the call again \
         with its arguments as one JSON object.{}",
        call.tool,
        call.id,
        error.said,
        advice
            .map(|advice| format!(" {advice}"))
            .unwrap_or_default(),
    )
}

/// What a call past the ceiling is answered with.
fn over_the_ceiling(asked: usize, ceiling: usize) -> String {
    format!(
        "not run: this answer asked for {asked} tool calls, and only the first {ceiling} of one \
         answer are run. Ask for the rest in your next answer, once you have read these results"
    )
}

/// Runs one of a tool's own methods, catching a panic in it as `invoke`'s are caught.
fn guarded<T>(asked: impl FnOnce() -> T) -> std::thread::Result<T> {
    std::panic::catch_unwind(AssertUnwindSafe(asked))
}

/// What a call is answered with when its tool panicked saying what the call needs.
fn panicked_needing(payload: &(dyn std::any::Any + Send)) -> String {
    let said = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));

    match said {
        Some(said) => {
            format!("the tool crashed reading this call's arguments, so it did not run: {said}")
        }
        None => "the tool crashed reading this call's arguments, so it did not run, and said \
                 nothing about why"
            .to_owned(),
    }
}

/// What a call whose tool panicked is told: that it crashed, and what the panic said.
///
/// note: "crashed" rather than "panicked", for the same reason [`refusal`] avoids a codebase's
/// idiom. A payload that is not a string (`panic_any` with anything else) is said to have said
/// nothing, not rendered as a type name.
fn panicked(payload: &(dyn std::any::Any + Send)) -> String {
    let said = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));

    match said {
        Some(said) => format!("the tool crashed while making this call: {said}"),
        None => "the tool crashed while making this call, and said nothing about why".to_owned(),
    }
}

/// What a call to a tool nobody registered is told, which is what it asked for and what is here.
///
/// note: the tools are named, because a model told only that its word was wrong guesses again; one
/// that spells every call `<tool>.<operation>` can read the definitions all along without seeing
/// which part it got wrong. A list to compare against says so most briefly.
///
/// note: the registered ones, not the ones a spelling is close to, because a suggestion guesses
/// what was meant and this runtime does not know. The list is short: these are one session's tools.
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
