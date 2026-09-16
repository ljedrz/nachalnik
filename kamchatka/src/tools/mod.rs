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
//! results.

use std::{collections::BTreeMap, sync::Arc};

use nachalnik::{BoxError, Tool, ToolSpec};
use parking_lot::Mutex;

use crate::sandbox::Reach;
use serde_json::Value;

mod files;
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

use crate::tools::{
    files::{Edit, Read, Write},
    search::{Glob, Grep, Looking},
};

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

/// How much of each tool's output the model is shown, which a person can change mid-session.
///
/// note: a shared handle rather than a number beside each `spec`, because the thing that changes
/// them (`/limit`) is in another file from the tools that declare them - and because `/limit`
/// lists the table, and a person raising one wants to see the others. [`Tool::spec`] is called
/// afresh for every request, so a change here lands on the next call rather than needing a
/// restart: the same property `/tools drop` leans on.
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
    /// The limits the tools here start with.
    ///
    /// note: 32,000 bytes is about a screenful of a large file or the tail of a long build, and
    /// it is what `read`, `shell` and `context` are worth being cut at. `amend` is 8,000
    /// because everything it says is a confirmation of something the caller just asked for, and a
    /// confirmation that long has gone wrong somewhere else.
    ///
    /// note: `grep` and `glob` are in here for `/limit` to list and to raise, and neither is
    /// normally what shapes their answer: both cut themselves at a number of *matches* or *paths*
    /// and say so, which a byte limit cannot do - it takes the tail of the last file searched and
    /// leaves nothing saying there was more. These are the backstop for the one line that is a
    /// megabyte wide.
    ///
    /// note: `context` is the one that chafes, because one number covers six actions of very
    /// different shapes - a context listing and a whole copy of this model's answer. `fork_result`
    /// leads with the answer for that reason, so what a limit takes there is the thinking.
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(BTreeMap::from([
            ("read".to_owned(), 32_000),
            ("grep".to_owned(), 32_000),
            ("glob".to_owned(), 32_000),
            ("shell".to_owned(), 32_000),
            ("context".to_owned(), 32_000),
            ("log".to_owned(), 32_000),
            ("setup".to_owned(), 32_000),
            ("amend".to_owned(), 8_000),
        ]))))
    }

    /// The limit in force for a tool, if this holds one for it.
    pub fn of(&self, tool: &str) -> Option<usize> {
        self.0.lock().get(tool).copied()
    }

    /// Sets one, returning what it was; `None` if this holds no limit for that tool.
    pub fn set(&self, tool: &str, bytes: usize) -> Option<usize> {
        self.0.lock().get_mut(tool).map(|at| {
            let was = *at;
            *at = bytes;
            was
        })
    }

    /// Every limit this holds, by tool.
    pub fn all(&self) -> Vec<(String, usize)> {
        self.0
            .lock()
            .iter()
            .map(|(tool, bytes)| (tool.clone(), *bytes))
            .collect()
    }

    /// Puts whatever limit is in force onto a spec, found by the spec's own id.
    ///
    /// note: by the id rather than by a name passed in, so a tool cannot declare itself one thing
    /// and read another's limit. A tool this holds nothing for is handed back untouched, which is
    /// every MCP tool and every tool somebody else wrote.
    pub fn apply(&self, spec: ToolSpec) -> ToolSpec {
        match self.of(&spec.id) {
            Some(bytes) => spec.with_output_limit(bytes),
            None => spec,
        }
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
        Arc::new(Read(reach.clone(), limits)),
        Arc::new(Write(reach.clone())),
        Arc::new(Edit(reach)),
        Arc::new(Grep(looking.clone())),
        Arc::new(Glob(looking)),
        Arc::new(shell),
    ]
}
