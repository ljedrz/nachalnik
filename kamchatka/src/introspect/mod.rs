//! Tools an agent uses to inspect and manage its session: one for its context, one for the record
//! beside it, one for what the session runs with, and one for asking a copy of itself.
//!
//! note: everything here is ordinary user code, like the rest of `tools/`; none of it needed a line
//! added to the runtime. The runtime has a context that is a list of public values, a request that
//! can be built without being sent, and a session that can be snapshotted and resumed, and a tool
//! may call all of it. Introspection is what a tool can already do with the kernel's ordinary
//! surface, not a kernel feature; these tools add which of it a model may do.
//!
//! note: one tool per noun, and the noun is what the tool is about rather than what it does to it.
//! [`Context`] is the context: reading and changing it, twelve operations over one object. [`Log`]
//! is the record beside it. [`Setup`] is what the session runs with. [`Fork`] is a copy of this
//! session.
//!
//! note: reading and changing are one tool, for the reason the `context` module gives. [`Fork`] is
//! its own tool: neither a reading nor a change, but a second session and a cost.
//!
//! note: each is also separately revocable, by design. A session can take back the agent's ability
//! to check the record halfway through, and how a model behaves then is worth watching.
//!
//! note: [`Context`] will not undo a person's decisions. A pinned item, a system instruction, and
//! the assistant turn carrying the call being executed are all refused, with the reason handed back
//! to the model.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use nachalnik::{
    BoxError, ContextId, ContextItem, ContextKind, ContextState, Kernel, selectors::Selector,
};
use serde_json::{Value, json};

use crate::{
    tools::{Careful, Layout, Limits},
    wiring::Earlier,
};

mod context;
mod fork;
mod log;
mod setup;

pub use crate::introspect::{context::Context, fork::Fork, log::Log, setup::Setup};

/// Registers the tools, and returns the handle that keeps their reach into the kernel alive.
///
/// note: the policy is handed in rather than read off the kernel, and is the concrete one this
/// program ships rather than the trait. [`Setup`] reports what the policy will say before anything
/// is asked; through [`nachalnik::PermissionPolicy`] that would mean running a call through
/// `evaluate`, which [`Careful`] answers by recording a refusal, so it reads [`Careful`]'s own
/// tables instead. Its `permissions` says what that costs against a real refusal, and how the
/// answer names the policy the kernel is actually consulting.
///
/// note: dropping the return value switches the tools off: they hold a [`Weak`] to the kernel, and
/// a tool that cannot upgrade its weak handle refuses the call and says why. The indirection avoids
/// a reference cycle: a `Kernel` stored inside a `Tool` the same kernel holds keeps the whole
/// session alive after the last handle is gone, which the runtime's documentation warns about. An
/// [`Arc`] owned by something else, pointed at weakly from here, ends the cycle.
///
/// note: `layout` is how all four lay out their arguments, the layout `fs` and `shell` were given.
/// A model offered two layouts in one request writes one of them wrong.
pub fn install(kernel: &Kernel, policy: Arc<Careful>, limits: Limits, layout: Layout) -> Installed {
    let anchor = Arc::new(kernel.clone());
    let reach = Reach(Arc::downgrade(&anchor), policy.clone());
    let forked = Arc::new(AtomicU64::new(0));
    let history = History::default();
    let earlier = Earlier::default();

    kernel.add_tool(Arc::new(Context::new(
        reach.clone(),
        limits.clone(),
        layout,
        history.0.clone(),
    )));
    kernel.add_tool(Arc::new(Fork::new(
        reach.clone(),
        limits.clone(),
        forked.clone(),
        layout,
    )));
    kernel.add_tool(Arc::new(Log::new(
        reach.clone(),
        limits.clone(),
        earlier.clone(),
        layout,
    )));
    kernel.add_tool(Arc::new(Setup::new(reach, policy, limits, layout)));

    Installed {
        _anchor: anchor,
        forked,
        history,
        earlier,
    }
}

/// What [`install`] hands back: the handle the tools reach the kernel through, and what their forks
/// have cost.
///
/// note: a fork is a kernel of its own, so what it is charged is in no event of this session's,
/// and a ceiling read off this session's events alone would never see it. The fork adds what its
/// provider reported here, and [`Installed::forked`] is how whoever keeps the ceiling counts it.
pub struct Installed {
    _anchor: Arc<Kernel>,
    forked: Arc<AtomicU64>,
    history: History,
    earlier: Earlier,
}

impl Installed {
    /// What forks have been charged since this was last asked, as their provider reported it.
    ///
    /// note: taken rather than read, so that a figure is counted once however often it is asked.
    pub fn forked(&self) -> u64 {
        self.forked.swap(0, Ordering::Relaxed)
    }

    /// The `context` tool's own undo history; see [`History`].
    pub fn history(&self) -> History {
        self.history.clone()
    }

    /// What the `log` tool reads before the kernel's own records; see [`Earlier`].
    pub fn earlier(&self) -> Earlier {
        self.earlier.clone()
    }
}

/// The changes the `context` tool made and can undo with its own `undo`. A snapshot carries them so
/// a resumed session's model can still undo them.
///
/// note: the runtime's snapshot has no place for one tool's memory and should not; it stores the
/// session. The history is written to the snapshot file beside the runtime's fields under
/// [`HISTORY`]. The runtime does not read [`HISTORY`]; resuming here reads it back. An entry read
/// back follows the same rule as a live one: an item changed since it was recorded is left alone.
#[derive(Clone, Default)]
pub struct History(Arc<parking_lot::Mutex<context::Journal>>);

impl std::fmt::Debug for History {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("History").finish_non_exhaustive()
    }
}

/// The key a snapshot's file holds the [`History`] under.
pub const HISTORY: &str = "kamchatka-context-history";

impl History {
    /// What to write into a snapshot's file under [`HISTORY`].
    pub fn saved(&self) -> Value {
        serde_json::to_value(&*self.0.lock()).unwrap_or(Value::Null)
    }

    /// Takes on what a snapshot's file held under [`HISTORY`], in place of what is here.
    pub fn carry(&self, saved: &Value) -> Result<(), String> {
        let mut journal: context::Journal = serde_json::from_value(saved.clone())
            .map_err(|e| format!("the `context` tool's undo history could not be read: {e}"))?;
        journal.carried = true;
        *self.0.lock() = journal;

        Ok(())
    }
}

/// What a snapshot's file holds under [`HISTORY`], where it holds anything there.
pub fn saved_history(path: &std::path::Path) -> Option<Value> {
    let written: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;

    written.get(HISTORY).cloned()
}

/// A snapshot as the bytes of its file, with the `context` tool's history beside the runtime's
/// fields where there is one.
pub fn snapshot_with(
    snapshot: &nachalnik::Snapshot,
    history: Option<&History>,
) -> Result<Vec<u8>, String> {
    let mut written =
        serde_json::to_value(snapshot).map_err(|e| format!("could not render the session: {e}"))?;
    if let (Some(history), Value::Object(fields)) = (history, &mut written) {
        fields.insert(HISTORY.to_owned(), history.saved());
    }

    serde_json::to_vec_pretty(&written).map_err(|e| format!("could not render the session: {e}"))
}

/// The way back to the kernel a tool is registered on, and the policy its calls are judged by.
#[derive(Clone)]
struct Reach(Weak<Kernel>, Arc<Careful>);

impl Reach {
    /// The kernel, or the reason there is not one any more.
    fn kernel(&self) -> Result<Arc<Kernel>, BoxError> {
        self.0.upgrade().ok_or_else(|| {
            "this session is over; there is nothing left to look at or change".into()
        })
    }

    /// The policy, which says what of what an answer could point at the model could use now.
    fn policy(&self) -> &Careful {
        &self.1
    }
}

// ------------------------------------------------------------------------------------ helpers

/// A sentence pointing at a sibling tool, or nothing if that tool is not on offer.
///
/// note: everything named in an answer is read as something to try, so an answer names only what
/// the session can reach, and a refusal follows the same rule. Otherwise a session that removed
/// `log` with `/tools toggle log` would still have `setup tools` point at `log`, a tool the model
/// has just been told it does not have. Tools are registered, not a fixed set, so the check reads
/// the registry and the sentence disappears when its subject does.
pub(crate) fn if_offered(kernel: &Kernel, tool: &str, said: impl FnOnce() -> String) -> String {
    match kernel.tool(tool).is_some() {
        true => said(),
        false => String::new(),
    }
}

/// A sentence sending the model to `capability` - `log:read`, `context:move` - or nothing if
/// the model could not use it now: no tool on offer declares it, or it would be refused.
///
/// note: [`if_offered`] one level further. A tool on offer whose every call a rule refuses, or
/// that a headless run refuses for want of somebody to ask, is no more something to try than one
/// that was taken away; see [`Careful::reachable`]. The tool naming its own operations is no
/// exception, since a rule may be about one operation and not the others. What is on offer is
/// asked of the kernel's registry, which is what a tool taken away by any door leaves.
pub(crate) fn if_reachable(
    kernel: &Kernel,
    policy: &Careful,
    capability: &str,
    said: impl FnOnce() -> String,
) -> String {
    match reachable(kernel, policy, capability) {
        true => said(),
        false => String::new(),
    }
}

/// Whether the model could use `capability` now; see [`if_reachable`].
pub(crate) fn reachable(kernel: &Kernel, policy: &Careful, capability: &str) -> bool {
    let Ok(capability) = nachalnik::Capability::parse(capability) else {
        return false;
    };
    let offered = kernel
        .tool_specs()
        .iter()
        .any(|spec| spec.capabilities.contains(&capability));

    offered && policy.permits(&capability)
}

/// The action the call names, or the fact that it names none or names it with something else.
///
/// note: one that is not a string is refused as what it is, for the reason `tools::arg` gives.
fn action(args: &Value) -> Result<&str, BoxError> {
    match &args["action"] {
        Value::String(named) => Ok(named),
        Value::Null => Err("the `action` argument is required".into()),
        other => Err(format!(
            "`action` is one word, and this one is {}; nothing was done",
            crate::tools::what(other)
        )
        .into()),
    }
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
/// note: a value it cannot read is refused rather than dropped. Dropped, `ids: [-1]` would come
/// back as no items at all, which is also how a call that named none arrives, so `search` with one
/// bad number would search the whole context, and a call giving `ids` *and* `select` would go
/// through as a `select`, the refusal below having found no numbers to object to. A number that is
/// not an item number is a call nobody made, like an argument nobody reads.
///
/// note: the same number twice is one item. It is not a mistake worth refusing - a model gathering
/// ids from two places writes it - but counted twice, a move naming one item would report that two
/// had moved.
fn ids(args: &Value, name: &str) -> Result<Vec<ContextId>, String> {
    if args[name].is_null() {
        return Ok(Vec::new());
    }
    if let Some(inside) = wrapped(&args[name]) {
        return Err(format!(
            "`{name}` is `{}`: the list is inside an `item`, and nothing was done. Send the list \
             itself, as numbers - `{name}: {}`.",
            args[name],
            as_numbers(inside)
        ));
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

/// What an argument holds when a model has put it inside an object of one `item` key.
///
/// note: a spelling of lists some models carry over from markup, where each element is an `<item>`.
/// Read through it, a call would be a guess at what was meant. Refused as an argument that is not a
/// list, the model is not told where its list went and sends it the same way again.
fn wrapped(value: &Value) -> Option<&Value> {
    let fields = value.as_object()?;
    (fields.len() == 1).then(|| fields.get("item")).flatten()
}

/// A list of item numbers written the way `ids` takes it, for a refusal to show.
fn as_numbers(value: &Value) -> String {
    let numbers: Vec<String> = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(std::slice::from_ref(value))
        .iter()
        .filter_map(|it| it.as_u64().or_else(|| it.as_str()?.trim().parse().ok()))
        .map(|n: u64| n.to_string())
        .collect();

    match numbers.is_empty() {
        true => "[7]".to_owned(),
        false => format!("[{}]", numbers.join(", ")),
    }
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
/// note: refused rather than letting one of the two win. [`crate::tools::ops::inner`] refuses the
/// same failure one level out, where a call puts arguments inside the wrapper and beside it, for
/// the same reason: whichever of the two is read, the answer comes back as an ordinary one, and
/// nothing in it says the other half was never looked at. `elide` with `ids: [4]` and
/// `select: "all:tool_results"` would move whatever the selector matched, report that, and read as
/// a call that did what it was told.
///
/// note: refused here rather than said in the schema, because the schema cannot say it. Mutual
/// exclusion is `oneOf` or `not`, and neither is a keyword both dialects take: Google's `Schema` is
/// a closed set of fields and has neither. `anyOf` does not say it either, since a branch per
/// argument still matches a call carrying both.
pub(crate) fn named<'a>(items: &[Arc<ContextItem>], args: &'a Value) -> Result<Named<'a>, String> {
    let numbers = ids(args, "ids")?;
    // note: refused rather than read as no selector. A `select` that is not a string would
    // otherwise drop out here, and `look` would list everything as though it had never been given:
    // the call ignoring an argument, answered as a call that did what it was told
    if let Some(inside) = wrapped(&args["select"]).filter(|inside| inside.is_array()) {
        return Err(format!(
            "`select` holds `{}`, which is not a selector: it is a list inside an `item`, and \
             nothing was done. Items named by number go in `ids`, as numbers - `ids: {}`.",
            args["select"],
            as_numbers(inside)
        ));
    }
    if !args["select"].is_null() && !args["select"].is_string() {
        return Err(format!(
            "`select` holds `{}`, which is not a selector, and nothing was done. A selector is \
             one string - an item number, `all`, `tool:fs`, `label:notes` - and `ids` takes \
             numbers.",
            args["select"]
        ));
    }
    let Some(input) = args["select"].as_str().filter(|it| !it.trim().is_empty()) else {
        return Ok(Named {
            select: None,
            ids: numbers,
        });
    };

    // note: checks whether `ids` named anything, not whether it was given. `ids: []` names no
    // items, so reading the selector alone answers exactly the call that was made. A model that
    // fills every optional list with `[]` sends one beside every selector it writes, and was
    // refused for it again and again. What `ids()` cannot read is refused above, so a list that
    // comes to nothing here was empty
    if let Some(first) = numbers.first() {
        return Err(format!(
            "`ids` and `select` in one call, and nothing was done. They are two ways of saying \
             which items - `ids: {}` is those by number, `select: \"{input}\"` is a class of them \
             - and reading one of the two would have answered a call you did not make. Send \
             whichever you meant; a selector takes an item number too, so `select: \"{}\"` is \
             that one item.",
            serde_json::Value::Array(numbers.iter().map(|id| json!(id.0)).collect::<Vec<_>>()),
            first.0
        ));
    }

    match input.parse::<Selector>() {
        Ok(selector) => Ok(Named {
            select: Some(input),
            ids: selector.matches(items),
        }),
        Err(e) => Err(format!(
            "`{input}` is not a selector: {e}\n\n{}\n\n`undo` takes back any change one of \
             these made.",
            crate::help::SELECTORS
        )),
    }
}

/// What a refusal adds when the selector that matched nothing was a `file:` one, or one naming the
/// person's notes.
///
/// note: `memories` are the notes the person wrote at the prompt, and the ones the model wrote are
/// `source:agent`. A model looking for its own notes reaches for `memories`, matches nothing, and
/// is told so here rather than left to guess again.
///
/// note: `file:` names a file attached to the context. A path the model read is a tool result
/// instead, and no `file:` matches it - nearly every empty `file:` is that - so it is said where
/// the mistake is made.
///
/// note: the tool is named only where the session still has it. A selector that names no tool is
/// named either way: `kind:tool_result` is a word this tool prints itself, so it stays true however
/// the session is set up. Everything named in an answer is read as something to try.
pub(crate) fn unmatched(kernel: &Kernel, select: &str) -> String {
    let source = select.trim().to_lowercase();
    let source = (source.strip_prefix("all:"))
        .or_else(|| source.strip_prefix("source:"))
        .unwrap_or(&source);
    if matches!(source, "memory" | "memories") {
        return " `memories` are the notes the person wrote; the notes you wrote are \
                `source:agent`."
            .to_owned();
    }
    if !select.trim_start().starts_with("file:") {
        return String::new();
    }

    format!(
        " `file:` names a file attached to the context; a path you read is a tool result instead, \
         which `kind:tool_result` names with the others of its kind and `look` numbers one by \
         one.{}",
        if_offered(kernel, "fs", || " `tool:fs` names the ones `fs` produced."
            .to_owned())
    )
}

/// How many lines one `search`, or records one `log` read, may hand back in a single call.
///
/// note: a ceiling at all, because both tools exist to price what they are about to hand over and
/// then hand it over, and a `take` of "as many as there are" is the one way to ask for it not to be
/// priced. `search` on a real context and `log` on a real log each once returned tens of thousands
/// of tokens in one result, which the compactor elided on the way in: the tool paid for the answer,
/// the model was left holding a marker saying `compacted to make room`, and the result of the read
/// was in the record the read was supposed to explain. So a number above this is taken as this and
/// the answer says so.
///
/// note: clamped rather than refused, and said in the answer either way. A refusal would be the
/// stricter shape, and `undo`/`redo` refuse a `steps` above their own bound because a walk of
/// sixty-four where a hundred was asked for reads as though the hundred was undone. Here the answer
/// opens with the true count and the true price of every match whatever the `take` was, so a
/// clamped answer is still true, and only the number of lines in it is lost.
///
/// note: one number for both, because it is one word. `search` and `log` are the two tools that
/// hand over pieces of the session, and a model that has learned what 64 buys from one of them has
/// learned it for the other.
pub(crate) const TAKE: usize = 64;

/// The pins the model made itself, and the note each was made with.
///
/// note: the note as well as the identifier, because an identifier alone goes stale. The list is
/// only written by the model's own moves, so an item the model pinned and the person then unpinned
/// and pinned again would still be on it, and the model could take the person's pin off. The
/// person's pins carry no note and the model's carry its reason, so a pin is the model's while the
/// item still says what the model wrote.
type Mine = BTreeMap<ContextId, Option<String>>;

/// The metadata key a pin the model made is written under: `{"by": "context", "note": ...}`.
const PINNED: &str = "pinned";

/// Whether the item's metadata says the model pinned it, with the note it still carries.
///
/// note: what survives a resume, where `Mine` does not. It is the same test `Mine` makes - the pin
/// is the model's while the item still says what the model wrote - so an item the person unpinned
/// and pinned again is the person's however its metadata reads: their pin carries no note.
fn marked_mine(item: &ContextItem) -> bool {
    let pinned = &item.meta[PINNED];
    pinned["by"] == "context" && pinned["note"] == serde_json::json!(item.note)
}

/// Why this item is not the model's to change, if it is not.
fn protected(item: &ContextItem, mine: &Mine, own_turn: Option<ContextId>) -> Option<String> {
    if matches!(item.kind, ContextKind::System) {
        return Some("a system instruction, which belongs to whoever started this session".into());
    }
    if item.state == ContextState::Pinned
        && mine.get(&item.id) != Some(&item.note)
        && !marked_mine(item)
    {
        return Some("pinned by the person you are working with, and a pin is a promise".into());
    }
    if own_turn == Some(item.id) {
        return Some("the assistant turn this very call is part of".into());
    }

    None
}
