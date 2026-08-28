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
    context::{Context, ContextId, ContextItem, ContextState},
    error::{Error, Result},
    event::{DeltaSink, Event, OutputSink},
    model::{
        Content, ModelInfo, ModelRequest, ModelResponse, Params, Provider, StopReason, ToolCall,
        ToolCallId,
    },
    permissions::{
        AskAlways, Grant, GrantSource, PermissionId, PermissionPolicy, PermissionRequest, Verdict,
    },
    projection::{LinearProjector, Projection, Projector},
    session::{Record, Session, Snapshot},
    tokens::{BytesPerToken, Calibrating, TokenCounter},
    tool::{Tool, ToolOutput, ToolSpec},
};

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

/// A tool call that has been matched to a tool and is waiting for a decision, or for its turn.
struct PreparedCall {
    call: ToolCall,
    tool: Arc<dyn Tool>,
    spec: ToolSpec,
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
    /// actually cost, so it *is* [`BytesPerToken`] right up to the moment there is something
    /// better to be. Leaving it off by default meant the low estimate was what everybody who did
    /// not read the documentation got. Unwrap it with [`Kernel::set_counter`] if a counter that
    /// never changes its mind is what a measurement needs.
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
    /// The context, the parameters and the identifiers the session had already handed out come
    /// back; everything else is as [`Kernel::new`] leaves it, because a provider, a policy and a
    /// set of tools are the caller's to supply and were never the session's to remember.
    ///
    /// note: The name comes from the snapshot unless [`Config::session_name`] is set, which is
    /// how a session gets forked rather than continued.
    ///
    /// note: The items are recounted with this kernel's [`TokenCounter`], so a snapshot taken
    /// under a different one reports honest numbers rather than inherited ones.
    ///
    /// note: What the previous counter had *learned* does come back, when both counters deal in
    /// [`Calibration`](crate::Calibration)s, and it is offered *before* the items are counted. So
    /// a resumed session does not spend its first few requests relearning what it had already been
    /// told - and, since resuming recounts, the figures it comes back with are the corrected ones
    /// rather than the stale ones it was saved with. That is a visible change in the numbers, and
    /// the right one: it is what [`Kernel::recount`] before saving would have produced, and it is
    /// nearer to what the provider had been charging. A counter that learns nothing ignores all of
    /// this.
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
            kernel
                .0
                .context
                .write()
                .restore(snapshot.items, snapshot.next_item, &*counter);
        }
        *kernel.0.params.write() = snapshot.params;
        kernel.0.seen_calls.lock().extend(snapshot.used_calls);

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

    /// Returns everything a later [`Kernel::resume`] needs to carry on from here.
    ///
    /// note: Cheap enough to take after every turn, and worth it: this is the only thing that
    /// can rebuild a context. The event log cannot, by design - an event names an item rather
    /// than carrying its contents, which is what makes the log affordable to keep.
    pub fn snapshot(&self) -> Snapshot {
        let session = self.session_name();
        let params = self.params();
        let counter = self.counter();
        let mut used_calls: Vec<_> = self.0.seen_calls.lock().iter().cloned().collect();
        used_calls.sort();

        let context = self.0.context.read();

        Snapshot {
            session,
            items: context.items().iter().map(|i| (**i).clone()).collect(),
            params,
            next_item: context.next_id(),
            used_calls,
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
            tools: RwLock::new(BTreeMap::new()),
            policy: RwLock::new(Arc::new(AskAlways)),
            projector: RwLock::new(Arc::new(LinearProjector::default())),
            // note: wrapped, not bare. `BytesPerToken` is an admitted estimate and a low one;
            // `Calibrating` corrects nothing until a provider has said what a request cost, so
            // this is the same counter until the moment there is something better to be, and
            // then it is better. A user who never reads the documentation gets the honest number
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
    pub fn history(&self) -> Vec<Record> {
        self.0.session.lock().records().cloned().collect()
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
    /// and the kernel stops holding on to them. Nothing goes missing behind anybody's back - you
    /// asked for them, and now you have them.
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
    /// Nothing is lost, which is the difference between stopping and pulling the plug.
    ///
    /// note: It stops the loop in three places, in increasing order of how much has to
    /// cooperate. Before a transition, [`Kernel::step`] and [`Kernel::turn`] spend one attempt
    /// acknowledging it and do nothing else. During a request, a [`Provider`] that checks
    /// [`DeltaSink::is_interrupted`] can stop reading and hand back what it has. During a tool
    /// call, a [`Tool`] that checks [`OutputSink::is_interrupted`] can do the same - and in the
    /// serial case the kernel does not start the calls that had not begun.
    ///
    /// note: The flag is cleared by whichever transition attempt acts on it, so it can never
    /// outlive the thing it was meant to stop. What it never does is discard work: a partial
    /// answer and a half-finished tool result are recorded like any other, because the whole
    /// point of a context you can see is that you get to decide what to do with them.
    pub fn interrupt(&self) -> bool {
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
        let previous = self.0.provider.write().replace(provider);
        self.emit(Event::ModelChanged {
            from: previous.as_ref().map(|p| p.info()),
            to: Some(to),
        });

        previous
    }

    /// Removes the provider, returning it.
    pub fn clear_provider(&self) -> Option<Arc<dyn Provider>> {
        let previous = self.0.provider.write().take();
        self.emit(Event::ModelChanged {
            from: previous.as_ref().map(|p| p.info()),
            to: None,
        });

        previous
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
        let previous = self.0.tools.write().insert(id, tool);
        self.emit(Event::ToolsChanged {
            tools: self.tool_ids(),
        });

        previous
    }

    /// Unregisters a tool, returning it.
    pub fn remove_tool(&self, id: &str) -> Option<Arc<dyn Tool>> {
        let previous = self.0.tools.write().remove(id);
        if previous.is_some() {
            self.emit(Event::ToolsChanged {
                tools: self.tool_ids(),
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
        let previous = std::mem::replace(&mut *self.0.policy.write(), policy);
        self.emit(Event::PolicyChanged);

        previous
    }

    /// Returns the permission policy.
    pub fn policy(&self) -> Arc<dyn PermissionPolicy> {
        self.0.policy.read().clone()
    }

    /// Sets the projector, returning the previous one.
    pub fn set_projector(&self, projector: Arc<dyn Projector>) -> Arc<dyn Projector> {
        std::mem::replace(&mut *self.0.projector.write(), projector)
    }

    /// Returns the projector.
    pub fn projector(&self) -> Arc<dyn Projector> {
        self.0.projector.read().clone()
    }

    /// Sets the token counter and recounts the context, returning the previous counter.
    pub fn set_counter(&self, counter: Arc<dyn TokenCounter>) -> Arc<dyn TokenCounter> {
        let previous = std::mem::replace(&mut *self.0.counter.write(), counter);
        self.recount();

        previous
    }

    /// Returns the token counter.
    pub fn counter(&self) -> Arc<dyn TokenCounter> {
        self.0.counter.read().clone()
    }

    /// Sets (or, with `None`, removes) the compactor, returning the previous one.
    ///
    /// note: With no compactor set, the context only ever changes because somebody asked it to.
    pub fn set_compactor(
        &self,
        compactor: Option<Arc<dyn Compactor>>,
    ) -> Option<Arc<dyn Compactor>> {
        std::mem::replace(&mut *self.0.compactor.write(), compactor)
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
    pub fn set_params(&self, params: Params) -> Params {
        let previous = std::mem::replace(&mut *self.0.params.write(), params.clone());
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
    /// note: Ten separate [`Kernel::push`]es are ten checkpoints, which at the default depth
    /// means opening a project wipes the undo history before the user has done anything. Putting
    /// a set of files in the context is one thing the user did, so it is one thing to undo.
    pub fn push_all(&self, items: impl IntoIterator<Item = ContextItem>) -> Vec<ContextId> {
        let mut items = items.into_iter();
        let Some(first) = items.next() else {
            return Vec::new();
        };

        let mut ids = vec![self.add_item(first, true)];
        ids.extend(items.map(|item| self.add_item(item, false)));

        ids
    }

    /// Adds an item in place of an existing one, marking the old one
    /// [`ContextState::Superseded`], as one undoable operation.
    ///
    /// note: This is the only thing that ever sets that state, and it is explicit because the
    /// kernel cannot tell whether a second read of a file replaces the first or stands beside
    /// it. The old item keeps its identifier and its contents, and comes back with a
    /// [`Kernel::set_state`] or a [`Kernel::undo`] like anything else.
    pub fn supersede(&self, old: ContextId, item: ContextItem) -> Result<ContextId> {
        if self.item(old).is_none() {
            return Err(Error::UnknownItem(old));
        }

        let new = self.add_item(item, true);
        self.set_state_one(
            old,
            ContextState::Superseded,
            Some(format!("superseded by item {new}")),
        );

        Ok(new)
    }

    /// Replaces an item's metadata, which is otherwise write-once.
    ///
    /// note: [`ContextItem::meta`] exists so that a [`Compactor`] or a [`Projector`] can be given
    /// hints, and a hint you can only set before the item exists is not much use - by the time
    /// you know a tool result was worthless, it has already been recorded.
    pub fn annotate(&self, id: ContextId, meta: Value) -> Result<()> {
        let changed = {
            let mut context = self.0.context.write();
            if context.item(id).is_none() {
                return Err(Error::UnknownItem(id));
            }
            context.annotate(id, meta.clone())
        };

        if changed {
            self.emit(Event::ContextAnnotated { id, meta });
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
    /// This is the whole of context control:
    ///
    /// ```text
    /// prune   -> set_state(ids, ContextState::Excluded, Some("garbage".into()))
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
            // mean the next `undo` walked back somebody else's work instead
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
        }

        for announcement in announcements {
            self.emit(announcement);
        }

        outcome
    }

    /// Replaces an item's content in place, keeping its identifier.
    pub fn replace(&self, id: ContextId, content: impl Into<Content>) -> Result<()> {
        let counter = self.counter();
        let changed = {
            let mut context = self.0.context.write();
            // an operation that is about to fail does not get a checkpoint
            if context.item(id).is_none() {
                return Err(Error::UnknownItem(id));
            }
            context.checkpoint();
            context.replace(id, content.into(), &*counter)
        };

        match changed {
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

    /// Reverts the most recent context operation, returning whether there was one.
    ///
    /// note: The granularity is one operation, not one item: undoing a [`Kernel::set_state`]
    /// that excluded eight items puts all eight back. Model responses and tool results are
    /// operations too, so undo can also walk back a turn's worth of additions.
    pub fn undo(&self) -> bool {
        let Some(diff) = ({
            let mut context = self.0.context.write();
            let before = context.items().to_vec();
            context.undo().then(|| Self::diff(&before, context.items()))
        }) else {
            return false;
        };

        self.emit(Event::ContextUndone {
            items: diff.items,
            removed: diff.gone,
            changed: diff.changed,
        });

        true
    }

    /// Puts back what the last [`Kernel::undo`] took away, returning whether there was any.
    ///
    /// note: A stack, not a toggle: undoing three operations and redoing them puts all three
    /// back, in order. Any new context operation makes the redone future unreachable, because a
    /// redo that reached across work done since would be overwriting it rather than restoring
    /// anything.
    pub fn redo(&self) -> bool {
        let Some(diff) = ({
            let mut context = self.0.context.write();
            let before = context.items().to_vec();
            context.redo().then(|| Self::diff(&before, context.items()))
        }) else {
            return false;
        };

        self.emit(Event::ContextRedone {
            items: diff.items,
            restored: diff.appeared,
            changed: diff.changed,
        });

        true
    }

    /// Recounts every item's tokens with the active [`TokenCounter`].
    pub fn recount(&self) {
        let counter = self.counter();
        let (tokens_before, tokens_after) = {
            let mut context = self.0.context.write();
            let before = context.tokens();
            context.recount(&*counter);
            (before, context.tokens())
        };

        self.emit(Event::ContextRecounted {
            tokens_before,
            tokens_after,
        });
    }

    /// Returns how much room the next request would take, and how much there is.
    pub fn budget(&self) -> Budget {
        let tool_tokens = self.tool_tokens();
        let limit = self.model_info().and_then(|i| i.context_limit);
        let reported = self.last_response().and_then(|response| response.usage);

        Budget {
            context_tokens: self.projected().1,
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
    /// note: This is the whole point of the exercise: there is no step between this and the wire
    /// where the kernel adds something of its own.
    ///
    /// note: The one thing that can still change the request is a [`Compactor`], which runs at
    /// the start of the next [`Kernel::step`] - and says exactly what it did.
    pub fn preview_request(&self) -> Result<ModelRequest> {
        self.build_request().map(|(request, _, _)| request)
    }

    /// Renders the payload the provider would send for the next request, exactly as it would
    /// send it - or `None` when the provider cannot show one.
    ///
    /// note: This is as close to the wire as a kernel with no wire format can get, and it is
    /// worth being precise about what it is: [`Kernel::preview_request`] is the kernel's own
    /// account, which it can guarantee, and this is the provider's account of what it will make
    /// of it, which it cannot. A provider that renders one payload here and sends another is
    /// lying in the same way a [`Tool`] that declares `Read` and opens a socket is lying, and
    /// the defence is the same: you chose the provider.
    ///
    /// note: The body only. Headers, URLs and credentials never pass through the kernel.
    pub fn preview_payload(&self) -> Result<Option<Value>> {
        let provider = self.provider().ok_or(Error::NoProvider)?;
        let (request, _, _) = self.build_request()?;

        Ok(provider.render(&request))
    }

    /// Applies a compaction plan, returning (and broadcasting) a report of what it did.
    ///
    /// note: Pinned items in the plan are refused, and listed in [`CompactionReport::refused`].
    pub fn apply_compaction(&self, plan: CompactionPlan) -> CompactionReport {
        let counter = self.counter();
        let CompactionPlan {
            remove,
            summary,
            reason,
        } = plan;

        let mut removed = Vec::new();
        let mut refused = Vec::new();
        let mut announcements = Vec::new();
        let mut added = None;

        // the whole pass happens under one lock. Taking it item by item would let a push land
        // between the checkpoint and the removals, and the next `undo` would then restore a
        // context that never existed - one without the item somebody had just added
        let (tokens_before, tokens_after) = {
            let mut context = self.0.context.write();
            let tokens_before = context.tokens();
            context.checkpoint();

            for id in remove {
                let Some(item) = context.item(id) else {
                    continue;
                };
                let entry = Removed {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                };
                let (pinned, projected) = (item.state == ContextState::Pinned, item.is_projected());

                if pinned {
                    refused.push(entry);
                } else if projected {
                    let note = Some(format!("compaction: {reason}"));
                    if let Some(from) = context.set_state(id, ContextState::Excluded, note.clone())
                    {
                        announcements.push(Event::ContextChanged {
                            id,
                            from,
                            to: ContextState::Excluded,
                            note,
                        });
                    }
                    removed.push(entry);
                }
            }

            if let Some(item) = summary {
                let id = context.add(item, &*counter);
                let item = context.item(id).expect("the item was just added");
                announcements.push(Event::ContextAdded {
                    id,
                    kind: item.kind.name().to_owned(),
                    source: item.source.clone(),
                    label: item.label.clone(),
                    tokens: item.tokens,
                    because: item.included_because.clone(),
                });
                added = Some(Removed {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                });
            }

            (tokens_before, context.tokens())
        };

        for announcement in announcements {
            self.emit(announcement);
        }

        let report = CompactionReport {
            removed,
            refused,
            summary: added,
            reason,
            tokens_before,
            tokens_after,
        };
        self.emit(Event::Compacted {
            report: report.clone(),
        });

        report
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
    pub fn cancel_pending_calls(&self, reason: impl Into<String>) -> usize {
        let reason = reason.into();
        let prepared = {
            let mut machine = self.0.machine.lock();
            let prepared = std::mem::take(&mut machine.pending);
            if !prepared.is_empty() {
                self.transition(&mut machine, State::Idle);
            }

            prepared
        };
        let count = prepared.len();

        for call in prepared {
            self.emit(Event::PermissionDecided {
                id: call.request.id,
                call: call.call.id.clone(),
                tool: call.call.tool.clone(),
                grant: Grant::Deny,
                source: GrantSource::Cancellation,
            });
            self.record_tool_result(
                &call.call,
                ToolOutput::error(format!("the call was cancelled: {reason}")),
                None,
                None,
                true,
            );
        }

        count
    }

    /// Performs one transition of the state machine, and returns the state it produced.
    ///
    /// | from | what happens | to |
    /// | --- | --- | --- |
    /// | [`State::Idle`], [`State::Finished`] | a request is built and sent | `Finished`, `Ready` or `Deciding` |
    /// | [`State::Ready`] | the tools run, in order, and their results are recorded | `Idle` |
    /// | [`State::Deciding`] | nothing; the answer has to come from you | `Deciding` |
    /// | [`State::Requesting`], [`State::Executing`] | nothing; [`Error::Busy`] | - |
    ///
    /// note: One step is one transition, so the model asking for a tool and that tool running
    /// are two of them. That is deliberate: [`State::Ready`] is a checkpoint at which the model
    /// has said what it wants and nothing has happened yet.
    pub async fn step(&self) -> Result<State> {
        // somebody asked to stop. One transition attempt is spent acknowledging it, which is
        // also what keeps the flag from outliving the request it was meant for: a client that
        // drives `step` itself has no `turn` to consume it
        if self.0.interrupted.swap(false, SeqCst) {
            return Ok(self.state());
        }

        let claim = {
            let mut machine = self.0.machine.lock();
            match machine.state.clone() {
                state if state.is_busy() => return Err(Error::Busy),
                State::Deciding { calls } => return Ok(State::Deciding { calls }),
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
            Claim::Request => self.request().await,
            Claim::Execute(prepared) => self.execute(prepared).await,
        }
    }

    /// Repeats [`Kernel::step`] until the model ends its turn, a decision is needed, or
    /// [`Config::max_requests_per_turn`] is reached; returns the state it stopped in.
    ///
    /// note: [`State::Finished`] means the model answered, [`State::Deciding`] means it is your
    /// move, and [`State::Idle`] means the request budget ran out mid-loop - calling `turn`
    /// again picks up exactly where it left off.
    pub async fn turn(&self) -> Result<State> {
        let mut requests = 0;

        loop {
            // somebody asked to stop; the step that just finished was recorded in full, and the
            // flag is cleared so that the next `turn` is not surprised by it
            if self.0.interrupted.swap(false, SeqCst) {
                return Ok(self.state());
            }

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

            match self.step().await? {
                state @ (State::Finished { .. } | State::Deciding { .. }) => return Ok(state),
                _ => continue,
            }
        }
    }

    // -------------------------------------------------------------------------------- internals

    /// Builds and sends a request, records the answer, and prepares whatever it asked for.
    async fn request(&self) -> Result<State> {
        // whatever happens - an error, or this future being dropped - the kernel does not stay
        // in `Requesting`
        let mut restore = Restore::new(self, State::Idle);

        self.maybe_compact().await;

        // a step that gets this far and then cannot proceed says why, rather than showing up on
        // the stream as a pair of state changes with nothing between them
        let prepared = self
            .provider()
            .ok_or(Error::NoProvider)
            .and_then(|provider| self.build_request().map(|built| (provider, built)));
        let (provider, (request, projection, tokens)) = match prepared {
            Ok(prepared) => prepared,
            Err(e) => {
                self.emit(Event::StepFailed {
                    error: e.to_string(),
                });
                return Err(e);
            }
        };

        // the provider's own account of what it is about to send, when it can give one and the
        // user has asked for it to be kept; see `Config::record_payloads` for why it is not free
        if self.0.config.record_payloads
            && let Some(payload) = provider.render(&request)
        {
            self.emit(Event::ModelPayload { payload });
        }

        self.emit(Event::ModelRequested {
            model: provider.info(),
            messages: request.messages.len(),
            tools: request.tools.len(),
            tokens,
            items: projection.included,
            skipped: projection.skipped,
            repairs: projection.repairs,
        });

        let mut response = match provider
            .respond(request, DeltaSink::new(self.clone()))
            .await
        {
            Ok(response) => response,
            Err(e) => {
                self.emit(Event::ModelFailed {
                    error: e.to_string(),
                });
                return Err(Error::Provider(e));
            }
        };
        // the provider has just said what the request it was handed actually cost, beside the
        // estimate that was made of it; the counter is told, and decides for itself whether that
        // is worth anything to it
        if let Some(reported) = response.usage.and_then(|usage| usage.input_tokens) {
            self.counter().observe(tokens, reported as usize);
        }

        // a model's tool calls are only useful if their identifiers are, and in practice they
        // sometimes are not (a streamed call whose first fragment carried no id, a provider that
        // numbers them all `0`)
        self.repair_call_ids(&mut response.tool_calls);

        let response = Arc::new(response);
        *self.0.last_response.write() = Some(response.clone());

        // note: the reasoning is recorded on the turn that produced it, so that it is counted
        // and prunable like everything else, and so that a provider whose API insists on seeing
        // its own thinking again can get it back; whether it is *sent* is the projector's call
        let content = response.content.clone().unwrap_or_default();
        let item = self.add_item(
            ContextItem::assistant(content, response.tool_calls.clone())
                .with_reasoning(response.reasoning.clone()),
            true,
        );
        self.emit(Event::ModelFinished {
            stop: response.stop.clone(),
            usage: response.usage,
            tool_calls: response.tool_calls.iter().map(|c| c.id.clone()).collect(),
            item,
        });

        let to = if response.tool_calls.is_empty() {
            let to = State::Finished {
                item,
                stop: response.stop.clone(),
            };
            self.transition(&mut self.0.machine.lock(), to.clone());
            to
        } else {
            self.prepare_calls(&response.tool_calls).await
        };
        restore.disarm();

        Ok(to)
    }

    /// Gives every tool call a usable identifier that is unique *within the session*, announcing
    /// each change.
    ///
    /// note: This runs before the model's turn is recorded, so the call and its result always
    /// agree. Doing nothing instead would mean recording a pair that cannot be matched up -
    /// which most providers reject, and which is very hard to see afterwards.
    ///
    /// note: The identifiers a whole session has used are remembered, not just the ones in the
    /// response being repaired. A provider that numbers its calls from zero on every turn - and
    /// they exist - would otherwise produce a request carrying the same `tool_call_id` twice,
    /// and, worse, one in which pruning a single result silently leaves a call unanswered,
    /// because a set of identifiers cannot tell the two apart.
    fn repair_call_ids(&self, calls: &mut [ToolCall]) {
        let mut repairs = Vec::new();
        {
            let mut seen = self.0.seen_calls.lock();
            let mut in_response: HashSet<ToolCallId> = HashSet::with_capacity(calls.len());

            for (index, call) in calls.iter_mut().enumerate() {
                let reason = if call.id.0.is_empty() {
                    "the provider left the identifier empty"
                } else if in_response.contains(&call.id) {
                    "the provider used the identifier twice in one response"
                } else if seen.contains(&call.id) {
                    "the provider reused an identifier from earlier in the session"
                } else {
                    seen.insert(call.id.clone());
                    in_response.insert(call.id.clone());
                    continue;
                };

                let was = std::mem::take(&mut call.id.0);
                let mut attempt = 0;
                call.id = loop {
                    let candidate = match attempt {
                        0 => ToolCallId(format!("call_{index}")),
                        n => ToolCallId(format!("call_{index}_{n}")),
                    };
                    if !seen.contains(&candidate) {
                        break candidate;
                    }
                    attempt += 1;
                };
                seen.insert(call.id.clone());
                in_response.insert(call.id.clone());

                repairs.push(Event::ToolCallRepaired {
                    call: call.id.clone(),
                    was,
                    reason: reason.to_owned(),
                });
            }
        }

        for repair in repairs {
            self.emit(repair);
        }
    }

    /// Matches the model's calls to tools, asks the policy about each, and queues them.
    ///
    /// note: Nothing about a permission is announced until every call is queued and the state
    /// machine has moved. A client's whole job here is to answer the question it is handed, and
    /// it would not be much of a runtime if [`Kernel::decide`] could fail purely because the
    /// client was quick about it - which it did, for as long as the policy was still being
    /// consulted about the *next* call in the batch.
    async fn prepare_calls(&self, calls: &[ToolCall]) -> State {
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
    async fn execute(&self, prepared: Vec<PreparedCall>) -> Result<State> {
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
        let (grant, _) = grant.expect("every claimed call has been decided");
        if grant == Grant::Deny {
            return ToolOutput::error("the call was not permitted");
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
            item.note = Some("the whole output; the model was shown a shortened copy".to_owned());

            // the pair is one thing that happened, so it gets one checkpoint, taken here
            self.add_item(item, true)
        });

        let truncated = limit.and_then(|limit| output.content.truncate_to(limit));
        self.record_tool_result(&prepared.call, output, truncated, whole, whole.is_none());
    }

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
    fn transition(&self, machine: &mut Machine, to: State) {
        if machine.state == to {
            return;
        }

        let from = std::mem::replace(&mut machine.state, to.clone());
        self.emit(Event::StateChanged { from, to });
    }

    /// Records a tool result in the context and broadcasts [`Event::ToolFinished`].
    ///
    /// note: `checkpoint` is false only when the caller has already taken one for this result -
    /// a truncated output is recorded as two items, and one [`Kernel::undo`] should take back
    /// both of them rather than leaving half a tool call behind.
    fn record_tool_result(
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
        item.note = match (truncated, whole) {
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

    /// Adds an item to the context, optionally checkpointing it for [`Kernel::undo`] first.
    fn add_item(&self, item: ContextItem, checkpoint: bool) -> ContextId {
        let counter = self.counter();
        let (id, kind, source, label, tokens, because) = {
            let mut context = self.0.context.write();
            if checkpoint {
                context.checkpoint();
            }
            let id = context.add(item, &*counter);
            let item = context.item(id).expect("the item was just added");

            (
                id,
                item.kind.name().to_owned(),
                item.source.clone(),
                item.label.clone(),
                item.tokens,
                item.included_because.clone(),
            )
        };

        self.emit(Event::ContextAdded {
            id,
            kind,
            source,
            label,
            tokens,
            because,
        });

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

    /// Moves one item to the given state, without checkpointing; `None` if there is no such item
    /// or if nothing would change.
    fn set_state_one(
        &self,
        id: ContextId,
        state: ContextState,
        note: Option<String>,
    ) -> Option<ContextState> {
        let from = self.0.context.write().set_state(id, state, note.clone())?;

        self.emit(Event::ContextChanged {
            id,
            from,
            to: state,
            note,
        });

        Some(from)
    }

    /// Builds the next request, along with the projection it came from and its estimated size.
    fn build_request(&self) -> Result<(ModelRequest, Projection, usize)> {
        let counter = self.counter();
        let tools = self.tool_specs();
        let tool_tokens = tool_tokens(&tools, &*counter);

        let (projection, context_tokens) = self.projected();

        if projection.messages.is_empty() {
            return Err(Error::EmptyProjection);
        }

        let request = ModelRequest {
            messages: projection.messages.clone(),
            tools,
            params: self.params(),
        };

        Ok((request, projection, context_tokens + tool_tokens))
    }

    /// Asks the compactor whether the context needs managing, and applies whatever it says.
    async fn maybe_compact(&self) {
        let Some(compactor) = self.compactor() else {
            return;
        };

        let budget = self.budget();
        if !compactor.should_compact(&budget) {
            return;
        }

        let items = self.items();
        if let Some(plan) = compactor.plan(&items, &budget).await {
            self.apply_compaction(plan);
        }
    }

    /// Projects the context, and returns the projection together with what the items that made
    /// it into it are estimated to cost.
    ///
    /// note: Both [`Kernel::budget`] and the request builder go through here, so that the number
    /// a client is shown is the number that is about to be sent. Summing the projected *items*
    /// instead would quietly count the ones the projector then repairs away.
    fn projected(&self) -> (Projection, usize) {
        let projector = self.projector();
        let context = self.0.context.read();
        let projection = projector.project(context.items());
        let tokens = projection
            .included
            .iter()
            .filter_map(|id| context.item(*id).map(|i| i.tokens))
            .sum::<usize>();

        (projection, tokens)
    }

    /// Returns the estimated size of the tool definitions.
    fn tool_tokens(&self) -> usize {
        let counter = self.counter();

        tool_tokens(&self.tool_specs(), &*counter)
    }
}

/// Estimates the size of the given tool definitions: the schemas plus the descriptions.
fn tool_tokens(specs: &[ToolSpec], counter: &dyn TokenCounter) -> usize {
    specs
        .iter()
        .map(|spec| {
            counter.count_schema(&spec.schema)
                + counter.count(&Content::text(format!("{} {}", spec.id, spec.description)))
        })
        .sum()
}
