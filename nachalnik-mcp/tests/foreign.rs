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
    assert!(read_only.capabilities.contains(&Capability::Read));

    let silent = kernel.tool("py__shout").unwrap().spec();
    assert!(silent.capabilities.contains(&Capability::Write));
    assert!(
        !silent.capabilities.contains(&Capability::Read),
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
