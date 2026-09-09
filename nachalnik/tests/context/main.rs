//! Tests for the context: the state the user is supposed to be able to control.

// note: `tests/context/main.rs` rather than `tests/context.rs`, for the reason given in
// `tests/kernel/main.rs`: a directory with a `main.rs` in it is one test binary named for the
// directory, where a crate root's submodules would each be one of their own.

mod compaction;
mod items;
mod sending;
mod undo;

use nachalnik::{Config, ContextId, Event, Kernel, selectors::Selector};

/// Returns the events received so far.
pub(crate) fn drain(events: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<Event> {
    let mut received = Vec::new();
    while let Ok(event) = events.try_recv() {
        received.push(event);
    }

    received
}

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
