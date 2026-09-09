//! The three tools that touch a file, and the argument description all of them share.
//!
//! note: they run in this process with no shell in front of them, so what keeps them inside the
//! working directory is their own code asking [`Reach`] rather than a kernel refusing an `open`.
//! That is weaker in kind than what `shell` gets and the readmes say so; the reason it is worth
//! having anyway is that a model reads a refusal here in the same words either way.

use std::sync::Arc;

use nachalnik::{
    BoxError, Capability, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};

use crate::sandbox::{Access, Reach};
use serde_json::json;

use crate::tools::{Limits, arg};

/// What the three file tools say about the path they take.
///
/// note: one string because it is one rule, and three copies of a sentence is three places to
/// remember when the rule changes. The `~` clause is the half a model cannot work out for itself:
/// these tools run in process with no shell in front of them, so nothing expands it, and the
/// alternative to saying so is a path that quietly becomes a directory called `~` under the
/// working directory. `Reach::allows` says it again at the point of failure, which is the half
/// that actually lands - a description is what makes the refusal legible when it arrives.
///
/// note: and the literal-`~` spelling is here rather than in that refusal, which is where it used
/// to be. A refusal is read under pressure to try something else, so every concrete path in one is
/// read as a path to try: two models answered a refusal about `~/notes.txt` by reading `./~`, a
/// file neither of them wanted and neither of them had. A schema is read while choosing, which is
/// when a rare spelling is worth knowing and nobody is about to act on it.
const PATH_ARG: &str = "absolute, or relative to the working directory. `~` is not expanded - \
                        there is no shell here - so write the path out or use one relative to the \
                        working directory. A file whose name really is `~` is `./~`";

pub(super) struct Read(pub(super) Arc<Reach>, pub(super) Limits);

#[async_trait]
impl Tool for Read {
    fn spec(&self) -> ToolSpec {
        self.1.apply(
            ToolSpec::new(
                "read",
                "reads a whole text file. Long files are cut off at the end; a shell command is \
                 the way to read part of one.",
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": PATH_ARG,
                    },
                },
                "required": ["path"],
            }))
            .with_capabilities([Capability::Read]),
        )
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let path = match self.0.allows(arg(&call.args, "path")?, Access::Reading) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };

        // a failure the model should read and react to, rather than one that stops the loop
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(ToolOutput::new(content)),
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}

pub(super) struct Write(pub(super) Arc<Reach>);

#[async_trait]
impl Tool for Write {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "write",
            "writes a whole file. An existing one is replaced entirely, so `edit` is the safer \
             way to change part of one.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": PATH_ARG,
                },
                "content": { "type": "string", "description": "the whole of the new file" },
            },
            "required": ["path", "content"],
        }))
        .with_capabilities([Capability::Write])
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let (path, content) = (arg(&call.args, "path")?, arg(&call.args, "content")?);
        let path = match self.0.allows(path, Access::Writing) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };

        match tokio::fs::write(&path, content).await {
            Ok(()) => Ok(ToolOutput::new(format!(
                "wrote {} bytes to {}",
                content.len(),
                path.display()
            ))),
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}

pub(super) struct Edit(pub(super) Arc<Reach>);

#[async_trait]
impl Tool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "edit",
            "replaces the first occurrence of `old` with `new` in a file. Nothing is written if \
             `old` is not there.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": PATH_ARG,
                },
                "old": {
                    "type": "string",
                    "description": "the exact text to replace, whitespace included; include \
                                    enough of the surrounding lines to make it the only match",
                },
                "new": { "type": "string", "description": "what to put there instead" },
            },
            "required": ["path", "old", "new"],
        }))
        .with_capabilities([Capability::Edit])
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let (old, new) = (arg(&call.args, "old")?, arg(&call.args, "new")?);
        let path = match self.0.allows(arg(&call.args, "path")?, Access::Writing) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };

        let before = match tokio::fs::read_to_string(&path).await {
            Ok(before) => before,
            Err(e) => return Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        };
        let Some(at) = before.find(old) else {
            return Ok(ToolOutput::error(format!(
                "`old` does not occur in {}",
                path.display()
            )));
        };

        let after = format!("{}{new}{}", &before[..at], &before[at + old.len()..]);
        match tokio::fs::write(&path, after).await {
            Ok(()) => Ok(ToolOutput::new(format!(
                "replaced one occurrence in {}",
                path.display()
            ))),
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}
