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

use crate::sandbox::{Access, Reach};
use nachalnik::{BoxError, OutputSink, ToolOutput};
use serde_json::Value;

use crate::tools::{CEILING, Careful, KEPT, Limits, arg, number};

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
/// note: "`fs` does not go through a shell" rather than "there is no shell here", which is the same
/// fact about `fs` and reads as one about the session: a live model offered `shell` beside it
/// said it had no way to run a command.
///
/// note: and the literal-`~` spelling is here rather than in that refusal. A refusal is read
/// under pressure to try something else, so every concrete path in one is read as a path to try:
/// offered `./~` beside a refusal about `~/notes.txt`, a model reads `./~`, a file it neither
/// wanted nor had. A schema is read while choosing, which is when a rare spelling is worth knowing
/// and nobody is about to act on it.
pub(super) const PATH_ARG: &str = "absolute, or relative to the working directory. `~` is not \
                        expanded, since `fs` does not go through a shell, and a path starting \
                        with one is refused; a file whose name really is `~` is `./~`";

pub(super) struct Read(
    pub(super) Arc<Reach>,
    pub(super) Limits,
    pub(super) Arc<Careful>,
);

impl Read {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        _output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        let named = arg(args, "path")?;
        let path = match self.0.allows_under(named, Access::Reading, &self.2) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        if let Some(refusal) = linked(named, &path, &self.0, &self.2, "read") {
            return Ok(ToolOutput::error(refusal));
        }
        let span = match Span::of(args) {
            Ok(span) => span,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        // the row `/limit fs:read` changes, read afresh for every call as the kernel reads it
        let budget = self.1.of("fs:read").unwrap_or(CEILING);

        let (reach, opened) = (self.0.clone(), path.clone());
        let read = tokio::task::spawn_blocking(move || read(&reach, &opened, span, budget))
            .await
            .map_err(std::io::Error::other);
        // a failure the model should read and react to, rather than one that stops the loop
        match read.and_then(|read| read) {
            Ok(Ok(content)) => Ok(ToolOutput::new(content)),
            Ok(Err(refusal)) => Ok(ToolOutput::error(refusal)),
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}

/// Why a path is refused for leading, through a link, to a file a path rule has not allowed; see
/// [`led_past`](super::search::led_past).
///
/// note: refused rather than asked about, because the question would be about a name nobody
/// wrote - and the answer names the target, so asking for it by that name is one call away and is
/// asked like any other.
pub(super) fn linked(
    named: &str,
    resolved: &Path,
    reach: &Reach,
    policy: &Careful,
    doing: &str,
) -> Option<String> {
    let barred = super::search::barred(policy);
    let rule = super::search::led_past(named, resolved, &reach.workdir, &barred)?;
    let target = resolved
        .strip_prefix(
            reach
                .workdir
                .canonicalize()
                .unwrap_or_else(|_| reach.workdir.clone()),
        )
        .unwrap_or(resolved);

    Some(format!(
        "`{named}` leads to `{}`, which the path rule for `{rule}` has not allowed, so nothing was \
         {doing}. A rule is about the name a file is asked for by: name it as `{}` and it is asked \
         about like any other.",
        target.display(),
        target.display(),
    ))
}

/// Which lines of a file a `read` asked for.
#[derive(Debug, Clone, Copy)]
struct Span {
    /// The first, counting from 1.
    from: u64,
    /// How many, or every one to the end.
    lines: Option<u64>,
}

impl Span {
    /// What the call asked for, or why that is not something to read.
    fn of(args: &Value) -> Result<Self, String> {
        let nothing = " Nothing was read, rather than something other than you asked for.";
        let from = number(args, "from").map_err(|refusal| format!("{refusal}{nothing}"))?;
        let lines = number(args, "lines").map_err(|refusal| format!("{refusal}{nothing}"))?;
        if from == Some(0) {
            return Err(format!(
                "`from` counts lines from 1, so 0 is no line.{nothing}"
            ));
        }
        if lines == Some(0) {
            return Err(format!(
                "`lines` is how many to read, and 0 reads none; leave it out to read to the end.\
                 {nothing}"
            ));
        }

        Ok(Self {
            from: from.unwrap_or(1),
            lines,
        })
    }

    /// Whether this is the whole file, which is what a call naming neither argument asks for.
    fn whole(&self) -> bool {
        self.from == 1 && self.lines.is_none()
    }
}

/// Room kept under the output limit for the line saying which lines these are.
const HEADER: usize = 256;

/// The lines `span` names of a file `allows` answered for, as text, stopping at the last whole
/// line that fits `budget` and saying where to read on from; or a sentence saying why there is
/// nothing to show.
///
/// note: cut here, at a line, rather than left to the kernel's output limit, which cuts at a byte
/// and says only how many went. A model shown that has to guess where the file broke off and read
/// on through `shell` - an `exec:run` call a session may be asking about, for a file it is allowed
/// to read. Cut here, the answer names the next line, and `from` reads on from it inside `fs:read`.
///
/// note: line by line, keeping only what is shown, so a file of any size can be read a part at a
/// time; what [`KEPT`] bounds is the counting. A file larger than that is not read to its end to
/// say how many lines it has, and the answer says that more follow instead of how many.
///
/// note: the whole file with nothing added where it fits and nothing narrower was asked for, which
/// is the answer `read` has always given: a line of bookkeeping on every small file is a toll on
/// the common case for the sake of the rare one.
fn read(
    reach: &Reach,
    path: &Path,
    span: Span,
    budget: usize,
) -> std::io::Result<Result<String, String>> {
    let file = reach.open(path, Access::Reading)?;
    // a size is what a file says about itself, and `/proc` says nothing - so this decides only
    // whether to count to the end, and a file claiming less than it holds is counted anyway
    let countable = file.metadata().is_ok_and(|meta| meta.len() <= KEPT as u64);
    let mut reader = std::io::BufReader::new(file);
    let room = budget.saturating_sub(HEADER);

    let (mut number, mut kept, mut shown) = (0u64, Vec::new(), None::<(u64, u64)>);
    let (mut line, mut ended) = (Vec::new(), false);
    // what stopped the reading short of the span, if something did
    let mut stopped = None;
    let last = span.lines.map(|lines| span.from.saturating_add(lines - 1));
    loop {
        line.clear();
        let this = number + 1;
        let inside = this >= span.from && last.is_none_or(|last| this <= last);
        let keep = match inside && stopped.is_none() {
            true => room.saturating_sub(kept.len()),
            false => 0,
        };
        let Some(length) = next_line(&mut reader, &mut line, keep)? else {
            ended = true;
            break;
        };
        number = this;
        if this < span.from {
            continue;
        }
        if !inside || stopped.is_some() {
            // past the span, or past what fits: counted to the end where that is cheap
            match countable {
                true => continue,
                false => break,
            }
        }
        if line.len() < length {
            stopped = Some(match kept.is_empty() {
                // a line longer than everything this may show: its start, said to be one
                true => {
                    kept.append(&mut line);
                    shown = Some((this, this));
                    Stop::Long
                }
                false => Stop::Full(this),
            });
            continue;
        }
        kept.append(&mut line);
        shown = Some((shown.map_or(this, |(first, _)| first), this));
    }
    // the end was reached, so the count is the file's, whichever way it went
    let total = ended.then_some(number);

    let Some((first, through)) = shown else {
        return Ok(Err(match number {
            0 => "the file is empty, so there is no line to start from".to_owned(),
            _ => format!(
                "`from` is line {} and the file has {number} line(s); it starts at 1",
                span.from
            ),
        }));
    };
    let text = match String::from_utf8(kept) {
        Ok(text) => text,
        // a line cut short can end part-way through a character, which is the cut and not the file
        Err(e) if stopped == Some(Stop::Long) && e.utf8_error().error_len().is_none() => {
            let valid = e.utf8_error().valid_up_to();
            let mut kept = e.into_bytes();
            kept.truncate(valid);
            String::from_utf8(kept).expect("cut where it stops being valid")
        }
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            ));
        }
    };

    let of = match total {
        Some(total) => format!(" of {total}"),
        None => String::new(),
    };
    let header = match stopped {
        None if span.whole() => return Ok(Ok(text)),
        None => match total {
            Some(_) => format!("[lines {first}-{through}{of}]"),
            None => format!("[lines {first}-{through}, and more after them]"),
        },
        Some(Stop::Full(next)) => format!(
            "[lines {first}-{through}{of}: the output limit ({budget} bytes) stops it there - \
             read on with `from: {next}`]"
        ),
        Some(Stop::Long) => format!(
            "[line {first}{of} is longer than the output limit ({budget} bytes), so this is its \
             start; `grep` finds what is in it, and `shell` can read the rest]"
        ),
    };

    Ok(Ok(format!("{header}\n{text}")))
}

/// What stopped a `read` short of the lines it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The output limit, with this line the first that did not fit.
    Full(u64),
    /// The first line asked for did not fit on its own.
    Long,
}

/// Reads one line into `into`, keeping at most `keep` bytes of it, and says how long the whole
/// line was; `None` at the end of the file.
///
/// note: a line is read through whatever its length, so a file with one enormous line is walked
/// rather than held.
fn next_line(
    reader: &mut impl std::io::BufRead,
    into: &mut Vec<u8>,
    keep: usize,
) -> std::io::Result<Option<usize>> {
    let mut length = 0;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok((length > 0).then_some(length));
        }
        let (took, ended) = match chunk.iter().position(|byte| *byte == b'\n') {
            Some(at) => (at + 1, true),
            None => (chunk.len(), false),
        };
        let room = keep.saturating_sub(into.len());
        into.extend_from_slice(&chunk[..took.min(room)]);
        reader.consume(took);
        length += took;
        if ended {
            return Ok(Some(length));
        }
    }
}

/// The whole of a file `edit` is to change, as text - or, past [`KEPT`], a refusal saying how else
/// to change it.
///
/// note: one byte past the ceiling is read rather than the size asked for first, because a size is
/// what a file says about itself: `/proc` reports nothing, and a log is larger by the time it has
/// been read.
async fn whole(reach: &Reach, path: &Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt as _;

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
            "{size}, more than `fs` edits at once ({KEPT} bytes), so it was not changed. Change it \
             through `shell` - `sed -i`."
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

pub(super) struct Write(pub(super) Arc<Reach>, pub(super) Arc<Careful>);

impl Write {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        _output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        let (named, content) = (arg(args, "path")?, arg(args, "content")?);
        let path = match self.0.allows_under(named, Access::Writing, &self.1) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        if let Some(refusal) = linked(named, &path, &self.0, &self.1, "written") {
            return Ok(ToolOutput::error(refusal));
        }

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

pub(super) struct Edit(pub(super) Arc<Reach>, pub(super) Arc<Careful>);

impl Edit {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        _output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        let (old, new) = (arg(args, "old")?, arg(args, "new")?);
        let named = arg(args, "path")?;
        let path = match self.0.allows_under(named, Access::Writing, &self.1) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        if let Some(refusal) = linked(named, &path, &self.0, &self.1, "changed") {
            return Ok(ToolOutput::error(refusal));
        }

        let before = match whole(&self.0, &path).await {
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
