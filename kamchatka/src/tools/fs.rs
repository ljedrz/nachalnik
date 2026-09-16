//! `fs`: one tool for the filesystem, and the five things it does to one.
//!
//! note: one tool per *object* rather than per verb. Reading a file, listing files, searching
//! them, writing one and changing part of one are five acts on one thing, and a model choosing
//! between five tools has to know which of them is the filesystem before it can choose at all.
//! The permission subjects come out of the same decision - `fs:read` and `fs:write` are what this
//! declares, so a rule is about the filesystem and an act on it rather than about whichever tool
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
use serde_json::json;

use crate::{
    sandbox::Reach,
    tools::{
        Limits,
        files::{Edit, PATH_ARG, Read, Write},
        search::{GLOB_ARG, Glob, Grep, Looking, MATCHES, PATHS, WIDTH},
    },
};

/// The acts this tool offers, in the order a schema lists them.
///
/// note: the whole of the vocabulary, in one place, because three things have to agree about it -
/// the `action` enum a model chooses from, the subject each call declares, and the row `/limit`
/// keys on. They were three lists when there were five tools, and the only thing keeping them in
/// step was that each tool had one of each.
pub(super) const ACTS: [&str; 5] = ["read", "glob", "grep", "write", "edit"];

/// Everything a session may do to a file, as one tool.
pub(super) struct Fs {
    read: Read,
    glob: Glob,
    grep: Grep,
    write: Write,
    edit: Edit,
    limits: Limits,
}

impl Fs {
    pub(super) fn new(reach: Arc<Reach>, looking: Looking, limits: Limits) -> Self {
        Self {
            read: Read(reach.clone(), limits.clone()),
            glob: Glob(looking.clone()),
            grep: Grep(looking),
            write: Write(reach.clone()),
            edit: Edit(reach),
            limits,
        }
    }

    /// The act a call names, if it names one this tool has.
    fn act<'a>(&self, call: &'a ToolCall) -> Option<&'a str> {
        call.args["action"]
            .as_str()
            .filter(|action| ACTS.contains(action))
    }
}

#[async_trait]
impl Tool for Fs {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "fs",
            format!(
                "the filesystem: `read` a whole text file, `glob` for paths, `grep` inside \
                 files, `write` a whole file, `edit` part of one. `action` says which, and the \
                 arguments it wants are named on each. `glob` and `grep` walk a directory here, \
                 with no shell: they obey `.gitignore`, they do look at hidden files, and what \
                 they passed over is counted at the end. At most {MATCHES} matches or {PATHS} \
                 paths come back, a line wider than {WIDTH} characters is cut with a `…`, and an \
                 answer that stopped early says so and says what to do about it."
            ),
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ACTS,
                    "description": "`read` needs `path`. `glob` and `grep` need `pattern` and \
                                    take `path` for where to look. `write` needs `path` and \
                                    `content`. `edit` needs `path`, `old` and `new`.",
                },
                "path": {
                    "type": "string",
                    "description": format!(
                        "the file to act on, or for `glob` and `grep` where to look - one file, \
                         or a directory and everything under it, the working directory if left \
                         out. {PATH_ARG}"
                    ),
                },
                "pattern": {
                    "type": "string",
                    "description": format!(
                        "for `grep`, a regular expression in Rust's regex syntax - \
                         `\\bKernel\\b`, `impl .* for`. It has no look-around; escape anything \
                         you mean literally: `Vec<u8>\\(`. For `glob`, {GLOB_ARG}"
                    ),
                },
                "content": { "type": "string", "description": "for `write`: the whole new file" },
                "old": {
                    "type": "string",
                    "description": "for `edit`: the exact text to replace, whitespace included; \
                                    include enough of the surrounding lines to make it the only \
                                    match",
                },
                "new": {
                    "type": "string",
                    "description": "for `edit`: what to put there instead",
                },
                "glob": { "type": "string", "description": GLOB_ARG },
                "ignore_case": {
                    "type": "boolean",
                    "description": "for `grep`: match without regard to case; false by default",
                },
                "context": {
                    "type": "integer",
                    "description": "for `grep`: lines to show either side of each match, up to \
                                    10; none by default. They are marked with a `-` where a \
                                    match is marked with a `:`",
                },
                "files_only": {
                    "type": "boolean",
                    "description": "for `grep`: answer with the files that match and how many \
                                    each has - `path: 12`, most first - instead of the lines, \
                                    which is `grep -l`. For a common word, or when you do not \
                                    know where something lives: a fraction of the tokens, and it \
                                    names the file to search properly next",
                },
            },
            "required": ["action"],
        }))
        .with_capabilities(ACTS.map(Capability::fs))
    }

    /// note: `action` and nothing else, so a rule about `fs:read` is about reading whichever way
    /// a call asked for it. A call naming no act it has declares all five, which is the strictest
    /// reading of a call nobody can place - and `invoke` then refuses it by name.
    fn needs(&self, call: &ToolCall) -> Vec<Capability> {
        match self.act(call) {
            Some(action) => vec![Capability::fs(action)],
            None => self.spec().capabilities,
        }
    }

    /// note: per act, because `/limit` is about how much of an answer is worth reading and the
    /// answers here are not the same size. A whole file and a repo-wide search were two rows
    /// while they were two tools, and collapsing them into one number would have been the merge
    /// quietly taking something away.
    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits
            .of(&format!("fs:{}", self.act(call).unwrap_or("read")))
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        // note: the act is checked here as well as in `needs`, because a refusal a model can act
        // on is one that names what was wrong. The alternative - dispatching on a default - runs
        // something nobody asked for
        let Some(action) = self.act(call) else {
            return Ok(ToolOutput::error(format!(
                "`{}` is not something `fs` does; it does {}",
                call.args["action"].as_str().unwrap_or("nothing"),
                ACTS.join(", ")
            )));
        };

        match action {
            "read" => self.read.invoke(call, output).await,
            "glob" => self.glob.invoke(call, output).await,
            "grep" => self.grep.invoke(call, output).await,
            "write" => self.write.invoke(call, output).await,
            _ => self.edit.invoke(call, output).await,
        }
    }
}
