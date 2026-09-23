//! The three `fs` operations that touch one file, and the argument description every tool with a
//! path shares.
//!
//! note: they run in this process with no shell in front of them, so what keeps them inside the
//! working directory is their own code asking [`Reach`] rather than a kernel refusing an `open`.
//! That is weaker in kind than what `shell` gets and the readmes say so; the reason it is worth
//! having anyway is that a model reads a refusal here in the same words either way.
//!
//! note: every open goes through [`Reach::open`] rather than a path handed to `tokio::fs`, so that
//! what is opened is what was checked; see there.

use std::{path::Path, sync::Arc};

use nachalnik::{BoxError, OutputSink, ToolOutput};
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::sandbox::{Access, Reach};

use crate::tools::{KEPT, arg};

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
/// note: and the literal-`~` spelling is here rather than in that refusal. A refusal is read
/// under pressure to try something else, so every concrete path in one is read as a path to try:
/// offered `./~` beside a refusal about `~/notes.txt`, a model reads `./~`, a file it neither
/// wanted nor had. A schema is read while choosing, which is when a rare spelling is worth knowing
/// and nobody is about to act on it.
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
        match read(&self.0, &path).await {
            Ok(content) => Ok(ToolOutput::new(content)),
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}

/// The whole of a file `allows` answered for, as text - or, past [`KEPT`], a refusal saying how
/// to read a part of it.
///
/// note: one byte past the ceiling is read rather than the size asked for first, because a size is
/// what a file says about itself: `/proc` reports nothing, and a log is larger by the time it has
/// been read.
async fn read(reach: &Reach, path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::from_std(reach.open(path, Access::Reading)?);
    let mut bytes = Vec::new();
    (&mut file)
        .take(KEPT as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > KEPT {
        let size = match file.metadata().await.map(|meta| meta.len()) {
            Ok(size) if size > KEPT as u64 => format!("{size} bytes"),
            _ => "larger".to_owned(),
        };
        return Err(std::io::Error::other(format!(
            "{size}, more than `fs` reads at once ({KEPT} bytes), so it was not read. Search it \
             with `grep`, or read a part of it through `shell` - `head`, `tail`, `sed -n`."
        )));
    }

    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        )
    })
}

/// Replaces the whole of a file `allows` answered for, creating it if it is not there; see
/// [`Reach::replace`] for why that is not an open that empties it.
async fn write(reach: &Arc<Reach>, path: &Path, content: &str) -> std::io::Result<()> {
    let (reach, path, content) = (reach.clone(), path.to_path_buf(), content.to_owned());
    tokio::task::spawn_blocking(move || reach.replace(&path, content.as_bytes()))
        .await
        .map_err(std::io::Error::other)?
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

        match write(&self.0, &path, content).await {
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

        let before = match read(&self.0, &path).await {
            Ok(before) => before,
            Err(e) => return Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        };
        // note: the argument asks for enough of the surrounding lines to name one place, and this
        // checks that it does. `find` takes the first of however many there are, so unchecked, an
        // `old` occurring twice would edit one of them and answer `replaced one occurrence`,
        // which is true of the file and reads as the edit being done. What it costs is the half
        // nobody goes back for: a model that has been told its change landed does not read the
        // file again. An empty `old` is the same failure at the other end - it names position
        // zero and would put `new` at the front of the file.
        if old.is_empty() {
            return Ok(ToolOutput::error(format!(
                "`old` is empty, so it names no text in {}; give the text to replace, or use \
                 `write` for the whole file",
                path.display()
            )));
        }
        // note: overlapping, which `matches` does not count - `\n\n` is in `a\n\n\nb` twice, and
        // counted as once, the edit would go ahead on the first and say it had replaced the one
        let occurrences = {
            let (mut count, mut from) = (0, 0);
            while let Some(found) = before[from..].find(old) {
                count += 1;
                let at = from + found;
                from = at + before[at..].chars().next().map_or(1, char::len_utf8);
            }
            count
        };
        let Some(at) = before.find(old).filter(|_| occurrences == 1) else {
            return Ok(ToolOutput::error(match occurrences {
                0 => format!("`old` does not occur in {}", path.display()),
                n => format!(
                    "`old` occurs {n} times in {} and nothing was changed; include enough of \
                     the lines around the one you mean to name it, or use `write` for the \
                     whole file",
                    path.display()
                ),
            }));
        };

        let after = format!("{}{new}{}", &before[..at], &before[at + old.len()..]);
        match write(&self.0, &path, &after).await {
            Ok(()) => Ok(ToolOutput::new(format!(
                "replaced one occurrence in {}",
                path.display()
            ))),
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}
