//! `Trim`: the compactor, which elides the oldest tool results and says exactly what it took.
//!
//! note: it refuses to elide anything smaller than the marker that would replace it, because an
//! elided item leaves behind a sentence carrying the reason for eliding it - and on a short result
//! that costs more than the content did. A compactor that did not check would watch the total
//! refuse to move and elide everything it had.
//!
//! note: with one exception, which is why blobs are partitioned out first. A
//! [`Content::Blob`](nachalnik::Content::Blob) is counted at `0` by every counter in this
//! workspace, because what a picture costs is a formula over its dimensions and no byte length
//! reaches one - so that same check reads a picture as recovering nothing and skips it, and the
//! largest thing in the context would be the one thing this could never take.
//!
//! note: so size decides nothing about a blob, in *either* direction - not which one goes first,
//! and not whether a small one is worth taking at all. The second half is not an oversight: a
//! small blob is small in base64, which is the one measure that says nothing about what it costs.
//! An eight-pixel PNG is a hundred bytes and 255 tokens at a vendor charging 85 plus 170 a tile,
//! so "too small to be worth eliding" is a judgement this pass can make about prose and cannot
//! make about a picture. Where it cannot judge, it takes.

use std::sync::Arc;

use nachalnik::{
    Budget, CompactionPlan, Compactor, Content, ContextId, ContextItem, ContextKind, ContextState,
    async_trait,
};

/// Elides any tool result carrying a blob, then the oldest of the rest, once the context gets
/// full - and says so.
///
/// note: it does not summarize what it elided, and the note it leaves behind claims only that
/// the content existed and is gone - a compactor that invented a paraphrase of output it never
/// read would be putting words in a tool's mouth. Every elision is reversible: the items keep
/// what they hold, they stay in the context pane with it counted as held back, and restoring one
/// is a keystroke. Anything pinned is refused by the kernel and reported as refused.
///
/// note: blobs go first on principle rather than on measurement, and the principle is that this
/// is a *terminal* client. It renders no pictures and is not going to, so a blob here is a cost
/// the person cannot see, paid out of a budget that cannot count it, for a payload the screen
/// will only ever name. Every one of those is a reason to be the first thing out and none of them
/// is visible to an arithmetic over `tokens`. A client that shows pictures should want a
/// different rule, which is why this one is `kamchatka`'s and not the runtime's.
pub struct Trim {
    /// How full the context has to be before this bothers.
    pub threshold: f64,
    /// How empty it is trying to get it.
    pub target: f64,
}

impl Trim {
    /// One that starts at this fraction of the limit and aims below it.
    ///
    /// note: the pair has to be ordered or the pass is a no-op that keeps being asked for, and
    /// nothing in the two fields says so. The target is twenty points under where it starts
    /// bothering, or half of it, whichever leaves more. With the target at or above the
    /// threshold, a context between the two is over the threshold and already under the target,
    /// so every pass finds nothing and says so, before every request, for as long as it stays in
    /// that band. Below a fifth the subtraction goes negative and the target becomes no context
    /// at all, so something has to catch it; a fraction of the threshold catches it without ever
    /// rising above it.
    #[must_use]
    pub fn under(threshold: f64) -> Self {
        Self {
            threshold,
            target: (threshold - 0.2).max(threshold / 2.0),
        }
    }
}

#[async_trait]
impl Compactor for Trim {
    /// note: the threshold, *or* anything in the request the counter would not price. The second
    /// half is what makes taking blobs first worth anything: a context that is mostly pictures
    /// reports a handful of tokens, so the fraction never reaches the threshold, so `plan` is
    /// never called and the pass that would have taken them never runs. Without it, the state in
    /// which this compactor is most needed is the one state it sleeps through.
    ///
    /// note: it is not a second threshold in disguise. `plan` still takes only what it may, and
    /// still answers `None` when there is nothing worth taking, which is the same answer a
    /// pinned oversized result has always got.
    ///
    /// note: and where the unpriced thing is one nothing may take - a pinned attachment, most of
    /// them - this stays `true` for the rest of the session, so `plan` is asked before every
    /// request and declines every time. That is the intended answer rather than a wasted pass:
    /// the alternative is a rule for when to stop asking, and any such rule is a way to be
    /// holding an unpriced item and not know it. What it costs is one walk of the items per
    /// request, which is what the projection already does twice.
    fn should_compact(&self, budget: &Budget) -> bool {
        let over = budget
            .fraction_used()
            .is_some_and(|used| used >= self.threshold);

        over || !budget.fully_counted()
    }

    async fn plan(&self, items: &[Arc<ContextItem>], budget: &Budget) -> Option<CompactionPlan> {
        let target = (budget.limit? as f64 * self.target) as usize;

        // written before anything is chosen, because what one elision recovers depends on it:
        // the kernel makes this the note on every item in the pass, and the note is the marker.
        // The model reads it too, in the brackets the projector puts round it, so it is written
        // to be read by both - what happened, why, and what not to do about it
        //
        // note: the second sentence closes a retry, for the reason `Reach::allows` closes one: a
        // refusal that does not say the next attempt will end the same way is read as an
        // invitation to make it. A marker that only says what happened leaves a model reading a
        // file too big for the context again and again, every read discarded on arrival.
        //
        // note: what it says is what *would* happen rather than what will. Re-reading is not
        // certainly compacted again - the threshold is about the whole context, and something
        // else may have gone since - so the true sentence is about the tokens going back into a
        // context that had no room for them, and the model can draw the conclusion. And it names
        // the way out, because a closed retry with nowhere to go is worse than no sentence: the
        // part it needs is a narrower read away, which is what `grep` is for.
        //
        // note: it costs what it says. The marker is the thing this refuses to elide anything
        // smaller than, so a longer reason raises that floor by a handful of tokens - paid once
        // per elided item, against a pass that only runs when thousands are at stake.
        let reason = format!(
            "compacted to make room; the context had reached {}% of the {}-token limit. Reading \
             it again would put the same tokens back into a context that had no room for them - \
             ask for the part you need instead",
            (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize,
            budget.limit.unwrap_or_default(),
        );
        let marker = marker_tokens(&reason);

        // oldest first, because the results a conversation has moved past are the ones it is
        // least likely to want back.
        //
        // note: `sends_content` rather than `is_projected`. An elided item *is* projected - as a
        // marker - so with `is_projected` every item a pass had already elided would come back as
        // a candidate on the next one. The plan would never be empty, so never `None`, so a
        // summary would go into the context before every request from then on: a compactor
        // growing the context by a line and burning an undo per request, for as long as the
        // session lasts. It takes only a pinned file bigger than the target to get there
        //
        // note: and not a pinned one, which `sends_content` says yes to. The kernel refuses those
        // - a pin is a promise - so proposing one produces a plan that moves nothing, and a plan
        // is not nothing: it is a summary and an undo. One pinned result bigger than the target
        // keeps the context over the threshold for the rest of the session, so the pass is asked
        // again before every request and refuses again every time. Not naming what may not be
        // taken is what makes the plan `None` instead
        //
        // note: and not one whose call is no longer sent, which the projector leaves out and the
        // kernel will not elide. Named anyway, it was counted into the summary's total as though
        // it had gone, and the model was told more results were elided than had been
        let asked: std::collections::HashSet<&nachalnik::ToolCallId> = items
            .iter()
            .filter(|item| item.state.is_projected())
            .flat_map(|item| item.calls().map(|call| &call.id))
            .collect();
        let candidates = items.iter().filter(|item| {
            item.state.sends_content()
                && item.state != ContextState::Pinned
                && matches!(&item.kind, ContextKind::ToolResult { call, .. } if asked.contains(call))
        });
        // blobs first and unconditionally, which is the one place this pass does not consult a
        // size. A picture is the largest thing in the context and the counter puts it at `0`, so
        // every arithmetic below would rank it last and then decline to take it at all - and a
        // compactor that skips the biggest item because it was told the item is free is not
        // making a decision, it is reporting one that was made by a gap in the estimate. Taken
        // first, before age is consulted: whatever a byte of prose is worth here, a megabyte of
        // base64 nothing can count is worth less
        //
        // note: except one the model has not been shown yet, for the reason the fresh results
        // below are kept: a screenshot elided on its way in is a picture the model asked for and
        // never saw, and asks for again. It goes on the next pass, first, once it has been read
        //
        // what the model has not been shown yet: the results after its last turn, which it asked
        // for and the request about to go is the first to carry. Taken for the threshold like the
        // rest, a search that fitted the limit was elided on its way in - the model ran it again
        // for a marker, and again. So these go only where the request would not fit the limit
        // without them, and after everything already read
        let unseen: Vec<ContextId> = items
            .iter()
            .rposition(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
            .map(|turn| items[turn + 1..].iter().map(|item| item.id).collect())
            .unwrap_or_default();
        let (fresh, seen): (Vec<_>, Vec<_>) =
            candidates.partition(|item| unseen.contains(&item.id));
        let (blobs, rest): (Vec<_>, Vec<_>) = seen
            .into_iter()
            .partition(|item| carries_blob(&item.content));
        let limit = budget.limit?;

        let mut used = budget.used();
        let mut elide = Vec::new();
        let blob_count = blobs.len();
        for item in blobs {
            // the same arithmetic the loop below uses, and it is here for what it will be worth
            // later rather than for what it is worth now: a blob is counted at `0`, so this
            // credits the pass with nothing, which is the honest figure for a saving nobody can
            // measure. Put a counter that does know what a picture costs behind `set_counter` and
            // the same line starts crediting the real one, without this pass learning a formula.
            // `saturating_sub` rather than the `checked_sub` below, because there the `None` is a
            // decision - too small to be worth taking - and here nothing is allowed to be one
            used -= item.tokens.saturating_sub(marker).min(used);
            elide.push(item.id);
        }
        for item in rest {
            if used <= target {
                break;
            }
            // what eliding one recovers is its content less the marker that takes its place, and
            // a result no bigger than the marker is not worth eliding at all: the pass would
            // spend the person's undo and a line of the model's attention to make the request
            // bigger. Skipped rather than breaking, because these are in the order the
            // conversation happened and a small one early says nothing about the next
            let Some(recovered) = item.tokens.checked_sub(marker).filter(|net| *net != 0) else {
                continue;
            };
            used -= recovered.min(used);
            elide.push(item.id);
        }
        for item in fresh {
            if used <= limit {
                break;
            }
            let Some(recovered) = item.tokens.checked_sub(marker).filter(|net| *net != 0) else {
                continue;
            };
            used -= recovered.min(used);
            elide.push(item.id);
        }
        if elide.is_empty() {
            return None;
        }

        // note: the last pass's summary goes out as this one goes in, and this is the difference
        // between a compactor that manages a context and one that fills it. Every pass leaves a
        // sentence behind; a summary is a `Reference` and this pass only ever takes a tool
        // result, so nothing else takes one back out, and a long session would pile up copies
        // of the same sentence in a context the pass was called on to make room in.
        //
        // note: `remove` and not `elide`, because a marker where a summary was is a line of text
        // saying a line of text has been taken away. The warning on `CompactionPlan::remove` is
        // about tool results, whose call goes down with them; a summary answers nothing, so
        // excluding it takes nothing with it. It stays in the context, on the tab, restorable -
        // the state is the mechanism, as everywhere else here.
        //
        // note: not the pinned ones. The kernel refuses those and reports the refusal, and a
        // plan that names what it may not have is the same mistake the candidate filter above
        // exists to avoid.
        let superseded: Vec<ContextId> = items
            .iter()
            .filter(|item| {
                item.source == "compaction"
                    && item.state.sends_content()
                    && item.state != ContextState::Pinned
            })
            .map(|item| item.id)
            .collect();

        // so the standing sentence has to speak for every pass rather than for this one, since
        // it is the only one left saying anything
        let (elided_before, blobs_before) = items
            .iter()
            .filter(|item| {
                item.state.is_elided() && matches!(item.kind, ContextKind::ToolResult { .. })
            })
            .fold((0, 0), |(all, with_blob), item| {
                (all + 1, with_blob + carries_blob(&item.content) as usize)
            });
        let (elided_now, blobs_now) = (elided_before + elide.len(), blobs_before + blob_count);

        // elided rather than removed, so that the call each of these answers keeps its answer.
        // Removing them would have the projector take the calls down as well - it has to, a call
        // with no result is a request most providers reject - and the model would then be reading
        // a conversation in which it never asked for any of this, directly above a note saying
        // the results had been dropped. The two accounts would disagree, and the marker is the
        // true one
        Some(CompactionPlan {
            // note: `elided`, which is the word the pane puts on the row, the word `context`'s own
            // `elide` takes, and the word the runtime's state is called. Not `shortened to a
            // marker`, which is another name for the thing, and not `truncated`, which is what an
            // output limit does and is not this at all
            //
            // note: the blobs get their own clause, and they need one. What the model is left
            // reading in place of an elided item is the pass's `reason`, which says the context
            // was full and says nothing about what used to be there - so a picture named by its
            // media type and size a moment ago becomes a sentence about a token limit, and the
            // model has no way left to know an image was ever in the conversation. A gap
            // where a picture was is worse than a sentence saying there was one; this is the
            // sentence, and it is the only place in the plan there is room for it
            summary: Some(ContextItem::summary(match blobs_now {
                0 => format!(
                    "{elided_now} earlier tool result(s) have been elided to make room: each is \
                     now a one-line marker where its content was. Ask again for anything you \
                     still need."
                ),
                blobs => format!(
                    "{elided_now} tool result(s) have been elided to make room, {blobs} of them \
                     carrying an image or other non-text payload: each is now a one-line marker \
                     where its content was. Ask again for anything you still need."
                ),
            })),
            reason,
            elide,
            remove: superseded,
        })
    }
}

/// Estimates what one elision marker costs: the pass's reason, in the brackets
/// [`LinearProjector`](nachalnik::LinearProjector) puts round it, at the four bytes a token
/// [`BytesPerToken`](nachalnik::BytesPerToken) assumes.
///
/// note: an estimate, and it does not have to be better than one - a counter that has learnt a
/// different ratio moves the boundary by one small result either way. What it has to be right
/// about is that a marker costs *something*. Credited with the whole of what it elided, a pass
/// that replaces short results with a longer marker reports tokens recovered while the request
/// grows - through the limit the pass exists to keep it under.
///
/// note: taken from the reason rather than fixed, because the reason is what the marker says. A
/// constant here would be a second place to remember when that sentence is reworded.
fn marker_tokens(reason: &str) -> usize {
    // `[... ` and ` ...]`, which the projector supplies and this does not get to choose
    (reason.len() + 10).div_ceil(4)
}

/// Whether the content is a blob or has one somewhere inside it.
///
/// note: [`Content::blobs`] does the walking rather than this. The nesting is the hard part - a
/// sentence-and-a-screenshot turn is a blob one level down inside `Content::Blocks` - and
/// `Content` and `Block` are both `#[non_exhaustive]`, so a walk written out here would answer
/// `false` for a variant added later and a picture would quietly stop going first, with no arm
/// this file could have written to catch it. The runtime's own counter needs the same walk for
/// the same reason, and a second copy here would go stale without anybody noticing.
fn carries_blob(content: &Content) -> bool {
    !content.blobs().is_empty()
}
