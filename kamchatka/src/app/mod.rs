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
    time::{Instant, SystemTime},
};

#[cfg(feature = "tui")]
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nachalnik::{
    Content, ContextId, ContextItem, Delta, Event, Grant, GrantSource, Kernel, PermissionId,
    PermissionRequest, State, Tool, Usage, Verdict,
};
use nachalnik_providers::Dialect;
#[cfg(feature = "tui")]
use ratatui_textarea::{TextArea, WrapMode};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    sandbox::Confinement,
    tools::{Careful, Limits},
};

mod command;
mod search;
mod transcript;
mod views;

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
    /// wait between one turn and the next message. `+11.0s` beside `permission.decided` is not the
    /// runtime taking eleven seconds - it is a person reading the question - and it was the
    /// largest figure in the column, which made the one number nobody should act on the one the
    /// eye goes to first.
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
/// will contain, what the item says, and what it said before somebody rewrote it - and picking
/// one of them to show was how the viewer came to be quietly wrong about the other two.
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
/// a pin made while reading this is honoured rather than refused after the fact - which is the
/// whole reason the question is pinned rather than modal: the context tab is a keystroke away
/// while it waits, and `p` there is the answer to "not that one".
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

/// What the next request does with each context item.
///
/// note: the two halves are one answer, taken from one projection, because they have to agree:
/// every item is either in the request for some number of tokens or out of it for a reason, and a
/// screen that worked the two out separately would have rows that are neither.
///
/// note: `#[non_exhaustive]`, which is what every public *enum* in this workspace carries and the
/// first struct to. The reason is the same and this one earned it: `holds` was added to it this
/// cycle, and a struct of public fields that anybody may build with a literal cannot gain one
/// without breaking them. Nothing should build one - it is an answer rather than a request, and
/// [`Going::of`] is where it comes from - so saying so costs the caller nothing and makes the
/// next field a patch instead of a major.
#[non_exhaustive]
pub struct Going {
    /// What each item in the request costs it.
    pub costs: BTreeMap<ContextId, usize>,
    /// What each item holds, counted at the same moment as [`Going::costs`] and with the same
    /// counter.
    ///
    /// note: not [`ContextItem::tokens`], which is the figure taken when the item arrived - and
    /// the difference is not pedantry. A `Calibrating` counter learns a new scale from every
    /// response, and the items already in the context keep the figure they were counted with
    /// until something recounts them, which `App` does when a *turn* ends. Every reading taken
    /// inside a turn therefore had a stored figure on one scale and a freshly projected message
    /// on another, and the few percent between them arrived in the `held` column as tokens held
    /// back that nothing was holding: a live session against Gemini - whose projector carries
    /// thinking and ordered blocks back in full, so it holds nothing at all - reported 1,264
    /// tokens held across eighteen rows. The `context` tool is worse off again, because it is
    /// only ever called from inside a turn.
    pub holds: BTreeMap<ContextId, usize>,
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
    pub fn of(kernel: &Kernel) -> Going {
        let projection = kernel.project();
        let counter = kernel.counter();
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
                    .filter(|(_, id)| kernel.item(**id).is_some_and(|item| item.state.is_elided()))
                    .filter_map(|(at, id)| {
                        let said = projection.messages[at].content.as_ref()?;
                        Some((*id, said.to_text().into_owned()))
                    })
                    .collect(),
            },
            // every item, counted now, so that a subtraction against `costs` is one ruler at one
            // moment - see the note on the field
            holds: kernel
                .items()
                .iter()
                .map(|item| (item.id, counter.count_item(item)))
                .collect(),
            costs: projection
                .included
                .iter()
                .enumerate()
                .map(|(at, id)| {
                    let cost = match paired {
                        true => counter.count_message(&projection.messages[at]),
                        false => kernel.item(*id).map(|item| item.tokens).unwrap_or(0),
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

    /// What this item holds that the next request will not carry - which is what sending the
    /// whole of it would add.
    ///
    /// note: the difference between the two counts rather than the whole of an item that is not
    /// going, because an item is not in or out any more. Those are the same number for an
    /// excluded, archived or repaired-away item, whose message costs nothing; they are not for
    /// the two that are partly there. An elided one holds its content and sends a marker. And an
    /// assistant turn under an endpoint that will not take reasoning back - which is every
    /// OpenAI-compatible one - sends what it said and holds what it thought, which on a reasoning
    /// model is most of the session: 25,903 tokens of thinking on one turn, reported nowhere,
    /// with every row on the pane reading under 2k.
    ///
    /// note: `count_item` and `count_message` count an assistant turn's reasoning and calls the
    /// same way, so the two figures are like for like and the difference is a number rather than
    /// an artefact. Where the projection adds something of its own - a label in front of a tool
    /// result - the message is the larger of the two and this is nought, which is the right
    /// answer: nothing is being held back.
    ///
    /// note: and [`Going::holds`] rather than [`ContextItem::tokens`], because like for like is
    /// also about *when*. The field's own note has what a live run made of the difference.
    pub fn held_back(&self, item: &ContextItem) -> usize {
        self.holds(item)
            .saturating_sub(self.costs.get(&item.id).copied().unwrap_or(0))
    }

    /// What this item holds, on the same scale as everything else here.
    pub fn holds(&self, item: &ContextItem) -> usize {
        self.holds
            .get(&item.id)
            .copied()
            // an item added since this was taken; its own figure is the best there is
            .unwrap_or(item.tokens)
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
    /// The advisor wrapped around it, where there is one, for the rating the question draws.
    ///
    /// note: the only thing this is read for. What *decides* is inside the kernel and is reached
    /// through no field here - see the note in `wiring`, which is careful that the policy the
    /// kernel holds and the stances the screen draws are one object and not two. This is the
    /// advisor's own memory of what it said about a command, which nothing else has a way to ask
    /// it for.
    ///
    /// note: `None` in a session started without `--advise`, which is the default, and the whole
    /// of what `assisted-shell` off means at this end.
    #[cfg(feature = "assisted-shell")]
    pub advisor: Option<Arc<crate::tools::Advised>>,
    /// The provider, for switching models - whichever dialect it speaks.
    pub provider: Arc<dyn Dialect>,
    /// A `/model` or `/provider` still settling, which the next line waits for.
    ///
    /// note: both commands hand the switch to a task rather than standing there while it happens,
    /// because finding out what the new model holds and whether the new address serves it is two
    /// round trips and a screen should not stop for them. What the *next line* may not do is read
    /// a session that has not finished changing: `/provider URL ID` followed by `/model` reported
    /// the old model, and a message on the line after a switch could be asked of whichever of the
    /// two won the race. Down a pipe there is no gap between the lines at all, so what is a race
    /// at a keyboard is the ordinary case in a script.
    ///
    /// note: awaited in [`App::submit`] rather than anywhere the provider is read, which is the
    /// narrower door and the right one: a frame drawn mid-switch showing the old name for a
    /// moment is a frame, and the next one corrects it. A *line* acting on the old name is an
    /// answer. The provider's client carries its own timeout, so this cannot wait forever.
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
    /// client which wants the history can have it, and one that does not pays nothing. Before
    /// this, a `context` that rewrote a tool result left the old text nowhere a person could read
    /// it: on the trace as a line of JSON, and in an undo window that closes.
    ///
    /// note: both hands land here now. A terminal edit replaces in place as `context: revise`
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
    /// note: it used to be what `/introspect` moved - the four tools were switched off by dropping
    /// this, which also threw away what `context` was remembering. Turning a tool off is
    /// [`App::toggle`] now, and a shelved tool is still the same tool: what it pinned is still
    /// pinned, and what it could still walk back it still can.
    pub introspect: Option<Arc<Kernel>>,
    /// The tools this session is not offering, by id, kept so that they can be offered again.
    ///
    /// note: the tool itself rather than its id, which is what makes this work for a tool nobody
    /// here wrote. A list of names would mean rebuilding whatever was named, and there is no way
    /// to rebuild an MCP server's tool or an embedder's - so turning one off would have been
    /// turning it off for good. What [`Kernel::remove_tool`] hands back is the tool; keeping it is
    /// the whole mechanism.
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
    /// The failure already reported for the turn now running, so the same one is not said twice.
    ///
    /// note: scoped to a turn, because one failure being reported twice is a fact about a turn -
    /// the kernel emits the event and then the turn comes to the same end, the second wrapping
    /// the first. What this replaced compared against the last line in [`App::loose`], which
    /// outlives every turn: nothing said between two turns goes in there, because a message and
    /// an answer are both drawn from the context, so the first red line stayed the last loose
    /// line for the rest of the session and every later failure with the same words was
    /// swallowed. A model with one canned refusal fails silently from its second refusal on.
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
    /// eleven seconds would still be drawn beside `permission.decided`.
    acted: bool,
    /// Whether it is time to leave.
    pub quit: bool,
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
    /// a browser attached over a socket both have no `ctrl+p` to press, and both were being handed
    /// six pages about one.
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
            #[cfg(feature = "assisted-shell")]
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
            overlay: None,
            scroll: 0,
            follow: true,
            tab: Tab::Chat,
            trace_scroll: 0,
            busy: false,
            acted: false,
            quit: false,
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
        if self.busy || self.broke() {
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
            //
            // note: unless every question was answered while this outcome was on its way, in which
            // case the turn is carried on here. `permission.requested` is broadcast while the turn
            // that raised it is still unwinding, so an answer inside that window is recorded and
            // then goes nowhere: `App::decide` calls `start_turn`, `start_turn` refuses because the
            // old turn is still marked as running, and the session stops for good with every
            // question answered and nothing to answer. The window is narrow and it is not
            // theoretical - it is whatever the gap is between a client's socket and this loop - and
            // what it costs when it opens is a session that never moves again. `headless.rs` stays
            // out of it by only answering while the kernel rests; the keys and a socket cannot,
            // because a person answers when they answer, so it is closed here instead, once, for
            // all three
            Outcome::Stopped(State::Deciding { .. })
                if !self.stepping && self.kernel.pending_permissions().is_empty() =>
            {
                self.start_turn();
            }
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
                // and honestly reports it again, which put this line in the conversation after
                // every message for the rest of a session over one tool result taken out once
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
                // driven down a pipe was told to press a key that does not exist there, about a
                // list it had no other way to see. The command works in both
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
            // the same fact from the two places that can know it, and the difference between them
            // is the whole of what the second line says: one is a count and the other is a guess
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

    /// Answers one of the questions the kernel is waiting on, and carries the turn on if that was
    /// the last of them.
    ///
    /// note: here rather than in `keys.rs`, where the rest of it was, because three loops answer
    /// questions and while it lived with the keys each of them did it in its own words. An answer
    /// is four things: telling the sandbox about a granted command that reaches the network, which
    /// is [`App::answer`] and was already shared for it; honouring `always` over what the policy
    /// consulted; sweeping the questions queued behind this one; and driving the turn on, because
    /// a decision leaves the kernel resting with nobody driving it. The last three were the keys'
    /// alone, so a headless run answered and then sat there. A third caller reached the same fork
    /// and that is what made it a function - see [`crate::remote`].
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

        if remember {
            // everything the policy actually consulted, not just what the tool declared
            self.policy.always(&self.policy.judges(&request));
        }
        // the other thing a session waits on somebody for. Whatever the question cost in wall time
        // was spent reading it, and `permission.decided` is the line it lands on
        self.acted = true;
        self.answer(&request, grant)?;
        if remember {
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

    /// What the *program* has said since the last look, for a loop that has to print it.
    ///
    /// note: only [`Speaker::Note`] and [`Speaker::Error`]. The model's own words arrive as
    /// fragments and land in the same list, so a caller echoing those as well prints every answer
    /// twice.
    ///
    /// note: `said` is how many of *these* have been taken rather than how far down
    /// [`App::loose`] the caller had got, and the difference is a bug only a live run can find.
    /// The list is not append-only - a turn being recorded takes every line that streamed out of
    /// it, because the context says those now - so a mark against the whole list slides backwards
    /// under its own watermark, and everything said between one shrink and the next is skipped. A
    /// scripted model answers between two looks, so nothing shrinks in between and no test sees
    /// it; a live run lost `spent 1,106 tokens of 500; stopping`, which was decided, recorded,
    /// and acted on, and whose only missing reader was the person. What the filtered sequence
    /// *is* is append-only: a note is not something that streams.
    ///
    /// note: with one exception, and it is `/clear`. [`App::clear_notices`] empties this sequence
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

        // note: and what it is *called* follows the same rule. A caller with no keys was being
        // handed a page of slash commands under the title `the keys`, which is the panel telling
        // it that what it is reading is the thing it has not got
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
            // three were swallowing it: `context_key` and `permissions_key` return early when
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
                // the pinned question, which is what `Focus::Body` means on the chat tab
                (Tab::Chat, Focus::Body) => self.question_key(key).await,
                // a question is on the screen and has not been given the keys, so the prompt is
                // not on the screen either and there is nothing here for a key to do
                (Tab::Chat, Focus::Input) if self.asking() => self.locked_key(key),
                (Tab::Context, Focus::Body) => self.context_key(key, &count),
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
    /// note: it counts, and [`App::cleared`] is the count. A loop with no screen reads its lines
    /// through [`App::notes`], which is a watermark over the filtered sequence and is safe only
    /// while that sequence grows - and this is the one thing that empties it. A caller that did
    /// not notice would skip the next `said` lines for good.
    pub fn clear_notices(&mut self) {
        self.loose
            .retain(|entry| entry.open || !matches!(entry.speaker, Speaker::Note | Speaker::Error));
        self.cleared += 1;
    }

    /// How many times the program's own lines have been taken off the chat.
    ///
    /// note: for a caller holding a watermark into [`App::notes`], and it is the whole of what
    /// such a caller has to do about `/clear`: keep this beside `said`, and when it moves, set
    /// `said` back to nothing. There is no arithmetic to do, because what a clear leaves behind
    /// is not a shorter sequence but an empty one - everything it removes is exactly what `notes`
    /// filters *for*, and the only survivor is a line still being streamed, which is a model
    /// speaking and not a notice.
    pub fn cleared(&self) -> u64 {
        self.cleared
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

    /// Whether anything is standing in the prompt's place, waiting to be answered.
    ///
    /// note: the two kinds are a tool waiting on a decision and a compaction waiting on one, and
    /// everything about the screen that cares - what has the keys, what `tab` reaches, whether
    /// there is a prompt at all - cares only that there is one. Which it is, is a question for
    /// the panel that draws it and the key that answers it.
    pub fn asking(&self) -> bool {
        self.asked().is_some() || self.proposed.is_some()
    }

    /// Answers the compaction standing in the prompt's place.
    ///
    /// note: `take` works the pass out again rather than applying what was listed. The list is a
    /// snapshot of a context somebody has just been invited to change, so applying it would take
    /// exactly what they had protected while reading it. The kernel refuses a pinned item and
    /// says so, which would catch it - afterwards, in a report, which is the shape this whole
    /// question exists to get away from.
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
            Tab::Chat => !self.asking(),
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
