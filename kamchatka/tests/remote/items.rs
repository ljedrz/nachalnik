//! An item of the context, edited from somewhere that is not the terminal.
//!
//! note: the same act as `e` on the context tab, through the protocol - so what is checked is
//! that it keeps the item's number and its state, says whose hand it was, refuses what a
//! person's `e` refuses, and can be read back at either version afterwards.

use kamchatka::remote::protocol::{Command, Message};

use crate::{Peer, served};

/// An item edited from a client is the item, and says whose hand it was.
///
/// note: `Kernel::replace` rather than a new item, which is the whole of why this is one command
/// and not a `/exclude` and a fresh message: the identifier, the kind, the state and the place in
/// the conversation are all the same afterwards, and what it used to say is a version page. The
/// terminal's `e` has worked this way for a while; what this pins is that the wire reaches the
/// same operation rather than a second one written beside it.
#[tokio::test]
async fn an_item_edited_from_a_client_keeps_its_place_and_says_who_edited_it() {
    let session = served(vec![], |app| {
        app.kernel
            .push(nachalnik::ContextItem::user("what it said before"));
    })
    .await;

    let (mut peer, attached) = Peer::attached(&session.at).await;
    let id = attached.items[0].id;
    assert_eq!(attached.items[0].beyond, None, "this one can be edited");

    peer.send(Command::Revise {
        id,
        text: "what it says now".to_owned(),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Projected(..)))
        .await;
    let Some(Message::Projected(projected)) = heard.last() else {
        unreachable!("just matched")
    };
    // the same row, not a second one: an edit is not a way to grow the context
    assert_eq!(projected.items.len(), 1);
    assert_eq!(projected.items[0].id, id);

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    let item = app.kernel.item(id).expect("the item is still there");
    assert_eq!(item.content.to_text(), "what it says now");
    // a person's hand, and never the `context` tool's - a model reading its own metadata should
    // not find its own tool credited with a sentence somebody else wrote
    assert_eq!(item.meta["revised"]["by"], "user");
}

/// An edit that changes nothing is answered, and writes nothing.
///
/// note: one operation is one undo, and an operation that changes nothing takes no checkpoint -
/// so the thing that must not happen here is a `context.replaced` in the log for a box somebody
/// opened, read and closed. A browser commits when the box is let go of, which is a gesture
/// somebody makes without having typed a thing.
#[tokio::test]
async fn an_edit_that_changes_nothing_is_not_an_edit() {
    let session = served(vec![], |app| {
        app.kernel.push(nachalnik::ContextItem::user("unchanged"));
    })
    .await;

    let (mut peer, attached) = Peer::attached(&session.at).await;
    let (id, seq) = (attached.items[0].id, attached.seq);
    peer.send(Command::Revise {
        id,
        text: "unchanged".to_owned(),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Done { .. } | Message::Projected(..)))
        .await;
    let Some(Message::Done { about, .. }) = heard.last() else {
        panic!("an edit that changed nothing should not answer with a projection");
    };
    assert_eq!(about, "revise");

    // asked for rather than read off the session afterwards, because leaving is itself recorded -
    // the log this is about is the one as it stands now, with the edit behind it and the `/quit`
    // still to come
    peer.send(Command::Project).await;
    let heard = peer
        .until(|message| matches!(message, Message::Projected(..)))
        .await;
    let Some(Message::Projected(projected)) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(projected.seq, seq, "nothing should have been recorded");

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// The three shapes an edit cannot reach say so on the row, and refuse it if asked anyway.
///
/// note: both halves, because either alone is the wrong answer. A row that did not say would let
/// somebody type a paragraph into a picture and find out at the end; a session that only said,
/// and took the edit when it came, would write `[image/png, 12.05kB]` over the picture itself.
#[tokio::test]
async fn an_item_no_edit_can_reach_says_so_and_refuses() {
    let session = served(vec![], |app| {
        app.kernel
            .push(nachalnik::ContextItem::user(nachalnik::Content::blob(
                "image/png",
                "A".repeat(64),
            )));
    })
    .await;

    let (mut peer, attached) = Peer::attached(&session.at).await;
    let id = attached.items[0].id;
    let why = attached.items[0]
        .beyond
        .as_deref()
        .expect("a picture cannot be edited, and the row should say so");
    assert!(why.contains("picture"), "{why}");

    peer.send(Command::Revise {
        id,
        text: "a sentence over a picture".to_owned(),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Failed { .. }))
        .await;
    let Some(Message::Failed { about, error }) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(about, "revise");
    assert!(error.contains("picture"), "{error}");

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    assert!(
        app.kernel
            .item(id)
            .expect("the item is still there")
            .content
            .as_blob()
            .is_some(),
        "the picture was written over"
    );
}

/// The reading of an item and the text of it are two answers, and only one of them is an edit.
///
/// note: what this is about went wrong in a browser before it was written down. A row's body is
/// the box somebody types into, and it was being filled with `text::stored` - the content with
/// why the item is here above it - so letting go of the box committed `it is here because: named
/// on the command line` *into* the item it was describing. The reading is for reading.
#[tokio::test]
async fn the_reading_of_an_item_is_not_the_text_an_edit_is_made_of() {
    let session = served(vec![], |app| {
        app.kernel.push(
            nachalnik::ContextItem::user("the text and nothing else")
                .because("a reason that is not part of what it says"),
        );
    })
    .await;

    let (mut peer, attached) = Peer::attached(&session.at).await;
    let id = attached.items[0].id;

    peer.send(Command::Inspect {
        id,
        raw: false,
        version: None,
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Item { .. }))
        .await;
    let Some(Message::Item { body, raw, .. }) = heard.last() else {
        unreachable!("just matched")
    };
    assert!(!raw);
    assert!(
        body.contains("a reason that is not part of what it says"),
        "{body}"
    );

    peer.send(Command::Inspect {
        id,
        raw: true,
        version: None,
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Item { raw: true, .. }))
        .await;
    let Some(Message::Item { body, .. }) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(body, "the text and nothing else");

    // and committing what the raw answer gave back changes nothing, which is the property that
    // makes a box safe to let go of: a client that round-trips is not an edit
    peer.send(Command::Revise {
        id,
        text: body.clone(),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Done { .. } | Message::Projected(..)))
        .await;
    assert!(
        matches!(heard.last(), Some(Message::Done { about, .. }) if about == "revise"),
        "a round trip should not be an edit"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    assert_eq!(
        app.kernel.item(id).expect("still there").content.to_text(),
        "the text and nothing else"
    );
}

/// An item that has been rewritten says how many versions of it there are, and hands them back.
///
/// note: the count is what a client draws a control from, so it has to mean the same thing as the
/// terminal's strip of faces - `v1` is the oldest kept, and what the item says now is one past
/// the last and has no number of its own, because editing moves it.
#[tokio::test]
async fn an_item_rewritten_twice_can_be_read_back_at_either_version() {
    let session = served(vec![], |app| {
        app.kernel
            .push(nachalnik::ContextItem::user("the first thing"));
    })
    .await;

    let (mut peer, attached) = Peer::attached(&session.at).await;
    let id = attached.items[0].id;
    assert_eq!(
        attached.items[0].versions, 0,
        "nothing has rewritten it yet"
    );

    for text in ["the second thing", "the third thing"] {
        peer.send(Command::Revise {
            id,
            text: text.to_owned(),
        })
        .await;
        peer.until(|message| matches!(message, Message::Projected(..)))
            .await;
    }

    peer.send(Command::Project).await;
    let heard = peer
        .until(|message| matches!(message, Message::Projected(..)))
        .await;
    let Some(Message::Projected(now)) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(
        now.items[0].versions, 2,
        "two rewrites, two earlier versions"
    );

    // each of them, by the number the row counts to
    for (at, expected) in [(1, "the first thing"), (2, "the second thing")] {
        peer.send(Command::Inspect {
            id,
            raw: true,
            version: Some(at),
        })
        .await;
        let heard = peer
            .until(|message| {
                matches!(
                    message,
                    Message::Item {
                        version: Some(_),
                        ..
                    }
                )
            })
            .await;
        let Some(Message::Item { body, version, .. }) = heard.last() else {
            unreachable!("just matched")
        };
        assert_eq!(*version, Some(at));
        assert_eq!(body, expected);
    }

    // and what it says now, which is the one with no number
    peer.send(Command::Inspect {
        id,
        raw: true,
        version: None,
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Item { version: None, .. }))
        .await;
    let Some(Message::Item { body, .. }) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(body, "the third thing");

    // note: and an undo is why the count is not simply how many are kept. Putting an old content
    // back makes the newest remembered version the current one as well, and a client offering
    // both would be offering the same words twice under two labels
    assert!(
        session.kernel.undo().unwrap(),
        "there was something to undo"
    );
    peer.until_record("context.undone").await;
    peer.send(Command::Project).await;
    let heard = peer
        .until(|message| matches!(message, Message::Projected(..)))
        .await;
    let Some(Message::Projected(undone)) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(
        undone.items[0].versions, 1,
        "the version an undo restored is the current one, and is not also an earlier one"
    );

    // so the one it stopped counting is refused, even though it is still kept
    peer.send(Command::Inspect {
        id,
        raw: true,
        version: Some(2),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Failed { .. }))
        .await;
    let Some(Message::Failed { error, .. }) = heard.last() else {
        unreachable!("just matched")
    };
    assert!(error.contains("1 earlier version"), "{error}");

    // a version that was never there is refused rather than answered with the nearest one
    peer.send(Command::Inspect {
        id,
        raw: true,
        version: Some(9),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Failed { .. }))
        .await;
    let Some(Message::Failed { about, error }) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(about, "inspect");
    assert!(error.contains("1 earlier version"), "{error}");

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// A restart on the *drawn* loop lets go of its clients too, and starts a session without them.
///
/// note: a pseudo-terminal, because nothing else picks that loop. `headless` is `asked || piped ||
/// no screen in the build`, so a served run whose stdout is a pipe is `Server::run` and a served
/// run on a terminal is `drawn` with a socket beside it. Two loops, each with a `Serving` of its
/// own, and the one every other test in this file reaches is the first. `script(1)` is a pty and
/// one process, and it is in the base install of the platform this is gated to.
///
/// note: the pair with `a_restart_from_a_client_ends_the_session_and_lets_go_of_everybody`, which
/// makes the same claim about the other loop and can make it in-process. What cannot be shared is
/// the reaching: `drawn` is in `main.rs`, so this one is about the program or it is about nothing.
///
/// note: linux only, for `script`'s flags - macOS spells it `script -q /dev/null cmd` and windows
/// has no such thing. The claim is about a loop rather than a platform, and it is the same loop
/// everywhere.
///
/// note: and `tui`, which is the third of the disjuncts above. A screenless build has no `drawn`
/// to reach, so the pty lands on `Server::run` and the guard for exactly that fires - a test
/// about a loop that is not in the build, failing to find it. CI builds this crate twice without
/// a screen, and the rest of this file runs in both.
#[cfg(all(target_os = "linux", feature = "tui"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_on_the_drawn_loop_lets_go_of_its_clients_too() {
    // note: imported here rather than at the top, because this is the one test in the file behind
    // a `cfg` and the three are only ever its. At the top they are an unused import in the two
    // screenless builds CI checks, which is a failure there and nothing at all under
    // `--all-features`
    use std::{io::Write as _, time::Duration};

    use crate::{CLOSED, connect};

    let dir = crate::common::scratch("drawn-restart");
    let socket = dir.join("kamchatka.sock");
    let records = dir.join("records");
    std::fs::create_dir_all(&records).expect("a directory to record into");

    // note: no `-m`, so a message is put in the context and nothing is sent. What this is about is
    // which session a line is in, and a turn against an endpoint that is not there would be the
    // run failing about something else
    let mut host = std::process::Command::new("script")
        .arg("-qec")
        .arg(format!(
            "{} --serve unix:{}",
            crate::common::program().display(),
            socket.display()
        ))
        .arg("/dev/null")
        .env("TMPDIR", &records)
        .env("KAMCHATKA_BASE_URL", CLOSED)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the host did not start");

    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(socket.exists(), "nothing ever listened at {socket:?}");

    // note: the stdin of this one is held open on purpose, and it is the whole of how the claim is
    // made. A client that closed its input would leave of its own accord - which is what every
    // other client in this file does, and it proves nothing about who let go of whom. This one
    // says its piece and then waits, so the only thing that can end it is the host
    let mut asked = std::process::Command::new(crate::common::program())
        .arg("--connect")
        .arg(format!("unix:{}", socket.display()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the client did not start");
    let mut typing = asked.stdin.take().expect("a pipe");
    typing
        .write_all(b"before the restart\n")
        .expect("could not type");
    typing.flush().expect("could not type");
    // note: a gap, because the two lines are one write otherwise and a command runs the moment it
    // arrives. What is being set up is a session with something in it that the next one must not
    // have, and a restart that overtook the message would leave nothing to tell them apart by
    tokio::time::sleep(Duration::from_millis(400)).await;
    typing.write_all(b"/restart\n").expect("could not type");
    typing.flush().expect("could not type");

    let left = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::task::spawn_blocking(move || asked.wait_with_output()),
    )
    .await
    .expect(
        "the client was never let go of: its input is still open, so only the host could end it",
    )
    .expect("the client panicked")
    .expect("the client did not finish");
    // held open until here, which is what makes the line above a claim about the host
    drop(typing);

    let read = String::from_utf8_lossy(&left.stderr);
    // note: which loop this ran on, read off what the session said rather than assumed from the
    // pty. A served run says `serving on`; a *headless* one also says `headless: a line is a
    // message`, and that line's absence is the whole of what says `drawn` was the loop
    assert!(
        read.contains("serving on"),
        "this was not a served session at all: {read}"
    );
    assert!(
        !read.contains("headless: a line is a message"),
        "the pty did not take: this ran on `Server::run`, which another test already covers: {read}"
    );

    // a second client reaches a session that is not the one the first was in
    let second = tokio::task::spawn_blocking({
        let socket = socket.clone();
        move || connect(&socket, b"/quit\n")
    })
    .await
    .expect("the second client panicked");
    let after = String::from_utf8_lossy(&second.stderr);
    assert!(
        !after.contains("before the restart"),
        "the fresh session carried the old one's conversation into it: {after}"
    );

    tokio::task::spawn_blocking(move || host.wait())
        .await
        .expect("the host panicked")
        .expect("the host did not finish");

    // two sessions, two records: the one the restart wrote out and the one `/quit` did
    let logs = std::fs::read_dir(records.join("kamchatka"))
        .expect("the record directory")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
        .count();
    assert_eq!(logs, 2, "one record per session");
}
