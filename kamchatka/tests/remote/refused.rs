//! What the session does with a message it cannot read.
//!
//! note: malformed, oversized, and named for something this build has never heard of are
//! three different answers: two close the connection and one does not, because a name a later
//! version knows is not a frame a client got wrong.

use kamchatka::remote::{
    Server,
    protocol::{self, Address, Command, Message},
};
use nachalnik::Grant;
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

/// A path too long to hold a socket is refused with the limit in it, and two shorter places.
///
/// note: the refusal the kernel gives is `path must be shorter than SUN_LEN` at 130 bytes and
/// `File name too long (os error 36)` at 124 - neither names the limit, neither says how long the
/// path was, and neither offers anywhere to put it, which is all somebody has to go on when the
/// path came out of a container's working directory. `RUNNING.md` opens this section with a path
/// under `/run/user`, so a path that size is what the documentation asks a reader to type.
///
/// note: no bind is attempted and no directory is needed. The whole of the claim is that the
/// refusal arrives before the filesystem is touched, which is why it can be a path of 200 `k`s in
/// a directory that does not exist.
#[tokio::test]
async fn a_path_too_long_for_a_socket_is_refused_with_the_limit_and_a_way_out() {
    let path = "/tmp/".to_owned() + &"k".repeat(200);
    let at = format!("unix:{path}");

    let Err(refused) = Server::bind(&at).await else {
        panic!("it bound a socket at a path the kernel cannot name");
    };
    // the count, the limit, and the two answers - a shorter place on disk or a port
    assert!(refused.contains("205 bytes"), "{refused}");
    assert!(
        refused.contains(&protocol::MAX_PATH.to_string()),
        "it refused without saying the limit: {refused}"
    );
    assert!(refused.contains("$XDG_RUNTIME_DIR"), "{refused}");
    assert!(refused.contains("tcp:127.0.0.1:PORT"), "{refused}");
    // and not the whole of it, which is what the finding was about: a refusal nobody can read is
    // a refusal about a path they cannot copy
    assert!(
        !refused.contains(&"k".repeat(40)),
        "it echoed the path back whole: {refused}"
    );

    // the same at the other end, because a client that cannot reach the socket is the one person
    // who cannot tell a wrong path from a path nothing is listening at
    let refused = kamchatka::remote::Client::new(Grant::Deny, &mut Vec::new(), &mut Vec::new())
        .run(&at, "".as_bytes())
        .await
        .expect_err("it connected to a path that cannot hold a socket");
    assert!(refused.contains("205 bytes"), "{refused}");
    assert!(
        refused.contains(&protocol::MAX_PATH.to_string()),
        "it refused without saying the limit: {refused}"
    );
}

/// A path the kernel *can* name is not refused for length, only for what is in the way.
///
/// note: the other side of the check above. A limit checked as `<=` rather than `<` would refuse
/// the longest path that works, and a person shortening their path to satisfy a rule that was one
/// byte out would never find the socket.
#[test]
fn a_path_at_the_limit_is_still_a_path_a_socket_can_hold() {
    let at_limit = "/".to_owned() + &"k".repeat(protocol::MAX_PATH - 1);
    assert_eq!(at_limit.len(), protocol::MAX_PATH);
    assert_eq!(protocol::overlong_path(&at_limit), None);
    // and one more byte is one too many
    let over = format!("{at_limit}k");
    assert_eq!(protocol::overlong_path(&over), Some(protocol::MAX_PATH + 1));
}

/// A projection over the cap is refused by name, rather than sent as a frame the client cannot read.
///
/// note: the *record* half of this is closed - a record over the cap goes out as
/// `Message::Oversized`, names its sequence, and the client carries on. A projection is the other
/// half, and unlike a record it cannot be skipped: a client with no projection has nothing. It was
/// written with no size check at all, so a message past `MAX_LINE` in the context went out as a
/// frame the reader refused - which closed the connection, and the client read a refused frame as a
/// dropped one, and spent a minute reattaching to a session that had answered perfectly well every
/// time before giving up saying the session was gone.
///
/// note: what a client can do about this is read the records without a projection, so the sentence
/// says that. Abridging the projection is a decision about what every client is handed and is not
/// taken here; see `POSTPONED.md`.
///
/// note: over a socket file rather than a loopback port, because a sandbox that refuses the port
/// is a machine on which this whole suite cannot run, and one test that can is better than none.
/// On a socket rather than in the tests above because this one is about what happens once a client
/// is attached, which takes a session and a running loop.
#[tokio::test]
async fn a_projection_too_long_to_send_is_refused_rather_than_written() {
    use crate::{Socket, served_over_a_socket};
    use nachalnik::ContextItem;

    let session = served_over_a_socket("oversized-projection", Vec::new(), |app| {
        // one message larger than the cap, which is what makes the projection itself that long
        app.kernel
            .push(ContextItem::user("x".repeat(protocol::MAX_LINE)));
    })
    .await;

    let mut socket = Socket::connect(&session.at).await;
    socket.send(crate::attaching(None, None)).await;

    let Message::Failed { about, error } = socket.recv().await else {
        panic!("a projection no client can read was sent whole");
    };
    // named as itself, so a client does not read it as a drop and start a minute of attempts
    assert_eq!(about, "projection");
    assert!(error.contains("cannot be attached"), "{error}");
    assert!(error.contains(&protocol::MAX_LINE.to_string()), "{error}");
    // and told what it can do: the records are still there, and `inspect` is how one item is read
    assert!(error.contains("records are still there"), "{error}");
    assert!(error.contains("inspect"), "{error}");

    // and the session is not lost: a client that asks for the records with a watermark rather than
    // a projection is served, which is the way out the sentence points at
    let mut socket = Socket::connect(&session.at).await;
    socket
        .send(crate::attaching(Some(session.kernel.last_seq()), None))
        .await;
    // a resume is answered with the standing messages first and the `done` last; see
    // `Answered::standing`
    let mut done = false;
    for _ in 0..4 {
        if matches!(socket.recv().await, Message::Done { .. }) {
            done = true;
            break;
        }
    }
    assert!(done, "the records were refused along with the projection");

    // ended from this connection rather than a fresh one, which cannot attach: this session's
    // projection is over the cap, and that is the whole of what is under test above
    socket
        .send(Command::Submit {
            line: "/quit".to_owned(),
        })
        .await;
    session.ended().await.1.expect("the session failed");
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
    let dir = crate::common::scratch("unlink");
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
}

/// A served socket is made `0600`, since who may connect to it is who may run the `shell` tool.
#[tokio::test]
async fn a_served_socket_is_nobody_elses() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = crate::common::scratch("private");
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
}

/// A socket file in the way is named rather than taken over, and the refusal says whether a
/// session is behind it, since the two want opposite things done.
#[tokio::test]
async fn a_socket_in_the_way_is_named_rather_than_taken() {
    let dir = crate::common::scratch("in-the-way");
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
}

/// A path holding something that is not a socket is named as what it is, so a mistyped `--serve`
/// never tells anybody to delete their own file.
///
/// note: the sentence a stale socket gets ends in "remove it", which is right for a socket a
/// killed session left and wrong for a file somebody mistyped the address of - and the two were the
/// same sentence, because the only question asked was whether the path existed. The two links are
/// here as well: one that points at a socket is refused rather than followed, and one that points
/// nowhere used to reach the bind and come back as a bare `Address already in use`.
#[tokio::test]
async fn something_that_is_not_a_socket_is_named_rather_than_a_stale_one() {
    let dir = crate::common::scratch("not-a-socket");

    for (what, path) in [
        ("a file", dir.join("a.file")),
        ("a directory", dir.join("a.dir")),
        ("a link", dir.join("a.link")),
        ("a link that points nowhere", dir.join("a.nowhere")),
    ] {
        match path.extension().and_then(|it| it.to_str()) {
            Some("dir") => {
                std::fs::create_dir(&path).expect("a directory");
            }
            Some("link") => {
                std::os::unix::fs::symlink(dir.join("s.sock"), &path).expect("a link");
            }
            _ if what.ends_with("nowhere") => {
                std::os::unix::fs::symlink(dir.join("nowhere"), &path).expect("a link");
            }
            _ => {
                std::fs::write(&path, "somebody's own file").expect("a file");
            }
        };
        let at = format!("unix:{}", path.display());

        let Err(refused) = Server::bind(&at).await else {
            panic!("it listened over {what}");
        };
        assert!(
            refused.contains("not a socket"),
            "{what} was called a socket somebody should remove: {refused}"
        );
        assert!(
            !refused.contains("remove it"),
            "{what} was told to remove it: {refused}"
        );
        assert!(
            std::fs::symlink_metadata(&path).is_ok(),
            "the refusal took the {what} away itself"
        );
    }
}

/// A command the session confined is not a client: it is hung up on, where the same client run
/// unconfined is answered.
///
/// note: a socket file in the working directory, which a confined command reaches on any kernel -
/// Linux 7.1 lets it connect to a socket it may write to, and below 7.1 nothing governs a connect.
/// A client answers permission questions, so this one would be answering its own. The probe sends a
/// line that is not a message, which a session answers with a failure before it closes; a refused
/// connection closes with nothing.
#[tokio::test]
async fn a_command_the_session_confined_is_not_a_client() {
    use kamchatka::{
        sandbox::{Confinement, available},
        tools::{Careful, Limits, Shell},
    };
    use nachalnik::{OutputSink, Tool, test::call};

    let program = crate::common::program();
    if available(&program).confinement != Confinement::Full {
        eprintln!("skipped: this machine cannot confine a command");
        return;
    }
    let dir = crate::common::workdir("remote-confined-client");
    let socket = dir.join("s");
    let crate::Wired {
        mut app,
        mut events,
        mut finished,
    } = crate::wired(vec![]);
    let mut server = Server::bind(&format!("unix:{}", socket.display()))
        .await
        .expect("it listens");
    let loop_ = tokio::spawn(async move {
        let _ = server.run(&mut app, &mut events, &mut finished).await;
    });
    // a session that refuses the connection may have closed it before the line is written, or
    // leave the line unread and close it with a reset rather than an end, and each of those is a
    // hang-up. The connect stays outside, so a command that could not reach the socket at all
    // fails here rather than passing as refused
    let probe = format!(
        r#"python3 -c "
import socket
s = socket.socket(socket.AF_UNIX)
s.connect('{}')
try:
    s.sendall(b'{{\n')
    got = s.recv(4096)
except OSError:
    got = b''
print('answered' if got else 'hung up')
""#,
        socket.display()
    );

    let control = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&probe)
        .output()
        .await
        .expect("sh is here");
    let control = String::from_utf8_lossy(&control.stdout);
    assert!(control.contains("answered"), "{control}");

    let shell = Shell {
        workdir: dir.clone(),
        extra: Vec::new(),
        readable: Vec::new(),
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        policy: std::sync::Arc::new(Careful::new()),
        confiner: Some(program),
        limits: Limits::default(),
    };
    let said = tokio::time::timeout(
        PATIENCE,
        shell.invoke(
            &call("1", "shell", serde_json::json!({ "cmd": probe })),
            OutputSink::disconnected(),
        ),
    )
    .await
    .expect("the command never answered")
    .expect("the tool answers either way")
    .content
    .to_text()
    .into_owned();
    assert!(said.contains("hung up"), "{said}");
    assert!(!said.contains("answered"), "{said}");

    loop_.abort();
}
