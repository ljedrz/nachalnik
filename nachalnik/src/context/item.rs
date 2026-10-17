//! What the context is made of: [`ContextItem`], the [`ContextId`] it is known by, what kind of
//! thing it is and what state it is in.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(doc)]
use super::Context;
use crate::model::{Content, ToolCall, ToolCallId};
#[cfg(doc)]
use crate::{Compactor, Event, Kernel, Projector, TokenCounter};

/// The identifier of a [`ContextItem`].
///
/// note: Identifiers are assigned by the [`Context`] when an item is added, are unique within a
/// session, and are never reused, including by removed items. `ContextId(0)` is the unassigned
/// identifier for an item not yet added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContextId(pub u64);

impl ContextId {
    /// The identifier of an item that has not been added to a [`Context`] yet.
    pub const UNASSIGNED: Self = Self(0);
}

impl fmt::Display for ContextId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What a [`ContextItem`] is, in terms of the model protocol.
///
/// note: This is the only taxonomy the kernel uses. The variants carrying data carry exactly what
/// is needed to rebuild a wire message from the item alone: an assistant turn's tool calls, and a
/// tool result's call identity. Without them, a context with excluded items could not be projected
/// back into a valid request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContextKind {
    /// Instructions to the model.
    System,
    /// Input from the user.
    UserMessage,
    /// A turn produced by the model.
    AssistantMessage {
        /// The tool calls the model requested in this turn.
        tool_calls: Vec<ToolCall>,
        /// The model's own reasoning, where the provider exposed it.
        ///
        /// note: It lives on the turn, not in its own item, because some APIs require it echoed
        /// back on exactly the turn it came from. Excluding a turn excludes its reasoning; that is
        /// the only correct answer for a signed block.
        #[serde(default)]
        reasoning: Option<Content>,
    },
    /// The result of a tool call.
    ToolResult {
        /// The call this result answers.
        call: ToolCallId,
        /// The tool that produced it.
        tool: String,
        /// Whether the tool reported a failure.
        is_error: bool,
    },
    /// Material the model is given to work with: a file, a selection, a diagnostic, a memory.
    Reference,
}

impl ContextKind {
    /// Returns the name of the kind, as used in events and reports.
    pub fn name(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::UserMessage => "user_message",
            Self::AssistantMessage { .. } => "assistant_message",
            Self::ToolResult { .. } => "tool_result",
            Self::Reference => "reference",
        }
    }
}

/// The reason the kernel gives for the whole of a tool output that an output limit shortened: the
/// second result it records for one call, kept excluded beside the copy the model is shown.
///
/// note: Defined once because [`Snapshot::problems`] uses it to tell the kernel's own second answer
/// from something else put there.
///
/// [`Snapshot::problems`]: crate::Snapshot::problems
pub(crate) const WHOLE_OUTPUT: &str = "the whole of a tool output an output limit shortened";

/// Whether, and how, an item takes part in the next request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
/// note: one state for each thing the projector does with an item, and no more. `archived` (the
/// whole of a shortened output) and `superseded` (an item [`Kernel::supersede`] replaced) used to
/// be two more words for [`ContextState::Excluded`], and nothing branched on them. A word that
/// names no behaviour is a second thing to learn, so the note says why an item is out and the state
/// says only that it is. Both words still read back as `excluded`, so old snapshots and logs load.
///
/// note: [`ContextState::Elided`] is the one distinction the projector makes beyond in and out; its
/// note says how.
pub enum ContextState {
    /// Included in the projection.
    Active,
    /// Not included; the item's note says who took it out and why.
    ///
    /// note: A person, the model, a [`Compactor`], or the kernel itself took it out. The kernel
    /// keeps a shortened tool output out beside the copy the model is shown, and takes out an item
    /// [`Kernel::supersede`] replaced. It is restored the same way whoever did it.
    #[serde(alias = "archived", alias = "superseded")]
    Excluded,
    /// Included in the projection, and protected: the kernel refuses to let a [`Compactor`]
    /// remove it.
    Pinned,
    /// Included in the projection, but as a short marker instead of its content.
    ///
    /// note: This state means it happened and the content is gone; it is different from
    /// [`ContextState::Excluded`]. An excluded tool result takes its call down with it; the
    /// projector must, or the request is malformed. The model reads a conversation in which the
    /// call never happened. An elided item still answers its call, so the turn's shape survives and
    /// only the content is gone. A [`Compactor`] should prefer it.
    ///
    /// note: The marker is the item's `note`; the projector supplies only the brackets. The content
    /// is untouched, so restoring is [`Kernel::set_state`] to [`ContextState::Active`]; nothing was
    /// copied or destroyed.
    ///
    /// note: For an assistant turn, the reasoning goes with the words: see
    /// [`LinearProjector::send_reasoning`](crate::LinearProjector::send_reasoning). The turn stays,
    /// and its calls still answer their results.
    Elided,
}

impl ContextState {
    /// Returns whether an item in this state takes part in the projection.
    ///
    /// note: [`ContextState::Active`], [`ContextState::Pinned`] and [`ContextState::Elided`] do;
    /// [`ContextState::Excluded`] does not.
    ///
    /// note: An elided item takes part as a marker, so `true` does not mean the model reads what
    /// the item says. Use [`ContextState::is_elided`] to ask how much of it; a client showing a
    /// context wants both.
    pub fn is_projected(self) -> bool {
        matches!(self, Self::Active | Self::Pinned | Self::Elided)
    }

    /// Returns whether an item in this state is projected as a marker rather than as its content.
    pub fn is_elided(self) -> bool {
        matches!(self, Self::Elided)
    }

    /// Returns whether an item in this state sends the model what it actually says.
    ///
    /// note: [`ContextState::is_projected`] cannot draw this distinction, and the token figures
    /// are built on it. An elided item is in the request but does not cost what it holds, so it
    /// counts as withheld rather than spent.
    pub fn sends_content(self) -> bool {
        self.is_projected() && !self.is_elided()
    }
}

impl fmt::Display for ContextState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Active => "active",
            Self::Excluded => "excluded",
            Self::Pinned => "pinned",
            Self::Elided => "elided",
        };
        f.write_str(s)
    }
}

/// A single, identifiable piece of context.
///
/// note: Every field is public. The [`Context`] owns only `id`, `tokens` and `uncounted`, and keeps
/// them in sync with the content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    /// The item's identifier, assigned when it is added to a [`Context`].
    pub id: ContextId,
    /// What the item is.
    pub kind: ContextKind,
    /// Where it came from.
    ///
    /// note: It is free-form because the kernel never branches on it; it only reports it.
    /// Constructors use `user`, `system`, `instruction`, `file`, `selection`, `diagnostic`,
    /// `memory`, `tool_result`, `model` and `compaction`. An extension should use its own name, so
    /// the source of injected tokens can be identified.
    pub source: String,
    /// A short, human-facing name: a path, a command, a description.
    pub label: String,
    /// The content itself.
    pub content: Content,
    /// The estimated size of the item as it would be sent, as counted by the active
    /// [`TokenCounter`].
    pub tokens: usize,
    /// How many pieces of the item's content the active [`TokenCounter`] would not put a number
    /// on.
    ///
    /// note: the per-item half of [`Budget::uncounted`](crate::Budget::uncounted), and it is
    /// here rather than only on the budget so that a client showing a context can say *which*
    /// row is the one nobody priced. A pane listing an item at `0 tokens` next to one at `12,000`
    /// invites exactly the wrong conclusion about which of the two to get rid of.
    ///
    /// note: set by the counter, whenever `tokens` is, and by the same rules - it does not
    /// change by itself when the counter changes, and [`Kernel::recount`](crate::Kernel::recount)
    /// is what brings both into line.
    ///
    /// note: `serde(default)`, so a snapshot written before this existed still resumes, reading
    /// `0` - which is what a counter that could not say so was reporting.
    #[serde(default)]
    pub uncounted: usize,
    /// Whether the item takes part in the next request.
    pub state: ContextState,
    /// Anything the user, a client or an extension wants to attach.
    ///
    /// note: the kernel never reads this. It lets a [`Compactor`] or a [`Projector`] receive hints,
    /// so the kernel does not need a vocabulary for them.
    pub meta: Value,
    /// Why the item is in the context at all.
    ///
    /// note: this and `note` below are the two halves of "why is this here". A fact belongs here if
    /// it outlives a state change. A shortened tool result is always a shortened tool result,
    /// whatever state the item moves to, so which item holds the whole of it is recorded here. In
    /// `note` it would be lost the first time the state changed, and someone trying to understand
    /// the pair may change the state back and forth.
    ///
    /// note: `serde(default)`, so a snapshot written before this existed still resumes.
    #[serde(default)]
    pub included_because: Option<String>,
    /// Why the item is in its current state; set whenever the state changes.
    ///
    /// note: it is replaced whenever the state is set, including with `None`, on purpose. A reason
    /// for being excluded stops being true as soon as the item is put back, and a stale reason is
    /// worse than none. Nothing that must survive a state change may be kept here; put that in
    /// `included_because`.
    pub note: Option<String>,
}

impl ContextItem {
    /// Creates an item in the [`ContextState::Active`] state, with no identifier yet.
    pub fn new(
        kind: ContextKind,
        source: impl Into<String>,
        label: impl Into<String>,
        content: impl Into<Content>,
    ) -> Self {
        Self {
            id: ContextId::UNASSIGNED,
            kind,
            source: source.into(),
            label: label.into(),
            content: content.into(),
            tokens: 0,
            uncounted: 0,
            state: ContextState::Active,
            meta: Value::Null,
            included_because: None,
            note: None,
        }
    }

    /// Creates a system instruction, attributed to the harness itself.
    pub fn system(content: impl Into<Content>) -> Self {
        Self::new(ContextKind::System, "system", "system", content)
    }

    /// Creates a system instruction that came from a file or a preamble.
    pub fn instruction(label: impl Into<String>, content: impl Into<Content>) -> Self {
        Self::new(ContextKind::System, "instruction", label, content)
    }

    /// Creates a message from the user.
    pub fn user(content: impl Into<Content>) -> Self {
        Self::new(ContextKind::UserMessage, "user", "user", content)
    }

    /// Creates a turn produced by the model.
    pub fn assistant(content: impl Into<Content>, tool_calls: Vec<ToolCall>) -> Self {
        Self::new(
            ContextKind::AssistantMessage {
                tool_calls,
                reasoning: None,
            },
            "model",
            "assistant",
            content,
        )
    }

    /// Creates the result of a tool call.
    pub fn tool_result(
        call: ToolCallId,
        tool: impl Into<String>,
        content: impl Into<Content>,
        is_error: bool,
    ) -> Self {
        let tool = tool.into();
        Self::new(
            ContextKind::ToolResult {
                call,
                tool: tool.clone(),
                is_error,
            },
            "tool_result",
            tool,
            content,
        )
    }

    /// Creates a file reference; the label is the path.
    pub fn file(path: impl Into<String>, content: impl Into<Content>) -> Self {
        Self::new(ContextKind::Reference, "file", path, content)
    }

    /// Creates a reference to a selection in an editor.
    pub fn selection(label: impl Into<String>, content: impl Into<Content>) -> Self {
        Self::new(ContextKind::Reference, "selection", label, content)
    }

    /// Creates a diagnostic reference.
    pub fn diagnostic(label: impl Into<String>, content: impl Into<Content>) -> Self {
        Self::new(ContextKind::Reference, "diagnostic", label, content)
    }

    /// Creates a recalled memory.
    pub fn memory(label: impl Into<String>, content: impl Into<Content>) -> Self {
        Self::new(ContextKind::Reference, "memory", label, content)
    }

    /// Creates a summary produced by a [`Compactor`].
    pub fn summary(content: impl Into<Content>) -> Self {
        Self::new(ContextKind::Reference, "compaction", "summary", content)
    }

    /// Marks the item as [`ContextState::Pinned`].
    pub fn pinned(mut self) -> Self {
        self.state = ContextState::Pinned;
        self
    }

    /// Attaches the model's reasoning to an assistant turn.
    ///
    /// note: Only [`ContextKind::AssistantMessage`] carries reasoning; an item of any other kind
    /// is returned unchanged, since there is no turn for it to belong to.
    pub fn with_reasoning(mut self, reasoning: Option<Content>) -> Self {
        if let ContextKind::AssistantMessage {
            reasoning: slot, ..
        } = &mut self.kind
        {
            *slot = reasoning;
        }

        self
    }

    /// Returns the model's reasoning if this is an assistant turn that carries it in the
    /// conventional slot.
    ///
    /// note: use [`ContextItem::thinking`] instead. A turn recorded as ordered blocks keeps its
    /// thinking in its content, where this cannot see it. It can also carry more than one piece, so
    /// this returns `None` for a turn that is visibly full of reasoning. It is kept because a
    /// conventional turn has exactly one, and an [`Option`] is what a caller wants for that.
    pub fn reasoning(&self) -> Option<&Content> {
        match &self.kind {
            ContextKind::AssistantMessage { reasoning, .. } => reasoning.as_ref(),
            _ => None,
        }
    }

    /// Returns the model's thinking, wherever it is recorded, in the order it was produced.
    ///
    /// note: this is the counterpart of [`ContextItem::calls`] and exists for the same reason. An
    /// ordered turn keeps its thinking in [`Block::Reasoning`](crate::Block::Reasoning) blocks
    /// inside its content, so a client that only knew the conventional slot would show a reasoning
    /// model as having done no reasoning.
    ///
    /// note: it yields the content of each thinking block, not a [`Part`](crate::Part), because the
    /// conventional slot is a [`Content`] and there is no part to borrow. Anything a provider
    /// attached to a thinking block is reachable through the blocks themselves and belongs to the
    /// provider, not to a client showing what the model thought.
    pub fn thinking(&self) -> impl Iterator<Item = &Content> {
        let (ordered, flat) = match &self.kind {
            ContextKind::AssistantMessage { reasoning, .. } => {
                (self.content.as_blocks(), reasoning.as_ref())
            }
            _ => (None, None),
        };

        crate::model::thinking_in(ordered, flat)
    }

    /// Attaches metadata for a [`Compactor`] or a [`Projector`].
    pub fn with_meta(mut self, meta: Value) -> Self {
        self.meta = meta;
        self
    }

    /// Records why the item is being added.
    pub fn because(mut self, reason: impl Into<String>) -> Self {
        self.included_because = Some(reason.into());
        self
    }

    /// Returns whether the item takes part in the next request.
    pub fn is_projected(&self) -> bool {
        self.state.is_projected()
    }

    /// Returns the tool calls this turn asked for, wherever they are recorded.
    ///
    /// note: an assistant turn records its calls either in
    /// [`ContextKind::AssistantMessage::tool_calls`] or, when the order they came in is part of the
    /// turn, as [`Block::Call`](crate::Block::Call) blocks inside a [`Content::Blocks`]. The turn
    /// never uses both, so there is never a second account of what the model asked for. This reads
    /// whichever is in use. A [`Projector`] pairing calls with results should read this; matching
    /// on the kind alone would silently find no calls in an ordered turn.
    pub fn calls(&self) -> impl Iterator<Item = &ToolCall> {
        let (ordered, flat) = match &self.kind {
            ContextKind::AssistantMessage { tool_calls, .. } => {
                (self.content.as_blocks(), &tool_calls[..])
            }
            _ => (None, &[][..]),
        };

        crate::model::calls_in(ordered, flat)
    }
}
