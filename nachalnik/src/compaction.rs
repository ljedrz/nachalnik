use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::{Context, Event, Kernel};
use crate::{
    context::{ContextId, ContextItem},
    model::Usage,
};

/// How much room the context is taking up, and how much there is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// The estimated tokens of the items that would be sent.
    pub context_tokens: usize,
    /// The estimated tokens of the tool definitions that would be sent.
    pub tool_tokens: usize,
    /// The model's context limit, if the provider reports one.
    pub limit: Option<usize>,
    /// The token counts the provider reported for the most recent response, if it reported any.
    ///
    /// note: The figures above are the kernel's estimates, produced by a
    /// [`TokenCounter`](crate::TokenCounter) that does not know the model's tokenizer; this one
    /// is the truth as of the last request. They differ, sometimes by a lot - measured against a
    /// small model over a real API, the default counter came out roughly a third low, and a
    /// reasoning model's output tokens are largely invisible to it. A compactor deciding when to
    /// act deserves both numbers - and [`Calibrating`](crate::Calibrating) is the counter that
    /// uses this one to correct the others.
    pub reported: Option<Usage>,
}

impl Budget {
    /// Returns the estimated size of the next request.
    pub fn used(&self) -> usize {
        self.context_tokens + self.tool_tokens
    }

    /// Returns the fraction of the limit the next request would occupy, or `None` if the limit
    /// is unknown.
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
    /// refused by the kernel and reported in [`CompactionReport::refused`]. A pin is a promise.
    pub remove: Vec<ContextId>,
    /// An item to add in their place, e.g. a summary.
    pub summary: Option<ContextItem>,
    /// Why this is being proposed, in words a user can read.
    pub reason: String,
}

/// An item a compaction pass removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Removed {
    /// The item's identifier; it can be brought back with [`Kernel::set_state`].
    pub id: ContextId,
    /// The item's label.
    pub label: String,
    /// What it was costing.
    pub tokens: usize,
}

/// Exactly what a compaction pass did.
///
/// note: Automatic context management is allowed; *invisible* context management is not, and
/// every field here exists so that the user can be shown what happened, disagree, and put
/// something back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionReport {
    /// The items that were excluded.
    pub removed: Vec<Removed>,
    /// The items the kernel refused to remove because they are pinned.
    pub refused: Vec<Removed>,
    /// The summary item that was added, if any.
    pub summary: Option<Removed>,
    /// Why the pass happened.
    pub reason: String,
    /// The projected token total before.
    pub tokens_before: usize,
    /// The projected token total after.
    pub tokens_after: usize,
}

/// Optional, and optionally automatic, context management.
///
/// note: The kernel calls [`Compactor::should_compact`] before every request and applies
/// whatever [`Compactor::plan`] returns, then broadcasts an [`Event::Compacted`] describing it.
/// It never summarizes, drops or reorders anything of its own accord: with no compactor set
/// ([`Kernel::set_compactor`] with `None`), the context only ever changes because someone asked
/// it to.
#[async_trait::async_trait]
pub trait Compactor: Send + Sync {
    /// Returns whether a compaction pass should be attempted now.
    ///
    /// note: This is called before every request, so it should be cheap.
    fn should_compact(&self, budget: &Budget) -> bool;

    /// Returns what to do about the current context, or `None` to leave it alone.
    ///
    /// note: The items arrive in insertion order, with their states, labels, sizes and
    /// metadata - everything needed to make an informed choice, and nothing hidden.
    async fn plan(&self, items: &[Arc<ContextItem>], budget: &Budget) -> Option<CompactionPlan>;

    /// What this is, for a client that wants to say which one is installed.
    ///
    /// note: The default is the implementing type's own path, which costs an implementor nothing
    /// and is right often enough to be worth having. Override it to say something friendlier. It
    /// is for showing a person, not for matching on: `type_name` makes no stability promise.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}
