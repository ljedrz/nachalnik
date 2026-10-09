//! The third loop: a session with a socket in front of it.
//!
//! The loop in `main.rs` draws a frame and waits for a key, the one in `headless.rs` waits for a
//! line, and this one waits for its client - one at a time - to say something. Everything
//! between the three is the same [`App`]: `submit` takes the line somebody would have typed,
//! `decide` answers the question a tool is waiting on, `on_event` takes what the kernel says back,
//! and the session, the tools, the policy and the trace are where they always were.
//!
//! note: what a connection needs from this loop is much less than it looks. Every connection holds
//! a [`Kernel`](nachalnik::Kernel) of its own and reads the session log directly - which is why it
//! is `connection.rs`, and holds no `App` - so only the commands that need `&mut App` come through
//! the channel at all - a projection, a line, an interrupt, a decision, a move, an edit, an earlier
//! version of an item - and `apply` says which one does not. There is no outbound queue per client
//! in here, for the reason [`crate::remote`] gives, and a slow client costs one socket buffer.
//!
//! note: **one client at a time**, and the newest one wins. A connection that attaches takes the
//! session, and whichever had it is told it was replaced and let go of - see `Serving::seated`.
//! Several clients driving one agent is a conversation nobody has designed: every one of them may
//! submit, interrupt and answer questions, and each would be steering the others' turns.
//! And the newest rather than the first, because the ordinary second connection is the *same*
//! client coming back - a laptop that changed access points, a browser tab reconnecting - while
//! the server still holds its old one half-open, which keepalive takes minutes to notice. Refused,
//! that client would be locked out of its own session for longer than it retries.

use std::{collections::HashMap, sync::Arc, time::Duration};

use nachalnik::Event;
use tokio::{
    sync::{Notify, broadcast, mpsc, oneshot},
    task::JoinSet,
    time::MissedTickBehavior,
};

use crate::{
    app::{App, NOTICES, Outcome, Overlay, Speaker, text},
    remote::protocol::{
        self, Address, Attached, Command, Judged, Line, Listed, Message, Printed, Stanced, Tracing,
        Unjudged,
    },
};

mod connection;

use connection::serve;

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

/// How long the listener is left alone after a connection failed to arrive.
///
/// note: the usual failure is this process out of file descriptors, and the listener stays
/// readable while the connection that could not be taken waits in the backlog - so a loop that
/// asked again at once would fail again at once, as fast as it could go, and say so every time.
const RESTING: Duration = Duration::from_secs(1);

/// A session, and the socket somebody reaches it on.
pub struct Server {
    listener: Listener,
    /// Where the socket file is, when it is one this process made and has to take away again.
    ///
    /// note: with the device and inode the bind made, because the path is only a name. A file
    /// removed by hand and bound again by another session is that session's socket.
    unlink: Option<(std::path::PathBuf, u64, u64)>,
    /// The port, when it is one, closed to this process's confined commands while it is served.
    closed: Option<u16>,
    /// When to try again after a connection last failed to arrive; `None` once they have been
    /// arriving for a while. See [`Server::arrived`].
    ///
    /// note: behind a lock because [`Server::arrived`] takes `&self`, for the `select!` it is a
    /// branch of - and a future holding a `Cell` would not be `Send`.
    resting: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// Why [`Server::run`] stopped before somebody said `/quit`, if a signal was the reason; see
    /// [`Server::stopped`].
    stopped: Option<crate::headless::Stop>,
    /// `SIGTERM` and `SIGHUP`, subscribed before the socket exists; see [`Server::terminated`].
    ///
    /// note: behind a lock for the reason `resting` is. Made in [`Server::bind`] rather than in
    /// [`Server::run`], because the socket appearing is what says a session is there to be
    /// reached, and a signal sent between the two ended the process where it stood - status
    /// `SIGTERM` rather than `143`, and a socket file left for every later `--serve` to refuse.
    terminations: tokio::sync::Mutex<crate::stopping::Terminated>,
    /// `ctrl+c`, subscribed before the socket exists for the reason `terminations` is, and taken
    /// by [`Server::run`] for as long as it runs.
    ///
    /// note: subscribed in `run`, a press between the socket appearing and the loop starting met
    /// the default action, and the process died of `SIGINT` with no record written.
    presses: Option<crate::stopping::Stopping>,
}

/// Whichever kind of socket this is listening on.
enum Listener {
    Unix(tokio::net::UnixListener),
    Tcp(tokio::net::TcpListener),
}

/// What the session loop says back about one command.
///
/// note: an attach has more than the answer. A subscription to the program's own voice has to be
/// taken in the same breath as the projection it goes with, and the session loop is the only place
/// both can happen with nothing in between - see the note on `Attached::seq`, which makes the same
/// argument about the records.
struct Answered {
    /// The message that answers it, where it has one; `None` means the records will carry it.
    message: Option<Message>,
    /// The program's own voice, from this moment on.
    voice: Option<broadcast::Receiver<Arc<Message>>>,
    /// Where the session stands on what the voice says only when it changes, for a resume.
    ///
    /// note: a resume is answered with no projection, and the model and the questions a running
    /// command is waiting on are in no record - so a client that was away when either changed would
    /// otherwise go on showing the old one, and answer a network question it had never heard of
    /// with a `y` that went to the model as a message. Taken with the subscription, for its reason.
    standing: Vec<Message>,
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
pub struct Arrived(super::Connection);

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
        let terminations = crate::stopping::Terminated::new()
            .map_err(|e| format!("could not listen for a request to end: {e}"))?;
        let presses = crate::stopping::Stopping::new()
            .map_err(|e| format!("could not listen for ctrl+c: {e}"))?;
        let mut server = match protocol::address(address)? {
            Address::Unix(path) => Self::unix(path, terminations).await,
            Address::Tcp(host) => Self::tcp(host, terminations).await,
        }?;
        server.presses = Some(presses);

        Ok(server)
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
    ///
    /// note: and a third refusal, for a path holding something that is not a socket at all. A
    /// mistyped `--serve` says the path of a file somebody cares about, and the two sentences above
    /// would name a regular file a socket left by a killed session and tell whoever read it to
    /// remove it - so what is actually there is said instead, and only a socket is offered as one.
    async fn unix(path: &str, terminations: crate::stopping::Terminated) -> Result<Self, String> {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};

        let path = std::path::PathBuf::from(path);
        // note: `symlink_metadata` and not `exists`, which follows a link and so sees nothing at
        // all where one points nowhere - the one case that then reached the bind and came back as
        // a bare `Address already in use`. A link at this path is named as a link and refused
        // rather than followed, and that is a decision: `Drop` takes the file away by the device
        // and inode the bind made, which for a link would be the link's and never the socket's, so
        // a session served through one would leave the socket behind for every later `--serve` to
        // refuse. The address a session is served on is its own path.
        if let Ok(there) = std::fs::symlink_metadata(&path) {
            let what = match there.file_type() {
                socket if socket.is_socket() => None,
                kind if kind.is_dir() => Some("a directory"),
                kind if kind.is_symlink() => Some("a link"),
                _ => Some("a file"),
            };
            if let Some(what) = what {
                return Err(format!(
                    "there is {what} at {}, and it is not a socket; a socket file is what \
                     `--serve` makes, and nothing here would take this away",
                    path.display()
                ));
            }

            return Err(match tokio::net::UnixStream::connect(&path).await {
                Ok(_) => format!(
                    "there is a session listening at {}; attach to it with \
                     `--connect unix:{}`",
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
        // note: after the refusals above rather than before them, because they are the ones that
        // matter to whoever reads them. A mistyped `--serve` over somebody's own file has to be
        // answered as a file, whatever the path weighs - telling that person the path is too long
        // sends them looking for a shorter directory while the file they nearly deleted sits there
        // still. This one is about a path with nothing at it, which is the only case a length is
        // the whole of the answer
        if let Some(bytes) = protocol::overlong_path(&path.to_string_lossy()) {
            return Err(format!(
                "a socket file cannot be named by {bytes} bytes, and this one is; a `unix:` path \
                 can be at most {} bytes - put it under `$XDG_RUNTIME_DIR` or in `/tmp`, or \
                 serve on `--serve tcp:127.0.0.1:PORT`",
                protocol::MAX_PATH
            ));
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
        let made = std::fs::symlink_metadata(&path)
            .map(|meta| (meta.dev(), meta.ino()))
            .map_err(|e| format!("could not read {} back: {e}", path.display()))?;

        Ok(Self {
            listener: Listener::Unix(listener),
            unlink: Some((path, made.0, made.1)),
            closed: None,
            resting: std::sync::Mutex::new(None),
            stopped: None,
            terminations: tokio::sync::Mutex::new(terminations),
            presses: None,
        })
    }

    /// A port, and only ever a loopback one.
    async fn tcp(host: &str, terminations: crate::stopping::Terminated) -> Result<Self, String> {
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
        // note: closed to every command this process confines from here on, since a client answers
        // permission questions. Only the port can be refused this way: a loopback connection
        // carries no pid, so a command allowed the network would otherwise be a client like any
        // other. See `Sandbox::closed`
        let port = listener
            .local_addr()
            .map_err(|e| format!("could not read back where {address} is: {e}"))?
            .port();
        crate::sandbox::serving_on(port);

        Ok(Self {
            listener: Listener::Tcp(listener),
            unlink: None,
            closed: Some(port),
            resting: std::sync::Mutex::new(None),
            stopped: None,
            terminations: tokio::sync::Mutex::new(terminations),
            presses: None,
        })
    }

    /// Where it is listening, in the spelling a client would type.
    ///
    /// note: asked of the socket rather than remembered from the address, because `tcp:127.0.0.1:0`
    /// is a reasonable thing to ask for and only the socket knows which port it got.
    pub fn address(&self) -> String {
        match &self.listener {
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
    ///
    /// note: a failure is handed back once, and every later one in the same run of them is waited
    /// out quietly, `RESTING` apart. Both loops say what this hands back to everybody attached, and
    /// out of file descriptors it would otherwise be a line to each of them for every turn of the
    /// loop until somebody closed something.
    ///
    /// note: a run ends once a whole `RESTING` has gone by past the retry without another failure,
    /// and not when one connection arrives. Descriptors come back one at a time, as the tasks
    /// holding them read to the end, so the first connection taken on the way out of a shortage is
    /// often followed by another failure - and ended there, the run would be said twice.
    pub async fn arrived(&self) -> std::io::Result<Arrived> {
        loop {
            let resting = *self.resting.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(until) = resting {
                tokio::time::sleep_until(until).await;
            }
            let accepted = self.accept().await;
            let mut resting = self.resting.lock().unwrap_or_else(|e| e.into_inner());
            let now = tokio::time::Instant::now();
            let over = run_is_over(*resting, now);
            match accepted {
                Ok(connection) => {
                    if over {
                        *resting = None;
                    }

                    return Ok(Arrived(connection));
                }
                Err(e) => {
                    *resting = Some(now + RESTING);
                    if over {
                        return Err(std::io::Error::new(
                            e.kind(),
                            format!(
                                "{e}; waiting a moment between attempts from here on, and saying \
                                 nothing more until they have stopped failing"
                            ),
                        ));
                    }
                }
            }
        }
    }

    /// Takes whatever connected next.
    ///
    /// note: a port gets the two options a socket file has no use for; see [`super::tuned`]. It is
    /// done here rather than on the listener because neither of them is inherited - they are
    /// properties of a connection, so every accepted one has to be told.
    async fn accept(&self) -> std::io::Result<super::Connection> {
        match &self.listener {
            Listener::Unix(listener) => listener
                .accept()
                .await
                .map(|(it, _)| super::Connection::Unix(it)),
            Listener::Tcp(listener) => listener.accept().await.map(|(it, _)| {
                super::tuned(&it);

                super::Connection::Tcp(it)
            }),
        }
    }

    /// Why the last [`Server::run`] stopped, where a signal rather than somebody's `/quit` was the
    /// reason: `ctrl+c`, `SIGHUP` or `SIGTERM`, the first of them that arrived.
    ///
    /// note: what the program's exit status is made of, as [`crate::headless::Headless::stopped`]
    /// is for a headless run. A served session ended by `SIGTERM` left with `0`, which a script or
    /// a service manager reads as a session that finished rather than one that was stopped.
    pub fn stopped(&self) -> Option<crate::headless::Stop> {
        self.stopped
    }

    /// Waits for the next `SIGTERM` or `SIGHUP`, from the moment this was bound; cancel-safe, as
    /// [`crate::stopping::Terminated::which_arrived`] is.
    ///
    /// note: for a loop that serves this session with a screen as well, which has to read these
    /// rather than subscribe again: a signal that arrives before a subscription is made reaches
    /// only the ones already made, so a loop of its own would wait for it for ever.
    pub async fn terminated(&self) -> crate::stopping::Ending {
        self.terminations.lock().await.which_arrived().await
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
        // what wakes the loop for a running command's question, which is no event of the kernel's;
        // `pump` at the top is what tells the clients
        let mut reaching = app.policy.reaching().subscribe();
        let mut failed = None;
        // subscribed once, in `bind`, because a second press arriving while the first is being
        // handled is the one that means leave; see `crate::stopping`. A second `run` after the
        // first took it back gets it again, and only one that failed before then subscribes anew
        let mut presses = match self.presses.take() {
            Some(presses) => presses,
            None => crate::stopping::Stopping::new()
                .map_err(|e| format!("could not listen for ctrl+c: {e}"))?,
        };

        // set by whichever branch found a reason to stop, rather than each of them breaking where
        // it stands: one of them is nested inside a second `select!`, and a `break` there ends the
        // wrong loop
        let mut leaving = false;
        // what a provider says while it waits, which no event carries; `pump` hands it on
        let mut notices = tokio::time::interval(NOTICES);
        notices.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            // what the session has said first - the list a command came back with - and then the
            // lines that waited for it, so that nothing is heard in an order it did not happen in
            serving.pump(app);
            if app.release().await {
                serving.pump(app);
            }
            // a second `ctrl+c` or a closed stream leaves at once; a `/quit` waits for the turn
            if app.leaving() && !leaving {
                failed = app.wait_for_turn(events, finished, |_| {}).await.or(failed);
                break;
            }
            if leaving {
                break;
            }
            // and a first `ctrl+c` once the turn has stopped, which waits for a `/model` or
            // `/endpoint` still settling as `/quit` does: its change is a record, and a session
            // ended under it is one that changed model after it had finished
            if stopping && !app.busy {
                failed = app.wait_for_turn(events, finished, |_| {}).await.or(failed);
                break;
            }

            tokio::select! {
                incoming = self.arrived() => apply_arrival(&mut serving, app, incoming),
                Some(ask) = serving.asked() => {
                    // note: the command holds the `App` for as long as it takes - there is one of
                    // it - so the loop waits. Nothing a client sends waits on an endpoint here any
                    // more, since a command that would is sent out and finished when it comes back
                    // (see `App::in_flight`), but answering one is still not instant. What the loop
                    // keeps doing meanwhile is reading the kernel's broadcast, in a second
                    // `select!` underneath this one, because that is the only thing here that
                    // *loses* rather than queues: a subscription that falls behind drops what it
                    // did not read. What this loop reads the stream for is `App::trace`, which is
                    // handed to every client that attaches afterwards, holes and all.
                    //
                    // note: the events and nothing else. A connection waits in the listen backlog,
                    // an outcome in an unbounded channel and a `ctrl+c` in its own stream: all
                    // three arrive late either way and none of them is dropped, so taking them
                    // early would buy an ordering no client can tell apart.
                    //
                    // note: what is still *held* is the next command - the client's own, or the
                    // attach of one replacing it - because answering one needs the `App` and the
                    // `App` is lent out, for no longer than the command takes to do.
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
                () = presses.pressed() => {
                    self.stopped.get_or_insert(crate::headless::Stop::Interrupted);
                    leaving |= apply_press(app, &mut stopping);
                }
                // taken as `/quit`: the turn is stopped and waited for above
                ending = self.terminated() => {
                    app.quit = true;
                    self.stopped.get_or_insert(match ending {
                        crate::stopping::Ending::HungUp => crate::headless::Stop::HungUp,
                        _ => crate::headless::Stop::Terminated,
                    });
                }
                Some(outcome) = finished.recv() => apply_outcome(app, events, outcome, &mut failed),
                Ok(()) = reaching.changed() => {}
                // note: while a turn runs, for the reason `headless.rs` gives
                _ = notices.tick(), if app.busy => {
                    app.take_notices();
                }
            }
        }

        // what never went in, said to whoever is still attached; see `App::unsent`
        app.unsent();
        // note: the session is ended here rather than by the caller, for the reason `headless.rs`
        // gives: `session.finished` is a record like any other, and a caller that ended it after
        // this returned would have written every record but the last one
        app.kernel.finish();
        serving.last(app).await;
        self.presses = Some(presses);

        match failed {
            Some(_) => Err("the last turn failed".to_owned()),
            None => Ok(()),
        }
    }
}

/// Whether a run of failed arrivals is over by `now`, given when the listener was next to be tried
/// after the last of them; see [`Server::arrived`].
fn run_is_over(resting: Option<tokio::time::Instant>, now: tokio::time::Instant) -> bool {
    resting.is_none_or(|until| now >= until + RESTING)
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

/// Takes in the end of a turn, and keeps what it failed with if it did; or a command coming back,
/// which says nothing about how the last turn went.
fn apply_outcome(
    app: &mut App,
    events: &mut broadcast::Receiver<Event>,
    outcome: Outcome,
    failed: &mut Option<String>,
) {
    // the turn's last events are still queued behind this one, and `select!` picks whichever
    // branch is ready rather than whichever happened first
    while let Ok(event) = events.try_recv() {
        app.on_event(event);
    }
    match &outcome {
        Outcome::Failed(e) => *failed = Some(e.clone()),
        Outcome::Returned(_) => {}
        _ => *failed = None,
    }
    app.on_outcome(outcome);
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
/// running on and from a browser at the same time. What that costs a loop is [`pump`] before it
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
    /// Which running commands were waiting on the network when anybody was last told; see
    /// [`Message::Reaching`].
    reaching: Vec<crate::tools::Reached>,
    /// How many connections have arrived, which is what names them.
    clients: u64,
    /// The client whose session this is at the moment: the last one to attach, until it leaves.
    ///
    /// note: an attach rather than an arrival, so that a connection that never says anything - a
    /// port scan, a `nc` somebody forgot - takes nothing from anybody. A connection that has not
    /// attached can do nothing else either; see `connection::attend`.
    seated: Option<u64>,
    /// How to tell each connection still open that it has been replaced.
    ///
    /// note: a `Notify` rather than a `oneshot`, because the connection waits on it in a `select!`
    /// it goes round many times, and a `oneshot` polled again after it has fired panics. A
    /// notification sent while the connection is busy elsewhere is kept for its next look.
    replaced: HashMap<u64, Arc<Notify>>,
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
            announced: app.working(),
            model: app.kernel.model_info(),
            reaching: app.policy.reaching().waiting(),
            clients: 0,
            seated: None,
            replaced: HashMap::new(),
            connections: JoinSet::new(),
        }
    }

    /// Gives the session to the client that has just attached, and lets go of the one it had.
    ///
    /// note: before the attach is answered, so that there is no moment at which two clients both
    /// hold it. The one replaced is told by its own connection, which flushes what it owes and
    /// closes; a question it had not answered is left for the newcomer, as one is for anybody
    /// arriving after a client that left.
    fn seated(&mut self, app: &mut App, client: u64) {
        if let Some(had) = self.seated.replace(client).filter(|had| *had != client) {
            if let Some(replaced) = self.replaced.remove(&had) {
                replaced.notify_one();
            }
            app.trace(
                "client.replaced",
                format!("client {had}, by client {client}; one client at a time is served"),
            );
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
        self.say(app);
        // note: on a change and nothing else. What closes the gap between a client asking for
        // something and being told what came of it is the *answer* to its command, which every
        // command has and which carries this same figure - see `Message::Done`. A broadcast that
        // also fired per command would reach every other client as news about a session that had
        // not changed
        if self.announced != app.working() {
            self.announced = app.working();
            let _ = self.voice.send(Arc::new(Message::Busy {
                busy: self.announced,
            }));
        }
        // and the same rule for the model: a switch finishes inside the provider the kernel
        // already holds, and a client that did not ask for a fresh projection went on naming the
        // model before it. See `Message::Model`
        //
        // note: asked once rather than once per side of the comparison. `Kernel::model_info` builds
        // a `ModelInfo` each time it is called - two strings and the list of parameters the model
        // takes - and the drawn loop pumps on every frame
        let model = app.kernel.model_info();
        if self.model != model {
            self.model = model.clone();
            let _ = self.voice.send(Arc::new(Message::Model { model }));
        }
        // and the same again for a command that reached for the network, which is in no record at
        // all: the question is the gate's, not the kernel's, so this is the only way a client
        // hears of it
        let reaching = app.policy.reaching().waiting();
        if self.reaching != reaching {
            self.reaching = reaching.clone();
            let _ = self
                .voice
                .send(Arc::new(Message::Reaching { waiting: reaching }));
        }
    }

    /// Says every line of the program's own since the last one said, and moves the mark past them.
    fn say(&mut self, app: &App) {
        for entry in app.notes(self.said) {
            self.said += 1;
            let _ = self.voice.send(Arc::new(Message::Said {
                speaker: entry.speaker,
                text: entry.text.clone(),
            }));
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
    /// there is one of it, and answering anybody needs it. What it does not do is wait on an
    /// endpoint: `/models`, a compaction pass and a switch are sent out and finished when they come
    /// back, and a line arriving meanwhile is answered `queued` and handed in after - see
    /// [`App::in_flight`]. So a client's command is answered in the time it takes to do, and the
    /// loop goes back to everybody else.
    ///
    /// note: what it does not cost is anything *lost*. Both loops that call this read the
    /// kernel's broadcast while they wait - see the branch in [`Server::run`] - because a
    /// subscription that falls behind drops what it did not read, and `App::trace` is built from
    /// what this loop read. Everything else that arrives meanwhile queues: a connection in the
    /// listen backlog, an outcome in an unbounded channel, a `ctrl+c` in its own stream.
    pub async fn answer(&mut self, app: &mut App, asked: Asked) {
        match asked.0 {
            FromClient::Left { client } => {
                self.replaced.remove(&client);
                if self.seated == Some(client) {
                    self.seated = None;
                }
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
                // note: the seat is asked here, where the command would be done, and not only by
                // the replaced connection going. It is told to go and goes when it next looks, and
                // a command it had read before then arrives after the newcomer has the session.
                // An attach is the one command that takes the seat rather than needing it
                if !matches!(command, Command::Attach { .. }) && self.seated != Some(client) {
                    let _ = answer.send(Answered {
                        message: Some(Message::Failed {
                            about: "replaced".to_owned(),
                            error: "another client has attached to this session, and it serves \
                                    one at a time; attaching again takes it back"
                                .to_owned(),
                        }),
                        voice: None,
                        standing: Vec::new(),
                    });
                    return;
                }
                // taken before the projection rather than after it, so that a line said between the
                // two would arrive twice rather than not at all. Nothing runs in between today; the
                // order is which way to be wrong if anything ever does
                let voice =
                    matches!(command, Command::Attach { .. }).then(|| self.voice.subscribe());
                let standing = match command {
                    Command::Attach { since: Some(_), .. } => standing(app),
                    _ => Vec::new(),
                };
                if matches!(command, Command::Attach { .. }) {
                    self.seated(app, client);
                }
                let _ = answer.send(Answered {
                    message: apply(app, command).await,
                    voice,
                    standing,
                });
            }
        }
    }

    /// Takes on a connection that has just arrived.
    pub fn attend(&mut self, app: &mut App, arrived: Arrived) {
        // note: a command this process confined, or anything it left running in its session, is
        // not a client, because a client answers permission questions and it would be answering its
        // own. A socket file is reachable by every confined command below Linux 7.1, and from 7.1
        // by one that may write where it is. A process a command started under a `setsid` of its
        // own is in a session nothing here has seen, and gets through
        if let super::Connection::Unix(stream) = &arrived.0
            && stream
                .peer_cred()
                .ok()
                .and_then(|cred| cred.pid())
                .and_then(|pid| u32::try_from(pid).ok())
                .is_none_or(crate::sandbox::from_a_command)
        {
            app.trace(
                "client.refused",
                "a connection from a command this session confined".to_owned(),
            );
            return;
        }
        self.clients += 1;
        // note: the *trace* rather than the conversation. A session somebody else can type into
        // should say when somebody else can type into it - but said through `App::say` it would go
        // into `App::loose`, which is the conversation, which is in every projection handed out
        // afterwards. A browser reconnecting on a flaky link opens a connection a second, and
        // `web` tells it to with `retry: 1000`, so the chat would fill with arrivals and
        // departures until somebody typed `/cleanup`.
        //
        // The trace is a ring of the last few hundred lines and is where a thing that happens once
        // a second belongs. Nothing is lost: `Attached::trace` carries it, so it is a row on the
        // trace tab of every client
        app.trace("client.attached", format!("client {}", self.clients));
        let (client, kernel, asks) = (self.clients, app.kernel.clone(), self.asks.clone());
        let replaced = Arc::new(Notify::new());
        self.replaced.insert(client, replaced.clone());
        // note: **not** subscribed here. The subscription is taken where the projection is, which
        // is the only place the two can be taken together - see `Answered`. A receiver taken here
        // would also catch a line the projection already carries, and print it twice
        //
        // note: the ones that have finished are let go of here, because a `JoinSet` keeps each
        // until it is asked for it, and a browser reconnecting once a second would otherwise be a
        // list that only grows
        while self.connections.try_join_next().is_some() {}
        self.connections
            .spawn(serve(client, arrived.0, kernel, asks, replaced));
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
    pub fn last(mut self, app: &App) -> impl Future<Output = ()> + Send + use<> {
        self.say(app);

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
    /// Takes the socket file away again, or opens the port to confined commands again.
    ///
    /// note: only the file this process made, and only because a socket file that outlives its
    /// listener is a path every later client is refused at and every later `--serve` refuses as
    /// stale.
    fn drop(&mut self) {
        if let Some(port) = self.closed {
            crate::sandbox::stopped_serving_on(port);
        }
        if let Some((path, dev, ino)) = &self.unlink {
            use std::os::unix::fs::MetadataExt as _;

            if std::fs::symlink_metadata(path)
                .is_ok_and(|meta| (meta.dev(), meta.ino()) == (*dev, *ino))
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// Does one of the things that need the session itself, and says what it did.
///
/// note: every command but one. `Inspect` of what an item says *now* is answered by the connection
/// that asked it, out of a `Kernel` handle of its own, because reading it needs no `App` - and a
/// client reading a four-megabyte tool result should not be something the session stops to do. An
/// earlier version is `App`'s to remember, so that one comes here.
async fn apply(app: &mut App, command: Command) -> Option<Message> {
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
                busy: app.working(),
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
        // note: answered with the projection, for `cycle`'s reason: the answer to "change this
        // rule" is the list of rules as it now stands, and a row put back to a question is not on
        // it any more
        Command::Rule { subject, verdict } => Some(match app.rule(&subject, verdict) {
            Ok(()) => Message::Projected(Box::new(project(app))),
            Err(error) => Message::Failed {
                about: "rule".to_owned(),
                error,
            },
        }),
        // note: answered with a projection, which is what `cycle` answers with and for its reason.
        // An edit changes what the item says, what it costs, and therefore what the next request
        // comes to - and a client could work out none of that from the `context.replaced` the
        // stream is about to carry.
        //
        // note: a text that changes nothing is a `Done` rather than a projection, because nothing
        // moved: no record, no version page, no checkpoint. Saying so plainly is better than a
        // projection identical to the one the client already had, which reads as an edit that
        // silently did not take
        Command::Revise { id, text } => Some(match app.revise(id, &text, "edited from a client") {
            Ok(true) => Message::Projected(Box::new(project(app))),
            Ok(false) => Message::Done {
                about: "revise".to_owned(),
                busy: app.working(),
            },
            Err(error) => Message::Failed {
                about: "revise".to_owned(),
                error,
            },
        }),
        // note: refused here as well as at the two line drivers, because the protocol is open to
        // any client and a blank line handed to `App::submit` is a message: an empty turn sent to
        // the model, and a request some endpoints refuse outright
        Command::Submit { line } if line.trim().is_empty() => Some(Message::Failed {
            about: "submit".to_owned(),
            error: "a blank line is not a message, so nothing was sent".to_owned(),
        }),
        Command::Submit { line } => {
            // note: a client has no keys of this program's to press, whatever the loop driving the
            // session has, so `App::keys` is set around the one call that reads it rather than once
            // for the session. A served session can have a screen as well: a `/help` typed at the
            // desk wants the key pages, and the same `/help` sent from a browser would be a
            // reference to a program the reader is not using. The flag is a fact about whoever just
            // asked. See `App::help`
            //
            // note: and `/compact` reads it when its pass comes back, which is after this: a client
            // has no keys to answer the question with, so the pass is taken and its list said -
            // see `App::planned`
            let keys = std::mem::replace(&mut app.keys, false);
            let reply = app.submit(&line).await;
            app.keys = keys;

            Some(Message::Replied {
                did: reply.did,
                page: reply.page.map(|overlay| match overlay {
                    Overlay::Text { title, pages, .. } => Printed { title, pages },
                }),
                busy: app.working(),
            })
        }
        Command::Interrupt => {
            app.interrupt();
            Some(Message::Done {
                about: "interrupt".to_owned(),
                busy: app.working(),
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
            // note: `busy` read *after* the decision rather than before, because answering the
            // last outstanding question is what starts the next turn - so a figure taken first
            // would tell the client the session was quiet in the one moment it had just stopped
            // being so
            match app.decide(id, grant, remember) {
                Ok(()) => Some(Message::Done {
                    about: "decide".to_owned(),
                    busy: app.working(),
                }),
                Err(error) => Some(Message::Failed {
                    about: "decide".to_owned(),
                    error,
                }),
            }
        }
        // note: on sight as well, and for a reason of its own: the question holds a call that is
        // running, so there is no turn unwinding for an answer to land in the middle of
        Command::Reach {
            id,
            grant,
            remember,
        } => match app.decide_reach(id, grant, remember) {
            Ok(()) => Some(Message::Done {
                about: "reach".to_owned(),
                busy: app.working(),
            }),
            Err(error) => Some(Message::Failed {
                about: "reach".to_owned(),
                error,
            }),
        },
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

/// What a client coming back is told of what the voice only says on a change; see
/// [`Answered::standing`].
///
/// note: both, whether or not they changed, because this end cannot know what the client last
/// heard. An empty list is news too, to a client still offering a question somebody else answered
/// while it was gone.
fn standing(app: &App) -> Vec<Message> {
    vec![
        Message::Model {
            model: app.kernel.model_info(),
        },
        Message::Reaching {
            waiting: app.policy.reaching().waiting(),
        },
    ]
}

/// Where the session stands, in the form a client can start rendering from.
///
/// note: abridged here rather than at either of the writes that send it, because there are two -
/// an attach and a `project` - and a projection one of them cut and the other refused would be a
/// session a browser can attach to and then never redraw. See [`Attached::abridge`].
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

    let mut attached = Attached {
        rated: rated(app, &asking),
        unrated: unrated(app, &asking),
        version: protocol::VERSION,
        seq,
        session: app.kernel.session_name(),
        state: app.kernel.state(),
        busy: app.working(),
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
        reaching: app.policy.reaching().waiting(),
        trace: tracing(app),
        policy: app.policy_name(),
        untold: crate::tools::Careful::untold(),
        permissions: app.permissions().iter().map(Stanced::of).collect(),
        undecided: app.undecided(),
        queued: app.queued().next().map(str::to_owned),
        queued_behind: app.queued().skip(1).map(str::to_owned).collect(),
        confinement: app.confinement(),
    };
    attached.abridge();

    attached
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
        Command::Reach { .. } => "reach",
        Command::Inspect { .. } => "inspect",
        Command::Project => "project",
        Command::Cycle { .. } => "cycle",
        Command::Rule { .. } => "rule",
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::wiring::{Setup, Wired};
    use nachalnik::ModelResponse;
    use nachalnik_providers::OpenAiCompatible;

    /// A session wired the way the program wires one, with a scripted model behind it.
    fn session() -> Wired {
        let wired = Setup {
            tools: Some(Vec::new()),
            compact: None,
            ..Default::default()
        }
        .wire(Arc::new(OpenAiCompatible::new(
            "scripted",
            "http://127.0.0.1:1",
            "",
        )))
        .expect("the wiring failed");
        wired
            .app
            .kernel
            .set_provider(Arc::new(nachalnik::test::ScriptedProvider::new(vec![
                ModelResponse::text("nothing to see"),
            ])));

        wired
    }

    /// A run of failed arrivals is one run until a whole `RESTING` goes by past the retry, however
    /// many connections got through in the middle of it.
    #[test]
    fn a_run_of_failures_ends_with_a_quiet_rest_and_not_with_one_arrival() {
        let start = tokio::time::Instant::now();
        let at = |millis: u64| start + Duration::from_millis(millis);

        // nothing has failed yet, so the first failure is said
        assert!(run_is_over(None, at(0)));

        // failed at 0, so tried again at 1000: a failure there, and one just after a connection
        // got through at 1000, are both the same run
        let resting = Some(at(0) + RESTING);
        assert!(!run_is_over(resting, at(1_000)));
        assert!(!run_is_over(resting, at(1_010)));

        // and a whole rest past the retry with nothing failing is the end of it
        assert!(run_is_over(resting, at(2_000)));
    }

    /// The gap goes out where the program spent the time, and nowhere a person did.
    ///
    /// note: this is a client reading it rather than a pane drawing it, which is the half only here
    /// can see: a client handed two timestamps works the rule out itself and gets a column whose
    /// largest figure is the operator reading. Both halves of the guard are in one trace, because
    /// either alone is satisfied by a gap column that is always empty.
    #[test]
    fn the_gap_column_leaves_out_the_wait_that_was_a_person() {
        use crate::app::Traced;
        use std::time::{Duration, Instant, SystemTime};

        let (outcomes, _finished) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            nachalnik::Kernel::new(nachalnik::Config::default()),
            Arc::new(crate::tools::Careful::new()),
            Arc::new(nachalnik_providers::OpenAiCompatible::new(
                "no",
                "http://127.0.0.1:1",
                "",
            )),
            crate::tools::Limits::new(),
            outcomes,
        );
        let start = Instant::now();
        let wall = SystemTime::now();

        // four seconds of somebody reading a question, and then a quarter of a second of the
        // program doing what they allowed: both waits are longer than the tenth of a second the
        // column reports at all, so the only thing telling them apart is whose they were
        for (name, after, theirs) in [
            ("permission.requested", 0, false),
            ("permission.decided", 4_000, true),
            ("tool.started", 4_010, false),
            ("tool.finished", 4_260, false),
        ] {
            app.trace.push_back(Traced {
                name: name.to_owned(),
                detail: "shell".to_owned(),
                at: start + Duration::from_millis(after),
                wall: wall + Duration::from_millis(after),
                after_a_person: theirs,
            });
        }

        let lines = tracing(&app);
        let gap_of = |name: &str| {
            lines
                .iter()
                .find(|line| line.name == name)
                .map(|line| line.gap.clone())
        };

        // the four seconds somebody spent is on no line at all
        let decided = gap_of("permission.decided");
        assert_eq!(
            decided,
            Some(None),
            "how long a person took went out as a gap: {lines:?}"
        );

        // and the quarter of a second the program spent is, on the line it ended on
        assert_eq!(
            gap_of("tool.finished")
                .as_ref()
                .and_then(|gap| gap.as_deref()),
            Some("+250ms"),
            "a wait the program spent went out as no gap: {lines:?}"
        );
    }

    /// The first `ctrl+c` stops what is running and does not leave; the second one leaves at once.
    ///
    /// note: the pair, because either half on its own is a session that behaves. `Server::run`
    /// leaves on the press branch rather than waiting for the loop top, so a first press that
    /// also returned `true` ended the session with the turn still running and nothing said about
    /// what was being kept - the note and the `interrupt` are what the first press is for. And a
    /// press that never returned `true` is a session nothing can end but `/quit`: a served
    /// session has no keys, so `ctrl+c` twice is how somebody at the other end of a socket stops
    /// it, and a run with a tool in it that never answers stays up until a signal arrives from
    /// outside the process.
    #[test]
    fn a_second_ctrl_c_is_what_leaves_a_served_session() {
        let Wired { mut app, .. } = session();
        let mut stopping = false;

        // the first one stops what is running and keeps the session
        assert!(!apply_press(&mut app, &mut stopping));

        // and the second one is the one that ends it, whatever the turn was doing
        assert!(apply_press(&mut app, &mut stopping));
    }

    /// The seat goes to the client that is here, and only its own departure takes it away.
    ///
    /// note: the ordering, which is the whole of it. A replaced client is told so by its own
    /// connection and only then goes - `connection::serve` sends `Left` on its way out - so the
    /// departure that lands after a replacement is the one that was replaced, and taking the seat
    /// off whoever holds it when it lands leaves the session served by nobody. The next client to
    /// attach then finds an empty seat, replaces nobody and is served beside the one that was
    /// holding it: two clients steering one agent, neither told about the other, and the newest
    /// wins having evicted nobody. So a departure is asked whether it is the seated client's own
    /// before the seat is cleared.
    #[tokio::test]
    async fn the_departure_of_a_replaced_client_does_not_vacate_the_seat() {
        let Wired { mut app, .. } = session();
        let mut serving = Serving::new(&app);

        attach(&mut serving, &mut app, 1).await;
        attach(&mut serving, &mut app, 2).await;
        assert_eq!(
            serving.seated,
            Some(2),
            "the newest client does not hold it"
        );

        // client 1 was replaced by client 2, and its connection now closes and says so
        left(&mut serving, &mut app, 1).await;
        assert_eq!(
            serving.seated,
            Some(2),
            "a replaced client took the seat with it, and nobody holds the session"
        );

        // and client 3 finds a session that has an owner
        attach(&mut serving, &mut app, 3).await;
        assert_eq!(serving.seated, Some(3));
    }

    /// A command from a client that has been replaced is refused, rather than done.
    ///
    /// note: the seat is what makes one client at a time true, and nothing else stands between a
    /// replaced client and the session: its connection is told to go, and goes when it next
    /// looks. A command it had read before then still arrived here and was done - an interrupt
    /// stopping the turn the newcomer had just started, or a `y` answering a question the
    /// newcomer was about to answer for itself - with nothing telling the newcomer it happened.
    #[tokio::test]
    async fn a_command_from_a_replaced_client_is_refused() {
        let Wired { mut app, .. } = session();
        let mut serving = Serving::new(&app);

        attach(&mut serving, &mut app, 1).await;
        attach(&mut serving, &mut app, 2).await;

        let (answer, answered) = oneshot::channel();
        serving
            .asks
            .send(FromClient::Asked {
                client: 1,
                command: Command::Submit {
                    line: "/note from a client that was replaced".to_owned(),
                },
                answer,
            })
            .expect("the session is still listening");
        let asked = serving.asked().await.expect("something to answer");
        serving.answer(&mut app, asked).await;

        let answered = answered.await.expect("the command was answered");
        assert!(
            matches!(&answered.message, Some(Message::Failed { about, .. }) if about == "replaced"),
            "a replaced client's command was not refused: {:?}",
            answered.message
        );
        assert!(
            !app.kernel.items().iter().any(|item| item
                .content
                .to_text()
                .contains("a client that was replaced")),
            "a replaced client's command was done"
        );
        assert_eq!(serving.seated, Some(2), "and the seat is where it was");
    }

    /// One client attaching, through the channel it really comes through.
    async fn attach(serving: &mut Serving, app: &mut App, client: u64) {
        let (answer, answered) = oneshot::channel();
        serving
            .asks
            .send(FromClient::Asked {
                client,
                command: Command::Attach {
                    since: None,
                    session: None,
                    version: None,
                },
                answer,
            })
            .expect("the session is still listening");
        let asked = serving.asked().await.expect("something to answer");
        serving.answer(app, asked).await;
        answered.await.expect("the command was answered");
    }

    /// One client going, which is what its connection says on the way out.
    async fn left(serving: &mut Serving, app: &mut App, client: u64) {
        serving
            .asks
            .send(FromClient::Left { client })
            .expect("the session is still listening");
        let asked = serving.asked().await.expect("something to answer");
        serving.answer(app, asked).await;
    }

    /// A question the advisor placed travels to the client beside the question it is about, and a
    /// question it could not place travels with the reason rather than as nothing at all.
    ///
    /// note: the two rows, because they are two halves of one thing. `Attached::rated` is where a
    /// browser draws the band, and an empty list is not the same thing as no question: a client
    /// cannot tell a command nobody rated from a command this end forgot to rate, and the second
    /// is a bug it has no way of noticing. `Attached::unrated` exists for the case where the
    /// advisor answered and could not place the command - a firewall's refusal is the command the
    /// colour is most for - and a reason dropped is a question drawn as though nobody had anything
    /// to say about it.
    #[cfg(feature = "shell-advisor")]
    #[tokio::test]
    async fn what_the_advisor_said_about_a_waiting_question_travels_beside_it() {
        use crate::tools::{Advised, Careful};
        use nachalnik::{
            Capability, PermissionId, PermissionPolicy, PermissionRequest, ToolCallId, Verdict,
        };
        use nachalnik_providers::system1::{Answers, Question, SystemOne};
        use serde_json::{Value, json};

        /// An engine that places everything it is asked about, except the one command whose name
        /// says it cannot.
        struct Rubric;

        #[nachalnik::async_trait]
        impl SystemOne for Rubric {
            async fn ask(
                &self,
                state: Value,
                _questions: Vec<(String, Question)>,
            ) -> Result<Answers, nachalnik::BoxError> {
                let unreadable = state["arguments"]["cmd"].as_str() == Some("unreadable");

                Ok(Answers::read(match unreadable {
                    // an answer with neither rubric nor claim in it: what a firewall's refusal
                    // page reads as
                    true => json!({ "model": "rubric", "answers": {} }),
                    false => json!({
                        "model": "rubric",
                        "answers": { "rating": { "type": "score", "score": 0.1, "confidence": 0.99 } },
                    }),
                }))
            }

            fn named(&self) -> String {
                "rubric".to_owned()
            }
        }

        let asking = |id: u64, command: &str| PermissionRequest {
            id: PermissionId(id),
            call: ToolCallId::from(format!("call-{id}")),
            tool: "shell".to_owned(),
            capabilities: vec![Capability::exec("run")],
            args: Arc::new(json!({ "cmd": command })),
        };
        let (placed, unplaceable) = (asking(1, "ls -la"), asking(2, "unreadable"));

        let advised = Arc::new(Advised::new(Arc::new(Careful::new()), Arc::new(Rubric)));
        let mut app = session().app;
        app.advisor = Some(advised.clone());
        // the verdict is the standing rules' either way; what the advisor wrote down while it was
        // worked out is what is read afterwards
        for request in [&placed, &unplaceable] {
            assert_eq!(advised.evaluate(request).await, Verdict::Ask);
        }

        let judged = rated(&app, std::slice::from_ref(&placed));
        assert_eq!(judged.len(), 1, "a placed question was not carried");
        assert_eq!(judged[0].id, placed.id, "on the wrong question");
        assert_eq!(judged[0].band, protocol::Band::Reads, "at the wrong band");

        let why = unrated(&app, std::slice::from_ref(&unplaceable));
        assert_eq!(why.len(), 1, "a question with no rating carried no reason");
        assert_eq!(why[0].id, unplaceable.id, "on the wrong question");
        assert!(!why[0].why.is_empty(), "a reason that says nothing");

        // and neither list carries a row for a question the advisor was never asked about
        assert!(judged.iter().all(|row| row.id != unplaceable.id));
        assert!(why.iter().all(|row| row.id != placed.id));
    }
}
