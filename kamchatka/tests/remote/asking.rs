//! What one attached client asks of a session, and what comes back.
//!
//! note: a whole item, a command's own answer, the help, an interrupt, and a fragment
//! arriving out of order - the ordinary traffic, which is where a protocol is usually wrong
//! about who an answer belongs to.

use std::sync::Arc;

use kamchatka::{
    app::Did,
    remote::{
        Server,
        protocol::{Command, Message},
    },
    wiring::Wired,
};
use nachalnik::{ContextId, ModelResponse, test::call};
use serde_json::json;

use crate::{Peer, Served, Slow, Trickle, records, served, streamed, wired};

/// `inspect` says what an item holds, which nothing on the record stream can.
#[tokio::test]
async fn inspect_answers_with_the_whole_of_an_item() {
    let session = served(vec![], |app| {
        app.kernel
            .push(nachalnik::ContextItem::user("the whole of what it says"));
    })
    .await;

    let (mut peer, attached) = Peer::attached(&session.at).await;
    peer.send(Command::Inspect {
        id: attached.items[0].id,
        raw: false,
        version: None,
    })
    .await;
    // note: `until` rather than the next message, because the session's own voice is on this
    // connection too - a client hears itself arrive - and a test that took whatever came next would
    // be asserting about the order two unrelated things happened in
    let heard = peer
        .until(|message| matches!(message, Message::Item { .. }))
        .await;
    let Some(Message::Item { body, .. }) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(body, "the whole of what it says");

    peer.send(Command::Inspect {
        id: ContextId(9_999),
        raw: false,
        version: None,
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Failed { .. }))
        .await;
    let Some(Message::Failed { about, error }) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(about, "inspect");
    assert!(error.contains("no item 9999"), "{error}");

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// A command's page comes back to whoever asked for it, and its lines to everybody.
///
/// note: the split is the point. What `/budget` *says* goes out as `Said` to every client, because
/// a session with two people watching has one voice; the page it opened comes back in the reply,
/// because a page is the answer to one line somebody handed in and a second copy of it on every
/// other screen would be chrome nobody asked for.
#[tokio::test]
async fn a_command_answers_the_client_that_ran_it() {
    let session = served(vec![], |_| {}).await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "/budget".to_owned(),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Replied { .. }))
        .await;
    let Some(Message::Replied { did, page, .. }) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(*did, Did::Ran);
    let page = page.as_ref().expect("a command with no page");
    assert!(page.title.contains("budget"), "{}", page.title);

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// A fragment can arrive after the record that ended the thing it was part of, and says so.
///
/// note: the contract every client has to honour, and the one that bit the browser first. A
/// fragment is unnumbered and best-effort; the records are read out of the log and never dropped.
/// So a connection that has fallen behind is caught up on *records* first - see the note in
/// `server.rs` on why that order and not the other - and the fragments it was holding arrive after
/// the `model.finished` they belong to, carrying it as `after`.
///
/// note: what a client must do about it is drop them, because the item named by that record is now
/// the authority on what was said. `kamchatka`'s terminal does it under the name
/// `Entry::transient`; `examples/browser.html` did not, and showed one answer twice and another in
/// two pieces with a message typed in between. A phone is what made it happen, by being slow enough
/// to stall the writes all the way back to the session - which is the backpressure design working,
/// with the consequence it is documented to have.
#[tokio::test]
async fn a_fragment_can_arrive_after_the_record_that_ended_it() {
    let session = served(
        vec![ModelResponse::text("the whole answer at once")],
        |_| {},
    )
    .await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    let heard = peer.until_words("the whole answer at once").await;

    let ended = heard
        .iter()
        .find_map(|message| match message {
            Message::Record(record) if record.event.name() == "model.finished" => Some(record.seq),
            _ => None,
        })
        .expect("the turn never finished");
    let after = heard
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Progress { after, .. } => Some(*after),
            _ => None,
        })
        .expect("nothing streamed");

    assert!(
        after >= ended,
        "a fragment arrived naming record {after}, before the {ended} that ended its answer -          which would be the one order a client could not be asked to sort out"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// `/help` from a client is the commands, and nothing about keys.
///
/// note: the second of these two found from a phone. A served session has no keys of this
/// program's to press, and `/help` was handing a browser six pages of them - `ctrl+p` shows the
/// next request, `g` goes to the top of the trace - which is a reference to a program the reader is
/// not using. The greeting had the same shape and was fixed the same afternoon; both came of a
/// question that had only ever had two answers, screen or pipe, being asked by a third thing.
///
/// note: and it is settled per command rather than per session, which is what the second half
/// below is about. A session can be drawn and served at once - `--serve` no longer means "instead
/// of a screen" - so one `App` has two audiences for one `/help`, and the flag saying which is a
/// fact about whoever just asked. The session here has keys and the client asking still gets none.
#[tokio::test]
async fn help_from_a_client_is_the_commands() {
    // `App::keys` is `true` here, as it is in any session nothing has said otherwise about -
    // which is what a drawn-and-served one looks like. The client asking still gets none
    let session = served(vec![], |_| {}).await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "/help".to_owned(),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Replied { .. }))
        .await;
    let Some(Message::Replied { page, .. }) = heard.last() else {
        unreachable!("just matched")
    };
    let page = page.as_ref().expect("`/help` opened nothing");

    assert_eq!(page.title, "the commands");
    assert_eq!(page.pages.len(), 1, "{:?}", page.pages);
    let body = &page.pages[0].body;
    assert!(body.contains("/attach"), "the commands are missing: {body}");
    assert!(
        !body.contains("ctrl+p"),
        "a client was told about keys: {body}"
    );
    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    // and the session's own answer is untouched, which is the half that makes it per-caller rather
    // than a flag a client turns off for everybody: a `/help` at the screen still has the keys
    assert!(
        app.keys,
        "answering a client took the keys off the session it was attached to"
    );
}

/// A client can stop a turn somebody else started.
#[tokio::test]
async fn an_interrupt_from_a_client_stops_the_turn() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
        ModelResponse::text("never reached"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(Slow));
    })
    .await;

    let (mut one, _) = Peer::attached(&session.at).await;
    let (mut two, _) = Peer::attached(&session.at).await;
    one.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    one.until_record("tool.started").await;
    two.send(Command::Interrupt).await;

    let heard = one.until_record("turn.interrupted").await;
    assert!(records(&heard).contains(&"turn.interrupted".to_owned()));

    two.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// A client attaching mid-answer gets the projection and then the rest of what is arriving.
///
/// note: the case the terminal never exercises, because a screen has been there from the start.
/// What it is checking is the seam between the two halves: the projection is taken at a sequence
/// number, everything after it comes as records, and the fragments hang off whichever record went
/// out last - so the answer a late client assembles is the *rest* of the sentence rather than all
/// of it or none.
#[tokio::test]
async fn a_client_can_arrive_in_the_middle_of_an_answer() {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(vec![]);
    app.kernel.set_provider(Arc::new(Trickle {
        words: (0..12).map(|n| format!("word{n} ")).collect(),
    }));
    let mut server = Server::bind("tcp:127.0.0.1:0")
        .await
        .expect("nothing would listen");
    let at = server.address();
    let kernel = app.kernel.clone();
    let loop_ = tokio::spawn(async move {
        let outcome = server.run(&mut app, &mut events, &mut finished).await;

        (app, outcome)
    });
    let session = Served { at, kernel, loop_ };

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "say something long".to_owned(),
    })
    .await;
    peer.until(|message| matches!(message, Message::Progress { .. }))
        .await;

    // and now somebody else, with the answer already half written
    let (mut late, attached) = Peer::attached(&session.at).await;
    assert!(
        attached.busy,
        "the projection did not say a turn was running"
    );
    let heard = late.until_record("model.finished").await;
    let rest = streamed(&heard);
    assert!(!rest.is_empty(), "a late client saw none of the answer");
    assert!(
        !rest.starts_with("word0 "),
        "a late client was replayed the whole answer: {rest}"
    );
    // every fragment says which record it hangs off, and for a late client that is the one its
    // projection was taken at
    assert!(
        heard.iter().any(
            |message| matches!(message, Message::Progress { after, .. } if *after >= attached.seq)
        ),
        "a fragment named a record the client had not seen"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}
