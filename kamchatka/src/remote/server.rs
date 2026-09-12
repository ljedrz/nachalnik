//! The third loop: a session with a socket in front of it.
//!
//! The loop in `main.rs` draws a frame and waits for a key, the one in `headless.rs` waits for a
//! line, and this one waits for whichever of several clients says something first. Everything
//! between the three is the same [`App`]: `submit` takes the line somebody would have typed,
//! `decide` answers the question a tool is waiting on, `on_event` takes what the kernel says back,
//! and the session, the tools, the policy and the trace are where they always were.
//!
//! note: what a connection needs from this loop is much less than it looks, and that is what keeps
//! the fan-out honest. A [`Kernel`] is a cheap `Arc` handle, so every connection has one of its
//! own and reads the session log directly - which means the numbered half of the stream is not
//! something this loop hands out, queues, or is able to drop. Only the four things that need
//! `&mut App` - a fresh client's projection, a submitted line, an interrupt and a decision - come
//! through the channel at all.
//!
//! note: and the consequence worth stating, because it is the whole of the backpressure design:
//! there is **no outbound queue per client anywhere in here**. A connection that stops reading
//! stops being written to, its broadcast receivers fall behind, and it is told how many fragments
//! it missed; the records it missed are still in the log, where it goes back for them by sequence.
//! A slow client therefore costs one socket buffer and loses exactly the thing that could not have
//! been recovered anyway.

use std::sync::Arc;

use nachalnik::{Event, Kernel};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader},
    sync::{broadcast, mpsc, oneshot},
};

use crate::{
    app::{App, Outcome, Overlay, Speaker, text},
    remote::protocol::{self, Address, Attached, Command, Line, Listed, Message, Printed, Stanced},
};

/// How many of the program's own lines a client may fall behind before it starts losing them.
///
/// note: smaller than the kernel's own event queue by a lot, and it can be: a note is not
/// something that streams. What goes through here is what a command answered, what the runtime had
/// to say about a turn, and an error - a handful per turn against a thousand fragments - so a
/// client this far behind on *these* has not been reading for a very long time.
const VOICE: usize = 256;

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

/// What reaches the session loop from a connection.
enum FromClient {
    /// It asked for something that needs the session itself.
    Asked {
        /// Which connection.
        client: u64,
        /// What it asked for.
        command: Command,
        /// Where the answer goes; `None` there means the records will carry it.
        answer: oneshot::Sender<Option<Message>>,
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
    #[cfg(unix)]
    async fn unix(path: &str) -> Result<Self, String> {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::path::PathBuf::from(path);
        if path.exists() {
            return Err(format!(
                "there is already something at {}; remove it if no session is using it",
                path.display()
            ));
        }
        let listener = tokio::net::UnixListener::bind(&path)
            .map_err(|e| format!("could not listen at {}: {e}", path.display()))?;
        // note: after the bind, because there is nowhere earlier - the file is created by the bind
        // itself, under whatever the umask says. The window is one syscall wide, and what closes it
        // properly is the directory the socket is in, which is why the refusal above names the path
        // rather than quietly taking it over
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("could not make {} private: {e}", path.display()))?;

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
    /// is a reasonable thing to ask for and the answer to it is the point.
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

    /// Takes whatever connected next.
    async fn accept(&self) -> std::io::Result<Incoming> {
        match &self.listener {
            #[cfg(unix)]
            Listener::Unix(listener) => listener.accept().await.map(|(it, _)| Incoming::Unix(it)),
            Listener::Tcp(listener) => listener.accept().await.map(|(it, _)| Incoming::Tcp(it)),
        }
    }

    /// Serves the session until somebody says to stop, and returns when nothing is left in flight.
    ///
    /// note: it does **not** stop when the last client leaves, and that is the invariant the whole
    /// design is for: a session belongs to the host, not to whoever happens to be looking at it. A
    /// turn carries on with nobody attached, a question waits for somebody to come back and answer
    /// it, and a client picking the session up an hour later picks up the same session.
    pub async fn run(
        &mut self,
        app: &mut App,
        events: &mut broadcast::Receiver<Event>,
        finished: &mut mpsc::UnboundedReceiver<Outcome>,
    ) -> Result<(), String> {
        let (voice, _) = broadcast::channel(VOICE);
        let (asks, mut asked) = mpsc::unbounded_channel();
        // how many of the program's own lines have gone out; see `App::notes` for why it counts
        // the filtered sequence rather than the list
        let mut said = 0;
        let mut clients = 0;
        // whether the session was busy the last time anybody was told. See `Message::Busy` for why
        // this is the session saying so rather than a client working it out of the records
        let mut announced = app.busy;
        let mut stopping = false;
        let mut failed = None;

        loop {
            // the program has one voice and every client hears it, which is most of the difference
            // between a session several people are attached to and several sessions
            let fresh: Vec<_> = app
                .notes(said)
                .map(|entry| Message::Said {
                    speaker: entry.speaker,
                    text: entry.text.clone(),
                })
                .collect();
            said += fresh.len();
            for message in fresh {
                let _ = voice.send(Arc::new(message));
            }
            // note: on a change and nothing else. What closes the gap between a client asking for
            // something and being told what came of it is the *answer* to its command, which every
            // command has and which carries this same figure - see `Message::Done`. A broadcast
            // that also fired per command would reach every other client as news about a session
            // that had not changed
            if announced != app.busy {
                announced = app.busy;
                let _ = voice.send(Arc::new(Message::Busy { busy: announced }));
            }
            if app.quit || (stopping && !app.busy) {
                break;
            }

            tokio::select! {
                incoming = self.accept() => match incoming {
                    Ok(stream) => {
                        clients += 1;
                        // said through `App`, so it is in the transcript and every other client
                        // sees it. A session somebody else can type into should say when somebody
                        // else can type into it
                        app.say(Speaker::Note, format!("client {clients} attached"));
                        let (kernel, asks) = (app.kernel.clone(), asks.clone());
                        let voice = voice.subscribe();
                        match stream {
                            #[cfg(unix)]
                            Incoming::Unix(stream) => {
                                tokio::spawn(serve(clients, stream, kernel, asks, voice));
                            }
                            Incoming::Tcp(stream) => {
                                tokio::spawn(serve(clients, stream, kernel, asks, voice));
                            }
                        }
                    }
                    // one connection failing to arrive is not a reason to end a session that may
                    // have a turn running in it
                    Err(e) => app.say(Speaker::Error, format!("a client could not connect: {e}")),
                },
                Some(ask) = asked.recv() => match ask {
                    FromClient::Left { client } => app.say(
                        Speaker::Note,
                        format!("client {client} left; the session carries on"),
                    ),
                    FromClient::Asked { client, command, answer } => {
                        let _ = answer.send(apply(app, client, command).await);
                    }
                },
                event = events.recv() => match event {
                    Ok(event) => app.on_event(event),
                    // note: nothing a client can see is lost here. What this loop is doing with the
                    // stream is keeping `App` up to date for the projections it hands out; the
                    // clients read the log for themselves, and the log drops nothing
                    Err(broadcast::error::RecvError::Lagged(missed)) => app.say(
                        Speaker::Note,
                        format!(
                            "{missed} events went by too fast for this session's own view of \
                             itself; the records have them all"
                        ),
                    ),
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = tokio::signal::ctrl_c() => match stopping {
                    // the second one: whatever is still running is somebody else's problem now
                    true => break,
                    false => {
                        stopping = true;
                        app.interrupt();
                        app.say(
                            Speaker::Note,
                            "stopping; what has arrived is kept, and again leaves at once",
                        );
                    }
                },
                Some(outcome) = finished.recv() => {
                    // the turn's last events are still queued behind this one, and `select!` picks
                    // whichever branch is ready rather than whichever happened first
                    while let Ok(event) = events.try_recv() {
                        app.on_event(event);
                    }
                    failed = match &outcome {
                        Outcome::Failed(e) => Some(e.clone()),
                        _ => None,
                    };
                    app.on_outcome(outcome);
                }
            }
        }

        // note: the session is ended here rather than by the caller, for the reason `headless.rs`
        // gives: `session.finished` is a record like any other, and a caller that ended it after
        // this returned would have written every record but the last one
        app.kernel.finish();
        // and the last of the voice, which nothing else is going to send. Whoever is still attached
        // is about to find the socket closed, and these are the lines that say why
        let last: Vec<_> = app
            .notes(said)
            .map(|entry| Message::Said {
                speaker: entry.speaker,
                text: entry.text.clone(),
            })
            .collect();
        for message in last {
            let _ = voice.send(Arc::new(message));
        }

        match failed {
            Some(_) => Err("the last turn failed".to_owned()),
            None => Ok(()),
        }
    }
}

impl Drop for Server {
    /// Takes the socket file away again.
    ///
    /// note: only the one this process made, and only because a socket file that outlives its
    /// listener is a path every later client connects to and hangs on. There is nothing to do for
    /// a port.
    fn drop(&mut self) {
        if let Some(path) = &self.unlink {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Does one of the things that need the session itself, and says what it did.
///
/// note: four, where the protocol has five. `Inspect` is answered by the connection that asked it,
/// out of a `Kernel` handle of its own, because reading what an item says needs no `App` - and a
/// client reading a four-megabyte tool result should not be something the session stops to do.
async fn apply(app: &mut App, client: u64, command: Command) -> Option<Message> {
    match command {
        Command::Attach { .. } => Some(Message::Attached(Box::new(project(app)))),
        Command::Submit { line } => {
            // note: read before the line goes in, because handing one in is what replaces it.
            // There is room for exactly one queued message, so a second client typing during a turn
            // silently takes the first one's place - see `App::queued`. Saying so is the least this
            // can do about it, and it is said to everybody, because the person who lost a line is
            // the one who is not asking
            let replacing = app.queued().map(str::to_owned);
            let reply = app.submit(&line).await;
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
        // note: applied on sight, exactly as the keys apply one, and the window this used to hold a
        // queue against is closed in `App::on_outcome` instead. It belongs there: a person at a
        // terminal answers inside the same window and cannot be asked to be careful about it
        // either, so a guard that only this loop had was a guard the keys went without
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
        // answered by the connection, which never sends it here
        Command::Inspect { .. } => None,
    }
}

/// Where the session stands, in the form a client can start rendering from.
fn project(app: &App) -> Attached {
    let items = app.kernel.items();
    let going = app.going();

    Attached {
        // note: taken here, in the same synchronous stretch as everything below it, and that is
        // what makes the seam airtight rather than nearly so. Nothing else can be driving the
        // session while this runs, so every record up to this number is described by what follows
        // and every record after it arrives on the stream. There is no window in which a change is
        // in neither
        seq: app.kernel.last_seq(),
        session: app.kernel.session_name(),
        state: app.kernel.state(),
        busy: app.busy,
        stepping: app.stepping,
        model: app.kernel.provider().map(|provider| provider.info()),
        budget: app.kernel.budget(),
        spent: app.spent(),
        spend: app.spend(),
        overspent: app.overspent(),
        conversation: app.conversation(&items).iter().map(Line::of).collect(),
        // note: every item, rather than `App::listed`, which is the *screen's* list and leaves out
        // what has been pruned when somebody has asked it to. Which rows to show is a decision
        // belonging to whoever is reading, and a projection that had already made it would be one
        // client's view of the context standing in for the context
        items: items.iter().map(|item| Listed::of(item, &going)).collect(),
        asking: app.kernel.pending_permissions(),
        permissions: app.permissions().iter().map(Stanced::of).collect(),
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
    voice: broadcast::Receiver<Arc<Message>>,
) {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    if let Err(e) = attend(client, &mut lines, &mut write, &kernel, &asks, voice).await {
        // the connection is going either way; this is the last thing it is told, and it is written
        // on a best-effort basis because the usual way to be here is that it stopped listening
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
    lines: &mut tokio::io::Lines<BufReader<R>>,
    write: &mut W,
    kernel: &Kernel,
    asks: &mpsc::UnboundedSender<FromClient>,
    mut voice: broadcast::Receiver<Arc<Message>>,
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
    let mut last = match protocol::read::<Command>(lines).await? {
        None => return Ok(()),
        Some(Command::Attach { since }) => watermark(since, kernel, asks, client, write).await?,
        Some(other) => {
            return Err(format!(
                "`{}` before `attach`: a connection says where it stands first",
                name(&other)
            ));
        }
    };
    flush(kernel, &mut last, write).await?;

    loop {
        tokio::select! {
            command = protocol::read::<Command>(lines) => match command? {
                None => return Ok(()),
                // note: re-attaching on a live connection is allowed, and is the cheapest way for a
                // client that has confused itself to start again: it asks for the projection and
                // moves its own watermark to whatever that says
                Some(Command::Attach { since }) => {
                    last = watermark(since, kernel, asks, client, write).await?;
                    flush(kernel, &mut last, write).await?;
                }
                // answered here rather than by the session loop: it needs a `Kernel` and nothing
                // else, and the session has better things to be doing
                Some(Command::Inspect { id }) => {
                    let message = match kernel.items().iter().find(|item| item.id == id) {
                        Some(item) => Message::Item { id, body: text::stored(item) },
                        None => Message::Failed {
                            about: "inspect".to_owned(),
                            error: format!("there is no item {id} in the context"),
                        },
                    };
                    protocol::write(write, &message).await?;
                }
                Some(command) => {
                    let about = name(&command);
                    match ask(asks, client, command).await {
                        Ok(Some(message)) => protocol::write(write, &message).await?,
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
                    // note: what that costs, and it is worth writing down rather than implying
                    // otherwise: a connection that is behind the broadcast can be handed a fragment
                    // *after* the record that ends the thing it was part of, because the flush
                    // reads wherever the log has got to rather than wherever it had got to when the
                    // fragment was emitted. Reconstructing the true interleaving would mean flushing
                    // one record per non-progress event - which is exact, and is a linear scan of
                    // the log per event, per client. It is not worth it: a fragment whose item has
                    // already arrived is a fragment the item supersedes, and every client in this
                    // workspace already drops one on those grounds. The terminal does it under the
                    // name `Entry::transient`
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
                Ok(message) => protocol::write(write, &*message).await?,
                // the program's own lines are in no log either, and the same rule applies to them
                Err(broadcast::error::RecvError::Lagged(frames)) => {
                    protocol::write(write, &Message::Missed { frames }).await?;
                }
                // note: the session has ended, and this is the last thing this connection does:
                // write out whatever the log grew while it was being written to. What it closes is
                // a coin toss - `session.finished` is emitted before this channel is dropped, so
                // both are ready at once and `select!` picks either - and what the toss costs is a
                // client left short of the last records of a session it has no socket left to go
                // back for them on.
                //
                // note: measured, and it could not be made to matter: removing this and running
                // `quitting_from_a_client_reads_as_an_ending` five times lost nothing, because the
                // event branch is ready first every time in practice. It is three lines against a
                // race that is real and rare, and it is kept on those terms rather than on the
                // strength of a test - which is why this paragraph is here instead of one
                Err(broadcast::error::RecvError::Closed) => {
                    flush(kernel, &mut last, write).await?;

                    return Ok(());
                }
            },
        }
    }
}

/// Settles where this client's numbered stream starts, and sends the projection if it needs one.
async fn watermark<W: AsyncWrite + Unpin>(
    since: Option<u64>,
    kernel: &Kernel,
    asks: &mpsc::UnboundedSender<FromClient>,
    client: u64,
    write: &mut W,
) -> Result<u64, String> {
    let Some(since) = since else {
        let Some(Message::Attached(attached)) =
            ask(asks, client, Command::Attach { since: None }).await?
        else {
            return Err("the session answered an attach with something else".to_owned());
        };
        let seq = attached.seq;
        protocol::write(write, &Message::Attached(attached)).await?;

        return Ok(seq);
    };

    // note: a client claiming to have seen more than has happened is refused rather than clamped.
    // It is either a client that has confused two sessions or one that made the number up, and
    // quietly starting it from the end would leave it convinced it held a history it never had
    let last = kernel.last_seq();
    match since > last {
        true => Err(format!(
            "this session has {last} record(s) and you say you have {since}; attach with no \
             `since` to start again"
        )),
        false => Ok(since),
    }
}

/// Puts one command to the session loop and waits for what it says.
async fn ask(
    asks: &mpsc::UnboundedSender<FromClient>,
    client: u64,
    command: Command,
) -> Result<Option<Message>, String> {
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

/// Writes out every record the session has grown since this client last saw one.
///
/// note: the log rather than the subscription, and that is the whole of why a client cannot lose
/// one. A broadcast has a capacity and drops for whoever falls behind it; the log has neither, so
/// this is a read from wherever this connection had got to and it is correct however long the
/// connection was asleep, however far behind its subscription fell, and whether or not it was here
/// at all when the record was written.
async fn flush<W: AsyncWrite + Unpin>(
    kernel: &Kernel,
    last: &mut u64,
    write: &mut W,
) -> Result<(), String> {
    for record in kernel.history_since(*last) {
        let seq = record.seq;
        protocol::write(write, &Message::Record(record)).await?;
        *last = seq;
    }

    Ok(())
}

/// What a command is called, for the message that says it could not be done.
fn name(command: &Command) -> &'static str {
    match command {
        Command::Attach { .. } => "attach",
        Command::Submit { .. } => "submit",
        Command::Interrupt => "interrupt",
        Command::Decide { .. } => "decide",
        Command::Inspect { .. } => "inspect",
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
