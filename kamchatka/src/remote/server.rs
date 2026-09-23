//! The third loop: a session with a socket in front of it.
//!
//! The loop in `main.rs` draws a frame and waits for a key, the one in `headless.rs` waits for a
//! line, and this one waits for whichever of several clients says something first. Everything
//! between the three is the same [`App`]: `submit` takes the line somebody would have typed,
//! `decide` answers the question a tool is waiting on, `on_event` takes what the kernel says back,
//! and the session, the tools, the policy and the trace are where they always were.
//!
//! note: what a connection needs from this loop is much less than it looks. A [`Kernel`] is a
//! cheap `Arc` handle, so every connection has one of its own and reads the session log directly -
//! which means the numbered half of the stream is not something this loop hands out, queues, or is
//! able to drop. Only the commands that need `&mut App` come through the channel at all - a
//! projection, a line, an interrupt, a decision, a move, an edit, an earlier version of an item -
//! and `apply` says which one does not.
//!
//! note: so there is **no outbound queue per client anywhere in here**, which is the backpressure
//! design. A connection that stops reading stops being written to, its broadcast receivers fall
//! behind, and it is told how many fragments it missed; the records it missed are still in the log,
//! where it goes back for them by sequence. A slow client therefore costs one socket buffer and
//! loses exactly the thing that could not have been recovered anyway.

use std::{sync::Arc, time::Duration};

use nachalnik::{Event, Kernel};
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    sync::{broadcast, mpsc, oneshot},
    task::JoinSet,
};

use crate::{
    app::{App, Outcome, Overlay, Speaker, text},
    remote::protocol::{
        self, Address, Attached, Command, Judged, Line, Listed, Message, Printed, Stanced, Tracing,
        Unjudged,
    },
};

/// How many of the program's own lines a client may fall behind before it starts losing them.
///
/// note: smaller than the kernel's own event queue by a lot, and it can be: a note is not
/// something that streams. What goes through here is what a command answered, what the runtime had
/// to say about a turn, and an error - a handful per turn against a thousand fragments - so a
/// client this far behind on *these* has not been reading for a very long time.
const VOICE: usize = 256;

/// How long an ended session waits for its connections to write what they still owe.
///
/// note: a bound rather than a wait for all of them, because one of them may be a client that
/// stopped reading, and this is what stands between it and a process that never exits. What they
/// owe is the answer to the last command and `session.finished`, which is a few hundred bytes.
const PARTING: Duration = Duration::from_secs(2);

/// A session, and the socket somebody reaches it on.
pub struct Server {
    listener: Listener,
    /// Where the socket file is, when it is one this process made and has to take away again.
    unlink: Option<std::path::PathBuf>,
}

/// Whichever kind of socket this is listening on.
enum Listener {
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
    Tcp(tokio::net::TcpListener),
}

/// Whichever kind of connection just arrived.
enum Incoming {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    Tcp(tokio::net::TcpStream),
}

/// What the session loop says back about one command.
///
/// note: two things, and only an attach has the second. A subscription to the program's own voice
/// has to be taken in the same breath as the projection it goes with, and the session loop is the
/// only place both can happen with nothing in between - see the note on `Attached::seq`, which
/// makes the same argument about the records.
struct Answered {
    /// The message that answers it, where it has one; `None` means the records will carry it.
    message: Option<Message>,
    /// The program's own voice, from this moment on.
    voice: Option<broadcast::Receiver<Arc<Message>>>,
}

/// One thing a client has asked for, waiting for the loop that owns the [`App`] to do it.
///
/// note: opaque, and it is the shape of the seam rather than shyness about the type inside. A
/// `select!` branch may borrow the receiver or the `App` and not both, so waiting and doing are two
/// calls - [`Serving::asked`] and [`Serving::answer`] - and this is what passes between them.
pub struct Asked(FromClient);

/// A connection that has just arrived and has not been taken on yet.
///
/// note: the same shape and for the same reason: [`Server::arrived`] borrows the listener and
/// [`Serving::attend`] borrows the `App`, so they are two calls with this in between.
pub struct Arrived(Incoming);

/// What reaches the session loop from a connection.
enum FromClient {
    /// It asked for something that needs the session itself.
    Asked {
        /// Which connection.
        client: u64,
        /// What it asked for.
        command: Command,
        /// Where the answer goes.
        answer: oneshot::Sender<Answered>,
    },
    /// It has gone.
    Left {
        /// Which connection.
        client: u64,
    },
}

impl Server {
    /// Listens where the address says, and refuses where listening would be a mistake.
    ///
    /// note: there is **no authentication in this protocol**, by decision rather than by omission -
    /// see the module note in [`super`] - so where it listens is the whole of the boundary. A unix
    /// socket's file permissions are that boundary; a loopback port is the machine. Binding
    /// anywhere else hands the `shell` tool to whoever finds the port, so it is refused here
    /// rather than written down as something not to do.
    pub async fn bind(address: &str) -> Result<Self, String> {
        match protocol::address(address)? {
            Address::Unix(path) => Self::unix(path).await,
            Address::Tcp(host) => Self::tcp(host).await,
        }
    }

    /// A socket file, made `0600` the moment it exists.
    ///
    /// note: a stale file is refused rather than removed. Unlinking whatever is in the way is the
    /// convenient thing, and it is how one session silently takes another's socket away from every
    /// client attached to it - so what happens instead is a sentence naming the path, and whoever
    /// knows nothing is using it removes it themselves.
    ///
    /// note: and the sentence says which of the two it found, because they want opposite things
    /// done about them and the refusal is all anybody has to go on. `Drop` is the only thing that
    /// takes the file away, so a `SIGKILL` leaves a path every later `--serve` refuses for ever -
    /// and connecting to it distinguishes "a session is using this" from "nothing is" without
    /// taking anything away from anybody.
    #[cfg(unix)]
    async fn unix(path: &str) -> Result<Self, String> {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::path::PathBuf::from(path);
        if path.exists() {
            return Err(match tokio::net::UnixStream::connect(&path).await {
                Ok(_) => format!(
                    "there is a session listening at {}; attach to it with `--connect unix:{}`",
                    path.display(),
                    path.display()
                ),
                Err(_) => format!(
                    "there is a socket file at {} that nothing is listening on, left behind by a \
                     session that was killed; remove it and try again",
                    path.display()
                ),
            });
        }
        let listener = tokio::net::UnixListener::bind(&path)
            .map_err(|e| format!("could not listen at {}: {e}", path.display()))?;
        // note: after the bind, because there is nowhere earlier - the file is created by the bind
        // itself, under whatever the umask says. The window is one syscall wide, and what closes it
        // properly is the directory the socket is in, which is why the refusal above names the path
        // rather than quietly taking it over
        //
        // a socket that cannot be made private is removed here rather than left to be refused as
        // stale next time: the listener is dropped on the way out without the `Drop` that would
        // remove it
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            let _ = std::fs::remove_file(&path);
            return Err(format!("could not make {} private: {e}", path.display()));
        }

        Ok(Self {
            listener: Listener::Unix(listener),
            unlink: Some(path),
        })
    }

    /// Not here.
    ///
    /// note: `tokio::net::UnixListener` is `cfg(unix)`, and this says so where somebody typed the
    /// address rather than where the crate would otherwise have failed to compile.
    #[cfg(not(unix))]
    async fn unix(path: &str) -> Result<Self, String> {
        Err(format!(
            "`unix:{path}`: this is not a unix, so there are no socket files here - use \
             `tcp:127.0.0.1:PORT`"
        ))
    }

    /// A port, and only ever a loopback one.
    async fn tcp(host: &str) -> Result<Self, String> {
        let address = tokio::net::lookup_host(host)
            .await
            .map_err(|e| format!("`{host}` is nowhere: {e}"))?
            .next()
            .ok_or_else(|| format!("`{host}` is nowhere"))?;
        if !address.ip().is_loopback() {
            return Err(format!(
                "{address} is not a loopback address, and this protocol carries no \
                 authentication: whatever reaches the port runs the `shell` tool as you. Listen on \
                 `tcp:127.0.0.1:PORT` or on `unix:PATH`, and reach it from elsewhere through \
                 something that does authenticate - `ssh -L` is the usual one"
            ));
        }
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|e| format!("could not listen on {address}: {e}"))?;

        Ok(Self {
            listener: Listener::Tcp(listener),
            unlink: None,
        })
    }

    /// Where it is listening, in the spelling a client would type.
    ///
    /// note: asked of the socket rather than remembered from the address, because `tcp:127.0.0.1:0`
    /// is a reasonable thing to ask for and only the socket knows which port it got.
    pub fn address(&self) -> String {
        match &self.listener {
            #[cfg(unix)]
            Listener::Unix(listener) => listener
                .local_addr()
                .ok()
                .and_then(|at| {
                    at.as_pathname()
                        .map(|path| format!("unix:{}", path.display()))
                })
                .unwrap_or_else(|| "unix:?".to_owned()),
            Listener::Tcp(listener) => listener
                .local_addr()
                .map(|at| format!("tcp:{at}"))
                .unwrap_or_else(|_| "tcp:?".to_owned()),
        }
    }

    /// Waits for the next connection, for a loop to hand to [`Serving::attend`].
    ///
    /// note: this and `attend` are the pair a loop that is not [`Server::run`] needs, and they are
    /// two calls because a `select!` branch may borrow the listener or the `App` and not both.
    pub async fn arrived(&self) -> std::io::Result<Arrived> {
        self.accept().await.map(Arrived)
    }

    /// Takes whatever connected next.
    ///
    /// note: a port gets the two options a socket file has no use for; see [`super::tuned`]. It is
    /// done here rather than on the listener because neither of them is inherited - they are
    /// properties of a connection, so every accepted one has to be told.
    async fn accept(&self) -> std::io::Result<Incoming> {
        match &self.listener {
            #[cfg(unix)]
            Listener::Unix(listener) => listener.accept().await.map(|(it, _)| Incoming::Unix(it)),
            Listener::Tcp(listener) => listener.accept().await.map(|(it, _)| {
                super::tuned(&it);

                Incoming::Tcp(it)
            }),
        }
    }

    /// Serves the session until somebody says to stop, and returns when nothing is left in flight.
    ///
    /// note: it does **not** stop when the last client leaves, and that is the invariant the
    /// design is for: a session belongs to the host, not to whoever happens to be looking at it. A
    /// turn carries on with nobody attached, a question waits for somebody to come back and answer
    /// it, and a client picking the session up an hour later picks up the same session.
    pub async fn run(
        &mut self,
        app: &mut App,
        events: &mut broadcast::Receiver<Event>,
        finished: &mut mpsc::UnboundedReceiver<Outcome>,
    ) -> Result<(), String> {
        // note: `App::keys` is **not** set here. It says whether whoever just asked has keys to
        // press, and a client never does, whatever the loop has - so `apply` settles it per
        // command, because a session can be drawn and served at once and the two audiences want
        // different answers out of one `/help`. A loop with no screen leaves the flag alone:
        // nothing local ever asks it anything
        let mut serving = Serving::new(app);
        let mut stopping = false;
        let mut failed = None;
        // subscribed once, because a second press arriving while the first is being handled is the
        // one that means leave; see `crate::stopping`
        let mut presses = crate::stopping::Stopping::new()
            .map_err(|e| format!("could not listen for ctrl+c: {e}"))?;
        let mut terminations = crate::stopping::Terminated::new()
            .map_err(|e| format!("could not listen for a request to end: {e}"))?;

        // set by whichever branch found a reason to stop, rather than each of them breaking where
        // it stands: one of them is nested inside a second `select!`, and a `break` there ends the
        // wrong loop
        let mut leaving = false;

        loop {
            serving.pump(app);
            // a second `ctrl+c` or a closed stream leaves at once; a `/quit` waits for the turn
            if app.leaving() && !leaving {
                failed = app.wait_for_turn(events, finished, |_| {}).await.or(failed);
                break;
            }
            if leaving || (stopping && !app.busy) {
                break;
            }

            tokio::select! {
                incoming = self.arrived() => apply_arrival(&mut serving, app, incoming),
                Some(ask) = serving.asked() => {
                    // note: the command holds the `App` for as long as it takes - there is one of
                    // it - so the loop waits. What it keeps doing meanwhile is reading the
                    // kernel's broadcast, in a second `select!` underneath this one, because that
                    // is the only thing here that *loses* rather than queues: a subscription that
                    // falls behind drops what it did not read. What this loop reads the stream for
                    // is `App::trace`, which is handed to every client that attaches afterwards -
                    // so without this, a `/models` at an endpoint that has gone quiet gives
                    // everybody who arrives later a trace with holes in it, and nothing says so.
                    //
                    // note: the events and nothing else. A connection waits in the listen backlog,
                    // an outcome in an unbounded channel and a `ctrl+c` in its own stream: all
                    // three arrive late either way and none of them is dropped, so taking them
                    // early would buy an ordering no client can tell apart.
                    //
                    // note: what is still *held* is another client's command, because answering
                    // one needs the `App` and the `App` is lent out. That is not a queue this can
                    // add; it is `App::submit` being `&mut self` for the length of a round trip,
                    // and `POSTPONED.md` has what splitting it would take.
                    let mut held = Vec::new();
                    // a block of its own, because the future borrows the `App` until it is
                    // dropped and the drain below is what wants it back
                    {
                        let doing = serving.answer(app, ask);
                        tokio::pin!(doing);
                        loop {
                            tokio::select! {
                                () = &mut doing => break,
                                event = events.recv() => held.push(event),
                            }
                        }
                    }
                    for event in held {
                        leaving |= !apply_event(app, event);
                    }
                },
                event = events.recv() => leaving |= !apply_event(app, event),
                () = presses.pressed() => leaving |= apply_press(app, &mut stopping),
                // taken as `/quit`: the turn is stopped and waited for above
                () = terminations.arrived() => app.quit = true,
                Some(outcome) = finished.recv() => failed = apply_outcome(app, events, outcome),
            }
        }

        // note: the session is ended here rather than by the caller, for the reason `headless.rs`
        // gives: `session.finished` is a record like any other, and a caller that ended it after
        // this returned would have written every record but the last one
        app.kernel.finish();
        serving.last(app).await;

        match failed {
            Some(_) => Err("the last turn failed".to_owned()),
            None => Ok(()),
        }
    }
}

/// Takes on a connection, or says why there is not one; one branch of [`Server::run`]'s loop.
///
/// note: these four are functions rather than bodies in the loop, because the events have a second
/// caller in the drain that catches up after a command held the `App`, and because a branch cannot
/// `break` where it stands. Out of the loop they read as the four things that happen to a served
/// session.
fn apply_arrival(serving: &mut Serving, app: &mut App, incoming: std::io::Result<Arrived>) {
    match incoming {
        Ok(arrived) => serving.attend(app, arrived),
        // one connection failing to arrive is not a reason to end a session that may have a turn
        // running in it
        Err(e) => app.say(Speaker::Error, format!("a client could not connect: {e}")),
    }
}

/// Takes in one thing the kernel said; `false` when the stream has ended and the session with it.
fn apply_event(app: &mut App, event: Result<Event, broadcast::error::RecvError>) -> bool {
    match event {
        Ok(event) => app.on_event(event),
        // note: nothing a client can see is lost here. What this loop is doing with the stream is
        // keeping `App` up to date for the projections it hands out; the clients read the log for
        // themselves, and the log drops nothing
        Err(broadcast::error::RecvError::Lagged(missed)) => app.say(
            Speaker::Note,
            format!(
                "{missed} events went by too fast for this session's own view of itself; the \
                 records have them all"
            ),
        ),
        Err(broadcast::error::RecvError::Closed) => return false,
    }

    true
}

/// Takes in the end of a turn, and hands back what it failed with if it did.
fn apply_outcome(
    app: &mut App,
    events: &mut broadcast::Receiver<Event>,
    outcome: Outcome,
) -> Option<String> {
    // the turn's last events are still queued behind this one, and `select!` picks whichever
    // branch is ready rather than whichever happened first
    while let Ok(event) = events.try_recv() {
        app.on_event(event);
    }
    let failed = match &outcome {
        Outcome::Failed(e) => Some(e.clone()),
        _ => None,
    };
    app.on_outcome(outcome);

    failed
}

/// Takes in a `ctrl+c`; `true` when it is the second one and the session leaves at once.
fn apply_press(app: &mut App, stopping: &mut bool) -> bool {
    // the second one: whatever is still running is somebody else's problem now
    if *stopping {
        return true;
    }
    *stopping = true;
    app.interrupt();
    app.say(
        Speaker::Note,
        "stopping; what has arrived is kept, and again leaves at once",
    );

    false
}

/// The half of a served session that is not a loop.
///
/// note: it exists because there are two loops that can own the [`App`] and either of them may be
/// serving. [`Server::run`] is one - a session with a socket and nothing else - and the terminal's
/// own loop in `main.rs` is the other, which is what lets a session be driven from the desk it is
/// running on and from a phone at the same time. What that costs a loop is [`pump`] before it
/// waits, [`asked`] as a branch to wait on and [`answer`] for what it yields, [`attend`] for each
/// connection, and [`last`], awaited once the session has ended. The loop stays the loop, and this
/// is the part neither should be writing twice.
///
/// [`pump`]: Serving::pump
/// [`asked`]: Serving::asked
/// [`answer`]: Serving::answer
/// [`attend`]: Serving::attend
/// [`last`]: Serving::last
pub struct Serving {
    /// What every attached client hears the program say.
    voice: broadcast::Sender<Arc<Message>>,
    /// The end each connection puts its commands into, cloned into every one of them.
    asks: mpsc::UnboundedSender<FromClient>,
    /// The end the loop takes them out of.
    asked: mpsc::UnboundedReceiver<FromClient>,
    /// How many of the program's own lines have gone out; see [`App::notes`] for why it counts the
    /// filtered sequence rather than the list.
    said: usize,
    /// Which generation of that sequence, because `/cleanup` starts it again from nothing.
    cleared: u64,
    /// Whether the session was busy the last time anybody was told.
    announced: bool,
    /// Which model it was talking to then, for the same reason and a worse one; see
    /// [`Message::Model`].
    model: Option<nachalnik::ModelInfo>,
    /// How many connections have arrived, which is what names them.
    clients: u64,
    /// The connections, so that the session can wait for them on the way out; see
    /// [`Serving::last`].
    connections: JoinSet<()>,
}

impl Serving {
    /// One, for a session that is about to start answering clients.
    pub fn new(app: &App) -> Self {
        let (voice, _) = broadcast::channel(VOICE);
        let (asks, asked) = mpsc::unbounded_channel();

        Self {
            voice,
            asks,
            asked,
            said: 0,
            cleared: app.cleared(),
            announced: app.busy,
            model: app.kernel.model_info(),
            clients: 0,
            connections: JoinSet::new(),
        }
    }

    /// Says whatever the session has said since the last look, and whatever has changed about it.
    ///
    /// note: called before the loop waits rather than after something happens, because what it is
    /// reading is [`App`] and anything at all may have changed it - a key, a client, a tool, the
    /// model. A loop that broadcast from the places that cause changes would be a list of those
    /// places to keep complete, and it would be wrong the first time somebody added one.
    pub fn pump(&mut self, app: &App) {
        // before the lines, because it is about the ones already sent: `/cleanup` takes the
        // program's own half of every attached client's screen away, and the watermark below goes
        // back to nothing with it. Broadcast rather than answered to whoever asked, for the reason
        // every other notice is - the program has one voice, and a session two people are watching
        // does not clear for one of them
        if self.cleared != app.cleared() {
            self.cleared = app.cleared();
            self.said = 0;
            let _ = self.voice.send(Arc::new(Message::Cleared));
        }
        // the program has one voice and every client hears it, which is most of the difference
        // between a session several people are attached to and several sessions
        let fresh: Vec<_> = app
            .notes(self.said)
            .map(|entry| Message::Said {
                speaker: entry.speaker,
                text: entry.text.clone(),
            })
            .collect();
        self.said += fresh.len();
        for message in fresh {
            let _ = self.voice.send(Arc::new(message));
        }
        // note: on a change and nothing else. What closes the gap between a client asking for
        // something and being told what came of it is the *answer* to its command, which every
        // command has and which carries this same figure - see `Message::Done`. A broadcast that
        // also fired per command would reach every other client as news about a session that had
        // not changed
        if self.announced != app.busy {
            self.announced = app.busy;
            let _ = self.voice.send(Arc::new(Message::Busy {
                busy: self.announced,
            }));
        }
        // and the same rule for the model, which is here because nothing else says it: a switch
        // finishes inside the provider the kernel already holds, so no record is written and a
        // client that did not ask for a fresh projection went on naming the model before it. See
        // `Message::Model`
        //
        // note: asked once rather than once per side of the comparison. `Kernel::model_info` builds
        // a `ModelInfo` each time it is called - two strings and the list of parameters the model
        // takes - and the drawn loop pumps on every frame
        let model = app.kernel.model_info();
        if self.model != model {
            self.model = model.clone();
            let _ = self.voice.send(Arc::new(Message::Model { model }));
        }
    }

    /// The next thing a client wants, as a branch to wait on.
    ///
    /// note: it borrows this and not the [`App`], which is why it is not one call with
    /// [`Serving::answer`]. A `select!` branch holds its borrow for the length of the `select!`, so
    /// a branch that took the `App` would leave no other branch able to touch it.
    pub async fn asked(&mut self) -> Option<Asked> {
        self.asked.recv().await.map(Asked)
    }

    /// Does one of them, and answers whoever asked.
    ///
    /// note: the doing happens here, in the caller's own loop, and it takes the [`App`] with it -
    /// there is one of it, and answering anybody needs it. So a command that awaits an endpoint
    /// holds the loop: `/models` fetches a listing, `/compact` runs a whole pass, and
    /// [`App::submit`] awaits a switch still in flight before it reads the line at all. What that
    /// costs is another client waiting for its turn, and a screen that does not redraw where the
    /// loop is also drawing one.
    ///
    /// note: what it does not cost is anything *lost*. Both loops that call this read the
    /// kernel's broadcast while they wait - see the branch in [`Server::run`] - because a
    /// subscription that falls behind drops what it did not read, and `App::trace` is built from
    /// what this loop read. Everything else that arrives meanwhile queues: a connection in the
    /// listen backlog, an outcome in an unbounded channel, a `ctrl+c` in its own stream.
    ///
    /// note: the waiting is in `POSTPONED.md`, and it is not a queue anybody can add out here. It
    /// is `App::submit` taking `&mut self` for the length of a round trip.
    pub async fn answer(&mut self, app: &mut App, asked: Asked) {
        match asked.0 {
            FromClient::Left { client } => {
                app.trace(
                    "client.left",
                    format!("client {client}; the session carries on"),
                );
            }
            FromClient::Asked {
                client,
                command,
                answer,
            } => {
                // taken before the projection rather than after it, so that a line said between the
                // two would arrive twice rather than not at all. Nothing runs in between today; the
                // order is which way to be wrong if anything ever does
                let voice =
                    matches!(command, Command::Attach { .. }).then(|| self.voice.subscribe());
                let _ = answer.send(Answered {
                    message: apply(app, client, command).await,
                    voice,
                });
            }
        }
    }

    /// Takes on a connection that has just arrived.
    pub fn attend(&mut self, app: &mut App, arrived: Arrived) {
        self.clients += 1;
        // note: the *trace* rather than the conversation. A session somebody else can type into
        // should say when somebody else can type into it - but said through `App::say` it would go
        // into `App::loose`, which is the conversation, which is in every projection handed out
        // afterwards. A browser reconnecting on a flaky link opens a connection a second, and
        // `examples/relay` tells it to with `retry: 1000`, so the chat would fill with arrivals and
        // departures until somebody typed `/cleanup`.
        //
        // The trace is a ring of the last few hundred lines and is where a thing that happens once
        // a second belongs. Nothing is lost: `Attached::trace` carries it, so it is a row on the
        // trace tab of every client
        app.trace("client.attached", format!("client {}", self.clients));
        let (client, kernel, asks) = (self.clients, app.kernel.clone(), self.asks.clone());
        // note: **not** subscribed here. The subscription is taken where the projection is, which
        // is the only place the two can be taken together - see `Answered`. A receiver taken here
        // would also catch a line the projection already carries, and print it twice
        //
        // note: the ones that have finished are let go of here, because a `JoinSet` keeps each
        // until it is asked for it, and a browser reconnecting once a second would otherwise be a
        // list that only grows
        while self.connections.try_join_next().is_some() {}
        match arrived.0 {
            #[cfg(unix)]
            Incoming::Unix(stream) => {
                self.connections.spawn(serve(client, stream, kernel, asks));
            }
            Incoming::Tcp(stream) => {
                self.connections.spawn(serve(client, stream, kernel, asks));
            }
        }
    }

    /// The last of the voice, once the session has ended, and the connections let go of.
    ///
    /// note: nothing else is going to send these. Whoever is still attached is about to find the
    /// socket closed, and these are the lines that say why.
    ///
    /// note: it waits, for up to two seconds, for every connection to write out what it owes and
    /// close. Closing the voice is what tells a connection the session is over, and it flushes the
    /// log on the way out - so this is where a client is sent the answer to its last command and
    /// `session.finished`. The caller is usually about to exit, and the connections are tasks on
    /// its runtime: without the wait, a `/quit` whose answer is still in one when the process goes
    /// reads to the client that typed it as a dropped connection, and it goes looking for a
    /// session that is gone.
    ///
    /// note: the lines are said now and the waiting is what is handed back, so that the future
    /// does not hold the [`App`]. With a screen built in, an `App` cannot be shared across threads,
    /// and a future holding one across an `await` would make [`Server::run`] one nobody could
    /// spawn.
    pub fn last(self, app: &App) -> impl Future<Output = ()> + Send + use<> {
        let last: Vec<_> = app
            .notes(self.said)
            .map(|entry| Message::Said {
                speaker: entry.speaker,
                text: entry.text.clone(),
            })
            .collect();
        for message in last {
            let _ = self.voice.send(Arc::new(message));
        }

        // and a command still waiting on the loop is answered with its refusal rather than kept
        // waiting for a loop that has stopped
        let Self {
            voice,
            asked,
            mut connections,
            ..
        } = self;
        drop((voice, asked));
        async move {
            let _ = tokio::time::timeout(PARTING, async {
                while connections.join_next().await.is_some() {}
            })
            .await;
        }
    }
}

impl Drop for Server {
    /// Takes the socket file away again.
    ///
    /// note: only the one this process made, and only because a socket file that outlives its
    /// listener is a path every later client is refused at and every later `--serve` refuses as
    /// stale. There is nothing to do for a port.
    fn drop(&mut self) {
        if let Some(path) = &self.unlink {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Does one of the things that need the session itself, and says what it did.
///
/// note: every command but one. `Inspect` of what an item says *now* is answered by the connection
/// that asked it, out of a `Kernel` handle of its own, because reading it needs no `App` - and a
/// client reading a four-megabyte tool result should not be something the session stops to do. An
/// earlier version is `App`'s to remember, so that one comes here.
async fn apply(app: &mut App, client: u64, command: Command) -> Option<Message> {
    match command {
        // note: a resume is answered with a `done` rather than with nothing, and the reason is the
        // invariant `Message::Done` states: every command gets exactly one answer. A resume that
        // got none would leave a client with a count of answers owed that never came back down, and
        // the two things that count on it - stdin closing, and the session going quiet - would stop
        // working for the rest of the process. It carries `busy` because that is the other thing a
        // reconnecting client cannot know: a turn may have ended while it was away, and no record
        // says so
        Command::Attach { since, .. } => Some(match since {
            None => Message::Attached(Box::new(project(app))),
            Some(_) => Message::Done {
                about: "attach".to_owned(),
                busy: app.busy,
            },
        }),
        // note: the same projection under a name that does not mean "start again", which is the
        // difference a client cares about. See `Message::Projected`
        Command::Project => Some(Message::Projected(Box::new(project(app)))),
        // note: answered with the projection rather than a bare `done`, because the answer to
        // "move this item" is what the context now says - and the row the client is looking at is
        // in it. Every *other* client hears about it as a `context.changed` record and asks for
        // its own, which is the same round trip it would make anyway
        Command::Cycle { id } => Some(match app.cycle(id) {
            Ok(_) => Message::Projected(Box::new(project(app))),
            Err(error) => Message::Failed {
                about: "cycle".to_owned(),
                error,
            },
        }),
        // note: answered with a projection, which is what `cycle` answers with and for its reason.
        // An edit changes what the item says, what it costs, and therefore what the next request
        // comes to - and a client could work out none of that from the `context.replaced` the
        // stream is about to carry. The other clients get the record and ask for their own.
        //
        // note: a text that changes nothing is a `Done` rather than a projection, because nothing
        // moved: no record, no version page, no checkpoint. Saying so plainly is better than a
        // projection identical to the one the client already had, which reads as an edit that
        // silently did not take
        Command::Revise { id, text } => Some(match app.revise(id, &text, "edited from a client") {
            Ok(true) => Message::Projected(Box::new(project(app))),
            Ok(false) => Message::Done {
                about: "revise".to_owned(),
                busy: app.busy,
            },
            Err(error) => Message::Failed {
                about: "revise".to_owned(),
                error,
            },
        }),
        Command::Submit { line } => {
            // note: read before the line goes in, because handing one in is what replaces it.
            // There is room for exactly one queued message, so a second client typing during a turn
            // silently takes the first one's place - see `App::queued`. Saying so is the least this
            // can do about it, and it is said to everybody, because the person who lost a line is
            // the one who is not asking
            let replacing = app.queued().map(str::to_owned);
            // note: a client has no keys of this program's to press, whatever the loop driving the
            // session has, so `App::keys` is set around the one call that reads it rather than once
            // for the session. A served session can have a screen as well: a `/help` typed at the
            // desk wants the key pages, and the same `/help` sent from a browser would be a
            // reference to a program the reader is not using. The flag is a fact about whoever just
            // asked. See `App::help`
            let keys = std::mem::replace(&mut app.keys, false);
            let reply = app.submit(&line).await;
            app.keys = keys;
            if let Some(lost) = replacing {
                app.say(
                    Speaker::Note,
                    format!(
                        "client {client}'s line replaced one that was already waiting for this \
                         turn to end: `{}`",
                        text::one_line(&lost)
                    ),
                );
            }

            Some(Message::Replied {
                did: reply.did,
                page: reply.page.map(|overlay| match overlay {
                    Overlay::Text { title, pages, .. } => Printed { title, pages },
                }),
                busy: app.busy,
            })
        }
        Command::Interrupt => {
            app.interrupt();
            Some(Message::Done {
                about: "interrupt".to_owned(),
                busy: app.busy,
            })
        }
        // note: applied on sight, exactly as the keys apply one. The window an answer can land in
        // while the turn that asked is still unwinding is closed in `App::on_outcome`, not by a
        // queue here: a person at a terminal answers inside the same window and cannot be asked to
        // be careful about it either, so a guard that only this loop had would be one the keys
        // went without
        Command::Decide {
            id,
            grant,
            remember,
        } => {
            // note: `app.busy` read *after* the decision rather than before, because answering the
            // last outstanding question is what starts the next turn - so a figure taken first
            // would tell the client the session was quiet in the one moment it had just stopped
            // being so
            match app.decide(id, grant, remember) {
                Ok(()) => Some(Message::Done {
                    about: "decide".to_owned(),
                    busy: app.busy,
                }),
                Err(error) => Some(Message::Failed {
                    about: "decide".to_owned(),
                    error,
                }),
            }
        }
        // note: only a question about an *earlier* version reaches here. What an item says now
        // is answered by the connection off the kernel, without troubling this loop at all - so
        // `None` here is that one, already answered. The viewer is what keeps the earlier
        // versions, and the viewer is this side of the channel
        Command::Inspect { id, raw, version } => version.map(|at| match app.version(id, at) {
            // note: the text, and no `stored` reading of it. What `stored` puts above an item is
            // why it is here and what its turn was thinking, which are facts about the item as
            // it stands rather than about what it used to say - printed over an old version they
            // would be dated wrongly, and confidently
            Some(was) => Message::Item {
                id,
                body: was.to_text().into_owned(),
                raw,
                version,
            },
            None => Message::Failed {
                about: "inspect".to_owned(),
                error: match app.versions(id) {
                    0 => format!("item {id} has not been rewritten, so it has no version {at}"),
                    kept => format!("item {id} has {kept} earlier version(s), not {at}"),
                },
            },
        }),
        // note: answered rather than left to close the connection, because a client that sent
        // something is owed one answer whether or not this end knows what it was - see
        // `Message::Done`. What reaches here is a client newer than this session
        Command::Unknown => Some(Message::Failed {
            about: "unknown".to_owned(),
            error: format!(
                "this session speaks version {} of the protocol and has no such command",
                protocol::VERSION
            ),
        }),
    }
}

/// What the advisor made of the questions waiting, for the client that has to draw them.
///
/// note: read at projection time out of what the advisor wrote down while the verdict was being
/// worked out, which is the same moment and the same reading the terminal's panel takes - see
/// [`App::rating`]. The kernel awaits the policy before it raises a question, so by the time a
/// question is in a projection its rating is either already there or was never coming, and nothing
/// on the wire has to describe a request in flight.
#[cfg(feature = "shell-advisor")]
fn rated(app: &App, asking: &[nachalnik::PermissionRequest]) -> Vec<Judged> {
    asking
        .iter()
        .filter_map(|request| Some(Judged::of(request.id, app.rating(request)?)))
        .collect()
}

/// The same where the ratings are not in the build, which is nothing to send.
#[cfg(not(feature = "shell-advisor"))]
fn rated(_: &App, _: &[nachalnik::PermissionRequest]) -> Vec<Judged> {
    Vec::new()
}

/// Why the advisor has no rating for the questions waiting that it was asked about.
#[cfg(feature = "shell-advisor")]
fn unrated(app: &App, asking: &[nachalnik::PermissionRequest]) -> Vec<Unjudged> {
    asking
        .iter()
        .filter_map(|request| {
            Some(Unjudged {
                id: request.id,
                why: app.why_unrated(request)?,
            })
        })
        .collect()
}

/// The same where the ratings are not in the build.
#[cfg(not(feature = "shell-advisor"))]
fn unrated(_: &App, _: &[nachalnik::PermissionRequest]) -> Vec<Unjudged> {
    Vec::new()
}

/// Where the session stands, in the form a client can start rendering from.
fn project(app: &App) -> Attached {
    // note: first, before anything it is the watermark for. A turn runs on a task of its own and
    // goes on changing the kernel while this reads it, so a record written between reading the
    // items and reading the number would be in neither the projection nor the stream after it -
    // and one of those is a permission question, which a client that never sees it cannot answer.
    // Taken first, such a record is in both instead, which every client here reads as the same
    // thing twice
    let seq = app.kernel.last_seq();
    let items = app.kernel.items();
    let going = app.going();
    let asking = app.kernel.pending_permissions();

    Attached {
        rated: rated(app, &asking),
        unrated: unrated(app, &asking),
        version: protocol::VERSION,
        seq,
        session: app.kernel.session_name(),
        state: app.kernel.state(),
        busy: app.busy,
        stepping: app.stepping,
        model: app.kernel.provider().map(|provider| provider.info()),
        budget: app.kernel.budget(),
        spent: app.spent(),
        spend: app.spend(),
        overspent: app.overspent(),
        conversation: app
            .conversation(&items, &going)
            .iter()
            .map(Line::of)
            .collect(),
        // note: every item, rather than `App::listed`, which is the *screen's* list and leaves out
        // what has been pruned when somebody has asked it to. Which rows to show is a decision
        // belonging to whoever is reading, and a projection that had already made it would be one
        // client's view of the context standing in for the context
        items: items
            .iter()
            .map(|item| Listed::of(item, &going, app.versions(item.id)))
            .collect(),
        asking,
        trace: tracing(app),
        policy: app.policy_name(),
        untold: crate::tools::Careful::untold(),
        permissions: app.permissions().iter().map(Stanced::of).collect(),
        undecided: app.undecided(),
        queued: app.queued().map(str::to_owned),
        confinement: app.confinement(),
    }
}

/// One connection, from the moment it arrives to the moment it goes.
async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    client: u64,
    stream: S,
    kernel: Kernel,
    asks: mpsc::UnboundedSender<FromClient>,
) {
    let (read, mut write) = tokio::io::split(stream);
    let mut frames = protocol::Frames::new(BufReader::new(read));
    if let Err(e) = attend(client, &mut frames, &mut write, &kernel, &asks).await {
        // the connection is going either way; this is the last thing it is told, and it is written
        // on a best-effort basis because the usual way to be here is that it stopped listening
        //
        // note: and there is a second way, which is the one the cap exists for. A peer that is
        // still sending when this closes leaves data in the receive buffer nobody read, and TCP
        // answers a close like that with a reset - which on some platforms discards what the peer
        // had already been sent, this sentence among it. Draining first would deliver it and is
        // exactly what `MAX_LINE` refuses to do: not reading a peer that floods is the point, so
        // the sentence is the thing that gives. What a client can rely on is that the connection
        // ends, not that it is told why
        let _ = protocol::write(
            &mut write,
            &Message::Failed {
                about: "the connection".to_owned(),
                error: e,
            },
        )
        .await;
    }
    let _ = asks.send(FromClient::Left { client });
}

/// The connection proper, up to whatever ends it.
///
/// note: the two streams a client reads are both taken from a [`Kernel`] handle of this
/// connection's own, and neither is handed to it by the session loop. The numbered one is the log,
/// read with `history_since` from wherever this client says it had got to, which is why a client
/// that falls behind, lags its subscription or loses its socket cannot lose a record: the log is
/// not a queue and nothing drains it. The unnumbered one is the subscription, which carries the
/// fragments - and doubles as the thing that says it is worth looking at the log again.
async fn attend<R, W>(
    client: u64,
    frames: &mut protocol::Frames<BufReader<R>>,
    write: &mut W,
    kernel: &Kernel,
    asks: &mpsc::UnboundedSender<FromClient>,
) -> Result<(), String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // note: subscribed *before* the first look at the log rather than after, so that an event
    // landing between the two is a duplicate wakeup rather than a record nobody went back for
    let mut events = kernel.subscribe();
    // note: whether the fragments are in the log as well. With `record_progress` on they are
    // records, so sending them from here too would put every fragment on the wire twice - once
    // numbered and once not - and a client assembling an answer out of both would read it double
    let progress_recorded = kernel.config().record_progress;

    // note: a connection says where it stands before it is told anything, and nothing else is
    // accepted first. Streaming at a client that has not said what it already has is how a resume
    // becomes a replay
    let settled = match protocol::read::<Command>(frames).await? {
        None => return Ok(()),
        Some(attach @ Command::Attach { .. }) => {
            watermark(attach, kernel, asks, client, write).await
        }
        Some(other) => {
            return Err(format!(
                "`{}` before `attach`: a connection says where it stands first",
                name(&other)
            ));
        }
    };
    // note: reported as the attach's failure rather than the connection's, because a client that
    // can tell a refused watermark from a broken socket has something to do about it - come back
    // with none. Told only that the connection failed, it would read the close as a drop and retry
    // the same impossible resume until it gave up
    let (mut last, mut voice) = match settled {
        Ok(settled) => settled,
        Err(refused) => return refuse(write, refused.about, refused.error).await,
    };
    flush(kernel, &mut last, write).await?;

    loop {
        tokio::select! {
            command = protocol::read::<Command>(frames) => match command? {
                None => return Ok(()),
                // note: re-attaching on a live connection is allowed, and is the cheapest way for a
                // client that has confused itself to start again: it asks for the projection and
                // moves its own watermark to whatever that says
                Some(attach @ Command::Attach { .. }) => {
                    let since = matches!(attach, Command::Attach { since: None, .. });
                    let settled = watermark(attach, kernel, asks, client, write).await;
                    let (at, fresh) = match settled {
                        Ok(settled) => settled,
                        Err(refused) => return refuse(write, refused.about, refused.error).await,
                    };
                    last = at;
                    // note: the subscription is swapped exactly where a projection is handed over,
                    // because that is what they have to be taken together for. A resume on a live
                    // connection is answered with no projection, so the one it already has is
                    // holding lines nothing else would bring back
                    if since {
                        voice = fresh;
                    }
                    flush(kernel, &mut last, write).await?;
                }
                // answered here rather than by the session loop: it needs a `Kernel` and nothing
                // else, and the session has better things to be doing
                //
                // note: what an item says *now*, which is the kernel's. An earlier version is not
                // - the viewer keeps those, and the viewer is the `App` - so a question about one
                // falls through to the branch below and is answered where they are. Reading them
                // here would mean the session holding what it has already given away
                Some(Command::Inspect {
                    id,
                    raw,
                    version: None,
                }) => {
                    let message = match kernel.items().iter().find(|item| item.id == id) {
                        // the item's own text where a client is about to put it in front of
                        // somebody to edit, and the reading of it where somebody is going to read
                        // it. `Kernel::replace` writes content, so the reading is the one thing
                        // that must never come back as an edit
                        Some(item) => Message::Item {
                            id,
                            body: match raw {
                                true => item.content.to_text().into_owned(),
                                false => text::stored(item),
                            },
                            raw,
                            version: None,
                        },
                        None => Message::Failed {
                            about: "inspect".to_owned(),
                            error: format!("there is no item {id} in the context"),
                        },
                    };
                    answer(write, &message, "inspect").await?;
                }
                Some(command) => {
                    let about = name(&command);
                    match ask(asks, client, command).await.map(|it| it.message) {
                        // note: caught up first, for the reason `Message::Busy` is: a reply carries
                        // `busy`, and a client whose input has closed leaves on `busy: false` - so
                        // `/note keep this` piped in and answered ahead of its `context.added` was
                        // a client gone before the record of what it had just done
                        Ok(Some(message)) => {
                            caught_up(&mut events, kernel, &mut last, write, progress_recorded)
                                .await?;
                            answer(write, &message, about).await?
                        }
                        Ok(None) => {}
                        Err(error) => {
                            protocol::write(write, &Message::Failed {
                                about: about.to_owned(),
                                error,
                            })
                            .await?;

                            return Ok(());
                        }
                    }
                }
            },
            event = events.recv() => match event {
                Ok(event) => {
                    // the numbered half first, always. A fragment names the last record the client
                    // was sent, so one written before that record had gone out would be naming a
                    // number nobody had seen
                    //
                    // note: the cost is that a connection behind the broadcast can be handed a
                    // fragment *after* the record that ends the thing it was part of, because the
                    // flush reads wherever the log has got to rather than wherever it had got to
                    // when the fragment was emitted. Reconstructing the true interleaving would
                    // mean flushing one record per non-progress event - exact, and a linear scan
                    // of the log per event, per client. It is not worth it: a fragment whose item
                    // has already arrived is a fragment the item supersedes, and every client in
                    // this workspace already drops one on those grounds. The terminal does it
                    // under the name `Entry::transient`
                    flush(kernel, &mut last, write).await?;
                    if protocol::is_progress(&event) && !progress_recorded {
                        protocol::write(write, &Message::Progress { after: last, event }).await?;
                    }
                }
                // note: this is the only loss a client is exposed to, and it is exactly the loss
                // that cannot be helped: the fragments are not in the log, so there is nothing to
                // go back for. The records are untouched - `flush` reads them out of the log, which
                // nothing drains - so what this number means is "you missed some of the typing",
                // and never "you missed something that happened"
                Err(broadcast::error::RecvError::Lagged(frames)) => {
                    flush(kernel, &mut last, write).await?;
                    protocol::write(write, &Message::Missed { frames }).await?;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            said = voice.recv() => match said {
                // note: the numbered half first, for the reason the event branch above gives and
                // for a sharper one. `Message::Busy` is how a client learns a turn is over, and a
                // client whose input has closed takes that at its word and leaves - so a
                // `busy: false` written *before* the records of the turn it is about says the turn
                // is done while the last of it is still in the log, and `kamchatka --connect` with
                // a question piped into it detaches without the answer
                Ok(message) => {
                    caught_up(&mut events, kernel, &mut last, write, progress_recorded).await?;
                    protocol::write(write, &*message).await?;
                }
                // the program's own lines are in no log either, and the same rule applies to them
                Err(broadcast::error::RecvError::Lagged(frames)) => {
                    protocol::write(write, &Message::Missed { frames }).await?;
                }
                // note: the session has ended, and this is the last thing this connection does:
                // write out whatever the log grew while it was being written to. Which branch sees
                // the end first is a coin toss - `session.finished` is emitted before this channel
                // is dropped, so both are ready at once and `select!` picks either - and what the
                // toss costs is a client left short of the last records of a session it has no
                // socket left to go back for them on.
                //
                // note: no test fails without this, because in practice the event branch is ready
                // first. The race is real and rare, and the flush is kept on those terms
                Err(broadcast::error::RecvError::Closed) => {
                    flush(kernel, &mut last, write).await?;

                    return Ok(());
                }
            },
        }
    }
}

/// A refusal, and which of the client's commands to name it as.
///
/// note: two names for what one function refuses, because the two are not the same news. An
/// `attach` refusal is mended by attaching afresh, which is what a client does with it; a `version`
/// refusal is not mended by anything, and a client that treats it the same way reattaches, is
/// refused identically, and gives up a minute later saying the session has not answered, when it
/// answered at once. See [`crate::remote::Client`].
struct Refused {
    /// The command to name it as: `attach`, or `version`.
    about: &'static str,
    /// What went wrong.
    error: String,
}

impl From<String> for Refused {
    /// Anything else that stops an attach is the attach's.
    fn from(error: String) -> Self {
        Self {
            about: "attach",
            error,
        }
    }
}

/// Settles where this client's numbered stream starts, and sends the projection if it needs one.
///
/// note: the subscription to the program's own voice comes back with the watermark, because the
/// session loop is where both are taken and it takes them together. See [`Answered`].
async fn watermark<W: AsyncWrite + Unpin>(
    attach: Command,
    kernel: &Kernel,
    asks: &mpsc::UnboundedSender<FromClient>,
    client: u64,
    write: &mut W,
) -> Result<(u64, broadcast::Receiver<Arc<Message>>), Refused> {
    let Command::Attach {
        since,
        session,
        version,
    } = attach
    else {
        return Err("that is not an attach".to_owned().into());
    };
    // note: a version this session does not know is refused before anything else is read off the
    // message, because what the rest of it means is the thing in question. An older one it does
    // know is served - see `protocol::VERSION`
    let spoken = version.unwrap_or(1);
    if spoken > protocol::VERSION {
        return Err(Refused {
            about: "version",
            error: format!(
                "you speak version {spoken} of this protocol and this session speaks {}; the \
                 older end is this one",
                protocol::VERSION
            ),
        });
    }
    let Some(since) = since else {
        let answered = ask(
            asks,
            client,
            Command::Attach {
                since: None,
                session: None,
                version: None,
            },
        )
        .await?;
        let (Some(Message::Attached(attached)), Some(voice)) = (answered.message, answered.voice)
        else {
            return Err("the session answered an attach with something else"
                .to_owned()
                .into());
        };
        let seq = attached.seq;
        protocol::write(write, &Message::Attached(attached)).await?;

        return Ok((seq, voice));
    };

    // note: the loud half of the same check, and it is first because it is the one that catches
    // the quiet case. A session restarted at this address has a log of its own, and a watermark
    // from the one before it can be perfectly plausible against it - at which point the client
    // draws one session's records under another's conversation with nothing anywhere saying so.
    // A client that does not name a session is not made to; see `Command::Attach`
    let named = kernel.session_name();
    if session.is_some_and(|session| session != named) {
        return Err(format!(
            "you are resuming a session this is not: this one is `{named}`, and attaching with no \
             `since` starts again here"
        )
        .into());
    }
    // note: a client claiming to have seen more than has happened is refused rather than clamped.
    // It is either a client that has confused two sessions or one that made the number up, and
    // quietly starting it from the end would leave it convinced it held a history it never had
    let last = kernel.last_seq();
    if since > last {
        return Err(format!(
            "this session has {last} record(s) and you say you have {since}; attach with no \
             `since` to start again"
        )
        .into());
    }
    // note: refused above without troubling the session, and answered here by the session itself,
    // because the answer carries `busy` and nothing but the loop driving the kernel knows it
    let answered = ask(
        asks,
        client,
        Command::Attach {
            since: Some(since),
            session: None,
            version: None,
        },
    )
    .await?;
    let (Some(message), Some(voice)) = (answered.message, answered.voice) else {
        return Err("the session answered an attach with nothing"
            .to_owned()
            .into());
    };
    // note: what was missed before the answer, because the answer carries `busy` and a client
    // whose input has closed leaves on `busy: false` - the rule `Message::Busy` and a command's
    // reply are held to. Written after, a client coming back to collect the end of an answer was
    // told the session was resting and left before the records it had come back for
    let mut last = since;
    flush(kernel, &mut last, write).await?;
    protocol::write(write, &message).await?;

    Ok((last, voice))
}

/// Says a command could not be done, and ends the connection on it.
///
/// note: named rather than reported as the connection's, so that a failure a client can do
/// something about reads as itself - and named one of two ways, because what there is to do about
/// the two differs. See [`Refused`] and the first attach in [`attend`].
async fn refuse<W: AsyncWrite + Unpin>(
    write: &mut W,
    about: &str,
    error: String,
) -> Result<(), String> {
    protocol::write(
        write,
        &Message::Failed {
            about: about.to_owned(),
            error,
        },
    )
    .await
}

/// Puts one command to the session loop and waits for what it says.
async fn ask(
    asks: &mpsc::UnboundedSender<FromClient>,
    client: u64,
    command: Command,
) -> Result<Answered, String> {
    let (answer, answered) = oneshot::channel();
    asks.send(FromClient::Asked {
        client,
        command,
        answer,
    })
    .map_err(|_| "the session has ended".to_owned())?;

    answered
        .await
        .map_err(|_| "the session did not answer".to_owned())
}

/// Writes out everything the session has already emitted, numbered and not.
///
/// note: what this is for is `Message::Busy`. A client with its input closed leaves when the
/// session goes quiet, and `busy: false` is how it finds out - so that message overtaking the
/// fragments of the turn it is about is a client detaching without the answer. The records cannot
/// stand in for them: the log names what happened and does not copy it, so `context.added` says an
/// assistant turn exists and not one word of what it said. What the model actually *said* reaches a
/// client only as `Message::Progress`, and only if it is written first.
///
/// note: neither end is wrong without this. The session records the answer and stops the turn, and
/// the client leaves when it is told the session has nothing left to do; the fault is only in the
/// order they are written in.
async fn caught_up<W: AsyncWrite + Unpin>(
    events: &mut broadcast::Receiver<Event>,
    kernel: &Kernel,
    last: &mut u64,
    write: &mut W,
    progress_recorded: bool,
) -> Result<(), String> {
    loop {
        match events.try_recv() {
            Ok(event) => {
                flush(kernel, last, write).await?;
                if protocol::is_progress(&event) && !progress_recorded {
                    let after = *last;
                    protocol::write(write, &Message::Progress { after, event }).await?;
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(frames)) => {
                flush(kernel, last, write).await?;
                protocol::write(write, &Message::Missed { frames }).await?;
            }
            // nothing waiting, or the session has ended and the log is the last word either way
            Err(_) => return flush(kernel, last, write).await,
        }
    }
}

/// Writes the answer to a command, or says why it cannot be sent.
///
/// note: the rule [`flush`] holds records to, for the answers that carry content. An `inspect` of
/// an item past `MAX_LINE` is a frame the client refuses, and refusing one closes the connection -
/// and `inspect` is how the protocol tells a client to get past an `Oversized` record, so without
/// this the way out would be a way off the session.
async fn answer<W: AsyncWrite + Unpin>(
    write: &mut W,
    message: &Message,
    about: &str,
) -> Result<(), String> {
    let line = protocol::framed(message)?;
    match protocol::overlong(&line) {
        Some(bytes) => {
            protocol::write(
                write,
                &Message::Failed {
                    about: about.to_owned(),
                    error: format!(
                        "the answer is {bytes} bytes, more than a client reads in one line ({})",
                        protocol::MAX_LINE
                    ),
                },
            )
            .await
        }
        None => protocol::write_frame(write, &line).await,
    }
}

/// Writes out every record the session has grown since this client last saw one.
///
/// note: the log rather than the subscription, which is why a client cannot lose one. A broadcast
/// has a capacity and drops for whoever falls behind it; the log has neither, so this is a read
/// from wherever this connection had got to and it is correct however long the connection was
/// asleep, however far behind its subscription fell, and whether or not it was here at all when
/// the record was written.
async fn flush<W: AsyncWrite + Unpin>(
    kernel: &Kernel,
    last: &mut u64,
    write: &mut W,
) -> Result<(), String> {
    // note: asked first because it is a read of one number, and `history_since` is a walk of the
    // whole log under the lock every emit waits on. This runs for every event, `model.delta`
    // included, for every connection - and most of those events add no record this connection
    // has not already been sent
    if kernel.last_seq() <= *last {
        return Ok(());
    }
    for record in kernel.history_since(*last) {
        let seq = record.seq;
        let line = protocol::framed(&Message::Record(record))?;
        // note: a record the other end would refuse to read is named rather than sent, and the
        // naming carries its sequence - so the client takes it as seen and resumes after it. Sent,
        // it is a frame over `MAX_LINE`, which closes the connection; and because a client resumes
        // by sequence it would come straight back to the same record on every attempt, and one
        // `context.replaced` over the limit would lock everybody out for the rest of the session.
        // See `Message::Oversized`
        match protocol::overlong(&line) {
            Some(bytes) => protocol::write(write, &Message::Oversized { seq, bytes }).await?,
            None => protocol::write_frame(write, &line).await?,
        }
        *last = seq;
    }

    Ok(())
}

/// The trace as the pane draws it, with the gap between lines worked out the pane's way.
///
/// note: the whole ring rather than what a search left, because `App::traced` answers for a screen
/// with a query typed into it and a client's view is its own. A filter is the client's to apply.
///
/// note: the gap is computed here rather than sent as two timestamps, because the rule for when
/// there *is* one is a decision rather than a format: nothing under a tenth of a second, and
/// nothing after a line that ended a wait for a person - however long somebody took to answer a
/// question, that is not a step this program spent. A client left to work that out would get a
/// column whose largest figure is how long the operator was thinking, which is the one number in
/// there nobody should act on.
fn tracing(app: &App) -> Vec<Tracing> {
    let mut before = None;
    app.trace
        .iter()
        .map(|event| {
            let gap = match before.replace(event.at) {
                Some(previous) if !event.after_a_person => {
                    text::waited_since(event.at.saturating_duration_since(previous))
                }
                _ => None,
            };

            Tracing {
                name: event.name.clone(),
                detail: event.detail.clone(),
                gap,
                at: event
                    .wall
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|since| since.as_millis() as u64)
                    .unwrap_or(0),
            }
        })
        .collect()
}

/// What a command is called, for the message that says it could not be done.
fn name(command: &Command) -> &'static str {
    match command {
        Command::Attach { .. } => "attach",
        Command::Submit { .. } => "submit",
        Command::Interrupt => "interrupt",
        Command::Decide { .. } => "decide",
        Command::Inspect { .. } => "inspect",
        Command::Project => "project",
        Command::Cycle { .. } => "cycle",
        Command::Revise { .. } => "revise",
        Command::Unknown => "unknown",
    }
}

/// What a session about to be driven from somewhere else should say for itself.
///
/// note: through [`App::say`] for the reason the headless opening gives: it is addressed to whoever
/// is reading rather than to the model, so it goes where a `/save` cannot mistake it for something
/// the model was told - and every client that attaches later reads it in the conversation, which is
/// where somebody arriving wants to find out what they have joined.
pub fn opening(app: &mut App, address: &str) {
    app.say(
        Speaker::Note,
        format!(
            "serving on {address}: the session is this program's rather than any client's, so it \
             carries on when they leave, and waits when a tool needs an answer nobody is here to \
             give"
        ),
    );
}
