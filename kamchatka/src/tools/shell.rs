//! Running a command, and stopping one.
//!
//! note: the tool that shows what an [`OutputSink`] is for - every line reaches the screen as it
//! is read, so a command that takes a minute is visible for that minute rather than arriving all
//! at once - and the one that runs under a real confinement, built from the stances the policy
//! has accumulated rather than from the verdict on this one call.

use std::{collections::BTreeMap, path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use nachalnik::{
    BoxError, Capability, OutputSink, Tool, ToolCall, ToolCallId, ToolOutput, ToolSpec, Verdict,
    async_trait,
};

use crate::sandbox::{Network, Sandbox};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::tools::{
    Careful, KEPT, Limits, arg,
    ops::{self, Arg, Layout, Op, inner, schema},
};

/// How long a running command may say nothing before the tool looks up to check whether it has
/// been asked to stop.
const HEARTBEAT: Duration = Duration::from_millis(120);

/// What the first line of a shell result says happened.
///
/// note: the line is written here, not where it is drawn; [`Shell::invoke`] is the only thing that
/// produces one. A colour worked out elsewhere from a string it does not own drifts when the
/// wording changes.
///
/// note: three variants, not one per shape, because a colour answers a coarse question: did the
/// command say it worked, say it failed, or never get to say. `1` from `grep` meaning no match is
/// not singled out, because telling that from a fault means knowing the command, and this program
/// would be guessing; a guess that colours a working pipeline red, or a real failure yellow, is
/// worse than the number itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The command finished and reported success.
    Ok,
    /// The command finished and reported a failure.
    Failed,
    /// The command never got to report: stopped by an interrupt, killed by a signal, or
    /// a status that could not be read at all.
    Stopped,
}

impl Exit {
    /// Reads one back off a line, if that line is a shell result's first one.
    ///
    /// note: the number decides it, not the words after it. `exit: ` is followed by an exit code or
    /// by a reason no command reported - `stopped`, `signal: 9 (SIGKILL)`, `unknown` - so a token
    /// that parses is an exit code and one that does not is a command that never got to report.
    /// The prose can be rewritten without this having to be.
    pub fn of(line: &str) -> Option<Self> {
        let said = line.strip_prefix("exit: ")?;
        let code = said.split_whitespace().next()?;

        Some(match code.parse::<i32>() {
            Ok(0) => Self::Ok,
            Ok(_) => Self::Failed,
            Err(_) => Self::Stopped,
        })
    }
}

/// Top-level joints of a command line - `|`, `||`, `&&`, `;` - as byte ranges; empty when there are
/// none or the scan cannot be trusted.
///
/// note: here rather than beside the panel that colours them, because a joint is a fact about the
/// command, not about drawing it, and `ui` is behind feature `tui`, so headless sessions and
/// `tools` that read a command before it runs could not ask.
///
/// note: ranges rather than a rewritten command. Breaking the line at each joint costs a row per
/// stage on a panel whose rows are scarce, and `sh` would not take back a broken command, so the
/// panel would show a spelling nobody could act on. Colouring the joints in place says the same
/// thing at a glance, in the command as the model wrote it.
///
/// note: worked out here rather than from the highlighter. `synoptic`'s `sh` mode calls every
/// flag's hyphen an operator - `-n`, `-u`, `-5` - and does not tokenise `|` or `;`, so painting its
/// operators would colour noise and miss the joints.
///
/// note: quote-aware, and it gives up rather than guessing.
/// [`reaches_the_network`](crate::tools::reaches_the_network) splits on these same characters
/// wherever they are, because a policy that over-reads a command only asks a question it need not
/// have, which is the right way for a policy to be wrong. A panel that over-reads tells somebody a
/// quoted `|` is a pipe on the screen where they decide whether to run it. So an unterminated
/// quote, an unclosed `$(` or a trailing backslash comes back empty and the command is drawn with
/// nothing picked out.
///
/// note: a command that already has newlines is left alone without being scanned. What is inside a
/// heredoc is arbitrary text - the `|` in the middle of a Python string is not a joint, and nothing
/// readable from one line says so.
pub fn joints(cmd: &str) -> Vec<(usize, usize)> {
    if cmd.contains('\n') {
        return Vec::new();
    }

    let mut found: Vec<(usize, usize)> = Vec::new();

    // note: bytes rather than characters, which is safe because everything looked at here is ASCII:
    // every byte of a multi-byte character is `0x80` or above, so it matches none of them, and
    // every index taken is one of theirs
    let bytes = cmd.as_bytes();
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if escaped {
            escaped = false;
            at += 1;
            continue;
        }
        match quote {
            // nothing inside these is an escape, not even a backslash
            Some(b'\'') => {
                if byte == b'\'' {
                    quote = None;
                }
                at += 1;
                continue;
            }
            Some(mark) => {
                match byte {
                    b'\\' => escaped = true,
                    it if it == mark => quote = None,
                    _ => {}
                }
                at += 1;
                continue;
            }
            None => {}
        }

        match byte {
            b'\\' => {
                escaped = true;
                at += 1;
                continue;
            }
            // a backtick closes with the character it opened with, so it counts here as a quote
            // rather than as a depth
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                at += 1;
                continue;
            }
            b'(' => {
                depth += 1;
                at += 1;
                continue;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                at += 1;
                continue;
            }
            _ => {}
        }
        // a joint inside `$(…)` joins the inner command's stages, not this one's
        if depth != 0 || !matches!(byte, b'|' | b'&' | b';') {
            at += 1;
            continue;
        }

        let twice = bytes.get(at + 1) == Some(&byte);
        let width = match (byte, twice) {
            // a lone `&` is a job put in the background, and it is also the `&` of `2>&1`;
            // neither is a seam worth picking out
            (b'&', false) => {
                at += 1;
                continue;
            }
            // and `;;` ends a `case` arm rather than separating two commands
            (b';', true) => {
                at += 2;
                continue;
            }
            (_, true) => 2,
            (_, false) => 1,
        };

        found.push((at, at + width));
        at += width;
    }

    // a scan that ended in the middle of something did not understand the command, and a command
    // this cannot read is one it picks nothing out of
    match quote.is_some() || escaped || depth != 0 {
        true => Vec::new(),
        false => found,
    }
}

/// Runs a command, reporting its output as it arrives and stopping when asked to, under the
/// policy's confinement.
///
/// note: every line goes to [`OutputSink`] as it is read, so a command that takes a minute is
/// visible for that minute instead of appearing all at once at the end. Between lines it checks
/// whether escape was pressed; if so, the child is killed and the call answers with what it got.
///
/// note: it holds the policy rather than being handed a verdict, because the kernel only answers
/// whether the call may run at all. What it may reach is a separate question that the policy
/// answers: [`Sandbox::of`](crate::sandbox::Sandbox::of).
///
/// note: the paths are given resolved, as `Setup::wire` gives them. A confined command is granted
/// each only while no link stands on the way to it, for the reason [`Reach`](crate::sandbox::Reach)
/// gives, so one given through a link is granted nothing.
pub struct Shell {
    /// The stances the confinement is built from.
    pub policy: Arc<Careful>,
    /// The directory a command may work in.
    pub workdir: PathBuf,
    /// Paths outside the working directory the user asked to open up, read-write.
    pub writable: Vec<PathBuf>,
    /// Paths outside the working directory the user asked to open up for reading only.
    pub readable: Vec<PathBuf>,
    /// Paths a confined command may read that `fs` may not; see
    /// [`SYSTEM`](crate::sandbox::SYSTEM) for the usual ones.
    pub shell_readable: Vec<PathBuf>,
    /// The devices under `/dev` a confined command may read and write; see
    /// [`DEVICES`](crate::sandbox::DEVICES) for the usual ones.
    pub devices: Vec<PathBuf>,
    /// Whether a command runs in a sandlock sandbox; `false` runs `sh` directly.
    ///
    /// note: settled once at startup, by a probe that ran a command in one - see
    /// [`crate::sandbox::available`] - rather than tried per call. A kernel that would not start
    /// one gives a `shell` that runs unconfined and a permissions view that says so, not a shell
    /// whose every command fails with an error nobody can account for.
    pub confined: bool,
    /// How much of a command's output the model is shown.
    ///
    /// note: on the struct rather than assigned by [`super::builtin`], because this is a public
    /// type that somebody can construct and register directly; a limit set by one route is a limit
    /// `/limit` silently fails to change on the other.
    pub limits: Limits,
    /// What a command left running when its call ended, for whoever ends the session to stop;
    /// see [`Stragglers`].
    pub stragglers: Stragglers,
    /// How the schema lays out the call's arguments, and so how a refusal tells a model to write
    /// them.
    pub layout: Layout,
}

/// What the commands of this session left running when their calls ended.
///
/// note: `sleep 300 &`, a server, a watcher - a job put in the background survives the call, and it
/// once survived the program too, reparented to init with nothing left that answers its network
/// questions or records what it does. The call says so while the session lasts; this makes the end
/// of the session say it, and stop them - [`Stragglers::stop`] - unless somebody asked to leave
/// them running.
///
/// note: a command's processes are known by a mark, not by their process group. Every command is
/// started with [`CALL_VAR`] in its environment, naming this session and the call, and everything
/// it starts inherits it, so a job that left the group is found as surely as one that stayed in it:
/// `setsid sleep 300 &`, a daemon's double fork, `nohup`. A model asked for a job that outlives its
/// call wraps it in `setsid` as often as not, and found by group, that job outlived the session
/// unnamed. What escapes is what clears its own environment - `env -i`, a command saying it wants
/// nothing of where it came from - and what is not this user's to read.
///
/// note: by the mark, not by a number remembered. A process identifier is handed out again once its
/// process is gone, and a number written down when a call ended can be somebody else's process by
/// the time the session ends. The processes are looked for when they are about to be stopped, each
/// one signalled through a `pidfd` opened on it and read again through `/proc` after, so that what
/// is signalled is a process that carries the mark.
///
/// note: shared, since the tool marks and whoever ends the session stops; a clone is the same.
#[derive(Clone)]
pub struct Stragglers(Arc<Marks>);

/// This session's mark, and the commands it has been handed, by call.
struct Marks {
    session: String,
    calls: parking_lot::Mutex<Vec<String>>,
}

impl Default for Stragglers {
    /// A session of its own: a mark no other process carries.
    fn default() -> Self {
        static SESSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let n = SESSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Self(Arc::new(Marks {
            session: format!("{}-{since}-{n}", std::process::id()),
            calls: parking_lot::Mutex::new(Vec::new()),
        }))
    }
}

/// The variable a command's processes carry, naming the session and call.
///
/// note: a list, `;` between its entries, and a command adds to it rather than writing over it: a
/// session run inside another one's command is that command's, and so is everything it starts.
pub const CALL_VAR: &str = "KAMCHATKA_CALL";

/// How long a straggler is given to leave on `SIGTERM` before it is sent `SIGKILL`.
const GRACE: Duration = Duration::from_secs(2);

/// How long a straggler sent `SIGKILL` is given to be gone before it is said to be still running.
const KILLED: Duration = Duration::from_millis(500);

/// What `look` finds once it finds nothing or `within` has gone by, looking again every 20
/// milliseconds: the processes still there after a signal was given time to land.
fn settled(
    mut look: impl FnMut() -> BTreeMap<usize, Vec<u32>>,
    within: Duration,
) -> BTreeMap<usize, Vec<u32>> {
    let until = std::time::Instant::now() + within;
    let mut found = look();
    while !found.is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
        found = look();
    }

    found
}

/// The calls whose processes were all stopped: those found to stop, less any still running after.
fn stopped<'a>(
    found: impl Iterator<Item = usize> + 'a,
    left: &'a BTreeMap<usize, Vec<u32>>,
) -> impl Iterator<Item = usize> + 'a {
    found.filter(|call| !left.contains_key(call))
}

impl Stragglers {
    /// Takes in a command about to run, and hands back what its environment carries for it.
    fn mark(&self, cmd: &str) -> String {
        let mut calls = self.0.calls.lock();
        calls.push(crate::app::text::one_line(cmd));
        let ours = format!("{}.{}", self.0.session, calls.len() - 1);

        match std::env::var(CALL_VAR) {
            Ok(outer) if !outer.is_empty() => format!("{outer};{ours}"),
            _ => ours,
        }
    }

    /// The processes still running that a command of this session started, by the call.
    fn found(&self) -> BTreeMap<usize, Vec<u32>> {
        let mut found: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
        let me = std::process::id();
        let Ok(proc) = std::fs::read_dir("/proc") else {
            return found;
        };
        for pid in proc.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok()) {
            if pid == me {
                continue;
            }
            if let Some(call) = self.call_of(pid) {
                found.entry(call).or_default().push(pid);
            }
        }

        found
    }

    /// Which of this session's calls process `pid` came from, by what its environment says.
    fn call_of(&self, pid: u32) -> Option<usize> {
        let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
        let named = format!("{CALL_VAR}=");
        let marks = environ
            .split(|byte| *byte == 0)
            .filter_map(|entry| std::str::from_utf8(entry).ok())
            .find_map(|entry| entry.strip_prefix(named.as_str()))?;

        marks.split(';').find_map(|mark| {
            let (session, call) = mark.rsplit_once('.')?;
            (session == self.0.session).then(|| call.parse().ok())?
        })
    }

    /// The commands of `calls`, as they were typed.
    fn named(&self, calls: impl Iterator<Item = usize>) -> Vec<String> {
        let typed = self.0.calls.lock();
        calls.filter_map(|call| typed.get(call).cloned()).collect()
    }

    /// The commands that still have something running, as they were typed.
    pub fn running(&self) -> Vec<String> {
        self.named(self.found().into_keys())
    }

    /// Stops everything a command left running - `SIGTERM`, then `SIGKILL` after two seconds for
    /// what is still there - and hands back the commands whose processes were stopped.
    ///
    /// note: blocking and short: it is called as a session ends, where nothing is left to be
    /// responsive for and a runtime may already be on its way out.
    ///
    /// note: looked for again before the `SIGKILL`, not from the first list: a job that forks as it
    /// is being stopped forks something that carries the mark too.
    pub fn stop(&self) -> Vec<String> {
        let found = self.found();
        for pid in found.values().flatten() {
            self.signal(*pid, rustix::process::Signal::TERM);
        }
        let lasting = settled(|| self.found(), GRACE);
        for pid in lasting.values().flatten() {
            self.signal(*pid, rustix::process::Signal::KILL);
        }
        // note: looked for once more, since a `SIGKILL` is not always the end of a process: one
        // waiting on a device in the kernel takes it only when the wait is over, and a signal can
        // be refused. Named as stopped regardless, a command still running was said to be over;
        // `Stragglers::running` names what is left
        let left = settled(|| self.found(), KILLED);

        self.named(stopped(found.into_keys(), &left))
    }

    /// Sends `signal` to process `pid`, if it still carries this session's mark.
    ///
    /// note: through a `pidfd`, opened first and the mark read after, so a process that exited and
    /// had its number reused between the look and the signal is not the one signalled.
    fn signal(&self, pid: u32, signal: rustix::process::Signal) {
        let Some(opened) = self::pid(pid).and_then(|pid| {
            rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).ok()
        }) else {
            return;
        };
        if self.call_of(pid).is_some() {
            let _ = rustix::process::pidfd_send_signal(&opened, signal);
        }
    }
}

/// `pid` as the identifier `rustix` takes, if it is one.
fn pid(pid: u32) -> Option<rustix::process::Pid> {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
}

/// The one thing it does, and the one argument that does it.
///
/// note: a function rather than written into the schema, because the refusal for an argument
/// `run` does not read reads the same table the schema is built from - which is what stops the
/// two from disagreeing about what `shell` takes.
fn ops() -> Vec<Op> {
    vec![Op::new(
        "run",
        "",
        vec![Arg::text("cmd", "the command line, as a shell would read it").needed()],
    )]
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        // note: the confinement is reported only when there is one. A command stopped by Landlock
        // comes back with an ordinary permission error and nothing to distinguish it from a file
        // that really is protected, and a model that cannot tell those apart spends its turns
        // trying `sudo`

        // note: the figure rather than "long output", because a model finds out about a limit it
        // cannot see by spending it - and read off the table rather than written into the sentence,
        // since `/limit exec:run` moves it and a description is built afresh for every request.
        // Bare, like the marker a cut output carries: the model reads
        // `[... 4000 bytes truncated by an output limit ...]` against this, and the two should be
        // the same kind of number
        let cut = match self.limits.for_call(&[Capability::exec("run")]) {
            Some(bytes) => format!(" Output over {bytes} bytes is cut off at the end."),
            None => String::new(),
        };

        // note: the working directory being read-only is said here rather than at the point of
        // failure, where the rest of the confinement is accounted for. `Sandbox::note_for` says
        // nothing about a refusal naming a path this session reaches, on the grounds that such a
        // refusal is the file's own permissions. That is right until `fs:write` is refused, and
        // then every write inside the working directory hits the boundary and reads the same way.
        // Standard error does not say whether a refusal was a read or a write, so the only sentence
        // that can be certain is this one, before anything is run
        let confined = self.confined.then(|| {
            Sandbox::of(
                &self.policy,
                self.workdir.clone(),
                self.writable.clone(),
                self.readable.clone(),
                false,
            )
        });
        let read_only = confined
            .as_ref()
            .is_some_and(|sandbox| !sandbox.workdir_writable);
        // note: the paths that were opened are named, because a model told only about "the working
        // directory and the system paths" has no reason to try `~/.rustup` even when somebody
        // opened it for that. Read the confinement the command runs under rather than the flags,
        // because a refusal of `fs:write` makes the paths `--sandbox-write` opened read-only there
        let (writable, readable) = match &confined {
            Some(sandbox) => (&sandbox.writable, &sandbox.readable),
            None => (&self.writable, &self.readable),
        };
        let opened: Vec<String> = writable
            .iter()
            .map(|path| format!("{} read-write", path.display()))
            .chain(
                readable
                    .iter()
                    .map(|path| format!("{} read-only", path.display())),
            )
            .collect();
        // note: what the stance makes of the network, said as precisely as it is known. A command
        // that reaches out waits on a person, and a model that does not know that reads a long
        // silence as a hang
        let network = match confined.as_ref().map(|sandbox| sandbox.network) {
            Some(Network::Shut) => ", and it has no network",
            Some(Network::Asked) => {
                ", and the first time a command reaches for the network it waits while the person \
                 you are working with is asked"
            }
            _ => "",
        };

        // note: `$TMPDIR` is named because it is writable even where the working directory is not;
        // a model in a read-only session that is not told so has nowhere it knows it may write.
        //
        // note: the directory is named as where every call starts. A model that cannot see where it
        // is, or has been trained where a shell's directory drifts between calls, opens each
        // command with `cd` to a guessed path - a stage the advisor asks about, and a relative path
        // `Sandbox::note_for` then judges from the wrong place
        ToolSpec::new(
            "shell",
            format!(
                "runs one command with `sh -c` and returns its exit status, its errors and its \
                 output. Every call starts afresh in the working directory, {}, so a `cd` lasts \
                 only as long as the command it is in.{cut} Nothing \
                 is typed at it: a command reading its input reads end-of-file.{}",
                self.workdir.display(),
                match self.confined {
                    true => format!(
                        " It runs confined: outside the working directory it can read {}{} and \
                         no more{network} - so a permission error there is the confinement rather \
                         than the command. `$TMPDIR` is a directory of the command's own that it \
                         may write in, removed when it ends.{}",
                        match self.shell_readable.as_slice() {
                            [] => "nothing of this machine's own".to_owned(),
                            [one] => one.display().to_string(),
                            [rest @ .., last] => format!(
                                "{} and {}",
                                rest.iter()
                                    .map(|path| path.display().to_string())
                                    .collect::<Vec<_>>()
                                    .join(", "),
                                last.display()
                            ),
                        },
                        match opened.is_empty() {
                            true => String::new(),
                            false => format!(", and {},", opened.join(" and ")),
                        },
                        match read_only {
                            true =>
                                " The working directory is read-only in this session, so a \
                                     refusal to write in it is that boundary too.",
                            false => "",
                        }
                    ),
                    false => String::new(),
                }
            ),
        )
        // note: one operation, asked for by name, because every tool this program offers takes an
        // `action` and a model should not have to remember which of them is the exception. It costs
        // one word in the call and buys a rule with no holes, the same reasoning `log` is written
        // to
        .with_schema(schema(&ops(), self.layout))
        .with_capabilities([Capability::exec("run")])
    }

    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits.for_call(&self.needs(call))
    }

    fn unparsed(&self, written: &str) -> Option<String> {
        crate::tools::ops::advice(written, self.layout)
    }

    /// note: what `invoke` refuses before anything runs, so a command that could never run is not
    /// put to somebody to allow first.
    fn malformed(&self, call: &ToolCall) -> Option<String> {
        let args = match inner(&call.args) {
            Ok(args) => args,
            Err(misread) => return Some(misread.said(self.layout)),
        };
        let args = &*args;
        // note: named rather than assumed, for the reason `fs` refuses one it does not have: a call
        // that meant something else and was answered as `run` is a command nobody asked for. The
        // tool is `shell` and the operation is `run`; the domain it declares is `exec`, which is
        // what a permission rule is written against
        let named = &args["action"];
        if !named.is_null() && named.as_str() != Some("run") {
            let named = named
                .as_str()
                .map_or_else(|| named.to_string(), str::to_owned);
            return Some(format!(
                "`{named}` is not something `shell` does; it does run"
            ));
        }
        // the rule the other tools hold to: an ignored argument comes back as a real answer - the
        // answer to the call without it. `cmd` beside a stray `path` is a command meant to point
        // somewhere
        if let Some(refusal) = ops::unread("run", args, &ops()) {
            return Some(refusal);
        }

        arg(args, "cmd").err().map(|e| e.to_string())
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        if let Some(refusal) = self.malformed(call) {
            return Ok(ToolOutput::error(refusal));
        }
        let args = inner(&call.args).map_err(|misread| misread.said(self.layout))?;
        let args = &*args;
        let cmd = arg(args, "cmd")?;

        // what the command may reach, which is a different question from whether it may run: the
        // kernel answered that one before this was called
        //
        // note: kept rather than built and dropped, because it can tell afterwards whether a
        // permission error came from this boundary; see `Sandbox::note_for`
        let sandbox = self.confined.then(|| Sandbox {
            shell_readable: self.shell_readable.clone(),
            devices: self.devices.clone(),
            ..Sandbox::of(
                &self.policy,
                self.workdir.clone(),
                self.writable.clone(),
                self.readable.clone(),
                self.policy.was_granted_the_network(&call.id),
            )
        });
        // the mark everything it starts carries lets the end of the session find what it left
        // running; see `Stragglers`
        let mark = self.stragglers.mark(cmd);

        // note: where the network is not open, every attempt at it comes to the gatekeeper, which
        // asks or refuses; see `Gatekeeper`
        let network = sandbox.as_ref().map(|sandbox| sandbox.network);
        let (gatekeeper, gate) = match network {
            Some(Network::Shut | Network::Asked) => {
                let (gatekeeper, gate) = Gatekeeper::keep(
                    self.policy.clone(),
                    &call.id,
                    cmd,
                    network == Some(Network::Shut),
                );
                (Some(gatekeeper), Some(gate))
            }
            _ => (None, None),
        };
        let started = match &sandbox {
            Some(sandbox) => Confined::start(sandbox, cmd, &mark, gate).await,
            // a group of its own, so that stopping the command can stop everything the command
            // started; see `stop`
            None => {
                let mut command = tokio::process::Command::new("sh");
                command
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(&self.workdir)
                    .process_group(0);
                // the program's keys stay with the program; see `endpoint::KEYS` and
                // `endpoint::ADDRESSES`
                scrub(&mut command, |name| std::env::var_os(name));
                command.env(CALL_VAR, &mark);
                // killed if this call is dropped before it is over; see `Running`
                command.kill_on_drop(true);
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map(|mut child| {
                        let stdout = child.stdout.take().expect("stdout was piped");
                        let stderr = child.stderr.take().expect("stderr was piped");
                        Started {
                            stdout: Box::new(stdout),
                            stderr: Box::new(stderr),
                            said: Vec::new(),
                            child: Child::Plain(child),
                            scratch: None,
                        }
                    })
            }
        };
        let Started {
            stdout,
            mut stderr,
            said,
            mut child,
            scratch,
        } = match started {
            Ok(started) => started,
            // note: the reason and not the command, which the model has in its own call. Quoted
            // first, a command long enough to fail here pushed the reason past the output limit
            Err(e) if e.kind() == std::io::ErrorKind::ArgumentListTooLong => {
                return Ok(ToolOutput::error(format!(
                    "could not run the command: {e}. It is too long to hand a program as one \
                     argument; write it to a file and run the file"
                )));
            }
            Err(e) => return Ok(ToolOutput::error(format!("could not run the command: {e}"))),
        };
        // the group of a command run without a sandbox, killed if this call is dropped, and the
        // temporary directory, removed then; a sandbox does both when it is dropped
        let running = Running {
            group: match &child {
                Child::Plain(child) => child.id(),
                Child::Confined(_) => None,
            },
            scratch: scratch.as_ref().map(|scratch| scratch.path().to_path_buf()),
        };

        // note: this end keeps a buffer rather than one the task hands back when done, because the
        // task may never be done; see below. What arrived by then is still worth telling the model.
        // Kept up to `KEPT`, and past that read and counted, so a command that writes without end
        // fills neither this process nor the pipe it writes to
        let heard = Arc::new(std::sync::Mutex::new((said, 0)));
        let mut collecting_stderr = tokio::spawn({
            let heard = heard.clone();
            async move {
                let mut chunk = [0u8; 4096];
                while let Ok(read @ 1..) = stderr.read(&mut chunk).await {
                    let mut heard = heard.lock().expect("nothing panics holding it");
                    let (kept, dropped) = &mut *heard;
                    let room = KEPT.saturating_sub(kept.len()).min(read);
                    kept.extend_from_slice(&chunk[..room]);
                    *dropped += read - room;
                }
            }
        });

        let Watched {
            collected,
            dropped,
            interrupted,
            halted,
            waited,
        } = watch(stdout, &mut child, &output).await;

        // note: the end of the command is not the end of its standard error. A killed shell can
        // leave children behind - `sleep 60; echo done` is two processes - and so can one that
        // finished: `python3 -m http.server > log &` holds the pipe while the server runs. Waiting
        // for the end would be a call that never answers, so it gets a moment to drain once the
        // command is gone
        let held = tokio::time::timeout(HEARTBEAT, &mut collecting_stderr)
            .await
            .is_err();
        if held {
            collecting_stderr.abort();
        }
        let (mut errors, mut dropped, read) = {
            let heard = heard.lock().expect("nothing panics holding it");
            (
                String::from_utf8_lossy(&heard.0).into_owned(),
                dropped + heard.1,
                heard.0.clone(),
            )
        };
        // held to the ceiling as it is kept, for the reason standard output is: the bytes were
        // counted as they arrived, and what is not UTF-8 grows on the way to text
        if errors.len() > KEPT {
            let mut cut = KEPT;
            while !errors.is_char_boundary(cut) {
                cut -= 1;
            }
            dropped += read.len() - read_into(&read, cut);
            errors.truncate(cut);
        }
        if held && !interrupted {
            errors.push_str("\n[standard error is still open: something this command started is still running]\n");
        }
        // note: not `ExitStatus`'s `Display`, which renders `exit status: 0` and would make the
        // line `exit: exit status: 0`. It gives the number and whether the command worked.
        //
        // note: stopped is reported here, not after the standard error, because an output limit
        // cuts from the end. A stopped command that said more than the limit would lose the line
        // explaining why output stops mid-sentence, and the truncation marker in its place would
        // mislead the model. The first line survives anything
        let waited = match waited {
            Some(waited) => waited,
            None => child.wait().await,
        };
        let reached = gatekeeper.and_then(Gatekeeper::over);
        // what the command left running is kept until it is gone; see `Confined::keep`
        child.keep();
        drop(scratch);
        let scratch = running.over();
        let status = status_line(interrupted, halted, waited);

        // note: under the status line rather than beside the message it explains, for the same
        // reason the status line says whether the command was stopped: an output limit cuts from
        // the end, and a note accounting for a permission error is worth nothing if the
        // truncation takes the note and leaves the error. The top of the output survives anything
        //
        // note: on a blocking thread, since judging a refused path is a dozen `canonicalize` calls
        // and the note judges up to `LOOKED` of them: seconds of a runtime worker, otherwise
        let note = match sandbox {
            Some(sandbox) => {
                let (errors, scratch) = (errors.clone(), scratch.clone());
                tokio::task::spawn_blocking(move || {
                    sandbox.note_for_in(&errors, scratch.as_deref())
                })
                .await
                .ok()
                .flatten()
            }
            None => None,
        };
        let note = match note {
            Some(note) => format!("{note}\n"),
            None => String::new(),
        };
        // after the note, which compares against the directory and needs it still there
        if let Some(scratch) = scratch {
            let _ =
                tokio::task::spawn_blocking(move || crate::sandbox::remove_scratch(&scratch)).await;
        }
        // note: what came of it, where the command reached for the network. Up here with the rest,
        // for the same reason, and said because the output alone cannot say it: a refused socket is
        // `Permission denied` from the kernel, the same words a file's own permissions produce, and
        // a refused name lookup is `Temporary failure in name resolution`, which reads as a network
        // having trouble. A model that does not know it was refused looks for another way out
        // rather than asking
        let refused = "so every connection it tried and every datagram it sent was refused \
                       with `Permission denied`, and a name it tried to look up failed with \
                       `Temporary failure in name resolution`";
        // note: "when it was asked about", not "the person said": the kernel's words for its own
        // questions, because what answered may be `--on-ask` in a run nobody is watching, and the
        // model repeats whatever this claims about who decided
        let reached = match reached {
            Some(Reached::Let) => {
                "[this command reached for the network, and was let through when it was asked \
                 about]\n"
                    .to_owned()
            }
            Some(Reached::Refused) => format!(
                "[this command reached for the network, and was refused when it was asked about, \
                 {refused}. That is an answer to this command rather than a standing rule: say \
                 what you need the network for before trying again.]\n"
            ),
            Some(Reached::Shut) => format!(
                "[this command reached for the network, which this session refuses, {refused}. \
                 Work without it, or say what you need it for and ask for it to be allowed.]\n"
            ),
            None => String::new(),
        };
        // and what was not kept, under it for the same reason
        let unkept = match dropped {
            0 => String::new(),
            dropped => format!(
                "[{dropped} more bytes of output were read and not kept: a stream is kept up to \
                 {KEPT} bytes]\n"
            ),
        };
        // note: standard error before standard output because an output limit cuts from the end,
        // and a command that failed after printing a lot would lose the line saying why; errors are
        // usually the shorter stream. A build whose warnings fill the limit pushes its output off
        // the end into the excluded whole; cutting standard output to make room would lose it
        // outright, since the whole holds only what the tool returned
        let text = format!(
            "{status}\n{reached}{note}{unkept}--- stderr ---\n{errors}\n--- stdout ---\n{collected}"
        );

        Ok(ToolOutput::new(text))
    }
}

/// What came of a command's first attempt to reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reached {
    /// It was asked about, and let through.
    Let,
    /// It was asked about, and refused.
    Refused,
    /// The session refuses the network, and nobody was asked.
    Shut,
}

/// Answers a confined command's attempts to reach the network for as long as anything it started
/// is still making them.
///
/// note: a task of its own rather than a branch of the call's loop, because attempts can outlast
/// the call. `python3 -m http.server &` leaves a process behind in the same sandbox, and once the
/// call is over nothing else answers it. sandlock hands each attempt over with its approval, and
/// an attempt nobody answers waits, so the task runs until the sandbox is gone and takes the
/// sending end with it.
///
/// note: a connection to a unix socket comes here too, with no address, since sandlock holds every
/// `connect` it sees. It is let through to sandlock's own check, which holds it to the paths the
/// command may write; see the note at the top of [`crate::sandbox`].
///
/// note: a command started under a refusal is refused throughout. A command started under a
/// question consults the stance at its first attempt - an answer of `always` to another command's
/// question counts - and asks the person once where it is still `ask`. Whatever was decided holds
/// for the rest of the command. Once the call is over nobody can ask on its behalf, so an attempt
/// after that with nothing decided is refused.
struct Gatekeeper {
    /// What came of the first attempt, where there was one.
    reached: Arc<parking_lot::Mutex<Option<Reached>>>,
    /// Said when the call is over.
    over: tokio::sync::watch::Sender<bool>,
}

impl Gatekeeper {
    /// The gatekeeper for one command, and the policy that brings its attempts to it.
    fn keep(
        policy: Arc<Careful>,
        call: &ToolCallId,
        cmd: &str,
        refusing: bool,
    ) -> (Self, sandlock_core::PolicyFn) {
        use sandlock_core::policy_fn::Approval;

        let reached = Arc::new(parking_lot::Mutex::new(None));
        let (over, mut ended) = tokio::sync::watch::channel(false);
        let (call, cmd) = (call.clone(), cmd.to_owned());
        let (attempts, mut attempted) = tokio::sync::mpsc::unbounded_channel::<Approval>();
        // note: on sandlock's thread, so it only passes the attempt on. An approval dropped
        // unanswered - the task gone with the runtime - is a refusal
        let gate = sandlock_core::PolicyFn::deferred(move |event, _, approval| {
            if let Some(approval) = approval {
                match event.host {
                    Some(_) => {
                        let _ = attempts.send(approval);
                    }
                    None => approval.allow(),
                }
            }
        });

        tokio::spawn({
            let reached = reached.clone();
            async move {
                let mut decided = None;
                while let Some(attempt) = attempted.recv().await {
                    let allow = match decided {
                        Some(allow) => allow,
                        None => {
                            let stance = match refusing {
                                true => Verdict::Deny,
                                false => policy
                                    .stance(&super::Subject::Capability(Capability::net("reach"))),
                            };
                            // over, or dropped without saying so, which is a call that went
                            // with the process
                            let over = *ended.borrow() || ended.has_changed().is_err();
                            let (allow, came) = match stance {
                                Verdict::Allow => (true, None),
                                Verdict::Ask if !over => {
                                    // the question goes with this future, which is dropped the
                                    // moment the call is over
                                    let asking = policy.reaching().ask(call.clone(), cmd.clone());
                                    let answer = tokio::select! {
                                        answer = asking => answer,
                                        _ = ended.wait_for(|over| *over) => None,
                                    };
                                    match answer {
                                        Some(true) => (true, Some(Reached::Let)),
                                        Some(false) => (false, Some(Reached::Refused)),
                                        None => (false, None),
                                    }
                                }
                                Verdict::Ask => (false, None),
                                _ => (false, Some(Reached::Shut)),
                            };
                            *reached.lock() = came;
                            decided = Some(allow);
                            allow
                        }
                    };
                    // note: only a `Grant` can be refused, and none is given here
                    let _ = attempt.resolve(match allow {
                        true => sandlock_core::policy_fn::Verdict::Allow,
                        false => sandlock_core::policy_fn::Verdict::DenyWith(libc::EACCES),
                    });
                }
            }
        });

        (Self { reached, over }, gate)
    }

    /// The call is over; hands back what came of the command's first attempt, if it made one.
    fn over(self) -> Option<Reached> {
        let _ = self.over.send(true);
        *self.reached.lock()
    }
}

/// A command just started: its two streams, anything to put in front of what it says on its
/// standard error, the command itself, and its temporary directory, held until it is over.
struct Started {
    stdout: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    stderr: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    said: Vec<u8>,
    child: Child,
    scratch: Option<crate::sandbox::Scratch>,
}

/// A running command: a process of this one's, or a sandbox sandlock runs one in.
enum Child {
    Plain(tokio::process::Child),
    Confined(Box<Confined>),
}

/// A command running in a sandlock sandbox.
///
/// note: the sandbox is the command's session. Dropping it kills everything the command started,
/// which is what a call dropped before its command ended wants; a call that ended hands it to
/// [`Confined::keep`] instead, for what the command left running.
struct Confined {
    sandbox: sandlock_core::Sandbox,
    controller: sandlock_core::Controller,
}

impl Confined {
    /// Starts `cmd` in a sandbox built from `sandbox`, its network answered by `gate` where it is
    /// not open.
    async fn start(
        sandbox: &Sandbox,
        cmd: &str,
        mark: &str,
        gate: Option<sandlock_core::PolicyFn>,
    ) -> std::io::Result<Started> {
        let scratch = crate::sandbox::make_scratch(&crate::sandbox::scratch_path());
        // note: removed here where nothing started, since nothing else knows of it then
        match Self::spawn(sandbox, cmd, mark, gate, scratch.as_ref()).await {
            Ok(started) => Ok(Started { scratch, ..started }),
            Err(e) => {
                if let Some(scratch) = scratch {
                    let _ = crate::sandbox::remove_scratch(scratch.path());
                }
                Err(e)
            }
        }
    }

    /// [`Confined::start`], given its temporary directory, which the caller puts in what it hands
    /// back.
    async fn spawn(
        sandbox: &Sandbox,
        cmd: &str,
        mark: &str,
        gate: Option<sandlock_core::PolicyFn>,
        scratch: Option<&crate::sandbox::Scratch>,
    ) -> std::io::Result<Started> {
        // note: the whole environment, since sandlock is handed one rather than told what to take
        // out. The program's keys stay with the program; see `endpoint::KEYS` and
        // `endpoint::ADDRESSES`
        let removed = scrubbed(|name| std::env::var_os(name));
        let mut env: Vec<(String, String)> = crate::sandbox::environment()
            .filter(|(key, _)| {
                !removed.contains(&key.as_str())
                    && ![CALL_VAR, "TMPDIR", "GIT_CONFIG_GLOBAL"].contains(&key.as_str())
            })
            .collect();
        env.push((CALL_VAR.to_owned(), mark.to_owned()));
        // note: and taken away where there is none, as `make_scratch` says. What this program has
        // is not a directory the command may write, and passing it on would tell the command it
        // has somewhere to write where it has not
        if let Some(scratch) = scratch {
            env.push(("TMPDIR".to_owned(), scratch.path().display().to_string()));
        }
        // note: unconfined, git can read its own configuration, and pointing it elsewhere would
        // take a person's identity and aliases away for nothing
        match sandbox.git_config_global() {
            Some(global) => {
                env.push(("GIT_CONFIG_GLOBAL".to_owned(), global.display().to_string()))
            }
            None => env.extend(
                std::env::var("GIT_CONFIG_GLOBAL")
                    .ok()
                    .map(|global| ("GIT_CONFIG_GLOBAL".to_owned(), global)),
            ),
        }

        let (builder, rules) = sandbox.sandlock(scratch, gate, false);
        crate::sandbox::fits(&env, cmd)?;
        let (builder, argv) = crate::sandbox::command(builder, env, cmd);
        let mut confined = builder.build().map_err(std::io::Error::other)?;
        let mut process = confined
            .popen(
                &argv,
                sandlock_core::StdioMode::Null,
                sandlock_core::StdioMode::Piped,
                sandlock_core::StdioMode::Piped,
            )
            .await
            .map_err(std::io::Error::other)?;
        let (stdout, stderr) = (process.take_stdout(), process.take_stderr());
        // note: a `Process` dropped leaves the command running; the sandbox is what ends it
        drop(process);
        let left_out: Vec<String> = rules.left_out().to_vec();
        let stream = |fd: Option<std::os::fd::OwnedFd>| -> std::io::Result<_> {
            let fd = fd.ok_or_else(|| std::io::Error::other("a stream was not piped"))?;
            Ok(Box::new(tokio::net::unix::pipe::Receiver::from_owned_fd(
                fd,
            )?))
        };
        let (stdout, stderr) = (stream(stdout)?, stream(stderr)?);
        let controller = confined.controller().map_err(std::io::Error::other)?;
        // a served session refuses anything in it; see `sandbox::from_a_command`
        crate::sandbox::began(controller.clone());

        Ok(Started {
            stdout,
            stderr,
            said: left_out
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>()
                .into_bytes(),
            child: Child::Confined(Box::new(Self {
                sandbox: confined,
                controller,
            })),
            scratch: None,
        })
    }

    /// The command's own process, the shell, until it has exited.
    ///
    /// note: asked of sandlock each time rather than kept, since once the shell has exited its
    /// number may name another process.
    fn pid(&self) -> Option<u32> {
        self.sandbox.pid().and_then(|pid| u32::try_from(pid).ok())
    }

    /// Keeps the sandbox for as long as something the command started is running in it, so that
    /// a job left in the background is still answered, and ends it after.
    ///
    /// note: looked at every second rather than waited on, since sandlock says when a group empties
    /// only when asked. It is cheap: a read of each group the command made.
    fn keep(self) {
        let Self {
            mut sandbox,
            controller,
        } = self;
        tokio::spawn(async move {
            while controller
                .process_groups()
                .is_ok_and(|groups| !groups.is_empty())
            {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let _ = sandbox.shutdown().await;
        });
    }
}

impl Child {
    /// How the command ended, if it has.
    async fn exited(&mut self) -> Option<std::io::Result<std::process::ExitStatus>> {
        match self {
            Self::Plain(child) => child.try_wait().transpose(),
            Self::Confined(confined) => match confined.controller.main_exited() {
                true => Some(self.wait().await),
                false => None,
            },
        }
    }

    /// Waits for the command to end.
    ///
    /// note: cancellable either way. sandlock keeps what a cancelled wait had started, so the next
    /// one carries on.
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        use std::os::unix::process::ExitStatusExt as _;

        match self {
            Self::Plain(child) => child.wait().await,
            Self::Confined(confined) => {
                let waited = confined.sandbox.wait().await;
                // as `waitpid` writes it: a code in the second byte, a signal in the first
                Ok(std::process::ExitStatus::from_raw(
                    match waited.map_err(std::io::Error::other)? {
                        sandlock_core::ExitStatus::Code(code) => code << 8,
                        sandlock_core::ExitStatus::Signal(signal) => signal,
                        sandlock_core::ExitStatus::Killed | sandlock_core::ExitStatus::Timeout => {
                            libc::SIGKILL
                        }
                    },
                ))
            }
        }
    }

    /// The command's own process, until it has been waited for.
    fn id(&self) -> Option<u32> {
        match self {
            Self::Plain(child) => child.id(),
            Self::Confined(confined) => confined.pid(),
        }
    }

    /// Hands what the command left running to whoever keeps it.
    fn keep(self) {
        if let Self::Confined(confined) = self {
            confined.keep();
        }
    }
}

/// The process group of a command still running, killed if the call is dropped before it ends, and
/// the temporary directory it was given, removed then.
///
/// note: what `kill_on_drop` does for the child alone, done for its group. A call is dropped when
/// the process is on its way out - a runtime shut down with a turn still going, a panic - and the
/// group is not sent the terminal's hangup, so without this a `make` or a server the command
/// started keeps running after the agent is gone, with nothing left to record what it does.
///
/// note: and the directory for the same reason. The confined child cannot remove it, and left
/// behind it holds whatever the command wrote there until its identifier comes round again.
struct Running {
    group: Option<u32>,
    scratch: Option<PathBuf>,
}

impl Running {
    /// The command has been waited for, so its identifier may already be somebody else's. Returns
    /// the temporary directory for the caller to remove without blocking.
    fn over(mut self) -> Option<PathBuf> {
        self.group = None;
        self.scratch.take()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(pid) = self.group {
            // waited for here rather than through tokio because a drop cannot wait on a future, and
            // this may be the last thing the runtime does
            let _ = killing(pid).status();
        }
        if let Some(scratch) = &self.scratch {
            let _ = crate::sandbox::remove_scratch(scratch);
        }
    }
}
/// What reading a command's standard output and waiting for it came to.
struct Watched {
    /// What it wrote, as far as `KEPT`.
    collected: String,
    /// The bytes of standard output read past `KEPT` and let go.
    dropped: usize,
    /// Whether the turn was interrupted, and the command stopped for it.
    interrupted: bool,
    /// The signal the command stopped itself with, where it did; see `stopped_itself`.
    halted: Option<i32>,
    /// How it ended, where that is known yet.
    waited: Option<std::io::Result<std::process::ExitStatus>>,
}

/// Reads a command's standard output to its end, or until the command stops itself or the turn is
/// interrupted, and then waits for the command the same way.
async fn watch(
    stdout: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    child: &mut Child,
    output: &OutputSink,
) -> Watched {
    // note: bytes rather than `lines()`, which refuses a line that is not UTF-8. Stopping there
    // would leave the pipe full and unread, so `cat` of a picture would block writing and the
    // call would wait for it for ever. A command's output is whatever it wrote; what is not
    // text is shown with `from_utf8_lossy`
    let mut stdout = BufReader::new(stdout);
    let mut line = Vec::new();
    let mut collected = String::new();
    // bytes read past `KEPT` and let go. Once one line does not fit, nothing after it is kept,
    // so the kept output is the start with no hole
    let (mut dropped, mut full) = (0, false);
    let mut interrupted = false;
    // the signal the command stopped itself with, where it did; see `stopped_itself`
    let mut halted = None;
    let mut waited = None;
    loop {
        // the timeout is what makes a command that says nothing at all interruptible; without
        // it this would sit in `read_until` until the child felt like talking. A timed-out
        // read keeps what it had in `line`, and the next one carries on from there
        //
        // note: and a read takes no more than would put `line` one byte past the ceiling. A
        // command writing without newlines never ends a line, and the heartbeat is all that
        // stopped the read - which at the speed of a pipe held hundreds of megabytes against
        // a ceiling of eight. A line that stops there is over it, and only its start is kept
        let room = (KEPT + 1).saturating_sub(line.len()) as u64;
        let mut bounded = (&mut stdout).take(room);
        match tokio::time::timeout(HEARTBEAT, bounded.read_until(b'\n', &mut line)).await {
            // a line a timed-out read left in `line` is only ever finished here, by the end
            Ok(Ok(0)) => {
                keep(&line, &mut collected, output, &mut full, &mut dropped);
                break;
            }
            Ok(Ok(_)) => {
                keep(&line, &mut collected, output, &mut full, &mut dropped);
                line.clear();
            }
            Ok(Err(e)) => {
                collected.push_str(&format!("\n[could not read the output: {e}]\n"));
                break;
            }
            // a line that never ends is held to the ceiling as well, or it would be the way
            // round it
            Err(_) if line.len() > KEPT => {
                keep(&line, &mut collected, output, &mut full, &mut dropped);
                line.clear();
            }
            // note: the end of the command is not the end of its standard output, and there is
            // no draining it here: `sleep 60 & echo started` leaves the pipe open for a minute,
            // and a server started with `&` for ever. A quiet moment after the command has gone
            // is the end of what it said
            Err(_) => {
                if let Some(status) = child.exited().await {
                    keep(&line, &mut collected, output, &mut full, &mut dropped);
                    collected.push_str(
                        "[standard output is still open: something this command started is \
                         still running]\n",
                    );
                    waited = Some(status);
                    break;
                }
                if let Some(signal) = stopped_itself(child) {
                    stop(child).await;
                    keep(&line, &mut collected, output, &mut full, &mut dropped);
                    halted = Some(signal);
                    break;
                }
            }
        }

        // an unfinished line from a timed-out read is part of what the command said by then, so
        // the answer to a stopped command holds it. `printf 'downloading 45%'` never ends its
        // line, and it is the line worth having
        if output.is_interrupted() {
            stop(child).await;
            keep(&line, &mut collected, output, &mut full, &mut dropped);
            interrupted = true;
            break;
        }
    }
    // whatever stopped the reading, nobody reads this pipe again. Closing it tells a command
    // still writing to it, rather than leaving it blocked on a full pipe
    drop(stdout);

    // note: the end of standard output is not the end of the command. `sleep 5 >&-` has closed
    // it and is still running, so the wait is watched the way the reading was
    while !interrupted && waited.is_none() {
        match tokio::time::timeout(HEARTBEAT, child.wait()).await {
            Ok(status) => {
                waited = Some(status);
                break;
            }
            Err(_) if output.is_interrupted() => {
                stop(child).await;
                interrupted = true;
            }
            Err(_) if halted.is_none() => {
                if let Some(signal) = stopped_itself(child) {
                    stop(child).await;
                    halted = Some(signal);
                }
            }
            Err(_) => {}
        }
    }

    Watched {
        collected,
        dropped,
        interrupted,
        halted,
        waited,
    }
}

/// The first line of a command's answer, which says how it ended.
fn status_line(
    interrupted: bool,
    halted: Option<i32>,
    waited: std::io::Result<std::process::ExitStatus>,
) -> String {
    let (meant, status) = match (interrupted, waited) {
        // note: said as stopped, not as the kill that ended it. A stopped command waits for a
        // `SIGCONT` nothing here will send, so the model that wrote `kill -STOP $$` is told why
        // the command came back and does not try the same thing again
        (false, _) if let Some(signal) = halted => (
            Exit::Stopped,
            format!(
                "exit: stopped itself with {}, and was ended, since nothing would continue \
                 it; what is below is what it had said by then",
                signal_name(signal)
            ),
        ),
        (true, _) => (
            Exit::Stopped,
            // note: not "at the person's request": a deadline interrupts the same way and
            // carries no word of who asked, so the model is told only what happened
            "exit: stopped before it finished, when the turn was interrupted; what is below \
             is what it had said by then"
                .to_owned(),
        ),
        (false, Ok(status)) => match status.code() {
            Some(0) => (Exit::Ok, "exit: 0".to_owned()),
            Some(code) => (
                Exit::Failed,
                format!("exit: {code} (the command reported a failure)"),
            ),
            None => (
                Exit::Stopped,
                format!("exit: {status} (the command was killed)"),
            ),
        },
        (false, Err(e)) => (Exit::Stopped, format!("exit: unknown ({e})")),
    };
    // note: the one place the line and its meaning are checked together, at no cost in a
    // release build. A test that runs a real command then also tests that the line says what
    // the run meant; a separate table of strings could not, since it would keep agreeing with
    // itself after the wording changed
    debug_assert_eq!(
        Exit::of(&status),
        Some(meant),
        "the status line and what it means have come apart"
    );

    status
}

/// The signal the command stopped itself with: `kill -STOP $$`, or a `SIGTSTP` to its group, which
/// stops the shell as well as what it runs.
///
/// note: asked without reaping and without taking the stop away (`WNOWAIT`), so it is asked again
/// each heartbeat and the end of the command is left to tokio, which waits on it. A stopped command
/// is waited on for ever otherwise: nothing here sends a `SIGCONT`, and a confined one cannot be
/// continued by anybody else.
///
/// note: the command's own process, which is the shell. A program the shell runs that stops itself
/// and nothing else leaves the shell waiting on it, asleep rather than stopped, and is not seen
/// here; nor is a job the command started in the background, which it may stop on purpose.
///
/// note: in a sandbox the command's own process is not this one's child: sandlock forks a process
/// of its own, which stays behind, and the shell is that one's child. So its state is read from
/// `/proc` instead, which does not say which signal stopped it: `0` stands for one that is not
/// known.
fn stopped_itself(child: &Child) -> Option<i32> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};

    match child {
        Child::Plain(_) => {
            let pid = Pid::from_raw(i32::try_from(child.id()?).ok()?)?;
            waitid(
                WaitId::Pid(pid),
                WaitIdOptions::STOPPED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )
            .ok()??
            .stopping_signal()
        }
        Child::Confined(confined) => {
            let shell = confined.pid()?;
            let stat = std::fs::read_to_string(format!("/proc/{shell}/stat")).ok()?;
            // the state is the field after the name, which is in parentheses and may hold anything
            let (_, rest) = stat.rsplit_once(") ")?;
            rest.starts_with('T').then_some(0)
        }
    }
}

/// A stopping signal's name, as a model would write it.
fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGSTOP => "SIGSTOP".to_owned(),
        libc::SIGTSTP => "SIGTSTP".to_owned(),
        libc::SIGTTIN => "SIGTTIN".to_owned(),
        libc::SIGTTOU => "SIGTTOU".to_owned(),
        0 => "a stop signal".to_owned(),
        other => format!("signal {other}"),
    }
}

/// Stops a running command and the processes it started that are still in its process group.
///
/// note: it signals groups rather than the process, because a shell command is rarely one
/// process: `make` starts a compiler and `npm test` starts a runner. A signal to the shell alone
/// leaves those running, still writing in the working directory, with nothing left to report
/// them, after someone who pressed escape was told it stopped.
///
/// note: a sandboxed command's groups are every one sandlock saw it make or join, so a process run
/// under `setsid`, or a daemon that detaches itself, is stopped too. A command run without a
/// sandbox has one group, its own, and a process that left it runs on after the call has said it
/// stopped: holding the whole tree there takes a cgroup or a PID namespace per command.
///
/// note: the unsandboxed group is signalled through `sh`, because signalling a group is not in
/// `std` and this crate has no `unsafe`. It is the same `sh` the tool is built on, so it brings
/// nothing new into the program. The group is the child's own, asked for with `process_group(0)` at
/// spawn, and the child has not been waited on yet, so the identifier cannot yet mean anybody
/// else.
async fn stop(child: &mut Child) {
    match child {
        Child::Plain(child) => {
            if let Some(pid) = child.id() {
                let _ = tokio::process::Command::from(killing(pid)).status().await;
            }
            // and the child itself, in case it was never in a group of its own
            let _ = child.start_kill();
        }
        // every group the command made, which sandlock knows: one that left with `setsid` too
        Child::Confined(confined) => {
            let _ = confined.controller.kill();
        }
    }
}

/// The command that kills process group `pid`, for [`stop`] and for a [`Running`] dropped with
/// its command still going; see the note on `stop` for why it is `sh`.
fn killing(pid: u32) -> std::process::Command {
    let mut kill = std::process::Command::new("sh");
    kill.arg("-c")
        .arg(format!("kill -KILL -{pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    kill
}

/// Keeps one line of standard output, or as much of its start as fits under [`KEPT`], unless the
/// output is already there.
///
/// note: measured as it is kept rather than as it arrived. A non-UTF-8 byte is kept as `�`, three
/// bytes, so a line held to the ceiling as it arrived could be kept at three times its size.
///
/// note: keeps the start of a line that does not fit, rather than none of it, so a line past the
/// ceiling comes back as the start of what it said rather than as nothing.
fn keep(
    line: &[u8],
    collected: &mut String,
    output: &OutputSink,
    full: &mut bool,
    dropped: &mut usize,
) {
    if line.is_empty() {
        return;
    }
    if *full {
        *dropped += line.len();
        return;
    }
    let text = String::from_utf8_lossy(line);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let mut text = text.strip_suffix('\r').unwrap_or(text);
    let room = KEPT.saturating_sub(collected.len() + 1);
    // against the newline too, which an empty line is all of
    if collected.len() + 1 + text.len() > KEPT {
        let mut cut = room;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        *full = true;
        *dropped += line.len() - read_into(line, cut);
        if cut == 0 {
            return;
        }
        text = &text[..cut];
    }
    output.push(format!("{text}\n"));
    collected.push_str(text);
    collected.push('\n');
}

/// How many of the bytes read went into the first `text` bytes of their lossy text.
///
/// note: what is not UTF-8 grows on the way to text, each run of it becoming the three bytes of
/// `�`, so a cut in the text is not a cut at the same place in what was read. The count of bytes
/// not kept is of what the command wrote, and a run of bytes that are not text counted at three
/// times their size was reported as more output than there had been.
fn read_into(read: &[u8], text: usize) -> usize {
    let (mut left, mut taken) = (text, 0);
    for chunk in read.utf8_chunks() {
        let valid = chunk.valid().len().min(left);
        (left, taken) = (left - valid, taken + valid);
        if valid < chunk.valid().len() || chunk.invalid().is_empty() {
            if left == 0 {
                break;
            }
            continue;
        }
        if left < char::REPLACEMENT_CHARACTER.len_utf8() {
            break;
        }
        left -= char::REPLACEMENT_CHARACTER.len_utf8();
        taken += chunk.invalid().len();
    }

    taken
}

/// Removes the program's keys from `command`'s environment, and an address with a password in it,
/// as `value` reads each variable.
fn scrub(
    command: &mut tokio::process::Command,
    value: impl Fn(&str) -> Option<std::ffi::OsString>,
) {
    for removed in scrubbed(value) {
        command.env_remove(removed);
    }
}

/// The variables a command is not handed: the program's keys, and an address with a password in
/// it, as `value` reads each one.
///
/// note: an address is read as `endpoint::address_in` reads it, with what is not text replaced,
/// since non-text is still in the environment the command inherits. Read with `env::var`, which
/// answers such a value as no value, so its password went along with it.
fn scrubbed(value: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<&'static str> {
    crate::endpoint::KEYS
        .iter()
        .copied()
        .chain(
            crate::endpoint::ADDRESSES
                .iter()
                .copied()
                .filter(|address| {
                    value(address)
                        .is_some_and(|url| crate::endpoint::has_credentials(&url.to_string_lossy()))
                }),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Subject;

    /// An address with a password in it is taken out of a command's environment whether or not it
    /// is text, and one without is left.
    #[test]
    fn an_address_with_a_password_is_kept_from_a_command_whatever_its_bytes() {
        use std::os::unix::ffi::OsStringExt as _;

        let [base, advisor] = crate::endpoint::ADDRESSES;
        let mut not_text = b"http://user:password@example.com/v1/".to_vec();
        not_text.push(0xff);
        let value = |variable: &str| match variable {
            name if name == advisor => Some(std::ffi::OsString::from_vec(not_text.clone())),
            name if name == base => Some("http://127.0.0.1:1/v1".into()),
            _ => None,
        };
        let mut command = tokio::process::Command::new("true");
        scrub(&mut command, value);

        let removed: Vec<_> = command
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        assert!(removed.iter().any(|name| name == advisor), "{removed:?}");
        assert!(!removed.iter().any(|name| name == base), "{removed:?}");
        for key in crate::endpoint::KEYS {
            assert!(removed.iter().any(|name| name == key), "{key}: {removed:?}");
        }
    }

    /// What no command of this session started is left alone at its end - a process of this user's
    /// with no mark, and one carrying another session's - and is not named as stopped.
    ///
    /// note: under the signal scope a session runs under, on this test's thread, which is the one
    /// `stop` signals from. Without it `call_of` answering for every process - a mutant did - stops
    /// everything the person has, the desktop with it, and the test passing or failing is moot.
    #[test]
    fn what_this_session_did_not_start_is_left_alone() {
        crate::sandbox::scope_signals();
        let somebody = |mark: Option<&str>| {
            let mut sleep = std::process::Command::new("sleep");
            sleep.arg("30").env_remove(CALL_VAR);
            if let Some(mark) = mark {
                sleep.env(CALL_VAR, mark);
            }
            sleep.spawn().expect("sleep runs")
        };
        let stragglers = Stragglers::default();
        let mut others = [
            somebody(None),
            somebody(Some(&format!("{}-0-0.0", std::process::id() + 1))),
        ];

        assert!(stragglers.running().is_empty());
        assert!(stragglers.stop().is_empty());
        for other in &mut others {
            assert!(
                other.try_wait().expect("it can be asked").is_none(),
                "somebody else's process was stopped"
            );
            other.kill().expect("it is still there to kill");
            other.wait().expect("and to reap");
        }
    }

    #[tokio::test]
    async fn an_action_that_is_not_a_word_runs_nothing() {
        let call = ToolCall::new(
            "c1",
            "shell",
            serde_json::json!({ "action": 42, "cmd": "echo ran" }),
        );
        let said = unconfined()
            .invoke(&call, OutputSink::disconnected())
            .await
            .expect("the tool answers the call either way")
            .content
            .to_text()
            .into_owned();
        assert!(said.contains("is not something `shell` does"), "{said}");
    }

    /// An argument `run` does not take is refused by name, and nothing is run.
    ///
    /// note: `path` beside a command is the mistake this rule prevents: a call that ignored it
    /// would answer with the command's whole output as though the narrowing it asked for had
    /// happened. The refusal reads off the same table the schema is built from, so the two cannot
    /// disagree about what `shell` takes.
    #[tokio::test]
    async fn an_argument_run_does_not_take_is_refused_and_runs_nothing() {
        let call = ToolCall::new(
            "c1",
            "shell",
            serde_json::json!({ "cmd": "echo ran", "path": "notes.md" }),
        );
        let said = unconfined()
            .invoke(&call, OutputSink::disconnected())
            .await
            .expect("the tool answers the call either way")
            .content
            .to_text()
            .into_owned();

        assert!(said.contains("`run` does not take `path`"), "{said}");
        assert!(
            said.contains("nothing was done"),
            "and that the command was not run anyway: {said}"
        );
        assert!(!said.contains("ran"), "no command output: {said}");
    }

    /// The description names the directory every call starts in, and a `cd` in one call is gone by
    /// the next - which is the claim that lets a model stop opening each command with one.
    #[tokio::test]
    async fn every_call_starts_where_the_description_says() {
        let here = std::env::temp_dir();
        let said = unconfined().spec().description;
        assert!(
            said.contains(&format!("working directory, {},", here.display())),
            "{said}"
        );

        let moved = ran("cd / && pwd").await;
        assert!(moved.lines().any(|line| line == "/"), "{moved}");
        let after = ran("pwd").await;
        let at = here.canonicalize().expect("the temporary directory");
        assert!(
            after.lines().any(|line| std::path::Path::new(line) == at),
            "{after}"
        );
    }

    /// A command that stops itself is ended and says so, rather than waited on until somebody
    /// interrupts the turn; one that stops a job of its own is not.
    #[tokio::test]
    async fn a_command_that_stops_itself_is_ended_and_says_so() {
        for (cmd, signal) in [
            ("echo before; kill -STOP $$; echo after", "SIGSTOP"),
            ("echo before; kill -TSTP 0; echo after", "SIGTSTP"),
        ] {
            let said = ran(cmd).await;
            assert!(
                said.starts_with(&format!("exit: stopped itself with {signal}")),
                "{cmd}: {said}"
            );
            assert_eq!(Exit::of(&said), Some(Exit::Stopped), "{said}");
            assert!(said.contains("before") && !said.contains("after"), "{said}");
        }

        // a job it stopped itself is its own business, and the command goes on to its end
        let said = ran("sleep 30 & kill -STOP $!; echo went on; kill -KILL $!").await;
        assert!(said.starts_with("exit: 0"), "{said}");
        assert!(said.contains("went on"), "{said}");
    }

    /// A command that stops itself after closing its standard output is ended too, and says so.
    ///
    /// note: with the output closed the reading is over, so the stop is found by the wait that is
    /// left, a different place from the one above.
    #[tokio::test]
    async fn a_command_that_stops_itself_with_its_output_closed_is_ended_and_says_so() {
        let said = ran("exec 1>&-; echo before >&2; kill -STOP $$; echo after >&2").await;
        assert!(
            said.starts_with("exit: stopped itself with SIGSTOP"),
            "{said}"
        );
        assert!(said.contains("before") && !said.contains("after"), "{said}");
    }

    /// A call whose arguments never parsed is answered with the advice `ops` gives for the fault,
    /// and with nothing where `ops` has none.
    ///
    /// note: the kernel puts this into the answer the model reads, so an empty string, a
    /// placeholder or a fixed word would be told to the model as the fault.
    #[test]
    fn an_unparsed_command_is_answered_with_the_advice_for_its_fault() {
        assert_eq!(
            unconfined().unparsed("{\"cmd\": \"echo a\nb\"}").as_deref(),
            Some("Write it with a line break inside a string written as `\\n`."),
        );
    }

    /// A shell with no confinement, in the temporary directory.
    fn unconfined() -> Shell {
        Shell {
            policy: Arc::new(Careful::new()),
            workdir: std::env::temp_dir(),
            writable: Vec::new(),
            readable: Vec::new(),
            shell_readable: crate::sandbox::SYSTEM.iter().map(Into::into).collect(),
            devices: crate::sandbox::DEVICES.iter().map(Into::into).collect(),
            confined: false,
            limits: Limits::default(),
            stragglers: Default::default(),
            layout: Layout::Wrapped,
        }
    }

    /// Runs one command, and gives up on it rather than on the suite.
    ///
    /// note: ten seconds was not enough for `yes | head -c 9388608` on a loaded machine - a
    /// mutants run in several worktrees at once - and a test that fails for the load passes a
    /// mutant off as caught. Under the `sleep 30` of the tests that leave something running, so
    /// a call that waits for them is still one that never answers.
    async fn ran(command: &str) -> String {
        let call = ToolCall::new("c1", "shell", serde_json::json!({ "cmd": command }));
        tokio::time::timeout(
            Duration::from_secs(25),
            unconfined().invoke(&call, OutputSink::disconnected()),
        )
        .await
        .unwrap_or_else(|_| panic!("`{command}` never answered"))
        .expect("the tool answers the call either way")
        .content
        .to_text()
        .into_owned()
    }

    /// Runs one command under a session that stops it after `wait`, and gives back what the command
    /// had to say by then.
    ///
    /// note: a session rather than a dropped call, because an interrupt reaches a tool only through
    /// the sink it was given, and `OutputSink::disconnected` discards the question as well as the
    /// answer.
    async fn interrupted(command: &str, wait: Duration) -> String {
        use nachalnik::{
            Config, ContextItem, ContextKind, Kernel, ModelResponse,
            test::{AllowAll, ScriptedProvider},
        };

        let kernel = Kernel::new(Config::default());
        kernel.set_provider(Arc::new(ScriptedProvider::new([
            ModelResponse::tool_calls(vec![ToolCall::new(
                "c1",
                "shell",
                serde_json::json!({ "cmd": command }),
            )]),
            ModelResponse::text("stopped"),
        ])));
        kernel.set_policy(Arc::new(AllowAll));
        kernel.add_tool(Arc::new(unconfined()));
        kernel.push(ContextItem::user("go"));

        let turn = tokio::spawn({
            let kernel = kernel.clone();
            async move { kernel.turn().await }
        });
        tokio::time::sleep(wait).await;
        kernel.interrupt();

        tokio::time::timeout(Duration::from_secs(8), turn)
            .await
            .unwrap_or_else(|_| {
                panic!("a stopped command answers now, not when whatever it started has finished")
            })
            .expect("the turn is not a panic")
            .expect("a stopped command is not a failed one");

        kernel
            .items()
            .into_iter()
            .find_map(|item| match item.kind {
                ContextKind::ToolResult { .. } => Some(item.content.to_text().into_owned()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no result for the call it was stopped in"))
    }

    /// A call dropped while its command runs takes the command's whole group with it.
    ///
    /// note: a call is dropped when the runtime shuts down mid-turn or panics. A group of its own
    /// is not sent the terminal's hangup, so without `Running` the subshell the shell started
    /// finishes and writes the file after nobody is watching.
    #[tokio::test]
    async fn a_dropped_call_takes_its_command_with_it() {
        let dir = std::env::temp_dir().join(format!("kamchatka-dropped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        let late = dir.join("late.txt");
        // in the background, so `kill_on_drop` does not reach the process that would write it;
        // killing `sh` alone leaves this running
        let command = format!("(sleep 1; touch {}) & wait", late.display());
        let call = ToolCall::new("c1", "shell", serde_json::json!({ "cmd": command }));

        let dropped = tokio::time::timeout(
            Duration::from_millis(300),
            unconfined().invoke(&call, OutputSink::disconnected()),
        )
        .await;
        assert!(
            dropped.is_err(),
            "the command was meant to still be running"
        );

        tokio::time::sleep(Duration::from_secs(2)).await;
        let survived = late.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!survived, "the command outlived the call");
    }

    /// Output that is not text is shown rather than stopped at, and what follows it arrives.
    ///
    /// note: data larger than the pipe holds follows the non-UTF-8 byte, which makes this a hang
    /// rather than a loss: a reader that stops at the first such line leaves the command blocked
    /// writing into a full pipe nobody is reading.
    #[tokio::test]
    async fn output_that_is_not_text_is_read_to_the_end() {
        let said = ran(
            "printf '\\377\\n'; head -c 200000 /dev/zero | tr '\\0' a; echo; \
             printf 'refused\\n\\377\\n' >&2",
        )
        .await;

        assert!(
            said.contains('\u{FFFD}'),
            "{}",
            &said[..said.len().min(200)]
        );
        assert!(
            said.contains(&"a".repeat(1_000)),
            "what followed never arrived"
        );
        assert!(
            said.contains("refused"),
            "one byte that is not text cost the whole of standard error"
        );
    }

    /// Output past the ceiling is read to the end and let go, on either stream, even in a line that
    /// never ends. The answer keeps the start and says how much went at the top.
    ///
    /// note: read to the end rather than stopped at, which the exit status shows: `head` finishes
    /// and the command succeeds, where a reader that stopped would leave it blocked on a full pipe.
    #[tokio::test]
    async fn output_past_the_ceiling_is_read_and_not_kept() {
        let past = KEPT + 1_000_000;
        for command in [
            format!("yes | head -c {past}"),
            format!("yes | head -c {past} >&2"),
            format!("head -c {past} /dev/zero | tr '\\0' a"),
        ] {
            let said = ran(&command).await;

            // the start of what it said is kept, a line past the ceiling included
            assert!(
                said.contains(&"a".repeat(1_000)) || said.contains("y\ny\n"),
                "{command}: nothing was kept"
            );

            assert!(said.starts_with("exit: 0"), "{command}");
            assert!(
                said.len() <= KEPT + 1_000,
                "{command}: {} bytes",
                said.len()
            );
            let unkept = said.lines().nth(1).unwrap_or_default();
            assert!(
                unkept.contains("not kept"),
                "{command}: said under the status line, not after the output: {unkept}"
            );
        }
    }

    /// Empty lines count toward the ceiling too; each one is one byte kept.
    ///
    /// note: the ceiling was measured against a line without its newline, so a line that was only a
    /// newline never reached it, and `yes ''` grew what was kept without end.
    #[tokio::test]
    async fn empty_lines_past_the_ceiling_are_not_kept() {
        let past = KEPT + 1_000_000;
        let said = ran(&format!("head -c {past} /dev/zero | tr '\\0' '\\n'")).await;

        assert!(said.starts_with("exit: 0"), "{}", &said[..200]);
        assert!(said.len() <= KEPT + 1_000, "{} bytes", said.len());
        let unkept = said.lines().nth(1).unwrap_or_default();
        assert!(unkept.contains("not kept"), "{unkept}");
    }

    /// Standard error that is not text is held to the ceiling as text, cut where a character
    /// ends, and every byte the command wrote is either kept or counted.
    ///
    /// note: held to the ceiling as bytes while it is read, standard error grows on the way to
    /// text - a byte that is not UTF-8 is the three of `�` - so it is held to it again there, and
    /// the ceiling falls inside a character.
    #[tokio::test]
    async fn standard_error_that_is_not_text_is_cut_between_characters() {
        let said = ran(&format!("head -c {KEPT} /dev/zero | tr '\\0' '\\377' >&2")).await;

        let errors = said
            .split_once("--- stderr ---\n")
            .and_then(|(_, rest)| rest.split_once("\n--- stdout ---"))
            .map(|(errors, _)| errors)
            .unwrap_or_else(|| panic!("no standard error: {}", &said[..200]));
        assert!(errors.len() <= KEPT, "{} bytes kept", errors.len());
        let unkept = said.lines().nth(1).unwrap_or_default();
        // and what was not kept is counted in the bytes the command wrote, a third of the text
        // each of these became
        assert!(
            unkept.starts_with(&format!("[{} more bytes", KEPT - errors.len() / 3)),
            "{unkept}"
        );
    }

    /// A command whose processes outlived the kill is not among those named as stopped.
    ///
    /// note: the partition alone, since a process that outlives `SIGKILL` - one waiting on a device
    /// in the kernel, or a signal refused - is not one a test here can make.
    #[test]
    fn what_outlived_the_kill_is_not_said_to_be_stopped() {
        let left = BTreeMap::from([(1, vec![4_242])]);
        let said: Vec<usize> = stopped([0, 1, 2].into_iter(), &left).collect();
        assert_eq!(said, [0, 2]);
        assert_eq!(
            stopped([0, 2].into_iter(), &BTreeMap::new()).collect::<Vec<_>>(),
            [0, 2]
        );
    }

    /// What a signal is waited out for is looked for again until it is gone, and no longer.
    ///
    /// note: a process takes a signal when it is next scheduled, and one waiting in the kernel only
    /// when the wait ends, so the first look after a signal can still find what is on its way out.
    /// The look is handed in, which is how a test makes one that finds a process for a while.
    #[test]
    fn what_a_signal_is_waited_out_for_is_looked_for_until_it_is_gone() {
        let found = || BTreeMap::from([(1, vec![4_242])]);

        // gone on the fourth look, well within the wait
        let mut looks = 0;
        let left = settled(
            || {
                looks += 1;
                match looks {
                    4.. => BTreeMap::new(),
                    _ => found(),
                }
            },
            Duration::from_secs(2),
        );
        assert!(left.is_empty(), "said to be still there: {left:?}");
        assert_eq!(looks, 4, "looked for again after it was gone");

        // gone at once, and not looked for again
        let mut looks = 0;
        let left = settled(
            || {
                looks += 1;
                BTreeMap::new()
            },
            Duration::from_secs(2),
        );
        assert!(left.is_empty());
        assert_eq!(looks, 1, "looked for again with nothing found");

        // and never gone, which is given up on once the wait is over
        let started = std::time::Instant::now();
        let left = settled(found, Duration::from_millis(100));
        assert_eq!(left, found(), "what is still there is said");
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the wait was not over"
        );
    }

    /// Standard output that is not text is counted as standard error is: in the bytes written.
    #[tokio::test]
    async fn standard_output_that_is_not_text_is_counted_in_bytes_written() {
        let said = ran(&format!("head -c {KEPT} /dev/zero | tr '\\0' '\\377'")).await;

        let output = said
            .split_once("--- stdout ---\n")
            .map(|(_, output)| output.trim_end_matches('\n'))
            .unwrap_or_else(|| panic!("no standard output: {}", &said[..200]));
        let kept = output
            .chars()
            .filter(|c| *c == char::REPLACEMENT_CHARACTER)
            .count();
        assert!(kept > 0, "{}", &said[..200]);
        let unkept = said
            .lines()
            .find(|line| line.contains("not kept"))
            .unwrap_or_default();
        assert!(
            unkept.starts_with(&format!("[{} more bytes", KEPT - kept)),
            "{unkept}"
        );
    }

    /// A cut in the text is taken back to the bytes it came from, a run that is not text being
    /// the three bytes of `�` there and however many it was here.
    #[test]
    fn a_cut_in_the_text_is_a_count_of_bytes_read() {
        let read = b"ab\xff\xfecd";
        // "ab", a `�` for each of the two bytes that are not text, then "cd": ten bytes of text
        assert_eq!(String::from_utf8_lossy(read).len(), 10);
        assert_eq!(read_into(read, 0), 0);
        assert_eq!(read_into(read, 2), 2);
        assert_eq!(read_into(read, 4), 2, "half a replacement is none of it");
        assert_eq!(read_into(read, 5), 3);
        assert_eq!(read_into(read, 8), 4);
        assert_eq!(read_into(read, 10), 6);
        assert_eq!(read_into("zażółć".as_bytes(), 3), 3);
    }

    /// What a command said went wrong comes before what it printed, where a limit cutting from the
    /// end cannot take it.
    #[tokio::test]
    async fn standard_error_survives_a_long_standard_output() {
        let said = ran("seq 1 10000; echo 'FATAL: config key foo missing' >&2; exit 1").await;

        let mut content = nachalnik::Content::text(said);
        content.truncate_to(1_000).expect("it is over the limit");
        let said = content.to_text();
        assert!(said.contains("FATAL"), "{said}");
    }

    /// A command stopped in the middle of a line answers with that line too.
    ///
    /// note: the answer marks what follows as what the command said by then. An endless progress
    /// line such as `printf 'downloading 45%'` is what a command stopped for being slow said. It
    /// was dropped, and the answer under that sentence was empty.
    #[tokio::test]
    async fn a_stopped_command_keeps_the_line_it_was_in_the_middle_of() {
        let said = interrupted(
            "printf 'downloading 45%%'; sleep 30",
            Duration::from_millis(1_500),
        )
        .await;

        assert!(said.starts_with("exit: stopped"), "{said}");
        assert!(said.contains("downloading 45%"), "{said}");
    }

    /// The description says what the stance means for the network, for each answer it can have.
    ///
    /// note: it is said in the description, not at the point of failure, because the point of
    /// failure cannot say it. A refused socket is `Permission denied`; a refused name lookup is
    /// `Temporary failure in name resolution`, which reads as network trouble. A model that does
    /// not know it was refused looks for another way out. The two sentences are not
    /// interchangeable: one refuses outright, one waits for a person to be asked. Each is checked
    /// against the stance that earns it.
    ///
    /// note: a gated session with nothing said about the network stands at `ask`, which is what
    /// `Careful` answers for anything undecided; the gate turns that into a question rather than a
    /// refusal. See `Careful::gate_the_network`.
    #[test]
    fn the_description_says_what_the_stance_makes_of_the_network() {
        let said = |gated: bool, stance: Verdict| {
            let policy = Arc::new(Careful::new());
            if gated {
                policy.gate_the_network();
            }
            policy.set(&Subject::Capability(Capability::net("reach")), stance);
            Shell {
                policy,
                workdir: std::env::temp_dir(),
                writable: Vec::new(),
                readable: Vec::new(),
                shell_readable: crate::sandbox::SYSTEM.iter().map(Into::into).collect(),
                devices: crate::sandbox::DEVICES.iter().map(Into::into).collect(),
                // the sentence is only written where there is a confinement to be confined by
                confined: true,
                limits: Limits::default(),
                stragglers: Default::default(),
                layout: Layout::Wrapped,
            }
            .spec()
            .description
        };

        for (gated, stance, said_of_it) in [
            // the gate refuses it outright, and nobody is asked
            (true, Verdict::Deny, "it has no network"),
            // the gate holds it, and the first attempt waits for a person
            (
                true,
                Verdict::Ask,
                "it waits while the person you are working with is asked",
            ),
        ] {
            let said = said(gated, stance);
            assert!(said.contains(said_of_it), "{gated}/{stance:?}: {said}");
        }

        // the two hold-outs are told apart; that is the point of the wording. A session that
        // refuses outright does not wait for anybody
        let shut = said(true, Verdict::Deny);
        assert!(!shut.contains("waits while the person"), "{shut}");

        // an outright allowance says nothing, because there is nothing for a model to be told
        let open = said(true, Verdict::Allow);
        assert!(
            !open.contains("no network") && !open.contains("TCP may be closed"),
            "{open}"
        );
    }

    /// A line that exactly fills the window a read is given arrives whole, newline and all.
    ///
    /// note: the window is one byte past the ceiling, so a line filling it exactly - newline
    /// included - is read in one go and kept as one line. A window a byte short splits the line at
    /// its last byte: the read returns what it had, `keep` writes the line and a newline, and the
    /// command's newline is read again as a second line. A caller sees two lines where there was
    /// one, and the ceiling's bytes become ceiling plus one.
    ///
    /// note: `tr` rather than one `write`, because that is what makes the two differ. A command
    /// that writes its whole line at once has it in the pipe before the first read is made, so the
    /// window is filled by data that is already there and the cap decides nothing; a pipeline hands
    /// it over as it is produced, and a heartbeat falls inside the drain.
    #[tokio::test]
    async fn a_line_filling_the_room_a_read_is_given_arrives_whole() {
        let said = ran(&format!(
            "head -c {} /dev/zero | tr '\\0' a; echo",
            KEPT - 1
        ))
        .await;

        let stdout = said
            .split_once("\n--- stdout ---\n")
            .map(|(_, stdout)| stdout)
            .unwrap_or_else(|| panic!("no standard output: {}", &said[..said.len().min(200)]));
        assert_eq!(
            stdout.len(),
            KEPT,
            "a line of {} bytes and its newline is one line, not two: {stdout:?}",
            KEPT - 1
        );
        assert!(
            !said.contains("not kept"),
            "a line that fits the ceiling is not reported as over it: {said}"
        );
    }

    /// A command that could not be started is answered with the reason it could not, and only that
    /// reason's own advice comes with it.
    ///
    /// note: the advice for a command too long to start says to write it to a file and run the
    /// file, which is right for an argument list the kernel refused and wrong for a working
    /// directory that is not there; a model told to write a 200000-byte command to a file, when nothing about the
    /// command was the problem, goes looking for a length it cannot find. The two refusals are told
    /// apart by the kind of the error behind them, so a test that only ever started a command that
    /// was too long never saw the other one.
    ///
    /// note: a missing working directory is how to reach it. The spawn fails before a single byte
    /// is run, so nothing of the command's own output can be mistaken for the reason.
    #[tokio::test]
    async fn a_command_that_could_not_start_is_not_told_it_was_too_long() {
        let said = {
            let call = ToolCall::new("c1", "shell", serde_json::json!({ "cmd": "echo ran" }));
            let missing = std::env::temp_dir().join("kamchatka-no-such-workdir");
            Shell {
                workdir: missing.clone(),
                ..unconfined()
            }
            .invoke(&call, OutputSink::disconnected())
            .await
            .expect("the tool answers the call either way")
            .content
            .to_text()
            .into_owned()
        };

        assert!(said.contains("could not run the command"), "{said}");
        assert!(
            !said.contains("too long"),
            "a directory that is not there is not an argument list: {said}"
        );
        assert!(!said.contains("write it to a file"), "{said}");
        assert!(!said.contains("ran"), "nothing was run: {said}");
    }

    /// A command too long to start is answered with the reason and what to do instead, and not with
    /// the command, which would push both past any limit.
    #[tokio::test]
    async fn a_command_too_long_to_start_says_why_first() {
        let said = ran(&format!("echo {}", "a".repeat(200_000))).await;

        assert!(said.len() < 500, "{}", &said[..said.len().min(500)]);
        assert!(said.contains("write it to a file"), "{said}");
    }

    /// A line past the ceiling keeps its start, and what went is counted to the byte.
    ///
    /// note: the line ends in three-byte characters, and the ceiling falls inside the first one, so
    /// the kept start ends where a character does rather than at the ceiling.
    #[test]
    fn a_line_past_the_ceiling_keeps_its_start() {
        let line = [vec![b'x'; KEPT - 2], "€€€".as_bytes().to_vec()].concat();
        let (mut collected, mut full, mut dropped) = (String::new(), false, 0);
        keep(
            &line,
            &mut collected,
            &OutputSink::disconnected(),
            &mut full,
            &mut dropped,
        );

        assert!(full);
        assert!(collected.starts_with("xxx") && collected.len() <= KEPT);
        assert_eq!(collected.trim_end().len() + dropped, line.len());
    }

    /// A line that exactly fills the ceiling is kept whole, and does not fill it for the next one.
    #[test]
    fn a_line_that_exactly_fills_the_ceiling_is_kept_whole() {
        // the room for a line is the ceiling less the newline it is kept with
        let line = [vec![b'x'; KEPT - 1], b"\n".to_vec()].concat();
        let (mut collected, mut full, mut dropped) = (String::new(), false, 0);
        keep(
            &line,
            &mut collected,
            &OutputSink::disconnected(),
            &mut full,
            &mut dropped,
        );

        assert_eq!((collected.len(), full, dropped), (KEPT, false, 0));
    }

    /// A line whose bytes fill the ceiling leaves no room for the newline it is kept with, so its
    /// last byte goes and the newline takes its place.
    ///
    /// note: the ceiling counts the newline too. A line of `KEPT` bytes with no newline of its own
    /// fills the ceiling as it arrived but not as it is kept; keeping it whole would be `KEPT + 1`
    /// bytes under a budget of `KEPT`. Every other path through this file holds a caller to `KEPT`.
    #[test]
    fn a_line_that_fills_the_ceiling_without_its_newline_is_cut() {
        let line = vec![b'x'; KEPT];
        let (mut collected, mut full, mut dropped) = (String::new(), false, 0);
        keep(
            &line,
            &mut collected,
            &OutputSink::disconnected(),
            &mut full,
            &mut dropped,
        );

        assert_eq!((collected.len(), full, dropped), (KEPT, true, 1));
    }

    /// A command stopped while being waited for, after closing its output, answers at once and says
    /// it was stopped.
    ///
    /// note: the wait is watched on a heartbeat so that this can be answered, and the heartbeat is
    /// where the interrupt is read. A wait that acted on nothing would keep watching a stopped
    /// command for as long as that command said it would run.
    #[tokio::test]
    async fn a_command_stopped_while_being_waited_for_answers_at_once() {
        let said = interrupted("exec 1>&-; sleep 30", Duration::from_millis(500)).await;

        assert!(
            said.starts_with("exit: stopped before it finished"),
            "{said}"
        );
    }

    /// A command nobody asked to stop is not stopped, however long it is quiet.
    ///
    /// note: the wait is watched on a heartbeat rather than left to block, and the heartbeat reads
    /// whether the turn was interrupted. A wait that acted on anything else would stop a command
    /// that was only quiet. `exec 1>&-` puts the wait where the heartbeat is: the reading is over,
    /// and what is left is the command itself.
    #[tokio::test]
    async fn a_quiet_command_nobody_asked_to_stop_is_not_stopped() {
        let said = ran("exec 1>&-; sleep 2; echo late >&2").await;

        assert!(said.starts_with("exit: 0"), "{said}");
        assert!(said.contains("late"), "{said}");
    }

    /// The end of a command's standard output is not the end of its standard error. A command that
    /// closed its output and kept running has the rest of its error kept.
    ///
    /// note: `exec 1>&-` is the note beside the loop this is about: the end of the output is not
    /// the end of the command, so the command is waited for on its own rather than while reading
    /// it. A wait skipped the moment reading stops is the same wait held to no heartbeat at all;
    /// standard error is drained after it and cut by one, so the error a command says after it
    /// closed its output is exactly the error this would lose.
    #[tokio::test]
    async fn what_a_command_says_after_closing_its_output_is_kept() {
        let said = ran("exec 1>&-; sleep 1; echo first >&2; sleep 1; echo second >&2").await;

        assert!(said.starts_with("exit: 0"), "{said}");
        assert!(said.contains("first"), "{said}");
        assert!(
            said.contains("second"),
            "the last of its standard error: {said}"
        );
    }

    /// A command that finishes and leaves something running still answers.
    ///
    /// note: the background job inherits standard error, and standard output unless it is sent
    /// elsewhere, so the pipes stay open for as long as it runs. A call that waits for the end of
    /// them never answers for a server started with `&`.
    #[tokio::test]
    async fn a_command_that_leaves_something_running_still_answers() {
        let said = ran("sleep 30 > /dev/null & echo started").await;

        assert!(said.starts_with("exit: 0"), "{said}");
        assert!(said.contains("started"), "{said}");
        assert!(said.contains("standard error is still open"), "{said}");

        let said = ran("sleep 30 & printf 'started\\nno newline'").await;

        assert!(said.starts_with("exit: 0"), "{said}");
        assert!(said.contains("started\nno newline"), "{said}");
        assert!(said.contains("standard output is still open"), "{said}");

        // and only where something was left holding the pipe: a command whose standard error
        // ended with it says nothing about one that did not, or the note is on every result and
        // the model reads it as a claim about this one
        let said = ran("echo complained >&2").await;

        assert!(said.contains("complained"), "{said}");
        assert!(!said.contains("still open"), "{said}");
    }

    /// The last line of the output is kept when it has no newline and the command was quiet
    /// before it ended.
    ///
    /// note: a read that times out holds what it had for the next one, and the next one meets
    /// the end of the output with nothing of its own to return.
    #[tokio::test]
    async fn an_unfinished_last_line_is_kept_after_a_quiet_moment() {
        let said = ran("printf 'first\\nno newline'; sleep 0.5").await;

        assert!(said.starts_with("exit: 0"), "{said}");
        assert!(said.contains("first\nno newline"), "{said}");
    }

    /// What a command reported is what its first line says, for every shape the tool writes.
    ///
    /// note: real commands rather than a table of the strings this file writes. A table would keep
    /// agreeing with itself after somebody reworded the status line, the one failure worth catching
    /// here. The `debug_assert` in `invoke` is the same check from the other side, run by every
    /// test in this crate that runs a command.
    ///
    /// note: `kill -9 $$` for the third, because a signal is the only way to leave a status with no
    /// code in it, and SIGKILL is the one a shell cannot decline.
    #[tokio::test]
    async fn what_a_command_reported_is_what_its_first_line_says() {
        let shell = unconfined();

        for (command, meant) in [
            ("exit 0", Exit::Ok),
            ("exit 3", Exit::Failed),
            ("kill -9 $$", Exit::Stopped),
        ] {
            let call = ToolCall::new("c1", "shell", serde_json::json!({ "cmd": command }));
            let output = shell
                .invoke(&call, OutputSink::disconnected())
                .await
                .expect("the tool answers the call either way");

            let said = output.content.to_text();
            let first = said.lines().next().unwrap_or_default();
            assert_eq!(Exit::of(first), Some(meant), "`{command}` said {first:?}");
        }
    }

    /// A result that is not a shell result is not given a colour it did not ask for.
    #[test]
    fn a_line_that_is_not_an_exit_line_reads_as_nothing() {
        for line in [
            "",
            "exit:",
            "exiting: 0",
            "the file says exit: 0",
            "--- stdout ---",
        ] {
            assert_eq!(Exit::of(line), None, "{line:?}");
        }
    }

    /// The pieces a set of joint ranges picks out of a command, for reading a test by.
    fn picked(cmd: &str) -> Vec<&str> {
        joints(cmd)
            .into_iter()
            .map(|(from, to)| &cmd[from..to])
            .collect()
    }

    /// Every joint, and only the joints.
    #[test]
    fn a_commands_own_joints_are_found() {
        assert_eq!(
            picked("cargo build --release 2>&1 | tail -5 && echo done"),
            ["|", "&&"]
        );
        assert_eq!(picked("cd src; ls || true"), [";", "||"]);
        // and they are where they are, not merely how many: the offsets are what gets coloured
        let cmd = "a | b";
        assert_eq!(joints(cmd), vec![(2, 3)]);
        assert_eq!(&cmd[2..3], "|");
        // a `case` arm's `;;` is stepped over whole rather than one `;` at a time, so the
        // command after the arm is still read as a stage of its own
        assert_eq!(picked("case $x in a) echo one;; esac; echo two"), [";"]);
    }

    /// A quote ends where it began, whatever it held in between.
    ///
    /// note: a `\"` in a double-quoted string is an ordinary character, not the end of the string,
    /// so a separator beside it is still inside one. A string read as ending at its first letter is
    /// not a string the scan understood, so a separator after it is not inside one either. Which of
    /// the two was believed decides whether the panel colours a `|` that is really a character of
    /// an argument.
    #[test]
    fn a_quote_ends_where_it_began() {
        assert_eq!(picked("echo \"a | b\" | wc -l"), ["|"]);
        assert_eq!(picked(r#"echo "a\" | b"; ls"#), [";"]);
    }

    /// A separator inside a subshell is the subshell's, and the command it is beside is found.
    #[test]
    fn a_separator_inside_a_subshell_is_not_this_commands() {
        assert!(joints("(cd src; ls)").is_empty());
        assert_eq!(picked("(cd src; ls) && cargo build"), ["&&"]);
        assert_eq!(picked("echo $(date | tr -d '\\n') | wc -l"), ["|"]);
    }

    /// What looks like a joint and is not.
    #[test]
    fn what_is_not_a_joint_is_not_picked_out() {
        assert!(joints("cargo build --release").is_empty());
        assert!(joints("").is_empty());
        // `2>&1` and a job in the background are both a lone `&`, and neither is a seam
        assert!(joints("make 2>&1").is_empty());
        assert!(joints("sleep 60 &").is_empty());
        // a `case` arm's `;;` ends an arm rather than separating two commands
        assert!(joints("case $x in a) echo one;; esac").is_empty());
        // a command with newlines of its own is not scanned: what is inside a heredoc is arbitrary
        // text, and the `|` in a Python expression is not a pipe
        assert!(joints("python3 - <<'PY'\nprint(1 | 2)\nPY").is_empty());
    }

    /// A separator that is part of an argument is not a joint, whichever way it was quoted.
    ///
    /// note: a quoted separator must be invisible to the scan. Otherwise the panel would show the
    /// `|` in `echo 'a | b'` as a pipe on the screen where the user decides whether to run the
    /// command.
    #[test]
    fn a_separator_inside_a_quote_is_not_a_joint() {
        assert!(joints("echo 'a | b'").is_empty());
        assert!(joints("echo \"a && b\"").is_empty());
        assert!(joints(r"echo a\;b").is_empty());
        // the inner command's joint is the inner command's
        assert!(joints("echo $(date | tr -d '\\n')").is_empty());
        assert!(joints("echo `date | wc -c`").is_empty());

        // and the real one beside a quoted one is still found
        assert_eq!(picked("grep -e '|' file | wc -l"), ["|"]);
    }

    /// A command this cannot read to the end has nothing picked out of it.
    #[test]
    fn an_unreadable_command_is_left_plain() {
        assert!(joints("echo 'unterminated | still going").is_empty());
        assert!(joints("echo \"unterminated && more\" | wc -l\"").is_empty());
        assert!(joints("make | tail -1 $(echo").is_empty());
        assert!(joints("make | tail -1 \\").is_empty());
    }
}
