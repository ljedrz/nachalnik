//! `Trim`: the compactor, which elides the oldest tool results and says exactly what it took.
//!
//! note: it refuses to elide anything smaller than the marker that would replace it, because an
//! elided item leaves behind a sentence carrying the reason for eliding it - and on a short result
//! that costs more than the content did. A compactor that did not check would watch the total
//! refuse to move and elide everything it had.

use std::sync::Arc;

use nachalnik::{
    Budget, CompactionPlan, Compactor, ContextItem, ContextKind, ContextState, async_trait,
};

/// Elides the oldest tool results once the context gets full, and says so.
///
/// note: It does not summarize what it elided, and the note it leaves behind claims only that
/// the content existed and is gone - a compactor that invented a paraphrase of output it never
/// read would be putting words in a tool's mouth. Every elision is reversible: the items keep
/// what they hold, they stay in the context pane with it counted as held back, and restoring one
/// is a keystroke. Anything pinned is refused by the kernel and reported as refused.
pub struct Trim {
    /// How full the context has to be before this bothers.
    pub threshold: f64,
    /// How empty it is trying to get it.
    pub target: f64,
}

#[async_trait]
impl Compactor for Trim {
    fn should_compact(&self, budget: &Budget) -> bool {
        budget
            .fraction_used()
            .is_some_and(|used| used >= self.threshold)
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

        let mut used = budget.used();
        let mut elide = Vec::new();
        for item in candidates {
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
            summary: Some(ContextItem::summary(format!(
                "{} earlier tool result(s) were elided to make room: each is now a one-line \
                 marker where its content was. Ask again for anything you still need.",
                elide.len()
            ))),
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
