//! The tools an agent inspects and manages its own session with: one for its context, one for the
//! record kept beside it, one for what the session is running with, and one for asking a copy of
//! itself.
//!
//! note: Everything here is ordinary user code, like the rest of `tools/`, and none of it
//! needed a line added to the runtime. What the runtime has is a context that is a list of public
//! values, a request that can be built without being sent, and a session that can be snapshotted
//! and resumed - and a tool is allowed to call all of it. That is the whole trick: introspection
//! is not a feature of the kernel, it is what a tool can already do with the kernel's ordinary
//! surface. What they add is the part the kernel has no opinion about: which of it a *model*
//! may do.
//!
//! note: a tool per noun, and the noun is what the tool is *about* rather than what it does to
//! it. [`Context`] is the context - reading it and changing it, twelve operations over one object.
//! [`Log`] is the record beside it, [`Setup`] is what the session is running with, and [`Fork`]
//! is a copy of this session standing up to answer something.
//!
//! note: reading and changing were two tools, on the argument that a [`nachalnik::ToolSpec`]
//! declares its capabilities once, so one tool would mean answering *always* to "may it look at
//! its own items?" also answered "may it rewrite a tool result?" That hazard is real and it is no
//! longer this file's: a subject is `<domain>:<operation>` and [`nachalnik::Tool::needs`] lets a
//! call declare which one it is, so `context:look` and `context:revise` are separate rows on the
//! permissions tab whether they arrive under one tool's name or two. [`Fork`] went the other way
//! for the same reason - it was two actions of `context` and it is neither a reading nor a change,
//! it is a second session and a bill.
//!
//! note: which also makes each of them separately *revocable*, and that is not a side effect worth
//! designing away. A session in which the agent's ability to check the record is taken back
//! half way through is a thing this program can do, and a thing worth watching a model in.
//!
//! note: What [`Context`] will not do is undo a person's decisions. A pinned item, a system
//! instruction, and the assistant turn carrying the call being executed are all refused, with the
//! reason handed back to the model. The agent is not the boss.

use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use nachalnik::{
    BoxError, ContextId, ContextItem, ContextKind, ContextState, Kernel, selectors::Selector,
};
use serde_json::{Value, json};

use crate::tools::{Careful, Limits};

mod context;
mod fork;
mod log;
mod setup;

pub use crate::introspect::{context::Context, fork::Fork, log::Log, setup::Setup};

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

    kernel.add_tool(Arc::new(Context::new(reach.clone(), limits.clone())));
    kernel.add_tool(Arc::new(Fork::new(reach.clone(), limits.clone())));
    kernel.add_tool(Arc::new(Log::new(reach.clone(), limits.clone())));
    kernel.add_tool(Arc::new(Setup::new(reach, policy, limits)));

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

// ------------------------------------------------------------------------------------ helpers

/// A sentence pointing at a sibling tool, or nothing if that tool is not on offer.
///
/// note: bought by a live run. `/tools toggle log` took the log away mid-session, and `setup tools`
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

/// The context ids in a named array argument, or what is wrong with what is in it.
///
/// note: it used to drop whatever it could not read. `ids: [-1]` came back as no items at all,
/// which is also how a call that named none arrives - so `search` with one bad number searched
/// the whole context, and a call giving `ids` *and* `select` went through as a `select`, the
/// refusal below having found no numbers to object to. A number that is not an item number is a
/// call nobody made, the same as an argument nobody reads.
///
/// note: and the same number twice is one item. It is not a mistake worth refusing - a model
/// gathering ids from two places writes it - but it was counted twice in what a move reported,
/// so a call naming one item was told two had moved.
fn ids(args: &Value, name: &str) -> Result<Vec<ContextId>, String> {
    if args[name].is_null() {
        return Ok(Vec::new());
    }
    let Some(given) = args[name].as_array() else {
        return Err(format!(
            "`{name}` is `{}`, and nothing was done. It is a list of the numbers `look` prints, \
             so one item is `{name}: [{}]`.",
            args[name],
            args[name].as_u64().unwrap_or(7)
        ));
    };

    let mut ids: Vec<ContextId> = Vec::new();
    for value in given {
        let Some(id) = value.as_u64().map(ContextId) else {
            return Err(format!(
                "`{name}` holds `{value}`, which is not an item number, and nothing was done. \
                 They are the numbers `look` prints, as whole numbers."
            ));
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }

    Ok(ids)
}

/// Which items a call named, and which of the two ways it named them.
pub(crate) struct Named<'a> {
    /// The selector as it was written, where the call named a class rather than numbers.
    pub(crate) select: Option<&'a str>,
    /// The items it comes to, either way.
    pub(crate) ids: Vec<ContextId>,
}

/// Reads the two arguments that say which items, and refuses a call that gives both.
///
/// note: `select` used to win and `ids` was dropped without a word. That is the failure
/// [`crate::tools::ops::inner`] refuses one level out, where a call puts arguments inside the
/// wrapper and beside it, and it is the same failure for the same reason: whichever of the two is
/// read, the answer comes back as an ordinary one, and nothing in it says the other half was never
/// looked at. `elide` with `ids: [4]` and `select: "all:tool_results"` moves whatever the selector
/// matched, reports exactly that, and reads as a call that did what it was told.
///
/// note: refused here rather than said in the schema, because the schema cannot say it. Mutual
/// exclusion is `oneOf` or `not`, and neither is a keyword both dialects take - Google's `Schema`
/// is a closed set of fields and has neither. `anyOf` does not say it either: a branch per argument
/// still matches a call carrying both, since nothing in it is exclusive.
pub(crate) fn named<'a>(items: &[Arc<ContextItem>], args: &'a Value) -> Result<Named<'a>, String> {
    let numbers = ids(args, "ids")?;
    let Some(input) = args["select"].as_str().filter(|it| !it.trim().is_empty()) else {
        return Ok(Named {
            select: None,
            ids: numbers,
        });
    };

    // note: whether `ids` was *given*, rather than whether it came to anything. `ids: []` beside a
    // selector is the same call as any other that names items twice, and it used to be read as a
    // selector on its own
    if !args["ids"].is_null() {
        return Err(format!(
            "`ids` and `select` in one call, and nothing was done. They are two ways of saying \
             which items - `ids: {}` is those by number, `select: \"{input}\"` is a class of them \
             - and reading one of the two would have answered a call you did not make. Send \
             whichever you meant{}.",
            serde_json::Value::Array(numbers.iter().map(|id| json!(id.0)).collect::<Vec<_>>()),
            match numbers.first() {
                Some(id) => format!(
                    "; a selector takes an item number too, so `select: \"{}\"` is that one item",
                    id.0
                ),
                None => String::new(),
            }
        ));
    }

    match input.parse::<Selector>() {
        Ok(selector) => Ok(Named {
            select: Some(input),
            ids: selector.matches(items),
        }),
        Err(e) => Err(format!(
            "`{input}` is not a selector: {e}\n\n{}",
            crate::help::SELECTORS
        )),
    }
}

/// The pins the model made itself, and the note each was made with.
///
/// note: the note because an identifier alone went stale. It was only ever written by the model's
/// own moves, so an item the model pinned and the person then unpinned and pinned again was still
/// on the list - and the model could take the person's pin off. The person's pins carry no note and
/// the model's carry its reason, so a pin is the model's while the item still says what the model
/// wrote.
type Mine = BTreeMap<ContextId, Option<String>>;

/// Why this item is not the model's to change, if it is not.
fn protected(item: &ContextItem, mine: &Mine, own_turn: Option<ContextId>) -> Option<String> {
    if matches!(item.kind, ContextKind::System) {
        return Some("a system instruction, which belongs to whoever started this session".into());
    }
    if item.state == ContextState::Pinned && mine.get(&item.id) != Some(&item.note) {
        return Some("pinned by the person you are working with, and a pin is a promise".into());
    }
    if own_turn == Some(item.id) {
        return Some("the assistant turn this very call is part of".into());
    }

    None
}
