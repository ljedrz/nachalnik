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
//! declare `fs:grep` and `fs:glob`, and the path rules that bind a read bind them too: see
//! [`Looking::barred`], which is the part that had to be built rather than linked.

use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use globset::GlobBuilder;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::WalkBuilder;
use nachalnik::{BoxError, OutputSink, ToolOutput, Verdict};
use serde_json::Value;

use crate::{
    sandbox::{Access, Reach},
    tools::{Careful, Limits, arg, path_matches, truth, whole},
};

/// How many matching lines one `grep` answers with.
///
/// note: matches rather than bytes, and the two are not the same cut. A byte limit takes the tail
/// of the last file searched and leaves a model believing it has seen the whole of the rest; this
/// stops the search and says it stopped, which is a thing the model can act on - narrow the
/// pattern, or name a directory. The byte limit is still in force underneath, as a backstop for
/// the pathological line rather than as the thing that shapes the answer.
pub(super) const MATCHES: usize = 100;

/// How many paths one `glob` answers with.
pub(super) const PATHS: usize = 200;

/// How many lines either side of a match `context` will go to.
///
/// note: said out loud when a call asks for more, rather than clamped quietly. Watched live: a
/// model asked for 20, got ten either side, asked again for 25 and got the same answer back - a
/// request spent on a number nothing had told it was a ceiling. The same lesson as the compaction
/// marker, one tool along: an answer that does not say what it did with your argument reads as an
/// answer to the argument you gave.
const CONTEXT: u64 = 10;

/// How much of one line is shown, in characters.
///
/// note: enough for any line somebody wrote and not enough for a minified one, which is the whole
/// job. `MATCHES * WIDTH` is deliberately under the byte limit these start with, so the two cuts
/// do not both fire on an ordinary answer.
pub(super) const WIDTH: usize = 200;

/// What both tools say about a glob.
///
/// note: one string for the same reason [`PATH_ARG`](crate::tools::files) is one: `grep`'s filter
/// and `glob`'s pattern are the same language, and two descriptions of it are two places for a
/// model to learn two different rules. The clause that earns its keep is the last one - a model
/// that reads `*` as "not across a separator", which is what a shell taught it, writes `**/*.rs`
/// where `*.rs` would have done and `src/*.rs` where it wanted everything under `src`.
pub(super) const GLOB_ARG: &str = "a glob over the whole path, not just the name: `**/*.rs`, `src/**/mod.rs`, \
                        `Cargo.*`. `*` crosses `/`, so `*.rs` finds every Rust file at any depth";

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
/// note: four decisions in here, and each of them is a thing a model would otherwise conclude
/// something false from. Sorted, because the parallel walker's order is nondeterministic and two
/// identical searches answering in two different orders would be two different context items.
/// Hidden files searched, because a model that cannot find `.github/workflows` concludes the file
/// does not exist - an absence it cannot account for is worse than a few extra files opened - and
/// `.git` alone is pruned, because it is a database rather than anything anybody wrote. And the
/// walker does not follow links: each one is yielded as itself, and [`followed`] decides about it
/// one at a time, which is where a link is a question about the *reach* rather than about walking.
///
/// note: `require_git(false)` is the fourth, and it is what makes the tool's own description true.
/// The walker honours a `.gitignore` only inside a git repository by default, and `fs` tells the
/// model it obeys one with no condition attached - so outside a repository a session was handed
/// build output while its own definition said it had been spared it. What a `.gitignore` says is
/// what it says wherever it is found; whether the directory around it has been committed to
/// anything is a fact about a workflow, not about which files somebody meant.
fn walk(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
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
    /// A file with a second name: search it, by its first.
    Read(PathBuf),
    /// A directory the walk reaches by its own name: nothing to do and nothing to say.
    Skip,
    /// Somewhere this session does not reach: count it and say so.
    Refuse,
}

fn followed(reach: &Reach, path: &Path) -> Link {
    match reach.allows(&path.to_string_lossy(), Access::Reading) {
        Err(_) => Link::Refuse,
        Ok(resolved) => match path.is_dir() {
            true => Link::Skip,
            false => Link::Read(resolved),
        },
    }
}

/// The working directory in the shape the walked paths are in, so that one is a prefix of the
/// other.
///
/// note: resolved rather than taken off `Reach`, and this is a Windows fact with a Unix no-op in
/// front of it. The walk root came through `Reach::allows`, which canonicalizes - and on Windows
/// that is an extended-length path, `\\?\C:\…`, whose prefix component is not the one a plain
/// `C:\…` has. So `strip_prefix` matched nothing, and every path in every answer carried the
/// whole of `\\?\C:\Users\…` in front of it, on the one platform nobody here runs.
fn under(workdir: &Path) -> PathBuf {
    workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf())
}

/// What to call a path in an answer: relative to the working directory wherever it is under it,
/// with `/` between the parts whatever the platform writes.
///
/// note: the spelling `read` takes back, and the one the person is looking at in their editor.
/// `Reach::allows` hands back a resolved absolute path, so without the strip every line of every
/// answer would carry the whole of somebody's home directory in front of it.
///
/// note: and one separator, because three things downstream of this compare against what it
/// returns: `path_matches`, which normalises to `/` itself; the `glob` filter, so that `**/*.rs`
/// means the same thing on every platform; and the model, which is handed `/` everywhere else in
/// the answer and hands it back to `read`, where Windows takes it. `MAIN_SEPARATOR` rather than a
/// bare backslash, so that a Unix file whose name really contains one keeps it.
fn relative(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
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
    /// How many matches this file had, which is not how many lines were kept.
    matched: usize,
    /// How many more matches there is room to keep a line for; `None` keeps none of them and
    /// counts every one, which is what `files_only` wants.
    room: Option<usize>,
    /// Set where the file turned out to be binary, which discards the rest.
    binary: bool,
}

impl Lines {
    /// Whether the searcher should keep going through this file.
    ///
    /// note: counting to the end of a file is the point of `files_only`, so a sink keeping no
    /// lines is never full. One keeping them stops when it has as many as the answer has room
    /// for, which is what makes a hundred matches cost a hundred lines and not a file's worth.
    fn wanted(&self) -> bool {
        self.room.is_none_or(|room| room > 0)
    }

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
        if let Some(room) = self.room.as_mut() {
            *room = room.saturating_sub(1);
            self.keep(mat.line_number(), mat.bytes(), ':');
        }

        Ok(self.wanted())
    }

    fn context(&mut self, _searcher: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, io::Error> {
        if self.room.is_some() {
            self.keep(ctx.line_number(), ctx.bytes(), '-');
        }

        Ok(self.wanted())
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
    /// Which files matched and how often, kept whichever way the answer was asked for.
    ///
    /// note: in line mode this is a by-product - one entry per matching file, at most a hundred
    /// of them - and it is what a lines answer too big to send falls back to. Collected always so
    /// that the fallback is a rearrangement of what was already found rather than a second walk.
    by_file: Vec<(String, usize)>,
    /// Whether the room ran out before the walk did.
    full: bool,
    /// Whether somebody stopped it.
    stopped: bool,
}

/// Searches the text of files for a regular expression.
pub(super) struct Grep(pub(super) Looking);

impl Grep {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        let pattern = arg(args, "pattern")?.to_owned();
        let asked = args["path"].as_str().unwrap_or(".").to_owned();
        let root = match self.0.reach.allows(&asked, Access::Reading) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };

        // read the way `files_only` is, so that `"true"` in quotes is not a case-sensitive search
        let ignore_case = match truth(args, "ignore_case") {
            Ok(ignore) => ignore,
            Err(why) => return Ok(ToolOutput::error(why)),
        };
        // note: the regex is built before anything is walked, so a pattern that does not parse
        // costs no directory at all - and the answer is the one thing a model can act on
        // immediately, which is why it says how to spell what it probably meant
        let matcher = match RegexMatcherBuilder::new()
            .case_insensitive(ignore_case)
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
        let only = match args["glob"].as_str() {
            Some(glob) => match GlobBuilder::new(glob).build() {
                Ok(built) => Some(built.compile_matcher()),
                Err(e) => return Ok(ToolOutput::error(format!("`{glob}` is not a glob: {e}"))),
            },
            None => None,
        };

        // note: read rather than reached for. `as_u64().unwrap_or(0)` on an argument a model
        // quoted - `"context": "3"` - is a search that quietly does something else and says it
        // did what was asked, and for `files_only` that is the expensive answer arriving with
        // nothing to explain it. See `tools::whole`
        let wanted = match whole(args, "context", 0) {
            Ok(lines) => lines,
            Err(why) => return Ok(ToolOutput::error(why)),
        };
        let context = wanted.min(CONTEXT) as usize;
        let files_only = match truth(args, "files_only") {
            Ok(only) => only,
            Err(why) => return Ok(ToolOutput::error(why)),
        };
        let barred = self.0.barred();
        let reach = self.0.reach.clone();
        let workdir = under(&reach.workdir);
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
                by_file: Vec::new(),
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
                // opened by the name it was checked under: a link by what it resolved to, since an
                // open that stays beneath the root refuses a link however it was made
                let opening = match kind.is_symlink() {
                    true => match followed(&reach, entry.path()) {
                        Link::Read(resolved) => resolved,
                        Link::Skip => continue,
                        Link::Refuse => {
                            found.skipped.links += 1;
                            continue;
                        }
                    },
                    false => entry.path().to_path_buf(),
                };

                let path = relative(entry.path(), &workdir);
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
                    room: (!files_only).then(|| MATCHES - found.matches),
                    ..Lines::default()
                };
                let searched = reach
                    .open(&opening, Access::Reading)
                    .and_then(|file| searcher.search_file(&matcher, &file, &mut lines));
                if searched.is_err() {
                    found.skipped.unreadable += 1;
                    continue;
                }
                if lines.binary {
                    found.skipped.binary += 1;
                    continue;
                }
                // counted here rather than before the search, so that the two numbers in the
                // answer add up: a file that would not open or turned out to be binary is one of
                // the skipped, and was also being reported as one of the files read through
                found.searched += 1;
                if lines.matched == 0 {
                    continue;
                }

                // every line reaches the screen as it is found, the way a command's output does:
                // a search of a large tree is then visible while it runs rather than at the end
                match files_only {
                    true => sink.push(format!("{}: {}\n", lines.named, lines.matched)),
                    false => {
                        for line in &lines.kept {
                            sink.push(format!("{line}\n"));
                        }
                    }
                }
                found.matches += lines.matched;
                found.files += 1;
                found.by_file.push((lines.named, lines.matched));
                if !files_only {
                    found.lines.extend(lines.kept);
                }

                // the cap is on whichever thing the answer is made of: lines of one file after
                // another, or one line per file
                let full = match files_only {
                    true => found.files >= PATHS,
                    false => found.matches >= MATCHES,
                };
                if full {
                    found.full = true;
                    break;
                }
            }

            // note: by how much each file matched rather than by path, which is the one place
            // here that does not answer in walk order. The question `files_only` is asked is
            // *where does this live*, and the file with twelve matches is the answer to it far
            // more often than the file with one. The path breaks a tie, so it is still the same
            // answer twice for the same tree
            found
                .by_file
                .sort_by(|(a, count), (b, than)| than.cmp(count).then_with(|| a.cmp(b)));
            if files_only {
                found.lines = found.by_file.iter().map(as_a_count).collect();
            }

            found
        })
        .await?;

        let answer = report(&found, &pattern, &asked, files_only, wanted);
        // note: a lines answer that will not fit is answered as the files those lines were in,
        // rather than as the first however-many-thousand bytes of it. Both are less than was
        // found; the difference is that one of them is *true of the whole tree it looked at* and
        // the other is true of whatever the walk reached before the room ran out - measured on a
        // broad pattern here, an answer cut at the limit came entirely from `.github/` and never
        // reached the file the question was about. The advice this tool already gives a capped
        // answer - ask for `files_only` - is the same move, so taking it rather than printing it
        // is one round trip saved and several thousand tokens of lines nobody asked for.
        //
        // note: what is lost is that the lines are not archived beside the shortened copy, the
        // way an output limit's truncation leaves them. They were never handed over: a tool
        // deciding what its answer *is* is a different thing from the kernel shortening one it
        // was given, and this tool has always decided - it stops at a hundred matches and never
        // mentions the hundred and first.
        let over = self
            .0
            .limits
            .of("fs:grep")
            .is_some_and(|limit| answer.len() > limit);
        let answer = match over && !files_only && !found.by_file.is_empty() {
            true => instead(&found, &pattern, &asked, answer.len()),
            false => answer,
        };

        Ok(ToolOutput::new(answer))
    }
}

/// One file and how often it matched, as `files_only` reads it back.
fn as_a_count((path, count): &(String, usize)) -> String {
    format!("{path}: {count}")
}

/// The answer a lines search gives when its lines would not fit: where they were.
///
/// note: it says what happened to the lines and what to do to see some of them, because a model
/// handed a list of files where it asked for lines will otherwise ask the same question again.
/// The two ways out are the two this tool has always named - narrow it, or take one file - and
/// naming them here is what stops the retry.
fn instead(found: &Found, pattern: &str, path: &str, bytes: usize) -> String {
    let head = format!(
        "{} match(es) in {} file(s) for `{pattern}` in {path}{} · the lines came to {bytes} \
         bytes, which is more than this answers with, so here is where they are. Narrow the \
         pattern or name one of these files to read them",
        found.matches,
        found.files,
        // the same caveat a capped lines answer carries, because the same thing happened to it:
        // the walk stopped when the room ran out, so these are the files it reached and not
        // every file that matches
        match found.full {
            true => ", which is as many as this searches for, so there may be more",
            false => "",
        },
    );

    said(
        head,
        [found.skipped.line()],
        &found.by_file.iter().map(as_a_count).collect::<Vec<_>>(),
    )
}

/// What a search says about itself, above the lines it found.
///
/// note: above them, because an output limit cuts from the end - the same thing `shell` learnt
/// about its exit line. A summary under a hundred matches is the first thing a limit takes, and
/// what it leaves is a list of lines with nothing saying how many more there were.
fn report(found: &Found, pattern: &str, path: &str, files_only: bool, wanted: u64) -> String {
    let files = format!("{} file(s) searched", found.searched);
    // what ran out, named as the thing the caller asked for: a search answering with lines fills
    // up with lines, and one answering with files fills up with files
    let full = match files_only {
        true => "that is as many file(s) as this answers with",
        false => "that is as many as this answers with",
    };
    let head = match (found.stopped, found.matches) {
        (true, 0) => format!("stopped before it found anything · {files} so far"),
        (true, n) => format!("stopped before it finished · {n} match(es) so far, {files}"),
        (false, 0) => format!("no matches for `{pattern}` in {path} · {files}"),
        // note: "there may be more" rather than "there are more", which is the difference between
        // a true sentence and one that is usually true: the room can run out on the last match in
        // the tree. What the model is told is the fact it can act on - that the search stopped
        // early - and nothing beyond it
        // note: `files_only` is named first of the four, because it is the one that answers the
        // situation rather than working around it. A capped line answer is filled from the start
        // of the alphabet - measured here, a broad pattern came back entirely from `.github/`
        // and never reached the file the question was about - and `files_only` sees the whole
        // tree for a fraction of the tokens
        (false, n) if found.full => format!(
            "{} · {full}, so there may be more: ask for `files_only` to see where they are, or \
             narrow the pattern, give a path, or pass a `glob`",
            counted(n, found.files, files_only)
        ),
        (false, n) => format!("{} · {files}", counted(n, found.files, files_only)),
    };

    let clamped = (wanted > CONTEXT).then(|| {
        format!("context: {CONTEXT} lines either side is the most this answers with, and you asked for {wanted}")
    });

    said(head, [found.skipped.line(), clamped], &found.lines)
}

/// What a search found, counted the way the caller asked for it.
fn counted(matches: usize, files: usize, files_only: bool) -> String {
    match files_only {
        true => format!("{files} file(s) match, {matches} match(es) in all"),
        false => format!("{matches} match(es) in {files} file(s)"),
    }
}

/// A header, whatever the answer has to account for, and the lines - with nothing left dangling
/// where one of those is empty.
///
/// note: a function because both tools had the same bug in it: an empty list joined onto the
/// header left a trailing newline, which is one byte and the difference between two answers a
/// test can compare and two it cannot.
fn said<const N: usize>(head: String, notes: [Option<String>; N], lines: &[String]) -> String {
    std::iter::once(Some(head))
        .chain(notes)
        .chain(std::iter::once(
            (!lines.is_empty()).then(|| lines.join("\n")),
        ))
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lists the files whose path matches a glob.
pub(super) struct Glob(pub(super) Looking);

impl Glob {
    pub(super) async fn invoke(
        &self,
        args: &Value,
        output: OutputSink,
    ) -> Result<ToolOutput, BoxError> {
        let pattern = arg(args, "pattern")?.to_owned();
        let asked = args["path"].as_str().unwrap_or(".").to_owned();
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
        let workdir = under(&reach.workdir);
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
                        Link::Read(_) => {}
                        Link::Skip => continue,
                        Link::Refuse => {
                            skipped.links += 1;
                            continue;
                        }
                    }
                }

                let path = relative(entry.path(), &workdir);
                // the file the *call* named is one the policy has already been asked about; only
                // what the walk found under it is barred here. See `Looking::barred`
                if entry.path() != root.as_path()
                    && barred.iter().any(|rule| path_matches(rule, &path))
                {
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
            // note: the cap belongs in this arm as much as in the one below it. A walk stopped
            // after its two hundredth path said how many it had found and handed over the first
            // two hundred, with nothing accounting for the difference
            (true, n) if n > paths.len() => format!(
                "stopped before it finished · {n} path(s) so far · the first {} of them",
                paths.len()
            ),
            (true, n) => format!("stopped before it finished · {n} path(s) so far"),
            (false, 0) => format!("nothing matches `{pattern}` under {asked}"),
            (false, n) if n > paths.len() => {
                format!("{n} path(s) · the first {} of them", paths.len())
            }
            (false, n) => format!("{n} path(s)"),
        };
        Ok(ToolOutput::new(said(head, [skipped.line()], &paths)))
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

    /// A path is named with one separator, whichever one the platform walks with.
    ///
    /// note: `join` writes the platform's, so this is `w\\src\\a.rs` on Windows and `w/src/a.rs`
    /// here, and the answer is the same string either way. That matters to three readers: the
    /// `glob` filter, so `**/*.rs` means one thing everywhere; `path_matches`, which normalises to
    /// `/` itself; and the model, which reads `/` in the rest of the answer and hands it back to
    /// `read`, where Windows takes it.
    #[test]
    fn a_named_path_is_written_with_one_separator() {
        let root = PathBuf::from("w");
        assert_eq!(relative(&root.join("src").join("a.rs"), &root), "src/a.rs");
    }

    /// And the separator replaced is the platform's rather than a backslash, so a unix file whose
    /// name really contains one is not quietly renamed.
    #[cfg(unix)]
    #[test]
    fn a_unix_name_that_contains_a_backslash_keeps_it() {
        let root = PathBuf::from("w");
        assert_eq!(relative(&root.join(r"a\b.rs"), &root), r"a\b.rs");
    }

    /// The resolved working directory is a prefix of the paths the walk hands back.
    ///
    /// note: windows-only because it is a Windows fact, and it is the one that sent seven tests
    /// red there while every one of them passed here. `canonicalize` returns `\\?\C:\…`, whose
    /// prefix component is not the one a plain `C:\…` has, so `strip_prefix` matched nothing and
    /// every path in every answer carried the whole of somebody's home directory in front of it.
    #[cfg(windows)]
    #[test]
    fn a_resolved_workdir_is_a_prefix_of_what_the_walk_hands_back() {
        let dir = std::env::temp_dir().join("kamchatka-named");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("a directory to work in");
        std::fs::write(dir.join("src").join("a.rs"), "x").expect("a file");

        let root = under(&dir);
        assert_eq!(relative(&root.join("src").join("a.rs"), &root), "src/a.rs");
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
