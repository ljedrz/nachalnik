//! Shared scaffolding for the integration tests.
#![allow(dead_code)]

use std::sync::Arc;

use nachalnik::{
    Config, Event, Kernel, ModelResponse,
    test::{AllowAll, ScriptedProvider},
};
use tokio::sync::broadcast::Receiver;

/// Creates a kernel that answers with the given responses and permits everything.
pub fn permissive(
    responses: impl IntoIterator<Item = ModelResponse>,
) -> (Kernel, Arc<ScriptedProvider>) {
    let (kernel, provider) = inquisitive(responses);
    kernel.set_policy(Arc::new(AllowAll));

    (kernel, provider)
}

/// Creates a kernel that answers with the given responses and asks about everything.
pub fn inquisitive(
    responses: impl IntoIterator<Item = ModelResponse>,
) -> (Kernel, Arc<ScriptedProvider>) {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(ScriptedProvider::new(responses));
    kernel.set_provider(provider.clone());

    (kernel, provider)
}

/// Returns the events received so far.
pub fn drain(events: &mut Receiver<Event>) -> Vec<Event> {
    let mut received = Vec::new();
    while let Ok(event) = events.try_recv() {
        received.push(event);
    }

    received
}

/// Returns the names of the events received so far.
pub fn names(events: &mut Receiver<Event>) -> Vec<String> {
    drain(events).iter().map(|e| e.name().to_owned()).collect()
}

/// Returns how many of the events have the given name.
pub fn count(events: &[Event], name: &str) -> usize {
    events.iter().filter(|e| e.name() == name).count()
}
