//! Tests for the context: the state the user is supposed to be able to control.

// note: `tests/context/main.rs` rather than `tests/context.rs`, for the reason given in
// `tests/kernel/main.rs`: a directory with a `main.rs` in it is one test binary named for the
// directory, where a crate root's submodules would each be one of their own.
#[path = "../common/mod.rs"]
mod common;

mod compaction;
mod items;
mod sending;
mod undo;

use nachalnik::{Config, ContextId, Kernel, selectors::Selector};

pub(crate) fn sel(input: &str) -> Selector {
    input.parse().unwrap()
}

pub(crate) fn kernel() -> Kernel {
    Kernel::new(Config::default())
}

/// Resolves a selector the way a client would.
pub(crate) fn select(kernel: &Kernel, input: &str) -> Vec<ContextId> {
    sel(input).matches(&kernel.items())
}
