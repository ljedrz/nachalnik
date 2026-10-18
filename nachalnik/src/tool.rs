//! Tools the model can invoke: what they declare, what they produce, and the trait that runs them.
//!
//! note: three types and no implementations, because the kernel executes nothing itself: no
//! filesystem, no processes, no network. [`ToolSpec`]'s capability list is what
//! [`PermissionPolicy`](crate::PermissionPolicy) reads before anything runs.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(doc)]
use crate::{Config, Kernel, PermissionPolicy};
use crate::{
    error::BoxError,
    event::OutputSink,
    model::{Content, ToolCall},
    permissions::Capability,
};

/// A tool's definition: its stable identity, its schema, and the capabilities it needs.
///
/// note: The capability list is the tool's own permission declaration, visible to the user before
/// anything runs. [`PermissionPolicy`] is asked about the part one call needs: [`Tool::needs`],
/// which is the whole list unless the tool says otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// The tool's stable identifier; this is the name the model calls.
    pub id: String,
    /// What the tool does, as told to the model.
    pub description: String,
    /// The JSON schema of the tool's arguments.
    ///
    /// note: shared, because [`Tool::spec`] is called afresh for every request, and copying the
    /// schema each time would cost more the more useful the tools are.
    ///
    /// note: descriptive, not enforced. The kernel sends it to the model and counts what that
    /// costs, but never checks a call's arguments against it; the tool receives whatever the model
    /// produced and validating is [`Tool::invoke`]'s job. A mismatch is then an ordinary
    /// [`ToolOutput::error`](crate::ToolOutput::error) the model reads and can act on, where a
    /// refusal from underneath would be a failure it never sees the shape of.
    ///
    /// note: enforcing it would make this crate choose a JSON Schema dialect and validator for
    /// every tool author, for models that disagree about dialects, when the check is one line in a
    /// tool that already parses its arguments. A caller who wants it everywhere can wrap `Tool`
    /// once.
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
/// note: the kernel ships no tools. It hands a tool the arguments the model produced, enforces the
/// permission decision and the output limit, and records everything on the event stream; what the
/// tool does is the user's business.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Returns the tool's definition.
    ///
    /// note: called every time a request is built, so it should be cheap. A spec that changes
    /// between calls is legal and visible in [`Kernel::preview_request`], but its `id` should stay
    /// the same.
    ///
    /// note: called with the kernel's tool registry locked, so it must not call back into the
    /// [`Kernel`] it is registered with; [`Tool::invoke`] runs with no lock held and may.
    ///
    /// note: it must not panic. A panic in [`Tool::invoke`], [`Tool::needs`], [`Tool::limit`] or
    /// [`Tool::unparsed`], which read what a model wrote, is caught and answers the call; this one
    /// reads nothing the model wrote, is asked wherever a tool is described, and is outside any
    /// catch.
    fn spec(&self) -> ToolSpec;

    /// What *this* call needs, at most what the spec declares.
    ///
    /// note: a tool that does one thing declares it once and never implements this. Only a tool
    /// that does several, such as `fs`, whose `action` picks between reading and writing, knows
    /// which one a given call is, because only it understands its arguments; a policy reading
    /// `action` would have to guess whether `<name>:<name>` was an operation or a tool with a colon
    /// in its name.
    ///
    /// note: the default is the whole of [`ToolSpec::capabilities`], right for a tool with one
    /// operation and safe for any other, since the strictest of everything consulted wins.
    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        let _ = call;
        self.spec().capabilities
    }

    /// How much of *this* call's output the model is shown, where that differs per call.
    ///
    /// note: the sibling of [`Tool::needs`], for the same reason: a tool that reads a file and
    /// searches a directory has two natural answer sizes, and only the tool knows which call it was
    /// handed. A tool with one sets its limit on the spec.
    ///
    /// note: it decides what the model is shown, never what is kept: unless
    /// [`Config::keep_truncated_output`] is off, the kernel keeps the whole of anything this
    /// shortens, excluded.
    fn limit(&self, call: &ToolCall) -> Option<usize> {
        let _ = call;
        self.spec().output_limit
    }

    /// What to add to the kernel's answer to a call of this tool whose arguments were not a JSON
    /// object, given what the model wrote; see [`ToolCall::unparsed`].
    ///
    /// note: the kernel answers such a call itself, without asking the policy or handing the tool a
    /// call with no arguments, and says which tool, that the arguments did not parse, where, and
    /// what was written there. It cannot say what should have been written, which depends on the
    /// tool's arguments and the faults models make writing them. The default adds nothing, and
    /// nothing is often better: measured, advice about escaping sent models to escape what was
    /// already right.
    fn unparsed(&self, written: &str) -> Option<String> {
        let _ = written;
        None
    }

    /// Executes the call.
    ///
    /// note: returning `Err` is not a kernel failure: the error becomes an error tool result,
    /// recorded in the context and handed to the model. Use [`ToolOutput::error`] when the failure
    /// is expected and part of the tool's normal output.
    ///
    /// note: a tool that takes a while should report through `output` as it goes, so its progress
    /// is on the event stream rather than invisible until it returns.
    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError>;
}
