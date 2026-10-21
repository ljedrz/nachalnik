//! The wire: what a client may ask for, what a session says back, and how either is framed.
//!
//! note: every type here projects something the runtime or [`App`] already has; none is a second
//! account of it. A runtime type that serializes usefully is carried verbatim, as [`Record`] and
//! [`PermissionRequest`] are. The types defined here exist because the thing they project is either
//! borrowed ([`App::conversation`] hands out `Cow`s into the context) or is half screen state
//! ([`crate::app::Overlay`] carries which page is open and how far down it is scrolled, neither of
//! which is any client's business but its own).

use nachalnik::{
    Budget, ContextId, ContextItem, ContextState, Event, Grant, ModelInfo, PermissionId,
    PermissionRequest, Record, State,
};
use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::app::App;
use crate::app::{Did, Going, Page, Said, Speaker, Stance};
pub use crate::tools::Reached;

mod abridge;
mod frames;

pub use frames::{
    Frames, MAX_LINE, MAX_PATH, framed, overlong, overlong_path, read, write, write_frame,
};

/// The version of this wire that this build speaks.
///
/// note: the field exists before it is needed because it cannot be added once two ends are
/// deployed. `RUNNING.md` recommends reaching a session with `ssh -L` from another machine, where
/// two installed versions meet and where a session that has grown a [`Message`] variant meets a
/// client that has not. A session refuses a version it does not know and serves an older version it
/// does.
///
/// note: the second half of that rule is not implemented yet, and has nothing to do while the
/// version is `1`. Serving an older client means not sending it a message its version lacks, which
/// needs the version number kept per connection and every write to check it. Meanwhile
/// [`Message::Unknown`] makes an unrecognised message something a client survives rather than
/// something that ends it. When the version moves past `1`, both ends need the whole rule;
/// `POSTPONED.md` says what that costs.
pub const VERSION: u32 = 1;

/// What a client asks a session to do.
///
/// note: ten, and the set is meant to stay about this size. Eight of them are things a person at
/// the terminal does with a *key* rather than with a line - hand in a line, stop the turn, answer
/// either kind of question, move an item, read one, rewrite one, change a rule - and the other two
/// are the connection itself. Anything a person types is a slash command, sent as a
/// [`Command::Submit`], because every verb this program has goes through [`App::submit`], and a
/// message per verb would be a second vocabulary to keep in step with the first. The test for
/// anything new is whether a person could type it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Command {
    /// Begin or resume watching the session.
    ///
    /// note: one message covers `attach` and `resume` because resuming is attaching with a
    /// watermark. `since: None` means "I have nothing"; the session answers with an [`Attached`]
    /// and then the records after it. `since: Some(n)` means "I have everything through n"; the
    /// session answers with a [`Message::Done`] and the records after n. A client that lost its
    /// process sends the first; a client that only lost its socket sends the second.
    Attach {
        /// The last record the client is sure it has.
        since: Option<u64>,
        /// The session those records came from, as [`Attached::session`] named it.
        ///
        /// note: keeps a watermark from landing in the wrong session, the quiet case. A session
        /// restarted at the same address has a log of its own, and a number from the one before it
        /// is either too large - refused, loudly - or plausible, and then a client draws one
        /// session's records under another session's conversation with no sign of it anywhere. A
        /// name costs one field and turns that into a refusal.
        ///
        /// note: `None` is a client not saying, as a resume typed by hand into `nc` is, and it is
        /// served on the old terms. Anything that keeps a conversation across a reconnection should
        /// send it.
        session: Option<String>,
        /// Which version of this wire the client speaks; see [`VERSION`].
        ///
        /// note: `None` is a client written before the field existed, so it is version 1 by
        /// definition. A default of "unknown" would refuse the clients the field was added to keep
        /// working.
        version: Option<u32>,
    },
    /// Hand in one line, exactly as it would be typed at the prompt: a message, or a command.
    Submit {
        /// The line.
        line: String,
    },
    /// Ask the running turn to stop at the next opportunity.
    Interrupt,
    /// Answer one of the questions a tool is waiting on.
    Decide {
        /// Which question.
        id: PermissionId,
        /// The answer.
        grant: Grant,
        /// Whether to remember it, so the policy stops asking. This is the `a` key.
        ///
        /// note: use it with an allow only. It remembers every subject the policy consulted, as
        /// allowed. A refusal with this on is answered `failed` rather than taken.
        remember: bool,
    },
    /// Answer a running command that reached for the network; see [`Message::Reaching`].
    ///
    /// note: separate from [`Command::Decide`] because the two questions are numbered by different
    /// things. `Decide` answers the kernel's question, which the kernel numbers and records; this
    /// answers one the kernel never asked, raised by the gate during a call and numbered by the
    /// policy. One command taking either identifier would make a client guess which question a
    /// number meant.
    Reach {
        /// Which question, as [`Reached::id`] names it.
        id: u64,
        /// The answer.
        grant: Grant,
        /// Whether to allow the network from here on. This is the `a` key.
        ///
        /// note: allow only, for the same reason as `Decide`.
        remember: bool,
    },
    /// Move one item to the next state in the ring: seen, a marker where it was, gone, seen again.
    ///
    /// note: its own command rather than a [`Command::Submit`] line, because there is no line to
    /// submit; `space` on the context tab does this at a terminal. The three moves are one ring
    /// rather than three commands because the middle move is used most often and has no name a
    /// person types.
    ///
    /// note: the session decides the next state and the note written beside it, not the client. The
    /// model reads that note, so a client choosing its own words would put a second account of one
    /// act in front of it. See [`crate::app::App::cycle`], which the keys and this command both
    /// call.
    Cycle {
        /// Which item.
        id: ContextId,
    },
    /// Put an edited item into the context in place of the one it came from.
    ///
    /// note: its own command rather than a [`Command::Submit`] line, because, as with
    /// [`Command::Cycle`], there is no line. `e` on the context tab does this at a terminal and
    /// opens an editor holding the item. The command hands over a body of text somebody has been
    /// editing, not a line typed at a prompt and not something a slash command could carry without
    /// quoting rules.
    ///
    /// note: the session decides whether an item can be rewritten at all, not the client. Three
    /// shapes cannot be rewritten: a picture, a turn recorded in blocks, and a turn that is nothing
    /// but a call. The reason is the item's shape, not who is asking. See
    /// [`crate::app::App::revise`], which the keys and this command both call.
    Revise {
        /// Which item.
        id: ContextId,
        /// What it should say now.
        text: String,
    },
    /// Tell the policy what to answer about one row of the permissions tab from here on.
    ///
    /// note: a command of its own rather than [`Command::Submit`] of a line, as [`Command::Cycle`]
    /// shows: `a`, `n` and `r` on the permissions tab do this at a terminal, and nobody types a
    /// line for it.
    ///
    /// note: only a row the tab draws, a subject somebody has decided; the session answers with the
    /// projection the rows come from. [`crate::app::App::rule`] is what the keys and this both go
    /// through.
    Rule {
        /// Which row, spelled as [`Stanced::subject`] spells it.
        subject: String,
        /// What the policy answers about it from here on; `ask` makes it a question again.
        verdict: nachalnik::Verdict,
    },
    /// Ask for the projection again, as it stands now.
    ///
    /// note: [`Command::Attach`] already answers with one and is not the way to do this. A client
    /// takes an `attached` as *start again* (see [`Message::Projected`]), so a client that only
    /// wants today's figures would throw away the conversation it already had to refresh a token
    /// count.
    ///
    /// note: this is for the half of a session that is not the conversation: the items, the budget,
    /// and what the policy will answer. A client renders this as a second view, which the records
    /// cannot supply: `context.added` names an item and says nothing about what the *next request*
    /// will do with it. `going`, `left_out` and `marker` answer that question, not what happened.
    /// They are worked out by projecting, so they are only true as of a moment; this is how a
    /// client asks for that moment to be now.
    Project,
    /// Ask what one context item actually says.
    ///
    /// note: the session log names items rather than copying them, so a client fed only events
    /// cannot read anything that happened before it connected. It gets the conversation from
    /// [`Attached`] and the *whole* of any one item from here, on demand; see [`crate::remote`].
    Inspect {
        /// Which item.
        id: ContextId,
        /// Ask for the item's own text rather than the reading of it.
        ///
        /// note: two questions about one item, with different answers. The reading is what the
        /// context tab shows: the content, and above it why the item is here, what the turn was
        /// thinking, and the calls it carries, which is what somebody wants in front of them.
        /// [`Command::Revise`] takes the content and nothing else, because that is what
        /// `Kernel::replace` writes. A client that committed the reading back would put
        /// `it is here because: …` inside the item it was describing.
        ///
        /// note: a flag rather than a second field on [`Message::Item`] carrying both. An inspect
        /// is the one message that exists to be large: it is how the whole of a four-hundred-line
        /// tool result is fetched. A client wants one of these per row, never both.
        #[serde(default)]
        raw: bool,
        /// Which version to answer with: `None` for what the item says now, or `1` for the oldest
        /// one still kept, counting up to [`Listed::versions`].
        ///
        /// note: the numbering the terminal's own strip of faces uses, so `v1` means the same
        /// thing on a screen and in a browser. What the item says *now* has no number here because
        /// it has no fixed one: it is one past the last, and that moves every time somebody edits.
        #[serde(default)]
        version: Option<usize>,
    },
    /// Something this build has no name for.
    ///
    /// note: a newer client's verb, answered with a [`Message::Failed`] rather than by closing the
    /// connection. This keeps the invariant [`Message::Done`] states: a client that sent something
    /// is owed exactly one answer whether or not this end knows what it was. Without it an unknown
    /// `do` would be a parse error, and a parse error takes the connection with it.
    #[serde(other)]
    Unknown,
}

/// What a session says to a client.
///
/// note: the split that matters is [`Message::Record`] against everything else. It is the same
/// split the runtime draws. `Config::record_progress` is the runtime's name for that line, and this
/// protocol does not draw a second one. A record is numbered, is in the session log, and cannot be
/// lost: a client that missed one asks for it by sequence and gets it. Everything else - fragments
/// of a model still writing, what the program said for itself, the answer to a line - is in no log,
/// is best-effort, and is gone once it has gone past.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "is", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Message {
    /// Where the session stands, for a client that has just arrived.
    ///
    /// note: boxed because it is much the largest of these and every other variant would
    /// otherwise be sized against it.
    Attached(Box<Attached>),
    /// The projection again, for a client that asked, and nothing else changes with it.
    ///
    /// note: the same payload as [`Message::Attached`] under a different name, and the name is what
    /// matters. A client takes an `attached` as *start again*: it has just been handed the
    /// conversation and every record after it, so whatever it had drawn belongs to a stream it is
    /// no longer on. This one changes nothing: the stream is where it was, the records already sent
    /// are still the records, and only the figures are newer. A client that could not tell the two
    /// apart would wipe its own screen to refresh a token count.
    ///
    /// note: it carries no `id:` where a gateway gives one, for the same reason [`Message::Item`]
    /// does not. An id is what a browser resumes from, and this is an answer to a command rather
    /// than a place in the stream.
    Projected(Box<Attached>),
    /// One entry of the session log, verbatim.
    Record(Record),
    /// One entry of the session log that is longer than the other end will read, named instead of
    /// sent.
    ///
    /// note: the record is still in the log - this is a gap in what was *sent*, not in what
    /// happened. [`MAX_LINE`] says why it is named rather than sent.
    ///
    /// note: numbered, which is what lets a client get past it: it takes the `seq` as seen and
    /// resumes after it. What it has is a hole it knows the size and position of, and
    /// [`Command::Inspect`] is where the content is when somebody wants it, as for any content
    /// fetched on demand.
    Oversized {
        /// Which record.
        seq: u64,
        /// How many bytes it would have been on the wire.
        bytes: usize,
    },
    /// A fragment of something still arriving: a model writing, or a tool talking.
    ///
    /// note: unnumbered on purpose. These are not in the log by default, so there is no sequence to
    /// give them and nothing to recover them from. `after` is the last record the client was sent,
    /// which tells it where the fragment hangs.
    Progress {
        /// The record this arrived after.
        after: u64,
        /// The fragment, as `model.delta` or `tool.output`.
        event: Event,
    },
    /// The program's own lines are gone: `/cleanup`, or `ctrl+l` at a terminal watching the same
    /// session.
    ///
    /// note: a client drops the [`Message::Said`] lines it has drawn and keeps everything else,
    /// because that is what was cleared. The conversation is the context and is not this program's
    /// to take away.
    ///
    /// note: unnumbered and best-effort, like the lines it is about. They were never in the log, so
    /// there is nothing to recover them from. A client that missed this is carrying lines the
    /// session no longer has, and the next [`Message::Attached`] takes them away, because its
    /// conversation does not have them either. A missed `Said` recovers the same way, which is why
    /// the two have the same standing.
    Cleared,
    /// Something the program said for itself: a command's answer, a notice, an error.
    Said {
        /// Who said it.
        speaker: Speaker,
        /// What it says.
        text: String,
    },
    /// One command was done, and there is nothing to say about it beyond that.
    ///
    /// note: every command a client sends gets exactly one answer - this, or the
    /// [`Message::Attached`], [`Message::Projected`], [`Message::Replied`] or [`Message::Item`]
    /// that carries one, or a [`Message::Failed`] - and that is a property of the protocol rather
    /// than a convenience. A client that cannot tell when the session has caught up with what it
    /// asked for is guessing, and it always guesses wrong about the same thing: whether its own
    /// last line has happened yet. See [`Message::Busy`] for the other half.
    Done {
        /// Which command it was about.
        about: String,
        /// Whether a turn is running, as of applying it.
        busy: bool,
    },
    /// Whether a turn is running, sent whenever that changes.
    ///
    /// note: on the wire because it cannot be worked out from the records. A turn is a loop over
    /// transitions, so `state.changed` reaches `Idle` between two requests of one turn and `Ready`
    /// between a decision and the calls it decided running: every state a turn rests in is also a
    /// state it passes through. Only the loop driving the kernel knows where a turn begins and
    /// ends. A client may read this as the session doing something on its behalf;
    /// [`Attached::busy`] is the same fact for a client that has just arrived.
    Busy {
        /// Whether a turn is running, or a command is still out at the endpoint; see
        /// [`App::working`](crate::app::App::working).
        busy: bool,
    },
    /// The running commands waiting to hear whether they may reach the network, sent whenever that
    /// changes and to a client that resumes: the whole list, oldest first, and empty once none are.
    ///
    /// note: the whole list rather than one arrival at a time, because nothing numbers these on the
    /// wire. The question is not the kernel's, so it is in no record - see
    /// [`crate::tools::Reaching`] - and a client that missed an arrival, or the answer somebody
    /// else gave, would otherwise offer a question the session no longer has.
    /// [`Attached::reaching`] is the same list for a client that has just arrived.
    ///
    /// note: an older client reads this as [`Message::Unknown`] and cannot answer it, leaving the
    /// command waiting for somebody who can. That is the rule `Unknown` is for, and the question is
    /// still on every screen that knows it.
    Reaching {
        /// The questions, oldest first.
        waiting: Vec<Reached>,
    },
    /// Which model the requests are going to, sent whenever that changes and to a client that
    /// resumes.
    ///
    /// note: on the wire and in the records; `Kernel::provider_changed` says when the model
    /// changed. `/model` and `/endpoint` finish inside the
    /// [`Dialect`](nachalnik_providers::Dialect) the kernel already holds, so the record comes when
    /// the switch is done. A client that follows the model on screen is told the moment the
    /// session's answer changes, as [`Message::Busy`] tells it the other thing a screen shows.
    ///
    /// note: broadcast rather than answered to whoever typed it, like every other notice: a session
    /// two people are watching is one session, with one model.
    Model {
        /// The model the requests are going to, or nothing where there is no provider.
        model: Option<ModelInfo>,
    },
    /// What one submitted line did.
    ///
    /// note: it does not carry the lines the program said about it. Those go to every attached
    /// client as [`Message::Said`], because a session with two people watching has one voice, and
    /// carrying them in the reply would print them twice for whoever asked.
    Replied {
        /// What the line did.
        did: Did,
        /// The page it opened, if it opened one.
        page: Option<Printed>,
        /// Whether a turn is running, as of handing the line in.
        ///
        /// note: carried rather than inferred from `did`, because `Asked` starts a turn unless the
        /// session has spent its ceiling, and inferring it from `did` would make a client wait for
        /// the end of a turn that never started.
        busy: bool,
    },
    /// The whole of what one context item says.
    Item {
        /// Which item.
        id: ContextId,
        /// What it says: laid out the way the context tab lays it out, or the item's own text
        /// where [`Command::Inspect`]'s `raw` asked for that instead.
        body: String,
        /// Which of the two this is.
        ///
        /// note: carried back rather than remembered by the client, because both answers are found
        /// by the same identifier, and a client with two places to put one - a line on the chat and
        /// a box on the context tab - would otherwise fill the wrong one with the wrong reading.
        /// Echoing the question is the cheapest way to tell them apart and the only way that
        /// survives two inspects crossing.
        #[serde(default)]
        raw: bool,
        /// Which version this is, echoed from [`Command::Inspect`]; `None` is what it says now.
        #[serde(default)]
        version: Option<usize>,
    },
    /// The text of an item this client asked `/copy` for, to put on its own clipboard.
    ///
    /// note: sent to the client that asked and nobody else, just before the answer to the line.
    /// The session's terminal is not where the person who typed `/copy` is, so the session cannot
    /// put it on their clipboard itself.
    Copy {
        /// Which item.
        id: ContextId,
        /// The item's own text, as the terminal's `/copy` takes it.
        text: String,
    },
    /// How many fragments the client was not sent: while it was not keeping up, or because one was
    /// too long for a frame.
    ///
    /// note: reported, with a number. The records are unaffected: they are read from the log, which
    /// drops nothing, so no record is ever lost this way. It counts what is in no record: a model's
    /// fragments and the session's own lines. The log names these lines but does not copy them, so
    /// a client told that the records hold what it missed would be sent after something that was
    /// never in them. The number means "you missed some of what was being said", never "you missed
    /// something that happened".
    Missed {
        /// How many.
        frames: u64,
    },
    /// A command could not be done.
    ///
    /// note: `about` names a command, or something else. `version`, `projection` and `attach`
    /// refuse an attach, for the reasons `connection::Refused` gives. `the connection` is one that
    /// failed. `replaced` is a client let go of because another has attached. A session serves one
    /// client at a time. A client replaced should not come back by itself: it would take the
    /// session from whoever has just taken it.
    Failed {
        /// Which command it was about.
        about: String,
        /// What went wrong.
        error: String,
    },
    /// Something this build has no name for.
    ///
    /// note: the rule is **ignore what you do not know**, so a new variant is not a break. A client
    /// written against version 1, attached to a session that has grown a message since, reads this,
    /// prints nothing, and carries on with the records. Without it, the new message is a parse
    /// error and a closed connection, and a minute of trying to get back to a session that was
    /// working.
    ///
    /// note: it cannot carry the payload, so a relay - the page in `web` - reads the wire as JSON
    /// and passes it on rather than parsing each message and writing it out again. Turning an
    /// unknown message into this and back would let the relay decide what a page is allowed to
    /// hear.
    #[serde(other)]
    Unknown,
}

/// Where a session stands, for a client that has just arrived.
///
/// note: a projection rather than a [`nachalnik::Snapshot`]: a snapshot carries every item's whole
/// content, blobs included, so attaching a browser to a session that has read four files would push
/// megabytes at it before it asked for anything. A client needs the conversation, what each item
/// is, and what the next request does with it to start rendering. The whole of any one item is one
/// [`Command::Inspect`] away.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Attached {
    /// The version of this wire the session speaks. See [`VERSION`].
    ///
    /// note: the other half of [`Command::Attach`]'s `version`, here now because it cannot be added
    /// later on the same terms. A client learns what the session speaks from the first thing it is
    /// sent, which tells it whether a command this build knows is worth sending. A field added
    /// after both ends are deployed is one a client can only read as `Option`, which means "and it
    /// might be anything".
    pub version: u32,
    /// The session's name, which is also what its record is filed under.
    pub session: String,
    /// The last record this reflects. Everything after it arrives as a [`Message::Record`].
    ///
    /// note: taken before the rest of this, because a running turn goes on changing the session
    /// while the projection is read. So nothing is in neither the projection nor the stream after
    /// it, and a record just after `seq` may already be reflected here: a client applies records
    /// by the identifiers in them rather than by counting.
    pub seq: u64,
    /// What the runtime is doing.
    pub state: State,
    /// Whether a turn is running, or a command is still out at the endpoint.
    pub busy: bool,
    /// Whether the loop is being driven a transition at a time.
    pub stepping: bool,
    /// The model the requests are going to, if one is set.
    pub model: Option<ModelInfo>,
    /// What the next request comes to, and how much room there is.
    pub budget: Budget,
    /// What the next request is expected to cost, as the terminal's status line says it; see
    /// `App::expected`.
    ///
    /// note: beside [`Attached::budget`] rather than in place of it, because the budget is the
    /// counter's own estimate and this is anchored on what the provider charged for the last
    /// request, where it said. A client that showed the budget's figure showed a different number
    /// from the terminal's. `None` from a session that predates the field, where the budget's
    /// figure is all there is.
    #[serde(default)]
    pub expected: Option<usize>,
    /// What the provider has charged for this session so far.
    pub spent: u64,
    /// What it may charge before the session stops, if anything says.
    pub spend: Option<u64>,
    /// Whether that ceiling has been reached.
    pub overspent: bool,
    /// The conversation as it reads now.
    ///
    /// note: whole unless it does not fit in one frame, in which case the longest lines are cut to
    /// fit and each cut says by how much; see [`Line::clipped`].
    pub conversation: Vec<Line>,
    /// Every context item, named rather than carried.
    pub items: Vec<Listed>,
    /// Every question waiting on somebody, in the order they were asked.
    pub asking: Vec<PermissionRequest>,
    /// Every running command waiting to hear whether it may reach the network; see
    /// [`Message::Reaching`].
    ///
    /// note: empty from a session that predates the field, which has no such question to ask.
    #[serde(default)]
    pub reaching: Vec<Reached>,
    /// What the advisor made of the ones it was asked about; see [`Judged`].
    ///
    /// note: a list beside the questions rather than a field on them, and empty in every session
    /// that did not start with an advisor. Without it the rating exists only inside the terminal's
    /// drawing code: the advisor rates the question, but nothing a person was supposed to see
    /// leaves the process.
    pub rated: Vec<Judged>,
    /// Why the advisor has no rating for the questions it was asked about and could not answer.
    ///
    /// note: beside [`Attached::rated`] and in the same shape, one row per question, so a client
    /// draws one line or the other where the band would go. Without it, an unrated question looks
    /// like one the advisor had nothing to say about, and the requests the endpoint refuses are
    /// about the commands most worth a colour.
    ///
    /// note: empty for a session that predates the field; a client reads that as the absence it
    /// drew before.
    #[serde(default)]
    pub unrated: Vec<Unjudged>,
    /// What the policy in force is called.
    pub policy: String,
    /// What it answers about anything nobody has told it about.
    ///
    /// note: the line the permissions tab puts above the rules, and it goes with them for the
    /// reason it is above them there: a list of decisions is not a policy, and a client that drew
    /// the rows alone would answer "what will this session allow" from only the part of the answer
    /// somebody already gave.
    pub untold: nachalnik::Verdict,
    /// What the policy will answer about each capability and path rule somebody has decided.
    pub permissions: Vec<Stanced>,
    /// The trace, as the trace tab draws it: what happened, in this program's words.
    ///
    /// note: capped where `App` caps it and not otherwise trimmed. The session log is unbounded and
    /// is what `Command::Attach` streams; this pane is a ring of the last few hundred lines.
    pub trace: Vec<Tracing>,
    /// How many more it will ask about.
    ///
    /// note: a count rather than rows, as the permissions tab gives: a row for a `.aws` rule nobody
    /// has thought about is not information. The count is for the other half: a client listing two
    /// decisions while standing for eighteen answers misleads too, and a client cannot work the
    /// count out from the list above, because the list is what it leaves out.
    pub undecided: usize,
    /// The actions listed under several of [`Attached::permissions`]'s rows, which need every one
    /// of them, as the sentence `/permissions` puts under its rows. The permissions tab and the web
    /// page leave it to GUIDE.md.
    ///
    /// note: `undo` under an allowed `context` row and a refused `context:revise` row reads as
    /// allowed by the first; it is refused by the second. Empty from a session older than this.
    #[serde(default)]
    pub together: Vec<String>,
    /// The next message somebody typed into the running turn, waiting for it to end.
    ///
    /// note: cut where [`Attached::conversation`] is cut, with nothing on it to say so. A waiting
    /// message is the last lines of the conversation, word for word, so the same length cuts both;
    /// the line there carries [`Line::clipped`], which this cannot. Left whole, the message would
    /// be carried twice in a projection being cut down because it is too long.
    pub queued: Option<String>,
    /// The ones waiting behind that one, oldest first; each gets a turn.
    ///
    /// note: beside [`Attached::queued`] rather than instead of it, because a session holds a queue
    /// now and held one message before. A client written against `queued` reads the next one and is
    /// not broken by the rest; a projection from an older session reads as nothing waiting behind.
    #[serde(default)]
    pub queued_behind: Vec<String>,
    /// What the shell tool's sandbox came to, or nothing where there is none.
    pub confinement: Option<String>,
}

/// What the advisor made of the command the waiting question is about.
///
/// note: beside [`Attached::asking`] rather than on a question, because a question is a
/// [`PermissionRequest`] and that is the runtime's type. The runtime has no advisor and will not
/// grow a field for one (see the module note on carrying runtime types verbatim), so the rating
/// travels as its own row, named by the question it is about.
///
/// note: only for questions something rated: `shell` commands in a build with `shell-advisor` and a
/// session started with `--rate-shell-commands`. A client draws the band where there is a row for
/// the question and draws nothing where there is not, as the panel at a terminal does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Judged {
    /// Which question.
    pub id: PermissionId,
    /// The band it is drawn as.
    pub band: Band,
    /// What that band is called, in the advisor's own words.
    ///
    /// note: carried rather than left for the client to word from `band`, so the browser and
    /// terminal print one string with one author. A client that wrote its own would be a second
    /// account of a rubric it cannot see.
    pub said: String,
    /// How sure the advisor was, from 0 to 1.
    ///
    /// note: sent beside the band and not folded into it, for the reason the terminal prints it: a
    /// band that went yellow because nothing could be told apart is not the same warning as one
    /// that went yellow because the command changes something, and a client that could not tell
    /// them apart would be told the advisor was sure when it was not.
    pub confidence: f64,
    /// Which stage of the command earned the band, as a byte range into the `cmd` argument.
    ///
    /// note: on the wire because it cannot be worked out at the other end. A client would need two
    /// things to point at this itself: where the command comes apart, which is
    /// [`joints`](crate::tools::joints) and is Rust in this crate, and which stage the advisor
    /// liked least, which only the advisor knows. Left off, where in a long chain the trouble is
    /// would be unreachable from anywhere but the process that produced it.
    ///
    /// note: a range, not the text, so a client points at the stage in the command it is already
    /// drawing instead of printing a copy underneath. A second copy of a long stage takes a row at
    /// a terminal and most of a phone screen.
    ///
    /// note: absent for a command rated in one piece, and for a session that predates the field. To
    /// a client that is one case: nothing to point at, and a band to draw as before.
    #[serde(default)]
    pub worst: Option<(usize, usize)>,
}

/// A question the advisor was asked about and could not rate, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Unjudged {
    /// Which question.
    pub id: PermissionId,
    /// Why there is no rating, in the program's words and cut short.
    pub why: String,
}

#[cfg(feature = "shell-advisor")]
impl Judged {
    /// Reads one off what the advisor wrote down while the verdict was being worked out.
    ///
    /// note: [`Rated::shown`](crate::tools::Rated::shown) rather than the score, so that a browser
    /// and a terminal cannot land on two different colours for one command. Where the thresholds
    /// are, and what an unsure reading is drawn as, stay in `tools::advice` and are asked rather
    /// than reimplemented.
    pub(super) fn of(id: PermissionId, rated: crate::tools::Rated) -> Self {
        use crate::tools::Rating;

        let shown = rated.shown();

        Self {
            id,
            band: match shown {
                Rating::Reads => Band::Reads,
                Rating::Changes => Band::Changes,
                Rating::Grave => Band::Grave,
            },
            said: shown.said().to_owned(),
            confidence: rated.confidence,
            worst: rated.worst,
        }
    }
}

/// The three bands a rated command is drawn as.
///
/// note: the band is what the advisor's rating is shown as, not what it scored;
/// [`crate::tools::Rated::shown`] owns that distinction and nothing on this side of the wire
/// repeats it. An uncertain reading is never sent as [`Band::Reads`], because green is the one
/// colour that would say a command is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Band {
    /// It looks, or moves about; nothing is left changed and nothing goes out.
    Reads,
    /// It changes files inside the working directory, the way git or a rebuild could undo.
    Changes,
    /// It reaches outside the working directory, destroys something that cannot be got back, or
    /// sends something off this machine.
    Grave,
}

/// One line of the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Line {
    /// Who said it.
    pub speaker: Speaker,
    /// What it says.
    pub text: String,
    /// The context item it is part of, where it is part of one.
    ///
    /// note: carried so that a client can ask [`Command::Inspect`] about the line somebody is
    /// looking at. A tool result reads as its first six lines here, the same as it does on the
    /// chat tab, and this is how the other four hundred are reachable.
    pub item: Option<ContextId>,
    /// How many bytes were left out of [`Line::text`], which is then the start of the line; `None`
    /// where the line is whole.
    ///
    /// note: set only where a projection would otherwise be longer than [`MAX_LINE`], which a
    /// client cannot read and a session cannot skip. Nothing is cut until it has to be, and then
    /// only lines longer than the longest length that lets the whole projection fit, down to that
    /// length. Whether a line arrives whole depends on the length of the session, so a line a
    /// projection carried whole can be cut in the next projection once the session has grown.
    ///
    /// note: where the rest is. If the line is an item, [`Command::Inspect`] asks for all of it and
    /// is answered wherever the answer fits in a frame. An item longer than [`MAX_LINE`] on its own
    /// does not fit, so it is refused and a snapshot holds it whole. If the line is not an item -
    /// something the program said, or a message still waiting for a turn to end - the rest is not
    /// anywhere a client can ask for, and the count is all there is.
    ///
    /// note: a field rather than a sentence at the end of the text, because the text is somebody's
    /// message and a note in this program's voice would read as part of what they said. Drawing it
    /// is the client's job.
    #[serde(default)]
    pub clipped: Option<usize>,
}

impl Line {
    /// Copies one out of what [`App::conversation`] borrowed.
    pub(super) fn of(said: &Said<'_>) -> Self {
        Self {
            speaker: said.speaker,
            text: said.text.to_string(),
            item: said.item.map(|item| item.id),
            clipped: None,
        }
    }
}

/// One line of the trace, as the trace tab draws it.
///
/// note: the trace rather than the records. A record says `context.compacted` and carries a
/// `CompactionReport`; the trace line says what that pass took and what it left, in this program's
/// words, because a reader wants the sentence and not the structure. A client that rendered the
/// records itself would write a second vocabulary for the same events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Tracing {
    /// The dotted name, e.g. `model.requested`; empty for a continuation of the line above.
    pub name: String,
    /// The rest of it.
    pub detail: String,
    /// Always `None`. It was how long after the line above this one arrived, and the trace no
    /// longer says that: the line above was often not the wait anybody was asking about.
    ///
    /// note: kept so that removing it waits for a release that breaks the API anyway. A client that
    /// wants the time between two lines has `at` on both.
    pub gap: Option<String>,
    /// When it arrived, in milliseconds since the Unix epoch.
    ///
    /// note: the wall clock rather than the monotonic one, because this is what a reader matches
    /// against a server log or their own memory, and an `Instant` has no rendering as a time of
    /// day.
    pub at: u64,
}

/// One context item, as a row: what it is, what it costs, and what the next request does with it.
///
/// note: no content, which is why it is this type rather than a [`nachalnik::ContextItem`], which
/// is `Serialize` and would otherwise have done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Listed {
    /// The item's identifier.
    pub id: ContextId,
    /// What it is, by the name events use: `user_message`, `tool_result`.
    pub kind: String,
    /// Where it came from.
    pub source: String,
    /// Its short name: a path, a command, a description.
    pub label: String,
    /// What it is estimated to cost.
    pub tokens: usize,
    /// How many pieces of it nothing here could put a number on.
    pub uncounted: usize,
    /// Whether it takes part in the next request, and how.
    pub state: ContextState,
    /// What it costs the next request, where its own content is going into one.
    pub going: Option<usize>,
    /// Why it is not in the next request, in the projector's own words.
    pub left_out: Option<String>,
    /// What the model reads in its place, where it is in the request as a marker.
    pub marker: Option<String>,
    /// How many earlier versions of it are still there to read.
    ///
    /// note: `0` for everything nobody has rewritten, which is nearly every row, so a client draws
    /// the way back through an item's history only where there is one. What it says now is not
    /// counted, because it has no fixed number: it is one past the last, and editing moves it. The
    /// cap is the viewer's, and the oldest goes first when it is reached.
    pub versions: usize,
    /// Why this one cannot be rewritten, where it cannot be.
    ///
    /// note: on the row rather than found out by trying, so that a client can say so before
    /// somebody has typed rather than after. [`Command::Revise`] asks the same question again and
    /// decides - a row is a moment old by the time anybody acts on it - but a client that only
    /// learns from the refusal takes an edit it was never going to keep.
    pub beyond: Option<String>,
}

impl Listed {
    /// Reads one row off an item and the projection it is about to take part in.
    pub(super) fn of(item: &ContextItem, going: &Going, versions: usize) -> Self {
        Self {
            id: item.id,
            kind: item.kind.name().to_owned(),
            source: item.source.clone(),
            label: item.label.clone(),
            tokens: item.tokens,
            uncounted: item.uncounted,
            state: item.state,
            // Use `Going::sends_content`, not the state: a projector repairs a request to keep it
            // valid, and an item in a state that sends content can still be out of that request
            going: going
                .sends_content(item)
                .then(|| going.costs.get(&item.id).copied())
                .flatten(),
            left_out: going.left_out.get(&item.id).cloned(),
            marker: going.marker.get(&item.id).cloned(),
            versions,
            beyond: crate::app::text::beyond_a_prompt(item).map(str::to_owned),
        }
    }
}

/// One row of the permissions tab.
///
/// note: the subject is its own spelling, not a structure. For the three subjects a `Subject` can
/// be read back from, it reads back from the same string it prints as, so
/// `--auto-deny "$(a row off this list)"` means what it says. A second wire shape for it would be a
/// second thing to keep in step with `Subject::parse`. A server's row is the exception, and it is
/// `Subject`'s rather than this type's: it prints as `server files` and is given back with
/// `--auto-allow-servers`, because a server's name and a domain are both bare words and nothing in
/// either says which.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Stanced {
    /// What the row is about: `fs`, `fs:read`, `.env*`, `server files`.
    pub subject: String,
    /// What the policy answers about it today.
    pub verdict: nachalnik::Verdict,
    /// The registered tools that declare it.
    pub tools: Vec<String>,
    /// The registered tools it is judged against only sometimes, by looking at the call.
    pub sometimes: Vec<String>,
}

impl Stanced {
    /// Copies one off the row the permissions tab draws.
    pub(super) fn of(stance: &Stance) -> Self {
        Self {
            subject: stance.subject.to_string(),
            verdict: stance.verdict,
            tools: stance.tools.clone(),
            sometimes: stance.sometimes.clone(),
        }
    }
}

/// Something long enough that a command put it on a screen of its own.
///
/// note: the overlay's two halves that are *about the text* and neither of the two that are about
/// a window. Which page is open and how far down it is scrolled belong to whoever is reading, and
/// a session drawn at a desk with a client attached has two answers to both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Printed {
    /// What it is.
    pub title: String,
    /// Its faces, in the order they are offered; almost everything has exactly one.
    pub pages: Vec<Page>,
}

/// Whether an event is a fragment of something still arriving, not a thing that happened.
///
/// note: the kernel's own list, [`Event::is_progress`], the events it keeps out of its log unless
/// asked.
pub fn is_progress(event: &Event) -> bool {
    event.is_progress()
}

/// Reads a `scheme:rest` address, the one spelling both ends of this take.
///
/// note: a scheme rather than a guess. `/run/kamchatka.sock` and `localhost:7878` are both valid
/// strings and nothing in either says which kind of thing it is, so working it out from a `/` or a
/// `:` is the same mistake as deciding a file's media type by looking at its bytes: right until it
/// is silently wrong, and this crate does not do it anywhere else.
pub fn address(text: &str) -> Result<Address<'_>, String> {
    match text.split_once(':') {
        Some(("unix", path)) if !path.is_empty() => Ok(Address::Unix(path)),
        Some(("tcp", host)) if !host.is_empty() => Ok(Address::Tcp(host)),
        _ => Err(format!(
            "`{text}` is not an address: write `unix:PATH` for a socket file, or \
             `tcp:HOST:PORT` for a port"
        )),
    }
}

/// Where a session listens, or where a client goes looking for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Address<'a> {
    /// A socket file, whose permissions are the whole of its authentication.
    Unix(&'a str),
    /// A host and a port.
    Tcp(&'a str),
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// The one thing a client must not be able to disagree with the terminal about.
    ///
    /// note: an unsure reading is never sent as [`Band::Reads`], because green is the colour that
    /// says a command is safe and a spread distribution over a safety rubric is not evidence that
    /// it is safe. That rule lives in [`crate::tools::Rated::shown`], and this test asks it rather
    /// than restating the threshold: what is under test is that the wire goes through `shown` at
    /// all, which stops a browser drawing green where a terminal drew yellow.
    ///
    /// note: the words travel with the band for the same reason. A client that worded the band
    /// itself would be a second account of a rubric it cannot see, and the first rewording of the
    /// rubric would make them differ.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn a_rating_reaches_the_wire_as_the_band_it_is_drawn_as() {
        use crate::tools::{Rated, Rating};

        let judged = |scored, confidence| {
            Judged::of(
                PermissionId(1),
                Rated {
                    scored,
                    confidence,
                    worst: None,
                },
            )
        };

        let sure = judged(Rating::Reads, 0.95);
        assert_eq!(sure.band, Band::Reads);
        assert_eq!(sure.said, Rating::Reads.said());
        assert_eq!(sure.confidence, 0.95);

        // the same score, and not the same band: nobody could tell, so it is not drawn green
        assert_eq!(judged(Rating::Reads, 0.3).band, Band::Changes);
        assert_eq!(judged(Rating::Reads, 0.3).said, Rating::Changes.said());
        // and what was already worth looking at is never softened by confidence either way
        assert_eq!(judged(Rating::Grave, 0.3).band, Band::Grave);
        assert_eq!(judged(Rating::Grave, 0.95).band, Band::Grave);
    }

    /// The stage that earned the band travels too, and a session that sent none is readable.
    ///
    /// note: `worst` is newer than the message it is on. A client reading a record written before
    /// the field existed must see `worst` as `None`, not a parse failure; a parse failure would
    /// leave the client with no session at all.
    #[cfg(feature = "shell-advisor")]
    #[test]
    fn the_stage_that_earned_a_band_travels_with_it_and_is_optional() {
        use crate::tools::{Rated, Rating};

        let judged = Judged::of(
            PermissionId(1),
            Rated {
                scored: Rating::Grave,
                confidence: 0.9,
                worst: Some((14, 27)),
            },
        );
        assert_eq!(judged.worst, Some((14, 27)));

        let written = serde_json::to_string(&judged).expect("it serialises");
        let read: Judged = serde_json::from_str(&written).expect("and comes back");
        assert_eq!(read, judged);

        // and the same message from before the field existed, which is every session older than
        // it and every command rated in one piece
        let older: Judged = serde_json::from_str(
            r#"{"id":1,"band":"grave","said":"reaches outside, destroys, or sends out","confidence":0.9}"#,
        )
        .expect("a message with no `worst` in it is still a message");
        assert_eq!(older.worst, None);
        assert_eq!(older.band, Band::Grave);
    }

    /// Reader and writer must agree on one byte: the limit is the frame without its trailing
    /// newline.
    ///
    /// note: [`Frames::next`] checks the frame it holds, without the newline that ended it. A
    /// message exactly on the limit is read; one byte more is not. This measures the framed bytes,
    /// because that is what the caller holds. An off-by-one error would either refuse a record
    /// every client could take or send one none can read.
    #[test]
    fn the_limit_is_the_frame_without_the_newline_that_ends_it() {
        let framed = |bytes: usize| {
            let mut line = vec![b'x'; bytes];
            line.push(b'\n');

            line
        };

        assert_eq!(overlong(&framed(MAX_LINE)), None);
        assert_eq!(overlong(&framed(MAX_LINE + 1)), Some(MAX_LINE + 1));
        assert_eq!(overlong(b"short\n"), None);
        // and nothing at all is not a frame, but it is not over the limit either
        assert_eq!(overlong(b""), None);
    }

    /// The reader accepts a frame at the limit and refuses one byte past it.
    ///
    /// note: the reader half of the test above. [`Frames`] is the reader end of every `write` and
    /// checks the frame without its trailing newline. If it refused the limit itself, it would
    /// refuse a message the writer measured as whole, and every client that could have received it
    /// would reject the projection on arrival.
    #[tokio::test]
    async fn a_frame_on_the_limit_is_read_and_one_over_it_is_refused() {
        // a JSON string of the given length, framed as a peer would send it
        let wire = |bytes: usize| {
            let mut line = vec![b'x'; bytes];
            line[0] = b'"';
            line[bytes - 1] = b'"';
            line.push(b'\n');

            line
        };
        async fn read(line: Vec<u8>) -> Result<Option<String>, String> {
            let mut frames = Frames::new(Cursor::new(line));

            super::read::<String>(&mut frames).await
        }

        let at = read(wire(MAX_LINE))
            .await
            .expect("a frame exactly on the limit was refused")
            .expect("a frame exactly on the limit was not read");
        assert_eq!(at.len(), MAX_LINE - 2);

        // and one byte more than that is the refusal, naming the limit it is over
        let error = read(wire(MAX_LINE + 1))
            .await
            .expect_err("a frame over the limit was read");
        assert!(error.contains("over the"), "{error}");
        assert!(error.contains(&MAX_LINE.to_string()), "{error}");
    }
}
