//! Driving a session from somewhere that is not this process.
//!
//! ```console
//! kamchatka --serve unix:/tmp/kamchatka.sock
//! kamchatka --connect unix:/tmp/kamchatka.sock
//! ```
//!
//! [`server::Server`] is the third loop over an [`App`](crate::app::App), beside the one in
//! `main.rs` that draws and the one in [`headless`](crate::headless) that reads lines.
//! [`client::Client`] is the smallest thing that exercises the whole of what it offers.
//!
//! # the one invariant
//!
//! **A session belongs to the program running it, not to whoever is attached.** A turn carries on
//! with nobody watching, a question waits for somebody to come back and answer it, and a client
//! that attaches an hour later attaches to the same session. Nothing here shortens a session
//! because a socket closed, and [`server::Server::run`] does not return when the last client
//! leaves. Every other decision in this module falls out of that one.
//!
//! # two streams, and only one of them can be lost
//!
//! The runtime already draws the line this protocol needs, and it is worth saying which line,
//! because the obvious design draws a different one and is wrong.
//!
//! A [`nachalnik::Record`] is numbered from 1, is never reused, is in the session log, and the log
//! is unbounded by decision - a capped append-only log is not one. So the numbered half of what a
//! client reads is not something the server hands out, queues or is able to drop: each connection
//! holds a [`Kernel`](nachalnik::Kernel) of its own, which is a cheap `Arc` handle, and reads
//! `history_since(wherever it had got to)` for itself. A client that fell behind, lagged its
//! subscription, slept, or was not connected at all when a record was written gets that record by
//! asking for it by sequence.
//!
//! The other half is `model.delta` and `tool.output` - a model typing, a tool talking - and those
//! are **not** in the log unless `Config::record_progress` says so, because a log that kept every
//! fragment would be dominated by them. They are therefore unnumbered, best-effort, and gone once
//! they have gone past. A client that misses some is told how many.
//!
//! That is the whole of the backpressure design, and what it buys is that **there is no outbound
//! queue per client anywhere in here**. A connection that stops reading stops being written to and
//! its subscriptions fall behind; it loses live observation, which could not have been recovered,
//! and loses no record, which could not have been lost.
//!
//! # what an event stream cannot do, and what is done instead
//!
//! The tempting claim about a protocol like this is that the events are the authoritative stream
//! and a client renders them however it likes. That is not true of this runtime and it is
//! important that it is not: **the log names things, it does not copy them.** `context.added`
//! carries an identifier, a kind, a source, a label and a token count, and not one word of
//! content - which is exactly what keeps a log small enough to keep for ever, and what leaves a
//! client fed nothing but events able to render a turn as it streams and unable to render a single
//! word of anything that happened before it connected.
//!
//! So content reaches a client two ways, and neither of them fattens the record stream. A
//! [`protocol::Attached`] carries the conversation as it reads now, every item as a row, the
//! budget, the questions outstanding and what the policy will say - a *projection*, and
//! deliberately not a [`nachalnik::Snapshot`], which carries every item's whole content and would
//! push megabytes at a phone that has asked for nothing. And [`protocol::Command::Inspect`] asks
//! for the whole of any one item, when somebody wants it.
//!
//! # what is not here
//!
//! **No authentication, and none is planned in the protocol.** A bearer token in every message is
//! a scheme to keep in step with, and it would be protecting a channel whose real boundary is
//! somewhere else: the socket. A unix socket's file permissions are that boundary; a loopback port
//! is the machine. [`server::Server::bind`] refuses to listen anywhere else, because this protocol
//! carries a `shell` tool - reaching the session *is* reaching the machine, and there is no
//! configuration in which handing that to a network interface is what somebody meant. Across a
//! network, tunnel something that does authenticate.
//!
//! **No arbitration between clients.** Every attached client may submit, interrupt and answer
//! questions, and there is room for exactly one message queued into a running turn - so a second
//! client typing during a turn takes the first one's place, and the session says so to everybody
//! rather than letting a line disappear quietly. Nothing on the wire carries a client identifier,
//! which is the first thing any answer to this would need. What several people driving one agent
//! should *mean* is undecided rather than unbuilt; `POSTPONED.md` has it, along with the two other
//! things this module is knowingly without - a command that awaits the endpoint holding the whole
//! loop, and a projection too large for [`protocol::MAX_LINE`], which no client can attach past. A
//! single record that large is named rather than sent; see [`protocol::Message::Oversized`].
//!
//! **Nothing in [`nachalnik`] knows any of this exists**, and that is the test this module was
//! held to rather than a remark about it. `nachalnik-mcp`, `kamchatka`'s introspection tools and
//! `nachalnik-eval` were each written with no change to the runtime at all; if remote control had
//! needed one, the seam would have been wrong rather than the protocol.
//!
//! **No crate of its own.** A protocol crate that nothing outside this one depends on is a
//! boundary drawn before anybody has asked for it. When something that is not `kamchatka` needs to
//! speak this, [`protocol`] is what moves - along with the three plain data types it borrows from
//! [`app`](crate::app) rather than redeclaring: [`Speaker`](crate::app::Speaker),
//! [`Did`](crate::app::Did) and [`Page`](crate::app::Page), which are an enum of seven, an enum of
//! three, and two strings. They are borrowed on purpose, because a wire that invented its own word
//! for who was talking would be a second vocabulary to keep in step with the first.
//! `examples/attached.rs` is a client written against exactly that much of this crate and nothing
//! else, which is how the claim is checked rather than asserted.

pub mod client;
pub mod protocol;
pub mod server;

pub use client::Client;
pub use server::{Server, Serving, opening};

use std::time::Duration;

/// How long a connection may sit idle before the kernel starts asking whether the peer is there.
///
/// note: short, for a protocol most of whose sessions are idle most of the time. A session waiting
/// on a permission answer is doing nothing *by design*, and the whole difficulty is that a peer
/// which is thinking and a peer whose laptop has closed look identical from here - so the probes
/// have to start while the silence is still ordinary, or they are not telling the two apart.
const IDLE: Duration = Duration::from_secs(30);

/// How long between those probes once they have started.
const PROBE: Duration = Duration::from_secs(10);

/// Sets the two things on a port that a socket file never needed.
///
/// note: neither matters on loopback, which is why neither was here to begin with, and both matter
/// the moment the bytes go near a network.
///
/// note: **`TCP_NODELAY`**, because Nagle's algorithm holds a small write back until the last one
/// is acknowledged, and every frame this protocol sends is small: a key somebody pressed, a
/// fragment of a model's sentence, an answer to a question. Batching those is the exact opposite
/// of the trade worth making for a stream whose whole value is that it is live.
///
/// note: **keepalive**, because a half-open connection has no other end to it. A peer that sends a
/// `FIN` is noticed at once; a peer whose machine slept, lost its wifi or was unplugged sends
/// nothing at all, and a read on this side waits for ever. On the session that is a connection task
/// holding a `Kernel` and never reporting the client as gone; on a client it is a process waiting
/// on a host that is not coming back, with the reconnection it was written to do never starting.
///
/// note: a failure is ignored rather than refused. These are an improvement on a connection that
/// already works, so a platform that will not take one of them is a reason to go without it and not
/// a reason to refuse somebody a session. The count of probes is left to the operating system:
/// `socket2` puts it behind a feature and the two that are set here are the two that decide how
/// soon anybody finds out.
pub(crate) fn tuned(stream: &tokio::net::TcpStream) {
    use socket2::{SockRef, TcpKeepalive};

    let _ = stream.set_nodelay(true);
    let _ = SockRef::from(stream)
        .set_tcp_keepalive(&TcpKeepalive::new().with_time(IDLE).with_interval(PROBE));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both options can actually be set on a socket on this platform.
    ///
    /// note: what this is for is the *platform*, not the logic - there is no logic. `socket2` gates
    /// `with_interval` on a list of operating systems and `set_tcp_keepalive` behaves differently
    /// on each of them, so the thing worth checking is that the call this crate makes is one the
    /// machine it was built for will take. Nothing about the protocol can see either option, so
    /// this is the only place they are observable at all.
    #[tokio::test]
    async fn a_port_takes_both_of_the_options_it_is_given() {
        use socket2::{SockRef, TcpKeepalive};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        let client = tokio::net::TcpStream::connect(at)
            .await
            .expect("nothing was listening");
        let (served, _) = listener.accept().await.expect("nothing connected");

        for stream in [&client, &served] {
            tuned(stream);
            assert!(stream.nodelay().expect("the option was refused"));
            // note: called again here rather than read back off the socket, and the difference is
            // what `tuned` does with the answer: it drops it, deliberately, so that a platform
            // refusing keepalive costs somebody a session rather than a connection. Nothing can
            // therefore observe it afterwards, and the only way to find out whether this machine
            // takes the call is to make it
            assert!(
                SockRef::from(stream)
                    .set_tcp_keepalive(&TcpKeepalive::new().with_time(IDLE).with_interval(PROBE))
                    .is_ok(),
                "this platform refused the keepalive `tuned` sets"
            );
        }
    }
}
