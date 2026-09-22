//! A permission question, answered from a client.
//!
//! note: the three moments it can arrive in. Asked while somebody is attached, answered
//! before the question was drawn, and asked with nobody here at all - the last of which is
//! why a question belongs in the projection rather than only in the record.

use std::sync::Arc;

use kamchatka::remote::protocol::{Command, Message};
use nachalnik::{
    Capability, Event, Grant, ModelResponse,
    test::{ConstTool, call},
};
use serde_json::json;

use crate::{Peer, records, served};

/// A question raised by a tool is answered from a client, and the turn carries on.
#[tokio::test]
async fn a_question_is_answered_from_a_client() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("it let me"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    let heard = peer.until_record("permission.requested").await;
    let Some(Message::Record(record)) = heard.last() else {
        unreachable!("just matched")
    };
    let Event::PermissionRequested { request } = &record.event else {
        unreachable!("just matched")
    };

    peer.send(Command::Decide {
        id: request.id,
        grant: Grant::Allow,
        remember: false,
    })
    .await;
    let heard = peer.until_words("it let me").await;
    assert!(records(&heard).contains(&"tool.finished".to_owned()));

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// An answer sent the instant the question is broadcast is honoured, and the turn carries on.
///
/// note: what this does *not* measure, and the measurement is the reason the note says so.
/// Answering inside the window where the turn that raised the question has not finished unwinding
/// is the interesting case - it is where a decision used to be recorded and then go nowhere - and
/// this cannot reach it: the outcome travels from the turn's own task to the loop, and a client's
/// answer has a socket round trip to make first, so the outcome wins every time. Measured, by
/// applying decisions on sight instead of guarding them: nothing here failed. The window is driven
/// by hand in `headless.rs`, under
/// `a_question_answered_inside_the_window_still_carries_the_turn_on`, and the guard lives in
/// `App::on_outcome` where all three loops come through rather than in this one.
///
/// note: it is kept because what it *does* measure is worth measuring - a decision answered as
/// fast as anything can answer it, through a socket, ending in a turn that ran.
#[tokio::test]
async fn an_answer_sent_the_moment_the_question_arrives_is_honoured() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("carried on anyway"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    let heard = peer.until_record("permission.requested").await;
    let Some(Message::Record(record)) = heard.last() else {
        unreachable!("just matched")
    };
    let Event::PermissionRequested { request } = &record.event else {
        unreachable!("just matched")
    };
    // no waiting for anything: this is sent while the turn that raised the question is still in
    // flight, which is the whole point of the test
    peer.send(Command::Decide {
        id: request.id,
        grant: Grant::Allow,
        remember: false,
    })
    .await;

    peer.until_words("carried on anyway").await;

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// A question raised before a client existed is in the projection it arrives to.
#[tokio::test]
async fn a_question_nobody_was_here_for_is_in_the_projection() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("eventually"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut first, _) = Peer::attached(&session.at).await;
    first
        .send(Command::Submit {
            line: "go".to_owned(),
        })
        .await;
    first.until_record("permission.requested").await;
    first.drop_it().await;

    // somebody else, arriving at a session that is stopped on a question raised before they
    // connected. The record that asked it is one they will never be sent
    let (mut peer, attached) = Peer::attached(&session.at).await;
    assert_eq!(attached.asking.len(), 1, "the question was not carried");
    assert_eq!(attached.asking[0].tool, "peek");

    peer.send(Command::Decide {
        id: attached.asking[0].id,
        grant: Grant::Allow,
        remember: false,
    })
    .await;
    peer.until_words("eventually").await;

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}
