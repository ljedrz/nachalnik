//! The other end: a session somebody else is running, driven by lines.
//!
//! note: deliberately the smallest client that exercises the whole protocol rather than the nicest
//! one to use. It proves the seam: attach, read a projection, follow a turn, submit, interrupt,
//! answer a question, lose the socket, and resume the same session at the record it had reached,
//! before anything with a screen is written against the protocol.
//!
//! note: it writes what `--headless` writes, in the same two streams and the same words: records to
//! one, what a person reads to the other. That makes `kamchatka --connect` a drop-in for
//! `kamchatka --headless` in a script, and lets the same test drive both.

use std::{
    collections::{BTreeSet, VecDeque},
    io::Write,
    time::Duration,
};

use nachalnik::{ContextId, Delta, Event, Grant, PermissionRequest};
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};

use crate::{
    app::{
        Speaker,
        text::{one_line, plural},
    },
    remote::protocol::{self, Address, Attached, Command, Message, Reached},
};

/// How long to retry a dropped connection before giving up.
///
/// note: a while rather than for ever, because the two reasons a socket dies look identical here -
/// the host was restarted, or the host is gone - and a client that retried indefinitely would be a
/// process somebody has to notice and kill. Each attempt reattaches with the last record this
/// client saw, so resuming costs the records it missed and nothing else.
///
/// note: one minute, sized for networks rather than patience. Over a socket file the only way to
/// lose a connection is the host going away, and it either comes back at once or not at all. Over a
/// port the usual reason is a laptop changing access points, and the usual time to get it back is
/// several seconds - so a limit sized for a socket file reports a session lost while it is still
/// there.
const GIVE_UP: Duration = Duration::from_secs(60);

/// How long to wait before picking it back up the first time.
const FIRST_WAIT: Duration = Duration::from_millis(250);

/// The longest the wait grows to, doubling.
///
/// note: exponential backoff rather than a fixed pause, for both cases. A host restarting is back
/// in well under a second and should be picked up in the first attempt or two; a network that has
/// gone is not coming back inside a minute however often anybody asks, and sixty seconds of
/// quarter-second attempts is two hundred and forty unwanted connections.
const LONGEST_WAIT: Duration = Duration::from_secs(5);

/// A client of somebody else's session.
pub struct Client<'a> {
    /// The record stream, one JSON object per line: the same bytes `/save` writes.
    records: &'a mut dyn Write,
    /// What a person reads: the model's own words, and what the session had to say about the run.
    prose: crate::headless::Prose<&'a mut dyn Write>,
    /// Whether any of the answer being written has been printed as it arrived.
    streamed: bool,
    /// The recorded turns whose words were not streamed, fetched with an `inspect`.
    ///
    /// note: the same gap `--headless` fills from the item, filled the only way a client can: the
    /// records name a turn and do not copy it, so a provider that answers in one piece puts nothing
    /// on this wire but a `model.finished`.
    unstreamed: VecDeque<ContextId>,
    /// Whether the fetched answer's fragments may still arrive, and are dropped.
    ///
    /// note: a session behind the broadcast can write a turn's fragments after the record that ends
    /// it, so a turn that looked unstreamed at `model.finished` may stream afterwards; printing it
    /// as well would print it twice.
    fetching: bool,
    /// The last record this client is sure it has.
    last: u64,
    /// The last record this client has written to its own stream.
    ///
    /// note: a second watermark because a projection is the *stream's* mark rather than this
    /// client's: a fresh attach is answered with where the log has got to and everything after it,
    /// so `last` is that number and the records before it are in no message this client has seen.
    /// They are the whole of what happened before it arrived - the `session.started` the kernel
    /// emits while it is still being built, the turn a host answered before anybody was here - and
    /// a `--connect` is documented as writing what `--headless` writes. So what has been *written*
    /// is asked for separately, by `Client::catch_up`, and starts at nothing however far the log
    /// has already got.
    written: u64,
    /// The session those records came from, where this client has attached to one.
    ///
    /// note: this says whether there is anything to resume, and what a resume names: `Some` is a
    /// client holding a conversation and a watermark, `None` one with nothing.
    ///
    /// note: a resume refused is the end of this client rather than a fresh attach, because the
    /// refusing session is not this one - a host restarted at the same address. Attaching to it
    /// would carry on the record stream with another session's records under the first one's, with
    /// nothing to say where one ended. `--connect` stands in for `--headless`, which is one
    /// session, and says how to attach to the other instead.
    session: Option<String>,
    /// The questions waiting on somebody, oldest first.
    asking: VecDeque<PermissionRequest>,
    /// The questions this has answered and not yet seen decided, oldest first.
    ///
    /// note: kept until the `permission.decided` record rather than forgotten with the answer,
    /// because an answer can die in a socket the session never read. A resume is sent the records
    /// after the last one this client saw, and the question was asked before that, so a question
    /// dropped here is one the session waits for and this client can no longer answer. What the
    /// resume leaves undecided goes back on `asking` once the attach is answered.
    answering: VecDeque<PermissionRequest>,
    /// The running commands waiting to hear whether they may reach the network, oldest first, as
    /// the session last listed them and less the ones this has answered.
    reaching: VecDeque<Reached>,
    /// Which of those this has printed, so that a list sent again prints only what is new in it.
    told: BTreeSet<u64>,
    /// Which of those this has answered, so that a list sent before the answer landed does not
    /// offer the question again.
    ///
    /// note: kept rather than cleared, because an identifier is never reused and the list the
    /// session sends is a snapshot: the one after an answer may still have been taken before it.
    let_through: BTreeSet<u64>,
    /// Whether this connection opened with a resume that has not been answered yet.
    resuming: bool,
    /// The model this last said the session was talking to, once it has said one.
    ///
    /// note: so that a resume, which is told the model whether or not it changed, prints it only
    /// where it did.
    model: Option<Option<String>>,
    /// What a question is answered with once there is nobody left here to answer it.
    ///
    /// note: the same flag `--headless` reads, reaching the same two states for the same reason: an
    /// input that has closed cannot be asked anything. It is this client's rather than the
    /// session's. `--connect` takes nothing else on the command line because everything else
    /// belongs to whoever is serving, and what a detaching client does with an open question does
    /// not.
    on_ask: Grant,
    /// Whether a question is left for somebody else once the input has closed, rather than
    /// answered with `on_ask`; see [`Client::leaves_questions`].
    leaves: bool,
    /// Whether it has said that it is leaving questions, which it says once.
    said_left: bool,
    /// Whether a turn is running, as of the last thing the session said about that.
    busy: bool,
    /// How many commands this has sent and not yet been answered about.
    ///
    /// note: the input closing waits on this count and `detaching`; see [`Client::resting`]. It is
    /// a count, not a flag, because the first `attach` and a line already sitting in a pipe overlap
    /// every time a question is piped in. It counts *answers owed*, not anything else, because that
    /// is the only thing a client knows without guessing: every command gets exactly one answer,
    /// and until it arrives the session has not caught up with what this asked for. Get either half
    /// wrong and a piped-in question is answered to a client that has already detached.
    outstanding: usize,
    /// Whether the input has closed and this is waiting for the session to come to rest.
    detaching: bool,
    /// Whether the session has said it is finished.
    ///
    /// note: it tells a closing socket from a dropped one. Without it, `/quit` leads to a minute of
    /// reattach attempts to a session that already did what it was asked. `session.finished` is a
    /// record like any other: the session says so, rather than this client inferring it from
    /// silence.
    over: bool,
    /// Whether the last turn this client saw end failed.
    ///
    /// note: what the exit status says, as it does for `--headless`: a script whose question never
    /// reached a model must not read a `0`. The last one rather than any, for headless's reason - a
    /// session that failed once and was carried on from has recovered - so a request that finishes
    /// clears it.
    failed: bool,
    /// Whether the session has said anything on this connection.
    ///
    /// note: it makes [`GIVE_UP`] a minute from the last drop, not a minute across the whole run. A
    /// connection the session answered on is picked back up, and the next time it goes down is a
    /// drop of its own. Without this, short outages over a day add up to the minute, and the next
    /// one gives up without a single attempt, saying the session has not answered.
    reached: bool,
    /// Whether a line is read only once the session is ready for it.
    ///
    /// note: what makes this a drop-in for `--headless` down a pipe, which reads the next line
    /// only once the turn before it is over. Read the moment they arrive, a script's lines all go
    /// at once: a `/note` after a question is refused for a turn still running, a second message
    /// replaces the first one waiting, and a `/quit` stops the turn the script was asking for.
    /// Somebody at a terminal types while a turn runs on purpose, so it is set for a pipe alone.
    paced: bool,
    /// A line read while a question waited that does not answer it, sent once the turn is over.
    ///
    /// note: it matches `--headless`: a question nobody at a pipe answers is answered with
    /// `--on-ask`, and the next line is read once the turn it paused is over. Sent at once, that
    /// line met a turn still running and was refused, and the lines after it went too.
    ///
    /// note: it is a queue because a client that leaves its questions keeps reading while a line is
    /// held: the line waits for somebody else's answer, and only reading on shows whether the input
    /// has closed. What it reads meanwhile waits behind the line, in order.
    held: VecDeque<String>,
    /// Whether a write to this connection has failed.
    ///
    /// note: this keeps a refused write from ending the connection by itself. When a session
    /// finishes, it writes `session.finished` and closes, so a resume sent in the meantime meets a
    /// closed socket. A client that gave up on the write would not read the ending already in front
    /// of it, and would report a session that did what it was told as a failure. The remaining
    /// input decides instead: after the session said it was finished, the end of the input is
    /// `Left::Done`, and anything else is a drop.
    unwritable: bool,
}

/// Why a connection ended.
enum Left {
    /// Somebody asked to leave, or the input closed.
    Done,
    /// The socket went away, and the session may still be there.
    Dropped,
    /// It cannot be picked back up.
    Failed(String),
}

impl<'a> Client<'a> {
    /// One, writing the records to `records` and everything a person reads to `prose`.
    pub fn new(on_ask: Grant, records: &'a mut dyn Write, prose: &'a mut dyn Write) -> Self {
        Self {
            on_ask,
            leaves: false,
            said_left: false,
            records,
            prose: crate::headless::Prose::new(prose),
            streamed: false,
            unstreamed: VecDeque::new(),
            fetching: false,
            last: 0,
            written: 0,
            session: None,
            asking: VecDeque::new(),
            answering: VecDeque::new(),
            reaching: VecDeque::new(),
            told: BTreeSet::new(),
            let_through: BTreeSet::new(),
            resuming: false,
            model: None,
            busy: false,
            outstanding: 0,
            detaching: false,
            over: false,
            failed: false,
            reached: false,
            unwritable: false,
            paced: false,
            held: VecDeque::new(),
        }
    }

    /// Reads a line only once the session has answered the last one and no turn is running, as
    /// `--headless` does, or when a question is waiting for an answer.
    pub fn waits_for_turns(mut self) -> Self {
        self.paced = true;
        self
    }

    /// Answers nothing once the input has closed, and leaves a question waiting for another
    /// client, or for one that comes back - `--on-ask leave`, and what `--connect` does unless
    /// told otherwise.
    ///
    /// note: every attached client may answer, so one watching with its input closed answered
    /// questions another client's person had been asked, `deny` by default - while RUNNING.md says
    /// a question waits for somebody to come back. This client cannot tell whose question it is,
    /// since nothing on the wire says who typed what, so leaving them all is the answer that holds
    /// whoever raised it.
    pub fn leaves_questions(mut self) -> Self {
        self.leaves = true;
        self
    }

    /// Attaches, drives the session from lines, and returns when neither the input nor the session
    /// has anything left.
    ///
    /// note: `last` survives a reconnection, which is what the retry is for. A second attempt
    /// attaches with `since` set to the last record this client actually saw, so the session sends
    /// what it missed and no more. It does not get back the fragments that passed while it was
    /// away; they are in no log, and the session says so rather than pretending. It does not need a
    /// fresh projection, because everything that changed is in the records.
    pub async fn run(
        &mut self,
        address: &str,
        input: impl AsyncBufRead + Unpin,
    ) -> Result<(), String> {
        // note: the reader is built once and handed to each attempt, because `next_line` is
        // cancellation-safe and holds whatever it has read so far. Rebuilding it per attempt would
        // lose half a line whenever a socket died mid-read
        let mut typed = crate::headless::Typed::new(input);
        let (mut first, mut waited, mut wait) = (true, Duration::ZERO, FIRST_WAIT);

        loop {
            let left = self.attend(address, &mut typed, first).await;
            let was_first = std::mem::replace(&mut first, false);
            match left {
                // note: a first connection that closed before the session said a word is not a
                // session that dropped. Whatever is there took the connection and has nothing to
                // say to this client - something that is not a session, or one that will not have
                // it - and attaching again for `GIVE_UP` would end a minute later saying the
                // session had records "when this client last saw it", which it never did. Asked of
                // `reached` rather than of where it closed, because the attach failing to go and
                // the first read finding the end are the same answer from the other side
                Left::Dropped if was_first && !self.reached => {
                    return Err(format!(
                        "`{address}` closed the connection before it said anything; it is not a \
                         session this program serves, or it is one that would not take this client"
                    ));
                }
                // the reason is not repeated, as in `--headless`: it went by as the session said
                // it, and what is left to say is the exit status
                Left::Done if self.failed => return Err("the last turn failed".to_owned()),
                Left::Done => return self.unsent(),
                Left::Failed(e) => return Err(e),
                Left::Dropped if waited >= GIVE_UP => {
                    return Err(format!(
                        "the session has not answered for {}s; it had {} record(s) when this \
                         client last saw it, and `--connect` picks up from there if it comes back",
                        waited.as_secs(),
                        self.last
                    ));
                }
                Left::Dropped => {
                    if self.reached {
                        (waited, wait) = (Duration::ZERO, FIRST_WAIT);
                    }
                    self.prose.fresh_line()?;
                    self.tell(&format!(
                        "the connection went; attaching again from record {} in {:.1}s",
                        self.last,
                        wait.as_secs_f64()
                    ))?;
                    tokio::time::sleep(wait).await;
                    waited += wait;
                    wait = (wait * 2).min(LONGEST_WAIT);
                }
            }
        }
    }

    /// One connection, from attaching to whatever ends it.
    async fn attend(
        &mut self,
        address: &str,
        typed: &mut crate::headless::Typed<impl AsyncBufRead + Unpin>,
        first: bool,
    ) -> Left {
        // note: set `reached` to false before connecting, not after. A failed connect is not an
        // answer. Keeping the last connection's `true` would reset the wait on every failure, so a
        // dead session would retry every quarter of a second for as long as the process lived and
        // never reach `GIVE_UP`
        self.reached = false;
        self.unwritable = false;
        let stream = match connect(address).await {
            Ok(stream) => stream,
            // a first attempt that cannot connect is a wrong address or a session that is not
            // there; calling it a reconnect would claim a connection that never happened
            Err(e) => {
                return match first {
                    true => Left::Failed(e),
                    false => Left::Dropped,
                };
            }
        };
        let (read, mut write) = tokio::io::split(stream);
        let mut frames = protocol::Frames::new(BufReader::new(read));
        // note: whatever was in flight when the last socket died is owed an answer that never
        // comes, and nothing else can lower the count. It is reset per connection rather than
        // decremented on the way out, because a client cannot tell which of what it sent the
        // session had already read
        self.outstanding = 0;
        // whether `ctrl+c` has already asked the turn to stop on this connection
        let mut stopping = false;
        // subscribe once: a second press arriving while the first is still being handled means
        // leave; see `crate::stopping`
        let mut presses = match crate::stopping::Stopping::new() {
            Ok(presses) => presses,
            Err(e) => return Left::Failed(format!("could not listen for ctrl+c: {e}")),
        };

        // note: `since` is sent only where this client has a session to name it against. After a
        // first attach the conversation is already on screen, and asking for the projection again
        // would print the whole of it a second time above the records that carry on from it
        self.resuming = self.session.is_some();
        let attach = match &self.session {
            Some(session) => Command::Attach {
                since: Some(self.last),
                session: Some(session.clone()),
                version: Some(protocol::VERSION),
            },
            None => Command::Attach {
                since: None,
                session: None,
                version: Some(protocol::VERSION),
            },
        };
        if self.say_to(&mut write, attach).await.is_err() {
            return Left::Dropped;
        }

        loop {
            // `stopping` above is this connection's two-stage `ctrl+c`; reusing that name here
            // would read as the same flag being reassigned, so this binding is named for what it
            // holds
            let ended = tokio::select! {
                // note: biased, and the order is a decision rather than a tuning knob. What the
                // session has already said is read before anything else is decided - so a client
                // whose input closes while a socket full of answers is still unread reads them
                // first, instead of `select!` tossing a coin and leaving with the last two lines of
                // a turn sitting in a buffer
                biased;

                message = protocol::read::<Message>(&mut frames) => match message {
                    // note: settled before the rest is read, because a question arriving after the
                    // input closed is why this exists. The turn keeps producing questions, and each
                    // must be answered or the session stops
                    Ok(Some(message)) => match self.heard(message) {
                        Ok(()) => match self.follow(&mut write).await {
                            Ok(()) if self.detaching && self.resting() => Some(Left::Done),
                            Ok(()) => None,
                            Err(_) if self.unwritable => None,
                            Err(e) => Some(Left::Failed(e)),
                        },
                        Err(e) => Some(Left::Failed(e)),
                    },
                    // note: a session that has said it is finished closing its socket is not a
                    // dropped connection. The difference is a minute of reconnection attempts at
                    // something that did what it was told
                    Ok(None) => match self.over {
                        true => Some(Left::Done),
                        false => Some(Left::Dropped),
                    },
                    // note: a reset after `session.finished` is the end too. A session that has
                    // ended can close with something this client wrote still unread, and a socket
                    // closed that way reads here as a reset once the session's last write has been
                    // read
                    Err(_) if self.over => Some(Left::Done),
                    // note: a connection going is not a session saying something, and the two read
                    // one way at a reader: a reset is `ECONNRESET` and nothing was said in it. A
                    // sentence about what was unreadable sends whoever reads it looking for a
                    // message the session never wrote - and the two are told apart by the wording
                    // `protocol::read` gives each, which is why it is passed on as it stands
                    Err(e) => {
                        let _ = self.tell(&e);
                        Some(Left::Dropped)
                    }
                },
                // note: the branch is switched off once the input has closed, rather than reading
                // its end for ever. A `select!` arm over a reader at EOF is ready every loop
                // iteration, so this one would have spun on it
                line = typed.next_line(), if !self.detaching && self.reads() => match line {
                    // note: what `--headless` does with a blank line and with spaces round one,
                    // for its reason: a blank line sent is a request for nothing, and `  /help`
                    // would be a message here and a command there
                    Ok(Some((line, _))) if line.trim().is_empty() => None,
                    Ok(Some((line, mangled))) => {
                        if mangled {
                            let _ = self.tell(&crate::headless::not_text(&line));
                        }
                        match self.line(&mut write, line.trim()).await {
                            Ok(()) => None,
                            Err(_) => Some(Left::Dropped),
                        }
                    }
                    // note: when the input closes, the client detaches rather than stopping the
                    // session. That is the invariant, not a shortcut. A script that pipes in a
                    // question and leaves has asked about someone else's session; ending it would
                    // be this client deciding that session's lifetime.
                    //
                    // note: the client waits for the turn before it goes, the same rule
                    // `--headless` follows. `echo "question" | kamchatka --connect` asks for the
                    // answer, not for the question to be asked and abandoned. Detaching on sight
                    // loses the end of the answer: the socket closes while the model is still
                    // writing. It waits on `Client::resting`, which reads what the session said
                    Ok(None) => {
                        // note: `follow` asks for the records, not here - `Client::catch_up`
                        // explains why. A projection carries the watermark, so an input that closed
                        // before one arrived would ask against nothing and write none of the log
                        self.detaching = true;
                        match self.settle(&mut write).await {
                            Ok(()) => self.resting().then_some(Left::Done),
                            Err(_) if self.unwritable => None,
                            Err(e) => Some(Left::Failed(e)),
                        }
                    }
                    Err(e) => Some(Left::Failed(format!("could not read the input: {e}"))),
                },
                // one stop, then leave: the same two stages as `--headless`, for the same reason.
                // The first is for the turn, which keeps what arrived; the second is for this
                // process. The session carries on without it.
                //
                // note: the flag belongs to this connection, not this client. A socket that dropped
                // and came back is a fresh pair of stages; otherwise a single `ctrl+c` pressed an
                // hour ago could detach someone from a turn they are watching now
                () = presses.pressed() => match stopping {
                    // the second one: the turn has been asked to stop and this client is not
                    // waiting to watch it happen. The session carries on without it - the
                    // invariant, not a shortcut. See the note on `remote`
                    true => Some(Left::Done),
                    false => match self.say_to(&mut write, Command::Interrupt).await {
                        Ok(()) => {
                            stopping = true;
                            let _ = self.prose.fresh_line();
                            let _ = self.tell(
                                "asked it to stop; what has arrived is kept, and again detaches",
                            );
                            None
                        }
                        Err(_) => Some(Left::Dropped),
                    },
                }
            };
            if let Some(left) = ended {
                let _ = self.prose.fresh_line();
                // note: said wherever this client leaves, not on the branch that happened to
                // notice. `/restart` ends a session the way `/quit` does. A client whose input had
                // already closed detaches the moment the session is quiet, so the ending was told
                // to nobody and the run read as simply finished. Only a leaving client says it: a
                // connection that went after the session said it was finished is one to pick back
                // up, and the line belongs to the last one. The session after a restart is not this
                // client's to carry on into; see `Client::session`
                if self.over && matches!(left, Left::Done) {
                    let _ = self.tell(
                        "the session has ended; if it was restarted, `--connect` again attaches \
                         to the new one",
                    );
                }

                return left;
            }
        }
    }

    /// Takes in one message from the session.
    fn heard(&mut self, message: Message) -> Result<(), String> {
        self.reached = true;
        match message {
            Message::Attached(attached) => self.arrived(&attached),
            Message::Record(record) => {
                self.last = record.seq;
                self.written = record.seq;
                let line = serde_json::to_string(&record).map_err(|e| e.to_string())?;
                writeln!(self.records, "{line}").map_err(|e| e.to_string())?;
                self.records.flush().map_err(|e| e.to_string())?;

                self.happened(&record.event)
            }
            // note: the watermark moves, which is how this gets past it. The record exists and is
            // in the session's log, but this client cannot have it down this wire. A client that
            // left its watermark behind would ask for the same record on every reconnection for the
            // rest of the session
            Message::Oversized { seq, bytes } => {
                self.last = seq;
                // and the written mark with it, for the reason `Message::Oversized` exists: this
                // client will never be sent this record, so a catch-up stopping short of it would
                // ask for it again for the rest of the session and be named every time
                self.written = seq;
                self.prose.fresh_line()?;
                self.tell(&format!(
                    "record {seq} is {bytes} bytes, too long to send; it is in the session's log, \
                     and `inspect` fetches what it is about"
                ))
            }
            Message::Progress { event, .. } => self.happened(&event),
            // note: nothing, and the same nothing `--headless` does with it. What this writes is a
            // stream rather than a screen: the lines are already down a pipe and on somebody's
            // terminal, and there is no taking them back. A page can clear itself and this cannot,
            // so saying so would be a line about lines that are still there
            Message::Cleared => Ok(()),
            // note: counted but not drawn. It answers `project`, `cycle` and `revise`, which this
            // client does not send; it only renders the conversation. An answer is an answer
            // whoever asked, and one left uncounted is a count that never comes back down. A client
            // that wants the figures reads them off it; see `web/browser.html`
            Message::Projected(_) => {
                self.answered();

                Ok(())
            }
            Message::Said { speaker, text } => {
                self.prose.fresh_line()?;
                match speaker {
                    Speaker::Error => self.tell(&format!("error: {text}")),
                    _ => self.tell(&text),
                }
            }
            Message::Replied { page, busy, .. } => {
                self.busy = busy;
                self.answered();
                let Some(page) = page else {
                    return Ok(());
                };
                self.prose.fresh_line()?;
                writeln!(self.prose, "--- {} ---", page.title).map_err(|e| e.to_string())?;
                // note: print every page, not just the one it opened at. `--headless` gives all
                // pages because a screen turns them with arrow keys and there is no key to press
                // down a socket; a caller handed one page has no way to ask for the others
                for face in &page.pages {
                    if page.pages.len() > 1 {
                        writeln!(self.prose, "-- {} --", face.name).map_err(|e| e.to_string())?;
                    }
                    writeln!(self.prose, "{}", face.body).map_err(|e| e.to_string())?;
                }
                self.prose.flush().map_err(|e| e.to_string())
            }
            // the words of a turn that was never streamed; only this client asks for raw, and `?N`
            // asks for the reading.
            //
            // note: the fresh line stops one answer running into the next. It is here rather than
            // on `ModelRequested` because that is not where a whole answer begins: a turn whose
            // fragments never crossed the wire is fetched at the end, after every request in the
            // batch has been read, so a break taken on the request belongs to nothing that is
            // written
            Message::Item {
                body, raw: true, ..
            } => {
                self.answered();
                self.prose.fresh_line()?;
                self.prose.write_answer(&body)
            }
            Message::Item { id, body, .. } => {
                self.answered();
                self.prose.fresh_line()?;
                writeln!(self.prose, "--- item {id} ---\n{body}").map_err(|e| e.to_string())?;
                self.prose.flush().map_err(|e| e.to_string())
            }
            Message::Busy { busy } => {
                self.busy = busy;

                Ok(())
            }
            Message::Reaching { waiting } => self.reaching(&waiting),
            // note: said rather than swallowed, because it is the one change to a session that
            // nothing else here would show. This client prints the conversation and the records,
            // and a model switch is in neither - so without this, a session that changed model
            // under it would read as one that had not
            Message::Model { model } => {
                let model = model.map(|model| model.model);
                if self.model.as_ref() == Some(&model) {
                    return Ok(());
                }
                self.prose.fresh_line()?;
                self.tell(&match &model {
                    Some(model) => format!("the model is {model}"),
                    None => "there is no model".to_owned(),
                })?;
                self.model = Some(model);

                Ok(())
            }
            Message::Done { busy, .. } => {
                self.busy = busy;
                self.answered();
                // note: the resume's answer rather than its first record, because the missed
                // records are sent ahead of it and one may be the decision this is about to ask
                // again
                match std::mem::take(&mut self.resuming) {
                    true => self.reopened(),
                    false => Ok(()),
                }
            }
            Message::Missed { frames } => {
                self.prose.fresh_line()?;
                // note: no promise about the records, and the wording used to make one that was
                // false twice over. The records name what happened and do not copy it: an answer is
                // a turn by its identifier. What went past here can be the program's own lines as
                // well as a model's typing, which are in no log at all. This is the count of what
                // this client was not shown while it was not looking; to see any of it, ask the
                // session rather than reading the log
                self.tell(&format!(
                    "{frames} fragment(s) went by that this client was not sent; what it missed \
                     is not in the records, which name what happened without holding it"
                ))
            }
            // note: printed as nothing: ignore what you do not know. A session that has grown a
            // message since this build was made can still be followed, because the records carry
            // what happened.
            //
            // note: counted as an answer, because it may be one and this end cannot tell. `Unknown`
            // carries no payload - `#[serde(other)]` takes a unit variant - so a client owed an
            // answer and handed something it cannot read has been answered, as far as it can tell.
            // Counting it leaves this client early off the end of a command whose answer it could
            // not have printed anyway. Not counting it leaves `outstanding` up for the rest of the
            // connection: stdin closing never detaches and the session going quiet never ends
            Message::Unknown => {
                self.answered();

                Ok(())
            }
            Message::Failed { about, error } => {
                self.answered();
                // note: three failures nothing here can mend. A version refusal is said again,
                // for the same reason, on attaching afresh, so a client that retried it would
                // spend a minute on it and leave saying the session had not answered, when it
                // answered at once with the sentence below. A projection too large to send is the
                // same shape: the session answered, a retry would read the refused frame as a drop,
                // and what a client *can* do is read the records, which the sentence says. And
                // being replaced is the end of this client by design: the session serves one client
                // at a time, so coming back by itself would take it from whoever just did, and two
                // clients each reconnecting would trade it back and forth for a minute
                if matches!(about.as_str(), "version" | "projection" | "replaced") {
                    self.prose.fresh_line()?;

                    return Err(format!("{about}: {error}"));
                }
                // a resume refused by a session that has not ended is a different session at the
                // same address; see `Client::session`. One that has ended was cut short by the
                // ending, and the close that follows is read as the ending it is
                if about == "attach"
                    && !self.over
                    && let Some(followed) = self.session.take()
                {
                    self.prose.fresh_line()?;

                    return Err(format!(
                        "the session at this address is not `{followed}`, which this client was \
                         following, and `--connect` follows one session: {error}. Run it again to \
                         attach to the one there now"
                    ));
                }
                self.prose.fresh_line()?;
                self.tell(&format!("{about}: {error}"))
            }
        }
    }

    /// Prints where a session stands, for somebody who has just arrived at it.
    fn arrived(&mut self, attached: &Attached) -> Result<(), String> {
        self.last = attached.seq;
        self.session = Some(attached.session.clone());
        self.model = Some(attached.model.as_ref().map(|model| model.model.clone()));
        self.busy = attached.busy;
        self.answered();
        writeln!(
            self.prose,
            "--- {} · {} record(s), {} item(s), ~{} tokens{} ---",
            attached.session,
            attached.seq,
            attached.items.len(),
            attached.expected.unwrap_or_else(|| attached.budget.used()),
            match &attached.model {
                Some(model) => format!(", {}", model.model),
                None => String::new(),
            }
        )
        .map_err(|e| e.to_string())?;
        for line in &attached.conversation {
            writeln!(
                self.prose,
                "{} {}{}",
                speaks(line.speaker),
                line.text,
                clipped(line)
            )
            .map_err(|e| e.to_string())?;
        }
        writeln!(
            self.prose,
            "--- a line is a message, a line starting with `/` is a command, `?N` says what item \
             N holds, and `ctrl+c` stops the turn ---"
        )
        .map_err(|e| e.to_string())?;

        // note: taken from the projection rather than left to the records to rebuild, because a
        // question raised before this client existed was asked in a record it will never be sent
        self.asking = attached.asking.iter().cloned().collect();
        self.answering.clear();
        let asking: Vec<_> = self.asking.iter().cloned().collect();
        for request in &asking {
            self.question(request)?;
        }
        // the other kind is printed afresh: whatever was printed before belongs to a conversation
        // this has just been handed again
        self.told.clear();
        self.reaching(&attached.reaching)?;

        self.prose.flush().map_err(|e| e.to_string())
    }

    /// Takes in one thing that happened, whether it came numbered or not.
    ///
    /// note: the same four `--headless` prints are printed. The whole trace is in the records, and
    /// printing all of it would bury the answer somebody is waiting for under the forty lines it
    /// took to get there. The two extra here are the permission pair, because unlike a headless run
    /// there is somebody to ask. A question nobody printed is a session that has silently stopped.
    fn happened(&mut self, event: &Event) -> Result<(), String> {
        if let Event::ModelDelta {
            delta: Delta::Text(text),
        } = event
        {
            if self.fetching {
                return Ok(());
            }
            self.streamed = true;

            return self.prose.write_answer(text);
        }

        match event {
            // note: the fresh line is `--headless`'s: the last answer very likely ended
            // mid-sentence, and the first fragment of this one was written straight after it
            Event::ModelRequested { .. } => {
                (self.streamed, self.fetching) = (false, false);
                self.prose.fresh_line()?;
            }
            Event::ModelFinished { item, .. } => {
                self.failed = false;
                if !std::mem::take(&mut self.streamed) {
                    self.unstreamed.push_back(*item);
                    self.fetching = true;
                }
            }
            Event::ToolRequested { tool, args, .. } => {
                self.prose.fresh_line()?;
                writeln!(self.prose, "⟩ {tool}({args})").map_err(|e| e.to_string())?;
            }
            Event::ToolFinished {
                tool,
                is_error,
                tokens,
                ..
            } => {
                self.prose.fresh_line()?;
                writeln!(
                    self.prose,
                    "· {tool}: {}{}",
                    plural(*tokens, "token"),
                    match is_error {
                        true => ", an error",
                        false => "",
                    }
                )
                .map_err(|e| e.to_string())?;
            }
            Event::PermissionRequested { request } => {
                self.asking.push_back(request.clone());
                let request = request.clone();
                self.question(&request)?;
            }
            // note: however it was decided, and by whoever. Somebody else attached to the same
            // session answering a question this client is showing is exactly the case where a
            // client that only tracked its own answers would go on offering a question that had
            // been settled a minute ago
            Event::PermissionDecided {
                id, tool, grant, ..
            } => {
                self.asking.retain(|waiting| waiting.id != *id);
                self.answering.retain(|waiting| waiting.id != *id);
                self.prose.fresh_line()?;
                self.tell(&format!("{tool}: {grant}"))?;
            }
            Event::SessionFinished => self.over = true,
            Event::ModelFailed { .. } | Event::StepFailed { .. } => self.failed = true,
            event => {
                if let Some(line) = crate::app::text::went_in(event) {
                    self.prose.fresh_line()?;
                    self.tell(&line)?;
                }
            }
        }

        self.prose.flush().map_err(|e| e.to_string())
    }

    /// Asks again whatever this answered on a connection that went before the session decided it.
    fn reopened(&mut self) -> Result<(), String> {
        let lost: Vec<_> = self.answering.drain(..).collect();
        for request in lost.iter().rev() {
            self.asking.push_front(request.clone());
        }
        if !lost.is_empty() {
            self.prose.fresh_line()?;
        }
        for request in &lost {
            self.question(request)?;
        }

        Ok(())
    }

    /// Takes in the list of running commands waiting on the network, and prints the new ones.
    fn reaching(&mut self, waiting: &[Reached]) -> Result<(), String> {
        self.reaching = waiting
            .iter()
            .filter(|reached| !self.let_through.contains(&reached.id))
            .cloned()
            .collect();
        let fresh: Vec<Reached> = self
            .reaching
            .iter()
            .filter(|reached| !self.told.contains(&reached.id))
            .cloned()
            .collect();
        for reached in fresh {
            self.told.insert(reached.id);
            self.prose.fresh_line()?;
            writeln!(
                self.prose,
                "? `{}` is running and has reached for the network\n  it waits for an answer, \
                 which holds for the rest of it; `y` lets it, `n` refuses, `a` allows the network \
                 from now on",
                one_line(&reached.cmd)
            )
            .map_err(|e| e.to_string())?;
        }

        self.prose.flush().map_err(|e| e.to_string())
    }

    /// Prints a question, and the three answers to it.
    fn question(&mut self, request: &PermissionRequest) -> Result<(), String> {
        let capabilities: Vec<_> = request
            .capabilities
            .iter()
            .map(ToString::to_string)
            .collect();
        writeln!(
            self.prose,
            "? {} wants to run {}({})\n  it declares {}; `y` allows it, `n` refuses, `a` allows \
             this and stops asking",
            request.id,
            request.tool,
            one_line(&request.args.to_string()),
            match capabilities.is_empty() {
                true => "nothing".to_owned(),
                false => capabilities.join(", "),
            }
        )
        .map_err(|e| e.to_string())?;

        self.prose.flush().map_err(|e| e.to_string())
    }

    /// Interprets one typed line.
    ///
    /// note: two of these are the client's own rather than the session's, and both are spelled the
    /// way the terminal spells them. `y`, `n` and `a` are the keys the permission panel takes, with
    /// the meanings they have there, so a *message* of `y` cannot be sent while a question is open,
    /// just as it cannot be typed while the question holds the prompt. Everything else goes to the
    /// session verbatim, because every verb this program has is a line handed to `App::submit`, and
    /// a protocol with a message per verb would be a second vocabulary.
    async fn typed<W: AsyncWrite + Unpin>(
        &mut self,
        write: &mut W,
        line: &str,
    ) -> Result<(), String> {
        let answer = match line {
            "y" => Some((Grant::Allow, false)),
            "n" => Some((Grant::Deny, false)),
            "a" => Some((Grant::Allow, true)),
            _ => None,
        };
        // taken off as it is answered, for the reason `settle` gives
        if let Some((grant, remember)) = answer
            && let Some(request) = self.asking.pop_front()
        {
            let id = request.id;
            self.answering.push_back(request);

            return self
                .say_to(
                    write,
                    Command::Decide {
                        id,
                        grant,
                        remember,
                    },
                )
                .await;
        }
        // note: the kernel's question first if both are somehow waiting, which is the order the
        // panel at a terminal takes them in
        if let Some((grant, remember)) = answer
            && let Some(reached) = self.reaching.pop_front()
        {
            self.let_through.insert(reached.id);

            return self
                .say_to(
                    write,
                    Command::Reach {
                        id: reached.id,
                        grant,
                        remember,
                    },
                )
                .await;
        }
        if let Some(id) = line.strip_prefix('?').and_then(|n| n.trim().parse().ok()) {
            return self
                // `?N` at this client prints the reading rather than the text: an item for somebody
                // to look at, and nothing here edits one
                .say_to(
                    write,
                    Command::Inspect {
                        id: ContextId(id),
                        raw: false,
                        version: None,
                    },
                )
                .await;
        }

        self.say_to(
            write,
            Command::Submit {
                line: line.to_owned(),
            },
        )
        .await
    }

    /// Sends one command, and remembers that it is owed an answer to it.
    async fn say_to<W: AsyncWrite + Unpin>(
        &mut self,
        write: &mut W,
        command: Command,
    ) -> Result<(), String> {
        self.outstanding += 1;

        protocol::write(write, &command)
            .await
            .inspect_err(|_| self.unwritable = true)
    }

    /// Takes one of the answers this was owed.
    ///
    /// note: saturating, because a [`Message::Failed`] is the one answer that arrives without a
    /// request, when a connection breaks, and a count that wrapped there would leave this client
    /// owed answers that will never come.
    fn answered(&mut self) {
        self.outstanding = self.outstanding.saturating_sub(1);
    }

    /// Whether the session has caught up with everything this client asked, and is idle.
    ///
    /// note: what the input closing waits for. `echo "question" | kamchatka --connect` asks for the
    /// answer, not for the question to be asked and abandoned; `--headless` follows the same rule.
    ///
    /// note: a question is not rest, and a client that reads `busy` alone leaves a session wedged.
    /// A turn paused on a permission question is not running, so `busy` is false while the kernel
    /// sits in `Deciding`; a client that took that for the end of the turn would print the
    /// question, detach, and exit `0`, leaving a served session waiting on an answer that can no
    /// longer come. [`Client::settle`] ends the wait; this only keeps the state from counting as
    /// rest.
    ///
    /// note: a client that leaves its questions is the exception: a session waiting on nothing but
    /// a question is at rest, because the turn will not move until somebody else answers, and
    /// waiting here would be a client that never exits. A line held behind that question waits on
    /// the same somebody, so it does not keep the client either: it goes unsent, and
    /// [`Client::unsent`] says so.
    fn resting(&self) -> bool {
        let asked = !self.asking.is_empty() || !self.reaching.is_empty();
        match self.leaves && asked {
            true => self.outstanding == 0,
            false => self.idle() && self.held.is_empty(),
        }
    }

    /// Whether the next line of input may be read.
    ///
    /// note: past a held line only for a client that leaves its questions, and only while one is
    /// open: the held line waits for somebody else, and this client has to learn whether its own
    /// input has closed, or it waits with them for as long as nobody comes.
    fn reads(&self) -> bool {
        let asked = !self.asking.is_empty() || !self.reaching.is_empty();
        match self.held.is_empty() {
            true => self.ready(),
            false => self.leaves && asked,
        }
    }

    /// Says that the lines held behind a question were never sent, if any were; for a client
    /// leaving.
    fn unsent(&mut self) -> Result<(), String> {
        if self.held.is_empty() {
            return Ok(());
        }
        let lines: Vec<String> = self
            .held
            .drain(..)
            .map(|line| format!("`{line}`"))
            .collect();
        self.prose.fresh_line()?;
        self.tell(&format!(
            "{} not sent: {} for the turn to be over, and the turn waits on the question left for \
             another client",
            match lines.len() {
                1 => format!("{} was", lines[0]),
                _ => format!("{} were", lines.join(", ")),
            },
            match lines.len() {
                1 => "it waits",
                _ => "they wait",
            },
        ))
    }

    /// Whether the session has caught up with this client and asks it nothing, with no turn
    /// running.
    fn idle(&self) -> bool {
        !self.busy && self.outstanding == 0 && self.asking.is_empty() && self.reaching.is_empty()
    }

    /// Takes one line of input: sent, or held where a question waits and the line does not
    /// answer it; see [`Client::held`].
    async fn line<W: AsyncWrite + Unpin>(
        &mut self,
        write: &mut W,
        line: &str,
    ) -> Result<(), String> {
        let asked = !self.asking.is_empty() || !self.reaching.is_empty();
        if !self.held.is_empty() || (self.paced && asked && !matches!(line, "y" | "n" | "a")) {
            self.held.push_back(line.to_owned());
            return self.answer_for_nobody(write).await;
        }

        self.typed(write, line).await
    }

    /// Whether the next line may be read; see [`Client::paced`].
    ///
    /// note: a question waiting opens it while a turn runs, because the network gate asks while a
    /// command is still running and the line that answers it is the next one in the script.
    fn ready(&self) -> bool {
        !self.paced
            || (!self.busy && self.outstanding == 0)
            || !self.asking.is_empty()
            || !self.reaching.is_empty()
    }

    /// Sends what the last message heard calls for: a fetch of every turn that was not streamed,
    /// and whatever [`Client::settle`] answers.
    async fn follow<W: AsyncWrite + Unpin>(&mut self, write: &mut W) -> Result<(), String> {
        while let Some(id) = self.unstreamed.pop_front() {
            self.say_to(
                write,
                Command::Inspect {
                    id,
                    raw: true,
                    version: None,
                },
            )
            .await?;
        }

        // note: on every message rather than when the input closes. A projection carries where the
        // log has got to, so it is the first thing to say how many records this client has not
        // written. If the input closed before it arrived, the client would detach against a
        // watermark of nothing and write none of them. A client with somebody at it asks for
        // nothing, so this costs one comparison per turn
        if self.detaching {
            self.catch_up(write).await?;
        }
        if self.idle()
            && let Some(line) = self.held.pop_front()
        {
            self.typed(write, &line).await?;
        }

        self.settle(write).await
    }

    /// Asks the session for every record this client has not written out yet, and writes them.
    ///
    /// note: the client side of what `--headless` does at the top of every turn of its loop, and
    /// for the reason that loop is documented on. A `--connect` is a drop-in for it, and its
    /// stdout is meant to be the same bytes `/save` writes - which is the whole of the session's
    /// log, not the tail of it. A client that read no `Message::Record` at all wrote nothing, so
    /// `printf '/budget\n' | kamchatka --connect` handed a script an empty file beside a summary
    /// of a session on the other stream, and a script reading stdout for the record could not tell
    /// the two apart.
    ///
    /// note: asked for rather than kept up to date as records arrive, because a projection is the
    /// stream's mark and not this client's: a fresh attach is answered with where the log has got
    /// to, and everything before that is in no message this client was ever sent. `since` is what
    /// the protocol already answers with the records after it, so this is one more resume and no
    /// new command.
    ///
    /// note: and only what has not been written, so this is free when there is nothing to fetch -
    /// which is every turn of a session this client is watching, and the whole cost is one attach
    /// on a client that is leaving.
    async fn catch_up<W: AsyncWrite + Unpin>(&mut self, write: &mut W) -> Result<(), String> {
        // note: the `outstanding` as well as the watermark. A resume that is asked for is owed an
        // answer, and until that answer arrives `written` has not moved - so a second message in
        // between would ask again, and the client would owe itself one more answer than the session
        // will ever send
        if self.written >= self.last || self.outstanding > 0 {
            return Ok(());
        }
        // note: a resume rather than a fresh attach, so the session sends the records and not a
        // projection. The projection is what a client is leaving, and asking for it again would
        // print the whole conversation a second time
        self.say_to(
            write,
            Command::Attach {
                since: Some(self.written),
                session: self.session.clone(),
                version: Some(protocol::VERSION),
            },
        )
        .await
    }

    /// Answers whatever is still being asked, once there is nobody here to ask.
    ///
    /// note: nothing at all until the input has closed, which makes it safe to call on every
    /// message. A client with somebody at it answers with `y`, `n` and `a`; a question settled from
    /// under them would decide what they were about to.
    ///
    /// note: taken off the queue as it is answered rather than when the session says so. The answer
    /// comes back as a `permission.decided` record and clears it there too, but that record arrives
    /// after the next pass through this, which would send a second `Decide` for a question already
    /// answered, and a third, for as long as the session took to reply.
    async fn settle<W: AsyncWrite + Unpin>(&mut self, write: &mut W) -> Result<(), String> {
        if !self.detaching {
            return Ok(());
        }

        self.answer_for_nobody(write).await
    }

    /// Answers every question waiting with `--on-ask`, and says so.
    async fn answer_for_nobody<W: AsyncWrite + Unpin>(
        &mut self,
        write: &mut W,
    ) -> Result<(), String> {
        if self.leaves {
            let asked = !self.asking.is_empty() || !self.reaching.is_empty();
            if asked && !self.said_left {
                self.said_left = true;
                self.prose.fresh_line()?;
                self.tell(
                    "nobody is here to answer what the session is asking, so it is left for \
                     another client, or for this one coming back",
                )?;
            }
            return Ok(());
        }
        while let Some(request) = self.asking.pop_front() {
            self.prose.fresh_line()?;
            self.tell(&format!(
                "nobody is here to answer for `{}`, so it is answered `{}`",
                request.tool, self.on_ask
            ))?;
            let id = request.id;
            self.answering.push_back(request);
            self.say_to(
                write,
                Command::Decide {
                    id,
                    grant: self.on_ask,
                    // note: never. A standing rule outlives this client and this turn, and a rule
                    // nobody typed is the one kind the policy should not learn from - least of all
                    // from a run where nobody was watching
                    remember: false,
                },
            )
            .await?;
        }
        while let Some(reached) = self.reaching.pop_front() {
            self.prose.fresh_line()?;
            self.tell(&format!(
                "nobody is here to answer whether `{}` may reach the network, so it is answered \
                 `{}`",
                one_line(&reached.cmd),
                self.on_ask
            ))?;
            self.let_through.insert(reached.id);
            self.say_to(
                write,
                Command::Reach {
                    id: reached.id,
                    grant: self.on_ask,
                    // never, for the reason the kernel's questions are never remembered here
                    remember: false,
                },
            )
            .await?;
        }

        Ok(())
    }

    /// Says something in the client's own voice.
    fn tell(&mut self, text: &str) -> Result<(), String> {
        writeln!(self.prose, "· {text}").map_err(|e| e.to_string())?;

        self.prose.flush().map_err(|e| e.to_string())
    }
}

/// Opens whichever kind of connection the address asks for.
///
/// Also used by `web`, whose page is a client too.
pub(crate) async fn connect(address: &str) -> Result<super::Connection, String> {
    match protocol::address(address)? {
        Address::Unix(path) => match protocol::overlong_path(path) {
            // note: the same refusal the bind gives, said before the connect. The kernel refuses a
            // path this long without naming the limit or a shorter place, and a client that cannot
            // reach the socket is the one person who cannot tell a wrong path from a path nothing
            // is listening at
            Some(bytes) => Err(format!(
                "a socket file cannot be named by {bytes} bytes, and this one is; a `unix:` path \
                 can be at most {} bytes - ask for the socket under `$XDG_RUNTIME_DIR` or \
                 in `/tmp`",
                protocol::MAX_PATH
            )),
            None => tokio::net::UnixStream::connect(path)
                .await
                .map(super::Connection::Unix)
                .map_err(|e| format!("could not reach {path}: {e}")),
        },
        Address::Tcp(host) => tokio::net::TcpStream::connect(host)
            .await
            .map(|stream| {
                // the two options a socket file never needed; see `remote::tuned`
                super::tuned(&stream);

                super::Connection::Tcp(stream)
            })
            .map_err(|e| format!("could not reach {host}: {e}")),
    }
}

/// What a line of the conversation says on the end of it about what it was not sent with, if
/// anything; see [`protocol::Line::clipped`].
///
/// note: and the way to the rest where there is one, which is `?N` - the one command this client
/// has for reading an item whole, and the one somebody holding a cut line wants next. Except where
/// the line alone is longer than a frame: no answer carries that, `?N` comes back refused, and
/// pointing at it would be sending somebody round to be told no. What does hold it is a snapshot,
/// so that is where this points instead. Measured on the text rather than as JSON, so it is only
/// ever right to say - an item a little under the line can still be refused, and the refusal
/// says so - and it never says it of one an answer could carry.
fn clipped(line: &protocol::Line) -> String {
    let Some(gone) = line.clipped else {
        return String::new();
    };
    let rest = match line.item {
        // a line of the limit exactly is past it as an answer, which wraps it in quotes and more
        Some(_) if line.text.len() + gone >= protocol::MAX_LINE => {
            "; that is more than any one answer carries, and `/save` writes a snapshot that holds \
             it whole"
                .to_owned()
        }
        Some(id) => format!("; `?{id}` asks for the whole of it"),
        None => String::new(),
    };

    format!(" … [{} more not sent{rest}]", plural(gone, "byte"))
}

/// The mark a line of the conversation is printed under.
///
/// note: uses the same characters as the headless run where it uses any, and its own where it has
/// none, because it prints a whole conversation at once and identical marks down the left say
/// nothing about who was talking.
fn speaks(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::User => ">",
        Speaker::Model => " ",
        Speaker::Reasoning => "~",
        Speaker::Call => "⟩",
        Speaker::Result => "<",
        Speaker::Note => "·",
        Speaker::Error => "!",
    }
}
