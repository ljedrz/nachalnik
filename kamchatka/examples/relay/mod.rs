//! The HTTP half of putting a session in a browser: three routes, no framework, no router.
//!
//! note: a module rather than a part of `gateway.rs`, because two examples want it and only one of
//! them is about it. `gateway.rs` is a relay to a session somebody else is running, which is the
//! claim worth making on its own; `phone.rs` is a session and a relay in one process, which is the
//! convenience. Neither is the other, and an `#[path]`-free `mod relay;` is what stops them being
//! two copies - the same trick `tests/common` uses - and cargo builds no example out of this
//! directory, because there is no `main.rs` in it.
//!
//! note: why this is `text/event-stream` rather than a WebSocket is `gateway.rs`'s header: an SSE
//! `id:` is a record sequence and `Last-Event-ID` is `attach { since }`, so a browser implements
//! resume with no client code at all.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use kamchatka::remote::protocol::{self, Address, Command};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::mpsc,
};

/// The page, which is the whole client.
const PAGE: &str = include_str!("../browser.html");

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
/// gateway's half of it.
type Named = Arc<Mutex<Option<String>>>;

/// Serves the page and the stream to browsers, relaying each to `session`, until stopped.
pub async fn run(session: &str, listen: &str) -> Result<(), String> {
    let session = session.to_owned();
    // the session is reached the way any other client reaches it, and this checks the spelling here
    // rather than on every connection
    let Address::Tcp(_) = protocol::address(&session)? else {
        return Err(
            "this gateway speaks to a port; serve with `--serve tcp:127.0.0.1:PORT`".into(),
        );
    };
    let listener = TcpListener::bind(&listen)
        .await
        .map_err(|e| format!("could not listen on {listen}: {e}"))?;
    let at = listener.local_addr().map_err(|e| e.to_string())?;
    // note: `0.0.0.0` is not an address anybody can type into a phone, and printing it as though it
    // were is the first thing somebody reads when they run this for exactly that reason. A wildcard
    // bind says what it is instead, and leaves finding the address to `ip addr`, which knows
    println!(
        "· a browser reaches {session} at {}",
        match at.ip().is_unspecified() {
            true => format!("port {} on every address this machine has", at.port()),
            false => format!("http://{at}/"),
        }
    );
    if !at.ip().is_loopback() {
        println!(
            "· {at} is not a loopback address, and there is no authentication or encryption here: \
             anything that can reach this page can run the `shell` tool as you, and can read every \
             word of the session on the way past. That is a thing to do on a network you trust, \
             for as long as you are watching it."
        );
    }

    let tabs: Tabs = Arc::default();
    let named: Named = Arc::default();
    loop {
        let Ok((browser, _)) = listener.accept().await else {
            continue;
        };
        let _ = browser.set_nodelay(true);
        let (session, tabs, named) = (session.clone(), tabs.clone(), named.clone());
        tokio::spawn(async move {
            let _ = serve(browser, &session, tabs, named).await;
        });
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
            let body = body(&mut reader, &request).await?;
            let command: Command =
                serde_json::from_slice(&body).map_err(|e| format!("that is not a command: {e}"))?;
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
/// sharing one would be reading each other's answers - and a session that is somebody's own is
/// cheap enough to attach to twice.
async fn stream<W: AsyncWrite + Unpin>(
    write: &mut W,
    session: &str,
    tabs: Tabs,
    named: Named,
    tab: String,
    since: Option<u64>,
) -> Result<(), String> {
    let Ok(Address::Tcp(host)) = protocol::address(session) else {
        return reply(
            write,
            "500 Internal Server Error",
            "text/plain",
            b"bad address",
        )
        .await;
    };
    // note: the whole attach happens before the head goes out, which is what makes the retry below
    // possible: nothing has been said to the browser yet, so a second attempt is the first one it
    // hears about. A `200` on this route means "attached", and it is true when it is sent
    let Some(Upstream {
        mut up,
        mut down,
        first,
    }) = attach(host, since, &named, write).await?
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
    up: protocol::Frames<BufReader<OwnedReadHalf>>,
    /// What this end says back.
    down: OwnedWriteHalf,
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
    host: &str,
    since: Option<u64>,
    named: &Named,
    write: &mut W,
) -> Result<Option<Upstream>, String> {
    let mut since = since;
    loop {
        let upstream = match TcpStream::connect(host).await {
            Ok(stream) => stream,
            Err(e) => {
                let said = format!("could not reach the session at {host}: {e}");
                reply(write, "502 Bad Gateway", "text/plain", said.as_bytes()).await?;

                return Ok(None);
            }
        };
        let _ = upstream.set_nodelay(true);
        let (up, mut down) = upstream.into_split();
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
            let said = format!("the session at {host} closed without answering the attach");
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
async fn reply<W: AsyncWrite + Unpin>(
    write: &mut W,
    status: &str,
    kind: &str,
    body: &[u8],
) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
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
