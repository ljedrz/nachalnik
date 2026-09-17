//! The six tools, the permission policy and the compactor - all of it ordinary user code.
//!
//! note: The runtime ships none of this. It has no idea what a file is, it spawns no processes,
//! and it never decides that something may run. What it provides is the shape: a [`Tool`] that
//! declares what it needs, a [`nachalnik::PermissionPolicy`] that is asked before anything
//! happens, and a [`nachalnik::Compactor`] whose plan is applied in the open and can be undone.
//!
//! note: a file each, because they answer to three different traits and are read at three
//! different moments. `files`, `search` and `shell` are the tools themselves, `policy` is what
//! decides whether one of them runs, and `trim` is what happens when there is no room left for the
//! results. `ops` is under all of them: what a tool that does several things declares, and the
//! schema a model is shown for it.

use std::{collections::BTreeMap, sync::Arc};

use nachalnik::{BoxError, Capability, Tool};
use parking_lot::Mutex;

use crate::sandbox::Reach;
use serde_json::Value;

mod files;
mod fs;
pub(crate) mod ops;
mod policy;
mod search;
mod shell;
mod trim;

pub use crate::tools::{
    policy::{Careful, Subject, path_matches, reaches_the_network},
    shell::{Exit, Shell},
    trim::Trim,
};

/// The domains this program's own tools act in, beside the three the runtime names.
///
/// note: one function per domain rather than a constant per operation, because the operations are
/// each tool's own vocabulary and the domain is the part that has to agree across them. `context`
/// is the one worth pointing at: two tools act in it - one that reads this session's items and one
/// that changes them - and a rule about `context` is about the object, not about either tool.
/// Naming it here is what keeps those two from drifting into two domains.
pub mod domains {
    use nachalnik::{Capability, Domain};

    /// An operation on this session's own context: `look`, `revise`, `elide`.
    pub fn context(op: impl Into<String>) -> Capability {
        Capability::of(Domain::Other("context".into()), op)
    }

    /// An operation on what the session is running with: `model`, `tools`, `permissions`, `policy`.
    pub fn setup(op: impl Into<String>) -> Capability {
        Capability::of(Domain::Other("setup".into()), op)
    }

    /// An operation on the session's own record: `read`.
    pub fn log(op: impl Into<String>) -> Capability {
        Capability::of(Domain::Other("log".into()), op)
    }

    /// Standing up a copy of this session and asking it something: `draft`, `ask`.
    ///
    /// note: a domain of its own rather than an operation on the context, because it neither
    /// reads nor changes this context - it makes a second session and spends what that costs.
    /// Allowing something to read its own items should not be allowing it to buy another request.
    pub fn fork(op: impl Into<String>) -> Capability {
        Capability::of(Domain::Other("fork".into()), op)
    }
}

use crate::tools::search::Looking;

/// Reads the named argument, or explains which one is missing.
fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, BoxError> {
    args[name]
        .as_str()
        .ok_or_else(|| format!("the `{name}` argument is required").into())
}

/// A whole number argument, what was left out, or what was passed where one belonged.
///
/// note: a numeric string is taken as the number, which costs nothing and saves a turn - models
/// quote a number often enough that refusing one is refusing a spelling rather than a mistake. A
/// word is not, because the answer a bare `as_u64().unwrap_or(..)` gives is the *default*, which
/// reads as a tool that did what it was asked.
///
/// note: the same lesson `introspect::log`'s `counted` is named after, where `take: "3"` became
/// `None`, which is what leaving `take` out does - so a model that asked for three lines got a
/// summary and nothing saying its argument had not been read. The message is this side's rather
/// than shared, because what a swallowed argument costs is different here: not an empty answer
/// that reads as an empty log, but an expensive one that reads as the only one available.
fn whole(args: &Value, name: &str, default: u64) -> Result<u64, String> {
    let value = &args[name];
    if value.is_null() {
        return Ok(default);
    }
    if let Some(n) = value.as_u64() {
        return Ok(n);
    }
    if let Some(n) = value.as_str().and_then(|it| it.trim().parse::<u64>().ok()) {
        return Ok(n);
    }

    Err(format!(
        "`{name}` is a whole number and this one is `{value}`. Nothing was searched, rather than \
         something being searched for differently than you asked."
    ))
}

/// A yes-or-no argument, read the same way and refused the same way.
fn truth(args: &Value, name: &str) -> Result<bool, String> {
    let value = &args[name];
    if value.is_null() {
        return Ok(false);
    }
    if let Some(yes) = value.as_bool() {
        return Ok(yes);
    }
    match value.as_str().map(str::trim) {
        Some("true") => return Ok(true),
        Some("false") => return Ok(false),
        _ => {}
    }

    Err(format!(
        "`{name}` is true or false and this one is `{value}`. Nothing was searched, rather than \
         something being searched for differently than you asked."
    ))
}

/// What a call's output is cut at when nothing more specific is said, in bytes.
///
/// note: named because two places need the same number and one of them is not a row in this table.
/// Every tool this program ships has a row; a tool from an MCP server has none and holds no handle
/// to this, so what stops one of those filling a context is
/// [`Config::default_tool_output_limit`](nachalnik::Config), set to this in `wiring`.
pub(crate) const CEILING: usize = 32_000;

/// And what one is cut at whose answer is a report of a fixed shape rather than a piece of the
/// session, in bytes.
///
/// note: the distinction is measured rather than guessed. Between a session of ten items and one
/// of a thousand, with two hundred more tools registered, seven answers do not move: `fs:write`
/// and `fs:edit` are a line each, `context:revise` says what it replaced without quoting it,
/// `context:note` and `setup:model` and `setup:policy` are a short paragraph, and
/// `context:budget` is a fixed report with the four most expensive rows under it. Everything else
/// grows with what the session holds - `context:look` went from 10kB to 119kB over that pair, and
/// `log`'s records from 30kB to 517kB.
///
/// note: what a lower number buys where a limit never fires anyway. It is a tripwire: the seven
/// are bounded because of how each answer is built, so one of them arriving here cut is that
/// having quietly stopped being true - a `budget` that listed every item, a `revise` that echoed
/// what it wrote. `tests/introspect.rs` holds the seven to it at the size, which is where such a
/// change should be caught; this is what happens if it is not.
pub(crate) const REPORT: usize = 8_000;

/// How much of a call's output the model is shown, by subject, which a person can change
/// mid-session.
///
/// note: **one row per subject**, the same `<domain>:<operation>` string the permissions table is
/// keyed on. A call's limit is looked up by the very subject its permission was decided by, so
/// there is one vocabulary in this program and not two: `--allow fs:grep` and `/limit fs:grep`
/// name the same thing, and a person who has read either table can read the other.
///
/// note: it was keyed by tool id, which stopped working the day a tool did several things. `fs`
/// is one tool over five operations whose answers are nothing like the same size - a whole file
/// and a repo-wide search - and `context` is one over thirteen, from a listing of forty items to
/// a line confirming a pin. A number per tool is a number for whichever of those somebody thought
/// of first.
///
/// note: a shared handle rather than a number beside each `spec`, because the thing that changes
/// them (`/limit`) is in another file from the tools that declare them - and because `/limit`
/// lists the table, and a person raising one wants to see the others. It is read afresh for every
/// call, so a change here lands on the next one rather than needing a restart: the same property
/// `/tools toggle` leans on.
///
/// note: raising a limit does not recover a result that has already been shortened, and does not
/// need to. The whole of that one is archived beside the copy the model was shown, and one
/// keystroke on the context tab sends it instead - the projector pairs a call to one result, so
/// the whole claims the call and the short copy is dropped. This is for the next call.
#[derive(Debug, Clone)]
pub struct Limits(Arc<Mutex<BTreeMap<String, usize>>>);

impl Default for Limits {
    fn default() -> Self {
        Self::new()
    }
}

impl Limits {
    /// The limits this program's tools start with: every subject they declare, at 32,000 bytes -
    /// or 8,000, where the answer is a report rather than a piece of the session.
    ///
    /// note: 32,000 bytes is about a screenful of a large file or the tail of a long build, and it
    /// is what an answer made of what the session holds is worth being cut at. Two numbers rather
    /// than twenty-four: a number per row would be a set of opinions nobody asked for, and one
    /// number for every row says that a line confirming a write and the whole of a log are the
    /// same kind of answer. Which tier a subject is in is a measured fact about its answer rather
    /// than a view about its importance: seven of them do not move between a session of ten items
    /// and one of a thousand, and every other one does. The table is there so that somebody can
    /// hold the one that matters to them to something else.
    ///
    /// note: `fs:grep` and `fs:glob` are in here for `/limit` to list and to raise, and neither is
    /// normally what shapes their answer: both cut themselves at a number of *matches* or *paths*
    /// and say so, which a byte limit cannot do - it takes the tail of the last file searched and
    /// leaves nothing saying there was more. These are the backstop for the one line that is a
    /// megabyte wide.
    ///
    /// note: `exec:run` rather than `shell`, which is the tool's name. A limit row is a subject,
    /// and the subject a `shell` call is judged by is `exec:run` - so that is the row. The tool
    /// being named for the thing it runs and the domain for what running is are the one place
    /// those two words come apart in this program.
    pub fn new() -> Self {
        let rows = ["read", "glob", "grep", "write", "edit"]
            .map(Capability::fs)
            .into_iter()
            .chain([Capability::exec("run")])
            .chain(
                [
                    "look", "budget", "request", "search", "elide", "exclude", "pin", "restore",
                    "revise", "note", "undo", "redo",
                ]
                .map(domains::context),
            )
            .chain(["draft", "ask"].map(domains::fork))
            .chain([domains::log("read")])
            .chain(["model", "tools", "permissions", "policy"].map(domains::setup))
            .map(|subject| {
                let subject = subject.to_string();
                let bounded = matches!(
                    subject.as_str(),
                    "fs:write"
                        | "fs:edit"
                        | "context:budget"
                        | "context:note"
                        | "context:revise"
                        | "setup:model"
                        | "setup:policy"
                );

                (subject, if bounded { REPORT } else { CEILING })
            });

        Self(Arc::new(Mutex::new(rows.collect())))
    }

    /// The limit in force for a subject, if this holds one for it.
    pub fn of(&self, subject: &str) -> Option<usize> {
        self.0.lock().get(subject).copied()
    }

    /// The limit for a call, found by the subject that call needs.
    ///
    /// note: every tool's [`Tool::limit`] is this and nothing else, which is the whole of the
    /// rule: what a call may return is decided by the same subject that decided whether it could
    /// run. A call that needs more than one subject - which is what a tool says about a call it
    /// cannot place - gets no limit from here, because there is no one row it is about; it is
    /// about to be refused by name anyway.
    pub fn for_call(&self, needs: &[Capability]) -> Option<usize> {
        match needs {
            [subject] => self.of(&subject.to_string()),
            _ => None,
        }
    }

    /// Sets one, returning what it was; `None` if this holds no limit for that subject.
    pub fn set(&self, subject: &str, bytes: usize) -> Option<usize> {
        self.0.lock().get_mut(subject).map(|at| {
            let was = *at;
            *at = bytes;
            was
        })
    }

    /// Every limit this holds, by subject.
    pub fn all(&self) -> Vec<(String, usize)> {
        self.0
            .lock()
            .iter()
            .map(|(subject, bytes)| (subject.clone(), *bytes))
            .collect()
    }
}

/// Returns the six tools a terminal agent needs to be worth talking to, all held to `reach`.
///
/// note: the policy the two searching tools consult comes off the `shell` rather than being a
/// parameter of its own, because a second handle passed in is a second chance for them to
/// disagree - and two tools in one session honouring two different sets of path rules is the kind
/// of wrong that nothing on the screen would show. There is one `Careful` in a session; this is
/// the handle to it that is already here.
pub fn builtin(shell: Shell, reach: Reach, limits: Limits) -> Vec<Arc<dyn Tool>> {
    let reach = Arc::new(reach);
    let looking = Looking {
        reach: reach.clone(),
        policy: shell.policy.clone(),
        limits: limits.clone(),
    };

    vec![
        Arc::new(fs::Fs::new(reach, looking, limits)),
        Arc::new(shell),
    ]
}
