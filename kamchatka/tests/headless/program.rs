//! The program itself, spawned: the streams it keeps apart, the signals it is sent, the records it
//! leaves and the flags that only the binary reads.

use std::sync::Arc;

use kamchatka::tools::Subject;
use nachalnik::{
    ContextItem, ContextKind, ModelResponse, Record, Verdict,
    test::{ScriptedProvider, call},
};
use serde_json::json;

use crate::{common, priced, run, wired};

/// The program itself runs headless, and keeps its two streams apart.
///
/// note: everything else here drives the loop in process, which says nothing about the half a
/// caller actually meets: the arguments, the mode it chose, and which stream each thing goes to.
/// This one runs the binary. It cannot get as far as a model - there is no endpoint to reach and
/// no key to reach it with - and that is the case worth pinning anyway, because a program whose
/// stdout is a stream of JSON must not put a sentence in it when something goes wrong. A reader
/// would be part way through a session before hitting a line that is not a record.
///
/// note: no `--headless` in the arguments. The mode is chosen because stdout is a pipe here,
/// which is the auto-detection doing its job, and the announcement is on stderr where it belongs.
#[test]
fn the_program_runs_headless_and_keeps_its_streams_apart() {
    let out = common::command()
        .args(["-m", "nothing-serves-this", "--no-record", "hello"])
        // an address nothing answers on: the session is set up, the request is built, and the
        // send is what fails - which is the failure a piped run actually meets
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("stdout is not a terminal"),
        "it did not say which mode it chose: {said}"
    );
    assert!(
        said.contains("error sending request"),
        "the failure was not reported: {said}"
    );
    assert!(
        !out.status.success(),
        "an unreachable model is not a success"
    );

    // not that stdout is empty - a session that failed still happened, and its log is the whole
    // point of the stream. What matters is that every line of it is a record: one sentence in
    // there and a reader is part way through a session before hitting something it cannot parse
    let records = String::from_utf8(out.stdout).expect("the records are text");
    assert!(
        !records.is_empty(),
        "the session was not written out at all"
    );
    for line in records.lines() {
        serde_json::from_str::<Record>(line).unwrap_or_else(|e| {
            panic!("a line of the record stream is not a record ({e}): {line}")
        });
    }
}

/// A run told to be headless is not told which mode it chose, because it said so itself.
///
/// note: the announcement is there because a program that draws or does not draw depending on what
/// is on the other end of a pipe should say which it decided, and `--headless` is how somebody says
/// it themselves. Said to a run that asked for the mode, it is the program explaining an instruction
/// back to the person who gave it. A scripted run reads stderr for failures, and a run told to be
/// headless told the script itself.
#[test]
fn a_run_that_asked_to_be_headless_is_not_told_which_mode_it_chose() {
    let out = common::command()
        .args(["--headless", "--no-record"])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        !said.contains("so this is a headless run"),
        "a run that was asked for the mode was told it chose it: {said}"
    );
}

/// A headless run does not greet, and what it does say is the line driver's.
///
/// note: the greeting is `main.rs`'s and belongs to a session with a screen in front of it: it
/// names `ctrl+p` and `F1`, which are this program's keys, and a headless run has none. The test
/// beside this one is about what a piped run says about the mode it chose; this is about what it
/// says into the conversation, which is in the same stream. A greeting said here would be printed
/// to everybody who pipes a session in and reads it as though it were the session's own opening.
#[cfg(feature = "tui")]
#[test]
fn a_headless_run_says_the_line_driver_and_not_the_terminals_keys() {
    let out = common::command()
        .args(["--headless", "--no-record"])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("a line is a message"),
        "a headless run did not say what its lines are: {said}"
    );
    assert!(
        !said.contains("F1 lists the keys"),
        "a headless run with no keys to press was told about them: {said}"
    );
}

/// An empty message given on the command line is nothing, where an empty line down a pipe is.
///
/// note: the two are the same act - a first message, sent as soon as it starts - and the line
/// driver drops an empty one, while this asked whether any argument was given at all. A script
/// doing `kamchatka ... "$MESSAGE"` with the variable never set paid a round trip and a failed
/// turn for a request the provider refuses.
///
/// note: no endpoint to reach, which is what makes the run's own answer the assertion. A blank
/// message sends nothing, so the run is over; one that was sent is a turn that failed, and its
/// record says so.
#[test]
fn an_empty_message_on_the_command_line_is_nothing() {
    let out = common::command()
        // the shape a script has when the variable it interpolates is unset or all spaces
        .args(["-m", "nothing-serves-this", "--no-record", " ", "  "])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "a blank message was sent: {said}");

    let records = String::from_utf8(out.stdout).expect("the records are text");
    let names: Vec<String> = records
        .lines()
        .map(|line| serde_json::from_str::<Record>(line).expect("every line is a record"))
        .map(|record| record.event.name().to_owned())
        .collect();
    assert!(!names.contains(&"model.requested".to_owned()), "{names:?}");
    assert_eq!(names.last().map(String::as_str), Some("session.finished"));
}

/// A run nobody is reading the streams of says which mode it chose and carries on anyway.
///
/// note: the announcement a piped run gets is `eprintln!`ed, and the macros panic when the write
/// fails. A script that gives the program `2>/dev/full`, or closes its stderr, so that the run's
/// own chatter is out of the way, then had its whole run end in a panic and exit `101` - before
/// the session was written, and with nothing but the panic to say so. Saying which mode was chosen
/// cannot be allowed to end the run that was told.
///
/// note: closed rather than redirected, which is what makes the write fail: `/dev/null` accepts
/// everything. A pipe whose read end this end has let go of is the shape a script's is in once it
/// has stopped caring, and a broken pipe is what the program meets there.
///
/// note: `--no-record`, so the assertion is about the run rather than about a file, and the
/// record stream left on stdout is what a reader of a piped run is promised.
#[test]
fn a_run_nobody_is_reading_still_says_which_mode_it_chose() {
    let mut child = common::command()
        .args(["-m", "nothing-serves-this", "--no-record", "hello"])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    // the announcement went into a pipe nobody is going to read, and the run carries on regardless
    drop(child.stderr.take().expect("a pipe"));
    let out = child.wait_with_output().expect("the program never ended");

    assert_ne!(
        out.status.code(),
        Some(101),
        "it panicked on a line it could not write"
    );
    // and the record stream is still a stream of records, which is the promise the mode makes
    let records = String::from_utf8(out.stdout).expect("the records are text");
    assert!(
        !records.is_empty(),
        "the session was not written out at all"
    );
    for line in records.lines() {
        serde_json::from_str::<Record>(line).unwrap_or_else(|e| {
            panic!("a line of the record stream is not a record ({e}): {line}")
        });
    }
}

/// A resumed run still says how it is being driven.
///
/// note: it used to say one or the other. The opening line and the replay line were arms of one
/// match over `(resumed, headless)`, so a session carried on from a file was told what it had
/// picked up and not what would happen to a question nobody is there to answer - which is the
/// thing a headless run most needs said, and the thing a resumed one is no less headless for.
///
/// note: through the binary, because the branch is `main`'s. No key and no endpoint: resuming a
/// file and printing two lines reaches neither.
#[test]
fn a_resumed_headless_run_says_both_what_it_picked_up_and_how_it_is_driven() {
    let (wired, dir) = (wired(Vec::new()), common::scratch("resumed"));
    wired
        .app
        .kernel
        .push(nachalnik::ContextItem::user("the word is ZEPHYR"));
    let path = dir.join("session.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&wired.app.kernel.snapshot()).expect("a snapshot serializes"),
    )
    .expect("written");

    let out = common::command()
        .args(["--headless", "--no-record", "-r"])
        .arg(&path)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("resumed session"), "{said}");
    assert!(
        said.contains("a question nobody can be asked is answered"),
        "a resumed run was not told how it is driven: {said}"
    );
}

/// `ctrl+c` stops a run that is waiting for somebody, and the session survives it.
///
/// note: the last of the three things that can stop a run nobody is watching - the other two have
/// had tests since they were written and this one had none, because it is a *signal* and the suite
/// had no way to send one. It does: the run is a child process, and `kill -INT` is a command.
/// What it asserts is the difference between stopping and being killed - the line that says so,
/// the `session.finished` record at the end of the log, and the status a stopped run has rather
/// than a failure's or a finished one's.
#[test]
fn ctrl_c_stops_a_headless_run_rather_than_killing_it() {
    use std::io::Read as _;

    // stdin is a pipe this test holds open and never writes to, which is a run waiting for
    // somebody who has not typed anything yet - the state `ctrl+c` is for
    let mut child = common::command()
        .args(["--headless", "--no-record", "-m", "nothing-serves-this"])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    // once it has said what it is, it is in the loop with the signal branch armed
    let mut said = String::new();
    let mut stderr = child.stderr.take().expect("stderr is a pipe");
    while !said.contains("headless:") {
        let mut byte = [0u8; 1];
        if stderr.read(&mut byte).expect("it is still running") == 0 {
            panic!("it ended before it was interrupted: {said}");
        }
        said.push(byte[0] as char);
    }

    let killed = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("`kill` is on the path");
    assert!(killed.success());

    let status = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match child.try_wait().expect("it was spawned") {
                Some(status) => break status,
                None if std::time::Instant::now() > deadline => {
                    let _ = child.kill();
                    panic!("it did not stop: {said}");
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    };
    stderr.read_to_string(&mut said).expect("the rest of it");

    // `130`, as a shell reports a program `ctrl+c` ended: a run stopped short is not one that
    // finished, and a script is owed the difference
    assert_eq!(status.code(), Some(130), "{said}");
    assert!(
        said.contains("what has arrived is kept"),
        "it did not say it was stopping: {said}"
    );

    // and the log is a whole session rather than however much of one had been flushed, which is
    // the difference a cooperative stop is for
    let mut records = String::new();
    child
        .stdout
        .take()
        .expect("stdout is a pipe")
        .read_to_string(&mut records)
        .expect("the records are text");
    let names: Vec<String> = records
        .lines()
        .map(|line| {
            serde_json::from_str::<Record>(line)
                .expect("every line is a record")
                .event
                .name()
                .to_owned()
        })
        .collect();
    assert_eq!(names.last().map(String::as_str), Some("session.finished"));
}

/// A run ended from outside says so, whether or not a turn was running.
///
/// note: `ctrl+c` says it and the deadline says it, and neither of the signals that end a run
/// without a keyboard said anything at all - so a run closed by `docker stop`, an ssh drop or
/// `timeout` ended on a record and a `0` and looked exactly like one that had finished its work.
/// The line is what tells a reader of the tail that the run did not get to choose, and `143` - `128`
/// and `SIGTERM` - is what tells a script.
///
/// note: both an idle run and one with a turn in flight, because they are different code paths. A
/// turn running is interrupted and waited for; a run with nothing to do reaches the signal in a
/// `select!` where every other branch is asleep, and the same line has to come out of that.
#[test]
fn a_termination_signal_says_it_ended_the_run() {
    use std::io::Read as _;

    for what in ["an idle run", "a run with a turn in flight"] {
        // a pipe this test holds open, which is a run waiting for somebody who has not typed
        // anything yet - the state a signal is most often the only thing to end
        let mut child = common::command()
            .args(["--headless", "--no-record", "-m", "nothing-serves-this"])
            .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary under test is built");

        // once it has said what it is, it is in the loop with the signal branch armed
        let mut said = String::new();
        let mut stderr = child.stderr.take().expect("stderr is a pipe");
        while !said.contains("headless:") {
            let mut byte = [0u8; 1];
            if stderr.read(&mut byte).expect("it is still running") == 0 {
                panic!("{what} ended before it was signalled: {said}");
            }
            said.push(byte[0] as char);
        }

        let sent = std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .expect("`kill` is on the path");
        assert!(sent.success());

        let status = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                match child.try_wait().expect("it was spawned") {
                    Some(status) => break status,
                    None if std::time::Instant::now() > deadline => {
                        let _ = child.kill();
                        panic!("{what} did not stop: {said}");
                    }
                    None => std::thread::sleep(std::time::Duration::from_millis(50)),
                }
            }
        };
        stderr.read_to_string(&mut said).expect("the rest of it");

        assert_eq!(status.code(), Some(143), "{what}: {said}");
        assert!(
            said.contains("ended by a termination signal"),
            "{what} did not say a signal ended it: {said}"
        );
    }
}

/// What a command left running is stopped when the run ends, and left with `--leave-running` - and
/// named either way.
///
/// note: `sleep 30 &` outlived the program, reparented to init, with nothing at exit saying it was
/// there. The job writes its identifier so the test can see which it is.
#[tokio::test(flavor = "multi_thread")]
async fn what_a_command_left_running_ends_with_the_run() {
    for leave in [false, true] {
        let dir = common::scratch(&format!("stragglers-{leave}"))
            .canonicalize()
            .expect("it exists");
        let cmd = "sleep 30 >/dev/null 2>&1 & echo $! > job.pid";
        let base = common::endpoint(vec![
            format!(
                "data: {}",
                json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
                    "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                    "function": {"name": "shell",
                        "arguments": json!({"cmd": cmd}).to_string()}}
                ]}, "finish_reason": "tool_calls"}]})
            ),
            common::answer("started it"),
        ])
        .await;

        let mut command = common::command();
        command
            .args([
                "--headless",
                "--no-record",
                "-m",
                "nothing",
                "--allow",
                "exec:run,fs:write",
            ])
            .arg("go")
            .current_dir(&dir)
            .env("KAMCHATKA_BASE_URL", &base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .stdin(std::process::Stdio::null());
        if leave {
            command.arg("--leave-running");
        }
        let out = command.output().expect("the binary under test is built");
        let said = String::from_utf8_lossy(&out.stderr);

        let job: i32 = std::fs::read_to_string(dir.join("job.pid"))
            .unwrap_or_else(|e| panic!("the command never ran ({e}): {said}"))
            .trim()
            .parse()
            .expect("a process identifier");
        let running = std::path::Path::new(&format!("/proc/{job}")).exists();
        match leave {
            true => {
                assert!(
                    said.contains("left running, as `--leave-running` asked"),
                    "{said}"
                );
                assert!(running, "it was stopped although it was to be left: {said}");
                let _ = std::process::Command::new("kill")
                    .arg(job.to_string())
                    .status();
            }
            false => {
                assert!(said.contains("stopped what it had left running"), "{said}");
                // reaped by init, which takes a moment once the signal has landed
                let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while std::path::Path::new(&format!("/proc/{job}")).exists()
                    && std::time::Instant::now() < until
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                assert!(
                    !std::path::Path::new(&format!("/proc/{job}")).exists(),
                    "the job outlived the run: {said}"
                );
            }
        }
        assert!(said.contains(cmd), "it did not name the command: {said}");
    }
}

/// A resumed run says the parameters it is sending with, when there are any.
///
/// note: they come back in force from the snapshot and nothing on the screen shows them, so a
/// file somebody left `max_tokens: 5` in is a reason the next answer is short with nothing on
/// this program to explain it. A snapshot with none says nothing: there was nothing in force, and
/// a line announcing an empty map is one more thing to read.
#[test]
fn a_resumed_run_says_the_parameters_it_came_back_with() {
    for params in [
        serde_json::json!({ "max_tokens": 5 }),
        serde_json::json!({}),
    ] {
        let (wired, dir) = (wired(Vec::new()), common::scratch("resumed-params"));
        wired
            .app
            .kernel
            .push(nachalnik::ContextItem::user("the word is ZEPHYR"));
        wired
            .app
            .kernel
            .set_params(params.as_object().cloned().expect("an object"));
        let path = dir.join("session.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&wired.app.kernel.snapshot()).expect("a snapshot serializes"),
        )
        .expect("written");

        let out = common::command()
            .args(["--headless", "--no-record", "-r"])
            .arg(&path)
            .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the binary under test is built");

        let said = String::from_utf8_lossy(&out.stderr);
        let line = said
            .lines()
            .find(|line| line.contains("resumed session"))
            .unwrap_or_else(|| panic!("it said nothing about what it picked up: {said}"));
        assert_eq!(
            line.contains("parameters, sent verbatim: {\"max_tokens\":5}"),
            !params.as_object().expect("an object").is_empty(),
            "{line}"
        );
    }
}

/// A resumed run's spend is its own, and says so where the figure is.
///
/// note: a total that starts at nothing in every process reads as the session's to anybody carried
/// on with `-r`, and `0` is the case that misleads - right, in a session that had plainly spent
/// something, answering a question nobody asked. `/budget` counts what the counter learned, which
/// a snapshot does carry, so the two are on different bases.
///
/// note: through the binary, because the reset is between `main` and `App`: what a snapshot
/// carries is the runtime's, and a total is the program's.
#[test]
fn a_resumed_run_says_the_spend_is_this_runs() {
    use std::io::Write as _;

    // a session charged for something big enough to be worth learning from: a small request is
    // mostly framing, and a scale drawn from one is a picture of the framing
    let first = nachalnik::Kernel::new(nachalnik::Config::default());
    first.set_provider(std::sync::Arc::new(ScriptedProvider::new([priced(
        ModelResponse::text("done"),
        4_000,
        100,
    )])));
    first.push(ContextItem::file("big.rs", "a line of it\n".repeat(400)));
    first.push(ContextItem::user("go"));
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime")
        .block_on(first.turn())
        .expect("the turn ran");
    assert!(
        first.snapshot().calibration.is_some(),
        "the fixture must have taught the counter something"
    );

    let dir = common::scratch("resumed-spend");
    let path = dir.join("session.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&first.snapshot()).expect("a snapshot serializes"),
    )
    .expect("written");

    let mut child = common::command()
        .args(["--headless", "--no-record", "-r"])
        .arg(&path)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    child
        .stdin
        .take()
        .expect("a pipe")
        .write_all(b"/spend\n/budget\n")
        .expect("the commands were sent");
    let out = child.wait_with_output().expect("the program never ended");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("this run has spent 0 tokens"),
        "the figure did not say which run it is: {said}"
    );
    // and the other half: the counter's learning came back with the snapshot, on the other basis
    // from the spend above
    assert!(
        said.contains("the counter has learned from"),
        "the resumed run's budget said nothing about its counter: {said}"
    );
}

/// The program finds the gate for itself and puts the shell behind it: a command that opens a
/// socket under `--allow exec:run` is asked about when it tries, and a run with nobody at it
/// answers `deny`.
///
/// note: through the binary, because the half this is about is `Setup::wire` - the probe finding
/// that the gate holds, and the policy being told so. Everything past that is tested in process,
/// where no probe runs.
#[tokio::test(flavor = "multi_thread")]
async fn the_program_puts_its_shell_behind_the_gate_where_there_is_one() {
    let probed = kamchatka::sandbox::available(&common::program());
    if probed.confinement != kamchatka::sandbox::Confinement::Full || !probed.gated {
        eprintln!("skipped: the network gate does not hold here");
        return;
    }
    let base = common::endpoint(vec![
        format!(
            "data: {}",
            json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
                {"index": 0, "id": "c1", "type": "function", "function": {"name": "shell",
                 "arguments": "{\"cmd\": \"python3 -c 'import socket; socket.socket()'\"}"}}
            ]}, "finish_reason": "tool_calls"}]})
        ),
        common::answer("refused, then"),
    ])
    .await;

    let mut child = common::command()
        .args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--allow",
            "exec:run",
            "go",
        ])
        .current_dir(common::workdir("program-gate"))
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    // bounded, because a command left waiting on a question nobody answers is a run that never
    // ends, and a suite that hangs says nothing about which assertion never came true
    let watched = watch(child.stderr.take().expect("stderr is a pipe"));
    let mut stdout = child.stdout.take().expect("stdout is a pipe");
    let records = std::thread::spawn(move || {
        let mut records = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut records);
        records
    });
    waited_out(&mut child, std::time::Duration::from_secs(30), &watched);
    let records = records.join().expect("it was read");
    // what the watcher has read by the time the process has gone may be short of the end, which
    // is what the line this is looking for is near
    let waited = std::time::Instant::now();
    while !watched.lock().contains("reached for the network")
        && waited.elapsed() < std::time::Duration::from_secs(5)
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let said = watched.lock().clone();

    assert!(
        said.contains("reached for the network: deny, because nobody is here to be asked"),
        "{said}"
    );
    // `python3` is no name a policy reading the command would have asked about
    assert!(!said.contains("shell: deny"), "{said}");
    assert!(
        records
            .lines()
            .any(|line| line.contains("policy.ruled") && line.contains("net:reach")),
        "{records}"
    );
}

/// A command may signal what an earlier call left running, and nothing this session did not start.
///
/// note: through the binary, because the scope is on the program's own process, put there before
/// anything is started - see `sandbox::scope_signals` - and nothing in process can stand in for
/// that. A live session's shell had `kill -0` succeed against the program running it, and so
/// against every process the person has.
#[tokio::test(flavor = "multi_thread")]
async fn the_programs_commands_signal_each_other_and_nothing_outside() {
    if !kamchatka::sandbox::confines_signals() {
        eprintln!("skipped: no Landlock scope for a signal here, which is ABI 6");
        return;
    }
    let mut outside = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("a process outside the session");
    let call = |id: &str, cmd: &str| {
        format!(
            "data: {}",
            json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
                {"index": 0, "id": id, "type": "function", "function": {"name": "shell",
                 "arguments": json!({ "cmd": cmd }).to_string()}}
            ]}, "finish_reason": "tool_calls"}]})
        )
    };
    let base = common::endpoint(vec![
        call("c1", "sleep 300 > /dev/null 2>&1 & echo $! > job.pid"),
        call(
            "c2",
            &format!(
                "kill -0 $(cat job.pid) && echo job=reached >> said.txt; \
                 kill -0 {} 2>/dev/null || echo outside=refused >> said.txt",
                outside.id()
            ),
        ),
        common::answer("done"),
    ])
    .await;
    let dir = common::workdir("program-signals");

    let status = common::command()
        .args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--allow",
            "exec:run",
            "--deadline",
            "60",
            "go",
        ])
        .current_dir(&dir)
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("the binary under test is built");
    let _ = outside.kill();
    let _ = outside.wait();

    assert!(status.success(), "{status}");
    let said = std::fs::read_to_string(dir.join("said.txt")).expect("the second call wrote");
    assert!(
        said.contains("job=reached"),
        "an earlier call's job was out of reach: {said}"
    );
    assert!(
        said.contains("outside=refused"),
        "a process outside was signalled: {said}"
    );
}

/// `--no-sandbox` is the one run whose commands are held to nothing, signals included.
///
/// note: the other half of the pair above, and the flag's whole claim about signals: everything
/// this program starts inherits the scope, and `--no-sandbox` is the flag for a run whose commands
/// are meant to reach the whole machine - a set-user-ID program such as `sudo` gains nothing in
/// them, which is what the scope costs and why the flag says not to pay it. So a `--no-sandbox`
/// run's child must find a process of the user's reachable where a confined one cannot.
///
/// note: an `--mcp` server rather than a `shell` command, because the child is started by the
/// program itself before any provider is reached - so this needs no endpoint, and what it observes
/// is exactly what a session's every command observes.
#[cfg(feature = "mcp")]
#[tokio::test(flavor = "multi_thread")]
async fn a_run_with_no_sandbox_signals_whatever_it_can_reach() {
    if !kamchatka::sandbox::confines_signals() {
        eprintln!("skipped: no Landlock scope for a signal here, which is ABI 6");
        return;
    }
    let dir = common::scratch("no-sandbox-signals");
    // the child writes what `kill -0` said, and the run is over once it has
    let probe = dir.join("probe.sh");
    std::fs::write(
        &probe,
        "#!/bin/sh\nif kill -0 \"$1\" 2>/dev/null; then echo reached; else echo refused; fi > \
         said.txt\n",
    )
    .expect("written");
    let mut permissions = std::fs::metadata(&probe).expect("there").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(0o755);
    }
    std::fs::set_permissions(&probe, permissions).expect("made runnable");

    let mut outside = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("a process outside the session");

    let ran = common::command()
        .args(["--headless", "--no-record", "--no-sandbox", "-m", "nothing"])
        .arg(format!("--mcp=probe={} {}", probe.display(), outside.id()))
        .current_dir(&dir)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");
    let _ = outside.kill();
    let _ = outside.wait();

    // the server's handshake fails, which is what ends the run; what the child saw is the point
    let _ = ran;
    let said = std::fs::read_to_string(dir.join("said.txt")).expect("the server was started");
    assert_eq!(
        said.trim(),
        "reached",
        "a `--no-sandbox` run's child could not signal a process of the user's"
    );
}

/// `--sandbox-device` reaches the shell the program wires: a command that reads `/dev/zero`
/// succeeds under the usual list and fails where only `/dev/null` was named.
///
/// note: read out of a `/save`, since a headless run prints no tool's output and a command that
/// fails is not an error result. Through the binary, because what is under test is the flag's way
/// to `Shell`.
#[tokio::test(flavor = "multi_thread")]
async fn the_devices_the_program_is_given_are_the_shells() {
    use std::io::Write as _;

    let probed = kamchatka::sandbox::available(&common::program());
    if probed.confinement != kamchatka::sandbox::Confinement::Full {
        eprintln!("skipped: this machine cannot confine a command");
        return;
    }
    let run = async |name: &str, devices: &[&str]| {
        let base = common::endpoint(vec![
            format!(
                "data: {}",
                json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
                    {"index": 0, "id": "c1", "type": "function", "function": {"name": "shell",
                     "arguments": "{\"cmd\": \"head -c 1 /dev/zero >/dev/null\"}"}}
                ]}, "finish_reason": "tool_calls"}]})
            ),
            common::answer("read"),
        ])
        .await;
        let dir = common::workdir(name);
        let mut child = common::command()
            .args([
                "--headless",
                "--no-record",
                "-m",
                "nothing",
                "--allow",
                "exec:run",
            ])
            .args(devices)
            .current_dir(&dir)
            .env("KAMCHATKA_BASE_URL", &base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary under test is built");
        child
            .stdin
            .take()
            .expect("stdin is a pipe")
            .write_all(b"go\n/save saved.json\n")
            .expect("the lines were sent");
        let out = child.wait_with_output().expect("the program never ended");

        std::fs::read_to_string(dir.join("saved.json"))
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)))
    };

    let usual = run("program-devices-usual", &[]).await;
    assert!(usual.contains("exit: 0"), "{usual}");
    let narrowed = run(
        "program-devices-narrowed",
        &["--sandbox-device", "/dev/null"],
    )
    .await;
    assert!(narrowed.contains("exit: 1"), "{narrowed}");
}

/// `ctrl+c` stops a command that is running, and what arrived is kept.
///
/// note: the case this was written to test was a *second* press leaving a turn the first could not
/// stop - and it turns out there is no such turn to be had out of this program. Measured: a model
/// that has merely gone quiet ends the run in about 200ms on one press, because the provider
/// watches the interrupt while it waits; and a `shell` command halfway through `sleep 30` is
/// *killed* by one press, which is the re-exec being load-bearing rather than tidy. So what is
/// asserted here is what actually happens - and since an MCP call now watches the interrupt too,
/// no tool this program ships holds a turn past one press.
///
/// note: the endpoint is a socket rather than a scripted provider because the program builds its
/// own provider in a process of its own, and a listener is the only seam a child process has.
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_stops_a_command_that_is_running_and_keeps_what_arrived() {
    let base = common::endpoint(vec![format!(
        "data: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
            {"index": 0, "id": "c1", "type": "function",
             "function": {"name": "shell", "arguments": "{\"cmd\": \"sleep 30\"}"}}
        ]}, "finish_reason": "tool_calls"}]})
    )])
    .await;

    let mut child = common::command()
        .args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--allow",
            "exec:run",
            "go",
        ])
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    let said = watch(child.stderr.take().expect("stderr is a pipe"));
    until_said(&said, "⟩ shell(", "the tool").await;

    let pressed = std::time::Instant::now();
    interrupt(child.id());
    let status = waited_out(&mut child, std::time::Duration::from_secs(20), &said);

    assert_eq!(status.code(), Some(130), "{}", said.lock());
    assert!(
        pressed.elapsed() < std::time::Duration::from_secs(20),
        "`sleep 30` outlived the interrupt, so the command was waited for rather than stopped"
    );
    let said = said.lock().clone();
    // and it was running: a call refused for want of anybody to ask leaves nothing to stop, and
    // passes everything above. `--allow shell` did exactly that, being a domain no tool declares
    assert!(
        !said.contains("because nobody is here to be asked"),
        "the command was refused rather than run: {said}"
    );
    assert!(said.contains("what has arrived is kept"), "{said}");
    // the result of the call it was in the middle of is on the record, which is the whole of what
    // "what has arrived is kept" means
    assert!(said.contains("· shell:"), "{said}");

    let mut records = String::new();
    std::io::Read::read_to_string(
        &mut child.stdout.take().expect("stdout is a pipe"),
        &mut records,
    )
    .expect("the records are text");
    let last = records
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .next_back()
        .expect("a record");
    assert_eq!(last.event.name(), "session.finished");
}

/// A command the model runs is not handed the keys the program reads, confined or not - and is
/// handed the rest of the environment as before.
///
/// note: what it saw is written to a file rather than read off the prose, because a headless run
/// reports a tool result by its size and the record names results rather than copying them.
#[tokio::test(flavor = "multi_thread")]
async fn a_command_the_model_runs_is_not_handed_the_program_s_keys() {
    let cmd = "echo \"${KAMCHATKA_API_KEY:-none} ${OPENROUTER_API_KEY:-none} \
               ${OPENAI_API_KEY:-none} ${KAMCHATKA_SYSTEM1_API_KEY:-none} \
               ${HOME:+home}\" > seen.txt";
    for confined in [true, false] {
        let dir = common::scratch(&format!("keys-{confined}"));
        let base = common::endpoint(vec![
            format!(
                "data: {}",
                json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
                    "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                    "function": {"name": "shell", "arguments": json!({"cmd": cmd}).to_string()}}
                ]}, "finish_reason": "tool_calls"}]})
            ),
            common::answer("done"),
        ])
        .await;

        let mut command = common::command();
        // `fs:write` too, because a confined shell is read-only where writing is refused - and
        // headless, a question nobody can be asked is refused
        command.args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--allow",
            "exec:run,fs:write",
        ]);
        if !confined {
            command.arg("--no-sandbox");
        }
        let ran = command
            .arg("go")
            .current_dir(&dir)
            .env("KAMCHATKA_BASE_URL", &base)
            .env("KAMCHATKA_API_KEY", "sk-the-session-key")
            .env("OPENROUTER_API_KEY", "sk-the-router-key")
            .env("OPENAI_API_KEY", "sk-another-key")
            .env("KAMCHATKA_SYSTEM1_API_KEY", "sk-the-system1-key")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the binary under test is built");
        assert!(
            ran.status.success(),
            "confined: {confined}: {}",
            String::from_utf8_lossy(&ran.stderr)
        );

        let seen = std::fs::read_to_string(dir.join("seen.txt")).unwrap_or_else(|e| {
            panic!(
                "confined: {confined}: the command never ran ({e}): {}",
                String::from_utf8_lossy(&ran.stderr)
            )
        });
        assert_eq!(
            seen.trim(),
            "none none none none home",
            "confined: {confined}"
        );
    }
}

/// A session run inside another one's command keeps that command's mark and adds its own, and a
/// mark set to nothing is not an entry.
///
/// note: what the end of the outer session looks for. Its stragglers are found by their mark, so
/// a mark the inner session wrote over would leave everything it started outside the reach of the
/// session that started it.
#[tokio::test(flavor = "multi_thread")]
async fn a_command_carries_the_mark_of_the_command_it_runs_inside() {
    let cmd = "echo \"$KAMCHATKA_CALL\" > seen.txt";
    for (outer, starts) in [("elsewhere-0-0.4", "elsewhere-0-0.4;"), ("", "")] {
        let dir = common::scratch(&format!("mark-{}", outer.is_empty()));
        let base = common::endpoint(vec![
            format!(
                "data: {}",
                json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
                    "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                    "function": {"name": "shell", "arguments": json!({"cmd": cmd}).to_string()}}
                ]}, "finish_reason": "tool_calls"}]})
            ),
            common::answer("done"),
        ])
        .await;

        let ran = common::command()
            .args(["--headless", "--no-record", "--no-sandbox", "-m", "nothing"])
            .args(["--allow", "exec:run,fs:write", "go"])
            .current_dir(&dir)
            .env("KAMCHATKA_BASE_URL", &base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .env("KAMCHATKA_CALL", outer)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the binary under test is built");
        assert!(
            ran.status.success(),
            "{}",
            String::from_utf8_lossy(&ran.stderr)
        );

        let seen = std::fs::read_to_string(dir.join("seen.txt")).expect("the command ran");
        let ours = seen
            .trim()
            .strip_prefix(starts)
            .unwrap_or_else(|| panic!("outside {outer:?}, the command carried {seen:?}"));
        assert!(
            !ours.is_empty() && !ours.contains(';'),
            "outside {outer:?}, the command carried {seen:?}"
        );
    }
}

/// A confined command can move a file from one directory of the working directory to another.
///
/// note: through the program, because what refused it was not the command's ruleset but the
/// layer the program puts itself in to scope signals, which every command inherits: a layer that
/// does not grant `Refer` refuses every rename and link between directories, and a compiler
/// renames its output into place. `ln` rather than `mv`, which copies where the kernel refuses a
/// rename and so hides it.
#[tokio::test(flavor = "multi_thread")]
async fn a_confined_command_can_move_a_file_between_directories() {
    let probed = kamchatka::sandbox::available(&common::program());
    if probed.confinement != kamchatka::sandbox::Confinement::Full {
        eprintln!("skipped: this machine cannot confine a command");
        return;
    }
    let cmd =
        "mkdir -p from to && echo moved > from/it && ln from/it to/it && cat to/it > seen.txt";
    let dir = common::scratch("program-move");
    let base = common::endpoint(vec![
        format!(
            "data: {}",
            json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
                "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                "function": {"name": "shell", "arguments": json!({"cmd": cmd}).to_string()}}
            ]}, "finish_reason": "tool_calls"}]})
        ),
        common::answer("done"),
    ])
    .await;

    let ran = common::command()
        .args(["--headless", "--no-record", "-m", "nothing"])
        .args(["--allow", "exec:run,fs:write", "go"])
        .current_dir(&dir)
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );

    let seen = std::fs::read_to_string(dir.join("seen.txt")).unwrap_or_else(|e| {
        panic!(
            "the file never reached the other directory ({e}): {}",
            String::from_utf8_lossy(&ran.stderr)
        )
    });
    assert_eq!(seen.trim(), "moved");
}

/// Reads a child's output into a string as it arrives, so that a test can look at it without
/// blocking on a pipe that may never say another word.
fn watch(mut stream: std::process::ChildStderr) -> Arc<parking_lot::Mutex<String>> {
    let said = Arc::new(parking_lot::Mutex::new(String::new()));
    let writing = said.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(read) = std::io::Read::read(&mut stream, &mut buf) {
            if read == 0 {
                break;
            }
            writing
                .lock()
                .push_str(&String::from_utf8_lossy(&buf[..read]));
        }
    });

    said
}

/// Waits for a child to have said `phrase`, or says what it had said when it did not.
async fn until_said(said: &Arc<parking_lot::Mutex<String>>, phrase: &str, what: &str) {
    let waited = std::time::Instant::now();
    while !said.lock().contains(phrase) {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(20),
            "it never reached {what}: {}",
            said.lock()
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Presses `ctrl+c` at a child process.
fn interrupt(pid: u32) {
    let sent = std::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .expect("`kill` is on the path");
    assert!(sent.success());
}

/// Waits for a child to leave, or says what it had said when it did not.
fn waited_out(
    child: &mut std::process::Child,
    within: std::time::Duration,
    said: &Arc<parking_lot::Mutex<String>>,
) -> std::process::ExitStatus {
    let waited = std::time::Instant::now();
    loop {
        match child.try_wait().expect("it was spawned") {
            Some(status) => break status,
            None if waited.elapsed() > within => {
                let _ = child.kill();
                panic!("it did not leave: {}", said.lock());
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

/// A first `ctrl+c` stops an MCP call the server never answers.
///
/// note: a kernel interrupt lands between steps, and an MCP call in flight is not between steps -
/// so a server that never answered held the turn open for as long as it liked, and only a second
/// press, leaving at once, got out. The call watches the interrupt now and tells the server to
/// stop, so the first press ends the turn like any other, and `sleep 600` in forty lines of Python
/// is what shows it. No tool this program ships ignores the interrupt any more, so the second
/// press - for a tool of an embedder's that does - has nothing here to be shown on.
#[cfg(feature = "mcp")]
#[tokio::test(flavor = "multi_thread")]
async fn a_first_press_stops_a_call_the_server_never_answers() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: python3 is not on the path");
        return;
    }
    let base = common::endpoint(vec![format!(
        "data: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [
            {"index": 0, "id": "c1", "type": "function",
             "function": {"name": "py__hang", "arguments": "{}"}}
        ]}, "finish_reason": "tool_calls"}]})
    )])
    .await;

    // an argument the server ignores, so that this run's server is the one looked for below
    let marker = format!("first-press-{}", std::process::id());
    let server = format!(
        "py=python3 {} {marker}",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mcp_server.py")
    );
    let mut child = common::command()
        .args(["--headless", "--no-record", "-m", "nothing"])
        .args(["--mcp", &server, "--allow-server", "py", "go"])
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    let said = watch(child.stderr.take().expect("stderr is a pipe"));
    until_said(&said, "⟩ py__hang(", "the tool").await;

    interrupt(child.id());
    let pressed = std::time::Instant::now();
    let status = waited_out(&mut child, std::time::Duration::from_secs(10), &said);

    assert_eq!(status.code(), Some(130), "{}", said.lock());
    assert!(
        pressed.elapsed() < std::time::Duration::from_secs(5),
        "the first press waited for the server anyway: {:?}",
        pressed.elapsed()
    );

    // note: and the server went with it. It is still asleep in the call, so it never reads the
    // end of its input, and the kill `rmcp` sends on drop is a task the runtime was let go of
    // before it ran - so every run of this left a Python process behind for ten minutes
    let running = || {
        std::fs::read_dir("/proc")
            .expect("a /proc")
            .filter_map(|entry| std::fs::read(entry.ok()?.path().join("cmdline")).ok())
            .any(|cmdline| String::from_utf8_lossy(&cmdline).contains(&marker))
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while running() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        !running(),
        "the MCP server outlived the session that started it"
    );
}

/// `SIGTERM` and `SIGHUP` end the session rather than the process: the running command is
/// stopped, and the session is recorded as having ended.
///
/// note: both were left to their default, which ends the process where it stands - so closing the
/// terminal, an ssh drop, `timeout` or `docker stop` wrote no record at all, and a `shell` command,
/// in a process group of its own and never sent the terminal's hangup, went on running after the
/// agent. `late.txt` is what the command would have done had it been left to finish.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_to_end_is_a_quit_and_leaves_a_record() {
    for signal in ["TERM", "HUP"] {
        let dir = common::scratch(&format!("terminated-{signal}"))
            .canonicalize()
            .expect("it exists");
        let cmd = "sleep 3; touch late.txt";
        let base = common::endpoint(vec![format!(
            "data: {}",
            json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
                "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                "function": {"name": "shell", "arguments": json!({"cmd": cmd}).to_string()}}
            ]}, "finish_reason": "tool_calls"}]})
        )])
        .await;

        let mut child = common::command()
            .args([
                "--headless",
                "-m",
                "nothing",
                "--allow",
                "exec:run,fs:write",
            ])
            .arg("go")
            .current_dir(&dir)
            .env("KAMCHATKA_BASE_URL", &base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .env("TMPDIR", &dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary under test is built");
        let said = watch(child.stderr.take().expect("stderr is a pipe"));
        until_said(&said, "⟩ shell(", "the command").await;
        // a moment for the command to have started, rather than only been asked for
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let sent = std::process::Command::new("kill")
            .args([&format!("-{signal}"), &child.id().to_string()])
            .status()
            .expect("`kill` is on the path");
        assert!(sent.success());
        let status = waited_out(&mut child, std::time::Duration::from_secs(10), &said);
        // `128` and the signal, as a shell reports a program one ended
        let expected = match signal {
            "TERM" => 143,
            _ => 129,
        };
        assert_eq!(
            status.code(),
            Some(expected),
            "SIG{signal}: {}",
            said.lock()
        );

        let said = said.lock().clone();
        assert!(
            said.contains("ended by a termination signal"),
            "SIG{signal}: a run stopped mid-turn said nothing about what stopped it: {said}"
        );
        let path = said
            .split_whitespace()
            .find_map(|word| word.strip_suffix(',').filter(|it| it.ends_with(".jsonl")))
            .unwrap_or_else(|| panic!("SIG{signal}: it named no log: {said}"));
        let names: Vec<String> = std::fs::read_to_string(path)
            .expect("the file it named is there")
            .lines()
            .map(|line| {
                serde_json::from_str::<Record>(line)
                    .expect("every line is a record")
                    .event
                    .name()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            names.last().map(String::as_str),
            Some("session.finished"),
            "SIG{signal}: {names:?}"
        );
        assert!(
            names.contains(&"tool.finished".to_owned()),
            "SIG{signal}: the command's end is in the record: {names:?}"
        );

        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        assert!(
            !dir.join("late.txt").exists(),
            "SIG{signal}: the command went on running after the session ended"
        );
    }
}

/// A run killed outright leaves the record it had written up to that moment, and a snapshot to
/// carry on from.
///
/// note: `SIGKILL` cannot be caught, so nothing here runs on the way out - which is the case the
/// record is written as it goes for. The two above leave by the ordinary door and write the
/// session then; this one, and an out-of-memory kill or a pulled plug, leave nothing but what
/// was already on disk. The log has to hold the command being started, since that is what a
/// person reading the record afterwards needs to know happened, and it cannot hold the session's
/// end, since there was none.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_killed_outright_leaves_what_it_had_written() {
    let dir = common::scratch("killed").canonicalize().expect("it exists");
    let cmd = "sleep 3";
    let base = common::endpoint(vec![format!(
        "data: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
            "tool_calls": [{"index": 0, "id": "c1", "type": "function",
            "function": {"name": "shell", "arguments": json!({"cmd": cmd}).to_string()}}
        ]}, "finish_reason": "tool_calls"}]})
    )])
    .await;

    let mut child = common::command()
        .args(["--headless", "-m", "nothing", "--allow", "exec:run"])
        .arg("go")
        .current_dir(&dir)
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .env("TMPDIR", &dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    let said = watch(child.stderr.take().expect("stderr is a pipe"));
    until_said(&said, "⟩ shell(", "the command").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let sent = std::process::Command::new("kill")
        .args(["-KILL", &child.id().to_string()])
        .status()
        .expect("`kill` is on the path");
    assert!(sent.success());
    let status = waited_out(&mut child, std::time::Duration::from_secs(10), &said);
    assert!(!status.success(), "a killed run does not succeed");

    let mut files = std::fs::read_dir(dir.join("kamchatka"))
        .expect("the record directory was made at the start")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    files.sort();
    let log = files
        .iter()
        .find(|path| path.extension().is_some_and(|it| it == "jsonl"))
        .expect("the log was claimed at the start");
    let names: Vec<String> = std::fs::read_to_string(log)
        .expect("readable")
        .lines()
        .map(|line| {
            serde_json::from_str::<Record>(line)
                .expect("every line is a whole record")
                .event
                .name()
                .to_owned()
        })
        .collect();
    assert!(
        names.contains(&"tool.started".to_owned()),
        "the record holds the command being started: {names:?}"
    );
    assert!(
        !names.contains(&"session.finished".to_owned()),
        "a killed session did not end: {names:?}"
    );

    // the snapshot is where things stood when the session last came to rest, which is before
    // the turn began: the message is in it, the model's call is not
    let state = files
        .iter()
        .find(|path| path.extension().is_some_and(|it| it == "json"))
        .expect("a snapshot was written before the turn began");
    let snapshot: nachalnik::Snapshot =
        serde_json::from_slice(&std::fs::read(state).expect("readable")).expect("a session");
    assert!(
        snapshot
            .items
            .iter()
            .any(|item| item.kind == ContextKind::UserMessage && item.content.to_string() == "go"),
        "the message is in the snapshot"
    );
    assert!(
        !snapshot
            .items
            .iter()
            .any(|item| matches!(item.kind, ContextKind::AssistantMessage { .. })),
        "the snapshot is from before the turn, and says so by holding nothing of it"
    );
    assert!(
        !dir.join("kamchatka")
            .read_dir()
            .expect("listable")
            .any(|entry| {
                entry.is_ok_and(|entry| entry.path().to_string_lossy().ends_with(".writing"))
            }),
        "no half-written snapshot was left beside the record"
    );
}

/// A run with nobody left reading either stream still writes its record.
///
/// note: `kamchatka --headless … 2>&1 | head` gets here once `head` has gone. The closing lines
/// were printed with the macros that panic on a failed write, and before the record was written,
/// so the panic took the record with it.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_nobody_is_reading_still_writes_its_record() {
    let base = common::endpoint(vec![common::answer("said to nobody")]).await;
    let dir = common::scratch("unread");

    let mut child = common::command()
        .args(["--headless", "-m", "nothing", "go"])
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .env("TMPDIR", &dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    drop(child.stdout.take());
    drop(child.stderr.take());
    let nothing = Arc::new(parking_lot::Mutex::new(String::new()));
    let status = waited_out(&mut child, std::time::Duration::from_secs(20), &nothing);
    assert_ne!(status.code(), Some(101), "it panicked");

    let logs: Vec<_> = walkdir(&dir)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|it| it == "jsonl"))
        .collect();
    assert_eq!(logs.len(), 1, "one record: {logs:?}");
    let last = std::fs::read_to_string(&logs[0])
        .expect("the record is readable")
        .lines()
        .last()
        .map(|line| {
            serde_json::from_str::<Record>(line)
                .expect("a record")
                .event
                .name()
                .to_owned()
        });
    assert_eq!(last.as_deref(), Some("session.finished"));
}

/// Every file under a directory.
fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        match path.is_dir() {
            true => found.extend(walkdir(&path)),
            false => found.push(path),
        }
    }
    found
}

/// A `/restart` whose fresh session cannot be wired still says where the old one was written.
///
/// note: the old session is recorded before the new one is wired, and the line naming the file was
/// dropped when the wiring failed - so the error came out alone, over a record nobody was told
/// about. A `--file` that has gone since the run started is one way to make the wiring fail.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_that_cannot_start_again_still_says_where_the_session_went() {
    use std::io::Write as _;

    let base = common::endpoint(vec![]).await;
    let dir = common::scratch("restart-failed");
    std::fs::write(dir.join("notes.md"), "notes").expect("a file to attach");

    let mut child = common::command()
        .args(["--headless", "-m", "nothing", "--file", "notes.md"])
        .current_dir(&dir)
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .env("TMPDIR", &dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    let said = watch(child.stderr.take().expect("stderr is a pipe"));
    // once the first session has read it, which the opening lines come after
    let waited = std::time::Instant::now();
    while said.lock().is_empty() {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(20),
            "it never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    std::fs::remove_file(dir.join("notes.md")).expect("the file goes");
    child
        .stdin
        .as_mut()
        .expect("stdin is a pipe")
        .write_all(b"/restart\n")
        .expect("could not type");
    let status = waited_out(&mut child, std::time::Duration::from_secs(20), &said);

    let said = said.lock().clone();
    assert!(!status.success(), "{said}");
    assert!(said.contains("no fresh session could be started"), "{said}");
    assert!(
        said.contains("kamchatka -r "),
        "it names the record: {said}"
    );
}

/// What one answer costs, through the flag, against something that reports a cost.
///
/// note: the ceiling has tests through the library and a live run behind it; what it had not had
/// is the path a person takes - `--spend` on a command line, into `Setup`, into the `App` that
/// enforces it. The stub reports 1,200 tokens for one answer, which is over any ceiling worth
/// typing here.
#[tokio::test(flavor = "multi_thread")]
async fn the_spend_ceiling_stops_the_program_itself() {
    let base = common::endpoint(vec![common::answer("as much as it likes")]).await;

    let out = common::command()
        .args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--spend",
            "100",
        ])
        .arg("go")
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a run the ceiling stopped says so: {said}"
    );
    assert!(
        said.contains("spent 1,200 tokens of 100; stopping"),
        "the ceiling did not stop it: {said}"
    );
}

/// `--deadline 0` is no deadline, as `0` is no ceiling to `--spend` and `--requests`.
///
/// note: it was a deadline that had already passed, which raced the first line: a request sent
/// and interrupted at once, or the line never read.
#[tokio::test(flavor = "multi_thread")]
async fn a_deadline_of_nothing_is_none() {
    let base = common::endpoint(vec![common::answer("with time to spare")]).await;

    let out = common::command()
        .args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--deadline",
            "0",
        ])
        .arg("go")
        .current_dir(common::scratch("deadline-0"))
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("with time to spare"), "{said}");
    assert!(!said.contains("out of time"), "{said}");
    assert_eq!(out.status.code(), Some(0), "a run that finished: {said}");
}

/// `--requests` is how many requests one turn makes before it pauses and says so, and `0` is no
/// ceiling at all.
///
/// note: through the flag, as the spend ceiling above is: the runtime tests its own limit, and
/// what is under test here is the number getting from a command line to it. `0` read as a
/// ceiling of nothing is a turn refused before its first request.
#[tokio::test(flavor = "multi_thread")]
async fn the_request_ceiling_stops_the_program_itself() {
    let run = async |requests: &str, answers: Vec<String>, lines: &str| {
        use std::io::Write as _;

        let base = common::endpoint(answers).await;
        let mut child = common::command()
            .args(["--headless", "--no-record", "-m", "nothing", "--requests"])
            .args([requests, "go"])
            .current_dir(common::scratch(&format!("requests-{requests}")))
            .env("KAMCHATKA_BASE_URL", &base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary under test is built");
        let mut input = child.stdin.take().expect("stdin is a pipe");
        input.write_all(lines.as_bytes()).expect("typed");
        drop(input);
        let out = child.wait_with_output().expect("it ended");

        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // a call to a tool nobody has is an error result, and a turn with a request left asks again
    let calling = format!(
        "data: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
            "tool_calls": [{"index": 0, "id": "c1", "type": "function",
            "function": {"name": "nothing", "arguments": "{}"}}]},
            "finish_reason": "tool_calls"}]})
    );
    let (code, said) = run(
        "1",
        vec![calling.clone(), common::answer("asked again")],
        "",
    )
    .await;
    assert!(said.contains("paused after 1 request;"), "{said}");
    assert!(!said.contains("asked again"), "{said}");
    // and `4`, because a paused turn's records read as those of one the model ended - the status
    // is the one place a script can tell the two apart
    assert_eq!(code, Some(4), "{said}");

    // a call somebody is asked about is no way round it: answered by `--on-ask`, the turn carried
    // on with a budget of its own, made its second request and exited `0` - which is what a live
    // model reading two files one call at a time did under `--requests 1`
    let asking = format!(
        "data: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
            "tool_calls": [{"index": 0, "id": "c1", "type": "function",
            "function": {"name": "fs", "arguments": "{\"call\": {\"action\": \"read\", \"path\": \"a.txt\"}}"}}]},
            "finish_reason": "tool_calls"}]})
    );
    let (code, said) = run("1", vec![asking, common::answer("asked again")], "").await;
    assert!(
        said.contains("fs: deny"),
        "the call was asked about: {said}"
    );
    assert!(said.contains("paused after 1 request;"), "{said}");
    assert!(!said.contains("asked again"), "{said}");
    assert_eq!(code, Some(4), "{said}");

    // a pause that is carried on from is not where the run stopped: the status is the last turn's
    let (code, said) = run(
        "1",
        vec![calling, common::answer("asked again")],
        "/continue\n",
    )
    .await;
    assert!(said.contains("asked again"), "{said}");
    assert_eq!(code, Some(0), "{said}");

    let (code, said) = run("0", vec![common::answer("an answer")], "").await;
    assert!(said.contains("an answer"), "{said}");
    assert!(!said.contains("paused after"), "{said}");
    assert_eq!(code, Some(0), "{said}");
}

/// `--forget-truncated` reaches the session: what `setup` tells the model about the rest of a cut
/// result follows the flag.
///
/// note: `setup` reads it off the kernel's own configuration, and nothing else shows it before
/// something is cut. A model told the whole is excluded, in a session that forgets it, goes
/// looking for content that is not there.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_told_to_forget_what_was_cut_tells_the_model_so() {
    use std::io::Write as _;

    let run = async |name: &str, flags: &[&str]| {
        let base = common::endpoint(vec![
            format!(
                "data: {}",
                json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
                    "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                    "function": {"name": "setup", "arguments": "{\"action\": \"policy\"}"}}]},
                    "finish_reason": "tool_calls"}]})
            ),
            common::answer("read"),
        ])
        .await;
        let dir = common::scratch(name);
        let mut child = common::command()
            .args([
                "--headless",
                "--no-record",
                "-m",
                "nothing",
                "--allow",
                "setup",
            ])
            .args(flags)
            .current_dir(&dir)
            .env("KAMCHATKA_BASE_URL", &base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary under test is built");
        // the answer is shown to the model and not printed, so it is read back out of a save
        child
            .stdin
            .take()
            .expect("stdin is a pipe")
            .write_all(b"go\n/save saved.json\n")
            .expect("the lines were sent");
        let out = child.wait_with_output().expect("the program never ended");
        let said = String::from_utf8_lossy(&out.stderr);

        std::fs::read_to_string(dir.join("saved.json")).unwrap_or_else(|_| panic!("{said}"))
    };

    let saved = run("kept-what-was-cut", &[]).await;
    assert!(saved.contains("excluded beside"), "{saved}");

    let saved = run("forgot-what-was-cut", &["--forget-truncated"]).await;
    assert!(saved.contains("not kept"), "{saved}");
    assert!(!saved.contains("excluded beside"), "{saved}");
}

/// A run that was not told to keep quiet writes the session out, and says where.
///
/// note: what every real run does at the end, and the one thing about a headless run that nothing
/// checked - the suites all pass `--no-record`, because a test that wrote a file somewhere would
/// be a test that left one. This reads the path out of the line the program prints, which is also
/// the only promise made about it: that the line names a file somebody can open.
#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_run_writes_the_session_where_it_says_it_did() {
    let base = common::endpoint(vec![common::answer("something to keep")]).await;
    let dir = common::scratch("recorded");

    let out = common::command()
        .args(["--headless", "-m", "nothing", "go"])
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        // the program writes into a temporary directory of the system's choosing, and a test that
        // let it use the real one would leave a session behind on every run
        .env("TMPDIR", &dir)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    // the line reads `N records in <log>, and a session in <state>`, so the comma comes off
    let path = said
        .split_whitespace()
        .find_map(|word| word.strip_suffix(',').filter(|it| it.ends_with(".jsonl")))
        .unwrap_or_else(|| panic!("it named no log: {said}"));
    let written = std::fs::read_to_string(path).expect("the file it named is there");
    let names: Vec<String> = written
        .lines()
        .map(|line| {
            serde_json::from_str::<Record>(line)
                .expect("every line is a record")
                .event
                .name()
                .to_owned()
        })
        .collect();
    assert_eq!(names.first().map(String::as_str), Some("session.started"));
    assert_eq!(names.last().map(String::as_str), Some("session.finished"));
    assert!(names.contains(&"model.finished".to_owned()), "{names:?}");

    // and the snapshot beside it, which is what `-r` reads
    let beside = path.replace(".jsonl", ".json");
    let snapshot: nachalnik::Snapshot =
        serde_json::from_str(&std::fs::read_to_string(&beside).expect("a session beside the log"))
            .expect("it is a snapshot");
    assert!(
        !snapshot.items.is_empty(),
        "the snapshot carries the context"
    );
}

/// A run told not to keep a record says so at the end of it, as `/restart` does.
///
/// note: the count of events a run ends on reads as a pointer to a record, and under `--no-record`
/// there is no file to point at. RUNNING.md says the run says so, and said so of `/restart` only;
/// the flag's own help says the path is the last thing printed, which under this flag is nowhere.
///
/// note: a run that sent nothing, because what is under test is the parting lines and not a turn.
#[test]
fn a_run_that_keeps_no_record_says_so_when_it_ends() {
    let dir = common::scratch("unrecorded");

    let out = common::command()
        .args(["--headless", "--no-record"])
        .env("TMPDIR", &dir)
        // no model and nothing typed, so nothing is asked of anybody - what is under test is the
        // parting lines. A key is still wanted: the provider is built before the run
        .env("KAMCHATKA_MODEL", "")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("`--no-record`, so nothing was written"),
        "the parting lines do not say the flag was given: {said}"
    );
    // the count is still there, and the records went to the stream a piped run writes them to, so
    // the line is about the file and not about the session having gone nowhere
    assert!(said.contains("events recorded"), "{said}");
    assert!(
        !dir.join("kamchatka").exists(),
        "a record was written under `--no-record`"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("session.finished"),
        "the record stream is the whole of what was kept"
    );
}

/// A run that leaves on an error still records a session that ended.
///
/// note: the headless driver ends the session itself, and returned before it got there on a line
/// it could not read - so the record was written with no `session.finished` in it, which reads as
/// a process that was killed rather than one that stopped.
#[test]
fn a_run_that_fails_still_records_an_ending() {
    use std::io::Write as _;

    let dir = common::scratch("failed-record");
    let mut child = common::command()
        .args(["--headless", "-m", "nothing"])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .env("TMPDIR", &dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    child
        .stdin
        .take()
        .expect("a pipe")
        .write_all(b"\xff\n")
        .expect("typed");
    let out = child.wait_with_output().expect("it ends");
    assert!(
        !out.status.success(),
        "a line it could not read is a failure"
    );

    let log = std::fs::read_dir(dir.join("kamchatka"))
        .expect("the record directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|it| it == "jsonl"))
        .expect("a record was written");
    let last = std::fs::read_to_string(&log)
        .expect("readable")
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .next_back()
        .expect("a record");
    assert_eq!(last.event.name(), "session.finished");
}

/// A build with no screen runs headless at a terminal, rather than panicking at one.
///
/// note: the bug this is about shipped, and could not have been caught by anything else here:
/// every other test of this binary pipes its stdout, and that is the one condition in which the
/// missing case cannot arise. What it takes is a terminal, which `script` will allocate - so this
/// is the one test in the crate that runs the program under a pty.
///
/// note: compiled only where it is true. With `tui` on, this same command would draw a screen and
/// wait for a key, which is a test that hangs rather than one that passes; without it, there is
/// nothing to draw and the run is headless whatever stdout is.
#[cfg(not(feature = "tui"))]
#[test]
fn a_screenless_build_at_a_terminal_is_a_headless_run() {
    if std::process::Command::new("script")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: `script` is not on the path, so there is no pty to be had");
        return;
    }

    let out = std::process::Command::new("script")
        .current_dir(common::nowhere())
        .args([
            "-q",
            "-c",
            &format!(
                "{} --no-record -m nothing-serves-this hello",
                common::program().display()
            ),
            "/dev/null",
        ])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("`script` ran");

    // a pty merges the two streams, which is what a person at one sees anyway
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("built without the `tui` feature"),
        "it did not say why it was headless: {said}"
    );
    assert!(
        !said.contains("there is no screen in this build"),
        "it panicked on the `unreachable!`: {said}"
    );
    // and it got as far as trying: the model is the thing that fails here, not the program
    assert!(said.contains("error sending request"), "{said}");
}

/// `--deadline` ends a run that is waiting for somebody, through the flag.
///
/// note: the third of the guards, and the last to get a test at this level - the ceiling and
/// `ctrl+c` have theirs above. The mechanism is tested through the library, with a pipe nobody
/// writes to; what this adds is the flag, and that the program leaves rather than waiting on the
/// blocking read that made every early stop hang until yesterday.
#[tokio::test(flavor = "multi_thread")]
async fn the_deadline_ends_the_program_itself() {
    // stdin is a pipe this test holds and never writes to, so nothing but the deadline can end it
    let mut child = common::command()
        .args([
            "--headless",
            "--no-record",
            "-m",
            "nothing",
            "--deadline",
            "1",
        ])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    let said = watch(child.stderr.take().expect("stderr is a pipe"));
    let started = std::time::Instant::now();
    let status = waited_out(&mut child, std::time::Duration::from_secs(15), &said);

    // `124`, as `timeout(1)` leaves with
    assert_eq!(status.code(), Some(124), "{}", said.lock());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "it waited far longer than it was given: {:?}",
        started.elapsed()
    );
    assert!(said.lock().contains("out of time"), "{}", said.lock());
}

/// `--deadline` ends a run still starting: a server that never answers its handshake, and an
/// endpoint that never answers at all.
///
/// note: both are waits on somebody else's program with no bound of their own, before the driver
/// that keeps the deadline exists. The listener is bound and never accepted from, so a connection
/// is made and nothing ever comes back on it.
#[cfg(feature = "mcp")]
#[tokio::test(flavor = "multi_thread")]
async fn the_deadline_ends_a_run_that_is_still_starting() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("a port to never answer on");
    let silent = format!("http://{}/v1", silent.local_addr().expect("an address"));
    for (args, base, step) in [
        (
            vec!["--mcp", "hung=sleep 60"],
            "http://127.0.0.1:1/v1",
            "starting the MCP servers",
        ),
        (vec!["-m", "nothing"], silent.as_str(), "reaching the model"),
    ] {
        let mut child = common::command()
            .args(["--headless", "--no-record", "--deadline", "1"])
            .args(&args)
            .env("KAMCHATKA_BASE_URL", base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary under test is built");

        let said = watch(child.stderr.take().expect("stderr is a pipe"));
        let status = waited_out(&mut child, std::time::Duration::from_secs(15), &said);

        // the deadline's status wherever it falls, so a script need not know how far it got
        assert_eq!(status.code(), Some(124), "{}", said.lock());
        assert!(
            said.lock().contains(&format!("out of time {step}")),
            "{}",
            said.lock()
        );
    }
}

/// An error that is not a deadline's leaves with a failure's status, and says what failed.
///
/// note: the other half of the three statuses above, and the one a script reading them cannot
/// afford to get wrong: `124` says the run was out of time and nothing needs looking at, so an
/// error that was passed off as one leaves whoever reads the status believing a session ended on
/// time. `--check` of a path nothing is there for, because that is an error before any endpoint is
/// reached and so needs no network of its own to be asked for.
#[test]
fn an_error_that_is_not_a_deadlines_leaves_with_a_failure() {
    let out = common::command()
        .args(["--check", "nowhere"])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the binary under test is built");

    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a failure is `anyhow`'s one: {said}"
    );
    assert!(
        said.contains("there is no session at nowhere"),
        "the failure did not say what failed: {said}"
    );
}

/// A restart goes back to the model the flags named, not the one the session had switched to.
///
/// note: `/model` switches the provider in place and the restart carries the provider over, so the
/// new session came up on the old one's switch - where RUNNING.md says a restart goes back to the
/// flags, the model among them.
#[test]
fn a_restart_goes_back_to_the_model_the_flags_named() {
    let dir = common::scratch("reflag");

    let mut child = common::command()
        .args(["--headless", "-m", "flagged-model"])
        .env("TMPDIR", &dir)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    use std::io::Write as _;
    let mut stdin = child.stdin.take().expect("stdin is a pipe");
    stdin
        .write_all(b"/model switched-model\n/restart\n/model\n/quit\n")
        .expect("the lines go in");
    drop(stdin);

    let out = child.wait_with_output().expect("it ran");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(out.status.success(), "{said}");
    let (_, after) = said
        .split_once("ended:")
        .unwrap_or_else(|| panic!("no restart in: {said}"));
    assert!(after.contains("flagged-model"), "{after}");
    assert!(!after.contains("switched-model"), "{after}");
}

/// A restart goes back to the address the flags named as well as to the model, and the two are
/// restored separately.
///
/// note: `/endpoint` moves both, and one of them moved by hand is one of them moved - the flags
/// named `127.0.0.1:1` and the session went to `127.0.0.1:2`, and a restore that watches only the
/// model leaves the fresh session talking to the address the old one switched to, which reads as a
/// restart and is not one. `/endpoint` with nothing after it is the question, so its answer is what
/// is checked, asked of both ends of the restart.
#[test]
fn a_restart_goes_back_to_the_address_the_flags_named() {
    let dir = common::scratch("reflag-address");

    let mut child = common::command()
        .args(["--headless", "-m", "flagged-model"])
        .env("TMPDIR", &dir)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    use std::io::Write as _;
    let mut stdin = child.stdin.take().expect("stdin is a pipe");
    stdin
        .write_all(
            b"/endpoint http://127.0.0.1:2/v1 flagged-model\n/endpoint\n/restart\n/endpoint\n/quit\n",
        )
        .expect("the lines go in");
    drop(stdin);

    let out = child.wait_with_output().expect("it ran");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(out.status.success(), "{said}");
    let asked: Vec<&str> = said
        .lines()
        .filter(|line| line.contains("requests go to"))
        .collect();
    assert_eq!(
        asked.len(),
        2,
        "the question is asked of both ends of the restart: {said}"
    );
    assert!(asked[0].contains("127.0.0.1:2"), "{asked:?}");
    assert!(
        asked[1].contains("127.0.0.1:1"),
        "the fresh session is on the address the flags named, not the one the old one switched \
         to: {said}"
    );
    assert!(!asked[1].contains("127.0.0.1:2"), "{said}");
}

/// `/restart` writes the session out and carries on in a new one, in the program proper.
///
/// note: the binary rather than a driver, because the half worth testing is the loop around
/// `Setup::relaunch`: reading the flag, putting the new session where the loop was holding the
/// first, and one reader for the whole run. `App::restart` is a `bool` and the test above is all
/// there is to say about it here.
///
/// note: `TMPDIR` is the whole isolation. `record` writes under the temporary directory, so a run
/// pointed at one of its own leaves exactly the files this counts and nothing else's turn up in it.
#[test]
fn restart_writes_the_session_out_and_starts_another() {
    let dir = common::scratch("restart");

    let mut child = common::command()
        // note: no `-m`, so a message is put in the context and nothing is sent. What this is
        // about is which session a line lands in, and a turn against an endpoint that is not there
        // would be the run failing about something else
        .args(["--headless"])
        .env("TMPDIR", &dir)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");

    use std::io::Write as _;
    let mut stdin = child.stdin.take().expect("stdin is a pipe");
    // note: a message either side of the restart, because two of the three things this is about
    // are what happens to them. The first belongs to a session that is over and must not be in the
    // second; the second is a line *after* the restart and must be read at all - a session built
    // around a second `BufReader` drops whatever the first had read ahead into its buffer, which
    // is this command's own shape of the bug and is silent
    stdin
        .write_all(b"before the restart\n/restart\nafter the restart\n/quit\n")
        .expect("the lines go in");
    drop(stdin);

    let out = child.wait_with_output().expect("it ran");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(out.status.success(), "{said}");
    // the old session named itself, said where it went, and said it into the new session rather
    // than onto the terminal on its own
    assert!(
        said.contains("ended:") && said.contains("records in"),
        "the restart did not report the session it wrote out: {said}"
    );

    // two sessions, two records: the one `/restart` wrote and the one `/quit` did
    let logs: Vec<_> = std::fs::read_dir(dir.join("kamchatka"))
        .expect("the record directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".jsonl"))
        .collect();
    assert_eq!(logs.len(), 2, "one record per session: {logs:?}");

    // note: the session that was restarted, because it is the one with two ways to be ended. This
    // loop ends its own session - the last record owes the stream it is writing - and
    // `Setup::relaunch` ends the one it is handed, so the pair of them wrote `session.finished`
    // twice: the log said nothing more would be recorded and then recorded it again
    for log in &logs {
        let lines =
            std::fs::read_to_string(dir.join("kamchatka").join(log)).expect("it is readable");
        let ended = lines
            .lines()
            .filter(|line| line.contains("session.finished"))
            .count();
        assert_eq!(ended, 1, "{log} ended {ended} time(s)");
    }

    // and the second session is a session of its own rather than the first one's log written twice
    let mut names: Vec<String> = logs.iter().map(|it| it.replace(".jsonl", "")).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 2, "both records have the same name: {logs:?}");

    // the line after the restart was read, and it went into the session that came after it
    let mut snapshots: Vec<(std::time::SystemTime, String)> = names
        .iter()
        .map(|name| {
            let at = dir.join("kamchatka").join(format!("{name}.json"));
            let when = std::fs::metadata(&at)
                .and_then(|it| it.modified())
                .expect("a snapshot beside every log");

            (when, std::fs::read_to_string(&at).expect("it is readable"))
        })
        .collect();
    snapshots.sort_by_key(|(when, _)| *when);
    let (first, second) = (&snapshots[0].1, &snapshots[1].1);

    assert!(
        first.contains("before the restart"),
        "the session that was restarted did not keep what was said in it"
    );
    assert!(
        second.contains("after the restart"),
        "the line after /restart was swallowed with the old reader's buffer"
    );
    assert!(
        !second.contains("before the restart"),
        "the fresh session carried the old one's conversation into it"
    );
}

/// A record directory that is not a real one of the owner's is refused, and the refusal reads.
///
/// note: a link where the directory should be is the case one user can arrange; the other - a
/// directory somebody else made first - needs a second user, and it is the same refusal. What it
/// promises is that nothing is written through the link, not even the mode the directory is given,
/// and that the sentence says why and what to do instead, which is the only thing a person running
/// this is told.
#[test]
fn a_record_directory_that_is_a_link_is_refused_in_words() {
    let dir = common::scratch("linked");
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("a directory to point at");
    std::os::unix::fs::symlink(&elsewhere, dir.join("kamchatka")).expect("a link");
    let mode = |path: &std::path::Path| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .expect("the target")
            .permissions()
            .mode()
    };
    let before = mode(&elsewhere);

    let mut child = common::command()
        .args(["--headless"])
        .env("TMPDIR", &dir)
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    use std::io::Write as _;
    let mut stdin = child.stdin.take().expect("stdin is a pipe");
    stdin
        .write_all(b"a line\n/quit\n")
        .expect("the lines go in");
    drop(stdin);
    let out = child.wait_with_output().expect("it ran");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();

    let refusal = said
        .lines()
        .find(|line| line.contains("is not a directory only you can enter"))
        .unwrap_or_else(|| panic!("the record directory was not refused: {said}"));
    assert!(
        refusal.contains("was not recorded there") && refusal.contains("--no-record"),
        "the refusal does not say what happened and what to do: {refusal}"
    );
    assert!(
        !refusal.contains("  "),
        "the refusal has a run of spaces in the middle of it: {refusal}"
    );
    assert_eq!(
        std::fs::read_dir(&elsewhere).expect("the target").count(),
        0,
        "something was written through the link"
    );
    assert_eq!(
        mode(&elsewhere),
        before,
        "the directory behind the link was made private"
    );
}

/// A running command that reaches for the network is answered by `--on-ask`, while the turn it is
/// in is still running, and the answer is in the record and in what the model is handed.
///
/// note: the one question a headless run answers while busy. The kernel's questions wait for the
/// kernel to rest, because answering one mid-turn would decide a question the kernel had not
/// finished asking; this one holds a call that is already running, so waiting for the rest would
/// be waiting for the command that is waiting on the answer.
#[tokio::test]
async fn a_command_reaching_for_the_network_is_answered_by_on_ask_mid_turn() {
    let probed = kamchatka::sandbox::available(&common::program());
    if probed.confinement != kamchatka::sandbox::Confinement::Full || !probed.gated {
        eprintln!("skipped: the network gate does not hold here");
        return;
    }
    let dir = common::workdir("headless-gate");
    let script = vec![
        ModelResponse::tool_calls(vec![call(
            "c1",
            "shell",
            json!({ "cmd": "python3 -c \"import socket; socket.socket()\"" }),
        )]),
        ModelResponse::text("it was refused"),
    ];

    // bounded, because the failure this is about is a command waiting on an answer nobody gives,
    // and that is a suite that hangs rather than one that fails
    let run = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        run("fetch it\n", script, |app| {
            app.policy.gate_the_network();
            app.policy.set(&Subject::parse("exec:run"), Verdict::Allow);
            app.kernel.add_tool(Arc::new(kamchatka::tools::Shell {
                policy: app.policy.clone(),
                workdir: dir.clone(),
                extra: Vec::new(),
                readable: Vec::new(),
                devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
                confiner: Some(common::program()),
                limits: kamchatka::tools::Limits::default(),
                stragglers: Default::default(),
            }));
        }),
    )
    .await
    .expect("the command was left waiting on a question nobody answered");

    assert!(
        run.prose
            .contains("reached for the network: deny, because nobody is here to be asked"),
        "{}",
        run.prose
    );
    // an allowed `exec:run` is not a question, whatever the command is called
    assert!(!run.names().contains(&"permission.requested".to_owned()));
    let ruled = run.log().into_iter().any(|record| {
        matches!(
            record.event,
            nachalnik::Event::PolicyRuled { ref subject, verdict: Verdict::Deny, once: true, .. }
                if subject == "net:reach"
        )
    });
    assert!(ruled, "the answer is in the record: {:?}", run.names());
    let told = run.app.kernel.items().iter().any(|item| {
        item.content
            .to_text()
            .contains("refused when it was asked about")
    });
    assert!(told, "the model was not told it was refused");
}

/// `-r` carries on with the model the session was talking to, which is in its record and not in
/// its snapshot.
///
/// note: a snapshot holds the conversation and not the model, so a resume with no `-m` started a
/// session with none. The run's parting line said `kamchatka -r …` carries on from it, and a
/// headless resume given a message then said nothing is sent until there is a model, sent
/// nothing, and exited `0`. Found against a live model, resuming a saved session.
#[tokio::test(flavor = "multi_thread")]
async fn a_resume_carries_on_with_the_model_its_record_names() {
    let dir = common::scratch("resume-model");
    let base = common::endpoint(vec![common::answer("hello"), common::answer("hello again")]).await;
    let run = |args: &[&str]| {
        common::command()
            .args(["--headless", "--deadline", "20"])
            .args(args)
            .current_dir(&dir)
            .env("TMPDIR", &dir)
            .env("KAMCHATKA_BASE_URL", &base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .env_remove("KAMCHATKA_MODEL")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the binary under test is built")
    };

    let out = run(&["-m", "nothing", "go"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{said}");
    let state = said
        .lines()
        .find_map(|line| {
            line.strip_prefix("`kamchatka -r ")?
                .strip_suffix("` carries on from it")
        })
        .unwrap_or_else(|| panic!("the parting line says how to carry on: {said}"))
        .to_owned();

    let out = run(&["-r", &state, "again"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!said.contains("no model yet"), "{said}");
    assert!(said.contains("hello again"), "the message was sent: {said}");
    assert_eq!(out.status.code(), Some(0), "{said}");
    let records = String::from_utf8_lossy(&out.stdout);
    assert!(
        records.contains(
            r#""event":"model.requested","model":{"provider":"openai-compatible","model":"nothing""#
        ),
        "to the model it was talking to before: {records}"
    );
}

/// A `/endpoint` that keeps the model's name is in the record, with both addresses.
///
/// note: the kernel announces a switch by comparing what the provider says about itself, and that
/// carried no address - so `/endpoint URL` alone left a record saying the session never moved,
/// and a session resumed from it had nothing to say where it had been talking.
#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_switch_that_keeps_the_name_is_in_the_record() {
    let dir = common::scratch("endpoint-recorded");
    let first = common::endpoint(Vec::new()).await;
    let second = common::endpoint(Vec::new()).await;
    let out = common::command()
        .args([
            "--headless",
            "--no-record",
            "--deadline",
            "20",
            "-m",
            "nothing",
        ])
        .current_dir(&dir)
        .env("KAMCHATKA_BASE_URL", &first)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .expect("a pipe")
                .write_all(format!("/endpoint {second}\n").as_bytes())?;
            child.wait_with_output()
        })
        .expect("the binary under test is built");
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{said}");

    let changed: Vec<(Option<String>, Option<String>)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<nachalnik::Record>(line).ok())
        .filter_map(|record| match record.event {
            nachalnik::Event::ModelChanged { from, to } => Some((
                from.and_then(|info| info.endpoint),
                to.and_then(|info| info.endpoint),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        changed.last(),
        Some(&(Some(first.clone()), Some(second.clone()))),
        "{changed:?}"
    );
}

/// An address with a `user:password@` in it is said without one, as the record writes it: by
/// `/model`, `/endpoint` and `/seams`, where every line goes to a pipe somebody keeps.
#[tokio::test(flavor = "multi_thread")]
async fn an_address_is_said_without_the_credential_in_it() {
    let dir = common::scratch("endpoint-credential");
    let with = |url: &str| url.replacen("://", "://someone:hunter2@", 1);
    let first = common::endpoint(Vec::new()).await;
    let second = common::endpoint(Vec::new()).await;
    let out = common::command()
        .args([
            "--headless",
            "--no-record",
            "--deadline",
            "20",
            "-m",
            "nothing",
        ])
        .current_dir(&dir)
        .env("KAMCHATKA_BASE_URL", with(&first))
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write as _;
            child.stdin.take().expect("a pipe").write_all(
                format!("/model\n/endpoint {}\n/endpoint\n/seams\n", with(&second)).as_bytes(),
            )?;
            child.wait_with_output()
        })
        .expect("the binary under test is built");
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{said}");

    assert!(said.contains(&format!("nothing at {first}")), "{said}");
    assert!(said.contains(&format!("requests go to {second}")), "{said}");
    assert!(!said.contains("hunter2"), "{said}");
}

/// `-r` pointed somewhere other than the record says it was talking says so, and goes where it was
/// pointed.
///
/// note: said and not followed. The address is where this run's key goes, and a snapshot is a file
/// anybody can hand somebody: following the record would post the reader's key wherever its
/// sender chose.
#[tokio::test(flavor = "multi_thread")]
async fn a_resume_pointed_elsewhere_says_where_the_record_was_talking() {
    let dir = common::scratch("resume-elsewhere");
    let first = common::endpoint(vec![common::answer("from the first")]).await;
    let second = common::endpoint(vec![common::answer("from the second")]).await;
    let run = |base: &str, args: &[&str]| {
        common::command()
            .args(["--headless", "--deadline", "20"])
            .args(args)
            .current_dir(&dir)
            .env("TMPDIR", &dir)
            .env("KAMCHATKA_BASE_URL", base)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .env_remove("KAMCHATKA_MODEL")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the binary under test is built")
    };

    let out = run(&first, &["-m", "nothing", "go"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{said}");
    let state = said
        .lines()
        .find_map(|line| {
            line.strip_prefix("`kamchatka -r ")?
                .strip_suffix("` carries on from it")
        })
        .unwrap_or_else(|| panic!("the parting line says how to carry on: {said}"))
        .to_owned();

    let out = run(&second, &["-r", &state, "again"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert!(
        said.contains(&format!(
            "the record says this session was last talking to {first}"
        )),
        "{said}"
    );
    assert!(
        said.contains("from the second"),
        "it went where it was pointed: {said}"
    );
}
