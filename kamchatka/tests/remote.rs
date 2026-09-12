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

/// A served session, and where to find it.
struct Served {
    /// The address, in the spelling a client would type.
    at: String,
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
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired(script);
    setup(&app);

    // note: port zero, so the kernel picks one nothing else is using and `Server::address` is what
    // says which. A fixed port in a test suite is a suite that fails when somebody runs it twice
    let mut server = Server::bind("tcp:127.0.0.1:0")
        .await
        .expect("nothing would listen");
    let at = server.address();
    let loop_ = tokio::spawn(async move {
        let outcome = server.run(&mut app, &mut events, &mut finished).await;

        (app, outcome)
    });

    Served { at, loop_ }
}

/// The same `Setup` the headless suite uses, for the same reason: what these want is what
/// `main.rs` wants, minus the six tools and the child process it takes to ask Landlock anything.
fn wired(script: Vec<ModelResponse>) -> Wired {
    let wired = Setup {
        tools: Some(Vec::new()),
        compact: None,
        ..Default::default()
    }
    .wire(Arc::new(OpenAiCompatible::new(
        "scripted",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed");
    wired
        .app
        .kernel
        .set_provider(Arc::new(ScriptedProvider::new(script)));

    wired
}

/// One end of a connection, speaking the protocol by hand.
struct Peer {
    lines: tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
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
            lines: BufReader::new(read).lines(),
            write,
        }
    }

    /// Connects and attaches, and hands back the projection.
    async fn attached(at: &str) -> (Self, Attached) {
        let mut peer = Self::connect(at).await;
        peer.send(Command::Attach { since: None }).await;
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
    // note: the session says when somebody attaches, through `App::say`, so its own voice is in the
    // conversation beside the context. That is the point rather than noise: a session two people
    // can type into should say so where both of them read
    assert!(
        attached
            .conversation
            .iter()
            .any(|line| line.speaker == Speaker::Note && line.text.contains("client 1 attached")),
        "the session did not say a client had arrived"
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
    peer.send(Command::Attach { since: Some(0) }).await;
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
    again
        .send(Command::Attach {
            since: Some(attached.seq),
        })
        .await;
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
    peer.send(Command::Attach { since: Some(9_999) }).await;
    let Message::Failed { error, .. } = peer.recv().await else {
        panic!("a watermark from the future was accepted");
    };
    assert!(error.contains("you say you have 9999"), "{error}");
    assert!(peer.next().await.is_none(), "the connection stayed open");

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
    again
        .send(Command::Attach {
            since: Some(attached.seq),
        })
        .await;
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
/// note: the loop sets it rather than the caller, which is why this is a test of the *server*
/// rather than of `main.rs`: a host embedding `Server::run` has no keys either.
#[tokio::test]
async fn help_from_a_client_is_the_commands() {
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
    session.ended().await.1.expect("the session failed");
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
    let loop_ = tokio::spawn(async move {
        let outcome = server.run(&mut app, &mut events, &mut finished).await;

        (app, outcome)
    });
    let session = Served { at, loop_ };

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

/// So does a line nobody could have meant.
#[tokio::test]
async fn an_oversized_frame_closes_the_connection() {
    let session = served(vec![], |_| {}).await;

    let mut peer = Peer::connect(&session.at).await;
    let huge = vec![b'x'; protocol::MAX_LINE + 1];
    peer.raw(&huge).await;
    peer.raw(b"\n").await;
    let Message::Failed { error, .. } = peer.recv().await else {
        panic!("an oversized frame was accepted");
    };
    assert!(error.contains("over the"), "{error}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
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
    kamchatka::remote::Client::new(&mut records, &mut prose)
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
    kamchatka::remote::Client::new(&mut records, &mut prose)
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
#[tokio::test]
async fn the_program_has_one_voice_and_every_client_hears_it() {
    let session = served(vec![], |_| {}).await;

    let (mut one, _) = Peer::attached(&session.at).await;
    let (mut two, _) = Peer::attached(&session.at).await;
    one.send(Command::Submit {
        line: "/seams".to_owned(),
    })
    .await;

    // the second client ran nothing and hears the first one arrive, and itself
    let heard = two
        .until(|message| matches!(message, Message::Said { text, .. } if text.contains("client 2")))
        .await;
    assert!(heard.iter().any(
        |message| matches!(message, Message::Said { speaker, .. } if *speaker == Speaker::Note)
    ));

    two.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
    session.ended().await.1.expect("the session failed");
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
    let refused = kamchatka::remote::Client::new(&mut records, &mut prose)
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
    kamchatka::remote::Client::new(&mut records, &mut prose)
        .run(&session.at, BufReader::new(input))
        .await
        .expect("the client failed");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("the whole answer"), "{prose}");

    quit(&session.at).await;
    session.ended().await.1.expect("the session failed");
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
    kamchatka::remote::Client::new(&mut records, &mut prose)
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
