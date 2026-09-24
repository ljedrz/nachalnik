//! What a client with no keys can do about a turn that is running.
//!
//! note: a page has no `esc` and no `ctrl+c`, so everything a person at a terminal reaches
//! for is a typed line here. The three answers are the three states a session can be in when
//! somebody asks it to stop, and only one of them is a turn.

use std::sync::Arc;

use kamchatka::{
    app::Speaker,
    remote::protocol::{Command, Message},
};
use nachalnik::{
    Capability, Grant, ModelResponse,
    test::{ConstTool, call},
};
use serde_json::json;

use crate::{Peer, Slow, served};

#[tokio::test]
async fn a_turn_is_stopped_by_a_client_that_can_only_type() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
        ModelResponse::text("this should never be said"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(Slow));
    })
    .await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    // the turn is running: the tool has started and the second response has not been asked for
    peer.until_record("tool.started").await;

    peer.send(Command::Submit {
        line: "/stop".to_owned(),
    })
    .await;
    peer.until(|message| matches!(message, Message::Busy { busy: false }))
        .await;

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    assert!(
        !app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("this should never be said")),
        "the turn carried on past the stop"
    );
}

/// A `/quit` during a turn waits for it, so that the record it writes ends with the session.
///
/// note: an interrupt does not abort a step already in flight, and the loop ended as soon as it
/// was told to: `session.finished` went into the log while the tool was still running, and its
/// result landed after it, in a record already written. `Slow` does not look at its interrupt,
/// which is the case the wait is for.
#[tokio::test]
async fn a_quit_during_a_turn_waits_for_it_to_end() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "wait", json!({}))]),
        ModelResponse::text("this should never be said"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(Slow));
    })
    .await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    peer.until_record("tool.started").await;
    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");

    let names: Vec<_> = app
        .kernel
        .history()
        .into_iter()
        .map(|record| record.event.name().to_owned())
        .collect();
    assert_eq!(
        names.last().map(String::as_str),
        Some("session.finished"),
        "{names:?}"
    );
    assert!(
        names.iter().any(|name| name == "tool.finished"),
        "the tool's result came before the end: {names:?}"
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| *name == "model.requested")
            .count(),
        1,
        "and the turn stopped rather than asking again: {names:?}"
    );
    assert!(!app.busy);
}

#[tokio::test]
async fn stopping_nothing_says_there_was_nothing_to_stop() {
    let session = served(vec![], |_| {}).await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "/stop".to_owned(),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Said { .. }))
        .await;
    let Some(Message::Said { speaker, text }) = heard.last() else {
        unreachable!("just matched")
    };
    assert_eq!(*speaker, Speaker::Note);
    assert!(text.contains("nothing is running"), "{text}");

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

#[tokio::test]
async fn a_turn_resting_on_a_question_is_told_what_ends_it() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "touchy", json!({}))]),
        ModelResponse::text("and on, once the question had an answer"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("touchy", "done").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    peer.until_record("permission.requested").await;

    // the session is not `busy` here - the loop is resting on the question - which is the whole
    // of what this is about
    peer.send(Command::Project).await;
    let heard = peer
        .until(|message| matches!(message, Message::Projected(..)))
        .await;
    let Some(Message::Projected(paused)) = heard.last() else {
        unreachable!("just matched")
    };
    assert!(!paused.busy, "a turn resting on a question is not busy");
    assert_eq!(paused.asking.len(), 1);

    peer.send(Command::Submit {
        line: "/stop".to_owned(),
    })
    .await;
    let heard = peer
        .until(|message| matches!(message, Message::Said { .. }))
        .await;
    let Some(Message::Said { text, .. }) = heard.last() else {
        unreachable!("just matched")
    };
    assert!(text.contains("waiting on a question"), "{text}");
    assert!(
        !text.contains("nothing is running"),
        "a session resting on a question is not a session with nothing running"
    );

    // and the question is still there, which is what makes the sentence the honest one: denying
    // it is what ends this turn, and `/stop` has not pretended otherwise
    peer.send(Command::Decide {
        id: nachalnik::PermissionId(1),
        grant: Grant::Deny,
        remember: false,
    })
    .await;

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// An interrupt with nothing running is not saved up for the next message.
#[tokio::test]
async fn an_interrupt_with_nothing_running_does_not_swallow_the_next_message() {
    let session = served(vec![ModelResponse::text("answered")], |_| {}).await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Interrupt).await;
    peer.until(|message| matches!(message, Message::Done { .. }))
        .await;
    peer.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    peer.until(|message| matches!(message, Message::Busy { busy: false }))
        .await;

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    assert!(
        app.kernel
            .items()
            .iter()
            .any(|item| item.content.to_text().contains("answered")),
        "the message went unanswered"
    );
}
