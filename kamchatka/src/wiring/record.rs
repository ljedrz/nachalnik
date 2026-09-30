//! Where a session goes when it is over: written down as it goes, or all at once at the end.

use nachalnik::Kernel;

use crate::app::App;

/// Where a session went when it was written out: how many records, and the two files.
#[derive(Debug)]
#[non_exhaustive]
pub struct Recorded {
    /// How many records the log holds.
    pub records: usize,
    /// The event log, one record per line.
    pub log: String,
    /// The snapshot `kamchatka -r` starts from.
    pub state: String,
}

impl std::fmt::Display for Recorded {
    // the line every run ends with, and the one `/save` says
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} records in {}, and a session in {}",
            self.records, self.log, self.state
        )
    }
}

/// Writes the session where nobody has to have asked for it, and says where that was.
///
/// note: a temporary directory, because this is a safety net rather than an archive - `/save`
/// remains the way to put a session somewhere it will still be next week. [`Setup::record`](super::Setup::record) is
/// the setting that turns it off, and the caller is what reads it: this writes.
///
/// note: and the directory is the user's own, `0700`. What goes in it is a whole conversation and
/// every byte of output every tool produced, written without anybody asking for it; under the
/// default umask that is a world-readable file in a directory everyone on the machine can list.
/// Nobody would type `/save /tmp/everyone/notes.jsonl`, and this should not do it for them.
///
/// note: a session with a [`Recorder`] on it has been writing itself down since it started, and
/// this finishes that record rather than claiming a second pair of files beside it. Without one
/// the whole session is written here, at the end, which is what a loop that never started a
/// recorder gets - and what the recorder itself falls back to, so a session whose record could
/// not be claimed at the start still gets one chance at the end.
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
/// note: the record a process leaves behind was written once, at the end, from the log the
/// kernel keeps in memory - so a `kill -9`, an out-of-memory kill or a pulled plug left nothing
/// at all, of a session whose whole point is that everything that happened is written down.
/// This claims the pair of files when the session starts, appends every record the moment
/// [`App::on_event`] hears of it, and rewrites the snapshot whenever the session comes to rest:
/// the log is then complete to the last event however the process ends, and the snapshot is
/// where things stood at the end of the last turn that finished.
///
/// note: the records are read out of the kernel's log by sequence number rather than taken off
/// the broadcast, so a subscription that fell behind loses the screen a frame and the record
/// nothing. And the log runs ahead of the snapshot on purpose, never behind it: a snapshot names
/// the last record it reflects, so a log holding more than that is a session that went on after
/// the snapshot and a log holding less is one that was tampered with - the runtime's own
/// `tests/crash.rs` is where that ordering is argued.
///
/// note: `flush` after every append and `sync_all` only at the end. What this is for is the
/// process dying, and a write the kernel has taken survives that; what `sync_all` adds is the
/// machine dying, and a `sync_all` per event would cost a disk round trip for every fragment a
/// tool streams. The snapshot is synced every time, since it is a whole file replaced by rename.
#[derive(Debug)]
pub struct Recorder {
    log: String,
    state: String,
    kept: parking_lot::Mutex<Kept>,
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

    /// Claims a record under `dir`, which has to be a directory of this user's that only it can
    /// enter, and writes what the session holds so far.
    ///
    /// note: the check is the one [`record`] makes of the temporary directory, and it is made of
    /// any directory rather than only that one because what goes into the record is the same
    /// wherever it is put.
    pub fn start_under(app: &App, dir: &std::path::Path) -> Result<Self, String> {
        private(dir)?;
        let (log, state, file) = unclaimed(&dir.join(app.kernel.session_name()))?;
        let recorder = Self {
            log,
            state,
            kept: parking_lot::Mutex::new(Kept {
                file,
                written: 0,
                records: 0,
                snapshotted: None,
            }),
        };
        recorder.checkpoint(&app.kernel)?;

        Ok(recorder)
    }

    /// The event log, one record per line.
    pub fn log(&self) -> &str {
        &self.log
    }

    /// The snapshot `kamchatka -r` starts from.
    pub fn state(&self) -> &str {
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
    /// never behind the snapshot it sits beside - see the note on the type.
    pub fn checkpoint(&self, kernel: &Kernel) -> Result<(), String> {
        let mut kept = self.kept.lock();
        let snapshot = kernel.snapshot();
        Self::append_into(&mut kept, kernel)?;
        // note: every change to a session is a record in its log, so a snapshot naming the record
        // the last one named is that file again. Skipping it is what keeps a person's one command
        // one write: an exclusion or a compaction announces every item, and the kernel has made all
        // of the changes before the first announcement arrives here
        let reflects = snapshot.last_seq;
        if kept.snapshotted == Some(reflects) {
            return Ok(());
        }
        let snapshot = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| format!("could not render the session: {e}"))?;
        let beside = crate::app::beside(&self.state, &snapshot)
            .map_err(|e| format!("could not write {}: {e}", self.state))?;
        std::fs::rename(&beside, &self.state).map_err(|e| {
            let _ = std::fs::remove_file(&beside);
            format!("could not write {}: {e}", self.state)
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
            .map_err(|e| format!("could not write {}: {e}", self.log))?;

        Ok(Recorded {
            records: kept.records,
            log: self.log.clone(),
            state: self.state.clone(),
        })
    }

    fn append_into(kept: &mut Kept, kernel: &Kernel) -> Result<(), String> {
        use std::io::Write as _;

        for record in kernel.history_since(kept.written) {
            let line = serde_json::to_string(&record)
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

/// The directory under the temporary one that a record goes in, made and checked.
fn private_dir() -> Result<std::path::PathBuf, String> {
    let mut dir = std::env::temp_dir();
    dir.push("kamchatka");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    private(&dir)?;

    Ok(dir)
}

/// Refuses `dir` unless it is a directory of this user's that only its owner can enter.
fn private(dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

    // note: opened without following a link, and made private and looked at through what was
    // opened, because the temporary directory is everybody's and this name is fixed. A link
    // somebody left at it is refused before anything is done through it, a chmod of what it
    // points at included. A directory somebody else made there first is one the chmod cannot
    // make private - it fails, and is ignored - so without the look the transcript would go
    // into a directory they can read.
    //
    // note: and whose it is, before the chmod, because the mode bits say nothing to a process
    // that can read past them. Run as root, or holding `CAP_DAC_OVERRIDE`, this writes into a
    // directory another user made `0700` there first, and chmods one they left open - and they
    // read the transcript either way
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
            // it may already exist from an earlier run, made before this did it; either way,
            // this is the run that is about to write a transcript into it
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

    Ok(())
}

/// A `.jsonl` and `.json` pair under `stem` that no other session has written, and the log's
/// open file, which is the claim itself.
///
/// note: the name is a session's own, and a session's own name is not unique enough to be a
/// filename. Two of them collide in two ways, both silently. Two runs started inside one second
/// share a stamp, so the second to finish would write over the first. And **a resumed session
/// keeps the name of the session it resumed**, which is right for what a name is for and means
/// `-r` would write over the very file it had just read: the log of what happened replaced by that
/// of a sitting that did nothing. The snapshot would come through nearly intact, because a resumed
/// context renders to nearly the same bytes; the log would not, and the log is the half that says
/// what happened rather than where things ended up.
///
/// note: so the name stays what it is and the *file* moves - `…Z-2.jsonl` beside `…Z.jsonl`,
/// which sorts next to its sibling and reads as the second sitting of one session. Renaming the
/// session instead would put a process identifier in every filename to fix something rare, and
/// the name is what `#fork` derives from and what the trace shows.
///
/// note: `create_new` rather than asking whether the file is there, because between asking and
/// writing is exactly where the first of those two collisions lives. The `.json` is checked
/// before the `.jsonl` is claimed, so a suffix this passes over leaves nothing of its own behind.
///
/// note: a name that is taken is passed over and nothing else is: a directory that cannot be
/// written in, or a full disk, is the reason the record is not there, and saying "no unused name"
/// a thousand tries later would be the wrong one.
pub(crate) fn unclaimed(stem: &std::path::Path) -> Result<(String, String, std::fs::File), String> {
    // bounded, so that a directory full of these is an error rather than a loop
    for nth in 1..1_000 {
        let stem = match nth {
            1 => stem.display().to_string(),
            nth => format!("{}-{nth}", stem.display()),
        };
        let (log, state) = (format!("{stem}.jsonl"), format!("{stem}.json"));
        // anything there at all, a link to nothing included, which `exists` answers no to and
        // a write would follow
        if std::fs::symlink_metadata(&state).is_ok() {
            continue;
        }

        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log)
        {
            Ok(file) => return Ok((log, state, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not write {log}: {e}")),
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
    /// note: the second half is the case that matters, and it is not the exotic one: `-r` is the
    /// line this program prints at the end of every run, and a resumed session keeps the name of
    /// the session it resumed, so every resume would otherwise write over the log it had just read.
    #[test]
    fn a_record_never_writes_over_one_that_is_already_there() {
        // note: not `tests/common`'s `scratch`, which builds under `CARGO_TARGET_TMPDIR` -
        // cargo hands that to integration tests and not to a unit test. One fixed name, emptied
        // on the way in, so nothing accumulates either
        let dir = std::env::temp_dir().join("kamchatka-unclaimed");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to work in");
        let stem = dir.join("2026-09-15T13-34-29Z");

        let (log, state, _) = unclaimed(&stem).expect("nothing is there yet");
        assert!(log.ends_with("2026-09-15T13-34-29Z.jsonl"), "{log}");
        assert!(state.ends_with("2026-09-15T13-34-29Z.json"), "{state}");
        // what a session that got this far would leave behind
        std::fs::write(&log, "one").expect("written");
        std::fs::write(&state, "{}").expect("written");

        // the same name again - two runs in one second, or a resume - lands beside it
        let (again, beside, _) = unclaimed(&stem).expect("a second name");
        assert!(again.ends_with("2026-09-15T13-34-29Z-2.jsonl"), "{again}");
        assert!(beside.ends_with("2026-09-15T13-34-29Z-2.json"), "{beside}");
        assert_eq!(
            std::fs::read_to_string(&log).expect("still there"),
            "one",
            "the first record is untouched"
        );

        // and the claim is the file itself, so a third does not get the second's name back
        std::fs::write(&beside, "{}").expect("written");
        let (third, _, _) = unclaimed(&stem).expect("a third name");
        assert!(third.ends_with("2026-09-15T13-34-29Z-3.jsonl"), "{third}");
    }

    /// A link at the snapshot's name is a name that is taken, even when it points at nothing.
    ///
    /// note: `exists` follows a link and answers no for one to a file that is not there, and the
    /// write after it follows the link too - so a link left at a predictable name would be a file
    /// created wherever it pointed.
    #[test]
    fn a_link_at_the_snapshots_name_is_not_written_through() {
        let dir = std::env::temp_dir().join("kamchatka-unclaimed-link");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to work in");
        let stem = dir.join("2026-09-22T12-00-00Z");
        std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("2026-09-22T12-00-00Z.json"))
            .expect("a link");

        let (log, state, _) = unclaimed(&stem).expect("a name beside it");
        assert!(log.ends_with("2026-09-22T12-00-00Z-2.jsonl"), "{log}");
        assert!(state.ends_with("2026-09-22T12-00-00Z-2.json"), "{state}");
    }
}
