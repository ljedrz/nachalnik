//! Everything the terminal knows: what is on the screen, what the keys do, and what to make of
//! the events the kernel broadcasts.
//!
//! note: the kernel is driven from a task of its own, and this loop never blocks on it. What
//! arrives here is [`Event`]s - the same ones the session log is made of - so the screen is a
//! rendering of the record rather than a second account of it. When a turn stops for a decision,
//! the task ends and hands control back; nothing is waiting on a channel for an answer.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Instant, SystemTime},
};

#[cfg(feature = "tui")]
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nachalnik::{
    Capability, Content, ContextId, ContextItem, ContextState, Delta, Event, Grant, GrantSource,
    Kernel, PermissionId, PermissionRequest, State, StopReason, Tool, Usage, Verdict,
};
use nachalnik_providers::Dialect;
#[cfg(feature = "tui")]
use ratatui_textarea::{TextArea, WrapMode};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    sandbox::Confinement,
    tools::{Careful, Limits, Subject},
};

mod command;
mod going;
mod search;
mod transcript;
mod views;

pub use going::Going;
pub use search::Search;
pub use transcript::{Entry, Said, Speaker};
pub use views::Stance;
#[cfg(feature = "tui")]
mod keys;
pub(crate) mod text;
pub mod when;

use text::{moved, one_line, thousands, trace_line};
// only the key that prints a request without a command: `/request` imports its own
#[cfg(feature = "tui")]
use text::request_preview;

/// How many trace lines are kept; the session log is the one that keeps everything.
const TRACE_DEPTH: usize = 400;

/// How many earlier versions of one item the viewer keeps.
const VERSIONS: usize = 8;

/// How much of a still-running tool's output the transcript holds on to.
///
/// note: a tool's output and nothing else. A command can produce megabytes and the whole of it is
/// in the context either way, one keystroke from being read - but a *message* is never shortened
/// on the way to the screen, however long it is. See [`App::append`].
const LIVE_OUTPUT: usize = 8_000;

/// How far a chain of edits is followed before the conversation stops asking where an item
/// belongs.
///
/// note: a bound rather than a cycle check, because the thing being followed is a number in a
/// free-form `meta` that nothing in this program wrote alone - a hand-written snapshot can
/// point two items at each other, and a screen is a poor place to find out.
const HOPS: usize = 16;

/// Which half of the window the keys are talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The prompt, which is on the chat tab and wherever an item is being edited.
    Input,
    /// Whatever else on the screen takes keys: the tab's own body, or - on the chat tab - the
    /// question standing in the prompt's place, which is the only thing there that does.
    Body,
}

/// What the window is showing.
///
/// note: whole-window tabs rather than panes side by side. Three things want the screen - the
/// conversation, the context and the event stream - and split between them all three are
/// cramped: the trace is cut off mid-sentence, the context can only afford a label and a number,
/// and a long answer reads in sixty columns. Only one of them is being read at a time. The status
/// line is under all of them, because the budget is always worth seeing; the prompt is not,
/// because three of the four are read and operated rather than typed into, and a prompt there
/// would be a mode - every letter on those tabs would mean one of two things depending on where
/// the focus had got to.
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

/// One line of the trace pane: an event's name, and what it says for itself.
pub struct Traced {
    /// The dotted name, e.g. `model.requested`; empty for a continuation line.
    pub name: String,
    /// The rest of it.
    pub detail: String,
    /// When it arrived, on the clock that only goes forwards.
    ///
    /// note: what a log is missing without a clock is the question people actually bring to one:
    /// which step was slow. Kept as an instant rather than a rendered string because what the
    /// pane shows beside it is the gap to the line above, which is not a property of either line
    /// alone - and because an `Instant` cannot be dragged backwards by the system clock being
    /// set, which a duration computed from wall time can.
    pub at: Instant,
    /// When it arrived, on the clock a person reads.
    ///
    /// note: both, because they answer different questions and neither can answer the other's.
    /// `at` says how long a step took; this says when it happened, which is what somebody
    /// matching the pane against a server log, a ticket or their own memory of the afternoon
    /// needs. An `Instant` is deliberately opaque and has no rendering as a time of day.
    ///
    /// note: a `SystemTime` rather than something already formatted, so that nothing in `app`
    /// has to know about time zones or about how wide a column is. Turning it into digits is the
    /// screen's job, and the screen is the only thing that has a width.
    pub wall: SystemTime,
    /// Whether the time before this line was somebody thinking rather than the program working.
    ///
    /// note: the pane draws no gap where this is set, however long the gap was. The column exists
    /// to answer *which step was slow*, and a session spends most of its wall time in two places
    /// where nothing is stepping at all: a permission question nobody has answered yet, and the
    /// wait between one turn and the next message. The gap beside `permission.decided` is not the
    /// runtime working - it is a person reading the question - and it is usually the largest
    /// figure in the column, which would make the one number nobody should act on the one the eye
    /// goes to first.
    ///
    /// note: set on the line that *ends* the wait rather than the one that begins it, because the
    /// gap belongs to the line it is drawn beside. `App::trace` takes it, so it marks exactly one
    /// line: the first thing that happens after somebody acts, which is the line whose gap spans
    /// their thinking.
    pub after_a_person: bool,
}

/// What is being shown over the top of everything else.
#[derive(Clone)]
pub enum Overlay {
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
/// will contain, what the item says, and what it said before somebody rewrote it - and a viewer
/// that picked one of them to show would be quietly wrong about the other two.
///
/// note: serializable because a page is what a command answers with, and a caller answering a
/// line is not always in this process - see [`crate::remote`]. The two fields are what a page
/// *is*; which of them is on screen and how far down it is scrolled are the overlay's, and are
/// not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    /// What to call it on the strip along the top.
    pub name: String,
    /// The thing itself.
    pub body: String,
}

/// A compaction pass, listed and waiting to be told whether to take what it listed.
///
/// note: what it holds is the *list*, not the plan. Answering `y` works the pass out again, so
/// a pin made while reading this is honoured rather than refused after the fact - which is why
/// the question is pinned rather than modal: the context tab is a keystroke away while it waits,
/// and `p` there is the answer to "not that one".
#[derive(Clone)]
pub struct Proposed {
    /// One line per item, in the order the compactor chose them.
    pub rows: Vec<String>,
    /// How many items there are.
    pub count: usize,
    /// What they are holding between them, which is not what the request will fall by: an elided
    /// item leaves a marker behind, and the difference is the marker.
    pub holding: usize,
}

/// What a provider actually charged for a request, and what that request was made of.
///
/// note: the one exact number in this program's accounting, kept so that an estimate does not
/// have to carry the whole context. A counter without the model's tokenizer is out by a few
/// percent of *everything it is asked about*, even calibrated, and one percent of a hundred
/// thousand tokens is a thousand tokens, which is a poor thing to be reading while deciding
/// whether the next message fits.
///
/// note: so the figures on screen are this plus what has changed since, and the error is a few
/// percent of *the change* rather than of the context. It also absorbs, exactly and for free,
/// everything the counter is structurally blind to: per-message framing, the tool schemas, and
/// any picture that has already been sent - a `Content::Blob` the counter refuses to price is
/// inside this number, so it stops being unaccounted for the moment it has gone out once.
#[derive(Default)]
pub struct Anchor {
    /// The items whose own content was in that request.
    ///
    /// note: whose content, which is not the same as which items were in it. An elided one is in
    /// a request as a marker, and recording it here would have the arithmetic below take the
    /// whole of what it *holds* back out of a figure that only ever had a line of text in it.
    /// That is not a rounding error: once a large attachment is elided and one request goes out
    /// without it, the corner would read `~0` for the rest of the session.
    pub sent: Vec<ContextId>,
    /// What everything else in it came to - the markers standing where an elided item was.
    ///
    /// note: a stored figure, where everything else here is re-estimated on the way past, and
    /// the exception is deliberate. Re-estimating is what makes an unchanged item cancel exactly
    /// against itself, but it needs the *text* that was sent, and the text of a marker is gone
    /// as soon as the item stops being elided. A marker is one line, so what is lost by storing
    /// it in older money is a fraction of twenty tokens - against the whole of an elided item
    /// that getting it wrong costs.
    pub markers: usize,
    /// The provider's own figure for the whole of it, tool definitions and framing included.
    pub reported: usize,
}

/// How long leaving a session waits for a turn that is still running to stop; see
/// [`App::wait_for_turn`].
///
/// note: long enough for anything that looks at its interrupt - a model's stream, `shell`, the
/// searches and `fork` do - and short enough that a `/quit` does not look like a hang.
pub const LEAVING: std::time::Duration = std::time::Duration::from_secs(5);

/// What the kernel's task reports when it stops.
pub enum Outcome {
    /// The turn ended in this state.
    Stopped(State),
    /// One transition happened, and produced this state.
    Stepped(State),
    /// It could not be finished.
    Failed(String),
}

/// What one line handed to [`App::submit`] did.
///
/// note: three answers rather than two, because "it was queued" is a different thing to tell
/// somebody than "it was asked". A line sent into a running turn waits for the end of that turn -
/// see the note on [`App::submit`] for why it cannot go in any earlier - and a caller that read
/// that as "asked" would be waiting for an answer to a question the model has not been given yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Did {
    /// It went into the context as a message, and a turn was started for it.
    Asked(ContextId),
    /// A turn was already running, so it waits for the end of that one and then gets its own.
    Queued,
    /// It began with `/`, so it was a command, and it has been run.
    Ran,
}

/// What came back from one line: what it did, what was said about it, and any page it opened.
///
/// note: this is the half of `submit` that is otherwise readable only by watching [`App::loose`]
/// and [`App::overlay`] change - which is what a *screen* does, because a screen re-reads both
/// every frame. A caller that is not a screen is answering a line rather than redrawing a
/// window, and it asked a question: this is the answer to it.
///
/// note: what it carries is a copy rather than a move. The lines are still in [`App::loose`] and
/// the page is still in [`App::overlay`], because the terminal reads them from there and a reply
/// nobody collected must not take a command's output off the screen.
pub struct Reply {
    /// What the line did.
    pub did: Did,
    /// What the program said about it, in the order it said it.
    pub said: Vec<Entry>,
    /// The page it opened, if it opened one.
    ///
    /// note: an `Option` rather than an empty page, and it is the *new* one rather than whatever
    /// the overlay happens to hold: a command that says a line while an earlier command's page is
    /// still open opened nothing, and reporting that page again would have a headless caller
    /// print `/seams` twice for two unrelated commands.
    ///
    /// note: the whole overlay rather than the [`Page`] inside it, because the title is on the
    /// overlay and a `Page` opened by `App::preview` is deliberately nameless - there is one of
    /// them, and the strip along the top of a one-page box would be saying nothing. A caller
    /// handed the page alone gets `--- ---` where `--- the budget ---` belongs.
    pub page: Option<Overlay>,
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
    /// The advisor wrapped around it, where there is one, for the rating the question draws.
    ///
    /// note: the only thing this is read for. What *decides* is inside the kernel and is reached
    /// through no field here - see the note in `wiring`, which is careful that the policy the
    /// kernel holds and the stances the screen draws are one object and not two. This is the
    /// advisor's own memory of what it said about a command, which nothing else has a way to ask
    /// it for.
    ///
    /// note: `None` in a session started without `--advise`, which is the default.
    #[cfg(feature = "shell-advisor")]
    pub advisor: Option<Arc<crate::tools::Advised>>,
    /// The provider, for switching models - whichever dialect it speaks.
    pub provider: Arc<dyn Dialect>,
    /// A `/model` or `/provider` still settling, which the next line waits for.
    ///
    /// note: both commands hand the switch to a task rather than standing there while it happens,
    /// because finding out what the new model holds and whether the new address serves it is two
    /// round trips and a screen should not stop for them. What the *next line* may not do is read
    /// a session that has not finished changing: `/provider URL ID` followed by `/model` would
    /// report the old model, and a message on the line after a switch could be asked of whichever
    /// of the two won the race. Down a pipe there is no gap between the lines at all, so what is a
    /// race at a keyboard is the ordinary case in a script.
    ///
    /// note: awaited in [`App::submit`] rather than anywhere the provider is read, which is the
    /// narrower door and the right one: a frame drawn mid-switch showing the old name for a
    /// moment is a frame, and the next one corrects it. A *line* acting on the old name is an
    /// answer. Every question a switch asks the endpoint is bounded by the provider, so this
    /// cannot wait for ever.
    pub settling: Option<tokio::task::JoinHandle<()>>,
    /// How much of each tool's output the model is shown, which `/limit` changes.
    ///
    /// note: the same handle the tools were built with, so `/limit` changes the number they will
    /// actually declare on the next request. Holding a second one would be a command that reports
    /// success and does nothing.
    pub limits: Limits,
    /// What items used to say, oldest first, for the ones that have been rewritten.
    ///
    /// note: kept here rather than in the kernel because the kernel deliberately does not keep
    /// it. A replacement is the one context operation that overwrites something, which is why
    /// [`nachalnik::Event::ContextReplaced`] is the one event that carries content - so that a
    /// client which wants the history can have it, and one that does not pays nothing. Without
    /// this, a `context` that rewrote a tool result would leave the old text nowhere a person can
    /// read it: on the trace as a line of JSON, and in an undo window that closes.
    ///
    /// note: both hands land here. A terminal edit replaces in place as `context: revise`
    /// does, so this is where the words it changed are, and the reason it needs no second
    /// mechanism of its own.
    versions: BTreeMap<ContextId, Vec<Content>>,

    /// The handle the introspection tools reach the kernel through, kept for as long as the
    /// session lasts.
    ///
    /// note: it is here rather than in `main` because it has to outlive the tools: they hold a
    /// weak handle to it, and dropping it is what takes their reach away. See
    /// [`crate::introspect::install`].
    ///
    /// note: dropping it is not how the tools are switched off, because it would also throw away
    /// what `context` is remembering. Turning a tool off is [`App::toggle`], and a shelved tool is
    /// still the same tool: what it pinned is still pinned, and what it could still walk back it
    /// still can.
    pub introspect: Option<crate::introspect::Installed>,
    /// The tools this session is not offering, by id, kept so that they can be offered again.
    ///
    /// note: the tool itself rather than its id, which is what makes this work for a tool nobody
    /// here wrote. A list of names would mean rebuilding whatever was named, and there is no way
    /// to rebuild an MCP server's tool or an embedder's - so turning one off would be turning it
    /// off for good. What [`Kernel::remove_tool`] hands back is the tool, and this keeps it.
    pub shelved: BTreeMap<String, Arc<dyn Tool>>,
    /// How much of the shell's sandbox the kernel agreed to, asked once at startup.
    ///
    /// note: on `App` rather than worked out where it is drawn, because finding out means
    /// applying a ruleset in a child process and that is not something a frame should be doing
    /// sixty times a second. It cannot change while the program runs.
    pub confinement: Confinement,
    /// What the provider charged for the last request, and what was in it. See [`Anchor`].
    pub anchor: Option<Anchor>,
    /// The items the request now in flight was built from, until there is a figure to pair
    /// them with.
    ///
    /// note: two events, because the runtime reports what went out and what came back
    /// separately and neither is any use here without the other.
    pending: Anchor,
    /// The lines of the chat that are not context items, in the order they were said.
    ///
    /// note: not "the conversation". The conversation is the context, and
    /// [`App::conversation`] reads it every frame; this is what that reading cannot account
    /// for. See [`Entry`].
    pub loose: Vec<Entry>,
    /// How many times [`App::clear_notices`] has emptied the program's own lines.
    ///
    /// note: a generation rather than a flag, because what reads it is a watermark rather than a
    /// screen. [`App::notes`] is safe to count against only while the filtered sequence is
    /// append-only, and this is the one thing in the program that is not - it empties that
    /// sequence outright. A loop holding `said` against a sequence that went back to nothing
    /// swallows the next `said` lines, silently, for the rest of the session. Compare it, and
    /// start again from nothing when it moves.
    cleared: u64,
    /// Every event, name and detail.
    pub trace: VecDeque<Traced>,
    /// The prompt.
    #[cfg(feature = "tui")]
    pub input: TextArea<'static>,
    /// The colour of the window's frame, and of everything that is yellow to say *the keys are
    /// here*: the active tab, the prompt while it has them, an answerable question.
    ///
    /// note: one field rather than one per border, because they are one statement. All four are
    /// yellow to say the same thing, and a setting that moved three of them would leave the
    /// fourth reading as a different kind of thing rather than as the one somebody forgot.
    ///
    /// note: not every yellow in the program. `ask` on the permissions tab, a budget bar past
    /// seven tenths, a command that was killed, a pinned row - those are yellow because yellow
    /// *means* something there, and they stay yellow against a frame of any colour. This is the
    /// accent; those are the vocabulary.
    #[cfg(feature = "tui")]
    pub accent: ratatui::style::Color,
    /// Which pane the keys go to.
    pub focus: Focus,
    /// Which of the listed context items is picked out; an index into [`App::listed`], which is
    /// not the whole context when `sending_only` is on.
    pub selected: usize,
    /// Whether the context tab lists only what the next request carries, leaving out everything
    /// that is excluded, elided, archived, superseded or repaired away.
    pub sending_only: bool,
    /// Where the context pane is scrolled to, which it keeps between frames.
    #[cfg(feature = "tui")]
    pub list: ratatui::widgets::ListState,
    /// The context rows the last frame drew, until the next key.
    ///
    /// note: what the context keys count rows in, so that a key does not filter the whole
    /// context again to find the rows the frame has just found - with a search open, that doubled
    /// what every key cost. Only the first key after a frame reads them: whatever it changed, no
    /// frame has drawn yet, so the key after it filters afresh.
    #[cfg(feature = "tui")]
    pub(crate) drawn: Option<Vec<ContextId>>,
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
    /// The failure already reported for the turn now running, so the same one is not said twice.
    ///
    /// note: scoped to a turn, because one failure being reported twice is a fact about a turn -
    /// the kernel emits the event and then the turn comes to the same end, the second wrapping
    /// the first. Comparing against the last line in [`App::loose`] would not do, because that
    /// outlives every turn: nothing said between two turns goes in there, since a message and an
    /// answer are both drawn from the context. The first red line would stay the last loose line
    /// for the rest of the session and swallow every later failure with the same words, so a
    /// model with one canned refusal would fail silently from its second refusal on.
    failed: Option<String>,
    /// Whether a person has just done something the trace has not drawn a line for yet.
    ///
    /// note: private, and set at the two doors a person comes through - `submit`, where a line is
    /// handed in, and the answer to a permission question. [`Traced::after_a_person`] is what it
    /// becomes, and `App::trace` takes it, so it marks the one line whose gap is somebody's
    /// thinking rather than the program's working.
    ///
    /// note: set where the program learns a person acted rather than where the waiting begins.
    /// A question opens and `state.changed`, a recount and the question itself all arrive in the
    /// same millisecond, so a flag set at the opening would be spent on one of those and the
    /// person's wait would still be drawn beside `permission.decided`.
    acted: bool,
    /// Whether it is time to leave.
    pub quit: bool,
    /// Whether the session is to be written out and started again from nothing.
    ///
    /// note: beside [`App::quit`] and read the same way, by the loop rather than here, because
    /// what a restart replaces is this whole object. A method cannot hand back the thing it was
    /// called on, and every loop already has the one branch that ends it - so a restart is that
    /// branch with somewhere to go afterwards. [`App::restart`] is what sets it.
    ///
    /// note: it outranks `quit` nowhere. A session asked to do both leaves, because leaving is the
    /// one of the two that cannot be got back to by typing the other word again.
    pub restart: bool,
    /// Text the loop has been asked to hand the terminal, for its clipboard, and has not yet.
    ///
    /// note: a field rather than a write, because a screen belongs to whoever owns it. The two
    /// loops in this program take it and write the escape sequence; a host embedding the `App`
    /// gets the text and does whatever its own window does with a copy, which is what it would
    /// have had to do anyway with an escape it never asked for written into its terminal.
    ///
    /// note: the whole of the item rather than what is drawn of it. That is the point of copying
    /// from here instead of dragging a mouse over the pane: this text is not wrapped to a window,
    /// has no frame down either side of it, and is all there whether or not it fits on a screen.
    pub clipboard: Option<String>,
    /// How many tokens the provider may charge for this session before it stops; `None` never
    /// stops. [`App::set_spend`] is how it is changed, and [`App::spend`] reads it.
    ///
    /// note: here rather than in the loop that drives the session, which is what makes it a
    /// ceiling rather than a headless flag. Every caller
    /// hands events to [`App::on_event`] - the screen, the headless driver, and an embedder with a
    /// loop of its own - so this is the one place where counting them reaches all three. A guard
    /// that only the program's own loop applied would be no guard for the embedder who most needs
    /// one.
    ///
    /// note: private, alone among the things a caller sets, because it is not the only field that
    /// has to move: raising it has to let a stopped session go again, and a ceiling written
    /// straight in would leave `overspent` latched over a limit nothing has reached.
    spend: Option<u64>,
    /// How many wrapped lines the transcript came to, as of the last frame.
    pub rendered: usize,
    /// How many lines fit, as of the last frame.
    pub viewport: usize,
    /// The `/` filter over this tab's rows, while its box is on the screen.
    ///
    /// note: one box rather than one per tab, and it is cleared when the tab changes. A query
    /// written for the trace means nothing against the context, and carrying it over would filter
    /// a pane by a phrase nobody typed there.
    pub search: Option<Search>,
    /// Which context item the prompt is editing, if it is editing one rather than composing a
    /// message.
    pub editing: Option<ContextId>,
    /// Whether the loop is being driven a transition at a time, so that answering a permission
    /// does not quietly run the rest of the turn.
    pub stepping: bool,
    /// Whether somebody is at a screen, with this program's keys to press.
    ///
    /// note: what `/help` turns on, and it is a fact about the *caller* rather than about the
    /// session - which is why the loop sets it and `App` cannot work it out. A run down a pipe and
    /// a browser attached over a socket both have no `ctrl+p` to press, and neither should be
    /// handed pages about one.
    ///
    /// note: `true` by default, and the direction is deliberate rather than convenient. This type
    /// is the terminal's state - it holds a prompt, a focus, a tab and two scroll positions - so
    /// the caller that departs from its shape is the one without keys, and it is the one that says
    /// so. `headless.rs` and [`crate::remote`] are both driven by a loop that knows.
    pub keys: bool,
    /// Digits typed at the context tab, waiting for the key that uses them.
    pub count: String,
    /// Which capability is picked out on the permissions tab.
    pub chosen: usize,
    /// Where the permissions tab is scrolled to, which it keeps between frames.
    #[cfg(feature = "tui")]
    pub grants: ratatui::widgets::ListState,
    /// Whether the last stop was asked for rather than reached.
    interrupting: bool,
    /// Whether the model has been seen to think without showing any of it, so it is said once.
    ///
    /// note: a property of the endpoint rather than news about a turn, the way `repairs` below is.
    /// A model that returns no reasoning returns none of it every turn, and saying so after each
    /// one would be the bug that note describes, in a second place.
    thought_unseen: bool,
    /// The repairs the last request needed, so that a standing one is said once.
    ///
    /// note: a repair is a property of the context rather than news about a turn. The projection
    /// is built afresh for every request, so the projector re-does the repair and honestly
    /// re-reports it, and saying it each time would put the same line in the conversation after
    /// every message for the rest of a session, for one tool result excluded once. All four kinds
    /// behave this way: an orphaned call, an orphaned result, a flattened turn and a result held
    /// back all last as long as the state that caused them.
    ///
    /// note: the *conversation* only. The trace keeps every one, because it is the event log and a
    /// log that hid a repeated entry would be the wrong thing entirely - `model.requested` really
    /// does carry that repair, every time.
    reported_repairs: Vec<String>,
    /// How far down the pinned question's arguments are scrolled.
    ///
    /// note: on the app rather than on the question, because there is no question to hang it on:
    /// what stands in the prompt's place is drawn from `pending_permissions()` every frame, so
    /// there is no state saying a question is open and none to get out of step with the kernel.
    pub question_scroll: usize,
    /// A compaction pass waiting on a `y` or an `n`, if one is.
    ///
    /// note: held here, where `App::asked` is asked of the kernel every time. The kernel knows
    /// nothing about this one: a compaction nobody has agreed to yet is not a state the runtime
    /// has, and giving it one would be this program's screen leaking into the runtime's model of
    /// a session. What the kernel is told is the pass itself, once, when somebody says yes.
    pub proposed: Option<Proposed>,
    /// A message somebody sent into a turn that was already running, waiting for it to end.
    ///
    /// note: one, and the newest wins. [`App::put_back`] is the way back to *this* one - `up`
    /// takes it out of here and into the prompt, where it can be changed, sent again or simply
    /// dropped - and there is no way back to one it replaced, so the replacement is said out loud
    /// where it happens. The row is drawn at the end of the conversation, and a row that vanishes
    /// with no account of why is the thing this program does not do.
    typed_ahead: Option<String>,
    /// The last line submitted at the prompt, message or command, for [`App::put_back`].
    last_sent: Option<String>,
    /// What [`App::put_back`] last put in the prompt, for as long as the prompt still says
    /// exactly that.
    ///
    /// note: compared against what is in the box rather than trusted on its own, so `down` undoes
    /// a recall and nothing else. A word typed onto the end makes it a message somebody is
    /// writing, and a key that emptied the box then would be the worst kind of shortcut.
    ///
    /// note: behind the feature, unlike `last_sent` above it, and the difference is which side of
    /// the screen each belongs to. A line that was sent is a fact about the session - `submit`
    /// records it whether anybody is watching or not - and this is a fact about the prompt box,
    /// which a build with no screen does not have. Every use of it is in `keys`.
    #[cfg(feature = "tui")]
    recalled: Option<String>,
    /// How much the running tool has said so far, for the one trace line that counts it.
    streamed_bytes: usize,
    /// Where a finished turn reports itself.
    outcomes: UnboundedSender<Outcome>,
    /// How many pages have been opened over the session.
    ///
    /// note: a counter rather than a flag, and it exists for one question: did *this* call open a
    /// page? The overlay cannot answer it - it holds the last page opened, whenever that was -
    /// and duplicating the page somewhere else so that it could would be two copies of one thing
    /// to keep in step. See [`Reply::page`].
    previews: usize,
    /// What the provider has charged for this session so far: every response's own figure, added
    /// up.
    spent: u64,
    /// Whether the ceiling has been reached, so that nothing else is sent until somebody says so.
    overspent: bool,
    /// Whether it has already said that the endpoint reports no figures to add up.
    ///
    /// note: a property of the endpoint rather than news about a turn, like `thought_unseen` above
    /// and for the same reason.
    unreported: bool,
}

impl App {
    /// Builds the terminal's state around a kernel that is already wired up.
    pub fn new(
        kernel: Kernel,
        policy: Arc<Careful>,
        provider: Arc<dyn Dialect>,
        limits: Limits,
        outcomes: UnboundedSender<Outcome>,
    ) -> Self {
        #[cfg(feature = "tui")]
        let input = {
            let mut input = TextArea::default();
            input.set_placeholder_text("ask for something, or /help");
            input.set_cursor_line_style(ratatui::style::Style::default());
            // a long message wraps rather than scrolling sideways: the default keeps one long line
            // on one row and slides it under the left border, so what somebody typed a moment ago
            // is off the screen while they are still typing it. `WordOrGlyph` breaks at spaces and
            // splits a word only when it could not fit on a line of its own - a path or a URL,
            // which is exactly the thing worth seeing all of
            input.set_wrap_mode(WrapMode::WordOrGlyph);

            input
        };

        Self {
            kernel,
            policy,
            #[cfg(feature = "shell-advisor")]
            advisor: None,
            provider,
            limits,
            versions: BTreeMap::new(),
            introspect: None,
            shelved: BTreeMap::new(),
            // the terminal's own default, for a screen test that never spawns anything; the
            // program overwrites it with what a child process actually reported
            confinement: Confinement::Unsupported,
            anchor: None,
            pending: Anchor::default(),
            failed: None,
            loose: Vec::new(),
            cleared: 0,
            trace: VecDeque::new(),
            #[cfg(feature = "tui")]
            input,
            // note: the terminal's own yellow rather than a hex of one, so that a window with
            // nothing configured still belongs to whatever palette it is opened in. A default
            // written as `#ffff00` would look the same in one theme and wrong in every other
            #[cfg(feature = "tui")]
            accent: ratatui::style::Color::Yellow,
            focus: Focus::Input,
            selected: 0,
            sending_only: false,
            #[cfg(feature = "tui")]
            list: ratatui::widgets::ListState::default(),
            #[cfg(feature = "tui")]
            drawn: None,
            overlay: None,
            scroll: 0,
            follow: true,
            tab: Tab::Chat,
            trace_scroll: 0,
            busy: false,
            acted: false,
            quit: false,
            restart: false,
            clipboard: None,
            spend: None,
            rendered: 0,
            viewport: 0,
            search: None,
            editing: None,
            stepping: false,
            keys: true,
            count: String::new(),
            chosen: 0,
            #[cfg(feature = "tui")]
            grants: ratatui::widgets::ListState::default(),
            interrupting: false,
            thought_unseen: false,
            reported_repairs: Vec::new(),
            since: Instant::now(),
            question_scroll: 0,
            settling: None,
            proposed: None,
            typed_ahead: None,
            last_sent: None,
            #[cfg(feature = "tui")]
            recalled: None,
            streamed_bytes: 0,
            outcomes,
            previews: 0,
            spent: 0,
            overspent: false,
            unreported: false,
        }
    }

    // ---------------------------------------------------------------------- driving the kernel

    /// Starts, or carries on with, a turn.
    ///
    /// note: it refuses once the ceiling has been reached, and that refusal is what makes
    /// [`App::spend`] a bound rather than a report. Stopping the turn that crossed the line is
    /// only half of it: a loop that hands in the next line - a script, a person, an agent driving
    /// this from somewhere else - would start spending again, and every caller goes through here.
    pub fn start_turn(&mut self) {
        if self.busy || self.broke() || self.no_model() {
            return;
        }

        self.busy = true;
        self.since = Instant::now();
        self.stepping = false;
        self.interrupting = false;
        self.failed = None;
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
    /// note: this is the runtime's own shape, made visible. A turn is a loop over `step`, and
    /// running it a transition at a time is the only way to stand in [`State::Ready`] and look at
    /// what the model has asked for *before* any of it runs - which the kernel documents as a
    /// resting state on purpose, and which a whole turn walks straight through.
    pub fn start_step(&mut self) {
        if self.busy || self.broke() || self.no_model() {
            return;
        }

        self.busy = true;
        self.since = Instant::now();
        self.stepping = true;
        self.interrupting = false;
        self.failed = None;
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
            // what stepping is for: the calls are decided and about to run, and nothing has
            // happened yet
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
    }

    /// Takes in the end of a turn.
    pub fn on_outcome(&mut self, outcome: Outcome) {
        self.busy = false;
        self.close();

        // note: before anything this says about the turn, because most of what a provider puts
        // here is *about* the turn that just ended - "the model was cut off mid-answer; what had
        // arrived is kept" is the account of the answer above it, and reads as a remark about the
        // next one if it lands after. The loop also drains this on a tick, for the notices that
        // belong to no turn at all, and taking it twice costs nothing.
        //
        // note: here rather than only in that loop, so that a notice is not something only the
        // program's own `main` receives: an embedder driving `App` directly would otherwise never
        // hear that an answer was cut off.
        if let Some(notice) = self.provider.take_notice() {
            self.say(Speaker::Note, notice);
        }
        // note: the advisor's beside the provider's, for the same reason and in the same place.
        // It is a second thing this session depends on and cannot see
        #[cfg(feature = "shell-advisor")]
        if let Some(notice) = self.advisor.as_ref().and_then(|advised| advised.notice()) {
            self.say(Speaker::Note, notice);
        }

        // note: a turn that stopped to ask a question has not ended - the call it is asking about
        // still has a result to come - and a message pushed now would land between the call and
        // that result, which is a place a request cannot have one
        //
        // note: a failure has ended it as far as a waiting message is concerned, and so has a step
        // that came to rest. Left waiting, the message is overtaken by the next one sent, which
        // finds the session resting and runs at once - and goes in after that one's turn, or never
        // if there is no next turn. A step that stopped at `Ready` has calls still to run, and the
        // message waits for their results rather than landing between them and their calls
        let ended = match &outcome {
            Outcome::Stopped(state) => !matches!(state, State::Deciding { .. }),
            Outcome::Stepped(state) => matches!(state, State::Idle | State::Finished { .. }),
            Outcome::Failed(_) => true,
        };
        // a `Stepped` outcome is somebody driving this a transition at a time, and a failure is
        // not the moment to start something else; either way what was typed waits for `/continue`
        let interrupted = std::mem::take(&mut self.interrupting);
        let carry_on = ended && !interrupted && matches!(outcome, Outcome::Stopped(_));
        match outcome {
            Outcome::Failed(e) => self.say_error(e),
            // note: a turn stopping to ask says nothing here, and opens nothing. The question is
            // drawn from `pending_permissions()` every frame, so there is no moment at which it
            // has to be put on the screen and none at which it has to be taken off, so nothing
            // can be left standing over a question that was answered somewhere else
            //
            // note: unless every question was answered while this outcome was on its way, in which
            // case the turn is carried on here. `permission.requested` is broadcast while the turn
            // that raised it is still unwinding, so an answer inside that window is recorded and
            // then goes nowhere: `App::decide` calls `start_turn`, `start_turn` refuses because the
            // old turn is still marked as running, and the session stops for good with every
            // question answered and nothing to answer. The window is narrow but real - it is
            // whatever the gap is between a client's socket and this loop. `headless.rs` stays
            // out of it by only answering while the kernel rests; the keys and a socket cannot,
            // because a person answers when they answer, so it is closed here instead, once, for
            // all three
            Outcome::Stopped(State::Deciding { .. })
                if !self.stepping && self.kernel.pending_permissions().is_empty() =>
            {
                self.start_turn();
                self.interrupting = interrupted;
            }
            Outcome::Stopped(State::Deciding { .. }) => {}
            // a turn that stops in `Idle` either ran out of requests or was asked to stop, and
            // the difference matters to whoever is reading the screen
            Outcome::Stopped(State::Idle) if !interrupted => {
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

        // a message somebody sent into this turn has waited for it to end; now it goes in, and
        // unless the turn was stopped, stepped or failed it gets a turn of its own
        //
        // note: `caught_up` because the line saying it was waiting is a live one - it was said
        // when there was no item to say it from - and pushing is what gives it one. The item is
        // drawn in its place, at the end of the conversation, which is where the request has it
        if ended && let Some(message) = self.typed_ahead.take() {
            let id = self.kernel.push(ContextItem::user(message));
            self.caught_up(id);
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
        // be read - a long `cat` would erase the whole of it, one `tool.output` at a time. The
        // fragments themselves are on the chat tab; the session log
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
                Delta::Text(fragment) => self.append(Speaker::Model, &fragment),
                Delta::Reasoning(fragment) => self.append(Speaker::Reasoning, &fragment),
                // the arguments are shown once they parse, as the call the model actually made
                _ => {}
            },
            Event::ModelRequested {
                repairs,
                skipped,
                items,
                ..
            } => {
                // the last batch's one-off answers, which nothing can still be waiting for: a
                // request is only built from `Idle`, so every call they were given for has run
                self.policy.forget_network_grants();
                self.close();
                // note: split here rather than when the response lands, because *here* is the
                // one moment the two are the same thing: the context has just been projected
                // into that request and has not moved yet. Asked later, an item elided in the
                // meantime would look as though it had gone out as a marker
                let going = self.going();
                self.pending = items.iter().fold(Anchor::default(), |mut anchor, id| {
                    match self
                        .kernel
                        .item(*id)
                        .is_some_and(|it| going.sends_content(&it))
                    {
                        true => anchor.sent.push(*id),
                        false => anchor.markers += going.costs.get(id).copied().unwrap_or(0),
                    }

                    anchor
                });

                // the kernel altering what the model is told is not a detail for the trace pane.
                // One compaction pass can orphan half a dozen calls at once, though, and six
                // notices in a row push the answer off the screen to say one thing - so the
                // conversation gets the fact and the request preview gets the list
                //
                // note: and only when they change. See `App::reported_repairs` - a repair lasts as
                // long as the state that caused it, so the projector re-does it for every request
                // and honestly reports it again
                //
                // note: the count is everything being repaired rather than what is newly so,
                // because it is the number the preview will show. And the wording says the repair
                // stands: in the past tense it reads as something that happened to this one
                // request, which is exactly what somebody then goes looking for the cause of,
                // and there is nothing about this turn to find
                //
                // note: `/request` and not `ctrl+p`, which is the same page and is the key for it
                // on a screen. Everything this program says goes out of a headless run too, where
                // there is no keyboard and the trace holding the list is not printed - so a run
                // driven down a pipe would be told to press a key that does not exist there, about
                // a list it has no other way to see. The command works in both
                if repairs != self.reported_repairs {
                    match repairs.len() {
                        0 => {}
                        1 => self.say(
                            Speaker::Note,
                            format!(
                                "the request is repaired, and will be while this stands: {}",
                                repairs[0]
                            ),
                        ),
                        many => self.say(
                            Speaker::Note,
                            format!(
                                "the request is repaired in {many} places, and will be while they \
                                 stand; `/request` says where"
                            ),
                        ),
                    }
                    self.reported_repairs = repairs.clone();
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
            Event::ModelFinished {
                item, usage, stop, ..
            } => {
                // note: the tokens are real and the words are gone. Some endpoints bill for
                // reasoning and return none of it - `mercury-2.5`'s stream carries no reasoning
                // field at all - so the context tab shows a turn with nothing in it where the
                // thinking was, and the only trace of where the money went is a number in
                // `/budget`. Said once, because it is true of the endpoint rather than of this turn
                if !self.thought_unseen
                    && usage.is_some_and(|it| it.reasoning_tokens.is_some_and(|n| n > 0))
                    && self
                        .kernel
                        .item(item)
                        .is_none_or(|turn| turn.thinking().next().is_none())
                {
                    self.thought_unseen = true;
                    self.say(
                        Speaker::Note,
                        "this model is charged for reasoning it does not send back, so its \
                         thinking is a number in /budget and nowhere else"
                            .to_owned(),
                    );
                }
                self.charge(usage, &stop);
                // the provider has just said what that request really cost, and what it was
                // made of is still here from the event that sent it. Paired, they are the one
                // exact figure in this program's accounting; see `Anchor`
                if let Some(reported) = usage.and_then(|usage| usage.input_tokens) {
                    self.anchor = Some(Anchor {
                        reported: reported as usize,
                        ..std::mem::take(&mut self.pending)
                    });
                }
                // the turn is recorded, so whatever streamed is now the item's to say. This is
                // also what makes a provider that does not stream work without being detected:
                // there was nothing on the screen and there is an item, and the item is what
                // gets drawn either way
                self.caught_up(item);
            }
            // the same fact from the two places that can know it, and the second line says which
            // of them it is: one is a count and the other is a guess
            Event::ModelFailed { error, overrun } => {
                self.close();
                self.say_error(error);
                self.overran(overrun, true);
            }
            Event::StepFailed { error, overrun } => {
                self.close();
                self.say_error(error);
                self.overran(overrun, false);
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
            // note: nothing. The calls a turn asked for are on the turn's own item and are
            // drawn from it, so they arrive with the answer rather than one event later - and
            // they go when it goes, without anybody having to remember which turn proposed them
            Event::ToolRequested { .. } => {}
            Event::ToolOutput { chunk, .. } => self.append(Speaker::Result, &chunk),
            Event::ToolFinished { item, .. } => {
                // whatever streamed in is dropped for the item, which holds what the model was
                // actually given - and the line about what it cost is read off the item too,
                // truncation included, so a resumed session says the same thing this one does
                self.caught_up(item);
                // a fork is charged in a kernel of its own, and this is the first moment after
                // it that this session hears anything
                let forked = self.introspect.as_ref().map_or(0, |it| it.forked());
                if forked > 0 {
                    self.count(forked);
                }
            }
            Event::Compacted { report } => {
                let mut note = format!(
                    "compacted: {}, {} → {} tokens ({})",
                    moved(&report),
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
            // note: a file added to the context is not said out loud here. The chat already draws
            // a line for it off the item, and a second one off this event would say it twice, one
            // above the other, for every `-f` and every `/attach`. The derived line is the one
            // that cannot go out of date, so it is the one that stays. See `App::as_conversation`
            _ => {}
        }
    }

    // ------------------------------------------------------------------- what a caller asks of it

    /// Keeps what an item used to say, so that the viewer can still show it.
    pub(super) fn remember(&mut self, id: ContextId, was: Content) {
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

    /// How many earlier versions of an item are still there to read.
    ///
    /// note: the current content is not one of them, so an item nobody has rewritten answers `0`
    /// and an item rewritten once answers `1` - which is `v1`, with what it says now as `v2`.
    ///
    /// note: an undo is why this is not `Vec::len`. Putting an old content back makes the newest
    /// remembered version the current one as well, and two identical faces side by side say the
    /// same thing twice - so the last is dropped when it matches. The rule lives here rather than
    /// in the terminal's strip of faces, because a client counting versions for a button and a
    /// terminal counting them for a page have to reach the same number.
    pub fn versions(&self, id: ContextId) -> usize {
        let history = self.versions.get(&id).map(Vec::as_slice).unwrap_or(&[]);
        let same = self
            .kernel
            .item(id)
            .is_some_and(|now| history.last() == Some(&now.content));

        history.len() - usize::from(same)
    }

    /// What one of them said, counting from `1` as the oldest.
    ///
    /// note: `None` for `0`, for anything past [`App::versions`], and for an item that has none -
    /// which is one answer for "there is no such version" rather than three ways of saying it.
    /// The number is the one on the face: `v1` is `at = 1`.
    pub fn version(&self, id: ContextId, at: usize) -> Option<Content> {
        (at >= 1 && at <= self.versions(id))
            .then(|| self.versions.get(&id)?.get(at - 1).cloned())
            .flatten()
    }

    /// Adds what a response cost to the session's total, and stops the session if that was the
    /// last of what it was given.
    ///
    /// note: from the event rather than from [`nachalnik::Budget`], which is the kernel's estimate
    /// of the request it is *about to* build. What is added up here is what the provider charged
    /// for the ones already sent - `input + output`, which [`Usage`] defines to be the whole of a
    /// request's bill whichever dialect answered it - so this is a measurement rather than a
    /// conversion. Tokens rather than money because nothing here carries a price list, and a
    /// figure in money would be one: a table per model per endpoint, kept up to date by somebody,
    /// wrong quietly.
    ///
    /// note: a response the provider reported no figures for adds nothing, and the first time that
    /// happens it is said out loud. An endpoint that reports no usage is one this cannot see over,
    /// and a limit quietly never reached is worse than no limit at all: whoever set it would be
    /// reading the session as bounded when nothing is bounding it.
    ///
    /// note: except for a response that was interrupted. A stream cut short never reaches the
    /// chunk the figures ride on, so its silence is about this end rather than the endpoint's -
    /// and saying otherwise after a ctrl+c or a `--deadline` tells whoever set the ceiling it has
    /// stopped meaning anything when it has not. `interrupted` is the stop reason both of
    /// `nachalnik-providers`' dialects give a stream they were told to stop.
    ///
    /// note: it stops *after* the response that crosses the line, because that is the first moment
    /// anybody knows what the response cost. A ceiling is a stopping rule, not a cap: the session
    /// ends having spent a little more than it, and the line says how much.
    ///
    /// note: a `fork`'s request is counted too, when the call that made it finishes: it is money
    /// the session spent, and a model drafting over and over would otherwise spend without limit
    /// under a ceiling that says there is one.
    fn charge(&mut self, usage: Option<Usage>, stop: &StopReason) {
        let Some(usage) = usage else {
            let interrupted = matches!(stop, StopReason::Other(why) if why == "interrupted");
            // said only where it changes something: with no ceiling, a total nobody set a limit on
            // being short by one response is not news
            if self.spend.is_some()
                && !interrupted
                && !std::mem::replace(&mut self.unreported, true)
            {
                self.say(
                    Speaker::Note,
                    "this endpoint reports no usage, so nothing is counted against the ceiling; \
                     only a deadline can stop this session",
                );
            }

            return;
        };

        self.count(usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0));
    }

    /// Adds what a provider charged to what this session has spent, and stops the turn once that
    /// reaches the ceiling; see [`App::charge`].
    fn count(&mut self, tokens: u64) {
        // counted whether or not anything is watching the figure. Added up only under a ceiling,
        // a session that set one half way through would begin from zero, and `/spend` would
        // answer `0 tokens spent` after a turn that plainly cost some
        self.spent = self.spent.saturating_add(tokens);
        let Some(limit) = self.spend else {
            return;
        };
        if self.spent < limit || self.overspent {
            return;
        }
        self.overspent = true;
        self.interrupt();
        self.say(
            Speaker::Note,
            format!(
                "spent {} tokens of {}; stopping. `/spend N` raises the ceiling",
                thousands(self.spent as usize),
                thousands(limit as usize)
            ),
        );
    }

    /// What the provider has charged for this session, as the responses have reported it.
    pub fn spent(&self) -> u64 {
        self.spent
    }

    /// The ceiling that stops it, if anything has set one.
    pub fn spend(&self) -> Option<u64> {
        self.spend
    }

    /// Whether the ceiling has been reached, so that nothing more will be sent.
    ///
    /// note: what a loop reads to decide whether to go on handing lines in. The refusal itself is
    /// [`App::start_turn`]'s, so a caller that does not ask still cannot spend anything; this is
    /// for the caller that would rather stop reading than be told `no` once a line.
    pub fn overspent(&self) -> bool {
        self.overspent
    }

    /// Whether a turn is being asked for after the session has spent what it was given, and says
    /// so if it is.
    ///
    /// note: it speaks, because the alternative is a message that goes into the context and is
    /// never sent with nothing on the screen accounting for it - which is the failure the whole
    /// program is against. Said on each attempt rather than once: every line somebody hands in
    /// gets an answer, and the answer is the same one.
    fn broke(&mut self) -> bool {
        if !self.overspent {
            return false;
        }
        let spent = thousands(self.spent as usize);
        let limit = thousands(self.spend.unwrap_or_default() as usize);
        self.say(
            Speaker::Note,
            format!(
                "nothing more is being sent: {spent} tokens spent of {limit}. `/spend N` \
                     raises the ceiling, and `/spend 0` takes it away"
            ),
        );

        true
    }

    /// Whether a turn is being asked for before anything has been picked to ask, and says so if it
    /// is.
    ///
    /// note: beside [`App::broke`] and for the same reason. A session can start without a model -
    /// see `Setup::wire` - and the kernel refuses that too, with `no provider is set`, which is
    /// true and tells nobody whose move it is. What this adds is the sentence, and it adds it in
    /// the one place every way of starting a turn goes through: a person at the prompt, a line
    /// down a pipe, the message on the command line, a client on a socket.
    fn no_model(&mut self) -> bool {
        if self.kernel.model_info().is_some() {
            return false;
        }
        self.say(
            Speaker::Error,
            format!(
                "nothing is sent until there is a model: `/model ID` picks one, and `/models` \
                 lists what {} serves",
                self.provider.host()
            ),
        );

        true
    }

    /// Stops offering a tool, or offers one this session had turned off; `None` if there is no
    /// tool of that name either way.
    ///
    /// note: the registry is live, so this lands on the next request rather than needing a
    /// restart - the same property [`nachalnik::Kernel::add_tool`] is documented for and the
    /// plainest thing to demonstrate it with. Nothing else moves: what is in the context stays
    /// there, and a result the tool already produced is still a result.
    ///
    /// note: one function for both directions, because they are one act. Two - an `offer` and a
    /// `drop` - would need a caller that knew which state a tool was in before it could ask for
    /// the other one, and the thing asking is a person who read the name off a list.
    ///
    /// note: what it does **not** do is change what the tool is allowed to do. A shelved tool's
    /// permission rules are still in the table, and a tool offered again is under exactly the
    /// rules it was under before - which is the answer a person who turned it off for one turn
    /// wants, and a thing to know before turning one off as a way of stopping it.
    pub fn toggle(&mut self, id: &str) -> Option<bool> {
        if let Some(tool) = self.kernel.remove_tool(id) {
            self.shelved.insert(id.to_owned(), tool);
            return Some(false);
        }

        self.kernel.add_tool(self.shelved.remove(id)?);

        Some(true)
    }

    /// Sets the ceiling, or takes it away, and lets a stopped session carry on under the new one.
    pub fn set_spend(&mut self, limit: Option<u64>) {
        self.spend = limit;
        self.overspent = limit.is_some_and(|limit| self.spent >= limit);
        if self.overspent {
            self.interrupt();
        }
    }

    /// Whether the loop driving this session should let go of it.
    ///
    /// note: the two reasons are not the same thing, and no loop here cares which it is. One ends
    /// the program and the other hands the session back to be built again; both are *stop holding
    /// this `App`*. Which of the two it was is read once, outside, where there is somewhere to go
    /// with the answer.
    pub fn leaving(&self) -> bool {
        self.quit || self.restart
    }

    /// Stops a turn that is still running when the session is left, and takes in what it does
    /// until it has ended; `heard` is handed each event first, for a loop that prints them. Hands
    /// back what the turn failed with, if it did.
    ///
    /// note: what a loop does between [`App::leaving`] and `session.finished`. An interrupt does
    /// not abort a step already in flight, so the rest of a streamed answer and the result of a
    /// running tool are still to come - and a loop that ended at once put them in the old kernel
    /// after `session.finished`, in files already written, with a restarted session running
    /// beside it. Every loop here calls it on the way out however it was left - `/quit`, a
    /// `SIGTERM` or `SIGHUP`, or an error - except a second `ctrl+c`, which means at once.
    ///
    /// note: bounded by [`LEAVING`], because somebody who typed `/quit` is waiting, and a tool
    /// that does not look at its interrupt could hold them for as long as it runs. A turn still
    /// going when the bound runs out is said to be, and is in the record as it stood: its
    /// `turn.interrupted` with no `state.changed` to `idle` after it before `session.finished`.
    pub async fn wait_for_turn(
        &mut self,
        events: &mut tokio::sync::broadcast::Receiver<Event>,
        finished: &mut tokio::sync::mpsc::UnboundedReceiver<Outcome>,
        mut heard: impl FnMut(&Event),
    ) -> Option<String> {
        use tokio::sync::broadcast::error::RecvError;

        // a `/model` or `/provider` still settling first, for the reason the turn is waited for:
        // its change is a record, and a script whose last line is the switch reaches the end of
        // its input with no next line to wait for it. Under the same bound, and left the same way
        if let Some(settling) = self.settling.take() {
            let _ = tokio::time::timeout(LEAVING, settling).await;
        }
        if !self.busy {
            return None;
        }
        self.interrupt();
        let until = tokio::time::Instant::now() + LEAVING;
        while self.busy {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(event) => {
                        heard(&event);
                        self.on_event(event);
                    }
                    // the log has what went by; what is waited for here is the outcome
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return None,
                },
                Some(outcome) = finished.recv() => {
                    // the turn's last events are queued behind its outcome, as in every loop
                    while let Ok(event) = events.try_recv() {
                        heard(&event);
                        self.on_event(event);
                    }
                    let failed = match &outcome {
                        Outcome::Failed(e) => Some(e.clone()),
                        _ => None,
                    };
                    self.on_outcome(outcome);

                    return failed;
                }
                () = tokio::time::sleep_until(until) => {
                    self.say(
                        Speaker::Note,
                        format!(
                            "the turn was still running {} seconds after it was asked to stop, and \
                             was left; the record ends before it does",
                            LEAVING.as_secs()
                        ),
                    );

                    return None;
                }
            }
        }

        None
    }

    /// Asks for this session to be written out and a fresh one put in its place.
    ///
    /// note: it stops a running turn on the way rather than refusing while one is running.
    /// Refusing would make a command that works most of the time and says *not while busy* the
    /// rest, which is the shape somebody types `/restart` to get out of - a turn that has found a
    /// loop is the commonest reason to want one. What the kernel is asked for is the same stop
    /// `esc` asks for, so the turn ends the way an interrupted turn ends and the record has it.
    ///
    /// note: what actually happens is the loop's, not this object's - see [`App::restart`] the
    /// field. This sets the flag and says nothing: the line worth reading is the one naming the
    /// file the old session went to, and there is nothing to name until it has been written.
    pub fn restart(&mut self) {
        if self.busy {
            self.interrupt();
        }
        self.restart = true;
    }

    /// Asks the running turn to stop at the next opportunity.
    ///
    /// note: `pub` for the same reason [`App::submit`] is. `esc` is one caller; a deadline, a
    /// budget ceiling or a request cap watching from another task is another, and the kernel
    /// takes an interrupt from any thread. What it never does is discard what arrived.
    pub fn interrupt(&mut self) {
        if !self.busy {
            return;
        }
        self.interrupting = true;
        self.kernel.interrupt();
    }

    /// Puts an edited item into the context in place of the one it came from, and says whether
    /// that changed anything.
    ///
    /// note: `pub` and not gated on `tui` for the reason [`App::submit`] and [`App::interrupt`]
    /// are. The terminal's `e` is one editor; a browser has a box on the screen that is already
    /// showing what the item says, and needs somewhere to commit it to. Both go through here, so
    /// neither invents a second account of what an edit is.
    ///
    /// note: it asks `text::beyond_a_prompt` itself rather than trusting the caller to have
    /// asked. Both callers do ask first, because refusing after somebody has typed is worse than
    /// refusing before - the terminal before it opens the editor, a client off
    /// [`Listed::beyond`](crate::remote::protocol::Listed::beyond) before it offers one - and
    /// neither of those is a decision. A row is a moment old by the time anybody acts on it, and
    /// a guard that lives at the operation is the one that holds for every caller that ever
    /// arrives.
    ///
    /// note: `Ok(false)` where the text is what the item already said, which is not an error and
    /// is not silent either. Nothing is written - no `context.replaced`, no version page, no undo
    /// checkpoint, because an operation that changes nothing takes none - and the caller is told
    /// so it can say as much rather than report an edit that did not happen.
    ///
    /// note: `reason` is how the item's metadata says where the hand was, and `by` is `user` in
    /// every case because a person is a person wherever they are standing. The `context` tool
    /// writes this same key with `by: context`, and the one thing the two must not do is look
    /// alike: a model reading its own metadata should never find its own tool credited with a
    /// sentence a person rewrote.
    pub fn revise(&mut self, id: ContextId, text: &str, reason: &str) -> Result<bool, String> {
        let Some(old) = self.kernel.item(id) else {
            return Err(format!("[{id}] is no longer there"));
        };
        if let Some(why) = text::beyond_a_prompt(&old) {
            return Err(why.to_owned());
        }
        if old.content.to_text() == text {
            return Ok(false);
        }

        // note: nothing here keeps what it used to say. `Kernel::replace` emits the one event
        // that carries content and `App::remember` is already listening for it, so the version
        // pages fill themselves - which is also why an `undo` of this reaches the screen with
        // nothing here keeping a second account of what to put back
        self.kernel
            .replace(id, text.to_owned())
            .map_err(|e| e.to_string())?;

        let mut meta = match old.meta.is_object() {
            true => old.meta.clone(),
            false => serde_json::json!({}),
        };
        meta["revised"] = serde_json::json!({ "by": "user", "reason": reason });
        let _ = self.kernel.annotate(id, meta);

        Ok(true)
    }

    /// Answers one of the questions the kernel is waiting on, and carries the turn on if that was
    /// the last of them.
    ///
    /// note: here rather than in `keys.rs`, because three loops answer questions - the keys,
    /// `headless.rs` and [`crate::remote`] - and each doing it in its own words would leave each
    /// missing a different part. An answer is four things: telling the sandbox about a granted
    /// command that reaches the network, which is [`App::answer`]; honouring `always` over what
    /// the policy consulted; sweeping the questions queued behind this one; and driving the turn
    /// on, because a decision leaves the kernel resting with nobody driving it. Without the last,
    /// a headless run answers and then sits there.
    ///
    /// note: `remember` is what the `a` key means - *always* - and what it remembers is everything
    /// the policy actually consulted rather than what the tool declared, so a `yes, always` to a
    /// `curl` that left `network` on `ask` does not ask again on the next call. It then sweeps what
    /// is already in the queue, because a model asking for three things at once produces three
    /// questions before the first is shown, and a promise about what happens next has to cover
    /// what is already waiting.
    ///
    /// note: what it returns is about the question the caller asked about, and nothing else. The
    /// sweep's own failures go to [`App::say`], because they are the program reporting something
    /// nobody asked it to do - which is what that list is - and a caller handed them as *its*
    /// error would be told its own answer failed when it did not.
    pub fn decide(&mut self, id: PermissionId, grant: Grant, remember: bool) -> Result<(), String> {
        let Some(request) = self
            .kernel
            .pending_permissions()
            .into_iter()
            .find(|pending| pending.id == id)
        else {
            return Err(format!("there is no question {id} waiting to be answered"));
        };
        // `always` is an allow. What it remembers is every subject the policy consulted, because
        // that is what it takes to let such a call through; a refusal has no such set, and
        // refusing every one of them would refuse every call sharing any. Taken as it came, a
        // client asking never to allow this wrote the rules that allow it
        if remember && grant != Grant::Allow {
            return Err(
                "only an allow is remembered; a standing refusal is a rule, on the permissions tab \
                 or `--deny`"
                    .to_owned(),
            );
        }

        if remember {
            // everything the policy actually consulted, not just what the tool declared - and
            // each one recorded against the question it answered, so that the calls it lets through
            // later say where their permission came from
            let judged = self.policy.judges(&request);
            self.policy.always(&judged);
            for subject in &judged {
                self.kernel
                    .record_rule(subject.to_string(), Verdict::Allow, Some(id), false);
            }
        }
        // the other thing a session waits on somebody for. Whatever the question cost in wall time
        // was spent reading it, and `permission.decided` is the line it lands on
        self.acted = true;
        self.answer(&request, grant)?;
        if remember {
            for waiting in self.kernel.pending_permissions() {
                // note: `App::answer` rather than `Kernel::decide`, because a swept question is
                // answered rather than merely decided. A model that asks for `ls` and `curl` in one
                // breath produces two questions, and `a` on the first is what lets the second
                // through - so the second has to be let through the same door, network grant and
                // all. Decided straight into the kernel, it would run with the network cut and
                // nothing anywhere saying why
                if self.policy.verdict(&waiting) == Verdict::Allow
                    && let Err(e) = self.answer(&waiting, Grant::Allow)
                {
                    self.say(Speaker::Error, e);
                }
            }
        }
        // the model may have asked for several things at once, and each is its own question
        if self.kernel.pending_permissions().is_empty() {
            match self.stepping {
                // somebody driving this a transition at a time did not ask for the rest of the
                // turn, and running it here would be the harness taking the wheel back
                true => self.say(
                    Speaker::Note,
                    "decided; /step runs the calls, /continue runs the rest of the turn",
                ),
                false => self.start_turn(),
            }
        }

        Ok(())
    }

    /// Answers a running command that reached for the network, and writes the answer down.
    ///
    /// note: the counterpart of [`App::decide`] for the question the kernel does not ask, and it is
    /// where all three loops answer one for the same reason that one exists: an answer is more
    /// than the command hearing it. It is a `policy.ruled` in the record - the kernel has no event
    /// for this question, so the rule is the paper trail - and with `remember` it is `net:reach`
    /// allowed from here on, which lets through every command already waiting on the same
    /// question.
    ///
    /// note: the command reads the answer, not the model: its sockets open or come back
    /// `Permission denied`, and the result it hands the model says which the person chose.
    pub fn decide_reach(&mut self, id: u64, grant: Grant, remember: bool) -> Result<(), String> {
        if remember && grant != Grant::Allow {
            return Err(
                "only an allow is remembered; a standing refusal is a rule, on the permissions tab \
                 or `--deny`"
                    .to_owned(),
            );
        }
        let allow = grant == Grant::Allow;
        self.policy.reaching().answer(id, allow)?;

        let subject = Subject::Capability(Capability::net("reach"));
        if remember {
            self.policy.set(&subject, Verdict::Allow);
            for waiting in self.policy.reaching().waiting() {
                let _ = self.policy.reaching().answer(waiting.id, true);
            }
        }
        self.kernel.record_rule(
            subject.to_string(),
            match allow {
                true => Verdict::Allow,
                false => Verdict::Deny,
            },
            None,
            !remember,
        );
        self.acted = true;

        Ok(())
    }

    /// What the *program* has said since the last look, for a loop that has to print it.
    ///
    /// note: only [`Speaker::Note`] and [`Speaker::Error`]. The model's own words arrive as
    /// fragments and land in the same list, so a caller echoing those as well prints every answer
    /// twice.
    ///
    /// note: `said` is how many of *these* have been taken rather than how far down
    /// [`App::loose`] the caller had got. The list is not append-only - a turn being recorded
    /// takes every line that streamed out of it, because the context says those now - so a mark
    /// against the whole list slides backwards under its own watermark, and everything said
    /// between one shrink and the next is skipped: a line saying the ceiling was reached can be
    /// decided, recorded and acted on and still never reach the person. A scripted model answers
    /// between two looks, so nothing shrinks in between and a test driving one does not see it.
    /// What the filtered sequence *is* is append-only: a note is not something that streams.
    ///
    /// note: with one exception, and it is `/cleanup`. [`App::clear_notices`] empties this sequence
    /// rather than shortening it, so a watermark against it is stale in the one direction that is
    /// silent. [`App::cleared`] is what says so, and every caller of this has to read it.
    pub fn notes(&self, said: usize) -> impl Iterator<Item = &Entry> {
        self.loose
            .iter()
            .filter(|entry| matches!(entry.speaker, Speaker::Note | Speaker::Error))
            .skip(said)
    }

    /// The message waiting for the running turn to end, if there is one.
    ///
    /// note: there is room for exactly one, which is a decision a prompt can live with and a
    /// second client cannot. Two people attached to one session who both type during a turn
    /// produce one message and two [`Did::Queued`]s, and the one whose line was replaced is never
    /// told. Whoever hands a line in on somebody else's behalf reads this first and says so; see
    /// [`crate::remote`], which is the caller that made it worth exposing.
    pub fn queued(&self) -> Option<&str> {
        self.typed_ahead.as_deref()
    }

    /// Puts the prompt back to composing a message, whatever it was doing.
    pub(super) fn cancel_edit(&mut self) {
        if self.editing.take().is_some() {
            #[cfg(feature = "tui")]
            self.clear_input();
        }
    }

    /// Puts something long on the screen.
    pub(super) fn preview(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.preview_pages(
            title,
            vec![Page {
                name: String::new(),
                body: body.into(),
            }],
            0,
        );
    }

    /// The key reference, opened at whichever page is about where somebody is standing.
    ///
    /// note: this is the one place that knows a tab has a page, and it is here rather than in
    /// `help` because the mapping is a fact about the program: `help` holds the words and has no
    /// business knowing which tab `G` belongs to. Every section is still offered - `←` and `→`
    /// reach the rest - so what this decides is the *first* page, not which of them exist.
    ///
    /// note: a waiting question wins over the chat tab it is pinned to, because somebody pressing
    /// F1 with a tool waiting is asking about the thing that is blocking them. From another tab it
    /// does not, since what they are looking at is that tab; the page is still one key away, and
    /// the tab strip is already red to say the question is there.
    pub(super) fn help(&mut self) {
        let asked = self.asking();
        let here = match (self.tab, asked) {
            (Tab::Chat, true) => "question",
            (Tab::Chat, false) => "chat",
            (Tab::Context, _) => "context",
            (Tab::Trace, _) => "trace",
            (Tab::Permissions, _) => "permissions",
        };

        let offered: Vec<&crate::help::Section> = crate::help::SECTIONS
            .iter()
            .filter(|section| section.applies(asked, self.keys))
            .collect();
        // note: found rather than computed, because a section that is not offered shifts every
        // index after it - `question` is left out whenever nothing is waiting, which is nearly
        // always, and an index counted against `SECTIONS` would open `context` on the trace tab
        let at = offered
            .iter()
            .position(|section| section.name == here)
            .unwrap_or_default();
        let pages = offered
            .into_iter()
            .map(|section| Page {
                name: section.name.to_owned(),
                body: section.body.to_owned(),
            })
            .collect();

        // note: and what it is *called* follows the same rule. A caller with no keys is handed a
        // page of slash commands, and titled `the keys` the panel would be telling it that what it
        // is reading is the thing it has not got
        let title = match self.keys {
            true => "the keys",
            false => "the commands",
        };

        self.preview_pages(title, pages, at);
    }

    /// The same, for something with more than one face; `at` is the one to open on.
    fn preview_pages(&mut self, title: impl Into<String>, pages: Vec<Page>, at: usize) {
        self.previews += 1;
        self.overlay = Some(Overlay::Text {
            title: title.into(),
            page: at.min(pages.len().saturating_sub(1)),
            pages,
            scroll: 0,
        });
    }

    // ----------------------------------------------------------------------------------- keys

    /// Takes in one key press.
    #[cfg(feature = "tui")]
    pub async fn on_key(&mut self, key: KeyEvent) {
        // windows reports both halves of every press; everywhere else this is already true
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // taken by every key, whichever handler it reaches; see the field
        let drawn = self.drawn.take();

        // before anything else, including whatever is on top: these two mean the same thing
        // wherever they are pressed, and an overlay that took them for its own would be answering
        // a question nobody asked: `d` is a key at a permission question, and `ctrl+d` reaching it
        // would drop every pending call
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d')) {
            match self.busy && key.code == KeyCode::Char('c') {
                true => self.interrupt(),
                false => self.quit = true,
            }
            return;
        }

        if self.overlay.is_some() {
            self.overlay_key(key).await;
            return;
        }

        // the search box has the keys while it is open, and takes them before the arm below that
        // reads `esc` as "stop the turn". Not a loss of that gesture: `ctrl+c` is what interrupts
        // a run and it is handled above this, where nothing can shadow it. What would be a loss is
        // a box on the screen that `esc` does not close, which is the one thing everybody tries
        if self.search.is_some() && self.search_key(key) {
            return;
        }

        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // taken rather than read, so that a count lives for exactly one key wherever that key is
        // handled: only the digit arm below puts it back. Cleared at the end of `context_key`
        // instead, a `4` followed by `tab` or `F1` - neither of which gets that far - would survive
        // to send the next `G` to item 4
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
            // `ctrl+l` is what it is in every shell, narrowed to the only thing on this screen
            // that is safe to clear: the program's own lines. Everything else on the chat is the
            // context, and a key that took *that* off the screen would be hiding the thing the
            // screen is for - `space` on the context tab is how something stops being sent, and
            // it says so on the row afterwards
            (KeyCode::Char('l'), true) => self.clear_notices(),
            // the two ends of the conversation, one key each. With control held, because `home`
            // and `end` are the prompt's own - a prompt whose keys moved something else while
            // somebody was editing a line would be the trap
            (KeyCode::Home, true) => {
                self.scroll = 0;
                self.follow = false;
            }
            (KeyCode::End, true) => self.follow = true,
            (KeyCode::F(1), _) => self.help(),
            // `?` is what F1 is on the three tabs with no prompt for a letter to be typed into,
            // and it is answered here rather than in each of their handlers because two of the
            // three would swallow it: `context_key` and `permissions_key` return early when
            // their list is empty, which is exactly the moment somebody is most likely to ask what
            // the keys are. `Focus::Body` is the guard rather than the tab alone, so that the `?`
            // of a sentence typed into an item being edited is still a `?`
            (KeyCode::Char('?'), false) if self.tab != Tab::Chat && self.focus == Focus::Body => {
                self.help()
            }
            // `tab` moves the keys to the other thing on the screen that wants them, and on a tab
            // with no prompt there is no other thing - so it means the one gesture that is always
            // worth having: back to where typing happens
            (KeyCode::Tab, _) => match (self.tab, self.asking()) {
                // it is what puts the keys on a waiting question, and the only thing that does
                // from this tab. There is nothing to hand them back to until the question is
                // answered - it has the prompt's place - so pressing it again is not a way out
                (Tab::Chat, true) => self.focus = Focus::Body,
                // the conversation is read rather than operated, so the only thing on the chat tab
                // that takes keys of its own is a question, and only while there is one
                (Tab::Chat, false) => {}
                // an edit is the prompt doing a job for the tab underneath it, and both halves
                // take keys
                _ if self.prompted() => self.flip_focus(),
                _ => self.show(Tab::Chat),
            },
            _ => match (self.tab, self.focus) {
                // a chord nothing above took is somebody reaching for a key that works somewhere
                // else - readline's `ctrl+a`, a terminal's `ctrl+y` - and not for the letter, the
                // reasoning the search box already follows. Where a letter is an answer or a rule,
                // reading one as its letter had `ctrl+a` at a question answer *always*, and write
                // the rule that goes with it
                _ if (ctrl || alt)
                    && self.focus == Focus::Body
                    && matches!(key.code, KeyCode::Char(_)) => {}
                // the pinned question, which is what `Focus::Body` means on the chat tab
                (Tab::Chat, Focus::Body) => self.question_key(key).await,
                // a question is on the screen and has not been given the keys, so the prompt is
                // not on the screen either and there is nothing here for a key to do
                (Tab::Chat, Focus::Input) if self.asking() => self.locked_key(key),
                (Tab::Context, Focus::Body) => self.context_key(key, &count, drawn),
                (Tab::Trace, Focus::Body) => self.trace_key(key),
                (Tab::Permissions, Focus::Body) => self.permissions_key(key),
                _ => self.input_key(key).await,
            },
        }
    }

    /// Takes the program's own lines off the chat: what it said about what it did, and what it
    /// answered a key with.
    ///
    /// note: the chat is two things drawn as one - the conversation, which is read off the
    /// context every frame, and the lines this program said about it, which are not in the
    /// context and not anybody's turn. Only the second kind goes. An item stays until something
    /// changes *the context*, which is the context tab's to do and says so on the row.
    ///
    /// note: they pile up because each of them was worth saying once. Eleven items excluded one
    /// at a time is eleven lines that have done their job, and the run they are interleaved with
    /// is the thing somebody is trying to read.
    ///
    /// note: nothing is said to say it happened, which is the one place this program says
    /// nothing on purpose: a line reporting that the lines are gone would be the first line of
    /// the pile it just cleared. What went is on the trace, which is the record and is not
    /// touched by this.
    ///
    /// note: an entry still arriving stays, because it is not finished being said. Only a
    /// streamed answer is ever open, and it is a turn rather than a notice - but the guard is
    /// here rather than left to the speaker, since what must never happen is a line vanishing
    /// mid-sentence.
    ///
    /// note: it counts, and [`App::cleared`] is the count. A loop with no screen reads its lines
    /// through [`App::notes`], which is a watermark over the filtered sequence and is safe only
    /// while that sequence grows - and this is the one thing that empties it. A caller that did
    /// not notice would skip the next `said` lines for good.
    pub fn clear_notices(&mut self) {
        self.loose
            .retain(|entry| entry.open || !matches!(entry.speaker, Speaker::Note | Speaker::Error));
        self.cleared += 1;
    }

    /// Moves one item to the next state in the ring: seen, then a marker where it was, then
    /// nothing at all, then seen again.
    ///
    /// note: here rather than in `keys.rs`, for the reason [`App::decide`] is here: a second
    /// client turns the same ring, and each would otherwise keep its own copy of its shape. The
    /// two halves that must not be copied are the order and the notes: the notes are read by the
    /// *model*, in the brackets the projector puts round them, so a page writing its own words for
    /// the same act would put two accounts of one thing in front of it.
    ///
    /// note: a cycle rather than three buttons, and the middle step is the one that earns it.
    /// Taking a tool result out makes the projector drop the call that asked for it, so the model
    /// reads a conversation it never had; elided, the call keeps its answer and only the content
    /// is gone. Which of the two somebody wants is not something this program can guess.
    ///
    /// note: a pinned item goes to elided like an active one, rather than refusing. Pinning is a
    /// promise to the *compactor* and not to the person who made it, and somebody who pins
    /// something and then hides it by hand has not contradicted themselves.
    pub fn cycle(&mut self, id: ContextId) -> Result<ContextState, String> {
        let Some(item) = self.kernel.item(id) else {
            return Err(format!("there is no item {id}"));
        };
        let (to, note) = match item.state {
            // note: this one is read by the model, in the brackets the projector puts round it,
            // so it is written for somebody who has never heard of this program: no "at the
            // terminal", which is this codebase's own idiom for "a person did it here" and reads
            // to a model like a shell or a state. It does not invite the model to ask for it back
            // either - the thing hidden may be the thing that should not be asked for
            ContextState::Active | ContextState::Pinned => (
                ContextState::Elided,
                Some("removed from view by the user".into()),
            ),
            // note: no "at the terminal" here either, and for a reason of its own rather than the
            // one above: this ring is reachable from a socket, a pipe and a browser, so the person
            // who did it may never have seen one. It is read back in `Projection::skipped`, which
            // is what a client draws beside the row to say why the item is not in the request
            ContextState::Elided => (ContextState::Excluded, Some("taken out by the user".into())),
            _ => (ContextState::Active, None),
        };
        self.kernel.set_state([id], to, note);

        Ok(to)
    }

    /// How many times the program's own lines have been taken off the chat.
    ///
    /// note: for a caller holding a watermark into [`App::notes`], and it is all such a caller
    /// has to do about `/cleanup`: keep this beside `said`, and when it moves, set
    /// `said` back to nothing. There is no arithmetic to do, because what a clear leaves behind
    /// is not a shorter sequence but an empty one - everything it removes is exactly what `notes`
    /// filters *for*, and the only survivor is a line still being streamed, which is a model
    /// speaking and not a notice.
    pub fn cleared(&self) -> u64 {
        self.cleared
    }

    /// Opens a tab, and puts the keys wherever they are useful on it.
    ///
    /// note: switching to the context or the trace is something somebody does in order to work on
    /// it, so the focus follows - and there is nothing else on those tabs for it to be on. On the
    /// conversation the keys go to the prompt, unless something is being asked: coming back to a
    /// waiting question is what somebody does *in order to answer it*, having just been away
    /// looking at what it is about, and making them press `tab` first would be asking twice.
    pub fn show(&mut self, tab: Tab) {
        // an edit belongs to the tab it was started from, and leaving that tab abandons it.
        // Otherwise the prompt is still holding the item's text with `editing` still set, and the
        // next message somebody types and sends is committed into the context instead of asked
        self.cancel_edit();
        // and so does a filter. A query written against the trace means nothing against the
        // context, and one left running on a pane somebody comes back to is a pane showing four
        // rows of eight hundred with its explanation on another tab
        self.search = None;

        self.tab = tab;
        self.focus = match (tab, self.asking()) {
            (Tab::Chat, false) => Focus::Input,
            _ => Focus::Body,
        };
    }

    /// The question a tool is waiting on, if one is.
    ///
    /// note: asked of the kernel every time rather than held here. What stands in the prompt's
    /// place is a *rendering* of the kernel's state, so it cannot be open when there is nothing to
    /// answer, or shut when there is - and [`App::prompted`] reads it for the same reason.
    pub fn asked(&self) -> Option<PermissionRequest> {
        self.kernel.pending_permissions().into_iter().next()
    }

    /// The running command waiting to hear whether it may reach the network, if one is.
    ///
    /// note: the other kind of question a tool waits on, and the one the kernel knows nothing
    /// about: it is asked while the call runs rather than before, by the gate holding the
    /// command's first attempt at an internet socket. See [`crate::gate`] and
    /// [`App::decide_reach`].
    pub fn reached(&self) -> Option<crate::tools::Reached> {
        self.policy.reaching().first()
    }

    /// Whether anything is standing in the prompt's place, waiting to be answered.
    ///
    /// note: the two kinds are a tool waiting on a decision and a compaction waiting on one, and
    /// everything about the screen that cares - what has the keys, what `tab` reaches, whether
    /// there is a prompt at all - cares only that there is one. Which it is, is a question for
    /// the panel that draws it and the key that answers it.
    pub fn asking(&self) -> bool {
        self.asked().is_some() || self.reached().is_some() || self.proposed.is_some()
    }

    /// Answers the compaction standing in the prompt's place.
    ///
    /// note: `take` works the pass out again rather than applying what was listed. The list is a
    /// snapshot of a context somebody has just been invited to change, so applying it would take
    /// exactly what they had protected while reading it. The kernel refuses a pinned item and
    /// says so, which would catch it - afterwards, in a report, which is what this question
    /// exists to avoid.
    ///
    /// note: public because the screen is not the only thing entitled to answer. `--headless` has
    /// no keys and answers this itself; see the note there for why it takes it rather than
    /// refusing it the way it refuses a tool's question.
    pub async fn take_proposal(&mut self, take: bool) {
        self.proposed = None;
        self.focus = Focus::Input;
        // the next question starts at the top of itself, whatever was being read in this one
        self.question_scroll = 0;
        if !take {
            self.say(Speaker::Note, "left alone; nothing was compacted");
            return;
        }

        let Some(compactor) = self.kernel.compactor() else {
            return;
        };
        match compactor
            .plan(&self.kernel.items(), &self.kernel.budget())
            .await
        {
            // the report is said by `Event::Compacted`, like any other pass: one account of a
            // compaction, whoever asked for it
            Some(plan) => {
                self.kernel.apply_compaction(plan);
            }
            None => self.say(
                Speaker::Note,
                "nothing left to take: everything the pass had listed is pinned now",
            ),
        }
    }

    /// Whether the prompt is on the screen at all.
    ///
    /// note: it belongs to the conversation, rather than being under every tab so that a message
    /// can be sent from anywhere. That would cost a mode on three tabs that have no use for one:
    /// every key on them would be either a key or a letter depending on where the focus happened
    /// to be, and forgetting would be a `space` typed into a message instead of cycling the row
    /// somebody was looking at. The exception is an edit, which is the prompt doing a job for the
    /// tab underneath it: the item being rewritten is on that tab, and the box has to be beside
    /// it.
    ///
    /// note: and a waiting question takes its place rather than stacking above it, so that the box
    /// the keys are in is the box on the screen. Stacked, the two disagree on a short window: the
    /// question needs the room, so the prompt gives way - and goes on holding the keys and
    /// whatever had been typed into it from off the screen, which is a session waiting on an
    /// answer nobody can give it without first pressing a key nothing mentions. What was typed is
    /// not lost; the box comes back with it, and `App::locked_key` is what stands between a
    /// keystroke and a prompt that is not there.
    pub fn prompted(&self) -> bool {
        match self.tab {
            Tab::Chat => !self.asking(),
            _ => self.editing.is_some(),
        }
    }

    /// Puts pasted text into the prompt.
    ///
    /// note: the line breaks inside a paste arrive as carriage returns rather than newlines,
    /// because a terminal sends a paste as though it had been typed and that is what the enter key
    /// sends. The editor underneath splits on newlines, so a pasted stack trace would go in as one
    /// line with invisible characters where its breaks were and read as its lines run together -
    /// in the one place whose whole job is to show somebody what they are about to send.
    #[cfg(feature = "tui")]
    pub fn paste(&mut self, text: &str) {
        self.input
            .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
    }

    /// What is in the prompt.
    #[cfg(feature = "tui")]
    fn draft(&self) -> String {
        self.input.lines().join("\n")
    }

    /// Nothing: there is no prompt in a build with no screen, so nothing is half-typed.
    ///
    /// note: a method rather than a `#[cfg]` at each of the two places that ask, because what
    /// they ask for is a *figure* - `/budget` reports it and the status line adds it in - and an
    /// empty draft is the honest answer to both. The alternative was gating [`App::drafted`],
    /// which would have put a `#[cfg]` in the middle of `/budget`.
    #[cfg(not(feature = "tui"))]
    fn draft(&self) -> String {
        String::new()
    }

    // ---------------------------------------------------------------- the session, written out

    /// A name for a session, from the seconds since the epoch it started at.
    ///
    /// note: this is the session's identity *and* the name of the two files it leaves behind.
    /// Those go in a directory called `kamchatka`, so the program's name in a filename would say
    /// what the directory already says, and a bare count of seconds says nothing to anybody
    /// reading it. `2026-09-08T06-45-17Z` names the session, sorts the same way, and answers the
    /// question somebody is looking at a list of them to ask.
    ///
    /// note: UTC, and it says so, because a local time would be a name that quietly means
    /// something different depending on where it was written.
    ///
    /// note: to the second, so two sessions started inside one second collide. The identifier a
    /// session gets from the runtime by default is a counter that restarts with the process,
    /// which is fine as an identity and writes over the last session's record.
    pub fn session_stamp(secs: u64) -> String {
        // note: the epoch where the seconds are past what the calendar holds, year 9999, which is
        // not a moment this program is started at
        let at = i64::try_from(secs)
            .ok()
            .and_then(|secs| time::OffsetDateTime::from_unix_timestamp(secs).ok())
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);

        format!(
            "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}Z",
            at.year(),
            u8::from(at.month()),
            at.day(),
            at.hour(),
            at.minute(),
            at.second()
        )
    }

    /// Writes the event log and a resumable snapshot, and says how many records that was.
    ///
    /// note: separate from `save` because the last write of a session happens after the terminal
    /// has been restored, where `say` has nowhere to put a sentence. Both go through here so that
    /// what `/save` produces and what a session leaves behind on its way out are the same pair of
    /// files, written the same way.
    ///
    /// note: the snapshot first, and the log only up to the record it names. A snapshot reads the
    /// log's last number under the lock every change to the context is announced under, so those
    /// records are exactly the ones whose changes it shows - and a pair written while a turn runs
    /// agrees rather than holding items whose `context.added` came after the log was read.
    ///
    /// note: each file is written beside itself, flushed to disk and renamed over, both before
    /// either is renamed. `/save good` a second time is the checkpoint `/load good` is for, and an
    /// overwrite that ran out of disk half way would have destroyed the one it was replacing.
    pub fn write_session(&self, log: &str, state: &str) -> Result<usize, String> {
        let snapshot = self.kernel.snapshot();
        let history = self.kernel.history();
        let records = history
            .iter()
            .filter(|record| record.seq <= snapshot.last_seq)
            .map(|record| {
                serde_json::to_string(record)
                    .map_err(|e| format!("could not render record {}: {e}", record.seq))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let snapshot = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| format!("could not render the session: {e}"))?;

        // named, because "No such file or directory" on its own leaves somebody guessing which
        // one; `-r` says which file it could not read and this should match it
        let log_beside = beside(log, (records.join("\n") + "\n").as_bytes())
            .map_err(|e| format!("could not write {log}: {e}"))?;
        let state_beside = match beside(state, &snapshot) {
            Ok(it) => it,
            Err(e) => {
                let _ = std::fs::remove_file(&log_beside);
                return Err(format!("could not write {state}: {e}"));
            }
        };
        std::fs::rename(&log_beside, log).map_err(|e| {
            let _ = std::fs::remove_file(&log_beside);
            let _ = std::fs::remove_file(&state_beside);
            format!("could not write {log}: {e}")
        })?;
        std::fs::rename(&state_beside, state).map_err(|e| {
            let _ = std::fs::remove_file(&state_beside);
            format!("wrote {log}, and could not write {state} beside it: {e}")
        })?;

        Ok(records.len())
    }
}

/// Writes `bytes` to a new file beside `path`, flushed to disk, for a rename to put in its place.
fn beside(path: &str, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write as _;

    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // `create_new`, which follows no link, because `/save` names a path anywhere - a shared
    // directory included - and a name somebody predicted could be a link they left there
    let (at, mut file) = loop {
        let at = std::path::PathBuf::from(format!(
            "{path}.{}.{}.writing",
            std::process::id(),
            MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&at)
        {
            Ok(file) => break (at, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(e) = written {
        let _ = std::fs::remove_file(&at);
        return Err(e);
    }

    Ok(at)
}
