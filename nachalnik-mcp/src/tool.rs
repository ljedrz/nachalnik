use nachalnik::{
    BoxError, Capability, Content, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};
use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, CallToolResult, ContentBlock, ToolAnnotations},
    service::Peer,
};
use serde_json::Value;

/// How the capabilities of a server's tools are decided.
///
/// This is the most important decision in this crate, so it is worth being precise about the
/// problem. MCP tools may carry annotations describing themselves - `readOnlyHint`,
/// `destructiveHint`, `openWorldHint` - and the specification says, in as many words, that clients
/// "should never make tool use decisions based on annotations received from untrusted servers".
/// They are hints. A server that would rather not be asked about need only claim to be read-only.
///
/// A [`PermissionPolicy`](nachalnik::PermissionPolicy) that acted on those hints would therefore
/// be taking the word of the thing it is meant to be gating. So the default here takes nobody's
/// word for anything.
///
/// note: Whichever of these is in use, every tool from a server also declares
/// `Capability::Custom("mcp:<server>")`. That is a fact rather than a claim - it is where the tool
/// came from - and it makes "ask me once about this server" something a policy can express in one
/// line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Trust {
    /// Believe nothing. Tools declare only which server they came from, and what may be done with
    /// that server is a decision your policy makes once, knowingly.
    ///
    /// note: The default, because it is the only option here that cannot be talked out of anything
    /// by the server it is describing.
    #[default]
    Nothing,
    /// Give every tool from this server exactly these capabilities, whatever it says about itself.
    ///
    /// note: For when you know what a server is - your own, or one you have read - and want its
    /// tools gated like the rest of your own.
    Fixed(Vec<Capability>),
    /// Believe the server's annotations.
    ///
    /// A tool claiming `readOnlyHint` gets [`Capability::Read`]; anything else gets
    /// [`Capability::Write`] and [`Capability::Edit`], because the specification's default for
    /// that hint is `false` and an absent hint is not a reassurance. `openWorldHint` adds
    /// [`Capability::Network`].
    ///
    /// note: Reasonable for a server you run yourself, and a mistake for one you do not.
    Annotations,
}

impl Trust {
    /// Works out what a tool may do, given what it says about itself.
    pub(crate) fn capabilities(
        &self,
        server: &str,
        annotations: Option<&ToolAnnotations>,
    ) -> Vec<Capability> {
        // where it came from is a fact, so it is always recorded
        let mut capabilities = vec![Capability::Custom(format!("mcp:{server}"))];

        match self {
            Self::Nothing => {}
            Self::Fixed(fixed) => capabilities.extend(fixed.iter().cloned()),
            Self::Annotations => {
                let read_only = annotations.and_then(|a| a.read_only_hint).unwrap_or(false);
                match read_only {
                    true => capabilities.push(Capability::Read),
                    false => capabilities.extend([Capability::Write, Capability::Edit]),
                }
                if annotations.and_then(|a| a.open_world_hint).unwrap_or(false) {
                    capabilities.push(Capability::Network);
                }
            }
        }

        capabilities
    }
}

/// One tool on an MCP server, as a [`Tool`].
pub(crate) struct McpTool {
    peer: Peer<RoleClient>,
    /// The name the server knows it by, which is not necessarily the one the model uses.
    remote: String,
    spec: ToolSpec,
}

impl McpTool {
    pub(crate) fn new(peer: Peer<RoleClient>, remote: String, spec: ToolSpec) -> Self {
        Self { peer, remote, spec }
    }
}

#[async_trait]
impl Tool for McpTool {
    /// note: Cached, because this is called afresh for every request and a round trip to another
    /// process is not a thing to do sixty times a minute. A server whose tools have changed is
    /// picked up by [`Server::install`](crate::Server::install) running again.
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        // note: checked before dispatching rather than during. Once the request is with the
        // server, this crate lets it finish: abandoning it would mean either leaving the server
        // working on something nobody is waiting for, or claiming a cancellation that has not
        // been negotiated. Whether it *ran* is something the model should be told accurately
        if output.is_interrupted() {
            return Ok(ToolOutput::error(
                "interrupted before this call was made; it did not run",
            ));
        }

        let arguments = match &*call.args {
            Value::Object(map) => Some(map.clone()),
            Value::Null => None,
            // a model that produced something other than an object for a tool whose schema says
            // object gets to see that it did, rather than having it quietly reshaped
            other => {
                return Ok(ToolOutput::error(format!(
                    "arguments have to be a JSON object, and these are {other}"
                )));
            }
        };

        let mut request = CallToolRequestParams::new(self.remote.clone());
        if let Some(arguments) = arguments {
            request = request.with_arguments(arguments);
        }

        match self.peer.call_tool(request).await {
            Ok(result) => Ok(output_of(result)),
            // the server is not going to answer this one; that is a failure the model can act on,
            // not a reason to stop the loop
            Err(e) => Ok(ToolOutput::error(format!("the MCP server refused: {e}"))),
        }
    }
}

/// Turns what a server returned into what the kernel records.
///
/// note: MCP results are a list of blocks, and not all of them are text. A picture cannot go into
/// a text context, so it is *named* rather than dropped silently - the model is told that
/// something came back and what it was, which is a better answer than a gap.
fn output_of(result: CallToolResult) -> ToolOutput {
    let failed = result.is_error.unwrap_or(false);

    // a server that returned structured content meant it; it goes in as structure
    if let Some(structured) = result.structured_content {
        let content = Content::json(structured);
        return match failed {
            true => ToolOutput::error(content),
            false => ToolOutput::new(content),
        };
    }

    let mut parts = Vec::with_capacity(result.content.len());
    for block in &result.content {
        parts.push(match block {
            ContentBlock::Text(text) => text.text.clone(),
            ContentBlock::Image(image) => {
                format!(
                    "[an image ({}), not carried into the context]",
                    image.mime_type
                )
            }
            ContentBlock::Audio(audio) => {
                format!(
                    "[audio ({}), not carried into the context]",
                    audio.mime_type
                )
            }
            ContentBlock::Resource(resource) => match embedded_text(resource) {
                Some(text) => text,
                None => "[an embedded resource, not carried into the context]".to_owned(),
            },
            ContentBlock::ResourceLink(link) => format!("[a resource: {}]", link.uri),
            // `ContentBlock` is `#[non_exhaustive]`: a kind of content this crate has not heard
            // of is reported as one, rather than vanishing
            other => format!(
                "[a {} block this bridge does not understand]",
                serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v["type"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| "new".to_owned())
            ),
        });
    }

    let text = parts.join("\n");
    match failed {
        true => ToolOutput::error(text),
        false => ToolOutput::new(text),
    }
}

/// The text of an embedded resource, if it has any.
fn embedded_text(resource: &rmcp::model::EmbeddedResource) -> Option<String> {
    match serde_json::to_value(&resource.resource).ok()? {
        Value::Object(map) => map.get("text")?.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Builds the declaration the model is shown for one of a server's tools.
pub(crate) fn spec_of(
    server: &str,
    id: String,
    tool: &rmcp::model::Tool,
    trust: &Trust,
) -> ToolSpec {
    let description = tool
        .description
        .as_deref()
        .unwrap_or("a tool on an MCP server, which said nothing about what it does")
        .to_owned();

    ToolSpec::new(id, description)
        .with_schema(Value::Object((*tool.input_schema).clone()))
        .with_capabilities(trust.capabilities(server, tool.annotations.as_ref()))
}

/// Makes an identifier a model provider will accept.
///
/// note: The common restriction is `[a-zA-Z0-9_-]`, up to sixty-four characters, and an MCP server
/// is under no obligation to have heard of it. Rewriting a name can produce a collision, which is
/// why [`Server::install`](crate::Server::install) reports what it displaced rather than assuming
/// it displaced nothing.
pub(crate) fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(
            |c| match c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                true => c,
                false => '_',
            },
        )
        .take(64)
        .collect();

    if out.is_empty() {
        out.push('_');
    }

    out
}

/// A tool from an MCP server, as the kernel will know it.
pub(crate) fn tool_id(prefix: Option<&str>, remote: &str) -> String {
    match prefix {
        Some(prefix) => sanitize(&format!("{prefix}__{remote}")),
        None => sanitize(remote),
    }
}
