//! The session is not the client: it outlives one, and it serves one at a time.
//!
//! note: what a turn does when the client that started it goes away, what happens to a client when
//! another attaches - the one place this program takes something away, and says so - and what
//! happens to lines typed into a running turn.

use std::sync::Arc;

use kamchatka::{
    app::Did,
    remote::{
        Server,
        protocol::{Command, Message},
    },
    wiring::{Setup, Wired},
};
use nachalnik::{ModelResponse, test::call};
use nachalnik_providers::OpenAiCompatible;
use serde_json::json;

use crate::{Peer, Slow, attaching, common, quit, records, served, streamed};

/// A turn carries on while nobody is attached, and the records are all there afterwards.
///
/// note: the invariant the whole module is for, and the only test here that asserts it directly. A
/// client that disconnects mid-turn is the ordinary case - a laptop closing - and a session that
/// treated it as a reason to stop would be one nobody could trust to run anything long.
#[tokio::test]
async fn a_turn_outlives_the_client_that_started_it() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
        ModelResponse::text("finished with nobody watching"),
    ];
    // note: `Slow` declares no capabilities, so `Careful` allows it outright - the empty fold - and
    // the turn runs without stopping to ask. That is what this test wants: the thing it is about is
    // a client leaving, not a question nobody answered
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(Slow));
    })
    .await;

    let (mut peer, attached) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    // gone as soon as the tool starts, which is a hundred milliseconds before the turn ends
    peer.until_record("tool.started").await;
    peer.drop_it().await;

    // and back, from where it had got to: the rest of the turn is waiting in the log
    let mut again = Peer::connect(&session.at).await;
    again.send(attaching(Some(attached.seq), None)).await;
    let heard = again.until_words("finished with nobody watching").await;
    let names = records(&heard);
    assert!(names.contains(&"tool.finished".to_owned()), "{names:?}");

    again
        .send(Command::Submit {
            line: "/quit".to_owned(),
        })
        .await;
    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    assert!(
        app.kernel.items().iter().any(|item| item
            .content
            .to_text()
            .contains("finished with nobody watching")),
        "the turn did not finish"
    );
}

/// A client that attaches takes the session from the one that had it, which is told and let go of.
///
/// note: the newest rather than the first, because the ordinary second connection is the same
/// client coming back while its old one is still open - see `remote::server`. What must not happen
/// is the quiet version: a client left attached to a session somebody else is now driving, or one
/// closed with nothing saying why, which a client reads as a drop and reconnects from.
#[tokio::test]
async fn a_client_that_attaches_replaces_the_one_attached() {
    let session = served(vec![ModelResponse::text("only you")], |_| {}).await;

    let (mut one, _) = Peer::attached(&session.at).await;
    let (mut two, _) = Peer::attached(&session.at).await;

    let heard = one
        .until(|message| matches!(message, Message::Failed { .. }))
        .await;
    let Some(Message::Failed { about, error }) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(about, "replaced", "{error}");
    assert!(
        one.next().await.is_none(),
        "the replaced client was kept on"
    );

    // and the one that replaced it has the session to itself
    two.send(Command::Submit {
        line: "ask".to_owned(),
    })
    .await;
    let heard = two.until_words("only you").await;
    assert!(records(&heard).contains(&"model.requested".to_owned()));

    two.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// Lines typed into a running turn wait in a queue, and each goes in with a turn of its own.
///
/// note: the desk and the client share the queue where a session is drawn as well as served, so
/// what must not happen is a line answered `queued` and then never seen again - which is what the
/// one slot the newest won used to do to whoever typed first.
#[tokio::test]
async fn lines_typed_into_a_running_turn_each_get_a_turn() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
        ModelResponse::text("done"),
        ModelResponse::text("for the first"),
        ModelResponse::text("for the second"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(Slow));
    })
    .await;

    let (mut one, _) = Peer::attached(&session.at).await;
    one.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    // the reply to `go` as well as the tool starting, in whichever order they come: a reply waits
    // for the records already in the log, so it can arrive after the tool has started, and left
    // unread it is what the `until` below would find
    let heard = one
        .until(|message| matches!(message, Message::Replied { .. }))
        .await;
    let started = |message: &Message| matches!(message, Message::Record(record) if record.event.name() == "tool.started");
    if !heard.iter().any(started) {
        one.until_record("tool.started").await;
    }

    one.send(Command::Submit {
        line: "mine".to_owned(),
    })
    .await;
    // note: `until` rather than the next message. The session's own voice and its `busy` are on
    // this connection too, so whatever arrives next is not necessarily the answer to what was asked
    let heard = one
        .until(|message| matches!(message, Message::Replied { .. }))
        .await;
    assert!(
        matches!(
            heard.last(),
            Some(Message::Replied {
                did: Did::Queued,
                ..
            })
        ),
        "a line sent into a running turn was not queued"
    );
    // a command is not a line that waits, and runs at once
    one.send(Command::Submit {
        line: "/spend".to_owned(),
    })
    .await;
    one.send(Command::Submit {
        line: "and mine".to_owned(),
    })
    .await;
    one.until_words("for the second").await;

    one.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, ended) = session.ended().await;
    ended.expect("the session failed");
    let said: Vec<String> = app
        .kernel
        .items()
        .iter()
        .map(|item| item.content.to_text().into_owned())
        .filter(|text| !text.is_empty() && text != "waited")
        .collect();
    assert_eq!(
        said,
        [
            "go",
            "done",
            "mine",
            "for the first",
            "and mine",
            "for the second"
        ],
        "each line went in on its own, in order, with an answer of its own"
    );
}

/// A provider's retry reaches a client while the provider waits, above the answer it held up.
///
/// note: the pair of the headless suite's `a_retry_is_said_before_the_answer_it_held_up`. A notice
/// has no event to carry it, so the served loop reads it on a tick of its own; without one, a
/// client heard `trying again` when the turn ended, after the answer.
#[tokio::test]
async fn a_retry_reaches_a_client_before_the_answer_it_held_up() {
    let base = common::endpoint(vec![
        common::BUSY.to_owned(),
        common::answer("held up, then answered"),
    ])
    .await;
    // the provider the wiring builds, rather than a scripted one swapped in: the notice is its
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = Setup {
        tools: Some(Vec::new()),
        compact: None,
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new("nothing", base, "")))
    .expect("the wiring failed");
    let mut server = Server::bind("tcp:127.0.0.1:0")
        .await
        .expect("nothing would listen");
    let at = server.address();
    tokio::spawn(async move { server.run(&mut app, &mut events, &mut finished).await });

    let (mut peer, _) = Peer::attached(&at).await;
    peer.send(Command::Submit {
        line: "ask".to_owned(),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Said { text, .. } if text.contains("trying again")))
        .await;
    assert!(
        !streamed(&heard).contains("held up"),
        "the retry was said after the answer"
    );

    quit(&at).await;
}
