//! Everything the terminal knows: what is on the screen, what the keys do, and what to make of
//! the events the kernel broadcasts.
//!
//! note: The kernel is driven from a task of its own, and this loop never blocks on it. What
//! arrives here is [`Event`]s - the same ones the session log is made of - so the screen is a
//! rendering of the record rather than a second account of it. When a turn stops for a decision,
//! the task ends and hands control back; nothing is waiting on a channel for an answer.

use std::{collections::VecDeque, sync::Arc};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nachalnik::{
    BytesPerToken, Calibrating, ContextItem, ContextKind, ContextState, Delta, Event, Grant,
    Kernel, State, selectors::Selector,
};
use ratatui_textarea::{CursorMove, TextArea};
use tokio::sync::mpsc::UnboundedSender;

use crate::{provider::OpenAiCompatible, tools::Careful, ui::thousands};

/// How many trace lines are kept; the session log is the one that keeps everything.
const TRACE_DEPTH: usize = 400;

/// How much of a still-running tool's output the transcript holds on to.
const LIVE_OUTPUT: usize = 8_000;

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
}

impl Tab {
    /// The tabs, in the order they are shown.
    pub const ALL: [Self; 3] = [Self::Chat, Self::Context, Self::Trace];

    /// What it is called on the tab strip.
    pub fn name(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Context => "context",
            Self::Trace => "trace",
        }
    }
}

/// One line of the trace pane: an event's name, and what it says for itself.
pub struct Traced {
    /// The dotted name, e.g. `model.requested`; empty for a continuation line.
    pub name: String,
    /// The rest of it.
    pub detail: String,
}

/// What is being shown over the top of everything else.
pub enum Overlay {
    /// A tool wants to run, and somebody has to say so.
    Permission,
    /// Something long enough to need its own screen.
    Text {
        /// What it is.
        title: String,
        /// The thing itself.
        body: String,
        /// How far down it is scrolled.
        scroll: usize,
    },
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
    /// It could not be finished.
    Failed(String),
}

/// The whole of the terminal's state.
pub struct App {
    /// The runtime.
    pub kernel: Kernel,
    /// The policy, which the permission overlay teaches.
    pub policy: Arc<Careful>,
    /// The provider, for switching models.
    pub provider: Arc<OpenAiCompatible>,
    /// The counter, kept concrete so that what it has learned can be shown.
    pub counter: Arc<Calibrating<BytesPerToken>>,
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
    /// Whether the last stop was asked for rather than reached.
    interrupting: bool,
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
        provider: Arc<OpenAiCompatible>,
        outcomes: UnboundedSender<Outcome>,
    ) -> Self {
        // the counter the kernel is using, as the type it actually is: `Kernel::counter` hands
        // back a `dyn TokenCounter`, and what it has learned is not on that trait
        let counter = Arc::new(Calibrating::new(BytesPerToken::default()));
        kernel.set_counter(counter.clone());

        let mut input = TextArea::default();
        input.set_placeholder_text("ask for something, or /help");
        input.set_cursor_line_style(ratatui::style::Style::default());

        Self {
            kernel,
            policy,
            provider,
            counter,
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
            interrupting: false,
            streamed: false,
            streamed_bytes: 0,
            outcomes,
        }
    }

    // ------------------------------------------------------------------------ saying things

    /// Adds a finished entry to the transcript, ending whatever was still arriving.
    pub fn say(&mut self, speaker: Speaker, text: impl Into<String>) {
        self.close();
        self.transcript.push(Entry {
            speaker,
            text: text.into(),
            open: false,
        });
        self.follow = true;
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
        self.follow = true;
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
        for item in &items {
            match &item.kind {
                ContextKind::UserMessage => self.say(Speaker::User, item.content.to_text()),
                ContextKind::AssistantMessage { tool_calls, .. } => {
                    let text = item.content.to_text();
                    if !text.trim().is_empty() {
                        self.say(Speaker::Model, text);
                    }
                    for call in tool_calls {
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

    /// Adds an event to the trace pane.
    fn trace(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        if self.trace.len() == TRACE_DEPTH {
            self.trace.pop_front();
        }
        self.trace.push_back(Traced {
            name: name.into(),
            detail: detail.into(),
        });
    }

    // ---------------------------------------------------------------------- driving the kernel

    /// Starts, or carries on with, a turn.
    pub fn start_turn(&mut self) {
        if self.busy {
            return;
        }

        self.busy = true;
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

    /// Takes in the end of a turn.
    pub fn on_outcome(&mut self, outcome: Outcome) {
        self.busy = false;
        self.close();

        match outcome {
            Outcome::Failed(e) => self.say(Speaker::Error, e),
            Outcome::Stopped(State::Deciding { .. }) => self.overlay = Some(Overlay::Permission),
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
        }
        self.interrupting = false;

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

        if self.overlay.is_some() {
            self.overlay_key(key);
            return;
        }

        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match (key.code, ctrl) {
            (KeyCode::Char('c'), true) if self.busy => self.interrupt(),
            (KeyCode::Char('c'), true) | (KeyCode::Char('d'), true) => self.quit = true,
            (KeyCode::Esc, _) if self.busy => self.interrupt(),
            (KeyCode::Char('t'), true) => self.show(self.next_tab()),
            (KeyCode::Char('1'), _) if alt => self.show(Tab::Chat),
            (KeyCode::Char('2'), _) if alt => self.show(Tab::Context),
            (KeyCode::Char('3'), _) if alt => self.show(Tab::Trace),
            (KeyCode::Char('p'), true) => {
                self.preview("the next request", request_preview(&self.kernel))
            }
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
                (Tab::Context, Focus::Body) => self.context_key(key),
                (Tab::Trace, Focus::Body) => self.trace_key(key),
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
        self.tab = tab;
        self.focus = match tab {
            Tab::Chat => Focus::Input,
            _ => Focus::Body,
        };
    }

    /// Keys that belong to the prompt.
    async fn input_key(&mut self, key: KeyEvent) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            // enter sends, because that is what a prompt is for; a newline is alt+enter, which
            // is the one every terminal agrees on
            KeyCode::Enter if !alt => {
                let line = self.input.lines().join("\n").trim().to_owned();
                if line.is_empty() {
                    return;
                }
                self.input.select_all();
                self.input.cut();
                self.input.move_cursor(CursorMove::End);
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

    /// Keys that belong to the context pane.
    fn context_key(&mut self, key: KeyEvent) {
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
            KeyCode::End | KeyCode::Char('G') => self.selected = items.len() - 1,
            KeyCode::Char('?') => self.preview("the keys", crate::ui::HELP),
            // taking something out of the next request, and putting it back
            KeyCode::Char(' ') => {
                let (to, note) = match picked.state {
                    ContextState::Active | ContextState::Pinned => (
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
                self.preview(title, picked.content.to_text());
            }
            _ => {}
        }
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

    /// Keys that belong to whatever is on top.
    fn overlay_key(&mut self, key: KeyEvent) {
        let Some(overlay) = &mut self.overlay else {
            return;
        };

        match overlay {
            Overlay::Text { scroll, .. } => match key.code {
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll += 1,
                KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
                KeyCode::PageDown => *scroll += 20,
                // a tool is still waiting to be told whether it may run, so closing whatever was
                // being read goes back to the question rather than leaving it unanswered and
                // unreachable - which is what happened after [i] showed the exact JSON
                _ => {
                    self.overlay = match self.kernel.pending_permissions().is_empty() {
                        true => None,
                        false => Some(Overlay::Permission),
                    }
                }
            },
            Overlay::Permission => self.permission_key(key),
        }
    }

    /// Answers the question a tool is waiting on.
    fn permission_key(&mut self, key: KeyEvent) {
        let Some(request) = self.kernel.pending_permissions().into_iter().next() else {
            self.overlay = None;
            return;
        };

        let grant = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Grant::Allow,
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.policy.always(&request.capabilities);
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

                return;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Grant::Deny,
            _ => return,
        };

        if let Err(e) = self.kernel.decide(request.id, grant) {
            self.say(Speaker::Error, e.to_string());
        }
        // the model may have asked for several things at once, and each is its own question
        if self.kernel.pending_permissions().is_empty() {
            self.overlay = None;
            self.start_turn();
        }
    }

    /// Asks the running turn to stop at the next opportunity.
    fn interrupt(&mut self) {
        self.interrupting = true;
        self.kernel.interrupt();
    }

    /// Puts something long on the screen.
    fn preview(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.overlay = Some(Overlay::Text {
            title: title.into(),
            body: body.into(),
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
            "request" => self.preview("the next request", request_preview(&self.kernel)),
            "payload" => {
                let body = match self.kernel.preview_payload() {
                    Ok(Some(payload)) => pretty(&payload),
                    Ok(None) => "this provider cannot render a request without sending it".into(),
                    Err(e) => e.to_string(),
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
            "policy" => {
                let allowed = self.policy.listing().join(", ");
                self.say(
                    Speaker::Note,
                    format!("allowed without asking: {allowed}; everything else is a question, and the network is refused"),
                );
            }
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
                            "{} via {}, {} tokens of context",
                            info.model,
                            info.provider,
                            info.context_limit
                                .map(thousands)
                                .unwrap_or_else(|| "an unknown number of".into())
                        ),
                    ),
                    None => self.say(Speaker::Error, "there is no provider"),
                }
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
            other => self.say(
                Speaker::Error,
                format!("there is no `/{other}`; F1 lists what there is"),
            ),
        }
    }

    /// Prunes, pins or restores whatever a selector names.
    fn by_selector(&mut self, command: &str, input: &str) {
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

        // note: small requests teach it nothing and are not counted here, which is why this can
        // say "2" in a session that has sent six things
        let learned = self.counter.calibration();
        lines.push(match learned.observations {
            0 => "the counter has not been corrected yet: it is guessing at four bytes a token, \
                  and no request so far has been big enough to learn anything from"
                .to_owned(),
            n => format!(
                "the counter has learned from {n} request(s) and scaled itself by {:.3}: its own \
                 guesses came to {} tokens where the provider counted {}, so it was reading {:.1}% \
                 {}",
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

    /// Writes the session log and a snapshot that can be resumed from.
    fn save(&mut self, path: &str) {
        // the log says what happened; the snapshot is what can be picked back up. Keeping only
        // one of them means either losing the story or losing the context
        let stem = match path {
            "" => "session",
            given => given.strip_suffix(".json").unwrap_or(given),
        };
        let (log, state) = (format!("{stem}.jsonl"), format!("{stem}.json"));

        let records: Vec<String> = self
            .kernel
            .history()
            .iter()
            .filter_map(|record| serde_json::to_string(record).ok())
            .collect();
        let written = std::fs::write(&log, records.join("\n") + "\n").and_then(|()| {
            let snapshot = serde_json::to_vec_pretty(&self.kernel.snapshot())?;
            std::fs::write(&state, snapshot)
        });

        match written {
            Ok(()) => self.say(
                Speaker::Note,
                format!(
                    "{} records in {log}, and a session in {state} (kamchatka -r {state})",
                    records.len()
                ),
            ),
            Err(e) => self.say(Speaker::Error, e.to_string()),
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
        } => format!("{tool}: {grant}, by the {source:?}"),
        Event::Compacted { report } => format!(
            "{} out, {} → {} tokens",
            report.removed.len(),
            report.tokens_before,
            report.tokens_after
        ),
        Event::ModelFailed { error } | Event::StepFailed { error } => one_line(error),
        Event::ToolsChanged { tools } => format!("{} tools", tools.len()),
        _ => String::new(),
    };

    (name.to_owned(), detail)
}

/// The request the kernel would send, or what stopped it building one.
fn request_preview(kernel: &Kernel) -> String {
    let request = match kernel.preview_request() {
        Ok(request) => serde_json::to_string_pretty(&request)
            .unwrap_or_else(|e| format!("it will not serialize: {e}")),
        Err(e) => return e.to_string(),
    };

    // "why is that not in there?" is the question somebody opens this to answer, and the JSON on
    // its own can only say what *is* in there. The projection knows what it left out and what it
    // had to change to keep the request valid, so both go above it
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
    if header.is_empty() {
        return request;
    }

    format!(
        "{} item(s) in, {} out:\n{header}\n{request}",
        projection.included.len(),
        projection.skipped.len()
    )
}

/// JSON, indented.
fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("it will not serialize: {e}"))
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
