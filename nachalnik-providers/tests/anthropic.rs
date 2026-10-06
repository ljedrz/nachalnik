//! What the Anthropic provider makes of a turn, in both directions.
//!
//! note: a socket for the reading half and a plain call to `render` for the writing half, for the
//! reasons `gemini.rs` gives. The streams below are trimmed from what the API really sent through
//! OpenRouter's `/api/v1/messages`, pinned to Anthropic: the event names, the empty `signature` a
//! thinking block opens with, the `caller` on a call, the `[DONE]` OpenRouter adds at the end.
//!
//! note: the test that matters most is the round trip. A turn that thought and then asked for a
//! tool has to send that thinking back, signed, with the tool's result, or the next request is
//! refused; `anthropic_live.rs` is where a real endpoint says whether it was.

#![cfg(feature = "anthropic")]

use std::sync::Arc;

use nachalnik::{
    Block, Config, Content, ContextItem, Kernel, LinearProjector, ModelResponse, Part, Provider,
    StopReason, ToolCall, ToolCallId,
};
use nachalnik_providers::Anthropic;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Answers one request with this body, as a stream.
async fn server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("the request");
        let mut discard = [0u8; 16384];
        let _ = socket.read(&mut discard).await;
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = socket.flush().await;
        let _ = socket.shutdown().await;
    });

    format!("http://{address}")
}

/// Asks once, and hands back the kernel and what the provider made of the answer.
async fn answered(body: &'static str) -> (Kernel, Arc<Anthropic>, Arc<ModelResponse>) {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(Anthropic::new(
        "claude-test",
        server(body).await,
        "no key needed",
    ));
    kernel.set_provider(provider.clone());
    kernel.set_projector(Arc::new(LinearProjector {
        send_blocks: true,
        ..Default::default()
    }));
    kernel.push(ContextItem::user("read notes.md"));
    kernel.step().await.expect("the request is answered");

    let response = kernel.last_response().expect("the model answered");
    (kernel, provider, response)
}

/// A turn that thought and then asked for a tool, as the API streamed it.
const THOUGHT_THEN_CALLED: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"gen-1\",\"type\":\"message\",",
    "\"role\":\"assistant\",\"content\":[],\"model\":\"anthropic/claude-haiku-4.5\",",
    "\"usage\":{\"input_tokens\":588,\"output_tokens\":6,\"output_tokens_details\":null,",
    "\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0},\"provider\":\"Anthropic\"}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",",
    "\"thinking\":\"\",\"signature\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",",
    "\"thinking\":\"The user wants me to read\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",",
    "\"thinking\":\" a file.\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",",
    "\"signature\":\"EpwDCtgBCBIYAipA\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",",
    "\"id\":\"toolu_01EhmRMhoGZgGpjbopaNeBQm\",\"caller\":{\"type\":\"direct\"},\"name\":\"read\",",
    "\"input\":{}}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",",
    "\"partial_json\":\"{\\\"path\\\": \\\"notes.md\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",",
    "\"partial_json\":\"\\\"}\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},",
    "\"usage\":{\"input_tokens\":588,\"output_tokens\":86,\"output_tokens_details\":",
    "{\"thinking_tokens\":30},\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,",
    "\"cost\":0.000058}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
    "event: data\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn a_streamed_turn_keeps_its_order_its_signature_and_its_call() {
    let (_, _, response) = answered(THOUGHT_THEN_CALLED).await;
    let blocks = response
        .content
        .as_ref()
        .and_then(Content::as_blocks)
        .expect("a turn of blocks");

    assert_eq!(blocks.len(), 2, "{blocks:?}");
    match &blocks[0] {
        Block::Reasoning(part) => {
            assert_eq!(part.content.to_text(), "The user wants me to read a file.");
            assert_eq!(
                part.extra["signature"], "EpwDCtgBCBIYAipA",
                "the signature arrived in a delta of its own and rides on the block"
            );
        }
        other => panic!("the thinking came first: {other:?}"),
    }
    match &blocks[1] {
        Block::Call(call) => {
            assert_eq!(call.id.0, "toolu_01EhmRMhoGZgGpjbopaNeBQm");
            assert_eq!(call.tool, "read");
            assert_eq!(
                *call.args,
                json!({ "path": "notes.md" }),
                "the fragments are the arguments"
            );
        }
        other => panic!("then the call: {other:?}"),
    }
    assert_eq!(response.stop, StopReason::ToolUse);

    let usage = response.usage.expect("the cost is carried through");
    assert_eq!(usage.input_tokens, Some(588));
    assert_eq!(
        usage.output_tokens,
        Some(86),
        "the thinking is inside it already"
    );
    assert_eq!(usage.reasoning_tokens, Some(30));
}

#[tokio::test]
async fn what_came_off_the_wire_goes_back_on_to_it_signed() {
    // the assertion this provider exists for: a turn that thought before a call sends that
    // thinking back with the call's result, signature and all, or the next request is refused
    // the kernel has answered the call already - with no tool of that name, which is an answer -
    // so the next request is the one that carries the thinking back
    let (kernel, provider, _) = answered(THOUGHT_THEN_CALLED).await;

    let sent = provider
        .render(&kernel.preview_request().expect("a request"))
        .expect("rendered");
    let messages = sent["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 3, "{sent}");

    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(
        messages[1]["content"],
        json!([
            {
                "type": "thinking",
                "thinking": "The user wants me to read a file.",
                "signature": "EpwDCtgBCBIYAipA",
            },
            {
                "type": "tool_use",
                "id": "toolu_01EhmRMhoGZgGpjbopaNeBQm",
                "name": "read",
                "input": { "path": "notes.md" },
                "caller": { "type": "direct" },
            },
        ])
    );
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(
        messages[2]["content"][0]["tool_use_id"],
        "toolu_01EhmRMhoGZgGpjbopaNeBQm"
    );
}

/// A whole message where a stream was asked for is read as the stream it would have been.
#[tokio::test]
async fn a_whole_message_is_read_as_its_blocks() {
    const WHOLE: &str = concat!(
        "{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[",
        "{\"type\":\"text\",\"text\":\"Reading it.\"},",
        "{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read\",",
        "\"input\":{\"path\":\"notes.md\"}}],",
        "\"stop_reason\":\"tool_use\",\"usage\":{\"input_tokens\":20,\"output_tokens\":9}}",
    );

    let (_, _, response) = answered(WHOLE).await;
    assert_eq!(
        response
            .content
            .as_ref()
            .map(|said| said.to_text().into_owned()),
        Some("Reading it.".to_owned())
    );
    let calls: Vec<_> = response.calls().collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(*calls[0].args, json!({ "path": "notes.md" }));
    assert_eq!(response.stop, StopReason::ToolUse);
    assert_eq!(
        response.usage.and_then(|usage| usage.output_tokens),
        Some(9)
    );
}

/// What was read from the cache is part of the prompt, as `Usage` counts one.
///
/// note: this API's `input_tokens` is only what was neither read from the cache nor written to
/// it. Reported as it stands, a long conversation served from the cache reads as a few tokens.
#[tokio::test]
async fn a_cached_prompt_is_counted_whole() {
    const CACHED: &str = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,",
        "\"cache_creation_input_tokens\":300,\"cache_read_input_tokens\":4000,",
        "\"output_tokens\":1}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":",
        "{\"type\":\"text\",\"text\":\"ok\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},",
        "\"usage\":{\"output_tokens\":2}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let (_, _, response) = answered(CACHED).await;
    let usage = response.usage.expect("reported");
    assert_eq!(usage.input_tokens, Some(4312));
    assert_eq!(usage.cached_input_tokens, Some(4000));
    assert_eq!(usage.output_tokens, Some(2));
    assert_eq!(
        usage.reasoning_tokens, None,
        "nothing was said about thinking"
    );
}

#[tokio::test]
async fn the_reasons_a_turn_ends_are_read() {
    const REFUSED: &str = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\"},",
        "\"usage\":{\"output_tokens\":0}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    const OUT_OF_ROOM: &str = concat!(
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":",
        "{\"type\":\"text\",\"text\":\"and then\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},",
        "\"usage\":{\"output_tokens\":64}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    assert_eq!(answered(REFUSED).await.2.stop, StopReason::Refusal);
    assert_eq!(answered(OUT_OF_ROOM).await.2.stop, StopReason::Length);
}

// -------------------------------------------------------------------------------- going out

/// The payload this provider would send for a context.
fn rendered(items: Vec<ContextItem>) -> Value {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(Anthropic::new(
        "claude-test",
        "http://127.0.0.1:1",
        "no key",
    ));
    kernel.set_provider(provider.clone());
    kernel.set_projector(Arc::new(LinearProjector {
        send_blocks: true,
        ..Default::default()
    }));
    kernel.push_all(items);

    provider
        .render(&kernel.preview_request().expect("a request"))
        .expect("this provider always renders")
}

#[test]
fn a_request_carries_what_this_api_requires_and_its_instructions_apart() {
    let body = rendered(vec![
        ContextItem::system("be terse"),
        ContextItem::user("hello"),
    ]);

    assert_eq!(body["model"], "claude-test");
    assert_eq!(body["system"], "be terse");
    assert_eq!(body["stream"], true);
    assert!(
        body["max_tokens"].as_u64().is_some_and(|most| most > 0),
        "this API refuses a request without one: {body}"
    );
    assert_eq!(
        body["messages"],
        json!([{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }])
    );
}

#[test]
fn the_tools_registered_are_declared_with_their_schema() {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(Anthropic::new(
        "claude-test",
        "http://127.0.0.1:1",
        "no key",
    ));
    kernel.set_provider(provider.clone());
    kernel.add_tool(Arc::new(nachalnik::test::EchoTool::new("echo", [])));
    kernel.push(ContextItem::user("go"));

    let body = provider.render(&kernel.preview_request().unwrap()).unwrap();
    assert_eq!(body["tools"][0]["name"], "echo");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
}

#[test]
fn a_parameter_adds_to_the_request_without_replacing_what_it_is_built_from() {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(Anthropic::new(
        "claude-test",
        "http://127.0.0.1:1",
        "no key",
    ));
    kernel.set_provider(provider.clone());
    let mut params = nachalnik::Params::new();
    params.insert("max_tokens".to_owned(), json!(512));
    params.insert(
        "thinking".to_owned(),
        json!({ "type": "enabled", "budget_tokens": 256 }),
    );
    params.insert("messages".to_owned(), json!([]));
    params.insert("system".to_owned(), json!("not this"));
    kernel.set_params(params);
    kernel.push(ContextItem::user("go"));

    let body = provider.render(&kernel.preview_request().unwrap()).unwrap();
    assert_eq!(body["max_tokens"], 512, "the default is a default");
    assert_eq!(body["thinking"]["budget_tokens"], 256);
    assert_eq!(
        body["messages"].as_array().map(Vec::len),
        Some(1),
        "not replaced"
    );
    assert!(body.get("system").is_none(), "nor are the instructions");
}

/// In a user turn the results come first, whatever was put between the call and them.
///
/// note: this API refuses a turn that answers a call after something else has been said, and a
/// note or a reference the kernel put down while the tool ran is exactly that.
#[test]
fn a_tool_result_comes_first_in_the_turn_that_carries_it() {
    let body = rendered(vec![
        ContextItem::user("go"),
        ContextItem::assistant(
            Content::blocks([Block::Call(ToolCall::new("c1", "read", json!({})))]),
            Vec::new(),
        ),
        ContextItem::memory("note", "the codename is kotelnaya"),
        ContextItem::tool_result(ToolCallId::from("c1"), "read", "the notes", false),
    ]);

    let turn = body["messages"][2]["content"]
        .as_array()
        .expect("one user turn");
    assert_eq!(turn.len(), 2, "{body}");
    assert_eq!(turn[0]["type"], "tool_result");
    assert_eq!(turn[1]["type"], "text");
}

/// What another provider attached to a turn does not reach this one.
///
/// note: a session that started against Gemini carries `thoughtSignature` on its parts and
/// unsigned thinking, and either is a 400 here. Thinking with no signature is left out; a field
/// this API does not define for a block is not put on it.
#[test]
fn what_another_provider_attached_is_not_sent_here() {
    let foreign = json!({ "thoughtSignature": "SIG-GEMINI" });
    let body = rendered(vec![
        ContextItem::user("go"),
        ContextItem::assistant(
            Content::blocks([
                Block::Reasoning(Part::new("working it out").with_extra(foreign.clone())),
                Block::Text(Part::new("Checking.").with_extra(foreign.clone())),
                Block::Call(ToolCall::new("c1", "read", json!({})).with_extra(foreign)),
            ]),
            Vec::new(),
        ),
        ContextItem::tool_result(ToolCallId::from("c1"), "read", "the notes", false),
    ]);

    assert_eq!(
        body["messages"][1]["content"],
        json!([
            { "type": "text", "text": "Checking." },
            { "type": "tool_use", "id": "c1", "name": "read", "input": {} },
        ])
    );
}

/// Thinking this API would not show goes back exactly as it came.
#[test]
fn redacted_thinking_goes_back_as_it_came() {
    let redacted = json!({ "type": "redacted_thinking", "data": "EmwKAhgB" });
    let body = rendered(vec![
        ContextItem::user("go"),
        ContextItem::assistant(
            Content::blocks([
                Block::Reasoning(Part::new("").with_extra(redacted.clone())),
                Block::Text(Part::new("Done.")),
            ]),
            Vec::new(),
        ),
        ContextItem::user("and now?"),
    ]);

    assert_eq!(body["messages"][1]["content"][0], redacted);
}

/// An identifier this API would refuse is rewritten the same way on the call and on its result.
#[test]
fn a_call_s_identifier_is_one_this_api_takes_on_both_halves_of_the_pair() {
    let body = rendered(vec![
        ContextItem::user("go"),
        ContextItem::assistant(
            Content::blocks([Block::Call(ToolCall::new(
                "functions.read:0",
                "read",
                json!({}),
            ))]),
            Vec::new(),
        ),
        ContextItem::tool_result(
            ToolCallId::from("functions.read:0"),
            "read",
            "the notes",
            false,
        ),
    ]);

    let asked = &body["messages"][1]["content"][0]["id"];
    let answered = &body["messages"][2]["content"][0]["tool_use_id"];
    assert_eq!(*asked, "functions_read_0");
    assert_eq!(asked, answered);
}

/// A tool's JSON is handed over as the text of it, and a picture as an image block.
#[test]
fn a_result_is_text_or_the_blocks_it_is_made_of() {
    let body = rendered(vec![
        ContextItem::user("go"),
        ContextItem::assistant(
            Content::blocks([
                Block::Call(ToolCall::new("c1", "count", json!({}))),
                Block::Call(ToolCall::new("c2", "look", json!({}))),
            ]),
            Vec::new(),
        ),
        ContextItem::tool_result(
            ToolCallId::from("c1"),
            "count",
            Content::json(json!({ "lines": 12 })),
            false,
        ),
        ContextItem::tool_result(
            ToolCallId::from("c2"),
            "look",
            Content::blob("image/png", "iVBORw0KGgo="),
            false,
        ),
    ]);

    let results = body["messages"][2]["content"].as_array().expect("results");
    assert_eq!(results[0]["content"], "{\"lines\":12}");
    assert_eq!(
        results[1]["content"],
        json!([{
            "type": "image",
            "source": { "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo=" },
        }])
    );
}
