//! How much of a [`Sandbox`] the kernel takes, and the sandlock sandbox a command runs in: the
//! probes, the rules, the scratch directory a confined command writes into, and the mode this
//! program runs a command confined from the command line in. The notes at the top of
//! [the module](crate::sandbox) say why it is shaped this way.

use std::{
    ffi::OsString,
    fmt,
    os::fd::{AsRawFd as _, OwnedFd},
    path::{Path, PathBuf},
};

use sandlock_core::{
    Protection, ProtectionStatus, SandboxBuilder, StdioMode,
    policy_fn::{Permissions, PolicyFn, SyscallCategory, Verdict},
};

use super::{DEVICES, Network, SESSION_FLAG, SYSTEM, Sandbox, device, resolve};

/// How much of a [`Sandbox`] the kernel agreed to.
///
/// note: a separate value, so that "not confined" is sayable. A sandbox that silently did nothing
/// on an old kernel would be a promise on the screen and nothing behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confinement {
    /// Every restriction asked for is in force.
    Full,
    /// The kernel understood some of it; what it did not understand is not restricted.
    Partial,
    /// The sandbox would not start - too old a kernel, Landlock not enabled at boot, or seccomp
    /// refused.
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
            Self::Unavailable => "not confined (the sandbox would not start in this kernel)",
            Self::Off => "not confined (the sandbox is off)",
        })
    }
}

/// The Landlock ABI this kernel has; `0` where it has none.
fn abi() -> u32 {
    sandlock_core::landlock_abi_version().unwrap_or(0)
}

/// Whether this kernel refuses a confined command a connection to an abstract unix socket made
/// outside its confinement.
///
/// note: an abstract socket has no path for a filesystem rule to name, and a desktop listens on
/// them: the X server's `@/tmp/.X11-unix/X0` takes a connection from any process of the user's
/// with no cookie, and a connection to it can type into any window - the person's terminal
/// included. Landlock scopes it from ABI 6, which is Linux 6.12.
pub fn confines_abstract_sockets() -> bool {
    Protection::AbstractUnixSocketScope.min_abi() <= abi()
}

/// Whether this kernel can refuse a process a signal to anything outside its Landlock domain.
///
/// note: the scope is ABI 6, which is Linux 6.12.
pub fn confines_signals() -> bool {
    Protection::SignalScope.min_abi() <= abi()
}

/// What this kernel leaves out of a confined command's sandbox, in a sentence for the person; `None`
/// where it leaves out nothing this program asks it for.
///
/// note: said at startup rather than only on the permissions tab, because what is missing is the
/// kernel's age and the tab draws a sandbox that took as "confined" either way. Not worked around:
/// every gap here is closed by a kernel a year or more old.
pub fn weaker_here() -> Option<String> {
    weaker(Kernel {
        landlock: abi() > 0,
        abstract_sockets: confines_abstract_sockets(),
        signals: confines_signals(),
        scope_refused: SCOPE_REFUSED.load(std::sync::atomic::Ordering::Relaxed),
    })
}

/// Whether [`scope_signals`] was refused by a kernel that has the scope.
static SCOPE_REFUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a kernel's Landlock confines, of what [`weaker_here`] asks about.
#[derive(Clone, Copy)]
struct Kernel {
    landlock: bool,
    abstract_sockets: bool,
    signals: bool,
    /// Whether the kernel has the signal scope and refused to put this process in one.
    scope_refused: bool,
}

/// [`weaker_here`], from the kernel's answers.
fn weaker(kernel: Kernel) -> Option<String> {
    if !kernel.landlock {
        return Some(
            "this kernel has no Landlock, so shell commands run unconfined: the sandbox needs Linux \
             5.13 or later with Landlock enabled"
                .to_owned(),
        );
    }

    let open: Vec<&str> = [
        (
            kernel.abstract_sockets,
            "abstract unix sockets, the X server's among them (Linux 6.12)",
        ),
        (
            kernel.signals,
            "signals to your other processes (Linux 6.12)",
        ),
    ]
    .into_iter()
    .filter(|(confined, _)| !confined)
    .map(|(_, what)| what)
    .collect();

    let older = (!open.is_empty()).then(|| {
        format!(
            "this kernel's sandbox does not keep shell commands from {}; a newer kernel does",
            open.join("; ")
        )
    });
    // note: said apart from the list, since it is not the kernel's age: this kernel has the scope
    // and would not apply it, so every command may signal every process of yours, as on an older
    // one, and a newer kernel is not the answer
    let refused = (kernel.signals && kernel.scope_refused).then_some(
        "the kernel would not put this session where its shell commands cannot signal your other \
         processes, so they can",
    );
    match (older, refused) {
        (Some(older), Some(refused)) => Some(format!("{older}; and {refused}")),
        (older, refused) => older.or_else(|| refused.map(str::to_owned)),
    }
}

/// Puts this process in a Landlock domain that refuses signals to anything outside it. Everything
/// started from here on - every command, and what a command leaves running - may signal this
/// process and what it started, and nothing else. Returns whether the kernel took it.
///
/// note: applied to this process rather than a command's sandbox, because the scope is checked per
/// layer. A command's own layer is its alone, and a scope there refuses it the jobs an earlier call
/// left running; stopping those jobs is how a background server is stopped. A layer all commands
/// inherit is the session's boundary instead. Without it, `kill -9 -1` from a command reached every
/// process the person has, this program among them. So each command's sandbox has the scope off;
/// see [`Sandbox::sandlock`].
///
/// note: before the runtime exists, since Landlock binds the calling thread and the threads it
/// starts afterwards - a command spawned from an earlier worker thread would be outside the layer -
/// and before anything is spawned, since this process may not signal what it started before it and
/// stops its MCP servers by signal. It costs `no_new_privs` on everything this process starts, so
/// the caller does not ask for it under `--no-sandbox`: a command there, or an MCP server anywhere,
/// gains no privileges through a set-user-ID program such as `sudo`.
///
/// note: a kernel without the scope - below Linux 6.12 - returns `false` and applies nothing,
/// instead of a ruleset that restricts nothing and still costs the privileges.
///
/// note: every filesystem right is granted beneath `/`, because sandlock handles every one the
/// kernel has, and a right handled and not granted is refused: `Refer` alone, left out, made every
/// rename and link between directories `EXDEV`, in every command and in this process, and a
/// compiler writes output in one directory and renames it into another. Granted everywhere, each
/// is the same right on both sides of any move; the kernel's other condition - that a file gains no
/// rights by moving - is for the command's own ruleset.
///
/// note: the abstract socket scope is off, since it would bind this process: a clipboard helper
/// reaches the X server through an abstract socket. Each command's sandbox has it on.
pub fn scope_signals() -> bool {
    if !confines_signals() {
        return false;
    }
    let layer = sandlock_core::Sandbox::builder()
        .fs_read("/")
        .fs_write("/")
        .allow_degraded(Protection::FsRefer)
        .allow_degraded(Protection::FsTruncate)
        .allow_degraded(Protection::NetTcp)
        .allow_degraded(Protection::FsIoctlDev)
        .disable(Protection::AbstractUnixSocketScope)
        .build();
    // note: Landlock binds a process only once it may gain no privileges; sandlock leaves that to
    // whoever confines themselves
    let scoped = layer.is_ok_and(|layer| {
        rustix::thread::set_no_new_privs(true).is_ok()
            && sandlock_core::landlock_policy::confine_filesystem(&layer).is_ok()
    });
    // a kernel that has the scope but will not apply it is a weaker sandbox than it claims, and
    // `weaker_here` says so
    if !scoped {
        SCOPE_REFUSED.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    scoped
}

/// What a command's sandbox holds open for the roots it grants, and what it says about
/// the roots it left out.
///
/// note: each of the session's own roots is opened here without following a link, and granted by
/// the path the kernel gives back for what was opened. sandlock takes paths only, opens each again
/// in the child it forks and follows whatever is there by then, so between the two a command left
/// running could still swap a root for a link; the window runs from here, through building the
/// sandbox and forking, until the child has opened every path. Naming the descriptor as
/// `/proc/self/fd/N` would close it, but sandlock leaves out every grant under `/proc`, since it
/// serves `/proc` to a command itself. See POSTPONED.md.
pub struct Rules {
    /// The descriptors the rules name.
    held: Vec<OwnedFd>,
    /// A line for each root left out, for the command's standard error.
    left_out: Vec<String>,
}

impl Sandbox {
    /// A sandlock sandbox for this, around a command given `scratch` as its temporary directory.
    /// `policy` answers the network, where it is not open; `scope`
    /// asks for signals to be scoped to the command, which only a confinement with no session
    /// above it wants - see [`SESSION_FLAG`].
    ///
    /// note: `scratch` is this run's own directory, handed over as `TMPDIR`, not the whole of
    /// `/tmp`. A command that cannot write a temporary file will not run - a compiler or
    /// interpreter needs one - but opening `/tmp` wholesale gives it a shared write space, and, if
    /// somebody is working in a directory under `/tmp`, quietly makes a refused `write` stance mean
    /// nothing. It is an [`Option`] because [`make_scratch`] may fail and this must not hand the
    /// command a `TMPDIR` that is not there.
    ///
    /// note: under `/dev`, reading and writing [`Sandbox::devices`] and nothing else - [`DEVICES`]
    /// unless somebody said otherwise - because `/dev/null` is not optional and the rest of `/dev`
    /// reaches past the command: another terminal of the person's, their shared memory, a camera.
    ///
    /// note: each protection that needs a newer Landlock than the kernel's is asked for where the
    /// kernel has it and gone without where it does not, and [`degraded`] says which. The ones that
    /// matter are said at startup by [`weaker_here`].
    pub fn sandlock(
        &self,
        scratch: Option<&Scratch>,
        policy: Option<PolicyFn>,
        scope: bool,
    ) -> (SandboxBuilder, Rules) {
        let mut builder = sandlock_core::Sandbox::builder()
            .cwd(&self.workdir)
            .allow_degraded(Protection::FsRefer)
            .allow_degraded(Protection::FsTruncate)
            .allow_degraded(Protection::NetTcp)
            .allow_degraded(Protection::FsIoctlDev)
            .allow_degraded(Protection::AbstractUnixSocketScope);
        builder = match scope {
            true => builder.allow_degraded(Protection::SignalScope),
            false => builder.disable(Protection::SignalScope),
        };

        // note: a `shell-read` path beneath one of the session's own roots is left out. The root
        // grants reading it already, and is held open without following a link; this rule follows
        // one, as `/bin` needs, and a command that may write beside the path could swap it for a
        // link the next command's rule then led through. Judged by where the name is - its
        // directory resolved, and the name itself as it is - not by where a link at it leads
        let roots: Vec<PathBuf> = std::iter::once(&self.workdir)
            .chain(&self.readable)
            .chain(&self.writable)
            .filter_map(|root| resolve(root))
            .collect();
        for path in &self.shell_readable {
            let at = match (path.parent().and_then(resolve), path.file_name()) {
                (Some(parent), Some(name)) => parent.join(name),
                _ => path.clone(),
            };
            if !roots.iter().any(|root| at.starts_with(root)) {
                builder = builder.fs_read_if_exists(path);
            }
        }
        let mut rules = Rules {
            held: Vec::new(),
            left_out: Vec::new(),
        };
        let readable = std::iter::once(&self.workdir).chain(&self.readable);
        for root in readable {
            if let Some(named) = rules.hold(root) {
                builder = builder.fs_read(named);
            }
        }
        let writable = std::iter::once(&self.workdir)
            .filter(|_| self.workdir_writable)
            .chain(&self.writable);
        for root in writable {
            if let Some(named) = rules.hold(root) {
                builder = builder.fs_write(named);
            }
        }
        // a device that is not plugged in is not granted; sandlock refuses a rule for a path that is
        // not there rather than leaving it out
        for device in self.devices.iter().filter_map(|named| device(named)) {
            builder = builder.fs_read_if_exists(&device);
            if device.exists() {
                builder = builder.fs_write(device);
            }
        }
        if let Some(named) =
            scratch.and_then(|scratch| std::fs::read_link(fd_path(&scratch.held)).ok())
        {
            builder = builder.fs_write(named);
        }

        // note: `*:*` is TCP and UDP to anywhere, and `icmp://*` the ping sockets, which nothing
        // implies. The ports this process serves on are refused on any address, and a refusal
        // outranks every allow and every answer
        let anywhere = Permissions::new().net_allow("*:*").net_allow("icmp://*");
        builder = match (self.network, policy) {
            (Network::Open, _) => builder.net_allow("*:*").net_allow("icmp://*"),
            (Network::Shut | Network::Asked, Some(policy)) => builder.policy_fn(
                policy
                    .events(&[SyscallCategory::Network])
                    .verdict_timeout(None)
                    .approvable(anywhere),
            ),
            (Network::Shut | Network::Asked, None) => builder.policy_fn(refusing(anywhere)),
        };
        for port in &self.closed {
            builder = builder.net_deny(format!("tcp://*:{port}"));
        }

        (builder, rules)
    }
}

/// The command sandlock is to run, `cmd` under `sh`, and `builder` with `env` as its whole
/// environment.
pub fn command(
    builder: SandboxBuilder,
    env: impl IntoIterator<Item = (String, String)>,
    cmd: &str,
) -> (SandboxBuilder, [&str; 3]) {
    let builder = env
        .into_iter()
        .fold(builder.clean_env(true), |builder, (key, value)| {
            builder.env_var(key, value)
        });

    (builder, ["sh", "-c", cmd])
}

/// Whether the kernel would start `cmd` with `env`, as [`command`] hands them to sandlock;
/// `E2BIG` where it would not.
///
/// note: asked before sandlock forks, because its child is the one that finds out otherwise, and
/// says so on the command's standard error with exit 127, which a model cannot tell from a command
/// that failed. One string may be 32 pages; all of them, with a pointer each, a quarter of the
/// stack's limit and no more than 6 MiB, as `execve` counts them.
pub fn fits(env: &[(String, String)], cmd: &str) -> std::io::Result<()> {
    const ONE: usize = 32 * 4096;
    const ALL: usize = 6 << 20;

    let room = match rustix::process::getrlimit(rustix::process::Resource::Stack).current {
        Some(stack) => usize::try_from(stack / 4).unwrap_or(ALL).clamp(ONE, ALL),
        None => ALL,
    };
    let pointer = std::mem::size_of::<usize>();
    let environment = env.iter().map(|(key, value)| key.len() + value.len() + 2);
    let arguments = ["sh", "-c", cmd].into_iter().map(|arg| arg.len() + 1);
    let lengths: Vec<usize> = arguments.chain(environment).collect();
    let total: usize = lengths.iter().map(|length| length + pointer).sum();
    let too_long = lengths.iter().any(|length| *length > ONE) || total > room;

    match too_long {
        true => Err(std::io::Error::from_raw_os_error(libc::E2BIG)),
        false => Ok(()),
    }
}

/// A policy that refuses every connection and datagram to an internet address, and lets a unix
/// socket through, for a sandbox nobody is there to ask about.
fn refusing(anywhere: Permissions) -> PolicyFn {
    PolicyFn::new(|event, _| match event.host {
        Some(_) => Verdict::DenyWith(libc::EACCES),
        None => Verdict::Allow,
    })
    .events(&[SyscallCategory::Network])
    .verdict_timeout(None)
    .approvable(anywhere)
}

impl Rules {
    /// A line for each root left out, for the command's standard error.
    pub fn left_out(&self) -> &[String] {
        &self.left_out
    }

    /// Opens `root` without following a link, keeps the descriptor, and names it for sandlock;
    /// `None` where it is left out.
    ///
    /// note: the roots were resolved once, when the session started, and are never followed again.
    /// A root inside a directory a command may write - one beneath the working directory, or the
    /// working directory beneath a `--sandbox-write` path - can be renamed by one command and a
    /// link left at its name, and opened by its path the next command was granted wherever the link
    /// pointed. Such a root is left out and said on standard error, which the model reads. The
    /// `shell-read` paths are opened following links: `/bin` is one on most systems, and one
    /// beneath a root of the session's is left out; see [`Sandbox::sandlock`].
    ///
    /// note: where the kernel has no `openat2` (before 5.6), or a container's seccomp filter refuses
    /// it, the root is opened following links, with the window SECURITY.md describes. A root that is
    /// not there is left out without a word, as Landlock leaves out a rule it cannot build.
    fn hold(&mut self, root: &Path) -> Option<PathBuf> {
        use rustix::{
            fs::{Mode, OFlags, ResolveFlags},
            io::Errno,
        };

        let flags = OFlags::PATH | OFlags::CLOEXEC;
        let opened = match rustix::fs::openat2(
            rustix::fs::CWD,
            root,
            flags,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        ) {
            Err(Errno::NOSYS | Errno::PERM) => rustix::fs::open(root, flags, Mode::empty()),
            Err(Errno::LOOP) => {
                self.left_out.push(format!(
                    "{}: not granted to this command, because a link now stands on the way to it \
                     where there was none when the session started",
                    root.display()
                ));
                return None;
            }
            opened => opened,
        };
        let opened = opened.ok()?;
        let named = std::fs::read_link(fd_path(&opened)).ok()?;
        self.held.push(opened);

        Some(named)
    }
}

/// The name the kernel gives `fd` in this process.
fn fd_path(fd: &OwnedFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

/// How much of the sandbox `sandbox` came to: `Full` where every protection asked for is in force,
/// and `Partial` where the kernel was too old for some of them.
pub fn degraded(sandbox: &sandlock_core::Sandbox) -> Confinement {
    let partial = sandbox
        .active_protections()
        .into_iter()
        .flatten()
        .any(|(_, status)| status == ProtectionStatus::Degraded);

    match partial {
        true => Confinement::Partial,
        false => Confinement::Full,
    }
}

/// The temporary directory given to a confined command: a name of its own under the system's,
/// distinct from every other this process hands out.
///
/// note: made by this process before the command starts, not by the command: `/tmp` is not
/// writable under the ruleset, and the spawning end is the one that removes it. Removing it is the
/// caller's job - see [`crate::tools::Shell`].
pub fn scratch_path() -> PathBuf {
    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    std::env::temp_dir().join(format!("kamchatka-{}-{n}", std::process::id()))
}

/// Makes that directory, and only if this process is the one that made it; `None` if it could not
/// be.
///
/// note: exclusively, never through whatever is already there. The name must be predictable so the
/// spawning process can find it again, but a predictable name in a writable directory is a name
/// somebody else can get to first. `create_dir_all` accepts anything it finds, a symlink included,
/// and the ruleset grants the resolved path everything a writable root gets: a link left in `/tmp`
/// by another account opens whatever it pointed at, and `TMPDIR` sends the command there.
///
/// note: what is already there and ours is different, and commoner, since process identifiers come
/// round again. It is removed and remade, so a command does not inherit the leavings of whatever
/// held the number last. `/tmp` is sticky, so an entry belonging to somebody else cannot be
/// unlinked and the retry fails, leaving the command with no temporary directory rather than
/// theirs.
///
/// note: `0700`, for the same reason [`crate::app::App::write_session`]'s directory is. A command
/// puts whatever it was working on in here, written without anybody asking for it; under the
/// default umask a fresh directory is readable by everyone on the machine. It is made so rather
/// than changed after, since a directory opened while readable can be listed through that handle
/// for as long as it is held.
///
/// note: nothing is said about a failure. The model gets instead: no `TMPDIR`, a `/tmp` that is not
/// writable, and [`Sandbox::note_for`] on the permission error that follows.
///
/// note: opened as soon as it is made, refusing a link at its name, and held, and the sandbox is
/// granted the path the kernel gives back for what is held. sandlock opens that path again in the
/// child, with the window [`Rules`] describes. Nothing a command runs can make an entry in `/tmp`,
/// where the directory is, and another account cannot rename or remove ours there, since `/tmp` is
/// sticky.
pub fn make_scratch(path: &Path) -> Option<Scratch> {
    use std::os::unix::fs::DirBuilderExt as _;

    use rustix::fs::{Mode, OFlags};

    let private = || std::fs::DirBuilder::new().mode(0o700).create(path);
    if private().is_err() {
        // ours from a run whose identifier has come round, or somebody else's; only ours can be
        // unlinked, and `/tmp` being sticky makes that true
        let _ = std::fs::remove_dir_all(path).or_else(|_| std::fs::remove_file(path));
        private().ok()?;
    }
    let held = rustix::fs::open(
        path,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;

    Some(Scratch {
        path: path.to_path_buf(),
        held,
    })
}

/// A temporary directory of a confined command's own: where it is, for `TMPDIR`, and the
/// directory itself, held open from the moment it was made; see [`make_scratch`].
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
    held: OwnedFd,
}

impl Scratch {
    /// Where it is.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// What a probe found here: how much of a sandbox the kernel takes, and whether its network is
/// held for questions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Probed {
    /// How much of the sandbox the kernel took.
    pub confinement: Confinement,
    /// Why it did not take, where it did not and something said why.
    pub why: Option<String>,
    /// Whether a confined command's network can be held while somebody is asked; see
    /// [`crate::tools::Shell`].
    pub gated: bool,
}

/// Checks whether a confinement would hold here, and the network's questions with it, by running
/// `true` under one.
///
/// note: on a thread of its own with a runtime of its own, since sandlock's supervisor is a task
/// and this is asked from inside a runtime and outside one alike. A confined command is a child, so
/// nothing about this process changes.
pub fn available() -> Probed {
    let probe = || -> Result<Confinement, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("the probe could not be run: {e}"))?;
        runtime.block_on(async {
            let sandbox = Sandbox {
                workdir: std::env::temp_dir(),
                writable: Vec::new(),
                readable: Vec::new(),
                shell_readable: SYSTEM.iter().map(PathBuf::from).collect(),
                workdir_writable: true,
                network: Network::Shut,
                devices: DEVICES.iter().map(PathBuf::from).collect(),
                closed: Vec::new(),
            };
            let (builder, rules) = sandbox.sandlock(None, None, false);
            let (builder, argv) = command(builder, super::environment(), "true");
            let mut confined = builder.build().map_err(|e| e.to_string())?;
            let took = degraded(&confined);
            let ran = confined.run(&argv).await.map_err(|e| e.to_string());
            drop(rules);
            match ran?.success() {
                true => Ok(took),
                false => Err("the probe's command did not run".to_owned()),
            }
        })
    };
    let probed = std::thread::scope(|scope| {
        scope
            .spawn(probe)
            .join()
            .unwrap_or_else(|_| Err("the probe panicked".to_owned()))
    });

    match probed {
        Ok(confinement) => Probed {
            confinement,
            why: None,
            gated: true,
        },
        Err(why) => Probed {
            confinement: Confinement::Unavailable,
            why: Some(why),
            gated: false,
        },
    }
}

/// Confines a command and runs it, if this program was asked to from the command line; returns
/// the exit code it should leave with.
///
/// note: checked before anything else in `main`, and before a `tokio` runtime exists; it makes its
/// own, since this process is the command's supervisor for as long as the command runs.
///
/// note: nobody is there to ask about the network, so a network held for a question is refused
/// as a shut one is.
///
/// note: the caller's environment is passed on as it is, API keys included, unlike the `shell`
/// tool's. There is no model here to keep them from: whoever runs this chose the command and its
/// environment, and one wrapped around a whole agent needs that agent's key.
///
/// note: what the command leaves running is stopped when it ends. The supervisor is this process,
/// and a job outliving it would have its next held call refused by a kernel with nobody left to
/// answer.
pub fn run_if_asked() -> Option<i32> {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let (sandbox, cmd) = Sandbox::from_argv(&argv)?;
    let in_a_session = argv.get(1).is_some_and(|word| word == SESSION_FLAG);

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("nothing was run: the sandbox's supervisor could not start: {e}");
            return Some(126);
        }
    };

    // a temporary directory for this run, made before anything is restricted and handed to the
    // command as `TMPDIR`; removed again once it is over
    let scratch = make_scratch(&scratch_path());
    let mut env: Vec<(String, String)> = super::environment()
        .filter(|(key, _)| key != "TMPDIR")
        .collect();
    if let Some(scratch) = &scratch {
        env.push(("TMPDIR".to_owned(), scratch.path().display().to_string()));
    }
    // note: a sandbox is in force for the command, and that is the only case this is for.
    // Unconfined, git can read its own configuration, and pointing it elsewhere would take a
    // person's identity and aliases away for nothing
    if let Some(global) = sandbox.git_config_global() {
        env.retain(|(key, _)| key != "GIT_CONFIG_GLOBAL");
        env.push(("GIT_CONFIG_GLOBAL".to_owned(), global.display().to_string()));
    }

    let (builder, rules) = sandbox.sandlock(scratch.as_ref(), None, !in_a_session);
    let cmd = cmd.to_string_lossy();
    let (builder, argv) = command(builder, env, &cmd);
    for line in rules.left_out() {
        eprintln!("{line}");
    }
    // note: asked to confine and to run, in that order, so a sandbox that did not take leaves
    // nothing to do. Running the command anyway would run it unconfined, with the whole filesystem
    // and network, and with nothing saying so. `Confinement` exists to make that sayable
    let code = runtime.block_on(async {
        let mut confined = match builder.build() {
            Ok(confined) => confined,
            Err(e) => {
                eprintln!(
                    "nothing was run: this program was asked to confine the command first and the \
                     sandbox did not take: {e}"
                );
                return 126;
            }
        };
        let started = confined
            .popen(
                &argv,
                StdioMode::Inherit,
                StdioMode::Inherit,
                StdioMode::Inherit,
            )
            .await;
        let mut started = match started {
            Ok(started) => started,
            Err(e) => {
                eprintln!(
                    "nothing was run: this program was asked to confine the command first and the \
                     sandbox did not take: {e}"
                );
                return 126;
            }
        };
        let waited = started.wait().await;
        let _ = started.shutdown().await;
        match waited {
            Ok(sandlock_core::ExitStatus::Code(code)) => code,
            Ok(sandlock_core::ExitStatus::Signal(signal)) => 128 + signal,
            Ok(_) => 137,
            Err(e) => {
                eprintln!("could not run the command: {e}");
                127
            }
        }
    });
    drop(rules);
    if let Some(scratch) = scratch {
        let _ = std::fs::remove_dir_all(scratch.path());
    }

    Some(code)
}

#[cfg(test)]
mod weaker {
    use super::*;

    /// Each thing a kernel leaves out is named with the version that brings it, and a kernel that
    /// leaves out nothing says nothing.
    #[test]
    fn a_kernel_is_told_what_its_sandbox_leaves_out() {
        let all = Kernel {
            landlock: true,
            abstract_sockets: true,
            signals: true,
            scope_refused: false,
        };
        assert_eq!(weaker(all), None);

        // Linux 6.8: nothing from 6.12
        let said = weaker(Kernel {
            abstract_sockets: false,
            signals: false,
            ..all
        })
        .expect("a warning");
        assert!(
            said.contains("abstract unix sockets") && said.contains("6.12"),
            "{said}"
        );
        assert!(said.contains("signals"), "{said}");

        // and no Landlock at all is the one thing said
        let said = weaker(Kernel {
            landlock: false,
            ..all
        })
        .expect("a warning");
        assert!(
            said.contains("unconfined") && !said.contains("6.12"),
            "{said}"
        );

        // a kernel that has the signal scope and refused it is said to have, and not to be old
        let said = weaker(Kernel {
            scope_refused: true,
            ..all
        })
        .expect("a warning");
        assert!(
            said.contains("would not put this session") && !said.contains("newer kernel"),
            "{said}"
        );
        // and beside what an older kernel leaves out, both are said
        let said = weaker(Kernel {
            abstract_sockets: false,
            scope_refused: true,
            ..all
        })
        .expect("a warning");
        assert!(
            said.contains("abstract") && said.contains("would not put this session"),
            "{said}"
        );
    }

    /// `weaker_here` says nothing or one of `weaker`'s sentences.
    ///
    /// note: which of them depends on the machine, so this holds it to being a sentence about the
    /// kernel and nothing else.
    #[test]
    fn weaker_here_says_nothing_or_a_sentence_about_this_kernel() {
        if let Some(said) = weaker_here() {
            assert!(
                said.starts_with("this kernel") || said.starts_with("the kernel would not"),
                "{said}"
            );
        }
    }
}

#[cfg(test)]
mod asked {
    use super::*;

    /// Only a confinement that restricts something is a confinement.
    ///
    /// note: what three callers read, and none of them can be asked about it afterwards.
    /// `run_if_asked` runs nothing at all where the sandbox would not build, rather than running
    /// the command with the whole filesystem and the network and a promise on the screen; the
    /// startup probe reports `gated` as `false` without it; and `Setup::wire` confines `shell` only
    /// where this says yes. `Partial` is a yes, because something is restricted even where
    /// something is not - what did not take is named rather than granted.
    #[test]
    fn only_a_ruleset_in_force_is_a_confinement() {
        for (confinement, confined) in [
            (Confinement::Full, true),
            (Confinement::Partial, true),
            (Confinement::Unavailable, false),
            (Confinement::Off, false),
        ] {
            assert_eq!(
                confinement.is_confined(),
                confined,
                "{confinement} ({confinement:?})"
            );
        }
    }

    /// The scratch directories handed out are all different, so two commands running at once
    /// never share one.
    #[test]
    fn every_scratch_directory_is_its_own() {
        assert_ne!(scratch_path(), scratch_path());
    }
}
