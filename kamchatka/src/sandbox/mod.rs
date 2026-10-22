//! A confinement for the `shell` tool, so that the permission stances are enforced rather than
//! reported.
//!
//! note: here a permission stops being only a decision with a record and becomes a boundary. The
//! runtime cannot confine anything: it executes nothing. Confinement belongs in the program that
//! spawns the process.
//!
//! note: the confinement is [sandlock](https://github.com/multikernel/sandlock)'s: a Landlock
//! ruleset, a seccomp filter, and a supervisor in this process that answers the calls the filter
//! holds. No privileges, no setuid helper, no container, no daemon. `network: deny` is refused on
//! the `connect` itself, not by a policy reading `curl`. A model refused `curl` can use
//! `python3 -c "import urllib.request"`; under this, that call gets `Permission denied`.
//!
//! note: the network is the supervisor's. Every `connect` and `send` to an internet address, TCP,
//! UDP and ICMP alike, reaches it with its destination before it happens, so it is refused there
//! or held while the person is asked; see [`crate::tools::Shell`]. A socket in any family but
//! inet, unix and netlink is refused outright, and so is `io_uring`, which opens sockets of its
//! own. A served session's ports are a deny rule the supervisor applies after any allow, so no
//! answer opens them.
//!
//! note: a unix socket is a filesystem matter, not a network one. sandlock's supervisor holds a
//! `connect` to a pathname socket and lets it through only where the command may write, on any
//! kernel: the session bus, the compositor and the container daemon are each a socket under `/run`,
//! which is readable, and each runs a command outside the confinement on the caller's behalf, so a
//! connection to one is a way out. It pins the socket it checked and connects to that, so a link
//! swapped in after the check leads nowhere - which means the connection is made by this process,
//! and the other end sees this process's credentials rather than the command's; see
//! `from_a_command`. A path relative to the command's directory is refused, since the supervisor
//! cannot follow it from here. An abstract socket made outside the command's confinement is refused
//! by Landlock's scope (Linux 6.12).
//!
//! note: signals have a scope of the same age, and it is not in a command's own ruleset: each
//! command is confined in its own domain, so it would refuse a command stopping a server an
//! earlier call left running. The scope is checked per layer, so it goes on the one layer every
//! command shares - this program's own, put on by [`scope_signals`] before it starts anything - and
//! a command may signal this program and whatever it started, and nothing else. This program is
//! inside that boundary; keeping it out would take a separate process to start each command from. A
//! confinement run by hand, with no session above it, has no such layer and has sandlock scope its
//! signals instead; see [`SESSION_FLAG`].
//!
//! note: a command is started by sandlock from this process: it forks, confines the child, and
//! keeps a supervisor here for as long as anything the command started is alive.
//! `--confine-and-run` is a confinement somebody runs by hand, and it is the supervisor of the one
//! command it runs.

use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use nachalnik::{Capability, Verdict};

use crate::tools::{Careful, Subject};

mod confinement;
mod reach;

pub use confinement::{
    Confinement, Probed, Rules, Scratch, available, command, confines_abstract_sockets,
    confines_signals, degraded, fits, make_scratch, run_if_asked, scope_signals, scratch_path,
    weaker_here,
};
pub use reach::{Access, Reach};

/// The argument that puts this program into the mode that confines itself and runs a command.
pub const EXEC_FLAG: &str = "--confine-and-run";

/// The word after [`EXEC_FLAG`] that says the command is a session's, under the layer that scopes
/// its signals - see [`scope_signals`] - so the confinement leaves signals to that layer.
///
/// note: without it the confinement scopes signals itself, because then nothing above does.
/// `--confine-and-run` by hand, around a whole agent, was a domain whose processes could signal
/// everything the person had: a test under it stopping what it took for its own left-over jobs
/// stopped the desktop. A forged word buys nothing: inside a session the session's layer still
/// binds, and outside one it takes the scope from nobody but the caller.
pub const SESSION_FLAG: &str = "--in-a-session";

/// What the `shell` tool may reach.
///
/// note: a command that cannot read `/usr/bin` cannot run, so the shell reads the paths of
/// `shell-read` - [`SYSTEM`] unless somebody says otherwise - beside everything the tools reach.
/// The stances a person changes are what is writable and whether the network is reachable.
///
/// note: "the network" is every connection and datagram to an internet address. See [`Network`],
/// and the note at the top of this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    /// The directory a command may work in; everything outside it is out of reach.
    pub workdir: PathBuf,
    /// Paths outside the working directory the user asked for, read-write.
    pub writable: Vec<PathBuf>,
    /// Paths outside the working directory the user asked for, readable only.
    ///
    /// note: separate from `writable` because they are asked for by different flags for different
    /// reasons, and a command may hold both at once. What sends most people here is a toolchain:
    /// `cargo` cannot start without `~/.rustup`, and a model that can replace the toolchain it is
    /// about to run is a worse trade than anybody meant to make.
    pub readable: Vec<PathBuf>,
    /// Paths the command may read and `fs` may not; see [`SYSTEM`] for the ones it gets unless
    /// somebody says otherwise.
    ///
    /// note: the shell's alone because `/proc/self` is a different file to each tool: to a command
    /// it is the command, and to `fs`, which runs in this process, it is this process, with the
    /// keys in its environment.
    pub shell_readable: Vec<PathBuf>,
    /// Whether the working directory is writable, or only readable.
    pub workdir_writable: bool,
    /// What the command may do about the network.
    pub network: Network,
    /// The devices under `/dev` the command may read and write, and nothing else there; see
    /// [`DEVICES`] for the ones it gets unless somebody says otherwise.
    pub devices: Vec<PathBuf>,
    /// Ports on this machine the command may not connect to, whatever `network` says: the ones this
    /// process serves a session on.
    ///
    /// note: a served session takes a client's answer to a permission question, so a command that
    /// could reach it could answer its own. They are refused on every address, since a session
    /// served on all of them is reachable through each, and the refusal outranks any answer.
    pub closed: Vec<u16>,
}

/// What a confined command may do about the network.
///
/// note: three rather than open and closed, because the supervisor can hold a call as well as
/// refuse it. It sees every `connect` and `send` to an internet address with its destination,
/// which is the first thing any use of the network does, a DNS lookup included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Network {
    /// Nothing refuses it.
    Open,
    /// Every connection and datagram to an internet address is refused.
    Shut,
    /// Each is held until the person is asked, once per command.
    Asked,
}

impl Network {
    /// What a command gets: `open` where the network was allowed, `denied` where the stance
    /// refuses it, and otherwise a question.
    fn of(open: bool, denied: bool) -> Self {
        match (open, denied) {
            (true, _) => Self::Open,
            (false, true) => Self::Shut,
            (false, false) => Self::Asked,
        }
    }

    /// The word the arguments carry it as.
    fn word(self) -> &'static str {
        match self {
            Self::Open => "net",
            Self::Shut => "shut",
            Self::Asked => "held",
        }
    }

    fn from_word(word: &std::ffi::OsStr) -> Option<Self> {
        [Self::Open, Self::Shut, Self::Asked]
            .into_iter()
            .find(|network| *word == *network.word())
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "the network reachable",
            Self::Shut => "no network",
            Self::Asked => "the network only once the person you are working with allows it",
        })
    }
}

impl Sandbox {
    /// The confinement a command should run under, given what the policy currently answers.
    ///
    /// note: the network is reachable only if the stance is `allow`, or if this call was allowed by
    /// a person who was asked about it. A stance of `ask` that nobody has been asked about yet is
    /// not permission. The command still runs and is asked about if it reaches out, so the stance
    /// is not a refusal either.
    ///
    /// note: except where nobody is there to ask and the run refuses every question, which is
    /// `--on-ask deny`. The network is then shut, as [`Careful`] refuses the kernel's questions
    /// there rather than asking them. Asked, the refusal reached the model as an answer to one
    /// command, and a model told to say why it needs the network and try again ran the same
    /// command into the same refusal for the rest of the run.
    pub fn of(
        policy: &Careful,
        workdir: PathBuf,
        writable: Vec<PathBuf>,
        readable: Vec<PathBuf>,
        granted: bool,
    ) -> Self {
        let stance = policy.stance(&Subject::Capability(Capability::net("reach")));
        let workdir_writable = writes(policy);
        // note: it reaches the paths `--sandbox-write` opened, which stay readable. `fs` is refused
        // a write there under the same refusal. A shell that could still write in them was two
        // tools disagreeing about one path; the one that could write was the one whose writes
        // nothing checks
        let (writable, readable) = match workdir_writable {
            true => (writable, readable),
            false => (Vec::new(), writable.into_iter().chain(readable).collect()),
        };

        Self {
            workdir,
            writable,
            readable,
            workdir_writable,
            network: Network::of(
                granted || stance == Verdict::Allow,
                !policy.permits(&Capability::net("reach")),
            ),
            shell_readable: SYSTEM.iter().map(PathBuf::from).collect(),
            devices: DEVICES.iter().map(PathBuf::from).collect(),
            closed: SERVED
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }

    /// The arguments that ask this program to confine `cmd` this way and run it, as one of a
    /// session's commands; see [`SESSION_FLAG`].
    pub fn argv(&self, cmd: &str) -> Vec<OsString> {
        let mut argv = vec![
            OsString::from(EXEC_FLAG),
            OsString::from(SESSION_FLAG),
            self.workdir.clone().into(),
            OsString::from(match self.workdir_writable {
                true => "rw",
                false => "ro",
            }),
            OsString::from(self.network.word()),
            OsString::from(self.writable.len().to_string()),
        ];
        argv.extend(self.writable.iter().map(|path| path.clone().into()));
        argv.push(OsString::from(self.readable.len().to_string()));
        argv.extend(self.readable.iter().map(|path| path.clone().into()));
        argv.push(OsString::from(self.shell_readable.len().to_string()));
        argv.extend(self.shell_readable.iter().map(|path| path.clone().into()));
        argv.push(OsString::from(self.devices.len().to_string()));
        argv.extend(self.devices.iter().map(|path| path.clone().into()));
        argv.push(OsString::from(self.closed.len().to_string()));
        argv.extend(
            self.closed
                .iter()
                .map(|port| OsString::from(port.to_string())),
        );
        argv.push(cmd.into());

        argv
    }

    /// Reads back what [`Sandbox::argv`] wrote, plus the command; `None` if this is not one.
    ///
    /// note: [`SESSION_FLAG`] is read past rather than required, so that the same arguments
    /// without it - a confinement somebody runs by hand - read back as the same sandbox.
    pub fn from_argv(argv: &[OsString]) -> Option<(Self, OsString)> {
        let mut argv = argv.iter().peekable();
        if argv.next()? != EXEC_FLAG {
            return None;
        }
        argv.next_if(|word| *word == SESSION_FLAG);
        let workdir = PathBuf::from(argv.next()?);
        let workdir_writable = argv.next()? == "rw";
        let network = Network::from_word(argv.next()?)?;
        let count: usize = argv.next()?.to_str()?.parse().ok()?;
        let writable: Vec<PathBuf> = argv.by_ref().take(count).map(PathBuf::from).collect();
        let count: usize = argv.next()?.to_str()?.parse().ok()?;
        let readable: Vec<PathBuf> = argv.by_ref().take(count).map(PathBuf::from).collect();
        let count: usize = argv.next()?.to_str()?.parse().ok()?;
        let shell_readable: Vec<PathBuf> = argv.by_ref().take(count).map(PathBuf::from).collect();
        let count: usize = argv.next()?.to_str()?.parse().ok()?;
        let devices: Vec<PathBuf> = argv.by_ref().take(count).map(PathBuf::from).collect();
        let count: usize = argv.next()?.to_str()?.parse().ok()?;
        let closed = argv
            .by_ref()
            .take(count)
            .map(|port| port.to_str()?.parse().ok())
            .collect::<Option<Vec<u16>>>()?;
        let cmd = argv.next()?.clone();

        Some((
            Self {
                workdir,
                writable,
                readable,
                shell_readable,
                workdir_writable,
                network,
                devices,
                closed,
            },
            cmd,
        ))
    }

    /// Whether a confined command could open this path for reading.
    ///
    /// note: reading rather than writing, because every use of this is about a command that was
    /// refused one. A path under a read-only root answers `true` here and is still unwritable.
    ///
    /// note: both sides are resolved, so a symlink and a `../` are the same question as for
    /// [`Reach::allows`]. A path that is not there resolves through its parent, which is the common
    /// case: a command that named a file it could not open often could not `stat` its directory
    /// either.
    fn reaches(&self, path: &Path, scratch: Option<&Path>) -> bool {
        let Some(resolved) = resolve(path) else {
            return false;
        };
        // note: a `..` left over is a climb `resolve` could not settle - past a directory that is
        // not there, `/work/missing/../../outside` keeps its `..`s behind the working directory's
        // prefix, which a comparison by prefix passes. `Reach::allows` refuses the same path, and
        // a path refused there is not one to call reached here
        if resolved
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            return false;
        }
        // note: a named device was reached, so refusing it is its own permissions. Anything else
        // under `/dev` is the boundary unless a root the person named covers it: `--sandbox-write`
        // and `--sandbox-read` are granted under `/dev` as anywhere else, so answering `false` for
        // all of `/dev` blamed the confinement for a file's own permissions
        if self.names_device(&resolved) {
            return true;
        }

        self.shell_readable
            .iter()
            .cloned()
            .chain(std::iter::once(self.workdir.clone()))
            .chain(scratch.map(Path::to_path_buf))
            .chain(self.writable.iter().cloned())
            .chain(self.readable.iter().cloned())
            .any(|allowed| match allowed.canonicalize() {
                Ok(allowed) => resolved.starts_with(allowed),
                Err(_) => false,
            })
    }

    /// What to add to a confined command's output when a permission error in it was this
    /// confinement rather than the file's own permissions; `None` when nothing suggests it was.
    ///
    /// note: Landlock refuses an `open` with `EACCES`, which is what a command reports when a file
    /// is somebody else's, so a model handed `Permission denied (os error 13)` cannot tell a
    /// boundary from a protected file. It goes hunting for a `cargo` that was never missing, when
    /// what is out of reach is `~/.rustup`. The tool's description says a permission error out here
    /// is the confinement, but that is not enough on its own: this puts the sentence at the point
    /// of failure, naming the path.
    ///
    /// note: three answers rather than two. A refusal that names a path *within* reach was the
    /// file's own permissions - `/etc/shadow` is refused under this and would be refused without
    /// it - and saying "this may have been the sandbox" there would send a model looking for a
    /// boundary that had nothing to do with it. So a refusal whose paths are all reachable gets
    /// nothing said about it.
    ///
    /// note: the general sentence is for the refusal that names no path this can find. Standard
    /// error is arbitrary text and picking paths out of it is a guess; what is *not* a guess is
    /// that this command ran confined and something was refused, which is worth one line.
    ///
    /// note: it says "below" because [`Shell`](crate::tools::Shell) puts it under the status line
    /// and above the output. Beside the message would read better, but an output limit cuts from
    /// the end, and a note accounting for a permission error is worth nothing if the truncation
    /// takes the note and leaves the error.
    pub fn note_for(&self, stderr: &str) -> Option<String> {
        self.note_for_in(stderr, None)
    }

    /// [`Sandbox::note_for`] for a command given `scratch` as `TMPDIR`.
    ///
    /// note: [`Sandbox::sandlock`] grants that directory reading and writing even where the working directory
    /// is read-only, so a refusal in it is the file's own permissions. Blaming the confinement is
    /// the hedge `note_for` exists not to make. The caller names it, because only whoever spawned
    /// the command knows which directory it was given, and it must still be there to compare.
    pub(crate) fn note_for_in(&self, stderr: &str, scratch: Option<&Path>) -> Option<String> {
        let refusals: Vec<&str> = stderr.lines().filter(|line| refused(line)).collect();
        if refusals.is_empty() {
            return None;
        }

        // `Vec::dedup` drops only neighbours, but a path is usually named by two non-adjacent
        // lines: a warning and the failure it led to.
        //
        // note: it stops after naming three, or after judging `LOOKED` paths. A command's standard
        // error can be all of what `KEPT` holds; each path costs a dozen `canonicalize` calls, and
        // `find /` names thousands this reaches, none counting towards the three. That is seconds
        // of work after the command ended, with nothing to interrupt it. Stopped short with nothing
        // named, it says the general sentence, since saying nothing would call every refusal after
        // the last one judged the file's own
        //
        // note: a line naming no path is judged on its own. A path elsewhere in the output that was
        // refused by its own permissions says nothing about it, so it still gets the general
        // sentence: counted for the whole output, one `/etc/shadow` silenced a refusal beside it
        // that may have been the boundary. Only asked where nothing was named, the one case its
        // answer decides, since it reads every line
        let pathless = || refusals.iter().any(|line| !names_a_path(line));
        let (mut named, mut mentioned): (Vec<(String, Why)>, bool) = (Vec::new(), false);
        let (mut judged, mut stopped) = (std::collections::HashSet::new(), false);
        for path in refusals.iter().flat_map(|line| paths_in(line)) {
            mentioned = true;
            if !judged.insert(path.clone()) {
                continue;
            }
            if judged.len() > LOOKED {
                stopped = true;
                break;
            }
            // note: a relative path is judged where the command started and named as written. A
            // command that moved with `cd` first is judged from the wrong place, which is the rarer
            // mistake than reading every relative path as the boundary's
            let judged = self.workdir.join(&path);
            // a socket is judged by connecting and nothing else: see `connecting`
            let why = match self.connecting(&judged, scratch) {
                Some(refused) => refused.then_some(Why::Socket),
                None if !self.reaches(&judged, scratch) => Some(Why::Outside),
                None => self.reads_only(&judged, scratch).then_some(Why::ReadOnly),
            };
            if let Some(why) = why {
                named.push((path, why));
                if named.len() == 3 {
                    break;
                }
            }
        }

        let of = |why: Why| -> Vec<String> {
            named
                .iter()
                .filter(|(_, named)| *named == why)
                .map(|(path, _)| path.clone())
                .collect()
        };
        let (sockets, read_only, outside) = (of(Why::Socket), of(Why::ReadOnly), of(Why::Outside));
        match (
            named.is_empty(),
            named.is_empty() && (!mentioned || stopped || pathless()),
        ) {
            // every path it named is one this reaches, so the refusal is the file's own
            (true, false) => None,
            (true, true) => Some(format!(
                "[this command ran confined - {self} - so a permission error below may be that \
                 boundary rather than the file's own permissions.]"
            )),
            // a socket whose path the command can read but not connect to; "outside what this
            // session reaches" would be false about it
            (false, _) if sockets.len() == named.len() => Some(format!(
                "[{} {} outside what this session may write, and a confined command may \
                 connect only to a socket it could have written, so the permission error below \
                 is the confinement rather than the socket's own permissions. This command runs \
                 with {self}. Say what you need the socket for and ask for it to be opened up.]",
                sockets.join(", "),
                match sockets.len() {
                    1 => "is a socket",
                    _ => "are sockets",
                },
            )),
            // reached, and refused a write the person could have made: under `--auto-deny fs:write`
            // the working directory itself, or a path opened up with `--sandbox-read`. Only a
            // write, since such a path is held for reading and running as well - so a script
            // without its execute bit is refused by its own mode there, and the error does not
            // say which of the two it was
            (false, _) if read_only.len() == named.len() => Some(format!(
                "[{} {} where this session may read and not write, so if the permission error \
                 below is a write, it is the confinement rather than the file's own \
                 permissions. This command runs with {self}. Say what you need to write there \
                 and ask for it to be opened up.]",
                read_only.join(", "),
                are(&read_only),
            )),
            (false, _) if outside.len() == named.len() => Some(format!(
                "[{} {} outside what this session reaches, so the permission error below is \
                 the confinement rather than the file's own permissions. This command runs with \
                 {self}. Work inside the working directory, or say what you need the path for \
                 and ask for it to be opened up.]",
                outside.join(", "),
                are(&outside),
            )),
            // note: each path is refused for its own reason and said with it. One sentence for all
            // of them calls a readable path "outside what this session reaches", and a model takes
            // that for the whole rule and never reads it
            (false, _) => {
                let said = [
                    (outside, "is", "are", "outside what this session reaches"),
                    (
                        read_only,
                        "is",
                        "are",
                        "where this session may read and not write",
                    ),
                    (
                        sockets,
                        "is a socket",
                        "are sockets",
                        "a confined command may not connect to",
                    ),
                ]
                .into_iter()
                .filter(|(paths, ..)| !paths.is_empty())
                .map(|(paths, one, more, why)| {
                    let is = if paths.len() == 1 { one } else { more };
                    format!("{} {is} {why}", paths.join(", "))
                })
                .collect::<Vec<_>>()
                .join("; ");
                Some(format!(
                    "[{said}. So the permission errors below are the confinement rather than the \
                     files' own permissions. This command runs with {self}. Say what you need \
                     these paths for and ask for them to be opened up.]"
                ))
            }
        }
    }

    /// Whether `path` is somewhere a confined command may read but not write, and the person could
    /// have written to it. A refusal there is the confinement's, not the file's own permissions.
    ///
    /// note: a refusal at a reachable path is otherwise blamed on the file's own permissions, which
    /// is right for `cat /etc/shadow` but wrong for writing to a directory the session holds
    /// read-only - under `--auto-deny fs:write`, the working directory itself. The check uses
    /// `access(2)` from this unconfined process, on the path or the nearest part of it that exists,
    /// because a refused write is often a file being created.
    fn reads_only(&self, path: &Path, scratch: Option<&Path>) -> bool {
        let Some(resolved) = resolve(path) else {
            return false;
        };
        // a device that was reached was granted writing as well; see `reaches`
        if self.names_device(&resolved) || self.writes(&resolved, scratch) {
            return false;
        }
        resolved
            .ancestors()
            .find(|part| part.symlink_metadata().is_ok())
            .is_some_and(|there| rustix::fs::access(there, rustix::fs::Access::WRITE_OK).is_ok())
    }

    /// Whether a resolved path is one of the devices named, or beneath one.
    fn names_device(&self, resolved: &Path) -> bool {
        self.devices
            .iter()
            .filter_map(|named| device(named))
            .any(|device| resolved.starts_with(device))
    }

    /// Whether a resolved path is under one of the roots a confined command may write.
    fn writes(&self, resolved: &Path, scratch: Option<&Path>) -> bool {
        self.workdir_writable
            .then_some(self.workdir.as_path())
            .into_iter()
            .chain(scratch)
            .chain(self.writable.iter().map(PathBuf::as_path))
            .any(|root| {
                root.canonicalize()
                    .is_ok_and(|root| resolved.starts_with(root))
            })
    }

    /// Whether a confined command may not connect to `path`, if it is a socket; `None` if it is not
    /// one.
    ///
    /// note: the other half of [`Sandbox::reaches`], which asks about reading. A socket is held to
    /// the writing half of the ruleset, so a session can read the path of a socket under `/run` and
    /// be refused the connection. It is judged by the file's type, not by the error's words, which
    /// a client chooses. sandlock's supervisor refuses such a connection with `EACCES`, which
    /// is the error that brings a refusal here.
    ///
    /// note: a socket is judged by this alone, not by [`Sandbox::reaches`] as well: whether the
    /// session may read the path says nothing about whether it may connect.
    fn connecting(&self, path: &Path, scratch: Option<&Path>) -> Option<bool> {
        use std::os::unix::fs::FileTypeExt as _;

        let resolved = resolve(path)?;
        if !resolved
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_socket())
        {
            return None;
        }
        Some(!self.writes(&resolved, scratch))
    }

    /// What to hand a confined command as `GIT_CONFIG_GLOBAL`; `None` leaves git its own defaults.
    ///
    /// note: git is the one program where being out of reach is fatal rather than inconvenient,
    /// because under Landlock `access(2)` still answers from the file's own permissions. So git
    /// asks whether `~/.gitconfig` is readable, is told yes, opens it, gets `EACCES`, and takes the
    /// *unreadable configuration* branch rather than the *no configuration* branch -
    /// `fatal: unknown error occurred while reading the configuration files`, and every git command
    /// in the session is dead. A missing file is fine; an unreadable one is not, and a confined
    /// command cannot tell git which it has.
    ///
    /// note: only when a configuration a confined command could not read is what git would open
    /// otherwise. Git reads a person's aliases and identity out of these, and silently throwing
    /// them away for a command that could have had them would be its own bug. A path set in
    /// `GIT_CONFIG_GLOBAL` is a choice somebody made, and is left alone where it is readable; left
    /// alone out of reach, it would kill git like any other configuration, and the *no
    /// configuration* case is the one git handles.
    pub(crate) fn git_config_global(&self) -> Option<PathBuf> {
        let (mine, missed): (Vec<PathBuf>, Vec<PathBuf>) = std::env::var_os("GIT_CONFIG_GLOBAL")
            .map(PathBuf::from)
            .into_iter()
            .chain(global_git_config())
            .filter(|path| path.is_file())
            .partition(|path| self.reaches(path, None));

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

/// Whether the policy leaves the reach writable. A `Deny` for `write` reaches the shell too; any
/// other verdict leaves the working directory writable, because a shell that cannot write there is
/// unusable.
///
/// note: one function serves [`Sandbox::of`] and [`Reach`]'s refusals: they describe the same
/// session and must not produce two answers about it.
fn writes(policy: &Careful) -> bool {
    policy.stance(&Subject::Capability(Capability::fs("write"))) != Verdict::Deny
}

/// The deepest existing part of a path, resolved, with whatever is left over joined back on.
///
/// note: a file about to be created has no canonical form, and a path handed to `write` is usually
/// such a file. Resolve it through its parent instead, because a file cannot be created outside a
/// directory it is not in.
///
/// note: one function rather than two, so [`Reach::allows`] and [`Sandbox::reaches`] cannot give
/// different answers about the same path.
///
/// note: a symlink to a missing target has no canonical form either, but it is not a file about to
/// be created: writing through it creates the target, wherever that is. Follow it as the open will,
/// rather than step past it as a name with nothing behind it; stepping past would let `write`
/// create a file outside the directory through a link made inside it. `LINKS` bounds the following,
/// because two links can point at each other.
///
/// note: return `None` when that bound runs out with a link still unresolved and no link seen
/// twice. Treating it as a name with nothing behind it would allow the last link as a file about to
/// be made in its directory, and the open would then follow the rest of the chain wherever it went,
/// out of the directory, on a kernel with no `openat2` to stop it there. A loop is different: every
/// link in it has been seen, it leads nowhere, and the open refuses it, so answer it where it is,
/// inside or outside, like any other name.
pub(crate) fn resolve(path: &Path) -> Option<PathBuf> {
    const LINKS: usize = 40;

    let mut existing = path.to_path_buf();
    let mut rest = PathBuf::new();
    let mut followed = 0;
    let mut seen = std::collections::HashSet::new();
    let joined = |base: PathBuf, rest: &Path| match rest.as_os_str().is_empty() {
        // note: joined only when there is something to join. `Path::join("")` appends a separator,
        // and `/w/local.txt/` is a directory that is not there, so `./local.txt` would come back
        // `Not a directory`
        true => base,
        false => base.join(rest),
    };
    loop {
        match existing.canonicalize() {
            Ok(resolved) => break Some(joined(resolved, &rest)),
            Err(_)
                if existing
                    .symlink_metadata()
                    .is_ok_and(|meta| meta.file_type().is_symlink()) =>
            {
                if !seen.insert(existing.clone()) {
                    break Some(joined(existing, &rest));
                }
                if followed == LINKS {
                    break None;
                }
                let Ok(target) = existing.read_link() else {
                    break Some(joined(existing, &rest));
                };
                followed += 1;
                // `join` handles both: a relative target is relative to the link's directory, and
                // an absolute target replaces the whole path
                existing = match existing.parent() {
                    Some(parent) => parent.join(target),
                    None => target,
                };
            }
            Err(_) => match (existing.file_name(), existing.parent()) {
                (Some(name), Some(parent)) => {
                    // note: the same guard here, for the same reason. Without it every path that
                    // does not exist yet would come back with a trailing separator, so `write`
                    // could create no file at all: `notes.txt/` is a directory, and the tool
                    // reports `Is a directory (os error 21)` for a file it was just asked to create
                    rest = match rest.as_os_str().is_empty() {
                        true => PathBuf::from(name),
                        false => Path::new(name).join(&rest),
                    };
                    existing = parent.to_path_buf();
                }
                // the resolved path, not the path handed in. After a link is followed they differ,
                // and only the resolved path is where the open goes
                _ => break Some(joined(existing, &rest)),
            },
        }
    }
}

/// The device a path given to `--shell-devices` names, resolved; `None` where it lands anywhere
/// but beneath `/dev`, or is `/dev` itself.
///
/// note: resolved rather than read off the spelling, because `Path::starts_with` compares
/// components and never settles one: `/dev/../home` begins with `/dev`, and the ruleset opens it as
/// `/home`. A link is followed for the same reason - `/dev/fd` leads into `/proc`, and `/dev/shm`
/// is anybody's to make one in. A device that is not there yet resolves through its parent, so
/// one plugged in later is still grantable. `/dev` itself is every device at once, which is what
/// [`DEVICES`] exists not to grant.
///
/// note: asked twice, by `Setup::check` where the list is given and by [`Sandbox::sandlock`] where it is
/// granted. The first is where a person is told; the second is what holds for a [`Sandbox`] built
/// by hand, and grants the path it resolved rather than the one it was handed.
pub fn device(path: &Path) -> Option<PathBuf> {
    resolve(path).filter(|resolved| {
        resolved.starts_with("/dev")
            && resolved != Path::new("/dev")
            && !resolved
                .components()
                .any(|component| component == std::path::Component::ParentDir)
    })
}

/// Whether a line of standard error looks like a permission refusal.
///
/// note: there are three spellings because three layers write them: a C program's `strerror`,
/// Rust's `io::Error` display, and the errno name itself. All three mean the same refusal.
///
/// note: the lowercase spelling is included because it is Go's. `docker` says `connect: permission
/// denied`, and a refused socket is the refusal a Go client most often reports.
fn refused(line: &str) -> bool {
    line.contains("Permission denied")
        || line.contains("permission denied")
        || line.contains("os error 13")
        || line.contains("EACCES")
}

/// How many of the paths a command's refusals name are judged, at most.
const LOOKED: usize = 1000;

/// Why a path a refusal named was the confinement's doing.
#[derive(Clone, Copy, PartialEq)]
enum Why {
    /// A socket a confined command may not connect to.
    Socket,
    /// A path outside the reach.
    Outside,
    /// A path the reach holds for reading and not writing.
    ReadOnly,
}

/// The verb for the paths a note names. A command that writes one file and reads another is refused
/// twice, and the note names both.
fn are(named: &[String]) -> &'static str {
    match named.len() {
        1 => "is",
        _ => "are",
    }
}

/// The paths a line of standard error mentions: absolute paths, and relative paths that contain
/// `/`, as written.
///
/// note: a token counts as a path after the punctuation around it is stripped. Strip before
/// testing: in `could not read settings file: '/home/you/.rustup/settings.toml': Permission
/// denied`, the token is `'/home/...':`, which does not start with a slash.
///
/// note: strip opening quotes and brackets from the front, and closing quotes, brackets, and
/// sentence punctuation from the back. Never strip a `.` from the front: that turned a relative
/// `./x` into an absolute `/x` outside the reach, and the note then blamed the confinement for the
/// file's own permissions. Strip a trailing `.` only after a name, so `../..` keeps its meaning.
///
/// note: a relative path must contain `/` to count, because every refusal contains words, and a
/// word mistaken for a path in the working directory would be treated as a path reached. That would
/// silence the general sentence owed when a refusal names no path. A URL is not a path.
///
/// note: it misses a path with a space in it, which is the right way round: a caller that gets
/// nothing says something general, and a caller that gets a wrong path would say something false.
fn paths_in(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(bare)
        .filter(|token| is_path(token))
        .map(str::to_owned)
        .collect()
}

/// Whether a line names any path, as [`paths_in`] reads one.
fn names_a_path(line: &str) -> bool {
    line.split_whitespace().map(bare).any(is_path)
}

/// Whether a token, its punctuation taken off, is a path.
fn is_path(token: &str) -> bool {
    (token.starts_with('/') && token.len() > 1) || (token.contains('/') && !token.contains("://"))
}

/// A token with the punctuation a message wraps a path in taken off; see [`paths_in`].
fn bare(token: &str) -> &str {
    let mut token = token.trim_start_matches(|c: char| "'\"`([<".contains(c));
    loop {
        let trimmed = token.trim_end_matches(|c: char| "'\"`,;:)]>".contains(c));
        let trimmed = match trimmed.strip_suffix('.') {
            Some(rest) if !rest.is_empty() && !rest.ends_with(['.', '/']) => rest,
            _ => trimmed,
        };
        if trimmed.len() == token.len() {
            break token;
        }
        token = trimmed;
    }
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
            match self.workdir_writable {
                true => "read-write",
                false => "read-only",
            }
        )?;
        for path in &self.writable {
            write!(f, ", {} read-write", path.display())?;
        }
        for path in &self.readable {
            write!(f, ", {} read-only", path.display())?;
        }
        // its own temporary directory, writable even where the working directory is not
        write!(f, ", its own `$TMPDIR` read-write")?;
        for path in &self.shell_readable {
            write!(f, ", {} read-only", path.display())?;
        }
        write!(f, ", {}", self.network)?;
        // note: said wherever the network is not refused outright, since a closed port is the one
        // connection refused there. A command refused it was told only that the network was
        // reachable, with nothing to say why this address was not
        if self.network != Network::Shut && !self.closed.is_empty() {
            let ports: Vec<String> = self.closed.iter().map(u16::to_string).collect();
            write!(
                f,
                " but for {} {} on this machine, where this session is served",
                match ports.len() {
                    1 => "port",
                    _ => "ports",
                },
                ports.join(", "),
            )?;
        }
        Ok(())
    }
}
/// The directories a command has to be able to read before it can be a command at all, which the
/// shell reads unless somebody says otherwise. `--shell-read` and the settings file's `shell-read`
/// replace the list.
///
/// note: not `/run`, but for systemd-resolved's directory in it. What is in there is the person's
/// running session: the sockets of the session bus, the compositor and the container daemon, which
/// run a command outside the sandbox on its behalf; a USB drive under `/run/media`; who is logged
/// in.
/// `/run/systemd/resolve` holds the resolver's configuration and its own sockets and nothing else,
/// and Ubuntu, Fedora and Debian make `/etc/resolv.conf` a link into it: without it, no command
/// there could look up a host. A system without it is granted nothing by the rule, as one without
/// `/lib64` is. Any other `/etc/resolv.conf` that leads outside the list is said at startup, and
/// NixOS needs `/run/current-system` for everything on `PATH`.
pub const SYSTEM: &[&str] = &[
    "/usr",
    "/etc",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/opt",
    "/proc",
    "/sys",
    "/run/systemd/resolve",
];

/// The devices a confined command may read and write unless somebody says otherwise, by name;
/// nothing else under `/dev`. `--shell-devices` and the settings file's `shell-devices` replace
/// the list, which is how a pty is had back - `/dev/ptmx` and `/dev/pts` - at the price below.
///
/// note: by name rather than `/dev` beneath, because beneath reaches more than a command needs.
/// The person's other terminals under `/dev/pts` are theirs, so a command could read one and take
/// what is typed there - a password given to `sudo` included; the shared memory in `/dev/shm` of
/// any process of theirs can be written into; and on a desktop, logind hands the seated user the
/// camera, the microphone and the GPU. `/dev/stdin` and `/dev/fd` are links into `/proc`, which is
/// readable, and on to the command's own descriptors, so they still work.
///
/// note: what it costs is a pty. Opening the peer of a `/dev/ptmx` is opening a file in
/// `/dev/pts` - `TIOCGPTPEER` too - so granting one is granting the other terminals, and `script`
/// or `expect` fails. A confined command has no terminal to want one for.
pub const DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
];

/// The ports this process serves a session on, which [`Sandbox::of`] closes to every command.
///
/// note: the process's rather than a session's, because a command that reached any session served
/// from here could answer that session's questions, its own among them.
static SERVED: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// The sandboxes this process's confined commands run in, by a handle that names their process
/// groups.
///
/// note: every sandbox still holding a process, not just the ones whose call is still going. A
/// command's processes outlive the command in whatever it left running. One that has nothing
/// left is dropped when the next command starts or a client arrives.
static SESSIONS: Mutex<Vec<sandlock_core::Controller>> = Mutex::new(Vec::new());

/// Closes `port` to every command confined from here on; see [`Sandbox::closed`].
pub(crate) fn serving_on(port: u16) {
    SERVED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(port);
}

/// Opens `port` again, once nothing is served on it.
pub(crate) fn stopped_serving_on(port: u16) {
    SERVED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|served| *served != port);
}

/// Remembers the sandbox a confined command runs in; see [`from_a_command`].
///
/// note: and lets go of the ones with nothing left in them. Each holds a descriptor for every
/// process group its command made, and a session served nowhere never asks [`from_a_command`], so
/// letting go only there kept two descriptors a command until the process ran out of them.
pub(crate) fn began(sandbox: sandlock_core::Controller) {
    let mut sessions = SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
    live(&mut sessions);
    sessions.push(sandbox);
}

/// The process groups of every sandbox in `sessions` with something left in it; the others are
/// let go of.
fn live(sessions: &mut Vec<sandlock_core::Controller>) -> Vec<i32> {
    let mut live = Vec::new();
    sessions.retain(|sandbox| match sandbox.process_groups() {
        Ok(groups) if !groups.is_empty() => {
            live.extend(groups);
            true
        }
        _ => false,
    });

    live
}

/// Whether the process `pid` is one of a command confined from here, or cannot be distinguished
/// from one.
///
/// note: by process group. sandlock sees every group a confined process makes or joins, since it
/// signals a sandbox by its groups, so `setsid` and a daemon's double fork leave a process in a
/// group it knows. A group with a process in it cannot be handed to anybody else.
///
/// note: `SO_PEERCRED` gives a served socket this pid. A process that has since exited has no
/// group left to ask about, and refusing it loses nothing because there is nobody there to serve.
///
/// note: this process is a command too, while any command is running. sandlock lets a command reach
/// a unix socket by connecting to it on the command's behalf, from its supervisor here, so the pid
/// the other end is given is this one - and it does so only for a process alive in a sandbox. A
/// client in this process reaches a session served here without a socket - see
/// `remote::server::door` - so the socket is not how anything else comes from here while a command
/// could be the one.
pub(crate) fn from_a_command(pid: u32) -> bool {
    let live = live(&mut SESSIONS.lock().unwrap_or_else(PoisonError::into_inner));
    if pid == std::process::id() {
        return !live.is_empty();
    }
    let group = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .and_then(|pid| rustix::process::getpgid(Some(pid)).ok());

    match group {
        Some(group) => live.contains(&group.as_raw_nonzero().get()),
        None => true,
    }
}

/// The environment of this process as text, which is all sandlock hands a command; a variable
/// whose name or value is not text is left out.
pub(crate) fn environment() -> impl Iterator<Item = (String, String)> {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
}

#[cfg(test)]
mod tests {
    use super::Network;

    /// The network is open where it was allowed, shut where it is refused, and asked about
    /// otherwise.
    #[test]
    fn a_network_is_open_shut_or_asked_about() {
        assert_eq!(Network::of(true, false), Network::Open);
        assert_eq!(Network::of(true, true), Network::Open);
        assert_eq!(Network::of(false, true), Network::Shut);
        assert_eq!(Network::of(false, false), Network::Asked);
    }
}
