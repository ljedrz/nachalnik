//! `remote::Client`, driven rather than spoken past.
//!
//! note: most of this suite speaks the protocol directly, so that what it claims is about
//! the *session* rather than about the pair. These are the ones that are about the client:
//! what it writes where, what it does when the socket goes, and what it makes of an answer it
//! cannot read.

use std::sync::Arc;

use kamchatka::{
    app::Speaker,
    remote::{
        Server,
        protocol::{self, Address, Command, Message},
    },
    wiring::Wired,
};
use nachalnik::{
    Capability, Grant, ModelResponse, Provider,
    test::{ConstTool, call},
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

use crate::{
    PATIENCE, Peer, Reacher, Trickle, quit, records, says, served, served_as, until_session, wired,
};

/// The client's own two streams are the ones `--headless` writes.
#[tokio::test]
async fn the_client_writes_the_records_and_the_prose() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;

    // note: `/quit` is not typed until the session has the answer, rather than handed in behind
    // the message on one slice of bytes. A command runs the moment it arrives - at a prompt, and
    // here, where the client is not told to wait for turns as it is down a pipe - so a `/quit`
    // queued behind a question ends the session while the answer to it is still being written,
    // and the test would be asserting about a race
    let kernel = session.kernel.clone();
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"ask something\n")
            .await
            .expect("could not type");
        until_session(&kernel, |kernel| says(kernel, "an answer to read")).await;
        feed.write_all(b"/quit\n").await.expect("could not type");
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");
    let (records, prose) = (
        String::from_utf8(records).expect("the records are text"),
        String::from_utf8(prose).expect("the prose is text"),
    );

    assert!(prose.contains("an answer to read"), "{prose}");
    // and the records really are records, rather than a rendering of them
    for line in records.lines() {
        serde_json::from_str::<nachalnik::Record>(line).expect("every line is a record");
    }
    assert!(records.contains("model.finished"), "{records}");

    session.ended().await.1.expect("the session failed");
}

/// A client that had nothing to do writes the log it was watching, where one that typed a question
/// always did.
///
/// note: what made this a defect is not that the stream was short but that it was *selectively*
/// short. A record reached a client's stdout only as a `Message::Record` crossed the wire, so a
/// client that attached, had nothing to do and left wrote nothing at all - while the same client
/// typing `/budget` wrote the whole log behind it, and `--headless` writes the whole log in both
/// cases. A projection carries the conversation and a count of the records; it does not carry them.
///
/// note: so the client asks for the ones it has not written, on a resume rather than a fresh
/// attach, which is the command the protocol already answers with the records after a watermark.
/// The assertion is about the *first* record, which is the one no message ever carried: the
/// `session.started` the kernel emits while it is still being built, before anybody could attach.
///
/// note: over a socket file, so a machine that cannot bind a loopback port can still run it. A
/// `/note` is typed by a second connection rather than by this one, because the whole of what is
/// under test is a client that types nothing.
#[tokio::test]
async fn a_client_with_nothing_to_do_still_writes_the_log() {
    use crate::{Socket, quit_over_a_socket, served_over_a_socket};

    let session = served_over_a_socket("client-records", Vec::new(), |_| {}).await;

    // a record written by somebody else, so the log this client is watching is not one it made
    let mut watch = Socket::connect(&session.at).await;
    watch.send(crate::attaching(None, None)).await;
    while !matches!(watch.recv().await, Message::Attached(_)) {}
    watch
        .send(Command::Submit {
            line: "/note something to be in the log".to_owned(),
        })
        .await;

    // and a client that types nothing at all
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&session.at, "".as_bytes()),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");
    let records = String::from_utf8(records).expect("the records are text");

    // the conversation is printed as it always was - a note reads in a projection as the item it
    // became, which is the half of this that was never broken
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("note (memory)"), "{prose}");
    // and the log is written out rather than summarised
    assert!(
        !records.is_empty(),
        "a client with nothing to do wrote nothing"
    );
    let events: Vec<_> = records
        .lines()
        .map(|line| {
            serde_json::from_str::<nachalnik::Record>(line).expect("every line is a record")
        })
        .collect();
    // in order, from the beginning: the first is the record that predates any client, and it is
    // the one no message this client was sent could have carried
    assert_eq!(events[0].event.name(), "session.started");
    assert!(
        events
            .iter()
            .any(|record| record.event.name() == "context.added"),
        "{records}"
    );
    // and it is the whole of the log rather than the tail of it
    let last = session.kernel.last_seq();
    assert_eq!(
        events.last().expect("a record").seq,
        last,
        "the client wrote {} of {last} record(s)",
        events.len()
    );

    quit_over_a_socket(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A model that answers in one piece, with no fragment ahead of it.
struct Whole(&'static str);

#[nachalnik::async_trait]
impl Provider for Whole {
    fn info(&self) -> nachalnik::ModelInfo {
        nachalnik::ModelInfo::new("whole", "whole")
    }

    async fn respond(
        &self,
        _: nachalnik::ModelRequest,
        _: nachalnik::DeltaSink,
    ) -> Result<ModelResponse, nachalnik::BoxError> {
        Ok(ModelResponse::text(self.0))
    }
}

/// An answer that arrived whole is printed, once, though not a fragment of it crossed the wire.
#[tokio::test]
async fn an_answer_that_was_not_streamed_is_printed() {
    let session = served(Vec::new(), |app| {
        app.kernel
            .set_provider(Arc::new(Whole("said in one piece")));
    })
    .await;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(&b"ask something\n"[..]))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert_eq!(prose.matches("said in one piece").count(), 1, "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A call is printed with the whole of its arguments, as `--headless` prints it.
///
/// note: it was cut to one line of 96 characters, so a file write read as the first line of the
/// file - while this client says it writes what `--headless` writes, which prints them all, and
/// a script moved from one to the other is the reader that would notice.
#[tokio::test]
async fn a_call_is_printed_whole() {
    let args = json!({ "content": format!("{}\nand a second line", "x".repeat(200)) });
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", args.clone())]),
        ModelResponse::text("done"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(&b"ask something\n"[..]))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(prose.contains(&format!("⟩ peek({args})")), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// Two answers in a row are two lines, as they are down a pipe.
///
/// note: the last answer very likely ended mid-sentence, so the next one used to start on the end
/// of it - and in a client that is worse than it looks, because the break cannot be put where the
/// turn began. A turn whose fragments never crossed the wire is fetched at the end, by which time
/// every request of the batch has been read; what is written is one answer after another, and
/// nothing in between says where one stopped.
#[tokio::test]
async fn two_answers_do_not_run_into_each_other() {
    let session = served(
        vec![
            ModelResponse::text("the first answer"),
            ModelResponse::text("the second answer"),
        ],
        |_| {},
    )
    .await;

    let (mut feed, input) = tokio::io::duplex(256);
    let typed = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt as _;

        feed.write_all(b"ask something\n").await.expect("typed");
        feed.write_all(b"ask again\n").await.expect("typed");
        drop(feed);
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");
    typed.await.expect("the typing panicked");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(
        prose.contains("the first answer\nthe second answer"),
        "one answer ran into the next: {prose}"
    );

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// Down a pipe, a line waits for the turn before it, as it does under `--headless`.
///
/// note: found live. Every line of a script went the moment it was read, so a `/note` after a
/// question was refused for a turn still running, and the `/quit` at the end stopped the turn the
/// script had asked for before the model had said a word.
#[tokio::test]
async fn a_client_down_a_pipe_waits_for_the_turn_before_its_next_line() {
    let session = served(Vec::new(), |app| {
        app.kernel.set_provider(Arc::new(Trickle {
            words: ["every ", "word ", "of ", "it"].map(str::to_owned).to_vec(),
        }));
    })
    .await;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .waits_for_turns()
            .run(
                &session.at,
                "ask something\n/note after the answer\n/quit\n".as_bytes(),
            ),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(prose.contains("every word of it"), "{prose}");
    assert!(!prose.contains("not while a turn"), "{prose}");
    let (app, ended) = session.ended().await;
    ended.expect("the session failed");
    assert!(
        app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("after the answer")),
        "the note never went in"
    );
}

/// A session that is replaced under a client is said to have ended, whichever door the client left
/// by.
///
/// note: a `/restart` ends a session the way a `/quit` does, and a client whose input had already
/// closed detaches the moment the session goes quiet - so the ending was told on a branch this
/// client never reached, and the run finished with nothing said about it and a `0`. The session
/// after a restart is not this client's to carry on into, so the line is the whole of what it can
/// honestly say.
#[tokio::test]
async fn a_restarted_session_is_said_to_have_ended() {
    let session = served(Vec::new(), |_| {}).await;

    // note: the input closes behind the command, which is the shape that hid this: a client with
    // somebody at it waits for the socket, and one with nobody at it leaves as soon as the session
    // is quiet
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(&b"/restart\n"[..]))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(
        prose.contains("the session has ended") && prose.contains("`--connect` again"),
        "a restarted session ended in silence: {prose}"
    );
    assert!(
        String::from_utf8_lossy(&records).contains("session.finished"),
        "the ending was never sent"
    );

    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    assert!(
        app.restart,
        "a `/restart` from a client did not reach the loop"
    );
}

/// A `/note` typed at a client says what went in, as it does down a pipe.
#[tokio::test]
async fn what_a_command_put_in_is_said() {
    let session = served(Vec::new(), |_| {}).await;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(
            &session.at,
            BufReader::new(&b"/note the runner has no network\n"[..]),
        )
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(
        prose.contains("note (memory) went into the context"),
        "{prose}"
    );

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client whose socket is pulled out from under it picks the session back up, and still leaves
/// when it is done.
///
/// note: the headline claim of `remote::client` and the whole reason `GIVE_UP`, `FIRST_WAIT` and
/// `LONGEST_WAIT` are there, and nothing drove it: the two tests that mentioned reconnection both
/// assert that it does *not* happen. What that hid is that a resume was the one command answered
/// with nothing, so a client that survived a blip was owed an answer for ever - and both the
/// things that wait on `Client::resting` stopped working. `printf 'a question\n' | kamchatka
/// --connect` over a flaky link never came back.
///
/// note: a socket of this test's own in front of the session's, because what has to go away is
/// the *connection* and not the session. Restarting the session would be a different test with a
/// different claim, and would make the resume legitimately refusable.
///
/// note: the input is closed only once the client has connected a second time. Closed any earlier
/// it detaches a client that has not yet noticed anything was wrong, which passes without going
/// anywhere near the thing under test.
#[tokio::test]
async fn a_client_that_loses_its_socket_comes_back_and_still_detaches() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;
    let Ok(Address::Tcp(host)) = protocol::address(&session.at) else {
        panic!("the suite serves a port");
    };
    let host = host.to_owned();

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", proxy.local_addr().expect("its own address"));
    let (cut, cut_now) = tokio::sync::oneshot::channel::<()>();
    let (connected, mut reconnected) = tokio::sync::mpsc::unbounded_channel::<u32>();
    tokio::spawn(async move {
        let mut cut_now = Some(cut_now);
        let mut nth = 0;
        while let Ok((mut down, _)) = proxy.accept().await {
            let mut up = TcpStream::connect(&host).await.expect("the session went");
            nth += 1;
            let _ = connected.send(nth);
            match cut_now.take() {
                // the first connection is the one that goes away under the client
                Some(cut_now) => {
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = tokio::io::copy_bidirectional(&mut down, &mut up) => {}
                            _ = cut_now => {}
                        }
                    });
                }
                None => {
                    tokio::spawn(async move {
                        let _ = tokio::io::copy_bidirectional(&mut down, &mut up).await;
                    });
                }
            }
        }
    });

    let kernel = session.kernel.clone();
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"ask something\n")
            .await
            .expect("could not type");
        until_session(&kernel, |kernel| says(kernel, "an answer to read")).await;
        let _ = cut.send(());
        while reconnected.recv().await != Some(2) {}
        drop(feed);
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");
    let (records, prose) = (
        String::from_utf8(records).expect("the records are text"),
        String::from_utf8(prose).expect("the prose is text"),
    );

    assert!(
        prose.contains("the connection went; attaching again from record"),
        "the socket was cut and the client never noticed: {prose}"
    );
    // and it resumed rather than starting again: one header, which is what `Attached` prints
    assert_eq!(
        prose.matches("record(s), ").count(),
        1,
        "a resume was answered with a second projection: {prose}"
    );
    // a resume is told the model whether or not it changed, and one that had not is not news
    assert!(
        !prose.contains("the model is"),
        "a model that had not changed was announced on the way back: {prose}"
    );
    assert!(records.contains("model.finished"), "{records}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client whose session was replaced under it stops, rather than following another in its place.
///
/// note: it used to attach afresh to whatever answered, which carried the record stream on with a
/// second session's records under the first one's - numbers going back to the start with nothing
/// between them to say so. `--connect` stands in for `--headless`, which is one session, so it
/// stops at the first refusal and says how to attach to the one that is there. Stopping at the first
/// matters as much: retrying the same impossible resume spent a minute being refused and then gave
/// up saying the session had not answered.
#[tokio::test]
async fn a_client_whose_session_was_replaced_stops_rather_than_following_another() {
    let before = served_as(Some("the-one-that-went"), vec![], |_| {}).await;
    let after = served_as(Some("the-one-that-came-back"), vec![], |_| {}).await;
    let host = |at: &str| match protocol::address(at) {
        Ok(Address::Tcp(host)) => host.to_owned(),
        _ => panic!("the suite serves a port"),
    };
    let (before_at, after_at) = (host(&before.at), host(&after.at));

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", proxy.local_addr().expect("its own address"));
    let (connected, mut reconnected) = tokio::sync::mpsc::unbounded_channel::<u32>();
    tokio::spawn(async move {
        let mut nth = 0;
        while let Ok((down, _)) = proxy.accept().await {
            nth += 1;
            // the first connection reaches one session and everything after it the other, which is
            // what a host restarted under a client looks like from the client
            let to = match nth {
                1 => &before_at,
                _ => &after_at,
            };
            let up = TcpStream::connect(to).await.expect("the session went");
            let _ = connected.send(nth);
            let (down_r, mut down_w) = down.into_split();
            let (up_r, mut up_w) = up.into_split();
            tokio::spawn(async move {
                let mut lines = BufReader::new(up_r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if down_w
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                    // the first connection carries the projection through and then dies on it, so
                    // that the client is holding a session and a watermark when it goes
                    if nth == 1 && line.contains("\"is\":\"attached\"") {
                        return;
                    }
                }
            });
            tokio::spawn(async move {
                let mut down_r = BufReader::new(down_r);
                let _ = tokio::io::copy(&mut down_r, &mut up_w).await;
            });
        }
    });

    // held open, so that what ends the client is the refusal rather than its input closing
    let (_feed, input) = tokio::io::duplex(256);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    let refused = tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client kept asking for a resume nobody could give it")
    .expect_err("the client took another session for the one it was following");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(
        refused.contains("not `the-one-that-went`") && refused.contains("Run it again"),
        "{refused}"
    );
    // and the sentences in it are sentences: this one was reflowed to the source column and the
    // inter-word spaces came with it, so the single line a script reads when a host restarts
    // underneath it carried two runs of 26. `cargo fmt` cannot see inside a string literal
    assert!(
        !refused.contains("  "),
        "the refusal carried a run of spaces: {refused:?}"
    );
    assert!(
        prose.contains("--- the-one-that-went ·")
            && !prose.contains("--- the-one-that-came-back ·"),
        "the client attached to the session that replaced the first: {prose}"
    );
    let mut connections = Vec::new();
    while let Ok(nth) = reconnected.try_recv() {
        connections.push(nth);
    }
    assert_eq!(
        connections,
        [1, 2],
        "the client connected again after the refusal"
    );

    quit(&before.at).await;
    quit(&after.at).await;
    before.ended().await.1.expect("the first session failed");
    after.ended().await.1.expect("the second session failed");
}

/// A command the socket took with it is an answer that is never coming, and nothing waits for it.
///
/// note: the other half of the reconnection, and the half the test above cannot reach: there the
/// only thing owed an answer was the attach. A client counts answers owed so that a piped-in
/// question is not asked and abandoned - but a `submit` that died in the socket is owed one for
/// ever, and a count that never came down is a `--connect` in a script that hangs after a blip
/// instead of leaving. The session cannot help here, because it never saw the command.
#[tokio::test]
async fn a_client_does_not_wait_for_an_answer_the_dead_socket_took_with_it() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;
    let Ok(Address::Tcp(host)) = protocol::address(&session.at) else {
        panic!("the suite serves a port");
    };
    let host = host.to_owned();

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", proxy.local_addr().expect("its own address"));
    let (connected, mut reconnected) = tokio::sync::mpsc::unbounded_channel::<u32>();
    tokio::spawn(async move {
        let mut nth = 0;
        while let Ok((down, _)) = proxy.accept().await {
            let up = TcpStream::connect(&host).await.expect("the session went");
            nth += 1;
            let _ = connected.send(nth);
            let (down_r, mut down_w) = down.into_split();
            let (mut up_r, mut up_w) = up.into_split();
            let (die, dying) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::io::copy(&mut up_r, &mut down_w) => {}
                    _ = dying => {}
                }
            });
            tokio::spawn(async move {
                let mut lines = BufReader::new(down_r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // an attach goes through; on the first connection the line somebody typed is
                    // taken off the wire and the connection dies holding it
                    if nth == 1 && !line.contains("\"do\":\"attach\"") {
                        let _ = die.send(());

                        return;
                    }
                    if up_w
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });

    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"ask something\n")
            .await
            .expect("could not type");
        while reconnected.recv().await != Some(2) {}
        drop(feed);
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client waited for an answer nobody was going to send")
    .expect("the client failed");

    // the session never heard the line, which is what makes the answer one that cannot arrive
    let (watch, attached) = Peer::attached(&session.at).await;
    assert!(
        !attached
            .conversation
            .iter()
            .any(|line| line.text.contains("ask something")),
        "the proxy handed the command on after all"
    );
    watch.drop_it().await;

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// An answer the socket took with it leaves the question open, and the client asks it again.
///
/// note: the question was asked in a record the client had already seen, so the resume does not
/// send it again and there is no projection to carry it. The proxy swallows the `decide` and cuts
/// the first connection; the input closes once the client is back, so what answers the question
/// is the client's own `on_ask` - which it can only do if it still knows the question is open.
#[tokio::test]
async fn an_answer_the_dead_socket_took_with_it_is_asked_again() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("refused"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;
    let Ok(Address::Tcp(host)) = protocol::address(&session.at) else {
        panic!("the suite serves a port");
    };
    let host = host.to_owned();

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", proxy.local_addr().expect("its own address"));
    let (connected, mut reconnected) = tokio::sync::mpsc::unbounded_channel::<u32>();
    tokio::spawn(async move {
        let mut nth = 0;
        while let Ok((down, _)) = proxy.accept().await {
            let up = TcpStream::connect(&host).await.expect("the session went");
            nth += 1;
            let _ = connected.send(nth);
            let (down_r, mut down_w) = down.into_split();
            let (mut up_r, mut up_w) = up.into_split();
            let (die, dying) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::io::copy(&mut up_r, &mut down_w) => {}
                    _ = dying => {}
                }
            });
            tokio::spawn(async move {
                let mut lines = BufReader::new(down_r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if nth == 1 && line.contains("\"do\":\"decide\"") {
                        let _ = die.send(());

                        return;
                    }
                    if up_w
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });

    let kernel = session.kernel.clone();
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"go\n").await.expect("could not type");
        until_session(&kernel, |kernel| !kernel.pending_permissions().is_empty()).await;
        feed.write_all(b"y\n").await.expect("could not type");
        while reconnected.recv().await != Some(2) {}
        drop(feed);
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(
        prose.contains("the connection went; attaching again from record"),
        "the socket was never cut: {prose}"
    );
    assert!(prose.contains("peek: deny"), "{prose}");
    let (watch, attached) = Peer::attached(&session.at).await;
    assert!(
        attached.asking.is_empty(),
        "the client left a question it had forgotten: {:?}",
        attached.asking
    );
    watch.drop_it().await;

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A session that has gone is waited for longer each time, rather than every quarter of a second.
///
/// note: what the waits growing stands for is `GIVE_UP`, which is a minute and too long to wait
/// for here. A connection the session answered on starts the waits again, and an attempt that
/// could not connect used to count as one: so a client whose session had gone waited the first
/// wait, again and again, and never gave up. The proxy answers one attach and then stops
/// listening, which is a session that went.
#[tokio::test]
async fn a_session_that_went_is_waited_for_longer_each_time() {
    let session = served(Vec::new(), |_| {}).await;
    let Ok(Address::Tcp(host)) = protocol::address(&session.at) else {
        panic!("the suite serves a port");
    };
    let host = host.to_owned();

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", proxy.local_addr().expect("its own address"));
    tokio::spawn(async move {
        let (down, _) = proxy.accept().await.expect("the client came");
        drop(proxy);
        let up = TcpStream::connect(&host).await.expect("the session went");
        let (mut down, mut up) = (BufReader::new(down), BufReader::new(up));
        // the attach one way and the projection the other, and then nothing is there
        let mut line = String::new();
        down.read_line(&mut line).await.expect("an attach");
        up.get_mut()
            .write_all(line.as_bytes())
            .await
            .expect("it goes on");
        line.clear();
        up.read_line(&mut line).await.expect("a projection");
        down.get_mut()
            .write_all(line.as_bytes())
            .await
            .expect("it comes back");
    });

    // the input stays open, so nothing but the session going can be what the client is doing
    let (_feed, input) = tokio::io::duplex(256);
    let heard = Heard::default();
    let (mut records, mut prose) = (Vec::new(), heard.clone());
    let mut client = kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose);
    let client = client.run(&at, BufReader::new(input));
    // note: until the second wait is announced, rather than for a fixed time, which would be a
    // guess at how long a refused connection takes to be refused
    let second = async {
        while heard.text().matches("attaching again").count() < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    tokio::select! {
        left = client => panic!("the client left a session it was waiting for: {left:?}"),
        () = second => {}
        () = tokio::time::sleep(PATIENCE * 4) => {}
    }

    let prose = heard.text();
    let waits: Vec<_> = prose
        .lines()
        .filter(|line| line.contains("attaching again"))
        .collect();
    assert!(
        waits.len() >= 2 && waits[1].ends_with(" in 0.5s"),
        "the client did not wait any longer the second time: {prose}"
    );

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// Prose a test can read while the client is still writing it.
#[derive(Clone, Default)]
struct Heard(Arc<std::sync::Mutex<Vec<u8>>>);

impl Heard {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("not poisoned")).into_owned()
    }
}

impl std::io::Write for Heard {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("not poisoned")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The client answers a question with the same three letters the terminal's panel takes.
#[tokio::test]
async fn the_client_answers_a_question_with_the_keys_the_panel_uses() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("allowed"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    // note: `y` is typed only once the question has actually been asked, because the client reads
    // it as an answer only while one is open - which is exactly how the terminal's panel behaves,
    // and is the thing this test is about. Sent ahead of the question it is a *message* of `y`
    let kernel = session.kernel.clone();
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"go\n").await.expect("could not type");
        until_session(&kernel, |kernel| !kernel.pending_permissions().is_empty()).await;
        feed.write_all(b"y\n").await.expect("could not type");
        // and then the input closes, which detaches rather than ending anybody's session - so this
        // is also where the client's own rule is exercised: it stays for the turn it just let
        // through, and comes back when the session says it has nothing left to do
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(prose.contains("wants to run peek"), "{prose}");
    assert!(prose.contains("peek: allow"), "{prose}");
    // note: the *record* that the tool ran, rather than the words the model said afterwards. Both
    // happened, and only one of them is something this protocol promises to deliver: an answer
    // arrives as fragments, which are in no log, and a client that has just detached is exactly the
    // one they are allowed to have gone past
    assert!(prose.contains("tokens"), "the tool never finished: {prose}");

    // the client left and the session did not, which is the whole invariant
    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client whose input has closed answers a running command that reached for the network with
/// its own `--on-ask`, as it answers the kernel's questions, and waits for the turn it let through.
#[tokio::test]
async fn a_client_with_nobody_at_it_answers_a_running_command_with_on_ask() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "reacher", json!({}))]),
        ModelResponse::text("it went"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(Reacher(app.policy.clone())));
    })
    .await;

    // bounded, because the failure this is about is a client that never answers, and the command
    // it leaves waiting holds the turn and the client with it
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Allow, &mut records, &mut prose)
            .run(&session.at, BufReader::new(&b"go\n"[..])),
    )
    .await
    .expect("the client left the command waiting")
    .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(
        prose.contains("`curl x` is running and has reached"),
        "{prose}"
    );
    assert!(
        prose.contains(
            "nobody is here to answer whether `curl x` may reach the network, so it is \
             answered `allow`"
        ),
        "{prose}"
    );

    quit(&session.at).await;
    let (app, ended) = session.ended().await;
    ended.expect("the session failed");
    assert!(
        app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("answered Some(true)")),
        "the answer reached the command"
    );
}

/// What the program says for itself reaches the client, once, because a command that was silent is
/// a verb that does nothing visible.
///
/// note: a refused command is the cheapest line the program says for itself. It used to be a client
/// arriving, which said the same thing for nothing - until arrivals moved to the trace, because a
/// browser reconnecting on a flaky link put one in the conversation every second.
#[tokio::test]
async fn the_program_has_one_voice_and_the_client_hears_it_once() {
    let refused = "is not a number of tokens";
    let session = served(vec![], |_| {}).await;

    let (mut one, _) = Peer::attached(&session.at).await;

    // a line said *after* it attached, which is the only kind it can hear: what a client was told
    // before it arrived is in the conversation it was handed
    one.send(Command::Submit {
        line: "/spend nonsense".to_owned(),
    })
    .await;

    let heard = one
        .until(|message| matches!(message, Message::Said { text, .. } if text.contains(refused)))
        .await;
    assert!(heard.iter().any(
        |message| matches!(message, Message::Said { speaker, .. } if *speaker == Speaker::Error)
    ));

    // and a client that arrives afterwards, replacing it, reads it once - in the conversation it is
    // handed, rather than there and again underneath. Its subscription starts where its projection was taken, and
    // those are taken together for exactly this reason
    let (mut three, arriving) = Peer::attached(&session.at).await;
    assert!(
        arriving
            .conversation
            .iter()
            .any(|line| line.text.contains(refused)),
        "a line said before it arrived is not in the conversation it was handed"
    );
    three
        .send(Command::Submit {
            line: "/quit".to_owned(),
        })
        .await;
    let ending = three
        .until(|message| matches!(message, Message::Replied { .. }))
        .await;
    assert!(
        !ending
            .iter()
            .any(|message| matches!(message, Message::Said { text, .. } if text.contains(refused))),
        "a line the projection already carried arrived again underneath it: {ending:?}"
    );

    session.ended().await.1.expect("the session failed");
}

/// A model change reaches the client, because nothing else would tell it.
///
/// note: `/model` and `/endpoint` finish inside the `Dialect` the kernel already holds, and before
/// this a client went on naming the model before it until something made it ask for a fresh
/// projection. A browser's header is where that showed, and a client that had not typed the
/// switch itself - one beside a desk that did - would never have found out at all.
///
/// note: driven by replacing the provider rather than by typing `/model`, because the suite's
/// endpoint is a port nothing listens on and a switch is a round trip to it. What is under test is
/// the watch in `Serving::pump`, and what it watches is `Kernel::model_info` - which this moves the
/// honest way.
#[tokio::test]
async fn a_model_change_reaches_the_client() {
    let session = served(vec![], |_| {}).await;
    // a client that did not make the change, because the one that would have asked for a
    // projection anyway is not the one this is for
    let (mut one, attached) = Peer::attached(&session.at).await;
    let before = attached.model.expect("the suite wires a provider").model;

    // the session starts talking to something else, which is a thing that happens to a session.
    // `Trickle` rather than a second `ScriptedProvider`, because those report the same name and a
    // change nothing can see is not one
    let now = Arc::new(Trickle { words: Vec::new() });
    let named = now.info().model.clone();
    assert_ne!(
        named, before,
        "the two providers have to differ to say anything"
    );
    session.kernel.set_provider(now);

    let heard = one.until(|m| matches!(m, Message::Model { .. })).await;
    let Some(Message::Model { model }) = heard.last() else {
        unreachable!("the loop above only ends on one")
    };
    assert_eq!(
        model.as_ref().map(|it| it.model.as_str()),
        Some(named.as_str()),
        "the client was told the wrong model"
    );

    one.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// A loop that is not `Server::run` can serve the same session, which is what lets one be driven
/// from a desk and a phone at once.
///
/// note: `main.rs`'s drawn loop is the other caller and cannot be tested from here - it wants a
/// terminal, and every test of this binary pipes its stdout. What *is* testable is the claim
/// underneath it: that `Serving` is the whole of what a loop needs, so a loop written here can hold
/// the `App` and answer clients with no `Server::run` anywhere. If this compiles and passes, the
/// seam is real; if it needed one private thing more, it would not.
///
/// note: the loop is the shape of the one in `main.rs` rather than a convenience: `pump` before it
/// waits, `arrived` and `attend` for a connection, `asked` and `answer` for a command. What it
/// leaves out is the drawing.
#[tokio::test]
async fn a_loop_of_somebody_elses_can_serve_the_session() {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(vec![ModelResponse::text(
        "an answer from somebody else's loop",
    )]);
    let server = Server::bind("tcp:127.0.0.1:0")
        .await
        .expect("nothing would listen");
    let at = server.address();

    let loop_ = tokio::spawn(async move {
        let mut serving = kamchatka::remote::Serving::new(&app);
        while !app.quit {
            serving.pump(&app);
            tokio::select! {
                arrived = server.arrived() => {
                    if let Ok(arrived) = arrived {
                        serving.attend(&mut app, arrived);
                    }
                }
                Some(ask) = serving.asked() => serving.answer(&mut app, ask).await,
                event = events.recv() => match event {
                    Ok(event) => app.on_event(event),
                    Err(_) => break,
                },
                Some(outcome) = finished.recv() => {
                    while let Ok(event) = events.try_recv() {
                        app.on_event(event);
                    }
                    app.on_outcome(outcome);
                }
            }
        }
        serving.pump(&app);
        app.kernel.finish();
        serving.last(&app).await;

        app
    });

    // and from the outside it is a session like any other: a projection, a turn, and the answer
    let (mut peer, _) = Peer::attached(&at).await;
    peer.send(Command::Submit {
        line: "ask it something".to_owned(),
    })
    .await;
    let heard = peer
        .until_words("an answer from somebody else's loop")
        .await;
    assert!(
        !records(&heard).is_empty(),
        "the records never arrived: {heard:?}"
    );
    // the arrival is a trace line, and a loop of somebody else's gets that for nothing
    peer.send(Command::Project).await;
    let seen = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = seen
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one")
    };
    assert!(
        now.trace.iter().any(|line| line.name == "client.attached"),
        "{:?}",
        now.trace
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let app = tokio::time::timeout(PATIENCE, loop_)
        .await
        .expect("the loop did not end")
        .expect("the loop panicked");
    assert!(app.quit, "a `/quit` from a client did not reach the loop");
}

/// A restart from a client hands the session back to be built again, and lets go of the client.
///
/// note: two claims, and they are the two halves `main` leans on. The loop returning with
/// `restart` set and `quit` clear is what tells it to wire a second session rather than stop; and
/// the connection ending is what stops a client reading a session nobody is in any more. Neither
/// is visible from the other side - a client sees a socket close, and the loop sees a flag - so
/// this is the one place both are true at once.
///
/// note: the connections are let go of by the `Serving` going away with the loop, which closes the
/// voice every one of them is reading - the same mechanism, and the same last flush, that ends
/// them on `/quit`. It is worth a test rather than an assumption because nothing else would close
/// them: every connection owns a `Kernel` handle, so the session being replaced leaves the old one
/// alive inside the task reading it, and a client would go on being served a session that had been
/// written out and abandoned.
#[tokio::test]
async fn a_restart_from_a_client_ends_the_session_and_lets_go_of_it() {
    let session = served(vec![ModelResponse::text("unused")], |_| {}).await;
    let (mut asked, _) = Peer::attached(&session.at).await;

    asked
        .send(Command::Submit {
            line: "/restart".to_owned(),
        })
        .await;

    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    assert!(
        app.restart,
        "a `/restart` from a client did not reach the loop"
    );
    assert!(app.leaving(), "the loop was told to let go of the session");
    assert!(
        !app.quit,
        "a restart is not a quit: `main` wires another session rather than stopping"
    );

    // note: drained to the close rather than read once. What the session has to say on the way
    // out goes first, and the connection ending is the message this is about - a `while let` that
    // never ends is the failure, and `Peer::next` is what bounds it
    let mut heard = Vec::new();
    while let Some(message) = asked.next().await {
        heard.push(message);
    }
    assert!(
        heard.iter().any(|message| matches!(
            message,
            Message::Record(record) if record.event.name() == "session.finished"
        )),
        "the client was cut off without being told the session had ended: {heard:?}"
    );
}

/// A client another has replaced leaves saying so, rather than coming back for the session.
///
/// note: the input is held open throughout, so nothing but the session can end this client - and
/// a client that read being replaced as a drop would reconnect, replace the one that replaced it,
/// and be in the session still when the timeout below goes off.
#[tokio::test]
async fn a_replaced_client_leaves_and_does_not_come_back() {
    let session = served(Vec::new(), |_| {}).await;
    let (mut typing, input) = tokio::io::duplex(256);
    let (at, kernel) = (session.at.clone(), session.kernel.clone());
    let replacing = tokio::spawn(async move {
        // a note the client wrote, which is how the session says it has a client to replace
        typing
            .write_all(b"/note here first\n")
            .await
            .expect("could not type");
        until_session(&kernel, |kernel| says(kernel, "here first")).await;
        let (peer, _) = Peer::attached(&at).await;
        (typing, peer)
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    let left = tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&session.at, BufReader::new(input)),
    )
    .await
    .expect("the replaced client stayed");
    let left = left.expect_err("being replaced is not a clean exit");
    assert!(left.starts_with("replaced:"), "{left}");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        !prose.contains("attaching again"),
        "the replaced client went back for the session: {prose}"
    );

    let (typing, peer) = replacing.await.expect("the replacing client panicked");
    drop((typing, peer));
    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client whose turn failed leaves with a failure, as `--headless` does.
///
/// note: it left with `0` whatever happened, so a script piping a question into a session whose
/// model could not be reached read a success. The script here has nothing in it, which is a
/// provider failing to answer.
#[tokio::test]
async fn a_turn_that_failed_is_a_client_that_failed() {
    let session = served(vec![], |_| {}).await;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    let left = tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&session.at, BufReader::new(&b"ask something\n"[..])),
    )
    .await
    .expect("the client never left");
    assert_eq!(left, Err("the last turn failed".to_owned()));

    quit(&session.at).await;
    // and the session's own outcome says the same from the other end
    assert!(session.ended().await.1.is_err());
}

/// A client whose input closes after a command writes the records that command made before it
/// leaves.
///
/// note: a command's reply carries `busy`, and a client with its input closed leaves on
/// `busy: false`. The reply was written the moment the session answered, ahead of the records the
/// command had just emitted - so `/note` piped in was answered, the client left, and the
/// `context.added` of the note it had written was nowhere in what it wrote out.
#[tokio::test]
async fn a_command_piped_in_leaves_its_records_behind_before_the_client_goes() {
    let session = served(Vec::new(), |_| {}).await;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(&b"/note keep this\n"[..]))
        .await
        .expect("the client failed");
    let records = String::from_utf8(records).expect("the records are text");

    assert!(
        records
            .lines()
            .any(|line| line.contains("context.added") && line.contains("note")),
        "{records}"
    );

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A blank line is nothing and spaces round a line are not part of it, as they are headless.
///
/// note: the client sent each line with only its end trimmed, so a blank one was a message - a
/// request for nothing, answered - and `  /help` was a message here and a command down
/// `--headless`, which this client stands in for.
#[tokio::test]
async fn a_blank_line_from_a_client_is_not_a_message() {
    let session = served(vec![ModelResponse::text("one")], |_| {}).await;

    let kernel = session.kernel.clone();
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"\n   \n  /budget\nfirst\n")
            .await
            .expect("could not type");
        until_session(&kernel, |kernel| says(kernel, "one")).await;
        feed.write_all(b"/quit\n").await.expect("could not type");
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");

    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    let asked: Vec<String> = app
        .kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, nachalnik::ContextKind::UserMessage))
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(asked, vec!["first".to_owned()], "a blank line was sent");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        prose.contains("the next request"),
        "`  /budget` is a command: {prose}"
    );
}

/// A blank line from a client that sends one anyway is refused by the session.
///
/// note: `--connect` and `--headless` drop a blank line, and the session trusted every client to:
/// one sent over the wire by anybody else went into the context as an empty message and started a
/// turn on it.
#[tokio::test]
async fn a_blank_line_sent_anyway_is_refused_by_the_session() {
    let session = served(vec![ModelResponse::text("unused")], |_| {}).await;
    let (mut peer, _) = Peer::attached(&session.at).await;

    for line in ["", "   "] {
        peer.send(Command::Submit {
            line: line.to_owned(),
        })
        .await;
        let heard = peer
            .until(|message| {
                matches!(
                    message,
                    Message::Failed { .. } | Message::Replied { .. } | Message::Done { .. }
                )
            })
            .await;
        assert!(
            matches!(heard.last(), Some(Message::Failed { about, .. }) if about == "submit"),
            "{line:?} was taken: {heard:?}"
        );
    }

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, ended) = session.ended().await;
    ended.expect("the session failed");
    assert!(app.kernel.items().is_empty(), "{:?}", app.kernel.items());
}

/// Two answers typed in one breath answer two questions, rather than the first one twice.
#[tokio::test]
async fn two_answers_typed_together_answer_two_questions() {
    let script = vec![
        ModelResponse::tool_calls(vec![
            call("c1", "peek", json!({})),
            call("c2", "poke", json!({})),
        ]),
        ModelResponse::text("allowed"),
    ];
    let session = served(script, |app| {
        for name in ["peek", "poke"] {
            app.kernel.add_tool(Arc::new(
                ConstTool::new(name, "the answer").with_capabilities([Capability::fs("read")]),
            ));
        }
    })
    .await;

    let kernel = session.kernel.clone();
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"go\n").await.expect("could not type");
        until_session(&kernel, |kernel| kernel.pending_permissions().len() == 2).await;
        feed.write_all(b"y\ny\n").await.expect("could not type");
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(prose.contains("peek: allow"), "{prose}");
    assert!(prose.contains("poke: allow"), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// And a line that does not answer a waiting question lets `--on-ask` answer it, and waits.
///
/// note: found live, the moment the pacing above was in. A question opens the input so that a
/// script's `y` can answer it, and the line read was a `/note`: sent into a paused turn it was
/// refused, and every line after it went too, `/quit` among them, so the session ended with the
/// question unanswered. `--headless` answers with `--on-ask` and reads on once the turn is over.
#[tokio::test]
async fn a_line_that_is_not_an_answer_waits_for_the_question_to_be_answered() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("went on without it"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .waits_for_turns()
            .run(
                &session.at,
                "go\n/note after the question\n/quit\n".as_bytes(),
            ),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(prose.contains("so it is answered `deny`"), "{prose}");
    assert!(prose.contains("went on without it"), "{prose}");
    assert!(!prose.contains("not while a turn"), "{prose}");
    let (app, ended) = session.ended().await;
    ended.expect("the session failed");
    assert!(
        app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("after the question")),
        "the note never went in"
    );
}

/// A connection that goes is not a session that said something unreadable.
///
/// note: `ECONNRESET` is the connection stopping, and nothing was in it - the sentence it used to
/// be given sent whoever read it looking for a message the session never wrote, and named a
/// session that was there and talking the whole time. What the line says now is what the
/// connection did, which is the whole of what happened; a frame that is not one is a different
/// fault and still says so, so the two are not merged either.
#[tokio::test]
async fn a_connection_that_stops_reads_as_a_connection_and_not_as_a_message() {
    // note: a listener that writes half a frame and hangs up, which is the ordinary way a
    // connection stops - and the same branch a reset takes, so the claim covers both
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", listener.local_addr().expect("its own address"));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt as _;

                let _ = stream.write_all(b"{\"is\":\"rec").await;
                let _ = stream.shutdown().await;
            });
        }
    });

    // note: the input is held open, so the retry loop carries on after the drop rather than the
    // client leaving on the first one; what this reads is the line under the first attempt
    let (_feed, input) = tokio::io::duplex(256);
    let heard = Heard::default();
    let (mut records, mut prose) = (Vec::new(), heard.clone());
    let mut client = kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose);
    let run = client.run(&at, BufReader::new(input));
    tokio::select! {
        _ = run => {}
        () = async {
            while heard.text().matches("attaching again").count() < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        } => {}
        () = tokio::time::sleep(PATIENCE) => panic!("the client never noticed the connection went"),
    }
    let prose = heard.text();

    assert!(
        !prose.contains("unreadable"),
        "a connection that stopped was called a message nobody could read: {prose}"
    );
    assert!(
        prose.contains("the connection stopped"),
        "the connection going was not said: {prose}"
    );
}

/// A session that said it was finished and then reset the connection has ended, and is not looked
/// for again.
///
/// note: a Unix socket closed with something still unread in it reads at the other end as a reset,
/// after whatever was written before it. A session ending under a client that had just written - a
/// `/quit` followed by the input closing is enough - closes that way, and the client that typed
/// `/quit` spent a minute trying to reattach to a session that had told it it was over. The
/// listener here does that on purpose: it waits for the attach to arrive, leaves it unread, says
/// the session is finished and hangs up.
#[tokio::test]
async fn a_reset_after_the_session_finished_is_the_end() {
    let dir = crate::common::scratch("reset-after-finished");
    let socket = dir.join("kamchatka.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("a socket");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = stream.readable().await;
                let record: nachalnik::Record = serde_json::from_value(
                    json!({ "seq": 1, "at": 0, "event": { "event": "session.finished" } }),
                )
                .expect("a record");
                let _ = protocol::write(&mut stream, &Message::Record(record)).await;
            });
        }
    });

    let (_feed, input) = tokio::io::duplex(256);
    let heard = Heard::default();
    let (mut records, mut prose) = (Vec::new(), heard.clone());
    let mut client = kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose);
    let at = format!("unix:{}", socket.display());
    let run = client.run(&at, BufReader::new(input));
    let ran = tokio::time::timeout(PATIENCE, run).await;
    let prose = heard.text();

    assert!(
        ran.is_ok(),
        "the client looked for a session that had ended: {prose}"
    );
    assert!(!prose.contains("attaching again"), "{prose}");
    assert!(!prose.contains("stopped talking"), "{prose}");
    assert!(prose.contains("the session has ended"), "{prose}");
}

/// A line the projection cut says so, and says where the rest is - `?N` where an answer can carry
/// it, and a snapshot where none can - and `?N` then brings back the whole of it.
///
/// note: the two cases are the two a cut line can be in. A message of twenty megabytes fits in an
/// answer of its own and is only cut because the projection carries it beside another; a message
/// past the limit on its own is refused by any `inspect`, and a client pointing at `?N` for it
/// would be sending somebody round to be told no - which is what this said, until it was asked.
/// The second is the limit exactly, which is the edge: as an answer it is quoted, and over.
#[tokio::test]
async fn a_cut_line_says_where_the_rest_of_it_is() {
    use crate::served_over_a_socket;
    use nachalnik::ContextItem;

    const FITS: usize = 20 * 1024 * 1024;
    let session = served_over_a_socket("clipped-prose", Vec::new(), |app| {
        app.kernel.push(ContextItem::user("a".repeat(FITS)));
        app.kernel
            .push(ContextItem::user("b".repeat(protocol::MAX_LINE)));
    })
    .await;
    let ids: Vec<_> = session.kernel.items().iter().map(|item| item.id).collect();
    let [fits, too_long] = ids[..] else {
        panic!("the session is not the two messages: {ids:?}");
    };
    let typed = format!("?{fits}\n/quit\n");
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(typed.as_bytes()))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");
    let line = |starting: &str| {
        prose
            .lines()
            .find(|line| line.starts_with(starting))
            .unwrap_or_else(|| panic!("no line starts `{starting}`"))
            .to_owned()
    };

    // both are cut, because together they are the problem, and each says by how much
    let a = line("> aaa");
    let b = line("> bbb");
    for cut in [&a, &b] {
        let tail = &cut[cut.len().saturating_sub(200)..];
        assert!(tail.contains("not sent"), "{tail}");
    }
    let a_tail = &a[a.len() - 200..];
    assert!(
        a_tail.contains(&format!("`?{fits}` asks for the whole of it")),
        "{a_tail}"
    );
    // and the one no answer carries is not pointed at `?N`, but at what does hold it
    let b_tail = &b[b.len() - 200..];
    assert!(
        b_tail.contains("more than any one answer carries"),
        "{b_tail}"
    );
    assert!(b_tail.contains("`/save`"), "{b_tail}");
    assert!(!b_tail.contains(&format!("?{too_long}")), "{b_tail}");

    // and `?N` is as good as its word: the whole of the first comes back, not the start of it
    assert!(
        prose.lines().any(|line| line.contains(&"a".repeat(FITS))),
        "`?{fits}` did not bring the whole message back"
    );

    session.ended().await.1.expect("the session failed");
}
