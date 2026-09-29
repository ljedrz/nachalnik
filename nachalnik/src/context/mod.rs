//! The context: a list of identifiable items, what each one is, and what state it is in.
//!
//! note: the data structure the rest of this crate is about, and the only place identifiers are
//! handed out. Undo and redo are here rather than in the kernel that calls them, because what a
//! checkpoint has to restore is this structure's own invariants - every item still listed, every
//! identifier still its own, and nothing destroyed by having been taken out of a request.

use std::{collections::VecDeque, sync::Arc};

use serde_json::Value;

#[cfg(doc)]
use crate::{Compactor, Event, Kernel, Projector};
use crate::{model::Content, tokens::TokenCounter};

mod item;

pub(crate) use item::WHOLE_OUTPUT;
pub use item::{ContextId, ContextItem, ContextKind, ContextState};

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
        // saturating rather than overflowing: a snapshot numbered at the top is refused by
        // `Snapshot::problems`, and one resumed regardless should not be a panic
        self.next_id = self.next_id.saturating_add(1);
        item.id = id;
        measure(&mut item, counter);
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
        measure(item, counter);

        Some((was, before, item.tokens))
    }

    /// Replaces the contents with the items of a [`Snapshot`](crate::Snapshot), recounting them.
    ///
    /// note: The items are sorted by identifier and the next one is taken past the highest of
    /// them, so that a snapshot somebody edited by hand cannot quietly break the ordering the
    /// lookups depend on, or hand out an identifier that is already in use.
    ///
    /// note: and an identifier two items share, or the `0` that means none was given, is given
    /// the next free one - the first to hold a number keeps it - because a lookup by identifier
    /// finds one of two at random, and an operation on it moves whichever that was. It is a repair
    /// of a snapshot that should not exist; `Snapshot::problems` is how to refuse one instead.
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
    /// note: no checkpoint, because metadata rides with the operation it describes - a client
    /// that rewrites an item and records who did it wants one `undo` for the two. But it is new
    /// work all the same, so the redone future goes: a redo that reached across it would put the
    /// old metadata back.
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
    /// note: priced before it is touched, because `Arc::make_mut` copies an item the undo history
    /// shares, and a copy is what [`Kernel::undo`](crate::Kernel::undo) reads as a change. A
    /// recount that moves no figure is not one.
    pub(crate) fn recount(&mut self, counter: &dyn TokenCounter) {
        for item in &mut self.items {
            let figures = figures(item, counter);
            if figures != (item.tokens, item.uncounted) {
                let item = Arc::make_mut(item);
                (item.tokens, item.uncounted) = figures;
            }
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

    /// Makes every checkpoint taken after the one numbered `taken` part of that one, so that one
    /// undo takes back all of what happened since.
    ///
    /// note: what a batch of results is for when something else lands between two of them - a
    /// tool that changed the context while it ran, or a client that did. Left as checkpoints of
    /// their own, the first undo took back the change and the results recorded after it, and left
    /// the ones before: a turn half answered. The kernel refuses an undo while a batch runs, so
    /// nothing is popped between the two numbers but what was pushed.
    ///
    /// note: where more was checkpointed than the undo depth holds, the batch's own checkpoint went
    /// with the oldest of them, and folding leaves nothing of the batch to undo to rather than a
    /// point in the middle of it.
    pub(crate) fn fold_since(&mut self, taken: u64) {
        for _ in 0..self.taken.saturating_sub(taken) {
            if self.undo.pop_back().is_none() {
                break;
            }
        }
        self.taken = taken;
    }
}

/// Puts the counter's two figures on an item: what it costs, and how much of it the counter
/// would not price.
///
/// note: one function for every path that counts an item, because the two fields have to move
/// together and nothing in the type system says so. An `uncounted` left behind by a path that
/// recounted `tokens` is worse than no field at all: it reads as a definite "everything here is
/// priced" while the tokens beside it have just been rewritten by a different counter.
fn measure(item: &mut ContextItem, counter: &dyn TokenCounter) {
    (item.tokens, item.uncounted) = figures(item, counter);
}

/// The two figures [`measure`] puts on an item, for a path that has to know whether they moved.
fn figures(item: &ContextItem, counter: &dyn TokenCounter) -> (usize, usize) {
    (counter.count_item(item), counter.uncounted_item(item))
}
