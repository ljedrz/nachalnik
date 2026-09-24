//! A connection to one MCP server, and what installing its tools into a kernel did.
//!
//! note: the connection lives as long as the [`Server`] does: dropping it ends the session, and for
//! a server running as a child process the process goes with it. That is also why this crate is
//! not the runtime - holding a process open is precisely what `nachalnik` promises not to do.

use std::sync::Arc;

use nachalnik::{ContextItem, ContextKind, Kernel, Tool};
use rmcp::{RoleClient, ServiceExt, service::RunningService, transport::IntoTransport};

use crate::{
    Error, Result,
    tool::{McpTool, Trust, spec_of, tool_id},
};

/// The most pages of tools one listing reads; see `Server::tools`.
///
/// note: far past what a server offers - a page is commonly dozens of tools - and a bound all the
/// same, since the cursor is the server's to hand back.
const PAGES: usize = 100;

/// What installing a server's tools into a kernel did.
///
/// note: `replaced` exists because a kernel holds one tool per identifier, and two servers may
/// well both offer `read`. Prefixing makes that unlikely rather than impossible - a name has to be
/// rewritten to fit what model providers accept, and rewriting can collide - so what was displaced
/// is reported rather than assumed to be nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Installed {
    /// The identifiers the tools were given, in the order the server listed them.
    pub added: Vec<String>,
    /// The identifiers that already had a tool, which is now gone.
    pub replaced: Vec<String>,
}

impl Installed {
    /// Takes these tools back out of a kernel, returning how many were still there.
    ///
    /// note: It removes what was actually added, rather than asking the server again and removing
    /// whatever it says today. A server whose tool list has changed in between would otherwise
    /// leave tools behind that nothing can name.
    pub fn remove_from(&self, kernel: &Kernel) -> usize {
        self.added
            .iter()
            .filter(|id| kernel.remove_tool(id).is_some())
            .count()
    }
}

/// A connection to one MCP server.
///
/// note: The connection is held for as long as this is. Dropping it ends the session, and for a
/// server running as a child process that means the process goes with it.
pub struct Server {
    name: String,
    prefix: bool,
    trust: Trust,
    running: RunningService<RoleClient, ()>,
}

impl Server {
    /// Connects to a server over a transport you have opened.
    ///
    /// note: This is the general form, and what the others are written in terms of. A pipe, a
    /// socket, a pair of streams in the same process for a test - the protocol does not care.
    pub async fn connect<T, E, A>(name: impl Into<String>, transport: T) -> Result<Self>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let running = ().serve(transport).await.map_err(|e| Error::Connect(Box::new(e)))?;

        Ok(Self {
            name: name.into(),
            prefix: true,
            trust: Trust::default(),
            running,
        })
    }

    /// Connects to a server run as a child process, talking over its stdin and stdout.
    ///
    /// note: This is how most MCP servers are distributed, and it is the reason this crate exists
    /// separately: the runtime spawns no processes.
    ///
    /// note: its standard error is held and read rather than inherited, whatever the `Command`
    /// says - the transport sets all three streams, and inherits standard error by default.
    /// Inherited, a server that logs a line per request writes it across whatever the caller has
    /// on the terminal, a drawn screen included, and no caller can stop it. What it says is kept,
    /// a few lines of it, for the one moment it is worth reading: a handshake that failed, where
    /// it is the reason.
    #[cfg(feature = "child-process")]
    pub async fn spawn(name: impl Into<String>, command: tokio::process::Command) -> Result<Self> {
        let (transport, stderr) = rmcp::transport::TokioChildProcess::builder(command)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| Error::Connect(Box::new(e)))?;
        let said = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        // read for as long as the server runs, because a pipe nobody reads fills and blocks the
        // process writing to it
        let draining = stderr.map(|stderr| tokio::spawn(drain(stderr, said.clone())));

        match Self::connect(name, transport).await {
            Ok(server) => Ok(server),
            Err(Error::Connect(e)) => {
                // a server that failed the handshake has usually exited, and what it said on the
                // way out may not have been read yet
                if let Some(draining) = draining {
                    let _ = tokio::time::timeout(LAST_WORDS, draining).await;
                }
                let tail: Vec<String> = said
                    .lock()
                    .map(|said| said.iter().cloned().collect())
                    .unwrap_or_default();
                Err(Error::Connect(match tail.is_empty() {
                    true => e,
                    false => format!("{e}; it said:\n{}", tail.join("\n")).into(),
                }))
            }
            Err(e) => Err(e),
        }
    }

    /// Decides what this server's tools are allowed to do; see [`Trust`], which is worth reading
    /// before changing.
    pub fn trusting(mut self, trust: Trust) -> Self {
        self.trust = trust;
        self
    }

    /// Offers the tools under their own names rather than the server's plus theirs.
    ///
    /// note: Shorter, and worth it for a single server. With two of them it is how one server's
    /// `read` quietly becomes the other's.
    pub fn without_prefix(mut self) -> Self {
        self.prefix = false;
        self
    }

    /// The name this connection was given, which is also the prefix its tools carry.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the server said about itself when it answered the handshake: its name, its version,
    /// and which parts of the protocol it implements.
    pub fn info(&self) -> Option<std::sync::Arc<rmcp::model::ServerPeerInfo>> {
        self.running.peer().peer_info()
    }

    /// Asks the server what it offers, and wraps each of them as a [`Tool`](nachalnik::Tool).
    ///
    /// note: Nothing is registered anywhere by this; it hands back a list, and what to do with it
    /// is yours. [`Server::install`] is the common answer.
    ///
    /// note: a page at a time, to a hundred of them. A server hands the next page's cursor back
    /// with each one, and one that always hands back another would be listed for ever - at
    /// startup, where nothing interrupts it.
    pub async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>> {
        let mut listed = Vec::new();
        let mut cursor = None;
        for _ in 0..PAGES {
            let page = self
                .running
                .peer()
                .list_tools(Some(
                    rmcp::model::PaginatedRequestParams::default().with_cursor(cursor),
                ))
                .await
                .map_err(|e| Error::Request(Box::new(e)))?;
            listed.extend(page.tools);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        if cursor.is_some() {
            return Err(Error::Request(
                format!("it listed more than {PAGES} pages of tools and was still going").into(),
            ));
        }

        Ok(listed
            .into_iter()
            .map(|tool| {
                let prefix = self.prefix.then_some(self.name.as_str());
                let spec = spec_of(&self.name, tool_id(prefix, &tool.name), &tool, &self.trust);
                let wrapped: Arc<dyn Tool> = Arc::new(McpTool::new(
                    self.running.peer().clone(),
                    tool.name.to_string(),
                    spec,
                ));

                wrapped
            })
            .collect())
    }

    /// Puts every tool the server offers into a kernel.
    ///
    /// note: Running it again is how a server whose tool list has grown or changed is picked up -
    /// every tool it lists comes back in `replaced`, since each one had a tool under its name. A
    /// tool the server has *stopped* offering is left where it is, for
    /// [`Kernel::remove_tool`](nachalnik::Kernel::remove_tool). It is deliberately something you do
    /// rather than something that happens: the model is about to be told what it can do, and that
    /// is not a thing to change underneath a turn.
    pub async fn install(&self, kernel: &Kernel) -> Result<Installed> {
        let mut installed = Installed::default();

        for tool in self.tools().await? {
            let id = tool.spec().id;
            if kernel.add_tool(tool).is_some() {
                installed.replaced.push(id.clone());
            }
            installed.added.push(id);
        }

        Ok(installed)
    }

    /// Reads every resource the server offers, as context items ready to be pushed.
    ///
    /// note: They are handed back rather than pushed, because what goes into a context is the
    /// user's decision and a server offering forty documents is not an argument. Each one is a
    /// [`ContextKind::Reference`] labelled with its URI, so it arrives in the request saying where
    /// it came from.
    ///
    /// note: a resource with no text in it - an image, a binary blob - is *named* rather than
    /// dropped, exactly as a tool result's non-text blocks are. A list that quietly came back one
    /// shorter than the server's would leave a caller unable to say whether a document had been
    /// missed or had never been offered; the marker costs a line, and pushing it is optional like
    /// everything else here.
    pub async fn resources(&self) -> Result<Vec<ContextItem>> {
        // page by page and to the same bound as `tools`, for the same reason: the cursor is the
        // server's to hand back, and the SDK's own `list_all_resources` follows it for ever
        let mut listed = Vec::new();
        let mut cursor = None;
        for _ in 0..PAGES {
            let page = self
                .running
                .peer()
                .list_resources(Some(
                    rmcp::model::PaginatedRequestParams::default().with_cursor(cursor),
                ))
                .await
                .map_err(|e| Error::Request(Box::new(e)))?;
            listed.extend(page.resources);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        if cursor.is_some() {
            return Err(Error::Request(
                format!("it listed more than {PAGES} pages of resources and was still going")
                    .into(),
            ));
        }

        let mut items = Vec::with_capacity(listed.len());
        for resource in listed {
            let uri = resource.uri.clone();
            let read = self
                .running
                .peer()
                .read_resource(rmcp::model::ReadResourceRequestParams::new(uri.clone()))
                .await
                .map_err(|e| Error::Request(Box::new(e)))?;

            // every part, text or a marker for what was there, as a tool result's blocks are: a
            // blob beside some text was dropped without a word where a blob on its own was named
            let parts: Vec<(bool, String)> = read
                .contents
                .iter()
                .map(|content| {
                    let value = serde_json::to_value(content).unwrap_or_default();
                    match value.get("text").and_then(serde_json::Value::as_str) {
                        Some(text) => (true, text.to_owned()),
                        None => (
                            false,
                            format!(
                                "[a part with no text ({}), not carried into the context]",
                                value
                                    .get("mimeType")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("no media type given")
                            ),
                        ),
                    }
                })
                .collect();
            let text: Vec<String> = match parts.iter().any(|(text, _)| *text) {
                true => parts.into_iter().map(|(_, part)| part).collect(),
                false => Vec::new(),
            };

            // a blob cannot go into a text context; saying what was there beats a gap in the list
            let content = match text.is_empty() {
                true => format!(
                    "[a resource with no text ({}), not carried into the context]",
                    resource
                        .mime_type
                        .as_deref()
                        .unwrap_or("no media type given")
                ),
                false => text.join("\n"),
            };

            items.push(
                ContextItem::new(ContextKind::Reference, "mcp", uri, content)
                    .because(format!("a resource offered by the `{}` server", self.name)),
            );
        }

        Ok(items)
    }

    /// Ends the session, and waits for the server to notice.
    pub async fn shutdown(self) -> Result<()> {
        self.running
            .cancel()
            .await
            .map(|_| ())
            .map_err(|e| Error::Request(Box::new(e)))
    }
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("name", &self.name)
            .field("prefix", &self.prefix)
            .field("trust", &self.trust)
            .finish_non_exhaustive()
    }
}

/// How many of the last lines a spawned server wrote to standard error are kept.
#[cfg(feature = "child-process")]
const KEPT: usize = 20;

/// How long a server that failed its handshake is given to finish saying why.
#[cfg(feature = "child-process")]
const LAST_WORDS: std::time::Duration = std::time::Duration::from_millis(500);

/// How much of one line of a server's standard error is kept.
#[cfg(feature = "child-process")]
const LINE: usize = 1024;

/// Reads a spawned server's standard error to its end, keeping the start of each of the last
/// [`KEPT`] lines.
///
/// note: the start of a line, to [`LINE`] bytes, and the rest read and let go. The count of lines
/// was bounded and the length of one was not, so a server writing without newlines - a progress
/// bar redrawn with `\r`, or one that means harm - grew this without end for as long as it ran.
#[cfg(feature = "child-process")]
async fn drain(
    stderr: tokio::process::ChildStderr,
    said: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
) {
    use tokio::io::AsyncBufReadExt as _;

    let keep = |line: &[u8]| {
        let text = String::from_utf8_lossy(line).trim_end().to_owned();
        let Ok(mut said) = said.lock() else {
            return false;
        };
        if said.len() == KEPT {
            said.pop_front();
        }
        said.push_back(text);
        true
    };

    let mut stderr = tokio::io::BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        let read = match stderr.fill_buf().await {
            Ok(read) if !read.is_empty() => read,
            _ => break,
        };
        let (taken, ended) = match read.iter().position(|byte| *byte == b'\n') {
            Some(at) => (at + 1, true),
            None => (read.len(), false),
        };
        let room = LINE.saturating_sub(line.len()).min(taken);
        line.extend_from_slice(&read[..room]);
        stderr.consume(taken);
        if ended {
            if !keep(&line) {
                return;
            }
            line.clear();
        }
    }
    if !line.is_empty() {
        keep(&line);
    }
}
