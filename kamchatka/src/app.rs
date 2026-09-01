//! Everything the terminal knows: what is on the screen, what the keys do, and what to make of
//! the events the kernel broadcasts.
//!
//! note: The kernel is driven from a task of its own, and this loop never blocks on it. What
//! arrives here is [`Event`]s - the same ones the session log is made of - so the screen is a
//! rendering of the record rather than a second account of it. When a turn stops for a decision,
//! the task ends and hands control back; nothing is waiting on a channel for an answer.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nachalnik::{
    Block, Capability, Content, ContextId, ContextItem, ContextKind, ContextState, Delta, Event,
    Grant, GrantSource, Kernel, Projection, State, Verdict, selectors::Selector,
};
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    provider::Endpoint,
    sandbox::Confinement,
    tools::{Careful, Subject},
    ui::thousands,
};

/// How many trace lines are kept; the session log is the one that keeps everything.
const TRACE_DEPTH: usize = 400;

/// How long after the last keystroke a permission question starts taking keys as answers.
///
/// note: a question arrives on its own schedule, in the middle of whatever somebody happens to be
/// typing, and its keys are ordinary letters - `a` grants a capability for the rest of the session
/// and is also the third letter of "what". Long enough to cover the keystrokes already on their
/// way when it appeared; short enough that answering it is still one key.
pub const SETTLING: Duration = Duration::from_millis(300);

/// How much of a still-running tool's output the transcript holds on to.
const LIVE_OUTPUT: usize = 8_000;

/// How many lines `pgup` and `pgdn` move an overlay.
const PAGE: usize = 20;

/// How many earlier versions of one item the viewer keeps.
const VERSIONS: usize = 8;

/// Which half of the window the keys are talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The prompt, which is under every tab and can always be typed into.
    Input,
    /// Whatever the open tab is showing.
    Body,
}

/// What the window is showing.
///
/// note: Whole-window tabs rather than panes side by side. Three things want the screen - the
/// conversation, the context and the event stream - and splitting it between them meant all three
/// were cramped: the trace was cut off mid-sentence, the context could only afford a label and a
/// number, and a long answer was reading in sixty columns. Only one of them is being read at a
/// time. The prompt and the status line are under all of them, because a message can be sent from
/// anywhere and the budget is always worth seeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// The conversation.
    Chat,
    /// The context, item by item.
    Context,
    /// Every event, as it happens.
    Trace,
    /// What the policy will do about each capability, and what that covers.
    Permissions,
}

impl Tab {
    /// The tabs, in the order they are shown.
    pub const ALL: [Self; 4] = [Self::Chat, Self::Context, Self::Trace, Self::Permissions];

    /// What it is called on the tab strip.
    pub fn name(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Context => "context",
            Self::Trace => "trace",
            Self::Permissions => "permissions",
        }
    }
}

/// One row of the permissions tab: a capability or a path rule, what the policy will answer about
/// it, and the tools that would be affected.
pub struct Stance {
    /// What the row is about: a capability, or a pattern the paths are matched against.
    pub subject: Subject,
    /// What the policy answers about it today.
    pub verdict: Verdict,
    /// The registered tools that declare it, in the order the model is offered them.
    pub tools: Vec<String>,
    /// The registered tools the policy judges against it only sometimes, by looking at the call.
    ///
    /// note: `network` and `shell` are the pair this exists for. No tool here declares `network` -
    /// a model that wants the network writes `curl` - so the row read `nothing registered needs
    /// it` beside a verdict of `deny`, which is a restriction that was not there. What is there is
    /// [`crate::tools::Careful`] reading the command, and that is what this says.
    pub sometimes: Vec<String>,
}

impl Stance {
    /// Whether somebody has actually answered about this, as opposed to it being the default.
    pub fn is_decided(&self) -> bool {
        self.verdict != Verdict::Ask
    }
}

/// One line of the trace pane: an event's name, and what it says for itself.
pub struct Traced {
    /// The dotted name, e.g. `model.requested`; empty for a continuation line.
    pub name: String,
    /// The rest of it.
    pub detail: String,
    /// When it arrived.
    ///
    /// note: what a log is missing without a clock is the question people actually bring to one:
    /// which step was slow. Kept as an instant rather than a rendered string because what the
    /// pane shows is the gap to the line above, which is not a property of either line alone.
    pub at: Instant,
}

/// What is being shown over the top of everything else.
pub enum Overlay {
    /// A tool wants to run, and somebody has to say so.
    Permission {
        /// How far down its arguments are scrolled.
        scroll: usize,
    },
    /// Something long enough to need its own screen.
    Text {
        /// What it is.
        title: String,
        /// Its faces, in the order they are offered; almost everything has exactly one.
        pages: Vec<Page>,
        /// Which of them is on screen.
        page: usize,
        /// How far down it is scrolled.
        scroll: usize,
    },
}

/// One face of whatever an overlay is showing.
///
/// note: a context item has more than one honest answer to "what is this?" - what the request
/// will contain, what the item says, and what it said before somebody rewrote it - and picking
/// one of them to show was how the viewer came to be quietly wrong about the other two.
pub struct Page {
    /// What to call it on the strip along the top.
    pub name: String,
    /// The thing itself.
    pub body: String,
}

/// Who produced a line of the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    /// The person at the terminal.
    User,
    /// The model's answer.
    Model,
    /// The model's reasoning, where the provider exposes it.
    Reasoning,
    /// A tool the model asked for.
    Call,
    /// What that tool said.
    Result,
    /// The runtime, saying what it did.
    Note,
    /// Something went wrong.
    Error,
}

/// One contribution to the conversation.
pub struct Entry {
    /// Who said it.
    pub speaker: Speaker,
    /// What they said.
    pub text: String,
    /// Whether more of it is still arriving.
    pub open: bool,
}

/// What the kernel's task reports when it stops.
pub enum Outcome {
    /// The turn ended in this state.
    Stopped(State),
    /// One transition happened, and produced this state.
    Stepped(State),
    /// It could not be finished.
    Failed(String),
}

/// The whole of the terminal's state.
pub struct App {
    /// The runtime.
    pub kernel: Kernel,
    /// When the runtime last started doing something, for the marker that says it still is.
    ///
    /// note: read at draw time rather than counted in frames, so the marker keeps time with the
    /// world instead of with the redraw rate - and so that it stops dead if the screen stops
    /// being drawn, which is the one thing it exists to make visible.
    pub since: Instant,

    /// The policy, which the permission overlay teaches.
    pub policy: Arc<Careful>,
    /// The provider, for switching models - whichever dialect it speaks.
    pub provider: Arc<dyn Endpoint>,
    /// What items used to say, oldest first, for the ones that have been rewritten.
    ///
    /// note: kept here rather than in the kernel because the kernel deliberately does not keep
    /// it. A replacement is the one context operation that overwrites something, which is why
    /// [`nachalnik::Event::ContextReplaced`] is the one event that carries content - so that a
    /// client which wants the history can have it, and one that does not pays nothing. Before
    /// this, an `amend` that rewrote a tool result left the old text nowhere a person could read
    /// it: on the trace as a line of JSON, and in an undo window that closes.
    versions: BTreeMap<ContextId, Vec<Content>>,

    /// The handle the two introspection tools reach the kernel through, while they are offered.
    ///
    /// note: it is here rather than in `main` because `/introspect` turns them on and off, and this is
    /// the thing that has to move when it does: they hold a weak handle to it, so dropping it is
    /// what takes their reach away. See [`crate::introspect::install`].
    pub introspect: Option<Arc<Kernel>>,
    /// How much of the shell's sandbox the kernel agreed to, asked once at startup.
    ///
    /// note: on `App` rather than worked out where it is drawn, because finding out means
    /// applying a ruleset in a child process and that is not something a frame should be doing
    /// sixty times a second. It cannot change while the program runs.
    pub confinement: Confinement,
    /// The conversation.
    pub transcript: Vec<Entry>,
    /// Every event, name and detail.
    pub trace: VecDeque<Traced>,
    /// The prompt.
    pub input: TextArea<'static>,
    /// Which pane the keys go to.
    pub focus: Focus,
    /// Which context item is picked out.
    pub selected: usize,
    /// Where the context pane is scrolled to, which it keeps between frames.
    pub list: ratatui::widgets::ListState,
    /// What is on top, if anything.
    pub overlay: Option<Overlay>,
    /// The first transcript line on screen.
    pub scroll: usize,
    /// Whether the transcript sticks to the bottom.
    pub follow: bool,
    /// Which tab the window is showing.
    pub tab: Tab,
    /// How far back through the trace it is scrolled, in lines from the bottom.
    pub trace_scroll: usize,
    /// Whether a turn is running.
    pub busy: bool,
    /// Whether it is time to leave.
    pub quit: bool,
    /// How many wrapped lines the transcript came to, as of the last frame.
    pub rendered: usize,
    /// How many lines fit, as of the last frame.
    pub viewport: usize,
    /// Which context item the prompt is editing, if it is editing one rather than composing a
    /// message.
    pub editing: Option<ContextId>,
    /// Whether the loop is being driven a transition at a time, so that answering a permission
    /// does not quietly run the rest of the turn.
    pub stepping: bool,
    /// Digits typed at the context tab, waiting for the key that uses them.
    pub count: String,
    /// Which capability is picked out on the permissions tab.
    pub chosen: usize,
    /// Where the permissions tab is scrolled to, which it keeps between frames.
    pub grants: ratatui::widgets::ListState,
    /// Whether the last stop was asked for rather than reached.
    interrupting: bool,
    /// When a key that was not an answer to a question was last pressed.
    typing: Instant,
    /// A message somebody sent into a turn that was already running, waiting for it to end.
    typed_ahead: Option<String>,
    /// Whether the response being awaited has put anything on the screen of its own.
    streamed: bool,
    /// How much the running tool has said so far, for the one trace line that counts it.
    streamed_bytes: usize,
    /// Where a finished turn reports itself.
    outcomes: UnboundedSender<Outcome>,
}

impl App {
    /// Builds the terminal's state around a kernel that is already wired up.
    pub fn new(
        kernel: Kernel,
        policy: Arc<Careful>,
        provider: Arc<dyn Endpoint>,
        outcomes: UnboundedSender<Outcome>,
    ) -> Self {
        let mut input = TextArea::default();
        input.set_placeholder_text("ask for something, or /help");
        input.set_cursor_line_style(ratatui::style::Style::default());
        // a long message wraps rather than scrolling sideways: the default keeps one long line on
        // one row and slides it under the left border, so what somebody typed a moment ago is off
        // the screen while they are still typing it. `WordOrGlyph` breaks at spaces and splits a
        // word only when it could not fit on a line of its own - a path or a URL, which is
        // exactly the thing worth seeing all of
        input.set_wrap_mode(WrapMode::WordOrGlyph);

        Self {
            kernel,
            policy,
            provider,
            versions: BTreeMap::new(),
            introspect: None,
            // the terminal's own default, for a screen test that never spawns anything; the
            // program overwrites it with what a child process actually reported
            confinement: Confinement::Unsupported,
            transcript: Vec::new(),
            trace: VecDeque::new(),
            input,
            focus: Focus::Input,
            selected: 0,
            list: ratatui::widgets::ListState::default(),
            overlay: None,
            scroll: 0,
            follow: true,
            tab: Tab::Chat,
            trace_scroll: 0,
            busy: false,
            quit: false,
            rendered: 0,
            viewport: 0,
            editing: None,
            stepping: false,
            count: String::new(),
            chosen: 0,
            grants: ratatui::widgets::ListState::default(),
            interrupting: false,
            since: Instant::now(),
            typing: Instant::now(),
            typed_ahead: None,
            streamed: false,
            streamed_bytes: 0,
            outcomes,
        }
    }

    // ------------------------------------------------------------------------ saying things

    /// Adds a finished entry to the transcript, ending whatever was still arriving.
    ///
    /// note: only the person's own line takes the view back to the bottom. Everything else that
    /// arrives leaves the scroll where somebody put it - a model writing four hundred lines used
    /// to yank the window back to the newest of them on every fragment, so reading anything it
    /// had said thirty seconds ago was impossible until the turn ended.
    pub fn say(&mut self, speaker: Speaker, text: impl Into<String>) {
        self.close();
        self.transcript.push(Entry {
            speaker,
            text: text.into(),
            open: false,
        });
        if speaker == Speaker::User {
            self.follow = true;
        }
    }

    /// Appends to the open entry from this speaker, opening one if there is none.
    fn append(&mut self, speaker: Speaker, fragment: &str) {
        match self.transcript.last_mut() {
            Some(entry) if entry.open && entry.speaker == speaker => {
                entry.text.push_str(fragment);
                // a tool that produces megabytes should not be able to fill this up; the whole
                // of it is in the context either way, one keystroke from being read
                if entry.text.len() > LIVE_OUTPUT {
                    let cut = entry
                        .text
                        .char_indices()
                        .nth(entry.text.chars().count() - LIVE_OUTPUT / 2)
                        .map(|(at, _)| at)
                        .unwrap_or(0);
                    entry.text = format!("[...]\n{}", &entry.text[cut..]);
                }
            }
            _ => {
                self.close();
                self.transcript.push(Entry {
                    speaker,
                    text: fragment.to_owned(),
                    open: true,
                });
            }
        }
    }

    /// Closes whatever was still arriving, and drops it if it turned out to be nothing.
    fn close(&mut self) {
        let Some(entry) = self.transcript.last_mut() else {
            return;
        };

        entry.open = false;
        entry.text = entry.text.trim_end().to_owned();
        if entry.text.is_empty() {
            self.transcript.pop();
        }
    }

    /// Renders a context that already exists as a conversation, for a session picked back up.
    ///
    /// note: A resumed session arrives as one [`Event::SessionResumed`] rather than a thousand
    /// additions, which is the truthful thing for the runtime to broadcast - what happened is
    /// that a session was picked up, not that a thousand things were said. It does leave the
    /// screen with nothing on it, so this reads the conversation back off the context. It is a
    /// rendering of state rather than a replay of events, and it says so at the end, because the
    /// items it draws as a conversation are not all necessarily going to be sent.
    pub fn replay(&mut self) {
        let items = self.kernel.items();
        self.retell(&items);

        let withheld = items.iter().filter(|item| !item.is_projected()).count();
        self.say(
            Speaker::Note,
            format!(
                "resumed session {}: {} items, ~{} tokens{}",
                self.kernel.session_name(),
                items.len(),
                self.kernel.budget().context_tokens,
                match withheld {
                    0 => String::new(),
                    n => format!(", {n} of which the pane says are not being sent"),
                }
            ),
        );
    }

    /// Puts a run of context items on the transcript as the conversation they were.
    fn retell(&mut self, items: &[Arc<ContextItem>]) {
        for item in items {
            match &item.kind {
                ContextKind::UserMessage => self.say(Speaker::User, item.content.to_text()),
                ContextKind::AssistantMessage { .. } => {
                    let text = item.content.to_text();
                    if !text.trim().is_empty() {
                        self.say(Speaker::Model, text);
                    }
                    // `calls()`, so a turn the provider recorded as ordered blocks reads back
                    // with the tools it asked for rather than as bare text
                    for call in item.calls() {
                        let args = one_line(&call.args.to_string());
                        self.say(Speaker::Call, format!("{}({args})", call.tool));
                    }
                }
                ContextKind::ToolResult { tool, is_error, .. } => {
                    self.say(Speaker::Result, head(&item.content.to_text(), 6));
                    self.say(
                        Speaker::Note,
                        format!(
                            "{tool}: {} tokens{}",
                            item.tokens,
                            match is_error {
                                true => ", reported as an error",
                                false => "",
                            }
                        ),
                    );
                }
                _ => self.say(
                    Speaker::Note,
                    format!(
                        "[{}] {} ({}), {} tokens",
                        item.id, item.label, item.source, item.tokens
                    ),
                ),
            }
        }
    }

    /// Adds an event to the trace pane.
    fn trace(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        if self.trace.len() == TRACE_DEPTH {
            self.trace.pop_front();
        }
        self.trace.push_back(Traced {
            name: name.into(),
            detail: detail.into(),
            at: Instant::now(),
        });
    }

    // ---------------------------------------------------------------------- driving the kernel

    /// Starts, or carries on with, a turn.
    pub fn start_turn(&mut self) {
        if self.busy {
            return;
        }

        self.busy = true;
        self.since = Instant::now();
        self.stepping = false;
        self.interrupting = false;
        let (kernel, outcomes) = (self.kernel.clone(), self.outcomes.clone());
        tokio::spawn(async move {
            let outcome = match kernel.turn().await {
                Ok(state) => Outcome::Stopped(state),
                Err(e) => Outcome::Failed(e.to_string()),
            };
            let _ = outcomes.send(outcome);
        });
    }

    /// Performs exactly one transition of the state machine, and stops.
    ///
    /// note: This is the runtime's own shape, made visible. A turn is a loop over `step`, and
    /// running it a transition at a time is the only way to stand in [`State::Ready`] and look at
    /// what the model has asked for *before* any of it runs - which the kernel documents as a
    /// resting state on purpose, and which a whole turn walks straight through.
    pub fn start_step(&mut self) {
        if self.busy {
            return;
        }

        self.busy = true;
        self.since = Instant::now();
        self.stepping = true;
        self.interrupting = false;
        let (kernel, outcomes) = (self.kernel.clone(), self.outcomes.clone());
        tokio::spawn(async move {
            let outcome = match kernel.step().await {
                Ok(state) => Outcome::Stepped(state),
                Err(e) => Outcome::Failed(e.to_string()),
            };
            let _ = outcomes.send(outcome);
        });
    }

    /// What one transition landed in, in a form somebody can act on.
    fn stepped(&mut self, state: State) {
        let told = match &state {
            // the whole point of stepping: the calls are decided and about to run, and nothing
            // has happened yet
            State::Ready { calls } => {
                let waiting: Vec<String> = self
                    .kernel
                    .pending_calls()
                    .iter()
                    .map(|call| format!("    {} {}", call.tool, one_line(&call.args.to_string())))
                    .collect();
                format!(
                    "ready: {} call(s) decided, none of them run yet\n{}",
                    calls.len(),
                    waiting.join("\n")
                )
            }
            State::Deciding { calls } => format!("deciding: {} waiting on you", calls.len()),
            State::Executing { calls } => format!("executing: {} running", calls.len()),
            State::Finished { stop, .. } => format!("finished: the model stopped, {stop:?}"),
            other => other.name().to_owned(),
        };

        self.say(Speaker::Note, format!("step → {told}"));
        if matches!(state, State::Deciding { .. }) {
            self.overlay = Some(Overlay::Permission { scroll: 0 });
        }
    }

    /// Takes in the end of a turn.
    pub fn on_outcome(&mut self, outcome: Outcome) {
        self.busy = false;
        self.close();

        // note: a turn that stopped to ask a question has not ended - the call it is asking about
        // still has a result to come - and a message pushed now would land between the call and
        // that result, which is a place a request cannot have one. A live run put "what is the
        // capital of Peru" exactly there
        let ended = matches!(outcome, Outcome::Stopped(ref state) if !matches!(state, State::Deciding { .. }));
        // a `Stepped` outcome is somebody driving this a transition at a time, and a failure is
        // not the moment to start something else; either way what was typed waits for `/continue`
        let carry_on = ended && !self.interrupting;
        match outcome {
            Outcome::Failed(e) => self.say(Speaker::Error, e),
            Outcome::Stopped(State::Deciding { .. }) => {
                self.overlay = Some(Overlay::Permission { scroll: 0 })
            }
            // a turn that stops in `Idle` either ran out of requests or was asked to stop, and
            // the difference matters to whoever is reading the screen
            Outcome::Stopped(State::Idle) if !self.interrupting => {
                let budget = self
                    .kernel
                    .config()
                    .max_requests_per_turn
                    .map(|max| max.to_string())
                    .unwrap_or_else(|| "the".into());
                self.say(
                    Speaker::Note,
                    format!("the turn paused after {budget} requests; /continue to carry on"),
                );
            }
            Outcome::Stopped(_) => {}
            Outcome::Stepped(state) => self.stepped(state),
        }
        self.interrupting = false;

        // a message somebody sent into this turn has waited for it to end; now it goes in, and
        // unless the turn was stopped or stepped it gets a turn of its own
        if ended && let Some(message) = self.typed_ahead.take() {
            self.kernel.push(ContextItem::user(message));
            if carry_on {
                self.start_turn();
            }
        }

        // the counter has just been told what the last request really cost, so the figures on the
        // older items are out of date. Bringing them into line is a decision, not a side effect
        self.kernel.recount();
    }

    // ------------------------------------------------------------------------- kernel events

    /// Takes in one event from the runtime.
    pub fn on_event(&mut self, event: Event) {
        // a line per streamed fragment would push everything else out of the trace before it could
        // be read - and a `cat` of a thousand lines really did erase the whole of it, one
        // `tool.output` at a time. The fragments themselves are on the chat tab; the session log
        // has them if `record_progress` is on
        match &event {
            Event::ModelDelta { .. } => {}
            Event::ToolOutput { tool, chunk, .. } => {
                self.streamed_bytes += chunk.len();
                let detail = format!("{tool}, {} bytes so far", thousands(self.streamed_bytes));
                // one line that counts up, rather than one line per chunk
                match self.trace.back_mut() {
                    Some(last) if last.name == "tool.output" => last.detail = detail,
                    _ => self.trace("tool.output", detail),
                }
            }
            _ => {
                let (name, detail) = trace_line(&event);
                self.trace(name, detail);
            }
        }

        match event {
            Event::ModelDelta { delta } => match delta {
                Delta::Text(fragment) => {
                    self.streamed = true;
                    self.append(Speaker::Model, &fragment);
                }
                Delta::Reasoning(fragment) => self.append(Speaker::Reasoning, &fragment),
                // the arguments are shown once they parse, as the call the model actually made
                _ => {}
            },
            Event::ModelRequested {
                repairs, skipped, ..
            } => {
                self.streamed = false;
                self.close();

                // the kernel altering what the model is told is not a detail for the trace pane.
                // One compaction pass can orphan half a dozen calls at once, though, and six
                // notices in a row push the answer off the screen to say one thing - so the
                // conversation gets the fact and ctrl+p gets the list
                match repairs.len() {
                    0 => {}
                    1 => self.say(
                        Speaker::Note,
                        format!("the request was repaired: {}", repairs[0]),
                    ),
                    many => self.say(
                        Speaker::Note,
                        format!("the request was repaired in {many} places; ctrl+p says where"),
                    ),
                }
                for repair in &repairs {
                    self.trace("", format!("repaired: {repair}"));
                }
                for left_out in skipped {
                    self.trace(
                        "",
                        format!("[{}] left out: {}", left_out.id, left_out.reason),
                    );
                }
            }
            Event::ModelFinished { item, .. } => {
                // a provider that does not stream leaves nothing on the screen, so the answer is
                // read back off the item the kernel recorded. Whether it streamed is remembered
                // rather than guessed at from the transcript: something else may well have been
                // said in between - "stopped", for one - and guessing wrong prints the answer
                // twice
                if !self.streamed
                    && let Some(item) = self.kernel.item(item)
                {
                    let text = item.content.to_text();
                    if !text.trim().is_empty() {
                        self.say(Speaker::Model, text);
                    }
                }
                self.streamed = false;
                self.close();
            }
            Event::ModelFailed { error } | Event::StepFailed { error } => {
                self.close();
                // a provider that fails produces both of these, the second wrapping the first;
                // two red lines saying the same thing is one more than the news warrants, and
                // the trace pane has both either way
                let repeat = self.transcript.last().is_some_and(|last| {
                    last.speaker == Speaker::Error && error.contains(&last.text)
                });
                if !repeat {
                    self.say(Speaker::Error, error);
                }
            }
            // note: a refusal the policy made on its own, which nobody was asked about and which
            // the tool result records only as `the call was not permitted`. When the tool's own
            // capability is `allow` - `shell` usually is - that leaves a refused call with nothing
            // on screen accounting for it, and "why was that refused?" is the question the
            // permissions tab exists to answer
            Event::PermissionDecided {
                call,
                tool,
                grant: Grant::Deny,
                source: GrantSource::Policy,
                ..
            } => {
                if let Some(reason) = self.policy.why(&call) {
                    self.say(Speaker::Note, format!("{tool}: {reason}"));
                }
            }
            // the one event that carries content, and the only place the old text exists at all
            // once the undo window closes; the viewer reads it back off `←` and `→`
            Event::ContextReplaced { id, was, .. } => self.remember(id, was),
            Event::ToolStarted { .. } => self.streamed_bytes = 0,
            Event::ToolRequested { tool, args, .. } => {
                self.close();
                self.say(
                    Speaker::Call,
                    format!("{tool}({})", one_line(&args.to_string())),
                );
            }
            Event::ToolOutput { chunk, .. } => self.append(Speaker::Result, &chunk),
            Event::ToolFinished {
                tool,
                tokens,
                is_error,
                truncated,
                item,
                ..
            } => {
                // whatever streamed in is replaced by the thing the model was actually given
                if self
                    .transcript
                    .last()
                    .is_some_and(|entry| entry.open && entry.speaker == Speaker::Result)
                {
                    self.transcript.pop();
                }
                if let Some(item) = self.kernel.item(item) {
                    self.say(Speaker::Result, head(&item.content.to_text(), 6));
                }

                let mut note = format!("{tool}: {tokens} tokens");
                if is_error {
                    note.push_str(", reported as an error");
                }
                if let Some(bytes) = truncated {
                    note.push_str(&format!(", {bytes} bytes held back"));
                }
                self.say(Speaker::Note, note);
            }
            Event::Compacted { report } => {
                let mut note = format!(
                    "compacted: {} items out, {} → {} tokens ({})",
                    report.removed.len(),
                    report.tokens_before,
                    report.tokens_after,
                    report.reason
                );
                if !report.refused.is_empty() {
                    note.push_str(&format!(
                        "; {} refused, because pinned",
                        report.refused.len()
                    ));
                }
                self.say(Speaker::Note, note);
            }
            Event::Interrupted => self.say(Speaker::Note, "stopped"),
            Event::ToolUnknown { tool, .. } => self.say(
                Speaker::Error,
                format!("the model asked for `{tool}`, which is not a tool here"),
            ),
            Event::ContextAdded {
                source,
                label,
                tokens,
                id,
                ..
            } if source == "file" => self.say(
                Speaker::Note,
                format!("[{id}] {label} is in the context, {tokens} tokens"),
            ),
            _ => {}
        }
    }

    // ----------------------------------------------------------------------------------- keys

    /// Takes in one key press.
    pub async fn on_key(&mut self, key: KeyEvent) {
        // windows reports both halves of every press; everywhere else this is already true
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // before anything else, including whatever is on top: these two mean the same thing
        // wherever they are pressed, and an overlay that took them for its own would be answering
        // a question nobody asked. `ctrl+d` at a permission prompt used to drop every pending
        // call, because `d` is a key there and nothing was looking at the modifiers
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d')) {
            match self.busy && key.code == KeyCode::Char('c') {
                true => self.interrupt(),
                false => self.quit = true,
            }
            return;
        }

        if self.overlay.is_some() {
            // a key that answered a question is not somebody typing, and must not push the moment
            // the *next* question starts listening at; every other key is
            if !self.overlay_key(key).await {
                self.typing = Instant::now();
            }
            return;
        }
        self.typing = Instant::now();

        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // taken rather than read, so that a count lives for exactly one key wherever that key is
        // handled: only the digit arm below puts it back. Cleared at the end of `context_key`
        // instead, a `4` followed by `tab` or `F1` - neither of which gets that far - survived to
        // send the next `G` to item 4
        let count = std::mem::take(&mut self.count);

        match (key.code, ctrl) {
            (KeyCode::Esc, _) if self.busy => self.interrupt(),
            (KeyCode::Char('t'), true) => self.show(self.next_tab()),
            (KeyCode::Char('1'), _) if alt => self.show(Tab::Chat),
            (KeyCode::Char('2'), _) if alt => self.show(Tab::Context),
            (KeyCode::Char('3'), _) if alt => self.show(Tab::Trace),
            (KeyCode::Char('4'), _) if alt => self.show(Tab::Permissions),
            (KeyCode::Char('p'), true) => {
                self.preview("the next request", request_preview(&self.kernel))
            }
            // the way back down from wherever the reading got to, without paging through however
            // much arrived in the meantime. Scrolling to the bottom does it too, and that is the
            // gesture most people will find first; this is the one for a turn that wrote a
            // thousand lines while somebody was looking at the twelfth
            (KeyCode::Char('e'), true) => self.follow = true,
            (KeyCode::F(1), _) => self.preview("the keys", crate::ui::HELP),
            // the chat tab has nothing to move the focus to: the conversation is read, not
            // operated, and swallowing what somebody typed at it would be a trap
            (KeyCode::Tab, _) if self.tab != Tab::Chat => {
                self.focus = match self.focus {
                    Focus::Input => Focus::Body,
                    Focus::Body => Focus::Input,
                }
            }
            _ => match (self.tab, self.focus) {
                (Tab::Context, Focus::Body) => self.context_key(key, &count),
                (Tab::Trace, Focus::Body) => self.trace_key(key),
                (Tab::Permissions, Focus::Body) => self.permissions_key(key),
                _ => self.input_key(key).await,
            },
        }
    }

    /// The tab after the open one, wrapping round.
    fn next_tab(&self) -> Tab {
        let at = Tab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);

        Tab::ALL[(at + 1) % Tab::ALL.len()]
    }

    /// Opens a tab, and puts the keys wherever they are useful on it.
    ///
    /// note: Switching to the context or the trace is something somebody does in order to work
    /// on it, so the focus follows; switching back to the conversation is not, so it does not.
    /// `tab` moves it either way.
    pub fn show(&mut self, tab: Tab) {
        // an edit belongs to the tab it was started from, and leaving that tab abandons it.
        // Otherwise the prompt is still holding the item's text with `editing` still set, and the
        // next message somebody types and sends is committed into the context instead of asked
        self.cancel_edit();

        self.tab = tab;
        self.focus = match tab {
            Tab::Chat => Focus::Input,
            _ => Focus::Body,
        };
    }

    /// Opens a tab without taking the keys off the prompt.
    ///
    /// note: For a tab reached by *typing a command*, where moving the focus would hand the next
    /// thing somebody types to the tab. The permissions tab is the worst place for that: `a`, `n`
    /// and `r` are all bare letters there, and all of them change something.
    pub fn open(&mut self, tab: Tab) {
        self.cancel_edit();
        self.tab = tab;
    }

    /// Puts the prompt back to composing a message, whatever it was doing.
    fn cancel_edit(&mut self) {
        if self.editing.take().is_some() {
            self.clear_input();
        }
    }

    /// Keys that belong to the prompt.
    async fn input_key(&mut self, key: KeyEvent) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            // the prompt is editing an item rather than composing a message; enter commits it and
            // escape puts it back the way it was
            KeyCode::Enter if !alt && self.editing.is_some() => {
                let text = self.input.lines().join("\n");
                self.clear_input();
                self.commit_edit(&text);
            }
            KeyCode::Esc if self.editing.is_some() => {
                self.clear_input();
                self.editing = None;
                self.focus = Focus::Body;
            }
            // enter sends, because that is what a prompt is for; a newline is alt+enter, which
            // is the one every terminal agrees on
            KeyCode::Enter if !alt => {
                let line = self.input.lines().join("\n").trim().to_owned();
                if line.is_empty() {
                    return;
                }
                self.clear_input();
                self.submit(&line).await;
            }
            KeyCode::Enter => {
                self.input.insert_newline();
            }
            // at the edges of the prompt, the arrows go on to the conversation
            KeyCode::Up if self.input.cursor().0 == 0 => self.scroll_by(-1),
            KeyCode::Down if self.input.cursor().0 + 1 == self.input.lines().len() => {
                self.scroll_by(1)
            }
            KeyCode::PageUp => self.scroll_by(-(self.viewport as isize / 2)),
            KeyCode::PageDown => self.scroll_by(self.viewport as isize / 2),
            _ => {
                self.input.input(key);
            }
        }
    }

    /// Puts pasted text into the prompt.
    ///
    /// note: the line breaks inside a paste arrive as carriage returns rather than newlines,
    /// because a terminal sends a paste as though it had been typed and that is what the enter key
    /// sends. The editor underneath splits on newlines, so a pasted stack trace went in as one
    /// line with invisible characters where its breaks were and read as its lines run together -
    /// in the one place whose whole job is to show somebody what they are about to send.
    pub fn paste(&mut self, text: &str) {
        self.input
            .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
    }

    /// Empties the prompt, wherever what was in it has just gone.
    fn clear_input(&mut self) {
        self.input.select_all();
        self.input.cut();
        self.input.move_cursor(CursorMove::End);
    }

    /// Puts an edited item into the context in place of the one it came from.
    ///
    /// note: [`Kernel::supersede`] rather than a replacement in place, so the old one is still
    /// there to be read and put back - marked `~`, with a note saying which item replaced it.
    /// The kind is carried over whole, which matters for an assistant turn: its tool calls live
    /// inside the kind, and rebuilding it without them would orphan their results.
    fn commit_edit(&mut self, text: &str) {
        let Some(id) = self.editing.take() else {
            return;
        };
        self.focus = Focus::Body;

        let Some(old) = self.kernel.item(id) else {
            self.say(Speaker::Error, format!("[{id}] is no longer there"));
            return;
        };
        if old.content.to_text() == text {
            self.say(Speaker::Note, format!("[{id}] is unchanged"));
            return;
        }

        let mut edited = ContextItem::new(
            old.kind.clone(),
            old.source.clone(),
            old.label.clone(),
            text.to_owned(),
        )
        .because("edited at the terminal");
        // editing something decides what it says, not whether it is sent - so whatever it was
        // doing, it goes on doing. `ContextItem::new` starts out Active, and carrying over only
        // the pin meant editing a pruned item quietly put it back into the next request, and
        // editing an *archived* one promoted the whole of an oversized tool output into it. An
        // elided one is the same trap in a quieter form: the row says a marker is being sent, so
        // an edit that came back Active would be sending the new text against what the screen
        // says. It stays elided, and `space` round to active is how you say you meant it read
        edited.state = match old.state {
            ContextState::Pinned
            | ContextState::Excluded
            | ContextState::Elided
            | ContextState::Archived => old.state,
            _ => ContextState::Active,
        };

        match self.kernel.supersede(id, edited) {
            Ok(new) => {
                // the old item keeps its own row, but the new one is where somebody will be
                // looking, so what it used to say follows it there. Copied rather than moved:
                // both rows are real, and both can answer "what did this say before?"
                let history = self.versions.get(&id).cloned().unwrap_or_default();
                self.versions.insert(new, history);
                self.remember(new, old.content.clone());

                self.say(
                    Speaker::Note,
                    format!("[{id}] is now [{new}]; the old one is still there, marked superseded"),
                );
            }
            Err(e) => self.say(Speaker::Error, e.to_string()),
        }
    }

    /// Keys that belong to the context pane.
    fn context_key(&mut self, key: KeyEvent, count: &str) {
        let items = self.kernel.items();
        if items.is_empty() {
            return;
        }
        self.selected = self.selected.min(items.len() - 1);
        let picked = items[self.selected].clone();

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(items.len() - 1)
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(self.viewport.max(2) / 2)
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + self.viewport.max(2) / 2).min(items.len() - 1)
            }
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            // `23G` goes to the item *numbered* 23 rather than the twenty-third row, because the
            // number in the first column is the one `/prune` takes and the one every note names.
            // Bare `G` is the last item, as everywhere else
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = match count.parse::<u64>() {
                    Ok(id) => match items.iter().position(|item| item.id.0 == id) {
                        Some(at) => at,
                        None => {
                            self.say(Speaker::Note, format!("there is no item [{id}]"));
                            self.selected
                        }
                    },
                    Err(_) => items.len() - 1,
                }
            }
            KeyCode::Char(digit) if digit.is_ascii_digit() => {
                self.count = format!("{count}{digit}");
            }
            KeyCode::Char('?') => self.preview("the keys", crate::ui::HELP),
            KeyCode::Esc => self.cancel_edit(),
            // changing what an item says, which is the verb the other keys were missing: `space`
            // and `p` decide whether the model reads it, and this decides what it reads
            KeyCode::Char('e') => {
                self.clear_input();
                self.input.insert_str(picked.content.to_text());
                self.input.move_cursor(CursorMove::End);
                self.editing = Some(picked.id);
                self.focus = Focus::Input;
            }
            // how much of an item the model gets, in three steps out and one back: all of it,
            // then a marker where it was, then nothing at all
            //
            // note: the middle step is the one worth having a key for. Taking a tool result out
            // makes the projector drop the call that asked for it, so the model reads a
            // conversation it never had; elided, the call keeps its answer and only the content
            // is gone. Which of the two somebody wants is not something this program can guess -
            // hiding a result outright is a fair thing to want - so it is a cycle rather than a
            // decision, the same way the permissions tab cycles a stance through three
            KeyCode::Char(' ') => {
                let (to, note) = match picked.state {
                    // note: this one is read by the model, in the brackets the projector puts
                    // round it, so it is written for somebody who has never heard of this
                    // program: no "at the terminal", which is this codebase's own idiom for
                    // "a person did it here" and reads to a model like a shell or a state. It
                    // does not invite the model to ask for it back either - the thing hidden may
                    // be the thing that should not be asked for
                    ContextState::Active | ContextState::Pinned => (
                        ContextState::Elided,
                        Some("removed from view by the user".into()),
                    ),
                    ContextState::Elided => (
                        ContextState::Excluded,
                        Some("taken out at the terminal".into()),
                    ),
                    _ => (ContextState::Active, None),
                };
                self.kernel.set_state([picked.id], to, note);
            }
            KeyCode::Char('p') => {
                let to = match picked.state {
                    ContextState::Pinned => ContextState::Active,
                    _ => ContextState::Pinned,
                };
                self.kernel.set_state([picked.id], to, None);
            }
            KeyCode::Char('u') => {
                let note = match self.kernel.undo() {
                    true => "undone",
                    false => "there is nothing to undo",
                };
                self.say(Speaker::Note, note);
            }
            KeyCode::Char('U') => {
                let note = match self.kernel.redo() {
                    true => "redone",
                    false => "there is nothing to redo",
                };
                self.say(Speaker::Note, note);
            }
            KeyCode::Enter => {
                let title = format!(
                    "[{}] {} · {} · {} · {} tokens",
                    picked.id,
                    picked.label,
                    picked.kind.name(),
                    picked.state,
                    picked.tokens
                );
                let (pages, at) = self.faces(&picked);
                self.preview_pages(title, pages, at);
            }
            _ => {}
        }
    }

    /// Every face of a context item worth reading, and which of them to open on.
    ///
    /// note: the default is what the item says, except when that is not what the model reads. An
    /// elided item goes into the request as a marker, an excluded one does not go at all, and a
    /// tool result whose call has been taken out is repaired away though nothing on its row says
    /// so - all three are rows where the screen and the request disagree, and the disagreement is
    /// what somebody pressed enter to find. So the projection decides this, not the state.
    fn faces(&self, item: &ContextItem) -> (Vec<Page>, usize) {
        let projection = self.kernel.project();
        let reads_it = projection.included.contains(&item.id) && item.state.sends_content();
        let mut pages = vec![
            Page {
                name: "to the model".into(),
                body: projected(&projection, item.id),
            },
            Page {
                name: "as stored".into(),
                body: match item.meta.get("revised") {
                    // who rewrote it and why, which `amend` records on the item itself; the trace
                    // has the rest, and this is the line that sends somebody to it
                    Some(revised) => format!(
                        "rewritten by `{}`: {}\n\n{}",
                        revised["by"].as_str().unwrap_or("something"),
                        revised["reason"].as_str().unwrap_or("no reason given"),
                        stored(item)
                    ),
                    None => stored(item),
                },
            },
        ];

        let history = self
            .versions
            .get(&item.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        // an undo puts an old content back, and the version it restored is then the current one
        // too; two identical pages side by side would be saying nothing twice
        let keep = history.len() - usize::from(history.last() == Some(&item.content));
        for (n, was) in history.iter().enumerate().take(keep).rev() {
            pages.push(Page {
                name: format!("v{}", n + 1),
                body: format!(
                    "version {} of {}, before it was rewritten\n\n{}",
                    n + 1,
                    keep + 1,
                    whole(was)
                ),
            });
        }

        (pages, usize::from(reads_it))
    }

    /// Keeps what an item used to say, so that the viewer can still show it.
    fn remember(&mut self, id: ContextId, was: Content) {
        let versions = self.versions.entry(id).or_default();
        if versions.last() == Some(&was) {
            return;
        }

        versions.push(was);
        // the oldest goes rather than the newest: a rewrite somebody is asking about is nearly
        // always the last one, and a cap that dropped from that end would answer nothing
        if versions.len() > VERSIONS {
            versions.remove(0);
        }
    }

    /// What each item really costs in the next request, by item.
    ///
    /// note: not [`ContextItem::tokens`], which is what an item *holds*. An elided one holds a
    /// thousand tokens and costs the dozen its marker takes; an archived one holds whatever it
    /// holds and costs nothing. A pane that showed the held figure under a column headed `tokens`
    /// was answering a question nobody asked while the status line beside it answered the right
    /// one, and the two disagreed by exactly the elided items.
    ///
    /// note: read out of the projection rather than worked out here, because what an elided item
    /// costs is the marker the *projector* writes, in the brackets the projector chooses. A
    /// client that computed it would be keeping a second copy of a decision that is not its own.
    pub fn costs(&self) -> BTreeMap<ContextId, usize> {
        let projection = self.kernel.project();
        let counter = self.kernel.counter();
        // `included` and `messages` line up one for one under a projector that makes a message
        // per item; one that merges them has no per-item answer, and the item's own figure is a
        // better guess than a number taken from the wrong message
        let paired = projection.included.len() == projection.messages.len();

        projection
            .included
            .iter()
            .enumerate()
            .map(|(at, id)| {
                let cost = match paired {
                    true => counter.count_message(&projection.messages[at]),
                    false => self.kernel.item(*id).map(|item| item.tokens).unwrap_or(0),
                };

                (*id, cost)
            })
            .collect()
    }

    /// Every capability that matters here, and what would happen if a tool asked for it.
    ///
    /// note: The union of two lists, because either on its own is misleading. What the policy has
    /// been told about is not the whole story - a tool can need something nobody has mentioned,
    /// and that is exactly the row worth seeing, since it is the one that will stop and ask. And
    /// what the tools declare is not the whole story either: `network` is refused here and no
    /// built-in tool wants it, but a refusal you cannot see is not a policy you can trust.
    pub fn permissions(&self) -> Vec<Stance> {
        self.all_stances()
            .into_iter()
            .filter(Stance::is_decided)
            .collect()
    }

    /// How many subjects the policy will simply ask about, because nobody has told it otherwise.
    ///
    /// note: the tab does not list them - a row for a `.aws` rule nobody has thought about is not
    /// information - but it does say how many there are, because a screen showing two decisions
    /// and silently standing for sixteen answers would be a different kind of dishonest.
    pub fn undecided(&self) -> usize {
        self.all_stances()
            .iter()
            .filter(|row| !row.is_decided())
            .count()
    }

    /// Every subject this policy holds an opinion about, decided or not.
    fn all_stances(&self) -> Vec<Stance> {
        let mut rows: BTreeMap<Capability, Vec<String>> = BTreeMap::new();
        let mut sometimes: BTreeMap<Capability, Vec<String>> = BTreeMap::new();
        for (capability, _) in self.policy.stances() {
            rows.entry(capability).or_default();
        }
        for spec in self.kernel.tool_specs() {
            // a shell is judged against `network` too, when the command it was handed reaches for
            // it; the policy is the one that knows, and this is the row that has to say so
            if spec.capabilities.contains(&Capability::Shell) {
                sometimes
                    .entry(Capability::Network)
                    .or_default()
                    .push(spec.id.clone());
                rows.entry(Capability::Network).or_default();
            }
            for capability in spec.capabilities {
                rows.entry(capability).or_default().push(spec.id.clone());
            }
        }

        // the capabilities first, then the rules that are finer than any of them. note: a path
        // rule binds the three tools that are handed a path, and no others - a `shell` command
        // names its files inside a string this program does not parse, and pretending otherwise
        // would be exactly the sort of check that implies more than it delivers
        let bound: Vec<String> = self
            .kernel
            .tool_specs()
            .iter()
            .filter(|spec| {
                spec.capabilities.iter().any(|capability| {
                    matches!(
                        capability,
                        Capability::Read | Capability::Write | Capability::Edit
                    )
                })
            })
            .map(|spec| spec.id.clone())
            .collect();

        let listed = rows
            .into_iter()
            .map(|(capability, tools)| Stance {
                verdict: self.policy.stance(&Subject::Capability(capability.clone())),
                sometimes: sometimes.remove(&capability).unwrap_or_default(),
                subject: Subject::Capability(capability),
                tools,
            })
            .chain(
                self.policy
                    .paths()
                    .into_iter()
                    .map(|(pattern, verdict)| Stance {
                        subject: Subject::Path(pattern),
                        verdict,
                        tools: bound.clone(),
                        sometimes: Vec::new(),
                    }),
            );

        listed.collect()
    }

    /// What the shell can reach, in one line, or `None` if nothing here runs commands.
    pub fn confinement(&self) -> Option<String> {
        if !self.shell_is_live() {
            return None;
        }

        Some(match self.confinement.is_confined() {
            true => format!("shell: {}", self.confinement),
            false => "shell: a command can do any of these".to_owned(),
        })
    }

    /// Whether a registered tool can run commands, and the policy has not refused it outright.
    ///
    /// note: the question the permissions tab has to answer honestly. `Capability::Shell` subsumes
    /// every other capability - a command reads, writes and reaches the network - so while one is
    /// on the list and not denied, every other row is what a *tool* declares rather than what can
    /// happen, unless something is actually confining it.
    pub fn shell_is_live(&self) -> bool {
        self.policy.stance(&Subject::Capability(Capability::Shell)) != Verdict::Deny
            && self
                .kernel
                .tool_specs()
                .iter()
                .any(|spec| spec.capabilities.contains(&Capability::Shell))
    }

    /// Keys that belong to the permissions tab.
    fn permissions_key(&mut self, key: KeyEvent) {
        let rows = self.permissions();
        if rows.is_empty() {
            return;
        }
        self.chosen = self.chosen.min(rows.len() - 1);
        let subject = rows[self.chosen].subject.clone();

        let decided = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.chosen = self.chosen.saturating_sub(1);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.chosen = (self.chosen + 1).min(rows.len() - 1);
                return;
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.chosen = 0;
                return;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.chosen = rows.len() - 1;
                return;
            }
            KeyCode::Char('?') => {
                self.preview("the keys", crate::ui::HELP);
                return;
            }
            KeyCode::Char(' ') => self.policy.cycle(&subject),
            KeyCode::Char('a') => {
                self.policy.set(&subject, Verdict::Allow);
                Verdict::Allow
            }
            KeyCode::Char('n') => {
                self.policy.set(&subject, Verdict::Deny);
                Verdict::Deny
            }
            KeyCode::Char('r') | KeyCode::Backspace => {
                self.policy.set(&subject, Verdict::Ask);
                Verdict::Ask
            }
            _ => return,
        };

        // said out loud, because this is a decision about what may happen later and the tab it
        // was made on is not the one somebody will be looking at when it does
        self.say(
            Speaker::Note,
            match decided {
                Verdict::Allow => format!("`{subject}` runs without asking, from now on"),
                Verdict::Deny => format!("`{subject}` is refused, from now on"),
                Verdict::Ask => format!("`{subject}` is a question again"),
            },
        );
    }

    /// Keys that belong to the trace tab, which is a log and therefore worth reading backwards.
    fn trace_key(&mut self, key: KeyEvent) {
        // the pane draws the tail, so scrolling counts upwards from the newest line; the frame
        // clamps it to what there is
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.trace_scroll += 1,
            KeyCode::Down | KeyCode::Char('j') => {
                self.trace_scroll = self.trace_scroll.saturating_sub(1)
            }
            KeyCode::PageUp => self.trace_scroll += self.viewport.max(1),
            KeyCode::PageDown => {
                self.trace_scroll = self.trace_scroll.saturating_sub(self.viewport.max(1))
            }
            KeyCode::End | KeyCode::Char('G') => self.trace_scroll = 0,
            KeyCode::Home | KeyCode::Char('g') => self.trace_scroll = usize::MAX,
            KeyCode::Char('?') => self.preview("the keys", crate::ui::HELP),
            _ => {}
        }
    }

    /// Keys that belong to whatever is on top; returns whether one answered a question.
    async fn overlay_key(&mut self, key: KeyEvent) -> bool {
        let Some(overlay) = &mut self.overlay else {
            return false;
        };

        match overlay {
            Overlay::Text {
                pages,
                page,
                scroll,
                ..
            } => match key.code {
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll += 1,
                KeyCode::PageUp => *scroll = scroll.saturating_sub(PAGE),
                KeyCode::PageDown => *scroll += PAGE,
                // reading another face of the same thing is not leaving it. Round rather than
                // stop, so that two pages are one key apart in either direction
                KeyCode::Left | KeyCode::Right if pages.len() > 1 => {
                    let step = match key.code {
                        KeyCode::Left => pages.len() - 1,
                        _ => 1,
                    };
                    *page = (*page + step) % pages.len();
                    *scroll = 0;
                }
                // a tool is still waiting to be told whether it may run, so closing whatever was
                // being read goes back to the question rather than leaving it unanswered and
                // unreachable - which is what happened after [i] showed the exact JSON
                _ => {
                    self.overlay = match self.kernel.pending_permissions().is_empty() {
                        true => None,
                        false => Some(Overlay::Permission { scroll: 0 }),
                    }
                }
            },
            // the arguments can be longer than the box has room for - an `amend` carrying a
            // rewritten tool result is as long as the result - so the keys that scroll everything
            // else scroll them here too. They are not answers, and returning `true` says so: a
            // page read is not somebody typing, and must not put the settling window back
            Overlay::Permission { scroll } => {
                match key.code {
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(PAGE),
                    KeyCode::PageDown => *scroll += PAGE,
                    _ => return self.permission_key(key).await,
                }

                return true;
            }
        }

        false
    }

    /// Answers the question a tool is waiting on; returns whether it answered.
    ///
    /// note: a question that appears under somebody's fingers is not answered by those fingers.
    /// Its keys are letters, and a live session granted `shell` for good with the `a` of "what" -
    /// typed at the prompt, into a question that had arrived a second earlier and was never read.
    /// Letters keep going where they were aimed until the typing stops; see [`SETTLING`].
    async fn permission_key(&mut self, key: KeyEvent) -> bool {
        let Some(request) = self.kernel.pending_permissions().into_iter().next() else {
            self.overlay = None;
            return false;
        };
        if self.typing.elapsed() < SETTLING {
            self.input_key(key).await;

            return false;
        }

        let mut remembered = false;
        let grant = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Grant::Allow,
            KeyCode::Char('a') | KeyCode::Char('A') => {
                // everything the policy actually consulted, not just what the tool declared: a
                // `yes, always` to a `curl` that left `network` on `ask`, or to a `.env` that left
                // its rule on `ask`, would ask again on the very next call
                self.policy.always(&self.policy.judges(&request));
                remembered = true;
                Grant::Allow
            }
            KeyCode::Char('i') | KeyCode::Char('I') => {
                let spec = self
                    .kernel
                    .tool(&request.tool)
                    .map(|tool| serde_json::to_string_pretty(&tool.spec()).unwrap_or_default())
                    .unwrap_or_else(|| "this tool is not registered".into());
                let args = serde_json::to_string_pretty(&*request.args).unwrap_or_default();
                self.preview(
                    format!("{} · what it was asked to do", request.tool),
                    format!("{args}\n\n--- the tool ---\n{spec}"),
                );

                return true;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Grant::Deny,
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.drop_pending();
                return true;
            }
            // a key that is not one of the answers is somebody typing at the prompt underneath,
            // and that is where it goes rather than nowhere. The first key of a sentence arrives
            // after a pause, so it is not the typing this waits out, and it was being eaten one
            // character into every message; `enter` sends what is in the prompt, which waits for
            // this question the same way a message typed into a running turn does
            _ => {
                self.input_key(key).await;

                return false;
            }
        };

        // saying yes to a command that reaches for the network is permission for *that* command,
        // and the sandbox has to hear about it. Without this the call runs with the network cut
        // and fails, one keystroke after somebody was told it would run
        if grant == Grant::Allow
            && request.capabilities.contains(&Capability::Shell)
            && request
                .args
                .get("cmd")
                .and_then(|cmd| cmd.as_str())
                .is_some_and(crate::tools::reaches_the_network)
        {
            self.policy.grant_the_network(&request.call);
        }

        if let Err(e) = self.kernel.decide(request.id, grant) {
            self.say(Speaker::Error, e.to_string());
        }
        // note: `always` is a promise about what happens next, and what happens next is often
        // already in the queue. A model that asks for three commands in one answer produces three
        // questions, all of them decided before the first was shown - so answering `a` to the
        // first asked about the second one keystroke later, having just been told it would not.
        // Everything still waiting that the policy would now let through is let through
        if remembered {
            for waiting in self.kernel.pending_permissions() {
                if self.policy.verdict(&waiting) == Verdict::Allow
                    && let Err(e) = self.kernel.decide(waiting.id, Grant::Allow)
                {
                    self.say(Speaker::Error, e.to_string());
                }
            }
        }
        // the model may have asked for several things at once, and each is its own question
        if self.kernel.pending_permissions().is_empty() {
            self.overlay = None;
            // somebody driving this a transition at a time did not ask for the rest of the turn,
            // and running it here would be the harness taking the wheel back
            match self.stepping {
                true => self.say(
                    Speaker::Note,
                    "decided; /step runs the calls, /continue runs the rest of the turn",
                ),
                false => self.start_turn(),
            }
        }

        true
    }

    /// Drops every call the model is waiting on an answer for, and tells it so.
    ///
    /// note: Denying them one at a time says no to each; this says no to all of them with one
    /// reason, which is the answer when the model has gone off down the wrong path entirely.
    /// Either way the model is told - a call that simply vanished would leave it waiting.
    fn drop_pending(&mut self) {
        let dropped = self.kernel.cancel_pending_calls("dropped at the terminal");
        self.overlay = None;
        self.say(
            Speaker::Note,
            format!("{dropped} call(s) dropped; the model is told, and can try something else"),
        );

        // and then the model gets to say something about it. Answering `n` to every request ends
        // up at `start_turn`; dropping them all left the kernel idle with the refusals recorded
        // and nobody driving, so the session simply stopped until somebody typed `/continue`
        match self.stepping {
            true => self.say(Speaker::Note, "/step or /continue when you are ready"),
            false => self.start_turn(),
        }
    }

    /// Asks the running turn to stop at the next opportunity.
    fn interrupt(&mut self) {
        self.interrupting = true;
        self.kernel.interrupt();
    }

    /// Puts something long on the screen.
    fn preview(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.preview_pages(
            title,
            vec![Page {
                name: String::new(),
                body: body.into(),
            }],
            0,
        );
    }

    /// The same, for something with more than one face; `at` is the one to open on.
    fn preview_pages(&mut self, title: impl Into<String>, pages: Vec<Page>, at: usize) {
        self.overlay = Some(Overlay::Text {
            title: title.into(),
            page: at.min(pages.len().saturating_sub(1)),
            pages,
            scroll: 0,
        });
    }

    /// Moves the transcript, and stops following the bottom if it moved up.
    fn scroll_by(&mut self, lines: isize) {
        let bottom = self.rendered.saturating_sub(self.viewport);
        let at = (self.scroll as isize + lines).clamp(0, bottom as isize) as usize;

        self.scroll = at;
        self.follow = at >= bottom;
    }

    // ------------------------------------------------------------------------------- the prompt

    /// Sends a message, or runs a command.
    async fn submit(&mut self, line: &str) {
        if let Some(command) = line.strip_prefix('/') {
            self.command(command).await;
            return;
        }

        self.say(Speaker::User, line);
        // note: a message sent while a turn is running waits for the end of it rather than going
        // into the context there and then. Both of the obvious alternatives are worse. Pushed
        // immediately, it lands *before* the answer the model is still writing - so the next
        // request ends with a model turn, which Google refuses outright with `requests ending with
        // a model turn are not supported`, and which every other provider answers by replying to
        // itself. Pushed mid-tool-loop it is worse still: it lands between an assistant's call and
        // that call's result, which is a shape most of these APIs reject. What it costs is that a
        // message typed to steer a turn does not reach it - it is answered after, not during
        // ... and the same holds while a question is open: the turn is paused rather than over,
        // and the call it is waiting on still has a result to come
        if self.busy || !self.kernel.pending_permissions().is_empty() {
            self.typed_ahead = Some(line.to_owned());
            // said out loud, because until the turn ends this is the one thing on the screen that
            // the context does not have: a session saved now would not contain it
            self.say(
                Speaker::Note,
                "this goes in when the turn stops, and gets a turn of its own",
            );
            return;
        }

        // this is all "sending a message" is: one context item, and then the loop
        self.kernel.push(ContextItem::user(line));
        self.start_turn();
    }

    /// Runs one slash command.
    async fn command(&mut self, line: &str) {
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();

        match command {
            "quit" | "exit" | "q" => self.quit = true,
            "help" | "?" => self.preview("the keys", crate::ui::HELP),
            "continue" => self.start_turn(),
            // with a message, because otherwise the only way to reach the first transition is to
            // send one - which runs the whole turn, and there is nothing left to step through
            "step" => {
                if !rest.is_empty() {
                    self.say(Speaker::User, rest);
                    self.kernel.push(ContextItem::user(rest));
                }
                self.start_step();
            }
            "request" => self.preview("the next request", request_preview(&self.kernel)),
            "payload" => {
                let body = match self.kernel.preview_payload() {
                    Ok(Some(payload)) => pretty(&payload),
                    Ok(None) => "this provider cannot render a request without sending it".into(),
                    Err(e) => nothing_to_send(&self.kernel, &e.to_string()),
                };
                self.preview("the payload, as it would go out", body);
            }
            "raw" => {
                let body = match self.kernel.last_response().and_then(|r| r.raw.clone()) {
                    Some(raw) => pretty(&raw),
                    None => "nothing has been answered yet".into(),
                };
                self.preview("the provider's last answer, verbatim", body);
            }
            // the registry is live rather than fixed at startup, and taking a tool out of it is
            // the plainest demonstration of that: the next request simply does not offer it
            "tools" if rest.starts_with("drop ") => {
                let id = rest.strip_prefix("drop ").unwrap_or_default().trim();
                match self.kernel.remove_tool(id) {
                    Some(_) => self.say(
                        Speaker::Note,
                        format!(
                            "`{id}` is no longer offered; the next request will not mention it"
                        ),
                    ),
                    None => self.say(Speaker::Error, format!("there is no tool called `{id}`")),
                }
            }
            "tools" => {
                let body = self
                    .kernel
                    .tool_specs()
                    .into_iter()
                    .map(|spec| {
                        let capabilities: Vec<_> =
                            spec.capabilities.iter().map(|c| c.to_string()).collect();
                        format!(
                            "{:<24}{}\n{:<24}[{}]\n",
                            spec.id,
                            spec.description,
                            "",
                            capabilities.join(", ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.preview("what the model is offered", body);
            }
            "budget" => self.budget(),
            "seams" => self.seams(),
            "introspect" => self.introspect(),
            // it used to print a line naming the allowed capabilities. The tab is that line, plus
            // the ones that are refused, plus the ones nobody has decided about yet, plus what
            // each of them covers - and every row can be changed where it is read
            "policy" | "permissions" => self.open(Tab::Permissions),
            "prune" | "keep" | "restore" => self.by_selector(command, rest),
            "model" => {
                if !rest.is_empty() {
                    let (provider, model) = (self.provider.clone(), rest.to_owned());
                    self.say(Speaker::Note, format!("switching to {model}"));
                    // the new model has a context limit of its own, and finding it out is a round
                    // trip; the screen should not stop for it
                    tokio::spawn(async move { provider.set_model(model).await });

                    return;
                }
                match self.kernel.model_info() {
                    Some(info) => self.say(
                        Speaker::Note,
                        format!(
                            "{} at {} ({}), {} tokens of context",
                            info.model,
                            self.provider.endpoint(),
                            info.provider,
                            info.context_limit
                                .map(thousands)
                                .unwrap_or_else(|| "an unknown number of".into()),
                        ),
                    ),
                    None => self.say(Speaker::Error, "there is no provider"),
                }
            }
            // the ids an endpoint serves are its own - `google/gemini-3.5-flash` at one address
            // and `gemini-3.5-flash` at another - so after `/provider` there was no way to find
            // out what to hand `/model` except to guess it. The provider has always fetched this
            // list, to say when a model is not on it; this is the same call with the answer shown
            // rather than checked.
            //
            // note: awaited here rather than spawned, unlike the switches below. Those are told to
            // go and do something and the screen carries on; this one *is* the answer, and a
            // person who asked for a list is waiting for it either way
            "models" => {
                let provider = self.provider.clone();
                let listed = provider.models().await;
                if listed.is_empty() {
                    self.say(
                        Speaker::Error,
                        format!("{} lists no models", provider.host()),
                    );
                    return;
                }

                let filter = rest.trim().to_lowercase();
                let shown: Vec<&String> = listed
                    .iter()
                    .filter(|name| filter.is_empty() || name.to_lowercase().contains(&filter))
                    .collect();
                if shown.is_empty() {
                    self.say(
                        Speaker::Note,
                        format!("none of the {} listed match `{filter}`", listed.len()),
                    );
                    return;
                }

                // the one in use is marked where it stands rather than pulled to the top, so the
                // list keeps the order the endpoint gave it
                let current = self.kernel.model_info().map(|info| info.model);
                let body = shown
                    .iter()
                    .map(|name| {
                        let mark = match &current {
                            Some(model) if crate::provider::same_model(name, model) => "▸",
                            _ => " ",
                        };
                        format!("{mark} {name}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                // no address in the title: it is one `/provider` away, and the room it costs is
                // the room the line below needs to say what to do with any of this
                let title = match filter.is_empty() {
                    true => format!(" {} models", shown.len()),
                    false => format!(" {} of {} matching `{filter}`", shown.len(), listed.len()),
                };
                self.preview(format!("{title} · /model ID switches "), body);
            }
            // the other half of `/model`: the same model name means a different model at a
            // different address, and comparing what is hosted with what is on this machine is two
            // endpoints rather than two names
            "provider" | "endpoint" => {
                if rest.is_empty() {
                    let endpoint = self.provider.endpoint();
                    self.say(Speaker::Note, format!("requests go to {endpoint}"));
                    return;
                }
                // `URL MODEL`, because a model belongs to the address that serves it: switching
                // one and keeping the other is how a session ends up asking the ollama on this
                // machine for `gemini-3.6-flash`. Given no model the old name is kept, and the new
                // endpoint is asked whether it has one by that name
                let (url, model) = match rest.split_once(char::is_whitespace) {
                    Some((url, model)) => (url.to_owned(), Some(model.trim().to_owned())),
                    None => (rest.to_owned(), None),
                };
                let provider = self.provider.clone();
                self.say(
                    Speaker::Note,
                    match &model {
                        Some(model) => format!("{model} at {url}, from now on"),
                        None => format!(
                            "requests now go to {url}, still asking for {}; the key is the one \
                             this started with",
                            self.kernel
                                .model_info()
                                .map(|info| info.model)
                                .unwrap_or_default()
                        ),
                    },
                );
                // the new endpoint has a context limit of its own, and a list of what it serves;
                // both are round trips and the screen should not stop for them
                tokio::spawn(async move { provider.set_endpoint(url, model).await });
            }
            "params" => {
                if let Some((key, value)) = rest.split_once(' ') {
                    match serde_json::from_str(value.trim()) {
                        Ok(value) => {
                            let mut params = self.kernel.params();
                            params.insert(key.trim().to_owned(), value);
                            self.kernel.set_params(params);
                        }
                        Err(e) => {
                            self.say(Speaker::Error, format!("{key} needs a JSON value: {e}"));
                            return;
                        }
                    }
                }
                let params = serde_json::to_string(&self.kernel.params()).unwrap_or_default();
                self.say(Speaker::Note, format!("parameters: {params}"));
            }
            "save" => self.save(rest),
            // note: not aliased `/resume`. `--resume` at startup is the *other* answer to the
            // same file - a fresh session built around the snapshot - and two things a keystroke
            // apart that differ in what happens to the context you already have is a trap
            "load" => self.load(rest),
            other => self.say(
                Speaker::Error,
                format!("there is no `/{other}`; F1 lists what there is"),
            ),
        }
    }

    /// Offers the model the two tools that read and change its own context, or stops offering
    /// them.
    ///
    /// note: `add_tool` and `remove_tool`, like `/tools drop` - the registry is live and this is
    /// the plainest thing to demonstrate that with. What it also has to move is the handle the
    /// tools reach the kernel through, because that is the piece with an end to it: taking them
    /// away drops it, and with it whatever `amend` had been remembering - which items it pinned,
    /// and the changes it could still walk back. That is the right answer rather than a
    /// shortcoming. The tools that come back are new ones, and they have not done anything yet.
    fn introspect(&mut self) {
        match self.introspect.take() {
            Some(_) => {
                self.kernel.remove_tool("introspect");
                self.kernel.remove_tool("amend");
                self.say(
                    Speaker::Note,
                    "`introspect` and `amend` are no longer offered; the next request will not mention \
                     them",
                );
            }
            None => {
                self.introspect = Some(crate::introspect::install(&self.kernel));
                self.say(
                    Speaker::Note,
                    "`introspect` and `amend` go into the next request: the model can now read its own \
                     context, preview what it would say, ask a fork of itself, prune what it is \
                     carrying and walk its own changes back. It cannot touch anything you pinned",
                );
            }
        }
    }

    /// Prunes, pins or restores whatever a selector names.
    ///
    /// note: With nothing to act on, this shows the language rather than reporting that the empty
    /// string is not a selector. The grammar has ten forms and the terminal used to advertise two
    /// of them in a help line, so the only way to find the rest was the crate documentation.
    fn by_selector(&mut self, command: &str, input: &str) {
        if input.is_empty() {
            self.preview(
                format!("/{command} takes any of these"),
                crate::ui::SELECTORS,
            );
            return;
        }

        let ids = match input.parse::<Selector>() {
            Ok(selector) => selector.matches(&self.kernel.items()),
            Err(e) => {
                self.say(Speaker::Error, e.to_string());
                return;
            }
        };
        if ids.is_empty() {
            self.say(Speaker::Note, format!("nothing matches `{input}`"));
            return;
        }

        let (state, note) = match command {
            "prune" => (ContextState::Excluded, Some(format!("pruned by `{input}`"))),
            "keep" => (ContextState::Pinned, None),
            _ => (ContextState::Active, None),
        };
        let changed = self.kernel.set_state(ids, state, note);
        self.say(
            Speaker::Note,
            format!("{} item(s) are now {state}", changed.len()),
        );
    }

    /// What the next request is estimated to cost, beside what the last one actually did.
    ///
    /// note: The status line can only afford one number, and it shows the estimate - which is
    /// produced by a counter that does not have the model's tokenizer and is therefore wrong.
    /// This is where the two numbers sit side by side, along with the correction the counter has
    /// worked out for itself from the difference. A budget nobody can check is a decoration.
    /// What is plugged into each of the runtime's six seams, right now.
    ///
    /// note: The crate's headline claim is six replaceable parts, and until this there was no way
    /// to see any of them from here - `Kernel::policy`, `projector`, `counter` and `compactor`
    /// hand back trait objects, and a trait object you cannot name is not worth asking for. Each
    /// of those traits now names itself, so this is the claim, checked against the kernel rather
    /// than restated from what this program set up at startup.
    fn seams(&mut self) {
        let kernel = &self.kernel;
        let tools = kernel.tool_specs();
        let body = format!(
            "provider     {}\n\
             tools        {} offered: {}\n\
             policy       {}\n\
             projector    {}\n\
             counter      {}\n\
             compactor    {}\n\
             \n\
             Every one of these is a trait object the kernel holds, and every one of them can be\n\
             replaced while a session is running. Nothing here is the terminal's own bookkeeping:\n\
             it is what the kernel answers when asked.",
            match kernel.model_info() {
                Some(info) => format!(
                    "{} at {} ({})",
                    info.model,
                    self.provider.endpoint(),
                    info.provider
                ),
                None => "none set".to_owned(),
            },
            tools.len(),
            match tools.is_empty() {
                true => "-".to_owned(),
                false => tools
                    .iter()
                    .map(|spec| spec.id.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            kernel.policy().name(),
            kernel.projector().name(),
            kernel.counter().name(),
            match kernel.compactor() {
                Some(compactor) => compactor.name().to_owned(),
                // `--compact 1` leaves none installed, and "nothing will be dropped" is a fact
                // worth being able to check rather than infer from a flag
                None => "none: nothing is ever dropped to make room".to_owned(),
            },
        );

        self.preview("what is plugged into the runtime", body);
    }

    fn budget(&mut self) {
        let budget = self.kernel.budget();
        let withheld = self
            .kernel
            .with_context(|context| context.tokens_withheld());
        let out = self
            .kernel
            .items()
            .iter()
            .filter(|item| !item.is_projected())
            .count();

        let mut lines = vec![format!(
            "the next request: ~{} tokens, {} of context and {} of tool definitions",
            thousands(budget.used()),
            thousands(budget.context_tokens),
            thousands(budget.tool_tokens),
        )];
        lines.push(match budget.limit {
            Some(limit) => format!(
                "the limit: {}, which the next request would fill {:.1}% of",
                thousands(limit),
                budget.fraction_used().unwrap_or_default() * 100.0
            ),
            None => "the limit: unknown, so there is nothing to measure against".to_owned(),
        });
        if withheld != 0 {
            lines.push(format!(
                "held back: {} tokens in {out} items the projector is not sending",
                thousands(withheld)
            ));
        }

        match budget.reported.and_then(|usage| usage.input_tokens) {
            Some(reported) => lines.push(format!(
                "the last request really cost {}, as the provider counted it",
                thousands(reported as usize)
            )),
            None => lines.push("nothing has been sent yet, so there is no real figure".to_owned()),
        }

        // note: whichever counter is installed is asked what it has learned, rather than one this
        // program kept a typed handle to; a counter that learns nothing says so by having nothing
        // to report, and is a sentence rather than a missing line
        //
        // note: small requests teach it nothing and are not counted here, which is why this can
        // say "2" in a session that has sent six things
        lines.push(match self.kernel.counter().calibration() {
            None => "the counter installed here does not correct itself, so every figure above \
                     is whatever it estimates and nothing has told it otherwise"
                .to_owned(),
            Some(learned) if learned.observations == 0 => {
                "the counter has not been corrected yet: it is guessing at four bytes a token, \
                 and no request so far has been big enough to learn anything from"
                    .to_owned()
            }
            Some(learned) => format!(
                "the counter has learned from {} request(s) and scaled itself by {:.3}: its own \
                 guesses came to {} tokens where the provider counted {}, so it was reading {:.1}% \
                 {}",
                learned.observations,
                learned.scale,
                thousands(learned.estimated as usize),
                thousands(learned.reported as usize),
                ((learned.scale - 1.0) * 100.0).abs(),
                match learned.scale < 1.0 {
                    true => "high",
                    false => "low",
                },
            ),
        });

        self.preview("the budget", lines.join("\n\n"));
    }

    /// Brings a saved session's context back, setting aside whatever is in this one.
    ///
    /// note: not a swap of the kernel. `Kernel::resume` is a constructor, and everything plugged
    /// into a running one - the provider, the policy, the tools, the two introspection tools'
    /// handle, the subscription this screen is drawing from - is wired to *this* kernel; a second
    /// one built here would arrive with none of it. `kamchatka -r` is the swap, and it is a
    /// restart because that is what a swap is.
    ///
    /// note: so this is a context operation, and it follows the rule every other one here does:
    /// nothing is destroyed. What was in the context is archived rather than dropped, keeps its
    /// numbers and its contents, and `u` twice puts the whole thing back - once for the items
    /// that came in and once for the ones that were set aside. The loaded items are new items
    /// and are numbered as such: they are what that session said, in this session.
    fn load(&mut self, path: &str) {
        if self.busy || !self.kernel.pending_permissions().is_empty() {
            self.say(
                Speaker::Error,
                "not while a turn is running or a call is waiting to be answered",
            );
            return;
        }

        let file = match path {
            "" => "session.json".to_owned(),
            given if given.ends_with(".json") => given.to_owned(),
            given => format!("{given}.json"),
        };
        let snapshot: nachalnik::Snapshot = match std::fs::read(&file)
            .map_err(|e| format!("could not read {file}: {e}"))
            .and_then(|bytes| {
                serde_json::from_slice(&bytes).map_err(|e| format!("{file} is not a session: {e}"))
            }) {
            Ok(snapshot) => snapshot,
            Err(e) => return self.say(Speaker::Error, e),
        };
        if snapshot.items.is_empty() {
            self.say(Speaker::Error, format!("{file} holds no context"));
            return;
        }

        // set aside first, so that the calls in the loaded turns are the only ones the projector
        // can pair a loaded result with. Archived items are not projected, so an old copy of the
        // same conversation cannot answer the new one's calls
        //
        // note: except what is pinned. A pin is the person saying this stays, and `--system` is
        // pinned - a load that quietly archived the system instruction would be answering a
        // question about a saved conversation by revoking the one thing the session was told to
        // hold on to
        let standing: Vec<_> = self
            .kernel
            .items()
            .iter()
            .filter(|item| item.is_projected() && item.state != ContextState::Pinned)
            .map(|item| item.id)
            .collect();
        self.kernel.set_state(
            standing.iter().copied(),
            ContextState::Archived,
            Some(format!("set aside for the session loaded from {file}")),
        );

        let ids = self.kernel.push_all(snapshot.items);
        self.kernel.set_params(snapshot.params);
        // what the counter had learned, which is the one piece of a seam's state a snapshot
        // carries; without it the next few requests would be spent relearning what this file
        // already knows
        if let Some(calibration) = snapshot.calibration {
            self.kernel.counter().recalibrate(calibration);
        }

        let loaded: Vec<_> = ids.iter().filter_map(|id| self.kernel.item(*id)).collect();
        self.retell(&loaded);
        self.say(
            Speaker::Note,
            format!(
                "loaded {} items from session `{}` ({file}); {} of your own {} archived, \
                 anything pinned stayed, and `u` twice puts the rest back",
                loaded.len(),
                snapshot.session,
                standing.len(),
                match standing.len() {
                    1 => "was",
                    _ => "were",
                }
            ),
        );
    }

    /// Writes the session log and a snapshot that can be resumed from, at a path somebody gave.
    ///
    /// note: Two files, because they answer different questions: the log says what happened, and
    /// the snapshot is what can be picked back up. An event names an item rather than carrying
    /// it, so the log alone cannot rebuild a context - keeping only one of them means losing
    /// either the story or the state.
    ///
    /// note: the snapshot is what `/load` reads back into a running session and what
    /// `kamchatka -r` starts from.
    fn save(&mut self, path: &str) {
        // both extensions, so that `/save notes.jsonl` does not write `notes.jsonl.jsonl`
        let stem = match path {
            "" => "session",
            given => given
                .strip_suffix(".jsonl")
                .or_else(|| given.strip_suffix(".json"))
                .unwrap_or(given),
        };
        let (log, state) = (format!("{stem}.jsonl"), format!("{stem}.json"));

        // said rather than asked about: writing the same session again is the ordinary case and
        // a prompt every time would be noise, but a typo landing on somebody else's file should
        // not pass in silence
        let replacing: Vec<&str> = [log.as_str(), state.as_str()]
            .into_iter()
            .filter(|path| std::path::Path::new(path).exists())
            .collect();

        let records: Vec<String> = self
            .kernel
            .history()
            .iter()
            .filter_map(|record| serde_json::to_string(record).ok())
            .collect();
        // named, because "No such file or directory" on its own leaves somebody guessing which
        // one; `-r` says which file it could not read and this should match it
        let written = std::fs::write(&log, records.join("\n") + "\n")
            .map_err(|e| format!("could not write {log}: {e}"))
            .and_then(|()| {
                let snapshot = serde_json::to_vec_pretty(&self.kernel.snapshot())
                    .map_err(|e| format!("could not render the session: {e}"))?;
                std::fs::write(&state, snapshot)
                    .map_err(|e| format!("could not write {state}: {e}"))
            });

        match written {
            Ok(()) => {
                if !replacing.is_empty() {
                    self.say(
                        Speaker::Note,
                        format!("replaced {}", replacing.join(" and ")),
                    );
                }
                self.say(
                    Speaker::Note,
                    format!(
                        "{} records in {log}, and a session in {state} (`/load {state}` brings \
                         it back here, `kamchatka -r {state}` starts a session from it)",
                        records.len()
                    ),
                );
            }
            Err(e) => self.say(Speaker::Error, e),
        }
    }
}

// ------------------------------------------------------------------------------------ helpers

/// An event's name, and one line of whatever else it has to say.
fn trace_line(event: &Event) -> (String, String) {
    let name = event.name();

    let detail = match event {
        Event::StateChanged { from, to } => format!("{from} → {to}"),
        Event::ContextAdded {
            id, label, tokens, ..
        } => format!("[{id}] {label}, {tokens} tokens"),
        Event::ContextChanged { id, from, to, .. } => format!("[{id}] {from} → {to}"),
        Event::ContextRecounted {
            tokens_before,
            tokens_after,
        } => format!("{tokens_before} → {tokens_after} tokens"),
        Event::ModelRequested {
            messages,
            tools,
            tokens,
            ..
        } => format!("{messages} messages, {tools} tools, ~{tokens} tokens"),
        Event::ModelFinished { stop, usage, .. } => match usage {
            Some(usage) => format!(
                "{stop:?}, {} in / {} out (reported)",
                usage.input_tokens.unwrap_or(0),
                usage.output_tokens.unwrap_or(0)
            ),
            None => format!("{stop:?}"),
        },
        Event::ToolRequested { tool, .. } | Event::ToolStarted { tool, .. } => tool.clone(),
        Event::ToolFinished { tool, tokens, .. } => format!("{tool}, {tokens} tokens"),
        Event::PermissionRequested { request } => format!("{} ({})", request.tool, request.id),
        Event::PermissionDecided {
            tool,
            grant,
            source,
            ..
        } => format!(
            "{tool}: {grant}, {}",
            match source {
                GrantSource::Policy => "by the policy in force",
                GrantSource::User => "answered when it was asked about",
                GrantSource::Cancellation => "the calls were dropped",
                _ => "by something else",
            }
        ),
        Event::Compacted { report } => format!(
            "{} out, {} → {} tokens",
            report.removed.len(),
            report.tokens_before,
            report.tokens_after
        ),
        Event::ModelFailed { error } | Event::StepFailed { error } => one_line(error),
        // note: everything below here used to fall through to the catch-all and print its own
        // name against an empty line. Each of them carries something worth reading, and a log
        // that names an event and then says nothing about it is the shape of a log nobody opens
        Event::SessionStarted { session } => format!("session {session}"),
        Event::SessionResumed {
            session,
            items,
            tokens,
        } => format!("session {session}: {items} items, ~{tokens} tokens"),
        Event::SessionFinished => "nothing more will be recorded".to_owned(),
        Event::Interrupted => "stopped; whatever had arrived is kept".to_owned(),
        // the one event that carries content, because it is the only operation that overwrites
        // something - so the first line of what went is worth the room
        Event::ContextReplaced {
            id,
            tokens_before,
            tokens_after,
            was,
        } => format!(
            "[{id}] {tokens_before} → {tokens_after} tokens; it said: {}",
            one_line(&was.to_text())
        ),
        Event::ContextUndone {
            items,
            removed,
            changed,
        } => format!(
            "{items} items now; {} taken back out, {} put back as they were",
            removed.len(),
            changed.len()
        ),
        Event::ContextRedone {
            items,
            restored,
            changed,
        } => format!(
            "{items} items now; {} back in, {} changed again",
            restored.len(),
            changed.len()
        ),
        Event::ContextAnnotated { id, meta } => format!("[{id}] {}", one_line(&meta.to_string())),
        Event::ModelChanged { from, to } => format!(
            "{} → {}",
            from.as_ref().map(|i| i.model.as_str()).unwrap_or("none"),
            to.as_ref().map(|i| i.model.as_str()).unwrap_or("none")
        ),
        Event::ModelParamsChanged { params } => match params.is_empty() {
            true => "none; the provider's own defaults".to_owned(),
            false => one_line(&serde_json::to_string(params).unwrap_or_default()),
        },
        // not the payload: `/payload` prints the whole of it, and a log line that tried would
        // bury every other line in the pane
        Event::ModelPayload { payload } => format!(
            "{} bytes, rendered by the provider; /payload prints it",
            payload.to_string().len()
        ),
        Event::ToolUnknown { tool, .. } => {
            format!("`{tool}` was asked for and is not registered")
        }
        // a provider that does this is worth knowing about, which is why the kernel announces it
        Event::ToolCallRepaired { call, was, reason } => match was.is_empty() {
            true => format!("gave a call the identifier `{call}`: {reason}"),
            false => format!("`{was}` → `{call}`: {reason}"),
        },
        // the names, not the count: `4 tools` is a number somebody has to go and look up, and
        // this line exists because what the model is offered changed
        Event::ToolsChanged { tools } => match tools.is_empty() {
            true => "none; the model is offered no tools at all".to_owned(),
            false => format!("{} offered: {}", tools.len(), tools.join(", ")),
        },
        // a seam being swapped names what went out and what came in; the trace is where somebody
        // reading a session finds out that the thing projecting its requests changed half way
        Event::PolicyChanged { from, to }
        | Event::ProjectorChanged { from, to }
        | Event::CounterChanged { from, to } => format!("{} → {}", short(from), short(to)),
        Event::CompactorChanged { from, to } => format!(
            "{} → {}",
            from.as_deref().map(short).unwrap_or("none"),
            to.as_deref()
                .map(short)
                .unwrap_or("none, so nothing is dropped"),
        ),
        _ => String::new(),
    };

    (name.to_owned(), detail)
}

/// The request the kernel would send, or what stopped it building one.
fn request_preview(kernel: &Kernel) -> String {
    // "why is that not in there?" is the question somebody opens this to answer, and the JSON on
    // its own can only say what *is* in there. The projection knows what it left out and what it
    // had to change to keep the request valid, so both go above it - and above the reason there
    // is no request at all, which is where they are worth most
    let projection = kernel.project();
    let mut header = String::new();
    for left_out in &projection.skipped {
        header.push_str(&format!(
            "  [{}] left out: {}\n",
            left_out.id, left_out.reason
        ));
    }
    for repair in &projection.repairs {
        header.push_str(&format!("  repaired: {repair}\n"));
    }

    let request = match kernel.preview_request() {
        Ok(request) => serde_json::to_string_pretty(&request)
            .unwrap_or_else(|e| format!("it will not serialize: {e}")),
        Err(e) => nothing_to_send(kernel, &e.to_string()),
    };
    if header.is_empty() {
        return request;
    }

    format!(
        "{} item(s) in, {} out:\n{header}\n{request}",
        projection.included.len(),
        projection.skipped.len()
    )
}

/// Why there is no request to show, in a sentence somebody can do something about.
///
/// note: `the context projects to an empty request` is the runtime's own sentence and it is
/// accurate - `step` refuses to send a request with no messages in it, and it says so in the
/// vocabulary of the thing that refused. It is the wrong answer to "what would go next?" asked
/// in a session nobody has typed into yet, where what happened is that nothing has been said and
/// the projector is working perfectly. And when the context is *not* empty, this is the moment
/// the list of what was left out is worth most - which is exactly when it used to be thrown away,
/// because the error returned before the header was built.
fn nothing_to_send(kernel: &Kernel, why: &str) -> String {
    match kernel.items().len() {
        0 => "nothing yet: there is nothing in the context to send. Whatever you type next goes \
              in as an item, and this is where you will see what it turns into - as will \
              --system, --file, -r and /load, which put things in before you type anything."
            .to_owned(),
        items => format!(
            "{why}: not one of the {items} item(s) it holds is going. The list above says which \
             and why; `space` on the context tab puts one back."
        ),
    }
}

/// The last part of a type's path, which is the part somebody reads.
///
/// note: a seam names itself with `std::any::type_name`, so what arrives here is
/// `kamchatka::tools::Trim` and the column it goes in is thirty characters wide.
fn short(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// JSON, indented.
fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("it will not serialize: {e}"))
}

/// Exactly what one item puts into the next request, in the projector's own words.
///
/// note: the whole context is projected rather than the item on its own, because an item's
/// message is not always a function of the item. A tool result whose call is missing is repaired
/// away, and an item projected alone has no call anywhere - so a lone projection would report
/// "the model gets nothing" about a result the model is about to read.
///
/// note: `included` and `messages` line up one for one under a projector that makes one message
/// per item, which is the dialect this program speaks. One that merges them - and the `Projector`
/// documentation offers exactly that as an example - has no per-item answer to give, and saying
/// so is better than pointing confidently at the wrong message.
fn projected(projection: &Projection, id: ContextId) -> String {
    if let Some(left_out) = projection.skipped.iter().find(|item| item.id == id) {
        return format!(
            "nothing: this item is not in the request.\n\n  {}",
            left_out.reason
        );
    }
    let Some(at) = projection.included.iter().position(|other| *other == id) else {
        return "nothing: the projector neither included this item nor said why.".to_owned();
    };
    if projection.included.len() != projection.messages.len() {
        return format!(
            "{} item(s) became {} message(s), so no one of them is this item's alone. \
             ctrl+p shows the whole request.",
            projection.included.len(),
            projection.messages.len()
        );
    }

    // what the projector had to change about this item to keep the request valid: a dropped call,
    // an ordered turn flattened into slots. It is the answer to "why does this not look like what
    // I am reading on the other page?", and it is only ever visible on ctrl+p otherwise
    let mine: Vec<&str> = projection
        .repairs
        .iter()
        .filter(|repair| repair.contains(&format!("item {id}")))
        .map(String::as_str)
        .collect();
    let header = match mine.is_empty() {
        true => String::new(),
        false => format!("repaired: {}\n\n", mine.join("\nrepaired: ")),
    };

    format!("{header}{}", as_sent(&projection.messages[at]))
}

/// A projected message, laid out for reading.
///
/// note: not `to_string_pretty`. The JSON of a message writes every newline in its content out as
/// `\n` on one enormous line, and the content is the whole of what this page is for - the same
/// reason a permission question does not show somebody the JSON of what a tool is about to run.
/// `ctrl+p` is still the byte-for-byte view, of this and of everything around it.
fn as_sent(message: &nachalnik::Message) -> String {
    let mut out = format!("role: {}", message.role);
    if let Some(name) = &message.name {
        out.push_str(&format!("\nanswers: {name}"));
    }
    out.push_str("\n\n");

    match &message.content {
        Some(content) => out.push_str(&whole(content)),
        None => out.push_str("(no content)"),
    }
    if let Some(reasoning) = &message.reasoning {
        out.push_str(&format!("\n\nreasoning:\n{}", whole(reasoning)));
    }
    for call in &message.tool_calls {
        out.push_str(&format!("\n\n{}({})", call.tool, call.args));
    }

    out
}

/// The whole of what an item holds, including what its kind carries beside its content.
///
/// note: a turn recorded in the conventional three slots keeps its calls and its reasoning in the
/// kind rather than in the content, so reading the content alone showed an empty box for a turn
/// that was nothing but tool calls - which is most of them. One recorded as ordered blocks has
/// all three in the content already, and `whole` lays those out in the order they were produced.
fn stored(item: &ContextItem) -> String {
    let mut out = whole(&item.content);
    if let ContextKind::AssistantMessage {
        tool_calls,
        reasoning,
    } = &item.kind
    {
        if let Some(reasoning) = reasoning {
            out = format!("reasoning:\n{}\n\n{out}", whole(reasoning));
        }
        for call in tool_calls {
            out.push_str(&format!("\n\n{}({})", call.tool, call.args));
        }
    }

    out.trim().to_owned()
}

/// The whole of what an item says, including the parts `to_text` leaves out.
///
/// note: for most items this is the content and nothing else. For a turn a provider recorded in
/// the order it was produced, `to_text` is only the *text* blocks - which is right on the wire and
/// wrong on this screen, where the whole point is to be shown what the item really holds. The
/// thinking and the calls are read out where they happened, because between two calls is where
/// the thinking that led to the second one belongs.
fn whole(content: &Content) -> String {
    let Some(blocks) = content.as_blocks() else {
        return content.to_text().into_owned();
    };

    blocks
        .iter()
        .map(|block| {
            let said = match block {
                Block::Call(call) => format!("{}({})", call.tool, call.args),
                _ => block
                    .part()
                    .map(|part| part.content.to_text().into_owned())
                    .unwrap_or_default(),
            };
            match block {
                Block::Text(_) => said,
                _ => format!("{}:\n{said}", block.name()),
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The first line of something, shortened.
fn one_line(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    match first.chars().count() > 96 {
        true => format!("{}…", first.chars().take(95).collect::<String>()),
        false => first.to_owned(),
    }
}

/// The first few lines of something, with a note if there were more.
fn head(text: &str, lines: usize) -> String {
    let mut kept: Vec<&str> = text.lines().take(lines).collect();
    let total = text.lines().count();
    if total > lines {
        kept.push("…");
    }

    kept.join("\n")
}
