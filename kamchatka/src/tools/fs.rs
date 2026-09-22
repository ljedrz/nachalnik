//! `fs`: one tool for the filesystem, and the five things it does to one.
//!
//! note: one tool per *object* rather than per verb. Reading a file, listing files, searching
//! them, writing one and changing part of one are five operations on one thing, and a model choosing
//! between five tools has to know which of them is the filesystem before it can choose at all.
//! The permission subjects come out of the same decision - `fs:read` and `fs:write` are what this
//! declares, so a rule is about the filesystem and an operation on it rather than about whichever
//! happened to serve it.
//!
//! note: it dispatches to the five implementations rather than reimplementing them. Each of them
//! already knows how to ask [`Reach`](crate::sandbox::Reach) for a path, how to walk a directory
//! and what to say when it will not fit; a merge that rewrote any of that would be a merge that
//! could change behaviour without meaning to. What is new here is the schema, the dispatch and
//! the two things only this level can answer: which subject a call needs, and how much of its
//! answer the model is shown.

use std::sync::Arc;

use nachalnik::{
    BoxError, Capability, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};
use serde_json::Value;

use crate::{
    sandbox::Reach,
    tools::{
        Limits,
        files::{Edit, PATH_ARG, Read, Write},
        ops::{Arg, Op, action_of, actions, inner, schema, unread},
        search::{GLOB_ARG, Glob, Grep, Looking, MATCHES, PATHS, WIDTH},
    },
};

/// The five things this tool does, what each is for, and what each reads.
///
/// note: one table where there were three - the `action` enum, the `TAKES` list [`unread`] holds a
/// call to, and a flat schema listing every argument any operation takes. Four things have to
/// agree about this vocabulary: the enum a model chooses from, the subject each call declares, the
/// row `/limit` keys on, and what the model is actually allowed to pass. Three of them were
/// derived from a list and the fourth was written out by hand beside it.
///
/// note: what the arguments no longer have to say is which operation they belong to. `for
/// \`grep\`:` opened eight of the nine descriptions here, because a flat bag is the only place a
/// reader could be told - and it was advice, not a rule. Inside a branch there is nobody else to
/// be confused with, so each one says what it is and stops.
fn ops() -> Vec<Op> {
    vec![
        Op::new(
            "read",
            "reads a whole text file",
            vec![Arg::text("path", "the file to read").needed()],
        ),
        Op::new(
            "glob",
            "finds paths without opening anything",
            vec![
                Arg::text("pattern", GLOB_ARG).needed(),
                Arg::text("path", WHERE),
            ],
        ),
        Op::new(
            "grep",
            "searches inside files",
            vec![
                Arg::text(
                    "pattern",
                    "a regular expression in Rust's regex syntax - `\\bKernel\\b`, `impl .* for`. \
                     It has no look-around; escape anything you mean literally: `Vec<u8>\\(`",
                )
                .needed(),
                Arg::text("path", WHERE),
                Arg::text(
                    "glob",
                    "search only the files whose path matches this, written the way `glob`'s own \
                     `pattern` is",
                ),
                Arg::truth(
                    "ignore_case",
                    "match without regard to case; false by default",
                ),
                Arg::whole(
                    "context",
                    "lines to show either side of each match, up to 10; none by default. They are \
                     marked with a `-` where a match is marked with a `:`",
                ),
                Arg::truth(
                    "files_only",
                    "answer with the files that match and how many each has - `path: 12`, most \
                     first - instead of the lines, which is `grep -l`. For a common word, or when \
                     you do not know where something lives: a fraction of the tokens, and it names \
                     the file to search properly next",
                ),
            ],
        ),
        Op::new(
            "write",
            "writes a whole file, replacing whatever was there",
            vec![
                Arg::text("path", "the file to write").needed(),
                Arg::text("content", "the whole new file").needed(),
            ],
        ),
        Op::new(
            "edit",
            "replaces one piece of a file",
            vec![
                Arg::text("path", "the file to change").needed(),
                Arg::text(
                    "old",
                    "the exact text to replace, whitespace included; include enough of the \
                     surrounding lines to make it the only match",
                )
                .needed(),
                Arg::text("new", "what to put there instead").needed(),
            ],
        ),
    ]
}

/// What `glob` and `grep` say about the path they are pointed at, which is the same thing twice.
const WHERE: &str = "where to look - one file, or a directory and everything under it; the \
                     working directory if left out";

/// Everything a session may do to a file, as one tool.
pub(super) struct Fs {
    read: Read,
    glob: Glob,
    grep: Grep,
    write: Write,
    edit: Edit,
    limits: Limits,
    ops: Vec<Op>,
    /// note: built once rather than per `spec`, which is called afresh for every request. It was
    /// one `json!` before and cost the same; a branch per operation is enough more work to be
    /// worth not doing sixty times a session.
    schema: Arc<Value>,
}

impl Fs {
    pub(super) fn new(reach: Arc<Reach>, looking: Looking, limits: Limits) -> Self {
        let ops = ops();
        Self {
            read: Read(reach.clone()),
            glob: Glob(looking.clone()),
            grep: Grep(looking),
            write: Write(reach.clone()),
            edit: Edit(reach),
            limits,
            schema: Arc::new(schema(&ops)),
            ops,
        }
    }

    /// The operation a call names, if it names one this tool has.
    fn op(&self, call: &ToolCall) -> Option<String> {
        action_of(call, &self.ops)
    }
}

#[async_trait]
impl Tool for Fs {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "fs",
            format!(
                "the filesystem, five operations on it. Every `path` is {PATH_ARG}. `glob` \
                 and `grep` walk a directory with no shell in front of them: they obey \
                 `.gitignore` and stay out of `.git`, neither of them counted; they do look at \
                 hidden files, and they count everything else they passed over. \
                 At most {MATCHES} matches or {PATHS} paths come back, and a line wider than \
                 {WIDTH} characters is cut."
            ),
        )
        .with_schema(self.schema.clone())
        .with_capabilities(
            actions(&self.ops)
                .into_iter()
                .map(Capability::fs)
                .collect::<Vec<_>>(),
        )
    }

    /// note: `action` and nothing else, so a rule about `fs:read` is about reading whichever way
    /// a call asked for it. A call naming no operation it has declares all five, which is the
    /// strictest reading of a call nobody can place - and `invoke` then refuses it by name.
    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        match self.op(call) {
            Some(action) => vec![Capability::fs(action)],
            None => self.spec().capabilities,
        }
    }

    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits.for_call(&self.needs(call))
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        // the arguments wherever the model put them, which is the one thing that has to happen
        // before anything here reads one
        let args = match inner(&call.args) {
            Ok(args) => args,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        let args = &*args;

        // note: the operation is checked here as well as in `needs`, because a refusal a model
        // can act on is one that names what was wrong. The alternative - dispatching on a default
        // - runs something nobody asked for
        let Some(action) = self.op(call) else {
            return Ok(ToolOutput::error(format!(
                "`{}` is not something `fs` does; it does {}",
                args["action"].as_str().unwrap_or("nothing"),
                actions(&self.ops).join(", ")
            )));
        };

        if let Some(refusal) = unread(&action, args, &self.ops) {
            return Ok(ToolOutput::error(refusal));
        }

        match action.as_str() {
            "read" => self.read.invoke(args, output).await,
            "glob" => self.glob.invoke(args, output).await,
            "grep" => self.grep.invoke(args, output).await,
            "write" => self.write.invoke(args, output).await,
            _ => self.edit.invoke(args, output).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema and the permission subjects are one vocabulary.
    ///
    /// note: what the two-list check became. It used to assert that `OPS`, `TAKES` and a flat
    /// schema agreed about every argument, which they had to be made to do by hand; they are one
    /// `Vec<Op>` now, and the branch an argument appears in *is* the operation that reads it. What
    /// can still drift is this: a subject with no branch to reach it by, or a branch the policy
    /// was never told about, which is a call that cannot be refused by name.
    #[test]
    fn the_schema_and_the_subjects_are_one_vocabulary() {
        let reach = || {
            Arc::new(Reach {
                workdir: std::env::temp_dir(),
                extra: Vec::new(),
                readable: Vec::new(),
                confined: true,
            })
        };
        let tool = Fs::new(
            reach(),
            Looking {
                reach: reach(),
                policy: Arc::new(crate::tools::Careful::new()),
                limits: Limits::default(),
            },
            Limits::default(),
        );

        let spec = tool.spec();
        let offered: Vec<String> = crate::tools::ops::offered(&spec.schema)
            .into_iter()
            .map(|action| Capability::fs(action).to_string())
            .collect();
        let declared: Vec<String> = spec.capabilities.iter().map(ToString::to_string).collect();

        assert_eq!(offered, declared, "one list of operations, in one order");
        assert_eq!(
            offered,
            ["fs:read", "fs:glob", "fs:grep", "fs:write", "fs:edit"]
        );
    }

    /// An argument belongs to the operations that read it, and to no others.
    ///
    /// note: the half of the old check that was about the model rather than about the code. `old`
    /// was a well-formed argument to `fs: read` under the flat schema and the only thing saying
    /// otherwise was the phrase "for `edit`:" at the front of its description. Now the branch says
    /// it, so this asserts on the branch.
    #[test]
    fn an_argument_is_offered_by_the_operations_that_read_it() {
        let wanted = [
            ("read", vec!["action", "path"]),
            ("glob", vec!["action", "path", "pattern"]),
            (
                "grep",
                vec![
                    "action",
                    "context",
                    "files_only",
                    "glob",
                    "ignore_case",
                    "path",
                    "pattern",
                ],
            ),
            ("write", vec!["action", "content", "path"]),
            ("edit", vec!["action", "new", "old", "path"]),
        ];

        for (op, mut expected) in wanted {
            let branch = schema(&ops())["properties"]["call"]["anyOf"]
                .as_array()
                .expect("a branch per operation")
                .iter()
                .find(|it| it["properties"]["action"]["enum"][0] == op)
                .cloned()
                .unwrap_or_else(|| panic!("no branch for `{op}`"));

            let mut offered: Vec<&str> = branch["properties"]
                .as_object()
                .expect("a branch is an object")
                .keys()
                .map(String::as_str)
                .collect();
            offered.sort_unstable();
            expected.sort_unstable();

            assert_eq!(offered, expected, "`{op}` offers the wrong arguments");
        }
    }
}
