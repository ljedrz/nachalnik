//! What the OpenAI provider makes of a turn in Responses mode, in both directions.
//!
//! note: a socket for the reading half and a plain call to `render` for the writing half, for the
//! reasons `gemini.rs` gives. The streams below are trimmed from what the API really sent through
//! OpenRouter's `/api/v1/responses`, pinned to OpenAI: the two events that only say a response
//! exists, the reasoning item that opens with no summary and ends sealed, OpenRouter's own
//! `fc_tmp_` ids and `format`, and the `[DONE]` it adds at the end.
//!
//! note: the test that matters most is the round trip. A turn that reasoned and then asked for a
//! tool sends the sealed reasoning back with the call's result; `responses_live.rs` is where a real
//! endpoint says whether it took it.

#![cfg(feature = "openai")]

use std::sync::Arc;

use nachalnik::{
    Block, Config, Content, ContextItem, Kernel, ModelResponse, Part, Provider, StopReason,
    ToolCall, ToolCallId,
};
use nachalnik_providers::{Dialect, OpenAiCompatible};
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

/// A provider in Responses mode, at this address.
fn provider(at: String) -> Arc<OpenAiCompatible> {
    Arc::new(OpenAiCompatible::new("openai/gpt-test", at, "no key needed").responses(true))
}

/// A kernel set up the way a program holding this provider as a `Dialect` would set it up.
fn kernel_for(provider: &Arc<OpenAiCompatible>) -> Kernel {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider.clone());
    kernel.set_projector(Arc::new(provider.projection()));
    kernel
}

/// Asks once, and hands back the kernel and what the provider made of the answer.
async fn answered(body: &'static str) -> (Kernel, Arc<OpenAiCompatible>, Arc<ModelResponse>) {
    let provider = provider(server(body).await);
    let kernel = kernel_for(&provider);
    kernel.push(ContextItem::user("read notes.md"));
    kernel.step().await.expect("the request is answered");

    let response = kernel.last_response().expect("the model answered");
    (kernel, provider, response)
}

/// What answering failed with.
async fn failed(body: &'static str) -> String {
    let provider = provider(server(body).await);
    let kernel = kernel_for(&provider);
    kernel.push(ContextItem::user("read notes.md"));
    match kernel.step().await {
        Ok(state) => panic!("answered: {state:?}"),
        Err(e) => e.to_string(),
    }
}

/// A turn that reasoned and then asked for a tool, as the API streamed it.
const THOUGHT_THEN_CALLED: &str = concat!(
    ": OPENROUTER PROCESSING\n\n",
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"gen-1\",\"object\":\"response\",",
    "\"status\":\"in_progress\",\"output\":[],\"error\":null,\"usage\":null},\"sequence_number\":0}\n\n",
    "data: {\"type\":\"response.in_progress\",\"response\":{\"id\":\"gen-1\",\"object\":\"response\",",
    "\"status\":\"in_progress\",\"output\":[],\"error\":null,\"usage\":null},\"sequence_number\":1}\n\n",
    "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_05743\",",
    "\"type\":\"reasoning\",\"status\":\"in_progress\",\"summary\":[]},\"sequence_number\":2}\n\n",
    "data: {\"type\":\"response.reasoning_summary_part.added\",\"output_index\":0,",
    "\"item_id\":\"rs_05743\",\"summary_index\":0,\"part\":{\"type\":\"summary_text\",\"text\":\"\"},",
    "\"sequence_number\":3}\n\n",
    "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,",
    "\"item_id\":\"rs_05743\",\"summary_index\":0,\"delta\":\"**Reading the file**\\n\\nThe user\",",
    "\"sequence_number\":4}\n\n",
    "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,",
    "\"item_id\":\"rs_05743\",\"summary_index\":0,\"delta\":\" wants notes.md.\",",
    "\"sequence_number\":5}\n\n",
    "data: {\"type\":\"response.reasoning_summary_text.done\",\"output_index\":0,",
    "\"item_id\":\"rs_05743\",\"summary_index\":0,",
    "\"text\":\"**Reading the file**\\n\\nThe user wants notes.md.\",\"sequence_number\":6}\n\n",
    "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_05743\",",
    "\"type\":\"reasoning\",\"status\":\"completed\",\"summary\":[{\"type\":\"summary_text\",",
    "\"text\":\"**Reading the file**\\n\\nThe user wants notes.md.\"}],",
    "\"encrypted_content\":\"gAAAAABqxNvi3bGC\",\"format\":\"openai-responses-v1\"},",
    "\"sequence_number\":7}\n\n",
    "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{",
    "\"id\":\"fc_tmp_lrm02dfdq2\",\"type\":\"function_call\",\"status\":\"in_progress\",",
    "\"call_id\":\"call_kp7PhFaq\",\"name\":\"read\",\"arguments\":\"\"},\"sequence_number\":8}\n\n",
    "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,",
    "\"item_id\":\"fc_tmp_lrm02dfdq2\",\"delta\":\"{\\\"path\\\":\\\"no\",\"sequence_number\":9}\n\n",
    "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,",
    "\"item_id\":\"fc_tmp_lrm02dfdq2\",\"delta\":\"tes.md\\\"}\",\"sequence_number\":10}\n\n",
    "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":1,",
    "\"item_id\":\"fc_tmp_lrm02dfdq2\",\"name\":\"read\",",
    "\"arguments\":\"{\\\"path\\\":\\\"notes.md\\\"}\",\"sequence_number\":11}\n\n",
    "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{",
    "\"id\":\"fc_tmp_lrm02dfdq2\",\"type\":\"function_call\",\"status\":\"completed\",",
    "\"call_id\":\"call_kp7PhFaq\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"notes.md\\\"}\"},",
    "\"sequence_number\":12}\n\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"gen-1\",\"object\":\"response\",",
    "\"status\":\"completed\",\"output\":[],\"error\":null,\"incomplete_details\":null,",
    "\"usage\":{\"input_tokens\":59,\"input_tokens_details\":{\"cached_tokens\":0},",
    "\"output_tokens\":89,\"output_tokens_details\":{\"reasoning_tokens\":64},",
    "\"total_tokens\":148,\"cost\":0.00003855}},\"sequence_number\":13}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn a_streamed_turn_keeps_its_order_its_sealed_reasoning_and_its_call() {
    let (_, _, response) = answered(THOUGHT_THEN_CALLED).await;
    let blocks = response
        .content
        .as_ref()
        .and_then(Content::as_blocks)
        .expect("a turn of blocks");

    assert_eq!(blocks.len(), 2, "{blocks:?}");
    match &blocks[0] {
        Block::Reasoning(part) => {
            assert_eq!(
                part.content.to_text(),
                "**Reading the file**\n\nThe user wants notes.md."
            );
            assert_eq!(
                part.extra["encrypted_content"], "gAAAAABqxNvi3bGC",
                "the sealed reasoning arrived on the finished item and rides on the block"
            );
            assert_eq!(part.extra["id"], "rs_05743");
            assert!(
                part.extra.get("format").is_none(),
                "OpenRouter's own field is not kept: {:?}",
                part.extra
            );
        }
        other => panic!("the reasoning came first: {other:?}"),
    }
    match &blocks[1] {
        Block::Call(call) => {
            assert_eq!(call.id.0, "call_kp7PhFaq", "the call id, not the item's");
            assert_eq!(call.tool, "read");
            assert_eq!(*call.args, json!({ "path": "notes.md" }));
        }
        other => panic!("then the call: {other:?}"),
    }
    assert_eq!(response.stop, StopReason::ToolUse);

    let usage = response.usage.expect("the cost is carried through");
    assert_eq!(usage.input_tokens, Some(59));
    assert_eq!(usage.output_tokens, Some(89));
    assert_eq!(usage.reasoning_tokens, Some(64));
    assert_eq!(usage.cached_input_tokens, Some(0));
}

#[tokio::test]
async fn what_came_off_the_wire_goes_back_on_to_it_sealed() {
    // the kernel has answered the call already - with no tool of that name, which is an answer -
    // so the next request is the one that carries the reasoning back
    let (kernel, provider, _) = answered(THOUGHT_THEN_CALLED).await;

    let sent = provider
        .render(&kernel.preview_request().expect("a request"))
        .expect("rendered");
    let input = sent["input"].as_array().expect("input");
    assert_eq!(input.len(), 4, "{sent}");

    assert_eq!(
        input[0],
        json!({ "type": "message", "role": "user", "content": "read notes.md" })
    );
    assert_eq!(
        input[1],
        json!({
            "type": "reasoning",
            "id": "rs_05743",
            "summary": [{
                "type": "summary_text",
                "text": "**Reading the file**\n\nThe user wants notes.md.",
            }],
            "encrypted_content": "gAAAAABqxNvi3bGC",
        }),
        "only the fields this API defines, and not OpenRouter's `format`"
    );
    assert_eq!(
        input[2],
        json!({
            "type": "function_call",
            "call_id": "call_kp7PhFaq",
            "name": "read",
            "arguments": "{\"path\":\"notes.md\"}",
        }),
        "without OpenRouter's made-up item id"
    );
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_kp7PhFaq");
    assert!(input[3]["output"].is_string(), "{}", input[3]);
}

/// A whole response where a stream was asked for is read as the stream it would have been -
/// whitespace in front of it and all, which is how OpenRouter sends one.
#[tokio::test]
async fn a_whole_response_is_read_as_its_items() {
    const WHOLE: &str = concat!(
        "\n         \n",
        "{\"id\":\"gen-2\",\"object\":\"response\",\"status\":\"completed\",\"output\":[",
        "{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[],\"encrypted_content\":\"gAAA\"},",
        "{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[",
        "{\"type\":\"output_text\",\"text\":\"Reading it.\",\"annotations\":[]}]},",
        "{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read\",",
        "\"arguments\":\"{\\\"path\\\":\\\"notes.md\\\"}\"}],",
        "\"error\":null,\"incomplete_details\":null,",
        "\"usage\":{\"input_tokens\":20,\"input_tokens_details\":{\"cached_tokens\":0},",
        "\"output_tokens\":9,\"output_tokens_details\":{\"reasoning_tokens\":0}}}",
    );

    let (_, _, response) = answered(WHOLE).await;
    let blocks = response
        .content
        .as_ref()
        .and_then(Content::as_blocks)
        .expect("a turn of blocks");
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert_eq!(
        blocks[0].extra()["encrypted_content"],
        "gAAA",
        "sealed reasoning with no summary is still reasoning to send back"
    );
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

/// What was read from the cache is already inside the prompt's count, as `Usage` counts one.
#[tokio::test]
async fn a_text_answer_and_its_cached_prompt_are_read() {
    const SAID: &str = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\",",
        "\"output\":[],\"error\":null}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{",
        "\"id\":\"msg_tmp_3uf\",\"type\":\"message\",\"status\":\"in_progress\",",
        "\"role\":\"assistant\",\"content\":[]}}\n\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":0,",
        "\"item_id\":\"msg_tmp_3uf\",\"content_index\":0,\"part\":{\"type\":\"output_text\",",
        "\"text\":\"\",\"annotations\":[]}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,",
        "\"item_id\":\"msg_tmp_3uf\",\"content_index\":0,\"delta\":\"po\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,",
        "\"item_id\":\"msg_tmp_3uf\",\"content_index\":0,\"delta\":\"ng\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{",
        "\"id\":\"msg_tmp_3uf\",\"type\":\"message\",\"status\":\"completed\",",
        "\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"pong\",",
        "\"annotations\":[]}]}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",",
        "\"output\":[],\"error\":null,\"usage\":{\"input_tokens\":4312,",
        "\"input_tokens_details\":{\"cached_tokens\":4096},\"output_tokens\":2,",
        "\"output_tokens_details\":{\"reasoning_tokens\":0}}}}\n\n",
    );

    let (_, _, response) = answered(SAID).await;
    assert_eq!(
        response
            .content
            .as_ref()
            .map(|said| said.to_text().into_owned()),
        Some("pong".to_owned()),
        "said once, though both the fragments and the finished item carried it"
    );
    assert_eq!(response.stop, StopReason::EndTurn);
    let usage = response.usage.expect("reported");
    assert_eq!(usage.input_tokens, Some(4312));
    assert_eq!(usage.cached_input_tokens, Some(4096));
    assert_eq!(usage.reasoning_tokens, Some(0));
}

/// A summary in several parts is several paragraphs.
#[tokio::test]
async fn a_summary_in_parts_is_kept_in_paragraphs() {
    const PARTS: &str = concat!(
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{",
        "\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[]}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,",
        "\"summary_index\":0,\"delta\":\"First.\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,",
        "\"summary_index\":1,\"delta\":\"Second.\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{",
        "\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[",
        "{\"type\":\"summary_text\",\"text\":\"First.\"},",
        "{\"type\":\"summary_text\",\"text\":\"Second.\"}],\"encrypted_content\":\"gAAA\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{",
        "\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":1,\"delta\":\"ok\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",",
        "\"output\":[],\"error\":null}}\n\n",
    );

    let (_, _, response) = answered(PARTS).await;
    let blocks = response
        .content
        .as_ref()
        .and_then(Content::as_blocks)
        .expect("a turn of blocks");
    assert_eq!(
        blocks[0]
            .part()
            .map(|part| part.content.to_text().into_owned()),
        Some("First.\n\nSecond.".to_owned())
    );
}

#[tokio::test]
async fn the_reasons_a_turn_ends_are_read() {
    const OUT_OF_ROOM: &str = concat!(
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{",
        "\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[],\"encrypted_content\":\"gAAA\"}}\n\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",",
        "\"output\":[],\"error\":null,\"incomplete_details\":{\"reason\":\"max_output_tokens\"},",
        "\"usage\":{\"input_tokens\":12,\"output_tokens\":16}}}\n\n",
    );
    const REFUSED: &str = concat!(
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{",
        "\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\n",
        "data: {\"type\":\"response.refusal.delta\",\"output_index\":0,",
        "\"delta\":\"I can't help with that.\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",",
        "\"output\":[],\"error\":null}}\n\n",
    );

    assert_eq!(answered(OUT_OF_ROOM).await.2.stop, StopReason::Length);
    let refused = answered(REFUSED).await.2;
    assert_eq!(refused.stop, StopReason::Refusal);
    assert_eq!(
        refused
            .content
            .as_ref()
            .map(|said| said.to_text().into_owned()),
        Some("I can't help with that.".to_owned()),
        "a refusal is still what the model said"
    );
}

/// A failure reported as an event, after the two events that only say a response exists, is a
/// refusal in the server's own words rather than a turn that said nothing.
#[tokio::test]
async fn a_failure_before_anything_was_said_is_the_servers_refusal() {
    const FAILED: &str = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\",",
        "\"output\":[],\"error\":null}}\n\n",
        "data: {\"type\":\"response.in_progress\",\"response\":{\"status\":\"in_progress\",",
        "\"output\":[],\"error\":null}}\n\n",
        "data: {\"type\":\"error\",\"code\":\"invalid_prompt\",",
        "\"message\":\"Invalid prompt: the reasoning item was not followed.\",\"param\":null}\n\n",
    );
    const ENDED_FAILED: &str = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\",",
        "\"output\":[],\"error\":null}}\n\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"output\":[],",
        "\"error\":{\"code\":\"invalid_prompt\",\"message\":\"The model refused the prompt.\"}}}\n\n",
    );

    for (body, words) in [
        (
            FAILED,
            "Invalid prompt: the reasoning item was not followed.",
        ),
        (ENDED_FAILED, "The model refused the prompt."),
    ] {
        assert_eq!(
            failed(body).await,
            format!("the provider failed: {words}"),
            "the server's sentence, and only that"
        );
    }
}

// -------------------------------------------------------------------------------- going out

/// The payload this provider would send for a context, with these parameters.
fn rendered_with(items: Vec<ContextItem>, params: Value) -> Value {
    let provider = provider("http://127.0.0.1:1".to_owned());
    let kernel = kernel_for(&provider);
    kernel.set_params(serde_json::from_value(params).expect("parameters"));
    kernel.push_all(items);

    provider
        .render(&kernel.preview_request().expect("a request"))
        .expect("this provider always renders")
}

/// The payload this provider would send for a context.
fn rendered(items: Vec<ContextItem>) -> Value {
    rendered_with(items, json!({}))
}

#[test]
fn a_request_keeps_nothing_on_the_server_and_asks_for_its_reasoning_sealed() {
    let body = rendered(vec![ContextItem::user("hello")]);

    assert_eq!(body["model"], "openai/gpt-test");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert!(body.get("messages").is_none(), "{body}");
}

/// An instruction stays where it was put, so one added mid-session does not change the start of
/// the prompt this API caches by.
#[test]
fn an_instruction_stays_where_it_was_put() {
    let body = rendered(vec![
        ContextItem::system("be terse"),
        ContextItem::user("hello"),
        ContextItem::assistant("hi", Vec::new()),
        ContextItem::system("now in French"),
        ContextItem::user("again"),
    ]);

    let roles: Vec<&str> = body["input"]
        .as_array()
        .expect("input")
        .iter()
        .map(|item| item["role"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "system", "user"]);
    assert!(body.get("instructions").is_none(), "{body}");
}

#[test]
fn the_tools_registered_are_declared_flat() {
    let provider = provider("http://127.0.0.1:1".to_owned());
    let kernel = kernel_for(&provider);
    kernel.add_tool(Arc::new(nachalnik::test::EchoTool::new("echo", [])));
    kernel.push(ContextItem::user("go"));

    let body = provider.render(&kernel.preview_request().unwrap()).unwrap();
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "echo");
    assert_eq!(body["tools"][0]["parameters"]["type"], "object");
    assert!(body["tools"][0].get("function").is_none(), "{body}");
}

/// The sealed reasoning is asked for whatever else the caller included, unless the caller asked
/// the server to keep the conversation instead.
#[test]
fn an_include_of_the_callers_keeps_the_reasoning_asked_for() {
    let body = rendered_with(
        vec![ContextItem::user("go")],
        json!({ "include": ["message.output_text.logprobs"] }),
    );
    assert_eq!(
        body["include"],
        json!([
            "message.output_text.logprobs",
            "reasoning.encrypted_content"
        ])
    );

    let body = rendered_with(vec![ContextItem::user("go")], json!({ "store": true }));
    assert_eq!(body["store"], true);
    assert!(body.get("include").is_none(), "{body}");
}

#[test]
fn a_parameter_adds_to_the_request_without_replacing_what_it_is_built_from() {
    let body = rendered_with(
        vec![ContextItem::user("go")],
        json!({
            "reasoning": { "effort": "low" },
            "max_output_tokens": 512,
            "input": [],
            "model": "someone-else",
        }),
    );

    assert_eq!(body["reasoning"]["effort"], "low");
    assert_eq!(body["max_output_tokens"], 512);
    assert_eq!(body["model"], "openai/gpt-test", "not replaced");
    assert_eq!(
        body["input"].as_array().map(Vec::len),
        Some(1),
        "nor is the conversation"
    );
}

/// Reasoning with no sealed form - another provider's, or one recorded the conventional way - is
/// left out rather than sent and refused, and so are other providers' fields on a block.
#[test]
fn only_what_this_api_defines_goes_back_out() {
    let foreign = ContextItem::assistant(
        Content::blocks([
            Block::Reasoning(
                Part::new("thought elsewhere").with_extra(json!({ "signature": "EpwD" })),
            ),
            Block::Text(Part::new("Reading it.").with_extra(json!({ "thoughtSignature": "CiQB" }))),
            Block::Call(
                ToolCall::new("toolu_1", "read", json!({ "path": "notes.md" }))
                    .with_extra(json!({ "caller": { "type": "direct" } })),
            ),
        ]),
        Vec::new(),
    );
    let body = rendered(vec![
        ContextItem::user("go"),
        foreign,
        ContextItem::tool_result(ToolCallId::from("toolu_1"), "read", "the notes", false),
    ]);

    let input = body["input"].as_array().expect("input");
    assert_eq!(
        input[1..3],
        [
            json!({ "type": "message", "role": "assistant", "content": "Reading it." }),
            json!({
                "type": "function_call",
                "call_id": "toolu_1",
                "name": "read",
                "arguments": "{\"path\":\"notes.md\"}",
            }),
        ]
    );
}

/// A reasoning item has to be followed by what it reasoned towards, and a turn cut off after it
/// thought and before it said anything would otherwise refuse every request after it.
#[test]
fn reasoning_with_nothing_after_it_is_not_sent() {
    let cut_off = ContextItem::assistant(
        Content::blocks([Block::Reasoning(
            Part::new("about to").with_extra(json!({ "id": "rs_1", "encrypted_content": "gAAA" })),
        )]),
        Vec::new(),
    );
    let body = rendered(vec![
        ContextItem::user("go"),
        cut_off,
        ContextItem::user("go on"),
    ]);

    let kinds: Vec<&str> = body["input"]
        .as_array()
        .expect("input")
        .iter()
        .map(|item| item["type"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(kinds, ["message", "message"], "{body}");
}

/// A picture goes out as an `input_image`, from a person and from a tool alike, and a document as
/// an `input_file`.
#[test]
fn a_picture_and_a_document_go_out_as_input_parts() {
    let picture = Content::blob("image/png", "iVBORw0KGgo=");
    let document = Content::blob("application/pdf", "JVBERi0x");
    let body = rendered(vec![
        ContextItem::user(Content::blocks([
            Block::text("what is this?"),
            Block::text(picture.clone()),
            Block::text(document),
        ])),
        ContextItem::assistant(
            Content::blocks([Block::Call(ToolCall::new(
                "call_1",
                "screenshot",
                json!({}),
            ))]),
            Vec::new(),
        ),
        ContextItem::tool_result(ToolCallId::from("call_1"), "screenshot", picture, false),
    ]);

    let input = body["input"].as_array().expect("input");
    assert_eq!(
        input[0]["content"],
        json!([
            { "type": "input_text", "text": "what is this?" },
            { "type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo=" },
            {
                "type": "input_file",
                "filename": "file.pdf",
                "file_data": "data:application/pdf;base64,JVBERi0x",
            },
        ])
    );
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(
        input[2]["output"],
        json!([{ "type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo=" }])
    );
}

/// The mode is projected as blocks, and chat completions as it always was.
#[test]
fn the_mode_decides_the_projection() {
    let responses = provider("http://127.0.0.1:1".to_owned());
    assert!(responses.projection().send_blocks);

    let completions = OpenAiCompatible::new("m", "http://127.0.0.1:1", "");
    assert!(!completions.projection().send_blocks);
    assert!(!completions.projection().send_reasoning);
}
