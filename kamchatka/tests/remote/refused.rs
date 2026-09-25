//! What the session does with a message it cannot read.
//!
//! note: malformed, oversized, and named for something this build has never heard of are
//! three different answers: two close the connection and one does not, because a name a later
//! version knows is not a frame a client got wrong.

use kamchatka::remote::{
    Server,
    protocol::{self, Address, Command, Message},
};
use tokio::io::AsyncWriteExt;

use crate::{PATIENCE, Peer, quit, served};

/// A line that is not a message closes the connection, and says which rule it broke.
#[tokio::test]
async fn a_malformed_frame_closes_the_connection() {
    let session = served(vec![], |_| {}).await;

    let mut peer = Peer::connect(&session.at).await;
    peer.raw(b"{not json at all\n").await;
    let Message::Failed { error, .. } = peer.recv().await else {
        panic!("nonsense was accepted");
    };
    assert!(error.contains("not one"), "{error}");
    assert!(peer.next().await.is_none(), "the connection stayed open");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// So does one with no end to it, while it is still arriving.
///
/// note: nothing this sends contains a newline, which is the case `MAX_LINE` says it is there for
/// and the one a cap checked against an already-read line cannot catch - the reading is the thing
/// it was supposed to stop. This used to send `MAX_LINE + 1` bytes *and a newline*, so it only
/// ever exercised the check after the fact, which is the check that was there.
#[tokio::test]
async fn an_oversized_frame_closes_the_connection() {
    let session = served(vec![], |_| {}).await;

    let Peer {
        mut lines,
        mut write,
    } = Peer::connect(&session.at).await;
    // a peer that writes and never finishes a message, for as long as anybody will listen
    let writing = tokio::spawn(async move {
        let chunk = vec![b'x'; 1024 * 1024];
        let mut sent = 0;
        while write.write_all(&chunk).await.is_ok() {
            sent += chunk.len();
        }

        sent
    });

    let read = tokio::time::timeout(PATIENCE, protocol::read::<Message>(&mut lines))
        .await
        .expect("a frame with no end to it was read for ever");
    // note: two endings and the claim is about neither of them on its own. The session writes why
    // and closes, and this peer is still sending when it does - so the receive buffer holds bytes
    // nobody read, and TCP answers that close with a reset. Where the reset wins it takes the
    // sentence with it, which is what happens when the timing goes that way; the session will not
    // drain the flood to deliver it, because not reading a peer that floods is the thing under
    // test. What is promised is that the connection ends
    match read {
        Ok(Some(Message::Failed { error, .. })) => {
            assert!(error.contains("over the"), "{error}");
        }
        Err(reset) => assert!(
            reset.starts_with("the connection stopped"),
            "the connection ended, but not as a connection ending: {reset}"
        ),
        other => panic!("an oversized frame was accepted: {other:?}"),
    }
    // and it was stopped while it arrived: what got in is the cap and whatever was in flight
    // behind it, rather than however much this was willing to send
    let sent = writing.await.expect("the writer panicked");
    assert!(
        sent < protocol::MAX_LINE * 2,
        "{sent} bytes were read before anybody objected"
    );

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A command this build has no name for is answered, and the connection carries on.
///
/// note: the forward half of the compatibility rule, and the reason it is an answer rather than a
/// closed connection: a client that sent something is owed exactly one answer whether or not this
/// end knows what it was - see `Message::Done`. An unknown `do` used to be a parse error, and a
/// parse error takes the connection with it.
#[tokio::test]
async fn a_command_this_build_has_no_name_for_is_answered() {
    let session = served(vec![], |_| {}).await;

    let (mut peer, _) = Peer::attached(&session.at).await;
    peer.raw(b"{\"do\":\"something-later\",\"what\":1}\n").await;
    let Message::Failed { about, error } = peer.recv().await else {
        panic!("a command from a later version was not answered");
    };
    assert_eq!(about, "unknown");
    assert!(error.contains("no such command"), "{error}");

    // and the connection is still a connection, which is the half that matters
    peer.send(Command::Project).await;
    assert!(
        matches!(peer.recv().await, Message::Projected(_)),
        "an unknown command took the connection with it"
    );

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A message this build has no name for reads as one, rather than as a broken connection.
///
/// note: the other direction, and it cannot be driven through a socket because both ends of one
/// are this build. What a session with a message this build has never heard of looks like is a
/// line on the wire, so that is what this hands to the reader.
#[test]
fn a_message_this_build_has_no_name_for_reads_as_one() {
    let later: Message = serde_json::from_str(r#"{"is":"whatever-comes-next","seq":9,"x":[1]}"#)
        .expect("a message from a later version closed the connection");
    assert_eq!(later, Message::Unknown);

    // and the ones it does know still read as themselves
    let known: Message = serde_json::from_str(r#"{"is":"busy","busy":true}"#).expect("a message");
    assert_eq!(known, Message::Busy { busy: true });
}

/// Where a session listens is the whole of its authentication, so it refuses to listen elsewhere.
#[tokio::test]
async fn a_session_will_not_listen_where_anybody_could_reach_it() {
    // TEST-NET-1, which parses as an address without anybody having to resolve it
    let Err(refused) = Server::bind("tcp:192.0.2.1:7878").await else {
        panic!("it bound a public address");
    };
    assert!(refused.contains("no authentication"), "{refused}");
    assert!(refused.contains("ssh -L"), "it refused without a way out");
}

/// An address without a scheme is refused rather than guessed at.
#[test]
fn an_address_says_what_kind_of_thing_it_is() {
    assert!(matches!(
        protocol::address("unix:/run/kamchatka.sock"),
        Ok(Address::Unix("/run/kamchatka.sock"))
    ));
    assert!(matches!(
        protocol::address("tcp:127.0.0.1:7878"),
        Ok(Address::Tcp("127.0.0.1:7878"))
    ));
    // both of these are perfectly good strings and neither says which kind of thing it is
    for guess in ["/run/kamchatka.sock", "127.0.0.1:7878", "unix:", ""] {
        let refused = protocol::address(guess).expect_err("it guessed");
        assert!(refused.contains("unix:PATH"), "{refused}");
    }
}

/// A session leaving takes away its own socket file, and not one another session put there since.
#[tokio::test]
async fn leaving_does_not_take_away_another_sessions_socket() {
    let dir = std::env::temp_dir().join(format!("kamchatka-unlink-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("no scratch directory");
    let path = dir.join("s.sock");
    let at = format!("unix:{}", path.display());

    let first = kamchatka::remote::server::Server::bind(&at)
        .await
        .expect("the first bind failed");
    std::fs::remove_file(&path).expect("the socket was not there");
    let second = kamchatka::remote::server::Server::bind(&at)
        .await
        .expect("the second bind failed");
    drop(first);

    assert!(
        path.exists(),
        "the first session took the second one's socket with it"
    );
    drop(second);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A served socket is made `0600`, since who may connect to it is who may run the `shell` tool.
#[tokio::test]
async fn a_served_socket_is_nobody_elses() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = std::env::temp_dir().join(format!("kamchatka-private-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("no scratch directory");
    let path = dir.join("s.sock");

    let served = Server::bind(&format!("unix:{}", path.display()))
        .await
        .expect("the bind failed");
    let mode = std::fs::metadata(&path)
        .expect("the socket is there")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");

    drop(served);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A socket file in the way is named rather than taken over, and the refusal says whether a
/// session is behind it, since the two want opposite things done.
#[tokio::test]
async fn a_socket_in_the_way_is_named_rather_than_taken() {
    let dir = std::env::temp_dir().join(format!("kamchatka-in-the-way-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("no scratch directory");
    let path = dir.join("s.sock");
    let at = format!("unix:{}", path.display());

    let live = Server::bind(&at).await.expect("the bind failed");
    let Err(refused) = Server::bind(&at).await else {
        panic!("a second session took a live one's socket");
    };
    assert!(
        refused.contains("there is a session listening")
            && refused.contains(&format!("--connect {at}")),
        "{refused}"
    );
    drop(live);

    // one nothing listens on, as a killed session leaves it
    drop(std::os::unix::net::UnixListener::bind(&path).expect("a socket file"));
    let Err(refused) = Server::bind(&at).await else {
        panic!("a stale socket was taken over");
    };
    assert!(
        refused.contains("nothing is listening on") && refused.contains("remove it"),
        "{refused}"
    );
    assert!(path.exists(), "the refusal took the file away itself");

    let _ = std::fs::remove_dir_all(&dir);
}
