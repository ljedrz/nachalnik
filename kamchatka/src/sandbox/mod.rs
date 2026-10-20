//! A confinement for the `shell` tool, so that the permission stances are enforced rather than
//! reported.
//!
//! note: here a permission stops being only a decision with a record and becomes a boundary. The
//! runtime cannot confine anything: it executes nothing. Confinement belongs in the program that
//! spawns the process.
//!
//! note: [Landlock](https://landlock.io) is a Linux LSM a process applies to itself: no privileges,
//! no setuid helper, no container, no daemon. `network: deny` is refused by the kernel on the
//! `connect` syscall, not by a policy reading `curl`. A model refused `curl` can use
//! `python3 -c "import urllib.request"`; under this, that call gets `Permission denied` from the
//! kernel.
//!
//! note: the `landlock` crate exposes only `ConnectTcp` and `BindTcp` as network rights, so
//! Landlock confines only TCP here. The kernel grew UDP rights in ABI 10 (Linux 7.2), but the crate
//! stops at ABI 9 and `AccessNet` is `#[non_exhaustive]` over a sealed trait, so UDP rights cannot
//! be handed to a ruleset from this crate. The gate handles the rest of the network:
//! [`crate::gate`] stops a confined command's `socket()` in every family but unix and netlink,
//! covering UDP and everything else a socket can leave the machine by, and either refuses it or
//! holds it while the person is asked. Where the gate cannot be installed, a UDP datagram still
//! goes out, enough to put bytes in a DNS query; [`Network::NoTcp`] is how that case is written
//! everywhere it is shown rather than rounded up. What still holds against the rest is the
//! filesystem: a command that cannot read a file has nothing to send.
//!
//! note: a unix socket is a filesystem matter, not a network one. Linux 7.1 grew the right for it,
//! `AccessFs::ResolveUnix`, which the crate exposes. A command may connect to a unix socket it
//! could have written to, and to no other. Below that kernel a `connect` is ungoverned, leaving the
//! session bus, the compositor and the container daemon reachable; each runs a command outside the
//! domain on the caller's behalf, so the hole is worse than the UDP one: a datagram carries bytes
//! out, while `systemd-run --user` carries a command out and hands back the whole filesystem.
//! [`confines_unix_sockets`] asks about that.
//!
//! note: an abstract unix socket has no path for that right to name, and the X server listens on
//! one that accepts any of the user's processes without a cookie - a connection that can type into
//! the person's terminal. Linux 6.12 scopes it: a command may connect to an abstract socket made
//! inside its own confinement, and to no other. [`confines_abstract_sockets`] asks.
//!
//! note: signals have a scope of the same age, and it is not in a command's own ruleset: each
//! command confines itself in its own domain, so it would refuse a command stopping a server an
//! earlier call left running. The scope is checked per layer, so it goes on the one layer every
//! command shares - this program's own, put on by [`scope_signals`] before it starts anything - and
//! a command may signal this program and whatever it started, and nothing else. This program is
//! inside that boundary; keeping it out would take a separate process to start each command from. A
//! confinement run by hand, with no session above it, has no such layer and puts the scope on its
//! own; see [`SESSION_FLAG`].
//!
//! note: applied by re-executing this program in a mode that confines itself and then runs the
//! command. `CommandExt::pre_exec` is the alternative: it runs between `fork` and `exec` in a
//! process with a `tokio` runtime's threads, where almost nothing is safe to call, including the
//! ruleset's allocations. The child is not a `tokio` program: Landlock and the gate restrict the
//! calling thread, and a single-threaded helper makes that simple.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use nachalnik::{Capability, Verdict};

use crate::tools::{Careful, Subject};

mod confinement;
mod reach;

pub(crate) use confinement::REPORT_VAR;
pub use confinement::{
    Confinement, Probed, Scratch, available, confine, confine_saying, confines_abstract_sockets,
    confines_signals, confines_tcp, confines_unix_sockets, make_scratch, run_if_asked,
    scope_signals, scratch_for, weaker_here,
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
/// note: a command that cannot read `/usr/bin` cannot run, so system directories are always
/// readable. The stances a person changes are what is writable and whether the network is
/// reachable.
///
/// note: "the network" is every socket that can leave the machine where the gate holds, and TCP
/// where it does not.
/// See [`Network`], and the note at the top of this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    /// The directory a command may work in; everything outside it is out of reach.
    pub workdir: PathBuf,
    /// Extra paths the user asked for, read-write.
    pub extra: Vec<PathBuf>,
    /// Extra paths the user asked for, readable only.
    ///
    /// note: separate from `extra` because they are asked for by different flags for different
    /// reasons, and a command may hold both at once. What sends most people here is a toolchain:
    /// `cargo` cannot start without `~/.rustup`, and a model that can replace the toolchain it is
    /// about to run is a worse trade than anybody meant to make.
    pub readable: Vec<PathBuf>,
    /// Whether the working directory is writable, or only readable.
    pub writable: bool,
    /// What the command may do about the network.
    pub network: Network,
    /// The devices under `/dev` the command may read and write, and nothing else there; see
    /// [`DEVICES`] for the ones it gets unless somebody says otherwise.
    pub devices: Vec<PathBuf>,
    /// Ports on this machine the command may not connect to, whatever `network` says: the ones this
    /// process serves a session on.
    ///
    /// note: a served session takes a client's answer to a permission question, so a command that
    /// could reach it could answer its own. Under `Open`, and under `Asked` once somebody said yes,
    /// Landlock is asked to refuse these ports and nothing else. `NoTcp` and `Shut` refuse them
    /// already.
    pub closed: Vec<u16>,
}

/// What a confined command may do about the network, and what refuses it.
///
/// note: four rather than open and closed, because two mechanisms stand in the way and they refuse
/// different things. Landlock refuses a TCP `connect` or `bind`, and tells nobody. The gate - see
/// [`crate::gate`] - stops `socket()` for `AF_INET` and `AF_INET6`, and for the two families that
/// reach the internet the same way, which is the first thing any use of the network does, and it
/// can either refuse there or hold the call while somebody is asked. Where the gate cannot be
/// installed only Landlock is left, and a UDP datagram still goes out; that is the one this says as
/// `no TCP` rather than rounding it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Network {
    /// Nothing refuses it.
    Open,
    /// TCP is refused by Landlock, and nothing else is: there is no gate here.
    NoTcp,
    /// Every network socket is refused by the gate, and TCP by Landlock as well.
    Shut,
    /// The gate holds every network socket until the person is asked, once per command.
    ///
    /// note: Landlock leaves TCP alone here, except for [`Sandbox::closed`], because a ruleset
    /// cannot be lifted and a `yes` must be able to let the connection through. The gate alone
    /// stands in the way, so this is never asked for where the gate cannot be installed.
    Asked,
}

impl Network {
    /// What a command gets: `open` where the network was allowed, `denied` where the stance
    /// refuses it, `gated` where the gate holds here, and `tcp` where Landlock can refuse TCP here.
    ///
    /// note: with neither the gate nor Landlock's TCP rights, below Linux 6.7, nothing refuses
    /// anything, and that is [`Network::Open`]. Written as `NoTcp`, a command was told "no TCP"
    /// while its TCP connections went through.
    fn of(open: bool, denied: bool, gated: bool, tcp: bool) -> Self {
        match (open, gated) {
            (true, _) => Self::Open,
            (false, false) if tcp => Self::NoTcp,
            (false, false) => Self::Open,
            (false, true) if denied => Self::Shut,
            (false, true) => Self::Asked,
        }
    }

    /// Whether Landlock is asked to refuse TCP.
    fn refuses_tcp(self) -> bool {
        matches!(self, Self::NoTcp | Self::Shut)
    }

    /// The word the arguments carry it as.
    fn word(self) -> &'static str {
        match self {
            Self::Open => "net",
            Self::NoTcp => "nonet",
            Self::Shut => "shut",
            Self::Asked => "held",
        }
    }

    fn from_word(word: &std::ffi::OsStr) -> Option<Self> {
        [Self::Open, Self::NoTcp, Self::Shut, Self::Asked]
            .into_iter()
            .find(|network| *word == *network.word())
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "the network reachable",
            // TCP is the whole of what Landlock can refuse; see the note at the top
            Self::NoTcp => "no TCP",
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
    /// not permission. Where the gate holds, the command still runs and is asked about if it
    /// reaches out, so the stance is not a refusal either. See [`Careful::gates_the_network`].
    pub fn of(
        policy: &Careful,
        workdir: PathBuf,
        extra: Vec<PathBuf>,
        readable: Vec<PathBuf>,
        granted: bool,
    ) -> Self {
        let stance = policy.stance(&Subject::Capability(Capability::net("reach")));
        let tcp = confines_tcp();
        let writable = writes(policy);
        // note: it reaches the paths `--sandbox-write` opened, which stay readable. `fs` is refused
        // a write there under the same refusal. A shell that could still write in them was two
        // tools disagreeing about one path; the one that could write was the one whose writes
        // nothing checks
        let (extra, readable) = match writable {
            true => (extra, readable),
            false => (Vec::new(), extra.into_iter().chain(readable).collect()),
        };

        Self {
            workdir,
            extra,
            readable,
            writable,
            network: Network::of(
                granted || stance == Verdict::Allow,
                stance == Verdict::Deny,
                policy.gates_the_network(),
                tcp,
            ),
            devices: DEVICES.iter().map(PathBuf::from).collect(),
            closed: closing(tcp, &SERVED.lock().unwrap_or_else(PoisonError::into_inner)),
        }
    }

    /// The arguments that ask this program to confine itself this way and run `cmd`, as one of a
    /// session's commands; see [`SESSION_FLAG`].
    pub fn argv(&self, cmd: &str) -> Vec<OsString> {
        let mut argv = vec![
            OsString::from(EXEC_FLAG),
            OsString::from(SESSION_FLAG),
            self.workdir.clone().into(),
            OsString::from(match self.writable {
                true => "rw",
                false => "ro",
            }),
            OsString::from(self.network.word()),
            OsString::from(self.extra.len().to_string()),
        ];
        argv.extend(self.extra.iter().map(|path| path.clone().into()));
        argv.push(OsString::from(self.readable.len().to_string()));
        argv.extend(self.readable.iter().map(|path| path.clone().into()));
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
        let writable = argv.next()? == "rw";
        let network = Network::from_word(argv.next()?)?;
        let count: usize = argv.next()?.to_str()?.parse().ok()?;
        let extra: Vec<PathBuf> = argv.by_ref().take(count).map(PathBuf::from).collect();
        let count: usize = argv.next()?.to_str()?.parse().ok()?;
        let readable: Vec<PathBuf> = argv.by_ref().take(count).map(PathBuf::from).collect();
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
                extra,
                readable,
                writable,
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

        SYSTEM
            .iter()
            .map(PathBuf::from)
            .chain(std::iter::once(self.workdir.clone()))
            .chain(scratch.map(Path::to_path_buf))
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
    /// note: [`confine`] grants that directory reading and writing even where the working directory
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
        self.writable
            .then_some(self.workdir.as_path())
            .into_iter()
            .chain(scratch)
            .chain(self.extra.iter().map(PathBuf::as_path))
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
    /// a client chooses. Only where [`confines_unix_sockets`] says the kernel governs sockets;
    /// below that, the refusal is the socket's own permissions.
    ///
    /// note: a socket is judged by this alone, not by [`Sandbox::reaches`] as well. Below that
    /// kernel, the ruleset is not consulted about a connection, even to a socket in a directory the
    /// session cannot read; blaming the confinement for that directory blames it for a refusal it
    /// did not make.
    fn connecting(&self, path: &Path, scratch: Option<&Path>) -> Option<bool> {
        use std::os::unix::fs::FileTypeExt as _;

        let resolved = resolve(path)?;
        if !resolved
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_socket())
        {
            return None;
        }
        Some(!self.writes(&resolved, scratch) && confines_unix_sockets())
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
    fn git_config_global(&self) -> Option<PathBuf> {
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

/// The device a path given to `--sandbox-device` names, resolved; `None` where it lands anywhere
/// but beneath `/dev`, or is `/dev` itself.
///
/// note: resolved rather than read off the spelling, because `Path::starts_with` compares
/// components and never settles one: `/dev/../home` begins with `/dev`, and the ruleset opens it as
/// `/home`. A link is followed for the same reason - `/dev/fd` leads into `/proc`, and `/dev/shm`
/// is anybody's to make one in. A device that is not there yet resolves through its parent, so
/// one plugged in later is still grantable. `/dev` itself is every device at once, which is what
/// [`DEVICES`] exists not to grant.
///
/// note: asked twice, by `Setup::check` where the list is given and by [`confine`] where it is
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
        // its own temporary directory, writable even where the working directory is not
        write!(
            f,
            ", its own `$TMPDIR` read-write, the system directories read-only, {}",
            self.network
        )?;
        // note: said wherever TCP is not refused outright, since a closed port is the one
        // connection refused there. A command refused it was told only that the network was
        // reachable, with nothing to say why this address was not
        if !self.network.refuses_tcp() && !self.closed.is_empty() {
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
/// The directories a command has to be able to read before it can be a command at all.
const SYSTEM: &[&str] = &[
    "/usr", "/etc", "/bin", "/sbin", "/lib", "/lib64", "/opt", "/proc", "/sys", "/run",
];

/// The devices a confined command may read and write unless somebody says otherwise, by name;
/// nothing else under `/dev`. `--sandbox-device` and the settings file's `sandbox-device` replace
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

/// The ports a command is to have closed: the ones `served`, where Landlock can close a TCP port
/// (`tcp`), and none where it cannot.
///
/// note: none below Linux 6.7. Named anyway, a command was told that the ports a session is served
/// on were closed to it, and they were not; [`weaker_here`] says so to the person at startup.
fn closing(tcp: bool, served: &[u16]) -> Vec<u16> {
    match tcp {
        true => served.to_vec(),
        false => Vec::new(),
    }
}

/// The ports this process serves a session on, which [`Sandbox::of`] closes to every command.
///
/// note: the process's rather than a session's, because a command that reached any session served
/// from here could answer that session's questions, its own among them.
static SERVED: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// The sessions this process's confined commands have run in, each named by the command's pid.
///
/// note: every session ever started, not just the ones still running. A command's session outlives
/// the command in whatever it left running. A pid is four bytes a command.
static SESSIONS: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

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

/// Remembers the session a confined command runs in, which is its own and named by its pid; see
/// `run_if_asked`.
pub(crate) fn began(pid: u32) {
    SESSIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(pid);
}

/// Whether the process `pid` is in the session of a command confined from here, or cannot be
/// distinguished from one.
///
/// note: `SO_PEERCRED` gives a served socket this pid. A process that has since exited has no
/// session left to ask about, and refusing it loses nothing because there is nobody there to serve.
pub(crate) fn from_a_command(pid: u32) -> bool {
    let session = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .and_then(|pid| rustix::process::getsid(Some(pid)).ok());
    match session {
        Some(session) => SESSIONS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&session.as_raw_nonzero().get().unsigned_abs()),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{Network, closing};

    /// A network nothing can refuse is said to be open, whatever the stance.
    ///
    /// note: below Linux 6.7, where Landlock has no TCP rights, and with no gate, a command told
    /// "no TCP" had its TCP connections go through.
    #[test]
    fn a_network_nothing_can_refuse_is_open() {
        // neither the gate nor Landlock's TCP rights
        assert_eq!(Network::of(false, true, false, false), Network::Open);
        assert_eq!(Network::of(false, false, false, false), Network::Open);
        // Landlock's TCP rights and no gate
        assert_eq!(Network::of(false, true, false, true), Network::NoTcp);
        // the gate, with or without them
        for tcp in [true, false] {
            assert_eq!(Network::of(false, true, true, tcp), Network::Shut);
            assert_eq!(Network::of(false, false, true, tcp), Network::Asked);
            assert_eq!(Network::of(true, false, true, tcp), Network::Open);
        }
    }

    /// A served port is said to be closed to a command only where the kernel can close it.
    #[test]
    fn a_port_is_closed_only_where_the_kernel_can_close_it() {
        assert_eq!(closing(true, &[8080, 9090]), [8080, 9090]);
        assert!(closing(false, &[8080, 9090]).is_empty());
    }
}
