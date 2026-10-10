//! How much of a [`Sandbox`] the kernel takes, and the child that takes it: the Landlock probes,
//! the ruleset, the scratch directory a confined command writes into, and the mode this program
//! re-executes itself in to confine a command and run it. The notes at the top of
//! [the module](crate::sandbox) say why it is shaped this way.

use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};

use super::{DEVICES, Network, SESSION_FLAG, SYSTEM, Sandbox, device};

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

/// Whether this kernel refuses a confined command a connection to an abstract unix socket made
/// outside its confinement.
///
/// note: an abstract socket has no path, so [`confines_unix_sockets`]'s right does not reach it,
/// and a desktop listens on them: the X server's `@/tmp/.X11-unix/X0` takes a connection from
/// any process of the user's with no cookie, and a connection to it can type into any window -
/// the person's terminal included. Landlock scopes it from ABI 6, which is Linux 6.12, and the
/// question is put to the kernel the way that one is.
pub fn confines_abstract_sockets() -> bool {
    use landlock::{CompatLevel, Compatible, Ruleset, RulesetAttr, Scope};

    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .scope(Scope::AbstractUnixSocket)
        .is_ok()
}

/// Whether this kernel can refuse a process a signal to anything outside its Landlock domain.
///
/// note: put to the kernel through the crate, as [`confines_abstract_sockets`] is, and applying
/// nothing: the scope is ABI 6, which is Linux 6.12.
pub fn confines_signals() -> bool {
    use landlock::{CompatLevel, Compatible, Ruleset, RulesetAttr, Scope};

    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .scope(Scope::Signal)
        .is_ok()
}

/// What this kernel leaves out of a confined command's sandbox, in a sentence for the person; `None`
/// where it leaves out nothing this program asks it for.
///
/// note: said at startup rather than only on the permissions tab, because what is missing is the
/// kernel's age and the tab draws a sandbox that took as "confined" either way. Not worked around:
/// every gap here is closed by a kernel that is a year or more old, and a check of its own for each
/// would be this program re-implementing what Landlock does.
///
/// note: three questions put to the kernel the way the others here are, applying nothing. The
/// socket files Landlock covers from Linux 7.1 are not among them, since a kernel that recent is
/// still the exception, and a warning most people get is one nobody reads.
pub fn weaker_here() -> Option<String> {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, Ruleset, RulesetAttr,
    };

    let hard = || Ruleset::default().set_compatibility(CompatLevel::HardRequirement);
    weaker(Kernel {
        landlock: hard().handle_access(AccessFs::from_all(ABI::V1)).is_ok(),
        tcp: hard().handle_access(AccessNet::ConnectTcp).is_ok(),
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
    tcp: bool,
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
            kernel.tcp,
            "TCP connections, apart from the network gate's questions, and the ports a served \
             session listens on (Linux 6.7)",
        ),
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

/// Puts this process in a Landlock domain that refuses a signal to anything outside it, so that
/// everything it starts from here on - every command, and what a command leaves running - may
/// signal this process and what it started, and nothing else; whether the kernel took it.
///
/// note: on this process rather than in a command's ruleset, because the scope is checked per
/// layer: a command's own layer is its alone, and a scope there refuses it the jobs an earlier call
/// left running, which the next call stopping is how a server started in the background is ever
/// stopped. A layer all of them inherit is the session's boundary instead. `kill -9 -1` from a
/// command was every process the person has, this program among them.
///
/// note: before the runtime exists, since Landlock binds the calling thread and the threads it
/// starts afterwards - a command spawned from a worker thread made earlier would be outside the
/// layer - and before anything is spawned, since this process may not signal what it started
/// before it, and it stops its MCP servers by signal. It costs `no_new_privs` on everything this
/// process starts, which is why the caller does not ask for it under `--no-sandbox`: a command
/// there, or an MCP server anywhere, gains no privileges through a set-user-ID program such as
/// `sudo`.
///
/// note: `HardRequirement`, so a kernel without the scope - below Linux 6.12 - is a `false` and
/// nothing applied, rather than a ruleset that restricts nothing and still costs the privileges.
///
/// note: `Refer` handled and granted beneath `/`, because it is the one right a layer refuses
/// without handling it: a layer of signals alone made every rename and link from one directory to
/// another `EXDEV`, in every command and in this process. A compiler writes its output in one
/// directory and renames it into another, so nothing could be built. Granted everywhere, it is
/// the same right on both sides of any move, and the kernel's other condition - that a file gains
/// no rights by moving - is the command's own ruleset to decide.
pub fn scope_signals() -> bool {
    use landlock::{
        AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr, Scope,
        path_beneath_rules,
    };

    if !confines_signals() {
        return false;
    }
    let scoped = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::Refer)
        .and_then(|ruleset| ruleset.scope(Scope::Signal))
        .and_then(|ruleset| ruleset.create())
        .and_then(|created| created.add_rules(path_beneath_rules(["/"], AccessFs::Refer)))
        .and_then(|created| created.restrict_self())
        .is_ok();
    // a kernel that has the scope and would not apply it is a sandbox weaker than the one it says
    // it is, and `weaker_here` says so
    if !scoped {
        SCOPE_REFUSED.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    scoped
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
    confine_saying(sandbox, scratch).0
}

/// The same, with why it did not take where it did not.
///
/// note: `Unavailable` covers a kernel with no Landlock and a ruleset call that failed, and the
/// error of the second was dropped - so the child said the sandbox did not take and not why, and a
/// person with a kernel that has Landlock was left to guess what refused it. The reason travels
/// beside the value rather than in it, because `Confinement` is a `Copy` answer matched all over
/// this program, and the reason is for saying rather than for deciding.
pub fn confine_saying(sandbox: &Sandbox, scratch: Option<&Path>) -> (Confinement, Option<String>) {
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
    let mut ruleset = match Ruleset::default().handle_access(rights) {
        Ok(ruleset) => ruleset,
        Err(e) => return unavailable(format!("the filesystem rights were refused: {e}")),
    };
    // note: asked for where the kernel has it and nowhere else, for the reason `ResolveUnix` is.
    // Not `Scope::Signal` beside it: every command confines itself in a domain of its own, so a
    // command could no longer stop a server an earlier call left running. That scope is on the
    // layer every command shares; see `scope_signals`
    if confines_abstract_sockets() {
        match ruleset.scope(landlock::Scope::AbstractUnixSocket) {
            Ok(scoped) => ruleset = scoped,
            Err(e) => return unavailable(format!("the abstract socket scope was refused: {e}")),
        }
    }
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
            Err(e) => return unavailable(format!("the network rights were refused: {e}")),
        }
    }
    // note: a port is closed by granting every other one, since a ruleset only ever grants. That is
    // a rule a port, built in a few tens of milliseconds, and paid only by a command confined while
    // a session is served over TCP. `BindTcp` stays unhandled: listening refuses nobody's answer.
    // Never together with the refusal above - `closing` is false whenever TCP is refused outright -
    // which is the only reason `ConnectTcp` can be handled here as well as there
    let closing = !sandbox.network.refuses_tcp() && !sandbox.closed.is_empty();
    if closing {
        match ruleset.handle_access(AccessNet::ConnectTcp) {
            Ok(with_net) => ruleset = with_net,
            Err(e) => return unavailable(format!("the closed ports were refused: {e}")),
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
            RulesetStatus::FullyEnforced => (Confinement::Full, None),
            RulesetStatus::PartiallyEnforced => (Confinement::Partial, None),
            RulesetStatus::NotEnforced => {
                unavailable("the kernel enforces no Landlock ruleset".to_owned())
            }
        },
        Err(e) => unavailable(format!("the ruleset could not be applied: {e}")),
    }
}

/// A confinement that did not take, and why.
fn unavailable(why: String) -> (Confinement, Option<String>) {
    (Confinement::Unavailable, Some(why))
}

/// The line [`run_if_asked`] writes for [`available`] to read: how much took, whether the gate
/// holds, and why the sandbox did not take where it did not.
///
/// note: the reason goes last, after `; `, because it is the kernel's words and may hold spaces;
/// the two before it are single words. One function each way, so the two ends cannot drift.
fn report_line(confinement: Confinement, gated: bool, why: Option<&str>) -> String {
    let took = match confinement {
        Confinement::Full => "full",
        Confinement::Partial => "partial",
        Confinement::Unavailable | Confinement::Off => "unavailable",
    };
    let gate = match gated {
        true => " gated",
        false => "",
    };
    let why = why
        .map(|why| format!("; {}", why.replace('\n', " ")))
        .unwrap_or_default();

    format!("{REPORT}{took}{gate}{why}")
}

/// What [`report_line`] wrote, read back from after its prefix.
fn read_report(reported: &str) -> (Confinement, bool, Option<String>) {
    let (words, why) = match reported.split_once("; ") {
        Some((words, why)) => (words, Some(why.to_owned())),
        None => (reported, None),
    };
    let (took, gate) = words.split_once(' ').unwrap_or((words, ""));
    let confinement = match took {
        "full" => Confinement::Full,
        "partial" => Confinement::Partial,
        _ => Confinement::Unavailable,
    };

    (confinement, gate == "gated", why)
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
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Probed {
    /// How much of the ruleset the kernel took.
    pub confinement: Confinement,
    /// Why it did not take, where it did not and something said why: the kernel's refusal, or a
    /// probe that could not be run at all.
    pub why: Option<String>,
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

    let (confinement, gate, why) = match output {
        Err(e) => (
            Confinement::Unavailable,
            false,
            Some(format!("the probe could not be run: {e}")),
        ),
        Ok(output) => match String::from_utf8_lossy(&output.stderr)
            .lines()
            .find_map(|line| line.strip_prefix(REPORT).map(str::to_owned))
        {
            Some(reported) => read_report(&reported),
            None => (
                Confinement::Unavailable,
                false,
                Some("the probe said nothing about the sandbox".to_owned()),
            ),
        },
    };

    Probed {
        confinement,
        why,
        gated: confinement.is_confined() && gate && crate::gate::holds(),
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

    let (confinement, why) = confine_saying(&sandbox, scratch.as_deref());
    // note: a layer of its own, and only where no session's is above; see `SESSION_FLAG`. Taken
    // where the kernel has it and gone without where it does not, as the session's is in `main`
    if argv.get(1).is_none_or(|word| word != SESSION_FLAG) {
        scope_signals();
    }
    // note: after the ruleset, so that nothing the gate does is outside it, and before the command,
    // which inherits the filter across `exec` and into everything it starts
    let gated = match sandbox.network {
        Network::Shut | Network::Asked => Some(crate::gate::hold()),
        Network::Open | Network::NoTcp => None,
    };
    if std::env::var_os(REPORT_VAR).is_some() {
        eprintln!(
            "{}",
            report_line(confinement, matches!(gated, Some(Ok(()))), why.as_deref())
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
             did not take{}",
            why.map(|why| format!(": {why}")).unwrap_or_default()
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

#[cfg(test)]
mod weaker {
    use super::*;

    /// Each thing a kernel leaves out is named with the version that brings it, and a kernel that
    /// leaves out nothing says nothing.
    #[test]
    fn a_kernel_is_told_what_its_sandbox_leaves_out() {
        let all = Kernel {
            landlock: true,
            tcp: true,
            abstract_sockets: true,
            signals: true,
            scope_refused: false,
        };
        assert_eq!(weaker(all), None);

        // Linux 6.8: TCP, but nothing from 6.12
        let said = weaker(Kernel {
            abstract_sockets: false,
            signals: false,
            ..all
        })
        .expect("a warning");
        assert!(!said.contains("TCP"), "{said}");
        assert!(
            said.contains("abstract unix sockets") && said.contains("6.12"),
            "{said}"
        );
        assert!(said.contains("signals"), "{said}");

        // Linux 6.1: none of the three
        let said = weaker(Kernel {
            tcp: false,
            abstract_sockets: false,
            signals: false,
            ..all
        })
        .expect("a warning");
        assert!(said.contains("TCP") && said.contains("6.7"), "{said}");
        assert!(said.contains("served"), "the ports it cannot close: {said}");

        // and no Landlock at all is the one thing said
        let said = weaker(Kernel {
            landlock: false,
            ..all
        })
        .expect("a warning");
        assert!(
            said.contains("unconfined") && !said.contains("TCP"),
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
            tcp: false,
            scope_refused: true,
            ..all
        })
        .expect("a warning");
        assert!(
            said.contains("TCP") && said.contains("would not put this session"),
            "{said}"
        );
    }
}

#[cfg(test)]
mod report {
    use super::*;

    /// What the child says about its sandbox is what the probe reads, the reason included.
    ///
    /// note: the reason was dropped, so a probe that found no confinement could not say whether
    /// the kernel had no Landlock or refused this ruleset. It is the kernel's words, so the case
    /// that matters is one with spaces - and a newline, which would have ended the line early.
    #[test]
    fn the_report_carries_why_a_sandbox_did_not_take() {
        for (confinement, gated, why) in [
            (Confinement::Full, true, None),
            (Confinement::Partial, false, None),
            (
                Confinement::Unavailable,
                false,
                Some("the ruleset could not be applied: Operation not permitted"),
            ),
        ] {
            let line = report_line(confinement, gated, why);
            let reported = line.strip_prefix(REPORT).expect("the prefix");
            assert_eq!(
                read_report(reported),
                (confinement, gated, why.map(str::to_owned)),
                "{line}"
            );
        }

        let line = report_line(Confinement::Unavailable, false, Some("one\ntwo"));
        assert!(!line.contains('\n'), "{line:?}");
        let reported = line.strip_prefix(REPORT).expect("the prefix");
        assert_eq!(read_report(reported).2.as_deref(), Some("one two"));
    }
}

#[cfg(test)]
mod asked {
    use super::*;
    use landlock::{AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr};

    /// Whether this kernel has the filesystem right a confined command's ruleset would need to
    /// refuse a pathname socket it could not write.
    ///
    /// note: asked by building the ruleset, which is the question [`confines_unix_sockets`]
    /// itself asks, and the two are compared rather than one being read off the other: a test
    /// that took the function's own answer for the kernel's would pass on every kernel, and this
    /// one has to fail when the function stops agreeing. `HardRequirement` is the level at which a
    /// right the kernel does not have is an error rather than something quietly dropped.
    fn the_kernel_has(rights: landlock::BitFlags<AccessFs>) -> bool {
        // note: and not applied. Building it is the whole of what the question is, and a ruleset
        // applied here would confine every test in this binary
        Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(rights)
            .and_then(|ruleset| ruleset.create())
            .is_ok()
    }

    /// A pathname socket is said to be confined where the kernel has the right to refuse one.
    ///
    /// note: ABI 9, asked of the kernel rather than read off a version; the two scopes beside it
    /// are held to the kernel in `tests/boundary.rs`.
    #[test]
    fn a_unix_socket_is_confined_where_the_kernel_has_the_right() {
        assert_eq!(
            confines_unix_sockets(),
            the_kernel_has(AccessFs::ResolveUnix.into()),
            "a pathname socket"
        );
    }

    /// Only a confinement that restricts something is a confinement.
    ///
    /// note: what three callers read, and none of them can be asked about it afterwards.
    /// `run_if_asked` runs nothing at all where this says no, rather than running the command
    /// with the whole filesystem and the network and a promise on the screen; the startup probe
    /// reports `gated` as `false` without it, so a session whose sandbox did not take does not
    /// go on reading command lines for program names; and `Setup::wire` hands `shell` a confiner
    /// only where this says yes. `Partial` is a yes, because something is restricted even where
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
}
