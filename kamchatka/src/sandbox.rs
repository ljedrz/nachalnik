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

use crate::tools::Careful;

/// The argument that puts this program into the mode that confines itself and runs a command.
pub const EXEC_FLAG: &str = "--confine-and-run";

/// What the `shell` tool is allowed to reach.
///
/// note: read is not a stance here. A command that cannot read `/usr/bin` cannot run at all, so
/// the system directories are always readable and the interesting question is what is *writable*
/// and whether the network is reachable - which are the two stances a person actually changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    /// The directory a command may work in; everything outside it is out of reach.
    pub workdir: PathBuf,
    /// Extra paths the user asked for, read-write.
    pub extra: Vec<PathBuf>,
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
    pub fn of(policy: &Careful, workdir: PathBuf, extra: Vec<PathBuf>, granted: bool) -> Self {
        Self {
            workdir,
            extra,
            // a refusal of `write` reaches the shell too; anything short of a refusal leaves the
            // working directory writable, because a shell that cannot write in it is not one
            // anybody can work with
            writable: policy.stance(&Capability::Write) != Verdict::Deny,
            network: granted || policy.stance(&Capability::Network) == Verdict::Allow,
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
        let cmd = argv.next()?.clone();

        Some((
            Self {
                workdir,
                extra,
                writable,
                network,
            },
            cmd,
        ))
    }
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
        write!(
            f,
            ", the system directories read-only, {}",
            match self.network {
                true => "the network reachable",
                false => "no network",
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
    /// Extra paths the user opened up.
    pub extra: Vec<PathBuf>,
    /// Whether to hold them to it at all; `--no-sandbox` turns this off.
    pub confined: bool,
}

impl Reach {
    /// Returns the path to use, or what to tell the model instead.
    ///
    /// note: resolved before it is compared, so that `../` and a symlink out are the same question
    /// as a plain absolute path. A path that does not exist yet - which is most of what `write` is
    /// handed - is resolved through its parent, because a file cannot be created outside a
    /// directory it is not in.
    pub fn allows(&self, path: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(path);
        if !self.confined {
            return Ok(path);
        }

        let absolute = match path.is_absolute() {
            true => path.clone(),
            false => self.workdir.join(&path),
        };
        // the deepest part that exists, plus whatever is left over: a file about to be created has
        // no canonical form of its own
        let mut existing = absolute.as_path();
        let mut rest = PathBuf::new();
        let resolved = loop {
            match existing.canonicalize() {
                Ok(resolved) => break resolved.join(&rest),
                Err(_) => match (existing.file_name(), existing.parent()) {
                    (Some(name), Some(parent)) => {
                        rest = PathBuf::from(name).join(&rest);
                        existing = parent;
                    }
                    _ => break absolute.clone(),
                },
            }
        };

        match std::iter::once(&self.workdir)
            .chain(self.extra.iter())
            .any(|allowed| {
                allowed
                    .canonicalize()
                    .is_ok_and(|allowed| resolved.starts_with(allowed))
            }) {
            true => Ok(resolved),
            false => Err(format!(
                "{}: outside {}, which is as far as this agent reaches. Start it elsewhere, or                  pass --sandbox-allow",
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
#[cfg(target_os = "linux")]
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
/// note: `/dev` gets reading and writing of files and nothing else, because `/dev/null` is not
/// optional and creating things in `/dev` is not something a shell command needs to do.
#[cfg(target_os = "linux")]
pub fn confine(sandbox: &Sandbox, scratch: &Path) -> Confinement {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
        path_beneath_rules,
    };

    let abi = ABI::V1;
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
        .chain(std::iter::once(scratch.to_path_buf()))
        .chain(sandbox.extra.iter().cloned())
        .collect();
    let readable: Vec<PathBuf> = SYSTEM
        .iter()
        .map(PathBuf::from)
        .chain(std::iter::once(sandbox.workdir.clone()))
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
pub fn confine(_sandbox: &Sandbox, _scratch: &Path) -> Confinement {
    Confinement::Unsupported
}

/// Whether a confinement would hold here, asked without running anything.
///
/// note: asked in a child, because finding out means applying a ruleset and a process cannot take
/// one off again. Doing it in the terminal's own process would confine the terminal.
pub fn available(program: &Path) -> Confinement {
    let sandbox = Sandbox {
        workdir: std::env::temp_dir(),
        extra: Vec::new(),
        writable: true,
        network: false,
    };
    let output = std::process::Command::new(program)
        .args(sandbox.argv("exit 0"))
        .env(REPORT_VAR, "1")
        .output();

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
pub fn run_if_asked() -> Option<i32> {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let (sandbox, cmd) = Sandbox::from_argv(&argv)?;

    // a temporary directory of this run's own, made before anything is restricted and handed to
    // the command as `TMPDIR`; see the note on `confine`
    let scratch = std::env::temp_dir().join(format!("kamchatka-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);

    let confinement = confine(&sandbox, &scratch);
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

    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(&sandbox.workdir)
        .env("TMPDIR", &scratch)
        .status();
    let _ = std::fs::remove_dir_all(&scratch);

    Some(match status {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("could not run the command: {e}");
            127
        }
    })
}
