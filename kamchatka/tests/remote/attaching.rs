//! What a client is answered when it arrives, and what it is refused for.
//!
//! note: an attach is the one message with an argument the session did not choose - a
//! watermark, a session name, a protocol version - and each of the three is a way for a
//! client to be talking about something other than what is here. What is checked is that
//! each is refused by name rather than served something that looks right.

use kamchatka::remote::protocol::{self, Command, Message};
use nachalnik::ModelResponse;

use crate::{Peer, attaching, quit, records, served, served_as};

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
    let older: Message = serde_json::from_value(sent).expect("and reads without it");
    let Message::Attached(older) = older else {
        unreachable!("it was written as one")
    };
    assert!(older.unrated.is_empty());

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
    // note: the session says when somebody attaches, and says it on the *trace*, which is where a
    // thing that happens once a second belongs. It was in the conversation and that is what a
    // browser reconnecting on a flaky link filled with arrivals: the chat is the context and the
    // trace is the ring. What matters is that a session two people can type into still says so
    // somewhere both of them can read, which every projection carries
    assert!(
        !attached
            .conversation
            .iter()
            .any(|line| line.text.contains("client 1")),
        "an arrival is not part of the conversation: {:?}",
        attached.conversation
    );
    assert!(
        attached
            .trace
            .iter()
            .any(|line| line.name == "client.attached" && line.detail.contains("client 1")),
        "the session did not say a client had arrived: {:?}",
        attached.trace
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

    let (mut peer, attached) = Peer::attached(&session.at).await;
    let log = app_history(&session.at).await;
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

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// Reads the session log the way a second client would, to compare it against a projection.
async fn app_history(at: &str) -> String {
    let (mut peer, _) = Peer::attached(at).await;
    peer.send(attaching(Some(0), None)).await;
    let mut log = String::new();
    // everything already recorded arrives at once; `session.started` is always the first of them
    for message in peer.until_record("session.started").await {
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
#[tokio::test]
async fn a_client_from_a_later_version_is_refused_and_one_that_says_nothing_is_not() {
    let session = served(vec![], |_| {}).await;

    let mut ahead = Peer::connect(&session.at).await;
    ahead
        .send(Command::Attach {
            since: None,
            session: None,
            version: Some(protocol::VERSION + 1),
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
