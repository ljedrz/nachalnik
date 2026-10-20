//! What a caller can do to the context the kernel owns: add to it, change an item's state or
//! its content, undo and redo, count it, project it, and compact it.

use std::{collections::HashSet, sync::Arc};

use serde_json::Value;

use crate::{
    compaction::{Budget, CompactionPlan, CompactionReport, ReportItem},
    context::{Context, ContextId, ContextItem, ContextState},
    error::{Error, Result},
    event::Event,
    model::{Content, ModelRequest, ToolCallId},
    projection::Projection,
    session::named_calls,
    tokens::TokenCounter,
};

use super::{
    Kernel, State, StateChange, addition, arriving,
    request::{projection_cost, tool_cost},
};

impl Kernel {
    /// Adds an item to the context, returning its identifier.
    pub fn push(&self, item: ContextItem) -> ContextId {
        let held = self.0.counter.read();
        let counter = &**held;
        let mut context = self.0.context.write();
        context.checkpoint();

        self.added(&mut context, item, counter)
    }

    /// Adds several items as one undoable operation, returning their identifiers.
    ///
    /// note: separate [`Kernel::push`] calls take a checkpoint each, so adding a project file by
    /// file could spend the whole undo history. A set of files is one thing the user did, so it is
    /// one undo.
    ///
    /// note: under one lock: item by item, a turn recorded on another thread halfway through would
    /// land inside this checkpoint, and one `undo` would take back the turn and the last files but
    /// not the first.
    pub fn push_all(&self, items: impl IntoIterator<Item = ContextItem>) -> Vec<ContextId> {
        let mut items = items.into_iter().peekable();
        if items.peek().is_none() {
            return Vec::new();
        }

        let held = self.0.counter.read();
        let counter = &**held;
        let mut context = self.0.context.write();
        context.checkpoint();

        items
            .map(|item| self.added(&mut context, item, &*counter))
            .collect()
    }

    /// Adds an item in place of an existing one, excluding the old one with a note naming the new,
    /// as one undoable operation.
    ///
    /// note: not [`Kernel::replace`], which changes an item in place and keeps its identifier. This
    /// adds a new item, of any kind and label, at the end and takes the old one out. It is explicit
    /// because the kernel cannot tell whether a second read of a file replaces the first or stands
    /// beside it. The old item keeps its identifier and contents and comes back with
    /// [`Kernel::set_state`] or [`Kernel::undo`].
    pub fn supersede(&self, old: ContextId, item: ContextItem) -> Result<ContextId> {
        // one lock for the check and both changes, so an `undo` on another thread cannot take `old`
        // away in between and leave this answering `Ok` having superseded nothing
        let held = self.0.counter.read();
        let counter = &**held;
        let mut context = self.0.context.write();
        if context.item(old).is_none() {
            return Err(Error::UnknownItem(old));
        }
        context.checkpoint();

        let new = self.added(&mut context, item, counter);
        let note = Some(format!("replaced by item {new}"));
        if let Some(from) = context.set_state(old, ContextState::Excluded, note.clone()) {
            self.emit(Event::ContextChanged {
                id: old,
                from,
                to: ContextState::Excluded,
                note,
            });
        }

        Ok(new)
    }

    /// Replaces an item's metadata, which is otherwise write-once.
    ///
    /// note: [`ContextItem::meta`] holds hints for a [`Compactor`] or [`Projector`], and by the
    /// time a tool result is known to be worthless it is already recorded, so a hint has to be
    /// settable afterwards.
    ///
    /// note: no checkpoint of its own: metadata belongs to the operation it describes, so undo
    /// takes an annotation back with the operation before it. It still clears what a redo could
    /// have put back.
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
        self.set_states(ids.into_iter().map(|id| (id, state, note.clone())))
    }

    /// Moves each given item to a state of its own, with a note of its own, as one undoable
    /// operation, and returns the ones that actually changed.
    ///
    /// note: [`Kernel::set_state`] with a state per item, for one move that does not land
    /// everything in one state, such as putting back a change that excluded some items and pinned
    /// others. As one `set_state` per state it would be an undo step per state, and one `undo`
    /// would take back only part of it.
    ///
    /// note: an item named twice is moved twice, in order, and ends in the second state.
    pub fn set_states(
        &self,
        changes: impl IntoIterator<Item = (ContextId, ContextState, Option<String>)>,
    ) -> StateChange {
        let changes: Vec<_> = changes.into_iter().collect();
        let mut outcome = StateChange::default();
        if changes.is_empty() {
            return outcome;
        }

        let mut announcements = Vec::new();
        {
            // the whole operation happens under one lock, so the checkpoint is a snapshot of the
            // context the changes are applied to
            let mut context = self.0.context.write();
            let mut targets = Vec::new();
            for (id, state, note) in changes {
                match context.item(id) {
                    None => outcome.unknown.push(id),
                    Some(_) if context.would_change(id, state, &note) => {
                        targets.push((id, state, note))
                    }
                    Some(_) => outcome.unchanged.push(id),
                }
            }

            // an operation that changes nothing does not get a checkpoint: spending one would make
            // the next `undo` put back what is already there, and the operation somebody wanted
            // reverted would need a second one
            if targets.is_empty() {
                return outcome;
            }
            context.checkpoint();

            for (id, state, note) in targets {
                let Some(from) = context.set_state(id, state, note.clone()) else {
                    continue;
                };
                announcements.push(Event::ContextChanged {
                    id,
                    from,
                    to: state,
                    note,
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
    /// note: replacing an item with what it already says changes nothing, so nothing is announced
    /// and no checkpoint is taken, the rule [`Kernel::set_state`] follows through
    /// `Context::would_change`. A checkpoint would be an undo that restores an identical state, and
    /// the operation somebody wanted reverted would need a second one.
    pub fn replace(&self, id: ContextId, content: impl Into<Content>) -> Result<()> {
        let content = content.into();
        let held = self.0.counter.read();
        let counter = &**held;
        let mut context = self.0.context.write();
        // an operation that is about to fail does not get a checkpoint
        let Some(item) = context.item(id) else {
            return Err(Error::UnknownItem(id));
        };
        if item.content == content {
            return Ok(());
        }
        context.checkpoint();

        match context.replace(id, content, counter) {
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
    /// note: one operation, not one item: undoing a [`Kernel::set_state`] that excluded eight items
    /// puts all eight back. Model responses and tool results are operations too, so undo can walk
    /// back a turn's additions.
    ///
    /// note: refused from [`State::Requesting`] until the machine rests with nothing to run, so
    /// also in [`State::Deciding`], [`State::Ready`] and [`State::Executing`], because it would
    /// rewind the context the machine is acting on. Undoing the turn that asked for a call did not
    /// stop the call: it ran, its result was recorded against a turn that was gone and never
    /// reached the model, and recording it took a checkpoint that made the undone turn unreachable.
    /// Decide the calls, or cancel them with [`Kernel::cancel_pending_calls`], first.
    ///
    /// note: an undo that removes the answer [`State::Finished`] names moves the machine to
    /// [`State::Idle`], so the state never names a missing item.
    pub fn undo(&self) -> Result<bool> {
        self.walk(true)
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
    /// note: a stack, not a toggle: undoing three operations and redoing them puts all three back,
    /// in order. Any new context operation makes what was undone unreachable, since redoing across
    /// later work would overwrite it.
    ///
    /// note: [`Error::Busy`] while a turn holds calls, as for [`Kernel::undo`].
    pub fn redo(&self) -> Result<bool> {
        self.walk(false)
    }

    /// [`Kernel::undo`] where `back`, [`Kernel::redo`] otherwise: the two differ only in the way
    /// they go, the event that says so, and undo's answer to taking away the answer the machine
    /// names.
    fn walk(&self, back: bool) -> Result<bool> {
        // the machine first, for the lock order, and held with the context so that no turn starts
        // between the look and the move
        let mut machine = self.0.machine.lock();
        if Self::holds_calls(&machine.state) {
            return Err(Error::Busy);
        }
        // the counter between the two, for the lock order: what comes back was counted on the scale
        // in force when its checkpoint was taken, and is counted again on this one
        let held = self.0.counter.read();
        let mut context = self.0.context.write();
        let before = context.items().to_vec();
        let moved = match back {
            true => context.undo(),
            false => context.redo(),
        };
        if !moved {
            return Ok(false);
        }
        let tokens_before = context.tokens();
        let recounted = context.recount(&**held);
        let diff = Self::diff(&before, context.items());
        let answer_gone = back
            && matches!(machine.state, State::Finished { item, .. } if diff.gone.contains(&item));

        self.emit(match back {
            true => Event::ContextUndone {
                items: diff.items,
                removed: diff.gone,
                changed: diff.changed,
            },
            false => Event::ContextRedone {
                items: diff.items,
                restored: diff.appeared,
                changed: diff.changed,
            },
        });
        self.recounted(recounted, tokens_before, &context);
        if answer_gone {
            self.transition(&mut machine, State::Idle);
        }

        Ok(true)
    }

    /// Recounts every item's tokens with the active [`TokenCounter`].
    pub fn recount(&self) {
        let held = self.0.counter.read();
        let counter = &**held;
        self.recount_in(&mut self.0.context.write(), counter);
    }

    /// [`Kernel::recount`], for a caller already holding the context lock.
    pub(super) fn recount_in(&self, context: &mut Context, counter: &dyn TokenCounter) {
        let tokens_before = context.tokens();
        let _ = context.recount(counter);

        self.emit(Event::ContextRecounted {
            tokens_before,
            tokens_after: context.tokens(),
        });
    }

    /// Announces the recount an undo or a redo made of what it brought back, if it moved a figure.
    ///
    /// note: after the undo or redo itself, because that is the order they happened in: the
    /// items as checkpointed, then those items counted on the scale in force.
    fn recounted(&self, recounted: bool, tokens_before: usize, context: &Context) {
        if recounted {
            self.emit(Event::ContextRecounted {
                tokens_before,
                tokens_after: context.tokens(),
            });
        }
    }

    /// Returns how much room the next request would take, and how much there is.
    pub fn budget(&self) -> Budget {
        // one counter for both halves, as a request is priced: a `set_counter` between them would
        // otherwise price the tools and context by two different counters
        let counter = self.counter();
        let tools = tool_cost(&self.tool_specs(), &*counter);
        let limit = self.model_info().and_then(|i| i.context_limit);
        let reported = self.last_response().and_then(|response| response.usage);

        let context = self.projected_with(&*counter).1;

        Budget {
            context_tokens: context.tokens,
            uncounted: context.uncounted + tools.uncounted,
            tool_tokens: tools.tokens,
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
    /// note: the kernel adds nothing of its own between this and the wire. Only a [`Compactor`] can
    /// still change the request, at the start of the next [`Kernel::step`], and it reports what it
    /// did; with it goes the caller's full notice, placed or retired as the context is full or not
    /// (see [`Kernel::set_full_notice`]). Each is an ordinary context change with its own event,
    /// and neither is the kernel's own words.
    ///
    /// [`Compactor`]: crate::Compactor
    pub fn preview_request(&self) -> Result<ModelRequest> {
        self.build_request(&*self.counter())
            .map(|(request, _, _)| request)
    }

    /// Renders the payload the provider would send for the next request, exactly as it would send
    /// it - or `None` when the provider cannot show one.
    ///
    /// note: as close to the wire as a kernel without a wire format gets.
    /// [`Kernel::preview_request`] is the kernel's own account, which it can guarantee; this is the
    /// provider's, which it cannot. A provider that renders one payload here and sends another lies
    /// as a [`Tool`] that declares `Read` and opens a socket lies, and the defence is the same: you
    /// chose the provider.
    ///
    /// note: the body only; headers, URLs and credentials never pass through the kernel.
    ///
    /// [`Tool`]: crate::Tool
    pub fn preview_payload(&self) -> Result<Option<Value>> {
        let provider = self.provider().ok_or(Error::NoProvider)?;
        let (request, _, _) = self.build_request(&*self.counter())?;

        Ok(provider.render(&request))
    }

    /// Applies a compaction plan, returning (and broadcasting) a report of what it did.
    ///
    /// note: pinned items in the plan are refused and listed in [`CompactionReport::refused`], and
    /// so is removing the call or result paired with a pinned item, because a pair goes out
    /// together or not at all.
    ///
    /// note: a pass that moves nothing takes no checkpoint, like every operation here. A
    /// [`Compactor`] whose candidates are all pinned or already elided returns a plan before every
    /// request, and a checkpoint each time would cost an undo per request and make [`Kernel::redo`]
    /// unreachable for the rest of the session.
    ///
    /// [`Compactor`]: crate::Compactor
    pub fn apply_compaction(&self, plan: CompactionPlan) -> CompactionReport {
        // both are taken before the context lock, because that is the lock order and neither can be
        // reached once it is held
        let projector = self.projector();
        let held = self.0.counter.read();
        let counter = &**held;
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

        // the whole pass is under one lock: item by item, a push landing between the checkpoint and
        // the removals would make the next `undo` restore a context that never existed, one without
        // the newly added item
        {
            let mut context = self.0.context.write();
            // the projected total, not the sum of the items, since that is what the report states
            // and a pass is judged by. Summing credits an elision with everything it took and
            // nothing for its marker, so a pass that made the request bigger could report a
            // decrease. Projecting moves `Content` by pointer and happens at most once a request
            let before = projector.project(context.items());
            let cost_before = projection_cost(&before, counter);
            // only the projection knows what the request carries, which is what a pass may take. An
            // item the projector repaired away, such as a second result for a call that has one, is
            // `Active` and sends nothing: taking it recovers nothing while reporting all of it as
            // recovered
            let carrying: HashSet<ContextId> = before.included.iter().copied().collect();
            // a call and its result go out together or not at all, so excluding one half of a
            // pinned pair would take the pinned half out of the request too. Elision does not
            // count, because an elided item keeps its place in the pair
            let pinned_calls: HashSet<&ToolCallId> = context
                .items()
                .iter()
                .filter(|item| item.state == ContextState::Pinned && carrying.contains(&item.id))
                .flat_map(|item| named_calls(item))
                .collect();

            // the plan is worked out before anything moves, because the checkpoint must come before
            // the first change, and not at all if nothing changes. An id named twice, or in both
            // lists, is moved and reported once, and removal wins as the larger change. A pin is
            // refused once per list naming it, however often: the report says what each list asked
            // for
            let mut planned = HashSet::new();
            let mut removing = Vec::new();
            let mut refused_removal = HashSet::new();
            for id in remove {
                let Some(item) = context.item(id) else {
                    continue;
                };
                let entry = ReportItem {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                };
                if item.state == ContextState::Pinned
                    || named_calls(item).any(|call| pinned_calls.contains(call))
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
                let entry = ReportItem {
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

            // a summary stands in for what was taken, so a pass that took nothing gets none. Added
            // anyway it compounds: the context is no smaller, the compactor is asked again before
            // the next request and says yes again, and one whose every candidate is pinned would
            // add a summary, spend an undo and grow the request on every request for the rest of
            // the session
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

            // note: the note is set from the pass's reason, as for the removals above, because a
            // note explains the state an item is in, and any note it already carries explains the
            // state it is leaving
            for entry in eliding {
                // the reason as written, without the `compaction:` prefix a removal's note gets:
                // the model reads this one, inside the projector's brackets, and the client row
                // already says `elided`
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
                let id = context.add(item, counter);
                let item = context.item(id).expect("the item was just added");
                announcements.push(addition(item));
                announcements.extend(arriving(item));
                added = Some(ReportItem {
                    id,
                    label: item.label.clone(),
                    tokens: item.tokens,
                });
            }

            // a pass that moved nothing left the context unchanged under the same lock, and that is
            // what a compactor with nothing left to take reports before every request
            let cost_after = match moved {
                true => projection_cost(&projector.project(context.items()), counter),
                false => cost_before,
            };
            let report = CompactionReport {
                removed,
                elided,
                refused,
                summary: added,
                reason,
                tokens_before: cost_before.tokens,
                tokens_after: cost_after.tokens,
                uncounted_before: cost_before.uncounted,
                uncounted_after: cost_after.uncounted,
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
