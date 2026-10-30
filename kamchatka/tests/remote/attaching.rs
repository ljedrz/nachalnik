//! What a client is answered when it arrives, and what it is refused for.
//!
//! note: an attach is the one message with an argument the session did not choose - a
//! watermark, a session name, a protocol version - and each of the three is a way for a
//! client to be talking about something other than what is here. What is checked is that
//! each is refused by name rather than served something that looks right.

use kamchatka::remote::protocol::{self, Command, Message};
use nachalnik::{ContextItem, ModelResponse, Usage};

use crate::{Peer, attaching, quit, records, served, served_as};

/// A projection says what the next request is expected to cost in the terminal's own figure:
/// anchored on what the provider charged for the last request, not the counter's raw estimate.
///
/// note: the web page added up the budget for itself while the status line showed the anchored
/// figure, and on a context the counter is wrong about the two drifted apart by its whole error.
#[tokio::test]
async fn a_projection_carries_the_figure_the_status_line_shows() {
    let session = served(
        vec![ModelResponse {
            usage: Some(Usage {
                input_tokens: Some(9_000),
                ..Default::default()
            }),
            ..ModelResponse::text("done")
        }],
        // a context the counter is going to be wrong about, and a provider that says so
        |app| {
            app.kernel.push(ContextItem::file(
                "haystack.txt",
                "a needle in it. ".repeat(200),
            ));
        },
    )
    .await;
    let (mut peer, attached) = Peer::attached(&session.at).await;
    // with no response yet there is nothing to anchor on, and the estimate is all there is
    assert_eq!(attached.expected, Some(attached.budget.used()));

    peer.send(Command::Submit {
        line: "go".to_owned(),
    })
    .await;
    peer.until(|message| matches!(message, Message::Busy { busy: true }))
        .await;
    peer.until(|message| matches!(message, Message::Busy { busy: false }))
        .await;
    peer.send(Command::Project).await;
    let heard = peer
        .until(|message| matches!(message, Message::Projected(..)))
        .await;
    let Some(Message::Projected(projected)) = heard.last() else {
        unreachable!("just matched")
    };
    let expected = projected.expected.expect("this build sends the figure");
    assert!(
        expected >= 9_000,
        "not anchored on the provider's figure: {expected}"
    );
    assert_ne!(
        expected,
        projected.budget.used(),
        "the counter's raw estimate"
    );

    // and a session that predates the field is still read, with nothing in it
    let mut sent = serde_json::to_value(&heard[heard.len() - 1]).expect("it writes");
    sent.as_object_mut()
        .expect("an object")
        .remove("expected")
        .expect("this build sends the field");
    let Message::Projected(older) = serde_json::from_value::<Message>(sent).expect("it reads")
    else {
        unreachable!("it was a projection")
    };
    assert_eq!(older.expected, None);

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    let going = app.going();
    assert_eq!(
        expected,
        app.expected(&going, &app.kernel.budget()),
        "the status line's figure"
    );
}

/// A projection from a session that predates the advisor's reasons is still one a client reads.
///
/// note: the field was added once two ends could be deployed apart, so an older session answers
/// without it - and a client that refused the whole projection for a missing list of reasons
/// would have nothing to draw at all.
#[tokio::test]
async fn a_projection_with_no_reasons_for_missing_ratings_still_reads() {
    let session = served(vec![], |_| {}).await;
    let (peer, attached) = Peer::attached(&session.at).await;

    let mut sent = serde_json::to_value(Message::Attached(Box::new(attached))).expect("it writes");
    sent.as_object_mut()
        .expect("an object")
        .remove("unrated")
        .expect("this build sends the field");
    serde_json::from_value::<Message>(sent).expect("and reads without it");

    drop(peer);
    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client that has just arrived is told where the session stands, and then what happens next.
#[tokio::test]
async fn attaching_answers_with_a_projection_and_then_the_records() {
    let session = served(vec![ModelResponse::text("4")], |app| {
        app.kernel.push(nachalnik::ContextItem::user("what is 2+2"));
    })
    .await;

    let (mut peer, attached) = Peer::attached(&session.at).await;
    assert!(
        attached
            .conversation
            .iter()
            .any(|line| line.text == "what is 2+2"),
        "the conversation did not carry the item"
    );
    assert_eq!(attached.items.len(), 1);
    assert!(
        attached.seq > 0,
        "a session has records before anybody looks"
    );

    peer.send(Command::Submit {
        line: "and 3+3".to_owned(),
    })
    .await;
    let heard = peer.until_words("4").await;
    assert!(records(&heard).contains(&"model.requested".to_owned()));

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// The projection carries the conversation, because the records cannot.
///
/// note: this is the claim the whole snapshot exists for, and it is worth pinning as its own test
/// rather than as an assertion inside a bigger one. The session log names things and does not copy
/// them, so `context.added` says there is a user message of eleven tokens and not one word of what
/// it says. A client fed nothing but records is therefore unable to render a conversation that
/// happened before it connected - and a future change that "simplified" the projection into a
/// stream of events would pass every other test in this file.
#[tokio::test]
async fn the_records_alone_cannot_say_what_was_said() {
    let session = served(vec![ModelResponse::text("answered")], |app| {
        app.kernel.push(nachalnik::ContextItem::user(
            "a sentence nothing else carries",
        ));
    })
    .await;

    let (_, attached) = Peer::attached(&session.at).await;
    let log = app_history(&session.at).await;
    assert!(
        log.contains("context.added"),
        "the item was not recorded: {log}"
    );
    assert!(
        !log.contains("a sentence nothing else carries"),
        "the log copied an item's content: {log}"
    );
    assert!(
        attached
            .conversation
            .iter()
            .any(|line| line.text == "a sentence nothing else carries"),
        "the projection did not carry it either"
    );

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// Reads the session log the way a later client would, to compare it against a projection.
async fn app_history(at: &str) -> String {
    let (mut peer, _) = Peer::attached(at).await;
    peer.send(attaching(Some(0), None)).await;
    let mut log = String::new();
    // everything already recorded arrives before anything else the session says
    for message in peer
        .until(|message| !matches!(message, Message::Record(_)))
        .await
    {
        if let Message::Record(record) = message {
            log.push_str(&serde_json::to_string(&record).expect("a record is JSON"));
        }
    }
    peer.drop_it().await;

    log
}

/// A client that says where it had got to is sent what it missed, and no second copy of the rest.
#[tokio::test]
async fn resuming_sends_what_was_missed_and_no_projection() {
    let session = served(
        vec![ModelResponse::text("first"), ModelResponse::text("second")],
        |_| {},
    )
    .await;

    let (mut first, attached) = Peer::attached(&session.at).await;
    first
        .send(Command::Submit {
            line: "one".to_owned(),
        })
        .await;
    first.until_record("model.finished").await;
    first.drop_it().await;

    // a second connection claiming the same watermark the first one arrived at: everything since
    // is a record, and there is no projection, because it says it already has one
    let mut again = Peer::connect(&session.at).await;
    again.send(attaching(Some(attached.seq), None)).await;
    let heard = again.until_record("model.finished").await;
    assert!(
        !heard
            .iter()
            .any(|message| matches!(message, Message::Attached(_))),
        "a resume was answered with a projection"
    );
    assert!(records(&heard).contains(&"context.added".to_owned()));

    again
        .send(Command::Submit {
            line: "/quit".to_owned(),
        })
        .await;
    session.ended().await.1.expect("the session failed");
}

/// What a resume missed arrives before the answer saying whether the session is busy.
///
/// note: a client whose input has closed leaves on `busy: false`, and a resume is how such a
/// client comes back for the end of an answer - so the records it missed have to be in front of
/// that answer, or it is told the session is resting and leaves without them.
#[tokio::test]
async fn a_resume_is_sent_what_it_missed_before_it_is_told_the_session_rests() {
    let session = served(vec![ModelResponse::text("first")], |_| {}).await;

    let (mut first, attached) = Peer::attached(&session.at).await;
    first
        .send(Command::Submit {
            line: "one".to_owned(),
        })
        .await;
    first.until_record("model.finished").await;
    first.drop_it().await;

    let mut again = Peer::connect(&session.at).await;
    again.send(attaching(Some(attached.seq), None)).await;
    let heard = again
        .until(|message| matches!(message, Message::Done { .. }))
        .await;
    assert!(
        records(&heard).contains(&"model.finished".to_owned()),
        "the resume was answered before what it missed: {heard:?}"
    );

    again
        .send(Command::Submit {
            line: "/quit".to_owned(),
        })
        .await;
    session.ended().await.1.expect("the session failed");
}

/// A client claiming to have seen more than happened is refused rather than quietly humoured.
#[tokio::test]
async fn a_watermark_from_the_future_is_refused() {
    let session = served(vec![], |_| {}).await;

    let mut peer = Peer::connect(&session.at).await;
    peer.send(attaching(Some(9_999), None)).await;
    let Message::Failed { error, .. } = peer.recv().await else {
        panic!("a watermark from the future was accepted");
    };
    assert!(error.contains("you say you have 9999"), "{error}");
    assert!(peer.next().await.is_none(), "the connection stayed open");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A watermark from a different session is refused rather than quietly served.
///
/// note: the quiet half of the watermark check, and the one a magnitude test cannot reach. A
/// session restarted at the same address has a log of its own, and a number from the one before it
/// can be perfectly plausible against it - at which point a client draws one session's records
/// under another session's conversation, and every figure on its screen is about neither.
#[tokio::test]
async fn a_watermark_from_another_session_is_refused() {
    let session = served_as(Some("a-session-of-its-own"), vec![], |_| {}).await;
    let (peer, attached) = Peer::attached(&session.at).await;
    peer.drop_it().await;

    // a number this session really does have, under a name it does not answer to
    let mut again = Peer::connect(&session.at).await;
    again
        .send(attaching(
            Some(attached.seq),
            Some("somebody else's session"),
        ))
        .await;
    let Message::Failed { error, .. } = again.recv().await else {
        panic!("a watermark from another session was accepted");
    };
    assert!(error.contains("a-session-of-its-own"), "{error}");
    assert!(again.next().await.is_none(), "the connection stayed open");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client speaking a version this session does not is told which of the two ends is older, and
/// one that says nothing is served.
///
/// note: both halves, because the field is only worth having if the second is true. A default of
/// "unknown" would refuse exactly the clients the field was added to keep working - everything
/// written against this wire before it had a version at all.
///
/// note: an older version is refused too, and told to update. Version 2 writes a request's items
/// as runs, which the records a version-1 build of this program deserializes cannot hold, and
/// nothing serves an older version what it can read.
#[tokio::test]
async fn a_client_of_another_version_is_refused_and_one_that_says_nothing_is_not() {
    let session = served(vec![], |_| {}).await;

    let mut ahead = Peer::connect(&session.at).await;
    ahead
        .send(Command::Attach {
            since: None,
            session: None,
            version: Some(protocol::VERSION + 1),
            relaying: None,
        })
        .await;
    let Message::Failed { about, error } = ahead.recv().await else {
        panic!("a version this session does not speak was accepted");
    };
    assert!(
        error.contains(&format!("version {}", protocol::VERSION + 1))
            && error.contains(&format!("speaks {}", protocol::VERSION)),
        "{error}"
    );
    // named for itself rather than as the attach it arrived on, because the two want opposite
    // things done about them: an attach refusal is mended by attaching afresh and this is mended
    // by nothing. See `a_version_the_session_refuses_is_not_attached_to_again`
    assert_eq!(about, "version");

    let mut behind = Peer::connect(&session.at).await;
    behind
        .send(Command::Attach {
            since: None,
            session: None,
            version: Some(protocol::VERSION - 1),
            relaying: None,
        })
        .await;
    let Message::Failed { about, error } = behind.recv().await else {
        panic!("a version this session cannot serve was accepted");
    };
    assert!(
        error.contains(&format!("version {}", protocol::VERSION - 1))
            && error.contains("update the client"),
        "{error}"
    );
    assert_eq!(about, "version");

    // and the wire as it was written before the field existed, by hand, which is the case the
    // `None` is for
    let mut quiet = Peer::connect(&session.at).await;
    quiet.raw(b"{\"do\":\"attach\",\"since\":null}\n").await;
    let Message::Attached(attached) = quiet.recv().await else {
        panic!("a client that did not say a version was refused");
    };
    // and the projection says which version answered it, which is how a client learns what the
    // other end speaks without having to be refused to find out
    assert_eq!(attached.version, protocol::VERSION);

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// Anything before an attach is refused: a connection says where it stands first.
#[tokio::test]
async fn a_command_before_attaching_is_refused() {
    let session = served(vec![], |_| {}).await;

    let mut peer = Peer::connect(&session.at).await;
    peer.send(Command::Interrupt).await;
    let Message::Failed { error, .. } = peer.recv().await else {
        panic!("a command before an attach was taken");
    };
    assert!(error.contains("before `attach`"), "{error}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// The trace says when the session changes hands, once each time, naming each client by where it
/// came from.
///
/// note: over a socket file, where a client is the process at the other end, so every one is this
/// test's own. A client is seated after its projection is taken, so a third client is the one that
/// reads the first two lines.
///
/// note: the whole list, so that a handover said twice fails: the first client is let go when the
/// second attaches and its connection closes, and that is not a line of its own. Nor is a
/// connection that never attaches.
///
/// note: on the trace and not in the conversation, which a browser reconnecting once a second
/// would fill with arrivals.
#[tokio::test]
async fn the_trace_says_once_when_the_session_changes_hands() {
    use crate::served_over_a_socket;

    let session = served_over_a_socket("client-hands", Vec::new(), |_| {}).await;
    let pid = std::process::id();

    let idle = crate::Socket::connect(&session.at).await;
    let (first, _) = attached(&session.at).await;
    let (second, _) = attached(&session.at).await;
    // the first is let go as the second is seated; waited for, so its leaving is in the trace
    // the third reads if it is said at all
    let mut first = first;
    while first.next().await.is_some() {}
    let (mut third, attached) = attached(&session.at).await;

    let said: Vec<(&str, &str)> = attached
        .trace
        .iter()
        .filter(|line| line.name.starts_with("client."))
        .map(|line| (line.name.as_str(), line.detail.as_str()))
        .collect();
    let (named, replaced) = (format!("pid {pid}"), format!("pid {pid}, by pid {pid}"));
    assert_eq!(
        said,
        [
            ("client.attached", named.as_str()),
            ("client.replaced", replaced.as_str())
        ]
    );
    assert!(
        !attached
            .conversation
            .iter()
            .any(|line| line.text.contains(&named)),
        "a client is not part of the conversation: {:?}",
        attached.conversation
    );

    drop((idle, second));
    third
        .send(Command::Submit {
            line: "/quit".to_owned(),
        })
        .await;
    session.ended().await.1.expect("the session failed");
}

/// A relay is named on the trace by the browser it relays for, and only a relay is: a client that
/// sends the same field is named by the address the session sees.
///
/// note: the session believes the field from a connection out of its own process, which is what
/// the `--web` relay is, and what this test is over a socket file. Over a port it believes only
/// connections the relay has listed, and this test's are not.
#[tokio::test]
async fn a_relay_is_named_by_the_browser_it_relays_for_and_nobody_else_is() {
    use crate::{Peer, served_over_a_socket};

    let relaying = || Command::Attach {
        since: None,
        session: None,
        version: Some(protocol::VERSION),
        relaying: Some("192.0.2.7:51234".to_owned()),
    };
    // the first attach is on the trace that the next client's projection carries
    let said = |attached: &protocol::Attached| -> Vec<String> {
        attached
            .trace
            .iter()
            .filter(|line| line.name == "client.attached")
            .map(|line| line.detail.clone())
            .collect()
    };

    let session = served_over_a_socket("client-relaying", Vec::new(), |_| {}).await;
    let mut relay = crate::Socket::connect(&session.at).await;
    relay.send(relaying()).await;
    while !matches!(relay.recv().await, Message::Attached(_)) {}
    let (mut next, attached) = attached(&session.at).await;
    assert_eq!(said(&attached), ["192.0.2.7:51234 (web UI)"]);
    next.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");

    let session = served(vec![], |_| {}).await;
    let mut pretender = Peer::connect(&session.at).await;
    pretender.send(relaying()).await;
    while !matches!(pretender.recv().await, Message::Attached(_)) {}
    let (next, attached) = Peer::attached(&session.at).await;
    let named = said(&attached);
    assert!(
        named.len() == 1 && named[0].starts_with("127.0.0.1:"),
        "{named:?}"
    );
    drop((pretender, next));
    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client that has the session says on the trace when it goes, so nobody has it.
#[tokio::test]
async fn the_trace_says_when_the_client_with_the_session_goes() {
    use crate::served_over_a_socket;

    let session = served_over_a_socket("client-goes", Vec::new(), |_| {}).await;
    let named = format!("pid {}", std::process::id());

    let (first, _) = attached(&session.at).await;
    drop(first);
    // the session hears of it on its own time, so the next client may have to look again
    let said = loop {
        let (mut next, attached) = attached(&session.at).await;
        let said: Vec<(String, String)> = attached
            .trace
            .iter()
            .filter(|line| line.name.starts_with("client."))
            .map(|line| (line.name.clone(), line.detail.clone()))
            .collect();
        if said.iter().any(|(name, _)| name == "client.left") {
            next.send(Command::Submit {
                line: "/quit".to_owned(),
            })
            .await;
            break said;
        }
    };

    // the last line, since a client that looked too early took the session rather than finding it
    // empty, and its own leaving is the one said
    assert_eq!(
        said.first(),
        Some(&("client.attached".to_owned(), named.clone()))
    );
    assert_eq!(said.last(), Some(&("client.left".to_owned(), named)));
    session.ended().await.1.expect("the session failed");
}

/// Attaches over a socket file, and hands back the projection.
async fn attached(at: &str) -> (crate::Socket, Box<protocol::Attached>) {
    let mut socket = crate::Socket::connect(at).await;
    socket.send(attaching(None, None)).await;
    loop {
        if let Message::Attached(attached) = socket.recv().await {
            break (socket, attached);
        }
    }
}
