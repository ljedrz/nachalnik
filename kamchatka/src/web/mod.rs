//! The web page for a session (`--web`).
//!
//! ```console
//! kamchatka --web 127.0.0.1:8080 -m qwen/qwen3-coder
//! ```
//!
//! Browsers can't open raw TCP connections, so this serves the page over HTTP and relays each
//! browser tab to the session as an ordinary protocol client. It has three routes and needs no
//! dependency the crate doesn't already have.
//!
//! # Why server-sent events
//!
//! An SSE event can carry an `id:`. When a connection drops, the browser reconnects on its own and
//! sends the last id it saw as `Last-Event-ID`, which maps directly onto [`Command::Attach`]'s
//! `since`, so resuming needs no client code. Only messages that can be fetched again by sequence
//! number get an id:
//!
//! ```text
//! Message::Record    ->  id: <seq>   data: {…}     in the log, can be fetched again
//! Message::Attached  ->  id: <seq>   data: {…}     the projection the stream starts from
//! everything else    ->              data: {…}     best-effort, lost if missed
//! ```
//!
//! Commands go the other way as one `POST` each. A WebSocket would save a connection, but needs a
//! handshake, frame masking and fragmentation (or a dependency), plus reconnection code.
//!
//! # Security
//!
//! There is no authentication or encryption: anyone who can reach the page can drive the session,
//! including its `shell` tool. So [`Web::bind`] only listens on loopback, or on an IP address in a
//! private range (see [`Reach`]), for a phone on the same network. In the second case
//! [`Web::exposure`] returns a warning to show the user. Wildcards, public addresses and host names
//! are refused. From further away, use a tunnel that authenticates, such as `ssh -L`. `--serve`
//! stays loopback-only: its clients are programs, which can use a tunnel.
//!
//! Requests from other web pages open in the same browser are refused by `foreign`, and anything
//! not addressed beneath the page's key - a random path the program prints as part of the page's
//! address - is refused by `beneath`. The page can answer permission questions, so its port is
//! closed to the commands this process confines (see `Sandbox::closed`), the same as a served
//! session's port.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
        ReadHalf, WriteHalf,
    },
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

use crate::remote::{
    Connection,
    protocol::{self, Command},
};

/// The page, which is the whole client.
const PAGE: &str = include_str!("browser.html");

/// How long a browser waits before reconnecting a dropped stream, in milliseconds.
///
/// note: sent once at the top of every stream. The browser's default is a few seconds and this is
/// a session somebody is watching, so it is worth being quicker - and the reconnection costs only
/// the records that were missed, because `Last-Event-ID` says where to start.
const RETRY: u64 = 1_000;

/// The longest request head this will read, in bytes.
const MAX_HEAD: usize = 16 * 1024;

/// The longest body, which is one command.
const MAX_BODY: usize = 1024 * 1024;

/// Where every attached tab's commands go.
type Tabs = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Command>>>>;

/// The session's own name, read off the first projection that goes past.
///
/// note: remembered here because a browser's own reconnection opens a *new* connection to the
/// session, which therefore has no projection of its own to read the name off - and a resume that
/// cannot say which session it came from is the one `Command::Attach` describes as the quiet
/// failure. The browser keeps its resume with no client code at all; naming the session is the
/// relay's half of it.
type Named = Arc<Mutex<Option<String>>>;

/// Receives errors from individual browser connections, which would otherwise go unreported.
type Said = Arc<dyn Fn(String) + Send + Sync>;

/// How many wrong keys are taken before the page closes for [`PAUSE`].
const GUESSES: u32 = 100;

/// How long the page refuses everything once [`GUESSES`] wrong keys have been tried.
const PAUSE: std::time::Duration = std::time::Duration::from_secs(60);

/// Wrong keys tried so far, and until when the page is closed for them; see [`Guessing::closed`].
type Guesses = Arc<Mutex<Guessing>>;

#[derive(Default)]
struct Guessing {
    wrong: u32,
    closed_until: Option<std::time::Instant>,
}

impl Guessing {
    /// Whether the page is closed to everybody, the right key included, for wrong ones.
    ///
    /// note: the key is 32 bits, so that somebody can type it, and 32 bits is a few billion
    /// guesses - which a device on the same network can make in days, one request at a time. A
    /// hundred a minute is the better part of a century for half of them. The right key is refused
    /// with the rest while it is closed, because a guess that was checked while closed would be a
    /// guess that was not counted. It is a minute of a page that does not answer for the person it
    /// is theirs, which is what something on their network guessing at it ought to cost them
    /// anyway: it is said where they will read it.
    fn closed(&mut self, now: std::time::Instant) -> bool {
        match self.closed_until {
            Some(until) if now < until => true,
            _ => {
                self.closed_until = None;
                false
            }
        }
    }

    /// Counts one wrong key, and answers whether it is the one that closes the page.
    fn wrong(&mut self, now: std::time::Instant) -> bool {
        self.wrong += 1;
        if self.wrong < GUESSES {
            return false;
        }
        self.wrong = 0;
        self.closed_until = Some(now + PAUSE);

        true
    }
}

/// A listening page, ready to relay browsers to a session.
///
/// Binding is separate from [`Web::run`] so that a bad address is reported at startup, before the
/// session is set up.
pub struct Web {
    listener: TcpListener,
    /// The session's address, as `--connect` takes it.
    session: String,
    /// The listening port, closed to confined commands until this is dropped.
    port: u16,
    /// The path segment every route is beneath; see [`beneath`].
    key: Arc<str>,
    /// Who can reach the page.
    reach: Reach,
}

/// Who can reach a page, given the address it listens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reach {
    /// Only this machine (a loopback address).
    Machine,
    /// Any device on the local network (an address in 10/8, 172.16/12, 192.168/16 or fc00::/7).
    Network,
}

/// Classifies an address, or returns `None` if the page may not listen on it. IPv4-mapped IPv6
/// addresses are treated as the IPv4 address they contain.
fn reach(ip: std::net::IpAddr) -> Option<Reach> {
    match ip.to_canonical() {
        ip if ip.is_loopback() => Some(Reach::Machine),
        std::net::IpAddr::V4(v4) if v4.is_private() => Some(Reach::Network),
        std::net::IpAddr::V6(v6) if v6.is_unique_local() => Some(Reach::Network),
        _ => None,
    }
}

impl Web {
    /// Listens on `listen` and will relay browsers to the session at `session` (`unix:PATH` or
    /// `tcp:HOST:PORT`).
    ///
    /// # Errors
    ///
    /// If `session` is not a valid address; if `listen` is not a loopback address, or is not a
    /// private-network IP address typed as such; or if listening fails.
    pub async fn bind(listen: &str, session: &str) -> Result<Self, String> {
        // check the session address once, instead of on every browser connection
        protocol::address(session)?;
        let typed = listen.parse::<std::net::SocketAddr>().ok();
        let address = match typed {
            Some(address) => address,
            None => tokio::net::lookup_host(listen)
                .await
                .map_err(|e| format!("could not resolve `{listen}`: {e}"))?
                .next()
                .ok_or_else(|| format!("could not resolve `{listen}`"))?,
        };
        let reach = match reach(address.ip()) {
            // beyond loopback, only a typed IP address: what a host name resolves to, and which of
            // its addresses comes first, isn't up to the user
            reached if typed.is_none() && reached != Some(Reach::Machine) => {
                return Err(format!(
                    "`{listen}` is a host name; to listen beyond this machine, give an IP address, \
                     e.g. `192.168.1.5:8080`"
                ));
            }
            Some(reach) => reach,
            None if address.ip().is_unspecified() => {
                return Err(format!(
                    "{address} would listen on every interface, including public ones. Give a \
                     specific address: `127.0.0.1:PORT`, or this machine's address on the local \
                     network"
                ));
            }
            None => {
                return Err(format!(
                    "{address} is not a loopback or private-network address. The page has no \
                     authentication, so it only listens on `127.0.0.1:PORT` or a private address \
                     (10/8, 172.16/12, 192.168/16, fc00::/7); from further away, use a tunnel such \
                     as `ssh -L`"
                ));
            }
        };
        let listener = TcpListener::bind(address)
            .await
            .map_err(|e| format!("could not listen on {address}: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("could not read the address of {address}: {e}"))?
            .port();
        // the page can answer permission questions, so close its port to confined commands, as
        // for a served session's port
        let key = fresh_key().map_err(|e| format!("could not make the page's key: {e}"))?;
        crate::sandbox::serving_on(port);

        Ok(Self {
            listener,
            session: session.to_owned(),
            port,
            key: key.into(),
            reach,
        })
    }

    /// Who can reach the page.
    pub fn reach(&self) -> Reach {
        self.reach
    }

    /// A warning to show when the page is reachable from the local network; `None` on loopback.
    pub fn exposure(&self) -> Option<String> {
        match self.reach {
            Reach::Machine => None,
            Reach::Network => Some(format!(
                "the page is reachable from the local network at {} with no encryption, and \
                 nothing but the key in its address to keep anybody out: any device on the network \
                 that learns the address, which includes one reading the network's traffic, can \
                 control this session, including running shell commands as you",
                self.address()
            )),
        }
    }

    /// The page's URL, e.g. `http://127.0.0.1:8080/3fa91c07`, its key included. Read from the socket,
    /// so it has the actual port when bound to port 0.
    pub fn address(&self) -> String {
        match self.listener.local_addr() {
            // note: without a trailing slash, which nobody types; the page is served at the key
            // either way, and builds its routes from wherever it was opened
            Ok(at) => format!("http://{at}/{}", self.key),
            Err(_) => format!("port {}", self.port),
        }
    }

    /// Serves browsers until the future is dropped. Errors on individual connections are passed
    /// to `said`.
    pub async fn run(self, said: impl Fn(String) + Send + Sync + 'static) {
        let said: Said = Arc::new(said);
        let tabs: Tabs = Arc::default();
        let named: Named = Arc::default();
        let guesses: Guesses = Arc::default();
        loop {
            // reported, or nothing would explain why pages stop loading (e.g. out of descriptors)
            let browser = match self.listener.accept().await {
                Ok((browser, _)) => browser,
                Err(e) => {
                    said(format!("a browser could not be accepted: {e}"));
                    continue;
                }
            };
            let _ = browser.set_nodelay(true);
            let (session, tabs, named, said, key, guesses) = (
                self.session.clone(),
                tabs.clone(),
                named.clone(),
                said.clone(),
                self.key.clone(),
                guesses.clone(),
            );
            tokio::spawn(async move {
                let served = Served {
                    session: &session,
                    key: &key,
                    guesses: &guesses,
                    said: &said,
                };
                if let Err(e) = serve(browser, served, tabs, named).await {
                    said(format!("a browser connection failed: {e}"));
                }
            });
        }
    }
}

impl Drop for Web {
    /// Reopens the port to confined commands.
    fn drop(&mut self) {
        crate::sandbox::stopped_serving_on(self.port);
    }
}

/// What every connection to one page shares that is not the session's.
#[derive(Clone, Copy)]
struct Served<'a> {
    session: &'a str,
    key: &'a str,
    guesses: &'a Guesses,
    said: &'a Said,
}

/// One browser connection: read a request, answer it, and for a stream stay as long as it lasts.
async fn serve(
    browser: TcpStream,
    served: Served<'_>,
    tabs: Tabs,
    named: Named,
) -> Result<(), String> {
    let Served {
        session,
        key,
        guesses,
        said,
    } = served;
    let (read, mut write) = browser.into_split();
    let mut reader = BufReader::new(read);
    let Some(request) = head(&mut reader).await? else {
        return Ok(());
    };
    let (path, query) = match request.target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (request.target.as_str(), ""),
    };
    let tab = param(query, "tab").unwrap_or_default().to_owned();
    if let Some(refusal) = foreign(&request) {
        return reply(
            &mut write,
            "403 Forbidden",
            "text/plain",
            refusal.as_bytes(),
        )
        .await;
    }
    let now = std::time::Instant::now();
    if guesses
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .closed(now)
    {
        return reply(
            &mut write,
            "429 Too Many Requests",
            "text/plain; charset=utf-8",
            CLOSED.as_bytes(),
        )
        .await;
    }
    let Some(path) = beneath(path, key) else {
        // a guess is a path whose first part is as long as the key: the browser's own
        // `/favicon.ico`, or an address opened without its key, is somebody who does not have it
        // rather than somebody trying to find it out, and counting them would close the page on
        // its own person for opening it wrong
        let guessed = path
            .strip_prefix('/')
            .and_then(|rest| rest.split('/').next())
            .is_some_and(|given| given.len() == key.len());
        if guessed && guesses.lock().unwrap_or_else(|e| e.into_inner()).wrong(now) {
            said(format!(
                "{GUESSES} wrong keys have been tried at the page, so it refuses everything for \
                 the next {} seconds. If they were not yours, something on the network is \
                 guessing at it",
                PAUSE.as_secs()
            ));
        }
        let host = request.header("host").unwrap_or("HOST:PORT");
        return reply(
            &mut write,
            "403 Forbidden",
            "text/plain; charset=utf-8",
            keyless(host).as_bytes(),
        )
        .await;
    };

    match (request.method.as_str(), path) {
        ("GET", "" | "/") => page(&mut write).await,
        // note: the browser's own reconnection, arriving as a resume. `Last-Event-ID` is the last
        // `id:` this tab was sent, which is a record sequence, which is what `attach` takes
        ("GET", "/events") => {
            let since = request
                .header("last-event-id")
                .or_else(|| param(query, "since"))
                .and_then(|it| it.parse().ok());
            stream(&mut write, session, tabs, named, tab, since).await
        }
        ("POST", "/do") => {
            // a command that cannot be read is answered rather than hung up on, so that the page
            // reports a refusal and not a network that failed
            let command = match body(&mut reader, &request).await.and_then(|body| {
                serde_json::from_slice::<Command>(&body)
                    .map_err(|e| format!("that is not a command: {e}"))
            }) {
                Ok(command) => command,
                Err(e) => {
                    return reply(&mut write, "400 Bad Request", "text/plain", e.as_bytes()).await;
                }
            };
            let sent = tabs
                .lock()
                .expect("the tabs are not poisoned")
                .get(tab.as_str())
                .is_some_and(|to| to.send(command).is_ok());
            match sent {
                true => reply(&mut write, "202 Accepted", "text/plain", b"").await,
                // the stream this tab was talking through has gone; the browser will open another
                false => reply(&mut write, "409 Conflict", "text/plain", b"no such tab").await,
            }
        }
        _ => reply(&mut write, "404 Not Found", "text/plain", b"no").await,
    }
}

/// Why a request did not come from this page, if it did not.
///
/// note: a port on loopback is not a boundary a browser keeps. Any page open in the same browser
/// can put `/events` in a frame to open a tab and then post to `/do` - a POST with a text body is a
/// simple request and needs no preflight - which is a line typed into the session, or a permission
/// answered, by a page nobody meant to give either. And with no look at `Host`, a page whose name
/// is made to resolve here is same-origin with this server and can read the stream as well. So the
/// host has to be an address or `localhost` rather than a name, which is what a rebinding needs; a
/// request that says where it came from has to have come from here; and a command has to say it is
/// JSON, which a page elsewhere cannot send without a preflight this server never answers.
///
/// note: `Origin` is not on every request. A browser leaves it off a page's own `EventSource`, and
/// off a frame, an image or a no-cors fetch from anywhere else, so it cannot tell those apart; and
/// opening `/events` takes the session from the tab that had it, which stops for good. A browser
/// that says where a request came from with `Sec-Fetch-Site` is held to it: this page, or nowhere
/// at all, which is an address typed or opened from the terminal.
fn foreign(request: &Request) -> Option<&'static str> {
    let Some(host) = request.header("host") else {
        return Some("a request names the host it is for");
    };
    let name = match host.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    if name.parse::<std::net::IpAddr>().is_err() && !name.eq_ignore_ascii_case("localhost") {
        return Some("this is reached by address, not by a name");
    }
    if request
        .header("origin")
        .is_some_and(|origin| origin != format!("http://{host}"))
    {
        return Some("this answers its own page and no other");
    }
    if request
        .header("sec-fetch-site")
        .is_some_and(|site| site != "same-origin" && site != "none")
    {
        return Some("this answers its own page and no other");
    }
    if request.method == "POST"
        && !request
            .header("content-type")
            .is_some_and(|kind| kind.starts_with("application/json"))
    {
        return Some("a command is JSON, and says so");
    }

    None
}

/// The route a path names beneath the page's key, or `None` where it is not beneath it.
///
/// note: what `foreign` cannot cover. That rests on the browser saying where a request came from,
/// and one old enough to send neither `Origin` nor `Sec-Fetch-Site` let another page's image or
/// no-cors fetch open `/events` and take the session from the tab that had it. A page elsewhere
/// cannot guess a path it was never shown, and the program prints it only where the person reads
/// it. Compared without stopping at the first difference, so the time an answer takes says nothing
/// of how much of a guess was right.
fn beneath<'a>(path: &'a str, key: &str) -> Option<&'a str> {
    let rest = path.strip_prefix('/')?;
    let given = rest.split('/').next().unwrap_or_default();
    let same = given.len() == key.len()
        && given
            .bytes()
            .zip(key.bytes())
            .fold(0u8, |differ, (a, b)| differ | (a ^ b))
            == 0;

    same.then(|| &rest[given.len()..])
}

/// What a browser is shown for an address without the page's key, or with a wrong one.
///
/// note: the shape of the address rather than the address, which would be the key handed to
/// whoever asked without it.
fn keyless(host: &str) -> String {
    format!(
        "This address is missing the page's key.\n\n\
         Open the whole address kamchatka printed when it started: http://{host}/ followed by \
         eight letters and digits. Or scan the QR code it showed, or type /web in the \
         session to show both again.\n"
    )
}

/// What a browser is shown while the page is closed for wrong keys; see [`Guessing::closed`].
const CLOSED: &str = "Too many wrong keys have been tried, so the page refuses everything for a \
                      minute.\n\nIf that was not you, something on this network is guessing at \
                      it.\n";

/// A key for the page: 32 random bits, as eight hex digits.
///
/// note: 32 rather than 128 so that it can be typed on a phone; [`Guessing`] is what keeps 32
/// enough.
fn fresh_key() -> std::io::Result<String> {
    let mut bytes = [0u8; 4];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut bytes)?;

    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Serves the client, which is one file and no build step.
async fn page<W: AsyncWrite + Unpin>(write: &mut W) -> Result<(), String> {
    reply(write, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes()).await
}

/// Opens a connection to the session and relays it to this browser for as long as either lasts.
///
/// note: one connection to the session per stream, rather than one shared between tabs. The
/// protocol answers every command exactly once and in order *on a connection*, so two browsers
/// sharing one would be reading each other's answers. A session serves one client at a time, so a
/// second tab takes it from the first, which is told so and stops - see `browser.html`.
async fn stream<W: AsyncWrite + Unpin>(
    write: &mut W,
    session: &str,
    tabs: Tabs,
    named: Named,
    tab: String,
    since: Option<u64>,
) -> Result<(), String> {
    // note: the whole attach happens before the head goes out, which is what makes the retry below
    // possible: nothing has been said to the browser yet, so a second attempt is the first one it
    // hears about. A `200` on this route means "attached", and it is true when it is sent
    let Some(Upstream {
        mut up,
        mut down,
        first,
    }) = attach(session, since, &named, write).await?
    else {
        return Ok(());
    };

    // note: no `Content-Length` and no chunking - a response with neither is delimited by the
    // connection closing, which is what a stream is. It is the one thing this saves by not being a
    // real HTTP server, and it is exactly what `EventSource` expects
    write
        .write_all(
            b"HTTP/1.1 200 OK\r\n\
              Content-Type: text/event-stream\r\n\
              Cache-Control: no-cache\r\n\
              Connection: close\r\n\r\n",
        )
        .await
        .map_err(|e| e.to_string())?;
    write
        .write_all(format!("retry: {RETRY}\n\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    if !forward(write, &first, &named).await? {
        return Ok(());
    }

    let (to_session, mut commands) = mpsc::unbounded_channel();
    let mine = to_session.clone();
    tabs.lock()
        .expect("the tabs are not poisoned")
        .insert(tab.clone(), to_session);
    // whatever this tab posts, written to the session it is attached to
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            if protocol::write(&mut down, &command).await.is_err() {
                return;
            }
        }
    });

    let relayed = relay(write, &mut up, &named).await;
    // the browser has gone, or the session has. Dropping the sender ends the task above, which
    // closes the connection to the session, which is what makes the session say the client left
    //
    // note: removed only where the entry is still *this* stream's. A browser reconnecting under
    // the same tab id before this relay notices its socket is gone - a half-open TCP, which is the
    // case the session's own keepalive exists for - puts a live sender in the map, and a blind
    // `remove` would take that one out instead, leaving `POST /do` answering `409 no such tab` to
    // every line typed until the browser reconnects again
    let mut open = tabs.lock().expect("the tabs are not poisoned");
    if open.get(&tab).is_some_and(|to| to.same_channel(&mine)) {
        open.remove(&tab);
    }

    relayed
}

/// A connection to the session with its attach already answered.
struct Upstream {
    /// What the session says, framed.
    up: protocol::Frames<BufReader<ReadHalf<Connection>>>,
    /// What this end says back.
    down: WriteHalf<Connection>,
    /// The answer to the attach, which the browser is owed like every message after it.
    first: serde_json::Value,
}

/// Opens a connection to the session and attaches to it, starting again from nothing where the
/// watermark this tab is holding belongs to a session that is not there any more.
///
/// note: what `remote::Client` does with the same answer, and for the same reason - a relay is a
/// client. `/restart` leaves a session of its own behind this address, so a resume naming the one
/// before it, or numbered from its log, is refused, which is the check doing its job. Passed on to
/// the page, that refusal is a `failed` line, a closed stream, and a reconnection a second later
/// carrying the same `Last-Event-ID`, for as long as the tab is open - and the new session, whose
/// first line says where the old one was written, is never seen. Only a fresh attach can succeed,
/// so this is where it is made: the `attached` that answers one carries the new session's `seq`,
/// which is the `id:` that puts the browser's own watermark back where it belongs.
///
/// note: the answer is read here rather than left to the relay, because it is the only place both
/// halves of the retry are still to hand - the connection to replace and the watermark to drop -
/// and it is handed back so that the browser is told what it said like anything else.
///
/// note: a version refusal is passed on untouched, because nothing mends it: attaching again is
/// refused for the same reason, and `remote::Client` gives up on it for the same one.
async fn attach<W: AsyncWrite + Unpin>(
    session: &str,
    since: Option<u64>,
    named: &Named,
    write: &mut W,
) -> Result<Option<Upstream>, String> {
    let mut since = since;
    loop {
        // connect the way `--connect` does, so a `unix:` session works too
        let upstream = match crate::remote::client::connect(session).await {
            Ok(connection) => connection,
            Err(e) => {
                reply(write, "502 Bad Gateway", "text/plain", e.as_bytes()).await?;

                return Ok(None);
            }
        };
        let (up, mut down) = tokio::io::split(upstream);
        let mut up = protocol::Frames::new(BufReader::new(up));
        // taken out of the lock before the write rather than inside the call, because a guard held
        // across an `await` is a future that cannot be sent between threads
        let was = named.lock().expect("the name is not poisoned").clone();
        protocol::write(
            &mut down,
            &Command::Attach {
                since,
                session: was,
                version: Some(protocol::VERSION),
            },
        )
        .await?;

        // a session that took the connection and went away before answering it. Said as a gateway
        // error rather than as an empty reply, because that is what it is, and the browser opens
        // another in a second either way
        let Some(first) = protocol::read::<serde_json::Value>(&mut up).await? else {
            let said = format!("the session at {session} closed without answering the attach");
            reply(write, "502 Bad Gateway", "text/plain", said.as_bytes()).await?;

            return Ok(None);
        };
        let refused = first.get("is").and_then(serde_json::Value::as_str) == Some("failed")
            && first.get("about").and_then(serde_json::Value::as_str) == Some("attach");
        // `since` and not the refusal alone, so there are two attempts and not a loop: an attach
        // carrying no watermark cannot be refused for one
        if refused && since.is_some() {
            *named.lock().expect("the name is not poisoned") = None;
            since = None;
            continue;
        }

        return Ok(Some(Upstream { up, down, first }));
    }
}

/// Turns every message the session sends into one event, until one end stops.
async fn relay<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    write: &mut W,
    up: &mut protocol::Frames<R>,
    named: &Named,
) -> Result<(), String> {
    // note: read as JSON rather than as a `Message`, and passed on as it arrived. A relay that
    // parsed each message into this build's own enum and wrote it out again would turn everything
    // a later session said into `Message::Unknown` on the way past - which is the relay deciding
    // what the page is allowed to hear. What it has to understand is the tag and one number
    while let Some(message) = protocol::read::<serde_json::Value>(up).await? {
        if !forward(write, &message, named).await? {
            return Ok(());
        }
    }

    Ok(())
}

/// Writes one message out as one event; `false` where the browser has gone.
///
/// note: a function rather than the body of the loop above, because the first message off a fresh
/// connection is read by [`attach`] and owes the page exactly what every one after it does - the
/// same `id:`, and the same note of what the session is called.
async fn forward<W: AsyncWrite + Unpin>(
    write: &mut W,
    message: &serde_json::Value,
    named: &Named,
) -> Result<bool, String> {
    let is = message.get("is").and_then(serde_json::Value::as_str);
    // the one place the session says what it is called, and what the next stream's resume has
    // to name; see `Named`
    if is == Some("attached")
        && let Some(session) = message.get("session").and_then(serde_json::Value::as_str)
    {
        *named.lock().expect("the name is not poisoned") = Some(session.to_owned());
    }
    // note: the whole mapping. An `id:` is what a browser resumes from, so it goes on exactly the
    // messages that *can* be resumed from - which is the numbered ones, which is the ones in the
    // log
    //
    // note: `projected` carries a `seq` that would be a valid one. It is an answer to a command
    // rather than a place in the stream, and a browser resuming from it would be resuming from a
    // message nobody can ask for again by number. The rule is what can be *re-sent*, not what has
    // a number on it
    let id = matches!(is, Some("record" | "attached"))
        .then(|| message.get("seq").and_then(serde_json::Value::as_u64))
        .flatten();
    let data = serde_json::to_string(message).map_err(|e| e.to_string())?;
    let event = match id {
        Some(id) => format!("id: {id}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    };
    // a browser that has gone is not an error, it is a browser that has gone
    if write.write_all(event.as_bytes()).await.is_err() {
        return Ok(false);
    }
    write.flush().await.map_err(|e| e.to_string())?;

    Ok(true)
}

/// One request's first line and headers.
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl Request {
    /// What a header says, by its lowercased name.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Reads a request line and its headers, or `None` where the connection just closed.
async fn head<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Request>, String> {
    // note: capped while each line arrives rather than once it has, for every line of the head and
    // not only the first. `read_line` grows its buffer until a newline comes, so a limit checked
    // against what it handed back is no defence against the case it is there for - a peer that
    // never sends one. `take` stops at the limit whether or not a newline arrived, which is why
    // each read is followed by the question of which of the two happened
    let mut line = String::new();
    if tokio::io::AsyncReadExt::take(&mut *reader, MAX_HEAD as u64)
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?
        == 0
    {
        return Ok(None);
    }
    if !line.ends_with('\n') {
        return Err("a request line longer than anybody meant".to_owned());
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err("that is not a request".to_owned());
    };
    let (method, target) = (method.to_owned(), target.to_owned());

    let mut headers = Vec::new();
    let mut read = line.len();
    loop {
        // the allowance is spent, which is its own answer: a `take(0)` reads nothing and nothing
        // is what the end of a head looks like
        if read >= MAX_HEAD {
            return Err("a request head longer than anybody meant".to_owned());
        }
        let mut line = String::new();
        if tokio::io::AsyncReadExt::take(&mut *reader, (MAX_HEAD - read) as u64)
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?
            == 0
        {
            break;
        }
        read += line.len();
        if !line.ends_with('\n') {
            return Err("a request head longer than anybody meant".to_owned());
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }

    Ok(Some(Request {
        method,
        target,
        headers,
    }))
}

/// Reads the body a request said it was sending.
async fn body<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    request: &Request,
) -> Result<Vec<u8>, String> {
    let length: usize = request
        .header("content-length")
        .and_then(|it| it.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err(format!("a body of {length} bytes, which is not a command"));
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|e| format!("the body stopped early: {e}"))?;

    Ok(body)
}

/// Writes one whole response.
///
/// note: no page may put one of these in a frame. A framed copy of the page opens its own stream,
/// which takes the session, and stands under whatever the page around it draws over it. The
/// header cannot stop a request that is not a frame - an image or a no-cors fetch of `/events`
/// takes the session with nothing rendered - which is what `foreign` is for.
async fn reply<W: AsyncWrite + Unpin>(
    write: &mut W,
    status: &str,
    kind: &str,
    body: &[u8],
) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Content-Security-Policy: frame-ancestors 'none'\r\nX-Frame-Options: DENY\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    write
        .write_all(head.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    write.write_all(body).await.map_err(|e| e.to_string())?;
    write.flush().await.map_err(|e| e.to_string())
}

/// One value out of a query string.
///
/// note: no decoding, and nothing here needs any: the two parameters are a sequence number and an
/// identifier the page made out of its own alphabet. A gateway that grew a route taking anything a
/// person typed would need a real one.
fn param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key is eight hex digits, and only it opens a route beneath it: a key one bit off does
    /// not, and neither does one whose differences cancel out under XOR.
    #[test]
    fn only_the_key_opens_a_route_beneath_it() {
        let key = fresh_key().expect("a key");
        assert_eq!(key.len(), 8);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()), "{key}");
        assert_eq!(beneath(&format!("/{key}/events"), &key), Some("/events"));
        assert_eq!(beneath("/events", &key), None);
        assert_eq!(beneath("/00000000/events", "00000001"), None);
        // two bytes that each differ from the key's by one bit cancel out if the differences are
        // combined by XOR rather than OR, which would take this for the key
        assert_eq!(beneath("/00000010/events", "00000001"), None);
    }

    /// A hundred wrong keys close the page to everybody for a minute, and then it opens again.
    #[test]
    fn wrong_keys_close_the_page_for_a_while() {
        let mut guessing = Guessing::default();
        let now = std::time::Instant::now();
        for _ in 1..GUESSES {
            assert!(!guessing.wrong(now));
            assert!(!guessing.closed(now));
        }
        assert!(guessing.wrong(now), "the hundredth closes it");
        assert!(guessing.closed(now));
        assert!(guessing.closed(now + PAUSE - std::time::Duration::from_secs(1)));
        assert!(!guessing.closed(now + PAUSE));
        // and the count starts again rather than closing it on the next wrong key
        assert!(!guessing.wrong(now + PAUSE));
    }

    /// A page reachable from the local network says so, and where.
    #[tokio::test]
    async fn a_page_on_the_network_warns_where_it_can_be_reached() {
        let mut web = Web::bind("127.0.0.1:0", "tcp:127.0.0.1:1")
            .await
            .expect("it listens");
        assert_eq!(web.exposure(), None);

        web.reach = Reach::Network;
        let warned = web.exposure().expect("a warning");
        assert!(
            warned.contains("reachable from the local network"),
            "{warned}"
        );
        assert!(warned.contains(&web.address()), "{warned}");
    }

    /// Only an address whose first part is as long as the key counts as a guess at it: a hundred
    /// requests for the browser's own icon leave the page open, and a hundred wrong keys close it.
    #[tokio::test]
    async fn only_a_wrong_key_counts_towards_closing_the_page() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let web = Web::bind("127.0.0.1:0", "tcp:127.0.0.1:1")
            .await
            .expect("it listens");
        let at = web.listener.local_addr().expect("its address");
        let (told, mut telling) = tokio::sync::mpsc::unbounded_channel();
        let running = tokio::spawn(web.run(move |said| {
            let _ = told.send(said);
        }));
        // the status line of the answer to a request for `path`
        let status = |path: &'static str| async move {
            let mut browser = TcpStream::connect(at).await.expect("it answers");
            browser
                .write_all(format!("GET {path} HTTP/1.1\r\nHost: {at}\r\n\r\n").as_bytes())
                .await
                .expect("the request");
            let mut answer = String::new();
            let _ = browser.read_to_string(&mut answer).await;
            answer.lines().next().unwrap_or_default().to_owned()
        };

        for _ in 0..GUESSES {
            assert!(status("/favicon.ico").await.contains("403"));
        }
        assert!(status("/favicon.ico").await.contains("403"), "still open");

        for _ in 0..GUESSES {
            status("/00000000/events").await;
        }
        assert!(status("/favicon.ico").await.contains("429"), "closed");
        let said = telling.try_recv().expect("the closing is said");
        assert!(said.contains("wrong keys have been tried"), "{said}");

        running.abort();
    }

    /// The refusal for an address without the key says what to open instead, and not the key.
    #[test]
    fn a_keyless_address_is_told_what_to_open() {
        let said = keyless("192.168.1.5:8080");
        assert!(said.contains("missing the page's key"), "{said}");
        assert!(said.contains("http://192.168.1.5:8080/"), "{said}");
        assert!(said.contains("/web"), "{said}");
    }

    /// Loopback and private-network addresses are accepted in any notation, and nothing else is.
    #[test]
    fn only_loopback_and_private_addresses_are_accepted() {
        for (address, expected) in [
            ("127.0.0.1", Some(Reach::Machine)),
            ("127.8.9.10", Some(Reach::Machine)),
            ("::1", Some(Reach::Machine)),
            ("::ffff:127.0.0.1", Some(Reach::Machine)),
            ("192.168.1.36", Some(Reach::Network)),
            ("10.0.0.2", Some(Reach::Network)),
            ("172.16.0.1", Some(Reach::Network)),
            ("172.31.255.254", Some(Reach::Network)),
            ("fd73:40c:481a:8::1", Some(Reach::Network)),
            ("::ffff:192.168.1.36", Some(Reach::Network)),
            // just outside 172.16/12
            ("172.15.255.255", None),
            ("172.32.0.1", None),
            // wildcards
            ("0.0.0.0", None),
            ("::", None),
            // public, link-local, and carrier-grade NAT
            ("8.8.8.8", None),
            ("2001:4860:4860::8888", None),
            ("169.254.1.1", None),
            ("fe80::1", None),
            ("100.64.0.1", None),
        ] {
            assert_eq!(
                reach(address.parse().expect("an address")),
                expected,
                "{address}"
            );
        }
    }

    /// A parameter is the value of the pair with exactly its name, and a name that is not there is
    /// nothing at all, not an empty value.
    #[test]
    fn a_parameter_is_the_value_of_its_own_name() {
        assert_eq!(param("tab=first&since=7", "tab"), Some("first"));
        assert_eq!(param("tab=first&since=7", "since"), Some("7"));
        assert_eq!(param("since=7&tab=first", "tab"), Some("first"));
        assert_eq!(param("since=7", "tab"), None);
        assert_eq!(param("", "tab"), None);
        assert_eq!(
            param("tab", "tab"),
            None,
            "a pair without an equals sign is not one"
        );
    }

    /// A body longer than the most a command can be is refused; one exactly that long is read.
    #[tokio::test]
    async fn a_body_is_read_up_to_the_most_a_command_can_be() {
        let request = |length: usize| Request {
            method: "POST".to_owned(),
            target: "/do".to_owned(),
            headers: vec![("content-length".to_owned(), length.to_string())],
        };
        let over = vec![b'x'; MAX_BODY + 1];
        let refused = body(&mut over.as_slice(), &request(MAX_BODY + 1)).await;
        let refused = refused.expect_err("a body one byte over the limit is refused");
        assert!(refused.contains("not a command"), "{refused}");

        let exact = vec![b'x'; MAX_BODY];
        let read = body(&mut exact.as_slice(), &request(MAX_BODY))
            .await
            .expect("a body exactly at the limit is read");
        assert_eq!(read.len(), MAX_BODY);
    }

    /// A request head is read to a few kilobytes, which is what a browser's cookies and a proxy's
    /// headers can add up to.
    #[tokio::test]
    async fn a_request_head_of_a_few_kilobytes_is_read() {
        let padding = "x".repeat(4096);
        let text = format!(
            "GET /3fa91c07/events HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Padding: {padding}\r\n\r\n"
        );
        let mut reader = BufReader::new(text.as_bytes());
        let request = head(&mut reader)
            .await
            .expect("it is read")
            .expect("a request");
        assert_eq!(request.header("x-padding").map(str::len), Some(4096));
    }

    /// A command of a few kilobytes is read, since a command can carry a long line typed at the
    /// page.
    #[tokio::test]
    async fn a_command_of_a_few_kilobytes_is_read() {
        let text = "x".repeat(4096);
        let request = Request {
            method: "POST".to_owned(),
            target: "/3fa91c07/do".to_owned(),
            headers: vec![("content-length".to_owned(), "4096".to_owned())],
        };
        let mut reader = BufReader::new(text.as_bytes());
        let got = body(&mut reader, &request)
            .await
            .expect("a command of that size is read");
        assert_eq!(got.len(), 4096);
    }

    /// A request from this page, which says so with `Sec-Fetch-Site`, is answered; one from
    /// another site is not.
    #[test]
    fn a_request_says_where_it_came_from_and_only_another_site_is_refused() {
        let from = |site: &str| Request {
            method: "GET".to_owned(),
            target: "/3fa91c07/events".to_owned(),
            headers: vec![
                ("host".to_owned(), "127.0.0.1:8080".to_owned()),
                ("sec-fetch-site".to_owned(), site.to_owned()),
            ],
        };
        assert_eq!(foreign(&from("same-origin")), None);
        assert_eq!(foreign(&from("none")), None);
        assert!(foreign(&from("cross-site")).is_some());
        assert!(foreign(&from("same-site")).is_some());
    }

    /// A session on a loopback port that answers each attach in turn with the next of `answers`,
    /// and then closes every attach it is sent without answering. What each attach asked for is
    /// kept, in the order they came.
    async fn scripted(
        answers: Vec<serde_json::Value>,
    ) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
        // a socket file, which `connect` takes as well as a port
        let path = std::env::temp_dir().join(format!("web-attach-{}", fresh_key().expect("a key")));
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).expect("a socket file");
        let address = format!("unix:{}", path.display());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        tokio::spawn(async move {
            let mut answers = answers.into_iter();
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let (read, mut write) = socket.into_split();
                let mut line = String::new();
                if BufReader::new(read).read_line(&mut line).await.is_err() {
                    continue;
                }
                seen.lock()
                    .expect("not poisoned")
                    .push(serde_json::from_str(&line).expect("an attach"));
                if let Some(answer) = answers.next() {
                    let mut said = answer.to_string();
                    said.push('\n');
                    let _ = write.write_all(said.as_bytes()).await;
                }
            }
        });
        (address, asked)
    }

    /// A request head of exactly `total` bytes: a request line, one header padding it out, and the
    /// blank line that ends it.
    fn head_of(total: usize) -> Vec<u8> {
        let request = "GET / HTTP/1.1\r\n";
        let blank = "\r\n";
        let pad = total - request.len() - blank.len() - "X-Pad: \r\n".len();
        format!("{request}X-Pad: {}\r\n{blank}", "a".repeat(pad)).into_bytes()
    }

    /// A resume that the session refuses for its watermark is attached again without it, once,
    /// and the browser is told only the answer to the second attach.
    #[tokio::test]
    async fn a_refused_resume_is_retried_once_without_its_watermark() {
        let (address, asked) = scripted(vec![
            serde_json::json!({"is": "failed", "about": "attach", "error": "no such record"}),
            serde_json::json!({"is": "done"}),
        ])
        .await;
        let named: Named = Arc::new(Mutex::new(Some("old".to_owned())));
        let mut written = Vec::new();
        let attached = attach(&address, Some(7), &named, &mut written).await;
        match attached {
            Ok(Some(Upstream { first, .. })) => assert_eq!(first["is"], "done"),
            _ => panic!("the second attach should be the one answered"),
        }
        let asked = asked.lock().expect("not poisoned");
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(asked[0]["since"], 7);
        assert_eq!(asked[0]["session"], "old");
        assert!(asked[1]["since"].is_null(), "{asked:?}");
        assert!(asked[1]["session"].is_null(), "{asked:?}");
    }

    /// Only a refusal of an attach is retried: a message that is not a refusal, and a refusal about
    /// anything else such as the version, are both passed on as the answer.
    #[tokio::test]
    async fn only_a_refusal_of_the_attach_is_retried() {
        for answer in [
            serde_json::json!({"is": "done", "about": "attach"}),
            serde_json::json!({"is": "failed", "about": "version", "error": "too old"}),
        ] {
            let (address, asked) = scripted(vec![answer.clone()]).await;
            let named: Named = Arc::default();
            let mut written = Vec::new();
            let attached = attach(&address, Some(7), &named, &mut written).await;
            match attached {
                Ok(Some(Upstream { first, .. })) => assert_eq!(first, answer),
                _ => panic!("{answer} should be passed on"),
            }
            assert_eq!(asked.lock().expect("not poisoned").len(), 1, "{answer}");
        }
    }

    /// A refusal to a resume that had no watermark is passed on, because attaching again with none
    /// would be refused for the same reason.
    #[tokio::test]
    async fn a_refusal_with_no_watermark_is_passed_on() {
        let refused = serde_json::json!({"is": "failed", "about": "attach", "error": "no"});
        let (address, asked) = scripted(vec![refused.clone()]).await;
        let named: Named = Arc::default();
        let mut written = Vec::new();
        match attach(&address, None, &named, &mut written).await {
            Ok(Some(Upstream { first, .. })) => assert_eq!(first, refused),
            _ => panic!("a refusal with no watermark should be passed on"),
        }
        assert_eq!(asked.lock().expect("not poisoned").len(), 1);
    }

    /// The session names itself on its `attached` message, and on nothing else: the name is what
    /// the next attach resumes under.
    #[tokio::test]
    async fn the_session_is_named_by_its_attached_message_only() {
        let named: Named = Arc::default();
        let mut written = Vec::new();
        let record = serde_json::json!({"is": "record", "seq": 4, "session": "other"});
        assert!(
            forward(&mut written, &record, &named)
                .await
                .expect("written")
        );
        assert_eq!(named.lock().expect("not poisoned").clone(), None);

        let attached = serde_json::json!({"is": "attached", "session": "mine", "seq": 5});
        assert!(
            forward(&mut written, &attached, &named)
                .await
                .expect("written")
        );
        assert_eq!(
            named.lock().expect("not poisoned").clone(),
            Some("mine".to_owned())
        );
    }

    /// Every message the session sends is passed on, not only the first, until the session stops.
    #[tokio::test]
    async fn the_relay_passes_on_every_message_until_the_session_stops() {
        let wire: &[u8] = b"{\"is\":\"record\",\"seq\":1}\n{\"is\":\"record\",\"seq\":2}\n";
        let named: Named = Arc::default();
        let mut up = protocol::Frames::new(BufReader::new(wire));
        let mut written = Vec::new();
        relay(&mut written, &mut up, &named).await.expect("relayed");
        let said = String::from_utf8(written).expect("utf8");
        assert!(
            said.contains("id: 1\n") && said.contains("id: 2\n"),
            "{said}"
        );
    }

    /// A head of exactly the allowance is read whole, and one byte more is refused.
    #[tokio::test]
    async fn the_head_is_read_to_its_allowance_and_no_further() {
        let fits = head_of(MAX_HEAD);
        assert!(
            head(&mut BufReader::new(&fits[..]))
                .await
                .expect("read")
                .is_some()
        );
        let over = head_of(MAX_HEAD + 1);
        assert!(head(&mut BufReader::new(&over[..])).await.is_err());
    }

    /// A header longer than an even share of the allowance is still read whole.
    #[tokio::test]
    async fn a_long_header_is_read_whole() {
        let wire = head_of(3_000);
        let request = head(&mut BufReader::new(&wire[..]))
            .await
            .expect("read")
            .expect("a request");
        let around = "GET / HTTP/1.1\r\nX-Pad: \r\n\r\n".len();
        assert_eq!(request.header("x-pad").map(str::len), Some(3_000 - around));
    }
}
