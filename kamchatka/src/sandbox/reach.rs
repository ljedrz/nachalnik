//! What `fs` may reach: the boundary the shell's ruleset draws, held by this program's own code
//! for the tools the kernel cannot confine.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use nachalnik::Capability;

use crate::tools::Careful;

use super::{SYSTEM, resolve, writes};

/// Where the tools may work, for the ones the kernel cannot confine.
///
/// note: Landlock confines a *process*, and `fs` is not one - its `read`, `write`, `edit`, `grep`
/// and `glob` run on the terminal's own threads (the last two on a thread that may block, which is
/// still this process), and a ruleset applied there would confine the terminal. So they are held to
/// the same boundary by the only thing that can hold them to it, which is their own code. That is
/// weaker in kind: it is this program refusing rather than the kernel refusing, and a bug here is a
/// way out where a bug in the ruleset is not. It is still the difference between an `fs` that will
/// hand a model `~/.ssh/id_rsa` and one that will not.
#[derive(Debug, Clone)]
pub struct Reach {
    /// The directory the tools may work in.
    pub workdir: PathBuf,
    /// Extra paths the user opened up, read-write.
    pub extra: Vec<PathBuf>,
    /// Extra paths the user opened up for reading only.
    pub readable: Vec<PathBuf>,
    /// Whether to hold them to it at all; `--no-sandbox` turns this off.
    pub confined: bool,
}

/// What a tool is about to do with a path, which is what decides whether a read-only path is in
/// reach for it.
///
/// note: an argument rather than two methods, so that every call site says which it is. The
/// distinction only exists because of `--sandbox-read`, and a default would put it back where it
/// was: `write` quietly allowed somewhere only `read` was meant to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Opening it and no more.
    Reading,
    /// Creating, changing or replacing it.
    Writing,
}

/// Whether a refusal from `fs` may send a model to `shell`: it is offered, and may run; see
/// [`Careful::reachable`].
fn runs(policy: &Careful) -> bool {
    policy.reachable(&Capability::exec("run"))
}

impl Reach {
    /// Everywhere it reaches and what may be done there, in the order the rules are consulted.
    ///
    /// note: a refusal that names only the working directory and calls it as far as this session
    /// reaches stops being true the moment anybody passes `--sandbox-allow` or `--sandbox-read`.
    /// A refusal that under-reports the reach is worse than a vague one: a model reads it as the
    /// whole boundary and never goes near the path somebody opened up for exactly this, and there
    /// is nothing in front of it to say otherwise. [`Shell`](crate::tools::Shell) names them in
    /// its description for the same reason.
    ///
    /// note: the spelling is [`Sandbox`]'s, down to the `read-write` after each path, because the
    /// two say the same thing about the same session and a reader should not have to notice which
    /// of them is talking.
    ///
    /// note: and it is spelled under what the policy makes of writing, as `Sandbox::of` is. A
    /// refusal of `fs:write` makes every path here read-only for `shell`, and a refusal that went
    /// on calling them read-write would be two rules for one session.
    fn range(&self, writing: bool) -> String {
        let written = match writing {
            true => "read-write",
            false => "read-only",
        };
        std::iter::once(&self.workdir)
            .chain(self.extra.iter())
            .map(|path| format!("{} {written}", path.display()))
            .chain(
                self.readable
                    .iter()
                    .map(|path| format!("{} read-only", path.display())),
            )
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Everywhere it may change something, which is what a refused write needs to hear.
    fn writable(&self) -> String {
        std::iter::once(&self.workdir)
            .chain(self.extra.iter())
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Returns the path to use, or what to tell the model instead.
    ///
    /// note: resolved before it is compared, so that `../` and a symlink out are the same question
    /// as a plain absolute path. A path that does not exist yet - which is most of what `write` is
    /// handed - is resolved through its parent, because a file cannot be created outside a
    /// directory it is not in.
    ///
    /// note: a leading `~` is refused with a sentence rather than expanded, and it is refused
    /// *before* the unconfined early return. These tools run in process with no shell in front of
    /// them, so nothing expands it - and expanding it here would mean `--no-sandbox` handing over
    /// `$HOME/.ssh/id_rsa` for real, on a path the model wrote. Not expanding it is the safe
    /// default. Saying nothing is not: `~/.gitconfig` then joins onto the working directory as a
    /// directory literally called `~` and comes back `No such file or directory`. That is the
    /// `access(2)` trap in a second form: an error indistinguishable from the file being absent,
    /// which a model believes - so it concludes the home directory is empty rather than that its
    /// path was taken at its word, and nothing in front of it says otherwise.
    ///
    /// note: the sentence says the same path will be refused again, because a refusal that does
    /// not close the retry is an invitation to retry, and the same path comes straight back. And
    /// it names no path but the one it was handed. Every concrete path in a refusal is read as a
    /// path to try, because a refusal is read under pressure to try something else: offered
    /// `./~`, a model refused `~/notes.txt` reads `./~`. That spelling lives in `PATH_ARG`
    /// instead, which is read while choosing.
    pub fn allows(&self, path: &str, doing: Access) -> Result<PathBuf, String> {
        self.judged(path, doing, true, true)
    }

    /// [`Reach::allows`], with a refusal that says what `policy` makes of writing.
    ///
    /// note: what is allowed is the same either way. It is the policy that refuses a write, and it
    /// does that before a call gets here; this is only the sentence a refusal names the reach in.
    pub(crate) fn allows_under(
        &self,
        path: &str,
        doing: Access,
        policy: &Careful,
    ) -> Result<PathBuf, String> {
        self.judged(path, doing, writes(policy), runs(policy))
    }

    fn judged(
        &self,
        path: &str,
        doing: Access,
        writing: bool,
        shell: bool,
    ) -> Result<PathBuf, String> {
        // the whole string, not any component: `notes.txt~` is a real file and `./~` is how a
        // shell asks for a literal one, so both go through untouched and the message says so
        if path.starts_with('~') {
            return Err(format!(
                "{path}: `~` is not expanded here, and this path will be refused again exactly as \
                 it stands. `fs` does not go through a shell, so `~` was read as a directory of \
                 that name rather than as a home directory. Say the path in full, or relative to \
                 {}.",
                self.workdir.display()
            ));
        }

        let path = PathBuf::from(path);
        let absolute = match path.is_absolute() {
            true => path.clone(),
            false => self.workdir.join(&path),
        };
        let Some(resolved) = resolve(&absolute) else {
            return Err(format!(
                "{}: a chain of links too long to follow to its end, so where it leads cannot be \
                 checked",
                path.display()
            ));
        };
        // note: resolved even where nothing is held to the reach, because the path rules still
        // are, and a link is refused for leading past one by comparing where it leads with the
        // name it was asked for by. Handed back as it came, the two are the same name
        if !self.confined {
            return Ok(resolved);
        }
        // note: what `resolve` cannot resolve it leaves as it was, and a `..` after a directory
        // that is not there is one of those: `nope/../../../etc/passwd` came back with its `..`s
        // still in it, and a comparison by components found the working directory at the front of
        // it. Where such a path ends up depends on a directory that does not exist yet, so it is
        // not checked, it is refused
        if resolved
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(format!(
                "{}: goes up with `..` out of a directory that is not there, so where it ends up \
                 cannot be checked; say the path without the `..`",
                path.display()
            ));
        }

        let readable = matches!(doing, Access::Reading);
        match std::iter::once(&self.workdir)
            .chain(self.extra.iter())
            .chain(self.readable.iter().filter(|_| readable))
            .any(|allowed| {
                allowed
                    .canonicalize()
                    .is_ok_and(|allowed| resolved.starts_with(allowed))
            }) {
            true => Ok(resolved),
            // note: written for the model, which is what reads it. It cannot restart this
            // program or pass it a flag, so being told to is worse than being told nothing: what
            // it can do is work inside the directory, or say what it needs and why
            // note: a path opened for reading and asked for in writing gets its own answer.
            // Telling a model that `~/.rustup` is "outside what this session reaches" when it has
            // just read a file there is a contradiction it cannot do anything with, and the
            // useful fact - that this one is read-only - is one this knows
            false
                if !readable
                    && self.readable.iter().any(|allowed| {
                        allowed
                            .canonicalize()
                            .is_ok_and(|allowed| resolved.starts_with(allowed))
                    }) =>
            {
                Err(format!(
                    "{}: opened for reading only, so it cannot be changed. This session may write \
                     in {} - write there instead, or ask for this path to be opened up for \
                     writing and say what you need it for.",
                    path.display(),
                    self.writable()
                ))
            }
            // note: the system directories are in the shell's reach, read-only, and not in this
            // one. Sent to ask for one to be opened up, a model asks for a file it could already
            // `cat`; told where it can be read, it reads it there
            false
                if readable
                    && shell
                    && SYSTEM.iter().any(|system| resolved.starts_with(system)) =>
            {
                Err(format!(
                    "{}: in the system directories, which `shell` reads and `fs` does not - `fs` \
                     reaches {} - so this path will be refused here again as it stands. Read it \
                     through `shell` instead.",
                    path.display(),
                    self.range(writing)
                ))
            }
            false => Err(format!(
                "{}: outside what this session reaches, which is {}. Work where it does, or ask \
                 for this path to be opened up and say what you need it for.",
                path.display(),
                self.range(writing)
            )),
        }
    }
}

impl Reach {
    /// Opens a path [`Reach::allows`] answered for, beneath the directory it was allowed under:
    /// for reading, or for writing from the start.
    ///
    /// note: `allows` resolves a path and checks it, and the open comes after - so a part of the
    /// path swapped for a link in between would be followed wherever the link pointed. On Linux
    /// this opens with `openat2` and `RESOLVE_BENEATH` from the directory the path was allowed
    /// under, and the kernel will not let the open leave that directory whatever is on disk by
    /// then, so a swap is refused rather than followed. On a kernel that has no `openat2` it is an
    /// ordinary open and the window is the one SECURITY.md describes.
    ///
    /// note: `path` is what `allows` returned, which is resolved, so a link met on the way is one
    /// that was not there when the path was checked, and refusing it refuses nothing that was
    /// allowed.
    ///
    /// note: and only a regular file. A pipe blocks an open until somebody writes to it, which
    /// nobody will, and the open is not a place an interrupt reaches - so `mkfifo p` and `fs read p`
    /// was a turn nobody could stop. The path is looked at first, which is what the sentence comes
    /// from; the open does not block either, whatever is there by then, and what it opened is
    /// looked at again before anything reads it.
    pub fn open(&self, path: &Path, doing: Access) -> std::io::Result<std::fs::File> {
        self.opened(path, doing, None)
    }

    /// [`Reach::open`], for a tool whose refusal of what is not a file may name where else to go -
    /// as far as `policy` says the model could go there.
    pub(crate) fn open_for(
        &self,
        path: &Path,
        doing: Access,
        policy: &Careful,
    ) -> std::io::Result<std::fs::File> {
        self.opened(path, doing, Some(policy))
    }

    fn opened(
        &self,
        path: &Path,
        doing: Access,
        policy: Option<&Careful>,
    ) -> std::io::Result<std::fs::File> {
        if std::fs::metadata(path).is_ok_and(|meta| !meta.is_file()) {
            return Err(irregular(doing, policy));
        }

        let mut options = std::fs::OpenOptions::new();
        match doing {
            Access::Reading => options.read(true),
            Access::Writing => options.write(true).create(true).truncate(true),
        };
        std::os::unix::fs::OpenOptionsExt::custom_flags(
            &mut options,
            rustix::fs::OFlags::NONBLOCK.bits() as i32,
        );
        let opened = match self.confined {
            false => options.open(path)?,
            true => match beneath(&self.root(path, doing)?, path, doing) {
                Some(opened) => opened?,
                None => options.open(path)?,
            },
        };

        match opened.metadata()?.is_file() {
            true => Ok(opened),
            false => Err(irregular(doing, policy)),
        }
    }

    /// Replaces the whole of a file [`Reach::allows`] answered for with `content`, creating it if
    /// it is not there.
    ///
    /// note: written into a new file beside it and renamed over it, so that a full disk or a
    /// process killed halfway leaves the file as it was rather than shorter than either version.
    /// The new file and the rename go through the directory as it was opened, beneath what it was
    /// allowed under, for the reason [`Reach::open`] gives.
    ///
    /// note: a rename puts a different file at the path, and where that would show the file is
    /// written in place instead, as it was before: when another hard link shares it, since the link
    /// would keep the old contents; when the new file would not carry the old one's owner and group,
    /// which only root could set; when its owner has made it read-only, so that the open refuses it
    /// as it always did rather than a rename stepping round the refusal; when the directory will
    /// not take a new file, or its name is too long to carry the temporary's suffix; when what
    /// is there is not a regular file; and when the file was allowed on its own -
    /// `--sandbox-allow notes.txt` - since the directory it is in was never given, and a new file
    /// made there would be one a kill in the middle leaves outside the reach. The permission bits
    /// are copied across. Extended attributes are not.
    pub fn replace(&self, path: &Path, content: &[u8]) -> std::io::Result<()> {
        let was = std::fs::symlink_metadata(path).ok();
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return self.overwrite(path, content);
        };
        if self.confined && self.root(path, Access::Writing)?.is_file() {
            return self.overwrite(path, content);
        }
        if was
            .as_ref()
            .is_some_and(|was| !was.is_file() || linked(was) || protected(was))
        {
            return self.overwrite(path, content);
        }

        let beside = self.beside(dir, path)?;
        let (temporary, mut file) = match beside.create(name) {
            Ok(created) => created,
            // a name too long to carry the temporary's suffix is still one a file can have
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidFilename
                ) =>
            {
                return self.overwrite(path, content);
            }
            Err(e) => return Err(e),
        };
        let renamed = (|| {
            if let Some(was) = &was {
                if !owned_alike(was, &file.metadata()?) {
                    return Ok(false);
                }
                file.set_permissions(was.permissions())?;
            }
            std::io::Write::write_all(&mut file, content)?;
            file.sync_all()?;
            beside.rename(&temporary, name)?;
            Ok(true)
        })();

        match renamed {
            Ok(true) => Ok(()),
            Ok(false) => {
                beside.remove(&temporary);
                self.overwrite(path, content)
            }
            Err(e) => {
                beside.remove(&temporary);
                Err(e)
            }
        }
    }

    /// Writes over a file where it is, which is what [`Reach::replace`] does when a rename would
    /// change more than the contents.
    fn overwrite(&self, path: &Path, content: &[u8]) -> std::io::Result<()> {
        let mut file = self.open(path, Access::Writing)?;
        std::io::Write::write_all(&mut file, content)?;
        std::io::Write::flush(&mut file)
    }

    /// The directory `path` is replaced in, opened beneath where it was allowed when that can be
    /// done.
    fn beside(&self, dir: &Path, path: &Path) -> std::io::Result<Beside> {
        let named = Beside::Named(dir.to_path_buf());
        if !self.confined {
            return Ok(named);
        }

        match directory_beneath(&self.root(path, Access::Writing)?, dir) {
            Some(held) => held,
            None => Ok(named),
        }
    }

    /// The allowed directory `path` is under, as `open` and `replace` start from it.
    fn root(&self, path: &Path, doing: Access) -> std::io::Result<PathBuf> {
        let readable = matches!(doing, Access::Reading);
        std::iter::once(&self.workdir)
            .chain(self.extra.iter())
            .chain(self.readable.iter().filter(|_| readable))
            .filter_map(|allowed| allowed.canonicalize().ok())
            .find(|allowed| path.starts_with(allowed))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "outside what this session reaches",
                )
            })
    }
}

/// What opening something that is not a regular file comes to, as `doing` names it.
///
/// note: a directory is not one refusal whichever way it was asked for. Read, it is a list of
/// paths, which is what `glob` answers - and `shell` reads one that is meant to be read, a line
/// in the middle of a log - each named where `policy` says the model could use it, and neither
/// where there is no policy to ask. Written, it is the only thing a `write` is never for, and the
/// name of it is what a caller needs; `shell`, which can make a directory, is no part of that
/// answer.
fn irregular(doing: Access, policy: Option<&Careful>) -> std::io::Error {
    let reading = match doing {
        Access::Reading => policy
            .and_then(|policy| {
                policy.ways(&[
                    (
                        Capability::fs("glob"),
                        "`glob` lists what a directory holds",
                    ),
                    (
                        Capability::exec("run"),
                        "`shell` can read one that is meant to be read",
                    ),
                ])
            })
            .map(|ways| format!("; {ways}"))
            .unwrap_or_default(),
        Access::Writing => ", so nothing was written; `fs` makes no directories".to_owned(),
    };
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "not a regular file - a directory, a pipe or a device - so it was not opened{reading}"
        ),
    )
}

/// Where the file [`Reach::replace`] writes is made, and renamed from.
enum Beside {
    /// The directory, opened beneath what it was allowed under.
    Held(rustix::fd::OwnedFd),
    /// The directory by name, where the kernel will not open beneath one.
    Named(PathBuf),
}

impl Beside {
    /// Makes a file nobody else has, named after `name` so that one left behind says whose it was.
    fn create(&self, name: &std::ffi::OsStr) -> std::io::Result<(OsString, std::fs::File)> {
        static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let mut temporary = OsString::from(".");
            temporary.push(name);
            temporary.push(format!(
                ".{}.{}.kamchatka",
                std::process::id(),
                MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let created = match self {
                Self::Held(dir) => {
                    use rustix::fs::{Mode, OFlags};
                    rustix::fs::openat(
                        dir,
                        temporary.as_os_str(),
                        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
                        Mode::from_raw_mode(0o666),
                    )
                    .map(std::fs::File::from)
                    .map_err(std::io::Error::from)
                }
                Self::Named(dir) => std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(dir.join(&temporary)),
            };
            match created {
                Ok(file) => return Ok((temporary, file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn rename(&self, from: &std::ffi::OsStr, to: &std::ffi::OsStr) -> std::io::Result<()> {
        match self {
            Self::Held(dir) => Ok(rustix::fs::renameat(dir, from, dir, to)?),
            Self::Named(dir) => std::fs::rename(dir.join(from), dir.join(to)),
        }
    }

    /// Takes away a file `create` made; one that cannot be is left, and its name says what it is.
    fn remove(&self, name: &std::ffi::OsStr) {
        let _ = match self {
            Self::Held(dir) => rustix::fs::unlinkat(dir, name, rustix::fs::AtFlags::empty())
                .map_err(std::io::Error::from),
            Self::Named(dir) => std::fs::remove_file(dir.join(name)),
        };
    }
}

/// Whether another name shares this file, so that renaming over one would part them.
fn linked(was: &std::fs::Metadata) -> bool {
    std::os::unix::fs::MetadataExt::nlink(was) > 1
}

/// Whether the file's owner has taken away their own right to write it.
fn protected(was: &std::fs::Metadata) -> bool {
    std::os::unix::fs::PermissionsExt::mode(&was.permissions()) & 0o200 == 0
}

/// Whether a new file has the owner and group of the one it would stand in for.
fn owned_alike(was: &std::fs::Metadata, new: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    (was.uid(), was.gid()) == (new.uid(), new.gid())
}

/// Opens `path` without leaving `root`, or `None` where the kernel will not do that.
///
/// note: `None` for `ENOSYS`, a kernel older than 5.6, and for `EPERM`, which is what a container
/// runtime's seccomp filter answers for `openat2` when it does not know the call. The ordinary open
/// that follows gives the answer it would have given without this, and the real error where there
/// is one.
fn beneath(root: &Path, path: &Path, doing: Access) -> Option<std::io::Result<std::fs::File>> {
    use rustix::fs::{Mode, OFlags};

    // a mode only beside `CREATE`: `openat2` refuses one anywhere else, where `open` ignores it.
    // `NONBLOCK` so that a pipe is opened and refused rather than waited on; see `Reach::open`
    let (flags, mode) = match doing {
        Access::Reading => (OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty()),
        Access::Writing => (
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o666),
        ),
    };
    opened_beneath(root, path, flags, mode).map(|opened| opened.map(std::fs::File::from))
}

/// Opens the directory `dir` without leaving `root`, to make files in; `None` as for [`beneath`].
fn directory_beneath(root: &Path, dir: &Path) -> Option<std::io::Result<Beside>> {
    use rustix::fs::{Mode, OFlags};

    opened_beneath(root, dir, OFlags::PATH | OFlags::DIRECTORY, Mode::empty())
        .map(|opened| opened.map(Beside::Held))
}

fn opened_beneath(
    root: &Path,
    path: &Path,
    flags: rustix::fs::OFlags,
    mode: rustix::fs::Mode,
) -> Option<std::io::Result<rustix::fd::OwnedFd>> {
    use rustix::{
        fs::{Mode, OFlags, ResolveFlags},
        io::Errno,
    };

    // a file allowed on its own - `--sandbox-read notes.txt` - has nothing beneath it to hold an
    // open to, and opening it as a directory refused a file `Reach::allows` had just allowed. The
    // directory it is in is held instead and its name is the one step taken from there: the
    // place a kernel without `openat2` opens it from as well
    let root = match root.is_file() {
        true => root.parent().unwrap_or(root),
        false => root,
    };
    let dir = match rustix::fs::open(
        root,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(dir) => dir,
        Err(e) => return Some(Err(e.into())),
    };
    let relative = match path.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative,
        _ => Path::new("."),
    };

    match rustix::fs::openat2(
        &dir,
        relative,
        flags | OFlags::CLOEXEC,
        mode,
        ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
    ) {
        Ok(opened) => Some(Ok(opened)),
        Err(Errno::NOSYS | Errno::PERM) => None,
        Err(Errno::XDEV) => Some(Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "a part of this path has become a link out of what this session reaches since it \
             was checked, so it was not opened",
        ))),
        Err(e) => Some(Err(e.into())),
    }
}
