//! The session is not the client: it outlives one, and it serves several.
//!
//! note: what a turn does when the client that started it goes away, what two clients see of
//! one session, and what happens to a line queued into a running turn when a second arrives -
//! which is the one place this program takes something away, and says so.

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

/// Two clients are two views of one session, and each sees what the other did.
#[tokio::test]
async fn two_clients_see_one_session() {
    let session = served(vec![ModelResponse::text("both of you")], |_| {}).await;

    let (mut one, _) = Peer::attached(&session.at).await;
    let (mut two, _) = Peer::attached(&session.at).await;
    one.send(Command::Submit {
        line: "ask".to_owned(),
    })
    .await;

    let heard = two.until_words("both of you").await;
    assert!(records(&heard).contains(&"model.requested".to_owned()));

    two.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// A second line typed into a running turn replaces the first, and the session says so.
///
/// note: what this pins is an honest account of a limitation rather than the absence of one. There
/// is room for exactly one queued message, which is a decision a single prompt can live with and
/// two clients cannot - so the one thing that must not happen is the quiet version, where both
/// clients are told `queued` and one of the two lines is never seen again by anybody.
#[tokio::test]
async fn a_line_that_replaces_a_queued_one_says_so() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
        ModelResponse::text("done"),
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
    // a command is not a line that waits, and replaces nothing
    two.send(Command::Submit {
        line: "/spend".to_owned(),
    })
    .await;
    two.send(Command::Submit {
        line: "no, mine".to_owned(),
    })
    .await;

    let heard = two
        .until(|message| matches!(message, Message::Said { text, .. } if text.contains("replaced")))
        .await;
    let Some(Message::Said { text, .. }) = heard.last() else {
        unreachable!("just matched")
    };
    assert!(text.contains("`mine`"), "it did not say which line: {text}");

    // and said once for each line that went, which the next replacement is the end of
    one.send(Command::Submit {
        line: "mine again".to_owned(),
    })
    .await;
    let heard = [
        heard,
        two.until(
            |message| matches!(message, Message::Said { text, .. } if text.contains("`no, mine`")),
        )
        .await,
    ]
    .concat();
    let replaced: Vec<&String> = heard
        .iter()
        .filter_map(|message| match message {
            Message::Said { text, .. } if text.contains("replaced") => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(replaced.len(), 2, "{replaced:#?}");

    two.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
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
