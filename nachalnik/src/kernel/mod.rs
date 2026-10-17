//! The runtime: the state machine, the context it owns, and every operation a caller has on them.
//!
//! note: construction, resuming and observation, with the types the rest share. The state machine
//! and the operations that move it are in `machine`. What a turn is made of is in `request` and
//! `calls`, because a reader of this file wants to know what the kernel offers rather than how a
//! stream of fragments is read. What a caller can do to the context is in `context`, public still,
//! because the surface is too long to read at once with it.

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
use tokio::sync::broadcast;

#[cfg(doc)]
use crate::error::Error;
use crate::{
    compaction::Compactor,
    config::Config,
    context::{Context, ContextId, ContextItem, ContextState},
    event::Event,
    model::{ModelInfo, ModelResponse, Params, Provider, StopReason, ToolCallId},
    permissions::{AskAlways, PermissionId, PermissionPolicy},
    projection::{LinearProjector, Projector},
    session::{Record, Session, Snapshot},
    tokens::{BytesPerToken, Calibrating, TokenCounter},
    tool::Tool,
};

mod calls;
mod components;
mod context;
mod machine;
mod request;

use machine::{Machine, PreparedCall, Restore};

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
    /// note: this is a resting state: the model has said what it wants to do and nothing has
    /// happened yet, so a user may want to look (see [`Kernel::pending_calls`]).
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
        /// note: "finished" and "finished well" are different things, and a client that only
        /// matched on the variant could not tell them apart. A turn that ran out of output tokens
        /// ends in `Finished` with [`StopReason::Length`], possibly with no content at all, because
        /// a reasoning model can spend its whole budget before saying anything. Whether to
        /// continue, warn or shrug is the client's call; the kernel does not let it pass unnoticed.
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
/// note: `None` until the first item is added, which takes the checkpoint the rest belong to.
/// Joining a checkpoint already taken hands you the same batch, already holding its number; the
/// answer to a call nobody can run joins the turn that asked for it.
#[derive(Default)]
struct Batch(Option<u64>);

/// The state a kernel holds; see [`Kernel`].
///
/// note: a reader that needs several of these to agree holds them together, in this order:
/// `machine`, then `tools`, `projector`, `params` and `counter`, then `context`, then `session`.
/// Nothing takes one while holding one after it, which keeps two readers and a writer from waiting
/// on each other. Each setter holds only its own lock and then `session`, to announce; a reader
/// holds a component's lock across the sequence it reads, which makes the two agree.
///
/// note: a figure the context keeps is counted with `counter` held across `context`, not with a
/// clone taken first. [`Kernel::set_counter`] recounts after it swaps; an item measured with a
/// clone of a counter swapped out in between would keep that counter's figure permanently.
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
    /// Whether somebody has asked [`Kernel::turn`] to stop at the next opportunity; written only
    /// under the machine lock, and read without it.
    interrupted: AtomicBool,
    /// Whether the compactor last wanted room it could not make; see [`Event::ContextFull`].
    full: AtomicBool,
    /// What is put into the context as it becomes full; see [`Kernel::set_full_notice`].
    full_notice: RwLock<Option<ContextItem>>,
    /// How many of one answer's tool calls are run; see [`Kernel::set_call_ceiling`].
    call_ceiling: RwLock<Option<usize>>,
    /// How many requests a turn may send; see [`Kernel::set_max_requests_per_turn`].
    max_requests: RwLock<Option<usize>>,
    /// How large a picture in a tool's output is kept whole; see [`Kernel::set_picture_limit`].
    picture_limit: RwLock<Option<usize>>,
    /// What a tool that sets no limit of its own is cut at; see
    /// [`Kernel::set_default_tool_output_limit`].
    output_limit: RwLock<Option<usize>>,
}

/// The agent runtime: a state machine, a context, and nothing else.
///
/// A kernel is a cheaply clonable handle to shared state, so clients, tools and policies can hold
/// one.
///
/// note: a kernel does nothing on its own. [`Kernel::step`] and [`Kernel::turn`] run on the
/// caller's task, for as long as the caller lets them; between steps, nothing runs. It spawns no
/// tasks, with one opted-in exception: [`Config::parallel_tool_calls`] gives each of a turn's tool
/// calls a task of its own, for the length of the step that started them and no longer.
///
/// note: one agent is one kernel. A fleet of them shares nothing except whatever you hand to both -
/// a [`Tool`], a [`Provider`] - and needs no coordination, because a kernel's state is its own.
/// Several threads driving one kernel is also fine: reading is cheap and every mutation is atomic,
/// so a client can render, exclude and preview while a turn is running. It cannot drive the loop
/// twice at once; that is [`Error::Busy`], not a second request.
///
/// note: a kernel is not `Drop`-safe against reference cycles: if you store a `Kernel` inside a
/// [`Tool`], a [`Provider`] or a [`PermissionPolicy`] that the same kernel holds, the cycle keeps
/// everything alive. Either store a [`std::sync::Weak`] to your own state, or drop the components
/// ([`Kernel::remove_tool`], [`Kernel::clear_provider`]) when you are done.
#[derive(Clone)]
pub struct Kernel(Arc<InnerKernel>);

impl fmt::Debug for Kernel {
    /// note: a summary, not a dump. Everything here has an accessor that returns the real thing;
    /// this is for a log line that says which kernel is being talked about.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // gathered before the context is read, so that `{kernel:?}` cannot order two locks the
        // wrong way round
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
    /// permission policy, a [`LinearProjector`], a [`Calibrating<BytesPerToken>`] token counter, no
    /// compactor, no parameters, and an empty context: it will not talk to anything or agree to
    /// anything until told.
    ///
    /// note: the counter is wrapped rather than bare because [`Calibrating`] costs nothing until it
    /// is told something: it corrects by `1.0` until a provider has reported what a request
    /// actually cost, so it is [`BytesPerToken`] until there is something better. Unwrap it with
    /// [`Kernel::set_counter`] where a measurement needs a counter that never changes its mind.
    ///
    /// [`Calibrating<BytesPerToken>`]: Calibrating
    pub fn new(config: Config) -> Self {
        Self::with_items(config, [])
    }

    /// Creates a kernel as [`Kernel::new`] does, with `items` already in its context, in order.
    ///
    /// note: for what a client sets a session up with - a system instruction, files named on a
    /// command line. Pushed after `new`, each is an operation like any other, so the first
    /// [`Kernel::undo`] in a session nobody has said anything in yet takes the setup away. Placed
    /// here, they are where the undo history starts, as the items of a resumed session are. Each
    /// is still announced as it would be pushed, after [`Event::SessionStarted`].
    pub fn with_items(config: Config, items: impl IntoIterator<Item = ContextItem>) -> Self {
        let kernel = Self::build(config);
        kernel.emit(Event::SessionStarted {
            session: kernel.session_name(),
        });
        {
            let held = kernel.0.counter.read();
            let mut context = kernel.0.context.write();
            for item in items {
                kernel.added(&mut context, item, &**held);
            }
        }

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
    ///
    /// note: Parameters come back with a [`Event::ModelParamsChanged`] of their own rather than
    /// folded into [`Event::SessionResumed`], because a log is read for what is *in force* and a
    /// resumed session that says so is one whose next request can be explained from the log alone.
    /// It follows the resumption, which heads the log and is what tells a client this one is not
    /// whole, and a snapshot carrying none says nothing, since nothing is in force.
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
        // under the lock that sets them, held until the announcement below: the params are a
        // restored piece of state, and `model.params` is how a log says what is in force
        let mut params = kernel.0.params.write();
        *params = snapshot.params;
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
        // after `session.resumed`, which is the head of a resumed log and is what kamchatka's
        // `log` tool reads to tell a resumed log from a drained one. Only when there is something
        // to say: a resume carrying none has changed nothing, and `model.params` over an empty map
        // is a change nobody made.
        if !params.is_empty() {
            kernel.emit(Event::ModelParamsChanged {
                params: params.clone(),
            });
        }
        drop(params);

        kernel
    }

    /// Tells the kernel that these tool call identifiers are already spoken for, and returns how
    /// many of them it had not already been told about.
    ///
    /// note: [`Kernel::resume`] does this from [`Snapshot::used_calls`], which covers a session
    /// picked back up in another process. This is for the other way of reading a snapshot: a client
    /// that merges one into a session it is already running keeps the kernel it has, so the turns
    /// it pushes arrive carrying identifiers this kernel never issued. Without this the next
    /// response is free to hand one of them back, the repair has nothing to compare it against, and
    /// the request that follows carries the same `tool_call_id` twice, which most providers reject
    /// and which is very hard to see afterwards. See [`Event::ToolCallRepaired`].
    ///
    /// note: a snapshot of this same session names identifiers this kernel did issue, and reserving
    /// them changes nothing: the originals are still in the context. A client merging one renames
    /// the colliding calls, and the results answering them, before pushing.
    ///
    /// note: identifiers and nothing else. What they belonged to is not the kernel's business, and
    /// one reserved for an item that is later excluded stays reserved, because an identifier is
    /// never reused, not even by an item [`Kernel::undo`] took away.
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
    /// note: cheap enough to take after every turn, and worth it: this is the only thing that can
    /// rebuild a context. The event log cannot, by design: an event names an item rather than
    /// carrying its contents, which is what makes the log affordable to keep.
    ///
    /// note: the context is read before the used call identifiers, not after. A turn reserves a
    /// call's identifier before it records the item carrying the call, and an identifier is never
    /// given back, so identifiers read afterwards cover every call in the items read before. Read
    /// the other way round, a turn recorded between the two would leave a call in `items` whose
    /// identifier `used_calls` does not have, for a resumed session to hand out again.
    pub fn snapshot(&self) -> Snapshot {
        let session = self.session_name();

        // `last_seq` under the context lock, because every change to the context is announced
        // while holding it: every record numbered up to it describes a change these items already
        // show, and every one after it a change they do not. That is what lets a caller write the
        // log out to exactly this point and have the pair agree.
        //
        // note: and under the parameters' and the counter's, for the same reason: each setter
        // announces while holding its own, and a recalibration applies and announces under the
        // context's. Read before the context lock and let go, a change landing in between was
        // named by a sequence this snapshot did not reflect. The order is the one on
        // `InnerKernel`
        let (params, calibration, items, next_item, last_seq) = {
            let params = self.0.params.read();
            let counter = self.0.counter.read();
            let context = self.0.context.read();
            let items: Vec<ContextItem> = context.items().iter().map(|i| (**i).clone()).collect();
            (
                params.clone(),
                counter.calibration(),
                items,
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
            format: crate::session::FORMAT,
            session,
            items,
            params,
            next_item,
            used_calls,
            last_seq,
            next_permission: self.0.next_permission.load(SeqCst),
            calibration,
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
            machine: Mutex::new(Machine::new()),
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
            full: AtomicBool::new(false),
            full_notice: RwLock::new(None),
            call_ceiling: RwLock::new(None),
            max_requests: RwLock::new(config.max_requests_per_turn),
            picture_limit: RwLock::new(None),
            output_limit: RwLock::new(config.default_tool_output_limit),
            config,
        };

        Self(Arc::new(inner))
    }

    // ---------------------------------------------------------------- session and observation

    /// Returns the kernel's configuration as it was built.
    ///
    /// note: [`Config::max_requests_per_turn`] and [`Config::default_tool_output_limit`] are where
    /// the kernel started; [`Kernel::max_requests_per_turn`] and
    /// [`Kernel::default_tool_output_limit`] are what it holds to now.
    pub fn config(&self) -> &Config {
        &self.0.config
    }

    /// Returns the session's name.
    pub fn session_name(&self) -> String {
        self.0.session.lock().name().to_owned()
    }

    /// Subscribes to the event stream.
    ///
    /// note: a subscriber that cannot keep up with [`Config::event_queue_depth`] starts missing
    /// events (`tokio`'s broadcast semantics). The event stream is the live view; the session log,
    /// via [`Kernel::history`], is the complete one.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }

    /// Returns the session's records, oldest first.
    ///
    /// note: this returns a copy of the whole log, which is not free on a long session and bigger
    /// still with [`Config::record_payloads`] on. Use [`Kernel::history_since`] when a client is
    /// following along, and [`Kernel::with_history`] when the answer is a count or a search rather
    /// than the records themselves.
    pub fn history(&self) -> Vec<Record> {
        self.0.session.lock().records().cloned().collect()
    }

    /// Runs a closure against the session log, copying nothing.
    ///
    /// note: the mirror of [`Kernel::with_context`], with a sharper version of its warning: the log
    /// is locked for the duration, so the closure must not call back into the kernel. Every emit on
    /// this kernel waits on that lock.
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
    /// note: the log is unbounded; capping it would stop it being append-only. This is the other
    /// way to stop a long session growing forever: take the records, write them somewhere, and the
    /// kernel stops holding them. No record goes missing; every record the kernel drops is handed
    /// to the caller who asked.
    pub fn drain_history(&self, through: u64) -> Vec<Record> {
        self.0.session.lock().drain_through(through)
    }

    /// Returns the most recent response in full, including the token counts the provider reported
    /// and its raw payload.
    ///
    /// note: the tool call identifiers here are the ones the kernel settled on (see
    /// [`Event::ToolCallRepaired`]); [`ModelResponse::raw`] still holds whatever the provider sent.
    pub fn last_response(&self) -> Option<Arc<ModelResponse>> {
        self.0.last_response.read().clone()
    }

    /// Broadcasts [`Event::SessionFinished`], after [`Event::TurnUnfinished`] if the turn had calls
    /// waiting to run.
    ///
    /// note: this is a marker for whoever reads the log, not a shutdown. The kernel owns no tasks
    /// and remains usable afterwards; the waiting calls are still waiting.
    pub fn finish(&self) {
        let machine = self.0.machine.lock();
        let waiting: Vec<ToolCallId> = machine.pending.iter().map(|p| p.call.id.clone()).collect();
        if !waiting.is_empty() {
            self.emit(Event::TurnUnfinished { calls: waiting });
        }
        self.emit(Event::SessionFinished);
    }

    /// Records an event in the session log and broadcasts it.
    ///
    /// note: the log is written and the event broadcast under the same lock, so subscribers see
    /// events in the order the log holds them, even when several threads drive the same kernel.
    ///
    /// note: callers emit while still holding the lock that made the change: the machine lock for a
    /// transition, the context lock for everything the context does. Releasing first and announcing
    /// afterwards is wrong: two threads excluding the same item can then apply in one order and be
    /// logged in the other, and the log's last word on that item contradicts the item. The lock
    /// order is machine, then context, then session, and nothing goes back up it. `emit` takes the
    /// session lock and nothing else, and `send` on a broadcast channel runs no subscriber code, so
    /// holding a lock across it costs a memcpy and blocks nobody.
    ///
    /// note: the component setters hold their own lock the same way, which stops two clients
    /// swapping the same component from applying in one order and being logged in the other, with
    /// the log's last word on the provider naming the one that is not installed. A component's
    /// [`Provider::info`] and the `name` of a policy, projector, counter or compactor must not
    /// reach back into the kernel: each is called while the lock holding that component is held, to
    /// say what was replaced.
    pub(crate) fn emit(&self, event: Event) {
        let mut session = self.0.session.lock();

        let is_progress = matches!(event, Event::ModelDelta { .. } | Event::ToolOutput { .. });
        if !is_progress || self.0.config.record_progress {
            session.append(event.clone());
        }
        // an error only means nobody is listening; `send` does not block
        let _ = self.0.events.send(event);
    }

    // -------------------------------------------------------------------------------- internals

    /// Adds an item as part of a batch that is one operation for [`Kernel::undo`]: the first takes
    /// the checkpoint, and every later one folds whatever was checkpointed since into it.
    fn add_in(&self, item: ContextItem, batch: &mut Batch) -> ContextId {
        let held = self.0.counter.read();
        let counter = &**held;
        let mut context = self.0.context.write();
        match batch.0 {
            None => {
                context.checkpoint();
                batch.0 = Some(context.taken());
            }
            Some(taken) => context.fold_since(taken),
        }

        self.added(&mut context, item, counter)
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
        if let Some(change) = arriving(item) {
            self.emit(change);
        }

        id
    }

    /// Returns what swapping one snapshot of the items for another did.
    ///
    /// note: An item that survived as the same allocation is one the operation did not touch,
    /// which `Arc::make_mut` makes free to check. One that is a different allocation is looked
    /// at, because a recount copies an item it moves a figure of, and a figure is the counter's
    /// rather than a change of state, note or content that an undo reverts. Both lists are in
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
                Some(kept) if !Arc::ptr_eq(&kept, item) && !alike(&kept, item) => {
                    changed.push(item.id);
                }
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

/// Whether two versions of an item differ in nothing but the counter's two figures.
///
/// note: every field is named, so a field added to [`ContextItem`] must be placed on one side of
/// that line or the other before this compiles.
fn alike(a: &ContextItem, b: &ContextItem) -> bool {
    let ContextItem {
        id,
        kind,
        source,
        label,
        content,
        tokens: _,
        uncounted: _,
        state,
        meta,
        included_because,
        note,
    } = a;

    *id == b.id
        && *kind == b.kind
        && *source == b.source
        && *label == b.label
        && *content == b.content
        && *state == b.state
        && *meta == b.meta
        && *included_because == b.included_because
        && *note == b.note
}

/// The event announcing an item that has just been added.
///
/// note: one function serves both an ordinary push and a compaction's summary, so the record about
/// a new item cannot differ by which one added it.
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

/// The event for an item added in a state other than [`ContextState::Active`].
///
/// note: `context.added` reports what an item is, not its state, because every state change is its
/// own event. An item that arrives pinned or elided, because a loaded session left it that way,
/// would otherwise reach the log as active when it is not. The second event, from `Active` to the
/// item's actual state, is what a later `set_state` would emit and what a replayed log needs to
/// rebuild the context. Returns `None` for an item that arrives active, whose state the addition
/// already records.
///
/// note: a field on `context.added` would report the state in one record. `Event` is
/// `#[non_exhaustive]`, which does not cover its variants, and a field on one variant breaks every
/// pattern and struct literal written against it.
fn arriving(item: &ContextItem) -> Option<Event> {
    (item.state != ContextState::Active).then(|| Event::ContextChanged {
        id: item.id,
        from: ContextState::Active,
        to: item.state,
        note: item.note.clone(),
    })
}
