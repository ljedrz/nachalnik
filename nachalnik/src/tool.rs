//! Something the model can invoke: what it declares, what it produces, and the trait that runs it.
//!
//! note: three types and no implementations. The kernel executes nothing itself - no filesystem,
//! no process spawning, no network - so this is the whole of what it knows about tools, and
//! [`ToolSpec`]'s capability list is the part a [`PermissionPolicy`](crate::PermissionPolicy)
//! reads before anything runs.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(doc)]
use crate::{Config, Event, Kernel, PermissionPolicy};
use crate::{
    error::BoxError,
    event::OutputSink,
    model::{Content, ToolCall},
    permissions::Capability,
};

/// A tool's definition: its stable identity, its schema, and the capabilities it needs.
///
/// note: The capability list is the tool's own permission declaration, visible to the user before
/// anything runs. What the [`PermissionPolicy`] is asked about is the part of it one call needs -
/// [`Tool::needs`], which is the whole list unless the tool says otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// The tool's stable identifier; this is the name the model calls.
    pub id: String,
    /// What the tool does, as told to the model.
    pub description: String,
    /// The JSON schema of the tool's arguments.
    ///
    /// note: Shared, because [`Tool::spec`] is called afresh for every request and a schema
    /// copied each time is a cost that scales with how useful your tools are.
    ///
    /// note: **descriptive, not enforced.** The kernel sends this to the model and counts what it
    /// costs to send; it never checks a call's arguments against it, and a tool is handed whatever
    /// the model produced. Validating is [`Tool::invoke`]'s job, and a mismatch is an ordinary
    /// [`ToolOutput::error`](crate::ToolOutput::error) - which the model reads and can act on,
    /// where a refusal from underneath would be a failure it never sees the shape of.
    ///
    /// note: that is a deliberate boundary rather than a missing feature. Enforcing it would mean
    /// this crate choosing a JSON Schema dialect and a validator for everybody who ever writes a
    /// tool, on behalf of models that disagree about which dialect they emit for - and the check
    /// is one line in the tool that already has to parse the arguments to use them. A caller who
    /// wants it everywhere can wrap `Tool` once and install the wrapper, which is the seam this
    /// leaves open.
    pub schema: Arc<Value>,
    /// The capabilities an invocation of this tool requires.
    pub capabilities: Vec<Capability>,
    /// The maximum size (in bytes) of this tool's output before the kernel truncates it;
    /// `None` falls back to [`Config::default_tool_output_limit`].
    pub output_limit: Option<usize>,
}

impl ToolSpec {
    /// Creates a spec with an empty object schema, no capabilities and no output limit.
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            schema: Arc::new(json!({ "type": "object", "properties": {}, "required": [] })),
            capabilities: Vec::new(),
            output_limit: None,
        }
    }

    /// Sets the argument schema.
    pub fn with_schema(mut self, schema: impl Into<Arc<Value>>) -> Self {
        self.schema = schema.into();
        self
    }

    /// Declares the capabilities an invocation requires.
    pub fn with_capabilities(mut self, capabilities: impl IntoIterator<Item = Capability>) -> Self {
        self.capabilities = capabilities.into_iter().collect();
        self
    }

    /// Sets the tool's output limit in bytes.
    pub fn with_output_limit(mut self, limit: usize) -> Self {
        self.output_limit = Some(limit);
        self
    }
}

/// What a [`Tool`] produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// The output itself.
    pub content: Content,
    /// Whether the output describes a failure the model should see.
    pub is_error: bool,
}

impl ToolOutput {
    /// Creates a successful output.
    pub fn new(content: impl Into<Content>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    /// Creates a failed output; the model will see it as an error result.
    pub fn error(content: impl Into<Content>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// Something the model can invoke.
///
/// note: The kernel ships no tools. It defines this trait, hands out the arguments the model
/// produced, enforces the permission decision and the output limit, and records everything in
/// the event stream. What a tool *does* is entirely the user's business - which is why there is
/// no filesystem, no process spawning and no network code anywhere in this crate.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Returns the tool's definition.
    ///
    /// note: This is called every time a request is built, so it should be cheap. A tool whose
    /// spec changes between calls is legal (and visible in [`Kernel::preview_request`]), but the
    /// `id` is expected to be stable.
    fn spec(&self) -> ToolSpec;

    /// What *this* call needs, which is at most what the spec declares.
    ///
    /// note: a tool that does one thing declares it once and never implements this. A tool that
    /// does several - `fs`, whose `action` picks between reading a file and writing one - is the
    /// only thing that can say which of them a given call is, because it is the only thing that
    /// knows its own arguments. Before this the policy read the `action` argument itself, which
    /// meant it had to guess from a string's shape whether `<name>:<name>` was an operation or a
    /// tool that happened to have a colon in its name.
    ///
    /// note: the default is the whole of [`ToolSpec::capabilities`], which is right for a tool
    /// with one operation and safe for any other: the strictest of everything consulted wins, so
    /// a tool that declines to narrow is judged against all of it.
    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        let _ = call;
        self.spec().capabilities
    }

    /// How much of *this* call's output the model is shown, where that differs per call.
    ///
    /// note: the sibling of [`Tool::needs`] and for the same reason. A tool that does one thing
    /// has one natural answer size and sets it on the spec; a tool that reads a file and also
    /// searches a directory has two, and thousands of bytes is a reasonable answer to one of
    /// those and not to the other. Only the tool knows which call it was handed.
    ///
    /// note: what it decides is what the *model* is shown, never what is kept. The kernel still
    /// archives the whole of anything this shortens - see
    /// [`Config::keep_truncated_output`] - so a tool narrowing its own answer is not a tool
    /// throwing part of it away.
    fn limit(&self, call: &ToolCall) -> Option<usize> {
        let _ = call;
        self.spec().output_limit
    }

    /// Executes the call.
    ///
    /// note: Returning `Err` is not a kernel failure; the error is turned into an error tool
    /// result, recorded in the context, and handed to the model. Use [`ToolOutput::error`] when
    /// the failure is expected and part of the tool's normal output.
    ///
    /// note: A tool that takes a while should report through `output` as it goes, so that its
    /// progress is on the event stream instead of being invisible until it returns.
    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError>;
}
