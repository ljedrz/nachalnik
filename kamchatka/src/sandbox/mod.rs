//! A confinement for the `shell` tool, so that the permission stances are enforced rather than
//! reported.
//!
//! note: this is the part of the workspace where a permission stops being a decision point with a
//! paper trail and becomes a boundary. The runtime cannot do it - it executes nothing, so it has
//! nothing to confine - and it is exactly the sort of thing that belongs in the program that
//! actually spawns the process.
//!
//! note: [Landlock](https://landlock.io), which is a Linux LSM a process applies to *itself*: no
//! privileges, no setuid helper, no container, no daemon. What it buys is that `network: deny` is
//! refused by the kernel on the `connect` syscall rather than by a policy reading the word `curl`,
//! which is the difference between a heuristic somebody can walk around and one they cannot. A
//! model refused a `curl` can reach the same page with `python3 -c "import urllib.request"`;
//! under this, that call gets `Permission denied` from the kernel.
//!
//! note: and it is TCP that Landlock buys, because TCP is what the `landlock` crate has:
//! `ConnectTcp` and `BindTcp` are the only two network access rights it exposes. The kernel grew
//! UDP rights in ABI 10, which is Linux 7.2, but the crate stops at ABI 9 and `AccessNet` is
//! `#[non_exhaustive]` over a sealed trait, so those two bits cannot be handed to a ruleset from
//! out here. The rest of the network is the gate's: [`crate::gate`] stops a confined command's
//! `socket()` for `AF_INET` and `AF_INET6`, which covers UDP and everything else an internet
//! socket is for, and either refuses it or holds it while the person is asked. Where the gate
//! cannot be installed a UDP datagram still goes out, which is enough to put bytes in a DNS query,
//! and [`Network::NoTcp`] is how that case is written everywhere it is shown rather than rounded
//! up.
//! What still holds against the rest is the filesystem: a command that cannot read a file has
//! nothing to send.
//!
//! note: a unix socket is the filesystem's rather than the network's, and Linux 7.1 is where the
//! kernel grew the right for it - `AccessFs::ResolveUnix`, which the crate does expose. A command
//! may connect to one it could have written to, and to no other. Below that kernel a `connect` is
//! governed by nothing whatever, which leaves the session bus, the compositor and the container
//! daemon reachable; each of those runs a command outside the domain on the caller's behalf, so
//! the hole is worth more than the UDP one - a datagram carries bytes out, where `systemd-run
//! --user` carries a command out and hands back the whole filesystem.
//! [`confines_unix_sockets`] is where that is asked.
//!
//! note: it is applied by re-executing *this program* in a mode that confines itself and then runs
//! the command. The alternative is `CommandExt::pre_exec`, which runs between `fork` and `exec` in
//! a process that has a `tokio` runtime's threads in it, where almost nothing is safe to call - the
//! ruleset's own allocations among them. The child is deliberately not a `tokio` program: Landlock
//! and the gate restrict the calling thread, and a single-threaded helper is the one shape where
//! that needs no thought.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use nachalnik::{Capability, Verdict};

use crate::tools::{Careful, Subject};

mod reach;

pub use reach::{Access, Reach};

/// The argument that puts this program into the mode that confines itself and runs a command.
pub const EXEC_FLAG: &str = "--confine-and-run";

/// What the `shell` tool is allowed to reach.
///
/// note: read is not a stance here. A command that cannot read `/usr/bin` cannot run at all, so
/// the system directories are always readable and the interesting question is what is *writable*
/// and whether the network is reachable - which are the two stances a person actually changes.
///
/// note: "the network" is every internet socket where the gate holds, and TCP where it does not.
/// See [`Network`], and the note at the top of this module.
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
    /// Landlock is asked to refuse these and nothing else; `NoTcp` and `Shut` refuse them already.
    pub closed: Vec<u16>,
}

/// What a confined command may do about the network, and what refuses it.
///
/// note: four rather than open and closed, because two mechanisms stand in the way and they refuse
/// different things. Landlock refuses a TCP `connect` or `bind`, and tells nobody. The gate - see
/// [`crate::gate`] - stops `socket()` for `AF_INET` and `AF_INET6`, which is the first thing any
/// use of the network does, and it can either refuse there or hold the call while somebody is
/// asked. Where the gate cannot be installed only Landlock is left, and a UDP datagram still goes
/// out; that is the one this says as `no TCP` rather than rounding it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Network {
    /// Nothing refuses it.
    Open,
    /// TCP is refused by Landlock, and nothing else is: there is no gate here.
    NoTcp,
    /// Every internet socket is refused by the gate, and TCP by Landlock as well.
    Shut,
    /// Every internet socket is held by the gate until the person is asked, once per command.
    ///
    /// note: Landlock leaves TCP alone here, but for [`Sandbox::closed`], because a ruleset cannot
    /// be lifted and a `yes` has to be able to let the connection through. What stands in the way is the gate alone, which is
    /// why this is never asked for where the gate cannot be installed.
    Asked,
}

impl Network {
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
    /// note: the network is reachable only if the stance is an outright `allow`, or if this
    /// particular call was allowed by a person who was asked about it. A stance of `ask` that
    /// nobody has been asked about yet is not permission - and where the gate holds it is not a
    /// refusal either: the command runs, and is asked about if it reaches out. See
    /// [`Careful::gates_the_network`].
    pub fn of(
        policy: &Careful,
        workdir: PathBuf,
        extra: Vec<PathBuf>,
        readable: Vec<PathBuf>,
        granted: bool,
    ) -> Self {
        let stance = policy.stance(&Subject::Capability(Capability::net("reach")));
        let writable = writes(policy);
        // note: and it reaches the paths `--sandbox-allow` opened, which stay readable. `fs` is
        // refused a write there under the same refusal, so a shell that could still write in them
        // was two tools disagreeing about one path - and the one that could write was the one
        // whose writes nothing checks
        let (extra, readable) = match writable {
            true => (extra, readable),
            false => (Vec::new(), extra.into_iter().chain(readable).collect()),
        };

        Self {
            workdir,
            extra,
            readable,
            writable,
            network: match (
                granted || stance == Verdict::Allow,
                policy.gates_the_network(),
            ) {
                (true, _) => Network::Open,
                (false, false) => Network::NoTcp,
                (false, true) if stance == Verdict::Deny => Network::Shut,
                (false, true) => Network::Asked,
            },
            devices: DEVICES.iter().map(PathBuf::from).collect(),
            closed: SERVED
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
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
    pub fn from_argv(argv: &[OsString]) -> Option<(Self, OsString)> {
        let mut argv = argv.iter();
        if argv.next()? != EXEC_FLAG {
            return None;
        }
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
    /// note: resolved on both sides, so that a symlink and a `../` are the same question they are
    /// for [`Reach::allows`]. A path that is not there resolves through its parent - which is the
    /// common case, since a command that named a file it could not open often could not `stat`
    /// its directory either.
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
        // note: under `/dev` only the devices named are granted - see `confine` - so one of those
        // was reached and a refusal of it is its own permissions, and anything else there is the
        // boundary
        if resolved.starts_with("/dev") {
            return self
                .devices
                .iter()
                .filter_map(|named| device(named))
                .any(|device| resolved.starts_with(device));
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
    /// note: Landlock refuses an `open` with `EACCES`, which is what a command reports when a
    /// file is somebody else's - so a model is handed `Permission denied (os error 13)` and has
    /// no way at all to tell a boundary from a protected file. It goes hunting for a `cargo` that
    /// was never missing, when what is out of reach is `~/.rustup`. The tool's description says
    /// a permission error out here is the confinement, and that is not enough on its own: this
    /// puts the sentence at the point of failure, naming the path.
    ///
    /// note: three answers rather than two. A refusal that names a path *within* reach was the
    /// file's own permissions - `/etc/shadow` is refused under this and would be refused without
    /// it - and saying "this may have been the sandbox" there would be a hedge that sends a model
    /// looking for a boundary that had nothing to do with it. So a refusal whose paths are all
    /// reachable gets nothing said about it at all.
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
        self.note_for_in(stderr, None)
    }

    /// [`Sandbox::note_for`], for a command that was handed `scratch` as its `TMPDIR`.
    ///
    /// note: [`confine`] grants that directory what it grants the working directory, so a refusal
    /// in it is the file's own permissions and blaming the confinement for one is the hedge
    /// `note_for` exists not to make. It is the caller's to name, because only whoever spawned the
    /// command knows which directory it was given, and it has to still be there to be compared.
    pub(crate) fn note_for_in(&self, stderr: &str, scratch: Option<&Path>) -> Option<String> {
        let refusals: Vec<&str> = stderr.lines().filter(|line| refused(line)).collect();
        if refusals.is_empty() {
            return None;
        }

        // `Vec::dedup` drops only neighbours, and a path is usually named by two lines that are
        // not next to each other - a warning and the failure it led to.
        //
        // note: and it stops at the three it will name. A command's standard error can be the
        // whole of what `KEPT` holds, and every path in it compared against every one kept so far,
        // each costing a dozen `canonicalize` calls, was minutes of work after the command had
        // already ended, with nothing to interrupt it
        let (mut named, mut mentioned): (Vec<String>, bool) = (Vec::new(), false);
        let mut sockets = 0;
        for path in refusals.iter().flat_map(|line| paths_in(line)) {
            mentioned = true;
            if named.contains(&path) {
                continue;
            }
            // note: a relative path is judged where the command started, and named as it was
            // written. A command that moved with `cd` first is judged from the wrong place, which
            // is the rarer mistake than reading every relative path as the boundary's
            let judged = self.workdir.join(&path);
            // a socket is judged by connecting and nothing else: see `connecting`
            let (socket, confined) = match self.connecting(&judged, scratch) {
                Some(refused) => (true, refused),
                None => (false, !self.reaches(&judged, scratch)),
            };
            if confined {
                sockets += usize::from(socket);
                named.push(path);
                if named.len() == 3 {
                    break;
                }
            }
        }

        match (named.is_empty(), !mentioned) {
            // every path it named is one this reaches, so the refusal is the file's own
            (true, false) => None,
            (true, true) => Some(format!(
                "[this command ran confined - {self} - so a permission error below may be that \
                 boundary rather than the file's own permissions.]"
            )),
            // a socket the command could read the path of and not connect to, which "outside what
            // this session reaches" would be false about
            (false, _) if sockets == named.len() => Some(format!(
                "[{} is a socket outside what this session may write, and a confined command may \
                 connect only to a socket it could have written, so the permission error below \
                 is the confinement rather than the socket's own permissions. This command runs \
                 with {self}. Say what you need the socket for and ask for it to be opened up.]",
                named.join(", "),
            )),
            (false, _) => Some(format!(
                "[{} is outside what this session reaches{}, so the permission error below is the \
                 confinement rather than the file's own permissions. This command runs with \
                 {self}. Work inside the working directory, or say what you need the path for \
                 and ask for it to be opened up.]",
                named.join(", "),
                match sockets {
                    0 => "",
                    _ => " or may connect to",
                },
            )),
        }
    }

    /// Whether a confined command may not connect to `path`, if it is a socket; `None` if it is
    /// not one.
    ///
    /// note: the other half of [`Sandbox::reaches`], which asks about reading. A socket is held to
    /// the writing half of the ruleset, so a session can read the path of one under `/run` and be
    /// refused the connection - and `docker ps` came back `Permission denied` with nothing said,
    /// because the path was one the session reaches. By the file's type rather than by the words of
    /// the error, which a client chooses; and only where [`confines_unix_sockets`] says the kernel
    /// governs sockets at all, since below that the refusal really is the socket's own permissions.
    ///
    /// note: and a socket is judged by this alone, not by [`Sandbox::reaches`] as well. Below that
    /// kernel a connection is not something the ruleset is consulted about, even to a socket in a
    /// directory the session cannot read, so blaming the confinement for that directory is
    /// blaming it for a refusal it did not make.
    fn connecting(&self, path: &Path, scratch: Option<&Path>) -> Option<bool> {
        use std::os::unix::fs::FileTypeExt as _;

        let resolved = resolve(path)?;
        if !resolved
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_socket())
        {
            return None;
        }
        let writable = self
            .writable
            .then_some(self.workdir.as_path())
            .into_iter()
            .chain(scratch)
            .chain(self.extra.iter().map(PathBuf::as_path))
            .any(|root| {
                root.canonicalize()
                    .is_ok_and(|root| resolved.starts_with(root))
            });

        Some(!writable && confines_unix_sockets())
    }

    /// What to hand a confined command as `GIT_CONFIG_GLOBAL`; `None` leaves git its own defaults.
    ///
    /// note: git is the one program where being out of reach is fatal rather than inconvenient,
    /// because under Landlock `access(2)` still answers from the file's own permissions. So git
    /// asks whether `~/.gitconfig` is readable, is told yes, opens it, gets `EACCES`, and takes
    /// the *unreadable configuration* branch rather than the *no configuration* branch - `fatal:
    /// unknown error occurred while reading the configuration files`, and every git command in the
    /// session is dead. A missing file is fine; an unreadable one is not, and a confined command
    /// cannot tell git which it has.
    ///
    /// note: only when a configuration a confined command could not read is what git would open
    /// otherwise. Git reads a person's aliases and identity out of these, and quietly throwing
    /// them away for a command that could have had them would be its own bug. A path set in
    /// `GIT_CONFIG_GLOBAL` is a choice somebody made, and is left alone where it is readable - but
    /// left alone out of reach is what kills git, the same as any other configuration, and the
    /// *no configuration* case is the one it handles.
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

/// Whether the policy leaves the reach writable: a refusal of `write` reaches the shell too, and
/// anything short of a refusal leaves the working directory writable, because a shell that cannot
/// write in it is not one anybody can work with.
///
/// note: one function for [`Sandbox::of`] and [`Reach`]'s refusals, which describe the same session
/// and should not come to two answers about it.
fn writes(policy: &Careful) -> bool {
    policy.stance(&Subject::Capability(Capability::fs("write"))) != Verdict::Deny
}

/// The deepest existing part of a path, resolved, with whatever is left over joined back on.
///
/// note: a file about to be created has no canonical form of its own, and a path handed to
/// `write` is usually one of those. It is resolved through its parent instead, because a file
/// cannot be created outside a directory it is not in.
///
/// note: one function rather than two, so that [`Reach::allows`] and [`Sandbox::reaches`] cannot
/// come to different answers about the same path.
///
/// note: a symlink to something that is not there has no canonical form either, and it is *not* a
/// file about to be created: writing through it creates its target, wherever that is. So it is
/// followed, the way the open will follow it, rather than stepped past as though it were a name
/// with nothing behind it - which would let `write` create a file outside the directory through a
/// link made inside it. `LINKS` bounds the following, because two links can point at each other.
///
/// note: `None` where that bound runs out with a link still in hand and no link seen twice. Taken
/// for a name with nothing behind it, the last link was allowed as a file about to be made in the
/// directory it sits in, and the open then followed the rest of the chain wherever it went - out
/// of the directory, on a kernel with no `openat2` to stop it there. A loop is not that: every
/// link in it has been seen, it leads nowhere, and the open refuses it, so it is answered where it
/// is - inside or outside, like any other name.
pub(crate) fn resolve(path: &Path) -> Option<PathBuf> {
    const LINKS: usize = 40;

    let mut existing = path.to_path_buf();
    let mut rest = PathBuf::new();
    let mut followed = 0;
    let mut seen = std::collections::HashSet::new();
    let joined = |base: PathBuf, rest: &Path| match rest.as_os_str().is_empty() {
        // note: joined only when there is something to join. `Path::join("")` appends a
        // separator, and `/w/local.txt/` is a directory that is not there, so a plain
        // `./local.txt` would come back `Not a directory`
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
                // a relative target is relative to the directory the link is in, and an absolute
                // one replaces the lot, which is what `join` does with each
                existing = match existing.parent() {
                    Some(parent) => parent.join(target),
                    None => target,
                };
            }
            Err(_) => match (existing.file_name(), existing.parent()) {
                (Some(name), Some(parent)) => {
                    // note: and the same guard here, for the same reason. Without it every path
                    // that does not exist yet comes back with a separator on the end, so `write`
                    // can create no file at all: `notes.txt/` is a directory, and the tool
                    // reports `Is a directory (os error 21)` for a file it has just been asked to
                    // make
                    rest = match rest.as_os_str().is_empty() {
                        true => PathBuf::from(name),
                        false => Path::new(name).join(&rest),
                    };
                    existing = parent.to_path_buf();
                }
                // what is left, rather than the path as it was handed in: after a link has been
                // followed those are different paths, and only this one is where the open goes
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

/// Whether a line of standard error looks like something was refused permission.
///
/// note: three spellings because three layers write them: a C program's `strerror`, Rust's
/// `io::Error` display, and the errno name itself. All three are the same refusal.
///
/// note: the lowercase spelling too, which is Go's - `docker` says `connect: permission denied`,
/// and a refused socket is the refusal a Go client is most often the one reporting.
fn refused(line: &str) -> bool {
    line.contains("Permission denied")
        || line.contains("permission denied")
        || line.contains("os error 13")
        || line.contains("EACCES")
}

/// The paths a line of standard error mentions: absolute ones, and relative ones with a `/` in
/// them, as written.
///
/// note: a token that is a path once the punctuation a message wraps one in has been taken off.
/// Stripped *first* and tested afterwards, because a message such as `could not read settings
/// file: '/home/you/.rustup/settings.toml': Permission denied` has the token `'/home/...':`, which
/// does not start with a slash at all.
///
/// note: opening quotes and brackets from the front, and closing ones and a sentence's punctuation
/// from the back - never a `.` from the front, which turned a relative `./x` into an absolute `/x`
/// outside the reach, and the note then blamed the confinement for the file's own permissions. And
/// a trailing `.` only after a name, so `../..` keeps its meaning.
///
/// note: a relative path needs a `/` to count, because every refusal has words in it, and a word
/// taken for a path in the working directory would be a path reached - silencing the general
/// sentence a refusal naming nothing is owed. A URL is not a path.
///
/// note: it misses a path with a space in it, which is the right way round: a caller that gets
/// nothing says something general instead, and one that gets a wrong path would say something
/// false.
fn paths_in(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(bare)
        .filter(|token| {
            (token.starts_with('/') && token.len() > 1)
                || (token.contains('/') && !token.contains("://"))
        })
        .map(str::to_owned)
        .collect()
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
        write!(f, ", the system directories read-only, {}", self.network)
    }
}

/// How much of a [`Sandbox`] the kernel actually agreed to.
///
/// note: a separate value, so that "not confined" is sayable. A sandbox that silently did nothing
/// on an old kernel would be a promise on the screen and nothing behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confinement {
    /// Every restriction asked for is in force.
    Full,
    /// The kernel understood some of it; what it did not understand is not restricted.
    Partial,
    /// Landlock is not available - too old a kernel, or not enabled at boot.
    Unavailable,
    /// Nobody asked for one: `--no-sandbox`, or a session nothing has probed for it.
    Off,
}

impl Confinement {
    /// Returns whether a command running under this is actually restricted.
    pub fn is_confined(self) -> bool {
        matches!(self, Self::Full | Self::Partial)
    }
}

impl fmt::Display for Confinement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full => "confined",
            Self::Partial => "partly confined",
            Self::Unavailable => "not confined (no Landlock in this kernel)",
            Self::Off => "not confined (the sandbox is off)",
        })
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

/// The ports this process serves a session on, which [`Sandbox::of`] closes to every command.
///
/// note: the process's rather than a session's, because a command that reached any session served
/// from here could answer that session's questions, its own among them.
static SERVED: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// The sessions this process's confined commands have run in, each named by the command's pid.
///
/// note: every one ever started rather than the ones still running. A command's session outlives
/// the command in whatever it left running, which is what would come knocking, and a pid is four
/// bytes a command.
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

/// Whether the process `pid` is in the session of a command confined from here, or cannot be told
/// apart from one.
///
/// note: `SO_PEERCRED` is what hands a served socket this pid, and a process that has gone since
/// has no session left to ask about. Nothing is lost by refusing it, since there is nobody there to
/// serve.
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

/// Whether this kernel refuses a confined command a connection to a unix socket outside what it
/// may write.
///
/// note: Landlock governs a pathname unix socket from ABI 9, which is Linux 7.1. Below that a
/// `connect` is not an access right at all, so the socket answers whatever its own permissions say
/// and the ruleset is not consulted; the [module note](crate::sandbox) says what that leaves in
/// reach.
///
/// note: the question is put to the kernel, and put to it through the crate rather than by reading
/// a version number. `HardRequirement` is the level at which a right the kernel does not have is an
/// error instead of something quietly dropped, and a `Ruleset` that is only built restricts
/// nothing: this applies no ruleset and confines no process.
pub fn confines_unix_sockets() -> bool {
    use landlock::{AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr};

    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::ResolveUnix)
        .is_ok()
}

/// Applies the sandbox to *this* process, returning how much of it the kernel took.
///
/// note: `scratch` is a directory of this run's own, handed over as `TMPDIR`, rather than the
/// whole of `/tmp`. A command that cannot write a temporary file will not run - a compiler or an
/// interpreter needs one - but opening up `/tmp` wholesale gives it a shared space to write into,
/// and, if somebody happens to be working in a directory under `/tmp`, quietly makes a refused
/// `write` stance mean nothing at all.
///
/// note: and it is an [`Option`], because [`make_scratch`] is allowed to fail and this must not
/// paper over it by handing the command a `TMPDIR` that is not there. The rule for it is dropped
/// either way: `path_beneath_rules` leaves out a path it cannot open rather than failing, so a
/// directory that has gone away costs its own rule and nothing else - which is a fact about
/// `landlock` rather than about this program, and `tests/sandbox.rs` holds the version to it.
///
/// note: under `/dev`, reading and writing [`Sandbox::devices`] and nothing else - [`DEVICES`]
/// unless somebody said otherwise - because `/dev/null` is not optional and the rest of `/dev`
/// reaches past the command: another terminal of the person's, their shared memory, a camera.
pub fn confine(sandbox: &Sandbox, scratch: Option<&Path>) -> Confinement {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, NetPort, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetError, RulesetStatus, path_beneath_rules,
    };

    // note: V3 rather than V1, for `Truncate`. An access right the ruleset does not *handle* is
    // not restricted at all, and truncation is not covered by V1's `WriteFile`: `truncate(2)`
    // takes a path and never opens the file, so a confined command could zero any file it could
    // name. Handling it costs nothing here, since `from_all` grants it again on every writable
    // path. V2's `Refer` comes with it and is the more permissive of the two: with it unhandled
    // the kernel refuses every cross-directory rename outright, and with it granted a `mv`
    // between two directories of the working directory is allowed, which is what anybody would
    // expect of a shell in there.
    //
    // note: not V5's `IoctlDev`. A file's ioctls are decided when it is opened, so what handling it
    // would refuse is an ioctl on a device the command opened itself, and the only devices it can
    // open are [`DEVICES`], whose ioctls do nothing worth refusing. A `/dev/null` it opened would
    // answer `EACCES` where it answers `ENOTTY`, and the kernel would have to be 6.10.
    //
    // note: on a kernel older than 6.2 the rights below V3 still apply and the status comes back
    // `Partial`, which is said out loud rather than rounded up.
    //
    // note: V9's `ResolveUnix` is handled beside them where the kernel has it, and asked for
    // nowhere else. It is the one right here that a kernel in ordinary use may not have, and
    // handling a right that is not there costs the whole ruleset its `Full` status: best-effort
    // drops it and reports `Partial`, so every kernel below 7.1 would start calling itself
    // partially confined over a right it was never going to enforce. And `tests/sandbox.rs`
    // skips on anything short of `Full`, so it would quietly stop testing the sandbox where most
    // of it is run. Granted again on every writable path below, the same way `Truncate` is: a
    // command may connect to a socket it could have written to, and to no other.
    let abi = ABI::V3;
    let rights = match confines_unix_sockets() {
        true => AccessFs::from_all(abi) | AccessFs::ResolveUnix,
        false => AccessFs::from_all(abi),
    };
    let Ok(mut ruleset) = Ruleset::default().handle_access(rights) else {
        return Confinement::Unavailable;
    };
    if sandbox.network.refuses_tcp() {
        // ABI v4 and up; on an older kernel this is the part that comes back `Partial`.
        //
        // note: named rather than `AccessNet::from_all`, which is the same two rights today and
        // would silently become more than TCP the day the crate grows ABI 10's UDP pair - a
        // sandbox that started refusing more than it says it refuses, on a `cargo update`. The
        // two bits exist in the kernel already, and cannot be reached from here: see the note at
        // the top of this module for why, and change the wording along with the rights.
        match ruleset.handle_access(AccessNet::ConnectTcp | AccessNet::BindTcp) {
            Ok(with_net) => ruleset = with_net,
            Err(_) => return Confinement::Unavailable,
        }
    }
    // note: a port is closed by granting every other one, since a ruleset only ever grants. That is
    // a rule a port, built in a few tens of milliseconds, and paid only by a command confined while
    // a session is served over TCP. `BindTcp` stays unhandled: listening refuses nobody's answer
    let closing = !sandbox.network.refuses_tcp() && !sandbox.closed.is_empty();
    if closing {
        match ruleset.handle_access(AccessNet::ConnectTcp) {
            Ok(with_net) => ruleset = with_net,
            Err(_) => return Confinement::Unavailable,
        }
    }
    let open = (1..=u16::MAX)
        .filter(|port| closing && !sandbox.closed.contains(port))
        .map(|port| Ok::<_, RulesetError>(NetPort::new(port, AccessNet::ConnectTcp)));

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
    let devices: Vec<PathBuf> = sandbox
        .devices
        .iter()
        .filter_map(|named| device(named))
        .collect();

    let restricted = ruleset
        .create()
        .and_then(|created| {
            created.add_rules(path_beneath_rules(&readable, AccessFs::from_read(abi)))
        })
        .and_then(|created| {
            created.add_rules(path_beneath_rules(
                &devices,
                AccessFs::ReadFile | AccessFs::WriteFile,
            ))
        })
        .and_then(|created| created.add_rules(path_beneath_rules(&writable, rights)))
        .and_then(|created| created.add_rules(open))
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

/// The temporary directory a confined command is given, named after the process it belongs to.
///
/// note: named after the child rather than made with a random name, so that the process which
/// *spawned* that child can find it again and remove it. The child cannot: `/tmp` is not writable
/// under the ruleset and unlinking a directory is a write to the one it sits in, so every command
/// would leave an empty `kamchatka-<pid>` behind for good. Removing it is therefore the caller's
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
/// `create_dir_all` is happy with anything it finds, a symlink included, and the ruleset grants
/// the *resolved* path everything a writable root gets: a link left in `/tmp` by another account
/// would open up whatever it pointed at, and `TMPDIR` would send the command there.
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
    {
        use std::os::unix::fs::PermissionsExt as _;

        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }

    Some(path.to_path_buf())
}

/// What a probe found here: how much of a ruleset the kernel takes, and whether the network gate
/// holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Probed {
    /// How much of the ruleset the kernel took.
    pub confinement: Confinement,
    /// Whether the gate went on, and the kernel can hold a call for this process to answer; see
    /// [`crate::gate`].
    pub gated: bool,
}

/// Whether a confinement would hold here, and the gate with it, asked without running anything.
///
/// note: asked in a child, because finding out means applying a ruleset and a process cannot take
/// one off again. Doing it in the terminal's own process would confine the terminal.
///
/// note: the child is asked for [`Network::Shut`], so it installs the gate as it would for a
/// refusing session, sends the listener down the socket it is handed, and says whether both took.
/// Nothing reads the listener here, because `exit 0` makes no attempt to answer; it is dropped with
/// this end of the socket. What the child cannot try is a held call being let through, and
/// [`crate::gate::holds`] asks the kernel that without installing anything.
pub fn available(program: &Path) -> Probed {
    let sandbox = Sandbox {
        workdir: std::env::temp_dir(),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: Network::Shut,
        devices: DEVICES.iter().map(PathBuf::from).collect(),
        closed: Vec::new(),
    };
    // where there is no gate the child has nothing to send, fails to install one, and says so
    let (stdin, _arriving) = match crate::gate::pair() {
        Ok((stdin, arriving)) => (stdin, Some(arriving)),
        Err(_) => (std::process::Stdio::null(), None),
    };
    // spawned rather than run to completion in one call, because the answer is read out of a
    // child that has left a directory behind it, and the identifier is how it is found again
    let output = std::process::Command::new(program)
        .args(sandbox.argv("exit 0"))
        .env(REPORT_VAR, "1")
        .stdin(stdin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|child| {
            let scratch = scratch_for(child.id());
            let output = child.wait_with_output();
            let _ = std::fs::remove_dir_all(&scratch);

            output
        });

    let reported = output.ok().and_then(|output| {
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .find_map(|line| line.strip_prefix(REPORT).map(str::to_owned))
    });
    let (took, gate) = match &reported {
        Some(reported) => reported.split_once(' ').unwrap_or((reported, "")),
        None => ("", ""),
    };
    let confinement = match took {
        "full" => Confinement::Full,
        "partial" => Confinement::Partial,
        _ => Confinement::Unavailable,
    };

    Probed {
        confinement,
        gated: confinement.is_confined() && gate == "gated" && crate::gate::holds(),
    }
}

/// The line the confined child writes to say how much of the sandbox took.
const REPORT: &str = "kamchatka-confinement:";

/// Set by [`available`] to ask for that line, and by nothing else.
///
/// note: it is asked for rather than always written, because a confined command's standard error
/// is collected and put in front of the model. A line of this program's own bookkeeping in there
/// is context nobody added on purpose, counted against the budget and read by the model.
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
/// domain is inherited across `execve`.
pub fn run_if_asked() -> Option<i32> {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let (sandbox, cmd) = Sandbox::from_argv(&argv)?;

    // note: a session of its own, which leaves the terminal behind. The terminal is the one this
    // program's screen reads its keys from: a command that kept it could push a `y` into that
    // input with `TIOCSTI` and answer its own question, or draw over the question somebody is
    // reading. [`DEVICES`] leaves `/dev/tty` out as well, and this holds for whatever else could
    // open it: with no controlling terminal `/dev/tty` does not open, and `TIOCSTI` is refused on
    // every other one. It is also a group of its own, the one `shell` stops, with this process's
    // identifier, which is the number `shell` signals
    let detached = rustix::process::setsid().is_ok() || std::fs::File::open("/dev/tty").is_err();

    // a temporary directory of this run's own, made before anything is restricted and handed to
    // the command as `TMPDIR`; see the notes on `confine` and `make_scratch`. Whoever spawned this
    // removes it again, being the only one of the two processes that can
    let scratch = make_scratch(&scratch_for(std::process::id()));

    let confinement = confine(&sandbox, scratch.as_deref());
    // note: after the ruleset, so that nothing the gate does is outside it, and before the command,
    // which inherits the filter across `exec` and into everything it starts
    let gated = match sandbox.network {
        Network::Shut | Network::Asked => Some(crate::gate::hold()),
        Network::Open | Network::NoTcp => None,
    };
    if std::env::var_os(REPORT_VAR).is_some() {
        eprintln!(
            "{REPORT}{}{}",
            match confinement {
                Confinement::Full => "full",
                Confinement::Partial => "partial",
                Confinement::Unavailable | Confinement::Off => "unavailable",
            },
            match gated {
                Some(Ok(())) => " gated",
                _ => "",
            }
        );
    }

    // note: asked to confine *and* to run, in that order, so a ruleset that did not take leaves
    // nothing to do. Running the command anyway would run it unconfined, with the whole
    // filesystem and the network, and with nothing saying so, which is the one thing
    // `Confinement` exists to make sayable. What the permissions tab draws is the startup probe,
    // which is a different call in a different process: `Setup::wire` only hands `shell` a
    // confiner where that probe held, but `Shell` is public and an embedder can hand it one
    // anywhere. This is the half that makes the guarantee the child's rather than the caller's.
    //
    // note: no test reaches this on a machine whose kernel confines, which is where the suite
    // runs - what decides it is `create` failing, and Landlock is either there or it is not.
    // `landlock` drops a rule it cannot build rather than failing, so a missing path does not.
    if !confinement.is_confined() {
        eprintln!(
            "nothing was run: this program was asked to confine the command first and the sandbox \
             did not take"
        );
        return Some(126);
    }
    // note: the same guarantee for the gate. Asked to hold the network, the ruleset has left TCP
    // open for a `yes` to let through, so a command run without the filter would have the network
    // with nobody asked; and asked to shut it, it would have UDP with the screen saying otherwise.
    // What the probe found decides whether either is asked for, so this is for the case the probe
    // did not see - and for a child whose standard input is not a socket to send the listener down
    if let Some(Err(e)) = &gated {
        eprintln!(
            "nothing was run: this program was asked to put the network behind a gate first and \
             the filter did not take: {e}"
        );
        return Some(126);
    }
    // note: and for the terminal. `setsid` is refused to a process that already leads a group,
    // which `shell` does not make this one; whoever did has left it holding a terminal that its
    // keys may be read from
    if !detached {
        eprintln!(
            "nothing was run: this program was asked to confine the command and could not leave \
             the terminal it was started from"
        );
        return Some(126);
    }

    let mut command = std::process::Command::new("sh");
    command.arg("-c").arg(&cmd).current_dir(&sandbox.workdir);
    // standard input was the socket the listener went down, and the command has no business with
    // it: `shell` gives every command `/dev/null` there
    if gated.is_some() {
        command.stdin(std::process::Stdio::null());
    }
    // note: and taken away where there is none, as `make_scratch` says. What this program was
    // handed is the directory the scratch could not be made in, and passing it on would tell the
    // command it has somewhere to write where it most likely has not
    match &scratch {
        Some(scratch) => command.env("TMPDIR", scratch),
        None => command.env_remove("TMPDIR"),
    };
    // note: a ruleset is in force here - an unconfined run has already returned - and that is the
    // only case this is for. Unconfined, git can read its own configuration, and pointing it
    // elsewhere would take a person's identity and aliases away for nothing
    if let Some(global) = sandbox.git_config_global() {
        command.env("GIT_CONFIG_GLOBAL", global);
    }

    // `exec` returns only when it could not happen at all
    let failure = std::os::unix::process::CommandExt::exec(&mut command);
    eprintln!("could not run the command: {failure}");

    Some(127)
}
