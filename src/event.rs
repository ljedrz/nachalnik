use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(doc)]
use crate::{Config, Provider, Tool};
use crate::{
    compaction::CompactionReport,
    context::{ContextId, ContextState},
    kernel::{Kernel, State},
    model::{Content, ModelInfo, Params, StopReason, ToolCallId, Usage},
    permissions::{Grant, GrantSource, PermissionId, PermissionRequest},
    projection::Skipped,
};

/// A fragment of a streamed model response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Delta {
    /// A piece of the response text.
    Text(String),
    /// A piece of the model's reasoning.
    Reasoning(String),
    /// A piece of a tool call's arguments.
    ToolArgs {
        /// The call being assembled.
        call: ToolCallId,
        /// The fragment.
        fragment: String,
    },
}

/// Everything the kernel does, as it happens.
///
/// note: This enum is the whole observability story: there is no logging the user cannot see,
/// and no state change that skips it. Clients subscribe with [`Kernel::subscribe`] and render
/// what they like; the same events, minus the deltas, form the session log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event")]
#[non_exhaustive]
pub enum Event {
    /// A session began.
    #[serde(rename = "session.started")]
    SessionStarted {
        /// The session's name.
        session: String,
    },
    /// A session was rebuilt from a [`Snapshot`](crate::Snapshot).
    ///
    /// note: One event, not one `context.added` per item. Those describe things happening; this
    /// describes a session that already happened being picked back up, and a client that replayed
    /// a thousand additions to say so would be telling a story that is not true.
    #[serde(rename = "session.resumed")]
    SessionResumed {
        /// The session's name.
        session: String,
        /// How many items came back.
        items: usize,
        /// What the projected ones are estimated to cost.
        tokens: usize,
    },
    /// A session was declared finished by its owner.
    #[serde(rename = "session.finished")]
    SessionFinished,
    /// Somebody asked the loop to stop at the next opportunity.
    #[serde(rename = "turn.interrupted")]
    Interrupted,
    /// The runtime moved from one state to another.
    ///
    /// note: Together with the context events, this is enough to render what the agent is doing
    /// without inferring anything: every transition the loop makes is announced.
    #[serde(rename = "state.changed")]
    StateChanged {
        /// The state it was in.
        from: State,
        /// The state it is in now.
        to: State,
    },
    /// A context item was added.
    #[serde(rename = "context.added")]
    ContextAdded {
        /// The new item's identifier.
        id: ContextId,
        /// The name of its kind.
        kind: String,
        /// Where it came from.
        source: String,
        /// Its label.
        label: String,
        /// Its estimated size.
        tokens: usize,
        /// Why it was added, if a reason was given.
        because: Option<String>,
    },
    /// A context item changed state, e.g. was excluded from the projection or pinned.
    #[serde(rename = "context.changed")]
    ContextChanged {
        /// The item's identifier.
        id: ContextId,
        /// The state it was in.
        from: ContextState,
        /// The state it is in now.
        to: ContextState,
        /// Why.
        note: Option<String>,
    },
    /// A context item's content was replaced in place.
    #[serde(rename = "context.replaced")]
    ContextReplaced {
        /// The item's identifier.
        id: ContextId,
        /// Its estimated size before.
        tokens_before: usize,
        /// Its estimated size after.
        tokens_after: usize,
        /// What it said before.
        ///
        /// note: This is the one event that carries content, and the rule it follows is: the log
        /// records what nothing else can recover. A [`Event::ContextAdded`] needs no content,
        /// because the item is still in the context and in any [`Snapshot`](crate::Snapshot). A
        /// replacement is different - it is the only operation left that overwrites something,
        /// and without this the old text would exist nowhere once it fell out of the undo window.
        ///
        /// note: It is also what keeps [`Event::ModelRequested`] honest. That names the items a
        /// request was built from rather than copying their contents, which is what makes the log
        /// affordable; but a reference is only worth as much as the thing it points at holding
        /// still. With this, a snapshot and the log can be wound back to reconstruct exactly what
        /// any past request contained.
        was: Content,
    },
    /// A context change was undone.
    ///
    /// note: An undo that only said how many items were left would be the one context change a
    /// client could not render, and the one the user is most likely to want to check.
    #[serde(rename = "context.undone")]
    ContextUndone {
        /// The number of items the context holds now.
        items: usize,
        /// The items the undo took back out of existence.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed: Vec<ContextId>,
        /// The items whose state, note or content it reverted.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<ContextId>,
    },
    /// An undone context change was put back.
    #[serde(rename = "context.redone")]
    ContextRedone {
        /// The number of items the context holds now.
        items: usize,
        /// The items the redo brought back into existence.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restored: Vec<ContextId>,
        /// The items whose state, note or content it changed again.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<ContextId>,
    },
    /// An item's metadata was replaced.
    ///
    /// note: The kernel never reads [`ContextItem::meta`](crate::ContextItem::meta), but a
    /// [`Compactor`](crate::Compactor) does, and a hint that decides what gets dropped is not
    /// the sort of thing that should be able to change without anybody seeing it.
    #[serde(rename = "context.annotated")]
    ContextAnnotated {
        /// The item's identifier.
        id: ContextId,
        /// What it says now.
        meta: Value,
    },
    /// Every item's token count was recomputed, e.g. after the token counter was replaced.
    #[serde(rename = "context.recounted")]
    ContextRecounted {
        /// The projected total before.
        tokens_before: usize,
        /// The projected total after.
        tokens_after: usize,
    },
    /// The [`Provider`] was set or replaced.
    #[serde(rename = "model.changed")]
    ModelChanged {
        /// The model that was in use, if any.
        from: Option<ModelInfo>,
        /// The model that is in use now, if any.
        to: Option<ModelInfo>,
    },
    /// The model parameters were replaced.
    #[serde(rename = "model.params")]
    ModelParamsChanged {
        /// The parameters that will be sent from now on, verbatim.
        params: Params,
    },
    /// A request is being sent. Everything about it is inspectable beforehand with
    /// [`Kernel::preview_request`].
    #[serde(rename = "model.requested")]
    ModelRequested {
        /// The model it is going to.
        model: ModelInfo,
        /// The number of messages in the request.
        messages: usize,
        /// The number of tool definitions in the request.
        tools: usize,
        /// The kernel's estimate of the request's size in tokens.
        tokens: usize,
        /// The context items the request was projected from.
        items: Vec<ContextId>,
        /// The items the projector left out of it, and why.
        ///
        /// note: "Why was that file not in the request?" is the question this whole crate is
        /// for, and a log that recorded only what *was* sent could not answer it afterwards.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skipped: Vec<Skipped>,
        /// The adjustments the projector made to keep the request valid.
        ///
        /// note: Dropping a tool call whose result has been pruned is the kernel changing what
        /// the model is told. It is the right thing to do and it is still an alteration, so it
        /// goes on the record rather than only into a [`Projection`](crate::Projection) that
        /// nobody kept.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        repairs: Vec<String>,
    },
    /// The payload a provider rendered for a request, exactly as it will be sent.
    ///
    /// note: Emitted only when [`Config::record_payloads`] is on, and only by a provider that
    /// implements [`Provider::render`]. It is the provider's own account of what it sends - the
    /// kernel has no wire format to check it against - and it is the body, never the headers.
    #[serde(rename = "model.payload")]
    ModelPayload {
        /// The payload.
        payload: Value,
    },
    /// A fragment of a streamed response arrived.
    ///
    /// note: These are broadcast but, by default, not recorded in the session log; see
    /// [`Config::record_progress`].
    #[serde(rename = "model.delta")]
    ModelDelta {
        /// The fragment.
        delta: Delta,
    },
    /// A response arrived and was recorded in the context.
    #[serde(rename = "model.finished")]
    ModelFinished {
        /// Why the model stopped.
        stop: StopReason,
        /// The token counts the provider reported, if any.
        usage: Option<Usage>,
        /// The tool calls the model requested.
        tool_calls: Vec<ToolCallId>,
        /// The context item the response was recorded as.
        item: ContextId,
    },
    /// The provider failed to answer.
    #[serde(rename = "model.failed")]
    ModelFailed {
        /// What it said.
        error: String,
    },
    /// A [`Kernel::step`] could not get as far as a request, and gave up.
    ///
    /// note: Without this, a step that cannot build a request - an empty projection, a provider
    /// that went away between the claim and the call - would show up on the stream as nothing
    /// but a pair of state changes to [`State::Requesting`](crate::State::Requesting) and back,
    /// with no account of what went wrong. The error is the same one [`Kernel::step`] returns.
    #[serde(rename = "step.failed")]
    StepFailed {
        /// What went wrong.
        error: String,
    },
    /// A tool call needs a decision before it can run.
    #[serde(rename = "permission.requested")]
    PermissionRequested {
        /// The request, including the arguments the model produced.
        request: PermissionRequest,
    },
    /// A tool call was permitted or refused.
    #[serde(rename = "permission.decided")]
    PermissionDecided {
        /// The request being answered.
        id: PermissionId,
        /// The call it concerns.
        call: ToolCallId,
        /// The tool it concerns.
        tool: String,
        /// The answer.
        grant: Grant,
        /// Who gave it.
        source: GrantSource,
    },
    /// The model asked for a tool.
    #[serde(rename = "tool.requested")]
    ToolRequested {
        /// The call's identifier.
        call: ToolCallId,
        /// The tool's identifier.
        tool: String,
        /// The arguments, verbatim.
        ///
        /// note: Shared with the call recorded in the context, rather than a copy of it. The log
        /// should not be a second place the same bytes live.
        args: Arc<Value>,
    },
    /// The model asked for a tool that is not registered.
    #[serde(rename = "tool.unknown")]
    ToolUnknown {
        /// The call's identifier.
        call: ToolCallId,
        /// The name the model used.
        tool: String,
    },
    /// The kernel had to give one of the model's tool calls a usable identifier.
    ///
    /// note: Nothing else in the loop can work if a call cannot be told apart from its
    /// neighbour, so this is a repair rather than a refusal - and it is announced, because a
    /// provider that does this is worth knowing about.
    #[serde(rename = "tool.repaired")]
    ToolCallRepaired {
        /// The identifier the call has now.
        call: ToolCallId,
        /// The one the provider supplied.
        was: String,
        /// What was wrong with it.
        reason: String,
    },
    /// A tool started running.
    #[serde(rename = "tool.started")]
    ToolStarted {
        /// The call's identifier.
        call: ToolCallId,
        /// The tool's identifier.
        tool: String,
    },
    /// A tool reported a fragment of its output while still running.
    ///
    /// note: Like [`Event::ModelDelta`], these are broadcast but, by default, not recorded in
    /// the session log; see [`Config::record_progress`].
    #[serde(rename = "tool.output")]
    ToolOutput {
        /// The call producing it.
        call: ToolCallId,
        /// The tool producing it.
        tool: String,
        /// The fragment.
        chunk: String,
    },
    /// A tool finished, and its output was recorded in the context.
    #[serde(rename = "tool.finished")]
    ToolFinished {
        /// The call's identifier.
        call: ToolCallId,
        /// The tool's identifier.
        tool: String,
        /// Whether the output is an error.
        is_error: bool,
        /// How many bytes the kernel truncated, if it did.
        truncated: Option<usize>,
        /// The estimated size of the recorded output.
        tokens: usize,
        /// The context item the output was recorded as - the one the model is shown.
        item: ContextId,
        /// The archived item holding the whole output, when it had to be shortened.
        ///
        /// note: An output limit decides what the model is shown; it is not permission to throw
        /// the rest away. See [`Config::keep_truncated_output`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        whole: Option<ContextId>,
    },
    /// Context was compacted.
    #[serde(rename = "context.compacted")]
    Compacted {
        /// Exactly what was removed, what was kept, and why.
        report: CompactionReport,
    },
    /// The set of registered tools changed.
    #[serde(rename = "tools.changed")]
    ToolsChanged {
        /// The identifiers of the tools the model will be offered from now on.
        tools: Vec<String>,
    },
    /// The permission policy was replaced.
    #[serde(rename = "policy.changed")]
    PolicyChanged,
}

impl Event {
    /// Returns the event's dotted name, e.g. `model.requested`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session.started",
            Self::SessionResumed { .. } => "session.resumed",
            Self::SessionFinished => "session.finished",
            Self::Interrupted => "turn.interrupted",
            Self::StateChanged { .. } => "state.changed",
            Self::ContextAdded { .. } => "context.added",
            Self::ContextChanged { .. } => "context.changed",
            Self::ContextReplaced { .. } => "context.replaced",
            Self::ContextUndone { .. } => "context.undone",
            Self::ContextRedone { .. } => "context.redone",
            Self::ContextAnnotated { .. } => "context.annotated",
            Self::ContextRecounted { .. } => "context.recounted",
            Self::ModelChanged { .. } => "model.changed",
            Self::ModelParamsChanged { .. } => "model.params",
            Self::ModelRequested { .. } => "model.requested",
            Self::ModelPayload { .. } => "model.payload",
            Self::ModelDelta { .. } => "model.delta",
            Self::ModelFinished { .. } => "model.finished",
            Self::ModelFailed { .. } => "model.failed",
            Self::StepFailed { .. } => "step.failed",
            Self::PermissionRequested { .. } => "permission.requested",
            Self::PermissionDecided { .. } => "permission.decided",
            Self::ToolRequested { .. } => "tool.requested",
            Self::ToolUnknown { .. } => "tool.unknown",
            Self::ToolCallRepaired { .. } => "tool.repaired",
            Self::ToolStarted { .. } => "tool.started",
            Self::ToolOutput { .. } => "tool.output",
            Self::ToolFinished { .. } => "tool.finished",
            Self::Compacted { .. } => "context.compacted",
            Self::ToolsChanged { .. } => "tools.changed",
            Self::PolicyChanged => "policy.changed",
        }
    }
}

/// The channel a [`Provider`] reports streaming fragments through.
///
/// note: A sink handed to a provider by the kernel turns every fragment into an
/// [`Event::ModelDelta`]. [`DeltaSink::disconnected`] produces one that drops them, which is
/// handy when a provider is exercised on its own.
#[derive(Clone)]
pub struct DeltaSink(Option<Kernel>);

impl DeltaSink {
    /// Returns a sink that discards everything.
    pub fn disconnected() -> Self {
        Self(None)
    }

    pub(crate) fn new(kernel: Kernel) -> Self {
        Self(Some(kernel))
    }

    /// Returns whether the fragments are going anywhere.
    pub fn is_connected(&self) -> bool {
        self.0.is_some()
    }

    /// Reports a fragment.
    pub fn send(&self, delta: Delta) {
        if let Some(kernel) = &self.0 {
            kernel.emit(Event::ModelDelta { delta });
        }
    }

    /// Reports a fragment of the response text.
    pub fn text(&self, fragment: impl Into<String>) {
        self.send(Delta::Text(fragment.into()));
    }

    /// Reports a fragment of the model's reasoning.
    pub fn reasoning(&self, fragment: impl Into<String>) {
        self.send(Delta::Reasoning(fragment.into()));
    }

    /// Returns whether somebody has asked the loop to stop.
    ///
    /// note: This is how a request in flight is cancelled. The kernel cannot reach into a
    /// [`Provider`](crate::Provider) and stop it - it does not own the socket, the runtime or
    /// the future - so it offers the fact and the provider decides. A streaming provider that
    /// checks this between fragments can return the text it has, with whatever
    /// [`StopReason`](crate::StopReason) it thinks fits, and the kernel records that turn like
    /// any other: partial, but real, and in the context where it can be seen and pruned.
    ///
    /// note: A provider that ignores it is not broken, only slower to stop - the interrupt is
    /// still honoured before the next request. Dropping the future driving
    /// [`Kernel::step`](crate::Kernel::step) remains the blunt instrument, and costs whatever
    /// had been streamed.
    pub fn is_interrupted(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|kernel| kernel.is_interrupted())
    }

    /// Reports a fragment of a tool call's arguments.
    pub fn tool_args(&self, call: ToolCallId, fragment: impl Into<String>) {
        self.send(Delta::ToolArgs {
            call,
            fragment: fragment.into(),
        });
    }
}

/// The channel a [`Tool`](crate::Tool) reports its progress through while it is still running.
///
/// note: Without this, a tool that takes a minute is invisible until it returns, which would
/// make "observable invocation" a half-truth. A sink knows which call it belongs to, so a tool
/// cannot report output as somebody else.
#[derive(Clone)]
pub struct OutputSink(Option<(Kernel, ToolCallId, String)>);

impl OutputSink {
    /// Returns a sink that discards everything.
    pub fn disconnected() -> Self {
        Self(None)
    }

    pub(crate) fn new(kernel: Kernel, call: ToolCallId, tool: String) -> Self {
        Self(Some((kernel, call, tool)))
    }

    /// Returns whether the fragments are going anywhere.
    pub fn is_connected(&self) -> bool {
        self.0.is_some()
    }

    /// Returns whether somebody has asked the loop to stop.
    ///
    /// note: A tool that can take a while - a shell command, a large read - should check this
    /// between chunks and return what it has. The kernel records whatever comes back, so a tool
    /// that stops early still answers the call it was given, and the model is not left looking at
    /// a call with no result.
    pub fn is_interrupted(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|(kernel, _, _)| kernel.is_interrupted())
    }

    /// Reports a fragment of output.
    pub fn push(&self, chunk: impl Into<String>) {
        if let Some((kernel, call, tool)) = &self.0 {
            kernel.emit(Event::ToolOutput {
                call: call.clone(),
                tool: tool.clone(),
                chunk: chunk.into(),
            });
        }
    }
}
