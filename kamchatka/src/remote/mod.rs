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
//! **Nothing in [`nachalnik`] knows any of this exists**, and that is the test this module was
//! held to rather than a remark about it. `nachalnik-mcp`, `kamchatka`'s introspection tools and
//! `nachalnik-eval` were each written with no change to the runtime at all; if remote control had
//! needed one, the seam would have been wrong rather than the protocol.
//!
//! **No crate of its own.** A protocol crate that nothing outside this one depends on is a
//! boundary drawn before anybody has asked for it. When something that is not `kamchatka` needs to
//! speak this, [`protocol`] is what moves.

pub mod client;
pub mod protocol;
pub mod server;

pub use client::Client;
pub use server::{Server, opening};
