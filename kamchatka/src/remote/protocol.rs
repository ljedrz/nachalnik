//! The wire: what a client may ask for, what a session says back, and how either is framed.
//!
//! note: every type here is a projection of something the runtime or [`App`] already has, and
//! none of them is a second account of it. Where a runtime type serializes usefully it is carried
//! verbatim - a [`Record`] is a `Record`, a [`PermissionRequest`] is a `PermissionRequest` - and
//! the ones defined here exist because the thing they project is either borrowed
//! ([`App::conversation`] hands out `Cow`s into the context) or is half screen state
//! ([`crate::app::Overlay`] carries which page is open and how far down it is scrolled, neither of
//! which is any client's business but its own).

use nachalnik::{
    Budget, ContextId, ContextItem, ContextState, Event, Grant, ModelInfo, PermissionId,
    PermissionRequest, Record, State,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(doc)]
use crate::app::App;
use crate::app::{Did, Going, Page, Said, Speaker, Stance};
pub use crate::tools::Reached;

/// The longest line either end will read before giving up on the connection.
///
/// note: generous, and it has to be. A frame here is not something somebody typed - it is whatever
/// the session put in its log, and `context.replaced` is the one event that carries content, so a
/// rewritten tool result goes down the wire at whatever size it was. What this is defending
/// against is a peer that never sends a newline, which would otherwise be read into memory for
/// ever; a frame over it is a protocol error that closes the connection and says so, rather than a
/// truncation that would leave the reader parsing the second half of somebody's JSON.
///
/// note: enforced while the frame arrives rather than once it has, which is what [`Frames`] is for
/// and why it is not `tokio::io::Lines`. Checked afterwards it is no defence against the case
/// above at all, because the reading is the thing it was supposed to stop.
///
/// note: the reading side enforces this and the writing side *names* it. Nothing caps what the
/// session writes into its log and nothing could: a record is in the log, the log drops nothing,
/// and a client resuming by sequence comes back to the same record every time - so a
/// `context.replaced` over this, sent as it is, makes a session unattachable for the rest of its
/// life. What goes out instead is [`Message::Oversized`], which names the record and its size and
/// lets the client move past it; [`Command::Inspect`] is how the content is fetched when somebody
/// wants it. Raising the number is not the fix: it moves the size of the thing that breaks and
/// changes nothing else.
pub const MAX_LINE: usize = 32 * 1024 * 1024;

/// Whether a framed message is longer than the other end will read, and how long it is.
///
/// note: the newline is not part of what the reader measures. [`Frames`] holds the frame and
/// checks what it holds, which is everything up to the newline and not the newline, so the byte
/// that ends the line comes off before the comparison. One byte, and it decides whether a
/// record sitting exactly on the limit goes out or is named instead.
pub fn overlong(line: &[u8]) -> Option<usize> {
    let bytes = line.len().saturating_sub(1);

    (bytes > MAX_LINE).then_some(bytes)
}

/// The version of this wire that this build speaks.
///
/// note: here before anybody needs it, because it is the one field that cannot be added once two
/// ends are deployed. `RUNNING.md` recommends reaching a session with `ssh -L` from another
/// machine, which is exactly where two installed versions meet, and where a session that has grown
/// a [`Message`] variant meets a client that has not. The rule is that a session refuses a version
/// it does not know and serves an older one it does.
///
/// note: the second half of that rule is not machinery yet, and there is nothing for it to do
/// while this is `1`. Serving an older client means not sending it a message its version lacks,
/// which needs the number kept per connection and every write asking about it; what stands in
/// meanwhile is [`Message::Unknown`], which makes an unrecognised message something a client
/// survives rather than something that ends it. The day this moves, both ends want the whole
/// rule; `POSTPONED.md` says what that costs.
pub const VERSION: u32 = 1;

/// What a client asks a session to do.
///
/// note: nine, and the set is meant to stay about this size. Seven of them are things a person at
/// the terminal does with a *key* rather than with a line - hand in a line, stop the turn, answer
/// either kind of question, move an item, read one, rewrite one - and the other two are the
/// connection itself.
/// Anything a person types is a slash command, which is [`Command::Submit`]: every verb this
/// program has goes through [`App::submit`], so a protocol with a message per verb would be a
/// second vocabulary to keep in step with the first. The test for anything new is whether a person
/// could type it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Command {
    /// Begin, or resume, watching the session.
    ///
    /// note: one message rather than an `attach` and a `resume`, because resuming *is* attaching
    /// with a watermark. `since: None` says "I have nothing", and is answered with an
    /// [`Attached`] and then the records after it; `since: Some(n)` says "I have everything
    /// through n", and is answered with a [`Message::Done`] and the records after n. A client that
    /// lost its process sends the first; one that only lost its socket sends the second.
    Attach {
        /// The last record the client is sure it has.
        since: Option<u64>,
        /// The session those records came from, as [`Attached::session`] named it.
        ///
        /// note: what keeps a watermark from landing in the wrong session, and the case it is for
        /// is the quiet one. A session restarted at the same address has a log of its own, and a
        /// number from the one before it is either too large - refused, loudly - or perfectly
        /// plausible, at which point a client draws one session's records under another session's
        /// conversation and nothing anywhere says so. A name costs one field and makes that a
        /// sentence.
        ///
        /// note: `None` is a client not saying, which is what a resume typed by hand into `nc` is,
        /// and it is served on the old terms. Anything that keeps a conversation across a
        /// reconnection should send it.
        session: Option<String>,
        /// Which version of this wire the client speaks; see [`VERSION`].
        ///
        /// note: `None` is a client written before the field existed, which is version 1 by
        /// definition. A default of "unknown" would refuse exactly the clients it was added to
        /// keep working.
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
        /// note: with an allow only. What is remembered is every subject the policy consulted,
        /// as allowed, and a refusal with this on is answered `failed` rather than taken.
        remember: bool,
    },
    /// Answer a running command that reached for the network; see [`Message::Reaching`].
    ///
    /// note: beside [`Command::Decide`] rather than folded into it, because the two questions are
    /// numbered by different things. A `Decide` answers the kernel's question, which the kernel
    /// numbers and records; this answers one the kernel never asked, raised by the gate while a
    /// call was running and numbered by the policy that holds it. One command taking either
    /// identifier would be a client guessing which question a number meant.
    Reach {
        /// Which question, as [`Reached::id`] names it.
        id: u64,
        /// The answer.
        grant: Grant,
        /// Whether to allow the network from here on. This is the `a` key.
        ///
        /// note: with an allow only, for the reason `Decide`'s is.
        remember: bool,
    },
    /// Move one item to the next state in the ring: seen, a marker where it was, gone, seen again.
    ///
    /// note: a command of its own rather than a [`Command::Submit`] of a line, which is how most
    /// verbs reach a session from here. There is no line: `space` on the context tab is what does
    /// this at a terminal, and the three moves are one ring rather than three commands, because
    /// the step somebody wants most often is the middle one and it is the one with no name a
    /// person types.
    ///
    /// note: the session decides what the next state is and what to write beside it, not the
    /// client. The note it leaves is read by the *model*, so a client picking its own words would
    /// put a second account of one act in front of it - see [`crate::app::App::cycle`], which the
    /// keys and this both go through.
    Cycle {
        /// Which item.
        id: ContextId,
    },
    /// Put an edited item into the context in place of the one it came from.
    ///
    /// note: a command of its own rather than a [`Command::Submit`] of a line, on the same
    /// grounds as [`Command::Cycle`]: there is no line. `e` on the context tab is what does this
    /// at a terminal, and what it opens is an editor holding the item - so the thing being handed
    /// over is a body of text somebody has been editing, which is not something a person types at
    /// a prompt in one go and not something a slash command could carry without quoting rules
    /// nobody wants.
    ///
    /// note: the session decides whether the item can be rewritten at all, not the client. Three
    /// shapes cannot be - a picture, a turn recorded in blocks, and a turn that is nothing but a
    /// call - and the reason is about the item rather than about who is asking; see
    /// [`crate::app::App::revise`], which the keys and this both go through.
    Revise {
        /// Which item.
        id: ContextId,
        /// What it should say now.
        text: String,
    },
    /// Ask for the projection again, as it stands now.
    ///
    /// note: [`Command::Attach`] already answers with one and is deliberately not the way to do
    /// this. A client takes an `attached` as *start again* - see [`Message::Projected`] - so a
    /// client that only wants today's figures would be throwing away the conversation it already
    /// had in order to refresh a token count.
    ///
    /// note: what it is for is the half of a session that is not the conversation - the items, the
    /// budget, what the policy will answer - which a client renders as a second view and which the
    /// records deliberately cannot supply. `context.added` names an item and says nothing about
    /// what the *next request* will do with it, and `going`, `left_out` and `marker` are answers
    /// to that question rather than to the question of what happened. They are worked out by
    /// projecting, so they are only ever true as of a moment, and this is how a client asks for
    /// that moment to be now.
    Project,
    /// Ask what one context item actually says.
    ///
    /// note: this is why the protocol is not "events, and render them however you like". The
    /// session log deliberately names things rather than copying them - `context.added`
    /// carries an identifier, a kind, a label and a token count, and no content - which is what
    /// keeps a log affordable enough to keep for ever. A client fed nothing but events can
    /// therefore render a turn as it streams and cannot render one word of anything that happened
    /// before it connected. It gets the conversation from [`Attached`] and the *whole* of any one
    /// item from here, on demand, rather than from a stream fattened until it carried both.
    Inspect {
        /// Which item.
        id: ContextId,
        /// Ask for the item's own text rather than the reading of it.
        ///
        /// note: two questions about one item, and they are not the same answer. The reading is
        /// what the context tab shows - the content, and above it why the item is here, what the
        /// turn was thinking, and the calls it carries - which is what somebody wants in front of
        /// them. What [`Command::Revise`] takes is the content and nothing else, because that is
        /// what `Kernel::replace` writes: a client that committed the reading back would put `it
        /// is here because: …` inside the item it was describing.
        ///
        /// note: a flag rather than a second field on [`Message::Item`] carrying both. An inspect
        /// is the one message that exists to be large - it is how the whole of a four-hundred-line
        /// tool result is fetched - and a client wants one of these per row, never both.
        #[serde(default)]
        raw: bool,
        /// Which version to answer with: `None` for what the item says now, or `1` for the oldest
        /// one still kept, counting up to [`Listed::versions`].
        ///
        /// note: the numbering the terminal's own strip of faces uses, so `v1` means the same
        /// thing on a screen and on a phone. What the item says *now* has no number here because
        /// it has no fixed one: it is one past the last, and that moves every time somebody edits.
        #[serde(default)]
        version: Option<usize>,
    },
    /// Something this build has no name for.
    ///
    /// note: a newer client's verb, and it is answered with a [`Message::Failed`] rather than by
    /// closing the connection - which keeps the invariant [`Message::Done`] states, since a client
    /// that sent something is owed exactly one answer whether or not this end knows what it was.
    /// Without it an unknown `do` would be a parse error, and a parse error takes the connection
    /// with it.
    #[serde(other)]
    Unknown,
}

/// What a session says to a client.
///
/// note: the split that matters is [`Message::Record`] against everything else, and it is the
/// same split the runtime already draws. A record is numbered, is in the session log, and cannot
/// be lost: a client that missed one asks for it by sequence and gets it. Everything else -
/// fragments of a model still writing, what the program said for itself, the answer to a line -
/// is in no log, is best-effort, and is gone once it has gone past. `Config::record_progress` is
/// the runtime's own name for that line, and this protocol does not draw a second one somewhere
/// else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "is", rename_all = "snake_case")]
pub enum Message {
    /// Where the session stands, for a client that has just arrived.
    ///
    /// note: boxed because it is much the largest of these and every other variant would
    /// otherwise be sized against it.
    Attached(Box<Attached>),
    /// The projection again, for a client that asked, and nothing else changes with it.
    ///
    /// note: the same payload as [`Message::Attached`] under a different name, and the name is what
    /// matters. A client takes an `attached` as *start again* - it has just been handed the
    /// conversation and every record after it, so whatever it had drawn belongs to a stream it is
    /// no longer on. This one changes nothing: the stream is where it was, the records already
    /// sent are still the records, and only the figures are newer. A client that could not tell
    /// the two apart would wipe its own screen to refresh a token count.
    ///
    /// note: it carries no `id:` where a gateway gives one, for the same reason [`Message::Item`]
    /// does not: an id is what a browser resumes from, and this is an answer to a command rather
    /// than a place in the stream.
    Projected(Box<Attached>),
    /// One entry of the session log, verbatim.
    Record(Record),
    /// One entry of the session log that is longer than the other end will read, named instead of
    /// sent.
    ///
    /// note: the record still exists and is still in the log - this is a gap in what was *sent*,
    /// not in what happened, and `context.replaced` is the only event that can grow one. Without
    /// it a session that wrote such a record would be unattachable for the rest of its life: a
    /// client resumes by sequence, so it would come back to the same record on every attempt.
    ///
    /// note: numbered, and the number is what makes this a message rather than an apology. A
    /// client takes the `seq` as seen and resumes after it, which is how it gets past. What it has
    /// is a hole it knows the size and position of, and [`Command::Inspect`] is where the content
    /// is when somebody wants it - which is already how content is fetched on demand rather than
    /// streamed.
    Oversized {
        /// Which record.
        seq: u64,
        /// How many bytes it would have been on the wire.
        bytes: usize,
    },
    /// A fragment of something still arriving: a model writing, or a tool talking.
    ///
    /// note: unnumbered on purpose - these are not in the log by default, so there is no sequence
    /// to give them and nothing to recover them from. `after` is the last record the client was
    /// sent, which is what tells it where the fragment hangs.
    Progress {
        /// The record this arrived after.
        after: u64,
        /// The fragment, as `model.delta` or `tool.output`.
        event: Event,
    },
    /// The program's own lines are gone: `/cleanup`, or `ctrl+l` at a terminal watching the same
    /// session.
    ///
    /// note: what a client does with it is drop the [`Message::Said`] lines it has drawn and keep
    /// everything else, because that is what was cleared - the conversation is the context and is
    /// not this program's to take away.
    ///
    /// note: unnumbered and best-effort, like the lines it is about. They were never in the log,
    /// so there is nothing to recover and nothing that could recover it; a client that missed this
    /// is carrying lines the session no longer has, and gets them taken away by the next
    /// [`Message::Attached`], whose conversation does not have them either. That is the same
    /// recovery a missed `Said` has, which is the argument for giving the two the same standing.
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
    /// asked for is guessing, and the guess it gets wrong is always the same one: whether its own
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
    /// transitions, so
    /// `state.changed` reaches `Idle` *between two requests of one turn* and `Ready` in the instant
    /// between a decision and the calls it decided running - which means every state a turn rests
    /// in is also a state it passes through. Only the loop driving the kernel knows where a turn
    /// begins and ends. A client that reads this as "the session is doing something on my behalf"
    /// is reading it correctly, and [`Attached::busy`] is the same fact for one that has just
    /// arrived.
    Busy {
        /// Whether a turn is running.
        busy: bool,
    },
    /// The running commands waiting to hear whether they may reach the network, sent whenever
    /// that changes: the whole list, oldest first, and empty once none are.
    ///
    /// note: the whole list rather than one arrival at a time, because nothing numbers these on the
    /// wire. The question is not the kernel's, so it is in no record - see
    /// [`crate::tools::Reaching`] - and a client that missed an arrival, or the answer somebody
    /// else gave, would otherwise go on offering a question the session no longer has.
    /// [`Attached::reaching`] is the same list for a client that has just arrived.
    ///
    /// note: an older client reads this as [`Message::Unknown`] and cannot answer it, which leaves
    /// the command waiting for somebody who can. That is the rule `Unknown` is for, and the
    /// question is still on every screen that knows it.
    Reaching {
        /// The questions, oldest first.
        waiting: Vec<Reached>,
    },
    /// Which model the requests are going to, sent whenever that changes.
    ///
    /// note: on the wire as well as in the records, which since `Kernel::provider_changed` say
    /// when the model changed. `/model` and `/provider` finish inside the
    /// [`Dialect`](nachalnik_providers::Dialect) the kernel already holds, so the record comes when
    /// the switch is done - and a client that follows the model on screen is told the moment the
    /// session's answer changes, the way [`Message::Busy`] tells it the other thing a screen shows.
    ///
    /// note: broadcast rather than answered to whoever typed it, like every other notice. A
    /// session two people are watching is one session, and the model is not one of them's.
    Model {
        /// The model the requests are going to, or nothing where there is no provider.
        model: Option<ModelInfo>,
    },
    /// What one submitted line did.
    ///
    /// note: what it does *not* carry is the lines the program said about it. Those go to every
    /// attached client as [`Message::Said`], because a session with two people watching has one
    /// voice, and a reply that carried them too would print them twice for whoever asked.
    Replied {
        /// What the line did.
        did: Did,
        /// The page it opened, if it opened one.
        page: Option<Printed>,
        /// Whether a turn is running, as of handing the line in.
        ///
        /// note: carried rather than inferred from `did`, which is nearly the same thing and wrong
        /// in the case that matters: a line that was `Asked` starts a turn *unless* the session has
        /// spent its ceiling, and a client reading `Asked` as "a turn is running" would then wait
        /// for the end of one that never started.
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
        /// note: carried back rather than remembered by the client, because both answers are
        /// found by the same identifier and a client holding two places to put one - a line on
        /// the chat and a box on the context tab - would otherwise fill the wrong one with the
        /// wrong reading. It is the question echoed, which is the cheapest way to tell them
        /// apart and the only one that survives two inspects crossing.
        #[serde(default)]
        raw: bool,
        /// Which version this is, echoed from [`Command::Inspect`]; `None` is what it says now.
        #[serde(default)]
        version: Option<usize>,
    },
    /// How many fragments went past while this client was not keeping up.
    ///
    /// note: said rather than swallowed, and said with a number. The records are unaffected - they
    /// are read out of the log, which drops nothing - so this is exactly and only the loss of live
    /// observation, which is the one thing a slow client is entitled to lose.
    Missed {
        /// How many.
        frames: u64,
    },
    /// A command could not be done.
    Failed {
        /// Which command it was about.
        about: String,
        /// What went wrong.
        error: String,
    },
    /// Something this build has no name for.
    ///
    /// note: the rule that goes with it is **ignore what you do not know**, and it is what makes a
    /// new variant something other than a break. A client written against version 1 and attached
    /// to a session that has grown a message since reads this, prints nothing, and carries on with
    /// the records. Without it the new message is a parse error, a closed connection, and a minute
    /// of trying to get back to a session that was working perfectly.
    ///
    /// note: what it cannot do is carry the payload, so a relay - `examples/gateway.rs` - reads
    /// the wire as JSON and passes it on rather than parsing each message and writing it out
    /// again. Turning an unknown message into this one and back would be the relay deciding what a
    /// page is allowed to hear.
    #[serde(other)]
    Unknown,
}

/// Where a session stands, for a client that has just arrived.
///
/// note: a projection rather than a [`nachalnik::Snapshot`], which is the obvious thing to send
/// and the wrong one: a snapshot carries every item's whole content, blobs included, so attaching
/// a phone to a session that has read four files would push megabytes at it before it had asked
/// for anything. What a client needs in order to start rendering is the conversation, what each
/// item *is*, and what the next request does with it; the whole of any one item is one
/// [`Command::Inspect`] away.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Attached {
    /// Which version of this wire the session speaks; see [`VERSION`].
    ///
    /// note: the other half of [`Command::Attach`]'s `version`, and it is here now because it
    /// cannot be added later on the same terms. A client learns what the session speaks from the
    /// first thing it is sent, which is what tells it whether a command this build knows is worth
    /// sending at all - and a field added once two ends are deployed is one a client can only read
    /// as `Option`, which is the shape that means "and it might be anything".
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
    /// Whether a turn is running.
    pub busy: bool,
    /// Whether the loop is being driven a transition at a time.
    pub stepping: bool,
    /// The model the requests are going to, if one is set.
    pub model: Option<ModelInfo>,
    /// What the next request comes to, and how much room there is.
    pub budget: Budget,
    /// What the provider has charged for this session so far.
    pub spent: u64,
    /// What it may charge before the session stops, if anything says.
    pub spend: Option<u64>,
    /// Whether that ceiling has been reached.
    pub overspent: bool,
    /// The conversation as it reads now.
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
    /// own drawing code, and a build with `shell-advisor` in it serves a browser exactly what a
    /// build without it serves: the advisor runs and rates the question, and the one thing a
    /// person was supposed to see never leaves the process.
    pub rated: Vec<Judged>,
    /// Why the advisor has no rating for the questions it was asked about and could not answer.
    ///
    /// note: beside [`Attached::rated`] and in the same shape, one row per question, so that a
    /// client draws one line or the other where the band would go. Without it an unrated
    /// question looks like one the advisor had nothing to say about - and the requests the
    /// advisor's endpoint refuses are the ones about the commands most worth a colour.
    ///
    /// note: empty from a session that predates the field, which a client reads as the absence
    /// it drew before.
    #[serde(default)]
    pub unrated: Vec<Unjudged>,
    /// What the policy in force is called.
    pub policy: String,
    /// What it answers about anything nobody has told it about.
    ///
    /// note: the line the permissions tab puts *above* the rules, and it goes with them for the
    /// reason it is above them there: a list of decisions is not a policy, and a client that drew
    /// the rows alone would be answering "what will this session allow" with the part of the answer
    /// somebody happened to have given already.
    pub untold: nachalnik::Verdict,
    /// What the policy will answer about each capability and path rule somebody has decided.
    pub permissions: Vec<Stanced>,
    /// The trace, as the trace tab draws it: what happened, in this program's words.
    ///
    /// note: capped where `App` caps it and not otherwise trimmed. The session log is unbounded
    /// and is what `Command::Attach` streams; this is the pane, which is a ring of the last few
    /// hundred lines because that is what a person reads.
    pub trace: Vec<Tracing>,
    /// How many more it holds an opinion about and will simply ask.
    ///
    /// note: a count rather than rows, which is the same answer the permissions tab gives and for
    /// its reason: a row for a `.aws` rule nobody has thought about is not information. What the
    /// count is for is the other half of that - a client listing two decisions while standing for
    /// eighteen answers is a different kind of dishonest, and a client cannot work this out from
    /// the list above, because the list is exactly what it leaves out.
    pub undecided: usize,
    /// A message somebody typed into the running turn, waiting for it to end.
    pub queued: Option<String>,
    /// What the shell tool's sandbox came to, or nothing where there is none.
    pub confinement: Option<String>,
}

/// What the advisor made of the command one waiting question is about.
///
/// note: beside [`Attached::asking`] rather than on a question, because a question is a
/// [`PermissionRequest`] and that is the runtime's type. The runtime has no advisor and is not
/// going to grow a field for one - see the module note on carrying runtime types verbatim - so the
/// rating travels as its own row, named by the question it is about.
///
/// note: only for the questions something rated, which is `shell` commands in a build that has
/// `shell-advisor` in it and a session started with `--advise`. A client draws the band where
/// there is a row for the question it is drawing and draws nothing where there is not, which is
/// the same thing the panel at a terminal does with the same absence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Judged {
    /// Which question.
    pub id: PermissionId,
    /// The band it is drawn as.
    pub band: Band,
    /// What that band is called, in the advisor's own words.
    ///
    /// note: carried rather than left for the client to word from `band`, so that the sentence a
    /// browser prints and the sentence a terminal prints are one string with one author. A client
    /// that wrote its own would be a second account of a rubric it cannot see.
    pub said: String,
    /// How sure the advisor was, from 0 to 1.
    ///
    /// note: sent beside the band and not folded into it, for the reason the terminal prints it:
    /// a band that went yellow because nothing could be told apart is not the same warning as one
    /// that went yellow because the command changes something, and a client that could not tell
    /// them apart has been told the advisor was sure when it was not.
    pub confidence: f64,
    /// Which stage of the command earned the band, as a byte range into the `cmd` argument.
    ///
    /// note: on the wire because it cannot be worked out at the other end. A client would need
    /// two things to point at this itself - where the command comes apart, which is
    /// [`joints`](crate::tools::joints) and is Rust in this crate, and which stage the advisor
    /// liked least, which nothing but the advisor knows - so left off, the one fact that says
    /// *where* in a long chain is unreachable from anywhere but the process that produced it.
    ///
    /// note: a range and not the text, so a client points at the stage in the command it is
    /// already drawing rather than printing a copy of it underneath. A second copy of a long
    /// stage is a row at a terminal and most of the screen on a phone.
    ///
    /// note: absent for a command that was rated in one piece, and absent from a session that
    /// predates the field, which is one case as far as a client is concerned - nothing to point
    /// at, and a band to draw exactly as before.
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
/// note: what the advisor's rating is *shown* as rather than what it scored, which is a
/// distinction [`crate::tools::Rated::shown`] owns and nothing on this side of the wire repeats.
/// An uncertain reading is never sent as [`Band::Reads`], because green is the one colour that
/// would say a command is safe.
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
}

impl Line {
    /// Copies one out of what [`App::conversation`] borrowed.
    pub(super) fn of(said: &Said<'_>) -> Self {
        Self {
            speaker: said.speaker,
            text: said.text.to_string(),
            item: said.item.map(|item| item.id),
        }
    }
}

/// One line of the trace, as the trace tab draws it.
///
/// note: the trace rather than the records, and the difference is the point of the tab. A record
/// says `context.compacted` and carries a `CompactionReport`; the trace line says what that pass
/// took and what it left, in this program's words, because somebody reading a log wants the
/// sentence and not the structure. A client that rendered the records itself would be writing a
/// second vocabulary for the same events, and it would be the one nobody at the other end can see.
///
/// note: the gap rather than two timestamps, and it is `app::text::waited_since` - the very
/// formatter the pane uses, so the column reads the same in a browser as in a terminal. It is
/// `None` under a tenth of a second, and `None` after a line that ended a wait for a *person*:
/// however long somebody took to answer a question, it is not a step this program spent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Tracing {
    /// The dotted name, e.g. `model.requested`; empty for a continuation of the line above.
    pub name: String,
    /// The rest of it.
    pub detail: String,
    /// How long after the line above this one arrived, where that is worth saying.
    pub gap: Option<String>,
    /// When it arrived, in milliseconds since the Unix epoch.
    ///
    /// note: the wall clock rather than the monotonic one, because this is the half a reader
    /// matches against a server log or their own memory of the afternoon - and because an
    /// `Instant` has no rendering as a time of day. The gap above is the other half, and it is
    /// worked out from the monotonic clock, which a system clock being set cannot drag backwards.
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
    /// note: `0` for everything nobody has rewritten, which is nearly every row - so a client
    /// draws the way back through an item's history only where there is one. What it says now is
    /// not counted, because it has no fixed number: it is one past the last, and editing moves
    /// it. The cap is the viewer's, and the oldest goes first when it is reached.
    pub versions: usize,
    /// Why this one cannot be rewritten, where it cannot be.
    ///
    /// note: on the row rather than found out by trying, so that a client can say so before
    /// somebody has typed rather than after. [`Command::Revise`] asks the same question again and
    /// is the one that decides - a row is a moment old by the time anybody acts on it - but a
    /// client that only learns from the refusal is one that takes an edit it was never going to
    /// keep, which is the worse half of the same answer.
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
            // `Going::sends_content` rather than the state, because a projector repairs a request
            // to keep it valid and an item in a state that sends content can still be out of one
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
/// note: the subject is its own spelling rather than a structure, and that is deliberate: for the
/// three a `Subject` can be read back from, it reads back out of the same string it prints as,
/// which is what makes `--deny "$(a row off this list)"` mean what it says. Giving the wire a
/// second shape for it would be a second thing to keep in step with `Subject::parse`. A server's
/// row is the exception, and it is `Subject`'s rather than this one's: it prints as `server files`
/// and is given back with `--allow-server`, because a server's name and a domain are both bare
/// words and nothing in either says which.
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
/// a session with three clients attached has three answers to both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Printed {
    /// What it is.
    pub title: String,
    /// Its faces, in the order they are offered; almost everything has exactly one.
    pub pages: Vec<Page>,
}

/// A connection, read one frame at a time and no further than [`MAX_LINE`] into any of them.
///
/// note: not `tokio::io::Lines`, because of the cap. `next_line` grows its own buffer until a
/// newline arrives, so a limit over it can only ever be checked against a frame that has already
/// been read - which is no defence against the one case it exists for, a peer that never sends a
/// newline at all. This holds the part-read frame itself and stops as soon as
/// there is too much of it.
///
/// note: cancel-safe, which is what both loops that read commands need from it: the part-read
/// frame lives here rather than in the future, and `fill_buf` guarantees that a read dropped
/// before it resolved consumed nothing. So a `select!` that drops this mid-frame has lost nothing,
/// which is the property `Lines` had and the reason this can stand in for it.
pub struct Frames<R> {
    read: R,
    held: Vec<u8>,
}

impl<R: AsyncBufRead + Unpin> Frames<R> {
    /// One, over whatever this end of the connection reads as.
    pub fn new(read: R) -> Self {
        Self {
            read,
            held: Vec::new(),
        }
    }

    /// The next frame, or `None` where the peer has gone between two of them.
    async fn next(&mut self) -> Result<Option<String>, String> {
        loop {
            let available = self
                .read
                .fill_buf()
                .await
                .map_err(|e| format!("the connection stopped talking: {e}"))?;
            if available.is_empty() {
                return match self.held.is_empty() {
                    true => Ok(None),
                    // a peer that went away mid-frame, which is not the same thing as one that
                    // finished: half a message parses as nothing and says so
                    false => Err("the connection stopped in the middle of a message".to_owned()),
                };
            }
            let (whole, used) = match available.iter().position(|byte| *byte == b'\n') {
                Some(at) => {
                    self.held.extend_from_slice(&available[..at]);
                    (true, at + 1)
                }
                None => {
                    self.held.extend_from_slice(available);
                    (false, available.len())
                }
            };
            self.read.consume(used);
            // `at least`, because what is held is whatever had been buffered when the limit was
            // passed rather than the whole of what the peer meant to send - and the whole of it is
            // the number nobody here is ever going to know
            if self.held.len() > MAX_LINE {
                return Err(format!(
                    "a message of at least {} bytes, over the {MAX_LINE}-byte limit",
                    self.held.len()
                ));
            }
            if whole {
                // the `\r` of a `\r\n`, dropped where `Lines` drops it. Nothing here would notice
                // it - JSON reads it as whitespace either way - but this stands in for that reader,
                // and a stand-in that hands back a different string is a difference somebody finds
                // rather than one they read
                if self.held.last() == Some(&b'\r') {
                    self.held.pop();
                }

                return String::from_utf8(std::mem::take(&mut self.held))
                    .map(Some)
                    .map_err(|_| "a message that is not text".to_owned());
            }
        }
    }
}

/// Reads one message off a connection, or `None` where the peer has gone.
///
/// note: newline-delimited JSON, and the argument for it over a length prefix is that
/// `serde_json` escapes every control character it writes - a compact value never contains a
/// literal newline, so there is nothing for a delimiter to be confused by. What a length prefix
/// would buy is a payload that is not JSON, which this one is; what it costs is that the stream
/// stops being readable with the tools everybody already has. `nc | jq` is worth more than a
/// frame header here.
pub async fn read<T: for<'a> Deserialize<'a>>(
    frames: &mut Frames<impl AsyncBufRead + Unpin>,
) -> Result<Option<T>, String> {
    let Some(line) = frames.next().await? else {
        return Ok(None);
    };
    if line.trim().is_empty() {
        return Err("an empty message".to_owned());
    }

    serde_json::from_str(&line)
        .map(Some)
        .map_err(|e| format!("a message that is not one: {e}"))
}

/// Writes one message to a connection.
pub async fn write<T: Serialize>(
    out: &mut (impl AsyncWrite + Unpin),
    message: &T,
) -> Result<(), String> {
    write_frame(out, &framed(message)?).await
}

/// One message as the bytes it goes out as, newline and all.
///
/// note: split out of [`write()`] so that a caller can ask how long a message is before committing
/// to sending it, which is what `flush` does with a record; see [`overlong`]. Serializing twice to
/// answer that would be doing the expensive half twice on every record of every connection, and
/// the records this is about are the large ones.
pub fn framed<T: Serialize>(message: &T) -> Result<Vec<u8>, String> {
    let mut line = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    line.push(b'\n');

    Ok(line)
}

/// Sends one already-framed message.
pub async fn write_frame(out: &mut (impl AsyncWrite + Unpin), line: &[u8]) -> Result<(), String> {
    out.write_all(line)
        .await
        .map_err(|e| format!("could not write to the connection: {e}"))?;
    out.flush()
        .await
        .map_err(|e| format!("could not write to the connection: {e}"))
}

/// Whether an event is a fragment of something still arriving, rather than a thing that happened.
///
/// note: the same two the kernel keeps out of its log unless asked, and the list is deliberately
/// read off that decision rather than made here. If the runtime ever records a third kind of
/// progress, a copy of this list is where the two would disagree.
pub fn is_progress(event: &Event) -> bool {
    matches!(event, Event::ModelDelta { .. } | Event::ToolOutput { .. })
}

/// Reads a `scheme:rest` address, the one spelling both ends of this take.
///
/// note: a scheme rather than a guess. `/run/kamchatka.sock` and `localhost:7878` are both
/// perfectly good strings and nothing in either says which kind of thing it is, so working it out
/// from a `/` or a `:` is the same mistake as deciding a file's media type by looking at its
/// bytes - it is right until it is quietly wrong, and this crate does not do it anywhere else.
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
pub enum Address<'a> {
    /// A socket file, whose permissions are the whole of its authentication.
    Unix(&'a str),
    /// A host and a port.
    Tcp(&'a str),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one thing a client must not be able to disagree with the terminal about.
    ///
    /// note: an unsure reading is never sent as [`Band::Reads`], because green is the colour that
    /// says a command is safe and a spread distribution over a safety rubric is not evidence that
    /// it is. That rule lives in [`crate::tools::Rated::shown`] and this asks it rather than
    /// restating where the threshold is - what is under test is that the wire goes through
    /// `shown` at all, which is what stops a browser drawing green where a terminal drew yellow.
    ///
    /// note: the words travel with the band for the same reason. A client wording the band itself
    /// would be a second account of a rubric it cannot see, and the first rewording of the rubric
    /// is where the two would part.
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
    /// note: the second half is the one a client depends on. `worst` is newer than the message it
    /// is on, so a client built against this version reads a record written before the field
    /// existed, and it has to arrive as *nothing to point at* rather than as a message that will
    /// not parse - which for a projection is a client with no session at all.
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

    /// The one byte the reader and the writer have to agree about.
    ///
    /// note: [`Frames::next`] checks what it is *holding*, which is the frame without the newline
    /// that ended it, so a message exactly on the limit is one the other end will read and one
    /// more than that is not. Measured against the framed bytes here, because that is what the
    /// caller has in its hand - and getting this off by one would either refuse a record every
    /// client could have taken or send one none of them can.
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
}
