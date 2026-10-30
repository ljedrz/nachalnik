//! The context: a list of identifiable items, what each one is, and what state it is in.
//!
//! note: the only place identifiers are handed out. Undo and redo are here rather than in the
//! kernel because a checkpoint restores this structure's invariants: every item still listed, every
//! identifier still its own, and nothing destroyed by removal from a request.

use std::{collections::VecDeque, sync::Arc};

use serde_json::Value;

#[cfg(doc)]
use crate::{Compactor, Event, Kernel, Projector};
use crate::{model::Content, tokens::TokenCounter};

mod item;

pub(crate) use item::WHOLE_OUTPUT;
pub use item::{ContextId, ContextIds, ContextItem, ContextKind, ContextState};

/// The set of context items, in the order they were added.
///
/// note: the state a request is derived from, not the request. Nothing is silently dropped: removal
/// is a state change, so a removed item can still be listed, inspected and restored.
///
/// note: mutating operations are `pub(crate)` because they all go through [`Kernel`], which turns
/// them into [`Event`]s; reading is unrestricted.
///
/// note: insertion order is identifier order, because identifiers are handed out in order and never
/// reused; [`Context::item`] relies on it.
#[derive(Debug, Clone)]
pub struct Context {
    items: Vec<Arc<ContextItem>>,
    undo: VecDeque<Vec<Arc<ContextItem>>>,
    redo: Vec<Vec<Arc<ContextItem>>>,
    undo_depth: usize,
    next_id: u64,
    /// How many checkpoints have been taken, so that a run of them can be named and folded.
    taken: u64,
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
            taken: 0,
        }
    }

    /// Returns every item, in insertion order, regardless of state.
    ///
    /// note: Items are behind an [`Arc`] so the context and its undo snapshots can be cloned
    /// without copying content. Treat them as immutable records.
    pub fn items(&self) -> &[Arc<ContextItem>] {
        &self.items
    }

    /// Returns the item with the given identifier.
    pub fn item(&self, id: ContextId) -> Option<&Arc<ContextItem>> {
        self.index_of(id).map(|index| &self.items[index])
    }

    /// Returns the position of the item with the given identifier.
    ///
    /// note: items are sorted by identifier, which is handed out in insertion order and never
    /// reused, so this is a binary search; a compaction plan naming a thousand items would
    /// otherwise be a thousand scans.
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

    /// Returns the estimated number of tokens for items sending their content.
    ///
    /// note: An elided item is projected but not sending its content, so its size is in
    /// [`Context::tokens_withheld`], not here. The marker in its place costs little and is counted
    /// in [`Budget::context_tokens`](crate::Budget::context_tokens), over the projector's messages.
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
        // Uses saturating add rather than overflowing: a snapshot numbered at the top is refused
        // by `Snapshot::problems`, and resuming one must not panic.
        self.next_id = self.next_id.saturating_add(1);
        item.id = id;
        measure(&mut item, counter);
        self.items.push(Arc::new(item));

        id
    }

    /// Returns whether moving an item to this state with this note would change anything.
    ///
    /// note: the note counts: "excluded because it was enormous" and "excluded because the user
    /// said so" are different facts, and one silently replacing the other is an unannounced edit.
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
        measure(item, counter);

        Some((was, before, item.tokens))
    }

    /// Replaces the contents with the items of a [`Snapshot`](crate::Snapshot), recounting them.
    ///
    /// note: items are sorted by identifier and the next identifier is placed past the highest, so
    /// a hand-edited snapshot cannot break the ordering lookups rely on or reuse an identifier.
    ///
    /// note: an identifier shared by two items, or `0`, which means none, gets the next free one,
    /// and the first to hold a number keeps it; otherwise a lookup would find one of the two at
    /// random. This repairs a snapshot that should not exist; `Snapshot::problems` lets a caller
    /// refuse one instead.
    pub(crate) fn restore(
        &mut self,
        mut items: Vec<ContextItem>,
        next_id: u64,
        counter: &dyn TokenCounter,
    ) {
        items.sort_by_key(|item| item.id);
        let past_the_last = items
            .last()
            .map(|item| item.id.0.saturating_add(1))
            .unwrap_or(1);
        let mut next = next_id.max(past_the_last).max(1);
        let mut held = std::collections::HashSet::new();
        for item in &mut items {
            if item.id.0 == 0 || !held.insert(item.id) {
                item.id = ContextId(next);
                next = next.saturating_add(1);
            }
        }
        items.sort_by_key(|item| item.id);

        let items: Vec<_> = items
            .into_iter()
            .map(|mut item| {
                measure(&mut item, counter);
                Arc::new(item)
            })
            .collect();
        self.next_id = next;
        self.items = items;
        self.undo.clear();
        self.redo.clear();
    }

    /// Replaces an item's metadata, returning what it was if it changed.
    ///
    /// note: no checkpoint, because metadata goes with the operation it describes: a client that
    /// rewrites an item and records who did it wants one `undo` for both. It is still new work, so
    /// the redo stack is cleared; a redo across it would put the old metadata back.
    pub(crate) fn annotate(&mut self, id: ContextId, meta: Value) -> Option<Value> {
        let index = self.index_of(id)?;
        if self.items[index].meta == meta {
            return None;
        }
        let was = std::mem::replace(&mut Arc::make_mut(&mut self.items[index]).meta, meta);
        self.redo.clear();

        Some(was)
    }

    /// Recounts every item's tokens.
    ///
    /// note: each item is priced before it is touched, because `Arc::make_mut` copies an item the
    /// undo history shares, and [`Kernel::undo`](crate::Kernel::undo) reads a copy as a change; a
    /// recount that moves no figure must not be one. Returns whether any figure moved.
    pub(crate) fn recount(&mut self, counter: &dyn TokenCounter) -> bool {
        let mut moved = false;
        for item in &mut self.items {
            let figures = figures(item, counter);
            if figures != (item.tokens, item.uncounted) {
                let item = Arc::make_mut(item);
                (item.tokens, item.uncounted) = figures;
                moved = true;
            }
        }

        moved
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
    /// note: A stack, not a toggle: undoing three operations and redoing them puts all three back,
    /// in order. Doing anything new makes the redone future unreachable, so [`Context::checkpoint`]
    /// discards it.
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

    /// Records the current state of the items, so that the operation that follows can be reverted
    /// by [`Kernel::undo`].
    ///
    /// note: One checkpoint per operation, not per item: removing eight tool results is one thing
    /// the user did, and one `undo` puts all eight back.
    pub(crate) fn checkpoint(&mut self) {
        // Whatever was undone is now a future that did not happen; keeping it reachable would let a
        // redo silently overwrite work done since.
        self.redo.clear();
        self.taken += 1;

        if self.undo_depth == 0 {
            return;
        }
        if self.undo.len() == self.undo_depth {
            self.undo.pop_front();
        }
        self.undo.push_back(self.items.clone());
    }
}

impl Context {
    /// How many checkpoints have been taken; see [`Context::fold_since`].
    pub(crate) fn taken(&self) -> u64 {
        self.taken
    }

    /// Makes every checkpoint taken after the one numbered `taken` part of that one, so one undo
    /// takes back everything since.
    ///
    /// note: a batch groups results when something else lands between them, such as a tool that
    /// changed the context while running, or a client. As separate checkpoints, the first undo
    /// would take back the change and the later results but leave the earlier ones, a turn half
    /// answered. The kernel refuses an undo during a batch, so only what was pushed lies between
    /// the two numbers.
    ///
    /// note: if more was checkpointed than the undo depth holds, the batch's checkpoint went with
    /// the oldest, so folding leaves nothing of the batch to undo rather than a point in its
    /// middle.
    pub(crate) fn fold_since(&mut self, taken: u64) {
        for _ in 0..self.taken.saturating_sub(taken) {
            if self.undo.pop_back().is_none() {
                break;
            }
        }
        self.taken = taken;
    }
}

/// Puts the counter's two figures on an item: what it costs, and how much of it the counter would
/// not price.
///
/// note: one function for every path that counts an item, because `tokens` and `uncounted` must
/// move together and the types do not enforce it: a stale `uncounted` beside recounted `tokens`
/// would read as everything priced when another counter wrote the figure.
fn measure(item: &mut ContextItem, counter: &dyn TokenCounter) {
    (item.tokens, item.uncounted) = figures(item, counter);
}

/// The two figures [`measure`] puts on an item, for a path that has to know whether they moved.
fn figures(item: &ContextItem, counter: &dyn TokenCounter) -> (usize, usize) {
    (counter.count_item(item), counter.uncounted_item(item))
}
