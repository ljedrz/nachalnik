//! The other end: a session somebody else is running, driven by lines.
//!
//! note: deliberately the smallest client that exercises the whole protocol rather than the nicest
//! one to use. What it is for is proving the seam - attach, read a projection, follow a turn,
//! submit, interrupt, answer a question, lose the socket and pick the same session back up at the
//! record it had got to - before anything with a screen in it is written against the protocol. A
//! client that is pleasant to use is a different program, and this is the thing that says the
//! protocol is worth writing one against.
//!
//! note: it writes what `--headless` writes, in the same two streams and the same words: the
//! records to one, what a person reads to the other. That is what makes `kamchatka --connect` a
//! drop-in for `kamchatka --headless` in a script, and what lets the same shape of test drive both.

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

/// How long a dropped connection is picked back up for before this gives up on it.
///
/// note: a while rather than for ever, because the two reasons a socket dies look identical from
/// here - the host was restarted, or the host is gone - and a client that retried indefinitely
/// would be a process somebody has to notice and kill. Resuming is the point of the retry: each
/// attempt attaches with the last record this client actually saw, so picking the session back up
/// costs the records it missed and nothing else.
///
/// note: a minute, and the figure is about *networks* rather than about patience. Over a socket
/// file the only way to lose a connection is the host going away, and it either comes back at
/// once or it is not coming back. Over a port the ordinary reason to lose one is a laptop changing
/// access points, and the ordinary time to get it back is several seconds - so a limit sized for a
/// socket file reports a session lost while it is still there.
const GIVE_UP: Duration = Duration::from_secs(60);

/// How long to wait before picking it back up the first time.
const FIRST_WAIT: Duration = Duration::from_millis(250);

/// The longest that wait grows to, doubling.
///
/// note: backing off rather than a fixed pause, for the two cases at once. A host restarting is
/// back in well under a second and should be picked up in the first attempt or two; a network that
/// has gone is not coming back inside a minute however often anybody asks, and sixty seconds of
/// quarter-second attempts is two hundred and forty connections nobody wanted made.
const LONGEST_WAIT: Duration = Duration::from_secs(5);

/// A client of somebody else's session.
pub struct Client<'a> {
    /// The record stream, one JSON object per line: the same bytes `/save` writes.
    records: &'a mut dyn Write,
    /// What a person reads: the model's own words, and what the session had to say about the run.
    prose: crate::headless::Prose<&'a mut dyn Write>,
    /// Whether any of the answer being written has been printed as it arrived.
    streamed: bool,
    /// The recorded turns whose words were not streamed, to be fetched with an `inspect`.
    ///
    /// note: the same gap `--headless` fills from the item, filled the only way a client can: the
    /// records name a turn and do not copy it, so a provider that answers in one piece puts
    /// nothing on this wire but a `model.finished`.
    unstreamed: VecDeque<ContextId>,
    /// Whether the answer being fetched is one whose fragments may still arrive, and are dropped.
    ///
    /// note: a session behind the broadcast can write a turn's fragments after the record that
    /// ends it, so a turn that looked unstreamed at `model.finished` may stream afterwards; printed
    /// as well, it would be printed twice.
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
    /// note: this is what says whether there is anything to resume, as well as what a resume
    /// names: `Some` is a client holding a conversation and a watermark, `None` one with nothing.
    ///
    /// note: a resume refused is the end of this client rather than a fresh attach, because what
    /// refuses one is a session that is not this one - a host restarted at the same address - and
    /// attaching to it would carry on the record stream with another session's records under the
    /// first one's, with nothing in it to say where one ended. `--connect` stands in for
    /// `--headless`, which is one session, and says how to attach to the other instead.
    session: Option<String>,
    /// The questions waiting on somebody, oldest first.
    asking: VecDeque<PermissionRequest>,
    /// The questions this has answered and not yet seen decided, oldest first.
    ///
    /// note: kept until the `permission.decided` record rather than forgotten with the answer,
    /// because an answer can die in a socket the session never read. A resume is sent the records
    /// after the last one this client saw, and the question was asked before that - so a question
    /// dropped here is one the session goes on waiting for and this client can no longer answer.
    /// What the resume leaves undecided goes back on `asking` once the attach is answered.
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
    /// note: the same flag `--headless` reads, and it reaches the same two states for the same
    /// reason: an input that has closed cannot be asked anything. It is this client's rather than
    /// the session's - `--connect` takes nothing else on the command line precisely because
    /// everything else belongs to whoever is serving, and what a *detaching* client does with an
    /// open question does not.
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
    /// note: the pair of these is what the input closing waits on; see [`Client::resting`]. A count
    /// rather than a flag because the first `attach` and a line already sitting in a pipe overlap
    /// every time a question is piped in, and it is a count of *answers owed* rather than of
    /// anything else because that is the only thing a client knows without guessing: every command
    /// gets exactly one answer, and until it arrives the session has not caught up with what this
    /// asked for. Get either half wrong and a piped-in question is answered to a client that has
    /// already detached.
    outstanding: usize,
    /// Whether the input has closed and this is waiting for the session to come to rest.
    detaching: bool,
    /// Whether the session has said it is finished.
    ///
    /// note: what tells a closing socket apart from a dropped one, and without it a `/quit` typed
    /// at this client is answered by a minute of attempts to reattach to a session that did exactly
    /// what it was asked. `session.finished` is a record like any other, so this is the session
    /// saying so rather than this client inferring it from a silence.
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
    /// note: what makes [`GIVE_UP`] a minute from the last drop rather than a minute across the
    /// whole of a run. A connection the session answered on was picked back up, and the next time
    /// it goes is a drop of its own. Without this, short outages over a day add up to the minute,
    /// and the next one gives up without a single attempt, saying the session has not answered.
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
    /// note: what `--headless` does with the same script. A question nobody at a pipe answers is
    /// answered with `--on-ask`, and the line after it is read once the turn it paused is over -
    /// sent at once, it met a turn still running and was refused, and the lines after it went too.
    ///
    /// note: a queue, because a client that leaves its questions reads on while a line is held:
    /// the line waits on somebody else's answer, and only reading on says whether the input has
    /// closed. What it reads meanwhile waits behind the line, in order.
    held: VecDeque<String>,
    /// Whether a write to this connection has failed.
    ///
    /// note: what keeps a refused write from ending the connection by itself. A session that
    /// finishes writes `session.finished` and closes, and a resume this client sent in the
    /// meantime meets a closed socket - so a client that gave up on the write never read the
    /// ending already sitting in front of it, and reported a session that did what it was told
    /// as a failure. What is left to read decides instead: the end of it, after the session said
    /// it was finished, is `Left::Done`, and anything else is a drop.
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
    /// `--headless` does, or where a question waits for one to answer it.
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

    /// Attaches, drives the session from lines, and comes back when there is nothing left of
    /// either.
    ///
    /// note: `last` survives a reconnection, which is what the retry is for: a second attempt
    /// attaches with `since` set to the last record this client actually saw, so the session sends
    /// what it missed and no more. What it does *not* get back is the fragments that went by while
    /// it was away - those are in no log and the session says so rather than pretending - and what
    /// it does not need is a fresh projection, because everything that changed is in the records.
    pub async fn run(
        &mut self,
        address: &str,
        input: impl AsyncBufRead + Unpin,
    ) -> Result<(), String> {
        // note: the reader is built once and handed to each attempt, because `next_line` is
        // cancellation-safe and holds whatever it has read so far. Rebuilding it per attempt would
        // lose half a line every time a socket died mid-read
        let mut typed = crate::headless::Typed::new(input);
        let (mut first, mut waited, mut wait) = (true, Duration::ZERO, FIRST_WAIT);

        loop {
            let left = self.attend(address, &mut typed, first).await;
            first = false;
            match left {
                // the reason is not repeated, for headless's reason: it went by as the session said
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
        // note: before the connect rather than after it. An attempt that cannot connect has not
        // been answered either, and one that kept the last connection's `true` would reset the
        // wait on every failure - so a client whose session had gone would try every quarter of a
        // second for as long as the process lived, and never reach `GIVE_UP`
        self.reached = false;
        self.unwritable = false;
        let stream = match connect(address).await {
            Ok(stream) => stream,
            // a first attempt that cannot connect is a wrong address or a session that is not
            // there, and saying "reconnecting" about a connection that never happened would be
            // the program inventing a history for itself
            Err(e) => {
                return match first {
                    true => Left::Failed(e),
                    false => Left::Dropped,
                };
            }
        };
        let (read, mut write) = tokio::io::split(stream);
        let mut frames = protocol::Frames::new(BufReader::new(read));
        // note: whatever was in flight when the last socket died is owed an answer that is never
        // coming, and nothing else can ever take the count back down. It is reset per connection
        // rather than decremented on the way out, because a client cannot tell which of what it
        // sent the session had already read
        self.outstanding = 0;
        // whether `ctrl+c` has already asked the turn to stop on this connection
        let mut stopping = false;
        // subscribed once, because a second press arriving while the first is being handled is the
        // one that means leave; see `crate::stopping`
        let mut presses = match crate::stopping::Stopping::new() {
            Ok(presses) => presses,
            Err(e) => return Left::Failed(format!("could not listen for ctrl+c: {e}")),
        };

        // note: `since` is sent only where this client has a session to name it against. After a
        // first attach it has a conversation on the screen already, and asking for the projection
        // again would print the whole of it a second time above the records that carry on from it
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
            // named for what it holds rather than for the branch that usually fills it: `stopping`
            // above is this connection's two-stage `ctrl+c`, and a second binding of that name here
            // reads as the same flag being reassigned
            let ended = tokio::select! {
                // note: biased, and the order is a decision rather than a tuning knob. What the
                // session has already said is read before anything else is decided - so a client
                // whose input closes while a socket full of answers is still unread reads them
                // first, instead of `select!` tossing a coin and leaving with the last two lines of
                // a turn sitting in a buffer
                biased;

                message = protocol::read::<Message>(&mut frames) => match message {
                    // note: settled before the rest is read, because a question that arrived after
                    // the input closed is the case this exists for - the turn goes on producing
                    // them, and each one has to be answered by somebody or the session stops here
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
                    // connection that dropped, and the difference is a minute of reconnection
                    // attempts at something that did what it was told
                    Ok(None) => match self.over {
                        true => Some(Left::Done),
                        false => Some(Left::Dropped),
                    },
                    // note: a reset after `session.finished` is the end too, for the same reason. A
                    // session that has ended can close with something this client wrote still
                    // unread, and a socket closed that way reads here as a reset once the last of
                    // what the session wrote has been read
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
                // the end of it for ever. A `select!` arm over a reader at EOF is ready every time
                // round the loop, and this one would have spun on it
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
                    // note: the input closing detaches rather than stopping the session, and that
                    // is the invariant rather than a shortcut: a script that pipes a question in
                    // and goes away has asked a session that belongs to somebody else, and ending
                    // it on the way out would be this client deciding that session's lifetime.
                    //
                    // note: and it waits for the turn before it goes, which is the same rule
                    // `--headless` follows. `echo "question" | kamchatka --connect` is asking for
                    // the answer, not for the question to be asked and abandoned - and detaching
                    // on sight loses the end of the answer, because the socket closes while the
                    // model is still writing. What it waits on is `Client::resting`, which reads
                    // what the session said rather than guessing
                    Ok(None) => {
                        // note: the records are asked for from `follow` and not from here, and
                        // that is where `Client::catch_up` explains it: a projection carries the
                        // watermark, so an input that has closed before one arrived would ask
                        // against nothing and write none of the log
                        self.detaching = true;
                        match self.settle(&mut write).await {
                            Ok(()) => self.resting().then_some(Left::Done),
                            Err(_) if self.unwritable => None,
                            Err(e) => Some(Left::Failed(e)),
                        }
                    }
                    Err(e) => Some(Left::Failed(format!("could not read the input: {e}"))),
                },
                // one stop, then leave: the same two stages `--headless` has, and for the same
                // reason. The first is for the turn, which keeps what arrived; the second is for
                // this process, and the session carries on without it
                //
                // note: the flag is this connection's rather than this client's. A socket that
                // dropped and came back is a fresh pair of stages, because the alternative is a
                // single `ctrl+c` pressed an hour ago detaching somebody from the middle of a turn
                // they are watching now
                () = presses.pressed() => match stopping {
                    // the second one: the turn has been asked to stop and this client is not
                    // waiting to watch it happen. The session carries on without it, which is the
                    // invariant rather than a shortcut - see the note on `remote`
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
                // note: said wherever this client leaves rather than on the branch that happened
                // to notice. A `/restart` ends a session the way a `/quit` does, and a client
                // whose input had already closed detaches the moment the session is quiet - so the
                // ending was told to nobody, and the run read as one that had simply finished. Only
                // a client that is leaving says it: a connection that went after the session said
                // it was finished is one to pick back up, and the line belongs to the last one.
                // The session after a restart is not this client's to carry on into; see
                // `Client::session`
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
            // note: the watermark moves, which is how this gets past it. The record exists
            // and is in the session's log; what this client cannot have is it *down this wire*,
            // and a client that left its watermark behind would ask for the same record on every
            // reconnection for the rest of the session
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
            // note: counted and not drawn. It answers `project`, `cycle` and `revise`, which this
            // client does not send - it renders the conversation and nothing else - but an answer
            // is an answer whoever asked, and one left uncounted is a count that never comes back
            // down. A client that wants the figures reads them off it; see `web/browser.html`
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
                // note: every page rather than the one it opened at, for the reason `--headless`
                // gives: a screen turns them with the arrow keys and there is no key to press down
                // a socket, so a caller handed one page would be reading a reference whose other
                // pages it has no way to ask for
                for face in &page.pages {
                    if page.pages.len() > 1 {
                        writeln!(self.prose, "-- {} --", face.name).map_err(|e| e.to_string())?;
                    }
                    writeln!(self.prose, "{}", face.body).map_err(|e| e.to_string())?;
                }
                self.prose.flush().map_err(|e| e.to_string())
            }
            // the words of a turn that was never streamed, which only this client asks for raw;
            // `?N` asks for the reading
            //
            // note: the fresh line is what stops one answer running into the next, and it is here
            // rather than on `ModelRequested` because that is not where a whole answer begins: a
            // turn whose fragments never crossed the wire is fetched at the end, and by then every
            // request of the batch has been read, so a break taken on the request belongs to
            // nothing that is written
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
                // note: the resume's answer rather than its first record, because the records it
                // missed are sent ahead of it - and one of them may be the decision this is about
                // to ask again
                match std::mem::take(&mut self.resuming) {
                    true => self.reopened(),
                    false => Ok(()),
                }
            }
            Message::Missed { frames } => {
                self.prose.fresh_line()?;
                // note: no promise about the records, and the wording used to make one that was
                // false twice over. The records name what happened and do not copy it - an
                // answer is a turn by its identifier - and what went past here can be the
                // program's own lines as well as a model's typing, which are in no log at all. So
                // this is the count of what this client was not shown while it was not looking,
                // and the way to see any of it is to ask the session rather than to read the log
                self.tell(&format!(
                    "{frames} fragment(s) went by that this client was not sent; what it missed \
                     is not in the records, which name what happened without holding it"
                ))
            }
            // note: printed as nothing, which is the rule this variant is for: ignore what you do
            // not know. A session that has grown a message since this build was made is a session
            // this can still follow, because the records are what carry what happened
            //
            // note: but counted as an answer, because it may be one and this end cannot tell.
            // `Unknown` carries no payload - `#[serde(other)]` takes a unit variant - so a client
            // owed an answer and handed something it cannot read has, as far as it can tell, been
            // answered. The two ways to be wrong are not the same size: counting it leaves this
            // client early off the end of a command whose answer it could not have printed anyway,
            // and not counting it leaves `outstanding` up for the rest of the connection, which is
            // stdin closing never detaching and the session going quiet never ending it
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
            attached.budget.used(),
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
        // question raised before this client existed was asked in a record it will never be sent.
        // This is the case the projection exists for, in miniature
        self.asking = attached.asking.iter().cloned().collect();
        self.answering.clear();
        let asking: Vec<_> = self.asking.iter().cloned().collect();
        for request in &asking {
            self.question(request)?;
        }
        // and the other kind, printed afresh for the reason these are: whatever was printed before
        // belongs to a conversation this has just been handed again
        self.told.clear();
        self.reaching(&attached.reaching)?;

        self.prose.flush().map_err(|e| e.to_string())
    }

    /// Takes in one thing that happened, whether it came numbered or not.
    ///
    /// note: which of them are printed is the same four `--headless` prints, and the reasoning is
    /// its: the whole trace is in the records, and a session that printed all of it would bury the
    /// answer somebody is waiting for under the forty lines it took to get there. The two that are
    /// extra here are the permission pair, because unlike a headless run there *is* somebody to
    /// ask, and a question nobody printed is a session that has silently stopped.
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
            // note: the fresh line is `--headless`'s, for its reason: the last answer very likely
            // ended mid-sentence, and the first fragment of this one was written straight after it
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

    /// Turns one typed line into whatever it means.
    ///
    /// note: two of these are the client's own rather than the session's, and both are deliberately
    /// spelled the way the terminal spells them. `y`, `n` and `a` are the keys the permission panel
    /// takes, and they mean what they mean there - which also means a *message* of `y` cannot be
    /// sent while a question is open, exactly as it cannot be typed at a terminal whose prompt the
    /// question has taken the place of. Everything else goes to the session verbatim, because every
    /// verb this program has is a line handed to `App::submit` and a protocol with a message per
    /// verb would be a second vocabulary.
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
        // note: the kernel's question first where both are somehow waiting, which is the order the
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
                // the reading rather than the text: `?N` at this client prints an item for
                // somebody to look at, and there is nothing here that edits one
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
    /// note: saturating, because a [`Message::Failed`] is the one answer that also arrives without
    /// anybody having asked - a connection that broke says so through it - and a count that wrapped
    /// there would leave this client convinced it was owed more answers than will ever come.
    fn answered(&mut self) {
        self.outstanding = self.outstanding.saturating_sub(1);
    }

    /// Whether the session has caught up with everything this client asked of it, and is idle.
    ///
    /// note: what the input closing waits for. `echo "question" | kamchatka --connect` is asking
    /// for the answer, not for the question to be asked and abandoned - the same rule
    /// `--headless` follows.
    ///
    /// note: **a question is not rest**, and reading `busy` alone for it leaves a session wedged. A
    /// turn paused on a permission question is not running, so `busy` is false while the kernel
    /// sits in `Deciding` - and a client that took that for the end of the turn would print the
    /// question, detach, and exit `0`, leaving a served session waiting on an answer that could no
    /// longer come from anywhere. What ends the wait is [`Client::settle`]; this is only
    /// the half that stops it being called rest.
    ///
    /// note: except for a client that leaves its questions, for whom a session waiting on nothing
    /// but a question is at rest: the turn will not move until somebody else answers, and waiting
    /// here for that would be a client that never exits. A line held behind that question is
    /// waiting on the same somebody, so it does not keep the client either: it goes unsent, and
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

        // note: on every message rather than where the input closes, because of what a projection
        // is. It carries where the log has got to, so it is the first thing to say how many records
        // this client has not been written - and an input that closed before it arrived would
        // otherwise detach against a watermark of nothing and write none of them. A client with
        // somebody at it asks for nothing, so this costs a comparison a turn
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
        // note: the `outstanding` as well as the watermark. A resume asked for is a resume owed an
        // answer, and until that answer arrives `written` has not moved - so a second message in
        // between would ask again, and the client would owe itself one more answer than the
        // session will ever send
        if self.written >= self.last || self.outstanding > 0 {
            return Ok(());
        }
        // note: a resume rather than a fresh attach, so the session sends the records and not a
        // projection: the projection is the thing a client is leaving, and asking for it again
        // would print the whole conversation a second time
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
    /// note: nothing at all until the input has closed, which is what makes this safe to call on
    /// every message: a client with somebody at it answers with `y`, `n` and `a`, and a question
    /// settled from under them would be this deciding what they were about to.
    ///
    /// note: taken off the queue as it is answered rather than when the session says so. The
    /// answer comes back as a `permission.decided` record and clears it there too, but that record
    /// arrives after the next pass through this - which would send a second `Decide` for a
    /// question already answered, and a third, for as long as the session took to reply.
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
                    // from a run whose whole distinguishing feature is that nobody was watching it
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
            // note: the same refusal the bind gives, and said before the connect, because a path
            // this long is refused by the kernel with a sentence naming neither the limit nor a
            // shorter place - and a client that cannot reach the socket is the one person who
            // cannot tell a wrong path from a path nothing is listening at
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
/// note: the same characters the headless run uses where it uses any, and one of its own where it
/// has none, because this prints a whole conversation at once and a column of identical marks down
/// the left of it says nothing about who was talking.
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
