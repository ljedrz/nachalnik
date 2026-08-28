#![deny(missing_docs)]
#![deny(unsafe_code)]

//! An [MCP](https://modelcontextprotocol.io) bridge for [`nachalnik`]: a tool somebody else wrote,
//! as a [`Tool`](nachalnik::Tool) like any other.
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use nachalnik::{Config, Kernel};
//! use nachalnik_mcp::Server;
//! use tokio::process::Command;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let kernel = Kernel::new(Config::default());
//!
//! let files = Server::spawn("files", Command::new("mcp-server-filesystem")).await?;
//! let installed = files.install(&kernel).await?;
//! println!("{} tools: {}", installed.added.len(), installed.added.join(", "));
//! # Ok(())
//! # }
//! ```
//!
//! # Why this is not in the runtime
//!
//! `nachalnik` promises to spawn no processes, open no sockets and run nothing in the background.
//! Every one of those is what speaking MCP consists of. Tying a context library's version to a
//! protocol that revises faster than the library should would be the second problem.
//!
//! It needs nothing the runtime does not already expose. An MCP tool is a `Tool` that forwards to
//! a server; tools arriving and leaving is [`Kernel::add_tool`](nachalnik::Kernel::add_tool) and
//! [`remove_tool`](nachalnik::Kernel::remove_tool), which put it on the event stream; a tool's
//! progress is an [`OutputSink`](nachalnik::OutputSink); a structured result is
//! [`Content::Json`](nachalnik::Content). That is the point of a seam.
//!
//! # What it decides, and what it refuses to
//!
//! Two things here are judgement calls, and both are yours rather than the kernel's:
//!
//! - **What a tool is allowed to do.** MCP tools carry *hints* about themselves, and the
//!   specification says in as many words that a client should never make tool-use decisions
//!   based on hints from a server it does not trust. So [`Trust`] defaults to believing none of
//!   them. See its documentation - it is the most important decision in this crate.
//! - **What a tool is called.** Two servers may both offer `read`, and a kernel holds one tool per
//!   identifier, so names are prefixed with the server's by default. Nothing is ever replaced
//!   quietly: [`Installed::replaced`] says what was displaced.

mod server;
mod tool;

pub use crate::{
    server::{Installed, Server},
    tool::Trust,
};

use std::fmt;

/// The result of talking to an MCP server.
pub type Result<T> = std::result::Result<T, Error>;

/// Something an MCP server did, or failed to do.
///
/// note: A tool that *runs* and reports a failure is not one of these - that is an ordinary
/// [`ToolOutput::error`](nachalnik::ToolOutput::error), handed back to the model, because it is
/// information the model needs. These are the failures that mean the server is not going to
/// answer.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The server could not be reached, or refused the handshake.
    Connect(Box<dyn std::error::Error + Send + Sync>),
    /// The server was reached, but would not answer a request.
    Request(Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "the MCP server could not be reached: {e}"),
            Self::Request(e) => write!(f, "the MCP server would not answer: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(e) | Self::Request(e) => Some(&**e),
        }
    }
}
