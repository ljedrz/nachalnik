//! Helpers for testing an agent without a model provider, enabled by the `test` feature.
//!
//! note: These exist so that the kernel's own tests never need a network, and so that yours
//! don't either. The permission policies here are also the off-the-shelf ones the core
//! deliberately does not ship - a kernel has no business having opinions about what is allowed,
//! but a test does.

use std::{collections::BTreeMap, collections::VecDeque, sync::Arc};

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::{
    compaction::{Budget, CompactionPlan, Compactor},
    context::{ContextItem, ContextKind, ContextState},
    error::BoxError,
    event::{DeltaSink, OutputSink},
    model::{Content, ModelInfo, ModelRequest, ModelResponse, Provider, ToolCall},
    permissions::{Capability, PermissionPolicy, PermissionRequest, Verdict},
    tool::{Tool, ToolOutput, ToolSpec},
};

/// A [`Provider`] that answers with a prepared list of responses, and remembers what it was
/// asked.
pub struct ScriptedProvider {
    info: ModelInfo,
    script: Mutex<VecDeque<ModelResponse>>,
    seen: Mutex<Vec<ModelRequest>>,
}

impl ScriptedProvider {
    /// Creates a provider that will answer with the given responses, in order.
    pub fn new(responses: impl IntoIterator<Item = ModelResponse>) -> Self {
        Self {
            info: ModelInfo {
                context_limit: Some(128_000),
                tool_calling: true,
                ..ModelInfo::new("scripted", "scripted")
            },
            script: Mutex::new(responses.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Overrides the reported model identity.
    pub fn with_info(mut self, info: ModelInfo) -> Self {
        self.info = info;
        self
    }

    /// Returns the requests the provider has been sent, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.seen.lock().clone()
    }

    /// Returns the number of unused responses.
    pub fn remaining(&self) -> usize {
        self.script.lock().len()
    }
}

#[async_trait::async_trait]
impl Provider for ScriptedProvider {
    fn info(&self) -> ModelInfo {
        self.info.clone()
    }

    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        self.seen.lock().push(request);
        let response = self.script.lock().pop_front();

        match response {
            Some(response) => {
                if let Some(content) = &response.content {
                    deltas.text(content.to_text().into_owned());
                }
                Ok(response)
            }
            None => Err("the script ran out of responses".into()),
        }
    }
}

/// A [`Tool`] that returns its own arguments.
pub struct EchoTool {
    spec: ToolSpec,
}

impl EchoTool {
    /// Creates an echo tool with the given identifier and required capabilities.
    pub fn new(id: impl Into<String>, capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            spec: ToolSpec::new(id, "returns its arguments")
                .with_schema(json!({
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "required": ["value"],
                }))
                .with_capabilities(capabilities),
        }
    }

    /// Sets the tool's output limit.
    pub fn with_output_limit(mut self, limit: usize) -> Self {
        self.spec.output_limit = Some(limit);
        self
    }
}

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        Ok(ToolOutput::new(Content::Json(call.args.clone())))
    }
}

/// A [`Tool`] that always returns the same output, reporting it through the sink first.
pub struct ConstTool {
    spec: ToolSpec,
    output: Content,
}

impl ConstTool {
    /// Creates a tool that always answers with `output` and requires no capabilities.
    pub fn new(id: impl Into<String>, output: impl Into<Content>) -> Self {
        Self {
            spec: ToolSpec::new(id, "returns a fixed value"),
            output: output.into(),
        }
    }

    /// Declares the capabilities the tool requires.
    pub fn with_capabilities(mut self, capabilities: impl IntoIterator<Item = Capability>) -> Self {
        self.spec.capabilities = capabilities.into_iter().collect();
        self
    }

    /// Sets the tool's output limit.
    pub fn with_output_limit(mut self, limit: usize) -> Self {
        self.spec.output_limit = Some(limit);
        self
    }

    /// Sets the tool's argument schema.
    pub fn with_schema(mut self, schema: Value) -> Self {
        self.spec = self.spec.with_schema(schema);
        self
    }
}

#[async_trait::async_trait]
impl Tool for ConstTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn invoke(&self, _call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        output.push(self.output.to_text().into_owned());

        Ok(ToolOutput::new(self.output.clone()))
    }
}

/// A [`Tool`] that always fails.
pub struct BrokenTool {
    spec: ToolSpec,
}

impl BrokenTool {
    /// Creates a tool whose every invocation returns an error.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            spec: ToolSpec::new(id, "always fails"),
        }
    }
}

#[async_trait::async_trait]
impl Tool for BrokenTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn invoke(&self, _call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        Err("this tool is broken".into())
    }
}

/// A [`PermissionPolicy`] that allows everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

#[async_trait::async_trait]
impl PermissionPolicy for AllowAll {
    async fn evaluate(&self, _request: &PermissionRequest) -> Verdict {
        Verdict::Allow
    }
}

/// A [`PermissionPolicy`] that denies everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyAll;

#[async_trait::async_trait]
impl PermissionPolicy for DenyAll {
    async fn evaluate(&self, _request: &PermissionRequest) -> Verdict {
        Verdict::Deny
    }
}

/// A [`PermissionPolicy`] mapping capabilities to verdicts, with the strictest verdict among a
/// tool's declared capabilities winning.
///
/// ```
/// use nachalnik::{Capability, Verdict, test::Table};
///
/// let policy = Table::new(Verdict::Ask)
///     .rule(Capability::Read, Verdict::Allow)
///     .rule(Capability::Network, Verdict::Deny);
/// ```
#[derive(Debug, Clone)]
pub struct Table {
    /// The per-capability verdicts.
    pub rules: BTreeMap<Capability, Verdict>,
    /// The verdict for a capability that has no rule, and for a tool that declares none.
    pub default: Verdict,
}

impl Table {
    /// Creates a table in which every capability falls back to `default`.
    pub fn new(default: Verdict) -> Self {
        Self {
            rules: BTreeMap::new(),
            default,
        }
    }

    /// Adds a rule for a capability.
    pub fn rule(mut self, capability: Capability, verdict: Verdict) -> Self {
        self.rules.insert(capability, verdict);
        self
    }
}

impl Default for Table {
    fn default() -> Self {
        Self::new(Verdict::Ask)
    }
}

#[async_trait::async_trait]
impl PermissionPolicy for Table {
    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        request
            .capabilities
            .iter()
            .map(|c| self.rules.get(c).copied().unwrap_or(self.default))
            .reduce(Verdict::strictest)
            .unwrap_or(self.default)
    }
}

/// A [`Compactor`] that excludes the largest tool results once the context passes a fraction of
/// the model's limit, and leaves a note in their place.
///
/// note: It is purely mechanical - it never asks a model to summarize anything - which makes it
/// predictable enough to test against.
pub struct LargestFirstCompactor {
    /// The fraction of the context limit at which it starts working.
    pub threshold: f64,
    /// The fraction of the context limit it tries to get back down to.
    pub target: f64,
}

impl Default for LargestFirstCompactor {
    fn default() -> Self {
        Self {
            threshold: 0.8,
            target: 0.5,
        }
    }
}

#[async_trait::async_trait]
impl Compactor for LargestFirstCompactor {
    fn should_compact(&self, budget: &Budget) -> bool {
        budget
            .fraction_used()
            .is_some_and(|used| used >= self.threshold)
    }

    async fn plan(&self, items: &[Arc<ContextItem>], budget: &Budget) -> Option<CompactionPlan> {
        let limit = budget.limit?;
        let target = (limit as f64 * self.target) as usize;

        let mut candidates: Vec<_> = items
            .iter()
            .filter(|i| {
                i.state == ContextState::Active && matches!(i.kind, ContextKind::ToolResult { .. })
            })
            .collect();
        candidates.sort_by_key(|i| std::cmp::Reverse(i.tokens));

        let mut used = budget.used();
        let mut remove = Vec::new();
        let mut freed = 0;
        for item in candidates {
            if used <= target {
                break;
            }
            used -= item.tokens.min(used);
            freed += item.tokens;
            remove.push(item.id);
        }

        if remove.is_empty() {
            return None;
        }

        Some(CompactionPlan {
            summary: Some(ContextItem::summary(format!(
                "{} tool result(s) worth ~{freed} tokens were removed from the context",
                remove.len()
            ))),
            reason: format!(
                "the context reached {}% of the {limit}-token limit",
                (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize
            ),
            remove,
        })
    }
}

/// Returns a tool call with the given identifier, tool and arguments.
pub fn call(id: &str, tool: &str, args: Value) -> ToolCall {
    ToolCall::new(id, tool, args)
}
