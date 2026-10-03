//! The binary, serving and connecting, with a real socket between the two.
//!
//! note: spawned rather than wired, because what is under test is the program - the address
//! it prints, the loop it chose, what a `--connect` writes to stdout and what it writes to
//! stderr. None of that is reachable from inside a process that built an `App` for itself.

use std::{sync::Arc, time::Duration};

use kamchatka::{
    app::{Did, Speaker},
    remote::protocol::{self, Address, Command, Message},
};
use nachalnik::{
    Capability, ContextId, ContextItem, Grant, ModelResponse,
    test::{ConstTool, call},
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

use crate::{
    CLOSED, PATIENCE, Peer, Reacher, Socket, quit, served, served_at, served_over_a_socket,
};

/// The two flags, the socket file, and a whole session driven from one process to another.
///
/// note: the only test here that is about the *program* rather than the library. What it can catch
/// and nothing above it can: a flag wired to the wrong loop, a socket file left behind, a client
/// that tries to reach a model of its own, and the mode decision - a served run has no screen and
/// is not a headless one either, and the version of that decision this replaced announced a pipe
/// nobody had mentioned.
#[tokio::test(flavor = "multi_thread")]
async fn the_program_serves_a_socket_and_a_second_one_drives_it() {
    use crate::connect;

    let base =
        crate::common::endpoint(vec![crate::common::answer("what the other end reads")]).await;
    let dir = crate::common::scratch("served");
    let socket = dir.join("kamchatka.sock");

    let host = crate::common::command()
        .args(["--no-record", "-m", "nothing", "--serve"])
        .arg(format!("unix:{}", socket.display()))
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the host did not start");

    // the socket appears when the session is ready for somebody, and not before
    assert!(
        crate::listening(&socket).await,
        "nothing ever listened at {socket:?}"
    );

    let client = tokio::task::spawn_blocking({
        let socket = socket.clone();
        // note: the question, and then the input closes. It does not type `/quit`, because a
        // command runs the moment it arrives and one queued behind a question ends the session
        // while the answer to it is still being written. Closing the input detaches instead, and
        // the client stays for the turn it started - which is the rule, and is what makes this
        // deterministic rather than a race
        move || connect(&socket, b"say something\n")
    })
    .await
    .expect("the client panicked");

    let read = String::from_utf8_lossy(&client.stderr);
    assert!(
        read.contains("what the other end reads"),
        "the client never read the answer: {read}"
    );
    // note: a served session greets nobody, and this is where that is checked because the greeting
    // is `main.rs`'s. It is the terminal's - `ctrl+p` shows the next request, `F1` lists the keys -
    // and a served session has no keys of this program's to press: `--serve` is neither a screen
    // nor a pipe, and the condition that decided this had only ever been asked which of those two
    // it was. It went to every client that ever attached, because the greeting goes into the
    // conversation and the conversation is in every projection
    //
    // note: only a build with a screen in it has a greeting to get wrong, so under
    // `--no-default-features` this passes by having nothing to find. `--all-features` is where it
    // means something, and that is the configuration `cargo test --workspace` uses
    assert!(
        !read.contains("F1 lists the keys"),
        "a served session told a client about the terminal's keys: {read}"
    );
    // note: nor says the headless opening. A served session with no screen is asked the same
    // question a pipe is, and the answer put the host's `--on-ask` into every projection, though
    // no loop of a served session reads it: what a question gets is the attached clients' to say
    assert!(
        !read.contains("nobody can be asked is answered"),
        "a served session told a client how the host answers questions: {read}"
    );
    // and its stdout is the record stream, the same as a headless run's - and there is one, or
    // the loop says nothing about it
    let stdout = String::from_utf8_lossy(&client.stdout);
    assert!(stdout.lines().next().is_some(), "no records on stdout");
    for line in stdout.lines() {
        serde_json::from_str::<nachalnik::Record>(line).expect("every line is a record");
    }

    // the first client left and the session did not, so a second one can end it
    let quitter = tokio::task::spawn_blocking({
        let socket = socket.clone();
        move || connect(&socket, b"/quit\n")
    })
    .await
    .expect("the second client panicked");
    // note: and it is told the session ended, rather than finding the socket closed. The host is a
    // process that exits once the session is over, and a connection it had not yet written
    // `session.finished` to went with it - so the client that typed `/quit` read a drop, and went
    // looking for a session that was gone
    let said = String::from_utf8_lossy(&quitter.stderr);
    assert!(
        quitter.status.success() && !said.contains("attaching again"),
        "the client that ended the session was not told it had: {said}"
    );

    let host = tokio::task::spawn_blocking(move || host.wait_with_output())
        .await
        .expect("the host panicked")
        .expect("the host did not finish");
    let said = String::from_utf8_lossy(&host.stdout) + String::from_utf8_lossy(&host.stderr);
    // note: a served run has no screen and no record stream on stdout, so it is the one mode in
    // which the program can simply say things - and it has to say this one, because between
    // starting and being stopped it otherwise prints nothing at all
    assert!(
        said.contains(&format!("serving on unix:{}", socket.display())),
        "the host did not say where it was listening: {said}"
    );
    // note: a socket file that outlives its listener is a path every later client connects to and
    // then hangs on, which is a worse failure than not being able to connect at all
    assert!(!socket.exists(), "the socket file was left behind");
}

/// A served session says which mode it is in, and it is not a headless one.
///
/// note: the pair with `the_program_serves_a_socket_and_a_second_one_drives_it`, which reads the
/// host's own streams and is about the address it printed. What is under test here is the one line
/// it must not print: a served run is driven by a socket, not by a line driver, so an announcement
/// about headless runs is about a pipe nobody mentioned - and this host's stdout is one, which is
/// what the announcement would be about. Said to somebody reading the host's stderr it reads as
/// this session being a headless one.
///
/// note: a socket file rather than a port, so a machine that cannot bind a loopback port can run
/// it; the path is short for the reason `served_over_a_socket` gives.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_session_does_not_announce_a_headless_run() {
    use crate::common;

    let dir = common::scratch("snotice");
    let socket = dir.join("s.sock");

    let host = std::process::Command::new(common::program())
        .current_dir(common::nowhere())
        .args(["--no-record", "--serve"])
        .arg(format!("unix:{}", socket.display()))
        // an address nothing answers on: a turn would fail, and this is about the line the run
        // says about itself rather than about anything a turn did
        .env("KAMCHATKA_BASE_URL", CLOSED)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the host did not start");

    // a served session goes on for as long as somebody wants it to, so somebody has to end it
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(socket.exists(), "nothing ever listened at {socket:?}");
    let quitter = tokio::task::spawn_blocking({
        let socket = socket.clone();
        move || crate::connect(&socket, b"/quit\n")
    })
    .await
    .expect("the client panicked");

    let host = tokio::task::spawn_blocking(move || host.wait_with_output())
        .await
        .expect("the host panicked")
        .expect("the host did not finish");

    let said =
        String::from_utf8_lossy(&host.stdout).into_owned() + &String::from_utf8_lossy(&host.stderr);
    assert!(
        said.contains(&format!("serving on unix:{}", socket.display())),
        "the host did not say where it was listening: {said}"
    );
    assert!(
        !said.contains("is a headless run"),
        "a served session announced a mode it is not in: {said}"
    );
    assert!(quitter.status.success());
}

/// `examples/phone.rs` writes every session it ran out, the way the program does.
///
/// note: the example rather than a driver, because the bug was the example's and nothing
/// exercised it. It wired a session and waited for `Server::run`, which returns on `/quit` and on
/// `/restart` alike, so either command from the page ended the process with the session in memory
/// and nothing on disk. Driven the way a browser drives it: an event stream opens a tab, and
/// `POST /do` puts a line into the session through it.
///
/// note: `TMPDIR` is the whole isolation, as in `restart_writes_the_session_out_and_starts_another`:
/// the record goes under the temporary directory, so a run pointed at one of its own leaves
/// exactly the files this counts. No `-m`, so nothing is sent anywhere; what this is about is
/// which files are there afterwards.
///
/// note: `cargo test -p kamchatka` builds the example beside the binary and a run of this suite
/// alone may not, so the first assertion names that rather than leaving it to a spawn error.
#[test]
fn the_phone_example_writes_every_session_out() {
    use std::io::{BufRead as _, Read as _, Write as _};

    let example = crate::common::example("phone");
    assert!(
        example.exists(),
        "{} is not built: `cargo test -p kamchatka` builds the examples, `--test remote` alone \
         does not",
        example.display()
    );
    let dir = crate::common::scratch("phone-record");
    let mut child = std::process::Command::new(example)
        .current_dir(crate::common::nowhere())
        .env("TMPDIR", &dir)
        .env("KAMCHATKA_PHONE_LISTEN", "127.0.0.1:0")
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the example did not start");

    // the page's address is the second line it prints, and everything after that is the ending
    let mut out = std::io::BufReader::new(child.stdout.take().expect("stdout is a pipe")).lines();
    let page = loop {
        let line = out
            .next()
            .expect("the example stopped before saying where the page is")
            .expect("stdout is readable");
        if let Some((_, at)) = line.split_once(" at http://") {
            break at.trim_end_matches('/').to_owned();
        }
    };

    // a tab is a stream that is open, and it is open once its `retry:` has arrived
    let tab = |name: &str| -> std::net::TcpStream {
        let mut stream = std::net::TcpStream::connect(&page).expect("the page is reachable");
        stream.set_read_timeout(Some(PATIENCE)).expect("a timeout");
        write!(
            stream,
            "GET /events?tab={name} HTTP/1.1\r\nHost: {page}\r\n\r\n"
        )
        .expect("the request goes out");
        let mut seen = Vec::new();
        let mut chunk = [0u8; 1024];
        while !String::from_utf8_lossy(&seen).contains("retry: ") {
            let n = stream.read(&mut chunk).expect("the stream opens");
            assert!(
                n > 0,
                "the stream closed before it opened: {}",
                String::from_utf8_lossy(&seen)
            );
            seen.extend_from_slice(&chunk[..n]);
        }
        stream
    };
    // a line goes in through the tab. The relay hands a tab its channel just after the `retry:`
    // above, so a line posted in between is answered `409` and is posted again
    let post = |name: &str, line: &str| {
        let body = json!({ "do": "submit", "line": line }).to_string();
        for _ in 0..100 {
            let mut stream = std::net::TcpStream::connect(&page).expect("the page is reachable");
            stream.set_read_timeout(Some(PATIENCE)).expect("a timeout");
            write!(
                stream,
                "POST /do?tab={name} HTTP/1.1\r\nHost: {page}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("the request goes out");
            let mut answer = String::new();
            let _ = stream.read_to_string(&mut answer);
            if answer.starts_with("HTTP/1.1 202") {
                return;
            }
            assert!(
                answer.starts_with("HTTP/1.1 409"),
                "the page refused `{line}`: {answer}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the tab `{name}` never opened");
    };

    let mut first = tab("first");

    // and a page somewhere else is refused, whichever door it tries. Each would end the session
    // if it were taken, so a refusal missed here fails everything after it too
    let foreign = |line: &str, extra: &str, host: &str| -> String {
        let body = json!({ "do": "submit", "line": "/quit" }).to_string();
        let mut stream = std::net::TcpStream::connect(&page).expect("the page is reachable");
        stream.set_read_timeout(Some(PATIENCE)).expect("a timeout");
        write!(
            stream,
            "{line} HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("the request goes out");
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    };
    let post_first = "POST /do?tab=first";
    for (line, extra, host) in [
        (
            post_first,
            "Content-Type: application/json\r\nOrigin: http://elsewhere.example\r\n",
            page.as_str(),
        ),
        (post_first, "Content-Type: text/plain\r\n", page.as_str()),
        (
            post_first,
            "Content-Type: application/json\r\n",
            "elsewhere.example",
        ),
        // a frame or an image somewhere else, which carries no `Origin`, taking the session
        // from the tab that has it
        (
            "GET /events?tab=elsewhere",
            "Sec-Fetch-Site: cross-site\r\n",
            page.as_str(),
        ),
    ] {
        let answer = foreign(line, extra, host);
        assert!(
            answer.starts_with("HTTP/1.1 403"),
            "a request from somewhere else was taken: {line} {extra}{host}\n{answer}"
        );
    }

    post("first", "/restart");
    // the restart lets go of every client, which is this stream ending. The page comes back into
    // the new session by itself; this does the same by hand, under another name
    let mut rest = Vec::new();
    first
        .read_to_end(&mut rest)
        .expect("the stream ends when the session restarts");
    let _second = tab("second");
    post("second", "/quit");

    // the run ends on its own, and says what it wrote
    let deadline = std::time::Instant::now() + PATIENCE;
    let status = loop {
        match child.try_wait().expect("the child can be waited on") {
            Some(status) => break status,
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            None => {
                let _ = child.kill();
                let mut said = String::new();
                let _ = child
                    .stderr
                    .take()
                    .expect("stderr is a pipe")
                    .read_to_string(&mut said);
                panic!("the example did not end after `/quit`: {said}");
            }
        }
    };
    let mut said = String::new();
    child
        .stderr
        .take()
        .expect("stderr is a pipe")
        .read_to_string(&mut said)
        .expect("stderr is readable");
    assert!(status.success(), "{said}");
    let ending = out
        .map(|line| line.expect("stdout is readable"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        ending.contains("records in"),
        "the run did not say where the last session went: {ending}\n{said}"
    );

    // two sessions, two records: the one `/restart` wrote and the one `/quit` did, and the second
    // is a session of its own rather than the first one's log written twice
    let logs: Vec<String> = std::fs::read_dir(dir.join("kamchatka"))
        .expect("the record directory")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".jsonl"))
        .collect();
    assert_eq!(
        logs.len(),
        2,
        "one record per session: {logs:?}\n{ending}\n{said}"
    );
    let mut names: Vec<&str> = logs
        .iter()
        .map(|it| it.trim_end_matches(".jsonl"))
        .collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 2, "both records have the same name: {logs:?}");
}

/// `--serve` refuses the two flags a served session never reads, rather than serving for ever
/// beside them.
///
/// note: `--deadline 60` beside `--serve` reads as a run that ends by itself, and what it got is a
/// session serving until it is killed; `--on-ask` is a question answered by whoever is attached,
/// and a served session has none of its own. `--connect` refuses what does not apply to it the
/// same way, and this is the other half of that. The refusal is before the bind, so nothing is
/// left listening: a path that is refused is not a session anybody can attach to.
///
/// note: and the same two out of a **settings file**, which is the case that was dropped. The check
/// was `ValueSource::CommandLine`, and a value read out of a file is written into the arguments by
/// the merge and leaves nothing behind saying where it came from - so a project whose
/// `kamchatka.json` says `deadline` served for ever, beside a file saying when the run should end,
/// and said nothing. A person writing a setting down once rather than typing it every day is
/// exactly the case the refusal exists for, and least likely to be looking at this run's command
/// line - so the file is named, the way every other value out of one is.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_run_refuses_the_flags_it_does_not_read() {
    let dir = crate::common::scratch("served-refused");
    for (flag, value) in [("--deadline", "60"), ("--on-ask", "allow")] {
        let socket = dir.join(format!("{}.sock", flag.trim_start_matches("--")));
        let said = tokio::process::Command::from(crate::common::command())
            .args(["--no-record", "-m", "nothing", "--serve"])
            .arg(format!("unix:{}", socket.display()))
            .arg(flag)
            .arg(value)
            .output()
            .await
            .expect("the program did not start");
        let refused = String::from_utf8_lossy(&said.stderr);

        assert!(!said.status.success(), "{flag} was accepted");
        assert!(refused.contains(flag), "{flag} was not named: {refused}");
        assert!(
            refused.contains("goes on for as long as somebody wants it"),
            "{refused}"
        );
        assert!(
            !socket.exists(),
            "it listened before refusing {flag}, and left the socket behind"
        );
    }

    // and the same two written down in a file, which is where they are read most often
    for (key, value) in [("deadline", "60"), ("on-ask", "allow")] {
        let settings = dir.join(format!("{key}.json"));
        std::fs::write(&settings, format!(r#"{{ "{key}": "{value}" }}"#)).expect("a settings file");
        let socket = dir.join(format!("file-{key}.sock"));
        let said = tokio::process::Command::from(crate::common::command())
            .args(["--no-record", "-m", "nothing", "--serve"])
            .arg(format!("unix:{}", socket.display()))
            .arg("--config-file")
            .arg(&settings)
            .output()
            .await
            .expect("the program did not start");
        let refused = String::from_utf8_lossy(&said.stderr);

        assert!(!said.status.success(), "a file's `{key}` was accepted");
        // the key as it is written, and the file it was written in: a refusal naming `--{key}`
        // would be naming a flag nobody typed. The half of the sentence that explains why says the
        // arguments by their own names, so only the clause naming what was dropped is read here
        let named = refused
            .split("a served session is this program's")
            .next()
            .unwrap_or_default();
        assert!(named.contains(key), "the key was not named: {refused}");
        assert!(
            named.contains(&settings.display().to_string()),
            "the file was not named: {refused}"
        );
        assert!(
            !named.contains(&format!("--{key}")),
            "it was refused as a flag rather than as a setting: {refused}"
        );
        assert!(
            !socket.exists(),
            "it listened before refusing a file's `{key}`, and left the socket behind"
        );
    }

    // and the two the same file would carry with the *default* in them are not among the
    // refusals: `--print-config` writes every key, so the file this program hands out to be
    // edited says `on-ask: deny` and `deadline: null`, and refusing every `--serve` over it
    // would break the one the documentation tells a reader to write. Spelled as a person might
    // write it, since the value is read without regard to case
    let settings = dir.join("defaults.json");
    std::fs::write(
        &settings,
        r#"{ "on-ask": "Deny", "deadline": null, "spend": 100000 }"#,
    )
    .expect("a settings file");
    let socket = dir.join("defaults.sock");
    let mut host = tokio::process::Command::from(crate::common::command())
        .args(["--no-record", "-m", "nothing", "--serve"])
        .arg(format!("unix:{}", socket.display()))
        .arg("--config-file")
        .arg(&settings)
        .env("KAMCHATKA_BASE_URL", CLOSED)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the program did not start");
    assert!(
        crate::listening(&socket).await,
        "a file carrying only the defaults was refused, so the file `--print-config` writes \
         cannot be used to serve"
    );
    let _ = host.kill().await;
}

/// A client asked for a session that is not there says so, rather than trying five times.
///
/// note: the distinction the retry loop turns on. A connection that *drops* may well come back, and
/// picking it up from the last record is the point; one that was never made is a wrong address or a
/// session nobody started, and five attempts at it is five times as long before anybody is told.
#[tokio::test]
async fn a_client_that_finds_nothing_there_says_so_at_once() {
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    let refused = kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run("tcp:127.0.0.1:1", "".as_bytes())
        .await
        .expect_err("it connected to nothing");

    assert!(refused.contains("could not reach 127.0.0.1:1"), "{refused}");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        !prose.contains("attaching again"),
        "it reconnected to a connection it never had: {prose}"
    );
}

/// A question piped in, and the input closed behind it, still waits for the answer.
///
/// note: the shape of `echo "question" | kamchatka --connect`, and the rule is `--headless`'s: a
/// script that pipes one question in and goes away is asking for the answer, not for the question
/// to be asked and abandoned. What it must *not* do is end the session on the way out - that
/// belongs to whoever is serving it - so the two halves of this are that the client comes back with
/// the answer and that the session is still there afterwards.
#[tokio::test]
async fn a_question_piped_in_waits_for_its_answer_and_leaves_the_session() {
    let session = served(vec![ModelResponse::text("the whole answer")], |_| {}).await;

    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"say something\n")
        .await
        .expect("could not type");
    drop(feed);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("the whole answer"), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A question the piped-in line raised is answered on the way out rather than abandoned.
///
/// note: the hole the test above could not see, because its model asks for no tools. A turn paused
/// on a permission question is not *running*, so `busy` comes back false while the kernel sits in
/// `Deciding` - and a client reading that alone took it for the end of the turn, printed the
/// question, and exited `0`. What it left behind is the half that matters: a served session
/// waiting on an answer that no longer had anywhere to come from, for as long as the process
/// lived. So the two halves here are that the answer is given and said out loud, and that the turn
/// it was blocking reaches its end.
///
/// note: `deny` rather than `allow`, and the assertion names the refusal, because the default is
/// the load-bearing half: a run nobody is watching should not be able to do a thing nobody
/// allowed. `--on-ask allow` is one flag away for anybody who means it, and it is the same flag
/// and the same word `--headless` has always taken.
#[tokio::test]
async fn a_question_nobody_is_left_to_answer_is_answered_on_the_way_out() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("it would not let me"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"go\n").await.expect("could not type");
    drop(feed);

    // note: under `PATIENCE`, because the two ways to get this wrong fail in opposite directions.
    // Leaving the question unanswered ends the client early and trips the assertions below; not
    // letting it leave at all is a client waiting on an answer it is itself supposed to give, and
    // that one hangs rather than fails
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&session.at, BufReader::new(input)),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        prose.contains("nobody is here to answer for `peek`"),
        "it left without saying what it did with the question: {prose}"
    );
    // and the turn the question was holding up got to the other side of it
    assert!(prose.contains("it would not let me"), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client that leaves its questions exits without answering one, and the question waits for
/// the next client.
///
/// note: what `--connect` does unless `--on-ask` is typed. A watcher whose input closed answered
/// every open question with `deny` - questions another client's person had been asked - while
/// RUNNING.md says a question waits for somebody to come back. The second client is the somebody:
/// it finds the question still open, and its answer is what takes the turn to the other side.
#[tokio::test]
async fn a_client_that_leaves_its_questions_leaves_them_for_the_next_one() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("it would not let me"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"go\n").await.expect("could not type");
    drop(feed);

    // under `PATIENCE`: a client that waited for the question to be answered would be waiting on
    // nobody, and that one hangs rather than fails
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .leaves_questions()
            .waits_for_turns()
            .run(&session.at, BufReader::new(input)),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        prose.contains("it is left for another client"),
        "it left without saying what it did with the question: {prose}"
    );
    assert!(
        !prose.contains("nobody is here to answer for `peek`"),
        "it answered a question it was to leave: {prose}"
    );
    assert!(!prose.contains("it would not let me"), "{prose}");

    // and the next client finds it still open, answers it, and the turn goes on
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&session.at, BufReader::new(tokio::io::empty())),
    )
    .await
    .expect("the second client never left")
    .expect("the second client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        prose.contains("nobody is here to answer for `peek`"),
        "the question was not waiting for it: {prose}"
    );
    assert!(prose.contains("it would not let me"), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// The program leaves a question when its own command line says nothing about one, and answers it
/// when it says so.
///
/// note: `--connect` reads `on_ask` off its own command line, and `leave` is what a watcher gets by
/// default. What decides that is `main.rs` reading whether the flag was typed at all, which the
/// two client tests above reach only by saying `.leaves_questions()` themselves - so they pin what
/// that flag means to the `Client`, and nothing here would notice the program handing every
/// question it is shown to its own `on_ask`. A watcher whose input closed would answer every open
/// question `deny` - questions another client's person had been asked - and the question would be
/// gone before anybody could come back to it.
#[tokio::test(flavor = "multi_thread")]
async fn the_program_leaves_a_question_it_was_not_told_to_answer() {
    use crate::connect;

    let session = served_over_a_socket(
        "leave",
        vec![
            ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
            ModelResponse::text("it would not let me"),
        ],
        |app| {
            app.kernel.add_tool(Arc::new(
                ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
            ));
        },
    )
    .await;

    // the program as a watcher of somebody else's session, with nothing said about questions
    let watched = socket_of(&session.at);
    let out = tokio::task::spawn_blocking(move || connect(&watched, b"go\n"))
        .await
        .expect("the client panicked");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        said.contains("it is left for another client"),
        "it did not leave a question it was not told to answer: {said}"
    );
    assert!(
        !said.contains("nobody is here to answer for `peek`"),
        "it answered a question it was to leave: {said}"
    );
    assert!(!said.contains("it would not let me"), "{said}");

    // and the next client finds it still open, answers it, and the turn goes on
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&session.at, BufReader::new(tokio::io::empty())),
    )
    .await
    .expect("the second client never left")
    .expect("the second client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        prose.contains("nobody is here to answer for `peek`"),
        "the question was not waiting for it: {prose}"
    );
    assert!(prose.contains("it would not let me"), "{prose}");

    crate::quit_over_a_socket(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// The program reads a piped line only once the turn before it is over, so a `y` piped behind a
/// message answers the question rather than being sent as a message.
///
/// note: `--headless` reads a line only when the session is quiet, and a `--connect` whose input is
/// a pipe does the same - `echo "go
/// " | kamchatka --connect` is a script, not somebody at a keyboard, and a `y` in a script is an
/// answer to whatever question the session has open when the line is read. Reading it as soon as
/// the turn starts sends it as a message instead, so the question is never answered by the person
/// who wrote the `y`: it is left open, and answered by `settle` after the input has closed, with
/// the line saying nobody was there to answer it - which is a statement about a script nobody
/// wrote. What tells them apart is that line.
#[tokio::test(flavor = "multi_thread")]
async fn the_program_reads_a_piped_answer_when_the_question_is_asked() {
    use crate::connect;

    let session = served_over_a_socket(
        "paced",
        vec![
            ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
            ModelResponse::text("it would not let me"),
        ],
        |app| {
            app.kernel.add_tool(Arc::new(
                ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
            ));
        },
    )
    .await;

    // the question is asked by the turn, and the `y` behind it is the script's answer to it
    let watched = socket_of(&session.at);
    let out = tokio::task::spawn_blocking(move || connect(&watched, b"go\ny\n"))
        .await
        .expect("the client panicked");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        said.contains("wants to run peek"),
        "the question was never asked: {said}"
    );
    assert!(
        !said.contains("nobody is here to answer"),
        "the `y` in the script was not read as the answer it was: {said}"
    );
    // and the turn the question was holding up reached the other side
    assert!(said.contains("it would not let me"), "{said}");

    crate::quit_over_a_socket(&session.at).await;
    let (app, ended) = session.ended().await;
    ended.expect("the session failed");
    assert!(
        app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text() == "the answer"),
        "the call the `y` allowed did not run: {:?}",
        app.kernel.items()
    );
}

/// The path of the socket a `--connect` on a socket file is given.
fn socket_of(at: &str) -> std::path::PathBuf {
    let kamchatka::remote::protocol::Address::Unix(path) =
        kamchatka::remote::protocol::address(at).expect("a socket file")
    else {
        panic!("{at} is not a socket file")
    };
    path.into()
}

/// A client that leaves its questions leaves too when a line of its own is held behind one, and
/// says the line was not sent.
///
/// note: found live. `/budget` piped after a message whose turn stopped on a question was held for
/// the turn to be over, and the turn was waiting for another client: the client said it was
/// leaving the question and then stayed attached for as long as anybody let it.
#[tokio::test]
async fn a_line_held_behind_a_question_it_leaves_does_not_keep_the_client() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("it would not let me"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"go\n/budget\n")
        .await
        .expect("could not type");
    drop(feed);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .leaves_questions()
            .waits_for_turns()
            .run(&session.at, BufReader::new(input)),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("it is left for another client"), "{prose}");
    assert!(prose.contains("`/budget` was not sent"), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client that leaves its questions, with none waiting, says nothing about leaving any.
///
/// note: the line is a claim about what happened, and what it says is that the session is asking
/// something and this client is going away without answering it. A session asking nothing has
/// nothing to leave, and a client that says it anyway has told the person reading its stderr that
/// somebody else's session is waiting for an answer - which is the one thing `--connect` reports to
/// somebody who has come to look after a client that was doing nothing.
#[tokio::test]
async fn a_client_leaving_no_question_waiting_says_it_left_none() {
    let session = crate::served_over_a_socket("leaves-nothing-waiting", Vec::new(), |_| {}).await;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .leaves_questions()
            .run(&session.at, tokio::io::empty()),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(
        !prose.contains("it is left for another client"),
        "it said it left a question behind, and none was waiting: {prose}"
    );

    crate::quit_over_a_socket(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client that leaves its questions leaves a command waiting for the network gate as well as the
/// kernel's own, and says so.
///
/// note: two questions, and they are not the same one. The kernel's is a question about a tool,
/// asked with a turn paused behind it; the network gate's is asked while the command is still
/// running, is in no record, and reaches a client only in the list the session sends. A client
/// that leaves the first and not the second holds a command open with nobody left to answer it -
/// which is the whole of what it is for, said about the other kind.
#[tokio::test]
async fn a_client_leaving_a_command_waiting_for_the_network_says_it_left_it() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "reacher", json!({}))]),
        ModelResponse::text("it went"),
    ];
    let session = crate::served_over_a_socket("leaves-a-reacher", script, |app| {
        app.kernel.add_tool(Arc::new(Reacher(app.policy.clone())));
    })
    .await;

    // note: the input closes only once the question has been put to the client, so this is a
    // client leaving a question it has been shown rather than one that happened to be waiting by
    // the time its input ended. A test that closed it at once would pass or fail on how fast the
    // two raced, which is not the claim.
    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"go\n").await.expect("could not type");
    let heard = crate::Heard::default();
    let waiting = heard.clone();
    tokio::spawn(async move {
        while !waiting.text().contains("is running and has reached") {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(feed);
    });

    let (mut records, mut prose) = (Vec::new(), heard.clone());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .leaves_questions()
            .run(&session.at, tokio::io::BufReader::new(input)),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");
    let prose = heard.text();

    assert!(
        prose.contains("`curl x` is running and has reached"),
        "the question was never put to the client: {prose}"
    );
    assert!(
        prose.contains("it is left for another client"),
        "it left a command waiting and did not say so: {prose}"
    );
    assert!(
        !prose.contains("nobody is here to answer whether"),
        "it answered a question it was to leave: {prose}"
    );

    // and the next client is the somebody it left the question to, which is what makes the claim
    // above one about the command rather than about a line of prose
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&session.at, tokio::io::empty()),
    )
    .await
    .expect("the second client never left")
    .expect("the second client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        prose.contains(
            "nobody is here to answer whether `curl x` may reach the network, so it is answered \
             `deny`"
        ),
        "the question was not waiting for it: {prose}"
    );

    crate::quit_over_a_socket(&session.at).await;
    let (app, ended) = session.ended().await;
    ended.expect("the session failed");
    assert!(
        app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("answered Some(false)")),
        "the command was let through by a client that was to leave it: {:?}",
        app.kernel.items()
    );
}

/// A session speaking a version this client does not is left rather than attached to again.
///
/// note: the client sends the version this build speaks, so the only way to be refused for one is
/// to be a different build - and both ends of a connection here are this one. What stands in is a
/// relay that says one word differently on the way past, which is cheaper than a second
/// implementation of the protocol for the sake of one number.
///
/// note: what it is pinning is that a refusal a fresh attach cannot mend ends the client on the
/// session's own sentence. Treated like the watermark refusal beside it, the client reattached, was
/// refused identically, and left a minute later saying the session had not answered for sixty
/// seconds - which is the one thing that had not happened. The input is held open throughout, so
/// nothing but the refusal can be what ended it.
#[tokio::test]
async fn a_version_the_session_refuses_is_not_attached_to_again() {
    let session = served(vec![], |_| {}).await;
    let ahead = format!("\"version\":{}", protocol::VERSION + 1);
    let at = rewriting(
        &session.at,
        move |line| line.replace(&format!("\"version\":{}", protocol::VERSION), &ahead),
        |line| line,
    )
    .await;

    let (_feed, input) = tokio::io::duplex(256);
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    let left = tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client kept reattaching to a session that had already answered");

    let refused = left.expect_err("a refused version read as a session worth carrying on with");
    assert!(refused.contains("the older end is this one"), "{refused}");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        !prose.contains("attaching again"),
        "it went back for more of the same answer: {prose}"
    );

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// An answer in a name this build has never heard of is still an answer, and is not waited for.
///
/// note: the hole the forward-compatibility work would otherwise have left open with its own
/// escape hatch in it. `Message::Unknown` carries no payload - `#[serde(other)]` takes a unit
/// variant - so a client owed an answer and handed one it cannot read cannot tell it from a
/// broadcast. A count that never came back down was stdin closing that never detached and a
/// session going quiet that never ended it, for the rest of the connection: `printf 'a question\n'
/// | kamchatka --connect` against a session one version ahead hung.
///
/// note: what this deliberately does **not** assert is that the answer arrives. Counting an
/// unrecognised message as an answer is a decision about which way to be wrong, and this is the
/// cost of it: the client leaves on a message it could not have printed, where it used to wait for
/// one that had already come and was never coming again. The relay renames the one message that is
/// this client's answer, which is a session a version ahead answering an older client, and the
/// claim is that it survives it rather than that it understood it.
#[tokio::test]
async fn an_answer_this_build_cannot_read_still_counts_as_one() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;
    let at = rewriting(
        &session.at,
        |line| line,
        |line| line.replace("\"is\":\"replied\"", "\"is\":\"replied-and-then-some\""),
    )
    .await;

    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"ask something\n")
        .await
        .expect("could not type");
    drop(feed);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client waited for an answer it had already been handed")
    .expect("the client failed");

    // and the session is still a session, which is what the client leaving is not allowed to cost
    let (watch, attached) = Peer::attached(&session.at).await;
    assert!(
        attached
            .conversation
            .iter()
            .any(|line| line.text.contains("ask something")),
        "the line never reached the session: {:?}",
        attached.conversation
    );
    watch.drop_it().await;

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A socket in front of a session, with every line said differently on the way past.
///
/// note: what the two tests above need is a session that speaks something this build does not, and
/// both ends of a connection here are this build. One word rewritten on the wire is how a version
/// that does not exist gets said out loud.
async fn rewriting(
    at: &str,
    to_session: impl Fn(String) -> String + Clone + Send + 'static,
    to_client: impl Fn(String) -> String + Clone + Send + 'static,
) -> String {
    let Ok(Address::Tcp(host)) = protocol::address(at) else {
        panic!("the suite serves a port");
    };
    let host = host.to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", listener.local_addr().expect("its own address"));
    tokio::spawn(async move {
        while let Ok((down, _)) = listener.accept().await {
            let up = TcpStream::connect(&host).await.expect("the session went");
            let (down_r, down_w) = down.into_split();
            let (up_r, up_w) = up.into_split();
            tokio::spawn(relaying(down_r, up_w, to_session.clone()));
            tokio::spawn(relaying(up_r, down_w, to_client.clone()));
        }
    });

    at
}

/// Copies one direction of a connection, a line at a time.
async fn relaying(
    read: tokio::net::tcp::OwnedReadHalf,
    mut write: tokio::net::tcp::OwnedWriteHalf,
    say: impl Fn(String) -> String,
) {
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if write
            .write_all(format!("{}\n", say(line)).as_bytes())
            .await
            .is_err()
        {
            return;
        }
    }
}

/// `/quit` from a client ends the session, and reads as an ending rather than as a dropped socket.
///
/// note: the difference is five reconnection attempts at a session that did exactly what it was
/// told. What tells the two apart is the `session.finished` record, which is why this asserts on
/// the record as well as on the outcome - a client that returned `Ok` having never been told the
/// session ended got there by accident.
#[tokio::test]
async fn quitting_from_a_client_reads_as_an_ending() {
    let session = served(vec![], |_| {}).await;

    // note: the input stays open, which is what makes this about the *session* ending rather than
    // about the input closing. A client whose stdin has gone leaves as soon as the session is
    // quiet, and would be gone before the last record was written; this one has nothing left to
    // type and is still listening, so what ends it is the socket closing behind a session that
    // said it had finished
    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"/quit\n").await.expect("could not type");

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("quitting was read as a failure");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        !prose.contains("attaching again"),
        "it tried to reconnect to a session it had just ended: {prose}"
    );
    // and said so, because a `/restart` ends it the same way and the session after it is not this
    // client's to carry on into
    assert!(prose.contains("the session has ended"), "{prose}");
    assert!(
        String::from_utf8_lossy(&records).contains("session.finished"),
        "the ending was never sent to the client still attached for it"
    );

    session.ended().await.1.expect("the session failed");
}

/// A served run says the address it *got*, while it is still running, and a client can reach it.
///
/// note: `tcp:127.0.0.1:0` is the case this is for. Asking the kernel for a port is the sensible
/// thing to do and it leaves the address as the one fact the person who typed the flag does not
/// have - so a script starts a session, reads the line, and connects to what it says.
/// `Server::address` asks the socket rather than repeating the argument, which is the whole of why
/// that works, and this is what says so.
///
/// note: read off the *live* pipe rather than from a finished process, because a script does not
/// get to wait for the session to end before connecting to it.
///
/// note: measured, and it is worth saying which half of it is its own. That a served run says
/// something on stdout is shared with `the_program_serves_a_socket_and_a_second_one_drives_it`
/// above, which reads it after the process ends; that `Server::address` asks the socket is covered
/// by every test in this file, because they all connect to what it returns. What is only here is
/// the port nobody chose, read while the session is still up and connected to - which is the whole
/// of how a script is meant to use this.
///
/// note: what this is **not** about is flushing, and the first version of this note said it was. A
/// session whose stdout had been redirected looked as though it were holding the line back, and
/// that host had simply failed to bind, with the reason on stderr where it belonged. Rust's
/// `Stdout` is a `LineWriter` whatever it points at, so `println!` has already flushed by the time
/// it returns; the flush added here on the strength of that reading failed nothing when it was
/// taken away again, and was taken away.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_run_says_the_address_it_got_and_a_client_can_reach_it() {
    let base = crate::common::endpoint(vec![crate::common::answer(
        "reached through a port nobody chose",
    )])
    .await;

    let mut host = tokio::process::Command::from(crate::common::command())
        .args(["--no-record", "-m", "nothing", "--serve", "tcp:127.0.0.1:0"])
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // so a failing assertion below does not leave a session serving for the rest of the run
        .kill_on_drop(true)
        .spawn()
        .expect("the host did not start");

    let mut said = BufReader::new(host.stdout.take().expect("a pipe")).lines();
    let line = tokio::time::timeout(PATIENCE, said.next_line())
        .await
        .expect("the host never said where it was listening")
        .expect("the host's output stopped")
        .expect("the host said nothing at all");
    let at = line
        .split_whitespace()
        .next_back()
        .expect("the line named no address");
    assert!(at.starts_with("tcp:127.0.0.1:"), "{line}");
    assert!(
        !at.ends_with(":0"),
        "it reported the port it asked for, not the one it got"
    );

    // and the address it printed is one a client can actually reach
    let (mut peer, attached) = Peer::attached(at).await;
    assert_eq!(attached.items.len(), 0);
    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    tokio::time::timeout(PATIENCE, host.wait())
        .await
        .expect("the session did not end")
        .expect("the host did not finish");
}

/// A served run whose stdout nobody is reading still serves, and still leaves on its own terms.
///
/// note: the line naming the address is run-level prose on stdout, and it was printed with the
/// macros, which panic when the write fails. A supervisor that closes a server's stdout is an
/// ordinary arrangement - so is a script that pipes it into something with no use for it - and
/// either one ended the run with a panic and exit `101`, before a client could connect and long
/// before anything was written down.
///
/// note: closed rather than redirected, which is what makes the write fail: `/dev/null` accepts
/// everything. A pipe whose read end this end has let go of is the shape a script's is in by the
/// time it stops caring, and a broken pipe is what the program meets there.
///
/// note: the run is ended from the other end, because the assertion is that the session went on
/// being servable and then ended the way it was asked to rather than the way a panic ends it. A
/// client is attached first: a session that panicked before it could serve anything is not
/// servable.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_run_whose_stdout_nobody_is_reading_still_serves() {
    let dir = crate::common::scratch("served-unread");
    let socket = dir.join("kamchatka.sock");

    let mut host = tokio::process::Command::from(crate::common::command())
        .args(["--no-record", "-m", "nothing", "--serve"])
        .arg(format!("unix:{}", socket.display()))
        // no model is ever asked for, so nothing is sent to an endpoint and the address only has
        // to be one nothing is listening on
        .env("KAMCHATKA_BASE_URL", CLOSED)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the host did not start");
    // the address went into a pipe nobody is going to read, and the run carries on regardless
    drop(host.stdout.take().expect("a pipe"));

    assert!(
        crate::listening(&socket).await,
        "nothing ever listened at {socket:?}"
    );

    let mut peer = tokio::net::UnixStream::connect(&socket)
        .await
        .expect("a run that panicked on its first line serves nothing");
    let (read, mut write) = peer.split();
    let mut frames = protocol::Frames::new(BufReader::new(read));
    protocol::write(&mut write, &crate::attaching(None, None))
        .await
        .expect("the session stopped listening");
    let attached = loop {
        match tokio::time::timeout(PATIENCE, protocol::read::<Message>(&mut frames))
            .await
            .expect("a run that panicked on its first line serves nothing at all")
            .expect("the session said something unreadable")
        {
            Some(Message::Attached(attached)) => break attached,
            Some(_) => {}
            None => panic!("the session closed the connection"),
        }
    };
    // what it hands a client is the conversation, and the opening is the line saying where this
    // session is being served from - which went to a stdout nobody read, so this is where it is
    // still to be had
    assert!(
        attached
            .conversation
            .iter()
            .any(|line| line.text.contains("serving on")),
        "a client that attached was not told what it had joined: {:?}",
        attached.conversation
    );

    protocol::write(
        &mut write,
        &Command::Submit {
            line: "/quit".to_owned(),
        },
    )
    .await
    .expect("the session stopped listening");
    let ended = tokio::time::timeout(PATIENCE, host.wait())
        .await
        .expect("the session did not end")
        .expect("the host did not finish");
    assert!(
        ended.success(),
        "a `/quit` leaves on 0, and this left on {:?}",
        ended.code()
    );
}

/// `ctrl+c` at a client stops the turn, and a second one detaches without ending the session.
///
/// note: the two stages, and the invariant between them. The first press is for the *turn* and
/// keeps what arrived; the second is for this process, and the session carries on without it -
/// which is the one thing a client must never decide for somebody else. There was only ever one
/// stage: a second press sent another interrupt and printed the same line, and since
/// `tokio::signal::ctrl_c` does not put the default handler back, there was no way out of a
/// `--connect` at all short of killing it.
///
/// note: a child process, because `ctrl+c` is a *signal* and there is no other way to send one.
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_at_a_client_stops_the_turn_and_then_detaches() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;

    // stdin is a pipe this test holds open and never writes to, so nothing but the signal can end
    // this client - which is what makes the assertion about the signal
    let mut client = tokio::process::Command::from(crate::common::command())
        .args(["--connect", &session.at])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the client did not start");
    let mut read = BufReader::new(client.stderr.take().expect("a pipe")).lines();
    // note: held rather than left in the `Child`, and this is the trap in writing this test.
    // `Child::wait` closes stdin before it waits, stdin closing is the *other* way this client
    // leaves, and a test that reached for `wait` passed with the second stage taken out again
    let _stdin = client.stdin.take().expect("a pipe");

    // attached, and therefore in the loop with the signal branch armed
    let mut prose = String::new();
    while !prose.contains("a line is a message") {
        let line = tokio::time::timeout(PATIENCE, read.next_line())
            .await
            .expect("the client said nothing")
            .expect("the client's output stopped")
            .expect("the client ended before it was interrupted");
        prose.push_str(&line);
        prose.push('\n');
    }

    let id = client.id().expect("it is running").to_string();
    let interrupt = |id: &str| {
        std::process::Command::new("kill")
            .args(["-INT", id])
            .status()
            .expect("`kill` is on the path")
    };
    assert!(interrupt(&id).success());
    while !prose.contains("asked it to stop") {
        let line = tokio::time::timeout(PATIENCE, read.next_line())
            .await
            .expect("the first press was not heard")
            .expect("the client's output stopped")
            .expect("the client left on the first press");
        prose.push_str(&line);
        prose.push('\n');
    }
    // and it is still attached, which is the whole of what the first stage means
    assert!(
        client.try_wait().expect("it was spawned").is_none(),
        "the first press left: {prose}"
    );

    assert!(interrupt(&id).success());
    let status = {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            match client.try_wait().expect("it was spawned") {
                Some(status) => break status,
                None if std::time::Instant::now() > deadline => {
                    panic!("the second press did not detach it: {prose}")
                }
                None => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
    };
    assert!(status.success(), "detaching is not a failure: {prose}");
    drop(_stdin);

    // the session is still there, which is the invariant the whole module is built on
    let (peer, _) = Peer::attached(&session.at).await;
    peer.drop_it().await;
    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// `/cleanup` is said to the client, which has the program's lines on a screen of its own.
///
/// note: a broadcast rather than an answer, for the reason every other notice is one: the program
/// has one voice, and a session drawn at a desk and watched from a phone does not have half of it
/// cleared - a `/cleanup` typed at the desk reaches the client too.
#[tokio::test]
async fn clearing_the_notices_is_said_to_the_client() {
    let served = served(vec![], |_| {}).await;
    let (mut one, _) = Peer::attached(&served.at).await;

    one.send(Command::Submit {
        line: "/cleanup".to_owned(),
    })
    .await;

    let heard = one.until(|m| matches!(m, Message::Cleared)).await;
    assert!(
        heard.iter().any(|m| matches!(m, Message::Cleared)),
        "the client was not told the lines went: {heard:?}"
    );

    one.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// And a line said after a cleanup still reaches a client, which is the half that breaks
/// quietly.
///
/// note: this is the test that is worth having and the one above is the feature. The server reads
/// the program's lines through `App::notes`, which is a watermark over the filtered sequence -
/// safe only while that sequence grows, and `/cleanup` empties it. A server that did not notice
/// would hold a mark of three against a sequence of nothing and swallow the next three lines,
/// silently, for the rest of the session. Two lines are cleared here so that the mark is high
/// enough for the swallowing to be visible.
#[tokio::test]
async fn a_line_said_after_a_cleanup_is_not_swallowed() {
    let served = served(vec![], |_| {}).await;
    let (mut peer, _) = Peer::attached(&served.at).await;

    for line in ["/seams", "/budget", "/cleanup"] {
        peer.send(Command::Submit {
            line: line.to_owned(),
        })
        .await;
    }
    peer.until(|m| matches!(m, Message::Cleared)).await;

    // and now something to say. `/seams` says its piece as a page rather than a line, so this asks
    // for one that is refused - a refusal is a line, and it is the shape a session says most of
    // what it says in
    peer.send(Command::Submit {
        line: "/limit nonsense".to_owned(),
    })
    .await;
    let heard = peer
        .until(|m| matches!(m, Message::Said { .. } | Message::Done { .. }))
        .await;
    assert!(
        heard.iter().any(|m| matches!(m, Message::Said { .. })),
        "the line after a cleanup was swallowed: {heard:?}"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// `project` answers with the figures again and leaves the stream where it was.
///
/// note: the distinction the whole command exists for, and it is about what the answer *means*
/// rather than about what follows it. A client takes an `attached` as start again - it has just
/// been handed the conversation and the stream that carries on from it - so one that could not
/// tell the two apart would wipe its own screen to refresh a token count. So what is asserted is
/// the negative: a `projected`, and no `attached` behind it.
#[tokio::test]
async fn a_projection_can_be_asked_for_again_without_starting_over() {
    let script = vec![ModelResponse::text("the kernel is a state machine")];
    let served = served(script, |_| {}).await;
    let (mut peer, first) = Peer::attached(&served.at).await;

    peer.send(Command::Submit {
        line: "what does the kernel do?".to_owned(),
    })
    .await;
    peer.until_record("model.finished").await;

    peer.send(Command::Project).await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(again)) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one");
    };

    // the session is the same one and has moved on, which is the whole of what a client wanted
    assert_eq!(again.session, first.session);
    assert!(
        again.seq > first.seq,
        "a fresh projection reported a stale sequence: {} then {}",
        first.seq,
        again.seq
    );
    assert!(
        !again.items.is_empty() && first.items.is_empty(),
        "the items are what it was asked for: {:?}",
        again.items
    );

    // and nothing was replayed. The next thing on the wire is the answer to the next command,
    // because asking for a projection is not attaching - behind any record newer than the
    // projection, which is the log carrying on rather than going back, and which a reply waits for
    peer.send(Command::Interrupt).await;
    let next = loop {
        match peer.recv().await {
            Message::Record(record) if record.seq > again.seq => continue,
            other => break other,
        }
    };
    assert!(
        matches!(next, Message::Done { .. }),
        "asking for a projection put something back on the stream: {next:?}"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// `cycle` moves an item through the ring the context tab's `space` key moves it through.
///
/// note: the same function, which is the claim worth pinning. The ring and the notes it writes
/// were in `keys.rs`, and the notes are read by the *model* - so a client that picked its own
/// words for the same act would put a second account of it into the context. What this asserts is
/// the order and the fact that the answer carries the new state, not the wording; `App::cycle` is
/// where the wording is, and `tests/screen/context.rs` is where the key is.
#[tokio::test]
async fn an_item_can_be_cycled_through_its_states_from_a_client() {
    let served = served(vec![], |app| {
        app.kernel
            .push(nachalnik::ContextItem::user("what does the kernel do?"));
    })
    .await;
    let (mut peer, first) = Peer::attached(&served.at).await;

    let id = first.items.first().expect("the item was pushed").id;
    assert_eq!(first.items[0].state, nachalnik::ContextState::Active);

    // seen, then a marker where it was, then nothing at all, then seen again
    for expected in [
        nachalnik::ContextState::Elided,
        nachalnik::ContextState::Excluded,
        nachalnik::ContextState::Active,
    ] {
        peer.send(Command::Cycle { id }).await;
        let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
        let Some(Message::Projected(now)) = heard
            .into_iter()
            .find(|m| matches!(m, Message::Projected(_)))
        else {
            unreachable!("the loop above only ends on one");
        };
        assert_eq!(
            now.items[0].state, expected,
            "the ring went somewhere else: {:?}",
            now.items[0]
        );
    }

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// And cycling an item that is not there is refused rather than ignored.
#[tokio::test]
async fn cycling_an_item_that_is_not_there_says_so() {
    let served = served(vec![], |_| {}).await;
    let (mut peer, _) = Peer::attached(&served.at).await;

    peer.send(Command::Cycle { id: ContextId(404) }).await;
    let heard = peer.until(|m| matches!(m, Message::Failed { .. })).await;
    let Some(Message::Failed { about, .. }) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Failed { .. }))
    else {
        unreachable!("the loop above only ends on one");
    };
    assert_eq!(about, "cycle");

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// `rule` changes a row of the permissions tab the way the tab's keys change it, and a row put
/// back to a question leaves the list.
///
/// note: what a phone has no other way to do. The rows came over the wire and the keys that change
/// them did not, so a rule answered `always` from a page could be taken back only at the terminal.
#[tokio::test]
async fn a_rule_can_be_put_back_to_a_question_from_a_client() {
    use kamchatka::tools::Subject;
    use nachalnik::{Event, Verdict};

    let served = served(vec![], |app| {
        app.policy.set(&Subject::parse("fs:read"), Verdict::Allow);
        app.policy.set(&Subject::parse("exec:run"), Verdict::Deny);
    })
    .await;
    let (mut peer, first) = Peer::attached(&served.at).await;
    let row = |rows: &[protocol::Stanced], subject: &str| {
        rows.iter()
            .find(|row| row.subject == subject)
            .map(|row| row.verdict)
    };
    assert_eq!(row(&first.permissions, "fs:read"), Some(Verdict::Allow));

    peer.send(Command::Rule {
        subject: "exec:run".to_owned(),
        verdict: Verdict::Allow,
    })
    .await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = heard.last() else {
        unreachable!("the loop above only ends on one");
    };
    assert_eq!(row(&now.permissions, "exec:run"), Some(Verdict::Allow));

    peer.send(Command::Rule {
        subject: "fs:read".to_owned(),
        verdict: Verdict::Ask,
    })
    .await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = heard.last() else {
        unreachable!("the loop above only ends on one");
    };
    assert_eq!(
        row(&now.permissions, "fs:read"),
        None,
        "a question is not a row: {:?}",
        now.permissions
    );
    assert!(
        heard.iter().any(|m| matches!(
            m,
            Message::Record(record) if matches!(
                &record.event,
                Event::PolicyRuled { subject, verdict: Verdict::Ask, .. } if subject == "fs:read"
            )
        )),
        "and it is in the record, as a rule changed at the terminal is: {heard:?}"
    );

    // and a row that is not there any more is refused rather than ignored
    peer.send(Command::Rule {
        subject: "fs:read".to_owned(),
        verdict: Verdict::Deny,
    })
    .await;
    let heard = peer.until(|m| matches!(m, Message::Failed { .. })).await;
    assert!(
        matches!(heard.last(), Some(Message::Failed { about, .. }) if about == "rule"),
        "{heard:?}"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// An elided item reads as its marker in the conversation, not as the content it still holds.
///
/// note: the bug this is here for was invisible from the terminal, which is the shape of thing a
/// second client finds. The substitution lived in `ui/tabs.rs`, so the screen was right and every
/// client was handed the words of an item whose whole meaning is that the model no longer has
/// them - a page drawing that shows a conversation the model is not having.
///
/// note: asserted on the *marker* rather than on the absence of the content, because absence
/// passes for a hundred wrong reasons - a line dropped, an item skipped, a projection that failed.
/// What is being claimed is that the line is the projector's own words, which is what the model
/// reads there.
#[tokio::test]
async fn an_elided_item_reads_as_its_marker_to_a_client() {
    let served = served(vec![], |app| {
        app.kernel
            .push(nachalnik::ContextItem::user("the secret is hunter2"));
        // a turn that is a thought, a sentence and two calls, so that the item is four lines of
        // conversation rather than one - which is what says the marker stands for the *item*. The
        // thought is first because that is the order a turn happens in, and it is what makes the
        // speaker worth asserting below
        app.kernel.push(nachalnik::ContextItem::new(
            nachalnik::ContextKind::AssistantMessage {
                tool_calls: vec![
                    call("c1", "peek", json!({ "at": "one" })),
                    call("c2", "peek", json!({ "at": "two" })),
                ],
                reasoning: Some("weighing it up".into()),
            },
            "model",
            "assistant",
            "here is what I will do",
        ));
    })
    .await;
    let (mut peer, first) = Peer::attached(&served.at).await;

    let id = first.items[0].id;
    assert!(
        first
            .conversation
            .iter()
            .any(|line| line.text.contains("hunter2")),
        "an active item says what it says: {:?}",
        first.conversation
    );

    // one step of the ring is `elided`
    peer.send(Command::Cycle { id }).await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one");
    };

    assert_eq!(now.items[0].state, nachalnik::ContextState::Elided);
    let marker = now.items[0]
        .marker
        .clone()
        .expect("an elided item is in the request as a marker");
    let line = now
        .conversation
        .iter()
        .find(|line| line.item == Some(id))
        .expect("an elided item keeps its place in the conversation");
    assert_eq!(
        line.text, marker,
        "the conversation showed something other than the marker: {:?}",
        now.conversation
    );

    // and the turn below it, which is three lines, reads as one marker rather than three
    let turn = now.items[1].id;
    peer.send(Command::Cycle { id: turn }).await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one");
    };
    let lines: Vec<_> = now
        .conversation
        .iter()
        .filter(|line| line.item == Some(turn))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "a hidden turn read as one thing per line it used to have: {lines:?}"
    );
    // and in the voice of whoever's turn it was, rather than of whichever of its four lines came
    // first. A marker under `~` says a *thought* was hidden, where what went was a thought, a
    // sentence and two calls - which the terminal dims either way and a client drawing rows draws
    // as what the speaker says it is
    assert_eq!(
        lines[0].speaker,
        Speaker::Model,
        "a hidden turn read as a hidden thought: {lines:?}"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// The trace goes out as the trace tab draws it: this program's words, and the pane's own gaps.
///
/// note: what makes it worth a test rather than a glance is the gap, which is a *decision* about
/// when there is one rather than a format - nothing under a tenth of a second, and nothing after a
/// line that ended a wait for a person. A client left to work that out from timestamps would draw
/// a column whose largest figure is how long the operator spent reading, which is the one number
/// in there nobody should act on.
#[tokio::test]
async fn the_trace_goes_out_as_the_pane_draws_it() {
    let script = vec![ModelResponse::text("a state machine")];
    let served = served(script, |_| {}).await;
    let (mut peer, _) = Peer::attached(&served.at).await;

    peer.send(Command::Submit {
        line: "what does the kernel do?".to_owned(),
    })
    .await;
    peer.until_record("model.finished").await;
    peer.send(Command::Project).await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one");
    };

    // the names are the events', and the detail is this program's account of them - which is the
    // whole reason this is on the wire rather than left to a client and the records
    assert!(
        now.trace.iter().any(|line| line.name == "model.requested"),
        "the trace is not the trace: {:?}",
        now.trace
    );
    assert!(
        now.trace
            .iter()
            .any(|line| !line.name.is_empty() && !line.detail.is_empty()),
        "every line came through without its words: {:?}",
        now.trace
    );
    // and a wall clock that is a real one, since it is the half a reader matches against a log
    assert!(
        now.trace.iter().all(|line| line.at > 1_600_000_000_000),
        "a line arrived with no time on it: {:?}",
        now.trace
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// An endpoint that takes its time answering, so that a command can be caught in flight, and says
/// when a request has reached it.
///
/// note: a closed port will not do. A refused connection comes back at once, and what this needs
/// is a window - the one a `/models` at an endpoint that has gone quiet opens for real.
async fn slow_endpoint(after: Duration) -> (String, Arc<tokio::sync::Notify>) {
    use tokio::io::AsyncReadExt as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    let asked = Arc::new(tokio::sync::Notify::new());
    let told = asked.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let told = told.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = socket.read(&mut buf).await;
                told.notify_one();
                tokio::time::sleep(after).await;
                let body = r#"{"data":[{"id":"a-slow-model"}]}"#;
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \
                             {}\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (format!("http://{at}/v1"), asked)
}

/// The kernel is still heard while a client's command waits on an endpoint.
///
/// note: the loop used to hold the `App` for the length of a command, and stopped reading the
/// kernel's broadcast while it did - and that channel *drops* what nobody took rather than
/// queueing it, which no other channel here does. What this loop reads the stream for is
/// `App::trace`, handed to every client that attaches afterwards, so one `/models` at an endpoint
/// that had gone quiet left everybody who arrived later with a trace full of holes and nothing
/// anywhere saying so. The command is sent out now and the loop goes round while it is out; this
/// holds it to hearing everything meanwhile.
///
/// note: more items than the channel is deep, because the failure is a capacity exceeded rather
/// than an ordering; `Config::event_queue_depth` is 1024. And pushed once the command's request has
/// reached the endpoint, so that they land while it is in flight rather than before the loop has
/// taken it - where a loop that stopped listening would pass.
#[tokio::test]
async fn the_kernel_is_still_heard_while_a_command_waits_on_an_endpoint() {
    use nachalnik::ContextItem;

    let (endpoint, asked) = slow_endpoint(Duration::from_millis(400)).await;
    let session = served_at(None, &endpoint, Vec::new(), |_| {}).await;
    let (mut peer, _) = Peer::attached(&session.at).await;

    peer.send(Command::Submit {
        line: "/models".to_owned(),
    })
    .await;
    tokio::time::timeout(PATIENCE, asked.notified())
        .await
        .expect("the command never reached the endpoint");
    // note: yielding between them, and the test is wrong without it. `Kernel::push` is not async
    // and `#[tokio::test]` is one thread, so a tight loop of them never lets the session's task run
    // at all - and a channel that overflowed because nobody was *scheduled* would fail this whether
    // the loop was listening or not. What is under test is a loop that had stopped listening
    for n in 0..1500 {
        session.kernel.push(ContextItem::user(format!("item {n}")));
        if n % 64 == 0 {
            tokio::task::yield_now().await;
        }
    }

    // the list coming back is what says the command really was in flight for all of that
    peer.until(
        |message| matches!(message, Message::Said { text, .. } if text.contains("a-slow-model")),
    )
    .await;

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, _) = session.ended().await;

    assert!(
        !app.loose
            .iter()
            .any(|entry| entry.text.contains("went by too fast")),
        "the session stopped listening while it waited"
    );
    // and it is not that the events never came: every one of them is in the context the session
    // hands the next client that attaches
    assert_eq!(
        app.kernel.items().len(),
        1500,
        "the session's own view of the context is short"
    );
}

/// A line sent while a command waits on an endpoint is answered at once, and read after it.
///
/// note: the line after `/models` is often the `/model` it was asked for, so it must not be read
/// by a session that has not shown the list yet. And it must still be answered: a connection
/// answers its client's commands in order, so a line held unanswered would hold the client's next
/// command behind it - an interrupt among them.
#[tokio::test]
async fn a_line_sent_while_a_command_is_out_waits_for_it() {
    let (endpoint, asked) = slow_endpoint(Duration::from_millis(400)).await;
    let session = served_at(None, &endpoint, Vec::new(), |_| {}).await;
    let (mut peer, _) = Peer::attached(&session.at).await;

    peer.send(Command::Submit {
        line: "/models".to_owned(),
    })
    .await;
    tokio::time::timeout(PATIENCE, asked.notified())
        .await
        .expect("the command never reached the endpoint");
    peer.send(Command::Submit {
        line: "/note after the list".to_owned(),
    })
    .await;
    let heard = peer
        .until(|message| {
            matches!(
                message,
                Message::Replied {
                    did: Did::Queued,
                    ..
                }
            )
        })
        .await;
    assert!(
        session.kernel.items().is_empty(),
        "the line was read before the list came back: {heard:?}"
    );

    // and once the list is back, the line goes in
    peer.until(
        |message| matches!(message, Message::Said { text, .. } if text.contains("a-slow-model")),
    )
    .await;
    crate::until_session(&session.kernel, |kernel| !kernel.items().is_empty()).await;

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A command coming back after a turn failed does not make the session say it finished.
///
/// note: `Server::run` reports the last turn's failure as its own, and that is the difference
/// between `--serve` leaving on `1` and on `0`. A command's answer rides the same channel as a
/// turn's end and is not a turn ending - a compaction pass coming back says nothing about how the
/// last turn went - so it must leave the failure where it was. Read as one arm short it clears
/// it: a turn that failed, then any command at all, and the session reports a session that
/// finished its work.
///
/// note: a compaction pass rather than `/models`, because a pass is asked of the compactor and
/// needs no endpoint. Both halves here are bookkeeping, and neither request is the thing under
/// test.
#[tokio::test]
async fn a_command_coming_back_does_not_call_a_failed_turn_a_finished_one() {
    let session = served_over_a_socket("returned-after-failure", Vec::new(), |app| {
        app.kernel
            .set_compactor(Some(Arc::new(kamchatka::tools::Shedder {
                threshold: 0.0,
                target: 0.0,
            })));
        app.kernel.push(ContextItem::user("what is in big.rs?"));
    })
    .await;
    let mut socket = Socket::connect(&session.at).await;
    socket.send(crate::attaching(None, None)).await;
    while !matches!(socket.recv().await, Message::Attached(_)) {}

    socket
        .send(Command::Submit {
            line: "go".to_owned(),
        })
        .await;
    // the turn failed at the provider: the refusal goes to the model as an error item, and what
    // the session says about it is the line this waits for
    let mut failed = false;
    for _ in 0..24 {
        if matches!(
            socket.recv().await,
            Message::Said { speaker, .. } if speaker == Speaker::Error
        ) {
            failed = true;
            break;
        }
    }
    assert!(failed, "the turn did not fail, so nothing followed it");

    // and now a command's answer comes back over the same channel. The line saying what the pass
    // found is said by the command coming back - `Outcome::Returned` finishing it - so this is
    // where the loop has been told something is not a turn ending.
    socket
        .send(Command::Submit {
            line: "/compact".to_owned(),
        })
        .await;
    let mut came_back = false;
    for _ in 0..24 {
        if matches!(
            socket.recv().await,
            Message::Said { text, .. } if text.contains("Shedder")
        ) {
            came_back = true;
            break;
        }
    }
    assert!(
        came_back,
        "the command never came back, so nothing followed it"
    );

    socket
        .send(Command::Submit {
            line: "/quit".to_owned(),
        })
        .await;
    let (_, outcome) = session.ended().await;
    assert_eq!(
        outcome,
        Err("the last turn failed".to_owned()),
        "a command coming back called a failed turn a finished one"
    );
}

/// `/stop` is not held behind a command waiting on an endpoint: it is what ends the wait.
///
/// note: found live. Every line was held while a command was out, `/stop` among them, so it
/// stopped a turn only once the listing it was held behind came back.
#[tokio::test]
async fn a_stop_is_not_held_behind_the_command_it_stops() {
    let (endpoint, asked) = slow_endpoint(Duration::from_secs(30)).await;
    let session = served_at(None, &endpoint, Vec::new(), |_| {}).await;
    let (mut peer, _) = Peer::attached(&session.at).await;

    peer.send(Command::Submit {
        line: "/models".to_owned(),
    })
    .await;
    tokio::time::timeout(PATIENCE, asked.notified())
        .await
        .expect("the command never reached the endpoint");
    peer.send(Command::Submit {
        line: "/stop".to_owned(),
    })
    .await;
    peer.until(|message| {
        matches!(message, Message::Said { text, .. } if text.contains("stopped waiting for the list of models"))
    })
    .await;

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A `ctrl+c` at a served session ends it, and the turn it interrupted was really stopped.
///
/// note: a child process, because `ctrl+c` is a *signal* and there is no other way to send one -
/// the same shape as the headless suite's. What is under test is `Server::run`'s own handling of
/// it, which is not the headless driver's: one press interrupts and says so, and the session
/// leaves once whatever was running has stopped, on `130`.
///
/// note: a turn that has to be interrupted rather than waited out, so that "leaves once the turn
/// has stopped" has something to be true about. The provider is an endpoint that has gone quiet,
/// which is a turn a single interrupt ends and a second would leave at once - the case the two
/// stages are for.
#[tokio::test(flavor = "multi_thread")]
async fn a_ctrl_c_at_a_served_session_stops_the_turn_and_ends_it() {
    let endpoint = crate::common::endpoint(vec![
        // a tool call, and then nothing at all: the turn is running and waiting for the model
        format!(
            "data: {}",
            json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
                "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                "function": {"name": "wait", "arguments": "{}"}}]},
                "finish_reason": "tool_calls"}]})
        ),
    ])
    .await;
    let dir = crate::common::scratch("ctrlc");

    let mut host = crate::common::command()
        .args(["--no-record", "-m", "nothing", "--serve", "unix:s.sock"])
        .env("KAMCHATKA_BASE_URL", &endpoint)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the program did not start");
    for _ in 0..100 {
        if dir.join("s.sock").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(dir.join("s.sock").exists(), "nothing ever listened");

    // a client that starts a turn and then goes quiet, so the turn is running when the signal
    // arrives and the press has something to stop
    let mut client = tokio::process::Command::from(crate::common::command())
        .args(["--connect", "unix:s.sock"])
        .current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the client did not start");
    let mut read = BufReader::new(client.stderr.take().expect("a pipe")).lines();
    let mut prose = String::new();
    while !prose.contains("a line is a message") {
        let line = tokio::time::timeout(PATIENCE, read.next_line())
            .await
            .expect("the client said nothing")
            .expect("the client's output stopped")
            .expect("the client ended before it was attached");
        prose.push_str(&line);
        prose.push('\n');
    }
    client
        .stdin
        .as_mut()
        .expect("a pipe")
        .write_all(b"go\n")
        .await
        .expect("the line was not sent");
    // the turn has started and the tool is in it
    let deadline = std::time::Instant::now() + PATIENCE;
    while !prose.contains("wait") && std::time::Instant::now() < deadline {
        let line = read.next_line().await.expect("a line").expect("a line");
        prose.push_str(&line);
        prose.push('\n');
    }

    let sent = std::process::Command::new("kill")
        .args(["-INT", &host.id().to_string()])
        .status()
        .expect("`kill` is on the path");
    assert!(sent.success());

    let status = {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            match host.try_wait().expect("it was spawned") {
                Some(status) => break status,
                None if std::time::Instant::now() > deadline => {
                    let _ = host.kill();
                    panic!("the session did not end on a `ctrl+c`: {prose}");
                }
                None => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    };
    assert_eq!(status.code(), Some(130), "a `ctrl+c` leaves with 130");
    drop(client);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A `ctrl+c` at a served session with nothing running ends it, rather than waiting for a turn.
///
/// note: the press leaves once whatever was running has stopped, and with nothing running that is
/// at once - a session that waited for a turn to stop before leaving would wait for ever.
#[tokio::test(flavor = "multi_thread")]
async fn a_ctrl_c_at_a_quiet_served_session_ends_it() {
    let dir = crate::common::scratch("ctrlc-quiet");
    let mut host = crate::common::command()
        .args(["--no-record", "-m", "nothing", "--serve", "unix:s.sock"])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the program did not start");
    for _ in 0..100 {
        if dir.join("s.sock").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(dir.join("s.sock").exists(), "nothing ever listened");

    let sent = std::process::Command::new("kill")
        .args(["-INT", &host.id().to_string()])
        .status()
        .expect("`kill` is on the path");
    assert!(sent.success());

    let deadline = std::time::Instant::now() + PATIENCE;
    let status = loop {
        match host.try_wait().expect("it was spawned") {
            Some(status) => break status,
            None if std::time::Instant::now() > deadline => {
                let _ = host.kill();
                panic!("a `ctrl+c` with nothing running did not end the session");
            }
            None => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };
    assert_eq!(status.code(), Some(130));
}

/// A piped `--connect` that asks for `/models` stays for the list.
///
/// note: found live. The command is answered the moment it is sent and the list comes back after,
/// and a client whose input has closed leaves on `busy: false` - which the session said while the
/// list was still out, so the client left having written nothing it had asked for.
#[tokio::test]
async fn a_piped_client_stays_for_a_command_that_is_still_out() {
    let (endpoint, _asked) = slow_endpoint(Duration::from_millis(300)).await;
    let session = served_at(None, &endpoint, Vec::new(), |_| {}).await;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&session.at, BufReader::new(&b"/models\n"[..])),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("a-slow-model"), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// An interrupt stops a command waiting on an endpoint, and the line that waited behind it runs.
#[tokio::test]
async fn an_interrupt_stops_a_command_waiting_on_an_endpoint() {
    let (endpoint, asked) = slow_endpoint(Duration::from_secs(30)).await;
    let session = served_at(None, &endpoint, Vec::new(), |_| {}).await;
    let (mut peer, _) = Peer::attached(&session.at).await;

    peer.send(Command::Submit {
        line: "/models".to_owned(),
    })
    .await;
    tokio::time::timeout(PATIENCE, asked.notified())
        .await
        .expect("the command never reached the endpoint");
    peer.send(Command::Submit {
        line: "/note behind the list".to_owned(),
    })
    .await;
    peer.send(Command::Interrupt).await;

    // long before the endpoint would have answered, since `PATIENCE` bounds every wait here. A
    // record goes out ahead of a line said before it, so the two are looked for in either order
    let (mut stopped, mut noted) = (false, false);
    while !(stopped && noted) {
        match peer.recv().await {
            Message::Said { text, .. }
                if text.contains("stopped waiting for the list of models") =>
            {
                stopped = true
            }
            Message::Record(record) if record.event.name() == "context.added" => noted = true,
            _ => {}
        }
    }

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A record too long to send is named, and the session stays attachable.
///
/// note: the failure this is about is a lockout rather than a lost line. `context.replaced` is the
/// one event that carries content, the log drops nothing, and a client resumes by sequence - so
/// one rewritten tool result over `MAX_LINE` was refused by every client, on every attempt, for
/// the rest of the session. Raising the number would have moved the size of the thing that breaks.
///
/// note: thirty-three megabytes, because the limit is thirty-two and nothing smaller exercises it.
/// It is the one expensive test in this suite and the cost is the point: the case only exists at
/// that size.
///
/// note: the big one is what the item *used to* hold rather than what it holds, which is the shape
/// the case actually takes - a rewritten tool result - and the only shape this closes. A context
/// item that is large *now* makes the projection itself oversized, which is a second door to the
/// same room: the projection cannot be skipped, so it is cut down to fit instead - see
/// `refused::a_projection_too_long_to_send_is_cut_down_to_fit`.
#[tokio::test]
async fn a_record_too_long_to_send_is_named_rather_than_locking_everybody_out() {
    use nachalnik::ContextItem;

    let session = served(Vec::new(), |_| {}).await;
    // attached first, because a client that arrives afterwards is handed a projection and the
    // records *after* it - the ones it has to be able to read are the ones written while it is here
    let (mut peer, _) = Peer::attached(&session.at).await;
    let id = session
        .kernel
        .push(ContextItem::user("x".repeat(33 * 1024 * 1024)));
    session
        .kernel
        .replace(id, "and now it is short")
        .expect("the item is there");

    let heard = peer
        .until(|message| matches!(message, Message::Oversized { .. }))
        .await;
    let Some(Message::Oversized { seq, bytes }) = heard.last() else {
        panic!("the record was not named");
    };
    assert!(*bytes > protocol::MAX_LINE, "{bytes} is not over the limit");

    // and the connection is still there, still numbered, and still carrying what came after it:
    // before this the frame closed it and the next attempt came back to the same record
    let seq = *seq;
    peer.send(Command::Submit {
        line: "/note the session is still here".to_owned(),
    })
    .await;
    // waited for rather than asserted on what has already arrived: the record goes out on the
    // stream and the answer on the connection, and which of the two lands first is not this
    // test's business. Nothing coming at all is the failure, and it fails as a timeout
    peer.until(|message| matches!(message, Message::Record(record) if record.seq > seq))
        .await;

    // and the way past the record the protocol points at - asking for the version it replaced - is
    // answered in words rather than with a frame the client cannot read, which closed the
    // connection. Waited for as an answer, so that anything else in its place fails as a timeout
    peer.send(Command::Inspect {
        id,
        raw: true,
        version: Some(1),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Failed { about, .. } if about == "inspect"))
        .await;
    let Some(Message::Failed { error, .. }) = heard.last() else {
        unreachable!("the loop above only ends on one");
    };
    assert!(error.contains("bytes"), "{error}");

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// A session out of file descriptors says so once, rather than once per turn of its loop.
///
/// note: the listener stays readable while a connection it could not take waits in the backlog,
/// so a loop that asked again at once failed again at once - and every failure was a line in the
/// conversation, said to every client and kept in every projection handed out afterwards. A
/// session held at a low limit for a second was tens of thousands of them.
///
/// note: a child process under `ulimit -n`, because the limit is the process's and this suite's own
/// connections are what fill it. Low enough that a burst of idle connections reaches it, and high
/// enough that the host starts at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_out_of_descriptors_says_so_once() {
    let base = crate::common::endpoint(Vec::new()).await;
    let dir = crate::common::scratch("descriptors");
    let socket = dir.join("kamchatka.sock");

    let mut host = tokio::process::Command::new("sh")
        .current_dir(crate::common::nowhere())
        .arg("-c")
        .arg("ulimit -n 64 && exec \"$0\" \"$@\"")
        .arg(crate::common::program())
        .args(["--no-record", "-m", "nothing", "--serve"])
        .arg(format!("unix:{}", socket.display()))
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("the host did not start");
    assert!(
        crate::listening(&socket).await,
        "nothing ever listened at {socket:?}"
    );

    let mut idle = Vec::new();
    for _ in 0..100 {
        idle.push(
            tokio::net::UnixStream::connect(&socket)
                .await
                .expect("the backlog is full"),
        );
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    drop(idle);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut connection = tokio::net::UnixStream::connect(&socket)
        .await
        .expect("nothing was listening");
    let (read, mut write) = connection.split();
    let mut frames = protocol::Frames::new(BufReader::new(read));
    protocol::write(&mut write, &crate::attaching(None, None))
        .await
        .expect("the session stopped listening");
    let attached = loop {
        match tokio::time::timeout(PATIENCE, protocol::read::<Message>(&mut frames))
            .await
            .expect("the attach was never answered")
            .expect("the session said something unreadable")
        {
            Some(Message::Attached(attached)) => break attached,
            Some(_) => {}
            None => panic!("the session closed the connection"),
        }
    };
    let failures = attached
        .conversation
        .iter()
        .filter(|line| line.text.contains("could not connect"))
        .count();
    assert_eq!(
        failures, 1,
        "the failure to take a connection was said {failures} time(s)"
    );

    protocol::write(
        &mut write,
        &Command::Submit {
            line: "/quit".to_owned(),
        },
    )
    .await
    .expect("the session stopped listening");
    tokio::time::timeout(PATIENCE, host.wait())
        .await
        .expect("the session did not end")
        .expect("the host did not finish");
}

/// A served session a signal ended leaves with `128` and the signal, as a headless run does.
///
/// note: it was taken as `/quit` and left with `0`, so a service manager's `SIGTERM` or a closed
/// terminal's `SIGHUP` read to whoever started the host as a session that had finished its work.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_session_a_signal_ended_says_so_in_its_status() {
    for (signal, expected) in [("TERM", 143), ("HUP", 129)] {
        let dir = crate::common::scratch(&format!("served-{signal}"));
        let socket = dir.join("s.sock");
        let host = crate::common::command()
            .args(["--no-record", "-m", "nothing", "--serve"])
            .arg(format!("unix:{}", socket.display()))
            .env("KAMCHATKA_BASE_URL", CLOSED)
            .env("KAMCHATKA_API_KEY", "not-a-key")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the program did not start");
        assert!(
            crate::listening(&socket).await,
            "SIG{signal}: nothing listened"
        );

        let sent = std::process::Command::new("kill")
            .args([&format!("-{signal}"), &host.id().to_string()])
            .status()
            .expect("`kill` is on the path");
        assert!(sent.success());
        let out = host.wait_with_output().expect("it ended");
        assert_eq!(
            out.status.code(),
            Some(expected),
            "SIG{signal}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A served run's parting lines are stdout's, where a headless run's are not.
///
/// note: `finish` decides by whether standard output is carrying the records, because a sentence
/// in the middle of a stream of JSON is the one thing that would make it unparseable. A served run
/// has no record stream on stdout - it has a supervisor's - so the line saying where the session
/// got to belongs beside `serving on` rather than on the standard error of the process driving
/// it. A headless run puts the same line on standard error, and that is what tells the two apart.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_run_leaves_its_parting_lines_on_standard_output() {
    let dir = crate::common::scratch("parting");
    let socket = dir.join("k.sock");
    let host = crate::common::command()
        .args(["--no-record", "-m", "nothing", "--serve"])
        .arg(format!("unix:{}", socket.display()))
        .env("KAMCHATKA_BASE_URL", CLOSED)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the host did not start");

    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(socket.exists(), "nothing ever listened at {socket:?}");

    let quitter = tokio::task::spawn_blocking({
        let socket = socket.clone();
        move || crate::connect(&socket, b"/quit\n")
    })
    .await
    .expect("the client panicked");

    let host = tokio::task::spawn_blocking(move || host.wait_with_output())
        .await
        .expect("the host panicked")
        .expect("the host did not finish");
    assert!(
        quitter.status.success() && host.status.success(),
        "the session did not end: {}{}",
        String::from_utf8_lossy(&host.stdout),
        String::from_utf8_lossy(&host.stderr)
    );

    let out = String::from_utf8_lossy(&host.stdout);
    let err = String::from_utf8_lossy(&host.stderr);
    assert!(
        out.contains("events recorded"),
        "the parting lines did not go to stdout, beside the address: {out}{err}"
    );
    assert!(
        !err.contains("events recorded"),
        "the parting lines went to standard error, which is where a run whose stdout carries the \
         records puts them: {err}"
    );
}

/// The settings a served session was started with are said to the clients that attach to it.
///
/// note: a served run says the file on standard error before anything is wired, and a screen
/// covers that line while it is up. The conversation says it again, and a served session's
/// conversation is what every client that ever attaches is handed - so a client an hour later,
/// reading a projection, finds out where its settings came from without anybody having to leave
/// the line on the desk the session was started at.
///
/// note: the file is in the config directory rather than underfoot, because a file underfoot is
/// only read once somebody at a terminal says so and there is nobody here to say.
#[tokio::test(flavor = "multi_thread")]
async fn a_served_session_tells_its_clients_which_settings_it_read() {
    use crate::connect;

    let config = crate::common::scratch("owned-config");
    let home = config.join("xdg").join("kamchatka");
    std::fs::create_dir_all(&home).expect("a config directory to stand in for a person's");
    std::fs::write(
        home.join("kamchatka.json"),
        r#"{ "model": "a-model-from-ones-own-config" }"#,
    )
    .expect("written");

    let dir = crate::common::scratch("cfg");
    let socket = dir.join("k.sock");
    let host = crate::common::command()
        .args(["--no-record", "-m", "nothing", "--serve"])
        .arg(format!("unix:{}", socket.display()))
        .env("XDG_CONFIG_HOME", config.join("xdg"))
        .env("KAMCHATKA_BASE_URL", CLOSED)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the host did not start");

    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(socket.exists(), "nothing ever listened at {socket:?}");

    // note: nothing typed, because the run is a pipe and a message would be a turn against an
    // endpoint that is not there. What this client is here for is to be told something.
    let reader = tokio::task::spawn_blocking({
        let socket = socket.clone();
        move || connect(&socket, b"")
    })
    .await
    .expect("the client panicked");
    let said = String::from_utf8_lossy(&reader.stderr);
    assert!(
        said.contains("settings read from"),
        "a client of a served session was not told where its settings came from: {said}"
    );

    // the second client ends it, so the host is not left serving after the suite finishes
    tokio::task::spawn_blocking({
        let socket = socket.clone();
        move || connect(&socket, b"/quit\n")
    })
    .await
    .expect("the second client panicked");
    let host = tokio::task::spawn_blocking(move || host.wait_with_output())
        .await
        .expect("the host panicked")
        .expect("the host did not finish");
    assert!(
        host.status.success(),
        "the host did not end: {}{}",
        String::from_utf8_lossy(&host.stdout),
        String::from_utf8_lossy(&host.stderr)
    );
}
