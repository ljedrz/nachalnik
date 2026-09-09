//! Running a command, and stopping one.
//!
//! note: the tool that shows what an [`OutputSink`] is for - every line reaches the screen as it
//! is read, so a command that takes a minute is visible for that minute rather than arriving all
//! at once - and the one that runs under a real confinement, built from the stances the policy
//! has accumulated rather than from the verdict on this one call.

use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use nachalnik::{
    BoxError, Capability, OutputSink, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
};

use crate::sandbox::Sandbox;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::tools::{Careful, Limits, arg};

/// How long a running command may say nothing before the tool looks up to check whether it has
/// been asked to stop.
const HEARTBEAT: Duration = Duration::from_millis(120);

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
    /// Extra paths the user asked to open up, read-write.
    pub extra: Vec<PathBuf>,
    /// Extra paths the user asked to open up for reading only.
    pub readable: Vec<PathBuf>,
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
    /// How much of a command's output the model is shown.
    ///
    /// note: on the struct rather than assigned by [`super::builtin`], because this is a public
    /// type somebody constructs and registers directly (`builtin` is a convenience, not the only
    /// door), and a limit handed out by one of two routes is a limit `/limit` silently fails to
    /// change on the other.
    pub limits: Limits,
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        // note: the confinement is said out loud only when there is one. A command stopped by
        // Landlock comes back with an ordinary permission error and nothing to distinguish it
        // from a file that really is protected, and a model that cannot tell those apart spends
        // its turns trying `sudo`
        // note: and the paths that were opened up are named, because a model told only about
        // "the working directory and the system paths" has no reason to try `~/.rustup` even
        // when somebody opened it for exactly that
        let opened: Vec<String> = self
            .extra
            .iter()
            .map(|path| format!("{} read-write", path.display()))
            .chain(
                self.readable
                    .iter()
                    .map(|path| format!("{} read-only", path.display())),
            )
            .collect();

        let spec = ToolSpec::new(
            "shell",
            format!(
                "runs one command with `sh -c` in the working directory and returns its exit \
                 status, its output and its errors. Long output is cut off at the end. Nothing \
                 is typed at it: a command that waits for input waits for ever.{}",
                match self.confiner.is_some() {
                    true => format!(
                        " It runs confined: outside the working directory it can read this \
                         machine's system paths{} and no more, and TCP may be closed - so a \
                         permission error there is the confinement rather than the command.",
                        match opened.is_empty() {
                            true => String::new(),
                            false => format!(", and {},", opened.join(" and ")),
                        }
                    ),
                    false => String::new(),
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
        .with_capabilities([Capability::Shell]);

        self.limits.apply(spec)
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        let cmd = arg(&call.args, "cmd")?;

        // what the command may reach, which is a different question from whether it may run: the
        // kernel answered that one before this was called
        //
        // note: kept rather than built and dropped, because it is also what can tell afterwards
        // whether a permission error in the output was this boundary; see `Sandbox::note_for`
        let sandbox = self.confiner.is_some().then(|| {
            Sandbox::of(
                &self.policy,
                self.workdir.clone(),
                self.extra.clone(),
                self.readable.clone(),
                self.policy.was_granted_the_network(&call.id),
            )
        });
        let mut command = match (&self.confiner, &sandbox) {
            (Some(me), Some(sandbox)) => {
                let mut command = tokio::process::Command::new(me);
                command.args(sandbox.argv(cmd));
                command
            }
            _ => {
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

        // note: under the status line rather than beside the message it explains, for the same
        // reason the status line says whether the command was stopped: an output limit cuts from
        // the end, and a note accounting for a permission error is worth nothing if the
        // truncation takes the note and leaves the error. The top of the output survives anything
        let note = match sandbox
            .as_ref()
            .and_then(|sandbox| sandbox.note_for(&errors))
        {
            Some(note) => format!("{note}\n"),
            None => String::new(),
        };
        let text = format!("{status}\n{note}--- stdout ---\n{collected}\n--- stderr ---\n{errors}");

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
