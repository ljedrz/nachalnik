use std::{collections::VecDeque, fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(doc)]
use crate::{Compactor, Config, Event, Kernel, Projector};
use crate::{
    model::{Block, Content, ToolCall, ToolCallId},
    tokens::TokenCounter,
};

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

/// Whether, and how, an item takes part in the next request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContextState {
    /// Included in the projection.
    Active,
    /// Not included; removed by the user or by a [`Compactor`], and restorable.
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
    Elided,
    /// Not included; kept for the record, and not expected to come back.
    Archived,
    /// Not included; replaced by a newer item.
    ///
    /// note: The kernel sets this only through [`Kernel::supersede`], because deciding that one
    /// item replaces another is a judgement about meaning - two reads of the same file may be
    /// two versions or two separate facts - and the kernel is not the one who knows.
    Superseded,
}

impl ContextState {
    /// Returns whether an item in this state takes part in the projection.
    ///
    /// note: [`ContextState::Active`], [`ContextState::Pinned`] and [`ContextState::Elided`] do;
    /// the rest do not. The kernel attaches no other meaning to the remaining three - they are
    /// there so that *you* can tell why something is out.
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
            Self::Archived => "archived",
            Self::Superseded => "superseded",
        };
        f.write_str(s)
    }
}

/// A single, identifiable piece of context.
///
/// note: Every field is public. An item is data, not an object with a hidden life of its own;
/// the only things the [`Context`] insists on owning are the `id` and the `tokens` count, which
/// it keeps in sync with the content.
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
    /// `tool_result`, `model` and `compaction`; an extension should use its own name, so that
    /// "who injected these 12,000 tokens?" has an answer.
    pub source: String,
    /// A short, human-facing name: a path, a command, a description.
    pub label: String,
    /// The content itself.
    pub content: Content,
    /// The estimated size of the item as it would be sent, as counted by the active
    /// [`TokenCounter`].
    pub tokens: usize,
    /// Whether the item takes part in the next request.
    pub state: ContextState,
    /// Anything the user, a client or an extension wants to attach.
    ///
    /// note: The kernel never reads this. It exists so that a [`Compactor`] or a [`Projector`]
    /// can be given hints - how expendable an item is, which buffer it came from, when it was
    /// last seen - without the kernel having to invent a vocabulary for them.
    pub meta: Value,
    /// Why the item is in the context at all.
    pub included_because: Option<String>,
    /// Why the item is in its current state; set whenever the state changes.
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
    /// note: the content of each, not the [`Part`](crate::Part), because the conventional slot is a [`Content`]
    /// and there is nothing to borrow a part from. Whatever a provider attached to a thinking
    /// block is reachable through the blocks themselves, and belongs to the provider rather than
    /// to a client showing somebody what the model thought.
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

/// The set of context items, in the order they were added.
///
/// note: This is the state the model request is *derived* from, not the request itself. Nothing
/// here is ever silently dropped: removal is a state change, so a removed item can be listed,
/// inspected and restored.
///
/// note: The mutating operations are `pub(crate)` on purpose - they all go through [`Kernel`],
/// which is what turns them into [`Event`]s. Reading is unrestricted.
///
/// note: The items are in insertion order, which - because identifiers are handed out in that
/// order and never reused - is also identifier order. [`Context::item`] relies on it.
#[derive(Debug, Clone)]
pub struct Context {
    items: Vec<Arc<ContextItem>>,
    undo: VecDeque<Vec<Arc<ContextItem>>>,
    redo: Vec<Vec<Arc<ContextItem>>>,
    undo_depth: usize,
    next_id: u64,
}

impl Default for Context {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Context {
    /// Creates an empty context retaining `undo_depth` snapshots for [`Kernel::undo`].
    pub fn new(undo_depth: usize) -> Self {
        Self {
            items: Vec::new(),
            undo: VecDeque::new(),
            redo: Vec::new(),
            undo_depth,
            next_id: 1,
        }
    }

    /// Returns every item, in insertion order, regardless of state.
    ///
    /// note: Items are behind an [`Arc`] so that the context (and its undo snapshots) can be
    /// cloned without copying content; treat them as the immutable records they are.
    pub fn items(&self) -> &[Arc<ContextItem>] {
        &self.items
    }

    /// Returns the item with the given identifier.
    pub fn item(&self, id: ContextId) -> Option<&Arc<ContextItem>> {
        self.index_of(id).map(|index| &self.items[index])
    }

    /// Returns the position of the item with the given identifier.
    ///
    /// note: Identifiers are handed out in insertion order and never reused, so the items are
    /// sorted by identifier and this is a binary search. A compaction plan naming a thousand
    /// items would otherwise be a thousand scans of the whole context.
    fn index_of(&self, id: ContextId) -> Option<usize> {
        self.items.binary_search_by_key(&id, |item| item.id).ok()
    }

    /// Returns the identifier the next item added will be given.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Returns the number of items, regardless of state.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns whether the context is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns the items that take part in the next request.
    pub fn projected(&self) -> impl Iterator<Item = &Arc<ContextItem>> {
        self.items.iter().filter(|i| i.is_projected())
    }

    /// Returns the estimated number of tokens the items sending their content occupy.
    ///
    /// note: an elided item is projected but is not sending what it says, so its own size is not
    /// here - it is in [`Context::tokens_withheld`] with the rest of what the model is not being
    /// shown. What the marker in its place costs is small, and is counted where it is spent: in
    /// [`Budget::context_tokens`](crate::Budget::context_tokens), over the messages that came out
    /// of the projector.
    pub fn tokens(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.state.sends_content())
            .map(|i| i.tokens)
            .sum()
    }

    /// Returns the estimated number of tokens held by items the model is not being shown: the
    /// ones that are not projected, and the ones projected only as a marker.
    pub fn tokens_withheld(&self) -> usize {
        self.items
            .iter()
            .filter(|i| !i.state.sends_content())
            .map(|i| i.tokens)
            .sum()
    }

    /// Returns the number of context snapshots available to [`Kernel::undo`].
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Returns the number of context snapshots available to [`Kernel::redo`].
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Adds an item, assigning it an identifier and counting its tokens.
    pub(crate) fn add(&mut self, mut item: ContextItem, counter: &dyn TokenCounter) -> ContextId {
        let id = ContextId(self.next_id);
        self.next_id += 1;
        item.id = id;
        item.tokens = counter.count_item(&item);
        self.items.push(Arc::new(item));

        id
    }

    /// Returns whether moving an item to this state, with this note, would change anything.
    ///
    /// note: The note counts. "Excluded because it was enormous" and "excluded because the user
    /// said so" are different facts about the same item, and one quietly overwriting the other
    /// is exactly the kind of unannounced edit this crate exists not to make.
    pub(crate) fn would_change(
        &self,
        id: ContextId,
        state: ContextState,
        note: &Option<String>,
    ) -> bool {
        self.item(id)
            .is_some_and(|item| item.state != state || item.note != *note)
    }

    /// Sets an item's state, returning the previous one; `None` if there is no such item, or if
    /// nothing would change.
    pub(crate) fn set_state(
        &mut self,
        id: ContextId,
        state: ContextState,
        note: Option<String>,
    ) -> Option<ContextState> {
        if !self.would_change(id, state, &note) {
            return None;
        }

        let index = self.index_of(id)?;
        let item = Arc::make_mut(&mut self.items[index]);
        let previous = item.state;
        item.state = state;
        item.note = note;

        Some(previous)
    }

    /// Replaces an item's content in place, returning what it said before and the old and new
    /// token counts.
    pub(crate) fn replace(
        &mut self,
        id: ContextId,
        content: Content,
        counter: &dyn TokenCounter,
    ) -> Option<(Content, usize, usize)> {
        let index = self.index_of(id)?;
        let item = Arc::make_mut(&mut self.items[index]);
        let before = item.tokens;
        // a pointer, not a copy - which is what makes recording it affordable
        let was = std::mem::replace(&mut item.content, content);
        item.tokens = counter.count_item(item);

        Some((was, before, item.tokens))
    }

    /// Replaces the contents with the items of a [`Snapshot`](crate::Snapshot), recounting them.
    ///
    /// note: The items are sorted by identifier and the next one is taken past the highest of
    /// them, so that a snapshot somebody edited by hand cannot quietly break the ordering the
    /// lookups depend on, or hand out an identifier that is already in use.
    pub(crate) fn restore(
        &mut self,
        items: Vec<ContextItem>,
        next_id: u64,
        counter: &dyn TokenCounter,
    ) {
        let mut items: Vec<_> = items
            .into_iter()
            .map(|mut item| {
                item.tokens = counter.count_item(&item);
                Arc::new(item)
            })
            .collect();
        items.sort_by_key(|item| item.id);

        let past_the_last = items.last().map(|item| item.id.0 + 1).unwrap_or(1);
        self.next_id = next_id.max(past_the_last);
        self.items = items;
        self.undo.clear();
        self.redo.clear();
    }

    /// Replaces an item's metadata, returning whether it changed.
    pub(crate) fn annotate(&mut self, id: ContextId, meta: Value) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        if self.items[index].meta == meta {
            return false;
        }
        Arc::make_mut(&mut self.items[index]).meta = meta;

        true
    }

    /// Recounts every item's tokens.
    pub(crate) fn recount(&mut self, counter: &dyn TokenCounter) {
        for item in &mut self.items {
            let item = Arc::make_mut(item);
            item.tokens = counter.count_item(item);
        }
    }

    /// Restores the previous state of the context, returning whether anything was restored.
    pub(crate) fn undo(&mut self) -> bool {
        match self.undo.pop_back() {
            Some(items) => {
                let undone = std::mem::replace(&mut self.items, items);
                self.redo.push(undone);
                if self.redo.len() > self.undo_depth {
                    self.redo.remove(0);
                }
                true
            }
            None => false,
        }
    }

    /// Puts back what the last [`Context::undo`] took away, returning whether there was any.
    ///
    /// note: A stack, not a toggle: undoing three operations and redoing them puts all three
    /// back, in order. Doing anything new makes the redone future unreachable, which is why
    /// [`Context::checkpoint`] discards it.
    pub(crate) fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(items) => {
                let current = std::mem::replace(&mut self.items, items);
                if self.undo_depth != 0 {
                    if self.undo.len() == self.undo_depth {
                        self.undo.pop_front();
                    }
                    self.undo.push_back(current);
                }
                true
            }
            None => false,
        }
    }

    /// Records the current state of the items, so that the operation that follows can be
    /// reverted by [`Kernel::undo`].
    ///
    /// note: One checkpoint per *operation*, not per item: removing eight tool results is one
    /// thing the user did, and one `undo` puts all eight back.
    pub(crate) fn checkpoint(&mut self) {
        // whatever was undone is now a future that did not happen, and keeping it reachable
        // would let a redo silently overwrite work done since
        self.redo.clear();

        if self.undo_depth == 0 {
            return;
        }
        if self.undo.len() == self.undo_depth {
            self.undo.pop_front();
        }
        self.undo.push_back(self.items.clone());
    }
}
