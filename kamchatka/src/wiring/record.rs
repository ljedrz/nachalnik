//! Where a session goes when it is over: written down as it goes, or all at once at the end.

use std::{
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use nachalnik::{Kernel, Record};
use serde::Deserialize;

use crate::app::App;

/// Where a session went when it was written out: how many records, and the two files.
#[derive(Debug)]
#[non_exhaustive]
pub struct Recorded {
    /// How many records the log holds.
    pub records: usize,
    /// The event log, one record per line.
    pub log: PathBuf,
    /// The snapshot `kamchatka -r` starts from.
    pub state: PathBuf,
}

impl std::fmt::Display for Recorded {
    // the line every run ends with, and the one `/save` says
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} records in {}, and a session in {}",
            self.records,
            self.log.display(),
            self.state.display()
        )
    }
}

/// Writes the session where nobody has to have asked for it, and says where that was.
///
/// note: a temporary directory, because this is a safety net rather than an archive - `/save`
/// remains the way to put a session somewhere it will still be next week.
/// [`Setup::record`](super::Setup::record) is the setting that turns it off, and the caller reads
/// it: this only writes.
///
/// note: the directory is the user's own, `0700`. What goes in it is a whole conversation and every
/// byte of output every tool produced, written without anybody asking for it; under the default
/// umask that would be a world-readable file in a directory everyone on the machine can list.
///
/// note: a session with a [`Recorder`] on it has been writing itself down since it started, and
/// this finishes that record rather than claiming a second pair of files beside it. Without one the
/// whole session is written here, at the end, which is what a loop that never started a recorder
/// gets, and what the recorder itself falls back to, so a session whose record could not be claimed
/// at the start still gets one chance at the end.
pub fn record(app: &App) -> Result<Recorded, String> {
    if let Some(recorder) = &app.recorder {
        return recorder.finish(&app.kernel);
    }

    let dir = private_dir()?;
    let (log, state, _) = unclaimed(&dir.join(app.kernel.session_name()))?;
    let records = app.write_session(&log, &state)?;

    Ok(Recorded {
        records,
        log,
        state,
    })
}

/// A session's record, written as the session goes rather than when it is over.
///
/// note: the record a process leaves behind was once written at the end, from the log the kernel
/// keeps in memory, so a `kill -9`, an out-of-memory kill or a pulled plug left nothing at all.
/// This claims the pair of files when the session starts, appends every record the moment
/// [`App::on_event`] hears of it, and rewrites the snapshot whenever the session comes to rest: the
/// log is then complete to the last event however the process ends, and the snapshot is where
/// things stood at the end of the last turn that finished.
///
/// note: the records are read out of the kernel's log by sequence number rather than taken off the
/// broadcast, so a subscription that fell behind loses the screen a frame and the record nothing.
/// The log runs ahead of the snapshot, never behind it: a snapshot names the last record it
/// reflects, so a log holding more than that is a session that went on after the snapshot and a log
/// holding less is one that was tampered with - the runtime's own `tests/crash.rs` is where that
/// ordering is argued.
///
/// note: `flush` after every append and `sync_all` only at the end. What this is for is the process
/// dying, and a write the kernel has taken survives that; `sync_all` adds the machine dying, and a
/// `sync_all` per event would cost a disk round trip for every fragment a tool streams. The
/// snapshot is synced every time, since it is a whole file replaced by rename.
#[derive(Debug)]
pub struct Recorder {
    log: PathBuf,
    state: PathBuf,
    kept: parking_lot::Mutex<Kept>,
    /// The `context` tool's undo history, written into the snapshot beside the session.
    history: Option<crate::introspect::History>,
}

/// What the recorder holds between two writes.
#[derive(Debug)]
struct Kept {
    file: std::fs::File,
    /// The sequence number of the last record written to the log.
    written: u64,
    /// How many records the log holds.
    records: usize,
    /// The last record the snapshot on disk reflects, once one has been written.
    snapshotted: Option<u64>,
}

impl Recorder {
    /// Claims a record under the temporary directory and writes what the session holds so far.
    pub fn start(app: &App) -> Result<Self, String> {
        Self::start_under(app, &private_dir()?)
    }

    /// Claims a record under `dir`, which must be a directory of this user's that only this user
    /// can enter, and writes what the session holds so far.
    ///
    /// note: the check is the one [`record`] makes of the temporary directory, applied to any
    /// directory because what goes into the record is the same wherever it is put.
    pub fn start_under(app: &App, dir: &Path) -> Result<Self, String> {
        private(dir)?;
        let (log, state, file) = unclaimed(&dir.join(app.kernel.session_name()))?;
        let recorder = Self {
            log,
            state,
            history: app.introspect.as_ref().map(|it| it.history()),
            kept: parking_lot::Mutex::new(Kept {
                file,
                written: 0,
                records: 0,
                snapshotted: None,
            }),
        };
        // writes earlier records first, so the record is the whole session rather than only the
        // part this process saw. See `Earlier`
        {
            let mut kept = recorder.kept.lock();
            Self::write_all(&mut kept, app.earlier.records().iter())?;
        }
        recorder.checkpoint(&app.kernel)?;

        Ok(recorder)
    }

    /// The event log, one record per line.
    pub fn log(&self) -> &Path {
        &self.log
    }

    /// The snapshot `kamchatka -r` starts from.
    pub fn state(&self) -> &Path {
        &self.state
    }

    /// Appends every record the log has grown since the last time.
    pub fn append(&self, kernel: &Kernel) -> Result<(), String> {
        let mut kept = self.kept.lock();
        Self::append_into(&mut kept, kernel)
    }

    /// Appends what the log has grown and rewrites the snapshot.
    ///
    /// note: the snapshot is taken first and the log written up to date after it, so the log is
    /// never behind the snapshot. See the note on the type.
    pub fn checkpoint(&self, kernel: &Kernel) -> Result<(), String> {
        let mut kept = self.kept.lock();
        // asked before the snapshot is taken as well as after, since taking one copies every item:
        // the fifty announcements of an exclusion over fifty items were fifty copies of the
        // context, and all but the first were thrown away below
        if kept.snapshotted == Some(kernel.last_seq()) {
            return Self::append_into(&mut kept, kernel);
        }
        let snapshot = kernel.snapshot();
        Self::append_into(&mut kept, kernel)?;
        // note: every change to a session is a record in its log, so a snapshot naming the record
        // the last one named is the same file. Skipping it keeps one command to one write: an
        // exclusion or a compaction announces every item, and the kernel has made all the changes
        // before the first announcement arrives here
        let reflects = snapshot.last_seq;
        if kept.snapshotted == Some(reflects) {
            return Ok(());
        }
        let snapshot = crate::introspect::snapshot_with(&snapshot, self.history.as_ref())?;
        let beside = crate::app::beside(&self.state, &snapshot)
            .map_err(|e| format!("could not write {}: {e}", self.state.display()))?;
        std::fs::rename(&beside, &self.state).map_err(|e| {
            let _ = std::fs::remove_file(&beside);
            format!("could not write {}: {e}", self.state.display())
        })?;
        kept.snapshotted = Some(reflects);

        Ok(())
    }

    /// Writes everything that is left and says where the record went.
    pub fn finish(&self, kernel: &Kernel) -> Result<Recorded, String> {
        self.checkpoint(kernel)?;
        let kept = self.kept.lock();
        kept.file
            .sync_all()
            .map_err(|e| format!("could not write {}: {e}", self.log.display()))?;

        Ok(Recorded {
            records: kept.records,
            log: self.log.clone(),
            state: self.state.clone(),
        })
    }

    fn append_into(kept: &mut Kept, kernel: &Kernel) -> Result<(), String> {
        // note: one number read before the log is searched, for the reason `connection::flush`
        // gives: this runs on every event, every streamed fragment among them, and the search
        // takes the lock every emit waits on to find, nearly always, nothing
        if kernel.last_seq() <= kept.written {
            return Ok(());
        }
        Self::write_all(kept, kernel.history_since(kept.written).iter())
    }

    /// Writes these records to the log, a line each, and moves the mark past them.
    fn write_all<'a>(
        kept: &mut Kept,
        records: impl Iterator<Item = &'a Record>,
    ) -> Result<(), String> {
        use std::io::Write as _;

        for record in records {
            let line = serde_json::to_string(record)
                .map_err(|e| format!("could not render record {}: {e}", record.seq))?;
            writeln!(kept.file, "{line}").map_err(|e| format!("could not write the log: {e}"))?;
            kept.written = record.seq;
            kept.records += 1;
        }

        kept.file
            .flush()
            .map_err(|e| format!("could not write the log: {e}"))
    }
}

/// The records of the session a resumed one carries on from, read once from the log beside its
/// snapshot so this session's record and its `log` tool hold the whole of it.
///
/// note: a resumed kernel's log starts at `session.resumed` and numbers on from the last record the
/// snapshot reflects, so these and the kernel's own are one log with nothing missing between them.
/// Kept here rather than given to the kernel, whose log is what happened in this process.
///
/// note: copied forward into every record written after it - see [`Recorder`] and
/// [`App::write_session`] - so a log written by this program is the whole session however many
/// times it was resumed, and one hop back is always enough.
///
/// note: shared because the `log` tool is built before anything is read; a clone reads what the
/// first one was given.
#[derive(Clone, Debug, Default)]
pub struct Earlier(Arc<OnceLock<Arc<[Record]>>>);

impl Earlier {
    /// The records, oldest first; none until [`Earlier::read`] has found some.
    pub fn records(&self) -> Arc<[Record]> {
        self.0.get().cloned().unwrap_or_else(|| Arc::from([]))
    }

    /// Reads the log beside the snapshot at `state`, up to the record the snapshot reflects, and
    /// returns how many records it held past that; once, and a second read changes nothing.
    ///
    /// note: best effort, not an error. The session has resumed by the time this runs, and a log
    /// that is absent, unreadable or half-written costs the history before the resume rather than
    /// the session. A line that will not parse is skipped rather than ending the walk: the last
    /// line of a log from a killed run is the one most likely to be half a record, and the ones
    /// before it are fine.
    ///
    /// note: only up to the record the snapshot was taken at. A run killed in the middle of a turn
    /// leaves a log that runs past its snapshot, and those records are not this session's: what
    /// they did is not in it, and the kernel numbers on from the snapshot, so the next record here
    /// would share a number with one of them.
    ///
    /// note: where the snapshot ends is read from it, and `resumed` is for when it cannot be: where
    /// the caller's kernel numbers on from. Where neither says, nothing is read. Taken as no end at
    /// all, the records past a snapshot that could not be read came in as this session's.
    pub fn read(&self, state: &Path, resumed: Option<u64>) -> usize {
        /// The one field of the snapshot this needs, read without the rest of it.
        #[derive(Deserialize)]
        struct Taken {
            #[serde(default)]
            last_seq: u64,
        }

        if self.0.get().is_some() {
            return 0;
        }
        let Ok(log) = std::fs::File::open(state.with_extension("jsonl")) else {
            return 0;
        };
        let Some(taken) = std::fs::File::open(state)
            .ok()
            .and_then(|file| serde_json::from_reader::<_, Taken>(BufReader::new(file)).ok())
            .map(|taken| taken.last_seq)
            .or(resumed)
        else {
            return 0;
        };

        let (mut records, mut past) = (Vec::new(), 0);
        for line in BufReader::new(log).lines().map_while(Result::ok) {
            let Ok(record) = serde_json::from_str::<Record>(&line) else {
                continue;
            };
            match record.seq > taken {
                true => past += 1,
                false => records.push(record),
            }
        }
        let _ = self.0.set(Arc::from(records));

        past
    }
}

/// The directory under the temporary one that a record goes in, made and checked.
fn private_dir() -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut dir = std::env::temp_dir();
    dir.push("kamchatka");
    // note: made `0700` rather than made and then chmodded, because in between it is open to
    // anybody, and a directory somebody opened then can be listed through that descriptor after
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    private(&dir)?;

    Ok(dir)
}

/// Refuses `dir` unless it is a directory of this user's that only its owner can enter.
fn private(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

    // note: opened without following a link, made private and looked at through what was opened,
    // because the temporary directory is everybody's and this name is fixed. A link left at it is
    // refused before anything is done through it, including a chmod of what it points at. A
    // directory somebody else made there first is one the chmod cannot make private; it fails and
    // is ignored. Without the look, the transcript would go into a directory they can read.
    //
    // note: and whose it is, before the chmod, because mode bits say nothing to a process that can
    // read past them. Run as root, or holding `CAP_DAC_OVERRIDE`, this writes into a directory
    // another user made `0700` there first, and chmods one they left open; they read the transcript
    // either way
    let private = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::DIRECTORY).bits() as i32)
        .open(dir)
        .is_ok_and(|opened| {
            let ours = opened
                .metadata()
                .is_ok_and(|meta| meta.uid() == rustix::process::geteuid().as_raw());
            if !ours {
                return false;
            }
            // it may already exist from an earlier run; this run writes a transcript into it
            let _ = opened.set_permissions(std::fs::Permissions::from_mode(0o700));
            opened
                .metadata()
                .is_ok_and(|meta| meta.is_dir() && meta.permissions().mode() & 0o077 == 0)
        });
    if !private {
        return Err(format!(
            "{} is not a directory only you can enter, so the session was not recorded \
             there; remove it, or use `--no-record` and `/save`",
            dir.display()
        ));
    }
    // note: and nobody else can move it. It was checked through what was opened, and the record is
    // written into it by name after that, for the whole session. A directory above it that others
    // may rename things in - writable by them without the sticky bit - lets them put a
    // directory of their own at the name once the check is over. `/tmp` is sticky, so this refuses
    // only a temporary directory set up so that it is not
    if let Some(open) = dir.ancestors().skip(1).find(|above| movable(above)) {
        return Err(format!(
            "{} is in {}, where somebody else can move it, so the session was not recorded \
             there; set `TMPDIR` to a directory of your own, or use `--no-record` and `/save`",
            dir.display(),
            open.display()
        ));
    }

    Ok(())
}

/// Whether others can rename what is in `dir`: it is writable by them and not sticky.
///
/// note: by the mode alone, not the owner. Root is seen as `nobody` inside a user namespace, so
/// refusing a directory somebody else owns refused `/tmp` there, and a directory another ordinary
/// user owns is one the person chose to point `TMPDIR` at.
fn movable(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    std::fs::metadata(dir).is_ok_and(|meta| meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0)
}

/// A `.jsonl` and `.json` pair under `stem` that no other session has written, and the log's open
/// file, which is the claim itself.
///
/// note: a session name is not unique enough for a filename. Two runs started within one second
/// share a stamp, so the second to finish overwrites the first. A resumed session keeps the name of
/// the session it resumed, so `-r` would write over the log it just read, replacing the log of what
/// happened with that of a sitting that did nothing. The snapshot would come through nearly intact,
/// because a resumed context renders to nearly the same bytes; the log would not, and the log is
/// the half that says what happened rather than where things ended up.
///
/// note: the name stays and the file moves: `…Z-2.jsonl` beside `…Z.jsonl`, which sorts next to its
/// sibling and reads as the second sitting of one session. Renaming the session would add a process
/// identifier to every filename to fix something rare; the name is what `#fork` derives from and
/// what the trace shows.
///
/// note: `create_new` is used rather than checking first, because the first of those collisions
/// happens between asking and writing. The `.json` is checked before the `.jsonl` is claimed, so a
/// passed-over suffix leaves nothing behind.
///
/// note: a taken name is passed over and nothing else is. An unwritable directory or a full disk is
/// why the record is not there, and saying "no unused name" after many tries would be the wrong
/// reason.
///
/// note: suffixes are added to the stem's bytes, not to how it displays. A non-UTF-8 directory name
/// displays with a replacement character; a name built from that is a directory that is not there.
pub(crate) fn unclaimed(stem: &Path) -> Result<(PathBuf, PathBuf, std::fs::File), String> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let with = |suffix: &str| {
        let mut path = stem.as_os_str().to_owned();
        path.push(suffix);
        PathBuf::from(path)
    };
    // bounded, so that a directory full of these is an error rather than a loop
    for nth in 1..1_000 {
        let nth = match nth {
            1 => String::new(),
            nth => format!("-{nth}"),
        };
        let (log, state) = (with(&format!("{nth}.jsonl")), with(&format!("{nth}.json")));
        // anything there at all, a link to nothing included, which `exists` answers no to and
        // a write would follow
        if std::fs::symlink_metadata(&state).is_ok() {
            continue;
        }

        // note: `0600`, as the snapshot beside it is and as `/save` makes both. It is the whole
        // conversation, and the directory being private is a second guard rather than the only one
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&log)
        {
            Ok(file) => return Ok((log, state, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not write {}: {e}", log.display())),
        }
    }

    Err(format!(
        "could not find an unused name for the record beside {}",
        stem.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session writes beside a record rather than over it, however it came by the same name.
    ///
    /// note: `-r` is printed at the end of every run, and a resumed session keeps the name of the
    /// session it resumed, so every resume would otherwise write over the log it had just read.
    #[test]
    fn a_record_never_writes_over_one_that_is_already_there() {
        // note: not `tests/common`'s `scratch`, which builds under `CARGO_TARGET_TMPDIR`; cargo
        // hands that to integration tests, not unit tests. One fixed name, emptied on entry, so
        // nothing accumulates
        let dir = std::env::temp_dir().join("kamchatka-unclaimed");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to work in");
        let stem = dir.join("2026-09-15T13-34-29Z");

        let (log, state, _) = unclaimed(&stem).expect("nothing is there yet");
        assert_eq!(log, dir.join("2026-09-15T13-34-29Z.jsonl"));
        assert_eq!(state, dir.join("2026-09-15T13-34-29Z.json"));
        // what a session that got this far would leave behind
        std::fs::write(&log, "one").expect("written");
        std::fs::write(&state, "{}").expect("written");

        // the same name again - two runs in one second, or a resume - lands beside it
        let (again, beside, _) = unclaimed(&stem).expect("a second name");
        assert_eq!(again, dir.join("2026-09-15T13-34-29Z-2.jsonl"));
        assert_eq!(beside, dir.join("2026-09-15T13-34-29Z-2.json"));
        assert_eq!(
            std::fs::read_to_string(&log).expect("still there"),
            "one",
            "the first record is untouched"
        );

        // and the claim is the file itself, so a third does not get the second's name back
        std::fs::write(&beside, "{}").expect("written");
        let (third, _, _) = unclaimed(&stem).expect("a third name");
        assert_eq!(third, dir.join("2026-09-15T13-34-29Z-3.jsonl"));
    }

    /// A log with no snapshot beside it has still been claimed, and the next name is the next one.
    ///
    /// note: the `.json` is looked for first, so it is the only way to reach the `.jsonl` check. A
    /// log on its own is what a process that died between claiming it and writing the first
    /// snapshot leaves, and one that had never been written at all must not get its name back.
    #[test]
    fn a_log_left_without_its_snapshot_is_a_name_that_is_taken() {
        let dir = std::env::temp_dir().join("kamchatka-unclaimed-alone");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to work in");
        let stem = dir.join("2026-09-16T08-00-00Z");
        std::fs::write(dir.join("2026-09-16T08-00-00Z.jsonl"), "one").expect("written");

        let (log, state, _) = unclaimed(&stem).expect("a name beside the log that is there");
        assert_eq!(log, dir.join("2026-09-16T08-00-00Z-2.jsonl"));
        assert_eq!(state, dir.join("2026-09-16T08-00-00Z-2.json"));
        assert_eq!(
            std::fs::read_to_string(dir.join("2026-09-16T08-00-00Z.jsonl")).expect("still there"),
            "one",
            "the log that is there is untouched"
        );
    }

    /// A stem that cannot be written at all says which path and why instead of looking for another
    /// name.
    ///
    /// note: `unclaimed` passes over only a name that is taken. A directory that cannot be written
    /// in or a full disk is the reason there is no record.
    #[test]
    fn a_name_that_cannot_be_written_at_all_is_said_as_it_is() {
        let dir = std::env::temp_dir().join("kamchatka-unclaimed-nowhere");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to work in");
        std::fs::write(dir.join("afile"), "not a directory").expect("written");

        let refused = unclaimed(&dir.join("afile").join("session")).expect_err("nowhere to write");
        assert!(
            refused.contains("afile/session.jsonl") && refused.contains("could not write"),
            "the refusal does not name the path or the reason: {refused}"
        );
        assert!(
            !refused.contains("unused name"),
            "a name that cannot be written was looked for again and again: {refused}"
        );
    }

    /// A directory this user owns but cannot make private is refused, whatever it is made of.
    ///
    /// note: the mode is checked because it is what other users of the machine go by, and the check
    /// is after a `chmod` because a directory that can be tightened is. procfs is the case where
    /// the two come apart: `/proc/self/task` is this user's own, is `0555`, and refuses to be
    /// changed, so the `chmod` is ignored, the bits stay, and a transcript written under it would
    /// be readable by anybody on the machine.
    #[test]
    fn a_directory_that_cannot_be_made_private_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::path::Path::new("/proc/self/task");
        let Ok(meta) = std::fs::metadata(dir) else {
            return;
        };
        let mine = {
            use std::os::unix::fs::MetadataExt as _;
            meta.uid() == rustix::process::geteuid().as_raw()
        };
        if !mine {
            return;
        }
        // note: if the `chmod` succeeds, this directory is made private and there is nothing to
        // check
        let tightened = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        let mode = std::fs::metadata(dir)
            .expect("still there")
            .permissions()
            .mode();
        if tightened.is_ok() || mode & 0o077 == 0 {
            return;
        }

        let refused = private(dir).expect_err("a directory everybody can read is not private");
        assert!(
            refused.contains("is not a directory only you can enter"),
            "the refusal does not say what is wrong: {refused}"
        );
    }

    /// A private directory in one others can rename things in is refused, since they could move it
    /// once it is checked.
    #[test]
    fn a_record_directory_others_can_move_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join("kamchatka-private-movable");
        let _ = std::fs::remove_dir_all(&base);
        let open = base.join("open");
        let dir = open.join("kamchatka");
        std::fs::create_dir_all(&dir).expect("a directory");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("private");
        private(&dir).expect("private, in a directory of this user's");

        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).expect("opened");
        let refused = private(&dir).expect_err("anybody can move it");
        assert!(refused.contains("somebody else can move it"), "{refused}");

        // and the sticky bit, as `/tmp` has, is what stops that
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o1777)).expect("sticky");
        private(&dir).expect("only its owner can move it");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A link at the record directory's name is refused, even to a private directory of this
    /// user's.
    ///
    /// note: the name is fixed and the temporary directory is everybody's, so a link somebody left
    /// there would send the transcript wherever it pointed. The open refuses it by not following
    /// the link, before anything is looked at through it.
    #[test]
    fn a_link_at_the_record_directorys_name_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join("kamchatka-private-link");
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).expect("a directory to point at");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700))
            .expect("made private");
        private(&real).expect("the directory itself is private");
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("a link to it");

        let refused = private(&link).expect_err("a link is not the directory");
        assert!(
            refused.contains("is not a directory only you can enter"),
            "the refusal does not say what is wrong: {refused}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A link at the snapshot's name is a name that is taken, even when it points at nothing.
    ///
    /// note: `exists` follows a link and answers no for a link to a file that is not there, and the
    /// write after it follows the link too. A link left at a predictable name would create a file
    /// wherever it pointed.
    #[test]
    fn a_link_at_the_snapshots_name_is_not_written_through() {
        let dir = std::env::temp_dir().join("kamchatka-unclaimed-link");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to work in");
        let stem = dir.join("2026-09-22T12-00-00Z");
        std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("2026-09-22T12-00-00Z.json"))
            .expect("a link");

        let (log, state, _) = unclaimed(&stem).expect("a name beside it");
        assert_eq!(log, dir.join("2026-09-22T12-00-00Z-2.jsonl"));
        assert_eq!(state, dir.join("2026-09-22T12-00-00Z-2.json"));
    }
}
