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
    Careful, KEPT, Limits, arg,
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
/// and the two drift the first time the wording changes.
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

/// Where a command line's own top-level joints are - `|`, `||`, `&&`, `;` - as byte ranges into
/// it; empty when there are none, or when the scan could not be trusted.
///
/// note: here rather than beside the panel that colours them. Where one stage of a command ends
/// and the next begins is a fact about the command rather than about drawing it, and `ui` is
/// behind feature `tui` - so anything wanting it without a screen could not ask, which is every
/// headless session and everything in `tools` that reads a command before it runs.
///
/// note: ranges rather than a rewritten command. Breaking the line at each joint, one stage to a
/// row, costs a row per stage on a panel whose rows are its scarcest thing - and a broken command
/// is not something `sh` would take back, so the panel would show a spelling nobody could act on.
/// Colouring the joints in place says the same thing: where one stage ends and the next begins,
/// at a glance, in a command still written the way the model wrote it.
///
/// note: worked out here rather than taken from the highlighter. `synoptic`'s `sh` mode calls
/// every flag's hyphen an operator - `-n`, `-u`, `-5` - and does not tokenise `|` or `;` at all,
/// so painting its operators would colour the noise and miss the joints.
///
/// note: quote-aware, and it gives up rather than guessing.
/// [`reaches_the_network`](crate::tools::reaches_the_network) splits a command on these same
/// characters without caring where in it they are, because a policy that over-reads a command asks
/// a question it need not have, and that is the right way for a policy to be wrong. A panel that
/// over-reads one *tells somebody a quoted `|` is a pipe* on the screen where they decide whether
/// to run it. So an unterminated quote, an unclosed `$(` or a trailing backslash comes back empty
/// and the command is drawn with nothing picked out.
///
/// note: a command that already has newlines in it is left alone without being scanned at all.
/// What is inside a heredoc is arbitrary text - the `|` in the middle of a Python string is not a
/// joint, and nothing readable from one line says so.
pub fn joints(cmd: &str) -> Vec<(usize, usize)> {
    if cmd.contains('\n') {
        return Vec::new();
    }

    let mut found: Vec<(usize, usize)> = Vec::new();

    // note: bytes rather than characters, which is safe because everything looked at here is
    // ASCII: every byte of a multi-byte character is `0x80` or above, so it can match none of
    // them, and every index taken is one of theirs
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

/// Runs a command, reporting its output as it arrives and stopping when asked to, under whatever
/// confinement the policy's stances add up to.
///
/// note: this is the tool that shows what an [`OutputSink`] is for. Every line goes to the sink
/// the moment it is read, so a command that takes a minute is visible for that minute rather
/// than appearing all at once at the end; and between lines it asks whether somebody has pressed
/// escape, in which case the child is killed and the call still answers - with what it got.
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
        // refusal is the file's own permissions. That is right until `fs:write` is refused, and
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
        let args = &*args;
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

        // the program's keys stay with the program; see `endpoint::KEYS`
        for key in crate::endpoint::KEYS {
            command.env_remove(key);
        }

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
        let mut stderr = child.stderr.take().expect("stderr was piped");
        // note: into a buffer this end keeps, rather than one the task hands back when it is done,
        // because it may never be done - see below - and what had arrived by then is still worth
        // telling the model. Up to `KEPT`, and past that read and counted, so that a command that
        // writes without end fills neither this process nor the pipe it writes to
        let heard = Arc::new(std::sync::Mutex::new((Vec::new(), 0)));
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

        // note: bytes rather than `lines()`, which refuses a line that is not UTF-8 - and stopping
        // there would leave the pipe full and nobody reading it, so `cat` of a picture would block
        // writing and the call would wait for it for ever. A command's output is whatever it
        // wrote; what is not text is shown the way `from_utf8_lossy` shows it
        let mut stdout = BufReader::new(stdout);
        let mut line = Vec::new();
        let mut collected = String::new();
        // what was read past `KEPT` and let go; once one line does not fit, none after it is kept
        // either, so that what is kept is the start of the output with no hole in it
        let (mut dropped, mut full) = (0, false);
        let mut interrupted = false;
        loop {
            // the timeout is what makes a command that says nothing at all interruptible; without
            // it this would sit in `read_until` until the child felt like talking. A timed-out
            // read keeps what it had in `line`, and the next one carries on from there
            match tokio::time::timeout(HEARTBEAT, stdout.read_until(b'\n', &mut line)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(_)) if full || collected.len() + line.len() > KEPT => {
                    full = true;
                    dropped += line.len();
                    line.clear();
                }
                Ok(Ok(_)) => {
                    let text = String::from_utf8_lossy(&line);
                    let text = text.strip_suffix('\n').unwrap_or(&text);
                    let text = text.strip_suffix('\r').unwrap_or(text);
                    output.push(format!("{text}\n"));
                    collected.push_str(text);
                    collected.push('\n');
                    line.clear();
                }
                Ok(Err(e)) => {
                    collected.push_str(&format!("\n[could not read the output: {e}]\n"));
                    break;
                }
                // a line that never ends is held to the ceiling as well, or it would be the way
                // round it
                Err(_) if line.len() > KEPT => {
                    full = true;
                    dropped += line.len();
                    line.clear();
                }
                Err(_) => {}
            }

            if output.is_interrupted() {
                stop(&mut child).await;
                interrupted = true;
                break;
            }
        }
        // whatever stopped the reading, nobody reads this pipe again: closing it is what tells a
        // command still writing to it, rather than leaving it blocked on a full one
        drop(stdout);

        // note: the end of standard output is not the end of the command. `sleep 5 >&-` has closed
        // it and is still running, so the wait is watched the way the reading was
        let mut waited = None;
        while !interrupted {
            match tokio::time::timeout(HEARTBEAT, child.wait()).await {
                Ok(status) => {
                    waited = Some(status);
                    break;
                }
                Err(_) if output.is_interrupted() => {
                    stop(&mut child).await;
                    interrupted = true;
                }
                Err(_) => {}
            }
        }

        // note: and the end of the command is not the end of its standard error. A killed shell
        // can leave children of its own behind - `sleep 60; echo done` is two processes - and so
        // can one that finished: `python3 -m http.server > log &` holds the pipe for as long as the
        // server runs. Waiting for the end of it would be a call that never answers, so it gets a
        // moment to drain once the command is gone and no longer
        let held = tokio::time::timeout(HEARTBEAT, &mut collecting_stderr)
            .await
            .is_err();
        if held {
            collecting_stderr.abort();
        }
        let (mut errors, dropped) = {
            let heard = heard.lock().expect("nothing panics holding it");
            (
                String::from_utf8_lossy(&heard.0).into_owned(),
                dropped + heard.1,
            )
        };
        if held && !interrupted {
            errors.push_str("\n[standard error is still open: something this command started is still running]\n");
        }
        // note: not `ExitStatus`'s own `Display`, which renders `exit status: 0` and would make
        // the first line of every result read `exit: exit status: 0`. What a reader wants from
        // this line is the number, and whether it means the command worked
        //
        // note: and whether it was stopped, which is said here rather than in a line after the
        // standard error. An output limit cuts from the end, so a stopped command that had said
        // more than the limit would lose the one line explaining why its output stops
        // mid-sentence - the truncation marker takes its place, and the model is told the wrong
        // thing about what it is reading. The first line survives anything
        let waited = match waited {
            Some(waited) => waited,
            None => child.wait().await,
        };
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
        // and what was not kept, under it for the same reason
        let unkept = match dropped {
            0 => String::new(),
            dropped => format!(
                "[{dropped} more bytes of output were read and not kept: a stream is kept up to \
                 {KEPT} bytes]\n"
            ),
        };
        let text = format!(
            "{status}\n{note}{unkept}--- stdout ---\n{collected}\n--- stderr ---\n{errors}"
        );

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

    /// A shell with no confinement, in a directory every platform has.
    fn unconfined() -> Shell {
        Shell {
            policy: Arc::new(Careful::new()),
            workdir: std::env::temp_dir(),
            extra: Vec::new(),
            readable: Vec::new(),
            confiner: None,
            limits: Limits::default(),
        }
    }

    /// Runs one command, and gives up on it rather than on the suite.
    ///
    /// note: `#[cfg(unix)]` because every test that calls it is, and under the `RUSTFLAGS` this
    /// workspace builds with, a helper nothing calls fails the build on Windows
    #[cfg(unix)]
    async fn ran(command: &str) -> String {
        let call = ToolCall::new("c1", "shell", serde_json::json!({ "cmd": command }));
        tokio::time::timeout(
            Duration::from_secs(10),
            unconfined().invoke(&call, OutputSink::disconnected()),
        )
        .await
        .unwrap_or_else(|_| panic!("`{command}` never answered"))
        .expect("the tool answers the call either way")
        .content
        .to_text()
        .into_owned()
    }

    /// Output that is not text is shown rather than stopped at, and what follows it arrives.
    ///
    /// note: more than a pipe holds follows the byte that is not UTF-8, because that is what makes
    /// this a hang rather than a loss: a reader that stops at the first such line leaves the
    /// command behind it blocked writing into a full pipe that nobody is reading.
    #[cfg(unix)]
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

    /// Output past the ceiling is read to the end and let go, on either stream and in a line that
    /// never ends, and the answer says how much at the top.
    ///
    /// note: read to the end rather than stopped at, which the exit status shows: `head` finishes
    /// and the command succeeds, where a reader that stopped reading would leave it blocked on a
    /// full pipe.
    #[cfg(unix)]
    #[tokio::test]
    async fn output_past_the_ceiling_is_read_and_not_kept() {
        let past = KEPT + 1_000_000;
        for command in [
            format!("yes | head -c {past}"),
            format!("yes | head -c {past} >&2"),
            format!("head -c {past} /dev/zero | tr '\\0' a"),
        ] {
            let said = ran(&command).await;

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

    /// A command that finishes and leaves something running still answers.
    ///
    /// note: the background job inherits standard error, so the pipe stays open for as long as it
    /// runs. A call that waits for the end of it never answers for a server started with `&`, and
    /// an `esc` does nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_that_leaves_something_running_still_answers() {
        let said = ran("sleep 30 > /dev/null & echo started").await;

        assert!(said.starts_with("exit: 0"), "{said}");
        assert!(said.contains("started"), "{said}");
        assert!(said.contains("still open"), "{said}");
    }

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
    /// code - `kill -9 $$` comes back `2304`, which is `9 << 8` - so `ExitStatus::code` is `Some`
    /// there and the tool reads a failure. It is not wrong to: telling a signal wearing an exit
    /// code from a command that really exited `2304` means knowing what the command was, which is
    /// the reason `grep`'s `1` is not a fourth `Exit` either. So this case asserts something true
    /// of one platform, and is run on that one. What is left uncovered off unix is `Stopped`
    /// through the interrupt - the tests that press the button are `#[cfg(unix)]` in
    /// `tests/headless.rs` too, for want of a way to send the press.
    #[tokio::test]
    async fn what_a_command_reported_is_what_its_first_line_says() {
        let shell = unconfined();

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
        // and a command with newlines of its own is not scanned at all: what is inside a heredoc
        // is arbitrary text, and the `|` in a Python expression is not a pipe
        assert!(joints("python3 - <<'PY'\nprint(1 | 2)\nPY").is_empty());
    }

    /// A separator that is part of an argument is not a joint, whichever way it was quoted.
    ///
    /// note: the case this function is for. Coloured as a joint, the `|` in `echo 'a | b'` would
    /// be the panel telling somebody a quoted character is a pipe, on the screen where they decide
    /// whether to run it - so a quoted separator has to be invisible to the scan.
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
