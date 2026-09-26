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
    model::{
        Content, ModelInfo, ModelRequest, ModelResponse, Overrun, Provider, TooLong, ToolCall,
    },
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

/// A [`Provider`] that refuses everything, the way a model refuses a request longer than it
/// will read.
///
/// note: it fails with a [`TooLong`] rather than with a sentence, because reading a vendor's
/// wording is a dialect's job, while what to do with the numbers is the kernel's. The two are
/// worth keeping apart in a test for the same reason they are in the crates.
pub struct TooLongProvider {
    info: ModelInfo,
    tokens: u64,
    limit: u64,
}

impl TooLongProvider {
    /// Creates a provider that refuses every request as `tokens` long against a limit of `limit`.
    pub fn new(tokens: u64, limit: u64) -> Self {
        Self {
            info: ModelInfo {
                context_limit: Some(limit as usize),
                tool_calling: true,
                ..ModelInfo::new("scripted", "scripted")
            },
            tokens,
            limit,
        }
    }
}

#[async_trait::async_trait]
impl Provider for TooLongProvider {
    fn info(&self) -> ModelInfo {
        self.info.clone()
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        _deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        Err(TooLong {
            overrun: Overrun {
                tokens: self.tokens,
                limit: Some(self.limit),
            },
            said: format!(
                "the request is {} tokens long and the model takes {}",
                self.tokens, self.limit
            ),
        }
        .into())
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

/// A [`PermissionPolicy`] mapping capabilities to verdicts, with the strictest verdict among the
/// capabilities a call needs winning.
///
/// ```
/// use nachalnik::{Capability, Verdict, test::Table};
///
/// let policy = Table::new(Verdict::Ask)
///     .rule(Capability::fs("read"), Verdict::Allow)
///     .rule(Capability::net("reach"), Verdict::Deny);
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
            // this one *removes* rather than elides, so that the crate's own tests keep
            // exercising the orphan repair that removal needs; `kamchatka`'s compactor elides
            elide: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextId;

    /// A budget putting the next request at `used` tokens of a `limit`-token model.
    fn budget(used: usize, limit: usize) -> Budget {
        Budget {
            context_tokens: used,
            tool_tokens: 0,
            uncounted: 0,
            limit: Some(limit),
            reported: None,
        }
    }

    /// `item`, given an identifier and a size.
    ///
    /// note: a pass reads what an item is and what it costs, so a test of which items it reaches
    /// for says both rather than arranging content for a counter to measure.
    fn sized(item: ContextItem, id: u64, tokens: usize) -> Arc<ContextItem> {
        Arc::new(ContextItem {
            id: ContextId(id),
            tokens,
            ..item
        })
    }

    /// A tool result, given an identifier and a size.
    fn result(id: u64, tokens: usize) -> Arc<ContextItem> {
        sized(
            ContextItem::tool_result("c1".into(), "grep", "x", false),
            id,
            tokens,
        )
    }

    /// `remaining` counts the answers still in the script, and one goes with each request.
    #[tokio::test]
    async fn the_scripted_provider_counts_the_answers_it_has_left() {
        let provider =
            ScriptedProvider::new([ModelResponse::text("one"), ModelResponse::text("two")]);
        assert_eq!(provider.remaining(), 2);

        let request = ModelRequest {
            messages: Vec::new(),
            tools: Vec::new(),
            params: Default::default(),
        };
        provider
            .respond(request, DeltaSink::disconnected())
            .await
            .unwrap();
        assert_eq!(provider.remaining(), 1);
    }

    /// A pass is not due while the context is under the threshold.
    #[test]
    fn a_pass_is_not_due_below_the_threshold() {
        let compactor = LargestFirstCompactor::default();

        assert!(!compactor.should_compact(&budget(250, 1_000)));
        assert!(compactor.should_compact(&budget(900, 1_000)));
    }

    /// A pass takes tool results and nothing else, however large the rest is.
    #[tokio::test]
    async fn a_pass_takes_only_tool_results() {
        let file = sized(ContextItem::file("notes.md", "x"), 1, 400);
        let taken = result(2, 300);

        let plan = LargestFirstCompactor::default()
            .plan(&[file, taken.clone()], &budget(1_000, 1_000))
            .await
            .expect("a result is there to take");
        assert_eq!(plan.remove, vec![taken.id]);
    }

    /// A pass takes the largest results until the context is down to the target, stops there,
    /// and its summary says how much it took.
    ///
    /// note: every result taken is content the model no longer sees, so one taken past the
    /// target is a loss nothing asked for. The summary is what the model reads in its place.
    #[tokio::test]
    async fn a_pass_takes_until_the_target_and_stops_there() {
        let big = result(1, 300);
        let middle = result(2, 200);
        let small = result(3, 100);

        // a thousand against a target of five hundred: the two largest bring it down to it
        let plan = LargestFirstCompactor::default()
            .plan(&[small, big.clone(), middle.clone()], &budget(1_000, 1_000))
            .await
            .expect("the context is over the target");
        assert_eq!(plan.remove, vec![big.id, middle.id]);
        let summary = plan.summary.expect("a removal is summarised");
        assert_eq!(
            summary.content.as_text(),
            Some("2 tool result(s) worth ~500 tokens were removed from the context")
        );
    }
}
