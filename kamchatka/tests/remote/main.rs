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
//! together, which is the one thing a protocol exists not to be. `client.rs` and `program.rs` are
//! where the ones that drive `remote::Client` are, against a session served here and against the
//! binary.

// note: `tests/remote/main.rs` rather than `tests/remote.rs`, for the reason given in
// `tests/screen/main.rs`: a directory with a `main.rs` in it is one test binary named for the
// directory, where a crate root's submodules would each be one of their own. One file per thing
// this suite is about, and what holds of all of them - a session wired and served, a `Peer` that
// speaks the protocol by hand, and the two tools that answer slowly - stays here, where every one
// of them can reach it.

use std::{sync::Arc, time::Duration};

use kamchatka::{
    app::App,
    remote::{
        Server,
        protocol::{self, Address, Attached, Command, Message},
    },
    wiring::{Setup, Wired},
};
use nachalnik::{
    BoxError, DeltaSink, Event, ModelInfo, ModelRequest, ModelResponse, OutputSink, Provider, Tool,
    ToolCall, ToolOutput, ToolSpec, async_trait, test::ScriptedProvider,
};
use nachalnik_providers::OpenAiCompatible;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::TcpStream,
};

// the path is the price of the directory: `common` is shared with every other suite in here
// and stays where all of them can reach it
#[path = "../common/mod.rs"]
mod common;

mod asking;
mod attaching;
mod client;
mod items;
mod program;
mod questions;
mod refused;
mod sharing;
mod stopping;

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

/// Ends a session from a connection of its own, for the tests whose own peer has been closed.
async fn quit(at: &str) {
    let (mut peer, _) = Peer::attached(at).await;
    peer.send(Command::Submit {
        line: "/quit".to_owned(),
    })
    .await;
}

/// Runs `--connect` against a socket with these lines typed at it, and waits for it.
///
/// note: no model, no key and no endpoint. A client wires nothing up, and the day this needs one
/// again is the day `--connect` has stopped being a client.
fn connect(socket: &std::path::Path, typed: &[u8]) -> std::process::Output {
    use std::io::Write as _;

    let mut client = std::process::Command::new(crate::common::program())
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

/// A stand-in for a gated `shell`: it asks what the gate asks when a command reaches for the
/// network, and says what it was told.
pub(crate) struct Reacher(pub(crate) Arc<kamchatka::tools::Careful>);

#[nachalnik::async_trait]
impl nachalnik::Tool for Reacher {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec::new("reacher", "reaches for the network")
    }

    async fn invoke(
        &self,
        call: &nachalnik::ToolCall,
        _output: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
        let answer = self
            .0
            .reaching()
            .ask(call.id.clone(), "curl x".into())
            .await;

        Ok(nachalnik::ToolOutput::new(format!("answered {answer:?}")))
    }
}
