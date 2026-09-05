//! The four tools, the permission policy and the compactor - all of it ordinary user code.
//!
//! note: The runtime ships none of this. It has no idea what a file is, it spawns no processes,
//! and it never decides that something may run. What it provides is the shape: a [`Tool`] that
//! declares what it needs, a [`PermissionPolicy`] that is asked before anything happens, and a
//! [`Compactor`] whose plan is applied in the open and can be undone.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use nachalnik::{
    BoxError, Budget, Capability, CompactionPlan, Compactor, ContextItem, ContextKind, OutputSink,
    PermissionPolicy, PermissionRequest, Tool, ToolCall, ToolCallId, ToolOutput, ToolSpec, Verdict,
    async_trait,
};
use parking_lot::Mutex;

use crate::sandbox::{Reach, Sandbox};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

/// How long a running command may say nothing before the tool looks up to check whether it has
/// been asked to stop.
const HEARTBEAT: Duration = Duration::from_millis(120);

// ---------------------------------------------------------------------------------- the tools

/// Reads the named argument, or explains which one is missing.
fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, BoxError> {
    args[name]
        .as_str()
        .ok_or_else(|| format!("the `{name}` argument is required").into())
}

/// Returns the four tools a terminal agent needs to be worth talking to, all held to `reach`.
pub fn builtin(shell: Shell, reach: Reach) -> Vec<Arc<dyn Tool>> {
    let reach = Arc::new(reach);
    vec![
        Arc::new(Read(reach.clone())),
        Arc::new(Write(reach.clone())),
        Arc::new(Edit(reach)),
        Arc::new(shell),
    ]
}

struct Read(Arc<Reach>);

#[async_trait]
impl Tool for Read {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "read",
            "reads a whole text file. Long files are cut off at the end; a shell command is the \
             way to read part of one.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "absolute, or relative to the working directory",
                },
            },
            "required": ["path"],
        }))
        .with_capabilities([Capability::Read])
        .with_output_limit(32_000)
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let path = match self.0.allows(arg(&call.args, "path")?) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };

        // a failure the model should read and react to, rather than one that stops the loop
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(ToolOutput::new(content)),
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}

struct Write(Arc<Reach>);

#[async_trait]
impl Tool for Write {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "write",
            "writes a whole file. An existing one is replaced entirely, so `edit` is the safer \
             way to change part of one.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "absolute, or relative to the working directory",
                },
                "content": { "type": "string", "description": "the whole of the new file" },
            },
            "required": ["path", "content"],
        }))
        .with_capabilities([Capability::Write])
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let (path, content) = (arg(&call.args, "path")?, arg(&call.args, "content")?);
        let path = match self.0.allows(path) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };

        match tokio::fs::write(&path, content).await {
            Ok(()) => Ok(ToolOutput::new(format!(
                "wrote {} bytes to {}",
                content.len(),
                path.display()
            ))),
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}

struct Edit(Arc<Reach>);

#[async_trait]
impl Tool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "edit",
            "replaces the first occurrence of `old` with `new` in a file. Nothing is written if \
             `old` is not there.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "absolute, or relative to the working directory",
                },
                "old": {
                    "type": "string",
                    "description": "the exact text to replace, whitespace included; include \
                                    enough of the surrounding lines to make it the only match",
                },
                "new": { "type": "string", "description": "what to put there instead" },
            },
            "required": ["path", "old", "new"],
        }))
        .with_capabilities([Capability::Edit])
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let (old, new) = (arg(&call.args, "old")?, arg(&call.args, "new")?);
        let path = match self.0.allows(arg(&call.args, "path")?) {
            Ok(path) => path,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };

        let before = match tokio::fs::read_to_string(&path).await {
            Ok(before) => before,
            Err(e) => return Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        };
        let Some(at) = before.find(old) else {
            return Ok(ToolOutput::error(format!(
                "`old` does not occur in {}",
                path.display()
            )));
        };

        let after = format!("{}{new}{}", &before[..at], &before[at + old.len()..]);
        match tokio::fs::write(&path, after).await {
            Ok(()) => Ok(ToolOutput::new(format!(
                "replaced one occurrence in {}",
                path.display()
            ))),
            Err(e) => Ok(ToolOutput::error(format!("{}: {e}", path.display()))),
        }
    }
}

/// Runs a command, reporting its output as it arrives and stopping when asked to.
///
/// note: This is the tool that shows what an [`OutputSink`] is for. Every line goes to the sink
/// the moment it is read, so a command that takes a minute is visible for that minute rather
/// than appearing all at once at the end; and between lines it asks whether somebody has pressed
/// escape, in which case the child is killed and the call still answers - with what it got.
/// Runs a command, under whatever confinement the policy's stances add up to.
///
/// note: it holds the policy rather than being handed a verdict, because the kernel's answer is
/// only whether the call may run at all. What it may *reach* is a second question, and the policy
/// is the thing that knows: see [`Sandbox::of`](crate::sandbox::Sandbox::of).
pub struct Shell {
    /// The stances the confinement is built from.
    pub policy: Arc<Careful>,
    /// The directory a command may work in.
    pub workdir: PathBuf,
    /// Extra paths the user asked to open up.
    pub extra: Vec<PathBuf>,
    /// The binary that knows how to confine itself and run a command; `None` runs `sh` directly.
    ///
    /// note: a path settled once at startup rather than `current_exe()` per call, for two
    /// reasons. On Linux `current_exe()` reads `/proc/self/exe`, and a binary replaced while the
    /// program is running - `cargo build` in the very repository it is working on, an upgrade -
    /// makes that a path ending in ` (deleted)`, so every command comes back
    /// `No such file or directory` and nothing on screen accounts for it. And this crate is a
    /// library as well as a program: `current_exe()` in somebody else's process is somebody
    /// else's binary, which would be handed `--confine-and-run` and would make of it whatever it
    /// liked.
    pub confiner: Option<PathBuf>,
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        // note: the confinement is said out loud only when there is one. A command stopped by
        // Landlock comes back with an ordinary permission error and nothing to distinguish it
        // from a file that really is protected, and a model that cannot tell those apart spends
        // its turns trying `sudo`
        ToolSpec::new(
            "shell",
            format!(
                "runs one command with `sh -c` in the working directory and returns its exit \
                 status, its output and its errors. Long output is cut off at the end. Nothing \
                 is typed at it: a command that waits for input waits for ever.{}",
                match self.confiner.is_some() {
                    true =>
                        " It runs confined: outside the working directory it can read this \
                         machine's system paths and no more, and TCP may be closed - so a \
                         permission error there is the confinement rather than the command.",
                    false => "",
                }
            ),
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "cmd": {
                    "type": "string",
                    "description": "the command line, as a shell would read it",
                },
            },
            "required": ["cmd"],
        }))
        .with_capabilities([Capability::Shell])
        .with_output_limit(32_000)
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let cmd = arg(&call.args, "cmd")?;

        // what the command may reach, which is a different question from whether it may run: the
        // kernel answered that one before this was called
        let mut command = match &self.confiner {
            Some(me) => {
                let sandbox = Sandbox::of(
                    &self.policy,
                    self.workdir.clone(),
                    self.extra.clone(),
                    self.policy.was_granted_the_network(&call.id),
                );
                let mut command = tokio::process::Command::new(me);
                command.args(sandbox.argv(cmd));
                command
            }
            None => {
                let mut command = tokio::process::Command::new("sh");
                command.arg("-c").arg(cmd).current_dir(&self.workdir);
                command
            }
        };

        // a group of its own, so that stopping the command can stop everything the command
        // started; see `stop`
        #[cfg(unix)]
        command.process_group(0);

        let mut child = match command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => return Ok(ToolOutput::error(format!("could not run `{cmd}`: {e}"))),
        };
        // the confined child cannot remove its own temporary directory, so this is where that
        // happens; the identifier has to be read now, because a child that has been waited on no
        // longer has one. See `sandbox::scratch_for`
        let scratch = self
            .confiner
            .is_some()
            .then(|| child.id().map(crate::sandbox::scratch_for))
            .flatten();

        // taken out of the child, so that it can still be killed while these are being read
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let mut collecting_stderr = tokio::spawn(async move {
            let mut collected = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut collected).await;

            collected
        });

        let mut lines = BufReader::new(stdout).lines();
        let mut collected = String::new();
        let mut interrupted = false;
        loop {
            // the timeout is what makes a command that says nothing at all interruptible; without
            // it this would sit in `next_line` until the child felt like talking
            match tokio::time::timeout(HEARTBEAT, lines.next_line()).await {
                Ok(Ok(Some(line))) => {
                    output.push(format!("{line}\n"));
                    collected.push_str(&line);
                    collected.push('\n');
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    collected.push_str(&format!("\n[could not read the output: {e}]\n"));
                    break;
                }
                Err(_) => {}
            }

            if output.is_interrupted() {
                stop(&mut child).await;
                interrupted = true;
                break;
            }
        }

        // note: a killed shell can leave children of its own behind - `sleep 60; echo done` is
        // two processes - and they hold the standard error this is reading. Waiting for the end of
        // it would mean a stopped call that answers a minute after it was stopped, which is the
        // one thing `esc` is for. What has arrived by now is what the model is told
        let errors = match interrupted {
            true => match tokio::time::timeout(HEARTBEAT, &mut collecting_stderr).await {
                Ok(collected) => collected.unwrap_or_default(),
                Err(_) => {
                    collecting_stderr.abort();
                    String::new()
                }
            },
            false => collecting_stderr.await.unwrap_or_default(),
        };
        // note: not `ExitStatus`'s own `Display`, which renders `exit status: 0` and made the
        // first line of every result read `exit: exit status: 0`. What a reader wants from this
        // line is the number, and whether it means the command worked
        //
        // note: and whether it was stopped, which used to be a bracketed line after the standard
        // error instead. An output limit cuts from the end, so a stopped command that had said
        // more than the limit lost the one line explaining why its output stops mid-sentence -
        // the truncation marker took its place, and the model was told the wrong thing about
        // what it was reading. The first line survives anything
        let waited = child.wait().await;
        let status = match (interrupted, waited) {
            (true, _) => "exit: stopped before it finished, at the request of the person you are \
                          working with; what is below is what it had said by then"
                .to_owned(),
            (false, Ok(status)) => match status.code() {
                Some(0) => "exit: 0".to_owned(),
                Some(code) => format!("exit: {code} (the command reported a failure)"),
                None => format!("exit: {status} (the command was killed)"),
            },
            (false, Err(e)) => format!("exit: unknown ({e})"),
        };
        if let Some(scratch) = scratch {
            let _ = tokio::fs::remove_dir_all(scratch).await;
        }

        let text = format!("{status}\n--- stdout ---\n{collected}\n--- stderr ---\n{errors}");

        Ok(ToolOutput::new(text))
    }
}

/// Stops a running command, and whatever that command started.
///
/// note: the group rather than the process, because a shell command is rarely one process. `make`
/// starts a compiler, `npm test` starts a runner, and a signal addressed to the shell alone leaves
/// those running - confined, since the domain is inherited, but still writing in the working
/// directory with nothing left that will ever report them. Somebody who pressed escape has been
/// told it stopped.
///
/// note: through `sh`, because signalling a *group* is not in `std` and this workspace has no
/// `unsafe` and no libc to reach past it with. It is the same `sh` the tool is built on, so it
/// brings nothing new into the program. The group is the child's own - `process_group(0)` asked
/// for that at spawn - and the child has not been waited on yet, so the identifier cannot yet mean
/// anybody else.
#[cfg(unix)]
async fn stop(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(format!("kill -KILL -{pid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    // and the child itself, in case it was never in a group of its own
    let _ = child.start_kill();
}

/// The same, where there are no process groups.
#[cfg(not(unix))]
async fn stop(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
}

// --------------------------------------------------------------------------------- the policy

/// Something [`Careful`] holds an opinion about.
///
/// note: two kinds, because a capability is not fine enough on its own. `read: allow` is a
/// reasonable thing to want and `read .env: allow` is not, and the difference is a property of the
/// *file* rather than of the tool that opened it - which is why a path rule is one subject rather
/// than three, and binds `read`, `write` and `edit` alike.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subject {
    /// A class of side effect a tool declares.
    Capability(Capability),
    /// A pattern the path a tool was handed is matched against.
    Path(String),
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(capability) => write!(f, "{capability}"),
            Self::Path(pattern) => write!(f, "{pattern}"),
        }
    }
}

/// The paths a fresh policy has something to say about.
///
/// note: a short list of the names that are credentials by convention, and every one of them is
/// `ask` rather than `deny`, like everything else here. What they are for is the moment somebody
/// answers `always` for `read`: the capability goes to `allow` and these stay where they are, so
/// reading `src/main.rs` goes silent and reading `.env` is still a question. A rule finer than a
/// capability is only worth having once the capability is open.
const SUSPECT: &[&str] = &[
    ".env*",
    "*.pem",
    "*.key",
    "*.p12",
    "id_rsa*",
    "id_ed25519*",
    "*credentials*",
    "secrets/",
    ".ssh/",
    ".aws/",
    ".gnupg/",
];

/// Whether a path is one this pattern is about.
///
/// note: a pattern ending in `/` is a directory: it matches a path with that component anywhere in
/// it. Anything else is matched against the file name, with `*` standing for any run of
/// characters. That is less than a glob crate would give and it is what these rules need; a
/// pattern language nobody can predict is worse on a permissions screen than a small one.
///
/// note: the path is read as a [`Path`] rather than split on `/`, so that the name a rule is
/// matched against is the name the file will actually be opened under. It is the same string
/// [`Reach::allows`](crate::sandbox::Reach::allows) resolves, and the two used to disagree about
/// the simplest thing there is: `.env/` has no last component when it is split on slashes, so no
/// rule matched it, while resolving it produced `.env` and read it. A trailing slash, a `.` in the
/// middle, a doubled separator - none of them changes which file is meant, and none of them may
/// change which rule applies.
///
/// note: what this cannot see is a *symlink*. A rule is about a name, and a name that resolves
/// somewhere else resolves after this has answered. The boundary that does not care about names
/// is the sandbox, which is the kernel's - see [`crate::sandbox`].
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let path = path.replace('\\', "/");
    let path = Path::new(&path);

    if let Some(directory) = pattern.strip_suffix('/') {
        return path
            .components()
            .any(|component| component.as_os_str() == directory);
    }

    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => glob(pattern, name),
        None => false,
    }
}

/// Whether a name matches a pattern in which `*` stands for any run of characters.
///
/// note: it backtracks, which the first version did not: it walked the pattern's literals with
/// `find` and took the first hit, so `a*bc` refused `abcbc` - the `bc` it found was the one the
/// star should have swallowed, and there was no way back. A permission rule that silently fails to
/// match is the worst way for one to be wrong, and `*credentials*.json` is not an exotic thing to
/// write.
///
/// note: over bytes rather than characters. Both sides are `str`, so equal bytes are equal
/// characters, and a `*` landing mid-character can only ever be a position the match moves past.
fn glob(pattern: &str, name: &str) -> bool {
    let (pattern, name) = (pattern.as_bytes(), name.as_bytes());
    let (mut p, mut n) = (0, 0);
    // where the last `*` was, and how much of the name it has been asked to swallow so far
    let (mut star, mut swallowed) = (None, 0);

    while n < name.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some(p);
                swallowed = n;
                p += 1;
            }
            Some(c) if *c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some(at) => {
                    p = at + 1;
                    swallowed += 1;
                    n = swallowed;
                }
                None => return false,
            },
        }
    }

    // whatever is left of the pattern has to be stars, which match the empty rest
    while pattern.get(p) == Some(&b'*') {
        p += 1;
    }

    p == pattern.len()
}

/// Everything is a question until the person at the terminal answers one - including reading, and
/// including a handful of paths that look like credentials.
///
/// note: Capabilities are the unit rather than tool names, which is what makes this work for
/// tools this crate has never heard of. An MCP server's tools all carry a `mcp:<server>`
/// capability, so answering "always" to one of them is answering for that server, and only that
/// server.
///
/// note: The state is a map from a capability to the [`Verdict`] this will return for it, rather
/// than a set of the allowed ones and a hard-coded refusal for the network. Same behaviour, but
/// every one of those answers is now a value somebody can look at and change - which is what the
/// permissions tab is drawing. A policy whose decisions can only be observed by triggering them
/// is not much of a demonstration of a replaceable policy.
///
/// note: a capability is not fine enough on its own. `read: allow` is a reasonable thing to want
/// and `read .env: allow` is not, so there is a second kind of [`Subject`]: a pattern the *path* a
/// tool was handed is matched against. The strictest of everything consulted wins, so a rule can
/// only tighten what a capability allows - `read` stays `allow` and `.env` becomes a question.
///
/// note: those rules bind `read`, `write` and `edit`, and deliberately not `shell`. A command
/// names its files inside a string, and `cat .env`, `sed -n 1p .env`, `python -c "open('.env')"`
/// and `base64 <.env` are the same act written four ways: a check over that string would refuse
/// the first and wave the rest through while looking like a rule. What binds a command is the
/// kernel, and what the kernel can express is a directory - see [`crate::sandbox`]. So `cat .env`
/// works where `read .env` asks, and that is the honest shape of it rather than an oversight.
pub struct Careful {
    stances: Mutex<BTreeMap<Capability, Verdict>>,
    /// What it answers about paths matching a pattern, in the order they are consulted.
    ///
    /// note: ordered rather than a map, because these are read out on a screen and somebody
    /// adding one wants it where they put it. The strictest match wins regardless, so the order
    /// is for the reader rather than for the answer.
    paths: Mutex<Vec<(String, Verdict)>>,
    /// The calls a person was asked about and allowed, whose command reaches for the network.
    ///
    /// note: a stance of `ask` answered `yes, once` is permission for *that call*, and the
    /// sandbox has to know or the command runs with the network cut and fails in a way that
    /// contradicts what the person was just told. A stance is what the tab draws; this is the
    /// answer to a question, which the tab never sees.
    networked: Mutex<BTreeSet<ToolCallId>>,
    /// Why the last few refusals were refused, by the call they refused.
    ///
    /// note: the policy is the only thing that knows this, and nothing carries it out: the
    /// kernel is handed a `Verdict` and records `the call was not permitted`, which is true and
    /// unhelpful when the tool's own capability is `allow` and something else refused it. That is
    /// exactly the `shell: allow` / `network: deny` pair, and a refusal nobody can account for is
    /// the one thing this program is not for. So it is written down here, where it is known, and
    /// [`Careful::why`] hands it out.
    refusals: Mutex<BTreeMap<ToolCallId, String>>,
}

impl Default for Careful {
    fn default() -> Self {
        Self::new()
    }
}

impl Careful {
    /// Builds a policy that allows reads and asks about everything else, including a short list of
    /// paths that look like credentials.
    pub fn new() -> Self {
        Self {
            // note: empty, and `stance` answers `ask` for anything it does not find. Nothing is
            // decided on somebody's behalf - not `read`, which was `allow` here and is the one
            // people would have picked, and not `network`, which was `deny`. Both were decisions
            // taken for the user about things they may perfectly well want, and the sandbox is
            // what makes either answer mean something once they have given it
            stances: Mutex::new(BTreeMap::new()),
            paths: Mutex::new(
                SUSPECT
                    .iter()
                    .map(|pattern| ((*pattern).to_owned(), Verdict::Ask))
                    .collect(),
            ),
            networked: Mutex::new(BTreeSet::new()),
            refusals: Mutex::new(BTreeMap::new()),
        }
    }

    /// Everything this policy consults about one call, in the order it reads them out.
    ///
    /// note: the declared capabilities, plus the two things only the arguments can say - that a
    /// command reaches for the network, and that a path is one there is a rule about. It is one
    /// list rather than three checks because everything downstream wants the same thing: the
    /// question asks about these, `always` answers for these, and a refusal is blamed on whichever
    /// of these said no. A one-off `yes` to a `curl` that then ran with the network cut is the
    /// shape of bug that comes of having three of them.
    pub fn judges(&self, request: &PermissionRequest) -> Vec<Subject> {
        let mut judged: Vec<Subject> = request
            .capabilities
            .iter()
            .cloned()
            .map(Subject::Capability)
            .collect();

        if request.capabilities.contains(&Capability::Shell)
            && command(&request.args).is_some_and(reaches_the_network)
        {
            judged.push(Subject::Capability(Capability::Network));
        }
        // note: the path a *tool* was handed, which is not the same as a path named inside a shell
        // command; see the note on `Careful` for why the second is not attempted
        if let Some(path) = request.args.get("path").and_then(|path| path.as_str()) {
            judged.extend(
                self.paths
                    .lock()
                    .iter()
                    .filter(|(pattern, _)| path_matches(pattern, path))
                    .map(|(pattern, _)| Subject::Path(pattern.clone())),
            );
        }

        judged
    }

    /// What it would answer about a whole call, deciding nothing and recording nothing.
    ///
    /// note: [`Careful::evaluate`] is this and a note about whatever refused it, and the terminal
    /// asks the same question when it sweeps up the calls an `always` has just answered for. A
    /// second hand-written copy of the fold is a second place to get it wrong.
    pub fn verdict(&self, request: &PermissionRequest) -> Verdict {
        // the strictest answer among the subjects wins, so a call that needs both an allowed one
        // and an unmentioned one is still a question. `Verdict::strictest` is the runtime's own
        // fold for exactly this. A call that needs nothing is allowed: the empty fold
        self.judges(request)
            .iter()
            .map(|subject| self.stance(subject))
            .fold(Verdict::Allow, Verdict::strictest)
    }

    /// What it answers about one subject; asking is what it does about anything unmentioned.
    pub fn stance(&self, subject: &Subject) -> Verdict {
        match subject {
            Subject::Capability(capability) => self
                .stances
                .lock()
                .get(capability)
                .copied()
                .unwrap_or(Verdict::Ask),
            Subject::Path(pattern) => self
                .paths
                .lock()
                .iter()
                .find(|(known, _)| known == pattern)
                .map(|(_, verdict)| *verdict)
                .unwrap_or(Verdict::Ask),
        }
    }

    /// Decides what to answer about one subject from now on.
    pub fn set(&self, subject: &Subject, verdict: Verdict) {
        match subject {
            Subject::Capability(capability) => {
                self.stances.lock().insert(capability.clone(), verdict);
            }
            Subject::Path(pattern) => {
                let mut paths = self.paths.lock();
                match paths.iter_mut().find(|(known, _)| known == pattern) {
                    Some(rule) => rule.1 = verdict,
                    None => paths.push((pattern.clone(), verdict)),
                }
            }
        }
    }

    /// Moves one subject on to the next answer: ask, then allow, then deny, then ask again.
    pub fn cycle(&self, subject: &Subject) -> Verdict {
        let next = match self.stance(subject) {
            Verdict::Ask => Verdict::Allow,
            Verdict::Allow => Verdict::Deny,
            Verdict::Deny => Verdict::Ask,
        };
        self.set(subject, next);

        next
    }

    /// Remembers that these subjects may be used without asking again.
    pub fn always(&self, subjects: &[Subject]) {
        for subject in subjects {
            self.set(subject, Verdict::Allow);
        }
    }

    /// Records that a person, asked about this call, allowed it - and that it reaches the network.
    pub fn grant_the_network(&self, call: &ToolCallId) {
        let mut networked = self.networked.lock();
        if networked.len() > 32 {
            networked.clear();
        }
        networked.insert(call.clone());
    }

    /// Whether [`Careful::grant_the_network`] was told about this call.
    pub fn was_granted_the_network(&self, call: &ToolCallId) -> bool {
        self.networked.lock().contains(call)
    }

    /// Why the given call was refused, if this is what refused it.
    ///
    /// note: it used to be taken out rather than copied, on the grounds that the one caller
    /// rendered it and nothing was gained by holding it. There are two callers now and they want
    /// different things with it: the screen tells the person, and the kernel puts it into the
    /// tool result the *model* reads - see [`PermissionPolicy::why`]. Handing it over once meant
    /// whichever asked first got it and the other was told nothing.
    pub fn why(&self, call: &ToolCallId) -> Option<String> {
        self.refusals.lock().get(call).cloned()
    }

    /// The path rules, in the order they are read out.
    pub fn paths(&self) -> Vec<(String, Verdict)> {
        self.paths.lock().clone()
    }

    /// Every capability this has been told about, and what it will answer, in a stable order.
    pub fn stances(&self) -> Vec<(Capability, Verdict)> {
        self.stances
            .lock()
            .iter()
            .map(|(capability, verdict)| (capability.clone(), *verdict))
            .collect()
    }
}

#[async_trait]
impl PermissionPolicy for Careful {
    /// The reason this policy wrote down when it refused, handed to the kernel so that it reaches
    /// the model rather than only the screen.
    ///
    /// note: the same sentence both of them read. A model told `refused by the rule for
    /// `**/.env`` can do something with that - stop asking for it, ask what to use instead - and
    /// one told only that a call was not permitted cannot tell a standing rule from a bad moment.
    fn why(&self, call: &ToolCallId) -> Option<String> {
        Careful::why(self, call)
    }

    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        let verdict = self.verdict(request);

        if verdict == Verdict::Deny {
            // which subject did it, so that a refused `shell` in a session where `shell` is
            // allowed can say what actually refused it
            let blamed: Vec<String> = self
                .judges(request)
                .iter()
                .filter(|subject| self.stance(subject) == Verdict::Deny)
                .map(|subject| match subject {
                    Subject::Capability(Capability::Network) => {
                        "`network`, which this command reaches for".to_owned()
                    }
                    Subject::Path(pattern) => format!("the rule for `{pattern}`"),
                    subject => format!("`{subject}`"),
                })
                .collect();

            let mut refusals = self.refusals.lock();
            // nobody is obliged to read these; a session that never does should not grow a map
            if refusals.len() > 32 {
                refusals.clear();
            }
            refusals.insert(
                request.call.clone(),
                match blamed.is_empty() {
                    true => "the policy refused it".to_owned(),
                    false => format!("refused by {}", blamed.join(" and ")),
                },
            );
        }

        verdict
    }
}

/// The command a `shell` call was asked to run, if it was asked to run one.
fn command(args: &serde_json::Value) -> Option<&str> {
    args.get("cmd")?.as_str()
}

/// The programs this counts as going out to the network.
///
/// note: short on purpose, and made of what a model actually writes. Every entry here is a
/// program whose *whole point* is the network, so a false positive is nearly impossible; the
/// misses are the other way round, and are the subject of the note on
/// [`reaches_the_network`].
const NETWORKED: &[&str] = &[
    "curl",
    "wget",
    "aria2c",
    "http",
    "https",
    "xh",
    "httpie",
    "nc",
    "ncat",
    "netcat",
    "telnet",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "ftp",
    "ping",
    "dig",
    "nslookup",
    "host",
    "whois",
    "git",
    "gh",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "npx",
    "pip",
    "pip3",
    "uv",
    "poetry",
    "gem",
    "go",
    "brew",
    "apt",
    "apt-get",
    "dnf",
    "pacman",
    "apk",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "aws",
    "gcloud",
    "az",
    "terraform",
];

/// Whether a shell command names one of them.
///
/// note: a heuristic over the command as it was written, and it is worth being exact about what
/// that is worth. It catches `curl https://…`, `pip install x` and `git push`, which is what a
/// model writes when it wants the network, and it does not catch a script that curls, a binary
/// that opens a socket of its own, or `$(echo cur)l`. It is not a sandbox and this program does
/// not pretend it is one - `Capability::Shell` subsumes every other capability, and the runtime's
/// own documentation says so.
///
/// note: that is not hypothetical. Asked for a URL against a live model with `network` refused,
/// the `curl` was refused - and the next call was
/// `python3 -c "import urllib.request; urllib.request.urlopen(...)"`, which was allowed and
/// fetched it. Nothing here is going to win that argument, and trying to would be an arms race
/// with a model's vocabulary. What this *does* do is make the refusal real and visible for the
/// command that was actually written, which is the difference between a policy and a decoration.
///
/// note: what it *is* for is that `network` on the permissions tab should mean something. A row
/// that reads `deny` beside a `shell` the model uses for `curl` all day is worse than no row: it
/// reports a restriction that is not there. Several of these - `git`, `cargo`, `go`, `docker` -
/// also do plenty offline, so the answer will sometimes be a question about `git status`. Erring
/// that way is the point of a policy called `Careful`.
pub fn reaches_the_network(cmd: &str) -> bool {
    cmd.split([';', '|', '&', '\n', '(', ')', '`'])
        .filter_map(|segment| {
            // `FOO=bar curl …`: the assignments come first, and none of them is the program
            segment.split_whitespace().find(|word| !word.contains('='))
        })
        .any(|program| {
            let program = program.trim_matches(['"', '\'']);
            let program = program.rsplit('/').next().unwrap_or(program);

            NETWORKED.contains(&program)
        })
}

// ------------------------------------------------------------------------------ the compactor

/// Drops the oldest tool results once the context gets full, and says so.
///
/// note: It does not summarize what it removed, and the note it leaves behind claims only that
/// the items existed and are gone - a compactor that invented a paraphrase of output it never
/// read would be putting words in a tool's mouth. Every removal is reversible: the items are
/// excluded rather than deleted, they stay in the context pane, and restoring one is a
/// keystroke. Anything pinned is refused by the kernel and reported as refused.
pub struct Trim {
    /// How full the context has to be before this bothers.
    pub threshold: f64,
    /// How empty it is trying to get it.
    pub target: f64,
}

#[async_trait]
impl Compactor for Trim {
    fn should_compact(&self, budget: &Budget) -> bool {
        budget
            .fraction_used()
            .is_some_and(|used| used >= self.threshold)
    }

    async fn plan(&self, items: &[Arc<ContextItem>], budget: &Budget) -> Option<CompactionPlan> {
        let target = (budget.limit? as f64 * self.target) as usize;

        // oldest first, because the results a conversation has moved past are the ones it is
        // least likely to want back
        let candidates = items.iter().filter(|item| {
            item.is_projected() && matches!(item.kind, ContextKind::ToolResult { .. })
        });

        let mut used = budget.used();
        let mut elide = Vec::new();
        for item in candidates {
            if used <= target {
                break;
            }
            used -= item.tokens.min(used);
            elide.push(item.id);
        }
        if elide.is_empty() {
            return None;
        }

        // elided rather than removed, so that the call each of these answers keeps its answer.
        // Removing them would have the projector take the calls down as well - it has to, a call
        // with no result is a request most providers reject - and the model would then be reading
        // a conversation in which it never asked for any of this, directly above a note saying
        // the results had been dropped. The two accounts disagreed and the marker is the true one
        Some(CompactionPlan {
            // note: `elided`, which is the word the pane puts on the row, the word `amend`'s own
            // `prune` takes, and the word the runtime's state is called. It said `shortened to a
            // marker`, which is a third name for the thing - and a fourth mechanism away from
            // `truncated`, which is what an output limit does and is not this at all
            summary: Some(ContextItem::summary(format!(
                "{} earlier tool result(s) were elided to make room: each is now a one-line \
                 marker where its content was. Ask again for anything you still need.",
                elide.len()
            ))),
            // the model reads this too, in each marker's brackets, so it is written to be read by
            // both: what happened, and why
            reason: format!(
                "compacted to make room; the context had reached {}% of the {}-token limit",
                (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize,
                budget.limit.unwrap_or_default(),
            ),
            elide,
            remove: Vec::new(),
        })
    }
}
