//! The four tools, the permission policy and the compactor - all of it ordinary user code.
//!
//! note: The runtime ships none of this. It has no idea what a file is, it spawns no processes,
//! and it never decides that something may run. What it provides is the shape: a [`Tool`] that
//! declares what it needs, a [`PermissionPolicy`] that is asked before anything happens, and a
//! [`Compactor`] whose plan is applied in the open and can be undone.

use std::{collections::BTreeMap, process::Stdio, sync::Arc, time::Duration};

use nachalnik::{
    BoxError, Budget, Capability, CompactionPlan, Compactor, ContextItem, ContextKind, OutputSink,
    PermissionPolicy, PermissionRequest, Tool, ToolCall, ToolCallId, ToolOutput, ToolSpec, Verdict,
    async_trait,
};
use parking_lot::Mutex;
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

/// Returns the four tools a terminal agent needs to be worth talking to.
pub fn builtin() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Read),
        Arc::new(Write),
        Arc::new(Edit),
        Arc::new(Shell),
    ]
}

struct Read;

#[async_trait]
impl Tool for Read {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("read", "reads a file and returns its contents")
            .with_schema(json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }))
            .with_capabilities([Capability::Read])
            .with_output_limit(32_000)
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let path = arg(&call.args, "path")?;

        // a failure the model should read and react to, rather than one that stops the loop
        match tokio::fs::read_to_string(path).await {
            Ok(content) => Ok(ToolOutput::new(content)),
            Err(e) => Ok(ToolOutput::error(format!("{path}: {e}"))),
        }
    }
}

struct Write;

#[async_trait]
impl Tool for Write {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("write", "creates or replaces a file")
            .with_schema(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                },
                "required": ["path", "content"],
            }))
            .with_capabilities([Capability::Write])
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let (path, content) = (arg(&call.args, "path")?, arg(&call.args, "content")?);

        match tokio::fs::write(path, content).await {
            Ok(()) => Ok(ToolOutput::new(format!(
                "wrote {} bytes to {path}",
                content.len()
            ))),
            Err(e) => Ok(ToolOutput::error(format!("{path}: {e}"))),
        }
    }
}

struct Edit;

#[async_trait]
impl Tool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "edit",
            "replaces the first occurrence of `old` with `new` in a file",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old": { "type": "string" },
                "new": { "type": "string" },
            },
            "required": ["path", "old", "new"],
        }))
        .with_capabilities([Capability::Edit])
    }

    async fn invoke(&self, call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        let path = arg(&call.args, "path")?;
        let (old, new) = (arg(&call.args, "old")?, arg(&call.args, "new")?);

        let before = match tokio::fs::read_to_string(path).await {
            Ok(before) => before,
            Err(e) => return Ok(ToolOutput::error(format!("{path}: {e}"))),
        };
        let Some(at) = before.find(old) else {
            return Ok(ToolOutput::error(format!("`old` does not occur in {path}")));
        };

        let after = format!("{}{new}{}", &before[..at], &before[at + old.len()..]);
        match tokio::fs::write(path, after).await {
            Ok(()) => Ok(ToolOutput::new(format!(
                "replaced one occurrence in {path}"
            ))),
            Err(e) => Ok(ToolOutput::error(format!("{path}: {e}"))),
        }
    }
}

/// Runs a command, reporting its output as it arrives and stopping when asked to.
///
/// note: This is the tool that shows what an [`OutputSink`] is for. Every line goes to the sink
/// the moment it is read, so a command that takes a minute is visible for that minute rather
/// than appearing all at once at the end; and between lines it asks whether somebody has pressed
/// escape, in which case the child is killed and the call still answers - with what it got.
struct Shell;

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "shell",
            "runs a command with `sh -c` and returns its output",
        )
        .with_schema(json!({
            "type": "object",
            "properties": { "cmd": { "type": "string" } },
            "required": ["cmd"],
        }))
        .with_capabilities([Capability::Shell])
        .with_output_limit(32_000)
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let cmd = arg(&call.args, "cmd")?;

        let mut child = match tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => return Ok(ToolOutput::error(format!("could not run `{cmd}`: {e}"))),
        };

        // taken out of the child, so that it can still be killed while these are being read
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let collecting_stderr = tokio::spawn(async move {
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
                let _ = child.start_kill();
                interrupted = true;
                break;
            }
        }

        let errors = collecting_stderr.await.unwrap_or_default();
        let status = match child.wait().await {
            Ok(status) => status.to_string(),
            Err(e) => format!("unknown ({e})"),
        };

        let text = format!(
            "exit: {status}\n--- stdout ---\n{collected}\n--- stderr ---\n{errors}{}",
            match interrupted {
                true => "\n\n[the command was stopped before it finished]",
                false => "",
            }
        );

        Ok(ToolOutput::new(text))
    }
}

// --------------------------------------------------------------------------------- the policy

/// Reading is allowed, the network is refused, and everything else is a question - unless the
/// person at the terminal has said otherwise about that capability.
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
pub struct Careful {
    stances: Mutex<BTreeMap<Capability, Verdict>>,
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
    /// Builds a policy that allows reads, refuses the network, and asks about the rest.
    pub fn new() -> Self {
        Self {
            stances: Mutex::new(BTreeMap::from([
                (Capability::Read, Verdict::Allow),
                (Capability::Network, Verdict::Deny),
            ])),
            refusals: Mutex::new(BTreeMap::new()),
        }
    }

    /// What this will answer about one capability; asking is what it does about anything it has
    /// not been told about.
    pub fn stance(&self, capability: &Capability) -> Verdict {
        self.stances
            .lock()
            .get(capability)
            .copied()
            .unwrap_or(Verdict::Ask)
    }

    /// Decides what to answer about one capability from now on.
    pub fn set(&self, capability: Capability, verdict: Verdict) {
        self.stances.lock().insert(capability, verdict);
    }

    /// Moves one capability on to the next answer: ask, then allow, then deny, then ask again.
    pub fn cycle(&self, capability: &Capability) -> Verdict {
        let next = match self.stance(capability) {
            Verdict::Ask => Verdict::Allow,
            Verdict::Allow => Verdict::Deny,
            Verdict::Deny => Verdict::Ask,
        };
        self.set(capability.clone(), next);

        next
    }

    /// Remembers that these capabilities may be used without asking again.
    pub fn always(&self, capabilities: &[Capability]) {
        let mut stances = self.stances.lock();
        for capability in capabilities {
            stances.insert(capability.clone(), Verdict::Allow);
        }
    }

    /// Why the given call was refused, if this is what refused it; the answer is handed over once.
    ///
    /// note: taken out rather than copied, because the one caller renders it and there is nothing
    /// to be gained by holding it afterwards.
    pub fn why(&self, call: &ToolCallId) -> Option<String> {
        self.refusals.lock().remove(call)
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
    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        let stances = self.stances.lock();
        let answer = |capability| stances.get(capability).copied().unwrap_or(Verdict::Ask);

        // the strictest answer among them wins, so a tool that needs both an allowed capability
        // and an unmentioned one is still a question. `Verdict::strictest` is the runtime's own
        // fold for exactly this, and a second hand-written copy of a three-way ordering is a
        // second place to get it wrong. A call that needs nothing is allowed: the empty fold
        let declared = request
            .capabilities
            .iter()
            .map(answer)
            .fold(Verdict::Allow, Verdict::strictest);

        // ... and then what the call actually says. A `shell` that is about to run `curl` is using
        // the network whatever its spec declared, and `network: deny` sitting beside `shell: ask`
        // would otherwise be a row that nothing ever consults. This is the thing a policy can do
        // that a capability list cannot: it is handed the arguments
        let networked = request.capabilities.contains(&Capability::Shell)
            && command(&request.args).is_some_and(reaches_the_network);
        let verdict = match networked {
            true => declared.strictest(answer(&Capability::Network)),
            false => declared,
        };

        if verdict == Verdict::Deny {
            // the capability that did it, so that a refused `shell` in a session where `shell` is
            // allowed can say what actually refused it
            let blamed: Vec<String> = request
                .capabilities
                .iter()
                .chain(networked.then_some(&Capability::Network))
                .filter(|capability| answer(capability) == Verdict::Deny)
                .map(|capability| match (capability, networked) {
                    (Capability::Network, true) => {
                        "`network`, which this command reaches for".to_owned()
                    }
                    _ => format!("`{capability}`"),
                })
                .collect();

            drop(stances);
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
        let mut remove = Vec::new();
        let mut dropped = Vec::new();
        for item in candidates {
            if used <= target {
                break;
            }
            used -= item.tokens.min(used);
            remove.push(item.id);
            dropped.push(format!("{} ({} tokens)", item.label, item.tokens));
        }
        if remove.is_empty() {
            return None;
        }

        Some(CompactionPlan {
            summary: Some(ContextItem::summary(format!(
                "{} earlier tool results were removed from this conversation to make room: {}. \
                 Ask for them again if you need them.",
                dropped.len(),
                dropped.join(", ")
            ))),
            reason: format!(
                "the context reached {}% of the {}-token limit",
                (budget.fraction_used().unwrap_or_default() * 100.0).round() as usize,
                budget.limit.unwrap_or_default(),
            ),
            remove,
        })
    }
}
