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

use std::{collections::VecDeque, io::Write, time::Duration};

use nachalnik::{ContextId, Delta, Event, Grant, PermissionRequest};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};

use crate::{
    app::{Speaker, text::one_line},
    remote::protocol::{self, Address, Attached, Command, Message},
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
    prose: &'a mut dyn Write,
    /// Whether the prose is part-way through a line somebody else would finish.
    mid_line: bool,
    /// The last record this client is sure it has.
    last: u64,
    /// The session those records came from, where this client has attached to one.
    ///
    /// note: this is what says whether there is anything to resume, as well as what a resume
    /// names: `Some` is a client holding a conversation and a watermark, `None` one with nothing,
    /// and a refused attach puts it back to `None` so that the next attempt is a fresh one. A
    /// client that kept the watermark through a refusal would send the same impossible resume on
    /// every attempt until it gave up on a session that was there all along.
    session: Option<String>,
    /// The questions waiting on somebody, oldest first.
    asking: VecDeque<PermissionRequest>,
    /// What a question is answered with once there is nobody left here to answer it.
    ///
    /// note: the same flag `--headless` reads, and it reaches the same two states for the same
    /// reason: an input that has closed cannot be asked anything. It is this client's rather than
    /// the session's - `--connect` takes nothing else on the command line precisely because
    /// everything else belongs to whoever is serving, and what a *detaching* client does with an
    /// open question does not.
    on_ask: Grant,
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
    /// Whether the session has said anything on this connection.
    ///
    /// note: what makes [`GIVE_UP`] a minute from the last drop rather than a minute across the
    /// whole of a run. A connection the session answered on was picked back up, and the next time
    /// it goes is a drop of its own. Without this, short outages over a day add up to the minute,
    /// and the next one gives up without a single attempt, saying the session has not answered.
    reached: bool,
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
            records,
            prose,
            mid_line: false,
            last: 0,
            session: None,
            asking: VecDeque::new(),
            busy: false,
            outstanding: 0,
            detaching: false,
            over: false,
            reached: false,
        }
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
        let mut typed = input.lines();
        let (mut first, mut waited, mut wait) = (true, Duration::ZERO, FIRST_WAIT);

        loop {
            let left = self.attend(address, &mut typed, first).await;
            first = false;
            match left {
                Left::Done => return Ok(()),
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
                    self.fresh_line()?;
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
        typed: &mut tokio::io::Lines<impl AsyncBufRead + Unpin>,
        first: bool,
    ) -> Left {
        // note: before the connect rather than after it. An attempt that cannot connect has not
        // been answered either, and one that kept the last connection's `true` would reset the
        // wait on every failure - so a client whose session had gone would try every quarter of a
        // second for as long as the process lived, and never reach `GIVE_UP`
        self.reached = false;
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
                        Ok(()) => match self.settle(&mut write).await {
                            Ok(()) if self.detaching && self.resting() => Some(Left::Done),
                            Ok(()) => None,
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
                    Err(e) => {
                        let _ = self.tell(&format!("the session said something unreadable: {e}"));
                        Some(Left::Dropped)
                    }
                },
                // note: the branch is switched off once the input has closed, rather than reading
                // the end of it for ever. A `select!` arm over a reader at EOF is ready every time
                // round the loop, and this one would have spun on it
                line = typed.next_line(), if !self.detaching => match line {
                    Ok(Some(line)) => match self.typed(&mut write, line.trim_end()).await {
                        Ok(()) => None,
                        Err(_) => Some(Left::Dropped),
                    },
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
                        self.detaching = true;
                        match self.settle(&mut write).await {
                            Ok(()) => self.resting().then_some(Left::Done),
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
                            let _ = self.fresh_line();
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
                let _ = self.fresh_line();

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
                self.fresh_line()?;
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
            // down. A client that wants the figures reads them off it; see `examples/gateway.rs`
            Message::Projected(_) => {
                self.answered();

                Ok(())
            }
            Message::Said { speaker, text } => {
                self.fresh_line()?;
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
                self.fresh_line()?;
                writeln!(self.prose, "--- {} ---", page.title).map_err(|e| e.to_string())?;
                // note: every page rather than the one it opened at, for the reason `--headless`
                // gives: a screen turns them with the arrow keys and there is no key to press down
                // a socket, so a caller handed one page of seven would be reading a reference whose
                // other six it has no way to ask for
                for face in &page.pages {
                    if page.pages.len() > 1 {
                        writeln!(self.prose, "-- {} --", face.name).map_err(|e| e.to_string())?;
                    }
                    writeln!(self.prose, "{}", face.body).map_err(|e| e.to_string())?;
                }
                self.prose.flush().map_err(|e| e.to_string())
            }
            Message::Item { id, body, .. } => {
                self.answered();
                self.fresh_line()?;
                writeln!(self.prose, "--- item {id} ---\n{body}").map_err(|e| e.to_string())?;
                self.prose.flush().map_err(|e| e.to_string())
            }
            Message::Busy { busy } => {
                self.busy = busy;

                Ok(())
            }
            // note: said rather than swallowed, because it is the one change to a session that
            // nothing else here would show. This client prints the conversation and the records,
            // and a model switch is in neither - so without this, a session that changed model
            // under it would read as one that had not
            Message::Model { model } => {
                self.fresh_line()?;
                self.tell(&match model {
                    Some(model) => format!("the model is {}", model.model),
                    None => "there is no model".to_owned(),
                })
            }
            Message::Done { busy, .. } => {
                self.busy = busy;
                self.answered();

                Ok(())
            }
            Message::Missed { frames } => {
                self.fresh_line()?;
                self.tell(&format!(
                    "{frames} fragment(s) went by too fast to print; the records have everything \
                     that happened"
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
                // note: a version refusal is the one failure nothing here can mend. Attaching
                // afresh says the same thing and is refused for the same reason, so a client that
                // retried it would spend a minute on it and leave saying the session had not
                // answered, when it answered at once with the sentence below
                if about == "version" {
                    self.fresh_line()?;

                    return Err(format!("{about}: {error}"));
                }
                // a refused attach is the one failure this client can do something about, and what
                // it does is put down what it was holding: only a fresh attach can succeed now
                if about == "attach" {
                    self.session = None;
                    self.last = 0;
                }
                self.fresh_line()?;
                self.tell(&format!("{about}: {error}"))
            }
        }
    }

    /// Prints where a session stands, for somebody who has just arrived at it.
    fn arrived(&mut self, attached: &Attached) -> Result<(), String> {
        self.last = attached.seq;
        self.session = Some(attached.session.clone());
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
            writeln!(self.prose, "{} {}", speaks(line.speaker), line.text)
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
        let asking: Vec<_> = self.asking.iter().cloned().collect();
        for request in &asking {
            self.question(request)?;
        }

        self.prose.flush().map_err(|e| e.to_string())
    }

    /// Takes in one thing that happened, whether it came numbered or not.
    ///
    /// note: which of them are printed is the same three `--headless` prints, and the reasoning is
    /// its: the whole trace is in the records, and a session that printed all of it would bury the
    /// answer somebody is waiting for under the forty lines it took to get there. The two that are
    /// extra here are the permission pair, because unlike a headless run there *is* somebody to
    /// ask, and a question nobody printed is a session that has silently stopped.
    fn happened(&mut self, event: &Event) -> Result<(), String> {
        if let Event::ModelDelta {
            delta: Delta::Text(text),
        } = event
        {
            self.mid_line = !text.ends_with('\n');

            return write!(self.prose, "{text}")
                .and_then(|()| self.prose.flush())
                .map_err(|e| e.to_string());
        }

        match event {
            Event::ToolRequested { tool, args, .. } => {
                self.fresh_line()?;
                writeln!(self.prose, "⟩ {tool}({})", one_line(&args.to_string()))
                    .map_err(|e| e.to_string())?;
            }
            Event::ToolFinished {
                tool,
                is_error,
                tokens,
                ..
            } => {
                self.fresh_line()?;
                writeln!(
                    self.prose,
                    "· {tool}: {tokens} tokens{}",
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
                self.fresh_line()?;
                self.tell(&format!("{tool}: {grant}"))?;
            }
            Event::SessionFinished => self.over = true,
            _ => {}
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
        if let Some(request) = self.asking.front() {
            let answer = match line {
                "y" => Some((Grant::Allow, false)),
                "n" => Some((Grant::Deny, false)),
                "a" => Some((Grant::Allow, true)),
                _ => None,
            };
            if let Some((grant, remember)) = answer {
                let id = request.id;

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

        protocol::write(write, &command).await
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
    fn resting(&self) -> bool {
        !self.busy && self.outstanding == 0 && self.asking.is_empty()
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

        while let Some(request) = self.asking.pop_front() {
            self.fresh_line()?;
            self.tell(&format!(
                "nobody is here to answer for `{}`, so it is answered `{}`",
                request.tool, self.on_ask
            ))?;
            self.say_to(
                write,
                Command::Decide {
                    id: request.id,
                    grant: self.on_ask,
                    // note: never. A standing rule outlives this client and this turn, and a rule
                    // nobody typed is the one kind the policy should not learn from - least of all
                    // from a run whose whole distinguishing feature is that nobody was watching it
                    remember: false,
                },
            )
            .await?;
        }

        Ok(())
    }

    /// Ends whatever half-written line the model left, so a whole one can follow it.
    fn fresh_line(&mut self) -> Result<(), String> {
        if std::mem::take(&mut self.mid_line) {
            writeln!(self.prose).map_err(|e| e.to_string())?;
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
async fn connect(address: &str) -> Result<Connection, String> {
    match protocol::address(address)? {
        #[cfg(unix)]
        Address::Unix(path) => tokio::net::UnixStream::connect(path)
            .await
            .map(Connection::Unix)
            .map_err(|e| format!("could not reach {path}: {e}")),
        #[cfg(not(unix))]
        Address::Unix(path) => Err(format!(
            "`unix:{path}`: this is not a unix, so there are no socket files here"
        )),
        Address::Tcp(host) => tokio::net::TcpStream::connect(host)
            .await
            .map(|stream| {
                // the two options a socket file never needed; see `remote::tuned`
                super::tuned(&stream);

                Connection::Tcp(stream)
            })
            .map_err(|e| format!("could not reach {host}: {e}")),
    }
}

/// Whichever kind of connection this is.
///
/// note: an enum implementing the two traits by hand rather than a `Box<dyn>`, because the pair of
/// them is not object-safe together in a form `tokio::io::split` will take.
enum Connection {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    Tcp(tokio::net::TcpStream),
}

/// Dispatches one method over whichever kind of connection it is.
macro_rules! either {
    ($self:ident, $it:ident => $call:expr) => {
        match std::pin::Pin::into_inner($self) {
            #[cfg(unix)]
            Connection::Unix($it) => {
                let $it = std::pin::Pin::new($it);
                $call
            }
            Connection::Tcp($it) => {
                let $it = std::pin::Pin::new($it);
                $call
            }
        }
    };
}

impl AsyncRead for Connection {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        either!(self, it => it.poll_read(cx, buf))
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        either!(self, it => it.poll_write(cx, buf))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        either!(self, it => it.poll_flush(cx))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        either!(self, it => it.poll_shutdown(cx))
    }
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
