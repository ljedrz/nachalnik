//! One tool on a server, as a [`Tool`](nachalnik::Tool) - and how much of what the server says
//! about it is believed.
//!
//! note: [`Trust`] is the decision this crate exists to get right, and the reasoning is on the
//! type. The short of it: annotations are hints from the thing being gated, so the default takes
//! nobody's word for anything, and the one capability that is a fact rather than a claim -
//! `mcp:<server>` - is declared whatever the trust setting.

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

/// What a model provider will accept in a tool's name: `[a-zA-Z0-9_-]`, and no more than this
/// many of them.
const LIMIT: usize = 64;

/// What separates a server's name from its tool's.
const SEPARATOR: &str = "__";

/// Makes an identifier a model provider will accept.
///
/// note: An MCP server is under no obligation to have heard of the restriction. Rewriting a name
/// can produce a collision, which is why [`Server::install`](crate::Server::install) reports what
/// it displaced rather than assuming it displaced nothing.
pub(crate) fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(
            |c| match c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                true => c,
                false => '_',
            },
        )
        .take(LIMIT)
        .collect();

    if out.is_empty() {
        out.push('_');
    }

    out
}

/// A tool from an MCP server, as the kernel will know it.
///
/// note: where the two together are over the limit, it is the *prefix* that gives way. The tool's
/// own name is the half that tells one of a server's tools from another, and cutting the tail off
/// the pair cuts exactly that: a server whose name is sixty-two characters long would otherwise
/// have every one of its tools arrive under the same identifier, each quietly replacing the last.
/// A prefix that has no room left is dropped rather than shortened to nothing.
pub(crate) fn tool_id(prefix: Option<&str>, remote: &str) -> String {
    let remote = sanitize(remote);
    let Some(prefix) = prefix else {
        return remote;
    };

    let room = LIMIT.saturating_sub(remote.chars().count() + SEPARATOR.len());
    let prefix: String = sanitize(prefix).chars().take(room).collect();

    match prefix.is_empty() {
        true => remote,
        false => format!("{prefix}{SEPARATOR}{remote}"),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Whether an identifier is one a model provider will take, which is the whole of what
    /// [`sanitize`] promises.
    fn acceptable(id: &str) -> bool {
        !id.is_empty()
            && id.chars().count() <= LIMIT
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    /// The names to try: short ones, ones that straddle the limit, and long ones.
    ///
    /// note: `(?s).` rather than a charset, because the input is a name an MCP server chose and
    /// the point of this function is that a server is under no obligation to have heard of the
    /// restriction. Emoji, CJK, newlines, control characters and the empty string are all things
    /// a server may legitimately send, and each is a way for an identifier to reach a provider as
    /// something it rejects.
    ///
    /// note: the lengths are stated, and they are stated because the first version of this was
    /// `(?s).*` and was measurably weaker than it read. proptest's `*` tops out around thirty
    /// characters, so no single name it produced ever reached [`LIMIT`] - and doubling the cap in
    /// the implementation broke none of the properties below. Truncation is half of what
    /// [`sanitize`] does, `{60,70}` is the half of the strategy that reaches it, and it is
    /// weighted highest because the boundary is where every bug in this function's history has
    /// been.
    fn a_name() -> impl Strategy<Value = String> {
        prop_oneof![
            2 => "(?s).{0,8}",
            3 => "(?s).{60,70}",
            1 => "(?s).{0,200}",
        ]
    }

    proptest! {
        #[test]
        fn a_rewritten_name_is_one_a_provider_will_take(name in a_name()) {
            let id = sanitize(&name);

            prop_assert!(acceptable(&id), "{id:?} from {name:?}");
        }

        /// note: one character in, one character out, up to the cap - a rewrite rather than a
        /// filter. It matters because the alternative reads the same on every name anybody would
        /// think to try: dropping what it cannot use also produces something acceptable, and
        /// turns two names that differ only outside the charset into one identifier.
        #[test]
        fn rewriting_replaces_rather_than_removes(name in a_name()) {
            let expected = name.chars().count().clamp(1, LIMIT);

            prop_assert_eq!(sanitize(&name).chars().count(), expected);
        }

        /// note: idempotent, so that a name arriving already acceptable is left exactly as the
        /// server sent it. Nothing here calls it twice; what this pins is that it *could* be.
        #[test]
        fn rewriting_an_acceptable_name_changes_nothing(name in a_name()) {
            let once = sanitize(&name);

            prop_assert_eq!(sanitize(&once), once);
        }

        /// note: the property the 0.3.1 fix was about, stated over every pair of names rather
        /// than over the sixty-two-character server that found it. The tool's own name is the
        /// half that tells one of a server's tools from another, so it is the half that must
        /// arrive whole however long the prefix was.
        #[test]
        fn a_tools_own_name_survives_however_long_the_prefix(
            prefix in a_name(),
            remote in a_name(),
        ) {
            let id = tool_id(Some(&prefix), &remote);

            prop_assert!(acceptable(&id), "{id:?}");
            prop_assert!(id.ends_with(&sanitize(&remote)), "{id:?} lost {remote:?}");
        }

        /// note: a server that was asked for no prefix gets none, not an empty one - the
        /// difference being a leading `__` on every tool it offers.
        #[test]
        fn an_unprefixed_tool_is_its_own_rewritten_name(remote in a_name()) {
            prop_assert_eq!(tool_id(None, &remote), sanitize(&remote));
        }
    }

    /// A collision, constructed rather than found: two tools on one server arriving under one
    /// identifier.
    ///
    /// note: this is not a bug and is not to be "fixed". [`sanitize`]'s own note says a rewrite
    /// can collide, and [`Installed::replaced`](crate::Installed::replaced) is what the bridge
    /// answers with instead of assuming it displaced nothing - `bridge.rs` checks that it does.
    /// What this pins is the shape of the remaining case, so that nobody reads the 0.3.1 fix as
    /// having made identifiers unique: it made the *tool's* name survive, which is a different
    /// promise and the only one truncation can keep.
    ///
    /// note: no generator would find this. It needs the prefix to be truncated at a boundary the
    /// longer of the two tool names then reproduces out of the separator, and the two characters
    /// the cut lands on to be underscores - and the run that goes looking for it uniformly over
    /// strings will not put a sixty-first character anywhere in particular.
    #[test]
    fn rewriting_can_still_collide_and_the_bridge_reports_it() {
        // sixty-four characters after the rewrite, with underscores at the sixtieth and
        // sixty-first - which is where `room` for the shorter tool name happens to cut
        let server = format!("{}__{}", "y".repeat(59), "y".repeat(10));

        let short = tool_id(Some(&server), "a");
        let long = tool_id(Some(&server), "__a");

        assert_eq!(short, long, "the pair collides");
        assert_eq!(short, format!("{}____a", "y".repeat(59)));
        // and the surviving half of the promise still holds for both of them
        assert!(short.ends_with('a') && long.ends_with("__a"));
    }
}
