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
//! Requests from other web pages open in the same browser are refused by `foreign`. The page can
//! answer permission questions, so its port is closed to the commands this process confines (see
//! `Sandbox::closed`), the same as a served session's port.

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
        crate::sandbox::serving_on(port);

        Ok(Self {
            listener,
            session: session.to_owned(),
            port,
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
                "the page is reachable from the local network at {} with no authentication or \
                 encryption: any device on the network can control this session, including running \
                 shell commands as you, and can read its traffic",
                self.address()
            )),
        }
    }

    /// The page's URL, e.g. `http://127.0.0.1:8080/`. Read from the socket, so it has the actual
    /// port when bound to port 0.
    pub fn address(&self) -> String {
        match self.listener.local_addr() {
            Ok(at) => format!("http://{at}/"),
            Err(_) => format!("port {}", self.port),
        }
    }

    /// Serves browsers until the future is dropped. Errors on individual connections are passed
    /// to `said`.
    pub async fn run(self, said: impl Fn(String) + Send + Sync + 'static) {
        let said: Said = Arc::new(said);
        let tabs: Tabs = Arc::default();
        let named: Named = Arc::default();
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
            let (session, tabs, named, said) = (
                self.session.clone(),
                tabs.clone(),
                named.clone(),
                said.clone(),
            );
            tokio::spawn(async move {
                if let Err(e) = serve(browser, &session, tabs, named).await {
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

/// One browser connection: read a request, answer it, and for a stream stay as long as it lasts.
async fn serve(browser: TcpStream, session: &str, tabs: Tabs, named: Named) -> Result<(), String> {
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

    match (request.method.as_str(), path) {
        ("GET", "/") => page(&mut write).await,
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
}
