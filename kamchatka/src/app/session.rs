//! A session on disk: `/load` and `/save`, the name a session is written under, and the write
//! that puts a file in place whole or not at all.

use std::collections::{HashMap, HashSet};

use nachalnik::{Block, Content, ContextItem, ContextKind, ContextState, ToolCallId};

use super::{
    App, Speaker,
    text::{MID_TURN, plural},
};

impl App {
    /// A name for a session, from the seconds since the epoch it started at.
    ///
    /// note: this is the session's identity *and* the name of the two files it leaves behind.
    /// Those go in a directory called `kamchatka`, so the program's name in a filename would say
    /// what the directory already says, and a bare count of seconds says nothing to anybody
    /// reading it. `2026-09-08T06-45-17Z` names the session, sorts the same way, and answers the
    /// question somebody is looking at a list of them to ask.
    ///
    /// note: UTC, and it says so, because a local time would be a name that quietly means
    /// something different depending on where it was written.
    ///
    /// note: to the second, so two sessions started inside one second collide. The identifier a
    /// session gets from the runtime by default is a counter that restarts with the process,
    /// which is fine as an identity and writes over the last session's record.
    pub fn session_stamp(secs: u64) -> String {
        // note: the epoch where the seconds are past what the calendar holds, year 9999, which is
        // not a moment this program is started at
        let at = i64::try_from(secs)
            .ok()
            .and_then(|secs| time::OffsetDateTime::from_unix_timestamp(secs).ok())
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);

        format!(
            "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}Z",
            at.year(),
            u8::from(at.month()),
            at.day(),
            at.hour(),
            at.minute(),
            at.second()
        )
    }

    /// Writes the event log and a resumable snapshot, and says how many records that was.
    ///
    /// note: separate from `save` because the last write of a session happens after the terminal
    /// has been restored, where `say` has nowhere to put a sentence. Both go through here so that
    /// what `/save` produces and what a session leaves behind on its way out are the same pair of
    /// files, written the same way.
    ///
    /// note: the snapshot first, and the log only up to the record it names. A snapshot reads the
    /// log's last number under the lock every change to the context is announced under, so those
    /// records are exactly the ones whose changes it shows - and a pair written while a turn runs
    /// agrees rather than holding items whose `context.added` came after the log was read.
    ///
    /// note: each file is written beside itself, flushed to disk and renamed over, both before
    /// either is renamed. `/save good` a second time is the checkpoint `/load good` is for, and an
    /// overwrite that ran out of disk half way would have destroyed the one it was replacing.
    pub fn write_session(&self, log: &str, state: &str) -> Result<usize, String> {
        let snapshot = self.kernel.snapshot();
        let history = self.kernel.history();
        let records = history
            .iter()
            .filter(|record| record.seq <= snapshot.last_seq)
            .map(|record| {
                serde_json::to_string(record)
                    .map_err(|e| format!("could not render record {}: {e}", record.seq))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let snapshot = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| format!("could not render the session: {e}"))?;

        // named, because "No such file or directory" on its own leaves somebody guessing which
        // one; `-r` says which file it could not read and this should match it
        let log_beside = beside(log, (records.join("\n") + "\n").as_bytes())
            .map_err(|e| format!("could not write {log}: {e}"))?;
        let state_beside = match beside(state, &snapshot) {
            Ok(it) => it,
            Err(e) => {
                let _ = std::fs::remove_file(&log_beside);
                return Err(format!("could not write {state}: {e}"));
            }
        };
        std::fs::rename(&log_beside, log).map_err(|e| {
            let _ = std::fs::remove_file(&log_beside);
            let _ = std::fs::remove_file(&state_beside);
            format!("could not write {log}: {e}")
        })?;
        std::fs::rename(&state_beside, state).map_err(|e| {
            let _ = std::fs::remove_file(&state_beside);
            format!("wrote {log}, and could not write {state} beside it: {e}")
        })?;

        Ok(records.len())
    }

    /// Brings a saved session's context back, setting aside whatever is in this one.
    ///
    /// note: not a swap of the kernel. `Kernel::resume` is a constructor, and everything plugged
    /// into a running one - the provider, the policy, the tools, the introspection tools'
    /// handle, the subscription this screen is drawing from - is wired to *this* kernel; a second
    /// one built here would arrive with none of it. `kamchatka -r` is the swap, and it is a
    /// restart because that is what a swap is.
    ///
    /// note: so this is a context operation, and it follows the rule every other one here does:
    /// nothing is destroyed. What was in the context is archived rather than dropped, keeps its
    /// numbers and its contents, and `/undo` twice puts the whole thing back - once for the items
    /// that came in and once for the ones that were set aside. The loaded items are new items
    /// and are numbered as such: they are what that session said, in this session.
    pub(super) fn load(&mut self, path: &str) {
        if self.ready() {
            self.say(
                Speaker::Error,
                "not while the model's calls are decided and waiting to run: `/step` runs them, \
                 and `/stop` drops them",
            );
            return;
        }
        if self.busy
            || !self.kernel.pending_permissions().is_empty()
            || !self.kernel.pending_calls().is_empty()
        {
            self.say(Speaker::Error, MID_TURN);
            return;
        }

        // the same spellings `/save` takes, because a pair of commands that accept different ones
        // is a pair that does not round-trip: `/save notes.jsonl` writes `notes.json` beside the
        // log, and `/load notes.jsonl` would otherwise go looking for `notes.jsonl.json`
        let file = format!("{}.json", stem(path));
        // `/save` takes a directory and names the files in it after the session, which this
        // cannot know; saying what the argument is beats "could not read rec/.json"
        if std::fs::metadata(&file).is_err() && std::path::Path::new(path).is_dir() {
            self.say(
                Speaker::Error,
                format!(
                    "{path} is a directory, and `/load` takes a session's file: `/save` names \
                     the one it writes into a directory after the session, and said which"
                ),
            );
            return;
        }
        // a file and nothing else, for the reason `attach::contents` gives: a pipe with nobody
        // writing to it would be waited on by the thread this session runs on
        let snapshot: nachalnik::Snapshot = match std::fs::metadata(&file)
            .map_err(|e| format!("could not read {file}: {e}"))
            .and_then(|meta| match meta.is_file() {
                true => Ok(()),
                false => Err(format!("{file} is not a file, and a session is one")),
            })
            .and_then(|()| std::fs::read(&file).map_err(|e| format!("could not read {file}: {e}")))
            .and_then(|bytes| {
                serde_json::from_slice(&bytes).map_err(|e| format!("{file} is not a session: {e}"))
            }) {
            Ok(snapshot) => snapshot,
            Err(e) => return self.say(Speaker::Error, e),
        };
        if snapshot.items.is_empty() {
            self.say(Speaker::Error, format!("{file} holds no context"));
            return;
        }
        // refused for the reason `-r` refuses one; see `nachalnik::Snapshot::problems`
        let problems = snapshot.problems();
        if !problems.is_empty() {
            self.say(
                Speaker::Error,
                format!("{file} will not be loaded: {}", problems.join("; ")),
            );
            return;
        }

        // set aside first, so that the calls in the loaded turns are the only ones the projector
        // can pair a loaded result with. Archived items are not projected, so an old copy of the
        // same conversation cannot answer the new one's calls
        //
        // note: except what is pinned. A pin is the person saying this stays, and `--system` is
        // pinned - a load that quietly archived the system instruction would be answering a
        // question about a saved conversation by revoking the one thing the session was told to
        // hold on to. And with it what it is paired with: a pinned result whose call was archived
        // goes out of the request as an orphan, and so does a pinned call's result, so the turn
        // asking a pinned call stays, whole, and so does every result answering one of its calls
        let items = self.kernel.items();
        let mut held: HashSet<&ToolCallId> = items
            .iter()
            .filter(|item| item.state == ContextState::Pinned)
            .flat_map(|item| named_calls(item))
            .collect();
        let turns: Vec<&ToolCallId> = items
            .iter()
            .filter(|item| item.calls().any(|call| held.contains(&call.id)))
            .flat_map(|item| item.calls().map(|call| &call.id))
            .collect();
        held.extend(turns);
        let standing: Vec<_> = items
            .iter()
            .filter(|item| item.is_projected() && item.state != ContextState::Pinned)
            .filter(|item| !named_calls(item).any(|call| held.contains(call)))
            .map(|item| item.id)
            .collect();
        self.kernel.set_state(
            standing.iter().copied(),
            ContextState::Archived,
            Some(format!("set aside for the session loaded from {file}")),
        );

        // what the counter had learned, which is the one piece of a seam's state a snapshot
        // carries; without it the next few requests would be spent relearning what this file
        // already knows.
        //
        // note: `Kernel::recalibrate` rather than reaching through to the counter, and before the
        // items are counted rather than after. Counting first and correcting afterwards would give
        // every loaded item a figure from the scale this session happened to be on, while the
        // budget beside it is projected live and so is already on the loaded one - and the `held`
        // column would disagree with the `sending` column on the same row by exactly the
        // correction. The front door also recounts, which is
        // what brings the items already here - the ones the load is about to set aside, and which
        // `held back` adds to the loaded ones - onto the same scale
        if let Some(calibration) = snapshot.calibration {
            self.kernel.recalibrate(calibration);
        }

        // a file this session saved, or one forked from the same conversation, names calls this
        // kernel already issued - and what is pinned, or put back by `/restore`, is still asking
        // them. Pushed as they are, the next request would carry each such `tool_call_id` twice,
        // so the loaded copies take new ones, the call and the results answering it alike
        let mut snapshot = snapshot;
        let taken: HashSet<ToolCallId> = self.kernel.snapshot().used_calls.into_iter().collect();
        let renamed = rename_taken_calls(&mut snapshot, &taken);

        let ids = self.kernel.push_all(snapshot.items);
        // the rest are identifiers this kernel never issued, and nothing else would tell it so: a
        // provider that numbers its calls from zero every turn would hand one of them back, the
        // kernel would have nothing to compare it against, and the next request would carry the
        // same `tool_call_id` twice. `-r` gets this from `Kernel::resume`; this is the same fact,
        // said to a kernel that is already running
        self.kernel.reserve_calls(snapshot.used_calls);
        let params = snapshot.params.clone();
        let replaced = self.kernel.params() != params;
        self.kernel.set_params(snapshot.params);

        let loaded: Vec<_> = ids.iter().filter_map(|id| self.kernel.item(*id)).collect();
        self.say(
            Speaker::Note,
            format!(
                "loaded {} from session `{}` ({file}); {} of your own {} archived, \
                 anything pinned stayed with the calls and results it is paired with, and {}",
                plural(loaded.len(), "item"),
                snapshot.session,
                standing.len(),
                match standing.len() {
                    1 => "was",
                    _ => "were",
                },
                // one undo for the push and one for the archiving, which is no undo at all when
                // nothing was archived - and a second one then would take back something of the
                // person's own
                match standing.len() {
                    0 => "`/undo` takes the loaded ones back out",
                    _ => "`/undo` twice puts the rest back",
                },
            ),
        );
        if renamed != 0 {
            self.say(
                Speaker::Note,
                format!(
                    "this session had already used {} the loaded turns name, so their copies were \
                     given new ones",
                    plural(renamed, "call identifier"),
                ),
            );
        }
        // note: said, because nothing else would: `/undo` walks the context back and not the
        // parameters, and the next request goes out with the snapshot's
        if replaced {
            self.say(
                Speaker::Note,
                match params.is_empty() {
                    true => {
                        "the snapshot sets no parameters, so the ones set here are gone".to_owned()
                    }
                    false => format!(
                        "parameters are the snapshot's now: {}",
                        serde_json::to_string(&params).unwrap_or_default()
                    ),
                },
            );
        }
    }

    /// Writes the session log and a snapshot that can be resumed from, at a path somebody gave.
    ///
    /// note: two files, because they answer different questions: the log says what happened, and
    /// the snapshot is what can be picked back up. An event names an item rather than carrying
    /// it, so the log alone cannot rebuild a context - keeping only one of them means losing
    /// either the story or the state.
    ///
    /// note: the snapshot is what `/load` reads back into a running session and what
    /// `kamchatka -r` starts from.
    pub(super) fn save(&mut self, path: &str) {
        let stem = stem(path);
        // note: a directory is a place to put it rather than a name for it. Taken whole as the
        // stem, `/save sessions/` would write `sessions/.json` and `sessions/.jsonl` - two
        // dotfiles, invisible to `ls`, under a confirmation that prints the path and so reads as
        // though it had worked. The session's own name is what goes in a directory, which is
        // what this program already does when it writes a session out on its own.
        //
        // note: and it goes in beside whatever holds that name already, as the record at the end
        // of a run does and for its reason: a resumed session keeps the name of the one it
        // resumed, so the name alone would write over the log this session was carried on from.
        // Only this sitting's own earlier save there is replaced - see `App::saved_into`
        let directory =
            stem.ends_with(std::path::MAIN_SEPARATOR) || std::path::Path::new(stem).is_dir();
        let into: std::path::PathBuf = std::path::Path::new(stem).components().collect();
        // note: the directory it goes in is named where it is not there, rather than left to the
        // write, whose error names the file - `could not write notes/today.jsonl: No such file or
        // directory` reads as a problem with a file nobody expected to exist yet. Not made: making
        // directories is a change to what `/save` can do, the question `fs write` also waits on
        let within = match directory {
            true => into.as_path(),
            false => into.parent().unwrap_or(std::path::Path::new("")),
        };
        if !within.as_os_str().is_empty() && !within.is_dir() {
            return self.say(
                Speaker::Error,
                format!(
                    "there is no directory {} to save into; make it first",
                    within.display()
                ),
            );
        }
        let (log, state, claimed) = match (directory, self.saved_into.get(&into)) {
            (false, _) => (format!("{stem}.jsonl"), format!("{stem}.json"), false),
            (true, Some((log, state))) => (log.clone(), state.clone(), false),
            (true, None) => {
                match crate::wiring::unclaimed(&into.join(self.kernel.session_name())) {
                    Ok((log, state, _)) => (log, state, true),
                    Err(e) => return self.say(Speaker::Error, e),
                }
            }
        };

        // said rather than asked about: writing the same session again is the ordinary case and
        // a prompt every time would be noise, but a typo landing on somebody else's file should
        // not pass in silence. A name just claimed is this save's own, empty file
        let replacing: Vec<&str> = [log.as_str(), state.as_str()]
            .into_iter()
            .filter(|path| !claimed && std::path::Path::new(path).exists())
            .collect();

        let written = self.write_session(&log, &state);
        if claimed && written.is_err() {
            let _ = std::fs::remove_file(&log);
        }

        match written {
            Ok(records) => {
                if directory {
                    self.saved_into.insert(into, (log.clone(), state.clone()));
                }
                if !replacing.is_empty() {
                    self.say(
                        Speaker::Note,
                        format!("replaced {}", replacing.join(" and ")),
                    );
                }
                self.say(
                    Speaker::Note,
                    format!(
                        "{records} records in {log}, and a session in {state} (`/load {state}` \
                         brings it back here, `kamchatka -r {state}` starts a session from it)"
                    ),
                );
            }
            Err(e) => self.say(Speaker::Error, e),
        }
    }
}

/// Writes `bytes` to a new file beside `path`, flushed to disk, for a rename to put in its place.
///
/// note: `0600`, and not whatever the umask allows. A session is the whole conversation - every
/// tool result and whatever was pasted at the desk - and the rename puts this file's mode in place
/// of the target's, so under an ordinary umask a save over a file somebody had made private came
/// back readable by everyone. The record the session writes on its own goes through here too, into
/// a directory only its owner can open; this is the same rule for a path somebody named.
pub(crate) fn beside(path: &str, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};

    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // `create_new`, which follows no link, because `/save` names a path anywhere - a shared
    // directory included - and a name somebody predicted could be a link they left there
    let (at, mut file) = loop {
        let at = std::path::PathBuf::from(format!(
            "{path}.{}.{}.writing",
            std::process::id(),
            MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&at)
        {
            Ok(file) => break (at, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(e) = written {
        let _ = std::fs::remove_file(&at);
        return Err(e);
    }

    Ok(at)
}

/// The calls an item is one half of a pair with: the ones a turn asks, or the one a result answers.
fn named_calls(item: &ContextItem) -> impl Iterator<Item = &ToolCallId> {
    let result = match &item.kind {
        ContextKind::ToolResult { call, .. } => Some(call),
        _ => None,
    };
    item.calls().map(|call| &call.id).chain(result)
}

/// Gives every call identifier in `snapshot` that `taken` holds a new one, the same new one
/// wherever it appears, and returns how many were renamed.
///
/// note: an identifier is renamed in the calls, in the results that answer them and in
/// `used_calls` alike, so the loaded turns still pair with each other and the reservation that
/// follows covers the names they now carry.
fn rename_taken_calls(snapshot: &mut nachalnik::Snapshot, taken: &HashSet<ToolCallId>) -> usize {
    let mut unavailable: HashSet<ToolCallId> = taken.clone();
    unavailable.extend(snapshot.used_calls.iter().cloned());
    unavailable.extend(snapshot.items.iter().flat_map(named_calls).cloned());

    let mut renames: HashMap<ToolCallId, ToolCallId> = HashMap::new();
    let mut rename = |id: &mut ToolCallId| {
        if !taken.contains(id) {
            return;
        }
        let new = renames
            .entry(id.clone())
            .or_insert_with(|| {
                let fresh = (1..)
                    .map(|n| ToolCallId(format!("{}_{n}", id.0)))
                    .find(|candidate| !unavailable.contains(candidate))
                    .expect("an unbounded range has a name nothing holds");
                unavailable.insert(fresh.clone());
                fresh
            })
            .clone();
        *id = new;
    };

    for item in &mut snapshot.items {
        match &mut item.kind {
            ContextKind::AssistantMessage { tool_calls, .. } => {
                tool_calls.iter_mut().for_each(|call| rename(&mut call.id));
            }
            ContextKind::ToolResult { call, .. } => rename(call),
            _ => {}
        }
        // a turn recorded as ordered blocks keeps its calls in its content, and only a turn with
        // one to rename is rebuilt; the blocks share what they carry, so it is the list that is new
        let ordered = match (&item.kind, item.content.as_blocks()) {
            (ContextKind::AssistantMessage { .. }, Some(blocks)) => blocks,
            _ => continue,
        };
        if !ordered
            .iter()
            .filter_map(Block::call)
            .any(|call| taken.contains(&call.id))
        {
            continue;
        }
        let rebuilt: Vec<Block> = ordered
            .iter()
            .map(|block| match block {
                Block::Call(call) => {
                    let mut call = call.clone();
                    rename(&mut call.id);
                    Block::Call(call)
                }
                other => other.clone(),
            })
            .collect();
        item.content = Content::Blocks(rebuilt.into());
    }
    snapshot.used_calls.iter_mut().for_each(&mut rename);

    renames.len()
}

/// What `/save` and `/load` name a session's two files after: the path without its suffix, or
/// `session` where that leaves nothing.
///
/// note: nothing is left by no argument and by an argument that was only a suffix, and the second
/// would otherwise be the stem of `.json` and `.jsonl` - two dotfiles `ls` does not show.
fn stem(path: &str) -> &str {
    match without_suffix(path) {
        "" => "session",
        stem => stem,
    }
}

/// A session's path with the extension taken off, whichever of the two it was spelled with.
///
/// note: shared by `/save`, `/load` and `--check` because commands that accept different
/// spellings do not round-trip. A session is two files - the snapshot and the log - so
/// `/save notes.jsonl` writes `notes.json` beside `notes.jsonl`, and `/load` taking that same
/// argument at its word would go looking for `notes.jsonl.json`.
///
/// note: the suffix is matched without regard to case, the way `attach::media_type` reads an
/// extension - and the stem is left exactly as it was typed, because that half really does name a
/// different file. What it buys is a suffix typed in capitals still being read as the suffix
/// `/save` wrote.
pub(crate) fn without_suffix(path: &str) -> &str {
    for suffix in [".jsonl", ".json"] {
        let Some(at) = path.len().checked_sub(suffix.len()) else {
            continue;
        };
        if path
            .get(at..)
            .is_some_and(|end| end.eq_ignore_ascii_case(suffix))
        {
            return &path[..at];
        }
    }

    path
}
