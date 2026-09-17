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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::tools::{
    Careful, Limits, arg,
    ops::{self, Arg, Op, inner, schema},
};

/// How long a running command may say nothing before the tool looks up to check whether it has
/// been asked to stop.
const HEARTBEAT: Duration = Duration::from_millis(120);

/// What the first line of a shell result says happened, for a reader who has not got time to
/// read it.
///
/// note: here rather than where it is drawn, because the line is written here - see the `status`
/// match in [`Shell::invoke`], which is the only thing that produces one. A colour worked out at
/// the other end from a string it does not own is a second opinion about what a result means,
/// and the two drift the first time the wording changes. This is one opinion with two readers.
///
/// note: three, and not one per shape, because the question a colour answers is coarse: did the
/// command say it worked, did it say it failed, or did it never get to say. The fourth thing
/// somebody might want - a `1` from `grep` meaning *no match* rather than a fault - is not in
/// here, because telling those apart means knowing what the command was and this program would
/// be guessing. A guess that colours a working pipeline red, or a real failure yellow, is worse
/// than the number itself.
///
/// note: the third is narrower off unix, where there are no signals to report. A Windows shell
/// hands a killed child back to a native parent as an ordinary exit code - `kill -9` arrives as
/// `2304` - so a command that was killed reads as one that failed, and it is the same guess to
/// say otherwise. What is left yellow there is the stop this program does itself, which is not
/// the platform's to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The command finished and reported success.
    Ok,
    /// The command finished and reported a failure.
    Failed,
    /// The command never got to report: stopped at the person's request, killed by a signal, or
    /// a status that could not be read at all.
    Stopped,
}

impl Exit {
    /// Reads one back off a line, if that line is a shell result's first one.
    ///
    /// note: the number decides it, rather than the words after it. `exit: ` is followed by a
    /// code or by a reason no command reported - `stopped`, `signal: 9 (SIGKILL)`, `unknown` -
    /// so a token that parses is an exit code and one that does not is a command that never got
    /// to report. The prose can be rewritten without this having to be.
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

        // note: the figure rather than "long output", because what a model does about a limit it
        // cannot see is find out by spending it - and read off the table rather than written into
        // the sentence, since `/limit exec:run` moves it and a description is built afresh for
        // every request. Bare, the way the marker a cut output carries is bare: the model reads
        // `[... 4000 bytes truncated by an output limit ...]` against this, and the two should be
        // the same kind of number
        let cut = match self.limits.for_call(&[Capability::exec("run")]) {
            Some(bytes) => format!(" Output over {bytes} bytes is cut off at the end."),
            None => String::new(),
        };

        // note: the working directory being read-only is said here rather than at the point of
        // failure, where the rest of the confinement is accounted for. `Sandbox::note_for` says
        // nothing about a refusal naming a path this session reaches, on the grounds that such a
        // refusal is the file's own permissions - which is right until `fs:write` is refused, and
        // then every write inside the working directory is the boundary and reads the same way.
        // Standard error does not say whether a refusal was a read or a write, so the sentence
        // that can be certain is this one, before anything is run
        let read_only = self
            .confiner
            .is_some()
            .then(|| {
                Sandbox::of(
                    &self.policy,
                    self.workdir.clone(),
                    self.extra.clone(),
                    self.readable.clone(),
                    false,
                )
            })
            .is_some_and(|sandbox| !sandbox.writable);

        ToolSpec::new(
            "shell",
            format!(
                "runs one command with `sh -c` in the working directory and returns its exit \
                 status, its output and its errors.{cut} Nothing \
                 is typed at it: a command that waits for input waits for ever.{}",
                match self.confiner.is_some() {
                    true => format!(
                        " It runs confined: outside the working directory it can read this \
                         machine's system paths{} and no more, and TCP may be closed - so a \
                         permission error there is the confinement rather than the command.{}",
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
        // note: one operation, and asked for by name anyway, because every tool this program
        // offers takes an `action` and a model should not have to remember which of them is the
        // exception. It costs a word in the call and buys a rule with no holes in it - the same
        // reasoning `log` is written to
        .with_schema(schema(&ops()))
        .with_capabilities([Capability::exec("run")])
    }

    fn limit(&self, call: &ToolCall) -> Option<usize> {
        self.limits.for_call(&self.needs(call))
    }

    async fn invoke(&self, call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        // note: named rather than assumed, for the reason `fs` refuses one it does not have: a
        // call that meant something else and was answered as `run` is a command nobody asked for.
        // The tool is `shell` and the operation is `run` - the domain it declares is `exec`,
        // which is what a permission rule is written against
        let args = match inner(&call.args) {
            Ok(args) => args,
            Err(refusal) => return Ok(ToolOutput::error(refusal)),
        };
        if let Some(named) = args["action"].as_str().filter(|it| *it != "run") {
            return Ok(ToolOutput::error(format!(
                "`{named}` is not something `shell` does; it does run"
            )));
        }
        // the rule the other tools hold to, and there is no reason for the one with a single
        // operation to be the exception: an ignored argument comes back as a real answer - the
        // answer to the call without it - and `cmd` beside a stray `path` is a command somebody
        // meant to point somewhere
        if let Some(refusal) = ops::unread("run", args, &ops()) {
            return Ok(ToolOutput::error(refusal));
        }
        let cmd = arg(args, "cmd")?;

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
        let (meant, status) = match (interrupted, waited) {
            (true, _) => (
                Exit::Stopped,
                "exit: stopped before it finished, at the request of the person you are working \
                 with; what is below is what it had said by then"
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
        // note: the one place the writing and the reading of this line can be held together, and
        // it costs nothing in a release build. Every test that runs a real command is then also a
        // test that the line it wrote says what the run meant - which a table of strings
        // somewhere else could never be, since it would go on agreeing with itself after the
        // wording here changed
        debug_assert_eq!(
            Exit::of(&status),
            Some(meant),
            "the status line and what it means have come apart"
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What a command reported is what its first line says, for every shape the tool writes.
    ///
    /// note: real commands rather than a table of the strings this file writes. A table would go
    /// on agreeing with itself after somebody reworded the status line, which is the one failure
    /// worth catching here - and the `debug_assert` in `invoke` is the same check from the other
    /// side, run by every test in this crate that runs a command.
    ///
    /// note: `kill -9 $$` for the third, because a signal is the only way to leave a status with
    /// no code in it, and SIGKILL is the one a shell cannot decline.
    ///
    /// note: and unix-only, because that status does not exist elsewhere. The `sh` on a Windows
    /// runner is git's, and it reports a signalled child to a native parent as an ordinary exit
    /// code - `kill -9 $$` came back `2304`, which is `9 << 8` - so `ExitStatus::code` is `Some`
    /// there and the tool reads a failure. It is not wrong to: telling a signal wearing an exit
    /// code from a command that really exited `2304` means knowing what the command was, which is
    /// the reason `grep`'s `1` is not a fourth `Exit` either. So this case asserts something true
    /// of one platform, and is run on that one. What is left uncovered off unix is `Stopped`
    /// through the interrupt - the tests that press the button are `#[cfg(unix)]` in
    /// `tests/headless.rs` too, for want of a way to send the press.
    #[tokio::test]
    async fn what_a_command_reported_is_what_its_first_line_says() {
        let shell = Shell {
            policy: Arc::new(Careful::new()),
            workdir: std::env::temp_dir(),
            extra: Vec::new(),
            readable: Vec::new(),
            confiner: None,
            limits: Limits::default(),
        };

        for (command, meant) in [
            ("exit 0", Exit::Ok),
            ("exit 3", Exit::Failed),
            #[cfg(unix)]
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
}
