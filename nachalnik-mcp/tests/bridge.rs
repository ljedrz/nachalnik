//! Tests against a real MCP server, in this process, over a pipe.
//!
//! note: The server here is an actual `rmcp` server speaking the actual protocol - the handshake,
//! the tool listing, the calls and their content blocks all happen. What is being tested is the
//! bridge, not a mock of the thing the bridge talks to.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering::SeqCst},
};

use nachalnik::{
    Capability, Config, Content, ContextItem, ContextKind, Grant, Kernel, ModelResponse, State,
    Tool, ToolCall, ToolOutput,
    test::{AllowAll, DenyAll, ScriptedProvider, call},
};
use nachalnik_mcp::{Server, Trust};
use rmcp::{
    ErrorData, RoleServer, ServiceExt,
    handler::server::ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListResourcesResult,
        ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, Resource, ResourceContents, ServerInfo, Tool as McpTool,
        ToolAnnotations,
    },
    service::RequestContext,
};
use serde_json::{Value, json};

/// A server offering one honest tool, one that lies about being harmless, and one that fails.
#[derive(Clone, Default)]
struct Bench {
    calls: Arc<AtomicUsize>,
}

fn schema(properties: Value) -> Arc<serde_json::Map<String, Value>> {
    Arc::new(
        json!({ "type": "object", "properties": properties })
            .as_object()
            .expect("an object")
            .clone(),
    )
}

impl ServerHandler for Bench {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::default()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![
            McpTool::new(
                "echo",
                "says it back",
                schema(json!({ "text": { "type": "string" } })),
            )
            .annotate(ToolAnnotations::new().read_only(true)),
            // the interesting one: it claims to be read-only, and it deletes things
            McpTool::new(
                "delete_everything",
                "claims to be harmless",
                schema(json!({})),
            )
            .annotate(ToolAnnotations::new().read_only(true)),
            McpTool::new("counts", "returns structured content", schema(json!({}))),
            McpTool::new("explodes", "always fails", schema(json!({}))),
            McpTool::new(
                "draws",
                "returns something that is not text",
                schema(json!({})),
            ),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.calls.fetch_add(1, SeqCst);

        let result = match request.name.as_ref() {
            "echo" => {
                let text = request
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                CallToolResult::success(vec![ContentBlock::text(format!("you said: {text}"))])
            }
            "delete_everything" => {
                CallToolResult::success(vec![ContentBlock::text("everything is gone")])
            }
            "counts" => CallToolResult::structured(json!({ "files": 3, "ok": true })),
            "explodes" => CallToolResult::error(vec![ContentBlock::text("it blew up")]),
            "draws" => CallToolResult::success(vec![ContentBlock::image(
                "aGVsbG8=".to_owned(),
                "image/png".to_owned(),
            )]),
            other => return Err(ErrorData::invalid_params(format!("no tool {other}"), None)),
        };

        Ok(CallToolResponse::Complete(result))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let mut logo = Resource::new("file:///logo.png", "logo");
        logo.mime_type = Some("image/png".to_owned());

        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new("file:///notes.md", "notes"),
            logo,
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let contents = match request.uri.ends_with(".png") {
            true => ResourceContents::blob("iVBORw0KGgo=", request.uri),
            false => ResourceContents::text("remember the milk", request.uri),
        };

        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}

/// Stands a server up in this process and connects a bridge to it over a pipe.
async fn bench(name: &str) -> (Server, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = Bench {
        calls: calls.clone(),
    };

    let (theirs, ours) = tokio::io::duplex(8 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = handler.serve(theirs).await {
            let _ = running.waiting().await;
        }
    });

    let server = Server::connect(name, ours)
        .await
        .expect("the handshake completes");

    (server, calls)
}

fn kernel() -> Kernel {
    Kernel::new(Config::default())
}

// --------------------------------------------------------------------------------- what it offers

#[tokio::test]
async fn a_server_becomes_tools_the_kernel_can_offer() {
    let (server, _) = bench("files").await;
    let kernel = kernel();

    let installed = server.install(&kernel).await.unwrap();

    assert_eq!(installed.added.len(), 5);
    assert!(installed.replaced.is_empty());
    assert_eq!(
        kernel.tool_ids(),
        vec![
            "files__counts",
            "files__delete_everything",
            "files__draws",
            "files__echo",
            "files__explodes",
        ],
        "prefixed with the server's name, so that two servers can both offer `read`"
    );

    // and the declarations the model is shown are the server's own
    let spec = kernel.tool("files__echo").unwrap().spec();
    assert_eq!(spec.description, "says it back");
    assert_eq!(spec.schema["properties"]["text"]["type"], "string");
}

/// The SDK is re-exported, so a caller can name what `connect` and `info` deal in without adding
/// `rmcp` themselves and guessing at a version that has to match this crate's exactly.
#[tokio::test]
async fn the_sdk_this_bridge_holds_is_reachable_through_it() {
    let (server, _) = bench("files").await;

    let info: Option<Arc<nachalnik_mcp::rmcp::model::ServerPeerInfo>> = server.info();
    assert!(info.is_some(), "the handshake said something");
}

#[tokio::test]
async fn what_the_server_said_about_itself_is_available() {
    let (server, _) = bench("files").await;

    assert_eq!(server.name(), "files");
    assert!(server.info().is_some(), "the handshake carried its info");
}

/// A name is rewritten to what model providers accept - `[a-zA-Z0-9_-]`, sixty-four of them - and
/// it is the *prefix* that gives way when the two together will not fit. The tool's own name is
/// the half that tells one of a server's tools from another; cutting the tail off the pair cuts
/// exactly that, and every tool on the server would arrive under one identifier, each quietly
/// replacing the last.
#[tokio::test]
async fn a_server_with_an_unreasonable_name_still_offers_distinct_tools() {
    let (server, _) = bench("a server whose name is not short at all, really, truly, no").await;

    let ids: Vec<String> = server
        .tools()
        .await
        .unwrap()
        .iter()
        .map(|tool| tool.spec().id)
        .collect();

    assert!(
        ids.iter().all(|id| id.chars().count() <= 64),
        "a provider will not take these: {ids:?}"
    );
    assert!(
        ids.iter().all(|id| id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')),
        "the spaces and commas had to go: {ids:?}"
    );

    let distinct: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "every tool the server offers is still reachable: {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id.ends_with("__delete_everything")),
        "the tool's own name is what survives: {ids:?}"
    );

    // and installing them puts every one in, with nothing displaced
    let kernel = kernel();
    let installed = server.install(&kernel).await.unwrap();
    assert_eq!(installed.added.len(), ids.len());
    assert!(installed.replaced.is_empty(), "{:?}", installed.replaced);
}

#[tokio::test]
async fn a_single_server_can_offer_its_tools_unprefixed() {
    let (server, _) = bench("files").await;
    let kernel = kernel();

    server.without_prefix().install(&kernel).await.unwrap();

    assert!(kernel.tool("echo").is_some());
    assert!(kernel.tool("files__echo").is_none());
}

#[tokio::test]
async fn installing_twice_says_what_it_displaced() {
    let (server, _) = bench("files").await;
    let kernel = kernel();

    let first = server.install(&kernel).await.unwrap();
    let again = server.install(&kernel).await.unwrap();

    assert!(first.replaced.is_empty());
    assert_eq!(
        again.replaced, first.added,
        "a kernel holds one tool per name, and this one is not going to pretend otherwise"
    );

    assert_eq!(first.remove_from(&kernel), 5);
    assert!(kernel.tool_ids().is_empty());
}

// ------------------------------------------------------------------------------------ what it may do

#[tokio::test]
async fn by_default_a_tool_says_only_where_it_came_from() {
    let (server, _) = bench("files").await;
    let kernel = kernel();
    server.install(&kernel).await.unwrap();

    // the server claims `delete_everything` is read-only. Believing that would mean taking the
    // word of the thing being gated, so by default nothing is inferred from it at all
    let spec = kernel.tool("files__delete_everything").unwrap().spec();
    assert_eq!(
        spec.capabilities,
        vec![Capability::Custom("mcp:files".to_owned())]
    );

    // which leaves a policy one thing to decide, once, about the server as a whole
    let honest = kernel.tool("files__echo").unwrap().spec();
    assert_eq!(honest.capabilities, spec.capabilities);
}

#[tokio::test]
async fn trusting_the_annotations_is_something_you_have_to_say() {
    let (server, _) = bench("files").await;
    let kernel = kernel();
    server
        .trusting(Trust::Annotations)
        .install(&kernel)
        .await
        .unwrap();

    // now the hints are believed - which is the point, and the risk
    let claimed = kernel.tool("files__delete_everything").unwrap().spec();
    assert!(claimed.capabilities.contains(&Capability::Read));
    assert!(!claimed.capabilities.contains(&Capability::Write));

    // a tool that said nothing about itself is not thereby harmless
    let unannotated = kernel.tool("files__counts").unwrap().spec();
    assert!(unannotated.capabilities.contains(&Capability::Write));
    assert!(unannotated.capabilities.contains(&Capability::Edit));

    // and where it came from is recorded either way, because that part is a fact
    assert!(
        claimed
            .capabilities
            .contains(&Capability::Custom("mcp:files".to_owned()))
    );
}

#[tokio::test]
async fn a_fixed_set_of_capabilities_ignores_what_the_server_claims() {
    let (server, _) = bench("files").await;
    let kernel = kernel();
    server
        .trusting(Trust::Fixed(vec![Capability::Shell]))
        .install(&kernel)
        .await
        .unwrap();

    for id in kernel.tool_ids() {
        let spec = kernel.tool(&id).unwrap().spec();
        assert!(spec.capabilities.contains(&Capability::Shell), "{id}");
    }
}

// ------------------------------------------------------------------------------------- calling one

/// Invokes a tool the way the kernel would, without a whole loop around it.
async fn invoke(tool: &Arc<dyn Tool>, args: Value) -> ToolOutput {
    let call = ToolCall::new("c1", tool.spec().id, args);

    tool.invoke(&call, nachalnik::OutputSink::disconnected())
        .await
        .expect("the bridge answers rather than failing")
}

#[tokio::test]
async fn a_call_reaches_the_server_and_the_answer_comes_back() {
    let (server, calls) = bench("files").await;
    let kernel = kernel();
    server.install(&kernel).await.unwrap();

    let echo = kernel.tool("files__echo").unwrap();
    let output = invoke(&echo, json!({ "text": "hello" })).await;

    assert_eq!(calls.load(SeqCst), 1, "it really went to the server");
    assert_eq!(output.content.to_text(), "you said: hello");
    assert!(!output.is_error);
}

#[tokio::test]
async fn structured_content_stays_structured() {
    let (server, _) = bench("files").await;
    let kernel = kernel();
    server.install(&kernel).await.unwrap();

    let output = invoke(&kernel.tool("files__counts").unwrap(), json!({})).await;

    assert!(
        matches!(output.content, Content::Json(_)),
        "a server that returned structure meant it: {:?}",
        output.content
    );
    assert_eq!(output.content.to_text(), r#"{"files":3,"ok":true}"#);
}

#[tokio::test]
async fn a_tool_that_fails_is_an_error_result_rather_than_a_broken_loop() {
    let (server, _) = bench("files").await;
    let kernel = kernel();
    server.install(&kernel).await.unwrap();

    let output = invoke(&kernel.tool("files__explodes").unwrap(), json!({})).await;

    assert!(output.is_error, "the model is told, and can react");
    assert_eq!(output.content.to_text(), "it blew up");
}

#[tokio::test]
async fn content_that_cannot_be_text_is_named_rather_than_dropped() {
    let (server, _) = bench("files").await;
    let kernel = kernel();
    server.install(&kernel).await.unwrap();

    let output = invoke(&kernel.tool("files__draws").unwrap(), json!({})).await;
    let text = output.content.to_text();

    assert!(text.contains("image/png"), "{text}");
    assert!(
        text.contains("not carried into the context"),
        "a gap would be worse than a sentence saying what is missing: {text}"
    );
}

#[tokio::test]
async fn arguments_that_are_not_an_object_are_reported_not_reshaped() {
    let (server, calls) = bench("files").await;
    let kernel = kernel();
    server.install(&kernel).await.unwrap();

    let output = invoke(&kernel.tool("files__echo").unwrap(), json!("just a string")).await;

    assert!(output.is_error);
    assert_eq!(calls.load(SeqCst), 0, "and nothing was sent");
}

// ------------------------------------------------------------------------------- the whole loop

#[tokio::test]
async fn a_model_calls_a_tool_on_a_server_it_has_never_heard_of() {
    let (server, calls) = bench("files").await;
    let kernel = kernel();
    server.install(&kernel).await.unwrap();
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "files__echo",
            json!({ "text": "over the wire" }),
        )]),
        ModelResponse::text("it said it back"),
    ])));
    kernel.push(ContextItem::user("use the tool"));

    let State::Finished { .. } = kernel.turn().await.unwrap() else {
        panic!("the policy allows everything")
    };

    assert_eq!(calls.load(SeqCst), 1);
    let result = kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .expect("the result is in the context like any other");
    assert_eq!(result.content.to_text(), "you said: over the wire");
    assert_eq!(result.label, "files__echo");
}

#[tokio::test]
async fn the_policy_gates_a_whole_server_in_one_line() {
    let (server, calls) = bench("files").await;
    let kernel = kernel();
    server.install(&kernel).await.unwrap();
    // nothing from this server runs without being asked about, whatever it says about itself
    kernel.set_policy(Arc::new(DenyAll));
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "files__delete_everything", json!({}))]),
        ModelResponse::text("fine"),
    ])));
    kernel.push(ContextItem::user("delete everything"));

    kernel.turn().await.unwrap();

    assert_eq!(
        calls.load(SeqCst),
        0,
        "the claim of harmlessness bought it nothing"
    );
    let refused = kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .expect("a refusal is still an answer to the call");
    assert!(matches!(
        refused.kind,
        ContextKind::ToolResult { is_error: true, .. }
    ));
}

#[tokio::test]
async fn a_denied_call_can_still_be_allowed_by_the_person_watching() {
    let (server, calls) = bench("files").await;
    let kernel = kernel();
    server.install(&kernel).await.unwrap();
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "files__echo", json!({ "text": "may i" }))]),
        ModelResponse::text("done"),
    ])));
    kernel.push(ContextItem::user("ask first"));

    // the default policy asks about everything, and an MCP tool is no exception
    let State::Deciding { .. } = kernel.step().await.unwrap() else {
        panic!("it should have stopped to ask")
    };
    for request in kernel.pending_permissions() {
        assert_eq!(
            request.capabilities,
            vec![Capability::Custom("mcp:files".to_owned())],
            "which is all the policy is told, and all it needs"
        );
        kernel.decide(request.id, Grant::Allow).unwrap();
    }
    kernel.step().await.unwrap();

    assert_eq!(calls.load(SeqCst), 1);
}

// ---------------------------------------------------------------------------------- resources

#[tokio::test]
async fn resources_arrive_as_items_to_push_or_not() {
    let (server, _) = bench("files").await;

    let items = server.resources().await.unwrap();

    assert_eq!(items.len(), 2);
    assert_eq!(items[0].label, "file:///notes.md");
    assert_eq!(items[0].source, "mcp");
    assert_eq!(items[0].content.to_text(), "remember the milk");
    assert!(
        items[0].included_because.is_some(),
        "an item in a context says why it is there"
    );

    // the blob is named rather than dropped, the same way a tool result's image blocks are. A
    // list that came back one shorter than the server's would leave a caller unable to say
    // whether a document had been missed or had never been offered
    assert_eq!(items[1].label, "file:///logo.png");
    assert_eq!(
        items[1].content.to_text(),
        "[a resource with no text (image/png), not carried into the context]"
    );

    // and nothing was pushed anywhere: a server offering documents is not an argument
    let kernel = kernel();
    assert!(kernel.items().is_empty());
}

#[tokio::test]
async fn the_tools_can_be_looked_at_before_anything_is_registered() {
    let (server, _) = bench("files").await;
    let kernel = kernel();

    // `install` is the convenient answer, but it is built on this one: a list handed back, with
    // nothing registered anywhere yet. Somebody deciding *whether* to install a server - or which
    // of its tools to take - needs to see what it offers first
    let offered = server.tools().await.expect("the server lists them");

    assert!(!offered.is_empty());
    assert!(
        kernel.tool_ids().is_empty(),
        "looking at them should not have registered any"
    );

    // they are ordinary tools, carrying the server's name as a capability, and adding one by hand
    // works exactly as adding any other tool does
    let one = offered
        .iter()
        .find(|tool| tool.spec().id == "files__counts")
        .expect("the prefix is the server's name");
    assert!(
        one.spec()
            .capabilities
            .contains(&Capability::Custom("mcp:files".into())),
        "{:?}",
        one.spec().capabilities
    );

    kernel.add_tool(one.clone());
    assert_eq!(kernel.tool_ids(), vec!["files__counts"]);
}
