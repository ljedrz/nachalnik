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
use nachalnik::{Content, ContextId, ContextState, Event, Kernel, State, Tool, Verdict};
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
mod events;
mod going;
mod questions;
mod search;
mod session;
mod spend;
mod transcript;
mod turn;
mod views;

pub use going::Going;
pub use search::Search;
pub(crate) use session::named_calls;
pub use transcript::{Entry, Said, Speaker};
use turn::Errand;
pub use turn::{Outcome, Returned};
pub use views::Stance;
#[cfg(feature = "tui")]
mod keys;
pub(crate) mod text;
pub mod when;

pub(crate) use session::{beside, without_suffix};

use text::{one_line, plural};
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

/// How often a loop with nothing drawing it looks for a notice while a turn runs; see
/// [`App::take_notices`].
///
/// note: the drawn loop has a tick of its own and reads them on that.
pub(crate) const NOTICES: std::time::Duration = std::time::Duration::from_millis(120);

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
    /// A turn was already running, so it waits for the end of that one and then gets its own; or
    /// a command was still out at the endpoint, so the line waits for that and is handed in
    /// after, whatever it turns out to be. See [`App::in_flight`].
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
    /// A `/model` or `/endpoint` still settling, which the next line waits for.
    ///
    /// note: both commands hand the switch to a task rather than standing there while it happens,
    /// because finding out what the new model holds and whether the new address serves it is two
    /// round trips and a screen should not stop for them. What the *next line* may not do is read
    /// a session that has not finished changing: `/endpoint URL ID` followed by `/model` would
    /// report the old model, and a message on the line after a switch could be asked of whichever
    /// of the two won the race. Down a pipe there is no gap between the lines at all, so what is a
    /// race at a keyboard is the ordinary case in a script.
    ///
    /// note: held for in [`App::submit`] rather than anywhere the provider is read, which is the
    /// narrower door and the right one: a frame drawn mid-switch showing the old name for a
    /// moment is a frame, and the next one corrects it. A *line* acting on the old name is an
    /// answer. Every question a switch asks the endpoint is bounded by the provider, so this
    /// cannot hold lines for ever. See [`App::in_flight`].
    pub settling: Option<tokio::task::JoinHandle<()>>,
    /// A command's request still out at the endpoint - `/models`, or a compaction pass being
    /// worked out - which [`App::on_outcome`] finishes when it comes back.
    ///
    /// note: one, because nothing is handed a line while it is out; see [`App::in_flight`].
    errand: Option<Errand>,
    /// How many errands have been sent, which is what numbers them.
    errands: u64,
    /// Lines handed to [`App::submit`] while a command was in flight, oldest first, each with
    /// whether whoever sent it had keys to press; for [`App::release`].
    held: VecDeque<(String, bool)>,
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
    /// Why the sandbox did not take, where it was asked for and did not and something said why;
    /// see [`crate::sandbox::Probed::why`].
    pub unconfined_because: Option<String>,
    /// What the shell's commands left running, for whoever ends the session to stop or name; see
    /// [`crate::tools::Stragglers`].
    pub stragglers: crate::tools::Stragglers,
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
    /// The colour of the window's frame, and of everything drawn in it to say *the keys are
    /// here*: the active tab, the prompt while it has them, an answerable question.
    /// [`config::BORDER_COLOR`](crate::config::BORDER_COLOR) unless a settings file says otherwise.
    ///
    /// note: one field rather than one per border, because they are one statement. All four are
    /// this colour to say the same thing, and a setting that moved three of them would leave the
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
    /// that is excluded, elided or repaired away.
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
    /// The line [`App::copy`] said about what is in [`App::clipboard`], and the item it was about,
    /// for [`App::not_copied`] to take back.
    ///
    /// note: what is being taken back is a receipt, not a retraction. The text has left the
    /// program either way and the loop is the only thing that can find out what happened to it,
    /// so the line saying it is rewritten once, by whoever knows. See `App::not_copied`.
    receipt: Option<(String, ContextId)>,
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
    /// Whether the last turn stopped at the request budget rather than ending; see
    /// [`App::paused`].
    paused: bool,
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
    /// The messages somebody sent into a turn that was already running, oldest first, each
    /// waiting for a turn of its own.
    ///
    /// note: a queue rather than one slot, and each stays its own message. A session drawn at a
    /// desk and served is two ways in, and a slot the newest won took one person's line away
    /// whenever the other typed after them; merging them instead would hand the model two people's words as
    /// one. So every turn that ends takes the oldest in and gives it a turn, and the next waits for
    /// that one. [`App::put_back`] is the way back to the newest - `up` takes it out of here and
    /// into the prompt, where it can be changed, sent again or simply dropped. Each is drawn at the
    /// end of the conversation, in order, until it becomes an item.
    typed_ahead: VecDeque<String>,
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
    /// The session's record on disk, written as it goes, where one was started.
    ///
    /// note: here rather than on a loop, because [`App::on_event`] is the door every loop comes
    /// through and a record kept by one loop is a record the other two do not write. `main.rs`
    /// starts one with [`crate::wiring::Recorder::start`] unless `--no-record` says otherwise; an
    /// embedder that wants the same safety net puts one here.
    pub recorder: Option<crate::wiring::Recorder>,
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
    /// The sequence number of the last record in the log that [`App::charge`] has seen.
    ///
    /// note: the log rather than the broadcast, because a broadcast that falls behind drops
    /// events, `model.finished` among them, and a response dropped there was never counted - so a
    /// ceiling could be passed by whatever the lag took. The log drops nothing, and nothing in this
    /// program drains it.
    charged: u64,
    /// How much of [`Advised::spent`](crate::tools::Advised::spent) has been charged, which is
    /// how much of `spent` was the advisor's.
    #[cfg(feature = "shell-advisor")]
    advised: u64,
    /// Whether the ceiling has been reached, so that nothing else is sent until somebody says so.
    overspent: bool,
    /// Whether the last turn was refused for a request longer than the model takes, until another
    /// one starts.
    oversized: bool,
    /// Whether it has already said that the endpoint reports no figures to add up.
    ///
    /// note: a property of the endpoint rather than news about a turn, like `thought_unseen` above
    /// and for the same reason.
    unreported: bool,
    /// The files `/save` wrote this session to in each directory it was given, by directory.
    ///
    /// note: so that saving into one again replaces this sitting's own pair, which is the
    /// ordinary case, while the first save into it takes a name nothing else has written. A
    /// session's name is shared with the session it resumed and with any other started in the
    /// same second, so the name alone would have written over theirs.
    saved_into: BTreeMap<std::path::PathBuf, (String, String)>,
    /// What the last request failed with, while no answer has come since.
    ///
    /// note: for `/raw`. The kernel keeps the last *answer*, and a request that failed leaves the
    /// one before it standing, which shown on its own would be an older answer passed off as the
    /// provider's latest.
    unanswered: Option<String>,
    /// The fraction of the limit this program's own compactor aims for, when that is the one
    /// plugged in.
    ///
    /// note: for `/compact`, to tell a context already under the target from one with nothing
    /// eligible, both of which come back from `plan` as no plan. Known here rather than asked of
    /// the compactor, because the `Compactor` trait has no such question and a client's message is
    /// no reason to give it one. Whoever plugs in a compactor says what it aims for here, and
    /// `None` is the sentence that does not claim to know.
    pub compact_target: Option<f64>,
    /// The fraction of the limit at which that compactor starts making room, for the same
    /// question asked of a context between the two.
    ///
    /// note: `Shedder` makes room only past it, so a context between the target and the
    /// threshold is one with nothing to do rather than one with nothing eligible - and measured
    /// against the target alone, `/compact` there said the second.
    pub compact_threshold: Option<f64>,
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
        // what the kernel did before this was here to watch is not this session's to charge
        let charged = kernel.with_history(|log| log.last_seq());

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
            confinement: Confinement::Off,
            unconfined_because: None,
            stragglers: crate::tools::Stragglers::default(),
            anchor: None,
            pending: Anchor::default(),
            failed: None,
            loose: Vec::new(),
            cleared: 0,
            trace: VecDeque::new(),
            #[cfg(feature = "tui")]
            input,
            // note: the same colour the program draws with when nothing says otherwise, so that
            // an embedder's window and this program's are one window. Read off the constant
            // rather than written out again, and it cannot fail - it is a hex the suite parses
            #[cfg(feature = "tui")]
            accent: {
                let (r, g, b) = crate::config::rgb(crate::config::BORDER_COLOR)
                    .expect("the default border colour is a colour");
                ratatui::style::Color::Rgb(r, g, b)
            },
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
            receipt: None,
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
            paused: false,
            thought_unseen: false,
            unanswered: None,
            compact_target: None,
            compact_threshold: None,
            reported_repairs: Vec::new(),
            since: Instant::now(),
            question_scroll: 0,
            settling: None,
            errand: None,
            errands: 0,
            held: VecDeque::new(),
            proposed: None,
            typed_ahead: VecDeque::new(),
            last_sent: None,
            #[cfg(feature = "tui")]
            recalled: None,
            streamed_bytes: 0,
            outcomes,
            recorder: None,
            previews: 0,
            spent: 0,
            charged,
            #[cfg(feature = "shell-advisor")]
            advised: 0,
            overspent: false,
            oversized: false,
            unreported: false,
            saved_into: BTreeMap::new(),
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
        let offered = match self.kernel.remove_tool(id) {
            Some(tool) => {
                self.shelved.insert(id.to_owned(), tool);
                false
            }
            None => {
                self.kernel.add_tool(self.shelved.remove(id)?);
                true
            }
        };
        self.policy.withdraw(id, !offered);
        self.refresh_full_notice();

        Some(offered)
    }

    /// Words the notice the model is told the context is full with for what it could use to make
    /// room now, and rewrites a copy of it standing in the context to match.
    ///
    /// note: called wherever that can change - a tool toggled, a rule set, a headless run saying
    /// what a question nobody answers comes to. The standing copy is replaced where it stands
    /// rather than left: the kernel knows a notice it placed by what it says, so a copy in the old
    /// words would be one it never took out again, and would go on sending the model to a tool it
    /// no longer has. A session given no notice is given none here.
    pub fn refresh_full_notice(&self) {
        let Some(standing) = self.kernel.full_notice() else {
            return;
        };
        let wanted = crate::wiring::full_notice(&self.kernel, &self.policy);
        if wanted.content == standing.content {
            return;
        }
        for item in self.kernel.items() {
            if item.kind == standing.kind
                && item.source == standing.source
                && item.label == standing.label
                && item.content == standing.content
                && item.state.sends_content()
            {
                let _ = self.kernel.replace(item.id, wanted.content.clone());
            }
        }
        self.kernel.set_full_notice(Some(wanted));
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
    ///
    /// note: and what was waiting to go in and never will is said, as [`App::unsent`] says it,
    /// once the turn is done with - its end takes the oldest in.
    pub async fn wait_for_turn(
        &mut self,
        events: &mut tokio::sync::broadcast::Receiver<Event>,
        finished: &mut tokio::sync::mpsc::UnboundedReceiver<Outcome>,
        heard: impl FnMut(&Event),
    ) -> Option<String> {
        let failed = self.waited_for_turn(events, finished, heard).await;
        self.unsent();

        failed
    }

    /// Says what was waiting to go in when the session ended, and lets go of it.
    ///
    /// note: messages typed into a turn wait in a queue, and lines handed in while a command was
    /// out wait behind it; a session that ends takes none of them in. They used to go with it
    /// without a word - a line answered `queued` and then seen again by nobody, which is the one
    /// thing the queue is there to stop. What can be done for them is to say which, so that
    /// whoever typed one knows to send it again.
    ///
    /// note: for a loop that leaves without [`App::wait_for_turn`], which calls this - a second
    /// `ctrl+c`.
    pub fn unsent(&mut self) {
        let lines: Vec<String> = std::mem::take(&mut self.typed_ahead)
            .into_iter()
            .chain(
                std::mem::take(&mut self.held)
                    .into_iter()
                    .map(|(line, _)| line),
            )
            .collect();
        if lines.is_empty() {
            return;
        }
        let named: Vec<String> = lines
            .iter()
            .map(|line| format!("`{}`", one_line(line)))
            .collect();
        self.say(
            Speaker::Note,
            format!(
                "the session ended with {} waiting, not sent: {}",
                plural(lines.len(), "line"),
                named.join(", ")
            ),
        );
    }

    /// The whole of [`App::wait_for_turn`] but the parting.
    async fn waited_for_turn(
        &mut self,
        events: &mut tokio::sync::broadcast::Receiver<Event>,
        finished: &mut tokio::sync::mpsc::UnboundedReceiver<Outcome>,
        mut heard: impl FnMut(&Event),
    ) -> Option<String> {
        use tokio::sync::broadcast::error::RecvError;

        // a `/model` or `/endpoint` still settling first, for the reason the turn is waited for:
        // its change is a record, and a script whose last line is the switch reaches the end of
        // its input with no next line to wait for it. Under the same bound, and left the same way
        self.settled(Some(LEAVING)).await;
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
                    // a command coming back is not the turn ending, and the turn is still waited for
                    if let Outcome::Returned(_) = outcome {
                        self.on_outcome(outcome);
                        continue;
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
        // a listing or a pass being worked out is stopped as a turn is, and before one: it is
        // what holds every line after it. A switch is not, for the reason on `in_flight`
        if let Some(errand) = self.errand.take() {
            errand.task.abort();
            self.say(
                Speaker::Note,
                format!("stopped waiting for {}", errand.what),
            );
        }
        if !self.busy {
            return;
        }
        self.interrupting = true;
        self.kernel.interrupt();
        // and whatever the advisor is being asked for that turn, which the kernel cannot reach
        #[cfg(feature = "shell-advisor")]
        if let Some(advised) = &self.advisor {
            advised.stop();
        }
    }

    /// Whether the turn is resting in [`State::Ready`]: calls decided, and none of them run.
    pub fn ready(&self) -> bool {
        !self.busy && matches!(self.kernel.state(), State::Ready { .. })
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

    /// The messages waiting for the running turn to end, oldest first; each will get a turn of
    /// its own.
    pub fn queued(&self) -> impl ExactSizeIterator<Item = &str> {
        self.typed_ahead.iter().map(String::as_str)
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
        // a terminal speaking the kitty keyboard protocol reports releases and repeats as well,
        // and each of those answered as a press is one key doing its work twice
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
            match self.stoppable() && key.code == KeyCode::Char('c') {
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
            (KeyCode::Esc, _) if self.stoppable() => self.interrupt(),
            (KeyCode::Char('t'), true) => self.show(self.next_tab()),
            (KeyCode::Char('1'), _) if alt => self.show(Tab::Chat),
            (KeyCode::Char('2'), _) if alt => self.show(Tab::Context),
            (KeyCode::Char('3'), _) if alt => self.show(Tab::Trace),
            (KeyCode::Char('4'), _) if alt => self.show(Tab::Permissions),
            (KeyCode::Char('p'), true) => {
                self.preview("the next request", request_preview(&self.kernel, self.keys))
            }
            // the way back down from wherever the reading got to, without paging through however
            // much arrived in the meantime. Scrolling to the bottom does it too, and that is the
            // gesture most people will find first; this is the one for a turn that wrote a
            // thousand lines while somebody was looking at the twelfth
            (KeyCode::Char('e'), true) => self.follow = true,
            // `/continue`, `r` for resume
            (KeyCode::Char('r'), true) => self.carry_on(),
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

    /// Tells the policy what to answer about one of the permissions tab's rows from here on.
    ///
    /// note: here rather than in `keys.rs`, for the reason [`App::cycle`] is: the tab's keys and a
    /// client's `rule` change the same rows, and the record and the line saying so are the half
    /// that must not be copied.
    ///
    /// note: by the row's own spelling, and only for a row the tab draws, which is what a client
    /// was shown. Read back with `Subject::parse` instead, a server's row - `server files` - would
    /// be refused, and a rule about something nobody has decided is a flag's to write, not a row's.
    pub fn rule(&mut self, subject: &str, verdict: Verdict) -> Result<(), String> {
        let Some(row) =
            (self.permissions().into_iter()).find(|row| row.subject.to_string() == subject)
        else {
            return Err(format!("`{subject}` is not a row on the permissions tab"));
        };
        self.policy.set(&row.subject, verdict);
        self.ruled(&row.subject, verdict);

        Ok(())
    }

    /// Records a rule the policy has just been given, and says it.
    pub(super) fn ruled(&mut self, subject: &Subject, verdict: Verdict) {
        // recorded, because the rule is what a later call is allowed or refused by, and the record
        // otherwise says only that the policy decided
        self.kernel
            .record_rule(subject.to_string(), verdict, None, false);
        self.refresh_full_notice();
        // said out loud, because this is a decision about what may happen later and the tab it
        // was made on is not the one somebody will be looking at when it does
        self.say(
            Speaker::Note,
            match verdict {
                Verdict::Allow => format!("`{subject}` runs without asking, from now on"),
                Verdict::Deny => format!("`{subject}` is refused, from now on"),
                Verdict::Ask => format!("`{subject}` is a question again"),
            },
        );
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
            // note: a note here too, and for the compactor rather than the model, which is not
            // shown a note on an item it is shown: one the person brought back is one it leaves
            // alone until the context is full, where without it it was elided again before the
            // next request - see `tools::Shedder`
            _ => (
                ContextState::Active,
                Some("brought back by the user".into()),
            ),
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
        // rows of hundreds with its explanation on another tab
        self.search = None;

        self.tab = tab;
        self.focus = match (tab, self.asking()) {
            (Tab::Chat, false) => Focus::Input,
            _ => Focus::Body,
        };
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
}
