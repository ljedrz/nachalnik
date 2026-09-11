//! Somebody else's tools, in a run with nobody watching.
//!
//! note: `--mcp` and `--headless` are each covered elsewhere and had never been run together,
//! which is a gap rather than an omission: the two meet at a question. An MCP tool declares
//! `mcp:<server>` and nothing else, `Careful` asks about whatever nobody has answered for, and a
//! headless run has nobody to ask - so a server's tools are refused by default and `--allow
//! mcp:py` is the whole of how an unattended run is given them. That answer is recorded while the
//! session is being wired, *before* the server has been spawned or said what it offers, which is
//! the ordering worth a test.
//!
//! note: a real child process rather than an in-process fixture, because the untested path is the
//! child: [`kamchatka::mcp::attach`] takes a command line, splits it on whitespace and spawns it.
//! The server is `tests/mcp_server.py`, and these skip when there is no interpreter to run it
//! with - the way the runtime's live tests skip without a key.

use std::sync::Arc;

use kamchatka::{
    app::App,
    headless::Headless,
    tools::Subject,
    wiring::{Setup, Wired},
};
use nachalnik::{
    ContextKind, Grant, ModelResponse, Record,
    test::{ScriptedProvider, call},
};
use nachalnik_providers::OpenAiCompatible;
use serde_json::json;

/// `name=command` for the Python server, or `None` when it cannot be run here.
fn spec() -> Option<String> {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: python3 is not on the path");
        return None;
    }

    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mcp_server.py");
    // note: a spec is split on whitespace, so a path with a space in it cannot be given as one.
    // That is a limitation of `--mcp` rather than of this test, and working around it here would
    // hide it - a checkout under a directory with a space in the name skips, and says which
    if script.contains(char::is_whitespace) {
        eprintln!("skipped: an `--mcp` spec is split on whitespace, and this path has some in it");
        return None;
    }

    // the name is given rather than derived: taken from the program it would be `python3`, and
    // `mcp:python3` is not what anybody would write on a command line
    Some(format!("py=python3 {script}"))
}

macro_rules! spec {
    () => {
        match spec() {
            Some(spec) => spec,
            None => return,
        }
    };
}

/// What one headless run wrote, and the session it left behind.
struct Run {
    app: App,
    names: Vec<String>,
    prose: String,
}

/// Wires a session the way the program does and attaches the server to it, answering these
/// subjects in advance as `--allow` does.
///
/// note: the servers come back rather than being held here, because whoever holds them decides how
/// long the tools work for - which is the one thing about this crate's MCP that a caller can get
/// wrong silently, and the last test below is about getting it wrong.
async fn session(
    spec: String,
    allow: &[&str],
    script: Vec<ModelResponse>,
) -> (Wired, Vec<nachalnik_mcp::Server>) {
    let wired = Setup {
        builtin_tools: false,
        compact: None,
        allow: allow.iter().map(|it| Subject::parse(it)).collect(),
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

    let servers = kamchatka::mcp::attach(&wired.app.kernel, &[spec])
        .await
        .expect("the server did not start");

    (wired, servers)
}

/// Drives one with a line typed into it, and hands back what came out of both ends.
async fn driven(wired: Wired, line: &str) -> Run {
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired;

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    Headless::new(Grant::Deny, &mut records, &mut prose)
        .run(&mut app, &mut events, &mut finished, line.as_bytes())
        .await
        .expect("the run failed");

    Run {
        app,
        names: String::from_utf8(records)
            .expect("the records are text")
            .lines()
            .map(|line| {
                serde_json::from_str::<Record>(line)
                    .expect("every line is a record")
                    .event
                    .name()
                    .to_owned()
            })
            .collect(),
        prose: String::from_utf8(prose).expect("the prose is text"),
    }
}

/// A server's tool runs in a session nobody is watching, because somebody said so in advance.
#[tokio::test]
async fn an_answer_given_in_advance_covers_a_tool_that_did_not_exist_yet() {
    let spec = spec!();
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "py__add", json!({ "a": 2, "b": 40 }))]),
        ModelResponse::text("forty-two"),
    ];
    let (wired, _servers) = session(spec, &["mcp:py"], script).await;
    let run = driven(wired, "add two and forty\n").await;

    assert!(run.names.contains(&"tool.finished".to_owned()));
    assert!(run.prose.contains("forty-two"), "{}", run.prose);
    // the call crossed a process boundary and the answer came back into the context
    let answered = run
        .app
        .kernel
        .items()
        .iter()
        .any(|item| item.content.to_text() == "42");
    assert!(answered, "the tool's output is not in the context");
    // and it was never a question: the verdict was recorded against `mcp:py` while the session was
    // being wired, and the tool that carries that capability arrived afterwards
    assert!(
        !run.prose.contains("nobody is here to be asked"),
        "it was asked after all: {}",
        run.prose
    );
}

/// Without that answer it is a question, and a question here is a refusal.
///
/// note: this is what gives the test above its teeth. A run in which the tool is allowed and a run
/// in which nothing ever asks look the same from the outside, and the difference is whether
/// `mcp:py` is a subject at all - so the counterfactual is the assertion. It is also the
/// behaviour: a server named on a command line is not thereby trusted to run.
#[tokio::test]
async fn a_servers_tool_is_refused_when_nobody_has_answered_for_it() {
    let spec = spec!();
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "py__add", json!({ "a": 2, "b": 40 }))]),
        ModelResponse::text("told it was refused"),
    ];
    let (wired, _servers) = session(spec, &[], script).await;
    let run = driven(wired, "add two and forty\n").await;

    assert!(run.names.contains(&"permission.decided".to_owned()));
    assert!(!run.names.contains(&"tool.started".to_owned()));
    assert!(
        run.prose.contains("nobody is here to be asked"),
        "{}",
        run.prose
    );
}

/// A server let go of takes its answers with it, and the run ends anyway.
///
/// note: `attach` hands back servers that have to be held, which is written in its documentation
/// twice and was checked nowhere: `main` keeps them in a `_servers` binding, and the whole of what
/// stops an embedder writing `let _ =` is that sentence. The tools stay in the registry either way
/// - that is why this cannot be seen by asking what is offered - so what a dropped server costs is
/// every call from then on.
///
/// note: what is actually being pinned is that it *fails*. An unattended run has nobody to notice
/// a call that never comes back, and a dead child process is the shape that could hang one, so the
/// timeout here is the assertion and the rest is reporting what the failure looks like.
#[tokio::test]
async fn a_dropped_server_fails_its_calls_instead_of_hanging() {
    let spec = spec!();
    let script = vec![
        ModelResponse::tool_calls(vec![call("c1", "py__add", json!({ "a": 2, "b": 40 }))]),
        ModelResponse::text("it did not answer"),
    ];
    let (wired, servers) = session(spec, &["mcp:py"], script).await;
    assert!(wired.app.kernel.tool("py__add").is_some());
    drop(servers);

    let run = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        driven(wired, "add two and forty\n"),
    )
    .await
    .expect("a call to a server that is gone never came back");

    assert!(run.names.contains(&"tool.finished".to_owned()));
    // and the model is told what went wrong, in the result of the call it made, rather than being
    // handed an empty answer to carry on from
    // the bridge's own sentence rather than the transport's - `the MCP server refused: Transport
    // closed` is what arrives, and only the first half of that is anybody here's to promise
    let told = run.app.kernel.items().iter().any(|item| {
        matches!(item.kind, ContextKind::ToolResult { is_error: true, .. })
            && item.content.to_text().contains("the MCP server refused")
    });
    assert!(told, "the call failed without saying why: {}", run.prose);
}

/// The program spawns it, offers its tools, and writes that it did - with no screen anywhere.
///
/// note: the two above drive the loop in process, which says nothing about the half a caller
/// meets: `--mcp name=command` as an argument, the spec split into a program and its arguments,
/// and a server spawned after the wiring rather than during it. This one runs the binary, and it
/// needs no model to do it - `/tools` is answered here.
#[test]
fn the_program_offers_a_spawned_servers_tools() {
    let spec = spec!();

    // the test binary lives beside it
    let mut program = std::env::current_exe().expect("a test binary has a path");
    program.pop();
    if program.ends_with("deps") {
        program.pop();
    }
    program.push("kamchatka");

    let mut child = std::process::Command::new(&program)
        .args(["-m", "nothing-serves-this", "--no-record", "--mcp", &spec])
        .env("KAMCHATKA_BASE_URL", "http://127.0.0.1:1/v1")
        .env("KAMCHATKA_API_KEY", "not-a-key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test is built");
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin is a pipe");
        stdin.write_all(b"/tools\n").expect("the line was not sent");
    }
    let out = child.wait_with_output().expect("the program never ended");

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("py__add"),
        "the server's tool was not offered: {said}"
    );
    assert!(
        out.status.success(),
        "a session that only listed its tools is not a failure: {said}"
    );

    // and the log says so: `--mcp` is attached after the subscription, so installing the tools is
    // on the stream like everything else rather than a fact only the screen ever knew
    let installed = String::from_utf8(out.stdout)
        .expect("the records are text")
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .any(|record| {
            matches!(record.event,
            nachalnik::Event::ToolsChanged { tools } if tools.iter().any(|it| it == "py__add"))
        });
    assert!(installed, "the tools arriving is not on the record stream");
}
