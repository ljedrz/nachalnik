//! What a caller can do to the context the kernel owns: add to it, change an item's state or
//! its content, undo and redo, count it, project it, and compact it.

use std::{collections::HashSet, sync::Arc};

use serde_json::Value;

use crate::{
    compaction::{Budget, CompactionPlan, CompactionReport, Removed},
    context::{Context, ContextId, ContextItem, ContextState},
    error::{Error, Result},
    event::Event,
    model::{Content, ModelRequest, ToolCallId},
    projection::Projection,
    tokens::TokenCounter,
};

use super::{
    Kernel, State, StateChange, addition, arriving, paired,
    request::{projection_cost, tool_tokens},
};

impl Kernel {
    /// Adds an item to the context, returning its identifier.
    pub fn push(&self, item: ContextItem) -> ContextId {
        let counter = self.counter();
        let mut context = self.0.context.write();
        context.checkpoint();

        self.added(&mut context, item, &*counter)
    }

    /// Adds several items as one undoable operation, returning their identifiers.
    ///
    /// note: Separate [`Kernel::push`]es are a checkpoint each, so opening a project file by file
    /// can spend the whole undo history before the user has done anything. Putting a set of files
    /// in the context is one thing the user did, so it is one thing to undo.
    ///
    /// note: under one lock. Taken item by item, a turn recorded on another thread halfway through
    /// would land inside this checkpoint - and one `undo` would take back the turn and the tail of
    /// the files and leave their head.
    pub fn push_all(&self, items: impl IntoIterator<Item = ContextItem>) -> Vec<ContextId> {
        let mut items = items.into_iter().peekable();
        if items.peek().is_none() {
            return Vec::new();
        }

        let counter = self.counter();
        let mut context = self.0.context.write();
        context.checkpoint();

        items
            .map(|item| self.added(&mut context, item, &*counter))
            .collect()
    }

    /// Adds an item in place of an existing one, marking the old one
    /// [`ContextState::Superseded`], as one undoable operation.
    ///
    /// note: This is the operation that state is for. [`Kernel::set_state`] can set it too, and
    /// leaves saying what replaced the item to whoever did. It is explicit because the kernel
    /// cannot tell whether a second read of a file replaces the first or stands beside it. The
    /// old item keeps its identifier and its contents, and comes back with a
    /// [`Kernel::set_state`] or a [`Kernel::undo`] like anything else.
    pub fn supersede(&self, old: ContextId, item: ContextItem) -> Result<ContextId> {
        // one lock for the check and both changes, so that an `undo` on another thread cannot take
        // `old` away in between and leave this answering `Ok` having superseded nothing
        let counter = self.counter();
        let mut context = self.0.context.write();
        if context.item(old).is_none() {
            return Err(Error::UnknownItem(old));
        }
        context.checkpoint();

        let new = self.added(&mut context, item, &*counter);
        let note = Some(format!("replaced by item {new}"));
        if let Some(from) = context.set_state(old, ContextState::Superseded, note.clone()) {
            self.emit(Event::ContextChanged {
                id: old,
                from,
                to: ContextState::Superseded,
                note,
            });
        }

        Ok(new)
    }

    /// Replaces an item's metadata, which is otherwise write-once.
    ///
    /// note: [`ContextItem::meta`] exists so that a [`Compactor`] or a [`Projector`] can be given
    /// hints, and a hint you can only set before the item exists is not much use - by the time
    /// you know a tool result was worthless, it has already been recorded.
    ///
    /// [`Compactor`]: crate::Compactor
    /// [`Projector`]: crate::Projector
    pub fn annotate(&self, id: ContextId, meta: Value) -> Result<()> {
        let mut context = self.0.context.write();
        if context.item(id).is_none() {
            return Err(Error::UnknownItem(id));
        }

        if let Some(was) = context.annotate(id, meta.clone()) {
            self.emit(Event::ContextAnnotated { id, meta, was });
        }

        Ok(())
    }

    /// Returns every context item, in insertion order, whatever its state.
    pub fn items(&self) -> Vec<Arc<ContextItem>> {
        self.0.context.read().items().to_vec()
    }

    /// Returns the context item with the given identifier.
    pub fn item(&self, id: ContextId) -> Option<Arc<ContextItem>> {
        self.0.context.read().item(id).cloned()
    }

    /// Runs a closure against the context.
    ///
    /// note: The context is read-locked for the duration, so the closure must not call back into
    /// the kernel.
    pub fn with_context<R>(&self, f: impl FnOnce(&Context) -> R) -> R {
        f(&self.0.context.read())
    }

    /// Moves the given items to a state, as one undoable operation, and returns the ones that
    /// actually changed.
    ///
    /// Excluding, restoring and pinning are all this one call:
    ///
    /// ```text
    /// exclude -> set_state(ids, ContextState::Excluded, Some("garbage".into()))
    /// restore -> set_state(ids, ContextState::Active, None)
    /// pin     -> set_state(ids, ContextState::Pinned, None)
    /// ```
    ///
    /// note: Nothing is destroyed. An excluded item keeps its identifier, is still listed by
    /// [`Kernel::items`], and comes back with another `set_state` or with [`Kernel::undo`].
    pub fn set_state(
        &self,
        ids: impl IntoIterator<Item = ContextId>,
        state: ContextState,
        note: Option<String>,
    ) -> StateChange {
        let ids: Vec<_> = ids.into_iter().collect();
        let mut outcome = StateChange::default();
        if ids.is_empty() {
            return outcome;
        }

        let mut announcements = Vec::new();
        {
            // the whole operation happens under one lock, so that the checkpoint it takes is a
            // snapshot of the context the changes are actually applied to
            let mut context = self.0.context.write();
            let mut targets = Vec::new();
            for id in ids {
                match context.item(id) {
                    None => outcome.unknown.push(id),
                    Some(_) if context.would_change(id, state, &note) => targets.push(id),
                    Some(_) => outcome.unchanged.push(id),
                }
            }

            // an operation that changes nothing does not get a checkpoint: spending one would
            // make the next `undo` put back what is already there, and the operation somebody
            // wanted reverted would need a second one
            if targets.is_empty() {
                return outcome;
            }
            context.checkpoint();

            for id in targets {
                let Some(from) = context.set_state(id, state, note.clone()) else {
                    continue;
                };
                announcements.push(Event::ContextChanged {
                    id,
                    from,
                    to: state,
                    note: note.clone(),
                });
                outcome.changed.push(id);
            }

            // still under the lock: see the note on `Kernel::emit`
            for announcement in announcements {
                self.emit(announcement);
            }
        }

        outcome
    }

    /// Replaces an item's content in place, keeping its identifier.
    ///
    /// note: a replacement with what the item already says is not an operation. Nothing observable
    /// changes, so nothing is announced and no checkpoint is taken - the same rule
    /// [`Kernel::set_state`] follows through `Context::would_change`. A checkpoint for it would be
    /// an undo that puts back a state identical to the one it was asked from, and the operation
    /// somebody actually wanted reverted would need a second one.
    pub fn replace(&self, id: ContextId, content: impl Into<Content>) -> Result<()> {
        let counter = self.counter();
        let content = content.into();
        let mut context = self.0.context.write();
        // an operation that is about to fail does not get a checkpoint
        let Some(item) = context.item(id) else {
            return Err(Error::UnknownItem(id));
        };
        if item.content == content {
            return Ok(());
        }
        context.checkpoint();

        match context.replace(id, content, &*counter) {
            Some((was, tokens_before, tokens_after)) => {
                self.emit(Event::ContextReplaced {
                    id,
                    tokens_before,
                    tokens_after,
                    was,
                });
                Ok(())
            }
            None => Err(Error::UnknownItem(id)),
        }
    }

    /// Reverts the most recent context operation, returning whether there was one - or
    /// [`Error::Busy`] while a turn holds calls.
    ///
    /// note: The granularity is one operation, not one item: undoing a [`Kernel::set_state`]
    /// that excluded eight items puts all eight back. Model responses and tool results are
    /// operations too, so undo can also walk back a turn's worth of additions.
    ///
    /// note: refused from [`State::Requesting`] until the machine is resting with nothing to
    /// run, which takes in [`State::Deciding`], [`State::Ready`] and [`State::Executing`], because
    /// what it would rewind is the context the machine is acting on. Undoing the turn that asked
    /// for a call did not stop the call: it ran, its result was recorded against a turn no longer
    /// there and never reached the model, and recording it took a checkpoint that made the undone
    /// turn unreachable. Decide the calls, or cancel them with [`Kernel::cancel_pending_calls`],
    /// first.
    ///
    /// note: an undo that takes away the answer [`State::Finished`] names moves the machine to
    /// [`State::Idle`]. Left `Finished`, the state would name an item that is not there.
    pub fn undo(&self) -> Result<bool> {
        // the machine first, for the lock order, and held with the context so that no turn starts
        // between the look and the undo
        let mut machine = self.0.machine.lock();
        if Self::holds_calls(&machine.state) {
            return Err(Error::Busy);
        }
        let mut context = self.0.context.write();
        let before = context.items().to_vec();
        let Some(diff) = context.undo().then(|| Self::diff(&before, context.items())) else {
            return Ok(false);
        };
        let answer_gone =
            matches!(machine.state, State::Finished { item, .. } if diff.gone.contains(&item));

        self.emit(Event::ContextUndone {
            items: diff.items,
            removed: diff.gone,
            changed: diff.changed,
        });
        if answer_gone {
            self.transition(&mut machine, State::Idle);
        }

        Ok(true)
    }

    /// Whether the machine is part-way through a turn that has, or will have, calls of its own.
    pub(super) fn holds_calls(state: &State) -> bool {
        matches!(
            state,
            State::Requesting
                | State::Deciding { .. }
                | State::Ready { .. }
                | State::Executing { .. }
        )
    }

    /// Puts back what the last [`Kernel::undo`] took away, returning whether there was any.
    ///
    /// note: A stack, not a toggle: undoing three operations and redoing them puts all three
    /// back, in order. Any new context operation makes what was undone unreachable, because a
    /// redo that reached across work done since would be overwriting it rather than restoring
    /// anything.
    ///
    /// note: [`Error::Busy`] while a turn holds calls, for the reason [`Kernel::undo`] gives.
    pub fn redo(&self) -> Result<bool> {
        let machine = self.0.machine.lock();
        if Self::holds_calls(&machine.state) {
            return Err(Error::Busy);
        }
        let mut context = self.0.context.write();
        let before = context.items().to_vec();
        let Some(diff) = context.redo().then(|| Self::diff(&before, context.items())) else {
            return Ok(false);
        };

        self.emit(Event::ContextRedone {
            items: diff.items,
            restored: diff.appeared,
            changed: diff.changed,
        });

        Ok(true)
    }

    /// Recounts every item's tokens with the active [`TokenCounter`].
    pub fn recount(&self) {
        let counter = self.counter();
        self.recount_in(&mut self.0.context.write(), &*counter);
    }

    /// [`Kernel::recount`], for a caller already holding the context lock.
    pub(super) fn recount_in(&self, context: &mut Context, counter: &dyn TokenCounter) {
        let tokens_before = context.tokens();
        context.recount(counter);

        self.emit(Event::ContextRecounted {
            tokens_before,
            tokens_after: context.tokens(),
        });
    }

    /// Returns how much room the next request would take, and how much there is.
    pub fn budget(&self) -> Budget {
        // one counter for both halves, as a request is priced: a `set_counter` landing between
        // them would otherwise price the tools and the context by two different ones
        let counter = self.counter();
        let tool_tokens = tool_tokens(&self.tool_specs(), &*counter);
        let limit = self.model_info().and_then(|i| i.context_limit);
        let reported = self.last_response().and_then(|response| response.usage);

        let context = self.projected_with(&*counter).1;

        Budget {
            context_tokens: context.tokens,
            uncounted: context.uncounted,
            tool_tokens,
            limit,
            reported,
        }
    }

    /// Projects the current context into the messages of a request.
    pub fn project(&self) -> Projection {
        let projector = self.projector();

        projector.project(self.0.context.read().items())
    }

    /// Builds the request that the next [`Kernel::step`] would send - every message, every tool
    /// definition, every parameter.
    ///
    /// note: There is no step between this and the wire where the kernel adds something of its
    /// own.
    ///
    /// note: The one thing that can still change the request is a [`Compactor`], which runs at
    /// the start of the next [`Kernel::step`] - and says exactly what it did.
    ///
    /// [`Compactor`]: crate::Compactor
    pub fn preview_request(&self) -> Result<ModelRequest> {
        self.build_request(&*self.counter())
            .map(|(request, _, _)| request)
    }

    /// Renders the payload the provider would send for the next request, exactly as it would
    /// send it - or `None` when the provider cannot show one.
    ///
    /// note: This is as close to the wire as a kernel with no wire format can get.
    /// [`Kernel::preview_request`] is the kernel's own account, which it can guarantee; this is
    /// the provider's account of what it will make of it, which the kernel cannot. A provider
    /// that renders one payload here and sends another is lying in the same way a [`Tool`] that
    /// declares `Read` and opens a socket is lying, and the defence is the same: you chose the
    /// provider.
    ///
    /// note: The body only. Headers, URLs and credentials never pass through the kernel.
    ///
    /// [`Tool`]: crate::Tool
    pub fn preview_payload(&self) -> Result<Option<Value>> {
        let provider = self.provider().ok_or(Error::NoProvider)?;
        let (request, _, _) = self.build_request(&*self.counter())?;

        Ok(provider.render(&request))
    }

    /// Applies a compaction plan, returning (and broadcasting) a report of what it did.
    ///
    /// note: Pinned items in the plan are refused, and listed in [`CompactionReport::refused`].
    /// So is a removal of the call or the result a pinned item is paired with, because the pair
    /// goes out together or not at all.
    ///
    /// note: a pass that turns out to move nothing takes no checkpoint, like every other
    /// operation here. That is not a nicety about a hand-written plan: a [`Compactor`] is asked
    /// before every request, and one whose every candidate is pinned or already elided answers
    /// with a plan on every one of them - so a checkpoint spent here would cost an undo per
    /// request, and [`Kernel::redo`] would be unreachable for the rest of the session.
    ///
    /// [`Compactor`]: crate::Compactor
    pub fn apply_compaction(&self, plan: CompactionPlan) -> CompactionReport {
        let counter = self.counter();
        // both taken before the context lock, because that is the lock order and neither of these
        // may be reached for once it is held
        let projector = self.projector();
        let CompactionPlan {
            remove,
            elide,
            summary,
            reason,
        } = plan;

        let mut removed = Vec::new();
        let mut elided = Vec::new();
        let mut refused = Vec::new();
        let mut announcements = Vec::new();
        let mut added = None;

        // the whole pass happens under one lock. Taking it item by item would let a push land
        // between the checkpoint and the removals, and the next `undo` would then restore a
        // context that never existed - one without the item somebody had just added
        {
            let mut context = self.0.context.write();
            // the projected total rather than the sum of the items, which is what the report says
            // it is and what a pass is judged by. Summing the items credits an elision with the
            // whole of what it took away and charges nothing for the marker left in its place -
            // so a pass could report a decrease having made the request bigger. Projecting is a
            // walk over the items that moves `Content` by pointer, and it happens once a request
            // at most
            let before = projector.project(context.items());
            let tokens_before = projection_cost(&before, &*counter).tokens;
            // what a pass may take is what the request is carrying, and the projection is the
            // only thing that knows. An item the projector repaired away - a second result for a
            // call that already has one - is `Active`, holds everything it holds, and is
            // contributing nothing: taking it recovers nothing, reports the whole of it as
            // recovered, and moves something that was never being sent
            let carrying: HashSet<ContextId> = before.included.iter().copied().collect();
            // a call and its result go out together or not at all, so excluding one half of a
            // pinned pair takes the pinned half out of the request with it - reaching for the pin
            // through the item beside it. Elision is not in this, because an elided item keeps
            // its place in the pair
            let pinned_calls: HashSet<&ToolCallId> = context
                .items()
                .iter()
                .filter(|item| item.state == ContextState::Pinned && carrying.contains(&item.id))
                .flat_map(|item| paired(item))
                .collect();

            // what the plan comes to is worked out before anything moves, because the checkpoint
            // has to be taken before the first change and must not be taken at all if there is
            // not going to be one. An id named twice, or in both lists, is moved once and reported
            // once: removal wins, since it is the larger of the two. A pin is refused once per list
            // that names it, however many times that list does: the report says what each list
            // asked for, and a pin in both lists asked for two things
            let mut planned = HashSet::new();
            let mut removing = Vec::new();
            let mut refused_removal = HashSet::new();
            for id in remove {
                let Some(item) = context.item(id) else {
                    continue;
                };
                let entry = Removed {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                };
                if item.state == ContextState::Pinned
                    || paired(item).any(|call| pinned_calls.contains(call))
                {
                    if refused_removal.insert(id) {
                        refused.push(entry);
                    }
                } else if carrying.contains(&id) && planned.insert(id) {
                    removing.push(entry);
                }
            }

            let mut eliding = Vec::new();
            let mut refused_elision = HashSet::new();
            for id in elide {
                let Some(item) = context.item(id) else {
                    continue;
                };
                let entry = Removed {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                };

                if item.state == ContextState::Pinned {
                    if refused_elision.insert(id) {
                        refused.push(entry);
                    }
                    continue;
                }
                if !carrying.contains(&id) || item.state.is_elided() || !planned.insert(id) {
                    continue;
                }
                eliding.push(entry);
            }

            // a summary stands in the place of what was taken, so a pass that took nothing has no
            // place to put one. Added anyway, it compounds: the pass is asked again before the
            // next request, the context is no smaller than it was, so it says yes again - and a
            // compactor whose every candidate is pinned then adds a summary saying results were
            // elided, spends an undo, and grows the request it exists to shrink, on every request
            // for as long as the session lasts
            let moved = !removing.is_empty() || !eliding.is_empty();
            if moved {
                context.checkpoint();
            }

            for entry in removing {
                let note = Some(format!("compaction: {reason}"));
                if let Some(from) =
                    context.set_state(entry.id, ContextState::Excluded, note.clone())
                {
                    announcements.push(Event::ContextChanged {
                        id: entry.id,
                        from,
                        to: ContextState::Excluded,
                        note,
                    });
                    removed.push(entry);
                }
            }

            // note: the note is set from the pass's reason rather than kept, as the removals
            // above do - a note says why an item is in the state it is in, and the one it may
            // already be carrying explains the state it is leaving, which would be a strange
            // thing to hand the model as the reason it cannot see this any more
            for entry in eliding {
                // the reason as it was written, with no `compaction:` in front of it: unlike a
                // removal's note this one is read by the model, in the brackets the projector
                // puts round it, and a client showing it has already said `elided` in the row
                let note = Some(reason.clone());
                if let Some(from) = context.set_state(entry.id, ContextState::Elided, note.clone())
                {
                    announcements.push(Event::ContextChanged {
                        id: entry.id,
                        from,
                        to: ContextState::Elided,
                        note,
                    });
                    elided.push(entry);
                }
            }

            if let Some(item) = summary.filter(|_| moved) {
                let id = context.add(item, &*counter);
                let item = context.item(id).expect("the item was just added");
                announcements.push(addition(item));
                announcements.extend(arriving(item));
                added = Some(Removed {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                });
            }

            let report = CompactionReport {
                removed,
                elided,
                refused,
                summary: added,
                reason,
                tokens_before,
                // a pass that moved nothing left the context as it found it, under this same
                // lock - and it is the pass a compactor with nothing left to take answers with
                // before every request
                tokens_after: match moved {
                    true => projection_cost(&projector.project(context.items()), &*counter).tokens,
                    false => tokens_before,
                },
            };

            // still under the lock, and the pass's own events before the report of it: see the
            // note on `Kernel::emit`
            for announcement in announcements {
                self.emit(announcement);
            }
            self.emit(Event::Compacted {
                report: report.clone(),
            });

            report
        }
    }
}
