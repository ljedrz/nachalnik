//! One tool on a server, as a [`Tool`](nachalnik::Tool) - and how much of what the server says
//! about it is believed.
//!
//! note: [`Trust`] is the decision this crate exists to get right, and the reasoning is on the
//! type.

use nachalnik::{
    Block, BoxError, Capability, Content, Domain, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec,
    async_trait,
};
use rmcp::{
    RoleClient,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, ClientRequest, ContentBlock,
        ResourceContents, ServerResult, ToolAnnotations,
    },
    service::{Peer, PeerRequestOptions},
};
use serde_json::Value;

/// How long a call waits on its server before it looks up to see whether it has been interrupted.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_millis(120);

/// How the capabilities of a server's tools are decided.
///
/// MCP tools may carry annotations describing themselves - `readOnlyHint`, `destructiveHint`,
/// `openWorldHint` - and the specification says, in as many words, that clients "should never make
/// tool use decisions based on annotations received from untrusted servers". They are hints. A
/// server that would rather not be asked about need only claim to be read-only.
///
/// A [`PermissionPolicy`](nachalnik::PermissionPolicy) that acted on those hints would therefore
/// be taking the word of the thing it is meant to be gating. So the default here takes nobody's
/// word for anything.
///
/// note: Whichever of these is in use, every tool from a server also declares `mcp:call` - the
/// operation of calling somebody else's tool, which is true of all of them and is the subject a
/// policy that knows nothing else about them can answer. Which *server* it came from is provenance
/// rather than an act, so it is not spelled as one: [`Server::install`](crate::Server::install)
/// returns the ids it installed, and a client that wants per-server rules holds that mapping.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Trust {
    /// Believe nothing. Tools declare only that they are somebody else's, and what may be done
    /// with them is a decision your policy makes once, knowingly.
    ///
    /// note: The default, because it is the only option here that cannot be talked out of anything
    /// by the server it is describing.
    #[default]
    Nothing,
    /// Give every tool from this server exactly these capabilities, whatever it says about itself,
    /// beside the `mcp:call` every tool from a server declares under every mode.
    ///
    /// note: For when you know what a server is - your own, or one you have read - and want its
    /// tools gated like the rest of your own.
    Fixed(Vec<Capability>),
    /// Believe the server's annotations.
    ///
    /// A tool claiming `readOnlyHint` gets `fs:read`; anything else gets `fs:write` and
    /// `fs:edit`, because the specification's default for that hint is `false` and an absent hint
    /// is not a reassurance. `openWorldHint` adds `net:reach`, and so does its absence: the
    /// specification's default for that one is `true`.
    ///
    /// note: Reasonable for a server you run yourself, and a mistake for one you do not.
    Annotations,
}

impl Trust {
    /// Works out what a tool may do, given what it says about itself.
    pub(crate) fn capabilities(&self, annotations: Option<&ToolAnnotations>) -> Vec<Capability> {
        // note: calling somebody else's tool server is itself the operation, and every tool from
        // one declares it whatever else is believed about them. `Trust::Nothing` believes nothing,
        // and without this its list would be *empty* - and an empty list of capabilities is a
        // call that needs nothing and is allowed by the strictest policy there is
        let mut capabilities = vec![Capability::of(Domain::Other("mcp".into()), "call")];

        match self {
            Self::Nothing => {}
            Self::Fixed(fixed) => capabilities.extend(fixed.iter().cloned()),
            Self::Annotations => {
                let read_only = annotations.and_then(|a| a.read_only_hint).unwrap_or(false);
                match read_only {
                    true => capabilities.push(Capability::fs("read")),
                    false => {
                        capabilities.extend([Capability::fs("write"), Capability::fs("edit")]);
                    }
                }
                if annotations.and_then(|a| a.open_world_hint).unwrap_or(true) {
                    capabilities.push(Capability::net("reach"));
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

    /// note: interruptible while the server works on it, as `shell` is while a command runs. A
    /// server that never answers would otherwise hold the turn for as long as it liked, and escape,
    /// a deadline and a first `ctrl+c` would do nothing. MCP's own `notifications/cancelled` tells
    /// the server to stop, and the model is told the call was stopped part-way, since what the
    /// server had done by then is not known here.
    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        // before dispatching, so that a call interrupted before it went out says it did not run
        if output.is_interrupted() {
            return Ok(ToolOutput::error(
                "interrupted before this call was made; it did not run",
            ));
        }

        // note: a provider keeps arguments it could not parse as text under `_unparsed`, and that
        // object sent on as the arguments was a server answering about a missing parameter in a
        // call whose text holds it. Said here as what it is, and nothing is sent; text there that
        // does parse is a model copying the wrapper around a whole call, and that call is read
        let unwrapped;
        let args = match call.args.get(UNPARSED).and_then(Value::as_str) {
            Some(written) => match serde_json::from_str::<Value>(written) {
                Ok(inner @ Value::Object(_))
                    if call.args.as_object().is_some_and(|a| a.len() == 1) =>
                {
                    unwrapped = inner;
                    &unwrapped
                }
                Ok(_) => &*call.args,
                Err(why) => {
                    return Ok(ToolOutput::error(format!(
                        "the arguments arrived as text rather than as a JSON object ({why}), so \
                         nothing was sent to the server. What arrived was `{}`. Send the call \
                         again, as one JSON object.",
                        quoted(written)
                    )));
                }
            },
            None => &*call.args,
        };
        let arguments = match args {
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

        let sent = self
            .peer
            .send_cancellable_request(
                ClientRequest::CallToolRequest(CallToolRequest::new(request)),
                PeerRequestOptions::no_options(),
            )
            .await;
        let mut handle = match sent {
            Ok(handle) => handle,
            // the server is not going to answer this one; that is a failure the model can act on,
            // not a reason to stop the loop
            // note: not "refused". The server never saw the call: sending fails when the connection
            // is gone, and a model told it was refused goes looking for what it did wrong
            //
            // note: and a closed connection is said to be one. The model calls the server's other
            // tools next, and `Transport closed` alone does not say that each of them will fail the
            // same way
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "the call could not be sent to the MCP server: {e}{}",
                    match e {
                        rmcp::ServiceError::TransportClosed => {
                            ". The connection to it has closed, so every tool it offers fails \
                             this way from here on"
                        }
                        _ => "",
                    }
                )));
            }
        };
        let answered = loop {
            tokio::select! {
                answered = &mut handle.rx => break answered,
                () = tokio::time::sleep(HEARTBEAT) => {
                    if output.is_interrupted() {
                        let told = match handle.cancel(Some("interrupted".to_owned())).await {
                            Ok(()) => "was told to stop",
                            Err(_) => "could not be told to stop",
                        };
                        return Ok(ToolOutput::error(format!(
                            "interrupted while the MCP server was working on it, and the server \
                             {told}; what it had done by then is not known"
                        )));
                    }
                }
            }
        };

        match answered {
            Ok(Ok(ServerResult::CallToolResult(result))) => Ok(output_of(result)),
            Ok(Ok(_)) => Ok(ToolOutput::error(
                "the MCP server answered with something that is not the result of a call",
            )),
            Ok(Err(e)) => Ok(ToolOutput::error(format!("the MCP server refused: {e}"))),
            Err(_) => Ok(ToolOutput::error(
                "the MCP server went away before it answered",
            )),
        }
    }
}

/// The largest image carried into the context, in base64.
///
/// note: Anthropic's limit for one image, the strictest of the APIs that take one in a tool
/// result; past it the request is refused - this one and every later one, while the image stays
/// in the context - so a larger one is named instead, as every image was before.
const LARGEST_IMAGE: usize = 5 * 1024 * 1024;

/// Turns what a server returned into what the kernel records.
///
/// note: MCP results are a list of blocks, and not all of them are text. An image goes in as the
/// picture it is, beside the text it came with, and each provider sends it as its API lets it:
/// Anthropic's and OpenAI's Responses as an image in the result, Gemini in the `functionResponse`'s
/// own parts, Chat Completions as a line naming it, since its tool results are text. Anything else that is not text - audio, a binary resource,
/// an image too large to send - is *named* rather than dropped silently: the model is told that
/// something came back and what it was, which is a better answer than a gap.
fn output_of(result: CallToolResult) -> ToolOutput {
    let failed = result.is_error.unwrap_or(false);

    // a server that returned structured content meant it; it goes in as structure, and the
    // blocks beside it - which MCP says should repeat it as text - are not read
    if let Some(structured) = result.structured_content {
        let content = Content::json(structured);
        return match failed {
            true => ToolOutput::error(content),
            false => ToolOutput::new(content),
        };
    }

    // text runs are joined as they always were; a picture splits them
    let mut parts: Vec<Content> = Vec::with_capacity(result.content.len());
    let mut text: Vec<String> = Vec::new();
    // by value: the result is this function's, and a picture is up to five megabytes of base64
    // that a borrow would have to copy into the `Content` it becomes
    for block in result.content {
        text.push(match block {
            ContentBlock::Text(said) => said.text,
            ContentBlock::Image(image) if image.data.len() <= LARGEST_IMAGE => {
                if !text.is_empty() {
                    parts.push(Content::text(text.join("\n")));
                    text.clear();
                }
                parts.push(Content::blob(image.mime_type, image.data));
                continue;
            }
            ContentBlock::Image(image) => {
                left_out("an image too large to send", Some(&image.mime_type))
            }
            ContentBlock::Audio(audio) => left_out("audio", Some(&audio.mime_type)),
            ContentBlock::Resource(resource) => match text_of(&resource.resource) {
                Ok(text) => text.to_owned(),
                Err(media) => left_out("an embedded resource", media),
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

    let content = match parts.is_empty() {
        true => Content::text(text.join("\n")),
        false => {
            if !text.is_empty() {
                parts.push(Content::text(text.join("\n")));
            }
            Content::blocks(parts.into_iter().map(Block::text))
        }
    };
    match failed {
        true => ToolOutput::error(content),
        false => ToolOutput::new(content),
    }
}

/// What stands in for a part that is not text: what it was, and the type it said it had.
pub(crate) fn left_out(what: &str, media: Option<&str>) -> String {
    format!(
        "[{what} ({}), not carried into the context]",
        media.unwrap_or("no media type given")
    )
}

/// The text of one part of a resource, or the media type of a part that has none.
///
/// note: one reading for both places a resource arrives - embedded in a call's result, and read
/// on its own by `Server::resources` - so that the two say the same of the same part. Matched on
/// the type rather than read out of its JSON, which copied the whole of a blob to find that it
/// had no text.
pub(crate) fn text_of(contents: &ResourceContents) -> Result<&str, Option<&str>> {
    match contents {
        ResourceContents::TextResourceContents { text, .. } => Ok(text),
        ResourceContents::BlobResourceContents { mime_type, .. } => Err(mime_type.as_deref()),
        // `#[non_exhaustive]`: a kind of part this crate has not heard of is one with no text it
        // can read, rather than one that vanishes
        _ => Err(None),
    }
}

/// Builds the declaration the model is shown for one of a server's tools.
pub(crate) fn spec_of(id: String, tool: &rmcp::model::Tool, trust: &Trust) -> ToolSpec {
    let description = tool
        .description
        .as_deref()
        .unwrap_or("a tool on an MCP server, which said nothing about what it does")
        .to_owned();

    ToolSpec::new(id, description)
        .with_schema(Value::Object((*tool.input_schema).clone()))
        .with_capabilities(trust.capabilities(tool.annotations.as_ref()))
}

/// What a model provider will accept in a tool's name: `[a-zA-Z0-9_-]`, starting with a letter or
/// `_`, and no more than this many of them.
///
/// note: the start is Google's rule - its API refuses a declaration named `7zip__list` with a 400,
/// for every request the tool is offered in - and the charset and the length OpenAI's. A name has
/// to pass both, since the same tools go to every dialect.
const LIMIT: usize = 64;

/// What separates a server's name from its tool's.
const SEPARATOR: &str = "__";

/// Rewrites a name into the characters a model provider will accept, and no more of them than it
/// will; [`tool_id`] then sees to how it starts.
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
        return led(remote);
    };

    let room = LIMIT.saturating_sub(remote.chars().count() + SEPARATOR.len());
    let prefix: String = led(sanitize(prefix)).chars().take(room).collect();

    match prefix.is_empty() {
        true => led(remote),
        false => format!("{prefix}{SEPARATOR}{remote}"),
    }
}

/// Starts an identifier with a letter or `_`, by putting a `_` in front of one that starts with a
/// digit or a `-`.
///
/// note: put in front rather than written over, so that `1password` and `2password` stay two
/// tools. Only an identifier already at [`LIMIT`] has its first character replaced instead.
fn led(id: String) -> String {
    if id.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return id;
    }

    match id.chars().count() < LIMIT {
        true => format!("_{id}"),
        false => id.chars().skip(1).fold(String::from("_"), |mut out, c| {
            out.push(c);
            out
        }),
    }
}

/// The key a provider puts a call's arguments under when they would not parse as JSON.
const UNPARSED: &str = "_unparsed";

/// How much of the text that did not parse is quoted back.
const SHOWN: usize = 200;

/// The first [`SHOWN`] characters of `written`, and how much more there was.
fn quoted(written: &str) -> String {
    match written.char_indices().nth(SHOWN) {
        Some((cut, _)) => format!("{}… ({} bytes in all)", &written[..cut], written.len()),
        None => written.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Whether an identifier is one a model provider will take, which is all [`sanitize`]
    /// promises.
    fn acceptable(id: &str) -> bool {
        id.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
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
    /// note: the lengths are stated because proptest's `*` tops out around thirty characters, so
    /// `(?s).*` never produces a name that reaches [`LIMIT`], and raising the cap would break none
    /// of the properties below. Truncation is half of what [`sanitize`] does, `{60,70}` is
    /// the half of the strategy that reaches it, and it is weighted highest because the boundary
    /// is where this function goes wrong.
    fn a_name() -> impl Strategy<Value = String> {
        prop_oneof![
            2 => "(?s).{0,8}",
            3 => "(?s).{60,70}",
            1 => "(?s).{0,200}",
        ]
    }

    proptest! {
        // note: no seed file, the way the runtime's properties are configured. A failure is
        // reproduced by lifting the counterexample it prints into a named case
        #![proptest_config(ProptestConfig {
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        #[test]
        fn a_rewritten_name_is_one_a_provider_will_take(name in a_name()) {
            let id = tool_id(None, &name);

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

        /// note: a rewrite that turned every `-` into `_` would still hand back a name a provider
        /// takes. This says it leaves the server's name alone: `list-changes` and `list_changes`
        /// are two tools a server may offer side by side. Beside the first property it also makes
        /// the rewrite idempotent, so no property says that on its own.
        #[test]
        fn an_acceptable_name_arrives_as_the_server_sent_it(name in "[a-zA-Z0-9_-]{1,64}") {
            prop_assert_eq!(sanitize(&name), name);
        }

        /// note: stated over every pair of names rather than over one long server name. The
        /// tool's own name is the half that tells one of a server's tools from another, so it is
        /// the half that must arrive whole however long the prefix was.
        #[test]
        fn a_tools_own_name_survives_however_long_the_prefix(
            prefix in a_name(),
            remote in a_name(),
        ) {
            let id = tool_id(Some(&prefix), &remote);

            prop_assert!(acceptable(&id), "{id:?}");
            // all of it but a first character that had to give way to the `_` in front
            let own: String = sanitize(&remote).chars().skip(1).collect();
            prop_assert!(id.ends_with(&own), "{id:?} lost {remote:?}");
        }

        /// note: a server that was asked for no prefix gets none, not an empty one - the
        /// difference being a leading `__` on every tool it offers.
        #[test]
        fn an_unprefixed_tool_is_its_own_rewritten_name(remote in a_name()) {
            prop_assert_eq!(tool_id(None, &remote), led(sanitize(&remote)));
        }

        /// note: two names that differ only in a leading digit stay two tools, which writing a
        /// `_` over the digit would have made one.
        #[test]
        fn a_leading_digit_is_kept_behind_an_underscore(remote in "[0-9-][a-zA-Z0-9_-]{0,62}") {
            prop_assert_eq!(tool_id(None, &remote), format!("_{remote}"));
        }
    }

    /// A collision, constructed rather than found: two tools on one server arriving under one
    /// identifier.
    ///
    /// note: this is not a bug and is not to be "fixed". [`sanitize`]'s own note says a rewrite
    /// can collide, and [`Installed::replaced`](crate::Installed::replaced) is what the bridge
    /// answers with instead of assuming it displaced nothing - `bridge.rs` checks that it does.
    /// What this pins is the shape of the remaining case, so that nobody reads the prefix giving
    /// way in [`tool_id`] as having made identifiers unique: it makes the *tool's* name survive,
    /// which is a different promise and the only one truncation can keep.
    ///
    /// note: no generator would find this. It needs the prefix to be truncated at a boundary the
    /// longer of the two tool names then reproduces out of the separator, and the two characters
    /// the cut lands on to be underscores - and a search that is uniform over strings will not
    /// put a sixty-first character anywhere in particular.
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
