//! A connection to one MCP server, and what installing its tools into a kernel did.

use std::sync::Arc;

use nachalnik::{ContextItem, ContextKind, Kernel, Tool};
use rmcp::{
    RoleClient, ServiceExt, model::PaginatedRequestParams, service::RunningService,
    transport::IntoTransport,
};

use crate::{
    Error, Result,
    tool::{McpTool, Trust, left_out, spec_of, text_of, tool_id},
};

/// The most pages one listing reads, of tools or of resources; see `Server::tools` and
/// `Server::resources`.
///
/// note: far past what a server offers, and a bound all the same, since the cursor is the server's
/// to hand back.
const PAGES: usize = 100;

/// What installing a server's tools into a kernel did.
///
/// note: a kernel holds one tool per identifier, and two servers may both offer `read`. Prefixing
/// makes a clash unlikely, not impossible, since a name may be rewritten to fit what model
/// providers accept and rewriting can collide, so what was displaced is reported rather than
/// assumed to be nothing.
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
    /// note: it removes what was actually added, not what the server lists today, which would leave
    /// behind tools nothing can name.
    ///
    /// note: by identifier, so a tool installed since under one of the same identifiers is removed;
    /// that later install's `replaced` said the identifier was taken.
    pub fn remove_from(&self, kernel: &Kernel) -> usize {
        self.added
            .iter()
            .filter(|id| kernel.remove_tool(id).is_some())
            .count()
    }
}

/// A connection to one MCP server.
///
/// note: the connection lives as long as the [`Server`]; dropping it ends the session, and for a
/// child process the process goes with it. `nachalnik` itself holds no processes open.
pub struct Server {
    name: String,
    prefix: bool,
    trust: Trust,
    /// How long one call may wait on the server, where there is a limit; see
    /// [`Server::answering_within`].
    within: Option<std::time::Duration>,
    running: RunningService<RoleClient, ()>,
    /// The process, if this spawned one; see [`Server::spawn`].
    ///
    /// note: after `running`, so that dropping this ends the session before the process goes.
    #[cfg(feature = "child-process")]
    child: Option<tokio::process::Child>,
}

impl Server {
    /// Connects to a server over a transport you have opened.
    ///
    /// note: the general form; the others are written in terms of it. The protocol does not care: a
    /// pipe, a socket, or a pair of streams in the same process for a test.
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
            within: None,
            running,
            #[cfg(feature = "child-process")]
            child: None,
        })
    }

    /// Connects to a server run as a child process, talking over its stdin and stdout.
    ///
    /// note: most MCP servers are distributed this way, and the runtime spawns no processes, which
    /// is why this crate is separate.
    ///
    /// note: standard error is piped and read, whatever the `Command` says, because inherited
    /// output writes across the caller's terminal, a drawn screen included, and no caller could
    /// stop it. A few lines are kept for a failed handshake, where they are the reason.
    ///
    /// note: spawned here rather than by `rmcp`, so the process belongs to this value and goes when
    /// it does. `rmcp`'s transport kills a child it drops from a task it spawns, which a runtime
    /// shutting down need not run, so a server busy with a call outlived the program that started
    /// it. Held here with `kill_on_drop`, whatever the `Command` says, it is killed in the drop
    /// itself.
    #[cfg(feature = "child-process")]
    pub async fn spawn(
        name: impl Into<String>,
        mut command: tokio::process::Command,
    ) -> Result<Self> {
        use std::process::Stdio;

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Connect(Box::new(e)))?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            unreachable!("both were asked for as pipes a line above");
        };
        let said = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        // read for as long as the server runs, because a pipe nobody reads fills and blocks the
        // process writing to it
        let draining = child
            .stderr
            .take()
            .map(|stderr| tokio::spawn(drain(stderr, said.clone())));

        match Self::connect(name, (stdout, stdin)).await {
            Ok(server) => Ok(Self {
                child: Some(child),
                ..server
            }),
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
                    false => Box::new(Said {
                        under: e,
                        tail: tail.join("\n"),
                    }),
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

    /// Stops waiting for a tool's answer after this long, tells the server to stop, and tells the
    /// model the call went unanswered.
    ///
    /// note: no limit unless one is given, since how long a tool may take is the caller's to know.
    /// Without one, a call that never comes back (a hung server, or a response `rmcp` drops for
    /// carrying neither a result nor an error) holds the turn until it is interrupted.
    pub fn answering_within(mut self, within: std::time::Duration) -> Self {
        self.within = Some(within);
        self
    }

    /// Offers the tools under their own names rather than the server's plus theirs.
    ///
    /// note: worth it for one server. With two, one server's `read` becomes the other's, said only
    /// in [`Installed::replaced`].
    pub fn without_prefix(mut self) -> Self {
        self.prefix = false;
        self
    }

    /// The name this connection was given. Its tools carry this name as a prefix unless it was
    /// built [`without_prefix`](Server::without_prefix).
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
    /// note: nothing is registered; it returns a list, and [`Server::install`] is the usual next
    /// step.
    ///
    /// note: one page at a time, up to a hundred pages, since a server that always returned another
    /// cursor would otherwise be listed for ever at startup, where nothing interrupts it.
    pub async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>> {
        let listed = paged("tools", |page| async move {
            self.running
                .peer()
                .list_tools(Some(page))
                .await
                .map(|page| (page.tools, page.next_cursor))
        })
        .await?;

        Ok(listed
            .into_iter()
            .map(|tool| {
                let prefix = self.prefix.then_some(self.name.as_str());
                let spec = spec_of(tool_id(prefix, &tool.name), &tool, &self.trust);
                let wrapped: Arc<dyn Tool> = Arc::new(
                    McpTool::new(self.running.peer().clone(), tool.name.to_string(), spec)
                        .answering_within(&self.name, self.within),
                );

                wrapped
            })
            .collect())
    }

    /// Puts every tool the server offers into a kernel.
    ///
    /// note: running it again picks up a server whose list grew or changed: every tool it listed
    /// before comes back in `replaced`, since each had a tool under its name, and a new one is in
    /// `added` alone. A tool the server stopped offering is left for
    /// [`Kernel::remove_tool`](nachalnik::Kernel::remove_tool). It is something you do rather than
    /// something that happens, because what the model is told it can do should not change under a
    /// turn.
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
    /// note: returned rather than pushed, because what goes into a context is the user's decision
    /// and a server offering forty documents is no argument for all forty. Each is a
    /// [`ContextKind::Reference`] labelled with its URI, so the request says where it came from.
    ///
    /// note: a resource with no text, such as an image or a binary blob, is named rather than
    /// dropped, as a tool result's non-text blocks are: a list one shorter than the server's would
    /// leave a caller unable to tell a missed document from one never offered.
    pub async fn resources(&self) -> Result<Vec<ContextItem>> {
        let listed = paged("resources", |page| async move {
            self.running
                .peer()
                .list_resources(Some(page))
                .await
                .map(|page| (page.resources, page.next_cursor))
        })
        .await?;

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
            let parts: Vec<_> = read.contents.iter().map(text_of).collect();

            // a blob cannot go into a text context; saying what was there beats a gap in the list.
            // The type is the one the part was read with, as it is for a part beside some text,
            // and the listing's only where no part gave one
            let content = match parts.iter().any(|part| part.is_ok()) {
                true => parts
                    .iter()
                    .map(|part| match part {
                        Ok(text) => (*text).to_owned(),
                        Err(media) => left_out("a part with no text", *media),
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                false => left_out(
                    "a resource with no text",
                    parts
                        .iter()
                        .find_map(|part| part.err().flatten())
                        .or(resource.mime_type.as_deref()),
                ),
            };

            items.push(
                ContextItem::new(ContextKind::Reference, "mcp", uri, content)
                    .because(format!("a resource offered by the `{}` server", self.name)),
            );
        }

        Ok(items)
    }

    /// Ends the session and waits for the server to notice.
    ///
    /// note: a spawned server is told by its input closing and given three seconds to finish before
    /// it is killed. It is the wait `rmcp`'s own transport made, so what a server does on its way
    /// out is done by the time this returns.
    pub async fn shutdown(self) -> Result<()> {
        let ended = self
            .running
            .cancel()
            .await
            .map(|_| ())
            .map_err(|e| Error::Request(Box::new(e)));
        #[cfg(feature = "child-process")]
        if let Some(mut child) = self.child
            && tokio::time::timeout(GRACE, child.wait()).await.is_err()
        {
            let _ = child.kill().await;
        }

        ended
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

/// Reads one listing a page at a time, to [`PAGES`] of them, and refuses one still going after
/// that.
///
/// note: the SDK's own `list_all_tools` and `list_all_resources` follow the cursor for ever.
async fn paged<T, E, F>(
    what: &str,
    mut next: impl FnMut(PaginatedRequestParams) -> F,
) -> Result<Vec<T>>
where
    F: Future<Output = std::result::Result<(Vec<T>, Option<String>), E>>,
    E: std::error::Error + Send + Sync + 'static,
{
    let mut listed = Vec::new();
    let mut cursor = None;
    for _ in 0..PAGES {
        let (items, more) = next(PaginatedRequestParams::default().with_cursor(cursor))
            .await
            .map_err(|e| Error::Request(Box::new(e)))?;
        listed.extend(items);
        cursor = more;
        if cursor.is_none() {
            return Ok(listed);
        }
    }

    Err(Error::Request(
        format!("it listed more than {PAGES} pages of {what} and was still going").into(),
    ))
}

/// How many of the last lines a spawned server wrote to standard error are kept.
#[cfg(feature = "child-process")]
const KEPT: usize = 20;

/// How long a server that failed its handshake is given to finish saying why.
#[cfg(feature = "child-process")]
const LAST_WORDS: std::time::Duration = std::time::Duration::from_millis(500);

/// How long [`Server::shutdown`] waits for a spawned server to leave of its own accord.
#[cfg(feature = "child-process")]
const GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// How much of one line of a server's standard error is kept.
#[cfg(feature = "child-process")]
const LINE: usize = 1024;

/// A failed handshake, with what the server said on its way out.
///
/// note: a type rather than joining the two into a string, so the underlying failure is still
/// [`Error::source`](std::error::Error::source) - the promise [`Error`] makes of both its kinds.
#[cfg(feature = "child-process")]
#[derive(Debug)]
struct Said {
    under: Box<dyn std::error::Error + Send + Sync>,
    tail: String,
}

#[cfg(feature = "child-process")]
impl std::fmt::Display for Said {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; it said:\n{}", self.under, self.tail)
    }
}

#[cfg(feature = "child-process")]
impl std::error::Error for Said {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.under)
    }
}

/// Reads a spawned server's standard error to its end, keeping the start of each of the last
/// [`KEPT`] lines.
///
/// note: the start of a line, up to [`LINE`] bytes, with the rest read and dropped. The line count
/// was bounded but not a line's length, so a server writing without newlines (a progress bar
/// redrawn with `\r`, or a malicious one) grew this without end.
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
