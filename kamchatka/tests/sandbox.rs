//! Tests that run a command under the sandbox and check what it could not do.
//!
//! note: these spawn the real binary in its confining mode and assert on what the kernel refused,
//! because a sandbox is a claim like any other and the only way to check it is to try. A test that
//! asserted the ruleset was *built* would pass on a kernel that ignored every word of it.
//!
//! note: Linux-only, and skipped rather than failed where Landlock is not available - which is
//! what `Confinement::Unavailable` is for. A machine that cannot enforce this should say so once,
//! not fail a suite.

#![cfg(target_os = "linux")]

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use kamchatka::{
    sandbox::{Confinement, Sandbox, available},
    tools::{Careful, Shell},
};
use nachalnik::{
    Config, ContextItem, ContextKind, Kernel, ModelResponse,
    test::{AllowAll, ScriptedProvider, call},
};
use serde_json::json;

/// The binary under test, which is also the thing that confines itself.
fn program() -> PathBuf {
    // the test binary lives beside it
    let mut path = std::env::current_exe().expect("a test binary has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }

    path.join("kamchatka")
}

/// Runs a command under the given sandbox, returning its output and whether it succeeded.
///
/// note: spawned rather than run in one call, and the temporary directory removed afterwards,
/// because that is what the `shell` tool does: a confined process cannot remove its own, and a
/// test that skipped it would leave one behind per command and prove nothing about the tool.
fn run(sandbox: &Sandbox, cmd: &str) -> (bool, String) {
    let child = Command::new(program())
        .args(sandbox.argv(cmd))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    let scratch = kamchatka::sandbox::scratch_for(child.id());
    let output = child.wait_with_output().expect("it was spawned");
    let _ = std::fs::remove_dir_all(&scratch);

    let mut said = String::from_utf8_lossy(&output.stdout).into_owned();
    said.push_str(&String::from_utf8_lossy(&output.stderr));

    (output.status.success(), said)
}

/// A workspace of its own, so nothing here can touch the repository.
fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kamchatka-sandbox-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    std::fs::write(dir.join("inside.txt"), "hello").expect("a file in it");

    dir
}

fn sandbox(workdir: PathBuf, writable: bool, network: bool) -> Sandbox {
    Sandbox {
        workdir,
        extra: Vec::new(),
        readable: Vec::new(),
        writable,
        network,
    }
}

/// Whether this machine can enforce any of it; the tests say so and stop rather than failing.
fn enforced() -> bool {
    match available(&program()) {
        Confinement::Full => true,
        other => {
            eprintln!("skipped: {other}");
            false
        }
    }
}

#[test]
fn a_command_can_read_and_write_inside_the_working_directory() {
    if !enforced() {
        return;
    }
    let dir = workdir("inside");
    let sandbox = sandbox(dir.clone(), true, false);

    let (ok, said) = run(
        &sandbox,
        "cat inside.txt && echo written > made.txt && cat made.txt",
    );

    assert!(ok, "{said}");
    assert!(said.contains("hello"), "{said}");
    assert!(said.contains("written"), "{said}");
    assert!(dir.join("made.txt").exists());
}

#[test]
fn the_system_directories_are_readable_and_that_is_where_the_line_is() {
    if !enforced() {
        return;
    }
    // a command that cannot read `/usr/bin` cannot be a command at all, so `SYSTEM` is readable
    // on purpose and what is interesting is what is *writable*. It gets a test of its own because
    // the readme claimed "nothing outside that directory is reachable either way", which two live
    // sessions disproved by reading `/etc/passwd` through this with nothing refusing them - and
    // the test that sounded like it covered the claim reaches for a home directory, which is not
    // in `SYSTEM` and so was never the case in question
    let (ok, said) = run(&sandbox(workdir("system"), true, false), "cat /etc/passwd");
    assert!(ok, "the system directories are readable on purpose: {said}");
    assert!(said.contains("root:"), "{said}");

    // readable, and no more than that
    let (wrote, said) = run(
        &sandbox(workdir("system"), true, false),
        "touch /etc/kamchatka-probe",
    );
    assert!(!wrote, "a system directory is not writable: {said}");
}

#[test]
fn a_command_cannot_read_private_files_outside_it() {
    if !enforced() {
        return;
    }
    // outside the working directory *and* outside the system paths, which is the part somebody
    // installing this actually cares about
    let (ok, said) = run(
        &sandbox(workdir("read"), true, false),
        "cat /home/*/.bashrc",
    );

    assert!(!ok, "reading outside the sandbox should fail: {said}");
    assert!(
        said.contains("Permission denied") || said.contains("No such file"),
        "{said}"
    );
}

#[test]
fn a_command_cannot_write_outside_it() {
    if !enforced() {
        return;
    }
    // `/tmp` itself, which the sandbox does *not* open up: what a command gets instead is a
    // directory of its own, handed to it as `TMPDIR`
    let escape = std::env::temp_dir().join("kamchatka-escaped.txt");
    let _ = std::fs::remove_file(&escape);
    let (ok, said) = run(
        &sandbox(workdir("write-out"), true, false),
        &format!("echo escaped > {}", escape.display()),
    );
    assert!(!ok, "the whole of /tmp is not writable: {said}");
    assert!(!escape.exists());

    let elsewhere = PathBuf::from("/etc/kamchatka-escaped.txt");
    let (ok_etc, said_etc) = run(
        &sandbox(workdir("write-etc"), true, false),
        &format!("echo escaped > {}", elsewhere.display()),
    );
    assert!(!ok_etc, "writing to /etc should fail: {said_etc}");
    assert!(said_etc.contains("Permission denied"), "{said_etc}");
    assert!(!elsewhere.exists());

    // ... and a temporary file made the way a program actually makes one still works, or a
    // compiler would not run under this at all
    let (ok_tmp, said_tmp) = run(
        &sandbox(workdir("write-tmpdir"), true, false),
        "echo scratch > \"$TMPDIR/t.txt\" && cat \"$TMPDIR/t.txt\"",
    );
    assert!(ok_tmp, "{said_tmp}");
    assert!(said_tmp.contains("scratch"), "{said_tmp}");
}

/// Truncation is a write, and it does not go through `open`.
///
/// note: `truncate(2)` takes a path and never opens the file, so `WriteFile` does not cover it -
/// the kernel has a right of its own for it, from ABI 3. An access right the ruleset does not
/// *handle* is not restricted at all, so with the ruleset built on ABI 1 this call came back with
/// nothing to say and a file outside the working directory of nought bytes. GNU `truncate(1)`
/// opens the file and so was refused all along, which is exactly why nothing noticed.
#[test]
fn a_command_cannot_truncate_a_file_outside_the_working_directory() {
    if !enforced() {
        return;
    }
    let dir = workdir("truncate");
    let outside =
        std::env::temp_dir().join(format!("kamchatka-outside-{}.txt", std::process::id()));
    std::fs::write(&outside, "the whole of it").expect("a file outside");

    let (_, said) = run(
        &sandbox(dir, true, false),
        &format!(
            "python3 -c \"import os;os.truncate('{}',0)\" 2>&1 || echo refused",
            outside.display()
        ),
    );

    assert_eq!(
        std::fs::read_to_string(&outside).ok().as_deref(),
        Some("the whole of it"),
        "a file outside the working directory was emptied: {said}"
    );
    let _ = std::fs::remove_file(&outside);
}

/// And it still works where it is supposed to, which is what handling the right rather than
/// leaving it out costs: `from_all` grants it again on every writable path.
#[test]
fn a_command_can_truncate_a_file_inside_it() {
    if !enforced() {
        return;
    }
    let dir = workdir("truncate-inside");
    let (ok, said) = run(
        &sandbox(dir.clone(), true, false),
        "python3 -c \"import os;os.truncate('inside.txt',0)\" 2>&1",
    );

    assert!(ok, "{said}");
    assert_eq!(
        std::fs::metadata(dir.join("inside.txt"))
            .map(|m| m.len())
            .ok(),
        Some(0)
    );
}

#[test]
fn a_refused_write_stance_makes_the_working_directory_read_only() {
    if !enforced() {
        return;
    }
    let dir = workdir("readonly");

    let (ok, said) = run(&sandbox(dir.clone(), false, false), "echo x > nope.txt");

    assert!(!ok, "{said}");
    assert!(said.contains("Permission denied"), "{said}");
    assert!(!dir.join("nope.txt").exists());
    // ... and reading still works, or the shell would be useless
    let (ok, said) = run(&sandbox(dir, false, false), "cat inside.txt");
    assert!(ok && said.contains("hello"), "{said}");
}

#[test]
fn a_refused_network_is_refused_by_the_kernel_rather_than_by_reading_the_command() {
    if !enforced() {
        return;
    }
    let dir = workdir("network");

    // the way a model asks for the network, which a policy reading the command would also catch
    let (ok, said) = run(
        &sandbox(dir.clone(), true, false),
        "curl -sS --max-time 5 https://example.com",
    );
    assert!(!ok, "{said}");

    // ... and the way it asks after being refused, which no policy reading a command line catches.
    // This is the case that made the sandbox worth having: a live model did exactly this
    let (ok, said) = run(
        &sandbox(dir, true, false),
        "python3 -c \"import urllib.request; urllib.request.urlopen('https://example.com')\"",
    );
    assert!(!ok, "the second way round has to fail too: {said}");
    assert!(
        said.contains("Permission denied") || said.contains("Errno 13"),
        "{said}"
    );
}

#[test]
fn the_file_tools_are_held_to_the_same_boundary() {
    use kamchatka::sandbox::{Access, Reach};

    let dir = workdir("reach").canonicalize().expect("it exists");
    let reach = Reach {
        workdir: dir.clone(),
        extra: vec![PathBuf::from("/usr/share")],
        readable: Vec::new(),
        confined: true,
    };

    // inside, by any spelling. `./` is in here because the first live run of this came back
    // `Not a directory`: an empty remainder joined onto a resolved path appends a separator
    assert_eq!(
        reach.allows("inside.txt", Access::Reading),
        Ok(dir.join("inside.txt"))
    );
    assert_eq!(
        reach.allows("./inside.txt", Access::Reading),
        Ok(dir.join("inside.txt"))
    );
    assert_eq!(reach.allows(".", Access::Reading), Ok(dir.clone()));
    assert!(
        reach
            .allows(dir.join("inside.txt").to_str().unwrap(), Access::Reading)
            .is_ok()
    );
    // ... including one that is not there yet, which is most of what `write` is handed. Compared
    // as strings, deliberately: a `PathBuf` compares by component, so a separator on the end of
    // one is invisible to `assert_eq!` and visible to every `fs` call there is. That is how a
    // `write` which could not create a single file went on passing a test that asserted `Ok`
    let name = |path: &str| {
        reach
            .allows(path, Access::Reading)
            .map(PathBuf::into_os_string)
    };
    assert_eq!(
        name("not-there-yet.txt"),
        Ok(dir.join("not-there-yet.txt").into_os_string()),
        "a file about to be created"
    );
    assert_eq!(
        name("./not-there-yet.txt"),
        Ok(dir.join("not-there-yet.txt").into_os_string())
    );
    assert_eq!(
        name("sub/dir/new.txt"),
        Ok(dir.join("sub/dir/new.txt").into_os_string())
    );
    // ... and the whole of what that is for
    let made = reach
        .allows("not-there-yet.txt", Access::Reading)
        .expect("it is inside");
    std::fs::write(&made, "made").expect("a file about to be created can be created");

    // outside, by every spelling somebody would reach for
    assert!(reach.allows("/etc/passwd", Access::Reading).is_err());
    assert!(
        reach
            .allows("../../../etc/passwd", Access::Reading)
            .is_err(),
        "`..` is resolved, not matched"
    );
    assert!(reach.allows("/etc/../etc/passwd", Access::Reading).is_err());

    // ... including through a symlink, which is why the path is resolved rather than compared
    let link = dir.join("out");
    std::os::unix::fs::symlink("/etc", &link).expect("a symlink");
    assert!(
        reach
            .allows(link.join("passwd").to_str().unwrap(), Access::Reading)
            .is_err(),
        "a symlink out is still out"
    );

    // what was opened up on purpose
    assert!(reach.allows("/usr/share/anything", Access::Reading).is_ok());
    assert!(reach.allows("/usr/share/anything", Access::Writing).is_ok());

    // ... and what was opened up for reading and no more. The two answers differ only here, which
    // is why `allows` takes what the tool is about to do rather than defaulting to one of them
    let readable = Reach {
        readable: vec![PathBuf::from("/usr/lib")],
        ..reach.clone()
    };
    assert!(
        readable
            .allows("/usr/lib/anything", Access::Reading)
            .is_ok()
    );
    let refused = readable
        .allows("/usr/lib/anything", Access::Writing)
        .expect_err("a read-only path is not writable");
    assert!(
        refused.contains("reading only"),
        "a model that has just read a file there cannot use \"outside what this session \
         reaches\": {refused}"
    );

    // ... and none of it applies when nobody asked for it
    let open = Reach {
        confined: false,
        ..reach
    };
    assert!(open.allows("/etc/passwd", Access::Reading).is_ok());
}

/// A kernel whose one tool is the confined `shell`, answering with a fixed script.
///
/// note: through the tool rather than the binary, because what a confined command leaves behind is
/// removed by whoever *spawned* it, and a test that spawned the confiner itself would be checking
/// nobody's work but its own.
fn confined_agent(workdir: &Path, script: impl IntoIterator<Item = ModelResponse>) -> Kernel {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new(script)));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.add_tool(Arc::new(Shell {
        policy: Arc::new(Careful::new()),
        workdir: workdir.to_path_buf(),
        extra: Vec::new(),
        readable: Vec::new(),
        confiner: Some(program()),
    }));

    kernel
}

#[tokio::test]
async fn a_command_takes_its_temporary_directory_with_it() {
    if !enforced() {
        return;
    }
    let kernel = confined_agent(
        &workdir("scratch"),
        [
            ModelResponse::tool_calls(vec![call(
                "1",
                "shell",
                json!({ "cmd": "printf %s \"$TMPDIR\"" }),
            )]),
            ModelResponse::text("done"),
        ],
    );
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn runs");

    let said = kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .expect("the shell answered");
    let prefix = format!("{}/kamchatka-", std::env::temp_dir().display());
    let scratch = said
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| {
            panic!("the command should have been given a TMPDIR of its own: {said}")
        });

    assert!(
        !Path::new(scratch).exists(),
        "{scratch} outlived the command it was made for"
    );
}

/// The scratch directory is never made *through* whatever is already at its name. Its name has to
/// be predictable - it is how the process that spawned the confined one finds it again - and the
/// ruleset grants the resolved path everything a writable root gets, so a link left there by
/// somebody else would have opened up whatever it pointed at and `TMPDIR` would have sent the
/// command into it.
///
/// note: the link here belongs to this test, so it is unlinked and the directory made in its
/// place; what stops one belonging to *somebody else* being unlinked is `/tmp` being sticky, and
/// there is no second account here to write that with. What this checks is the half that can be:
/// whatever the name pointed at is not what the command gets.
#[test]
fn the_scratch_directory_is_never_somebody_elses() {
    use kamchatka::sandbox::make_scratch;

    let root = workdir("scratch-name");
    let (elsewhere, path) = (root.join("elsewhere"), root.join("kamchatka-0"));
    std::fs::create_dir_all(&elsewhere).expect("somewhere to point at");
    std::fs::write(elsewhere.join("secret.txt"), "hunter2").expect("something in it");

    #[cfg(unix)]
    std::os::unix::fs::symlink(&elsewhere, &path).expect("a link where the scratch goes");
    #[cfg(not(unix))]
    std::fs::create_dir(&path).expect("a directory where the scratch goes");

    let made = make_scratch(&path).expect("it is this test's own, so it gives way");
    assert_eq!(made, path);
    assert!(
        !std::fs::symlink_metadata(&path)
            .expect("it is there")
            .is_symlink(),
        "the scratch is still the link somebody else planted"
    );
    assert_eq!(
        std::fs::read_dir(&path).expect("a directory").count(),
        0,
        "and the command was handed a directory with things already in it"
    );
    assert!(
        elsewhere.join("secret.txt").exists(),
        "what the link pointed at was opened up rather than left alone"
    );

    // and it is the user's own, for the same reason a written session is: what a command leaves in
    // here is whatever it was working on, and a fresh directory under the default umask is one
    // everyone on the machine can read
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = std::fs::metadata(&path).expect("it is there").permissions();
        assert_eq!(mode.mode() & 0o777, 0o700, "{:o}", mode.mode());
    }

    // a directory left by a run whose process identifier has come round again is the common case,
    // and the command must not inherit its leavings
    std::fs::write(path.join("stale.txt"), "from the last run").expect("a leftover");
    assert_eq!(make_scratch(&path).as_ref(), Some(&path));
    assert_eq!(
        std::fs::read_dir(&path).expect("a directory").count(),
        0,
        "the leftovers of the last run were handed to this one"
    );
}

#[tokio::test]
async fn a_stopped_command_stops_and_says_so_at_once() {
    if !enforced() {
        return;
    }
    let dir = workdir("interrupt");
    let kernel = confined_agent(
        &dir,
        [
            // three processes: the shell, what it is waiting on, and one of its own that would
            // outlive it. The file is how a command that carried on regardless says so afterwards
            ModelResponse::tool_calls(vec![call(
                "1",
                "shell",
                json!({ "cmd": "(sleep 2; touch carried-on.txt) & sleep 20" }),
            )]),
            ModelResponse::text("stopped"),
        ],
    );
    kernel.push(ContextItem::user("go"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });
    // long enough for the call to have been decided and the command to be running
    tokio::time::sleep(Duration::from_millis(600)).await;
    kernel.interrupt();

    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("a stopped command answers now, not when whatever it started has finished")
        .expect("the turn is not a panic")
        .expect("an interrupted turn is not a failed one");

    // ... and everything the command started is gone with it, rather than left running with
    // nothing watching it. A confinement that ran the shell under a helper got the first half of
    // this wrong; a kill addressed to the shell alone gets the second half wrong
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !dir.join("carried-on.txt").exists(),
        "the command outlived the call it was stopped in"
    );
}

#[test]
fn what_goes_out_as_arguments_comes_back_as_the_same_sandbox() {
    let sandbox = Sandbox {
        workdir: PathBuf::from("/tmp/work dir"),
        extra: vec![PathBuf::from("/opt/one"), PathBuf::from("/opt/two")],
        readable: vec![PathBuf::from("/opt/three")],
        writable: false,
        network: true,
    };

    let argv = sandbox.argv("echo 'hello world'; ls");
    let (read_back, cmd) = Sandbox::from_argv(&argv).expect("it is one of ours");

    assert_eq!(read_back, sandbox, "a path with a space in it survives");
    assert_eq!(cmd, "echo 'hello world'; ls");
    assert!(Sandbox::from_argv(&[]).is_none());
    assert!(Sandbox::from_argv(&["--help".into()]).is_none());
}

/// A path opened for reading is readable and is not writable.
///
/// note: what sends most people here is a toolchain. `cargo` is a rustup shim, rustup keeps its
/// settings under `$HOME`, and `$HOME` is not a system directory - so a confined `cargo --version`
/// came back `could not read settings file: Permission denied` and a live model spent six calls
/// hunting for a compiler that was installed all along. Opening `~/.rustup` read-*write* would
/// have fixed that and handed the model the ability to replace the toolchain it was about to run.
#[test]
fn a_path_opened_for_reading_is_not_a_path_that_can_be_written() {
    if !enforced() {
        return;
    }
    let outside = workdir("read-only-extra");
    std::fs::write(outside.join("settings.toml"), "default = stable").expect("something to read");

    let mut sandbox = sandbox(workdir("read-only-home"), true, false);
    sandbox.readable = vec![outside.clone()];

    let (ok, said) = run(
        &sandbox,
        &format!("cat {}/settings.toml", outside.display()),
    );
    assert!(ok, "a read-only path is readable: {said}");
    assert!(said.contains("default = stable"), "{said}");

    let (ok, said) = run(
        &sandbox,
        &format!("echo mine > {}/settings.toml", outside.display()),
    );
    assert!(!ok, "a read-only path is not writable: {said}");
    assert!(said.contains("Permission denied"), "{said}");
    assert_eq!(
        std::fs::read_to_string(outside.join("settings.toml")).expect("it is still there"),
        "default = stable",
    );

    // and truncation, which does not go through `open` and needs its own right to refuse
    let (ok, said) = run(
        &sandbox,
        &format!(
            "python3 -c \"import os; os.truncate('{}/settings.toml', 0)\"",
            outside.display()
        ),
    );
    assert!(!ok, "a read-only path cannot be truncated either: {said}");
    assert_eq!(
        std::fs::read_to_string(outside.join("settings.toml")).expect("it is still there"),
        "default = stable",
    );
}

/// Git is handed a configuration it can read, because an unreadable one is fatal to it.
///
/// note: the trap this exists for is that `access(2)` under Landlock still answers from the file's
/// own permissions. Git asks whether `~/.gitconfig` is readable, is told yes, opens it, gets
/// `EACCES`, and takes the *unreadable configuration* branch rather than the *no configuration*
/// branch: `fatal: unknown error occurred while reading the configuration files`, exit 128, and
/// every git command in the session dead. Not a warning - a coding agent that cannot run `git log`.
#[test]
fn git_is_not_killed_by_a_configuration_it_cannot_read() {
    if !enforced() {
        return;
    }
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipped: no git here");
        return;
    }

    // a home of its own, out of reach like the real one, with a configuration in it that exists.
    // A file that is merely absent is not the case that breaks git
    let home = workdir("git-home");
    std::fs::write(home.join(".gitconfig"), "[user]\n\tname = someone\n").expect("a configuration");

    let dir = workdir("git-repo");
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "one",
        ],
    ] {
        let done = Command::new("git")
            .args(&args)
            .current_dir(&dir)
            .env("HOME", &home)
            .output()
            .expect("git runs");
        assert!(
            done.status.success(),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
    }

    let confined = Sandbox {
        workdir: dir.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: false,
    };
    let child = Command::new(program())
        .args(confined.argv("git log -n 1 --oneline"))
        .env("HOME", &home)
        .env_remove("GIT_CONFIG_GLOBAL")
        .output()
        .expect("the binary under test is built");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    let _ = std::fs::remove_dir_all(kamchatka::sandbox::scratch_for(std::process::id()));

    assert!(
        child.status.success(),
        "git should still run confined: {said}"
    );
    assert!(said.contains("one"), "{said}");
    assert!(
        !said.contains("fatal"),
        "an unreadable configuration is not git's problem to solve: {said}"
    );

    // ... and opened up, it is git's own configuration again rather than nothing
    let mut opened = confined.clone();
    opened.readable = vec![home.join(".gitconfig")];
    let child = Command::new(program())
        .args(opened.argv("git config --get user.name"))
        .env("HOME", &home)
        .env_remove("GIT_CONFIG_GLOBAL")
        .output()
        .expect("the binary under test is built");
    let said = String::from_utf8_lossy(&child.stdout).into_owned();
    let _ = std::fs::remove_dir_all(kamchatka::sandbox::scratch_for(std::process::id()));

    assert_eq!(
        said.trim(),
        "someone",
        "a configuration in reach is not thrown away"
    );
}

/// A refusal that was the confinement says so, and one that was not says nothing.
#[test]
fn a_permission_error_says_when_the_confinement_caused_it() {
    let confined = Sandbox {
        workdir: PathBuf::from("/w"),
        extra: Vec::new(),
        readable: Vec::new(),
        writable: true,
        network: false,
    };

    // the shape the live session produced, down to the quotes rustup wraps the path in
    let note = confined
        .note_for(
            "error: could not read settings file: '/home/someone/.rustup/settings.toml': \
             Permission denied (os error 13)\n",
        )
        .expect("the path is out of reach, so this is the confinement");
    assert!(
        note.contains("/home/someone/.rustup/settings.toml"),
        "the useful part is which path: {note}"
    );
    assert!(
        note.contains("/w"),
        "and what it could have used instead: {note}"
    );

    // a path this reaches was refused by its own permissions, and a hedge about the sandbox would
    // send a model looking for a boundary that had nothing to do with it
    assert_eq!(
        confined.note_for("cat: /etc/shadow: Permission denied\n"),
        None
    );

    // nothing refused, nothing to say
    assert_eq!(confined.note_for("ls: no such file\n"), None);

    // refused, and naming no path this can pick out: the general answer rather than silence
    let note = confined
        .note_for("bind: Permission denied\n")
        .expect("something was refused");
    assert!(note.contains("confined"), "{note}");

    // ... and a path that has been opened up is not the confinement either. A real directory,
    // because a root that is not there is not one this opens up - the same answer `Reach::allows`
    // gives, and for the same reason
    let opened_up = workdir("opened-up");
    let mut opened = confined.clone();
    opened.readable = vec![opened_up.clone()];
    assert_eq!(
        opened.note_for(&format!(
            "error: could not read settings file: '{}/settings.toml': Permission denied \
             (os error 13)\n",
            opened_up.display()
        )),
        None,
    );
}

/// ... and the `shell` tool actually puts it in front of the model.
#[tokio::test]
async fn the_shell_tool_accounts_for_a_refusal_it_caused() {
    if !enforced() {
        return;
    }
    // outside the working directory and outside the system paths, readable by whoever runs this,
    // so that the only thing standing between the command and the file is the ruleset
    let elsewhere = workdir("out-of-reach");
    let secret = elsewhere.join("secret.txt");
    std::fs::write(&secret, "hunter2").expect("something to be refused");

    let kernel = confined_agent(
        &workdir("accounted"),
        [
            ModelResponse::tool_calls(vec![call(
                "1",
                "shell",
                json!({ "cmd": format!("cat {}", secret.display()) }),
            )]),
            ModelResponse::text("done"),
        ],
    );
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn runs");

    let said = kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .expect("the shell answered");

    assert!(said.contains("Permission denied"), "{said}");
    assert!(
        said.contains(&secret.display().to_string())
            && said.contains("outside what this session reaches"),
        "the model has no other way to tell a boundary from a protected file: {said}"
    );
    // and near the top, where an output limit cutting from the end cannot take it
    let (status, rest) = said.split_once('\n').expect("a status line");
    assert!(status.starts_with("exit: 1"), "{status}");
    assert!(
        rest.starts_with('['),
        "the note comes before the output: {said}"
    );
}
