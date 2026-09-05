use std::sync::Arc;

use nachalnik::{ContextItem, ContextKind, Kernel, Tool};
use rmcp::{RoleClient, ServiceExt, service::RunningService, transport::IntoTransport};

use crate::{
    Error, Result,
    tool::{McpTool, Trust, spec_of, tool_id},
};

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
    #[cfg(feature = "child-process")]
    pub async fn spawn(name: impl Into<String>, command: tokio::process::Command) -> Result<Self> {
        let transport = rmcp::transport::TokioChildProcess::new(command)
            .map_err(|e| Error::Connect(Box::new(e)))?;

        Self::connect(name, transport).await
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
    pub async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>> {
        let listed = self
            .running
            .peer()
            .list_all_tools()
            .await
            .map_err(|e| Error::Request(Box::new(e)))?;

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
    /// note: Running it again is how a server whose tool list has changed is picked up. It is
    /// deliberately something you do rather than something that happens: the model is about to be
    /// told what it can do, and that is not a thing to change underneath a turn.
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
        let listed = self
            .running
            .peer()
            .list_all_resources()
            .await
            .map_err(|e| Error::Request(Box::new(e)))?;

        let mut items = Vec::with_capacity(listed.len());
        for resource in listed {
            let uri = resource.uri.clone();
            let read = self
                .running
                .peer()
                .read_resource(rmcp::model::ReadResourceRequestParams::new(uri.clone()))
                .await
                .map_err(|e| Error::Request(Box::new(e)))?;

            let text: Vec<String> = read
                .contents
                .iter()
                .filter_map(|content| match serde_json::to_value(content).ok()? {
                    serde_json::Value::Object(map) => map.get("text")?.as_str().map(str::to_owned),
                    _ => None,
                })
                .collect();

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
