//! Tests for the loop: what gets sent, what gets run, and who decides.

// note: `tests/kernel/main.rs` rather than `tests/kernel.rs`: a crate root looks for its
// submodules beside itself, so the five below would each be a test binary of its own. A directory
// with a `main.rs` in it is one target named for the directory.
#[path = "../common/mod.rs"]
mod common;

mod components;
mod deciding;
mod record;
mod running;
mod sending;

use std::sync::Arc;

use nachalnik::{ContextItem, Kernel, selectors::Selector};

pub(crate) fn tool_results(kernel: &Kernel) -> Vec<Arc<ContextItem>> {
    "kind:tool_result"
        .parse::<Selector>()
        .unwrap()
        .matches(&kernel.items())
        .into_iter()
        .filter_map(|id| kernel.item(id))
        .collect()
}
