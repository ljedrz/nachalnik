//! `remote::Client`, driven rather than spoken past.
//!
//! note: the rest of this suite speaks the protocol directly, so that what it claims is about
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

use crate::{PATIENCE, Peer, Trickle, quit, records, served, served_as, wired};

/// The client's own two streams are the ones `--headless` writes.
#[tokio::test]
async fn the_client_writes_the_records_and_the_prose() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;

    // note: `/quit` is not typed until a second connection has watched the turn end, rather than
    // handed in behind the message on one slice of bytes. A command runs the moment it arrives - at
    // a prompt, down a pipe, and here - so a `/quit` queued behind a question ends the session while
    // the answer to it is still being written, and the test would be asserting about a race
    let (mut watch, _) = Peer::attached(&session.at).await;
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"ask something\n")
            .await
            .expect("could not type");
        watch.until_words("an answer to read").await;
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

    let (mut watch, _) = Peer::attached(&session.at).await;
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"ask something\n")
            .await
            .expect("could not type");
        watch.until_words("an answer to read").await;
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
    assert!(records.contains("model.finished"), "{records}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client whose session was replaced under it starts again, rather than retrying a resume that
/// cannot ever be accepted.
///
/// note: the recovery the refusal exists for. Without it, a client that came back to a *different*
/// session at the same address sent the same impossible watermark every time it reconnected, was
/// refused identically for a minute, and gave up on a session that was there and would have had
/// it. The refusal is named `attach` rather than reported as the connection's for exactly this:
/// it is the one failure a client can do something about.
#[tokio::test]
async fn a_resume_the_session_refuses_starts_again_with_nothing() {
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

    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        // the refused resume is the second connection, and the third is the one that works
        while reconnected.recv().await != Some(3) {}
        let _ = feed.write_all(b"").await;
        drop(feed);
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client kept asking for a resume nobody could give it")
    .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    // refused once, rather than once per attempt for a minute
    assert_eq!(
        prose
            .matches("attach: you are resuming a session this is not")
            .count(),
        1,
        "{prose}"
    );
    // and then attached to the session that is there rather than to the one it remembered. The
    // header is what says it attached; the refusal names the new session too, and asserting on the
    // name alone passes without the client having got anywhere
    assert!(
        prose.contains("--- the-one-that-went ·") && prose.contains("--- the-one-that-came-back ·"),
        "the client never attached to the session that replaced the first: {prose}"
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
    // note: until the second wait is announced, rather than for a fixed time. A refused connection
    // is refused at once on Linux and after about two seconds on Windows, which retries the
    // handshake first, so a window that fitted one platform's second attempt missed the other's
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
    let (mut watch, _) = Peer::attached(&session.at).await;
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"go\n").await.expect("could not type");
        watch.until_record("permission.requested").await;
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

/// What the program says for itself reaches a client, because a command that was silent is a verb
/// that does nothing visible.
///
/// note: a refused command is the cheapest line the program says for itself. It used to be a client
/// arriving, which said the same thing for nothing - until arrivals moved to the trace, because a
/// browser reconnecting on a flaky link put one in the conversation every second.
#[tokio::test]
async fn the_program_has_one_voice_and_every_client_hears_it() {
    let refused = "is not a number of tokens";
    let session = served(vec![], |_| {}).await;

    let (mut one, _) = Peer::attached(&session.at).await;
    let (mut two, _) = Peer::attached(&session.at).await;

    // a line said *after* both are attached, which is the only kind either of them can hear: what a
    // client was told before it arrived is in the conversation it was handed
    one.send(Command::Submit {
        line: "/spend nonsense".to_owned(),
    })
    .await;

    // the second client asked for nothing and hears it, which is the whole of the claim
    let heard = two
        .until(|message| matches!(message, Message::Said { text, .. } if text.contains(refused)))
        .await;
    assert!(heard.iter().any(
        |message| matches!(message, Message::Said { speaker, .. } if *speaker == Speaker::Error)
    ));

    // and a client that arrives afterwards reads it once - in the conversation it is handed, rather
    // than there and again underneath. Its subscription starts where its projection was taken, and
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

/// A model change reaches every client, because nothing else would tell them.
///
/// note: the one change to a session that is in no record. `/model` and `/provider` finish inside
/// the `Dialect` the kernel already holds rather than by replacing the kernel's provider, so the
/// slot never changes and `model.changed` is never emitted - a client went on naming the model
/// before it until something happened to make it ask for a fresh projection. A browser's header is
/// where that showed, and a second client would never have found out at all.
///
/// note: driven by replacing the provider rather than by typing `/model`, because the suite's
/// endpoint is a port nothing listens on and a switch is a round trip to it. What is under test is
/// the watch in `Serving::pump`, and what it watches is `Kernel::model_info` - which this moves the
/// honest way.
#[tokio::test]
async fn a_model_change_reaches_every_client() {
    let session = served(vec![], |_| {}).await;
    // two, because the claim is that it is broadcast: the one that would have asked for a
    // projection anyway is not the one this is for
    let (mut one, attached) = Peer::attached(&session.at).await;
    let (mut two, _) = Peer::attached(&session.at).await;
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

    for (who, peer) in [
        ("the client that was here", &mut one),
        ("the other", &mut two),
    ] {
        let heard = peer.until(|m| matches!(m, Message::Model { .. })).await;
        let Some(Message::Model { model }) = heard.last() else {
            unreachable!("the loop above only ends on one")
        };
        assert_eq!(
            model.as_ref().map(|it| it.model.as_str()),
            Some(named.as_str()),
            "{who} was told the wrong model"
        );
    }

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

/// A restart from a client hands the session back to be built again, and lets go of everybody.
///
/// note: two claims, and they are the two halves `main` leans on. The loop returning with
/// `restart` set and `quit` clear is what tells it to wire a second session rather than stop; and
/// every connection ending is what stops a client reading a session nobody is in any more. Neither
/// is visible from the other side - a client sees a socket close, and the loop sees a flag - so
/// this is the one place both are true at once.
///
/// note: the connections are let go of by the `Serving` going away with the loop, which closes the
/// voice every one of them is reading - the same mechanism, and the same last flush, that ends
/// them on `/quit`. It is worth a test rather than an assumption because nothing else would close
/// them: every connection owns a `Kernel` handle, so the session being replaced leaves the old one
/// alive inside the task reading it, and a client would go on being served a session that had been
/// written out and abandoned.
///
/// note: two clients, because one would not catch a parting that reached only whoever spoke last.
/// The second says nothing at all and is let go of on the same terms.
#[tokio::test]
async fn a_restart_from_a_client_ends_the_session_and_lets_go_of_everybody() {
    let session = served(vec![ModelResponse::text("unused")], |_| {}).await;
    let (mut asked, _) = Peer::attached(&session.at).await;
    let (mut watching, _) = Peer::attached(&session.at).await;

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

    for (which, peer) in [
        ("the one that asked", &mut asked),
        ("the other", &mut watching),
    ] {
        // note: drained to the close rather than read once. What the session has to say on the way
        // out goes first, and the connection ending is the message this is about - a `while let`
        // that never ends is the failure, and `Peer::next` is what bounds it
        let mut heard = Vec::new();
        while let Some(message) = peer.next().await {
            heard.push(message);
        }
        assert!(
            heard.iter().any(|message| matches!(
                message,
                Message::Record(record) if record.event.name() == "session.finished"
            )),
            "{which} was cut off without being told the session had ended: {heard:?}"
        );
    }
}
