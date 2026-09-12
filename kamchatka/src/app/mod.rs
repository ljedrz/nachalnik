//! Everything the terminal knows: what is on the screen, what the keys do, and what to make of
//! the events the kernel broadcasts.
//!
//! note: The kernel is driven from a task of its own, and this loop never blocks on it. What
//! arrives here is [`Event`]s - the same ones the session log is made of - so the screen is a
//! rendering of the record rather than a second account of it. When a turn stops for a decision,
//! the task ends and hands control back; nothing is waiting on a channel for an answer.

use std::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Instant, SystemTime},
};

#[cfg(feature = "tui")]
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nachalnik::{
    Budget, Capability, Content, ContextId, ContextItem, ContextKind, Delta, Event, Grant,
    GrantSource, Kernel, PermissionRequest, State, Usage, Verdict, selectors::Selector,
};
use nachalnik_providers::Endpoint;
#[cfg(feature = "tui")]
use ratatui_textarea::{TextArea, WrapMode};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    sandbox::Confinement,
    tools::{Careful, Limits, Subject},
};

mod command;
#[cfg(feature = "tui")]
mod keys;
pub(crate) mod text;

use text::{head, moved, one_line, thousands, trace_line, unpadded};
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
/// note: Whole-window tabs rather than panes side by side. Three things want the screen - the
/// conversation, the context and the event stream - and splitting it between them meant all three
/// were cramped: the trace was cut off mid-sentence, the context could only afford a label and a
/// number, and a long answer was reading in sixty columns. Only one of them is being read at a
/// time. The status line is under all of them, because the budget is always worth seeing; the
/// prompt is not, because three of the four are read and operated rather than typed into, and a
/// prompt there was a mode - every letter on those tabs meant one of two things depending on where
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
/// will contain, what the item says, and what it said before somebody rewrote it - and picking
/// one of them to show was how the viewer came to be quietly wrong about the other two.
#[derive(Clone)]
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

/// One line of the chat that is not a context item.
///
/// note: everything else on the chat *is* one, and is read off the context every frame by
/// [`App::conversation`]. What is left over is two kinds of line, and they are one type because
/// they have to keep their order among each other: the fragments arriving between a model
/// starting to speak and the kernel recording what it said, and the chrome that is nobody's
/// context at all - a slash command's output, a compaction notice, "stopped", a provider's
/// error.
///
/// note: the first kind is [`Entry::transient`] and is dropped the moment the item exists; the
/// second stays for the session. Neither carries an identifier, because neither has one.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Who is saying it.
    pub speaker: Speaker,
    /// What it says.
    pub text: String,
    /// Whether more of it is still arriving.
    pub open: bool,
    /// Whether it arrived as fragments, and so is a context item's to say once there is one.
    ///
    /// note: not the same question as [`Entry::open`], which is only whether *more* is coming. A
    /// streamed answer stops being open the moment the stream ends and is still the item's a
    /// beat later, when the kernel records it.
    pub streamed: bool,
    /// The newest context item that existed when it was said, if any did.
    ///
    /// note: what puts it back in its place. A line like this belongs *between* two turns rather
    /// than at an index, because the turns around it can be excluded, edited, undone or
    /// compacted and it still happened where it happened. Anchoring to an identifier survives
    /// all of that, the anchor itself going away included: the line simply renders before
    /// whatever the next surviving item is.
    pub after: Option<ContextId>,
    /// Whether it was said while something was still arriving, and so belongs after whatever
    /// that arrival turns into.
    ///
    /// note: the one case an identifier cannot answer on its own. "stopped" is said while a
    /// model is mid-sentence, so the newest item at the time is the *question*, and anchoring
    /// there puts the interruption above the half-answer it interrupted. The turn is recorded a
    /// moment later with a higher identifier; this is what re-anchors to it. Without it the
    /// conversation reads in an order the session never had.
    pub arriving: bool,
}

impl Entry {
    /// Whether the context is going to say this line itself, once it catches up.
    ///
    /// note: whether it *streamed*, and not whether its speaker is one that gets context items.
    /// The difference is a message typed into a running turn: said by a person, so it will be an
    /// item eventually, but not until the turn it was typed into has ended - and dropping it
    /// when some other turn was recorded took it off the screen for as long as that took. What
    /// an arriving item replaces is what was arriving.
    pub fn transient(&self) -> bool {
        self.streamed
    }
}

/// What a provider actually charged for a request, and what that request was made of.
///
/// note: the one exact number in this program's accounting, and the point of keeping it is that
/// an estimate does not have to carry the whole context any more. A counter without the model's
/// tokenizer is out by a few percent of *everything it is asked about*; measured here, a third
/// low on a short request with tool definitions and about 7% low on a long one, and
/// `Calibrating` brings the second to within 1%. One percent of a hundred thousand tokens is a
/// thousand tokens, which is a poor thing to be reading while deciding whether the next message
/// fits.
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
    /// That is not a rounding error: it made the corner read `~0` for the rest of a session
    /// after one 12,278-token attachment was elided and one request went out without it.
    pub sent: Vec<ContextId>,
    /// What everything else in it came to - the markers standing where an elided item was.
    ///
    /// note: a stored figure, where everything else here is re-estimated on the way past, and
    /// the exception is deliberate. Re-estimating is what makes an unchanged item cancel exactly
    /// against itself, but it needs the *text* that was sent, and the text of a marker is gone
    /// as soon as the item stops being elided. A marker is one line, so what is lost by storing
    /// it in older money is a fraction of twenty tokens - against the twelve thousand that
    /// getting it wrong costs.
    pub markers: usize,
    /// The provider's own figure for the whole of it, tool definitions and framing included.
    pub reported: usize,
}

/// One line of the conversation as it stands now, ready to be drawn.
///
/// note: built per frame by [`App::conversation`] and held by nobody. A line that *is* a context
/// item carries it, so the drawing can ask the one question that is about the request rather
/// than about the text - is this going, and if not why - without a second lookup and a second
/// chance to disagree with itself.
pub struct Said<'a> {
    /// Who said it.
    pub speaker: Speaker,
    /// What it says now.
    pub text: Cow<'a, str>,
    /// The context item this line is part of, where it is part of one.
    pub item: Option<&'a Arc<ContextItem>>,
}

/// What the next request does with each context item.
///
/// note: the two halves are one answer, taken from one projection, because they have to agree:
/// every item is either in the request for some number of tokens or out of it for a reason, and a
/// screen that worked the two out separately would have rows that are neither.
pub struct Going {
    /// What each item in the request costs it.
    pub costs: BTreeMap<ContextId, usize>,
    /// Why each item that is not in the request was left out, in the projector's own words.
    pub left_out: BTreeMap<ContextId, String>,
    /// What the model reads in place of an item whose content is not going but which is still
    /// in the request - that is, an elided one.
    ///
    /// note: taken out of the projection rather than assembled from the item's state and note,
    /// for the same reason [`Going::left_out`] is. The brackets are the projector's, the words
    /// inside them are whoever elided it, and a screen that put its own version of the sentence
    /// next to the one the model is reading would be showing a third thing that is neither.
    pub marker: BTreeMap<ContextId, String>,
}

impl Going {
    /// Whether this item's own content is going into the request.
    ///
    /// note: two conditions rather than [`nachalnik::ContextState::sends_content`], and the
    /// second is the one that bites. An item may be in a state that sends content and still not
    /// be in the request, because a projector repairs a request to keep it valid - a second
    /// result for a call that already has one is dropped, which is what putting the whole of a
    /// truncated output back beside the short copy produces. Anything asking "is this item's
    /// content going" asks here, so that the columns, the reason beside them and `/budget`
    /// cannot drift apart.
    pub fn sends_content(&self, item: &ContextItem) -> bool {
        item.state.sends_content() && self.costs.contains_key(&item.id)
    }
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

/// What one line handed to [`App::submit`] did.
///
/// note: three answers rather than two, because "it was queued" is a different thing to tell
/// somebody than "it was asked". A line sent into a running turn waits for the end of that turn -
/// see the note on [`App::submit`] for why it cannot go in any earlier - and a caller that read
/// that as "asked" would be waiting for an answer to a question the model has not been given yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
/// note: this is the half of `submit` that used to be readable only by watching [`App::loose`]
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
    /// The provider, for switching models - whichever dialect it speaks.
    pub provider: Arc<dyn Endpoint>,
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
    /// Every event, name and detail.
    pub trace: VecDeque<Traced>,
    /// The prompt.
    #[cfg(feature = "tui")]
    pub input: TextArea<'static>,
    /// Which pane the keys go to.
    pub focus: Focus,
    /// Which of the listed context items is picked out; an index into [`App::listed`], which is
    /// not the whole context when `sending_only` is on.
    pub selected: usize,
    /// Whether the context tab lists only what the next request carries, leaving out everything
    /// that has been pruned, archived or superseded.
    pub sending_only: bool,
    /// Where the context pane is scrolled to, which it keeps between frames.
    #[cfg(feature = "tui")]
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
    /// How many tokens the provider may charge for this session before it stops; `None` never
    /// stops. [`App::set_spend`] is how it is changed, and [`App::spend`] reads it.
    ///
    /// note: here rather than in the loop that drives the session, which is where it started, and
    /// the move is the whole of what makes it a ceiling rather than a headless flag. Every caller
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
    /// re-reports it - which put a line about item 4's dropped call in the conversation after
    /// every message for the rest of a session, for one tool result excluded once. All four kinds
    /// behave this way: an orphaned call, an orphaned result, a flattened turn and a result held
    /// back all last as long as the state that caused them.
    ///
    /// note: the *conversation* only. The trace keeps every one, because it is the event log and a
    /// log that hid a repeated entry would be the wrong thing entirely - `model.requested` really
    /// did carry that repair, every time.
    reported_repairs: Vec<String>,
    /// How far down the pinned question's arguments are scrolled.
    ///
    /// note: on the app rather than on the question, because there is no question to hang it on:
    /// what stands in the prompt's place is drawn from `pending_permissions()` every frame, so
    /// there is no state saying a question is open and none to get out of step with the kernel.
    pub question_scroll: usize,
    /// A message somebody sent into a turn that was already running, waiting for it to end.
    typed_ahead: Option<String>,
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
        provider: Arc<dyn Endpoint>,
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
            provider,
            limits,
            versions: BTreeMap::new(),
            introspect: None,
            // the terminal's own default, for a screen test that never spawns anything; the
            // program overwrites it with what a child process actually reported
            confinement: Confinement::Unsupported,
            anchor: None,
            pending: Anchor::default(),
            loose: Vec::new(),
            trace: VecDeque::new(),
            #[cfg(feature = "tui")]
            input,
            focus: Focus::Input,
            selected: 0,
            sending_only: false,
            #[cfg(feature = "tui")]
            list: ratatui::widgets::ListState::default(),
            overlay: None,
            scroll: 0,
            follow: true,
            tab: Tab::Chat,
            trace_scroll: 0,
            busy: false,
            quit: false,
            spend: None,
            rendered: 0,
            viewport: 0,
            editing: None,
            stepping: false,
            count: String::new(),
            chosen: 0,
            #[cfg(feature = "tui")]
            grants: ratatui::widgets::ListState::default(),
            interrupting: false,
            thought_unseen: false,
            reported_repairs: Vec::new(),
            since: Instant::now(),
            question_scroll: 0,
            typed_ahead: None,
            streamed_bytes: 0,
            outcomes,
            previews: 0,
            spent: 0,
            overspent: false,
            unreported: false,
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
        let text = unpadded(&text.into()).to_owned();
        // whether something was arriving is read *before* closing it, because closing is what
        // makes it stop arriving and this line was said while it still was
        let arriving = self.arriving();
        self.close();
        self.loose.push(Entry {
            speaker,
            text,
            open: false,
            streamed: false,
            after: self.kernel.items().last().map(|item| item.id),
            arriving,
        });
        if speaker == Speaker::User {
            self.follow = true;
        }
    }

    /// Says a message of the person's own and puts it in the context.
    ///
    /// note: it does not say it on the chat, and that is the whole of what this method is now.
    /// The item *is* the line: the conversation is read off the context every frame, so pushing
    /// is saying. It used to be three statements - say it, push it, tie the two together - and
    /// the third one is the one `--message` forgot, which left the opening line of every `-m`
    /// session unable to be hidden, updated or undone for the rest of it. There is no third
    /// statement left to forget.
    pub fn ask(&mut self, text: &str) -> ContextId {
        self.follow = true;

        self.kernel.push(ContextItem::user(text))
    }

    /// Says an error, unless the last thing said was the same error in a smaller envelope.
    ///
    /// note: one provider failure is reported twice - once as the event the kernel emitted and
    /// once as the outcome the turn came to, the second wrapping the first - and two red lines
    /// saying the same thing is one more than the news warrants; the trace pane has both either
    /// way.
    fn say_error(&mut self, error: String) {
        let repeat = self.loose.last().is_some_and(|last| {
            last.speaker == Speaker::Error && unpadded(&error).contains(&last.text)
        });
        if !repeat {
            self.say(Speaker::Error, error);
        }
    }

    /// Whether something is part-way through arriving.
    fn arriving(&self) -> bool {
        self.loose.last().is_some_and(|entry| entry.open)
    }

    /// Appends to the line still arriving from this speaker, starting one if there is none.
    ///
    /// note: the bound is on a tool's output and on nothing else, which it did not used to be. A
    /// `find /` should not be able to fill the screen up, and the whole of it is in the context
    /// either way - but a model writing a long answer had its first paragraphs eaten while it
    /// was still writing the last one. A message is what somebody came here to read, and it is
    /// never shortened; the moment the turn is recorded the line is dropped and the item is what
    /// gets drawn.
    fn append(&mut self, speaker: Speaker, fragment: &str) {
        let after = self.kernel.items().last().map(|item| item.id);
        match self.loose.last_mut() {
            Some(entry) if entry.open && entry.speaker == speaker => {
                entry.text.push_str(fragment);
                if speaker == Speaker::Result && entry.text.len() > LIVE_OUTPUT {
                    let cut = entry
                        .text
                        .char_indices()
                        .nth(entry.text.chars().count() - LIVE_OUTPUT / 2)
                        .map(|(at, _)| at)
                        .unwrap_or(0);
                    entry.text = format!(
                        "[... the earlier output is not repeated here; the whole of it is in the \
                         context ...]\n{}",
                        &entry.text[cut..]
                    );
                }
            }
            _ => {
                self.close();
                self.loose.push(Entry {
                    speaker,
                    text: fragment.to_owned(),
                    open: true,
                    streamed: true,
                    after,
                    arriving: false,
                });
            }
        }
    }

    /// Closes whatever was still arriving, and drops it if it turned out to be nothing.
    fn close(&mut self) {
        let Some(entry) = self.loose.last_mut() else {
            return;
        };

        entry.open = false;
        entry.text = unpadded(entry.text.trim_end()).to_owned();
        if entry.text.is_empty() {
            self.loose.pop();
        }
    }

    /// Hands the lines that were arriving over to the item that now holds them.
    ///
    /// note: called with no thought about *what* arrived, which is the point. The old code
    /// remembered whether a provider had streamed so it would not print a non-streaming answer
    /// twice, popped the open tool result so the recorded one could take its place, and walked
    /// the tail backwards stamping identifiers onto lines. All three answered the same question -
    /// which lines has the context caught up with - and the answer is now always "all of them".
    ///
    /// note: what was said *while* they were arriving is re-anchored to the item rather than
    /// dropped, because it is not the item's and it did not happen before it. "stopped" is said
    /// mid-sentence, so the newest item at the time was the question; left there it would read
    /// above the half-answer it interrupted.
    fn caught_up(&mut self, item: ContextId) {
        self.loose.retain(|entry| !entry.transient());
        for entry in &mut self.loose {
            if entry.arriving {
                entry.after = Some(item);
                entry.arriving = false;
            }
        }
    }

    /// Says what a resumed session picked up.
    ///
    /// note: it says it and nothing else, which is the whole of what a resume needs now. It used
    /// to walk the context turning every item into a transcript line, because the transcript was
    /// a log and a resumed session had no log to show - and that walk was a *second*
    /// implementation of "what does this item look like as a conversation", beside the one the
    /// live path built event by event. They disagreed, as two of anything do: a resumed turn
    /// showed none of its thinking, and a resumed tool result's line left out what the output
    /// limit had taken. There is one implementation now, [`App::conversation`], and a resumed
    /// session is drawn by it without being told that it was resumed.
    pub fn replay(&mut self) {
        let items = self.kernel.items();
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

    /// The conversation as it stands: every context item that is going, in order, with the
    /// asides that were said between them and whatever is arriving at the end.
    ///
    /// note: **the one place a context item becomes a line of chat.** Everything the screen
    /// shows of the conversation is worked out here, from the context, every frame - so an item
    /// that was excluded is not in it, an item that was rewritten reads as it is now, an item an
    /// `undo` took away is gone and one a `redo` brought back is there, and none of those needed
    /// an event, a back-pointer or a second copy of the words. What used to do this was a log
    /// with three patches on it: a live text lookup for edits, a withheld lookup for exclusions,
    /// and a backwards walk stamping identifiers onto lines so the other two could find them.
    ///
    /// note: `is_projected` and not `sends_content`, so an *elided* item keeps its place. It is
    /// in the request as a marker, which is what the model reads there; the drawing marks it.
    /// A turn whose call has not come back yet also stays: the projector repairs one of those
    /// out of the request, and that is a momentary, mechanical absence rather than anything
    /// anybody decided - hiding on it blanked a call out of the conversation at the moment a
    /// permission question was asking about it.
    ///
    /// note: the items are the caller's, because they are `Arc`s the kernel hands out by clone
    /// and the lines borrow their text rather than copying it. A frame that copied every word it
    /// was about to draw would copy the whole conversation to show what it was already showing.
    pub fn conversation<'a>(&'a self, items: &'a [Arc<ContextItem>]) -> Vec<Said<'a>> {
        let mut said = Vec::new();
        let mut loose = self.loose.iter().peekable();
        let loosed = |entry: &'a Entry| Said {
            speaker: entry.speaker,
            text: Cow::Borrowed(&entry.text),
            item: None,
        };

        for (at, item) in Self::in_order(items) {
            // what was said before this item existed goes before it, in the order it was said
            while let Some(entry) = loose.next_if(|entry| entry.after < Some(at)) {
                said.push(loosed(entry));
            }
            if item.state.is_projected() {
                Self::as_conversation(item, &mut said);
            }
        }
        said.extend(loose.map(loosed));
        // and last, the message typed into a turn that is still running. It is drawn from the
        // field holding it rather than said onto the screen, for the same reason the fragments
        // are drawn from the context: it is state waiting to become an item, and the moment it
        // becomes one the item is what gets drawn, in the same place, with nothing to clean up
        if let Some(waiting) = &self.typed_ahead {
            said.push(Said {
                speaker: Speaker::User,
                text: Cow::Borrowed(waiting),
                item: None,
            });
        }

        said
    }

    /// The same text without the blank lines a provider put in front of it, and without
    /// copying it to find out there were none.
    ///
    /// note: applied to what an item says rather than to what the item holds. The item keeps
    /// what arrived - a record of "what arrived, tidied up" cannot answer what arrived - and the
    /// screen declines to spend rows on it. `App::say` has always done this to a line said
    /// outright, and the lines are read off items now, so this is where it moved to.
    fn trimmed(text: Cow<'_, str>) -> Cow<'_, str> {
        match text {
            Cow::Borrowed(text) => Cow::Borrowed(unpadded(text)),
            Cow::Owned(text) => Cow::Owned(unpadded(&text).to_owned()),
        }
    }

    /// The items in the order the conversation had them, each with the place it occupies.
    ///
    /// note: not the order the context holds them in, and the difference is an edit. An edit
    /// *supersedes*: the new words are a new item, appended, so its identifier is the highest
    /// in the context and reading the context in order puts a correction to a turn from twenty
    /// exchanges ago after everything that followed it. That is an order no request ever had -
    /// the request has the new words where the old ones were - so an item that replaces another
    /// takes its place, and its identifier is only the tie-break between two that claim the
    /// same one.
    ///
    /// note: the chain is followed rather than the one hop, because a turn can be edited twice
    /// and the second edit replaces the first. Bounded, because nothing here wrote the number
    /// it is following: `meta` is a free-form value and a hand-written snapshot could point one
    /// item at another in a circle.
    fn in_order(items: &[Arc<ContextItem>]) -> Vec<(ContextId, &Arc<ContextItem>)> {
        let replaces = |item: &ContextItem| {
            item.meta
                .get("replaces")
                .and_then(Value::as_u64)
                .map(ContextId)
        };
        let by_id: BTreeMap<_, _> = items.iter().map(|item| (item.id, item)).collect();

        let mut placed: Vec<_> = items
            .iter()
            .map(|item| {
                let mut at = item.id;
                for _ in 0..HOPS {
                    match by_id.get(&at).and_then(|item| replaces(item)) {
                        Some(prior) if by_id.contains_key(&prior) => at = prior,
                        _ => break,
                    }
                }

                (at, item)
            })
            .collect();
        // stable, so two items in the same place keep the order the context has them in
        placed.sort_by_key(|(at, _)| *at);

        placed
    }

    /// Turns one context item into the lines it reads as.
    ///
    /// note: a turn is more than one line - what it thought, what it said, and each tool it
    /// asked for - and all of them are the same item. That is why the drawing dedupes the
    /// withheld mark by identifier rather than by line: one turn says once why it is not going.
    fn as_conversation<'a>(item: &'a Arc<ContextItem>, said: &mut Vec<Said<'a>>) {
        let mut line = |speaker, text| {
            said.push(Said {
                speaker,
                text,
                item: Some(item),
            })
        };

        match &item.kind {
            ContextKind::System => {}
            ContextKind::UserMessage => line(Speaker::User, Self::trimmed(item.content.to_text())),
            ContextKind::AssistantMessage { .. } => {
                // the thinking first, because that is the order it happened in and the order a
                // turn recorded as ordered blocks holds it in
                for thought in item.thinking() {
                    let text = Self::trimmed(thought.to_text());
                    if !text.trim().is_empty() {
                        line(Speaker::Reasoning, text);
                    }
                }
                let text = Self::trimmed(item.content.to_text());
                if !text.trim().is_empty() {
                    line(Speaker::Model, text);
                }
                // `calls()`, so a turn a provider recorded as ordered blocks reads back with the
                // tools it asked for rather than as bare text
                for call in item.calls() {
                    let args = one_line(&call.args.to_string());
                    line(Speaker::Call, Cow::Owned(format!("{}({args})", call.tool)));
                }
            }
            ContextKind::ToolResult { tool, is_error, .. } => {
                line(
                    Speaker::Result,
                    Cow::Owned(head(&item.content.to_text(), 6)),
                );
                // note: what a tool cost and what the output limit took are *not* here, and
                // used to be. Both are facts about an item rather than anything said, both are
                // a column on the context tab already, and a conversation with a line of
                // accountancy under every tool call is one somebody has to read around. What
                // survives is the one thing that changes how the turn above and below it reads
                if *is_error {
                    line(
                        Speaker::Note,
                        Cow::Owned(format!("{tool} reported an error")),
                    );
                }
            }
            // note: this line is what `/attach` and `-f` say for themselves, and neither of them
            // says anything else. A command that pushed an item and then announced it would be
            // describing the context from beside the context, which is the arrangement the whole
            // chat was rewritten to get rid of: two accounts of one item, and only one of them
            // able to be wrong. What a person needs to see is here because it is *read off* the
            // item - which file, what it is, what it costs
            //
            // note: the `+` is the context pane's, for the same reason: a file attached as bytes
            // is counted at `0` by everything in this workspace, and a line saying `0 tokens`
            // about the largest thing in the request is the one number on this screen that reads
            // as good news when it is the opposite
            _ => line(
                Speaker::Note,
                Cow::Owned(format!(
                    "[{}] {} ({}){}, {} tokens{}",
                    item.id,
                    item.label,
                    item.source,
                    match crate::attach::describe(item) {
                        Some(what) => format!(" {what}"),
                        None => String::new(),
                    },
                    thousands(item.tokens),
                    match item.uncounted {
                        0 => String::new(),
                        n => format!(" and {n} piece(s) nothing here can price"),
                    }
                )),
            ),
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
            wall: SystemTime::now(),
        });
    }

    // ---------------------------------------------------------------------- driving the kernel

    /// Starts, or carries on with, a turn.
    ///
    /// note: it refuses once the ceiling has been reached, and that refusal is what makes
    /// [`App::spend`] a bound rather than a report. Stopping the turn that crossed the line is
    /// only half of it: a loop that hands in the next line - a script, a person, an agent driving
    /// this from somewhere else - would start spending again, and every caller goes through here.
    pub fn start_turn(&mut self) {
        if self.busy || self.broke() {
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
        if self.busy || self.broke() {
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
        // program's own `main` receives. A cut-off answer that says so to nobody is the failure
        // this was written to close, and it took a test driving `App` directly to see that it
        // could still happen.
        if let Some(notice) = self.provider.take_notice() {
            self.say(Speaker::Note, notice);
        }

        // note: a turn that stopped to ask a question has not ended - the call it is asking about
        // still has a result to come - and a message pushed now would land between the call and
        // that result, which is a place a request cannot have one. A live run put "what is the
        // capital of Peru" exactly there
        let ended = matches!(outcome, Outcome::Stopped(ref state) if !matches!(state, State::Deciding { .. }));
        // a `Stepped` outcome is somebody driving this a transition at a time, and a failure is
        // not the moment to start something else; either way what was typed waits for `/continue`
        let carry_on = ended && !self.interrupting;
        match outcome {
            Outcome::Failed(e) => self.say_error(e),
            // note: a turn stopping to ask says nothing here, and opens nothing. The question is
            // drawn from `pending_permissions()` every frame, so there is no moment at which it
            // has to be put on the screen and none at which it has to be taken off - which is
            // also the end of a class of bug this had: an overlay left standing over a question
            // that had been answered somewhere else, until the next key closed it
            Outcome::Stopped(State::Deciding { .. }) => {}
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
                // conversation gets the fact and ctrl+p gets the list
                //
                // note: and only when they change. See `App::reported_repairs` - a repair lasts as
                // long as the state that caused it, so the projector re-does it for every request
                // and honestly reports it again, which put this line in the conversation after
                // every message for the rest of a session over one tool result taken out once
                //
                // note: the count is everything being repaired rather than what is newly so,
                // because it is the number `ctrl+p` will show. And the wording says the repair
                // stands: in the past tense it reads as something that happened to this one
                // request, which is exactly what somebody then goes looking for the cause of,
                // and there is nothing about this turn to find
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
                                 stand; ctrl+p says where"
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
            Event::ModelFinished { item, usage, .. } => {
                // note: the tokens are real and the words are gone. Some endpoints bill for
                // reasoning and return none of it - `mercury-2.5` answered one question with 1,139
                // reasoning tokens and 273 of answer, and its stream carries no reasoning field at
                // all - so the context tab shows a turn with nothing in it where the thinking was,
                // and the only trace of where the money went is a number in `/budget`. Said once,
                // because it is true of the endpoint rather than of this turn
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
                self.charge(usage);
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
            Event::ModelFailed { error } | Event::StepFailed { error } => {
                self.close();
                self.say_error(error);
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
            // note: a file added to the context used to be said out loud here, and the chat then
            // drew it twice - `[1] notes.md (file), 10 tokens` off the item, and
            // `[1] notes.md is in the context, 10 tokens` off this event, one above the other,
            // for every `-f` and every `/attach`. The derived line is the one that cannot go out
            // of date, so it is the one that stays. See `App::as_conversation`
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
    /// note: it stops *after* the response that crosses the line, because that is the first moment
    /// anybody knows what the response cost. A ceiling is a stopping rule, not a cap: the session
    /// ends having spent a little more than it, and the line says how much.
    fn charge(&mut self, usage: Option<Usage>) {
        let Some(usage) = usage else {
            // said only where it changes something: with no ceiling, a total nobody set a limit on
            // being short by one response is not news
            if self.spend.is_some() && !std::mem::replace(&mut self.unreported, true) {
                self.say(
                    Speaker::Note,
                    "this endpoint reports no usage, so nothing is counted against the ceiling; \
                     only a deadline can stop this session",
                );
            }

            return;
        };

        // counted whether or not anything is watching the figure, which is not where this started:
        // it was added up only under a ceiling, so a session that set one half way through began
        // from zero and `/spend` answered `0 tokens spent` after a turn that plainly cost some.
        // Found by reading what a live run printed
        self.spent += usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0);
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

    /// Sets the ceiling, or takes it away, and lets a stopped session carry on under the new one.
    pub fn set_spend(&mut self, limit: Option<u64>) {
        self.spend = limit;
        self.overspent = limit.is_some_and(|limit| self.spent >= limit);
    }

    /// Asks the running turn to stop at the next opportunity.
    ///
    /// note: `pub` for the same reason [`App::submit`] is. `esc` is one caller; a deadline, a
    /// budget ceiling or a request cap watching from another task is another, and the kernel
    /// takes an interrupt from any thread. What it never does is discard what arrived.
    pub fn interrupt(&mut self) {
        self.interrupting = true;
        self.kernel.interrupt();
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
            self.overlay_key(key).await;
            return;
        }

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
            // the two ends of the conversation, one key each. With control held, because `home`
            // and `end` are the prompt's own - a prompt whose keys moved something else while
            // somebody was editing a line would be the trap
            (KeyCode::Home, true) => {
                self.scroll = 0;
                self.follow = false;
            }
            (KeyCode::End, true) => self.follow = true,
            (KeyCode::F(1), _) => self.preview("the keys", crate::help::HELP),
            // `tab` moves the keys to the other thing on the screen that wants them, and on a tab
            // with no prompt there is no other thing - so it means the one gesture that is always
            // worth having: back to where typing happens
            (KeyCode::Tab, _) => match (self.tab, self.asked().is_some()) {
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
                // the pinned question, which is what `Focus::Body` means on the chat tab
                (Tab::Chat, Focus::Body) => self.question_key(key).await,
                // a question is on the screen and has not been given the keys, so the prompt is
                // not on the screen either and there is nothing here for a key to do
                (Tab::Chat, Focus::Input) if self.asked().is_some() => self.locked_key(key),
                (Tab::Context, Focus::Body) => self.context_key(key, &count),
                (Tab::Trace, Focus::Body) => self.trace_key(key),
                (Tab::Permissions, Focus::Body) => self.permissions_key(key),
                _ => self.input_key(key).await,
            },
        }
    }

    /// Opens a tab, and puts the keys wherever they are useful on it.
    ///
    /// note: Switching to the context or the trace is something somebody does in order to work on
    /// it, so the focus follows - and there is nothing else on those tabs for it to be on. On the
    /// conversation the keys go to the prompt, unless something is being asked: coming back to a
    /// waiting question is what somebody does *in order to answer it*, having just been away
    /// looking at what it is about, and making them press `tab` first would be asking twice.
    pub fn show(&mut self, tab: Tab) {
        // an edit belongs to the tab it was started from, and leaving that tab abandons it.
        // Otherwise the prompt is still holding the item's text with `editing` still set, and the
        // next message somebody types and sends is committed into the context instead of asked
        self.cancel_edit();

        self.tab = tab;
        self.focus = match (tab, self.asked().is_some()) {
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

    /// Whether the prompt is on the screen at all.
    ///
    /// note: it belongs to the conversation, and it used to be under every tab so that a message
    /// could be sent from anywhere. What that cost was a mode on three tabs that have no use for
    /// one: every key on them was either a key or a letter depending on where the focus happened
    /// to be, and the answer was `tab`, and forgetting was a `space` typed into a message instead
    /// of cycling the row somebody was looking at. The exception is an edit, which is the prompt
    /// doing a job for the tab underneath it: the item being rewritten is on that tab, and the box
    /// has to be beside it.
    ///
    /// note: and a waiting question takes its place rather than stacking above it, so that the box
    /// the keys are in is the box on the screen. Stacked, the two disagreed on any window shorter
    /// than about fifteen rows: the question needs the room, so the prompt gave way - and went on
    /// holding the keys and whatever had been typed into it from off the screen, which is a
    /// session waiting on an answer nobody can give it without first pressing a key nothing
    /// mentions. What was typed is not lost; the box comes back with it, and `App::locked_key` is
    /// what stands between a keystroke and a prompt that is not there.
    pub fn prompted(&self) -> bool {
        match self.tab {
            Tab::Chat => self.asked().is_none(),
            _ => self.editing.is_some(),
        }
    }

    /// Puts pasted text into the prompt.
    ///
    /// note: the line breaks inside a paste arrive as carriage returns rather than newlines,
    /// because a terminal sends a paste as though it had been typed and that is what the enter key
    /// sends. The editor underneath splits on newlines, so a pasted stack trace went in as one
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

    // -------------------------------------------------------------------- what the screen asks

    /// What the next request does with each item: what it costs, or why it is not in it.
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
    ///
    /// note: and `left_out` comes from the projection for a sharper reason than tidiness. Whether
    /// an item is going cannot be read off its *state*: a projector repairs a request to keep it
    /// valid, and an item it repairs away is `Active`, holding everything it holds, and not in the
    /// request. Restoring the whole of a truncated output beside the copy the model was shown
    /// makes one - the pair answer one call, so the whole takes the call and the short copy is
    /// dropped. A pane keyed on the state then had that row claiming to send its content, showing
    /// `0` for it, and accounting for none of what it was holding: three wrong answers about one
    /// item, from asking the item instead of asking the request.
    pub fn going(&self) -> Going {
        let projection = self.kernel.project();
        let counter = self.kernel.counter();
        // `included` and `messages` line up one for one under a projector that makes a message
        // per item; one that merges them has no per-item answer, and the item's own figure is a
        // better guess than a number taken from the wrong message
        let paired = projection.included.len() == projection.messages.len();

        Going {
            // what the model reads where an elided item's content was, which only a paired
            // projection can answer: it is the message that came out, not anything the item
            // holds
            marker: match paired {
                false => BTreeMap::new(),
                true => projection
                    .included
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| {
                        self.kernel
                            .item(**id)
                            .is_some_and(|item| item.state.is_elided())
                    })
                    .filter_map(|(at, id)| {
                        let said = projection.messages[at].content.as_ref()?;
                        Some((*id, said.to_text().into_owned()))
                    })
                    .collect(),
            },
            costs: projection
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
                .collect(),
            // the projector's own words, rather than a second copy of them assembled out here
            // from the state and the note - which is what this was, and which had no answer at
            // all for an item the projector had repaired away
            left_out: projection
                .skipped
                .into_iter()
                .map(|skipped| (skipped.id, skipped.reason))
                .collect(),
        }
    }

    /// What the next request is expected to cost, taken from what the last one really cost.
    ///
    /// note: **the anchored figure**, and the arithmetic is one line of intent: the provider's
    /// number, plus what the context estimates now, minus what the estimator says the items
    /// that number covered would cost now. An item that has not moved appears in both estimates
    /// and cancels, so it contributes its *measured* cost and no error at all; only what
    /// changed since the last request is estimated. Add a message to a hundred-thousand-token
    /// context and the error is a few tokens rather than a few hundred.
    ///
    /// note: both estimates are taken *now*, with the counter as it currently stands, which is
    /// what makes the cancellation exact. Storing what each item was estimated at when the
    /// request went out would not: `Calibrating` revises its scale on the way past, when the
    /// very response this figure comes from is observed, so every stored figure would be in
    /// older money than the ones it is subtracted from and a context that had not changed at
    /// all would drift.
    ///
    /// note: `None` before any response, on an endpoint that reports no usage, and after a
    /// change of model until the next response - all three being cases where there is nothing
    /// exact to build on, and the caller falls back to [`Budget::used`].
    ///
    /// note: what it does not catch, until the next request re-anchors it: a tool added or
    /// dropped, since the schemas are inside the provider's figure and are not itemised in it.
    /// The context is the part that moves.
    pub fn anchored(&self, going: &Going, budget: &Budget) -> Option<usize> {
        let anchor = self.anchor.as_ref()?;
        // the markers are what they were - see `Anchor::markers` - and everything whose content
        // was really in that request is re-estimated now, which is what makes an item that has
        // not moved cancel against itself exactly
        let covered: usize = anchor.markers
            + anchor
                .sent
                .iter()
                .map(|id| self.valued(going, *id))
                .sum::<usize>();

        Some(
            (anchor.reported as i64 + budget.context_tokens as i64 - covered as i64).max(0)
                as usize,
        )
    }

    /// What one item would cost the request if it were sending its content, as the counter sees
    /// it now.
    ///
    /// Only ever asked about an item whose content really was in the anchored request - see
    /// [`Anchor::sent`] - which is what makes the middle arm below true rather than a guess.
    ///
    /// note: three answers rather than one, and the middle one is the reason. An item that is
    /// still sending its content is worth what the projection says it costs - the message it
    /// becomes, which is the figure the budget is built from. One that is *not* any more -
    /// excluded, archived, or elided since - is worth what it holds, because that is what it
    /// contributed to the provider's figure and that is what has to come back out of it; the
    /// marker standing in its place now is already counted on the other side. One that no longer
    /// exists at all, because an undo took it, is worth nothing anybody can recover, and the next
    /// request puts the accounting straight.
    fn valued(&self, going: &Going, id: ContextId) -> usize {
        match self.kernel.item(id) {
            Some(item) if going.sends_content(&item) => {
                going.costs.get(&id).copied().unwrap_or(item.tokens)
            }
            Some(item) => item.tokens,
            None => 0,
        }
    }

    /// What the message being typed would add to the next request.
    ///
    /// note: the counter rather than a rule of thumb, so that a draft is measured the same way
    /// as everything already in the context - including whatever `Calibrating` has learnt about
    /// this model. It is worth showing at all only because the figure it is added to is
    /// anchored: a draft moving a number that is itself a thousand tokens uncertain would be
    /// precision theatre.
    pub fn drafted(&self) -> usize {
        let draft = self.draft();
        match draft.trim().is_empty() || draft.starts_with('/') {
            true => 0,
            false => self.kernel.counter().count(&Content::text(draft)),
        }
    }

    /// What every item is holding out of the next request, and how many of them there are.
    ///
    /// note: [`Context::tokens_withheld`](nachalnik::Context::tokens_withheld) answers this from
    /// the item states, which is the right answer to a question about states and the wrong one
    /// here: it counts an excluded, archived or elided item and misses one the projector repaired
    /// away, because that one's state says it is sending. `/budget` and the context tab have to
    /// agree about this figure or they are two accounts of one request again.
    pub(crate) fn withheld(&self, going: &Going) -> (usize, usize) {
        self.kernel
            .items()
            .iter()
            .filter(|item| !going.sends_content(item))
            .fold((0, 0), |(tokens, count), item| {
                (tokens + item.tokens, count + 1)
            })
    }

    /// The context items a pending call names, described the way a row on the context tab is.
    ///
    /// note: `ids: [22]` is a true account of the arguments and a useless one to be asked about.
    /// The question covers a tool that rewrites and hides pieces of the context, the overlay is
    /// covering the list those numbers refer to, and the answer is `y` or `n` - so somebody being
    /// asked whether item 22 may be elided has to already know what item 22 is. Naming them turns
    /// the question into one that can be answered on what is on the screen.
    ///
    /// note: only for the two tools this program installs itself, and only because it knows what
    /// their arguments mean. `ids` on somebody else's tool is somebody else's vocabulary, and
    /// guessing at it would put a confident description of the wrong thing in front of a decision.
    /// Nothing here reaches the policy: it is the same arguments, read out.
    pub fn about(&self, request: &PermissionRequest) -> Vec<String> {
        if !matches!(request.tool.as_str(), "introspect" | "amend") {
            return Vec::new();
        }

        let items = self.kernel.items();
        let named: Vec<ContextId> = match request.args["select"].as_str() {
            // a selector is opaque in a way a number is not: `all:tool_results` is the argument
            // most worth expanding, because nobody can count them off the screen it is covering
            Some(select) => match select.parse::<Selector>() {
                Ok(selector) => selector.matches(&items),
                Err(_) => return Vec::new(),
            },
            None => request.args["ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_u64())
                        .map(ContextId)
                        .collect()
                })
                .unwrap_or_default(),
        };
        if named.is_empty() {
            return Vec::new();
        }

        let going = self.going();
        named
            .iter()
            .take(8)
            .map(|id| match items.iter().find(|item| item.id == *id) {
                None => format!("[{id}] there is no such item"),
                Some(item) => format!(
                    "[{id}] {} · {} · {} · {} tokens{}",
                    item.label,
                    item.kind.name(),
                    item.state,
                    thousands(item.tokens),
                    match going.left_out.contains_key(id) {
                        true => " · not in the next request",
                        false => "",
                    }
                ),
            })
            .chain((named.len() > 8).then(|| format!("… and {} more", named.len() - 8)))
            .collect()
    }

    /// The context items the tab is showing: all of them, or only the ones carrying content into
    /// the next request.
    ///
    /// note: the predicate is [`Going::sends_content`], which is exactly the set with a figure in
    /// the `held` column - so the toggle has one rule a person can hold in their head: it hides
    /// every row that is holding something back. That does leave out elided items, which do go
    /// into the request as a marker and do cost the marker's few tokens; showing them would be
    /// defensible on "what am I sending", but the reason somebody reaches for this is that half
    /// the list is wreckage after a compaction, and an elided row is wreckage.
    ///
    /// note: `Going`'s rather than the state's own, because a row the projector repaired away is
    /// holding everything it holds and would have survived this filter as though it were going -
    /// which is the one row somebody with the toggle on would most want to see the truth about.
    pub fn listed(&self) -> Vec<Arc<ContextItem>> {
        let items = self.kernel.items();
        match self.sending_only {
            false => items,
            true => {
                let going = self.going();
                items
                    .into_iter()
                    .filter(|item| going.sends_content(item))
                    .collect()
            }
        }
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

    /// What the policy in force is called, short enough to put at the top of a screen.
    ///
    /// note: asked of the kernel rather than of [`App::policy`], because what the permissions tab
    /// is reporting is the policy the *runtime* will consult - the same answer `/seams` gives, and
    /// the one that would notice if the two ever came apart.
    ///
    /// note: the last segment of the path. `PermissionPolicy::name` defaults to the implementing
    /// type's own path, which is right for `/seams` - a panel whose whole subject is which types
    /// are plugged in - and spends thirty columns of a list saying `kamchatka::tools::Careful`
    /// where `Careful` is the part anybody reads.
    pub fn policy_name(&self) -> String {
        let name = self.kernel.policy().name();

        name.rsplit("::").next().unwrap_or(name).to_owned()
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

    // ---------------------------------------------------------------- the session, written out

    /// A name for a session, from the seconds since the epoch it started at.
    ///
    /// note: this is the session's identity *and* the name of the two files it leaves behind, and
    /// it was `kamchatka-1788849917`. Those go in a directory called `kamchatka`, so half of every
    /// filename said what the directory had already said - and the other half said nothing at all
    /// to anybody reading it. `2026-09-08T06-45-17Z` names the same session, sorts the same way,
    /// and answers the question somebody is looking at a list of them to ask.
    ///
    /// note: UTC, and it says so, because the alternative is a local time that needs the timezone
    /// database to work out - a dependency for a filename - and a name that quietly means
    /// something different depending on where it was written.
    ///
    /// note: to the second, which is what it was before: two sessions started inside one second
    /// would collide, and did before too. The identifier a session gets from the runtime by
    /// default is a counter that restarts with the process, which is fine as an identity and
    /// writes over the last session's record.
    pub fn session_stamp(secs: u64) -> String {
        // days since the epoch, and what is left of the last one
        let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
        // note: Howard Hinnant's `civil_from_days`, which is the whole of the calendar in five
        // lines of integer arithmetic and gets the leap years right for every year rather than
        // for the ones a test happened to try. The shift is to an era starting in March, so that
        // a leap day is the last day of a year instead of the sixtieth
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = match mp < 10 {
            true => mp + 3,
            false => mp - 9,
        };
        let year = yoe + era * 400 + i64::from(month <= 2);

        format!(
            "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}Z",
            rest / 3_600,
            (rest % 3_600) / 60,
            rest % 60
        )
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
    /// Writes the event log and a resumable snapshot, and says how many records that was.
    ///
    /// note: separate from `save` because the last write of a session happens after the terminal
    /// has been restored, where `say` has nowhere to put a sentence. Both go through here so that
    /// what `/save` produces and what a session leaves behind on its way out are the same pair of
    /// files, written the same way.
    pub fn write_session(&self, log: &str, state: &str) -> Result<usize, String> {
        let records: Vec<String> = self
            .kernel
            .history()
            .iter()
            .filter_map(|record| serde_json::to_string(record).ok())
            .collect();
        // named, because "No such file or directory" on its own leaves somebody guessing which
        // one; `-r` says which file it could not read and this should match it
        std::fs::write(log, records.join("\n") + "\n")
            .map_err(|e| format!("could not write {log}: {e}"))?;
        let snapshot = serde_json::to_vec_pretty(&self.kernel.snapshot())
            .map_err(|e| format!("could not render the session: {e}"))?;
        std::fs::write(state, snapshot).map_err(|e| format!("could not write {state}: {e}"))?;

        Ok(records.len())
    }
}
