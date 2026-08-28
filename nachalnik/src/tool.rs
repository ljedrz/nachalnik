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
/// note: The capability list is the tool's own permission declaration. It is what the
/// [`PermissionPolicy`] sees, and it is visible to the user before anything runs.
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
