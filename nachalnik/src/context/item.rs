//! What the context is made of: [`ContextItem`], the [`ContextId`] it is known by, what kind of
//! thing it is and what state it is in.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(doc)]
use super::Context;
use crate::model::{Block, Content, ToolCall, ToolCallId};
#[cfg(doc)]
use crate::{Compactor, Event, Kernel, Projector, TokenCounter};

/// The identifier of a [`ContextItem`].
///
/// note: Identifiers are assigned by the [`Context`] when an item is added, are unique within a
/// session, and are never reused - including by items that were removed. `ContextId(0)` is the
/// unassigned identifier carried by an item that has not been added yet.
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
/// note: This is the one taxonomy the kernel actually uses: the variants carrying data carry
/// exactly what is needed to rebuild a wire message from the item alone - an assistant turn's
/// tool calls, and a tool result's call identity. Without them, a pruned context could not be
/// projected back into a valid request.
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
        /// note: This lives on the turn rather than in an item of its own, because some APIs
        /// require it to be echoed back attached to exactly the turn it came from. Pruning the
        /// turn prunes the reasoning with it, which is the only correct answer for a signed
        /// block.
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

/// What the kernel gives as the reason for the whole of a tool output an output limit shortened -
/// the second result it records for one call, kept out beside the copy the model is shown.
///
/// note: one sentence in one place, because it is also how [`Snapshot::problems`] tells the
/// kernel's own second answer from one something else put there.
///
/// [`Snapshot::problems`]: crate::Snapshot::problems
pub(crate) const WHOLE_OUTPUT: &str = "the whole of a tool output an output limit shortened";

/// Whether, and how, an item takes part in the next request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
/// note: one state for each thing the projector does with an item, and no more. There were two
/// more words for [`ContextState::Excluded`] - `archived` for the whole of a shortened output and
/// `superseded` for an item [`Kernel::supersede`] replaced - which nothing branched on: they were
/// equally absent from the request, took a call down with them alike, and came back the same way.
/// A word that names no behaviour of its own is a second thing to learn, so why an item is out is
/// its note, which says it in a sentence, and the state says only that it is. Both words still read
/// back as `excluded`, so a snapshot or a log written with them loads.
///
/// note: [`ContextState::Elided`] is the one distinction here the projector makes beyond in and
/// out; its note says how.
pub enum ContextState {
    /// Included in the projection.
    Active,
    /// Not included; the item's note says who took it out and why.
    ///
    /// note: whoever that was - a person, the model, a [`Compactor`], or the kernel itself, which
    /// keeps the whole of a shortened tool output out beside the copy the model is shown, and
    /// takes out an item [`Kernel::supersede`] replaced. It is restored the same way whoever did
    /// it.
    #[serde(alias = "archived", alias = "superseded")]
    Excluded,
    /// Included in the projection, and protected: the kernel refuses to let a [`Compactor`]
    /// remove it.
    Pinned,
    /// Included in the projection, but as a short marker instead of its content.
    ///
    /// note: This is the state for "it happened, and you cannot see it any more", which is a
    /// different thing from [`ContextState::Excluded`]. An excluded tool result takes its call
    /// down with it - the projector has to, or the request is malformed - so the conversation the
    /// model reads is one in which the call never happened. An elided one still answers its call,
    /// so the shape of the turn survives and only the content is gone. That is the honest account
    /// of a compaction pass, and it is why a [`Compactor`] should prefer it.
    ///
    /// note: The words belong to whoever elided it: the marker is the item's `note`, and the
    /// projector supplies only the brackets around it. The content itself is untouched, so
    /// restoring is [`Kernel::set_state`] back to [`ContextState::Active`] and nothing was copied
    /// or destroyed to get here.
    ///
    /// note: for an assistant turn, the thinking goes with the words - see
    /// [`LinearProjector::send_reasoning`](crate::LinearProjector::send_reasoning). What stays is
    /// the shape: the turn is still there, and its calls still answer their results.
    Elided,
}

impl ContextState {
    /// Returns whether an item in this state takes part in the projection.
    ///
    /// note: [`ContextState::Active`], [`ContextState::Pinned`] and [`ContextState::Elided`] do;
    /// [`ContextState::Excluded`] does not.
    ///
    /// note: an elided item takes part as a marker rather than as its content, so this being
    /// true does not mean the model reads what the item says. [`ContextState::is_elided`] is the
    /// question "how much of it?", and a client showing a context wants both.
    pub fn is_projected(self) -> bool {
        matches!(self, Self::Active | Self::Pinned | Self::Elided)
    }

    /// Returns whether an item in this state is projected as a marker rather than as its content.
    pub fn is_elided(self) -> bool {
        matches!(self, Self::Elided)
    }

    /// Returns whether an item in this state sends the model what it actually says.
    ///
    /// note: the distinction [`ContextState::is_projected`] cannot draw on its own, and the one
    /// the token figures are built on: an elided item is in the request and is not costing what
    /// it holds, so it belongs on the withheld side of the ledger rather than the spent side.
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
/// note: Every field is public. An item is data, not an object with a hidden life of its own;
/// the only things the [`Context`] insists on owning are the `id` and the two counts, `tokens`
/// and `uncounted`, which it keeps in sync with the content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    /// The item's identifier, assigned when it is added to a [`Context`].
    pub id: ContextId,
    /// What the item is.
    pub kind: ContextKind,
    /// Where it came from.
    ///
    /// note: A free-form name, because the kernel never branches on it - it only reports it.
    /// The constructors use `user`, `system`, `instruction`, `file`, `selection`, `diagnostic`,
    /// `memory`, `tool_result`, `model` and `compaction`; an extension should use its own name,
    /// so that "who injected these 12,000 tokens?" has an answer.
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
    /// note: The kernel never reads this. It exists so that a [`Compactor`] or a [`Projector`]
    /// can be given hints - how expendable an item is, which buffer it came from, when it was
    /// last seen - without the kernel having to invent a vocabulary for them.
    pub meta: Value,
    /// Why the item is in the context at all.
    ///
    /// note: this and `note` below are the two halves of "why is this here", and which one a fact
    /// belongs in is decided by whether it outlives a state change. A shortened tool result is
    /// *always* a shortened tool result, whatever state anybody moves it to, so which item holds
    /// the whole of it is recorded here. Kept in the note, it would go the first time somebody
    /// changed the item's state, and changing it back and forth is what a person does while
    /// trying to understand the pair.
    ///
    /// note: `serde(default)`, so a snapshot written before this existed still resumes.
    #[serde(default)]
    pub included_because: Option<String>,
    /// Why the item is in its current state; set whenever the state changes.
    ///
    /// note: *replaced* whenever the state is set, including with `None`, on purpose: a reason for
    /// being excluded stops being true the moment something is put back, and a stale one would be
    /// worse than none. So nothing that has to survive a state change may be kept in here; put
    /// that in `included_because`.
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

    /// Returns the model's reasoning, if this is an assistant turn that carries it in the
    /// conventional slot.
    ///
    /// note: [`ContextItem::thinking`] is the one to reach for. A turn recorded as ordered blocks
    /// keeps its thinking in its content, where this cannot see it, and can carry more than one
    /// piece of it besides - so this answers `None` for a turn that is visibly full of reasoning.
    /// It is kept because a conventional turn has exactly one and the [`Option`] is what a caller
    /// wants for that.
    pub fn reasoning(&self) -> Option<&Content> {
        match &self.kind {
            ContextKind::AssistantMessage { reasoning, .. } => reasoning.as_ref(),
            _ => None,
        }
    }

    /// Returns the model's thinking, wherever it is recorded, in the order it was produced.
    ///
    /// note: the counterpart of [`ContextItem::calls`], and it exists for the same reason: an
    /// ordered turn keeps its thinking in [`Block::Reasoning`]s inside its content, and a client
    /// that only knew about the conventional slot would show a reasoning model as having done no
    /// reasoning at all.
    ///
    /// note: the content of each, not the [`Part`](crate::Part), because the conventional slot is
    /// a [`Content`] and there is nothing to borrow a part from. Whatever a provider attached to a
    /// thinking block is reachable through the blocks themselves, and belongs to the provider
    /// rather than to a client showing somebody what the model thought.
    pub fn thinking(&self) -> impl Iterator<Item = &Content> {
        let ordered = match &self.kind {
            ContextKind::AssistantMessage { .. } => self.content.as_blocks(),
            _ => None,
        };
        let flat = match (&self.kind, ordered) {
            (ContextKind::AssistantMessage { reasoning, .. }, None) => reasoning.as_ref(),
            _ => None,
        };

        flat.into_iter().chain(
            ordered
                .into_iter()
                .flatten()
                .filter_map(Block::thought)
                .map(|part| &part.content),
        )
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
    /// note: an assistant turn records its calls *either* in
    /// [`ContextKind::AssistantMessage::tool_calls`] *or*, when the order they came in is part of
    /// the turn, as [`Block::Call`]s inside a [`Content::Blocks`] - never both, so that there is
    /// never a second account of what the model asked for. This reads whichever is in use, and it
    /// is what a [`Projector`] pairing calls with results should be reading; matching on the kind
    /// alone would silently find no calls at all in an ordered turn.
    pub fn calls(&self) -> impl Iterator<Item = &ToolCall> {
        let ordered = match &self.kind {
            ContextKind::AssistantMessage { .. } => self.content.as_blocks(),
            _ => None,
        };
        let flat = match (&self.kind, ordered) {
            (ContextKind::AssistantMessage { tool_calls, .. }, None) => Some(&tool_calls[..]),
            _ => None,
        };

        flat.into_iter()
            .flatten()
            .chain(ordered.into_iter().flatten().filter_map(Block::call))
    }
}
