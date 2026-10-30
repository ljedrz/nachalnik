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

/// A list of [`ContextId`]s, in order, held as runs of consecutive ones.
///
/// note: what [`Event::ModelRequested`] names the items of a request with. A request names nearly
/// every item in the context, and the log keeps every request, so as a `Vec` the log grew with the
/// square of the session: a thousand turns held millions of identifiers, most of the session's
/// memory. A request's items are consecutive but for what is left out or moved, so this holds one
/// run for each stretch between those.
///
/// note: written as it is held: a list in which a run of more than one is `[first, last]` and a
/// run of one is the identifier, so `[[1, 40], 42, [44, 90]]`. Written as the plain list it stands
/// for, the log on disk still grew with the square of the session. A plain list is still read,
/// since it is a list of runs of one, which is what logs before [`FORMAT`](crate::FORMAT) 2 hold.
/// Runs are made greedily as identifiers are added or read, so two lists of the same identifiers
/// in the same order are equal however they were built or written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextIds {
    /// The first identifier of each run, and how many follow it one by one, the first included.
    runs: Vec<(u64, u64)>,
    len: usize,
}

impl ContextIds {
    /// An empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an identifier at the end.
    pub fn push(&mut self, id: ContextId) {
        self.extend_run(id.0, 1);
    }

    /// Adds `count` consecutive identifiers from `first` at the end, onto the last run where they
    /// continue it.
    fn extend_run(&mut self, first: u64, count: u64) {
        self.len += count as usize;
        match self.runs.last_mut() {
            Some((start, held)) if start.checked_add(*held) == Some(first) => *held += count,
            _ => self.runs.push((first, count)),
        }
    }

    /// How many identifiers there are.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The identifiers, in order.
    pub fn iter(&self) -> impl Iterator<Item = ContextId> + '_ {
        self.runs
            .iter()
            .flat_map(|&(first, count)| (first..first + count).map(ContextId))
    }

    /// Whether `id` is one of them.
    pub fn contains(&self, id: &ContextId) -> bool {
        self.runs
            .iter()
            .any(|&(first, count)| (first..first + count).contains(&id.0))
    }
}

impl FromIterator<ContextId> for ContextIds {
    fn from_iter<I: IntoIterator<Item = ContextId>>(ids: I) -> Self {
        let mut all = Self::new();
        for id in ids {
            all.push(id);
        }
        all
    }
}

impl From<Vec<ContextId>> for ContextIds {
    fn from(ids: Vec<ContextId>) -> Self {
        ids.into_iter().collect()
    }
}

impl From<&[ContextId]> for ContextIds {
    fn from(ids: &[ContextId]) -> Self {
        ids.iter().copied().collect()
    }
}

/// One entry of a written [`ContextIds`]: an identifier, or the first and last of a run.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Run {
    One(u64),
    Many([u64; 2]),
}

impl Serialize for ContextIds {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.runs.iter().map(|&(first, count)| match count {
            1 => Run::One(first),
            _ => Run::Many([first, first + count - 1]),
        }))
    }
}

impl<'de> Deserialize<'de> for ContextIds {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut ids = Self::new();
        for run in Vec::<Run>::deserialize(deserializer)? {
            let (first, count) = match run {
                Run::One(id) => (id, 1),
                Run::Many([first, last]) => {
                    match last.checked_sub(first).and_then(|n| n.checked_add(1)) {
                        Some(count) => (first, count),
                        None => {
                            return Err(serde::de::Error::custom(format!(
                                "a run of identifiers from {first} to {last} runs backwards or has no end"
                            )));
                        }
                    }
                }
            };
            ids.extend_run(first, count);
        }

        Ok(ids)
    }
}

/// What a [`ContextItem`] is, in terms of the model protocol.
///
/// note: the only taxonomy the kernel uses. The variants with data carry exactly what rebuilding a
/// wire message from the item alone needs (an assistant turn's tool calls, a tool result's call
/// identity), so a context with excluded items can still be projected into a valid request.
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
        /// note: on the turn, not in an item of its own, because some APIs require it echoed back
        /// on exactly the turn it came from. Excluding a turn excludes its reasoning, the only
        /// correct answer for a signed block.
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
/// note: one state per thing the projector does with an item. `archived` (the whole of a shortened
/// output) and `superseded` (an item [`Kernel::supersede`] replaced) were once more words for
/// [`ContextState::Excluded`] that nothing branched on; now the note says why an item is out and
/// the state only that it is. Both still read back as `excluded`, so old snapshots and logs load.
///
/// note: [`ContextState::Elided`] is the projector's one distinction beyond in and out.
pub enum ContextState {
    /// Included in the projection.
    Active,
    /// Not included; the item's note says who took it out and why.
    ///
    /// note: taken out by a person, the model, a [`Compactor`] or the kernel itself, which keeps a
    /// shortened tool output out beside the copy the model is shown and takes out an item
    /// [`Kernel::supersede`] replaced. It is restored the same way whoever did it.
    #[serde(alias = "archived", alias = "superseded")]
    Excluded,
    /// Included in the projection, and protected: the kernel refuses to let a [`Compactor`]
    /// remove it.
    Pinned,
    /// Included in the projection, but as a short marker instead of its content.
    ///
    /// note: unlike [`ContextState::Excluded`], it keeps the item's place. An excluded tool result
    /// takes its call down with it (the projector must, or the request is malformed), so the model
    /// reads a conversation in which the call never happened; an elided one still answers its call,
    /// and only the content is gone. A [`Compactor`] should prefer it.
    ///
    /// note: the marker is the item's `note`, in brackets the projector supplies. The content is
    /// untouched, so restoring is [`Kernel::set_state`] to [`ContextState::Active`].
    ///
    /// note: an elided assistant turn loses its reasoning with its words (see
    /// [`LinearProjector::send_reasoning`](crate::LinearProjector::send_reasoning)) but keeps its
    /// calls, which still answer their results.
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
    /// note: free-form because the kernel never branches on it, only reports it. Constructors use
    /// `user`, `system`, `instruction`, `file`, `selection`, `diagnostic`, `memory`, `tool_result`,
    /// `model` and `compaction`; an extension should use its own name, so injected tokens can be
    /// traced to it.
    pub source: String,
    /// A short, human-facing name: a path, a command, a description.
    pub label: String,
    /// The content itself.
    pub content: Content,
    /// The estimated size of the item as it would be sent, as counted by the active
    /// [`TokenCounter`].
    pub tokens: usize,
    /// How many pieces of the item's content the active [`TokenCounter`] would not put a number on.
    ///
    /// note: the per-item half of [`Budget::uncounted`](crate::Budget::uncounted), so a client can
    /// show which row nobody priced: an item shown at `0 tokens` next to one at `12,000` invites
    /// the wrong conclusion about which to drop.
    ///
    /// note: set by the counter whenever `tokens` is, by the same rules; it does not change when
    /// the counter changes until [`Kernel::recount`](crate::Kernel::recount) brings both into line.
    ///
    /// note: `serde(default)`, so an older snapshot resumes reading `0`, which is what a counter
    /// that could not say so reported.
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
    /// note: this and `note` below are the two halves of "why is this here", and a fact belongs
    /// here if it outlives a state change. A shortened tool result stays one whatever its state, so
    /// which item holds the whole of it is recorded here; in `note` it would be lost at the first
    /// state change, and someone trying to understand the pair is likely to change the state back
    /// and forth.
    ///
    /// note: `serde(default)`, so an older snapshot still resumes.
    #[serde(default)]
    pub included_because: Option<String>,
    /// Why the item is in its current state; set whenever the state changes.
    ///
    /// note: replaced whenever the state is set, even with `None`, on purpose: a reason for being
    /// excluded stops being true once the item is back, and a stale reason is worse than none.
    /// Anything that must survive a state change goes in `included_because`.
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
    /// note: prefer [`ContextItem::thinking`]. A turn recorded as ordered blocks keeps its thinking
    /// in its content, where this cannot see it, and may hold several pieces, so this returns
    /// `None` for a turn full of reasoning. It stays because a conventional turn has exactly one,
    /// for which an [`Option`] is what a caller wants.
    pub fn reasoning(&self) -> Option<&Content> {
        match &self.kind {
            ContextKind::AssistantMessage { reasoning, .. } => reasoning.as_ref(),
            _ => None,
        }
    }

    /// Returns the model's thinking, wherever it is recorded, in the order it was produced.
    ///
    /// note: the counterpart of [`ContextItem::calls`]: an ordered turn keeps its thinking in
    /// [`Block::Reasoning`](crate::Block::Reasoning) blocks inside its content, and a client that
    /// only knew the conventional slot would show a reasoning model as having done no reasoning.
    ///
    /// note: it yields each block's content, not a [`Part`](crate::Part), because the conventional
    /// slot is a [`Content`] with no part to borrow. What a provider attached to a thinking block
    /// is reachable through the blocks and is the provider's, not a client's.
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
    /// note: a turn records its calls either in [`ContextKind::AssistantMessage::tool_calls`] or,
    /// when their order is part of the turn, as [`Block::Call`](crate::Block::Call) blocks in a
    /// [`Content::Blocks`], never both, so there is one account of what the model asked for. This
    /// reads whichever is in use; a [`Projector`] pairing calls with results should use it, since
    /// matching on the kind alone finds no calls in an ordered turn.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A request's items, consecutive but for a gap and a move, are held as one run for each
    /// stretch between them, and read back as the list they were.
    ///
    /// note: the point of the type. Held as a `Vec`, every request in the log named every item in
    /// the context, and a thousand turns kept millions of identifiers.
    #[test]
    fn identifiers_are_held_as_runs_and_read_back_as_they_were() {
        let consecutive: ContextIds = (1..=10_000).map(ContextId).collect();
        assert_eq!(consecutive.runs.len(), 1);
        assert_eq!(consecutive.len(), 10_000);

        // a gap where something was left out, and an item moved to where the one it replaces was
        let written = [1, 2, 3, 5, 6, 9, 7, 8].map(ContextId);
        let ids: ContextIds = written.iter().copied().collect();
        assert_eq!(ids.runs, [(1, 3), (5, 2), (9, 1), (7, 2)]);
        assert_eq!(ids.iter().collect::<Vec<_>>(), written);
        assert!(ids.contains(&ContextId(6)) && ids.contains(&ContextId(9)));
        assert!(!ids.contains(&ContextId(4)) && !ids.contains(&ContextId(10)));

        // built at once or one at a time, the same identifiers are the same list
        let mut pushed = ContextIds::new();
        for id in written {
            pushed.push(id);
        }
        assert_eq!(pushed, ids);
        assert!(ContextIds::new().is_empty());
    }

    /// The identifiers are written as their runs, a run of one as the identifier, and read from
    /// that or from the plain list a log before format 2 holds.
    ///
    /// note: written as the plain list, the log on disk grew with the square of the session as
    /// the log in memory did. A run that goes backwards, or past the last identifier there can be,
    /// is refused rather than read as some other list.
    #[test]
    fn identifiers_are_written_as_runs_and_read_from_runs_or_a_plain_list() {
        let ids: ContextIds = [1, 2, 3, 7, 9, 10].map(ContextId).into_iter().collect();
        assert_eq!(serde_json::to_string(&ids).unwrap(), "[[1,3],7,[9,10]]");
        assert_eq!(
            serde_json::from_str::<ContextIds>("[[1,3],7,[9,10]]").unwrap(),
            ids
        );
        assert_eq!(
            serde_json::from_str::<ContextIds>("[1,2,3,7,9,10]").unwrap(),
            ids,
            "a log in format 1"
        );
        assert!(serde_json::from_str::<ContextIds>("[]").unwrap().is_empty());

        for wrong in ["[[5,3]]", "[[0,18446744073709551615]]"] {
            assert!(
                serde_json::from_str::<ContextIds>(wrong).is_err(),
                "{wrong} is not a list of runs"
            );
        }
    }
}
