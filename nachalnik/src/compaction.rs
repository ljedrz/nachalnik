//! Compacts context, and reports exactly what it took.
//!
//! note: automatic context management is allowed; invisible management is not. Every field of
//! [`CompactionReport`] exists so someone can see what happened, disagree, and put it back; a
//! compactor's attempt on a pinned item ends up in `refused`.

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
    /// note: without this, `context_tokens` could be fiction. A counter that cannot price something
    /// (a picture, for the default one) must return some figure, and `0` is the only honest one,
    /// but `0` also means "measured, and free": 4,000 of 100,000 could be a nearly empty context or
    /// one carrying eight unpriced screenshots. This tells the two apart. It counts pieces, not
    /// tokens, because a counter that could give tokens would not be abstaining.
    ///
    /// note: when this is not `0`, **the request is bigger than `used()` says**, by an amount
    /// nothing here knows. A [`Compactor`] should act on that; [`Budget::fully_counted`] asks.
    pub uncounted: usize,
    /// The model's context limit, if the provider reports one.
    pub limit: Option<usize>,
    /// The token counts the provider reported for the most recent response, if it reported any.
    ///
    /// note: the figures above are the kernel's estimates, from a
    /// [`TokenCounter`](crate::TokenCounter) that does not know the model's tokenizer; this is the
    /// provider's count as of the last request. They can differ a lot: the default counter reads
    /// low, and a reasoning model's output tokens are largely invisible to it. A compactor deciding
    /// when to act needs both, and [`Calibrating`](crate::Calibrating) corrects the estimates with
    /// this one.
    pub reported: Option<Usage>,
}

impl Budget {
    /// Returns the estimated size of the next request.
    pub fn used(&self) -> usize {
        self.context_tokens + self.tool_tokens
    }

    /// Whether every piece of the next request has a number on it.
    ///
    /// note: ask this before believing [`Budget::used`] or [`Budget::fraction_used`]: when it is
    /// `false`, both are floors, and a compactor treating a floor as a measurement sits under its
    /// threshold while the real request passes the limit. [`Budget::uncounted`] makes that visible;
    /// fixing it is a counter's job.
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
    /// note: [`ContextState::Pinned`](crate::ContextState::Pinned) items in this list are refused
    /// by the kernel and reported in [`CompactionReport::refused`], as is the other half of a
    /// pinned call and its result, since excluding either takes both out of the request. A pin is a
    /// promise.
    ///
    /// note: prefer [`CompactionPlan::elide`] for anything a tool call answers. Excluding a tool
    /// result takes the call down with it (most providers reject a call with no result), so the
    /// model reads a conversation in which the call was never made beside a summary saying its
    /// result was removed.
    pub remove: Vec<ContextId>,
    /// The items to keep in the projection, as a marker instead of their content.
    ///
    /// note: the message keeps its place and still answers what it answered, so only the content is
    /// gone. The marker the model reads is the item's note, which the kernel sets to
    /// [`CompactionPlan::reason`], so the model reads the reason as well as a person.
    ///
    /// note: pinned items are refused here too, because eliding takes an item's content just as
    /// excluding does.
    pub elide: Vec<ContextId>,
    /// An item to add in their place, e.g. a summary.
    pub summary: Option<ContextItem>,
    /// Why this is being proposed, in words a user can read.
    pub reason: String,
}

/// An item a compaction pass accounted for: one it removed or elided, one the kernel refused, or
/// the summary it added. The [`CompactionReport`] field it is in shows which one.
///
/// note: `#[non_exhaustive]` on this and [`CompactionReport`] because both are accounts of what the
/// kernel did, never built by a [`Compactor`]. A compactor builds a [`CompactionPlan`] and is told
/// the result, so the plan stays open to construct.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReportItem {
    /// The item's identifier; one taken out can be brought back with [`Kernel::set_state`].
    pub id: ContextId,
    /// The item's label.
    pub label: String,
    /// The item's size, its [`ContextItem::tokens`]: what it costs when sent whole.
    ///
    /// note: not always what it was costing. An item already elided was costing only its marker, so
    /// the row of one that was then removed is more than removing it saved.
    /// [`CompactionReport::tokens_before`] and [`CompactionReport::tokens_after`] say what the
    /// pass saved.
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
    /// note: projected, not summed over the items, and the marker is the difference: an elided item
    /// sends a line instead of its content, so the item sum credits a pass with everything it took
    /// and nothing for what it added, and a pass that made the request bigger could report a
    /// decrease. Both totals are counted as [`Budget::context_tokens`] is, so a report can be
    /// checked against the budget that provoked it.
    pub tokens_before: usize,
    /// The projected token total after: what a request made now would cost, markers included.
    pub tokens_after: usize,
    /// How many pieces of the request before the pass the counter could not price, so
    /// `tokens_before` reads as a floor.
    ///
    /// note: the other half of [`Budget`]'s pair, because eliding an item the counter could not
    /// price makes the totals look as if the request shrank by their difference, when by how much
    /// is unknown. `serde(default)`, so an older report reads as having priced everything, which is
    /// what it claimed.
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
    /// compactor will take no more ([`Event::ContextFull`](crate::Event::ContextFull)). It defaults
    /// to [`Compactor::should_compact`]; override it where that asks for a pass for a reason other
    /// than room, such as wanting a look at something it cannot price.
    fn wants_room(&self, budget: &Budget) -> bool {
        self.should_compact(budget)
    }

    /// Returns what to do about the current context, or `None` to leave it alone.
    ///
    /// note: The items arrive in insertion order, with their states, labels, sizes and metadata.
    async fn plan(&self, items: &[Arc<ContextItem>], budget: &Budget) -> Option<CompactionPlan>;

    /// What this is, for a client that wants to say which one is installed.
    ///
    /// note: the default is the implementing type's path; override it to say something friendlier.
    /// It is for showing a person, not for matching on: `type_name` makes no stability promise.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}
