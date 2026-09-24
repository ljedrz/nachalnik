//! The runtime: the state machine, the context it owns, and every operation a caller has on them.
//!
//! note: the public surface, and beside it the private parts of the machine the states move
//! through - `Machine`, `Claim`, `Restore`. What a turn is *made of* is next door in `request`
//! and `calls`, because somebody reading this file wants to know what the kernel offers rather
//! than how a stream of fragments is read.

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering::SeqCst},
    },
};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::{
    compaction::{Budget, CompactionPlan, CompactionReport, Compactor, Removed},
    config::Config,
    context::{Context, ContextId, ContextItem, ContextKind, ContextState},
    error::{Error, Result},
    event::Event,
    model::{
        Content, ModelInfo, ModelRequest, ModelResponse, Params, Provider, StopReason, ToolCall,
        ToolCallId,
    },
    permissions::{
        AskAlways, Grant, GrantSource, PermissionId, PermissionPolicy, PermissionRequest, Verdict,
    },
    projection::{LinearProjector, Projection, Projector},
    session::{Record, Session, Snapshot},
    tokens::{BytesPerToken, Calibrating, Calibration, TokenCounter},
    tool::{Tool, ToolOutput, ToolSpec},
};

mod calls;
mod request;

use request::projection_cost;

/// A sequential numeric identifier assigned to sessions that were not given a name.
static SEQUENTIAL_SESSION_ID: AtomicU64 = AtomicU64::new(0);

/// What the runtime is doing, and therefore what it will do next.
///
/// The loop is a state machine with one transition per [`Kernel::step`]:
///
/// ```text
///                     ┌──────────────────────────── step ────────────────────────────┐
///                     │                                                              │
///                     ▼                          (no tool calls)                     │
///   Idle ── step ──> Requesting ──────────────────────────────────> Finished ─────────┤
///   Ready                │                                                            │
///     ▲                  ├──(tool calls, all decided by the policy)──> Ready ─── step ─┤──> Executing ──> Idle
///     │                  │                                                            │
///     └── decide ── Deciding <──(tool calls, at least one to ask about)────────────────┘
/// ```
///
/// note: `Requesting` and `Executing` mean somebody else is already driving the loop:
/// [`Kernel::step`] returns [`Error::Busy`] rather than sending a second request or running the
/// same tools twice. Everything else is a resting state, and whatever you change while the
/// kernel rests is what the next request will contain.
///
/// note: If the future driving a transition is dropped - a cancelled task, a client that went
/// away - the kernel returns to [`State::Idle`] instead of staying wedged, and says so on the
/// event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum State {
    /// Nothing is outstanding; the next step builds a request and sends it.
    Idle,
    /// A request is in flight.
    Requesting,
    /// The model asked for tools, and at least one of them needs a decision; answer with
    /// [`Kernel::decide`], or drop them with [`Kernel::cancel_pending_calls`].
    Deciding {
        /// The requests awaiting an answer; the details are in
        /// [`Kernel::pending_permissions`].
        calls: Vec<PermissionId>,
    },
    /// Every call the model asked for is decided; the next step runs them.
    ///
    /// note: This is a resting state on purpose. It is the moment at which the model has said
    /// what it wants to do and nothing has happened yet, which is exactly when a user may want
    /// to look (see [`Kernel::pending_calls`]).
    Ready {
        /// The calls that are about to run, in order.
        calls: Vec<ToolCallId>,
    },
    /// The tools are running.
    Executing {
        /// The calls being run, in order.
        calls: Vec<ToolCallId>,
    },
    /// The model ended its turn. In every other respect this is [`State::Idle`].
    Finished {
        /// The context item the model's turn was recorded as.
        item: ContextId,
        /// Why the model stopped.
        ///
        /// note: This is here because "finished" and "finished *well*" are different things, and
        /// a client that only matched on the variant would not be able to tell them apart. A
        /// turn that ran out of output tokens ends in `Finished` with
        /// [`StopReason::Length`] - and quite possibly no content at all, since a reasoning
        /// model can spend its whole budget before saying anything. Whether to continue, warn or
        /// shrug is the client's call; the kernel's job is not to let it pass unnoticed.
        stop: StopReason,
    },
}

impl State {
    /// Returns whether somebody is already driving the loop, so that a [`Kernel::step`] would
    /// fail with [`Error::Busy`].
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Requesting | Self::Executing { .. })
    }

    /// Returns the state's name, e.g. `requesting`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Requesting => "requesting",
            Self::Deciding { .. } => "deciding",
            Self::Ready { .. } => "ready",
            Self::Executing { .. } => "executing",
            Self::Finished { .. } => "finished",
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What swapping one snapshot of the context for another did; see [`Kernel::diff`].
struct Diff {
    items: usize,
    gone: Vec<ContextId>,
    appeared: Vec<ContextId>,
    changed: Vec<ContextId>,
}

/// What a [`Kernel::set_state`] did, item by item.
///
/// note: The three lists are kept apart because "there is no item 12" and "item 12 was already
/// excluded" are different things to tell a user, and a single count of what moved cannot say
/// which happened.
///
/// note: `#[non_exhaustive]` because this is an answer rather than a request. Nothing outside
/// this crate builds one - [`Kernel::set_state`] is where they come from - so the attribute costs
/// a caller nothing and makes a fourth list, the day there is one to report, a patch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StateChange {
    /// The items that moved.
    pub changed: Vec<ContextId>,
    /// The items that were already in that state, with that note.
    pub unchanged: Vec<ContextId>,
    /// The identifiers that name nothing.
    pub unknown: Vec<ContextId>,
}

impl StateChange {
    /// Returns whether nothing at all happened.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }

    /// Returns how many items moved.
    pub fn len(&self) -> usize {
        self.changed.len()
    }
}

/// A run of recorded items that one [`Kernel::undo`] takes back whole; see [`Context::fold_since`].
///
/// note: `None` until the first of them is added, which takes the checkpoint the rest belong to.
/// A batch that joins a checkpoint already taken - the turn a call nobody can run answers - starts
/// with that one's number.
#[derive(Default)]
struct Batch(Option<u64>);

/// A tool call that has been matched to a tool and is waiting for a decision, or for its turn.
struct PreparedCall {
    call: ToolCall,
    tool: Arc<dyn Tool>,
    request: PermissionRequest,
    grant: Option<(Grant, GrantSource)>,
}

/// The state machine and the calls it is holding on to.
struct Machine {
    state: State,
    pending: Vec<PreparedCall>,
}

/// What a [`Kernel::step`] claimed the right to do.
enum Claim {
    Request,
    Execute(Vec<PreparedCall>),
}

/// Puts the state machine back if the future driving a transition never finishes.
struct Restore<'a> {
    kernel: &'a Kernel,
    to: Option<State>,
}

impl<'a> Restore<'a> {
    fn new(kernel: &'a Kernel, to: State) -> Self {
        Self {
            kernel,
            to: Some(to),
        }
    }

    fn disarm(&mut self) {
        self.to = None;
    }
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        if let Some(to) = self.to.take() {
            let mut machine = self.kernel.0.machine.lock();
            // the transition this was guarding failed or was abandoned, so whatever it was asked
            // to stop has stopped. Left set, the interrupt would be spent on the next turn instead
            // - which transitions nothing, and the message somebody typed goes unanswered
            self.kernel.0.interrupted.store(false, SeqCst);
            self.kernel.transition(&mut machine, to);
        }
    }
}

/// The state a kernel holds; see [`Kernel`].
struct InnerKernel {
    config: Config,
    machine: Mutex<Machine>,
    context: RwLock<Context>,
    session: Mutex<Session>,
    events: broadcast::Sender<Event>,
    provider: RwLock<Option<Arc<dyn Provider>>>,
    /// The model the log last said was in use; see [`Kernel::provider_changed`].
    announced: Mutex<Option<ModelInfo>>,
    tools: RwLock<BTreeMap<String, Arc<dyn Tool>>>,
    policy: RwLock<Arc<dyn PermissionPolicy>>,
    projector: RwLock<Arc<dyn Projector>>,
    counter: RwLock<Arc<dyn TokenCounter>>,
    compactor: RwLock<Option<Arc<dyn Compactor>>>,
    params: RwLock<Params>,
    last_response: RwLock<Option<Arc<ModelResponse>>>,
    next_permission: AtomicU64,
    /// Every tool call identifier the session has used, so that a later turn cannot quietly
    /// reuse one; see [`Kernel::repair_call_ids`].
    seen_calls: Mutex<HashSet<ToolCallId>>,
    /// Whether somebody has asked [`Kernel::turn`] to stop at the next opportunity.
    interrupted: AtomicBool,
}

/// The agent runtime: a state machine, a context, and nothing else.
///
/// A kernel is a cheaply clonable handle to shared state, so clients, tools and policies can
/// hold on to it without ceremony.
///
/// note: A kernel does nothing on its own. [`Kernel::step`] and [`Kernel::turn`] run on the
/// caller's task, for as long as the caller lets them; between two steps, nothing is happening.
/// It spawns no tasks either, with one opted-in exception: [`Config::parallel_tool_calls`] gives
/// each of a turn's tool calls a task of its own, for the length of the step that started them
/// and no longer.
///
/// note: One agent is one kernel. A fleet of them shares nothing except whatever you hand to
/// both - a [`Tool`], a [`Provider`] - and needs no coordination, because a kernel's state is its
/// own. Several threads driving *one* kernel is also fine: reading is cheap and every mutation is
/// atomic, so a client can render, prune and preview while a turn is running. What it cannot do
/// is drive the loop twice at once, which is [`Error::Busy`] rather than a second request.
///
/// note: A kernel is not `Drop`-safe against reference cycles: if you store a `Kernel` inside a
/// [`Tool`], a [`Provider`] or a [`PermissionPolicy`] that the same kernel holds, the cycle
/// keeps everything alive. Either store a [`std::sync::Weak`] to your own state, or drop the
/// components ([`Kernel::remove_tool`], [`Kernel::clear_provider`]) when you are done.
#[derive(Clone)]
pub struct Kernel(Arc<InnerKernel>);

impl fmt::Debug for Kernel {
    /// note: A summary, not a dump. Everything here has an accessor that returns the real thing;
    /// this is for the line of a log that says which kernel is being talked about.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // gathered before the context is read, so that a `{kernel:?}` cannot be the thing that
        // orders two locks the wrong way round
        let session = self.session_name();
        let state = self.state();
        let tools = self.tool_ids();
        let model = self
            .model_info()
            .map(|info| format!("{}/{}", info.provider, info.model));

        let context = self.0.context.read();
        f.debug_struct("Kernel")
            .field("session", &session)
            .field("state", &state.name())
            .field("model", &model)
            .field("tools", &tools)
            .field("items", &context.len())
            .field("tokens", &context.tokens())
            .finish()
    }
}

impl Kernel {
    /// Creates a kernel with the given [`Config`], and broadcasts [`Event::SessionStarted`].
    ///
    /// The kernel starts out in [`State::Idle`] with no provider, no tools, an [`AskAlways`]
    /// permission policy, a [`LinearProjector`], a [`Calibrating<BytesPerToken>`] token counter,
    /// no compactor, no parameters, and an empty context: it will not talk to anything, or agree
    /// to anything, until it is told to.
    ///
    /// note: The counter is wrapped rather than bare because [`Calibrating`] costs nothing until
    /// it is told something: it corrects by `1.0` until a provider has reported what a request
    /// actually cost, so it *is* [`BytesPerToken`] until there is something better to be. Unwrap
    /// it with [`Kernel::set_counter`] where a measurement needs a counter that never changes its
    /// mind.
    ///
    /// [`Calibrating<BytesPerToken>`]: Calibrating
    pub fn new(config: Config) -> Self {
        let kernel = Self::build(config);
        kernel.emit(Event::SessionStarted {
            session: kernel.session_name(),
        });

        kernel
    }

    /// Creates a kernel that carries on from a [`Snapshot`], and broadcasts
    /// [`Event::SessionResumed`].
    ///
    /// The context, the parameters and the identifiers the session had already handed out - for
    /// items, tool calls, permission requests and records - come back; everything else is as
    /// [`Kernel::new`] leaves it, because a provider, a policy and a set of tools are the caller's
    /// to supply and were never the session's to remember. The new log starts empty and numbers
    /// its first record, [`Event::SessionResumed`], after the last one the snapshot's session had.
    ///
    /// note: The name comes from the snapshot unless [`Config::session_name`] is set, which is
    /// how a session gets forked rather than continued.
    ///
    /// note: The items are recounted with this kernel's [`TokenCounter`], so a snapshot taken
    /// under a different one reports honest numbers rather than inherited ones.
    ///
    /// note: What the previous counter had *learned* comes back too, where both counters deal in
    /// [`Calibration`](crate::Calibration)s, and it is offered *before* the items are counted - so
    /// a resumed session does not spend its first requests relearning what it had been told, and
    /// the figures it comes back with are corrected rather than the stale ones it was saved with.
    /// That is a visible change in the numbers, and is what [`Kernel::recount`] before saving
    /// would have produced.
    pub fn resume(mut config: Config, snapshot: Snapshot) -> Self {
        config
            .session_name
            .get_or_insert_with(|| snapshot.session.clone());
        let kernel = Self::build(config);

        {
            let counter = kernel.counter();
            // before the items are counted, not after: a resumed context that reported one set of
            // figures and then corrected itself on the first response would be showing the user
            // two different budgets for the same bytes, which is the thing a snapshot exists to
            // avoid
            if let Some(calibration) = snapshot.calibration {
                counter.recalibrate(calibration);
            }
            // every call the items name, not only the ones `used_calls` lists: a snapshot merged or
            // written by hand can leave a call out of the list, and a provider handing that
            // identifier back would then go unrepaired, two results answering one call
            kernel.0.seen_calls.lock().extend(
                snapshot
                    .items
                    .iter()
                    .flat_map(crate::session::named_calls)
                    .cloned(),
            );
            kernel
                .0
                .context
                .write()
                .restore(snapshot.items, snapshot.next_item, &*counter);
        }
        *kernel.0.params.write() = snapshot.params;
        kernel.0.seen_calls.lock().extend(snapshot.used_calls);
        kernel
            .0
            .next_permission
            .store(snapshot.next_permission.max(1), SeqCst);
        kernel.0.session.lock().carry_on_from(snapshot.last_seq);

        let (items, tokens) = {
            let context = kernel.0.context.read();
            (context.len(), context.tokens())
        };
        kernel.emit(Event::SessionResumed {
            session: kernel.session_name(),
            items,
            tokens,
        });

        kernel
    }

    /// Tells the kernel that these tool call identifiers are already spoken for, and returns how
    /// many of them it had not already been told about.
    ///
    /// note: [`Kernel::resume`] does this from [`Snapshot::used_calls`], which covers a session
    /// picked back up in another process. This is for the other way of reading a snapshot: a
    /// client that merges one *into* a session it is already running keeps the kernel it has, so
    /// the turns it pushes arrive carrying identifiers this kernel never issued.
    /// Without this the next response is free to hand one of them back, the repair has nothing to
    /// compare it against, and the request that follows carries the same `tool_call_id` twice -
    /// which most providers reject, and which is very hard to see afterwards. See
    /// [`Event::ToolCallRepaired`].
    ///
    /// note: identifiers and nothing else. What they belonged to is not the kernel's business,
    /// and one reserved for an item that is later pruned stays reserved, because an identifier is
    /// never reused - not even by an item [`Kernel::undo`] took away.
    pub fn reserve_calls(&self, calls: impl IntoIterator<Item = ToolCallId>) -> usize {
        let mut seen = self.0.seen_calls.lock();
        let before = seen.len();
        seen.extend(calls);
        let reserved = seen.len() - before;

        if reserved != 0 {
            self.emit(Event::ToolCallsReserved { reserved });
        }

        reserved
    }

    /// Returns everything a later [`Kernel::resume`] needs to carry on from here.
    ///
    /// note: Cheap enough to take after every turn, and worth it: this is the only thing that
    /// can rebuild a context. The event log cannot, by design - an event names an item rather
    /// than carrying its contents, which is what makes the log affordable to keep.
    ///
    /// note: the context is read before the used call identifiers, not after. A turn reserves a
    /// call's identifier before it records the item carrying the call, and an identifier is never
    /// given back - so identifiers read afterwards cover every call in the items read before. Read
    /// the other way round, a turn recorded between the two would leave a call in `items` whose
    /// identifier `used_calls` does not have, for a resumed session to hand out again.
    pub fn snapshot(&self) -> Snapshot {
        let session = self.session_name();
        let params = self.params();
        let counter = self.counter();

        // `last_seq` under the context lock, because every change to the context is announced
        // while holding it: every record numbered up to it describes a change these items already
        // show, and every one after it a change they do not. That is what lets a caller write the
        // log out to exactly this point and have the pair agree
        let (items, next_item, last_seq): (Vec<ContextItem>, _, _) = {
            let context = self.0.context.read();
            (
                context.items().iter().map(|i| (**i).clone()).collect(),
                context.next_id(),
                self.last_seq(),
            )
        };
        // and every call the items name, reserved or not: a client may push a turn it did not
        // reserve the calls of, and a snapshot whose items name a call its list leaves out is one
        // `Snapshot::problems` calls inconsistent
        let mut used_calls: Vec<_> = self.0.seen_calls.lock().iter().cloned().collect();
        used_calls.extend(items.iter().flat_map(crate::session::named_calls).cloned());
        used_calls.sort();
        used_calls.dedup();

        Snapshot {
            session,
            items,
            params,
            next_item,
            used_calls,
            last_seq,
            next_permission: self.0.next_permission.load(SeqCst),
            calibration: counter.calibration(),
        }
    }

    /// Assembles a kernel without announcing anything.
    fn build(mut config: Config) -> Self {
        // if there is no pre-configured name, assign a sequential numeric identifier
        let name = config
            .session_name
            .get_or_insert_with(|| SEQUENTIAL_SESSION_ID.fetch_add(1, SeqCst).to_string())
            .clone();

        let (events, _) = broadcast::channel(config.event_queue_depth.max(1));
        let inner = InnerKernel {
            machine: Mutex::new(Machine {
                state: State::Idle,
                pending: Vec::new(),
            }),
            context: RwLock::new(Context::new(config.context_undo_depth)),
            session: Mutex::new(Session::new(name)),
            events,
            provider: RwLock::new(None),
            announced: Mutex::new(None),
            tools: RwLock::new(BTreeMap::new()),
            policy: RwLock::new(Arc::new(AskAlways)),
            projector: RwLock::new(Arc::new(LinearProjector::default())),
            // note: wrapped, not bare, for the reason on `Kernel::new`. `BytesPerToken` is an
            // admitted estimate and a low one, and this way a user who never reads the
            // documentation still gets the honest number
            counter: RwLock::new(Arc::new(Calibrating::new(BytesPerToken::default()))),
            compactor: RwLock::new(None),
            params: RwLock::new(Params::new()),
            last_response: RwLock::new(None),
            next_permission: AtomicU64::new(1),
            seen_calls: Mutex::new(HashSet::new()),
            interrupted: AtomicBool::new(false),
            config,
        };

        Self(Arc::new(inner))
    }

    // ---------------------------------------------------------------- session and observation

    /// Returns the kernel's configuration.
    pub fn config(&self) -> &Config {
        &self.0.config
    }

    /// Returns the session's name.
    pub fn session_name(&self) -> String {
        self.0.session.lock().name().to_owned()
    }

    /// Subscribes to the event stream.
    ///
    /// note: A subscriber that cannot keep up with [`Config::event_queue_depth`] starts missing
    /// events (`tokio`'s broadcast semantics). The event stream is the live view; the session
    /// log, via [`Kernel::history`], is the complete one.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }

    /// Returns the session's records, oldest first.
    ///
    /// note: A copy of the whole log, which on a long session - and longer still with
    /// [`Config::record_payloads`] on - is not free. [`Kernel::history_since`] is the one to
    /// reach for when a client is following along, and [`Kernel::with_history`] when the answer
    /// is a count or a search rather than the records themselves.
    pub fn history(&self) -> Vec<Record> {
        self.0.session.lock().records().cloned().collect()
    }

    /// Runs a closure against the session log, copying nothing.
    ///
    /// note: The mirror of [`Kernel::with_context`], and it carries a sharper version of the same
    /// warning: the log is locked for the duration, so the closure must not call back into the
    /// kernel - every emit on this kernel is waiting on it.
    pub fn with_history<R>(&self, f: impl FnOnce(&Session) -> R) -> R {
        f(&self.0.session.lock())
    }

    /// Returns the session's records that follow the given sequence number.
    pub fn history_since(&self, seq: u64) -> Vec<Record> {
        self.0.session.lock().since(seq).cloned().collect()
    }

    /// Returns the sequence number of the most recent record.
    pub fn last_seq(&self) -> u64 {
        self.0.session.lock().last_seq()
    }

    /// Removes and returns the records up to and including `through`, oldest first.
    ///
    /// note: The log is unbounded because a capped append-only log is not one. This is the other
    /// way to keep a long session from growing forever: take the records, write them somewhere,
    /// and the kernel stops holding on to them. Nothing goes missing behind anybody's back: every
    /// record the kernel lets go of is handed to the caller who asked.
    pub fn drain_history(&self, through: u64) -> Vec<Record> {
        self.0.session.lock().drain_through(through)
    }

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

    /// Returns the most recent response, in full - including the token counts the provider
    /// reported and its raw payload.
    ///
    /// note: The tool call identifiers here are the ones the kernel settled on (see
    /// [`Event::ToolCallRepaired`]); [`ModelResponse::raw`] still holds whatever the provider
    /// actually sent.
    pub fn last_response(&self) -> Option<Arc<ModelResponse>> {
        self.0.last_response.read().clone()
    }

    /// Broadcasts [`Event::SessionFinished`].
    ///
    /// note: This is a marker for whoever is reading the log, not a shutdown: the kernel owns no
    /// tasks and remains perfectly usable afterwards.
    pub fn finish(&self) {
        self.emit(Event::SessionFinished);
    }

    /// Records an event in the session log and broadcasts it.
    ///
    /// note: The log is written and the event is broadcast under the same lock, so what a
    /// subscriber sees is in the same order as what the log ends up holding, even when several
    /// threads are driving the same kernel.
    ///
    /// note: Callers emit while still holding the lock that made the change - the machine lock
    /// for a transition, the context lock for everything the context does. Releasing first and
    /// announcing afterwards looks tidier and is wrong: two threads excluding the same item can
    /// then apply in one order and be logged in the other, and the log's last word on that item
    /// contradicts the item. The order is machine, then context, then session, and nothing goes
    /// back up it - `emit` takes the session lock and nothing else, and `send` on a broadcast
    /// channel runs no subscriber code, so holding a lock across it costs a memcpy and blocks
    /// nobody.
    ///
    /// note: the component setters hold their own lock the same way, which is what stops two
    /// clients swapping the same component from applying in one order and being logged in the
    /// other - and the log's last word on the provider naming the one that is not installed. What
    /// this asks of a component is that [`Provider::info`] and the `name` of a policy, projector,
    /// counter or compactor do not reach back into the kernel: each is called while the lock
    /// holding that component is held, to say what was replaced.
    pub(crate) fn emit(&self, event: Event) {
        let mut session = self.0.session.lock();

        let is_progress = matches!(event, Event::ModelDelta { .. } | Event::ToolOutput { .. });
        if !is_progress || self.0.config.record_progress {
            session.append(event.clone());
        }
        // an error only means nobody is listening; `send` does not block
        let _ = self.0.events.send(event);
    }

    // ------------------------------------------------------------------------------ components

    /// Sets the provider, returning the previous one.
    pub fn set_provider(&self, provider: Arc<dyn Provider>) -> Option<Arc<dyn Provider>> {
        let to = provider.info();
        let mut held = self.0.provider.write();
        let previous = held.replace(provider);
        *self.0.announced.lock() = Some(to.clone());
        // still under the lock: see the note on `Kernel::emit`
        self.emit(Event::ModelChanged {
            from: previous.as_ref().map(|p| p.info()),
            to: Some(to),
        });

        previous
    }

    /// Removes the provider, returning it.
    pub fn clear_provider(&self) -> Option<Arc<dyn Provider>> {
        let mut held = self.0.provider.write();
        let previous = held.take();
        *self.0.announced.lock() = None;
        self.emit(Event::ModelChanged {
            from: previous.as_ref().map(|p| p.info()),
            to: None,
        });

        previous
    }

    /// Says that the provider this kernel holds now answers as a different model, and announces it
    /// as [`Event::ModelChanged`]; returns whether anything had changed.
    ///
    /// note: for a provider that switches its model in place, which is the only way a client can
    /// switch one it shares with something else. [`Kernel::set_provider`] with the same provider
    /// would ask it what it was *after* the switch and report `from` and `to` as the same model,
    /// so the kernel remembers what it last announced and compares against that instead.
    pub fn provider_changed(&self) -> bool {
        // the component's lock, held while the change is announced, for the reason `emit` gives
        let held = self.0.provider.write();
        let to = held.as_ref().map(|provider| provider.info());
        let mut announced = self.0.announced.lock();
        if *announced == to {
            return false;
        }
        let from = std::mem::replace(&mut *announced, to.clone());
        self.emit(Event::ModelChanged { from, to });

        true
    }

    /// Records that the policy was told something that changes what it answers, as
    /// [`Event::PolicyRuled`].
    ///
    /// note: a record and nothing more. The kernel holds no rules and cannot see a policy's, so
    /// whoever changed one says what it changed, and the record is as good as what it is told -
    /// which is why the event names the rule in the policy's own words rather than in a form the
    /// kernel checks. `answering` is the question whose answer made it, where one did, and `once`
    /// that it holds for that question's call alone.
    pub fn record_rule(
        &self,
        subject: impl Into<String>,
        verdict: Verdict,
        answering: Option<PermissionId>,
        once: bool,
    ) {
        self.emit(Event::PolicyRuled {
            subject: subject.into(),
            verdict,
            answering,
            once,
        });
    }

    /// Returns the provider, if one is set.
    pub fn provider(&self) -> Option<Arc<dyn Provider>> {
        self.0.provider.read().clone()
    }

    /// Returns the identity and capabilities of the model in use.
    pub fn model_info(&self) -> Option<ModelInfo> {
        self.0.provider.read().as_ref().map(|p| p.info())
    }

    /// Registers a tool, returning the one it replaced, if any.
    pub fn add_tool(&self, tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
        let id = tool.spec().id;
        let mut held = self.0.tools.write();
        let previous = held.insert(id, tool);
        // the list is read off the guard rather than through `tool_ids`, which would ask for a
        // read lock on the one this guard holds for writing
        self.emit(Event::ToolsChanged {
            tools: held.keys().cloned().collect(),
        });

        previous
    }

    /// Unregisters a tool, returning it.
    pub fn remove_tool(&self, id: &str) -> Option<Arc<dyn Tool>> {
        let mut held = self.0.tools.write();
        let previous = held.remove(id);
        if previous.is_some() {
            self.emit(Event::ToolsChanged {
                tools: held.keys().cloned().collect(),
            });
        }

        previous
    }

    /// Returns the tool with the given identifier.
    pub fn tool(&self, id: &str) -> Option<Arc<dyn Tool>> {
        self.0.tools.read().get(id).cloned()
    }

    /// Returns the identifiers of the registered tools, in the order they are offered to the
    /// model.
    pub fn tool_ids(&self) -> Vec<String> {
        self.0.tools.read().keys().cloned().collect()
    }

    /// Returns the definitions of the registered tools, exactly as they will be sent.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.0.tools.read().values().map(|t| t.spec()).collect()
    }

    /// Sets the permission policy, returning the previous one.
    pub fn set_policy(&self, policy: Arc<dyn PermissionPolicy>) -> Arc<dyn PermissionPolicy> {
        let to = policy.name().to_owned();
        let mut held = self.0.policy.write();
        let previous = std::mem::replace(&mut *held, policy);
        self.emit(Event::PolicyChanged {
            from: previous.name().to_owned(),
            to,
        });

        previous
    }

    /// Returns the permission policy.
    pub fn policy(&self) -> Arc<dyn PermissionPolicy> {
        self.0.policy.read().clone()
    }

    /// Sets the projector, returning the previous one.
    pub fn set_projector(&self, projector: Arc<dyn Projector>) -> Arc<dyn Projector> {
        let to = projector.name().to_owned();
        let mut held = self.0.projector.write();
        let previous = std::mem::replace(&mut *held, projector);
        self.emit(Event::ProjectorChanged {
            from: previous.name().to_owned(),
            to,
        });

        previous
    }

    /// Returns the projector.
    pub fn projector(&self) -> Arc<dyn Projector> {
        self.0.projector.read().clone()
    }

    /// Sets the token counter and recounts the context, returning the previous counter.
    pub fn set_counter(&self, counter: Arc<dyn TokenCounter>) -> Arc<dyn TokenCounter> {
        let to = counter.name().to_owned();
        let previous = {
            let mut held = self.0.counter.write();
            let previous = std::mem::replace(&mut *held, counter);
            self.emit(Event::CounterChanged {
                from: previous.name().to_owned(),
                to,
            });

            previous
        };
        // the recount waits until the lock is let go, because it reads the counter through it.
        // What has to be under one lock is the change and the announcement of it; the figures
        // the recount brings into line are announced as an event of their own
        self.recount();

        previous
    }

    /// Returns the token counter.
    pub fn counter(&self) -> Arc<dyn TokenCounter> {
        self.0.counter.read().clone()
    }

    /// Tells the counter what a previous one had learned, recounts, and returns what it knew
    /// before.
    ///
    /// note: the front door for [`TokenCounter::recalibrate`], and it exists because reaching
    /// past the kernel for it is a trap. A correction changes what is counted *from then on*,
    /// like [`TokenCounter::observe`] - so a caller who applies one to a context that is already
    /// counted has every stored figure on the old scale and every projected figure on the new
    /// one, which is two budgets for the same bytes. [`Kernel::resume`] avoids it by
    /// recalibrating before the items are counted; anything reading a
    /// [`Snapshot::calibration`] into a session that is already running wants this instead.
    ///
    /// note: it recounts for the same reason [`Kernel::set_counter`] does. What changed is what
    /// the kernel's numbers mean, and leaving the old ones to be read is the quiet rewrite this
    /// crate does not do - so the correction is applied and the figures are brought into line, out
    /// loud, as [`Event::ContextRecounted`].
    ///
    /// note: `None`, and nothing done at all, when the counter does not learn. The kernel only
    /// ever offers back what a counter gave it, which is the rule [`TokenCounter::calibration`]
    /// states; a counter that never changes its mind has nothing to be told and nothing to
    /// recount for. A correction identical to the one in force is not a change either, and takes
    /// no recount - judged by what the counter reports afterwards rather than by what it was
    /// handed, because a counter may apply less than it is offered.
    pub fn recalibrate(&self, calibration: Calibration) -> Option<Calibration> {
        let counter = self.counter();
        let previous = counter.calibration()?;
        counter.recalibrate(calibration);
        // a counter is entitled to hold a correction to what it can actually apply, and
        // `Calibrating` does - so offering it a scale it refuses is not a change to recount for,
        // and a scale it takes in part is a recount against the part it took
        if counter.calibration() != Some(previous) {
            self.recount();
        }

        Some(previous)
    }

    /// Sets (or, with `None`, removes) the compactor, returning the previous one.
    ///
    /// note: With no compactor set, the context only ever changes because somebody asked it to.
    pub fn set_compactor(
        &self,
        compactor: Option<Arc<dyn Compactor>>,
    ) -> Option<Arc<dyn Compactor>> {
        let to = compactor.as_ref().map(|c| c.name().to_owned());
        let mut held = self.0.compactor.write();
        let previous = std::mem::replace(&mut *held, compactor);
        self.emit(Event::CompactorChanged {
            from: previous.as_ref().map(|c| c.name().to_owned()),
            to,
        });

        previous
    }

    /// Returns the compactor, if one is set.
    pub fn compactor(&self) -> Option<Arc<dyn Compactor>> {
        self.0.compactor.read().clone()
    }

    /// Returns the parameters that will be sent with the next request.
    pub fn params(&self) -> Params {
        self.0.params.read().clone()
    }

    /// Sets the parameters sent with every request, returning the previous ones.
    ///
    /// note: parameters equal to the ones in force are not an operation, which is the rule
    /// [`Kernel::replace`] and [`Kernel::set_state`] already follow. `model.params` over a request
    /// that will go out byte for byte the same is a change somebody reading the log goes looking
    /// for and cannot find. This is the one component setter that can tell: the rest hold a trait
    /// object, and two `Arc<dyn Provider>` that would behave alike are not comparable.
    pub fn set_params(&self, params: Params) -> Params {
        let mut held = self.0.params.write();
        if *held == params {
            return params;
        }
        let previous = std::mem::replace(&mut *held, params.clone());
        self.emit(Event::ModelParamsChanged { params });

        previous
    }

    // --------------------------------------------------------------------------------- context

    /// Adds an item to the context, returning its identifier.
    pub fn push(&self, item: ContextItem) -> ContextId {
        self.add_item(item, true)
    }

    /// Adds several items as one undoable operation, returning their identifiers.
    ///
    /// note: Separate [`Kernel::push`]es are a checkpoint each, so opening a project file by file
    /// can spend the whole undo history before the user has done anything. Putting a set of files
    /// in the context is one thing the user did, so it is one thing to undo.
    ///
    /// note: under one lock. Taken item by item, a turn recorded on another thread halfway through
    /// would land inside this checkpoint - and one `undo` would take back the turn and the tail of
    /// the files and leave their head.
    pub fn push_all(&self, items: impl IntoIterator<Item = ContextItem>) -> Vec<ContextId> {
        let mut items = items.into_iter().peekable();
        if items.peek().is_none() {
            return Vec::new();
        }

        let counter = self.counter();
        let mut context = self.0.context.write();
        context.checkpoint();

        items
            .map(|item| self.added(&mut context, item, &*counter))
            .collect()
    }

    /// Adds an item in place of an existing one, marking the old one
    /// [`ContextState::Superseded`], as one undoable operation.
    ///
    /// note: This is the operation that state is for. [`Kernel::set_state`] can set it too, and
    /// leaves saying what replaced the item to whoever did. It is explicit because the kernel
    /// cannot tell whether a second read of a file replaces the first or stands beside it. The
    /// old item keeps its identifier and its contents, and comes back with a
    /// [`Kernel::set_state`] or a [`Kernel::undo`] like anything else.
    pub fn supersede(&self, old: ContextId, item: ContextItem) -> Result<ContextId> {
        // one lock for the check and both changes, so that an `undo` on another thread cannot take
        // `old` away in between and leave this answering `Ok` having superseded nothing
        let counter = self.counter();
        let mut context = self.0.context.write();
        if context.item(old).is_none() {
            return Err(Error::UnknownItem(old));
        }
        context.checkpoint();

        let new = self.added(&mut context, item, &*counter);
        let note = Some(format!("replaced by item {new}"));
        if let Some(from) = context.set_state(old, ContextState::Superseded, note.clone()) {
            self.emit(Event::ContextChanged {
                id: old,
                from,
                to: ContextState::Superseded,
                note,
            });
        }

        Ok(new)
    }

    /// Replaces an item's metadata, which is otherwise write-once.
    ///
    /// note: [`ContextItem::meta`] exists so that a [`Compactor`] or a [`Projector`] can be given
    /// hints, and a hint you can only set before the item exists is not much use - by the time
    /// you know a tool result was worthless, it has already been recorded.
    pub fn annotate(&self, id: ContextId, meta: Value) -> Result<()> {
        let mut context = self.0.context.write();
        if context.item(id).is_none() {
            return Err(Error::UnknownItem(id));
        }

        if let Some(was) = context.annotate(id, meta.clone()) {
            self.emit(Event::ContextAnnotated { id, meta, was });
        }

        Ok(())
    }

    /// Returns every context item, in insertion order, whatever its state.
    pub fn items(&self) -> Vec<Arc<ContextItem>> {
        self.0.context.read().items().to_vec()
    }

    /// Returns the context item with the given identifier.
    pub fn item(&self, id: ContextId) -> Option<Arc<ContextItem>> {
        self.0.context.read().item(id).cloned()
    }

    /// Runs a closure against the context.
    ///
    /// note: The context is read-locked for the duration, so the closure must not call back into
    /// the kernel.
    pub fn with_context<R>(&self, f: impl FnOnce(&Context) -> R) -> R {
        f(&self.0.context.read())
    }

    /// Moves the given items to a state, as one undoable operation, and returns the ones that
    /// actually changed.
    ///
    /// Excluding, restoring and pinning are all this one call:
    ///
    /// ```text
    /// exclude -> set_state(ids, ContextState::Excluded, Some("garbage".into()))
    /// restore -> set_state(ids, ContextState::Active, None)
    /// pin     -> set_state(ids, ContextState::Pinned, None)
    /// ```
    ///
    /// note: Nothing is destroyed. An excluded item keeps its identifier, is still listed by
    /// [`Kernel::items`], and comes back with another `set_state` or with [`Kernel::undo`].
    pub fn set_state(
        &self,
        ids: impl IntoIterator<Item = ContextId>,
        state: ContextState,
        note: Option<String>,
    ) -> StateChange {
        let ids: Vec<_> = ids.into_iter().collect();
        let mut outcome = StateChange::default();
        if ids.is_empty() {
            return outcome;
        }

        let mut announcements = Vec::new();
        {
            // the whole operation happens under one lock, so that the checkpoint it takes is a
            // snapshot of the context the changes are actually applied to
            let mut context = self.0.context.write();
            let mut targets = Vec::new();
            for id in ids {
                match context.item(id) {
                    None => outcome.unknown.push(id),
                    Some(_) if context.would_change(id, state, &note) => targets.push(id),
                    Some(_) => outcome.unchanged.push(id),
                }
            }

            // an operation that changes nothing does not get a checkpoint: spending one would
            // make the next `undo` put back what is already there, and the operation somebody
            // wanted reverted would need a second one
            if targets.is_empty() {
                return outcome;
            }
            context.checkpoint();

            for id in targets {
                let Some(from) = context.set_state(id, state, note.clone()) else {
                    continue;
                };
                announcements.push(Event::ContextChanged {
                    id,
                    from,
                    to: state,
                    note: note.clone(),
                });
                outcome.changed.push(id);
            }

            // still under the lock: see the note on `Kernel::emit`
            for announcement in announcements {
                self.emit(announcement);
            }
        }

        outcome
    }

    /// Replaces an item's content in place, keeping its identifier.
    ///
    /// note: a replacement with what the item already says is not an operation. Nothing observable
    /// changes, so nothing is announced and no checkpoint is taken - the same rule
    /// [`Kernel::set_state`] follows through `Context::would_change`. A checkpoint for it would be
    /// an undo that puts back a state identical to the one it was asked from, and the operation
    /// somebody actually wanted reverted would need a second one.
    pub fn replace(&self, id: ContextId, content: impl Into<Content>) -> Result<()> {
        let counter = self.counter();
        let content = content.into();
        let mut context = self.0.context.write();
        // an operation that is about to fail does not get a checkpoint
        let Some(item) = context.item(id) else {
            return Err(Error::UnknownItem(id));
        };
        if item.content == content {
            return Ok(());
        }
        context.checkpoint();

        match context.replace(id, content, &*counter) {
            Some((was, tokens_before, tokens_after)) => {
                self.emit(Event::ContextReplaced {
                    id,
                    tokens_before,
                    tokens_after,
                    was,
                });
                Ok(())
            }
            None => Err(Error::UnknownItem(id)),
        }
    }

    /// Reverts the most recent context operation, returning whether there was one - or
    /// [`Error::Busy`] while a turn holds calls.
    ///
    /// note: The granularity is one operation, not one item: undoing a [`Kernel::set_state`]
    /// that excluded eight items puts all eight back. Model responses and tool results are
    /// operations too, so undo can also walk back a turn's worth of additions.
    ///
    /// note: refused from [`State::Requesting`] until the machine is resting with nothing to
    /// run, which takes in [`State::Deciding`], [`State::Ready`] and [`State::Executing`], because
    /// what it would rewind is the context the machine is acting on. Undoing the turn that asked
    /// for a call did not stop the call: it ran, its result was recorded against a turn no longer
    /// there and never reached the model, and recording it took a checkpoint that made the undone
    /// turn unreachable. Decide the calls, or cancel them with [`Kernel::cancel_pending_calls`],
    /// first.
    ///
    /// note: an undo that takes away the answer [`State::Finished`] names moves the machine to
    /// [`State::Idle`]. Left `Finished`, the state would name an item that is not there.
    pub fn undo(&self) -> Result<bool> {
        // the machine first, for the lock order, and held with the context so that no turn starts
        // between the look and the undo
        let mut machine = self.0.machine.lock();
        if Self::holds_calls(&machine.state) {
            return Err(Error::Busy);
        }
        let mut context = self.0.context.write();
        let before = context.items().to_vec();
        let Some(diff) = context.undo().then(|| Self::diff(&before, context.items())) else {
            return Ok(false);
        };
        let answer_gone =
            matches!(machine.state, State::Finished { item, .. } if diff.gone.contains(&item));

        self.emit(Event::ContextUndone {
            items: diff.items,
            removed: diff.gone,
            changed: diff.changed,
        });
        if answer_gone {
            self.transition(&mut machine, State::Idle);
        }

        Ok(true)
    }

    /// Whether the machine is part-way through a turn that has, or will have, calls of its own.
    fn holds_calls(state: &State) -> bool {
        matches!(
            state,
            State::Requesting
                | State::Deciding { .. }
                | State::Ready { .. }
                | State::Executing { .. }
        )
    }

    /// Puts back what the last [`Kernel::undo`] took away, returning whether there was any.
    ///
    /// note: A stack, not a toggle: undoing three operations and redoing them puts all three
    /// back, in order. Any new context operation makes what was undone unreachable, because a
    /// redo that reached across work done since would be overwriting it rather than restoring
    /// anything.
    ///
    /// note: [`Error::Busy`] while a turn holds calls, for the reason [`Kernel::undo`] gives.
    pub fn redo(&self) -> Result<bool> {
        let machine = self.0.machine.lock();
        if Self::holds_calls(&machine.state) {
            return Err(Error::Busy);
        }
        let mut context = self.0.context.write();
        let before = context.items().to_vec();
        let Some(diff) = context.redo().then(|| Self::diff(&before, context.items())) else {
            return Ok(false);
        };

        self.emit(Event::ContextRedone {
            items: diff.items,
            restored: diff.appeared,
            changed: diff.changed,
        });

        Ok(true)
    }

    /// Recounts every item's tokens with the active [`TokenCounter`].
    pub fn recount(&self) {
        let counter = self.counter();
        let mut context = self.0.context.write();
        let tokens_before = context.tokens();
        context.recount(&*counter);

        self.emit(Event::ContextRecounted {
            tokens_before,
            tokens_after: context.tokens(),
        });
    }

    /// Returns how much room the next request would take, and how much there is.
    pub fn budget(&self) -> Budget {
        let tool_tokens = self.tool_tokens();
        let limit = self.model_info().and_then(|i| i.context_limit);
        let reported = self.last_response().and_then(|response| response.usage);

        let context = self.projected().1;

        Budget {
            context_tokens: context.tokens,
            uncounted: context.uncounted,
            tool_tokens,
            limit,
            reported,
        }
    }

    /// Projects the current context into the messages of a request.
    pub fn project(&self) -> Projection {
        let projector = self.projector();

        projector.project(self.0.context.read().items())
    }

    /// Builds the request that the next [`Kernel::step`] would send - every message, every tool
    /// definition, every parameter.
    ///
    /// note: There is no step between this and the wire where the kernel adds something of its
    /// own.
    ///
    /// note: The one thing that can still change the request is a [`Compactor`], which runs at
    /// the start of the next [`Kernel::step`] - and says exactly what it did.
    pub fn preview_request(&self) -> Result<ModelRequest> {
        self.build_request(&*self.counter())
            .map(|(request, _, _)| request)
    }

    /// Renders the payload the provider would send for the next request, exactly as it would
    /// send it - or `None` when the provider cannot show one.
    ///
    /// note: This is as close to the wire as a kernel with no wire format can get.
    /// [`Kernel::preview_request`] is the kernel's own account, which it can guarantee; this is
    /// the provider's account of what it will make of it, which the kernel cannot. A provider
    /// that renders one payload here and sends another is lying in the same way a [`Tool`] that
    /// declares `Read` and opens a socket is lying, and the defence is the same: you chose the
    /// provider.
    ///
    /// note: The body only. Headers, URLs and credentials never pass through the kernel.
    pub fn preview_payload(&self) -> Result<Option<Value>> {
        let provider = self.provider().ok_or(Error::NoProvider)?;
        let (request, _, _) = self.build_request(&*self.counter())?;

        Ok(provider.render(&request))
    }

    /// Applies a compaction plan, returning (and broadcasting) a report of what it did.
    ///
    /// note: Pinned items in the plan are refused, and listed in [`CompactionReport::refused`].
    /// So is a removal of the call or the result a pinned item is paired with, because the pair
    /// goes out together or not at all.
    ///
    /// note: a pass that turns out to move nothing takes no checkpoint, like every other
    /// operation here. That is not a nicety about a hand-written plan: a [`Compactor`] is asked
    /// before every request, and one whose every candidate is pinned or already elided answers
    /// with a plan on every one of them - so a checkpoint spent here would cost an undo per
    /// request, and [`Kernel::redo`] would be unreachable for the rest of the session.
    pub fn apply_compaction(&self, plan: CompactionPlan) -> CompactionReport {
        let counter = self.counter();
        // both taken before the context lock, because that is the lock order and neither of these
        // may be reached for once it is held
        let projector = self.projector();
        let CompactionPlan {
            remove,
            elide,
            summary,
            reason,
        } = plan;

        let mut removed = Vec::new();
        let mut elided = Vec::new();
        let mut refused = Vec::new();
        let mut announcements = Vec::new();
        let mut added = None;

        // the whole pass happens under one lock. Taking it item by item would let a push land
        // between the checkpoint and the removals, and the next `undo` would then restore a
        // context that never existed - one without the item somebody had just added
        {
            let mut context = self.0.context.write();
            // the projected total rather than the sum of the items, which is what the report says
            // it is and what a pass is judged by. Summing the items credits an elision with the
            // whole of what it took away and charges nothing for the marker left in its place -
            // so a pass could report a decrease having made the request bigger. Projecting is a
            // walk over the items that moves `Content` by pointer, and it happens once a request
            // at most
            let before = projector.project(context.items());
            let tokens_before = projection_cost(&before, &*counter).tokens;
            // what a pass may take is what the request is carrying, and the projection is the
            // only thing that knows. An item the projector repaired away - a second result for a
            // call that already has one - is `Active`, holds everything it holds, and is
            // contributing nothing: taking it recovers nothing, reports the whole of it as
            // recovered, and moves something that was never being sent
            let carrying: HashSet<ContextId> = before.included.iter().copied().collect();
            // a call and its result go out together or not at all, so excluding one half of a
            // pinned pair takes the pinned half out of the request with it - reaching for the pin
            // through the item beside it. Elision is not in this, because an elided item keeps
            // its place in the pair
            let pinned_calls: HashSet<&ToolCallId> = context
                .items()
                .iter()
                .filter(|item| item.state == ContextState::Pinned && carrying.contains(&item.id))
                .flat_map(|item| paired(item))
                .collect();

            // what the plan comes to is worked out before anything moves, because the checkpoint
            // has to be taken before the first change and must not be taken at all if there is
            // not going to be one. An id named twice, or in both lists, is moved once and reported
            // once: removal wins, since it is the larger of the two
            let mut planned = HashSet::new();
            let mut removing = Vec::new();
            for id in remove {
                let Some(item) = context.item(id) else {
                    continue;
                };
                let entry = Removed {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                };
                if item.state == ContextState::Pinned
                    || paired(item).any(|call| pinned_calls.contains(call))
                {
                    refused.push(entry);
                } else if carrying.contains(&id) && planned.insert(id) {
                    removing.push(entry);
                }
            }

            let mut eliding = Vec::new();
            for id in elide {
                let Some(item) = context.item(id) else {
                    continue;
                };
                let entry = Removed {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                };

                if item.state == ContextState::Pinned {
                    refused.push(entry);
                    continue;
                }
                if !carrying.contains(&id) || item.state.is_elided() || !planned.insert(id) {
                    continue;
                }
                eliding.push(entry);
            }

            // a summary stands in the place of what was taken, so a pass that took nothing has no
            // place to put one. Added anyway, it compounds: the pass is asked again before the
            // next request, the context is no smaller than it was, so it says yes again - and a
            // compactor whose every candidate is pinned then adds a summary saying results were
            // elided, spends an undo, and grows the request it exists to shrink, on every request
            // for as long as the session lasts
            let moved = !removing.is_empty() || !eliding.is_empty();
            if moved {
                context.checkpoint();
            }

            for entry in removing {
                let note = Some(format!("compaction: {reason}"));
                if let Some(from) =
                    context.set_state(entry.id, ContextState::Excluded, note.clone())
                {
                    announcements.push(Event::ContextChanged {
                        id: entry.id,
                        from,
                        to: ContextState::Excluded,
                        note,
                    });
                    removed.push(entry);
                }
            }

            // note: the note is set from the pass's reason rather than kept, as the removals
            // above do - a note says why an item is in the state it is in, and the one it may
            // already be carrying explains the state it is leaving, which would be a strange
            // thing to hand the model as the reason it cannot see this any more
            for entry in eliding {
                // the reason as it was written, with no `compaction:` in front of it: unlike a
                // removal's note this one is read by the model, in the brackets the projector
                // puts round it, and a client showing it has already said `elided` in the row
                let note = Some(reason.clone());
                if let Some(from) = context.set_state(entry.id, ContextState::Elided, note.clone())
                {
                    announcements.push(Event::ContextChanged {
                        id: entry.id,
                        from,
                        to: ContextState::Elided,
                        note,
                    });
                    elided.push(entry);
                }
            }

            if let Some(item) = summary.filter(|_| moved) {
                let id = context.add(item, &*counter);
                let item = context.item(id).expect("the item was just added");
                announcements.push(addition(item));
                added = Some(Removed {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                });
            }

            let report = CompactionReport {
                removed,
                elided,
                refused,
                summary: added,
                reason,
                tokens_before,
                // a pass that moved nothing left the context as it found it, under this same
                // lock - and it is the pass a compactor with nothing left to take answers with
                // before every request
                tokens_after: match moved {
                    true => projection_cost(&projector.project(context.items()), &*counter).tokens,
                    false => tokens_before,
                },
            };

            // still under the lock, and the pass's own events before the report of it: see the
            // note on `Kernel::emit`
            for announcement in announcements {
                self.emit(announcement);
            }
            self.emit(Event::Compacted {
                report: report.clone(),
            });

            report
        }
    }

    // ------------------------------------------------------------------------------- the loop

    /// Returns what the runtime is doing.
    pub fn state(&self) -> State {
        self.0.machine.lock().state.clone()
    }

    /// Returns the tool calls that are waiting for a decision.
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
        let (call, tool) = (prepared.call.id.clone(), prepared.call.tool.clone());

        self.emit(Event::PermissionDecided {
            id,
            call,
            tool,
            grant,
            source: GrantSource::User,
        });

        let to = Self::state_for(&machine.pending);
        self.transition(&mut machine, to.clone());

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
    /// running them, and it has the same shape. The refusals are announced and the machine
    /// claimed as [`State::Executing`] without letting go of the lock, and the results are
    /// recorded before it returns to [`State::Idle`]. Going straight to `Idle` would let a step in
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

            for call in &prepared {
                self.emit(Event::PermissionDecided {
                    id: call.request.id,
                    call: call.call.id.clone(),
                    tool: call.call.tool.clone(),
                    grant: Grant::Deny,
                    source: GrantSource::Cancellation,
                });
            }
            let calls = prepared.iter().map(|p| p.call.id.clone()).collect();
            self.transition(&mut machine, State::Executing { calls });

            prepared
        };
        let mut restore = Restore::new(self, State::Idle);

        let mut batch = Batch::default();
        for call in &prepared {
            self.record_tool_result(
                &call.call,
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
    /// | [`State::Ready`] | the tools run, in order, and their results are recorded | `Idle` |
    /// | [`State::Deciding`] | nothing; the answer has to come from you | `Deciding` |
    /// | [`State::Requesting`], [`State::Executing`] | nothing; [`Error::Busy`] | - |
    /// | any resting state, with an interrupt outstanding | nothing; the interrupt is spent | the same state |
    ///
    /// note: One step is one transition, so the model asking for a tool and that tool running
    /// are two of them. That is deliberate: [`State::Ready`] is a resting state at which the
    /// model has said what it wants and nothing has happened yet.
    pub async fn step(&self) -> Result<State> {
        self.step_once().await.map(|(state, _)| state)
    }

    /// One transition, and whether it was spent acknowledging an interrupt rather than making
    /// one.
    ///
    /// note: [`Kernel::turn`] needs to be told, and cannot work it out. A step that spends an
    /// interrupt returns the state it found, which to `turn` looks like an ordinary resting state
    /// to go round again from - and the next request would go out with the stop already on the
    /// event log.
    async fn step_once(&self) -> Result<(State, bool)> {
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
                return Ok((state, true));
            }

            match state {
                State::Deciding { calls } => return Ok((State::Deciding { calls }, false)),
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
                    Claim::Request
                }
                State::Requesting | State::Executing { .. } => unreachable!("handled as busy"),
            }
        };

        match claim {
            Claim::Request => self.request().await.map(|state| (state, false)),
            Claim::Execute(prepared) => self.execute(prepared).await.map(|state| (state, false)),
        }
    }

    /// Repeats [`Kernel::step`] until the model ends its turn, a decision is needed, or
    /// [`Config::max_requests_per_turn`] is reached; returns the state it stopped in.
    ///
    /// note: [`State::Finished`] means the model answered, [`State::Deciding`] means it is your
    /// move, and [`State::Idle`] means the request budget ran out mid-loop - calling `turn`
    /// again picks up exactly where it left off. An interrupt stops it too, in whatever resting
    /// state the loop had reached: [`State::Ready`] or [`State::Idle`] as often as not.
    pub async fn turn(&self) -> Result<State> {
        let mut requests = 0;

        loop {
            // note: this loop does not read the interrupt flag itself. If it did, and the step it
            // then called read it again, an interrupt landing between the two would be spent on a
            // step that transitions nothing, and this loop, seeing an ordinary resting state come
            // back, would go round and send the next request anyway. With one reader, checked in
            // one place, there is no window at all

            // the next step will send a request, so it counts against the budget
            if matches!(self.state(), State::Idle | State::Finished { .. }) {
                if self
                    .0
                    .config
                    .max_requests_per_turn
                    .is_some_and(|max| requests >= max)
                {
                    return Ok(self.state());
                }
                requests += 1;
            }

            // an interrupt the step acknowledged is one this loop must not step past: it was
            // asked to stop, and the step it just spent doing so transitioned nothing
            let (state, acknowledged) = self.step_once().await?;
            if acknowledged {
                return Ok(state);
            }

            match state {
                State::Finished { .. } | State::Deciding { .. } => return Ok(state),
                _ => continue,
            }
        }
    }

    // -------------------------------------------------------------------------------- internals

    /// Returns the state a set of prepared calls implies.
    fn state_for(prepared: &[PreparedCall]) -> State {
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
    fn transition(&self, machine: &mut Machine, to: State) {
        if machine.state == to {
            return;
        }

        if matches!(to, State::Finished { .. }) {
            self.0.interrupted.store(false, SeqCst);
        }

        let from = std::mem::replace(&mut machine.state, to.clone());
        self.emit(Event::StateChanged { from, to });
    }

    /// Adds an item to the context, optionally checkpointing it for [`Kernel::undo`] first.
    fn add_item(&self, item: ContextItem, checkpoint: bool) -> ContextId {
        let counter = self.counter();
        let mut context = self.0.context.write();
        if checkpoint {
            context.checkpoint();
        }

        self.added(&mut context, item, &*counter)
    }

    /// Adds an item as part of a batch that is one operation for [`Kernel::undo`]: the first of
    /// them takes the checkpoint, and every later one folds whatever was checkpointed since into it.
    fn add_in(&self, item: ContextItem, batch: &mut Batch) -> ContextId {
        let counter = self.counter();
        let mut context = self.0.context.write();
        match batch.0 {
            None => {
                context.checkpoint();
                batch.0 = Some(context.taken());
            }
            Some(taken) => context.fold_since(taken),
        }

        self.added(&mut context, item, &*counter)
    }

    /// Adds an item to a context the caller holds the lock on, and announces it under that lock.
    fn added(
        &self,
        context: &mut Context,
        item: ContextItem,
        counter: &dyn TokenCounter,
    ) -> ContextId {
        let id = context.add(item, counter);
        let item = context.item(id).expect("the item was just added");
        self.emit(addition(item));

        id
    }

    /// Returns what swapping one snapshot of the items for another did.
    ///
    /// note: An item that survived but is no longer the same allocation is one the operation
    /// touched, which `Arc::make_mut` makes exact and free to check. Both lists are in
    /// identifier order, so the lookups are binary searches rather than a scan apiece.
    fn diff(before: &[Arc<ContextItem>], after: &[Arc<ContextItem>]) -> Diff {
        let find = |items: &[Arc<ContextItem>], id: ContextId| {
            items
                .binary_search_by_key(&id, |item| item.id)
                .ok()
                .map(|index| items[index].clone())
        };

        let mut gone = Vec::new();
        let mut changed = Vec::new();
        for item in before {
            match find(after, item.id) {
                Some(kept) if !Arc::ptr_eq(&kept, item) => changed.push(item.id),
                Some(_) => {}
                None => gone.push(item.id),
            }
        }
        let appeared = after
            .iter()
            .filter(|item| find(before, item.id).is_none())
            .map(|item| item.id)
            .collect();

        Diff {
            items: after.len(),
            gone,
            appeared,
            changed,
        }
    }
}

/// The event announcing an item that has just been added.
///
/// note: one function for both places an item comes in - an ordinary push and a compaction's
/// summary - so that what the record says about a new item cannot differ by which one added it.
fn addition(item: &ContextItem) -> Event {
    Event::ContextAdded {
        id: item.id,
        kind: item.kind.name().to_owned(),
        source: item.source.clone(),
        label: item.label.clone(),
        tokens: item.tokens,
        because: item.included_because.clone(),
        meta: item.meta.clone(),
    }
}

/// The calls an item is one half of a pair with: the ones an assistant turn asked for, or the one
/// a tool result answers.
fn paired(item: &ContextItem) -> impl Iterator<Item = &ToolCallId> {
    let answers = match &item.kind {
        ContextKind::ToolResult { call, .. } => Some(call),
        _ => None,
    };

    item.calls().map(|call| &call.id).chain(answers)
}
