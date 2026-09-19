//! The one thing a browser cannot do for itself: a socket.
//!
//! ```console
//! $ kamchatka --serve tcp:127.0.0.1:7878 -m mercury-2
//! $ cargo run --example gateway -- tcp:127.0.0.1:7878 0.0.0.0:8080
//! ```
//!
//! A browser cannot open a TCP connection - not inconveniently, at all - so something has to
//! terminate HTTP in front of a session. This is that, and it is deliberately the smallest version
//! of it: no framework, no router, no TLS, no dependency this crate did not already have. Three
//! routes, and the semantic protocol is not touched.
//!
//! # why events, not sockets
//!
//! The interesting part is that `text/event-stream` already has the protocol's own shape in it.
//!
//! An SSE event may carry an `id:`, and when a connection drops the browser reconnects **by
//! itself** and sends `Last-Event-ID:` with the last one it saw. That is exactly
//! [`Command::Attach`]'s `since`, which means the browser implements resume with no client code at
//! all - and resume is the fiddliest part of a client, the part [`kamchatka::remote::Client`] spends
//! eighty lines and a backoff on.
//!
//! The negative space matches too, which is the half that says the fit is real rather than
//! convenient. An event with **no** `id:` does not move `Last-Event-ID` - so:
//!
//! ```text
//! Message::Record    ->  id: <seq>   data: {…}     numbered, in the log, recoverable
//! Message::Attached  ->  id: <seq>   data: {…}     the projection, and where the stream starts
//! everything else    ->              data: {…}     unnumbered, best-effort, gone once past
//! ```
//!
//! A fragment of a model still typing is never something a browser tries to resume from, because
//! it was never given an id to resume from. That is the two-stream design written in somebody
//! else's standard.
//!
//! A WebSocket would be one connection instead of two, and would cost a handshake, client-frame
//! unmasking and fragmentation - or a dependency - and every line of the reconnection this gets for
//! nothing. Commands go the other way by `POST`, which is four or five messages in a session.
//!
//! # what this is not
//!
//! **There is no authentication here, and no encryption.** Whatever reaches this gateway reaches
//! the session behind it, which runs a `shell` tool as whoever started it. `kamchatka --serve`
//! refuses to listen anywhere but loopback for that reason; this asks for the listen address
//! outright and says what a non-loopback one means, because an example somebody runs on their own
//! network for an afternoon is a different thing from a program's default. It is not a thing to
//! leave running, and it is not a thing to put on a network you share.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use kamchatka::remote::protocol::{self, Address, Command, Message};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

/// The page, which is the whole client.
const PAGE: &str = include_str!("browser.html");

/// How long a browser waits before reconnecting a dropped stream, in milliseconds.
///
/// note: sent once at the top of every stream. The browser's default is three seconds and this is
/// a session somebody is watching, so it is worth being quicker - and the reconnection costs only
/// the records that were missed, because `Last-Event-ID` says where to start.
const RETRY: u64 = 1_000;

/// The longest request head this will read, in bytes.
const MAX_HEAD: usize = 16 * 1024;

/// The longest body, which is one command.
const MAX_BODY: usize = 1024 * 1024;

/// Where every attached tab's commands go.
type Tabs = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Command>>>>;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let session = args
        .next()
        .unwrap_or_else(|| "tcp:127.0.0.1:7878".to_owned());
    let listen = args.next().unwrap_or_else(|| "127.0.0.1:8080".to_owned());

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
    loop {
        let Ok((browser, _)) = listener.accept().await else {
            continue;
        };
        let _ = browser.set_nodelay(true);
        let (session, tabs) = (session.clone(), tabs.clone());
        tokio::spawn(async move {
            let _ = serve(browser, &session, tabs).await;
        });
    }
}

/// One browser connection: read a request, answer it, and for a stream stay for as long as it lasts.
async fn serve(browser: TcpStream, session: &str, tabs: Tabs) -> Result<(), String> {
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

    match (request.method.as_str(), path) {
        ("GET", "/") => page(&mut write).await,
        // note: the browser's own reconnection, arriving as a resume. `Last-Event-ID` is the last
        // `id:` this tab was sent, which is a record sequence, which is what `attach` takes
        ("GET", "/events") => {
            let since = request
                .header("last-event-id")
                .or_else(|| param(query, "since"))
                .and_then(|it| it.parse().ok());
            stream(&mut write, session, tabs, tab, since).await
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
    let upstream = match TcpStream::connect(host).await {
        Ok(stream) => stream,
        Err(e) => {
            let said = format!("could not reach the session at {host}: {e}");
            return reply(write, "502 Bad Gateway", "text/plain", said.as_bytes()).await;
        }
    };
    let _ = upstream.set_nodelay(true);
    let (up, mut down) = upstream.into_split();
    let mut up = BufReader::new(up).lines();
    protocol::write(&mut down, &Command::Attach { since }).await?;

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

    let (to_session, mut commands) = mpsc::unbounded_channel();
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

    let relayed = relay(write, &mut up).await;
    // the browser has gone, or the session has. Dropping the sender ends the task above, which
    // closes the connection to the session, which is what makes the session say the client left
    tabs.lock().expect("the tabs are not poisoned").remove(&tab);

    relayed
}

/// Turns every message the session sends into one event, until one end stops.
async fn relay<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    write: &mut W,
    up: &mut tokio::io::Lines<R>,
) -> Result<(), String> {
    while let Some(message) = protocol::read::<Message>(up).await? {
        // note: the whole mapping, and it is three lines because the standard already had the
        // shape. An `id:` is what a browser resumes from, so it goes on exactly the messages that
        // *can* be resumed from - which is the numbered ones, which is the ones in the log
        //
        // note: `Projected` falls to the default and carries a `seq` that would be a valid one,
        // which is the near miss worth naming. It is an answer to a command rather than a place in
        // the stream, and a browser resuming from it would be resuming from a message nobody can
        // ask for again by number. The rule is what can be *re-sent*, not what has a number on it
        let id = match &message {
            Message::Record(record) => Some(record.seq),
            Message::Attached(attached) => Some(attached.seq),
            _ => None,
        };
        let data = serde_json::to_string(&message).map_err(|e| e.to_string())?;
        let event = match id {
            Some(id) => format!("id: {id}\ndata: {data}\n\n"),
            None => format!("data: {data}\n\n"),
        };
        // a browser that has gone is not an error, it is a browser that has gone
        if write.write_all(event.as_bytes()).await.is_err() {
            return Ok(());
        }
        write.flush().await.map_err(|e| e.to_string())?;
    }

    Ok(())
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
    let mut line = String::new();
    if reader
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?
        == 0
    {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err("that is not a request".to_owned());
    };
    let (method, target) = (method.to_owned(), target.to_owned());

    let mut headers = Vec::new();
    let mut read = line.len();
    loop {
        let mut line = String::new();
        if reader
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?
            == 0
        {
            break;
        }
        read += line.len();
        if read > MAX_HEAD {
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
