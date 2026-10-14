//! The state machine: the states a turn moves through, and every operation that moves it.
//!
//! note: the loop's own file, apart from what a turn is made of - `request` and `calls` - and from
//! construction and observation in `mod.rs`. The other files still take the machine's lock where
//! they must - to queue a turn's calls, to undo past a turn, to finish - and what they use from
//! here is `pub(super)`.

use std::sync::{Arc, atomic::Ordering::SeqCst};

#[cfg(doc)]
use crate::{config::Config, model::Provider, projection::Projector};
use crate::{
    error::{Error, Result},
    event::Event,
    model::ToolCall,
    permissions::{Grant, GrantSource, PermissionId, PermissionPolicy, PermissionRequest},
    tool::{Tool, ToolOutput},
};

use super::{Batch, Kernel, State};

/// A tool call that has been matched to a tool and is waiting for a decision, or for its turn.
#[derive(Clone)]
pub(super) struct PreparedCall {
    pub(super) call: ToolCall,
    pub(super) tool: Arc<dyn Tool>,
    pub(super) request: PermissionRequest,
    pub(super) grant: Option<(Grant, GrantSource)>,
    /// The policy that evaluated the call, which is the one asked why it refused.
    ///
    /// note: kept rather than read again when the refusal is written, because a call rests in
    /// `Ready` or `Deciding` between the two and a `set_policy` there is ordinary. Asked then, the
    /// installed policy explains a decision it did not make, in rules the call was never held to.
    pub(super) policy: Arc<dyn PermissionPolicy>,
}

/// The state machine and what it holds on to: the calls, and the turn's count.
///
/// note: the interrupt flag is not here, though everything that writes it holds this lock. A
/// provider or a tool reads it through its sink while the turn runs, and a read that waited for
/// this lock would stall a stream behind whoever holds it - an announcement can wait on the
/// session log for as long as a `with_history` caller likes.
pub(super) struct Machine {
    pub(super) state: State,
    pub(super) pending: Vec<PreparedCall>,
    /// How many requests the turn in progress has sent; see [`Kernel::turn`].
    turn_requests: usize,
}

impl Machine {
    pub(super) fn new() -> Self {
        Self {
            state: State::Idle,
            pending: Vec::new(),
            turn_requests: 0,
        }
    }
}

/// What a [`Kernel::step`] claimed the right to do.
enum Claim {
    Request,
    Execute(Vec<PreparedCall>),
}

/// What one transition attempt did.
enum Attempt {
    /// It made the transition it was in a position to make, or found there was none to make,
    /// and left this state.
    Stepped(State),
    /// It spent itself acknowledging an interrupt, and did nothing else in this state.
    Acknowledged(State),
}

impl Attempt {
    fn state(self) -> State {
        match self {
            Self::Stepped(state) | Self::Acknowledged(state) => state,
        }
    }
}

/// Puts the state machine back to [`State::Idle`] if the future driving a transition never
/// finishes.
pub(super) struct Restore<'a> {
    kernel: &'a Kernel,
    armed: bool,
}

impl<'a> Restore<'a> {
    pub(super) fn new(kernel: &'a Kernel) -> Self {
        Self {
            kernel,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut machine = self.kernel.0.machine.lock();
            // the transition this was guarding failed or was abandoned, so whatever it was asked
            // to stop has stopped. Left set, the interrupt would be spent on the next turn instead
            // - which transitions nothing, and the message somebody typed goes unanswered
            self.kernel.0.interrupted.store(false, SeqCst);
            self.kernel.transition(&mut machine, State::Idle);
        }
    }
}

impl Kernel {
    /// Asks [`Kernel::turn`] to stop at the next opportunity, and returns whether it had already
    /// been asked.
    ///
    /// note: This does *not* abort a request that is already in flight, because the kernel does
    /// not own the task making it - drop the future driving [`Kernel::step`] for that. What it
    /// does is stop `turn` from starting anything else: the step in progress runs to the end,
    /// its result is recorded like any other, and `turn` returns instead of going round again.
    ///
    /// note: How far it reaches depends on how much cooperates. Before a transition,
    /// [`Kernel::step`] and [`Kernel::turn`] spend one attempt acknowledging it and do nothing
    /// else. During a request, a [`Provider`] that checks [`crate::DeltaSink::is_interrupted`] can
    /// stop reading and hand back what it has. During a tool call, a [`Tool`] that checks
    /// [`crate::OutputSink::is_interrupted`] can do the same - and in the serial case the kernel
    /// does not start the calls that had not begun.
    ///
    /// note: only in the serial case. With
    /// [`Config::parallel_tool_calls`](crate::Config::parallel_tool_calls) on there is nothing
    /// left to not-start: every call in the batch is spawned before the first one answers, so an
    /// interrupt reaches only the tools that check the sink for themselves. Neither mode can stop
    /// a tool that blocks without ever looking - the kernel does not own the thread it is on, and
    /// there is no safe way to take it back. What it always does is record: every call gets an
    /// output, even when that output is that it never ran.
    ///
    /// note: The flag is spent by the transition attempt that acts on it - [`Kernel::step`],
    /// including the one [`Kernel::turn`] is in the middle of making - and nothing else spends it.
    /// It is also put down when a turn reaches [`State::Finished`] and when the transition it was
    /// stopping fails or is abandoned, so it can never outlive the thing it was meant to stop. It
    /// discards no work: a partial answer and a half-finished tool result are recorded like any
    /// other. A step refused as [`Error::Busy`] acts on nothing and so clears nothing: the attempt
    /// that spends the flag is the one that was in a position to transition, and the request in
    /// flight keeps the stop its provider is reading.
    ///
    /// note: set and announced under the machine lock, which is where every step reads it and
    /// spends it. Outside it, a step could take the flag between the setting and the announcing,
    /// and the log would place `turn.interrupted` after the transition that acted on it.
    pub fn interrupt(&self) -> bool {
        let _machine = self.0.machine.lock();
        let already = self.0.interrupted.swap(true, SeqCst);
        if !already {
            self.emit(Event::Interrupted);
        }

        already
    }

    /// Returns whether an interrupt is outstanding.
    pub fn is_interrupted(&self) -> bool {
        self.0.interrupted.load(SeqCst)
    }

    /// Returns what the runtime is doing.
    pub fn state(&self) -> State {
        self.0.machine.lock().state.clone()
    }

    /// Returns the permission requests that are waiting for a decision.
    pub fn pending_permissions(&self) -> Vec<PermissionRequest> {
        self.0
            .machine
            .lock()
            .pending
            .iter()
            .filter(|p| p.grant.is_none())
            .map(|p| p.request.clone())
            .collect()
    }

    /// Returns the tool calls that are prepared but not yet executed, in order.
    pub fn pending_calls(&self) -> Vec<ToolCall> {
        self.0
            .machine
            .lock()
            .pending
            .iter()
            .map(|p| p.call.clone())
            .collect()
    }

    /// Answers a permission request, returning the state that answer produced.
    ///
    /// note: The state becomes [`State::Ready`] once the last outstanding question is answered;
    /// nothing runs until then, and nothing runs without a [`Kernel::step`] even then.
    pub fn decide(&self, id: PermissionId, grant: Grant) -> Result<State> {
        let mut machine = self.0.machine.lock();

        let Some(prepared) = machine
            .pending
            .iter_mut()
            .find(|p| p.request.id == id && p.grant.is_none())
        else {
            return Err(Error::UnknownPermission(id));
        };
        prepared.grant = Some((grant, GrantSource::User));
        let decided = Event::PermissionDecided {
            id,
            call: prepared.call.id.clone(),
            tool: prepared.call.tool.clone(),
            grant,
            source: GrantSource::User,
        };

        let to = Self::state_for(&machine.pending);
        self.settle(&mut machine, [decided], to.clone());

        Ok(to)
    }

    /// Drops every prepared tool call, recording each as refused, and returns how many there
    /// were.
    ///
    /// note: The model is told that the calls were refused, because a call without a result is
    /// not something most providers accept. Whether it *sees* that is up to the
    /// [`Projector`] - the record is kept either way.
    ///
    /// note: one checkpoint for the batch, taken by the first result and joined by the rest -
    /// the shape [`Kernel::push_all`] uses, for the reason it uses it. Dropping the calls is one
    /// thing somebody did, so one `undo` puts the whole of it back; a checkpoint each would make
    /// a single `undo` take back one refusal and leave the others, which is a turn where some of
    /// the model's calls were answered and one was never mentioned. It also spares
    /// [`Config::context_undo_depth`]: a model asking for sixteen tools and a person who says no
    /// would otherwise spend the whole undo history on one keystroke.
    ///
    /// note: this is [`Kernel::decide`] refusing every call followed by a [`Kernel::step`]
    /// running them, and it has the same shape. The refusals are announced as the machine is
    /// claimed as [`State::Executing`], and the results are recorded before it returns to
    /// [`State::Idle`]. Going straight to `Idle` would let a step in
    /// before the results were there - a request carrying calls nobody had answered - and would
    /// log the machine idle before the calls were refused. The results are not recorded *under*
    /// the lock because recording one asks the tool what the call needed, and that is somebody
    /// else's code.
    pub fn cancel_pending_calls(&self, reason: impl Into<String>) -> usize {
        let reason = reason.into();
        let prepared = {
            let mut machine = self.0.machine.lock();
            let prepared = std::mem::take(&mut machine.pending);
            if prepared.is_empty() {
                return 0;
            }

            let refusals = prepared.iter().map(|call| Event::PermissionDecided {
                id: call.request.id,
                call: call.call.id.clone(),
                tool: call.call.tool.clone(),
                grant: Grant::Deny,
                source: GrantSource::Cancellation,
            });
            let calls = prepared.iter().map(|p| p.call.id.clone()).collect();
            self.settle(&mut machine, refusals, State::Executing { calls });

            prepared
        };
        let mut restore = Restore::new(self);

        let mut batch = Batch::default();
        for call in &prepared {
            self.record_tool_result(
                &call.call,
                Some(&*call.tool),
                ToolOutput::error(format!("the call was cancelled: {reason}")),
                None,
                None,
                &mut batch,
            );
        }

        self.transition(&mut self.0.machine.lock(), State::Idle);
        restore.disarm();

        prepared.len()
    }

    /// Performs one transition of the state machine, and returns the state it produced.
    ///
    /// | from | what happens | to |
    /// | --- | --- | --- |
    /// | [`State::Idle`], [`State::Finished`] | a request is built and sent | `Finished`, `Ready` or `Deciding`; `Idle` if every call named a tool that is not there |
    /// | [`State::Ready`] | the tools run, and their results are recorded in the order the model asked for them | `Idle` |
    /// | [`State::Deciding`] | nothing; the answer has to come from you | `Deciding` |
    /// | [`State::Requesting`], [`State::Executing`] | nothing; [`Error::Busy`] | - |
    /// | any resting state, with an interrupt outstanding | nothing; the interrupt is spent | the same state |
    ///
    /// note: One step is one transition, so the model asking for a tool and that tool running
    /// are two of them. That is deliberate: [`State::Ready`] is a resting state at which the
    /// model has said what it wants and nothing has happened yet.
    pub async fn step(&self) -> Result<State> {
        self.step_once(None).await.map(Attempt::state)
    }

    /// One transition attempt, and whether it was spent acknowledging an interrupt rather than
    /// making a transition.
    ///
    /// note: [`Kernel::turn`] needs to be told, and cannot work it out. A step that spends an
    /// interrupt returns the state it found, which to `turn` looks like an ordinary resting state
    /// to go round again from - and the next request would go out with the stop already on the
    /// event log.
    ///
    /// note: `counted` is the count [`Kernel::turn`] has reached, stored as the turn's own only
    /// where this claims the request it counts; a request claimed with none, by a `step`, starts
    /// it again from nothing. Stored before the step, a `turn` refused as busy -
    /// a second driver arriving between another one's count and its claim - left its count in
    /// place of the turn that was running, and that turn, carried on from `Deciding`, counted from
    /// the wrong number and went past its budget.
    async fn step_once(&self, counted: Option<usize>) -> Result<Attempt> {
        let claim = {
            let mut machine = self.0.machine.lock();
            let state = machine.state.clone();
            // busy first, and before the flag is touched. A step refused as busy transitions
            // nothing, so it is not the attempt the interrupt is spent on - and consuming it here
            // would take the stop away from the request that is actually in flight, whose provider
            // is reading `is_interrupted` to decide whether to keep reading the stream
            if state.is_busy() {
                return Err(Error::Busy);
            }
            // somebody asked to stop. One transition attempt is spent acknowledging it, which is
            // also what keeps the flag from outliving the request it was meant for: a client that
            // drives `step` itself has no `turn` to consume it
            if self.0.interrupted.swap(false, SeqCst) {
                return Ok(Attempt::Acknowledged(state));
            }

            match state {
                State::Deciding { calls } => {
                    return Ok(Attempt::Stepped(State::Deciding { calls }));
                }
                State::Ready { calls } => {
                    let prepared = std::mem::take(&mut machine.pending);
                    self.transition(&mut machine, State::Executing { calls });
                    Claim::Execute(prepared)
                }
                State::Idle | State::Finished { .. } => {
                    // fail before claiming, so a kernel with no provider does not appear to
                    // have started something
                    if self.0.provider.read().is_none() {
                        return Err(Error::NoProvider);
                    }
                    self.transition(&mut machine, State::Requesting);
                    // a request a `step` claims is not a turn's, and nothing before it is either:
                    // it is claimed from a resting state no `turn` was counting through. Left
                    // alone, the count a `turn` carried on from it would read was the last turn's
                    machine.turn_requests = counted.unwrap_or(0);
                    Claim::Request
                }
                State::Requesting | State::Executing { .. } => unreachable!("handled as busy"),
            }
        };

        match claim {
            Claim::Request => self.request().await.map(Attempt::Stepped),
            Claim::Execute(prepared) => self.execute(prepared).await.map(Attempt::Stepped),
        }
    }

    /// Repeats [`Kernel::step`] until the model ends its turn, a decision is needed, or
    /// [`Kernel::max_requests_per_turn`] is reached; returns the state it stopped in.
    ///
    /// note: [`State::Finished`] means the model answered, [`State::Deciding`] means it is your
    /// move, and [`State::Idle`] means the request budget ran out mid-loop - calling `turn`
    /// again picks up exactly where it left off.
    ///
    /// note: the budget is the turn's, not the call's. A `turn` that stops in `Deciding` has not
    /// ended the turn, it has handed a question over, and the `turn` called once it is answered -
    /// from `Deciding` or `Ready` - carries on counting from where the last one left off. Counted
    /// per call, every answered question was a fresh budget, so a turn whose calls were all asked
    /// about ran as many requests as it liked under any limit. A `turn` called from anywhere else
    /// is a new turn, or somebody carrying on past a budget that ran out, and starts from nothing.
    /// An interrupt stops it too, in whatever resting state the loop had reached: [`State::Ready`]
    /// or [`State::Idle`] as often as not.
    pub async fn turn(&self) -> Result<State> {
        let mut requests = {
            let machine = self.0.machine.lock();
            match machine.state {
                State::Deciding { .. } | State::Ready { .. } => machine.turn_requests,
                _ => 0,
            }
        };

        loop {
            // note: this loop does not read the interrupt flag itself. If it did, and the step it
            // then called read it again, an interrupt landing between the two would be spent on a
            // step that transitions nothing, and this loop, seeing an ordinary resting state come
            // back, would go round and send the next request anyway. With one reader, checked in
            // one place, there is no window at all

            // the next step will send a request, so it counts against the budget
            let mut counted = None;
            if matches!(self.state(), State::Idle | State::Finished { .. }) {
                if self
                    .max_requests_per_turn()
                    .is_some_and(|max| requests >= max)
                {
                    // a turn that made no request has nothing to pause
                    if requests > 0 {
                        self.emit(Event::TurnPaused { requests });
                    }
                    return Ok(self.state());
                }
                requests += 1;
                counted = Some(requests);
            }

            // an interrupt the step acknowledged is one this loop must not step past: it was
            // asked to stop, and the step it just spent doing so transitioned nothing
            let state = match self.step_once(counted).await? {
                Attempt::Acknowledged(state) => return Ok(state),
                Attempt::Stepped(state) => state,
            };

            match state {
                State::Finished { .. } | State::Deciding { .. } => return Ok(state),
                _ => continue,
            }
        }
    }

    /// Returns the state a set of prepared calls implies.
    pub(super) fn state_for(prepared: &[PreparedCall]) -> State {
        if prepared.is_empty() {
            return State::Idle;
        }

        let undecided: Vec<_> = prepared
            .iter()
            .filter(|p| p.grant.is_none())
            .map(|p| p.request.id)
            .collect();

        if undecided.is_empty() {
            State::Ready {
                calls: prepared.iter().map(|p| p.call.id.clone()).collect(),
            }
        } else {
            State::Deciding { calls: undecided }
        }
    }

    /// Announces what was decided, then moves the machine to the state it leads to, in one hold
    /// of the machine's lock.
    ///
    /// note: in one hold so that by the time anybody can see a decision or a question, the machine
    /// is already in the state it leads to. A client answering a `permission.requested` finds the
    /// request there to answer rather than failing because it was quick, and a step racing the
    /// announcements waits for them instead of acting on the state before them.
    pub(super) fn settle(
        &self,
        machine: &mut Machine,
        announcements: impl IntoIterator<Item = Event>,
        to: State,
    ) {
        for announcement in announcements {
            self.emit(announcement);
        }
        self.transition(machine, to);
    }

    /// Moves the machine to a state, announcing it.
    ///
    /// note: reaching [`State::Finished`] also puts down an outstanding interrupt, because the
    /// turn it was asked of is over and there is nothing left in flight for it to stop. Without
    /// this the flag would outlive the request it was meant for in the one case
    /// [`Kernel::step_once`] cannot catch: a [`Provider`] that watches
    /// [`crate::DeltaSink::is_interrupted`] and hands back what it had has *honoured* the
    /// interrupt itself, so no step is spent acknowledging it - and the next turn's first step
    /// would be, transitioning nothing and returning the state it was already in. In a client,
    /// the message somebody types after pressing stop would land in the context with nothing
    /// answering it.
    ///
    /// note: only [`State::Finished`]. The resting states an interrupt is *for* -
    /// [`State::Ready`], [`State::Idle`] mid-loop, [`State::Deciding`] - are exactly the ones
    /// from which the loop would otherwise carry on, and there the flag has to survive to be
    /// read by the next step. That is the window the single reader in `step_once` exists to
    /// close, and this does not widen it: a stop asked for as a turn ends has nothing to stop.
    pub(super) fn transition(&self, machine: &mut Machine, to: State) {
        if machine.state == to {
            return;
        }

        if matches!(to, State::Finished { .. }) {
            self.0.interrupted.store(false, SeqCst);
        }

        let from = std::mem::replace(&mut machine.state, to.clone());
        self.emit(Event::StateChanged { from, to });
    }
}
