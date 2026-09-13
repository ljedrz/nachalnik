//! The tools an agent inspects and manages its own session with: one that reads its context, one
//! that reads the record kept beside it, one that reads what the session is running with, and one
//! that changes the first of them.
//!
//! note: Everything here is ordinary user code, like the rest of `tools.rs`, and none of it
//! needed a line added to the runtime. What the runtime has is a context that is a list of public
//! values, a request that can be built without being sent, and a session that can be snapshotted
//! and resumed - and a tool is allowed to call all of it. That is the whole trick: introspection
//! is not a feature of the kernel, it is what a tool can already do with the kernel's ordinary
//! surface. What they add is the part the kernel has no opinion about: which of it a *model*
//! may do.
//!
//! note: a tool per noun rather than one with an `action` argument, because a
//! [`nachalnik::ToolSpec`] declares its capabilities once for every call it will ever receive. One
//! tool would mean that answering *always* to "may it look at its own context?" also answered "may
//! it rewrite a tool result?" - a grant that delivers considerably more than it implies, which is
//! the shape of thing this program exists not to do. So [`Context`] reads the context, [`Log`]
//! reads the record beside it, [`Setup`] reads what the session is running with and [`Amend`]
//! changes the context; they declare different capabilities, and the permissions tab has a row
//! for each.
//!
//! note: which also makes each of them separately *revocable*, and that is not a side effect worth
//! designing away. A session in which the agent's ability to check the record is taken back
//! half way through is a thing this program can do, and a thing worth watching a model in.
//!
//! note: What [`Amend`] will not do is undo a person's decisions. A pinned item, a system
//! instruction, and the assistant turn carrying the call being executed are all refused, with the
//! reason handed back to the model. The agent is not the boss.

use std::{
    collections::BTreeSet,
    sync::{Arc, Weak},
};

use nachalnik::{BoxError, ContextId, ContextItem, ContextKind, ContextState, Kernel};
use parking_lot::Mutex;
use serde_json::Value;

use crate::tools::{Careful, Limits};

mod amend;
mod context;
mod log;
mod setup;

pub use crate::introspect::{amend::Amend, context::Context, log::Log, setup::Setup};

/// Registers the tools, and returns the handle that keeps their reach into the kernel alive.
///
/// note: the policy is handed in rather than read off the kernel, and it is the concrete one this
/// program ships rather than the trait. [`Setup`] reports what the policy will say *before*
/// anything is asked, and the only way to get that through [`nachalnik::PermissionPolicy`] is to
/// run a call through `evaluate` - which [`Careful`] answers by recording the reason it refused,
/// into a queue of sixty-four that a real refusal is going to read.
///
/// note: so a tool that reports state would be writing some, and evicting the explanations a
/// genuine refusal needs. It reads the same two lists the permissions tab draws instead. What it
/// cannot read it says so about: the verdicts are labelled as this policy's, beside the name of
/// whichever one the kernel is actually consulting.
///
/// note: the return value is load-bearing rather than informational, and dropping it is how the
/// tools are switched off: they hold a [`Weak`] to it, and a tool that cannot upgrade its weak
/// handle refuses the call and says why. That indirection is not decoration either. A `Kernel`
/// stored inside a `Tool` the same kernel holds is a reference cycle that keeps the whole session
/// alive after the last handle to it is gone, which the runtime's own documentation warns about;
/// an [`Arc`] somebody *else* owns, pointed at weakly from in here, is the shape that has an end.
pub fn install(kernel: &Kernel, policy: Arc<Careful>, limits: Limits) -> Arc<Kernel> {
    let anchor = Arc::new(kernel.clone());
    let reach = Reach(Arc::downgrade(&anchor));
    // shared, because these tools are one agent's hands: what `amend` pinned is what
    // `context` should report as the agent's own to unpin, and a second set would have them
    // disagreeing about a promise
    let pinned = Arc::new(Mutex::new(BTreeSet::new()));

    kernel.add_tool(Arc::new(Context::new(
        reach.clone(),
        pinned.clone(),
        limits.clone(),
    )));
    kernel.add_tool(Arc::new(Log::new(reach.clone(), limits.clone())));
    kernel.add_tool(Arc::new(Setup::new(reach.clone(), policy, limits.clone())));
    kernel.add_tool(Arc::new(Amend::new(reach, pinned, limits)));

    anchor
}

/// The way back to the kernel a tool is registered on.
#[derive(Clone)]
struct Reach(Weak<Kernel>);

impl Reach {
    /// The kernel, or the reason there is not one any more.
    fn kernel(&self) -> Result<Arc<Kernel>, BoxError> {
        self.0.upgrade().ok_or_else(|| {
            "this session is over; there is nothing left to look at or change".into()
        })
    }
}

/// The items the agent pinned itself, shared between the tool that sets them and the one that
/// reports them.
type Pinned = Arc<Mutex<BTreeSet<ContextId>>>;

// ------------------------------------------------------------------------------------ helpers

/// A sentence pointing at a sibling tool, or nothing if that tool is not on offer.
///
/// note: bought by a live run. `/tools drop log` took the log away mid-session, and `setup tools`
/// went on ending with "`log` with `kinds: [\"tools.changed\"]` says when it went" - advice naming
/// a tool the model had just been told it does not have. It is the same rule a refusal follows:
/// name only what the session can actually reach, because everything named in an answer is read as
/// something to try. These are registered tools rather than a fixed set, so the check is the
/// registry and the sentence simply goes when its subject does.
pub(crate) fn if_offered(kernel: &Kernel, tool: &str, said: impl FnOnce() -> String) -> String {
    match kernel.tool(tool).is_some() {
        true => said(),
        false => String::new(),
    }
}

/// The action the call names, or the fact that it names none.
fn action(args: &Value) -> Result<&str, BoxError> {
    args["action"]
        .as_str()
        .ok_or_else(|| "the `action` argument is required".into())
}

/// What to say about an action nobody implements.
fn unknown(action: &str, known: &[&str]) -> String {
    format!(
        "there is no `{action}`; this tool does {}",
        known.join(", ")
    )
}

/// The context ids in a named array argument.
fn ids(args: &Value, name: &str) -> Vec<ContextId> {
    args[name]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_u64())
                .map(ContextId)
                .collect()
        })
        .unwrap_or_default()
}

/// Why this item is not the model's to change, if it is not.
fn protected(
    item: &ContextItem,
    mine: &BTreeSet<ContextId>,
    own_turn: Option<ContextId>,
) -> Option<String> {
    if matches!(item.kind, ContextKind::System) {
        return Some("a system instruction, which belongs to whoever started this session".into());
    }
    if item.state == ContextState::Pinned && !mine.contains(&item.id) {
        return Some("pinned by the person you are working with, and a pin is a promise".into());
    }
    if own_turn == Some(item.id) {
        return Some("the assistant turn this very call is part of".into());
    }

    None
}
