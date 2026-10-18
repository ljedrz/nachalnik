//! The state machine: the states a turn moves through, and every operation that moves it.
//!
//! note: this file holds the loop; what a turn is made of is in `request` and `calls`, and
//! construction and observation in `mod.rs`. Those files still take the machine's lock where they
//! must (to queue a turn's calls, to undo past a turn, to finish), using what is `pub(super)` here.

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
    /// note: kept rather than read again when the refusal is written, because a `set_policy` while
    /// the call rests in `Ready` or `Deciding` is ordinary, and the new policy would explain a
    /// decision it did not make, in rules the call was never held to.
    pub(super) policy: Arc<dyn PermissionPolicy>,
}

/// The state machine: the calls and the turn's count.
///
/// note: the interrupt flag is not here, though everything that writes it holds this lock. A
/// provider or tool reads it through its sink during the turn, and a read waiting for this lock
/// would stall a stream behind whoever holds it; an announcement can wait on the session log for as
/// long as a `with_history` caller likes.
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
            // the guarded transition failed or was abandoned, so whatever it was asked to stop has
            // stopped. If left set, the interrupt would be spent on the next turn instead,
            // transitioning nothing and leaving the typed message unanswered
            self.kernel.0.interrupted.store(false, SeqCst);
            self.kernel.transition(&mut machine, State::Idle);
        }
    }
}

impl Kernel {
    /// Asks [`Kernel::turn`] to stop at the next opportunity. Returns whether it had already been
    /// asked.
    ///
    /// note: it does not abort a request in flight, since the kernel does not own the task making
    /// it; drop the future driving [`Kernel::step`] for that. The step in progress runs to the end
    /// and is recorded like any other, and `turn` then returns instead of starting another.
    ///
    /// note: how far it reaches depends on what cooperates. Before a transition, [`Kernel::step`]
    /// and [`Kernel::turn`] acknowledge it and do nothing else. During a request, a [`Provider`]
    /// that checks [`crate::DeltaSink::is_interrupted`] can stop reading and return what it has;
    /// during a tool call, a [`Tool`] that checks [`crate::OutputSink::is_interrupted`] can do the
    /// same. When calls run one at a time, the kernel also skips those not yet begun.
    ///
    /// note: with [`Config::parallel_tool_calls`](crate::Config::parallel_tool_calls) on, every
    /// call in the batch has started before the first answers, so an interrupt reaches only tools
    /// that check their sink. In neither mode can a tool that blocks without looking be stopped:
    /// the kernel does not own its thread. Every call still gets an output, even one saying it
    /// never ran.
    ///
    /// note: the flag is spent only by the transition attempt that acts on it, a [`Kernel::step`],
    /// including the one [`Kernel::turn`] makes. It is also cleared when a turn reaches
    /// [`State::Finished`] and when the transition it was stopping fails or is abandoned, so it
    /// never outlives what it was meant to stop. It discards no work: partial answers and results
    /// are recorded. A step refused as [`Error::Busy`] neither acts on it nor clears it, so the
    /// request in flight keeps the stop its provider is reading.
    ///
    /// note: set and announced under the machine lock, where every step reads and spends it.
    /// Otherwise a step could take the flag between the setting and the announcing, and the log
    /// would place `turn.interrupted` after the transition that acted on it.
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

    /// Answers a permission request and returns the state that answer produced.
    ///
    /// note: The state becomes [`State::Ready`] once the last outstanding question is answered.
    /// Nothing runs until then, and nothing runs without a [`Kernel::step`].
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

    /// Drops every prepared tool call, recording each as refused, and returns how many there were.
    ///
    /// note: the model is told the calls were refused, because most providers do not accept a call
    /// without a result. Whether it sees that is up to the [`Projector`]; the record is kept either
    /// way.
    ///
    /// note: one checkpoint for the batch, taken by the first result and joined by the rest, as in
    /// [`Kernel::push_all`]. Dropping the calls is one action, so one `undo` puts the whole batch
    /// back; a checkpoint each would let an `undo` leave some calls answered and others not, and a
    /// long batch would spend [`Config::context_undo_depth`] on one keystroke.
    ///
    /// note: the same shape as [`Kernel::decide`] refusing every call followed by a
    /// [`Kernel::step`]: the refusals are announced as the machine is claimed as
    /// [`State::Executing`], and the results recorded before it returns to [`State::Idle`]. Going
    /// straight to `Idle` would let a step send a request with unanswered calls, and log the
    /// machine idle before the calls were refused. The results are recorded outside the lock
    /// because recording one asks the tool what the call needed, which is somebody else's code.
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

    /// Performs one transition of the state machine and returns the state it produced.
    ///
    /// | from | what happens | to |
    /// | --- | --- | --- |
    /// | [`State::Idle`], [`State::Finished`] | a request is built and sent | `Finished`, `Ready` or `Deciding`; `Idle` if every call named a tool that is not there |
    /// | [`State::Ready`] | the tools run, and their results are recorded in the order the model asked for them | `Idle` |
    /// | [`State::Deciding`] | nothing; the answer has to come from you | `Deciding` |
    /// | [`State::Requesting`], [`State::Executing`] | nothing; [`Error::Busy`] | - |
    /// | any resting state, with an interrupt outstanding | nothing; the interrupt is spent | the same state |
    ///
    /// note: one step is one transition, so a tool being asked for and running are two steps, with
    /// [`State::Ready`] between them as a resting state where nothing has happened yet.
    pub async fn step(&self) -> Result<State> {
        self.step_once(None).await.map(Attempt::state)
    }

    /// One transition attempt, and whether it was spent acknowledging an interrupt rather than
    /// making a transition.
    ///
    /// note: [`Kernel::turn`] cannot work this out: a step that spends an interrupt returns the
    /// state it found, which looks like a resting state to continue from, and the next request
    /// would go out with the stop already logged.
    ///
    /// note: `counted` is the count [`Kernel::turn`] has reached, stored as the turn's own only
    /// where this claims the request it counts; a request a `step` claims with none starts the
    /// count again. Stored before the step, a `turn` refused as busy (a second driver arriving
    /// between another's count and its claim) overwrote the running turn's count, and that turn,
    /// carried on from `Deciding`, counted from the wrong number and went past its budget.
    async fn step_once(&self, counted: Option<usize>) -> Result<Attempt> {
        let claim = {
            let mut machine = self.0.machine.lock();
            let state = machine.state.clone();
            // busy first, before the flag is touched: a step refused as busy transitions nothing,
            // so it is not the attempt the interrupt is spent on, and consuming it here would take
            // the stop from the request actually in flight, whose provider reads `is_interrupted`
            // to decide whether to keep reading
            if state.is_busy() {
                return Err(Error::Busy);
            }
            // somebody asked to stop. One transition attempt is spent acknowledging it, which also
            // keeps the flag from outliving its request: a client driving `step` itself has no
            // `turn` to consume it
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
                    // a request a `step` claims is not a turn's, nor is anything before it, since
                    // it starts from a resting state no `turn` was counting through. Left alone, a
                    // `turn` carried on from here would read the last turn's count
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
    /// move, and [`State::Idle`] means the request budget ran out mid-loop; calling `turn` again
    /// picks up where it left off.
    ///
    /// note: the budget is the turn's, not the call's. A `turn` that stops in `Deciding` has handed
    /// a question over, not ended the turn, and the `turn` called once it is answered (from
    /// `Deciding` or `Ready`) carries on counting. Counted per call, every answered question was a
    /// fresh budget, so a turn whose calls were all asked about could run any number of requests. A
    /// `turn` called from any other state starts a new count. An interrupt stops it too, in
    /// whatever resting state the loop reached, often [`State::Ready`] or [`State::Idle`].
    pub async fn turn(&self) -> Result<State> {
        let mut requests = {
            let machine = self.0.machine.lock();
            match machine.state {
                State::Deciding { .. } | State::Ready { .. } => machine.turn_requests,
                _ => 0,
            }
        };

        loop {
            // note: this loop does not read the interrupt flag itself. If it did, an interrupt
            // landing between its read and the step's would be spent on a step that transitions
            // nothing, and the loop, seeing an ordinary resting state, would send the next request
            // anyway. One reader in one place leaves no window

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

    /// Announces what was decided, then moves the machine to the state it leads to, in one hold of
    /// the machine's lock.
    ///
    /// note: in one hold so that by the time anybody sees a decision or a question, the machine is
    /// already in the state it leads to: a client answering a `permission.requested` finds the
    /// request there, and a step racing the announcements waits for them rather than acting on the
    /// state before them.
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
    /// note: reaching [`State::Finished`] also clears an outstanding interrupt, because the turn it
    /// was asked of is over. This catches the case [`Kernel::step_once`] cannot: a [`Provider`]
    /// that honoured [`crate::DeltaSink::is_interrupted`] itself, so no step spent the flag. The
    /// next turn's first step would spend it instead, transitioning nothing, and in a client the
    /// message typed after pressing stop would sit in the context unanswered.
    ///
    /// note: only [`State::Finished`]. In the resting states an interrupt is for ([`State::Ready`],
    /// [`State::Idle`] mid-loop, [`State::Deciding`]) the loop would otherwise carry on, so the
    /// flag must survive for the next step to read. That is the window the single reader in
    /// `step_once` closes, and this does not widen it: a stop asked for as a turn ends has nothing
    /// to stop.
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
