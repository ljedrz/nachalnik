//! The three tools that touch one file, and the argument description every tool with a path
//! shares.
//!
//! note: they run in this process with no shell in front of them, so what keeps them inside the
//! working directory is their own code asking [`Reach`] rather than a kernel refusing an `open`.
//! That is weaker in kind than what `shell` gets and the readmes say so; the reason it is worth
//! having anyway is that a model reads a refusal here in the same words either way.

use std::sync::Arc;

use nachalnik::{BoxError, OutputSink, ToolOutput};
use serde_json::Value;

use crate::sandbox::{Access, Reach};

use crate::tools::arg;

/// What every tool here says about the path it takes.
///
/// note: one string because it is one rule, and a copy per tool is a place to remember when the
/// rule changes - which is why `grep` and `glob` read it too, though neither opens the path it is
/// handed the way these three do. The `~` clause is the half a model cannot work out for itself:
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
pub(super) const PATH_ARG: &str = "absolute, or relative to the working directory. `~` is not \
                        expanded - there is no shell here - and a path starting with one is \
                        refused; a file whose name really is `~` is `./~`";

pub(super) struct Read(pub(super) Arc<Reach>);

impl Read {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        _output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        let path = match self.0.allows(arg(args, "path")?, Access::Reading) {
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

impl Write {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        _output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        let (path, content) = (arg(args, "path")?, arg(args, "content")?);
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

impl Edit {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        _output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        let (old, new) = (arg(args, "old")?, arg(args, "new")?);
        let path = match self.0.allows(arg(args, "path")?, Access::Writing) {
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
