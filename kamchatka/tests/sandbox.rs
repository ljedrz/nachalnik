//! Tests that run a command under the sandbox and check what it could not do.
//!
//! note: these spawn the real binary in its confining mode and assert on what the kernel refused,
//! because a sandbox is a claim like any other and the only way to check it is to try. A test that
//! asserted the ruleset was *built* would pass on a kernel that ignored every word of it.
//!
//! note: skipped rather than failed where Landlock is not available - which is what
//! `Confinement::Unavailable` is for. A machine that cannot enforce this should say so once, not
//! fail a suite.
//!
//! note: everything here starts a process under a confinement the kernel has to hold, which is
//! what decides what belongs here. The half that does not is `boundary.rs`: which paths `Reach`
//! admits, what a refusal names, what a confinement travels as on a command line - all of which
//! runs whether or not the kernel here confines anything.

use std::{
    net::UdpSocket,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use kamchatka::{
    sandbox::{Confinement, Network, Sandbox, available},
    tools::{Careful, Limits, Shell, Subject},
};
use nachalnik::{
    Capability, Config, ContextItem, ContextKind, Kernel, ModelResponse, OutputSink, Tool, Verdict,
    test::{AllowAll, ScriptedProvider, call},
};
use serde_json::json;

mod common;

/// Runs a command under the given sandbox, returning its output and whether it succeeded.
///
/// note: spawned rather than run in one call, and the temporary directory removed afterwards,
/// because that is what the `shell` tool does: a confined process cannot remove its own, and a
/// test that skipped it would leave one behind per command and prove nothing about the tool.
///
/// note: a gated command is answered by whoever spawned it, and here that is this test. It refuses
/// every attempt, which is what `shell` does for a session that refuses the network; the tests that
/// answer otherwise go through the tool.
fn run(sandbox: &Sandbox, cmd: &str) -> (bool, String) {
    let mut command = Command::new(common::program());
    command
        .args(sandbox.argv(cmd))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let refusing = matches!(sandbox.network, Network::Shut | Network::Asked).then(|| {
        let (stdin, arriving) = kamchatka::gate::pair().expect("the gate is built here");
        command.stdin(stdin);
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime")
                .block_on(async move {
                    if let Ok(Some(listener)) = arriving.listener().await {
                        while let Some(attempt) = listener.next().await {
                            listener.answer(attempt, false);
                        }
                    }
                })
        })
    });
    let child = command.spawn().expect("the binary under test is built");
    drop(command);
    let scratch = kamchatka::sandbox::scratch_for(child.id());
    let output = child.wait_with_output().expect("it was spawned");
    let _ = std::fs::remove_dir_all(&scratch);
    if let Some(refusing) = refusing {
        refusing
            .join()
            .expect("the refusing ends once the command has");
    }

    let mut said = String::from_utf8_lossy(&output.stdout).into_owned();
    said.push_str(&String::from_utf8_lossy(&output.stderr));

    (output.status.success(), said)
}

fn sandbox(workdir: PathBuf, writable: bool, network: Network) -> Sandbox {
    Sandbox {
        workdir,
        extra: Vec::new(),
        readable: Vec::new(),
        writable,
        network,
    }
}

/// Whether this kernel has the one right in the ruleset that is newer than the rest; the socket
/// tests say so and stop, the way `enforced` does, rather than failing on a kernel that cannot.
fn sockets() -> bool {
    match kamchatka::sandbox::confines_unix_sockets() {
        true => true,
        false => {
            eprintln!("skipped: no Landlock right for a unix socket here, which is ABI 9");
            false
        }
    }
}

/// Whether this machine can enforce any of it; the tests say so and stop rather than failing.
fn enforced() -> bool {
    match available(&common::program()).confinement {
        Confinement::Full => true,
        other => {
            eprintln!("skipped: {other}");
            false
        }
    }
}

/// Whether the gate holds here as well; the tests about it say so and stop, as `enforced` does.
fn gated() -> bool {
    match available(&common::program()) {
        probed if probed.confinement == Confinement::Full && probed.gated => true,
        _ => {
            eprintln!("skipped: the network gate does not hold here");
            false
        }
    }
}

/// A session that refused `fs:write` says so in `shell`'s own description.
///
/// note: the point of failure cannot. `Sandbox::note_for` says nothing about a refusal naming a
/// path this session reaches, because such a refusal is normally the file's own permissions - and
/// with the working directory read-only it is the boundary instead, with the same wording and
/// nothing to tell them apart. Standard error does not say whether a refusal was a read or a
/// write, so the sentence that can be certain is the one written before anything runs.
#[test]
fn a_shell_that_may_not_write_says_so_before_it_is_asked_to() {
    let dir = common::workdir("sandbox-read-only-said");
    let policy = Arc::new(Careful::new());

    let said = |policy: &Arc<Careful>| {
        Shell {
            workdir: dir.clone(),
            extra: Vec::new(),
            readable: Vec::new(),
            policy: policy.clone(),
            confiner: Some(common::program()),
            limits: Limits::default(),
        }
        .spec()
        .description
    };

    assert!(
        !said(&policy).contains("read-only in this session"),
        "nothing was refused, so nothing is said"
    );

    policy.set(&Subject::Capability(Capability::fs("write")), Verdict::Deny);
    let refused = said(&policy);
    assert!(
        refused.contains("The working directory is read-only in this session"),
        "a refused `fs:write` is a read-only working directory: {refused}"
    );
}

/// A path the ruleset cannot open is left out of it, and the confinement still holds.
///
/// note: the premise the code above `confine` rests on, and it belongs to `landlock` rather than
/// to this program: `path_beneath_rules` drops a path it cannot open rather than failing, so a
/// `--sandbox-allow` directory that has gone away costs its own rule and nothing else. The note
/// there used to say such a path made `add_rules` fail and the command run unconfined - which
/// would make this test the one that catches a version where it becomes true.
#[test]
fn a_path_the_ruleset_cannot_open_costs_its_own_rule_and_no_more() {
    use std::os::unix::fs::PermissionsExt as _;

    if !enforced() {
        return;
    }

    let dir = common::workdir("sandbox-unopenable");
    let shut = dir.join("shut");
    std::fs::create_dir_all(&shut).expect("a directory");
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).expect("shut it");
    let outside = common::scratch("sandbox-unopenable-outside");

    let mut asked = sandbox(dir.clone(), true, Network::NoTcp);
    asked.extra = vec![shut.clone(), dir.join("gone")];

    let (ok, said) = run(&asked, "echo ran > ran.txt");
    assert!(
        ok,
        "the confinement was dropped over a path it could not open: {said}"
    );
    assert!(dir.join("ran.txt").exists(), "and the command ran: {said}");

    let (ok, said) = run(&asked, &format!("echo out > {}/out.txt", outside.display()));
    assert!(!ok, "the sandbox was not in force: {said}");
    assert!(
        !outside.join("out.txt").exists(),
        "a command wrote outside the working directory: {said}"
    );

    let _ = std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700));
}

#[test]
fn a_command_can_read_and_write_inside_the_working_directory() {
    if !enforced() {
        return;
    }
    let dir = common::workdir("inside");
    let sandbox = sandbox(dir.clone(), true, Network::NoTcp);

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
    let (ok, said) = run(
        &sandbox(common::workdir("system"), true, Network::NoTcp),
        "cat /etc/passwd",
    );
    assert!(ok, "the system directories are readable on purpose: {said}");
    assert!(said.contains("root:"), "{said}");

    // readable, and no more than that
    let (wrote, said) = run(
        &sandbox(common::workdir("system"), true, Network::NoTcp),
        "touch /etc/kamchatka-probe",
    );
    assert!(!wrote, "a system directory is not writable: {said}");
}

/// `/proc` is readable, and the environment of the process that spawned a command is not.
///
/// note: the keys are taken out of a command's own environment, and `/proc/$PPID/environ` would
/// hand them straight back - `/proc` is in `SYSTEM`, and the parent is the program holding them.
/// What refuses it is Landlock rather than the file's mode: a confined process may not look into
/// one outside its domain, which is the access a process's environment is read under. Unconfined,
/// the same command reads it, and SECURITY.md says so.
#[test]
fn a_command_cannot_read_the_environment_of_the_program_that_ran_it() {
    if !enforced() {
        return;
    }
    let (ok, said) = run(
        &sandbox(common::workdir("environ"), true, Network::NoTcp),
        "cat /proc/self/environ > /dev/null && echo own && cat /proc/$PPID/environ > /dev/null",
    );
    assert!(said.contains("own"), "its own is readable: {said}");
    assert!(!ok, "the parent's environment was read: {said}");
}

#[test]
fn a_command_cannot_read_private_files_outside_it() {
    if !enforced() {
        return;
    }
    // outside the working directory *and* outside the system paths, which is the part somebody
    // installing this actually cares about
    let (ok, said) = run(
        &sandbox(common::workdir("read"), true, Network::NoTcp),
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
    //
    // note: the one path in this suite that is still in the temp directory rather than under
    // `common::scratch`, and it has to be - the claim is about the *temp directory* being closed
    // even though a writable one inside it is handed to the command, and a path under `target/`
    // would be testing something else. Nothing is left behind: the write is refused, which is
    // what the assertion below is
    let escape = std::env::temp_dir().join("kamchatka-escaped.txt");
    let _ = std::fs::remove_file(&escape);
    let (ok, said) = run(
        &sandbox(common::workdir("write-out"), true, Network::NoTcp),
        &format!("echo escaped > {}", escape.display()),
    );
    assert!(!ok, "the whole of /tmp is not writable: {said}");
    assert!(!escape.exists());

    let elsewhere = PathBuf::from("/etc/kamchatka-escaped.txt");
    let (ok_etc, said_etc) = run(
        &sandbox(common::workdir("write-etc"), true, Network::NoTcp),
        &format!("echo escaped > {}", elsewhere.display()),
    );
    assert!(!ok_etc, "writing to /etc should fail: {said_etc}");
    assert!(said_etc.contains("Permission denied"), "{said_etc}");
    assert!(!elsewhere.exists());

    // ... and a temporary file made the way a program actually makes one still works, or a
    // compiler would not run under this at all
    let (ok_tmp, said_tmp) = run(
        &sandbox(common::workdir("write-tmpdir"), true, Network::NoTcp),
        "echo scratch > \"$TMPDIR/t.txt\" && cat \"$TMPDIR/t.txt\"",
    );
    assert!(ok_tmp, "{said_tmp}");
    assert!(said_tmp.contains("scratch"), "{said_tmp}");
}

/// A command whose temporary directory could not be made is given no `TMPDIR`, rather than the one
/// it could not be made in.
#[test]
fn a_command_with_no_temporary_directory_is_given_no_tmpdir() {
    use std::os::unix::fs::PermissionsExt as _;

    if !enforced() {
        return;
    }
    let closed = common::scratch("closed-tmpdir");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o500)).expect("closed");
    if std::fs::create_dir(closed.join("probe")).is_ok() {
        eprintln!("skipped: a directory this cannot write in is one root can");
        return;
    }

    let output = Command::new(common::program())
        .args(
            sandbox(common::workdir("no-tmpdir"), true, Network::NoTcp)
                .argv("printf %s \"${TMPDIR-none}\""),
        )
        .env("TMPDIR", &closed)
        .output()
        .expect("the binary under test is built");
    let said = String::from_utf8_lossy(&output.stdout);
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o700)).expect("reopened");

    assert!(output.status.success(), "{said}");
    assert_eq!(said, "none");
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
    let dir = common::workdir("truncate");
    let outside = common::scratch("truncate-outside").join("whole.txt");
    std::fs::write(&outside, "the whole of it").expect("a file outside");

    let (_, said) = run(
        &sandbox(dir, true, Network::NoTcp),
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
    let dir = common::workdir("truncate-inside");
    let (ok, said) = run(
        &sandbox(dir.clone(), true, Network::NoTcp),
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
    let dir = common::workdir("readonly");

    let (ok, said) = run(
        &sandbox(dir.clone(), false, Network::NoTcp),
        "echo x > nope.txt",
    );

    assert!(!ok, "{said}");
    assert!(said.contains("Permission denied"), "{said}");
    assert!(!dir.join("nope.txt").exists());
    // ... and reading still works, or the shell would be useless
    let (ok, said) = run(&sandbox(dir, false, Network::NoTcp), "cat inside.txt");
    assert!(ok && said.contains("hello"), "{said}");
}

#[test]
fn a_refused_network_is_refused_by_the_kernel_rather_than_by_reading_the_command() {
    if !enforced() {
        return;
    }
    let dir = common::workdir("network");

    // Landlock alone, and the gate in front of it where there is one: both refuse a connection
    let mut refusing = vec![Network::NoTcp];
    if available(&common::program()).gated {
        refusing.push(Network::Shut);
    }
    for network in refusing {
        // the way a model asks for the network, which a policy reading the command would also
        // catch
        let (ok, said) = run(
            &sandbox(dir.clone(), true, network),
            "curl -sS --max-time 5 https://example.com",
        );
        assert!(!ok, "{network:?}: {said}");

        // ... and the way it asks after being refused, which no policy reading a command line
        // catches. This is the case that made the sandbox worth having: a live model did exactly
        // this
        let (ok, said) = run(
            &sandbox(dir.clone(), true, network),
            "python3 -c \"import urllib.request; urllib.request.urlopen('https://example.com')\"",
        );
        assert!(
            !ok,
            "{network:?}: the second way round has to fail too: {said}"
        );
        // under the gate it never gets as far as a connection: looking the name up needs a socket
        // too, and the refusal of that one reads as a lookup failing - which is why `shell` says
        // what it was
        let refused = match network {
            Network::Shut => said.contains("name resolution"),
            _ => said.contains("Permission denied") || said.contains("Errno 13"),
        };
        assert!(refused, "{network:?}: {said}");
    }
}

/// A datagram to a port this test holds, sent from a confined command; what arrived, if anything.
///
/// note: loopback rather than a DNS query, so what it checks is the confinement rather than
/// whether this machine has a network at all.
fn datagram(network: Network, name: &str) -> (bool, String, Option<Vec<u8>>) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("a port");
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("a timeout, so a refused datagram is an answer rather than a hang");
    let port = socket.local_addr().expect("it is bound").port();

    let (ok, said) = run(
        &sandbox(common::workdir(name), true, network),
        &format!(
            "python3 -c \"import socket; socket.socket(socket.AF_INET, socket.SOCK_DGRAM)\
             .sendto(b'out', ('127.0.0.1', {port}))\" 2>&1"
        ),
    );
    let mut buf = [0u8; 8];
    let arrived = socket
        .recv_from(&mut buf)
        .ok()
        .map(|(read, _)| buf[..read].to_vec());

    (ok, said, arrived)
}

/// Where there is no gate, a UDP datagram leaves the confinement untouched - which is the reason
/// every sentence about that case promises no TCP rather than no network.
///
/// note: a test that asserts a *hole* is an odd thing until you ask what closes it. The kernel
/// grew `LANDLOCK_ACCESS_NET_BIND_UDP` and `LANDLOCK_ACCESS_NET_CONNECT_SEND_UDP` in ABI 10,
/// which is Linux 7.2; the `landlock` crate exposes neither, and `AccessNet` is
/// `#[non_exhaustive]` over a sealed trait, so there is nothing to hand a ruleset from out here.
/// The day the crate grows them this fails - and what it is really holding down is the wording of
/// `Network::NoTcp`, which may stop saying TCP on the day this stops passing and not before.
#[test]
fn a_udp_datagram_goes_out_where_there_is_no_gate_and_the_words_say_so() {
    if !enforced() {
        return;
    }
    let (ok, said, arrived) = datagram(Network::NoTcp, "udp");

    assert!(ok, "{said}");
    assert_eq!(
        arrived.as_deref(),
        Some(&b"out"[..]),
        "no datagram arrived. If the crate has grown ABI 10's rights and this is now refused, \
         that is good news and every sentence promising only TCP wants rewriting"
    );
    assert!(
        sandbox(PathBuf::from("/w"), true, Network::NoTcp)
            .to_string()
            .ends_with("no TCP"),
        "what the words promise is what holds"
    );
}

/// ... and where there is one, the same datagram is refused, which is what lets the words say no
/// network.
#[test]
fn a_shut_gate_refuses_a_datagram_as_well_as_a_connection() {
    if !gated() {
        return;
    }
    let (ok, said, arrived) = datagram(Network::Shut, "udp-shut");

    assert!(!ok, "{said}");
    assert!(said.contains("Permission denied"), "{said}");
    assert_eq!(arrived, None, "a datagram got out past a shut gate");
    assert!(
        sandbox(PathBuf::from("/w"), true, Network::Shut)
            .to_string()
            .ends_with("no network")
    );
}

/// What a confined command connects to, in the one spelling every one of these tests uses.
fn connect_to(socket: &Path) -> String {
    format!(
        "python3 -c \"import socket; socket.socket(socket.AF_UNIX).connect('{}')\" 2>&1",
        socket.display()
    )
}

/// The hole the UDP one is measured against, and the one that was worth closing: a datagram
/// carries bytes out, where a socket carries a *command* out.
///
/// note: this was live. Under a confinement that refused it the home directory directly,
/// `systemd-run --user` over `/run/user/<uid>/bus` read and wrote there anyway, because the work
/// was done by a process that was never in the domain. The session bus, the compositor and the
/// container daemon are each a pathname socket under `/run`, which is readable because the system
/// directories are - and below Linux 7.1 a `connect` was governed by no access right at all, so
/// the ruleset was not consulted about any of them.
#[test]
fn a_command_cannot_connect_to_a_unix_socket_it_could_not_write_to() {
    if !enforced() || !sockets() {
        return;
    }
    let outside = common::scratch("connect-outside").join("s.sock");
    let listening = UnixListener::bind(&outside).expect("a socket outside the working directory");

    let (ok, said) = run(
        &sandbox(common::workdir("connect"), true, Network::NoTcp),
        &connect_to(&outside),
    );

    assert!(
        !ok,
        "a socket outside the working directory was connected to: {said}"
    );
    assert!(said.contains("Permission denied"), "{said}");
    drop(listening);
}

/// And it still works where the command may write, which is what the rule costs: the socket a
/// session listens on is in a directory somebody handed it read-write, and a confinement that
/// refused that would take the session with it.
#[test]
fn a_command_can_connect_to_one_it_could_have_written() {
    if !enforced() || !sockets() {
        return;
    }
    let dir = common::workdir("connect-inside");
    let inside = dir.join("s.sock");
    let listening = UnixListener::bind(&inside).expect("a socket in the working directory");

    let (ok, said) = run(&sandbox(dir, true, Network::NoTcp), &connect_to(&inside));

    assert!(ok, "{said}");
    drop(listening);
}

/// A read-only working directory is read-only for this too, and a path opened up for reading
/// alone stays that way.
///
/// note: connecting is the writing half of the rule rather than the reading half, because what
/// comes back from a socket is whatever the process behind it was willing to do. A `--sandbox-read`
/// path that let a command drive a daemon would be the one flag in here that does not mean what
/// it says.
#[test]
fn reading_a_path_is_not_connecting_to_a_socket_in_it() {
    if !enforced() || !sockets() {
        return;
    }
    let opened = common::scratch("connect-readable");
    let socket = opened.join("s.sock");
    let listening = UnixListener::bind(&socket).expect("a socket in the readable directory");

    let mut readable = sandbox(common::workdir("connect-read"), true, Network::NoTcp);
    readable.readable = vec![opened];
    let (ok, said) = run(&readable, &connect_to(&socket));

    assert!(
        !ok,
        "a socket in a path opened for reading alone was connected to: {said}"
    );
    assert!(said.contains("Permission denied"), "{said}");
    drop(listening);
}

/// Two paths can be opened up in one flag, and both of them arrive.
///
/// note: through the program rather than through `Reach`, because what was in doubt is the
/// argument: `--sandbox-allow` took one value per use, so two paths meant saying it twice and
/// `--sandbox-allow a b` quietly read `b` as the message to send. It takes a comma-separated list
/// now, the way `--allow` and `--deny` do, and this asks the program what it ended up with -
/// `shell` names the paths that were opened up in its own description, so `/tools` is the answer
/// without a model anywhere.
#[test]
fn two_paths_go_in_as_one_flag() {
    if !enforced() {
        return;
    }

    let mut child = Command::new(common::program())
        .args([
            "-m",
            "nothing-serves-this",
            "--no-record",
            "--sandbox-allow",
            "/usr/share,/usr/include",
            "--sandbox-read",
            "/usr/lib,/usr/bin",
        ])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin is a pipe");
        stdin.write_all(b"/tools\n").expect("the line was not sent");
    }
    let out = child.wait_with_output().expect("the program never ended");
    let said = String::from_utf8_lossy(&out.stderr);

    for path in [
        "/usr/share read-write",
        "/usr/include read-write",
        "/usr/lib read-only",
        "/usr/bin read-only",
    ] {
        assert!(said.contains(path), "`{path}` did not arrive: {said}");
    }
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
        limits: Limits::default(),
        policy: Arc::new(Careful::new()),
        workdir: workdir.to_path_buf(),
        extra: Vec::new(),
        readable: Vec::new(),
        confiner: Some(common::program()),
    }));

    kernel
}

#[tokio::test]
async fn a_command_takes_its_temporary_directory_with_it() {
    if !enforced() {
        return;
    }
    let kernel = confined_agent(
        &common::workdir("scratch"),
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

/// A call dropped while its command runs takes the command's temporary directory with it, as it
/// takes the command.
#[tokio::test]
async fn a_dropped_call_takes_its_temporary_directory_with_it() {
    if !enforced() {
        return;
    }
    let dir = common::workdir("dropped-scratch");
    let shell = Shell {
        limits: Limits::default(),
        policy: Arc::new(Careful::new()),
        workdir: dir.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        confiner: Some(common::program()),
    };
    let args = json!({ "cmd": "printf %s \"$TMPDIR\" > where.txt; sleep 20" });

    let dropped = tokio::time::timeout(
        Duration::from_secs(2),
        shell.invoke(&call("1", "shell", args), OutputSink::disconnected()),
    )
    .await;
    assert!(
        dropped.is_err(),
        "the command was meant to still be running"
    );

    let scratch = std::fs::read_to_string(dir.join("where.txt")).expect("the command said where");
    assert!(scratch.contains("kamchatka-"), "{scratch}");
    assert!(
        !Path::new(&scratch).exists(),
        "{scratch} outlived the call it was made for"
    );
}

#[tokio::test]
async fn a_stopped_command_stops_and_says_so_at_once() {
    if !enforced() {
        return;
    }
    let dir = common::workdir("interrupt");
    let kernel = confined_agent(
        &dir,
        [
            // three processes: the shell, what it is waiting on, and one of its own that would
            // outlive it. `carried-on.txt` is how a command that carried on regardless says so
            // afterwards, and `started.txt` says all three are there to be stopped
            ModelResponse::tool_calls(vec![call(
                "1",
                "shell",
                json!({ "cmd": "(sleep 2; touch carried-on.txt) & touch started.txt; sleep 20" }),
            )]),
            ModelResponse::text("stopped"),
        ],
    );
    kernel.push(ContextItem::user("go"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });
    // an interrupt before the command is running stops nothing, and would pass for one that
    // stopped everything
    let waited = std::time::Instant::now();
    while !dir.join("started.txt").exists() {
        assert!(
            waited.elapsed() < Duration::from_secs(10),
            "the command never started"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
    let outside = common::workdir("read-only-extra");
    std::fs::write(outside.join("settings.toml"), "default = stable").expect("something to read");

    let mut sandbox = sandbox(common::workdir("read-only-home"), true, Network::NoTcp);
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
    let home = common::workdir("git-home");
    std::fs::write(home.join(".gitconfig"), "[user]\n\tname = someone\n").expect("a configuration");

    let dir = common::workdir("git-repo");
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
        network: Network::NoTcp,
    };
    // spawned rather than run in one call, for the reason `run` is: the directory a confined
    // command gets is named after *that* command, it cannot remove its own, and only whoever
    // spawned it knows the identifier. These two used to call `output()` and then remove
    // `scratch_for(std::process::id())` - this test's own identifier, naming a directory that
    // never existed - so every run of this file left two of the child's behind for good
    let spawned = Command::new(common::program())
        .args(confined.argv("git log -n 1 --oneline"))
        .env("HOME", &home)
        .env_remove("GIT_CONFIG_GLOBAL")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    let scratch = kamchatka::sandbox::scratch_for(spawned.id());
    let child = spawned.wait_with_output().expect("it was spawned");
    let _ = std::fs::remove_dir_all(&scratch);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );

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
    let spawned = Command::new(common::program())
        .args(opened.argv("git config --get user.name"))
        .env("HOME", &home)
        .env_remove("GIT_CONFIG_GLOBAL")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    let scratch = kamchatka::sandbox::scratch_for(spawned.id());
    let child = spawned.wait_with_output().expect("it was spawned");
    let _ = std::fs::remove_dir_all(&scratch);
    let said = String::from_utf8_lossy(&child.stdout).into_owned();

    assert_eq!(
        said.trim(),
        "someone",
        "a configuration in reach is not thrown away"
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
    let elsewhere = common::workdir("out-of-reach");
    let secret = elsewhere.join("secret.txt");
    std::fs::write(&secret, "hunter2").expect("something to be refused");

    let kernel = confined_agent(
        &common::workdir("accounted"),
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

/// A confined shell whose policy knows the gate holds, and the policy, for answering it.
fn gated_shell(dir: &Path) -> (Shell, Arc<Careful>) {
    let policy = Arc::new(Careful::new());
    policy.gate_the_network();
    let shell = Shell {
        limits: Limits::default(),
        policy: policy.clone(),
        workdir: dir.to_path_buf(),
        extra: Vec::new(),
        readable: Vec::new(),
        confiner: Some(common::program()),
    };

    (shell, policy)
}

/// Answers every question a command asks with `allow`, counting them, until it is dropped.
struct Answering {
    asked: Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Answering {
    fn asked(&self) -> usize {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Answering {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn answering(policy: &Arc<Careful>, allow: bool) -> Answering {
    let policy = policy.clone();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut woken = policy.reaching().subscribe();
    let task = tokio::spawn({
        let asked = asked.clone();
        async move {
            while woken.changed().await.is_ok() {
                for question in policy.reaching().waiting() {
                    if policy.reaching().answer(question.id, allow).is_ok() {
                        asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
        }
    });

    Answering { asked, task }
}

/// Runs one command through the tool, and gives up on it rather than on the suite.
async fn through(shell: &Shell, cmd: &str) -> String {
    tokio::time::timeout(
        Duration::from_secs(20),
        shell.invoke(
            &call("1", "shell", json!({ "cmd": cmd })),
            OutputSink::disconnected(),
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("`{cmd}` never answered"))
    .expect("the tool answers either way")
    .content
    .to_text()
    .into_owned()
}

/// Two datagrams from one command, each on a socket of its own, to a port this test holds; what
/// arrived.
fn two_datagrams(port: u16) -> String {
    format!(
        "python3 -c \"
import socket
for word in (b'one', b'two'):
    try:
        socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(word, ('127.0.0.1', {port}))
        print('sent', word.decode())
    except OSError as e:
        print('refused', e.errno)
\""
    )
}

fn listening() -> (UdpSocket, u16) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("a port");
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("a timeout");
    let port = socket.local_addr().expect("it is bound").port();

    (socket, port)
}

fn heard(socket: &UdpSocket) -> Vec<String> {
    let mut buf = [0u8; 8];
    std::iter::from_fn(|| {
        socket
            .recv_from(&mut buf)
            .ok()
            .map(|(read, _)| String::from_utf8_lossy(&buf[..read]).into_owned())
    })
    .collect()
}

/// A command is asked about when it reaches for the network, once, and not before.
///
/// note: the point of the gate. Read off its name, `python3` is not a program that wants the
/// network and would never have been asked about; `git status` is one and would have been. Here
/// the question is the attempt itself, and a command that never makes one runs unasked.
#[tokio::test]
async fn a_command_is_asked_about_when_it_reaches_for_the_network_and_not_before() {
    if !gated() {
        return;
    }
    let (shell, policy) = gated_shell(&common::workdir("gate-yes"));
    let answers = answering(&policy, true);

    let said = through(&shell, "echo quiet").await;
    assert!(said.starts_with("exit: 0"), "{said}");
    assert!(!said.contains("reached for the network"), "{said}");
    assert_eq!(
        answers.asked(),
        0,
        "a command that never reached out was asked about"
    );

    let (socket, port) = listening();
    let said = through(&shell, &two_datagrams(port)).await;
    assert!(
        said.contains("sent one") && said.contains("sent two"),
        "{said}"
    );
    assert!(
        said.contains("was asked and let it"),
        "the model is told whose answer it was: {said}"
    );
    assert_eq!(heard(&socket), ["one", "two"]);
    assert_eq!(
        answers.asked(),
        1,
        "two sockets, and one question about the command"
    );
    assert!(policy.reaching().waiting().is_empty());
}

/// A no refuses every internet socket the command asks for, and the model is told who said it.
#[tokio::test]
async fn a_no_refuses_every_socket_the_command_asks_for() {
    if !gated() {
        return;
    }
    let (shell, policy) = gated_shell(&common::workdir("gate-no"));
    let answers = answering(&policy, false);
    let (socket, port) = listening();

    let said = through(&shell, &two_datagrams(port)).await;

    assert_eq!(answers.asked(), 1);
    assert_eq!(said.matches("refused 13").count(), 2, "{said}");
    assert!(heard(&socket).is_empty(), "a datagram got out after a no");
    assert!(said.contains("was asked and said no"), "{said}");
    // near the top, where an output limit cutting from the end cannot take it
    assert!(
        said.lines()
            .nth(1)
            .is_some_and(|line| line.starts_with('[')),
        "{said}"
    );
}

/// A session that refuses the network refuses it without asking anybody, and says so.
#[tokio::test]
async fn a_refused_network_is_refused_without_a_question_and_said() {
    if !gated() {
        return;
    }
    let (shell, policy) = gated_shell(&common::workdir("gate-deny"));
    policy.set(
        &Subject::Capability(Capability::net("reach")),
        Verdict::Deny,
    );
    let (socket, port) = listening();

    let said = through(&shell, &two_datagrams(port)).await;

    assert!(policy.reaching().waiting().is_empty());
    assert_eq!(said.matches("refused 13").count(), 2, "{said}");
    assert!(heard(&socket).is_empty());
    assert!(said.contains("which this session refuses"), "{said}");
}

/// A stopped command takes its question with it, rather than leaving one nobody's answer reaches.
#[tokio::test]
async fn a_stopped_command_takes_its_question_with_it() {
    if !gated() {
        return;
    }
    let dir = common::workdir("gate-stopped");
    let (shell, policy) = gated_shell(&dir);
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call(
            "1",
            "shell",
            json!({ "cmd": "python3 -c \"import socket; socket.socket()\"" }),
        )]),
        ModelResponse::text("stopped"),
    ])));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.add_tool(Arc::new(shell));
    kernel.push(ContextItem::user("go"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });
    let waited = std::time::Instant::now();
    while policy.reaching().first().is_none() {
        assert!(
            waited.elapsed() < Duration::from_secs(10),
            "the command never asked"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    kernel.interrupt();

    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("a command waiting on a question stops when asked to")
        .expect("the turn is not a panic")
        .expect("an interrupted turn is not a failed one");
    assert!(
        policy.reaching().waiting().is_empty(),
        "{:?}",
        policy.reaching().waiting()
    );
}
