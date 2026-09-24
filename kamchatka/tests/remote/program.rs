//! The binary, serving and connecting, with a real socket between the two.
//!
//! note: spawned rather than wired, because what is under test is the program - the address
//! it prints, the loop it chose, what a `--connect` writes to stdout and what it writes to
//! stderr. None of that is reachable from inside a process that built an `App` for itself.

use std::{sync::Arc, time::Duration};

use kamchatka::{
    app::Speaker,
    remote::protocol::{self, Address, Command, Message},
};
use nachalnik::{
    Capability, ContextId, Grant, ModelResponse,
    test::{ConstTool, call},
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

use crate::{PATIENCE, Peer, quit, served, served_at};

/// The two flags, the socket file, and a whole session driven from one process to another.
///
/// note: the only test here that is about the *program* rather than the library. What it can catch
/// and nothing above it can: a flag wired to the wrong loop, a socket file left behind, a client
/// that tries to reach a model of its own, and the mode decision - a served run has no screen and
/// is not a headless one either, and the version of that decision this replaced announced a pipe
/// nobody had mentioned.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_program_serves_a_socket_and_a_second_one_drives_it() {
    // note: imported here rather than at the top, because `connect` is `#[cfg(unix)]` and so is
    // every test that calls it. At the top it is an unresolved import on Windows
    use crate::connect;

    let base =
        crate::common::endpoint(vec![crate::common::answer("what the other end reads")]).await;
    let dir = crate::common::scratch("served");
    let socket = dir.join("kamchatka.sock");

    let host = std::process::Command::new(crate::common::program())
        .args(["--no-record", "-m", "nothing", "--serve"])
        .arg(format!("unix:{}", socket.display()))
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the host did not start");

    // the socket appears when the session is ready for somebody, and not before
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(socket.exists(), "nothing ever listened at {socket:?}");

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
#[cfg(unix)]
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
    let foreign = |extra: &str, host: &str| -> String {
        let body = json!({ "do": "submit", "line": "/quit" }).to_string();
        let mut stream = std::net::TcpStream::connect(&page).expect("the page is reachable");
        stream.set_read_timeout(Some(PATIENCE)).expect("a timeout");
        write!(
            stream,
            "POST /do?tab=first HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("the request goes out");
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    };
    for (extra, host) in [
        (
            "Content-Type: application/json\r\nOrigin: http://elsewhere.example\r\n",
            page.as_str(),
        ),
        ("Content-Type: text/plain\r\n", page.as_str()),
        ("Content-Type: application/json\r\n", "elsewhere.example"),
    ] {
        let answer = foreign(extra, host);
        assert!(
            answer.starts_with("HTTP/1.1 403"),
            "a request from somewhere else was taken: {extra}{host}\n{answer}"
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
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_served_run_says_the_address_it_got_and_a_client_can_reach_it() {
    let base = crate::common::endpoint(vec![crate::common::answer(
        "reached through a port nobody chose",
    )])
    .await;

    let mut host = tokio::process::Command::new(crate::common::program())
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
/// `#[cfg(unix)]` for the same reason `ctrl_c_stops_a_headless_run_rather_than_killing_it` is.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_at_a_client_stops_the_turn_and_then_detaches() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;

    // stdin is a pipe this test holds open and never writes to, so nothing but the signal can end
    // this client - which is what makes the assertion about the signal
    let mut client = tokio::process::Command::new(crate::common::program())
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

/// `/cleanup` reaches every attached client, not only the one that typed it.
///
/// note: a broadcast rather than an answer, for the reason every other notice is one: the program
/// has one voice, and a session two people are watching does not have half of it cleared. The
/// second client here never sends anything.
#[tokio::test]
async fn clearing_the_notices_is_said_to_everybody() {
    let served = served(vec![], |_| {}).await;
    let (mut one, _) = Peer::attached(&served.at).await;
    let (mut two, _) = Peer::attached(&served.at).await;

    // something for it to take away, said by a command rather than invented: `/seams` answers with
    // a page, and the arrival of a second client is a note in its own right
    one.send(Command::Submit {
        line: "/cleanup".to_owned(),
    })
    .await;

    for (who, peer) in [("the client that asked", &mut one), ("the other", &mut two)] {
        let heard = peer.until(|m| matches!(m, Message::Cleared)).await;
        assert!(
            heard.iter().any(|m| matches!(m, Message::Cleared)),
            "{who} was not told the lines went: {heard:?}"
        );
    }

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

/// The kernel is still heard while one client's command waits on an endpoint.
///
/// note: the loop holds the `App` for the length of a command, because there is one of it and
/// answering anybody needs it. What it used to stop doing as well was reading the kernel's
/// broadcast - and that channel *drops* what nobody took rather than queueing it, which no other
/// channel here does. What this loop reads the stream for is `App::trace`, handed to every client
/// that attaches afterwards, so one `/models` at an endpoint that had gone quiet left everybody
/// who arrived later with a trace full of holes and nothing anywhere saying so.
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

    // the answer coming back is what says the command really was in flight for all of that
    peer.until(|message| matches!(message, Message::Replied { .. }))
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
/// same room and is in `POSTPONED.md`: the projection cannot be skipped, so it wants abridging and
/// that is a decision about what every client is handed.
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
