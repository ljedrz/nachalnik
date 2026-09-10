//! `Trim`: the compactor, which elides the oldest tool results and says exactly what it took.
//!
//! note: it refuses to elide anything smaller than the marker that would replace it, because an
//! elided item leaves behind a sentence carrying the reason for eliding it - and on a short result
//! that costs more than the content did. A compactor that did not check would watch the total
//! refuse to move and elide everything it had.
//!
//! note: with one exception, and it is the whole reason blobs are partitioned out first. A
//! [`Content::Blob`](nachalnik::Content::Blob) is counted at `0` by every counter in this
//! workspace, because what a picture costs is a formula over its dimensions and no byte length
//! reaches one - so that same check reads a picture as recovering nothing and skips it, which
//! made the largest thing in the context the one thing this could never take.
//!
//! note: so size decides nothing about a blob, in *either* direction - not which one goes first,
//! and not whether a small one is worth taking at all. The second half is not an oversight: a
//! small blob is small in base64, which is the one measure that says nothing about what it costs.
//! An eight-pixel PNG is a hundred bytes and 255 tokens at a vendor charging 85 plus 170 a tile,
//! so "too small to be worth eliding" is a judgement this pass can make about prose and cannot
//! make about a picture. Where it cannot judge, it takes.

use std::sync::Arc;

use nachalnik::{
    Budget, CompactionPlan, Compactor, Content, ContextItem, ContextKind, ContextState, async_trait,
};

/// Elides any tool result carrying a blob, then the oldest of the rest, once the context gets
/// full - and says so.
///
/// note: It does not summarize what it elided, and the note it leaves behind claims only that
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

#[async_trait]
impl Compactor for Trim {
    /// note: the threshold, *or* anything in the request the counter would not price. The second
    /// half is what makes taking blobs first worth anything: a context that is mostly pictures
    /// reports a handful of tokens, so the fraction never reaches the threshold, so `plan` is
    /// never called and the pass that would have taken them never runs. The one state in which
    /// this compactor is most needed was the one state it slept through.
    ///
    /// note: it is not a second threshold in disguise. `plan` still takes only what it may, and
    /// still answers `None` when there is nothing worth taking - so a context whose only
    /// unpriced item is pinned, or already elided, asks once and gets no plan, which is the
    /// same answer a pinned oversized result has always got.
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
        // to be read by both - what happened, and why
        let reason = format!(
            "compacted to make room; the context had reached {}% of the {}-token limit",
            (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize,
            budget.limit.unwrap_or_default(),
        );
        let marker = marker_tokens(&reason);

        // oldest first, because the results a conversation has moved past are the ones it is
        // least likely to want back.
        //
        // note: `sends_content` rather than `is_projected`, and the difference is the whole of
        // this compactor's behaviour once it has been round once. An elided item *is* projected -
        // as a marker - so with `is_projected` every item this pass had already elided came back
        // as a candidate on the next one. The plan was never empty, so it was never `None`, so a
        // summary went into the context before every single request from then on: a compactor
        // growing the context by a line and burning an undo per request, for as long as the
        // session lasted. It takes only a pinned file bigger than the target to get there
        //
        // note: and not a pinned one, which `sends_content` says yes to. The kernel refuses those
        // - a pin is a promise - so proposing one produces a plan that moves nothing, and a plan
        // is not nothing: it is a summary and an undo. One pinned result bigger than the target
        // keeps the context over the threshold for the rest of the session, so the pass is asked
        // again before every request and refuses again every time. Not naming what may not be
        // taken is what makes the plan `None` instead
        let candidates = items.iter().filter(|item| {
            item.state.sends_content()
                && item.state != ContextState::Pinned
                && matches!(item.kind, ContextKind::ToolResult { .. })
        });
        // blobs first and unconditionally, which is the one place this pass does not consult a
        // size. A picture is the largest thing in the context and the counter puts it at `0`, so
        // every arithmetic below would rank it last and then decline to take it at all - and a
        // compactor that skips the biggest item because it was told the item is free is not
        // making a decision, it is reporting one that was made by a gap in the estimate. Taken
        // first, before age is consulted: whatever a byte of prose is worth here, a megabyte of
        // base64 nothing can count is worth less
        let (blobs, rest): (Vec<_>, Vec<_>) =
            candidates.partition(|item| carries_blob(&item.content));

        let mut used = budget.used();
        let mut elide = Vec::new();
        let blob_count = blobs.len();
        for item in blobs {
            // the same arithmetic the loop below uses, and it is here for what it will be worth
            // later rather than for what it is worth now: today a blob is counted at `0`, so this
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
        if elide.is_empty() {
            return None;
        }

        // elided rather than removed, so that the call each of these answers keeps its answer.
        // Removing them would have the projector take the calls down as well - it has to, a call
        // with no result is a request most providers reject - and the model would then be reading
        // a conversation in which it never asked for any of this, directly above a note saying
        // the results had been dropped. The two accounts disagreed and the marker is the true one
        Some(CompactionPlan {
            // note: `elided`, which is the word the pane puts on the row, the word `amend`'s own
            // `prune` takes, and the word the runtime's state is called. It said `shortened to a
            // marker`, which is a third name for the thing - and a fourth mechanism away from
            // `truncated`, which is what an output limit does and is not this at all
            // note: the blobs get their own clause, and they need one. What the model is left
            // reading in place of an elided item is the pass's `reason`, which says the context
            // was full and says nothing about what used to be there - so a picture named
            // `[image/png, 12.05kB]` a moment ago becomes a sentence about a token limit, and
            // the model has no way left to know an image was ever in the conversation. A gap
            // where a picture was is worse than a sentence saying there was one; this is the
            // sentence, and it is the only place in the plan there is room for it
            summary: Some(ContextItem::summary(match blob_count {
                0 => format!(
                    "{} earlier tool result(s) were elided to make room: each is now a one-line \
                     marker where its content was. Ask again for anything you still need.",
                    elide.len()
                ),
                blobs => format!(
                    "{} tool result(s) were elided to make room, {blobs} of them carrying an \
                     image or other non-text payload: each is now a one-line marker where its \
                     content was. Ask again for anything you still need.",
                    elide.len()
                ),
            })),
            reason,
            elide,
            remove: Vec::new(),
        })
    }
}

/// Estimates what one elision marker costs: the pass's reason, in the brackets
/// [`LinearProjector`](nachalnik::LinearProjector) puts round it, at the four bytes a token
/// [`BytesPerToken`](nachalnik::BytesPerToken) assumes.
///
/// note: an estimate, and it does not have to be better than one - a counter that has learnt a
/// different ratio moves the boundary by one small result either way. What it has to be right
/// about is that a marker costs *something*, because crediting a pass with the whole of what it
/// elided is how one came to make the context 40% bigger than it found it: twenty `write`
/// confirmations, seven tokens each, replaced by twenty-one tokens of the same sentence, and the
/// arithmetic reported 140 tokens recovered while the request went from 852 to 1,190 - through
/// the 1,000-token limit the pass existed to keep it under.
///
/// note: taken from the reason rather than fixed, because the reason is what the marker says. A
/// constant here would be a second place to remember when that sentence is reworded.
fn marker_tokens(reason: &str) -> usize {
    // `[... ` and ` ...]`, which the projector supplies and this does not get to choose
    (reason.len() + 10).div_ceil(4)
}

/// Whether the content is a blob or has one somewhere inside it.
///
/// note: [`Content::blobs`] does the walking, and it is worth saying why this does not. The
/// nesting is the hard part - a sentence-and-a-screenshot turn is a blob one level down inside
/// `Content::Blocks` - and `Content` and `Block` are both `#[non_exhaustive]`, so a walk written
/// out here would answer `false` for a variant added later and a picture would quietly stop
/// going first, with no arm this file could have written to catch it. The runtime grew the seam
/// because its own counter needs the same walk for the same reason; this client should not be
/// keeping a second copy that goes stale on a day nobody is looking at this file.
fn carries_blob(content: &Content) -> bool {
    !content.blobs().is_empty()
}
