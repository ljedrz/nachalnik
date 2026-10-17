//! Compacts context, and reports exactly what it took.
//!
//! note: automatic context management is allowed; invisible context management is not. Every field
//! of [`CompactionReport`] exists so someone can be shown what happened, disagree with it, and put
//! it back. `refused` is where a compactor's attempt on a pinned item ends up.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::{Event, Kernel};
use crate::{
    context::{ContextId, ContextItem},
    model::Usage,
};

/// How much room the context is taking up, and how much there is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// The estimated tokens of the messages the context projects to - the items as they would be
    /// sent, reference labels and elision markers included.
    pub context_tokens: usize,
    /// The estimated tokens of the tool definitions that would be sent.
    pub tool_tokens: usize,
    /// How many pieces of content in the request the counter would not put a number on.
    ///
    /// note: what stops `context_tokens` above being a fiction. A counter that hits something it
    /// cannot price - a picture, for the default one - has to return *some* figure, and `0` is
    /// the only honest one available; the trouble is that `0` also means "measured, and free",
    /// so a budget reading 4,000 of 100,000 could be a context that is nearly empty or one
    /// carrying eight screenshots nobody has priced; this field tells the two apart. It is a count
    /// of pieces rather than of tokens, because a counter that could give tokens would not be
    /// abstaining.
    ///
    /// note: when this is not `0`, **the request is bigger than `used()` says** - and by an
    /// amount nothing here knows. A [`Compactor`] deserves to act on that, and
    /// [`Budget::fully_counted`] is the question to ask.
    pub uncounted: usize,
    /// The model's context limit, if the provider reports one.
    pub limit: Option<usize>,
    /// The token counts the provider reported for the most recent response, if it reported any.
    ///
    /// note: The figures above are the kernel's estimates, produced by a
    /// [`TokenCounter`](crate::TokenCounter) that does not know the model's tokenizer; this one is
    /// the truth as of the last request. They differ, sometimes by a lot: the default counter reads
    /// low, and a reasoning model's output tokens are largely invisible to it. A compactor deciding
    /// when to act needs both numbers. [`Calibrating`](crate::Calibrating) uses this one to correct
    /// the others.
    pub reported: Option<Usage>,
}

impl Budget {
    /// Returns the estimated size of the next request.
    pub fn used(&self) -> usize {
        self.context_tokens + self.tool_tokens
    }

    /// Whether every piece of the next request has a number on it.
    ///
    /// note: Ask this before believing [`Budget::used`] or [`Budget::fraction_used`]. When this is
    /// `false`, both are floors. A compactor that treats a floor as a measurement sits under its
    /// threshold while the real request runs past the limit. [`Budget::uncounted`] makes that
    /// failure visible; fixing it is a counter's job.
    pub fn fully_counted(&self) -> bool {
        self.uncounted == 0
    }

    /// Returns the fraction of the limit the next request would occupy, or `None` if the limit
    /// is unknown.
    ///
    /// note: a floor when [`Budget::fully_counted`] is `false`.
    pub fn fraction_used(&self) -> Option<f64> {
        self.limit
            .filter(|limit| *limit != 0)
            .map(|limit| self.used() as f64 / limit as f64)
    }
}

/// What a [`Compactor`] proposes doing about a [`Budget`].
#[derive(Debug, Clone, Default)]
pub struct CompactionPlan {
    /// The items to exclude from the projection.
    ///
    /// note: [`ContextState::Pinned`](crate::ContextState::Pinned) items in this list are
    /// refused by the kernel and reported in [`CompactionReport::refused`], and so is the other
    /// half of a pinned call and its result, since excluding either takes both out of the
    /// request. A pin is a promise.
    ///
    /// note: prefer [`CompactionPlan::elide`] for anything a tool call answers. Excluding a tool
    /// result takes the call down with it - the projector has no choice, since most providers
    /// reject a call with no result - so the model is left reading a conversation in which the
    /// call was never made, while a summary at the end says its result was removed. Those two
    /// accounts disagree, and the second one is right.
    pub remove: Vec<ContextId>,
    /// The items to keep in the projection, as a marker instead of their content.
    ///
    /// note: a message stays where it was and still answers what it answered, so the turn keeps its
    /// structure and only the content is gone. The marker the model reads is the item's note, which
    /// the kernel sets to [`CompactionPlan::reason`], so the reason is read by the model as well
    /// as by a person.
    ///
    /// note: pinned items are refused here for the same reason they are refused above: eliding an
    /// item takes its content just as excluding it does.
    pub elide: Vec<ContextId>,
    /// An item to add in their place, e.g. a summary.
    pub summary: Option<ContextItem>,
    /// Why this is being proposed, in words a user can read.
    pub reason: String,
}

/// An item a compaction pass accounted for: one it removed or elided, one the kernel refused, or
/// the summary it added. The [`CompactionReport`] field it is in shows which one.
///
/// note: `#[non_exhaustive]` on both this and [`CompactionReport`], because both are accounts of
/// what the kernel did and neither is built by a [`Compactor`]. A [`Compactor`] hands over a
/// [`CompactionPlan`] and is told the result, so the plan stays open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReportItem {
    /// The item's identifier; one taken out can be brought back with [`Kernel::set_state`].
    pub id: ContextId,
    /// The item's label.
    pub label: String,
    /// What it was costing.
    pub tokens: usize,
}

/// The old name of [`ReportItem`], from when the only items a report named were removed ones.
#[deprecated(note = "renamed to `ReportItem`")]
pub type Removed = ReportItem;

/// Exactly what a compaction pass did.
///
/// note: Automatic context management is allowed; invisible context management is not. Every field
/// here exists so the user can be shown what happened, disagree, and put something back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompactionReport {
    /// The items that were excluded.
    pub removed: Vec<ReportItem>,
    /// The items that were reduced to a marker, with what they were costing before.
    pub elided: Vec<ReportItem>,
    /// The items the kernel refused to remove because they are pinned, or are the call or the
    /// result a pinned item is paired with - once for each of the two lists that named one,
    /// however often that list named it.
    pub refused: Vec<ReportItem>,
    /// The summary item that was added, if any.
    pub summary: Option<ReportItem>,
    /// Why the pass happened.
    pub reason: String,
    /// The projected token total before: what a request made just before the pass would cost.
    ///
    /// note: projected rather than summed over the items; the marker accounts for the difference.
    /// An elided item stops sending its content and sends a line saying so instead, so the item sum
    /// credits the pass with everything it took away and nothing for what it put there. By that
    /// sum, a pass that made the request bigger would report a decrease. These two are counted the
    /// way [`Budget::context_tokens`] is, so a report can be checked against the budget that
    /// provoked it.
    pub tokens_before: usize,
    /// The projected token total after: what a request made now would cost, markers included.
    pub tokens_after: usize,
    /// How many pieces of the request before the pass the counter could not price, so
    /// `tokens_before` reads as a floor.
    ///
    /// note: the other half of [`Budget`]'s pair, because eliding an item the counter could not
    /// price makes the totals look like the request shrank by their difference, though the size is
    /// unknown. `serde(default)`, so a report recorded before these fields existed reads back as
    /// having priced everything, which is what it claimed.
    #[serde(default)]
    pub uncounted_before: usize,
    /// The same, for the request after the pass.
    #[serde(default)]
    pub uncounted_after: usize,
}

/// Optional, optionally automatic context management.
///
/// note: the kernel calls [`Compactor::should_compact`] before every request, applies whatever
/// [`Compactor::plan`] returns, and broadcasts an [`Event::Compacted`] describing it. It never
/// summarizes, drops or reorders anything on its own. With no compactor set
/// ([`Kernel::set_compactor`] with `None`), the context only changes when someone asks it to.
#[async_trait::async_trait]
pub trait Compactor: Send + Sync {
    /// Returns whether a compaction pass should be attempted now.
    ///
    /// note: This is called before every request, so it should be cheap.
    fn should_compact(&self, budget: &Budget) -> bool;

    /// Returns whether the context is too full for this compactor.
    ///
    /// note: the kernel asks this after a pass, to decide whether the context is full and the
    /// compactor will take nothing more ([`Event::ContextFull`](crate::Event::ContextFull)). It
    /// defaults to [`Compactor::should_compact`]. Override it where that asks for a pass for a
    /// reason other than room. A compactor that wants a look at something it cannot price wants a
    /// pass, but does not thereby find the context full.
    fn wants_room(&self, budget: &Budget) -> bool {
        self.should_compact(budget)
    }

    /// Returns what to do about the current context, or `None` to leave it alone.
    ///
    /// note: The items arrive in insertion order, with their states, labels, sizes and
    /// metadata - everything needed to make an informed choice, and nothing hidden.
    async fn plan(&self, items: &[Arc<ContextItem>], budget: &Budget) -> Option<CompactionPlan>;

    /// What this is, for a client that wants to say which one is installed.
    ///
    /// note: the default is the implementing type's own path, which costs an implementor nothing.
    /// Override it to say something friendlier. It is for showing a person, not for matching on:
    /// `type_name` makes no stability promise.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}
