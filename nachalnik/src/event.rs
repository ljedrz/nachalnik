//! The kernel's events as they happen, and the two channels that report what is still arriving.
//!
//! note: one enum is the whole observability story. No state change skips it, no logging the user
//! cannot see. Adding a variant promises an announcement; removing one hides it from clients.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(doc)]
use crate::{Config, Provider};
use crate::{
    compaction::CompactionReport,
    context::{ContextId, ContextState},
    kernel::{Kernel, State},
    model::{Content, ModelInfo, Overrun, Params, StopReason, ToolCallId, Usage},
    permissions::{Grant, GrantSource, PermissionId, PermissionRequest, Verdict},
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

/// The kernel's events as they happen.
///
/// note: clients subscribe with [`Kernel::subscribe`]; the same events form the session log, minus
/// streaming fragments unless [`Config::record_progress`] is on.
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
    /// note: one event, not one `context.added` per item. Those describe things happening; this
    /// describes picking up what already happened.
    #[serde(rename = "session.resumed")]
    SessionResumed {
        /// The session's name.
        session: String,
        /// How many items came back.
        items: usize,
        /// The estimated cost of the items that send their content, summed from their own figures.
        ///
        /// note: the items' own figures, not the projected total that
        /// [`Budget::context_tokens`](crate::Budget::context_tokens) carries for
        /// [`Projection::messages`](crate::Projection::messages). They differ by what the projector
        /// adds or removes: a reference gains its label, and an elided item is a marker in the
        /// projection and nothing in this sum.
        tokens: usize,
    },
    /// A session was declared finished by its owner.
    #[serde(rename = "session.finished")]
    SessionFinished,
    /// Somebody asked the loop to stop at the next opportunity.
    #[serde(rename = "turn.interrupted")]
    Interrupted,
    /// [`Kernel::turn`] stopped at [`Kernel::max_requests_per_turn`] before the model ended its
    /// turn, and the next `turn` carries on from here.
    ///
    /// note: the machine rests in [`State::Idle`] either way. Without this, a turn that ran out
    /// of requests is indistinguishable from one the model ended.
    #[serde(rename = "turn.paused")]
    TurnPaused {
        /// The requests the turn had made, which is the budget it reached.
        requests: usize,
    },
    /// The session was finished with calls the turn had asked for still waiting to run.
    ///
    /// note: what a stopped program most often leaves: a question nobody answered, or permitted
    /// calls not yet run, when [`Kernel::finish`] was called. Their `tool.requested` has no
    /// `tool.finished`, and this says that is how the session ended, not that a record was lost.
    #[serde(rename = "turn.unfinished")]
    TurnUnfinished {
        /// The calls, in the order the model asked for them.
        calls: Vec<ToolCallId>,
    },
    /// The runtime moved from one state to another.
    ///
    /// note: every transition is announced, so this and the context events are enough to render
    /// what the agent is doing without inferring anything.
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
        /// The metadata it was added with.
        ///
        /// note: copied rather than named, as [`Event::ContextReplaced`] copies what it overwrote:
        /// a [`Compactor`](crate::Compactor) decides by these hints, and an item's first metadata
        /// would otherwise survive only in a snapshot taken before the next
        /// [`Event::ContextAnnotated`]. `serde(default)`, so a log written before this reads as an
        /// item added with none.
        #[serde(default)]
        meta: Value,
    },
    /// A context item's state or note changed, e.g. it was excluded from the projection or pinned.
    ///
    /// note: a new note on an unchanged state is a change; `from` and `to` can be equal.
    #[serde(rename = "context.changed")]
    ContextChanged {
        /// The item's identifier.
        id: ContextId,
        /// The state it was in.
        from: ContextState,
        /// The state it is in now.
        to: ContextState,
        /// Why - the item's note from now on.
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
        /// note: the only event that carries content unasked; [`Config::record_payloads`] and
        /// [`Config::record_progress`] ask for more. A [`Event::ContextAdded`] needs none, because
        /// the item is still in the context and in any [`Snapshot`](crate::Snapshot). A replacement
        /// is the only operation that overwrites content, and without this the old text would be
        /// gone once it left the undo window.
        ///
        /// note: this is also what keeps [`Event::ModelRequested`] accurate: it names the items a
        /// request was built from rather than copying them, which only works if what it names
        /// cannot change unrecorded. With this, a snapshot and the log can be wound back to
        /// reconstruct exactly what any past request contained.
        was: Content,
    },
    /// A context change was undone.
    ///
    /// note: An undo showing only remaining count would be the one context change a client
    /// cannot render and the user most likely wants to check.
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
    /// note: the kernel never reads [`ContextItem::meta`](crate::ContextItem::meta), but a
    /// [`Compactor`](crate::Compactor) does. A hint deciding what gets dropped must not change
    /// unnoticed.
    #[serde(rename = "context.annotated")]
    ContextAnnotated {
        /// The item's identifier.
        id: ContextId,
        /// What it says now.
        meta: Value,
        /// What it said before, which nothing else keeps; see [`Event::ContextAdded`]'s `meta`.
        #[serde(default)]
        was: Value,
    },
    /// Every item's token count was recomputed, e.g. after the token counter was replaced.
    ///
    /// note: the two figures are summed over the items that send their content - the items' own
    /// numbers are the ones that moved. They are not the projected total; see
    /// [`Event::SessionResumed`] for the difference.
    #[serde(rename = "context.recounted")]
    ContextRecounted {
        /// What the items sending their content cost before.
        tokens_before: usize,
        /// And after.
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
        /// note: so the log can answer, afterwards, why something was not in the request.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skipped: Vec<Skipped>,
        /// What the projector had to take out to keep the request valid.
        ///
        /// note: dropping a tool call whose result was excluded changes what the model is told, so
        /// it goes on the record, not only into a [`Projection`](crate::Projection) nobody kept.
        ///
        /// note: [`Projection::reordered`](crate::Projection::reordered) is not here: a move takes
        /// nothing out, and a client shows this field to somebody who may have to act on it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        repairs: Vec<String>,
    },
    /// The payload a provider rendered for a request, exactly as it will be sent.
    ///
    /// note: Emitted only when [`Config::record_payloads`] is on and only by a provider that
    /// implements [`Provider::render`]. It is the provider's account of what it sends; the kernel
    /// has no wire format to check it against. It is the body, never the headers.
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
        /// How long the request was, where the failure was the model refusing to read it.
        ///
        /// note: the numbers and the sentence because this is the one failure a session can act on
        /// rather than only report, and the one measurement of a request in the units its limit is
        /// enforced in. The counter is told before this is emitted unless something in the request
        /// went unpriced, so a client redrawing on this event draws the corrected figure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overrun: Option<Overrun>,
    },
    /// A [`Kernel::step`] could not get as far as a request, and gave up.
    ///
    /// note: without this, a step that cannot build a request (an empty projection, or a provider
    /// removed between the claim and the call) shows on the stream only as a move to
    /// [`State::Requesting`](crate::State::Requesting) and back. The error is the one
    /// [`Kernel::step`] returns.
    #[serde(rename = "step.failed")]
    StepFailed {
        /// How long the request was, where the step gave up because it is longer than the model
        /// will read.
        ///
        /// note: the same field as [`Event::ModelFailed`]'s, because it is the same fact one step
        /// earlier, from the kernel's arithmetic instead of the endpoint; a client reports both the
        /// same way.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overrun: Option<Overrun>,
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
    /// The policy was told something that changes what it answers; see [`Kernel::record_rule`].
    ///
    /// note: the kernel holds no rules and cannot see a policy's, so this is what the policy's
    /// owner says changed. Without it, a call allowed by policy did not say why, and a rule made by
    /// answering "always" was in no record.
    #[serde(rename = "policy.ruled")]
    PolicyRuled {
        /// What the rule is about, in the policy's own words.
        subject: String,
        /// What the policy answers about it from here on.
        verdict: Verdict,
        /// The question whose answer made the rule, where one did.
        answering: Option<PermissionId>,
        /// Whether it holds for that question's call alone rather than from here on.
        once: bool,
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
        /// note: shared with the call recorded in the context rather than copied, so the log is not
        /// a second place the same bytes live.
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
    /// The model's arguments for a call were not a JSON object, so the kernel answered it without
    /// asking about it or running it; see [`ToolCall::unparsed`](crate::ToolCall::unparsed).
    ///
    /// note: what the model wrote is here as well as on the call, because [`Event::ToolRequested`]
    /// carries a call's arguments and this call has none. A log read without its snapshot would not
    /// otherwise say what the model wrote.
    #[serde(rename = "tool.unparsed")]
    ToolUnparsed {
        /// The call's identifier.
        call: ToolCallId,
        /// The tool's identifier.
        tool: String,
        /// What the model wrote for the arguments.
        written: String,
        /// What the parser said was wrong, with where.
        error: String,
    },
    /// The kernel had to give one of the model's tool calls a usable identifier.
    ///
    /// note: nothing else in the loop works if calls cannot be told apart, so this is a repair
    /// rather than a refusal. It is announced because a provider that needs it is worth knowing
    /// about.
    #[serde(rename = "tool.repaired")]
    ToolCallRepaired {
        /// The identifier the call has now.
        call: ToolCallId,
        /// The one the provider supplied.
        was: String,
        /// What was wrong with it.
        reason: String,
    },
    /// Tool call identifiers were declared spoken for without the kernel having issued them; see
    /// [`Kernel::reserve_calls`].
    ///
    /// note: the count rather than the identifiers: a later `tool.repaired` says a provider reused
    /// an identifier from earlier in the session, and this line says where that earlier use came
    /// from.
    #[serde(rename = "tool.reserved")]
    ToolCallsReserved {
        /// How many of them the kernel had not already been told about.
        reserved: usize,
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
    /// A tool panicked while running a call, and the call is answered as a failure.
    ///
    /// note: answered like any other failure, with an error result the model is shown and the
    /// [`Event::ToolFinished`] after it, rather than unwinding through
    /// [`Kernel::step`](crate::Kernel::step). This is the only thing that says it was a panic,
    /// since `is_error` cannot. What the panic said is in the result, not here, because the log
    /// names things rather than copying them.
    ///
    /// note: the process's panic hook still runs, by default writing to standard error, and a build
    /// with `panic = "abort"` ends before any of this happens.
    #[serde(rename = "tool.panicked")]
    ToolPanicked {
        /// The call's identifier.
        call: ToolCallId,
        /// The tool's identifier.
        tool: String,
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
        /// The excluded item holding the whole output, when it had to be shortened.
        ///
        /// note: an output limit decides what the model is shown; it is not permission to throw the
        /// rest away. See [`Config::keep_truncated_output`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        whole: Option<ContextId>,
    },
    /// Context was compacted.
    #[serde(rename = "context.compacted")]
    Compacted {
        /// Exactly what was removed, what was kept, and why.
        report: CompactionReport,
    },
    /// The compactor still wants room and can make no more, or it has stopped wanting room.
    ///
    /// note: measured by the kernel, not claimed by the compactor: after its pass the compactor
    /// still [wants room](crate::Compactor::wants_room). What is left is for the person, or the
    /// model through its tools, to take, and this is when to tell them.
    ///
    /// note: emitted on the change only, so a context that stays full is reported once, and one
    /// that drops back under says so. A session starts not full.
    #[serde(rename = "context.full")]
    ContextFull {
        /// Whether it is full from here on.
        full: bool,
        /// What the next request would cost, as [`Budget::used`](crate::Budget::used) counts it.
        used: usize,
        /// The limit it is measured against, if there is one.
        limit: Option<usize>,
    },
    /// The set of registered tools changed.
    #[serde(rename = "tools.changed")]
    ToolsChanged {
        /// The identifiers of the tools the model will be offered from now on.
        tools: Vec<String>,
    },
    /// The [`PermissionPolicy`](crate::PermissionPolicy) was replaced.
    ///
    /// note: the four seam-changed events name the old and the new seam by their `name()`s, so a
    /// reader can say what was deciding, projecting, counting or compacting for the requests on
    /// either side.
    #[serde(rename = "policy.changed")]
    PolicyChanged {
        /// What was deciding before.
        from: String,
        /// What decides now.
        to: String,
    },
    /// The [`Projector`](crate::Projector) was replaced.
    #[serde(rename = "projector.changed")]
    ProjectorChanged {
        /// What was turning the context into a request before.
        from: String,
        /// What turns it into one now.
        to: String,
    },
    /// The [`TokenCounter`](crate::TokenCounter) was replaced.
    ///
    /// note: followed by a [`Event::ContextRecounted`], because every figure the old one produced
    /// is now wrong and the kernel says so rather than leaving them to be read.
    #[serde(rename = "counter.changed")]
    CounterChanged {
        /// What was estimating before.
        from: String,
        /// What estimates now.
        to: String,
    },
    /// The [`Compactor`](crate::Compactor) was set, replaced or removed.
    #[serde(rename = "compactor.changed")]
    CompactorChanged {
        /// What was making room before, if anything.
        from: Option<String>,
        /// What makes room now; `None` means nothing will ever be dropped to make it.
        to: Option<String>,
    },
    /// An event this version does not know, read from a log written by a later version.
    ///
    /// note: never emitted. A reader gets it instead of an error, so one new event in a log from a
    /// newer runtime costs that record, not the whole log. Its name and contents are not kept; the
    /// line it came from has both. A record that names an event this version knows but does not fit
    /// it is still an error: that log is wrong, not newer.
    #[serde(rename = "unknown", other)]
    Unknown,
}

impl Event {
    /// Whether this is a fragment of something still arriving rather than a thing that happened:
    /// [`Event::ModelDelta`] and [`Event::ToolOutput`], which the log keeps only under
    /// [`Config::record_progress`](crate::Config::record_progress).
    pub fn is_progress(&self) -> bool {
        matches!(self, Self::ModelDelta { .. } | Self::ToolOutput { .. })
    }

    /// Returns the event's dotted name, e.g. `model.requested`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session.started",
            Self::SessionResumed { .. } => "session.resumed",
            Self::SessionFinished => "session.finished",
            Self::Interrupted => "turn.interrupted",
            Self::TurnPaused { .. } => "turn.paused",
            Self::TurnUnfinished { .. } => "turn.unfinished",
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
            Self::PolicyRuled { .. } => "policy.ruled",
            Self::ToolRequested { .. } => "tool.requested",
            Self::ToolUnknown { .. } => "tool.unknown",
            Self::ToolUnparsed { .. } => "tool.unparsed",
            Self::ToolCallRepaired { .. } => "tool.repaired",
            Self::ToolCallsReserved { .. } => "tool.reserved",
            Self::ToolStarted { .. } => "tool.started",
            Self::ToolOutput { .. } => "tool.output",
            Self::ToolPanicked { .. } => "tool.panicked",
            Self::ToolFinished { .. } => "tool.finished",
            Self::Compacted { .. } => "context.compacted",
            Self::ContextFull { .. } => "context.full",
            Self::ToolsChanged { .. } => "tools.changed",
            Self::PolicyChanged { .. } => "policy.changed",
            Self::ProjectorChanged { .. } => "projector.changed",
            Self::CounterChanged { .. } => "counter.changed",
            Self::CompactorChanged { .. } => "compactor.changed",
            Self::Unknown => "unknown",
        }
    }
}

/// The channel a [`Provider`] reports streaming fragments through.
///
/// note: a sink handed to a provider by the kernel turns every fragment into an
/// [`Event::ModelDelta`]. [`DeltaSink::disconnected`] produces one that drops them, for exercising
/// a provider on its own.
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

    /// Returns whether someone has asked the loop to stop.
    ///
    /// note: this is how a request in flight is cancelled. The kernel does not own the socket, the
    /// runtime or the future, so it exposes the request and the provider decides. A streaming
    /// provider that checks this between fragments can return what it has, with whatever
    /// [`StopReason`](crate::StopReason) fits, and the kernel records that partial turn like any
    /// other, where it can be seen and excluded.
    ///
    /// note: a provider that ignores it is only slower to stop; the interrupt is still honoured
    /// before the next request. Dropping the future driving [`Kernel::step`](crate::Kernel::step)
    /// stops it bluntly and loses whatever had been streamed.
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
/// note: without this, a tool that takes a minute is invisible until it returns. A sink knows which
/// call it belongs to, so a tool cannot report output as another call.
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
    /// note: a tool that can take a while, such as a shell command or a large read, should check
    /// this between chunks and return what it has. The kernel records whatever comes back, so the
    /// call still gets a result.
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
