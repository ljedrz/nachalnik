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

mod relay;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let session = args
        .next()
        .unwrap_or_else(|| "tcp:127.0.0.1:7878".to_owned());
    let listen = args.next().unwrap_or_else(|| "127.0.0.1:8080".to_owned());

    relay::run(&session, &listen).await
}
