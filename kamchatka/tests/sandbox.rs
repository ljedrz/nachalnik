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
fn a_command_cannot_read_outside_it() {
    if !enforced() {
        return;
    }
    // something that certainly exists and certainly is not in the working directory
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
    use kamchatka::sandbox::Reach;

    let dir = workdir("reach").canonicalize().expect("it exists");
    let reach = Reach {
        workdir: dir.clone(),
        extra: vec![PathBuf::from("/usr/share")],
        confined: true,
    };

    // inside, by any spelling. `./` is in here because the first live run of this came back
    // `Not a directory`: an empty remainder joined onto a resolved path appends a separator
    assert_eq!(reach.allows("inside.txt"), Ok(dir.join("inside.txt")));
    assert_eq!(reach.allows("./inside.txt"), Ok(dir.join("inside.txt")));
    assert_eq!(reach.allows("."), Ok(dir.clone()));
    assert!(
        reach
            .allows(dir.join("inside.txt").to_str().unwrap())
            .is_ok()
    );
    // ... including one that is not there yet, which is most of what `write` is handed. Compared
    // as strings, deliberately: a `PathBuf` compares by component, so a separator on the end of
    // one is invisible to `assert_eq!` and visible to every `fs` call there is. That is how a
    // `write` which could not create a single file went on passing a test that asserted `Ok`
    let name = |path: &str| reach.allows(path).map(PathBuf::into_os_string);
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
    let made = reach.allows("not-there-yet.txt").expect("it is inside");
    std::fs::write(&made, "made").expect("a file about to be created can be created");

    // outside, by every spelling somebody would reach for
    assert!(reach.allows("/etc/passwd").is_err());
    assert!(
        reach.allows("../../../etc/passwd").is_err(),
        "`..` is resolved, not matched"
    );
    assert!(reach.allows("/etc/../etc/passwd").is_err());

    // ... including through a symlink, which is why the path is resolved rather than compared
    let link = dir.join("out");
    std::os::unix::fs::symlink("/etc", &link).expect("a symlink");
    assert!(
        reach.allows(link.join("passwd").to_str().unwrap()).is_err(),
        "a symlink out is still out"
    );

    // what was opened up on purpose
    assert!(reach.allows("/usr/share/anything").is_ok());

    // ... and none of it applies when nobody asked for it
    let open = Reach {
        confined: false,
        ..reach
    };
    assert!(open.allows("/etc/passwd").is_ok());
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

#[test]
fn what_goes_out_as_arguments_comes_back_as_the_same_sandbox() {
    let sandbox = Sandbox {
        workdir: PathBuf::from("/tmp/work dir"),
        extra: vec![PathBuf::from("/opt/one"), PathBuf::from("/opt/two")],
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
