//! The two tools, the permission policy and the compactor are ordinary user code.
//!
//! note: the runtime ships none of this. It knows nothing of files, spawns no processes, and never
//! decides that something may run. It provides the shape: a [`Tool`] that declares what it needs, a
//! [`nachalnik::PermissionPolicy`] asked before anything happens, and a [`nachalnik::Compactor`]
//! whose plan is applied in the open and can be undone.
//!
//! note: one file each, because they answer to three traits and are read at three moments. `fs` and
//! `shell` are the tools; `fs` dispatches to `files` and `search` for opening one file and walking
//! many. `policy` decides whether one runs. `shedder` handles results the conversation is done
//! with, and whole exchanges when there is no room left. `ops` underlies them all: what a tool that
//! does several things declares, and the schema shown to a model.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use nachalnik::{BoxError, Capability, Tool};
use parking_lot::Mutex;

use crate::sandbox::Reach;
use serde_json::Value;

#[cfg(feature = "shell-advisor")]
pub(crate) mod advice;
mod files;
mod fs;
pub(crate) mod ops;
mod policy;
mod reaching;
mod search;
mod shedder;
mod shell;

#[cfg(feature = "shell-advisor")]
pub use crate::tools::advice::{Advised, Rated, Rating};
pub use crate::tools::{
    files::COUNTED,
    ops::Layout,
    policy::{Careful, Subject, objection_to, path_matches, reaches_the_network},
    reaching::{Reached, Reaching},
    shedder::Shedder,
    shell::{CALL_VAR, Exit, Shell, Stragglers, joints},
};

/// The domains this program's own tools act in, beside the three the runtime names.
///
/// note: one function per domain rather than a constant per operation, because the operations are
/// each tool's own vocabulary and the domain is the part that has to agree across them. `context`
/// is the example: one tool both reads this session's items and changes them, and a rule about
/// `context` is about the object, not about either kind of operation. Naming it here keeps the
/// reading and the changing from drifting into two domains.
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

/// Reads the named argument, or explains which one is missing or what arrived instead.
///
/// note: a value that is not text is refused as what it is, not as missing. Told the argument is
/// required, a model that wrote one looks for an argument it did not leave out.
///
/// note: an object is named by what it holds, because the usual one is the rest of the call nested
/// a level down: `{"new": {"old": …, "new": …}}`. "An object" alone does not say the arguments are
/// inside it.
fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, BoxError> {
    match &args[name] {
        Value::String(text) => Ok(text),
        Value::Null => Err(format!("the `{name}` argument is required").into()),
        Value::Object(held) if !held.is_empty() => Err(format!(
            "`{name}` is text, and this one is an object holding {}: every argument goes beside \
             the others rather than inside one, and nothing was done",
            held.keys()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
        other => Err(format!(
            "`{name}` is text, and this one is {}; nothing was done",
            what(other)
        )
        .into()),
    }
}

/// A value the model passed, as a refusal quotes it: whole if it is short, and its first
/// [`ops::SHOWN`] characters and its size if it is not.
///
/// note: a refusal is a tool result, and a wrong-typed argument can be anything the model wrote.
/// Quoted whole, a two-hundred-kilobyte string passed as a number went into the context, the
/// record and every save as the error about it.
pub(crate) fn quoted(value: &Value) -> String {
    let text = value.to_string();
    match text.chars().nth(ops::SHOWN) {
        None => text,
        Some(_) => format!(
            "{}... ({} bytes)",
            text.chars().take(ops::SHOWN).collect::<String>(),
            text.len()
        ),
    }
}

/// What kind of JSON value arrived, for a refusal that names it without copying it back.
pub(crate) fn what(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "`true` or `false`",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// A whole number argument, what was left out, or what was passed where one belonged.
///
/// note: a numeric string is taken as the number: it costs nothing and saves a turn, and models
/// quote numbers often enough that refusing one refuses a spelling, not a mistake. A word is not,
/// because `as_u64().unwrap_or(..)` would return the default, which reads as a tool that did what
/// it was asked.
///
/// note: the same rule `introspect::log` applies to `take` and `since`: `take: "3"` read as `None`
/// is what leaving `take` out does - a summary where three lines were asked for, with nothing
/// saying the argument was not read. The message is this side's rather than shared, because a
/// swallowed argument costs something different here: not an empty answer that reads as an empty
/// log, but an expensive one that reads as the only one available.
fn whole(args: &Value, name: &str, default: u64) -> Result<u64, String> {
    number(args, name)
        .map(|n| n.unwrap_or(default))
        .map_err(|refusal| format!("{refusal} {UNSEARCHED}"))
}

/// What a search refused over one of its arguments says it did not do.
const UNSEARCHED: &str =
    "Nothing was searched, rather than something being searched for differently than you asked.";

/// A whole number argument to any tool here, `None` left out, refused in words that say only
/// what was wrong with it - the caller says what was not done, as [`truth`] does for
/// [`yes_or_no`].
pub(crate) fn number(args: &Value, name: &str) -> Result<Option<u64>, String> {
    let value = &args[name];
    if value.is_null() {
        return Ok(None);
    }
    if let Some(n) = value.as_u64() {
        return Ok(Some(n));
    }
    if let Some(n) = value.as_str().and_then(|it| it.trim().parse::<u64>().ok()) {
        return Ok(Some(n));
    }

    Err(format!(
        "`{name}` is a whole number and this one is `{}`.",
        quoted(value)
    ))
}

/// A text argument that may be left out, or what was passed where one belonged.
fn words<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>, String> {
    match &args[name] {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text)),
        other => Err(format!(
            "`{name}` is text and this one is `{}`. {UNSEARCHED}",
            quoted(other)
        )),
    }
}

/// A yes-or-no argument, read the same way and refused the same way.
fn truth(args: &Value, name: &str) -> Result<bool, String> {
    yes_or_no(args, name).map_err(|refusal| format!("{refusal} {UNSEARCHED}"))
}

/// A yes-or-no argument to any tool here: a boolean, or the word in quotes, and `false` left out.
///
/// note: shared, because the introspection tools read `false` into a quoted `"true"` for as long
/// as they had their own `as_bool().unwrap_or(false)` - `pin` left a note unpinned and `whole`
/// read a sample - which is the swallowed argument the note on [`whole`] is about.
pub(crate) fn yes_or_no(args: &Value, name: &str) -> Result<bool, String> {
    let value = &args[name];
    if value.is_null() {
        return Ok(false);
    }
    if let Some(yes) = value.as_bool() {
        return Ok(yes);
    }
    match value.as_str().map(str::trim) {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(format!(
            "`{name}` is true or false and this one is `{}`.",
            quoted(value)
        )),
    }
}

/// The default cut-off for one call's output, in bytes.
///
/// note: two places need this number, and one is not a row in this table. Every tool this program
/// ships has a row; a tool from an MCP server has none and no handle to this, so
/// [`Config::default_tool_output_limit`](nachalnik::Config) stops it filling a context. It is set
/// to this in `wiring` and held to the window with the rows; see [`Limits::held`].
pub(crate) const CEILING: usize = 32_000;

/// The most of one call's output a tool keeps here, in bytes: what a command writes to each of its
/// two streams, and the largest file `fs` edits. [`COUNTED`] is how far `read` counts lines;
/// counting keeps nothing.
///
/// note: a limit decides what the model is shown. The kernel keeps the whole of what a tool
/// returned, excluded; without this, the whole was whatever arrived: a `yes` nobody stopped, or a 2
/// GB log held in this process, in the context and in every save after it. Past this, a command's
/// output is read and let go. The answer says how much at the top, where no limit cuts it. A file
/// is refused an edit with a sentence saying how else to make one.
pub const KEPT: usize = 8 * 1024 * 1024;

/// The limit for one call's output when the answer is a fixed-shape report rather than a piece of
/// the session, in bytes.
///
/// note: the distinction is measured, not guessed. Seven answers do not change between a small
/// session and a large one with many more tools registered: `fs:write` and `fs:edit` are one line
/// each; `context:revise` says what it replaced without quoting it; `context:note`, `setup:model`,
/// and `setup:policy` are a short paragraph; and `context:budget` is a fixed report with the ten
/// most expensive rows under it. Every other answer grows with what the session holds, especially
/// `context:look` and `log`'s records.
///
/// note: a lower number buys a tripwire where the limit never fires anyway. The seven answers are
/// bounded because of how each is built, so if one arrives cut, that has silently stopped being
/// true: a `budget` that listed every item, or a `revise` that echoed what it wrote.
/// `tests/introspect/shared.rs` holds the seven to this size, so such a change should be caught
/// there; this is what happens if it is not.
pub(crate) const REPORT: usize = 8_000;

/// How much of a call's output the model is shown, by subject. A person can change the rows
/// mid-session.
///
/// note: each row is keyed by subject, the same `<domain>:<operation>` string the permissions table
/// uses. A call's limit is looked up by the subject its permission was decided by, so permissions
/// and limits use one set of names: `--auto-allow fs:grep` and `/limit fs:grep` name the same
/// thing, and anyone who has read either table can read the other.
///
/// note: rows are not keyed by tool id, because a tool can do several things. `fs` covers five
/// operations whose answers range from a whole file to a repo-wide search, and `context` covers
/// twelve, from a listing of items to a line confirming a pin. A number per tool would fit
/// whichever operation somebody thought of first.
///
/// note: a shared handle rather than a number beside each `spec`, because `/limit`, which changes
/// the rows, is in another file from the tools that declare specs, and because `/limit` lists the
/// table and a person raising one row wants to see the others. It is read afresh for every call, so
/// a change lands on the next call without a restart; `/tools toggle` relies on the same property.
///
/// note: raising a limit does not recover a result that was already shortened, and need not: the
/// whole result is excluded alongside the shortened copy the model was shown, and one keystroke on
/// the context tab sends the whole instead. The projector pairs a call with one result, so the
/// whole result is used for that call and the shortened copy is dropped. The new limit applies to
/// the next call. `fs:read` is an exception: it stops at a line under the limit and says where to
/// read on from, so the file is the whole result and nothing was cut.
///
/// note: every row is also held to the context window until somebody sets it: one tenth of the
/// window's size, at about four bytes per token, and never more than the row. On a small window, a
/// 32,000-byte answer can be most of the context, and two in a turn can leave no room for the
/// model's next call, so the request is refused. Report rows are 8,000 bytes, so only a window
/// under twenty thousand tokens caps them. A row somebody set is used exactly as set.
#[derive(Debug, Clone)]
pub struct Limits(Arc<Mutex<Table>>);

/// What [`Limits`] holds.
#[derive(Debug)]
struct Table {
    rows: BTreeMap<String, usize>,
    /// The rows somebody has set, which the window does not move.
    set: BTreeSet<String>,
    /// The context window of the model in use, in tokens, where it is known.
    window: Option<usize>,
}

impl Table {
    /// The limit a row is at: its own, or less on a small window; see the note on [`Limits`].
    fn of(&self, subject: &str) -> Option<usize> {
        let row = *self.rows.get(subject)?;
        match self.set.contains(subject) {
            true => Some(row),
            false => Some(self.held(row)),
        }
    }

    /// A limit nobody set, held to the window.
    fn held(&self, bytes: usize) -> usize {
        match self.window {
            Some(window) => bytes.min((window * 4 / 10).max(FLOOR)),
            None => bytes,
        }
    }
}

/// The least a window holds a row down to, in bytes, so a window too small to work in still shows
/// some lines of an answer.
const FLOOR: usize = 1024;

impl Default for Limits {
    fn default() -> Self {
        Self::new()
    }
}

impl Limits {
    /// The default limits for this program's tools: every subject they declare starts at 32,000
    /// bytes, or 8,000 where the answer is a report rather than a piece of the session.
    ///
    /// note: 32,000 bytes is about a screenful of a large file or the tail of a long build, and it
    /// is the cut-off for an answer made from what the session holds. Two tiers rather than a
    /// number per row: a number per row would be a set of opinions nobody asked for, and one number
    /// for every row would treat a line confirming a write and a whole log as the same kind of
    /// answer. The tier is decided by whether the answer grows with the session, not by importance:
    /// seven subjects have answers that do not, and every other one does. The table exists so a
    /// person can change the row that matters to them.
    ///
    /// note: `fs:grep` and `fs:glob` are in the default table for `/limit` to list and raise.
    /// Neither row normally shapes its own answer: both cut at a number of matches or paths and say
    /// so, which a byte limit cannot do; a byte limit takes the tail of the last file searched and
    /// leaves nothing saying there was more. These rows are the fallback for the one line that is a
    /// megabyte wide.
    ///
    /// note: the row is `exec:run`, not `shell`, the tool's name, because a limit row is a subject
    /// and a `shell` call is judged by `exec:run`. It is the one place in this program where a
    /// tool's name and its domain come apart.
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

        Self(Arc::new(Mutex::new(Table {
            rows: rows.collect(),
            set: BTreeSet::new(),
            window: None,
        })))
    }

    /// Says how large the context window of the model in use is, in tokens; `None` where it is not
    /// known.
    pub fn window(&self, tokens: Option<usize>) {
        self.0.lock().window = tokens;
    }

    /// A limit nobody set, in bytes, held to the window like an unset row.
    ///
    /// note: the cut-off for a tool with no row, such as one from an MCP server. It is the kernel's
    /// own limit rather than a row, since what has no row cannot be set. The `App` hands it to
    /// [`Kernel::set_default_tool_output_limit`] as the window moves.
    ///
    /// [`Kernel::set_default_tool_output_limit`]: nachalnik::Kernel::set_default_tool_output_limit
    pub fn held(&self, bytes: usize) -> usize {
        self.0.lock().held(bytes)
    }

    /// The limit in force for a subject, if this holds one for it.
    pub fn of(&self, subject: &str) -> Option<usize> {
        self.0.lock().of(subject)
    }

    /// The limit for a call, found by the subject that call needs.
    ///
    /// note: every tool's [`Tool::limit`] is this and nothing else: what a call may return is
    /// decided by the same subject that decided whether it could run. A call that needs more than
    /// one subject - which is what a tool says about a call it cannot place - gets no limit from
    /// here, because there is no one row it is about; it is about to be refused by name anyway.
    pub fn for_call(&self, needs: &[Capability]) -> Option<usize> {
        match needs {
            [subject] => self.of(&subject.to_string()),
            _ => None,
        }
    }

    /// Sets one, returning what it was; `None` if this holds no limit for that subject.
    pub fn set(&self, subject: &str, bytes: usize) -> Option<usize> {
        let mut table = self.0.lock();
        let was = table.of(subject)?;
        table.rows.insert(subject.to_owned(), bytes);
        table.set.insert(subject.to_owned());

        Some(was)
    }

    /// Every limit this holds, by subject, as each is in force.
    pub fn all(&self) -> Vec<(String, usize)> {
        let table = self.0.lock();
        table
            .rows
            .keys()
            .filter_map(|subject| Some((subject.clone(), table.of(subject)?)))
            .collect()
    }
}

/// Returns the two tools a terminal agent needs to be worth talking to, `fs` and `shell`, both held
/// to `reach`.
///
/// note: the policy the two searching tools consult comes off the `shell` rather than being a
/// parameter of its own, because a second handle passed in is a second chance for them to disagree,
/// and two tools in one session honouring two different sets of path rules would be wrong with
/// nothing on the screen showing it. There is one `Careful` in a session; this is the handle to it
/// that is already here.
///
/// note: `fs` takes its [`Layout`] off the `shell` for the same reason: two tools of one session
/// offered in two layouts would have a model writing one of them wrong.
pub fn builtin(shell: Shell, reach: Reach, limits: Limits) -> Vec<Arc<dyn Tool>> {
    let reach = Arc::new(reach);
    let looking = Looking {
        reach: reach.clone(),
        policy: shell.policy.clone(),
        limits: limits.clone(),
    };

    vec![
        Arc::new(fs::Fs::new(reach, looking, limits, shell.layout)),
        Arc::new(shell),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CEILING, FLOOR, Limits, REPORT, arg, number, words, yes_or_no};

    /// A wrong-typed argument is quoted back cut short, whatever the model put in it.
    ///
    /// note: quoted whole, a long string passed as a number went into the context as its own
    /// refusal, and into the record and every save after it.
    #[test]
    fn a_long_wrong_argument_is_quoted_short() {
        let args = json!({ "it": "x".repeat(200_000) });
        let refusals = [
            number(&args, "it").expect_err("not a number"),
            yes_or_no(&args, "it").expect_err("not true or false"),
            words(&json!({ "it": ["x".repeat(200_000)] }), "it").expect_err("not text"),
        ];
        for refusal in refusals {
            assert!(refusal.len() < 1_000, "{} bytes", refusal.len());
            assert!(refusal.contains("bytes)"), "and says how large: {refusal}");
        }

        // and a short one is quoted as it was
        let refusal = number(&json!({ "it": "three" }), "it").expect_err("not a number");
        assert!(refusal.contains("`\"three\"`"), "{refusal}");
    }

    /// Every row is held to a tenth of a small window, never above itself, and a row somebody set
    /// is left as they set it.
    #[test]
    fn every_row_is_held_to_a_small_window_until_somebody_sets_it() {
        let limits = Limits::new();
        assert_eq!(limits.of("fs:read"), Some(CEILING), "no window known");
        assert_eq!(limits.of("context:budget"), Some(REPORT));

        // 14,000 tokens: a tenth is 1,400, at four bytes a token
        limits.window(Some(14_000));
        for subject in ["fs:read", "exec:run", "fs:grep", "context:look", "log:read"] {
            assert_eq!(limits.of(subject), Some(5_600), "{subject}");
        }
        assert_eq!(
            limits.of("context:budget"),
            Some(5_600),
            "a report is held too"
        );
        assert!(
            limits.all().contains(&("exec:run".to_owned(), 5_600)),
            "`/limit` lists what is in force"
        );
        // a window large enough leaves a report where it was, and a large one raises nothing
        limits.window(Some(40_000));
        assert_eq!(limits.of("context:budget"), Some(REPORT));
        assert_eq!(limits.of("exec:run"), Some(16_000));
        limits.window(Some(1_000_000));
        assert_eq!(limits.of("exec:run"), Some(CEILING));
        // and a tiny one does not take it to nothing
        limits.window(Some(100));
        assert_eq!(limits.of("exec:run"), Some(FLOOR));

        // a row somebody set is what they set, whatever the window, and the others still move
        limits.window(Some(14_000));
        assert_eq!(
            limits.set("exec:run", 20_000),
            Some(5_600),
            "what it was in force at"
        );
        assert_eq!(limits.of("exec:run"), Some(20_000));
        limits.window(Some(1_000));
        assert_eq!(limits.of("exec:run"), Some(20_000));
        assert_eq!(limits.of("fs:read"), Some(FLOOR));
    }

    /// A text argument that arrived as something else is refused as that, not as missing.
    #[test]
    fn an_argument_that_is_not_text_is_not_called_missing() {
        let wrong = arg(&json!({ "path": 7 }), "path").expect_err("a number is not a path");
        assert_eq!(
            wrong.to_string(),
            "`path` is text, and this one is a number; nothing was done"
        );

        let missing = arg(&json!({}), "path").expect_err("nothing is not a path");
        assert_eq!(missing.to_string(), "the `path` argument is required");

        assert_eq!(arg(&json!({ "path": "a.rs" }), "path").ok(), Some("a.rs"));
    }

    /// An object is named by what it holds, which is how a call nested a level down shows itself.
    #[test]
    fn an_object_where_text_belongs_says_what_it_holds() {
        let nested = arg(&json!({ "new": { "old": "a", "new": "b" } }), "new")
            .expect_err("an object is not text");
        assert_eq!(
            nested.to_string(),
            "`new` is text, and this one is an object holding `new`, `old`: every argument goes \
             beside the others rather than inside one, and nothing was done"
        );

        let empty = arg(&json!({ "new": {} }), "new").expect_err("nor is an empty one");
        assert_eq!(
            empty.to_string(),
            "`new` is text, and this one is an object; nothing was done"
        );
    }
}
