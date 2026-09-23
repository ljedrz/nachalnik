//! Tests against an MCP server that is not built on the same SDK as the bridge.
//!
//! note: `bridge.rs` stands an `rmcp` server up in-process, which tests the mapping but leaves
//! two things untested: the child-process transport, and whether any of this works against an
//! implementation that has never heard of `rmcp`. The server here is eighty lines of Python
//! speaking newline-delimited JSON-RPC by hand.
//!
//! note: They skip if `python3` is not on the path, the way the runtime's live tests skip without
//! an API key - a missing interpreter is not a failing bridge.

use std::sync::Arc;

use nachalnik::{
    Capability, Config, ContextItem, ContextKind, Kernel, ModelResponse, State,
    test::{AllowAll, ScriptedProvider, call},
};
use nachalnik_mcp::{Server, Trust};
use serde_json::json;
use tokio::process::Command;

/// The model the one live test asks when the environment names none.
///
/// note: the same free model the runtime's own live suite defaults to, and for the same two
/// reasons: it is served at the endpoint `nachalnik_utils` defaults to, and it calls tools, which
/// is the whole of what the test is about.
const DEFAULT_MODEL: &str = "liquid/lfm-2.5-2.6b:free";

/// Starts the Python server as a child process, or gives up quietly.
async fn foreign(name: &str) -> Option<Server> {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: python3 is not on the path");
        return None;
    }

    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/foreign_server.py");
    let mut command = Command::new("python3");
    command.arg(script);

    match Server::spawn(name, command).await {
        Ok(server) => Some(server),
        Err(e) => panic!("the foreign server should answer the handshake: {e}"),
    }
}

macro_rules! foreign {
    ($name:expr) => {
        match foreign($name).await {
            Some(server) => server,
            None => return,
        }
    };
}

#[tokio::test]
async fn a_server_in_another_language_is_just_a_server() {
    let server = foreign!("py");
    let kernel = Kernel::new(Config::default());

    let installed = server.install(&kernel).await.unwrap();

    assert_eq!(
        installed.added,
        vec!["py__add", "py__explodes", "py__shout"]
    );
    let spec = kernel.tool("py__add").unwrap().spec();
    assert_eq!(spec.description, "adds two numbers");
    assert_eq!(spec.schema["required"], json!(["a", "b"]));

    // the handshake carried what it said about itself
    let info = server.info().expect("server info");
    assert_eq!(
        info.server_info.as_ref().map(|it| it.name.as_str()),
        Some("foreign")
    );
}

#[tokio::test]
async fn a_call_crosses_the_process_boundary_and_comes_back() {
    let server = foreign!("py");
    let kernel = Kernel::new(Config::default());
    server.install(&kernel).await.unwrap();
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "py__add", json!({ "a": 2, "b": 40 }))]),
        ModelResponse::text("forty-two"),
    ])));
    kernel.push(ContextItem::user("add two and forty"));

    let State::Finished { .. } = kernel.turn().await.unwrap() else {
        panic!("the policy allows everything")
    };

    let result = kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .expect("the result is an ordinary context item");
    assert_eq!(result.content.to_text(), "42");
    assert_eq!(result.label, "py__add");
}

#[tokio::test]
async fn annotations_from_a_foreign_server_are_read_the_same_way() {
    let server = foreign!("py");
    let kernel = Kernel::new(Config::default());
    server
        .trusting(Trust::Annotations)
        .install(&kernel)
        .await
        .unwrap();

    // `add` says it is read-only, and `shout` says nothing
    let read_only = kernel.tool("py__add").unwrap().spec();
    assert!(read_only.capabilities.contains(&Capability::fs("read")));

    let silent = kernel.tool("py__shout").unwrap().spec();
    assert!(silent.capabilities.contains(&Capability::fs("write")));
    assert!(
        !silent.capabilities.contains(&Capability::fs("read")),
        "an absent hint is not a claim of harmlessness"
    );
}

/// Invokes a tool the way the kernel would, without a whole loop around it.
async fn invoke(kernel: &Kernel, id: &str, args: serde_json::Value) -> nachalnik::ToolOutput {
    kernel
        .tool(id)
        .expect("the tool is registered")
        .invoke(
            &nachalnik::ToolCall::new("c1", id, args),
            nachalnik::OutputSink::disconnected(),
        )
        .await
        .expect("the bridge answers rather than failing")
}

#[tokio::test]
async fn a_failure_the_server_reports_reaches_the_model_as_one() {
    let server = foreign!("py");
    let kernel = Kernel::new(Config::default());
    server.install(&kernel).await.unwrap();

    let worked = invoke(&kernel, "py__shout", json!({ "text": "quiet" })).await;
    assert_eq!(worked.content.to_text(), "QUIET");
    assert!(!worked.is_error);

    // `isError` on the wire is an error result in the context, not a broken loop
    let failed = invoke(&kernel, "py__explodes", json!({})).await;
    assert!(failed.is_error);
    assert_eq!(failed.content.to_text(), "it went wrong over here");
}

#[tokio::test]
async fn the_session_ends_when_it_is_told_to() {
    let server = foreign!("py");
    let kernel = Kernel::new(Config::default());
    let installed = server.install(&kernel).await.unwrap();

    server.shutdown().await.expect("it goes quietly");

    // the tools are still registered; the kernel has no idea the server is gone, which is why
    // taking them back out is something the caller does
    assert_eq!(kernel.tool_ids().len(), 3);
    assert_eq!(installed.remove_from(&kernel), 3);
}

/// A real model, offered a real server's tools, calls one and reads what came back.
///
/// note: the one claim neither suite above can make. `bridge.rs` and the tests here prove the
/// mapping and the transport - a `ToolSpec` comes out, a call goes in, an answer comes back - and
/// then hand the result to a scripted provider that was always going to agree with it. What none
/// of that settles is whether the thing on the other side of the bridge is usable: whether the
/// identifier survives a provider's charset, whether the schema is one a model fills in
/// correctly, and whether the description is enough to pick the right tool from three.
///
/// note: skipped without a key, and without `python3`, for the reason every test in this file is.
/// It also skips when the turn fails, because the free pool this runs against answers `429` often
/// enough that a rate limit is not news - which is why the default below has to be a model that
/// address really serves. `mercury-2.5` was Inception's spelling of one, and against the default
/// endpoint it was a 404 that arrived as a skip: the suite passed, and the one claim it is here to
/// make went untested with nothing saying so.
#[tokio::test]
async fn a_real_model_uses_a_tool_from_a_foreign_server() {
    let Some(provider) = nachalnik_utils::provider(&nachalnik_utils::test_model(DEFAULT_MODEL))
        .ok()
        .map(Arc::new)
    else {
        eprintln!("skipped: no key in the environment");
        return;
    };
    let server = foreign!("arith");

    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider.clone());
    kernel.set_policy(Arc::new(AllowAll));
    server.install(&kernel).await.expect("the tools install");

    // the identifier the model will be offered, which is the bridge's own making
    let offered: Vec<_> = kernel
        .tool_specs()
        .into_iter()
        .map(|spec| spec.id)
        .collect();
    println!("  offered: {offered:?}");
    assert!(
        offered.iter().all(|id| id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')),
        "every one has to be a name a provider accepts: {offered:?}"
    );

    kernel.push(ContextItem::user(
        "Use the tools you have been given to add 40 and 2. Then say the number.",
    ));
    let state = match kernel.turn().await {
        Ok(state) => state,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let State::Finished { .. } = state else {
        panic!("it stopped somewhere: {state:?}")
    };

    let results: Vec<String> = kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .collect();
    println!("  results: {results:?}");
    assert!(
        !results.is_empty(),
        "the model was offered three tools and reached for none of them"
    );

    let said = kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
        .map(|item| item.content.to_text().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    println!("  it said: {}", said.trim());
    assert!(
        said.contains("42"),
        "the server's answer should have reached the model: {said}"
    );
}

/// A server that dies before the handshake says why, in the error rather than on the terminal.
///
/// note: its standard error was inherited, so what a server said on the way out went wherever the
/// caller's terminal was - across a drawn screen, or nowhere a person would look - and the error
/// said only that the handshake had not happened. It is held and read now, and the last of it
/// rides on the error, which is the one place it is worth reading.
#[cfg(unix)]
#[tokio::test]
async fn a_server_that_dies_before_the_handshake_says_why() {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("echo 'ModuleNotFoundError: no module named mcp' >&2; exit 1");

    let refused = match Server::spawn("broken", command).await {
        Ok(_) => panic!("nothing answered the handshake"),
        Err(e) => e.to_string(),
    };

    assert!(refused.contains("no module named mcp"), "{refused}");
}

/// What a server writes to standard error without ending a line is kept to a line's worth.
///
/// note: the tail kept a bounded number of lines and no bound on one, so a server writing without
/// newlines - a progress bar redrawn with `\r`, or one that means harm - grew it for as long as it
/// ran. The handshake failing is when the tail is handed back, so that is where it can be seen.
#[tokio::test]
async fn a_line_that_never_ends_is_kept_to_a_lines_worth() {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("head -c 5000000 /dev/zero | tr '\\0' x >&2; exit 1");

    let refused = match Server::spawn("chatty", command).await {
        Ok(_) => panic!("nothing answered the handshake"),
        Err(e) => e.to_string(),
    };

    assert!(
        refused.contains("xxxx"),
        "the start of it is kept: {refused:.200}"
    );
    assert!(refused.len() < 4096, "{} bytes of it", refused.len());
}
