//! A session with a socket in front of it, and what a client on the other end can see of it.
//!
//! note: a real socket rather than a pair of pipes, because half of what is being tested is what
//! happens when one goes away - and a loopback port on `:0` is portable, needs no filesystem, and
//! cannot collide with another run of this suite. The model is a `ScriptedProvider` and the
//! session is wired by `Setup`, so a whole conversation happens with no terminal, no key and no
//! network beyond the loop back into this process.
//!
//! note: most of these speak the protocol directly rather than through `remote::Client`. That is
//! deliberate: the claims are about what a *session* offers anybody who attaches, and a test that
//! could only make them through this crate's own client would be pinning the pair of them
//! together, which is the one thing a protocol exists not to be. Two at the end drive the client,
//! because what they are about is the client.

use std::{sync::Arc, time::Duration};

use kamchatka::{
    app::{App, Did, Speaker},
    remote::{
        Server,
        protocol::{self, Address, Attached, Command, Message},
    },
    wiring::{Setup, Wired},
};
use nachalnik::{
    BoxError, Capability, ContextId, DeltaSink, Event, Grant, ModelInfo, ModelRequest,
    ModelResponse, OutputSink, Provider, Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
    test::{ConstTool, ScriptedProvider, call},
};
use nachalnik_providers::OpenAiCompatible;
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

mod common;

/// How long anything in here waits before deciding it is never going to happen.
///
/// note: every wait in this suite is bounded, and that is not belt and braces. What is under test
/// is a loop that can legitimately have nothing to say for a while, so a test that got its
/// expectation wrong would otherwise hang rather than fail - which on CI is a job that is killed
/// twenty minutes later with no output about which assertion never came true.
const PATIENCE: Duration = Duration::from_secs(5);

/// Where the session's own provider points unless a test says otherwise: a port with nothing on
/// it, so that anything reaching for the endpoint fails at once rather than waiting.
const CLOSED: &str = "http://127.0.0.1:1";

/// A served session, and where to find it.
struct Served {
    /// The address, in the spelling a client would type.
    at: String,
    /// A handle to the session, for the tests that change something under a running loop. It is an
    /// `Arc`, so this is the same session the loop is driving rather than a copy of it.
    kernel: nachalnik::Kernel,
    /// The loop, which hands the `App` back when it ends so that a test can read it afterwards.
    loop_: tokio::task::JoinHandle<(App, Result<(), String>)>,
}

impl Served {
    /// Waits for the session to end and hands back what it ended with.
    async fn ended(self) -> (App, Result<(), String>) {
        tokio::time::timeout(PATIENCE, self.loop_)
            .await
            .expect("the session did not end")
            .expect("the session panicked")
    }
}

/// A session wired the way the program wires one, with a scripted model behind it and a socket in
/// front.
async fn served(script: Vec<ModelResponse>, setup: impl FnOnce(&App)) -> Served {
    served_as(None, script, setup).await
}

/// The same, under a name of the test's own, for the two that have to tell one session from
/// another.
///
/// note: named rather than left to the clock, which is what `Session::default` uses. Two sessions
/// started in the same second have the same name, and a test about telling them apart would be
/// one that passes when the machine is slow.
async fn served_as(
    name: Option<&str>,
    script: Vec<ModelResponse>,
    setup: impl FnOnce(&App),
) -> Served {
    served_at(name, CLOSED, script, setup).await
}

/// The same, talking to an endpoint of the test's choosing rather than to a closed port - which is
/// what it takes to catch a command of a client's own still in flight.
async fn served_at(
    name: Option<&str>,
    at: &str,
    script: Vec<ModelResponse>,
    setup: impl FnOnce(&App),
) -> Served {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired_at(name, at, script);
    setup(&app);

    // note: port zero, so the kernel picks one nothing else is using and `Server::address` is what
    // says which. A fixed port in a test suite is a suite that fails when somebody runs it twice
    let mut server = Server::bind("tcp:127.0.0.1:0")
        .await
        .expect("nothing would listen");
    let at = server.address();
    let kernel = app.kernel.clone();
    let loop_ = tokio::spawn(async move {
        let outcome = server.run(&mut app, &mut events, &mut finished).await;

        (app, outcome)
    });

    Served { at, kernel, loop_ }
}

/// The same `Setup` the headless suite uses, for the same reason: what these want is what
/// `main.rs` wants, minus the six tools and the child process it takes to ask Landlock anything.
fn wired(script: Vec<ModelResponse>) -> Wired {
    wired_as(None, script)
}

/// The same, under a name of the test's own where it has one.
fn wired_as(name: Option<&str>, script: Vec<ModelResponse>) -> Wired {
    wired_at(name, CLOSED, script)
}

/// The same, over an endpoint named by the caller.
fn wired_at(name: Option<&str>, at: &str, script: Vec<ModelResponse>) -> Wired {
    let wired = Setup {
        tools: Some(Vec::new()),
        compact: None,
        session_name: name.map(str::to_owned),
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new("scripted", at, "")))
    .expect("the wiring failed");
    wired
        .app
        .kernel
        .set_provider(Arc::new(ScriptedProvider::new(script)));

    wired
}

/// One end of a connection, speaking the protocol by hand.
struct Peer {
    lines: protocol::Frames<BufReader<tokio::net::tcp::OwnedReadHalf>>,
    write: tokio::net::tcp::OwnedWriteHalf,
}

impl Peer {
    /// Connects, and says nothing.
    async fn connect(at: &str) -> Self {
        let Ok(Address::Tcp(host)) = protocol::address(at) else {
            panic!("{at} is not a port");
        };
        let (read, write) = TcpStream::connect(host)
            .await
            .expect("nothing was listening")
            .into_split();

        Self {
            lines: protocol::Frames::new(BufReader::new(read)),
            write,
        }
    }

    /// Connects and attaches, and hands back the projection.
    async fn attached(at: &str) -> (Self, Attached) {
        let mut peer = Self::connect(at).await;
        peer.send(attaching(None, None)).await;
        let Message::Attached(attached) = peer.recv().await else {
            panic!("an attach was not answered with a projection");
        };

        (peer, *attached)
    }

    /// Says something.
    async fn send(&mut self, command: Command) {
        protocol::write(&mut self.write, &command)
            .await
            .expect("the session stopped listening");
    }

    /// Says something that is not a command at all.
    async fn raw(&mut self, line: &[u8]) {
        self.write.write_all(line).await.expect("could not write");
        self.write.flush().await.expect("could not write");
    }

    /// The next message, or a failure saying none came.
    async fn recv(&mut self) -> Message {
        self.next()
            .await
            .expect("the connection closed with nothing left on it")
    }

    /// The next message, where the connection closing is an answer too.
    async fn next(&mut self) -> Option<Message> {
        tokio::time::timeout(PATIENCE, protocol::read(&mut self.lines))
            .await
            .expect("nothing arrived")
            .expect("the session said something unreadable")
    }

    /// Everything up to and including the first message this says yes to.
    async fn until(&mut self, wanted: impl Fn(&Message) -> bool) -> Vec<Message> {
        let mut heard = Vec::new();
        loop {
            let message = self.recv().await;
            let done = wanted(&message);
            heard.push(message);
            if done {
                return heard;
            }
        }
    }

    /// Everything up to and including the record with this event name.
    async fn until_record(&mut self, name: &str) -> Vec<Message> {
        self.until(
            |message| matches!(message, Message::Record(record) if record.event.name() == name),
        )
        .await
    }

    /// Everything up to and including the fragment that completes this phrase.
    ///
    /// note: the thing to wait for where a turn has more than one request in it, and it is not a
    /// convenience. A turn that calls a tool emits `model.finished` twice, so waiting on the record
    /// stops half way through and every assertion after it is about a session that had not got
    /// there yet - which is how three of these were wrong the first time. What a client is actually
    /// waiting for is the words.
    async fn until_words(&mut self, wanted: &str) -> Vec<Message> {
        let mut heard = Vec::new();
        loop {
            heard.push(self.recv().await);
            if streamed(&heard).contains(wanted) {
                return heard;
            }
        }
    }

    /// Hangs up.
    async fn drop_it(self) {
        drop(self.lines);
        drop(self.write);
    }
}

/// An attach, the way a client that speaks this version of the wire writes one.
fn attaching(since: Option<u64>, session: Option<&str>) -> Command {
    Command::Attach {
        since,
        session: session.map(str::to_owned),
        version: Some(protocol::VERSION),
    }
}

/// Every record among some messages, in order.
fn records(heard: &[Message]) -> Vec<String> {
    heard
        .iter()
        .filter_map(|message| match message {
            Message::Record(record) => Some(record.event.name().to_owned()),
            _ => None,
        })
        .collect()
}

/// Whatever a model said, assembled out of the fragments.
fn streamed(heard: &[Message]) -> String {
    heard
        .iter()
        .filter_map(|message| match message {
            Message::Progress {
                event:
                    Event::ModelDelta {
                        delta: nachalnik::Delta::Text(text),
                    },
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A tool that takes a moment, so that a turn is still running while a test looks at it.
struct Slow;

#[async_trait]
impl Tool for Slow {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new("wait", "takes a moment")
    }

    async fn invoke(&self, _call: &ToolCall, _output: OutputSink) -> Result<ToolOutput, BoxError> {
        tokio::time::sleep(Duration::from_millis(120)).await;

        Ok(ToolOutput::new("waited"))
    }
}

/// A model that streams a word at a time with a gap, so that a client can attach mid-answer.
struct Trickle {
    words: Vec<String>,
}

#[async_trait]
impl Provider for Trickle {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: Some(128_000),
            ..ModelInfo::new("trickle", "trickle")
        }
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        for word in &self.words {
            deltas.text(word.clone());
            tokio::time::sleep(Duration::from_millis(40)).await;
        }

        Ok(ModelResponse::text(self.words.concat()))
    }
}

// ------------------------------------------------------------------------------- attaching

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

// ------------------------------------------------------------------- the session is not the client

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
    one.until_record("tool.started").await;

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

    two.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

// ------------------------------------------------------------------------------- questions

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

// --------------------------------------------------------------------------------- the rest

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

// ------------------------------------------------------------------------- malformed and refused

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
    // sentence with it, which is what Windows does and what Linux does when the timing goes that
    // way; the session will not drain the flood to deliver it, because not reading a peer that
    // floods is the thing under test. What is promised is that the connection ends
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

// --------------------------------------------------------------------------------- the client

/// The client's own two streams are the ones `--headless` writes.
#[tokio::test]
async fn the_client_writes_the_records_and_the_prose() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;

    // note: `/quit` is not typed until a second connection has watched the turn end, rather than
    // handed in behind the message on one slice of bytes. A command runs the moment it arrives - at
    // a prompt, down a pipe, and here - so a `/quit` queued behind a question ends the session while
    // the answer to it is still being written, and the test would be asserting about a race
    let (mut watch, _) = Peer::attached(&session.at).await;
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"ask something\n")
            .await
            .expect("could not type");
        watch.until_words("an answer to read").await;
        feed.write_all(b"/quit\n").await.expect("could not type");
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");
    let (records, prose) = (
        String::from_utf8(records).expect("the records are text"),
        String::from_utf8(prose).expect("the prose is text"),
    );

    assert!(prose.contains("an answer to read"), "{prose}");
    // and the records really are records, rather than a rendering of them
    for line in records.lines() {
        serde_json::from_str::<nachalnik::Record>(line).expect("every line is a record");
    }
    assert!(records.contains("model.finished"), "{records}");

    session.ended().await.1.expect("the session failed");
}

/// A client whose socket is pulled out from under it picks the session back up, and still leaves
/// when it is done.
///
/// note: the headline claim of `remote::client` and the whole reason `GIVE_UP`, `FIRST_WAIT` and
/// `LONGEST_WAIT` are there, and nothing drove it: the two tests that mentioned reconnection both
/// assert that it does *not* happen. What that hid is that a resume was the one command answered
/// with nothing, so a client that survived a blip was owed an answer for ever - and both the
/// things that wait on `Client::resting` stopped working. `printf 'a question\n' | kamchatka
/// --connect` over a flaky link never came back.
///
/// note: a socket of this test's own in front of the session's, because what has to go away is
/// the *connection* and not the session. Restarting the session would be a different test with a
/// different claim, and would make the resume legitimately refusable.
///
/// note: the input is closed only once the client has connected a second time. Closed any earlier
/// it detaches a client that has not yet noticed anything was wrong, which passes without going
/// anywhere near the thing under test.
#[tokio::test]
async fn a_client_that_loses_its_socket_comes_back_and_still_detaches() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;
    let Ok(Address::Tcp(host)) = protocol::address(&session.at) else {
        panic!("the suite serves a port");
    };
    let host = host.to_owned();

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", proxy.local_addr().expect("its own address"));
    let (cut, cut_now) = tokio::sync::oneshot::channel::<()>();
    let (connected, mut reconnected) = tokio::sync::mpsc::unbounded_channel::<u32>();
    tokio::spawn(async move {
        let mut cut_now = Some(cut_now);
        let mut nth = 0;
        while let Ok((mut down, _)) = proxy.accept().await {
            let mut up = TcpStream::connect(&host).await.expect("the session went");
            nth += 1;
            let _ = connected.send(nth);
            match cut_now.take() {
                // the first connection is the one that goes away under the client
                Some(cut_now) => {
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = tokio::io::copy_bidirectional(&mut down, &mut up) => {}
                            _ = cut_now => {}
                        }
                    });
                }
                None => {
                    tokio::spawn(async move {
                        let _ = tokio::io::copy_bidirectional(&mut down, &mut up).await;
                    });
                }
            }
        }
    });

    let (mut watch, _) = Peer::attached(&session.at).await;
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"ask something\n")
            .await
            .expect("could not type");
        watch.until_words("an answer to read").await;
        let _ = cut.send(());
        while reconnected.recv().await != Some(2) {}
        drop(feed);
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");
    let (records, prose) = (
        String::from_utf8(records).expect("the records are text"),
        String::from_utf8(prose).expect("the prose is text"),
    );

    assert!(
        prose.contains("the connection went; attaching again from record"),
        "the socket was cut and the client never noticed: {prose}"
    );
    // and it resumed rather than starting again: one header, which is what `Attached` prints
    assert_eq!(
        prose.matches("record(s), ").count(),
        1,
        "a resume was answered with a second projection: {prose}"
    );
    assert!(records.contains("model.finished"), "{records}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A client whose session was replaced under it starts again, rather than retrying a resume that
/// cannot ever be accepted.
///
/// note: the recovery the refusal exists for. Without it, a client that came back to a *different*
/// session at the same address sent the same impossible watermark every time it reconnected, was
/// refused identically for a minute, and gave up on a session that was there and would have had
/// it. The refusal is named `attach` rather than reported as the connection's for exactly this:
/// it is the one failure a client can do something about.
#[tokio::test]
async fn a_resume_the_session_refuses_starts_again_with_nothing() {
    let before = served_as(Some("the-one-that-went"), vec![], |_| {}).await;
    let after = served_as(Some("the-one-that-came-back"), vec![], |_| {}).await;
    let host = |at: &str| match protocol::address(at) {
        Ok(Address::Tcp(host)) => host.to_owned(),
        _ => panic!("the suite serves a port"),
    };
    let (before_at, after_at) = (host(&before.at), host(&after.at));

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", proxy.local_addr().expect("its own address"));
    let (connected, mut reconnected) = tokio::sync::mpsc::unbounded_channel::<u32>();
    tokio::spawn(async move {
        let mut nth = 0;
        while let Ok((down, _)) = proxy.accept().await {
            nth += 1;
            // the first connection reaches one session and everything after it the other, which is
            // what a host restarted under a client looks like from the client
            let to = match nth {
                1 => &before_at,
                _ => &after_at,
            };
            let up = TcpStream::connect(to).await.expect("the session went");
            let _ = connected.send(nth);
            let (down_r, mut down_w) = down.into_split();
            let (up_r, mut up_w) = up.into_split();
            tokio::spawn(async move {
                let mut lines = BufReader::new(up_r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if down_w
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                    // the first connection carries the projection through and then dies on it, so
                    // that the client is holding a session and a watermark when it goes
                    if nth == 1 && line.contains("\"is\":\"attached\"") {
                        return;
                    }
                }
            });
            tokio::spawn(async move {
                let mut down_r = BufReader::new(down_r);
                let _ = tokio::io::copy(&mut down_r, &mut up_w).await;
            });
        }
    });

    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        // the refused resume is the second connection, and the third is the one that works
        while reconnected.recv().await != Some(3) {}
        let _ = feed.write_all(b"").await;
        drop(feed);
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client kept asking for a resume nobody could give it")
    .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    // refused once, rather than once per attempt for a minute
    assert_eq!(
        prose
            .matches("attach: you are resuming a session this is not")
            .count(),
        1,
        "{prose}"
    );
    // and then attached to the session that is there rather than to the one it remembered. The
    // header is what says it attached; the refusal names the new session too, and asserting on the
    // name alone passes without the client having got anywhere
    assert!(
        prose.contains("--- the-one-that-went ·") && prose.contains("--- the-one-that-came-back ·"),
        "the client never attached to the session that replaced the first: {prose}"
    );

    quit(&before.at).await;
    quit(&after.at).await;
    before.ended().await.1.expect("the first session failed");
    after.ended().await.1.expect("the second session failed");
}

/// A command the socket took with it is an answer that is never coming, and nothing waits for it.
///
/// note: the other half of the reconnection, and the half the test above cannot reach: there the
/// only thing owed an answer was the attach. A client counts answers owed so that a piped-in
/// question is not asked and abandoned - but a `submit` that died in the socket is owed one for
/// ever, and a count that never came down is a `--connect` in a script that hangs after a blip
/// instead of leaving. The session cannot help here, because it never saw the command.
#[tokio::test]
async fn a_client_does_not_wait_for_an_answer_the_dead_socket_took_with_it() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;
    let Ok(Address::Tcp(host)) = protocol::address(&session.at) else {
        panic!("the suite serves a port");
    };
    let host = host.to_owned();

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", proxy.local_addr().expect("its own address"));
    let (connected, mut reconnected) = tokio::sync::mpsc::unbounded_channel::<u32>();
    tokio::spawn(async move {
        let mut nth = 0;
        while let Ok((down, _)) = proxy.accept().await {
            let up = TcpStream::connect(&host).await.expect("the session went");
            nth += 1;
            let _ = connected.send(nth);
            let (down_r, mut down_w) = down.into_split();
            let (mut up_r, mut up_w) = up.into_split();
            let (die, dying) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::io::copy(&mut up_r, &mut down_w) => {}
                    _ = dying => {}
                }
            });
            tokio::spawn(async move {
                let mut lines = BufReader::new(down_r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // an attach goes through; on the first connection the line somebody typed is
                    // taken off the wire and the connection dies holding it
                    if nth == 1 && !line.contains("\"do\":\"attach\"") {
                        let _ = die.send(());

                        return;
                    }
                    if up_w
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });

    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"ask something\n")
            .await
            .expect("could not type");
        while reconnected.recv().await != Some(2) {}
        drop(feed);
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client waited for an answer nobody was going to send")
    .expect("the client failed");

    // the session never heard the line, which is what makes the answer one that cannot arrive
    let (watch, attached) = Peer::attached(&session.at).await;
    assert!(
        !attached
            .conversation
            .iter()
            .any(|line| line.text.contains("ask something")),
        "the proxy handed the command on after all"
    );
    watch.drop_it().await;

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// The client answers a question with the same three letters the terminal's panel takes.
#[tokio::test]
async fn the_client_answers_a_question_with_the_keys_the_panel_uses() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("allowed"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    // note: `y` is typed only once the question has actually been asked, because the client reads
    // it as an answer only while one is open - which is exactly how the terminal's panel behaves,
    // and is the thing this test is about. Sent ahead of the question it is a *message* of `y`
    let (mut watch, _) = Peer::attached(&session.at).await;
    let (mut feed, input) = tokio::io::duplex(256);
    tokio::spawn(async move {
        feed.write_all(b"go\n").await.expect("could not type");
        watch.until_record("permission.requested").await;
        feed.write_all(b"y\n").await.expect("could not type");
        // and then the input closes, which detaches rather than ending anybody's session - so this
        // is also where the client's own rule is exercised: it stays for the turn it just let
        // through, and comes back when the session says it has nothing left to do
    });

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");

    assert!(prose.contains("wants to run peek"), "{prose}");
    assert!(prose.contains("peek: allow"), "{prose}");
    // note: the *record* that the tool ran, rather than the words the model said afterwards. Both
    // happened, and only one of them is something this protocol promises to deliver: an answer
    // arrives as fragments, which are in no log, and a client that has just detached is exactly the
    // one they are allowed to have gone past
    assert!(prose.contains("tokens"), "the tool never finished: {prose}");

    // the client left and the session did not, which is the whole invariant
    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// What the program says for itself reaches a client, because a command that was silent is a verb
/// that does nothing visible.
///
/// note: a refused command is the cheapest line the program says for itself. It used to be a client
/// arriving, which said the same thing for nothing - until arrivals moved to the trace, because a
/// browser reconnecting on a flaky link put one in the conversation every second.
#[tokio::test]
async fn the_program_has_one_voice_and_every_client_hears_it() {
    let refused = "is not a number of tokens";
    let session = served(vec![], |_| {}).await;

    let (mut one, _) = Peer::attached(&session.at).await;
    let (mut two, _) = Peer::attached(&session.at).await;

    // a line said *after* both are attached, which is the only kind either of them can hear: what a
    // client was told before it arrived is in the conversation it was handed
    one.send(Command::Submit {
        line: "/spend nonsense".to_owned(),
    })
    .await;

    // the second client asked for nothing and hears it, which is the whole of the claim
    let heard = two
        .until(|message| matches!(message, Message::Said { text, .. } if text.contains(refused)))
        .await;
    assert!(heard.iter().any(
        |message| matches!(message, Message::Said { speaker, .. } if *speaker == Speaker::Error)
    ));

    // and a client that arrives afterwards reads it once - in the conversation it is handed, rather
    // than there and again underneath. Its subscription starts where its projection was taken, and
    // those are taken together for exactly this reason
    let (mut three, arriving) = Peer::attached(&session.at).await;
    assert!(
        arriving
            .conversation
            .iter()
            .any(|line| line.text.contains(refused)),
        "a line said before it arrived is not in the conversation it was handed"
    );
    three
        .send(Command::Submit {
            line: "/quit".to_owned(),
        })
        .await;
    let ending = three
        .until(|message| matches!(message, Message::Replied { .. }))
        .await;
    assert!(
        !ending
            .iter()
            .any(|message| matches!(message, Message::Said { text, .. } if text.contains(refused))),
        "a line the projection already carried arrived again underneath it: {ending:?}"
    );

    session.ended().await.1.expect("the session failed");
}

/// A model change reaches every client, because nothing else would tell them.
///
/// note: the one change to a session that is in no record. `/model` and `/provider` finish inside
/// the `Dialect` the kernel already holds rather than by replacing the kernel's provider, so the
/// slot never changes and `model.changed` is never emitted - a client went on naming the model
/// before it until something happened to make it ask for a fresh projection. A browser's header is
/// where that showed, and a second client would never have found out at all.
///
/// note: driven by replacing the provider rather than by typing `/model`, because the suite's
/// endpoint is a port nothing listens on and a switch is a round trip to it. What is under test is
/// the watch in `Serving::pump`, and what it watches is `Kernel::model_info` - which this moves the
/// honest way.
#[tokio::test]
async fn a_model_change_reaches_every_client() {
    let session = served(vec![], |_| {}).await;
    // two, because the claim is that it is broadcast: the one that would have asked for a
    // projection anyway is not the one this is for
    let (mut one, attached) = Peer::attached(&session.at).await;
    let (mut two, _) = Peer::attached(&session.at).await;
    let before = attached.model.expect("the suite wires a provider").model;

    // the session starts talking to something else, which is a thing that happens to a session.
    // `Trickle` rather than a second `ScriptedProvider`, because those report the same name and a
    // change nothing can see is not one
    let now = Arc::new(Trickle { words: Vec::new() });
    let named = now.info().model.clone();
    assert_ne!(
        named, before,
        "the two providers have to differ to say anything"
    );
    session.kernel.set_provider(now);

    for (who, peer) in [
        ("the client that was here", &mut one),
        ("the other", &mut two),
    ] {
        let heard = peer.until(|m| matches!(m, Message::Model { .. })).await;
        let Some(Message::Model { model }) = heard.last() else {
            unreachable!("the loop above only ends on one")
        };
        assert_eq!(
            model.as_ref().map(|it| it.model.as_str()),
            Some(named.as_str()),
            "{who} was told the wrong model"
        );
    }

    one.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}

/// A loop that is not `Server::run` can serve the same session, which is what lets one be driven
/// from a desk and a phone at once.
///
/// note: `main.rs`'s drawn loop is the other caller and cannot be tested from here - it wants a
/// terminal, and every test of this binary pipes its stdout. What *is* testable is the claim
/// underneath it: that `Serving` is the whole of what a loop needs, so a loop written here can hold
/// the `App` and answer clients with no `Server::run` anywhere. If this compiles and passes, the
/// seam is real; if it needed one private thing more, it would not.
///
/// note: the loop is the shape of the one in `main.rs` rather than a convenience: `pump` before it
/// waits, `arrived` and `attend` for a connection, `asked` and `answer` for a command. What it
/// leaves out is the drawing.
#[tokio::test]
async fn a_loop_of_somebody_elses_can_serve_the_session() {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(vec![ModelResponse::text(
        "an answer from somebody else's loop",
    )]);
    let server = Server::bind("tcp:127.0.0.1:0")
        .await
        .expect("nothing would listen");
    let at = server.address();

    let loop_ = tokio::spawn(async move {
        let mut serving = kamchatka::remote::Serving::new(&app);
        while !app.quit {
            serving.pump(&app);
            tokio::select! {
                arrived = server.arrived() => {
                    if let Ok(arrived) = arrived {
                        serving.attend(&mut app, arrived);
                    }
                }
                Some(ask) = serving.asked() => serving.answer(&mut app, ask).await,
                event = events.recv() => match event {
                    Ok(event) => app.on_event(event),
                    Err(_) => break,
                },
                Some(outcome) = finished.recv() => {
                    while let Ok(event) = events.try_recv() {
                        app.on_event(event);
                    }
                    app.on_outcome(outcome);
                }
            }
        }
        serving.pump(&app);
        app.kernel.finish();
        serving.last(&app);

        app
    });

    // and from the outside it is a session like any other: a projection, a turn, and the answer
    let (mut peer, _) = Peer::attached(&at).await;
    peer.send(Command::Submit {
        line: "ask it something".to_owned(),
    })
    .await;
    let heard = peer
        .until_words("an answer from somebody else's loop")
        .await;
    assert!(
        !records(&heard).is_empty(),
        "the records never arrived: {heard:?}"
    );
    // the arrival is a trace line, and a loop of somebody else's gets that for nothing
    peer.send(Command::Project).await;
    let seen = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = seen
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one")
    };
    assert!(
        now.trace.iter().any(|line| line.name == "client.attached"),
        "{:?}",
        now.trace
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let app = tokio::time::timeout(PATIENCE, loop_)
        .await
        .expect("the loop did not end")
        .expect("the loop panicked");
    assert!(app.quit, "a `/quit` from a client did not reach the loop");
}

/// A restart from a client hands the session back to be built again, and lets go of everybody.
///
/// note: two claims, and they are the two halves `main` leans on. The loop returning with
/// `restart` set and `quit` clear is what tells it to wire a second session rather than stop; and
/// every connection ending is what stops a client reading a session nobody is in any more. Neither
/// is visible from the other side - a client sees a socket close, and the loop sees a flag - so
/// this is the one place both are true at once.
///
/// note: the connections are let go of by the `Serving` going away with the loop, which closes the
/// voice every one of them is reading - the same mechanism, and the same last flush, that ends
/// them on `/quit`. It is worth a test rather than an assumption because nothing else would close
/// them: every connection owns a `Kernel` handle, so the session being replaced leaves the old one
/// alive inside the task reading it, and a client would go on being served a session that had been
/// written out and abandoned.
///
/// note: two clients, because one would not catch a parting that reached only whoever spoke last.
/// The second says nothing at all and is let go of on the same terms.
#[tokio::test]
async fn a_restart_from_a_client_ends_the_session_and_lets_go_of_everybody() {
    let session = served(vec![ModelResponse::text("unused")], |_| {}).await;
    let (mut asked, _) = Peer::attached(&session.at).await;
    let (mut watching, _) = Peer::attached(&session.at).await;

    asked
        .send(Command::Submit {
            line: "/restart".to_owned(),
        })
        .await;

    let (app, outcome) = session.ended().await;
    outcome.expect("the session failed");
    assert!(
        app.restart,
        "a `/restart` from a client did not reach the loop"
    );
    assert!(app.leaving(), "the loop was told to let go of the session");
    assert!(
        !app.quit,
        "a restart is not a quit: `main` wires another session rather than stopping"
    );

    for (which, peer) in [
        ("the one that asked", &mut asked),
        ("the other", &mut watching),
    ] {
        // note: drained to the close rather than read once. What the session has to say on the way
        // out goes first, and the connection ending is the message this is about - a `while let`
        // that never ends is the failure, and `Peer::next` is what bounds it
        let mut heard = Vec::new();
        while let Some(message) = peer.next().await {
            heard.push(message);
        }
        assert!(
            heard.iter().any(|message| matches!(
                message,
                Message::Record(record) if record.event.name() == "session.finished"
            )),
            "{which} was cut off without being told the session had ended: {heard:?}"
        );
    }
}

/// Ends a session from a connection of its own, for the tests whose own peer has been closed.
async fn quit(at: &str) {
    let (mut peer, _) = Peer::attached(at).await;
    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
}

// --------------------------------------------------------------------------------- the program

/// The two flags, the socket file, and a whole session driven from one process to another.
///
/// note: the only test here that is about the *program* rather than the library. What it can catch
/// and nothing above it can: a flag wired to the wrong loop, a socket file left behind, a client
/// that tries to reach a model of its own, and the mode decision - a served run has no screen and
/// is not a headless one either, and the version of that decision this replaced announced a pipe
/// nobody had mentioned.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_program_serves_a_socket_and_a_second_one_drives_it() {
    let base = common::endpoint(vec![answer("what the other end reads")]).await;
    let dir = common::scratch("served");
    let socket = dir.join("kamchatka.sock");

    let host = std::process::Command::new(common::program())
        .args(["--no-record", "-m", "nothing", "--serve"])
        .arg(format!("unix:{}", socket.display()))
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the host did not start");

    // the socket appears when the session is ready for somebody, and not before
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(socket.exists(), "nothing ever listened at {socket:?}");

    let client = tokio::task::spawn_blocking({
        let socket = socket.clone();
        // note: the question, and then the input closes. It does not type `/quit`, because a
        // command runs the moment it arrives and one queued behind a question ends the session
        // while the answer to it is still being written. Closing the input detaches instead, and
        // the client stays for the turn it started - which is the rule, and is what makes this
        // deterministic rather than a race
        move || connect(&socket, b"say something\n")
    })
    .await
    .expect("the client panicked");

    let read = String::from_utf8_lossy(&client.stderr);
    assert!(
        read.contains("what the other end reads"),
        "the client never read the answer: {read}"
    );
    // note: a served session greets nobody, and this is where that is checked because the greeting
    // is `main.rs`'s. It is the terminal's - `ctrl+p` shows the next request, `F1` lists the keys -
    // and a served session has no keys of this program's to press: `--serve` is neither a screen
    // nor a pipe, and the condition that decided this had only ever been asked which of those two
    // it was. It went to every client that ever attached, because the greeting goes into the
    // conversation and the conversation is in every projection
    //
    // note: only a build with a screen in it has a greeting to get wrong, so under
    // `--no-default-features` this passes by having nothing to find. `--all-features` is where it
    // means something, and that is the configuration `cargo test --workspace` uses
    assert!(
        !read.contains("F1 lists the keys"),
        "a served session told a client about the terminal's keys: {read}"
    );
    // and its stdout is the record stream, the same as a headless run's
    for line in String::from_utf8_lossy(&client.stdout).lines() {
        serde_json::from_str::<nachalnik::Record>(line).expect("every line is a record");
    }

    // the first client left and the session did not, so a second one can end it
    tokio::task::spawn_blocking({
        let socket = socket.clone();
        move || connect(&socket, b"/quit\n")
    })
    .await
    .expect("the second client panicked");

    let host = tokio::task::spawn_blocking(move || host.wait_with_output())
        .await
        .expect("the host panicked")
        .expect("the host did not finish");
    let said = String::from_utf8_lossy(&host.stdout) + String::from_utf8_lossy(&host.stderr);
    // note: a served run has no screen and no record stream on stdout, so it is the one mode in
    // which the program can simply say things - and it has to say this one, because between
    // starting and being stopped it otherwise prints nothing at all
    assert!(
        said.contains(&format!("serving on unix:{}", socket.display())),
        "the host did not say where it was listening: {said}"
    );
    // note: a socket file that outlives its listener is a path every later client connects to and
    // then hangs on, which is a worse failure than not being able to connect at all
    assert!(!socket.exists(), "the socket file was left behind");
}

/// Runs `--connect` against a socket with these lines typed at it, and waits for it.
///
/// note: no model, no key and no endpoint. A client wires nothing up, and the day this needs one
/// again is the day `--connect` has stopped being a client.
#[cfg(unix)]
fn connect(socket: &std::path::Path, typed: &[u8]) -> std::process::Output {
    use std::io::Write as _;

    let mut client = std::process::Command::new(common::program())
        .arg("--connect")
        .arg(format!("unix:{}", socket.display()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the client did not start");
    {
        let mut pipe = client.stdin.take().expect("a pipe");
        pipe.write_all(typed).expect("could not type");
    }

    client
        .wait_with_output()
        .expect("the client did not finish")
}

/// One SSE answer, in the shape the provider actually reads.
#[cfg(unix)]
fn answer(text: &str) -> String {
    format!(
        "data: {}\n\ndata: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "content": text},
               "finish_reason": null}]}),
        json!({"id": "1", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
               "usage": {"prompt_tokens": 700, "completion_tokens": 500, "total_tokens": 1200}})
    )
}

/// A client asked for a session that is not there says so, rather than trying five times.
///
/// note: the distinction the retry loop turns on. A connection that *drops* may well come back, and
/// picking it up from the last record is the point; one that was never made is a wrong address or a
/// session nobody started, and five attempts at it is five times as long before anybody is told.
#[tokio::test]
async fn a_client_that_finds_nothing_there_says_so_at_once() {
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    let refused = kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run("tcp:127.0.0.1:1", "".as_bytes())
        .await
        .expect_err("it connected to nothing");

    assert!(refused.contains("could not reach 127.0.0.1:1"), "{refused}");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        !prose.contains("attaching again"),
        "it reconnected to a connection it never had: {prose}"
    );
}

/// A question piped in, and the input closed behind it, still waits for the answer.
///
/// note: the shape of `echo "question" | kamchatka --connect`, and the rule is `--headless`'s: a
/// script that pipes one question in and goes away is asking for the answer, not for the question
/// to be asked and abandoned. What it must *not* do is end the session on the way out - that
/// belongs to whoever is serving it - so the two halves of this are that the client comes back with
/// the answer and that the session is still there afterwards.
#[tokio::test]
async fn a_question_piped_in_waits_for_its_answer_and_leaves_the_session() {
    let session = served(vec![ModelResponse::text("the whole answer")], |_| {}).await;

    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"say something\n")
        .await
        .expect("could not type");
    drop(feed);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("the whole answer"), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A question the piped-in line raised is answered on the way out rather than abandoned.
///
/// note: the hole the test above could not see, because its model asks for no tools. A turn paused
/// on a permission question is not *running*, so `busy` comes back false while the kernel sits in
/// `Deciding` - and a client reading that alone took it for the end of the turn, printed the
/// question, and exited `0`. What it left behind is the half that matters: a served session
/// waiting on an answer that no longer had anywhere to come from, for as long as the process
/// lived. So the two halves here are that the answer is given and said out loud, and that the turn
/// it was blocking reaches its end.
///
/// note: `deny` rather than `allow`, and the assertion names the refusal, because the default is
/// the load-bearing half: a run nobody is watching should not be able to do a thing nobody
/// allowed. `--on-ask allow` is one flag away for anybody who means it, and it is the same flag
/// and the same word `--headless` has always taken.
#[tokio::test]
async fn a_question_nobody_is_left_to_answer_is_answered_on_the_way_out() {
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("it would not let me"),
    ];
    let session = served(script, |app| {
        app.kernel.add_tool(Arc::new(
            ConstTool::new("peek", "the answer").with_capabilities([Capability::fs("read")]),
        ));
    })
    .await;

    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"go\n").await.expect("could not type");
    drop(feed);

    // note: under `PATIENCE`, because the two ways to get this wrong fail in opposite directions.
    // Leaving the question unanswered ends the client early and trips the assertions below; not
    // letting it leave at all is a client waiting on an answer it is itself supposed to give, and
    // that one hangs rather than fails
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&session.at, BufReader::new(input)),
    )
    .await
    .expect("the client never left")
    .expect("the client failed");

    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        prose.contains("nobody is here to answer for `peek`"),
        "it left without saying what it did with the question: {prose}"
    );
    // and the turn the question was holding up got to the other side of it
    assert!(prose.contains("it would not let me"), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A session speaking a version this client does not is left rather than attached to again.
///
/// note: the client sends the version this build speaks, so the only way to be refused for one is
/// to be a different build - and both ends of a connection here are this one. What stands in is a
/// relay that says one word differently on the way past, which is cheaper than a second
/// implementation of the protocol for the sake of one number.
///
/// note: what it is pinning is that a refusal a fresh attach cannot mend ends the client on the
/// session's own sentence. Treated like the watermark refusal beside it, the client reattached, was
/// refused identically, and left a minute later saying the session had not answered for sixty
/// seconds - which is the one thing that had not happened. The input is held open throughout, so
/// nothing but the refusal can be what ended it.
#[tokio::test]
async fn a_version_the_session_refuses_is_not_attached_to_again() {
    let session = served(vec![], |_| {}).await;
    let ahead = format!("\"version\":{}", protocol::VERSION + 1);
    let at = rewriting(
        &session.at,
        move |line| line.replace(&format!("\"version\":{}", protocol::VERSION), &ahead),
        |line| line,
    )
    .await;

    let (_feed, input) = tokio::io::duplex(256);
    let (mut records, mut prose) = (Vec::new(), Vec::new());
    let left = tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client kept reattaching to a session that had already answered");

    let refused = left.expect_err("a refused version read as a session worth carrying on with");
    assert!(refused.contains("the older end is this one"), "{refused}");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        !prose.contains("attaching again"),
        "it went back for more of the same answer: {prose}"
    );

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// An answer in a name this build has never heard of is still an answer, and is not waited for.
///
/// note: the hole the forward-compatibility work would otherwise have left open with its own
/// escape hatch in it. `Message::Unknown` carries no payload - `#[serde(other)]` takes a unit
/// variant - so a client owed an answer and handed one it cannot read cannot tell it from a
/// broadcast. A count that never came back down was stdin closing that never detached and a
/// session going quiet that never ended it, for the rest of the connection: `printf 'a question\n'
/// | kamchatka --connect` against a session one version ahead hung.
///
/// note: what this deliberately does **not** assert is that the answer arrives. Counting an
/// unrecognised message as an answer is a decision about which way to be wrong, and this is the
/// cost of it: the client leaves on a message it could not have printed, where it used to wait for
/// one that had already come and was never coming again. The relay renames the one message that is
/// this client's answer, which is a session a version ahead answering an older client, and the
/// claim is that it survives it rather than that it understood it.
#[tokio::test]
async fn an_answer_this_build_cannot_read_still_counts_as_one() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;
    let at = rewriting(
        &session.at,
        |line| line,
        |line| line.replace("\"is\":\"replied\"", "\"is\":\"replied-and-then-some\""),
    )
    .await;

    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"ask something\n")
        .await
        .expect("could not type");
    drop(feed);

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, BufReader::new(input)),
    )
    .await
    .expect("the client waited for an answer it had already been handed")
    .expect("the client failed");

    // and the session is still a session, which is what the client leaving is not allowed to cost
    let (watch, attached) = Peer::attached(&session.at).await;
    assert!(
        attached
            .conversation
            .iter()
            .any(|line| line.text.contains("ask something")),
        "the line never reached the session: {:?}",
        attached.conversation
    );
    watch.drop_it().await;

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// A socket in front of a session, with every line said differently on the way past.
///
/// note: what the two tests above need is a session that speaks something this build does not, and
/// both ends of a connection here are this build. One word rewritten on the wire is how a version
/// that does not exist gets said out loud.
async fn rewriting(
    at: &str,
    to_session: impl Fn(String) -> String + Clone + Send + 'static,
    to_client: impl Fn(String) -> String + Clone + Send + 'static,
) -> String {
    let Ok(Address::Tcp(host)) = protocol::address(at) else {
        panic!("the suite serves a port");
    };
    let host = host.to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = format!("tcp:{}", listener.local_addr().expect("its own address"));
    tokio::spawn(async move {
        while let Ok((down, _)) = listener.accept().await {
            let up = TcpStream::connect(&host).await.expect("the session went");
            let (down_r, down_w) = down.into_split();
            let (up_r, up_w) = up.into_split();
            tokio::spawn(relaying(down_r, up_w, to_session.clone()));
            tokio::spawn(relaying(up_r, down_w, to_client.clone()));
        }
    });

    at
}

/// Copies one direction of a connection, a line at a time.
async fn relaying(
    read: tokio::net::tcp::OwnedReadHalf,
    mut write: tokio::net::tcp::OwnedWriteHalf,
    say: impl Fn(String) -> String,
) {
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if write
            .write_all(format!("{}\n", say(line)).as_bytes())
            .await
            .is_err()
        {
            return;
        }
    }
}

/// `/quit` from a client ends the session, and reads as an ending rather than as a dropped socket.
///
/// note: the difference is five reconnection attempts at a session that did exactly what it was
/// told. What tells the two apart is the `session.finished` record, which is why this asserts on
/// the record as well as on the outcome - a client that returned `Ok` having never been told the
/// session ended got there by accident.
#[tokio::test]
async fn quitting_from_a_client_reads_as_an_ending() {
    let session = served(vec![], |_| {}).await;

    // note: the input stays open, which is what makes this about the *session* ending rather than
    // about the input closing. A client whose stdin has gone leaves as soon as the session is
    // quiet, and would be gone before the last record was written; this one has nothing left to
    // type and is still listening, so what ends it is the socket closing behind a session that
    // said it had finished
    let (mut feed, input) = tokio::io::duplex(256);
    feed.write_all(b"/quit\n").await.expect("could not type");

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("quitting was read as a failure");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(
        !prose.contains("attaching again"),
        "it tried to reconnect to a session it had just ended: {prose}"
    );
    assert!(
        String::from_utf8_lossy(&records).contains("session.finished"),
        "the ending was never sent to the client still attached for it"
    );

    session.ended().await.1.expect("the session failed");
}

/// A served run says the address it *got*, while it is still running, and a client can reach it.
///
/// note: `tcp:127.0.0.1:0` is the case this is for. Asking the kernel for a port is the sensible
/// thing to do and it leaves the address as the one fact the person who typed the flag does not
/// have - so a script starts a session, reads the line, and connects to what it says.
/// `Server::address` asks the socket rather than repeating the argument, which is the whole of why
/// that works, and this is what says so.
///
/// note: read off the *live* pipe rather than from a finished process, because a script does not
/// get to wait for the session to end before connecting to it.
///
/// note: measured, and it is worth saying which half of it is its own. That a served run says
/// something on stdout is shared with `the_program_serves_a_socket_and_a_second_one_drives_it`
/// above, which reads it after the process ends; that `Server::address` asks the socket is covered
/// by every test in this file, because they all connect to what it returns. What is only here is
/// the port nobody chose, read while the session is still up and connected to - which is the whole
/// of how a script is meant to use this.
///
/// note: what this is **not** about is flushing, and the first version of this note said it was. A
/// session whose stdout had been redirected looked as though it were holding the line back, and
/// that host had simply failed to bind, with the reason on stderr where it belonged. Rust's
/// `Stdout` is a `LineWriter` whatever it points at, so `println!` has already flushed by the time
/// it returns; the flush added here on the strength of that reading failed nothing when it was
/// taken away again, and was taken away.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_served_run_says_the_address_it_got_and_a_client_can_reach_it() {
    let base = common::endpoint(vec![answer("reached through a port nobody chose")]).await;

    let mut host = tokio::process::Command::new(common::program())
        .args(["--no-record", "-m", "nothing", "--serve", "tcp:127.0.0.1:0"])
        .env("KAMCHATKA_BASE_URL", &base)
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // so a failing assertion below does not leave a session serving for the rest of the run
        .kill_on_drop(true)
        .spawn()
        .expect("the host did not start");

    let mut said = BufReader::new(host.stdout.take().expect("a pipe")).lines();
    let line = tokio::time::timeout(PATIENCE, said.next_line())
        .await
        .expect("the host never said where it was listening")
        .expect("the host's output stopped")
        .expect("the host said nothing at all");
    let at = line
        .split_whitespace()
        .next_back()
        .expect("the line named no address");
    assert!(at.starts_with("tcp:127.0.0.1:"), "{line}");
    assert!(
        !at.ends_with(":0"),
        "it reported the port it asked for, not the one it got"
    );

    // and the address it printed is one a client can actually reach
    let (mut peer, attached) = Peer::attached(at).await;
    assert_eq!(attached.items.len(), 0);
    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    tokio::time::timeout(PATIENCE, host.wait())
        .await
        .expect("the session did not end")
        .expect("the host did not finish");
}

/// `ctrl+c` at a client stops the turn, and a second one detaches without ending the session.
///
/// note: the two stages, and the invariant between them. The first press is for the *turn* and
/// keeps what arrived; the second is for this process, and the session carries on without it -
/// which is the one thing a client must never decide for somebody else. There was only ever one
/// stage: a second press sent another interrupt and printed the same line, and since
/// `tokio::signal::ctrl_c` does not put the default handler back, there was no way out of a
/// `--connect` at all short of killing it.
///
/// note: a child process, because `ctrl+c` is a *signal* and there is no other way to send one.
/// `#[cfg(unix)]` for the same reason `ctrl_c_stops_a_headless_run_rather_than_killing_it` is.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_at_a_client_stops_the_turn_and_then_detaches() {
    let session = served(vec![ModelResponse::text("an answer to read")], |_| {}).await;

    // stdin is a pipe this test holds open and never writes to, so nothing but the signal can end
    // this client - which is what makes the assertion about the signal
    let mut client = tokio::process::Command::new(common::program())
        .args(["--connect", &session.at])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the client did not start");
    let mut read = BufReader::new(client.stderr.take().expect("a pipe")).lines();
    // note: held rather than left in the `Child`, and this is the trap in writing this test.
    // `Child::wait` closes stdin before it waits, stdin closing is the *other* way this client
    // leaves, and a test that reached for `wait` passed with the second stage taken out again
    let _stdin = client.stdin.take().expect("a pipe");

    // attached, and therefore in the loop with the signal branch armed
    let mut prose = String::new();
    while !prose.contains("a line is a message") {
        let line = tokio::time::timeout(PATIENCE, read.next_line())
            .await
            .expect("the client said nothing")
            .expect("the client's output stopped")
            .expect("the client ended before it was interrupted");
        prose.push_str(&line);
        prose.push('\n');
    }

    let id = client.id().expect("it is running").to_string();
    let interrupt = |id: &str| {
        std::process::Command::new("kill")
            .args(["-INT", id])
            .status()
            .expect("`kill` is on the path")
    };
    assert!(interrupt(&id).success());
    while !prose.contains("asked it to stop") {
        let line = tokio::time::timeout(PATIENCE, read.next_line())
            .await
            .expect("the first press was not heard")
            .expect("the client's output stopped")
            .expect("the client left on the first press");
        prose.push_str(&line);
        prose.push('\n');
    }
    // and it is still attached, which is the whole of what the first stage means
    assert!(
        client.try_wait().expect("it was spawned").is_none(),
        "the first press left: {prose}"
    );

    assert!(interrupt(&id).success());
    let status = {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            match client.try_wait().expect("it was spawned") {
                Some(status) => break status,
                None if std::time::Instant::now() > deadline => {
                    panic!("the second press did not detach it: {prose}")
                }
                None => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
    };
    assert!(status.success(), "detaching is not a failure: {prose}");
    drop(_stdin);

    // the session is still there, which is the invariant the whole module is built on
    let (peer, _) = Peer::attached(&session.at).await;
    peer.drop_it().await;
    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
}

/// `/cleanup` reaches every attached client, not only the one that typed it.
///
/// note: a broadcast rather than an answer, for the reason every other notice is one: the program
/// has one voice, and a session two people are watching does not have half of it cleared. The
/// second client here never sends anything.
#[tokio::test]
async fn clearing_the_notices_is_said_to_everybody() {
    let served = served(vec![], |_| {}).await;
    let (mut one, _) = Peer::attached(&served.at).await;
    let (mut two, _) = Peer::attached(&served.at).await;

    // something for it to take away, said by a command rather than invented: `/seams` answers with
    // a page, and the arrival of a second client is a note in its own right
    one.send(Command::Submit {
        line: "/cleanup".to_owned(),
    })
    .await;

    for (who, peer) in [("the client that asked", &mut one), ("the other", &mut two)] {
        let heard = peer.until(|m| matches!(m, Message::Cleared)).await;
        assert!(
            heard.iter().any(|m| matches!(m, Message::Cleared)),
            "{who} was not told the lines went: {heard:?}"
        );
    }

    one.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// And a line said after a cleanup still reaches a client, which is the half that breaks
/// quietly.
///
/// note: this is the test that is worth having and the one above is the feature. The server reads
/// the program's lines through `App::notes`, which is a watermark over the filtered sequence -
/// safe only while that sequence grows, and `/cleanup` empties it. A server that did not notice
/// would hold a mark of three against a sequence of nothing and swallow the next three lines,
/// silently, for the rest of the session. Two lines are cleared here so that the mark is high
/// enough for the swallowing to be visible.
#[tokio::test]
async fn a_line_said_after_a_cleanup_is_not_swallowed() {
    let served = served(vec![], |_| {}).await;
    let (mut peer, _) = Peer::attached(&served.at).await;

    for line in ["/seams", "/budget", "/cleanup"] {
        peer.send(Command::Submit {
            line: line.to_owned(),
        })
        .await;
    }
    peer.until(|m| matches!(m, Message::Cleared)).await;

    // and now something to say. `/seams` says its piece as a page rather than a line, so this asks
    // for one that is refused - a refusal is a line, and it is the shape a session says most of
    // what it says in
    peer.send(Command::Submit {
        line: "/limit nonsense".to_owned(),
    })
    .await;
    let heard = peer
        .until(|m| matches!(m, Message::Said { .. } | Message::Done { .. }))
        .await;
    assert!(
        heard.iter().any(|m| matches!(m, Message::Said { .. })),
        "the line after a cleanup was swallowed: {heard:?}"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// `project` answers with the figures again and leaves the stream where it was.
///
/// note: the distinction the whole command exists for, and it is about what the answer *means*
/// rather than about what follows it. A client takes an `attached` as start again - it has just
/// been handed the conversation and the stream that carries on from it - so one that could not
/// tell the two apart would wipe its own screen to refresh a token count. So what is asserted is
/// the negative: a `projected`, and no `attached` behind it.
#[tokio::test]
async fn a_projection_can_be_asked_for_again_without_starting_over() {
    let script = vec![ModelResponse::text("the kernel is a state machine")];
    let served = served(script, |_| {}).await;
    let (mut peer, first) = Peer::attached(&served.at).await;

    peer.send(Command::Submit {
        line: "what does the kernel do?".to_owned(),
    })
    .await;
    peer.until_record("model.finished").await;

    peer.send(Command::Project).await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(again)) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one");
    };

    // the session is the same one and has moved on, which is the whole of what a client wanted
    assert_eq!(again.session, first.session);
    assert!(
        again.seq > first.seq,
        "a fresh projection reported a stale sequence: {} then {}",
        first.seq,
        again.seq
    );
    assert!(
        !again.items.is_empty() && first.items.is_empty(),
        "the items are what it was asked for: {:?}",
        again.items
    );

    // and nothing was replayed. The next thing on the wire is the answer to the next command,
    // because asking for a projection is not attaching
    peer.send(Command::Interrupt).await;
    let next = peer.recv().await;
    assert!(
        matches!(next, Message::Done { .. }),
        "asking for a projection put something back on the stream: {next:?}"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// `cycle` moves an item through the ring the context tab's `space` key moves it through.
///
/// note: the same function, which is the claim worth pinning. The ring and the notes it writes
/// were in `keys.rs`, and the notes are read by the *model* - so a client that picked its own
/// words for the same act would put a second account of it into the context. What this asserts is
/// the order and the fact that the answer carries the new state, not the wording; `App::cycle` is
/// where the wording is, and `tests/screen/context.rs` is where the key is.
#[tokio::test]
async fn an_item_can_be_cycled_through_its_states_from_a_client() {
    let served = served(vec![], |app| {
        app.kernel
            .push(nachalnik::ContextItem::user("what does the kernel do?"));
    })
    .await;
    let (mut peer, first) = Peer::attached(&served.at).await;

    let id = first.items.first().expect("the item was pushed").id;
    assert_eq!(first.items[0].state, nachalnik::ContextState::Active);

    // seen, then a marker where it was, then nothing at all, then seen again
    for expected in [
        nachalnik::ContextState::Elided,
        nachalnik::ContextState::Excluded,
        nachalnik::ContextState::Active,
    ] {
        peer.send(Command::Cycle { id }).await;
        let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
        let Some(Message::Projected(now)) = heard
            .into_iter()
            .find(|m| matches!(m, Message::Projected(_)))
        else {
            unreachable!("the loop above only ends on one");
        };
        assert_eq!(
            now.items[0].state, expected,
            "the ring went somewhere else: {:?}",
            now.items[0]
        );
    }

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// And cycling an item that is not there is refused rather than ignored.
#[tokio::test]
async fn cycling_an_item_that_is_not_there_says_so() {
    let served = served(vec![], |_| {}).await;
    let (mut peer, _) = Peer::attached(&served.at).await;

    peer.send(Command::Cycle { id: ContextId(404) }).await;
    let heard = peer.until(|m| matches!(m, Message::Failed { .. })).await;
    let Some(Message::Failed { about, .. }) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Failed { .. }))
    else {
        unreachable!("the loop above only ends on one");
    };
    assert_eq!(about, "cycle");

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// An elided item reads as its marker in the conversation, not as the content it still holds.
///
/// note: the bug this is here for was invisible from the terminal, which is the shape of thing a
/// second client finds. The substitution lived in `ui/tabs.rs`, so the screen was right and every
/// client was handed the words of an item whose whole meaning is that the model no longer has
/// them - a page drawing that shows a conversation the model is not having.
///
/// note: asserted on the *marker* rather than on the absence of the content, because absence
/// passes for a hundred wrong reasons - a line dropped, an item skipped, a projection that failed.
/// What is being claimed is that the line is the projector's own words, which is what the model
/// reads there.
#[tokio::test]
async fn an_elided_item_reads_as_its_marker_to_a_client() {
    let served = served(vec![], |app| {
        app.kernel
            .push(nachalnik::ContextItem::user("the secret is hunter2"));
        // a turn that is a thought, a sentence and two calls, so that the item is four lines of
        // conversation rather than one - which is what says the marker stands for the *item*. The
        // thought is first because that is the order a turn happens in, and it is what makes the
        // speaker worth asserting below
        app.kernel.push(nachalnik::ContextItem::new(
            nachalnik::ContextKind::AssistantMessage {
                tool_calls: vec![
                    call("c1", "peek", json!({ "at": "one" })),
                    call("c2", "peek", json!({ "at": "two" })),
                ],
                reasoning: Some("weighing it up".into()),
            },
            "model",
            "assistant",
            "here is what I will do",
        ));
    })
    .await;
    let (mut peer, first) = Peer::attached(&served.at).await;

    let id = first.items[0].id;
    assert!(
        first
            .conversation
            .iter()
            .any(|line| line.text.contains("hunter2")),
        "an active item says what it says: {:?}",
        first.conversation
    );

    // one step of the ring is `elided`
    peer.send(Command::Cycle { id }).await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one");
    };

    assert_eq!(now.items[0].state, nachalnik::ContextState::Elided);
    let marker = now.items[0]
        .marker
        .clone()
        .expect("an elided item is in the request as a marker");
    let line = now
        .conversation
        .iter()
        .find(|line| line.item == Some(id))
        .expect("an elided item keeps its place in the conversation");
    assert_eq!(
        line.text, marker,
        "the conversation showed something other than the marker: {:?}",
        now.conversation
    );

    // and the turn below it, which is three lines, reads as one marker rather than three
    let turn = now.items[1].id;
    peer.send(Command::Cycle { id: turn }).await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one");
    };
    let lines: Vec<_> = now
        .conversation
        .iter()
        .filter(|line| line.item == Some(turn))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "a hidden turn read as one thing per line it used to have: {lines:?}"
    );
    // and in the voice of whoever's turn it was, rather than of whichever of its four lines came
    // first. A marker under `~` says a *thought* was hidden, where what went was a thought, a
    // sentence and two calls - which the terminal dims either way and a client drawing rows draws
    // as what the speaker says it is
    assert_eq!(
        lines[0].speaker,
        Speaker::Model,
        "a hidden turn read as a hidden thought: {lines:?}"
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// The trace goes out as the trace tab draws it: this program's words, and the pane's own gaps.
///
/// note: what makes it worth a test rather than a glance is the gap, which is a *decision* about
/// when there is one rather than a format - nothing under a tenth of a second, and nothing after a
/// line that ended a wait for a person. A client left to work that out from timestamps would draw
/// a column whose largest figure is how long the operator spent reading, which is the one number
/// in there nobody should act on.
#[tokio::test]
async fn the_trace_goes_out_as_the_pane_draws_it() {
    let script = vec![ModelResponse::text("a state machine")];
    let served = served(script, |_| {}).await;
    let (mut peer, _) = Peer::attached(&served.at).await;

    peer.send(Command::Submit {
        line: "what does the kernel do?".to_owned(),
    })
    .await;
    peer.until_record("model.finished").await;
    peer.send(Command::Project).await;
    let heard = peer.until(|m| matches!(m, Message::Projected(_))).await;
    let Some(Message::Projected(now)) = heard
        .into_iter()
        .find(|m| matches!(m, Message::Projected(_)))
    else {
        unreachable!("the loop above only ends on one");
    };

    // the names are the events', and the detail is this program's account of them - which is the
    // whole reason this is on the wire rather than left to a client and the records
    assert!(
        now.trace.iter().any(|line| line.name == "model.requested"),
        "the trace is not the trace: {:?}",
        now.trace
    );
    assert!(
        now.trace
            .iter()
            .any(|line| !line.name.is_empty() && !line.detail.is_empty()),
        "every line came through without its words: {:?}",
        now.trace
    );
    // and a wall clock that is a real one, since it is the half a reader matches against a log
    assert!(
        now.trace.iter().all(|line| line.at > 1_600_000_000_000),
        "a line arrived with no time on it: {:?}",
        now.trace
    );

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    served.ended().await.1.expect("the session failed");
}

/// An endpoint that takes its time answering, so that a command can be caught in flight.
///
/// note: a closed port will not do. A refused connection comes back at once, and what this needs
/// is a window - the one a `/models` at an endpoint that has gone quiet opens for real.
async fn slow_endpoint(after: Duration) -> String {
    use tokio::io::AsyncReadExt as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = socket.read(&mut buf).await;
                tokio::time::sleep(after).await;
                let body = r#"{"data":[{"id":"a-slow-model"}]}"#;
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \
                             {}\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });

    format!("http://{at}/v1")
}

/// The kernel is still heard while one client's command waits on an endpoint.
///
/// note: the loop holds the `App` for the length of a command, because there is one of it and
/// answering anybody needs it. What it used to stop doing as well was reading the kernel's
/// broadcast - and that channel *drops* what nobody took rather than queueing it, which no other
/// channel here does. What this loop reads the stream for is `App::trace`, handed to every client
/// that attaches afterwards, so one `/models` at an endpoint that had gone quiet left everybody
/// who arrived later with a trace full of holes and nothing anywhere saying so.
///
/// note: more items than the channel is deep, because the failure is a capacity exceeded rather
/// than an ordering; `Config::event_queue_depth` is 1024. And pushed a moment after the command
/// goes out, so that they land while it is in flight rather than before the loop has taken it.
#[tokio::test]
async fn the_kernel_is_still_heard_while_a_command_waits_on_an_endpoint() {
    use nachalnik::ContextItem;

    let endpoint = slow_endpoint(Duration::from_millis(400)).await;
    let session = served_at(None, &endpoint, Vec::new(), |_| {}).await;
    let (mut peer, _) = Peer::attached(&session.at).await;

    peer.send(Command::Submit {
        line: "/models".to_owned(),
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // note: yielding between them, and the test is wrong without it. `Kernel::push` is not async
    // and `#[tokio::test]` is one thread, so a tight loop of them never lets the session's task run
    // at all - and a channel that overflowed because nobody was *scheduled* would fail this whether
    // the loop was listening or not. What is under test is a loop that had stopped listening
    for n in 0..1500 {
        session.kernel.push(ContextItem::user(format!("item {n}")));
        if n % 64 == 0 {
            tokio::task::yield_now().await;
        }
    }

    // the answer coming back is what says the command really was in flight for all of that
    let heard = peer
        .until(|message| matches!(message, Message::Replied { .. }))
        .await;
    assert!(!heard.is_empty());

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    let (app, _) = session.ended().await;

    assert!(
        !app.loose
            .iter()
            .any(|entry| entry.text.contains("went by too fast")),
        "the session stopped listening while it waited"
    );
    // and it is not that the events never came: every one of them is in the context the session
    // hands the next client that attaches
    assert_eq!(
        app.kernel.items().len(),
        1500,
        "the session's own view of the context is short"
    );
}

/// A record too long to send is named, and the session stays attachable.
///
/// note: the failure this is about is a lockout rather than a lost line. `context.replaced` is the
/// one event that carries content, the log drops nothing, and a client resumes by sequence - so
/// one rewritten tool result over `MAX_LINE` was refused by every client, on every attempt, for
/// the rest of the session. Raising the number would have moved the size of the thing that breaks.
///
/// note: thirty-three megabytes, because the limit is thirty-two and nothing smaller exercises it.
/// It is the one expensive test in this suite and the cost is the point: the case only exists at
/// that size.
///
/// note: the big one is what the item *used to* hold rather than what it holds, which is the shape
/// the case actually takes - a rewritten tool result - and the only shape this closes. A context
/// item that is large *now* makes the projection itself oversized, which is a second door to the
/// same room and is in `POSTPONED.md`: the projection cannot be skipped, so it wants abridging and
/// that is a decision about what every client is handed.
#[tokio::test]
async fn a_record_too_long_to_send_is_named_rather_than_locking_everybody_out() {
    use nachalnik::ContextItem;

    let session = served(Vec::new(), |_| {}).await;
    // attached first, because a client that arrives afterwards is handed a projection and the
    // records *after* it - the ones it has to be able to read are the ones written while it is here
    let (mut peer, _) = Peer::attached(&session.at).await;
    let id = session
        .kernel
        .push(ContextItem::user("x".repeat(33 * 1024 * 1024)));
    session
        .kernel
        .replace(id, "and now it is short")
        .expect("the item is there");

    let heard = peer
        .until(|message| matches!(message, Message::Oversized { .. }))
        .await;
    let Some(Message::Oversized { seq, bytes }) = heard.last() else {
        panic!("the record was not named");
    };
    assert!(*bytes > protocol::MAX_LINE, "{bytes} is not over the limit");

    // and the connection is still there, still numbered, and still carrying what came after it:
    // before this the frame closed it and the next attempt came back to the same record
    let seq = *seq;
    peer.send(Command::Submit {
        line: "/note the session is still here".to_owned(),
    })
    .await;
    // waited for rather than asserted on what has already arrived: the record goes out on the
    // stream and the answer on the connection, and which of the two lands first is not this
    // test's business. Nothing coming at all is the failure, and it fails as a timeout
    peer.until(|message| matches!(message, Message::Record(record) if record.seq > seq))
        .await;

    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
}
// ------------------------------------------------------------- what a client with no keys can do

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

// --------------------------------------------------------------- an item, edited from elsewhere

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
    assert!(session.kernel.undo(), "there was something to undo");
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
    use std::io::Write as _;

    let dir = common::scratch("drawn-restart");
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
            common::program().display(),
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
    let mut asked = std::process::Command::new(common::program())
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
