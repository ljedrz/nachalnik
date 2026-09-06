//! A confinement for the `shell` tool, so that the permission stances are enforced rather than
//! reported.
//!
//! note: this is the part of the workspace where a permission stops being a decision point with a
//! paper trail and becomes a boundary. The runtime cannot do it - it executes nothing, so it has
//! nothing to confine - and it is exactly the sort of thing that belongs in the program that
//! actually spawns the process. That is the whole argument for the seam.
//!
//! note: [Landlock](https://landlock.io), which is a Linux LSM a process applies to *itself*: no
//! privileges, no setuid helper, no container, no daemon. What it buys is that `network: deny` is
//! refused by the kernel on the `connect` syscall rather than by a policy reading the word `curl`,
//! which is the difference between a heuristic somebody can walk around and one they cannot. A
//! live model, refused a `curl`, reached the same page with `python3 -c "import urllib.request"`
//! on its very next call; under this, that call gets `Permission denied` from the kernel.
//!
//! note: and it is TCP that it buys, because TCP is what Landlock has: `ConnectTcp` and `BindTcp`
//! are its only two network access rights. A confined command can still send a UDP datagram, which
//! is enough to put bytes in a DNS query, and AF_UNIX is only reachable at all from a kernel that
//! has ABI 9. So `no network` here means no TCP, and it is written that way everywhere it is shown
//! rather than rounded up to something this cannot do. What still holds against the rest is the
//! filesystem: a command that cannot read a file has nothing to send.
//!
//! note: it is applied by re-executing *this program* in a mode that confines itself and then runs
//! the command. The alternative is `Command::pre_exec`, which is `unsafe`, and this workspace does
//! not have any. The child is deliberately not a `tokio` program: Landlock restricts the calling
//! thread, and a single-threaded helper is the one shape where that needs no thought.

use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};

use nachalnik::{Capability, Verdict};

use crate::tools::{Careful, Subject};

/// The argument that puts this program into the mode that confines itself and runs a command.
pub const EXEC_FLAG: &str = "--confine-and-run";

/// What the `shell` tool is allowed to reach.
///
/// note: read is not a stance here. A command that cannot read `/usr/bin` cannot run at all, so
/// the system directories are always readable and the interesting question is what is *writable*
/// and whether the network is reachable - which are the two stances a person actually changes.
///
/// note: "the network" is TCP. Landlock has two network access rights and both are TCP; a UDP
/// datagram still goes out. See the note at the top of this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    /// The directory a command may work in; everything outside it is out of reach.
    pub workdir: PathBuf,
    /// Extra paths the user asked for, read-write.
    pub extra: Vec<PathBuf>,
    /// Extra paths the user asked for, readable and no more.
    ///
    /// note: separate from `extra` rather than a flag on it, because the two are asked for by
    /// different flags for different reasons and a command may hold both at once. What sends
    /// most people here is a toolchain: `cargo` cannot start without `~/.rustup`, and a model
    /// that can *replace* the toolchain it is about to run is a worse trade than the one anybody
    /// meant to make.
    pub readable: Vec<PathBuf>,
    /// Whether the working directory is writable, or only readable.
    pub writable: bool,
    /// Whether the command may open a TCP connection.
    pub network: bool,
}

impl Sandbox {
    /// The confinement a command should run under, given what the policy currently answers.
    ///
    /// note: the network is reachable only if the stance is an outright `allow`, or if this
    /// particular call was allowed by a person who was asked about it. A stance of `ask` that
    /// nobody has been asked about yet is not permission.
    pub fn of(
        policy: &Careful,
        workdir: PathBuf,
        extra: Vec<PathBuf>,
        readable: Vec<PathBuf>,
        granted: bool,
    ) -> Self {
        Self {
            workdir,
            extra,
            readable,
            // a refusal of `write` reaches the shell too; anything short of a refusal leaves the
            // working directory writable, because a shell that cannot write in it is not one
            // anybody can work with
            writable: policy.stance(&Subject::Capability(Capability::Write)) != Verdict::Deny,
            network: granted
                || policy.stance(&Subject::Capability(Capability::Network)) == Verdict::Allow,
        }
    }

    /// The arguments that ask this program to confine itself this way and run `cmd`.
    pub fn argv(&self, cmd: &str) -> Vec<OsString> {
        let mut argv = vec![
            OsString::from(EXEC_FLAG),
            self.workdir.clone().into(),
            OsString::from(match self.writable {
                true => "rw",
                false => "ro",
            }),
            OsString::from(match self.network {
                true => "net",
                false => "nonet",
            }),
            OsString::from(self.extra.len().to_string()),
        ];
        argv.extend(self.extra.iter().map(|path| path.clone().into()));
        argv.push(OsString::from(self.readable.len().to_string()));
        argv.extend(self.readable.iter().map(|path| path.clone().into()));
        argv.push(cmd.into());

        argv
    }

    /// Reads back what [`Sandbox::argv`] wrote, plus the command; `None` if this is not one.
    pub fn from_argv(argv: &[OsString]) -> Option<(Self, OsString)> {
        let mut argv = argv.iter();
        if argv.next()? != EXEC_FLAG {
            return None;
        }
        let workdir = PathBuf::from(argv.next()?);
        let writable = argv.next()? == "rw";
        let network = argv.next()? == "net";
        let count: usize = argv.next()?.to_str()?.parse().ok()?;
        let extra: Vec<PathBuf> = argv.by_ref().take(count).map(PathBuf::from).collect();
        let count: usize = argv.next()?.to_str()?.parse().ok()?;
        let readable: Vec<PathBuf> = argv.by_ref().take(count).map(PathBuf::from).collect();
        let cmd = argv.next()?.clone();

        Some((
            Self {
                workdir,
                extra,
                readable,
                writable,
                network,
            },
            cmd,
        ))
    }

    /// Whether a confined command could open this path for reading.
    ///
    /// note: reading rather than writing, because every use of this is about a command that was
    /// refused one. A path under a read-only root answers `true` here and is still unwritable.
    ///
    /// note: resolved on both sides, so that a symlink and a `../` are the same question they are
    /// for [`Reach::allows`]. A path that is not there resolves through its parent - which is the
    /// common case, since a command that named a file it could not open often could not `stat`
    /// its directory either.
    fn reaches(&self, path: &Path) -> bool {
        let resolved = resolve(path);

        SYSTEM
            .iter()
            .map(PathBuf::from)
            .chain(std::iter::once(self.workdir.clone()))
            .chain(self.extra.iter().cloned())
            .chain(self.readable.iter().cloned())
            .any(|allowed| match allowed.canonicalize() {
                Ok(allowed) => resolved.starts_with(allowed),
                Err(_) => false,
            })
    }

    /// What to add to a confined command's output when a permission error in it was this
    /// confinement rather than the file's own permissions; `None` when nothing suggests it was.
    ///
    /// note: the whole reason this exists. Landlock refuses an `open` with `EACCES`, which is
    /// what a command reports when a file is somebody else's - so a model is handed
    /// `Permission denied (os error 13)` and has no way at all to tell a boundary from a
    /// protected file. A live session spent six calls hunting for a `cargo` that was never
    /// missing: it was `~/.rustup` that was out of reach, and nothing in front of the model said
    /// so. The tool's description says a permission error out here is the confinement; a
    /// sentence at the point of failure, naming the path, is what that description was for.
    ///
    /// note: three answers rather than two, and the third one is the point. A refusal that names
    /// a path *within* reach was the file's own permissions - `/etc/shadow` is refused under this
    /// and would be refused without it - and saying "this may have been the sandbox" there would
    /// be a hedge that sends a model looking for a boundary that had nothing to do with it. So a
    /// refusal whose paths are all reachable gets nothing said about it at all.
    ///
    /// note: the general sentence is for the refusal that names no path this can find. Standard
    /// error is arbitrary text and picking paths out of it is a guess; what is *not* a guess is
    /// that this command ran confined and something was refused, which is worth one line.
    ///
    /// note: it says "below" because [`Shell`](crate::tools::Shell) puts it under the status line
    /// and above the output. That is not where it reads best - beside the message would be - but
    /// an output limit cuts from the end, and a note accounting for a permission error is worth
    /// nothing if the truncation takes the note and leaves the error.
    pub fn note_for(&self, stderr: &str) -> Option<String> {
        let refusals: Vec<&str> = stderr.lines().filter(|line| refused(line)).collect();
        if refusals.is_empty() {
            return None;
        }

        let mentioned: Vec<String> = refusals.iter().flat_map(|line| paths_in(line)).collect();
        // `Vec::dedup` drops only neighbours, and a path is usually named by two lines that are
        // not next to each other - a warning and the failure it led to
        let mut named: Vec<&String> = Vec::new();
        for path in &mentioned {
            if !self.reaches(Path::new(path)) && !named.contains(&path) {
                named.push(path);
            }
        }
        named.truncate(3);

        match (named.is_empty(), mentioned.is_empty()) {
            // every path it named is one this reaches, so the refusal is the file's own
            (true, false) => None,
            (true, true) => Some(format!(
                "[this command ran confined - {self} - so a permission error below may be that \
                 boundary rather than the file's own permissions.]"
            )),
            (false, _) => Some(format!(
                "[{} is outside what this session reaches, so the permission error below is the \
                 confinement rather than the file's own permissions. This command runs with \
                 {self}. Work inside the working directory, or say what you need the path for \
                 and ask for it to be opened up.]",
                named
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
        }
    }

    /// What to hand a confined command as `GIT_CONFIG_GLOBAL`; `None` leaves git its own defaults.
    ///
    /// note: git is the one program where being out of reach is fatal rather than inconvenient,
    /// and the reason is a trap worth writing down: under Landlock, `access(2)` still answers
    /// from the file's own permissions. So git asks whether `~/.gitconfig` is readable, is told
    /// yes, opens it, gets `EACCES`, and takes the *unreadable configuration* branch rather than
    /// the *no configuration* branch - `fatal: unknown error occurred while reading the
    /// configuration files`, and every git command in the session is dead. A missing file is
    /// fine; an unreadable one is not, and a confined command cannot tell git which it has.
    ///
    /// note: only when one of them is actually out of reach, and only when nobody set the
    /// variable already. Git reads a person's aliases and identity out of these, and quietly
    /// throwing them away for a command that could have had them would be its own bug.
    fn git_config_global(&self) -> Option<PathBuf> {
        if std::env::var_os("GIT_CONFIG_GLOBAL").is_some() {
            return None;
        }

        let (mine, missed): (Vec<PathBuf>, Vec<PathBuf>) = global_git_config()
            .into_iter()
            .filter(|path| path.is_file())
            .partition(|path| self.reaches(path));

        match missed.is_empty() {
            true => None,
            // the reachable one if there is one, so that a person who opened up `~/.gitconfig`
            // and left `~/.config/git/config` behind keeps the half they asked for
            false => Some(
                mine.into_iter()
                    .next()
                    .unwrap_or_else(|| "/dev/null".into()),
            ),
        }
    }
}

/// The deepest existing part of a path, resolved, with whatever is left over joined back on.
///
/// note: a file about to be created has no canonical form of its own, and a path handed to
/// `write` is usually one of those. It is resolved through its parent instead, because a file
/// cannot be created outside a directory it is not in.
///
/// note: one function rather than two, so that [`Reach::allows`] and [`Sandbox::reaches`] cannot
/// come to different answers about the same path - which is the whole substance of both.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = PathBuf::new();
    loop {
        match existing.canonicalize() {
            // note: joined only when there is something to join. `Path::join("")` appends a
            // separator, and `/w/local.txt/` is a directory that is not there - which is how a
            // plain `./local.txt` came back `Not a directory` the first time this ran
            Ok(resolved) if rest.as_os_str().is_empty() => break resolved,
            Ok(resolved) => break resolved.join(&rest),
            Err(_) => match (existing.file_name(), existing.parent()) {
                (Some(name), Some(parent)) => {
                    // note: and the same guard here, for the same reason. Without it every path
                    // that does not exist yet came back with a separator on the end, so `write`
                    // could create no file at all: `notes.txt/` is a directory, and the tool
                    // reported `Is a directory (os error 21)` for a file it had just been asked
                    // to make
                    rest = match rest.as_os_str().is_empty() {
                        true => PathBuf::from(name),
                        false => Path::new(name).join(&rest),
                    };
                    existing = parent;
                }
                _ => break path.to_path_buf(),
            },
        }
    }
}

/// Whether a line of standard error looks like something was refused permission.
///
/// note: three spellings because three layers write them: a C program's `strerror`, Rust's
/// `io::Error` display, and the errno name itself. All three are the same refusal.
fn refused(line: &str) -> bool {
    line.contains("Permission denied") || line.contains("os error 13") || line.contains("EACCES")
}

/// The absolute paths a line of standard error mentions.
///
/// note: a token that is absolute once the punctuation a message wraps one in has been taken off.
/// Stripped *first* and tested afterwards, because the message that sent anybody here reads
/// `could not read settings file: '/home/you/.rustup/settings.toml': Permission denied`, where the
/// token is `'/home/...':` and does not start with a slash at all.
///
/// note: it misses a path with a space in it, which is the right way round: a caller that gets
/// nothing says something general instead, and one that gets a wrong path would say something
/// false.
fn paths_in(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(|token| token.trim_matches(|c: char| "'\"`,;:.()[]<>".contains(c)))
        .filter(|token| token.starts_with('/') && token.len() > 1)
        .map(str::to_owned)
        .collect()
}

/// Where git looks for a person's own configuration, in the order it reads them.
fn global_git_config() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));

    [
        xdg.map(|xdg| xdg.join("git").join("config")),
        home.map(|home| home.join(".gitconfig")),
    ]
    .into_iter()
    .flatten()
    .collect()
}

impl fmt::Display for Sandbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}",
            self.workdir.display(),
            match self.writable {
                true => "read-write",
                false => "read-only",
            }
        )?;
        for path in &self.extra {
            write!(f, ", {} read-write", path.display())?;
        }
        for path in &self.readable {
            write!(f, ", {} read-only", path.display())?;
        }
        write!(
            f,
            ", the system directories read-only, {}",
            match self.network {
                true => "the network reachable",
                // TCP is the whole of what Landlock can refuse; see the note at the top
                false => "no TCP",
            }
        )
    }
}

/// Where the tools may work, for the ones the kernel cannot confine.
///
/// note: Landlock confines a *process*, and three of these four tools are not one - `read`,
/// `write` and `edit` run on the terminal's own threads, and a ruleset applied there would confine
/// the terminal. So they are held to the same boundary by the only thing that can hold them to it,
/// which is their own code. That is weaker in kind: it is this program refusing rather than the
/// kernel refusing, and a bug here is a way out where a bug in the ruleset is not. It is still the
/// difference between a `read` tool that will hand a model `~/.ssh/id_rsa` and one that will not.
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

impl Reach {
    /// Returns the path to use, or what to tell the model instead.
    ///
    /// note: resolved before it is compared, so that `../` and a symlink out are the same question
    /// as a plain absolute path. A path that does not exist yet - which is most of what `write` is
    /// handed - is resolved through its parent, because a file cannot be created outside a
    /// directory it is not in.
    pub fn allows(&self, path: &str, doing: Access) -> Result<PathBuf, String> {
        let path = PathBuf::from(path);
        if !self.confined {
            return Ok(path);
        }

        let absolute = match path.is_absolute() {
            true => path.clone(),
            false => self.workdir.join(&path),
        };
        let resolved = resolve(&absolute);

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
                    "{}: opened for reading only, so it cannot be changed. Write inside {} \
                     instead, or ask for this path to be opened up for writing and say what you \
                     need it for.",
                    path.display(),
                    self.workdir.display()
                ))
            }
            false => Err(format!(
                "{}: outside {}, which is as far as this session reaches. Work inside that \
                 directory, or ask for this path to be opened up and say what you need it for.",
                path.display(),
                self.workdir.display()
            )),
        }
    }
}

/// How much of a [`Sandbox`] the kernel actually agreed to.
///
/// note: the point of a separate value is that "not confined" must be sayable. A sandbox that
/// silently did nothing on an old kernel, or on a platform that has no Landlock, would be the
/// worst thing in this workspace: a promise on the screen and nothing behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confinement {
    /// Every restriction asked for is in force.
    Full,
    /// The kernel understood some of it; what it did not understand is not restricted.
    Partial,
    /// Landlock is not available - too old a kernel, or not enabled at boot.
    Unavailable,
    /// This platform has no Landlock at all.
    Unsupported,
}

impl Confinement {
    /// Returns whether a command running under this is actually restricted.
    pub fn is_confined(self) -> bool {
        matches!(self, Self::Full | Self::Partial)
    }

    /// What to tell somebody, in one line; `None` when everything asked for is in force.
    pub fn complaint(self) -> Option<&'static str> {
        match self {
            Self::Full => None,
            Self::Partial => Some("the kernel enforced only part of the sandbox"),
            Self::Unavailable => {
                Some("this kernel has no Landlock, so the shell is not confined at all")
            }
            Self::Unsupported => {
                Some("this platform has no Landlock, so the shell is not confined at all")
            }
        }
    }
}

impl fmt::Display for Confinement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full => "confined",
            Self::Partial => "partly confined",
            Self::Unavailable => "not confined (no Landlock in this kernel)",
            Self::Unsupported => "not confined (no Landlock on this platform)",
        })
    }
}

/// The directories a command has to be able to read before it can be a command at all.
///
/// note: not behind a `cfg`, because [`Sandbox::reaches`] answers on every platform and a
/// constant that a portable method names cannot be one. Off Linux nothing is confined, so
/// nothing asks.
const SYSTEM: &[&str] = &[
    "/usr", "/etc", "/bin", "/sbin", "/lib", "/lib64", "/opt", "/proc", "/sys", "/run",
];

/// Applies the sandbox to *this* process, returning how much of it the kernel took.
///
/// note: `scratch` is a directory of this run's own, handed over as `TMPDIR`, rather than the
/// whole of `/tmp`. A command that cannot write a temporary file will not run - a compiler or an
/// interpreter needs one - but opening up `/tmp` wholesale gives it a shared space to write into,
/// and, if somebody happened to be working in a directory under `/tmp`, quietly makes a refused
/// `write` stance mean nothing at all. That last one is not hypothetical: it is what the test for
/// the read-only case caught on the first run.
///
/// note: and it is an [`Option`], because [`make_scratch`] is allowed to fail and this must not
/// paper over it. A path that cannot be opened makes `add_rules` fail, which would come back
/// `Unavailable` - a *command running unconfined* because its temporary directory was not there.
/// No scratch means no `TMPDIR` and a confinement that still holds.
///
/// note: `/dev` gets reading and writing of files and nothing else, because `/dev/null` is not
/// optional and creating things in `/dev` is not something a shell command needs to do.
#[cfg(target_os = "linux")]
pub fn confine(sandbox: &Sandbox, scratch: Option<&Path>) -> Confinement {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
        path_beneath_rules,
    };

    // note: V3 rather than V1, for `Truncate`. An access right the ruleset does not *handle* is
    // not restricted at all, and truncation is not covered by V1's `WriteFile`: `truncate(2)`
    // takes a path and never opens the file, so a confined command could zero any file it could
    // name - `os.truncate('/home/you/.bashrc', 0)` came back with nothing to say and a file of
    // nought bytes. Handling it costs nothing here, since `from_all` grants it again on every
    // writable path. V2's `Refer` comes with it and is the more permissive of the two: with it
    // unhandled the kernel refuses every cross-directory rename outright, and with it granted a
    // `mv` between two directories of the working directory is allowed, which is what anybody
    // would expect of a shell in there.
    //
    // note: not V5's `IoctlDev` - `/dev` is granted reading and writing rather than the whole of
    // `from_all`, so handling it would deny ioctls on `/dev/null` and on a terminal to every
    // ordinary command - and not V9's `ResolveUnix`, which is the one that would close AF_UNIX
    // and needs a kernel from 2026. On a kernel older than 6.2 the rights below V3 still apply
    // and the status comes back `Partial`, which is said out loud rather than rounded up.
    let abi = ABI::V3;
    let Ok(mut ruleset) = Ruleset::default().handle_access(AccessFs::from_all(abi)) else {
        return Confinement::Unavailable;
    };
    if !sandbox.network {
        // ABI v4 and up; on an older kernel this is the part that comes back `Partial`
        match ruleset.handle_access(AccessNet::ConnectTcp | AccessNet::BindTcp) {
            Ok(with_net) => ruleset = with_net,
            Err(_) => return Confinement::Unavailable,
        }
    }

    let writable: Vec<PathBuf> = std::iter::once(sandbox.workdir.clone())
        .filter(|_| sandbox.writable)
        .chain(scratch.map(Path::to_path_buf))
        .chain(sandbox.extra.iter().cloned())
        .collect();
    let readable: Vec<PathBuf> = SYSTEM
        .iter()
        .map(PathBuf::from)
        .chain(std::iter::once(sandbox.workdir.clone()))
        .chain(sandbox.readable.iter().cloned())
        .collect();

    let restricted = ruleset
        .create()
        .and_then(|created| {
            created.add_rules(path_beneath_rules(&readable, AccessFs::from_read(abi)))
        })
        .and_then(|created| {
            created.add_rules(path_beneath_rules(
                &["/dev"],
                AccessFs::ReadFile | AccessFs::WriteFile,
            ))
        })
        .and_then(|created| {
            created.add_rules(path_beneath_rules(&writable, AccessFs::from_all(abi)))
        })
        .and_then(|created| created.restrict_self());

    match restricted {
        Ok(status) => match status.ruleset {
            RulesetStatus::FullyEnforced => Confinement::Full,
            RulesetStatus::PartiallyEnforced => Confinement::Partial,
            RulesetStatus::NotEnforced => Confinement::Unavailable,
        },
        Err(_) => Confinement::Unavailable,
    }
}

/// The same, where there is no Landlock.
#[cfg(not(target_os = "linux"))]
pub fn confine(_sandbox: &Sandbox, _scratch: Option<&Path>) -> Confinement {
    Confinement::Unsupported
}

/// The temporary directory a confined command is given, named after the process it belongs to.
///
/// note: named after the child rather than made with a random name, so that the process which
/// *spawned* that child can find it again and remove it. The child cannot: `/tmp` is not writable
/// under the ruleset and unlinking a directory is a write to the one it sits in, so every command
/// used to leave an empty `kamchatka-<pid>` behind for good. Removing it is therefore the caller's
/// job - see [`crate::tools::Shell`] - and this is the one place that spells the name.
pub fn scratch_for(pid: u32) -> PathBuf {
    std::env::temp_dir().join(format!("kamchatka-{pid}"))
}

/// Makes that directory, and only if this process is the one that made it; `None` if it could not
/// be.
///
/// note: exclusively, and never through whatever happens to be there already. The name has to be
/// predictable - it is how the process that spawned this one finds it again - and a predictable
/// name in a directory anybody can write to is a name somebody else can get to first.
/// `create_dir_all` was happy with anything it found, a symlink included, and the ruleset grants
/// the *resolved* path everything a writable root gets: a link left in `/tmp` by another account
/// would have opened up whatever it pointed at, and `TMPDIR` would have sent the command there.
///
/// note: what is already there and *ours* is a different matter, and much the commoner one, since
/// process identifiers come round again. That is removed and remade, so a command does not inherit
/// the leavings of whatever held the number last. `/tmp` is sticky, so an entry belonging to
/// somebody else cannot be unlinked and the retry fails - which is the answer that leaves the
/// command with no temporary directory rather than with theirs.
///
/// note: `0700`, for the same reason [`crate::app::App::write_session`]'s directory is. What a
/// command puts in here is whatever it was working on, written without anybody asking for it;
/// under the default umask a fresh directory is one everyone on the machine can read.
///
/// note: nothing is said about a failure, because there is nowhere honest to say it. A confined
/// command's standard error goes to the model, and this program's own bookkeeping does not belong
/// in it. What the model gets instead is the truth at the point it matters: no `TMPDIR`, a `/tmp`
/// that is not writable, and [`Sandbox::note_for`] on the permission error that follows.
pub fn make_scratch(path: &Path) -> Option<PathBuf> {
    if std::fs::create_dir(path).is_err() {
        // ours from a run whose identifier has come round, or somebody else's; only the first can
        // be unlinked, and `/tmp` being sticky is what makes that true rather than hopeful
        let _ = std::fs::remove_dir_all(path).or_else(|_| std::fs::remove_file(path));
        std::fs::create_dir(path).ok()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }

    Some(path.to_path_buf())
}

/// Whether a confinement would hold here, asked without running anything.
///
/// note: asked in a child, because finding out means applying a ruleset and a process cannot take
/// one off again. Doing it in the terminal's own process would confine the terminal.
pub fn available(program: &Path) -> Confinement {
    let sandbox = Sandbox {
        workdir: std::env::temp_dir(),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: false,
    };
    // spawned rather than run to completion in one call, because the answer is read out of a
    // child that has left a directory behind it, and the identifier is how it is found again
    let output = std::process::Command::new(program)
        .args(sandbox.argv("exit 0"))
        .env(REPORT_VAR, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|child| {
            let scratch = scratch_for(child.id());
            let output = child.wait_with_output();
            let _ = std::fs::remove_dir_all(&scratch);

            output
        });

    match output {
        Ok(output) => match String::from_utf8_lossy(&output.stderr)
            .lines()
            .find_map(|line| line.strip_prefix(REPORT))
        {
            Some("full") => Confinement::Full,
            Some("partial") => Confinement::Partial,
            Some("unsupported") => Confinement::Unsupported,
            _ => Confinement::Unavailable,
        },
        Err(_) => Confinement::Unavailable,
    }
}

/// The line the confined child writes to say how much of the sandbox took.
const REPORT: &str = "kamchatka-confinement:";

/// Set by [`available`] to ask for that line, and by nothing else.
///
/// note: it is asked for rather than always written, because a confined command's standard error
/// is collected and put in front of the model. A line of this program's own bookkeeping in there
/// is context nobody added on purpose, counted against the budget and read by the model - and
/// this one turned up in a live session doing exactly that.
const REPORT_VAR: &str = "KAMCHATKA_REPORT_CONFINEMENT";

/// Confines this process and runs the command, if this program was asked to; returns the exit
/// code it should leave with.
///
/// note: checked before anything else in `main`, and before a `tokio` runtime exists.
///
/// note: the shell *replaces* this process rather than running underneath it, which is what makes
/// a stopped command stop. Two processes deep, the signal a stopped call sends lands on the middle
/// one and the command carries on - and, still holding the standard error the tool is reading,
/// keeps that call waiting long after somebody asked it to stop. Leaving costs nothing: a Landlock
/// domain is inherited across `execve`, which is the point of an LSM a process applies to itself.
pub fn run_if_asked() -> Option<i32> {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let (sandbox, cmd) = Sandbox::from_argv(&argv)?;

    // a temporary directory of this run's own, made before anything is restricted and handed to
    // the command as `TMPDIR`; see the notes on `confine` and `make_scratch`. Whoever spawned this
    // removes it again, being the only one of the two processes that can
    let scratch = make_scratch(&scratch_for(std::process::id()));

    let confinement = confine(&sandbox, scratch.as_deref());
    if std::env::var_os(REPORT_VAR).is_some() {
        eprintln!(
            "{REPORT}{}",
            match confinement {
                Confinement::Full => "full",
                Confinement::Partial => "partial",
                Confinement::Unavailable => "unavailable",
                Confinement::Unsupported => "unsupported",
            }
        );
    }

    let mut command = std::process::Command::new("sh");
    command.arg("-c").arg(&cmd).current_dir(&sandbox.workdir);
    if let Some(scratch) = &scratch {
        command.env("TMPDIR", scratch);
    }
    // note: only when there is a ruleset in force. Unconfined, git can read its own configuration
    // and pointing it elsewhere would take a person's identity and aliases away for nothing
    if confinement.is_confined()
        && let Some(global) = sandbox.git_config_global()
    {
        command.env("GIT_CONFIG_GLOBAL", global);
    }

    #[cfg(unix)]
    let code = {
        use std::os::unix::process::CommandExt as _;

        // `exec` returns only when it could not happen at all
        let failure = command.exec();
        eprintln!("could not run the command: {failure}");
        127
    };
    // there is no Landlock here anyway, so nothing is lost by staying a parent
    #[cfg(not(unix))]
    let code = match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("could not run the command: {e}");
            127
        }
    };

    Some(code)
}
