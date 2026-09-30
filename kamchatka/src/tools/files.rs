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

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

use crate::sandbox::{Access, Reach};
use nachalnik::{BoxError, Capability, OutputSink, ToolOutput, Verdict};
use serde_json::Value;

use crate::tools::{CEILING, Careful, KEPT, Limits, Subject, arg, number};

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
/// wrote - and where the rule asks, the answer names the target, so asking for it by that name is
/// one call away and is asked like any other. Where the rule refuses, the answer says that name is
/// refused too, since a model told what a rule is about reads it as a way round.
pub(super) fn linked(
    named: &str,
    resolved: &Path,
    reach: &Reach,
    policy: &Careful,
    doing: &str,
) -> Option<String> {
    let barred = super::search::barred(policy);
    let (rule, verdict) = super::search::led_past(named, resolved, &reach.workdir, &barred)?;
    let target = resolved
        .strip_prefix(
            reach
                .workdir
                .canonicalize()
                .unwrap_or_else(|_| reach.workdir.clone()),
        )
        .unwrap_or(resolved);

    Some(match verdict {
        Verdict::Deny => format!(
            "`{named}` leads to `{}`, which the path rule for `{rule}` refuses, so nothing was \
             {doing}. Asked for by that name it is refused as well.",
            target.display(),
        ),
        _ => format!(
            "`{named}` leads to `{}`, which the path rule for `{rule}` has not allowed, so nothing \
             was {doing}. A rule is about the name a file is asked for by: name it as `{}` and it \
             is asked about like any other.",
            target.display(),
            target.display(),
        ),
    })
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

/// The largest file `read` counts the lines of to the end, in bytes, to say how many it has.
///
/// note: its own bound rather than [`KEPT`], because counting keeps nothing: it is a pass over the
/// bytes, and what `KEPT` bounds is what this program holds. Bounded by `KEPT`, a 9 MB log said
/// which lines it was showing and not how many there were, so a model asked for its last line
/// could only page forward 32 KB a request until the turn's budget ran out; told `of 120000`, it
/// reads `from: 120000`. Past this a pass is long enough to be worth not taking unasked.
pub const COUNTED: u64 = 64 * 1024 * 1024;

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
/// time; what [`COUNTED`] bounds is the counting. A file larger than that is not read to its end
/// to say how many lines it has, and the answer says that more follow instead of how many.
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
    let countable = file.metadata().is_ok_and(|meta| meta.len() <= COUNTED);
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
        return Ok(match (number, span.whole()) {
            // a file with nothing in it read whole is a file with nothing in it: an answer rather
            // than a refusal, in the brackets a read's own header goes in, so that it cannot be
            // taken for a file holding those words
            (0, true) => Ok("[the file is empty]".to_owned()),
            (0, false) => Err("the file is empty, so there is no line to start from".to_owned()),
            _ => Err(format!(
                "`from` is line {} and the file has {number} line(s); it starts at 1",
                span.from
            )),
        });
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
        // a byte that is not UTF-8 names the line it is in and where in that line, and `kept`
        // holds whole lines from `first` on - so the line is the one that many newlines before
        // it, and where in it is the one after the last of them
        Err(e) => {
            let at = e.utf8_error().valid_up_to();
            let before = &e.as_bytes()[..at];
            let line = first + before.iter().filter(|byte| **byte == b'\n').count() as u64;
            let within = match before.iter().rposition(|byte| *byte == b'\n') {
                Some(last) => at - last - 1,
                None => at,
            };
            return Err(untext(Some((line, within)), &e.utf8_error()));
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

    String::from_utf8(bytes).map_err(|e| untext(None, &e.utf8_error()))
}

/// What a file that is not UTF-8 is answered with, naming where it stops being text - the line
/// and where in it for a `read`, which is counting lines, and the byte for an `edit`, which is
/// not - and what to do about it.
///
/// note: `stream did not contain valid UTF-8` is the whole of what was said, and it is neither
/// the line nor the byte: it is the operating system on a stream, where a model cannot tell a
/// file with one Latin-1 byte in it from a binary one, so it has nothing to search and reaches
/// for `shell` to find out.
///
/// note: the two ways out are named, and both can be told about without knowing the policy:
/// `grep` reads bytes and says how a file came to be binary, and `shell` is a tool that reads
/// them as bytes. Whether `shell` may run is the policy's answer, not this one's. Whether `read`
/// showed nothing is also this one's, and `edit` says the same sentence about a file it changed
/// no line of, which is the point.
fn untext(line: Option<(u64, usize)>, fault: &std::str::Utf8Error) -> std::io::Error {
    let where_ = match line {
        Some((line, within)) => format!("byte {within} of line {line} of it"),
        None => format!("byte {} of it", fault.valid_up_to()),
    };
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "not text: {where_} is not UTF-8, so nothing was shown and nothing was changed. \
             `grep` searches a file whatever it holds, and `shell` can read it as bytes",
        ),
    )
}

/// Replaces the whole of a file `allows` answered for, creating it if it is not there; see
/// [`Reach::replace`] for why that is not an open that empties it.
async fn write(reach: &Arc<Reach>, path: &Path, content: &str) -> std::io::Result<()> {
    let (reach, path, content) = (reach.clone(), path.to_path_buf(), content.to_owned());
    tokio::task::spawn_blocking(move || reach.replace(&path, content.as_bytes()))
        .await
        .map_err(std::io::Error::other)?
}

/// One lock for each file a `write` or an `edit` is changing, by the path it resolved to.
///
/// note: `edit` reads the whole file, changes it and writes it back, so two edits of one file in a
/// parallel batch each read the old contents and the second rename keeps only its own change -
/// and both answer that they replaced one occurrence. Held from the read through the rename, the
/// second reads what the first wrote. Keyed by the resolved path, so that two names for one file
/// are one lock; a lock nobody holds is dropped the next time one is taken.
#[derive(Default)]
pub(super) struct Changing(parking_lot::Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>);

impl Changing {
    /// Waits until nothing else here is changing `path`, and keeps it that way until dropped.
    async fn hold(&self, path: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut held = self.0.lock();
            held.retain(|_, lock| lock.strong_count() > 0);
            match held.get(path).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(tokio::sync::Mutex::new(()));
                    held.insert(path.to_path_buf(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }
}

pub(super) struct Write(
    pub(super) Arc<Reach>,
    pub(super) Arc<Careful>,
    pub(super) Arc<Changing>,
);

impl Write {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        _output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        let (named, content) = (arg(args, "path")?, arg(args, "content")?);
        if let Some(refusal) = dir(named, "written", &self.1) {
            return Ok(ToolOutput::error(refusal));
        }
        let path = match self.0.allows_under(named, Access::Writing, &self.1) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        if let Some(refusal) = linked(named, &path, &self.0, &self.1, "written") {
            return Ok(ToolOutput::error(refusal));
        }

        let _changing = self.2.hold(&path).await;
        match write(&self.0, &path, content).await {
            Ok(()) => Ok(ToolOutput::new(format!(
                "wrote {} bytes to {}",
                content.len(),
                path.display()
            ))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(ToolOutput::error(match unmade(&path, &self.1) {
                    Some(refusal) => refusal,
                    None => format!("{}: {e}", path.display()),
                }))
            }
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}

/// Why a write found nowhere to put its file, naming the first directory on the way that is not
/// there, or `None` where they all are.
///
/// note: the system says `No such file or directory` about the file, which is the one part of the
/// path a write was never going to find, and nothing in it says `fs` makes no directories. What to
/// do next depends on whether `shell` may run, since that is what makes one.
fn unmade(path: &Path, policy: &Careful) -> Option<String> {
    let dir = path.parent()?;
    let missing = dir.ancestors().take_while(|it| !it.exists()).last()?;
    let next = match makes_directories(policy) {
        false => "`shell`, which makes directories, is refused in this session, so write it in a \
             directory that is there, or say which one you need made."
            .to_owned(),
        true => format!(
            "Make it with `shell` - `mkdir -p {}` - and write again.",
            dir.display()
        ),
    };

    Some(format!(
        "{}: the directory {} is not there, and `fs` makes no directories, so nothing was \
         written. {next}",
        path.display(),
        missing.display(),
    ))
}

/// Why a `write` or an `edit` was handed a path that ends in a separator, which is a directory and
/// no file; `None` where it does not.
///
/// note: refused rather than resolved to the name without it, which is what a path goes through
/// here: a model writing `trail/` means the directory, and `fs` makes no directories - so
/// resolving it would make a file called `trail`, which nothing asked for and which stands there
/// answering as a directory's worth of what was meant to go in one. Checked on the text and
/// before the reach, so a path outside it is still refused as the path it is.
///
/// note: on a relative path, and only there. An absolute one names a directory that is there,
/// which the open refuses by name; a name that is not a file - `.`, `..` - is one nobody ends a
/// path with a separator after.
///
/// note: the directory is offered to `shell` only where `shell` may run, as [`unmade`] offers it.
fn dir(named: &str, doing: &str, policy: &Careful) -> Option<String> {
    let without = named.trim_end_matches('/');
    if without.is_empty() || !named.ends_with('/') || std::path::Path::new(without).is_absolute() {
        return None;
    }
    let or = match makes_directories(policy) {
        true => format!(" - or make the directory with `shell` - `mkdir -p {named}`."),
        false => "; `shell`, which makes directories, is refused in this session.".to_owned(),
    };

    Some(format!(
        "`{named}` ends in a separator, so it is a directory, and a directory is not something to \
         be {doing}. Say the file you mean - `{without}`{or}"
    ))
}

/// Whether a refusal may send the model to `shell` to make a directory `fs` will not.
fn makes_directories(policy: &Careful) -> bool {
    policy.stance(&Subject::Capability(Capability::exec("run"))) != Verdict::Deny
}

/// How to add text with an edit, for the two refusals of an `edit` that names nothing to replace.
const INSERTING: &str = "`edit` replaces `old` with `new`, so to add text, put a line it goes \
                         next to in `old` and that line with the addition in `new`";

pub(super) struct Edit(
    pub(super) Arc<Reach>,
    pub(super) Arc<Careful>,
    pub(super) Arc<Changing>,
);

impl Edit {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        _output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        // note: said with how to add text, because that is what an `edit` with a `new` and no
        // `old` is almost always trying to do, and "required" alone sends a model back with the
        // same call and an `old` it has to invent
        if args["old"].is_null() && !args["new"].is_null() {
            return Ok(ToolOutput::error(format!(
                "the `old` argument is required, and nothing was done; {INSERTING}"
            )));
        }
        let (old, new) = (arg(args, "old")?, arg(args, "new")?);
        let named = arg(args, "path")?;
        if let Some(refusal) = dir(named, "changed", &self.1) {
            return Ok(ToolOutput::error(refusal));
        }
        let path = match self.0.allows_under(named, Access::Writing, &self.1) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        if let Some(refusal) = linked(named, &path, &self.0, &self.1, "changed") {
            return Ok(ToolOutput::error(refusal));
        }

        let _changing = self.2.hold(&path).await;
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
                 `write` for the whole file. {INSERTING}",
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
                0 => format!(
                    "`old` does not occur in {}{}",
                    path.display(),
                    // note: a file written on another machine, or by a tool that had its own idea
                    // of a line ending, holds `\r\n` where a model writes `\n` - and `old` is
                    // exact text, so the refusal is a spelling and not a place that is not there.
                    // Said here rather than in the argument's description, because a model reads
                    // a description once and this once, having tried it
                    eol(&before, old)
                ),
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

/// What an `edit` whose `old` does not occur says about the line endings, where the same text
/// spelled for the file's own does occur; nothing where it does not.
///
/// note: the two differ only in a `\r` before an `\n`, so the text is in the file under one
/// spelling and not the other exactly when one of them is there and the other is not. What to do
/// about it is said as the file's own spelling, which `read` has just shown the model.
fn eol(before: &str, old: &str) -> String {
    // a single line with no line ending in it cannot be spelled wrong, and a file with lines of
    // both endings has no one spelling to be told about
    if !old.contains('\n') && !old.contains('\r') {
        return String::new();
    }
    let (crlf, lf) = (as_eol(old, "\r\n"), as_eol(old, "\n"));

    match (before.contains(&crlf), before.contains(&lf)) {
        (true, false) => format!(
            " - the file ends its lines with CRLF, where `old` has {}, so spell `old` the file's \
             way",
            if old.contains('\r') { "CRLF" } else { "LF" }
        ),
        (false, true) => " - the file ends its lines with LF, where `old` has CRLF, so spell \
                          `old` the file's way"
            .to_owned(),
        _ => String::new(),
    }
}

/// `old` with every line ending in it spelled `ending`.
fn as_eol(old: &str, ending: &str) -> String {
    old.replace("\r\n", "\n").replace(['\r', '\n'], ending)
}
