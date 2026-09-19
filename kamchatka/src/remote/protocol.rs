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
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};

#[cfg(doc)]
use crate::app::App;
use crate::app::{Did, Going, Page, Said, Speaker, Stance};

/// The longest line either end will read before giving up on the connection.
///
/// note: generous, and it has to be. A frame here is not something somebody typed - it is whatever
/// the session put in its log, and `context.replaced` is the one event that carries content, so a
/// rewritten tool result goes down the wire at whatever size it was. What this is defending
/// against is a peer that never sends a newline, which would otherwise be read into memory for
/// ever; a line over it is a protocol error that closes the connection and says so, rather than a
/// truncation that would leave the reader parsing the second half of somebody's JSON.
pub const MAX_LINE: usize = 32 * 1024 * 1024;

/// What a client asks a session to do.
///
/// note: five, and the set is meant to stay about this size. Four of them are things a person at
/// the terminal does - hand in a line, stop the turn, answer a question, read an item - and the
/// fifth is the connection itself. Anything else a client wants is a slash command, which is
/// [`Command::Submit`]: every verb this program has goes through [`App::submit`], so a protocol
/// with a message per verb would be a second vocabulary to keep in step with the first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Command {
    /// Begin, or resume, watching the session.
    ///
    /// note: one message rather than an `attach` and a `resume`, because resuming *is* attaching
    /// with a watermark. `since: None` says "I have nothing", and is answered with a
    /// [`Attached`] and then every record; `since: Some(n)` says "I have everything through n",
    /// and is answered with the records after it and no snapshot. A client that lost its process
    /// sends the first; one that only lost its socket sends the second.
    Attach {
        /// The last record the client is sure it has.
        since: Option<u64>,
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
        remember: bool,
    },
    /// Ask for the projection again, as it stands now.
    ///
    /// note: [`Command::Attach`] already answers with one and is deliberately not the way to do
    /// this: attaching with no watermark is answered with the projection *and then every record
    /// there has ever been*, because that is what a client with nothing has to be given. A client
    /// that only wants today's figures would be asking for the whole session to be replayed at it,
    /// and would have to throw away the conversation it already had in order to take it.
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
    /// note: this is the whole reason the protocol is not "events, and render them however you
    /// like". The session log deliberately names things rather than copying them - `context.added`
    /// carries an identifier, a kind, a label and a token count, and no content - which is what
    /// keeps a log affordable enough to keep for ever. A client fed nothing but events can
    /// therefore render a turn as it streams and cannot render one word of anything that happened
    /// before it connected. It gets the conversation from [`Attached`] and the *whole* of any one
    /// item from here, on demand, rather than from a stream fattened until it carried both.
    Inspect {
        /// Which item.
        id: ContextId,
    },
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
    /// note: the same payload as [`Message::Attached`] under a different name, and the name is the
    /// whole point. A client takes an `attached` as *start again* - it has just been handed the
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
    /// The program's own lines are gone: `/clear`, or `ctrl+l` at a terminal watching the same
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
    /// [`Message::Attached`], [`Message::Replied`] or [`Message::Item`] that carries one, or a
    /// [`Message::Failed`] - and that is a property of the protocol rather than a convenience. A
    /// client that cannot tell when the session has caught up with what it asked for is guessing,
    /// and the guess it gets wrong is always the same one: whether its own last line has happened
    /// yet. See [`Message::Busy`] for the other half.
    Done {
        /// Which command it was about.
        about: String,
        /// Whether a turn is running, as of applying it.
        busy: bool,
    },
    /// Whether a turn is running, sent whenever that changes.
    ///
    /// note: on the wire because it cannot be worked out from the records, and the attempt is
    /// recorded here so that nobody tries it twice. A turn is a loop over transitions, so
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
        /// What it says, laid out the way the context tab lays it out.
        body: String,
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
pub struct Attached {
    /// The session's name, which is also what its record is filed under.
    pub session: String,
    /// The last record this reflects. Everything after it arrives as a [`Message::Record`].
    ///
    /// note: taken in the same breath as the rest of this, while nothing else can be driving the
    /// session, so there is no window in which a change is in neither the projection nor the
    /// stream that follows it.
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
    /// What the policy will answer about each capability and path rule somebody has decided.
    pub permissions: Vec<Stanced>,
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

/// One line of the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// One context item, as a row: what it is, what it costs, and what the next request does with it.
///
/// note: no content. That is the whole point of it being this type rather than a
/// [`nachalnik::ContextItem`], which is `Serialize` and would have done otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
}

impl Listed {
    /// Reads one row off an item and the projection it is about to take part in.
    pub(super) fn of(item: &ContextItem, going: &Going) -> Self {
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
pub struct Printed {
    /// What it is.
    pub title: String,
    /// Its faces, in the order they are offered; almost everything has exactly one.
    pub pages: Vec<Page>,
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
    lines: &mut tokio::io::Lines<impl AsyncBufRead + Unpin>,
) -> Result<Option<T>, String> {
    // note: the cap is checked against what arrived rather than enforced while arriving, which is
    // the honest description of what `next_line` does: it has already read the line. What this
    // catches is a peer sending something absurd, and it catches it before the parse rather than
    // after - the difference being an error that says which rule was broken
    let line = match lines.next_line().await {
        Ok(Some(line)) => line,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("the connection stopped talking: {e}")),
    };
    if line.len() > MAX_LINE {
        return Err(format!(
            "a message of {} bytes, over the {MAX_LINE}-byte limit",
            line.len()
        ));
    }
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
    let mut line = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    line.push(b'\n');
    out.write_all(&line)
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
