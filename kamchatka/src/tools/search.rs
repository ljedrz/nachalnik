//! `grep` and `glob`: finding text, and finding files, with no shell in front of either.
//!
//! note: ripgrep's own engine rather than a search written here or a call out to `rg`. The binary
//! is these four crates plus a printer - `grep-searcher` is the search loop, `grep-regex` the
//! matcher, `ignore` the walker that knows what a `.gitignore` means, and `globset` the patterns
//! both tools filter with - so linking them is not implementing a search, and it costs no `rg` on
//! the machine. The printer is the half this program wants of its own: one that cuts at *matches*
//! rather than at bytes and says how many it did not show, because a search truncated mid-file
//! with nothing accounting for the rest is the thing that makes a model run it three times.
//!
//! note: the point of the pair is which capability they ride. Finding a symbol used to mean
//! `shell`, which subsumes every other capability - so a session that only wanted to be asked
//! about the repository had to hand over the one permission that answers for everything. These
//! declare [`Capability::Read`], and the path rules that bind `read` bind them too: see
//! [`Looking::barred`], which is the part that had to be built rather than linked.

use std::{ffi::OsStr, io, path::Path, sync::Arc};

use globset::GlobBuilder;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::WalkBuilder;
use nachalnik::{
    BoxError, Capability, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, Verdict, async_trait,
};
use serde_json::json;

use crate::{
    sandbox::{Access, Reach},
    tools::{Careful, Limits, arg, files::PATH_ARG, path_matches},
};

/// How many matching lines one `grep` answers with.
///
/// note: matches rather than bytes, and the two are not the same cut. A byte limit takes the tail
/// of the last file searched and leaves a model believing it has seen the whole of the rest; this
/// stops the search and says it stopped, which is a thing the model can act on - narrow the
/// pattern, or name a directory. The byte limit is still in force underneath, as a backstop for
/// the pathological line rather than as the thing that shapes the answer.
const MATCHES: usize = 100;

/// How many paths one `glob` answers with.
const PATHS: usize = 200;

/// How much of one line is shown, in characters.
///
/// note: enough for any line somebody wrote and not enough for a minified one, which is the whole
/// job. `MATCHES * WIDTH` is deliberately under the byte limit these start with, so the two cuts
/// do not both fire on an ordinary answer.
const WIDTH: usize = 200;

/// What both tools need: where they may look, what they may not open, and how much they may say.
///
/// note: the policy is here rather than consulted through the kernel because the question is not
/// "may this call run" - that one was answered before `invoke` - but "may this *file* be opened",
/// once per file in a walk, without asking anybody. [`Careful::paths`] answers it as data.
#[derive(Clone)]
pub(super) struct Looking {
    pub(super) reach: Arc<Reach>,
    pub(super) policy: Arc<Careful>,
    pub(super) limits: Limits,
}

impl Looking {
    /// The path rules a walk has to honour by not opening things, in the order they are consulted.
    ///
    /// note: this is the one thing here that had to be thought about rather than linked, and it is
    /// the reason a search is not simply `read` in a loop. [`Careful::judges`] matches path rules
    /// against the path *in the call*, and a search names a directory: the nine hundred files
    /// under it are never judged, so `.env*: ask` would bind `read` and wave a `grep` through. A
    /// rule that is not `allow` therefore bars the file from the walk, and the answer says how
    /// many it barred - "ask me first" cannot be honoured nine hundred times, and the nearest
    /// honest thing to it is not to read them and to say so.
    ///
    /// note: what this does *not* cover is the path the call itself names. That one goes through
    /// `judges` like any other, so `grep` in `.env` is a question exactly as `read` of it is - and
    /// a file somebody has just answered that question about is not then skipped for the same
    /// rule. The distinction is between what was asked about and what was merely walked over.
    fn barred(&self) -> Vec<String> {
        self.policy
            .paths()
            .into_iter()
            .filter(|(_, verdict)| *verdict != Verdict::Allow)
            .map(|(pattern, _)| pattern)
            .collect()
    }
}

/// What a walk left behind, and why.
///
/// note: counted rather than listed. The point is that an answer accounts for itself - a model
/// that searched for a symbol and found nothing should be able to tell "it is not there" from
/// "eleven files were not opened" - and the names of those eleven are both longer and, for the
/// path rules, exactly what somebody did not want handed over.
#[derive(Default)]
struct Skipped {
    /// Files a path rule says to ask about, which a walk cannot ask about.
    asked: usize,
    /// Links pointing at something outside what this session reaches.
    links: usize,
    /// Files that turned out to be binary.
    binary: usize,
    /// Files that could not be read at all.
    unreadable: usize,
}

impl Skipped {
    /// One line, or nothing when nothing was skipped.
    fn line(&self) -> Option<String> {
        let said: Vec<String> = [
            (self.asked, "file(s) a path rule says to ask about"),
            (self.links, "link(s) pointing out of reach"),
            (self.binary, "binary file(s)"),
            (self.unreadable, "file(s) that could not be read"),
        ]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, why)| format!("{count} {why}"))
        .collect();

        (!said.is_empty()).then(|| format!("skipped: {}", said.join(", ")))
    }
}

/// The walk both tools do, which is the same walk with a different question asked of each file.
///
/// note: three decisions in here, and each of them is a thing a model would otherwise conclude
/// something false from. Sorted, because the parallel walker's order is nondeterministic and two
/// identical searches answering in two different orders would be two different context items.
/// Hidden files searched, because a model that cannot find `.github/workflows` concludes the file
/// does not exist - an absence it cannot account for is worse than a few extra files opened - and
/// `.git` alone is pruned, because it is a database rather than anything anybody wrote. And the
/// walker does not follow links: each one is yielded as itself, and [`followed`] decides about it
/// one at a time, which is where a link is a question about the *reach* rather than about walking.
fn walk(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != OsStr::new(".git"))
        .sort_by_file_path(Path::cmp)
        .build()
}

/// What one symbolic link is: something to read, something to leave alone, or something to count.
///
/// note: refusing every link was the first rule here, and a live run in this repository is what
/// argued it down: five crates each carry a `LICENSE-MIT` link to the file at the root, so every
/// answer to every search led with `skipped: 5 symbolic link(s)` - noise on a line whose whole job
/// is that it is rare, and a claim that something was withheld when nothing was. A link inside the
/// working directory is an ordinary file with a second name.
///
/// note: the question is answered by [`Reach::allows`], which resolves before it compares - the
/// same call, with the same answer, that `read` makes about the same path. So "outside the reach"
/// means here exactly what it means there, rather than being a second opinion that can drift.
///
/// note: a link to a *directory* is left alone rather than descended, and nothing is counted for
/// it. Where it points inside the reach, the walk arrives at those files by their real names
/// anyway, and following it as well would report each of them twice - or walk a cycle for ever.
enum Link {
    /// A file with a second name: search it.
    Read,
    /// A directory the walk reaches by its own name: nothing to do and nothing to say.
    Skip,
    /// Somewhere this session does not reach: count it and say so.
    Refuse,
}

fn followed(reach: &Reach, path: &Path) -> Link {
    match reach.allows(&path.to_string_lossy(), Access::Reading) {
        Err(_) => Link::Refuse,
        Ok(_) => match path.is_dir() {
            true => Link::Skip,
            false => Link::Read,
        },
    }
}

/// What to call a path in an answer: relative to the working directory wherever it is under it.
///
/// note: the spelling `read` takes back, and the one the person is looking at in their editor.
/// `Reach::allows` hands back a resolved absolute path, so without this every line of every
/// answer would carry the whole of somebody's home directory in front of it.
fn named(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Shortens a line too wide for the answer, at the point where it cuts it.
fn cut(line: &str, width: usize) -> String {
    match line.char_indices().nth(width) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_owned(),
    }
}

/// Collects what one file matched on, and stops when the answer is full.
///
/// note: a [`Sink`] of its own rather than `grep_searcher::sinks::UTF8`, which cannot carry
/// context lines and cannot tell the caller how many matches a file had. Both of those are the
/// shape of the answer rather than the search, which is the line this module draws.
#[derive(Default)]
struct Lines {
    /// What to call the file at the front of each line.
    named: String,
    /// The lines, formatted as they will be read.
    kept: Vec<String>,
    /// How many of them are matches rather than context.
    matched: usize,
    /// How many more matches there is room for.
    room: usize,
    /// Set where the file turned out to be binary, which discards the rest.
    binary: bool,
}

impl Lines {
    /// Formats one line the way `grep -n` does: `:` for a match, `-` for the context around it.
    fn keep(&mut self, number: Option<u64>, bytes: &[u8], sep: char) {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_end_matches(['\n', '\r']);
        let at = number.map(|n| n.to_string()).unwrap_or_default();

        self.kept
            .push(format!("{}{sep}{at}{sep}{}", self.named, cut(text, WIDTH)));
    }
}

impl Sink for Lines {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        self.matched += 1;
        self.room = self.room.saturating_sub(1);
        self.keep(mat.line_number(), mat.bytes(), ':');

        Ok(self.room > 0)
    }

    fn context(&mut self, _searcher: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, io::Error> {
        self.keep(ctx.line_number(), ctx.bytes(), '-');

        Ok(self.room > 0)
    }

    /// note: what a binary file matched on is thrown away rather than reported as
    /// `binary file matches`, which is what `grep` says and what a terminal wants. A model cannot
    /// do anything with a line of object code, and the count in the header is the whole of what
    /// the fact is worth here.
    fn binary_data(&mut self, _searcher: &Searcher, _offset: u64) -> Result<bool, io::Error> {
        self.binary = true;

        Ok(false)
    }
}

/// What one search came to.
struct Found {
    /// The lines, in path order, as they will be read.
    lines: Vec<String>,
    /// How many of them are matches.
    matches: usize,
    /// How many files had at least one.
    files: usize,
    /// How many files were opened and read through.
    searched: usize,
    /// What was left out of the walk, and why.
    skipped: Skipped,
    /// Whether the room ran out before the walk did.
    full: bool,
    /// Whether somebody stopped it.
    stopped: bool,
}

/// Searches the text of files for a regular expression.
pub(super) struct Grep(pub(super) Looking);

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        self.0.limits.apply(
            ToolSpec::new(
                "grep",
                format!(
                    "searches the text of files for a regular expression and answers \
                     `path:line:the line`, like `grep -rn`. It walks a directory itself, with no \
                     shell: what a `.gitignore` hides is skipped, and so are `.git`, binary \
                     files, symbolic links and anything a path rule says to ask about - the \
                     answer says how many of each. Hidden files *are* searched. At most \
                     {MATCHES} matches come back, and a line wider than {WIDTH} characters is cut \
                     with a `…`; when the search stops early the answer says so, and a narrower \
                     pattern or a path is what gets the rest."
                ),
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "a regular expression, in Rust's regex syntax - \
                                        `\\bKernel\\b`, `impl .* for`. It has no look-around. \
                                        Escape anything you mean literally: `Vec<u8>\\(`",
                    },
                    "path": {
                        "type": "string",
                        "description": format!(
                            "where to look: one file, or a directory and everything under it. \
                             {PATH_ARG}. Left out, it is the working directory"
                        ),
                    },
                    "glob": {
                        "type": "string",
                        "description": "only files whose path matches this, as a glob: `*.rs`, \
                                        `**/tests/**`, `Cargo.*`",
                    },
                    "ignore_case": {
                        "type": "boolean",
                        "description": "match without regard to case; false by default",
                    },
                    "context": {
                        "type": "integer",
                        "description": "lines to show either side of each match, up to 10; none \
                                        by default. They are marked with a `-` where a match is \
                                        marked with a `:`",
                    },
                },
                "required": ["pattern"],
            }))
            .with_capabilities([Capability::Read]),
        )
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let pattern = arg(&call.args, "pattern")?.to_owned();
        let asked = call.args["path"].as_str().unwrap_or(".").to_owned();
        let root = match self.0.reach.allows(&asked, Access::Reading) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };

        // note: the regex is built before anything is walked, so a pattern that does not parse
        // costs no directory at all - and the answer is the one thing a model can act on
        // immediately, which is why it says how to spell what it probably meant
        let matcher = match RegexMatcherBuilder::new()
            .case_insensitive(call.args["ignore_case"].as_bool().unwrap_or(false))
            .build(&pattern)
        {
            Ok(matcher) => matcher,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "`{pattern}` is not a regular expression this understands: {e}\nEscape \
                     anything you meant literally - `.` `^` `$` `*` `+` `?` `(` `)` `[` `]` `{{` \
                     `}}` `|` `\\` all mean something here."
                )));
            }
        };
        let only = match call.args["glob"].as_str() {
            Some(glob) => match GlobBuilder::new(glob).build() {
                Ok(built) => Some(built.compile_matcher()),
                Err(e) => return Ok(ToolOutput::error(format!("`{glob}` is not a glob: {e}"))),
            },
            None => None,
        };

        let context = call.args["context"].as_u64().unwrap_or(0).min(10) as usize;
        let barred = self.0.barred();
        let reach = self.0.reach.clone();
        let workdir = reach.workdir.clone();
        let sink = output.clone();

        // note: the walk is blocking and `ignore` has no async form, so it goes on a thread that
        // may block - and a blocking task cannot be aborted, which is why the loop asks the sink
        // whether somebody has pressed escape rather than relying on being cancelled. A search of
        // a large tree that nobody can stop is the one way this could be worse than the shell
        let found = tokio::task::spawn_blocking(move || {
            let mut searcher = SearcherBuilder::new()
                .binary_detection(BinaryDetection::quit(b'\x00'))
                .line_number(true)
                .before_context(context)
                .after_context(context)
                .build();

            let mut found = Found {
                lines: Vec::new(),
                matches: 0,
                files: 0,
                searched: 0,
                skipped: Skipped::default(),
                full: false,
                stopped: false,
            };

            for entry in walk(&root) {
                if sink.is_interrupted() {
                    found.stopped = true;
                    break;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => {
                        found.skipped.unreadable += 1;
                        continue;
                    }
                };
                let Some(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    continue;
                }
                if kind.is_symlink() {
                    match followed(&reach, entry.path()) {
                        Link::Read => {}
                        Link::Skip => continue,
                        Link::Refuse => {
                            found.skipped.links += 1;
                            continue;
                        }
                    }
                }

                let path = named(entry.path(), &workdir);
                // the file the *call* named is one the policy has already been asked about; only
                // what the walk found under it is barred here. See `Looking::barred`
                if entry.path() != root.as_path()
                    && barred.iter().any(|rule| path_matches(rule, &path))
                {
                    found.skipped.asked += 1;
                    continue;
                }
                if only.as_ref().is_some_and(|only| !only.is_match(&path)) {
                    continue;
                }

                let mut lines = Lines {
                    named: path,
                    room: MATCHES - found.matches,
                    ..Lines::default()
                };
                found.searched += 1;
                if searcher
                    .search_path(&matcher, entry.path(), &mut lines)
                    .is_err()
                {
                    found.skipped.unreadable += 1;
                    continue;
                }
                if lines.binary {
                    found.skipped.binary += 1;
                    continue;
                }
                if lines.matched == 0 {
                    continue;
                }

                // every line reaches the screen as it is found, the way a command's output does:
                // a search of a large tree is then visible while it runs rather than at the end
                for line in &lines.kept {
                    sink.push(format!("{line}\n"));
                }
                found.matches += lines.matched;
                found.files += 1;
                found.lines.extend(lines.kept);

                if found.matches >= MATCHES {
                    found.full = true;
                    break;
                }
            }

            found
        })
        .await?;

        Ok(ToolOutput::new(report(&found, &pattern, &asked)))
    }
}

/// What a search says about itself, above the lines it found.
///
/// note: above them, because an output limit cuts from the end - the same thing `shell` learnt
/// about its exit line. A summary under a hundred matches is the first thing a limit takes, and
/// what it leaves is a list of lines with nothing saying how many more there were.
fn report(found: &Found, pattern: &str, path: &str) -> String {
    let files = format!("{} file(s) searched", found.searched);
    let head = match (found.stopped, found.matches) {
        (true, 0) => format!("stopped before it found anything · {files} so far"),
        (true, n) => format!("stopped before it finished · {n} match(es) so far, {files}"),
        (false, 0) => format!("no matches for `{pattern}` in {path} · {files}"),
        // note: "there may be more" rather than "there are more", which is the difference between
        // a true sentence and one that is usually true: the room can run out on the last match in
        // the tree. What the model is told is the fact it can act on - that the search stopped
        // early - and nothing beyond it
        (false, n) if found.full => format!(
            "{n} match(es), in {} file(s) · that is as many as this answers with, so there may be \
             more: narrow the pattern, give a path, or pass a `glob`",
            found.files
        ),
        (false, n) => format!("{n} match(es) in {} file(s) · {files}", found.files),
    };

    said(head, found.skipped.line(), &found.lines)
}

/// The three parts of either answer, with nothing left dangling where one of them is empty.
///
/// note: a function because both tools had the same bug in it: an empty list joined onto the
/// header left a trailing newline, which is one byte and the difference between two answers a
/// test can compare and two it cannot.
fn said(head: String, skipped: Option<String>, lines: &[String]) -> String {
    [
        Some(head),
        skipped,
        (!lines.is_empty()).then(|| lines.join("\n")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
}

/// Lists the files whose path matches a glob.
pub(super) struct Glob(pub(super) Looking);

#[async_trait]
impl Tool for Glob {
    fn spec(&self) -> ToolSpec {
        self.0.limits.apply(
            ToolSpec::new(
                "glob",
                format!(
                    "lists the files whose path matches a glob, in alphabetical order. It walks a \
                     directory itself, with no shell, and skips what `grep` skips: what a \
                     `.gitignore` hides, `.git`, symbolic links, and anything a path rule says to \
                     ask about. Hidden files are listed. At most {PATHS} paths come back, and the \
                     answer says how many there were."
                ),
            )
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "a glob over the whole path, not just the name: `**/*.rs`, \
                                        `src/**/mod.rs`, `Cargo.*`. `*` crosses `/`, so `*.rs` \
                                        finds every Rust file at any depth",
                    },
                    "path": {
                        "type": "string",
                        "description": format!(
                            "the directory to list under. {PATH_ARG}. Left out, it is the working \
                             directory"
                        ),
                    },
                },
                "required": ["pattern"],
            }))
            .with_capabilities([Capability::Read]),
        )
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let pattern = arg(&call.args, "pattern")?.to_owned();
        let asked = call.args["path"].as_str().unwrap_or(".").to_owned();
        let root = match self.0.reach.allows(&asked, Access::Reading) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        let matching = match GlobBuilder::new(&pattern).build() {
            Ok(built) => built.compile_matcher(),
            Err(e) => return Ok(ToolOutput::error(format!("`{pattern}` is not a glob: {e}"))),
        };

        let barred = self.0.barred();
        let reach = self.0.reach.clone();
        let workdir = reach.workdir.clone();
        let sink = output.clone();

        let (paths, all, skipped, stopped) = tokio::task::spawn_blocking(move || {
            let mut paths: Vec<String> = Vec::new();
            let mut all = 0usize;
            let mut skipped = Skipped::default();
            let mut stopped = false;

            for entry in walk(&root) {
                if sink.is_interrupted() {
                    stopped = true;
                    break;
                }
                let Ok(entry) = entry else {
                    skipped.unreadable += 1;
                    continue;
                };
                let Some(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    continue;
                }
                if kind.is_symlink() {
                    match followed(&reach, entry.path()) {
                        Link::Read => {}
                        Link::Skip => continue,
                        Link::Refuse => {
                            skipped.links += 1;
                            continue;
                        }
                    }
                }

                let path = named(entry.path(), &workdir);
                if barred.iter().any(|rule| path_matches(rule, &path)) {
                    skipped.asked += 1;
                    continue;
                }
                if !matching.is_match(&path) {
                    continue;
                }

                all += 1;
                if paths.len() < PATHS {
                    sink.push(format!("{path}\n"));
                    paths.push(path);
                }
            }

            (paths, all, skipped, stopped)
        })
        .await?;

        let head = match (stopped, all) {
            (true, n) => format!("stopped before it finished · {n} path(s) so far"),
            (false, 0) => format!("nothing matches `{pattern}` under {asked}"),
            (false, n) if n > paths.len() => {
                format!("{n} path(s) · the first {} of them", paths.len())
            }
            (false, n) => format!("{n} path(s)"),
        };
        Ok(ToolOutput::new(said(head, skipped.line(), &paths)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line too wide for the answer is cut where it says it is cut.
    ///
    /// note: over characters rather than bytes, which is what stops a cut landing inside one. The
    /// third case is the one that panicked before `char_indices`: a line of two-byte characters
    /// is under the limit in characters and over it in bytes.
    #[test]
    fn a_long_line_is_cut_and_says_so() {
        assert_eq!(cut("short", 10), "short");
        assert_eq!(cut("0123456789abc", 10), "0123456789…");
        assert_eq!(cut(&"ł".repeat(12), 10), format!("{}…", "ł".repeat(10)));
    }

    /// Nothing skipped says nothing.
    #[test]
    fn the_skipped_line_is_only_there_when_something_was() {
        assert_eq!(Skipped::default().line(), None);
        assert_eq!(
            Skipped {
                asked: 2,
                binary: 1,
                ..Skipped::default()
            }
            .line()
            .as_deref(),
            Some("skipped: 2 file(s) a path rule says to ask about, 1 binary file(s)")
        );
    }
}
