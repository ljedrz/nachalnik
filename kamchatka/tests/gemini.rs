//! What the Gemini provider makes of a turn, in both directions.
//!
//! note: a socket rather than a unit test over a parser, for the same reason `streaming.rs` uses
//! one: the assembly of a streamed turn happens inside `respond`, between reading bytes off a
//! response and handing back a `ModelResponse`, and a test that reached in beside it would be
//! testing a copy of the code under test. The bodies below are the shapes the real API sent,
//! trimmed.
//!
//! note: the rendering half needs no socket, because `render` is a pure function of the request
//! and `respond` sends exactly what it returns. The test that matters most is the round trip: a
//! turn that came off the wire goes back on to it unchanged, signatures and all, because that is
//! what this API rejects the next request over.

use std::sync::Arc;

use kamchatka::gemini::Gemini;
use nachalnik::{
    Block, Config, Content, ContextItem, ContextKind, Kernel, LinearProjector, ModelResponse,
    Provider, StopReason, ToolCall, ToolCallId,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Answers one request with this body, as a stream, and remembers what it was asked.
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

/// Asks once, and hands back what the provider made of the answer.
async fn answered(body: &'static str) -> Arc<ModelResponse> {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(Gemini::new(
        "gemini-test",
        server(body).await,
        "no key needed",
    )));
    kernel.push(ContextItem::user("go"));
    kernel.step().await.expect("the request is answered");

    kernel.last_response().expect("the model answered")
}

/// A turn that thought, spoke and then asked for a tool - the shape the API really sends, with
/// the signature on the part it belongs to.
const ORDERED: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"**Working it out**\",",
    "\"thought\":true}],\"role\":\"model\"}}],\"usageMetadata\":{\"promptTokenCount\":67}}\n\n",
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Checking Warsaw.\"}],",
    "\"role\":\"model\"}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"weather\",",
    "\"args\":{\"city\":\"Warsaw\"},\"id\":\"call_1\"},\"thoughtSignature\":\"SIG-CALL\"}],",
    "\"role\":\"model\"}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"And now Krakow.\"}],",
    "\"role\":\"model\"}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"weather\",",
    "\"args\":{\"city\":\"Krakow\"},\"id\":\"call_2\"}}],\"role\":\"model\"},",
    "\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":67,",
    "\"candidatesTokenCount\":40,\"thoughtsTokenCount\":12}}\n\n",
);

/// A turn that only talks, arriving as a part per chunk, signed at the end.
const SPOKEN: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Thinking\",\"thought\":true}]}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Nine\"}]}}]}\n\n",
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\" sheep.\",",
    "\"thoughtSignature\":\"SIG-TEXT\"}]},\"finishReason\":\"STOP\"}]}\n\n",
);

// -------------------------------------------------------------------------------- coming in

#[tokio::test]
async fn a_streamed_turn_keeps_the_order_it_arrived_in() {
    let response = answered(ORDERED).await;

    let blocks = response
        .content
        .as_ref()
        .and_then(Content::as_blocks)
        .expect("recorded as an order");
    assert_eq!(
        blocks.iter().map(Block::name).collect::<Vec<_>>(),
        ["reasoning", "text", "call", "text", "call"]
    );

    // the conventional slots are empty, because they are the other way of recording the same turn
    assert!(response.tool_calls.is_empty());
    assert!(response.reasoning.is_none());
    // and the calls are found anyway, which is what the kernel runs
    assert_eq!(
        response
            .calls()
            .map(|call| call.id.0.as_str())
            .collect::<Vec<_>>(),
        ["call_1", "call_2"]
    );

    // `STOP` is what this API says for a turn that asked for two tools; the parts are what say
    // otherwise, and a turn reported as finished would never have run either of them
    assert_eq!(response.stop, StopReason::ToolUse);

    let usage = response.usage.expect("reported");
    assert_eq!(usage.input_tokens, Some(67));
    assert_eq!(usage.output_tokens, Some(40));
    assert_eq!(usage.reasoning_tokens, Some(12));
}

#[tokio::test]
async fn a_run_of_parts_of_one_kind_is_one_block() {
    let response = answered(SPOKEN).await;
    let blocks = response
        .content
        .as_ref()
        .and_then(Content::as_blocks)
        .expect("recorded as an order");

    // three chunks, two blocks: the model said one thing in two pieces
    assert_eq!(
        blocks.iter().map(Block::name).collect::<Vec<_>>(),
        ["reasoning", "text"]
    );
    assert_eq!(blocks[1].said().unwrap().content.to_text(), "Nine sheep.");
    assert_eq!(response.stop, StopReason::EndTurn);
}

#[tokio::test]
async fn a_signature_rides_on_the_part_it_belongs_to() {
    let ordered = answered(ORDERED).await;
    let blocks = ordered
        .content
        .as_ref()
        .and_then(Content::as_blocks)
        .unwrap();

    // on a call, where the kernel's own `extra` already carried it
    assert_eq!(blocks[2].extra()["thoughtSignature"], "SIG-CALL");
    // and not on the ones it did not arrive on
    assert!(blocks[0].extra().is_null());
    assert!(blocks[4].extra().is_null());

    // on a text part, which is the one a message has nowhere to keep
    let spoken = answered(SPOKEN).await;
    let blocks = spoken
        .content
        .as_ref()
        .and_then(Content::as_blocks)
        .unwrap();
    assert_eq!(blocks[1].extra()["thoughtSignature"], "SIG-TEXT");
}

// -------------------------------------------------------------------------------- going out

/// The payload this provider would send for a context.
fn rendered(items: Vec<ContextItem>, send_blocks: bool) -> Value {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(Gemini::new("gemini-test", "http://127.0.0.1:1", "no key"));
    kernel.set_provider(provider.clone());
    kernel.set_projector(Arc::new(LinearProjector {
        send_blocks,
        ..Default::default()
    }));
    kernel.push_all(items);

    provider
        .render(&kernel.preview_request().expect("a request"))
        .expect("this provider always renders")
}

#[test]
fn instructions_leave_the_conversation_rather_than_sitting_at_the_top_of_it() {
    let body = rendered(
        vec![ContextItem::system("be terse"), ContextItem::user("hello")],
        true,
    );

    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be terse");
    // and it is not also a turn, which would be the same words said twice
    assert_eq!(body["contents"].as_array().unwrap().len(), 1);
    assert_eq!(body["contents"][0]["role"], "user");
    assert_eq!(body["contents"][0]["parts"][0]["text"], "hello");
}

#[test]
fn an_ordered_turn_goes_back_out_in_order_with_what_was_attached_to_it() {
    let call = ToolCall::new("call_1", "weather", json!({ "city": "Warsaw" }))
        .with_extra(json!({ "thoughtSignature": "SIG-CALL" }));
    let body = rendered(
        vec![
            ContextItem::user("what is the weather?"),
            ContextItem::assistant(
                Content::blocks([
                    Block::reasoning("thinking about it"),
                    Block::Text(
                        nachalnik::Part::new("Checking Warsaw.")
                            .with_extra(json!({ "thoughtSignature": "SIG-TEXT" })),
                    ),
                    Block::Call(call),
                ]),
                Vec::new(),
            ),
            ContextItem::tool_result(ToolCallId::from("call_1"), "weather", "sunny", false),
        ],
        true,
    );

    let turn = &body["contents"][1];
    assert_eq!(turn["role"], "model");
    let parts = turn["parts"].as_array().expect("parts");
    assert_eq!(parts.len(), 3);

    // the thinking is marked as thinking, so it is not read back as something the model said
    assert_eq!(parts[0]["text"], "thinking about it");
    assert_eq!(parts[0]["thought"], true);
    // every signature is beside the part it signed, which is the whole reason for the exercise
    assert_eq!(parts[1]["text"], "Checking Warsaw.");
    assert_eq!(parts[1]["thoughtSignature"], "SIG-TEXT");
    assert!(parts[1]["thought"].is_null());
    assert_eq!(parts[2]["functionCall"]["name"], "weather");
    assert_eq!(parts[2]["functionCall"]["args"]["city"], "Warsaw");
    assert_eq!(parts[2]["functionCall"]["id"], "call_1");
    assert_eq!(parts[2]["thoughtSignature"], "SIG-CALL");

    // and the result answers it from inside a user turn, which is where this API keeps them
    let answer = &body["contents"][2];
    assert_eq!(answer["role"], "user");
    assert_eq!(answer["parts"][0]["functionResponse"]["name"], "weather");
    assert_eq!(answer["parts"][0]["functionResponse"]["id"], "call_1");
    assert_eq!(
        answer["parts"][0]["functionResponse"]["response"]["result"],
        "sunny"
    );
}

#[test]
fn a_conventional_turn_is_assembled_into_the_order_this_api_expects() {
    // a session that began against another provider, or a projector told not to send blocks
    let body = rendered(
        vec![
            ContextItem::user("go"),
            ContextItem::assistant("I will look", vec![ToolCall::new("c1", "read", json!({}))])
                .with_reasoning(Some(Content::text("a tool is needed"))),
            ContextItem::tool_result(ToolCallId::from("c1"), "read", "contents", false),
        ],
        false,
    );

    let parts = body["contents"][1]["parts"].as_array().expect("parts");
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0]["thought"], true);
    assert_eq!(parts[1]["text"], "I will look");
    assert_eq!(parts[2]["functionCall"]["name"], "read");
}

#[test]
fn several_results_are_parts_of_one_turn_rather_than_several_turns() {
    let body = rendered(
        vec![
            ContextItem::user("go"),
            ContextItem::assistant(
                Content::blocks([
                    Block::Call(ToolCall::new("c1", "read", json!({}))),
                    Block::Call(ToolCall::new("c2", "read", json!({}))),
                ]),
                Vec::new(),
            ),
            ContextItem::tool_result(ToolCallId::from("c1"), "read", "one", false),
            ContextItem::tool_result(ToolCallId::from("c2"), "read", "two", false),
        ],
        true,
    );

    // three turns, not four: this API alternates, and two answers to one turn are two parts
    let contents = body["contents"].as_array().expect("contents");
    assert_eq!(contents.len(), 3, "{contents:#?}");
    assert_eq!(contents[2]["role"], "user");
    assert_eq!(contents[2]["parts"].as_array().unwrap().len(), 2);
}

#[test]
fn a_json_result_is_handed_over_as_it_is() {
    let body = rendered(
        vec![
            ContextItem::user("go"),
            ContextItem::assistant(
                Content::blocks([Block::Call(ToolCall::new("c1", "read", json!({})))]),
                Vec::new(),
            ),
            ContextItem::tool_result(
                ToolCallId::from("c1"),
                "read",
                Content::json(json!({ "lines": 12 })),
                false,
            ),
        ],
        true,
    );

    let answered = &body["contents"][2]["parts"][0]["functionResponse"]["response"];
    assert_eq!(
        answered["lines"], 12,
        "not a string of its own serialization"
    );
}

#[test]
fn thinking_is_asked_for_and_can_be_turned_off() {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(Gemini::new("gemini-test", "http://127.0.0.1:1", "no key"));
    kernel.set_provider(provider.clone());
    kernel.push(ContextItem::user("go"));

    // a provider whose reason to exist is the order of a turn's thinking asks to be told some
    let body = provider.render(&kernel.preview_request().unwrap()).unwrap();
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["includeThoughts"],
        true
    );

    // and it is a default rather than a decision: what the user set is merged over it, and the
    // rest of `generationConfig` survives being set
    let mut params = nachalnik::Params::new();
    params.insert(
        "generationConfig".into(),
        json!({ "temperature": 0, "thinkingConfig": { "includeThoughts": false } }),
    );
    kernel.set_params(params);

    let body = provider.render(&kernel.preview_request().unwrap()).unwrap();
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["includeThoughts"],
        false
    );
    assert_eq!(body["generationConfig"]["temperature"], 0);
}

// ------------------------------------------------------------------------------ the round trip

#[tokio::test]
async fn what_came_off_the_wire_goes_back_on_to_it_unchanged() {
    // the assertion this whole provider exists for: this API answers `400 Function call is
    // missing a thought_signature` to a request that returns a turn altered, so what matters is
    // not that the parts are recorded but that they come back out identical
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(Gemini::new(
        "gemini-test",
        server(ORDERED).await,
        "no key needed",
    ));
    kernel.set_provider(provider.clone());
    kernel.set_projector(Arc::new(LinearProjector {
        send_blocks: true,
        ..Default::default()
    }));
    kernel.push(ContextItem::user("what is the weather?"));
    kernel.step().await.expect("the request is answered");

    // the results the turn is waiting for, so the projector keeps its calls
    for id in ["call_1", "call_2"] {
        kernel.push(ContextItem::tool_result(
            ToolCallId::from(id),
            "weather",
            "sunny",
            false,
        ));
    }

    let sent = provider
        .render(&kernel.preview_request().expect("a request"))
        .expect("rendered");
    let turn = &sent["contents"][1];
    let parts = turn["parts"].as_array().expect("parts");

    assert_eq!(turn["role"], "model");
    assert_eq!(parts.len(), 5);
    assert_eq!(parts[0]["thought"], true);
    assert_eq!(parts[0]["text"], "**Working it out**");
    assert_eq!(parts[1]["text"], "Checking Warsaw.");
    assert_eq!(parts[2]["functionCall"]["id"], "call_1");
    assert_eq!(
        parts[2]["thoughtSignature"], "SIG-CALL",
        "the signature that arrived on this call goes back on it"
    );
    assert_eq!(parts[3]["text"], "And now Krakow.");
    assert_eq!(parts[4]["functionCall"]["id"], "call_2");

    // and the turn really is in the context as an order, not just on the wire as one
    let recorded = kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
        .expect("recorded");
    assert_eq!(recorded.calls().count(), 2);
    assert_eq!(recorded.thinking().count(), 1);
}

#[tokio::test]
async fn a_part_this_provider_does_not_speak_is_dropped_rather_than_reattached() {
    // `inlineData` and its relatives have no `Content` variant to be recorded as. What must not
    // happen is the fallback that looks tidy: hanging it on the text before it, which would send
    // an image back out as a field of that sentence
    const WITH_AN_IMAGE: &str = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Here it is.\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"inlineData\":{\"mimeType\":",
        "\"image/png\",\"data\":\"AAAA\"}}]},\"finishReason\":\"STOP\"}]}\n\n",
    );

    let response = answered(WITH_AN_IMAGE).await;
    let blocks = response
        .content
        .as_ref()
        .and_then(Content::as_blocks)
        .unwrap();

    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].said().unwrap().content.to_text(), "Here it is.");
    assert!(
        blocks[0].extra().is_null(),
        "the image is not a signature: {:?}",
        blocks[0].extra()
    );
    // and it is not lost either - the provider's own account of the exchange keeps it
    let raw = response.raw.as_ref().expect("recorded");
    assert!(raw.to_string().contains("inlineData"));
}
