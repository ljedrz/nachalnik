//! Tools the model can invoke: what they declare, what they produce, and the trait that runs them.
//!
//! note: three types and no implementations. The kernel executes nothing itself: no filesystem, no
//! process spawning, no network. This is all it knows about tools. [`ToolSpec`]'s capability list
//! is the part [`PermissionPolicy`](crate::PermissionPolicy) reads before anything runs.

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
    /// note: Shared, because [`Tool::spec`] is called afresh for every request and copying the
    /// schema each time costs more the more useful the tools are.
    ///
    /// note: descriptive, not enforced. The kernel sends it to the model and counts the cost of
    /// sending; it never checks a call's arguments against it, and the tool receives whatever the
    /// model produced. Validating is [`Tool::invoke`]'s job. A mismatch is an ordinary
    /// [`ToolOutput::error`](crate::ToolOutput::error) that the model reads and can act on; a
    /// refusal from underneath would be a failure the model never sees the shape of.
    ///
    /// note: enforcing it would make this crate choose a JSON Schema dialect and a validator for
    /// everyone who writes a tool, for models that disagree about dialects. The check is one line
    /// in the tool, which already parses the arguments to use them. A caller who wants it
    /// everywhere can wrap `Tool` once and install the wrapper.
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
/// produced, enforces the permission decision and the output limit, and records everything in the
/// event stream. What a tool does is the user's business, so there is no filesystem, no process
/// spawning and no network code in this crate.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Returns the tool's definition.
    ///
    /// note: Called every time a request is built, so it should be cheap. A tool whose spec changes
    /// between calls is legal and visible in [`Kernel::preview_request`], but its `id` is expected
    /// to be stable.
    ///
    /// note: Called with the kernel's tool registry locked, so it must not call back into the
    /// [`Kernel`] it is registered with. [`Tool::invoke`] runs with no lock held and may.
    ///
    /// note: It must not panic. A panic in [`Tool::invoke`], [`Tool::needs`], [`Tool::limit`] or
    /// [`Tool::unparsed`] - the ones that read what a model wrote - is caught and answers the call.
    /// This one reads nothing the model wrote, is asked wherever a tool is described, and is
    /// outside any catch.
    fn spec(&self) -> ToolSpec;

    /// What *this* call needs, at most what the spec declares.
    ///
    /// note: a tool that does one thing declares it once and never implements this. A tool that
    /// does several - `fs`, whose `action` picks between reading a file and writing one - is the
    /// only thing that can say which of them a given call is, because only it knows its own
    /// arguments. A policy reading the `action` argument would have to guess from a string's shape
    /// whether `<name>:<name>` was an operation or a tool with a colon in its name.
    ///
    /// note: the default is the whole of [`ToolSpec::capabilities`], right for a tool with one
    /// operation and safe for any other: the strictest of everything consulted wins, so a tool that
    /// declines to narrow is judged against all of it.
    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        let _ = call;
        self.spec().capabilities
    }

    /// How much of *this* call's output the model is shown, where that differs per call.
    ///
    /// note: the sibling of [`Tool::needs`] and for the same reason. A tool that does one thing has
    /// one natural answer size and sets it on the spec. A tool that reads a file and searches a
    /// directory has two, and thousands of bytes is reasonable for one but not the other. Only the
    /// tool knows which call it was handed.
    ///
    /// note: what it decides is what the *model* is shown, never what is kept. Unless
    /// [`Config::keep_truncated_output`] is off, the kernel keeps the whole of anything this
    /// shortens, excluded, so a tool narrowing its own answer is not throwing part of it away.
    fn limit(&self, call: &ToolCall) -> Option<usize> {
        let _ = call;
        self.spec().output_limit
    }

    /// What to add to the kernel's answer to a call of this tool whose arguments were not a JSON
    /// object, given what the model wrote; see [`ToolCall::unparsed`].
    ///
    /// note: the kernel answers such a call itself: the policy is not asked, and the tool is not
    /// handed it, since there are no arguments to read. It says which tool, that the arguments did
    /// not parse, where, and what was written there. What it cannot say is what the model should
    /// have written instead; that depends on the tool's arguments. A tool whose arguments are laid
    /// out its own way knows the faults a model makes writing them. The default adds nothing, and
    /// nothing is often better: measured, advice about escaping sent models to escape what was
    /// already right.
    fn unparsed(&self, written: &str) -> Option<String> {
        let _ = written;
        None
    }

    /// Executes the call.
    ///
    /// note: Returning `Err` is not a kernel failure. The error becomes an error tool result,
    /// recorded in the context and handed to the model. Use [`ToolOutput::error`] when the failure
    /// is expected and part of the tool's normal output.
    ///
    /// note: A tool that takes a while should report through `output` as it goes, so its progress
    /// is on the event stream instead of invisible until it returns.
    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError>;
}
