//! What gets sent: the request the kernel builds, and what a turn it gets back is recorded as.
//!
//! note: the invariant under most of these is that the previewed request *is* the request - there
//! is no step between `preview_request` and the wire where the kernel adds anything of its own.
//! A projector is free to make a request any shape it likes, and the last test here builds one in
//! which the whole context is a single user message, to make the point that it is one method.

use std::sync::Arc;

use nachalnik::{
    Config, ContextItem, ContextKind, ContextState, Kernel, Message, ModelResponse, Params,
    Projection, Projector, Role, Skipped, State, StopReason, test::ConstTool,
};
use serde_json::json;

use crate::common::permissive;

#[tokio::test]
async fn the_request_contains_exactly_what_the_user_put_in_it() {
    let (kernel, provider) = permissive([ModelResponse::text("hello")]);
    kernel.push(ContextItem::user("hi"));

    let State::Finished { item, stop } = kernel.turn().await.unwrap() else {
        panic!("no tools were involved")
    };
    assert_eq!(kernel.item(item).unwrap().source, "model");
    assert_eq!(
        stop,
        StopReason::EndTurn,
        "the model's own reason, carried by the state"
    );
    assert_eq!(
        kernel
            .last_response()
            .unwrap()
            .content
            .clone()
            .unwrap()
            .to_text(),
        "hello"
    );

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages.len(), 1, "no prompt was added");
    assert_eq!(requests[0].messages[0].role, Role::User);
    assert_eq!(
        requests[0].messages[0].content.as_ref().unwrap().to_text(),
        "hi"
    );
    assert!(requests[0].tools.is_empty());
    assert!(requests[0].params.is_empty(), "no knobs were invented");
}

#[tokio::test]
async fn params_are_carried_verbatim() {
    let (kernel, provider) = permissive([ModelResponse::text("ok")]);
    kernel.push(ContextItem::user("hi"));

    let mut params = Params::new();
    params.insert("temperature".into(), json!(0.0));
    params.insert("thinking".into(), json!({ "type": "enabled" }));
    kernel.set_params(params.clone());

    kernel.turn().await.unwrap();
    assert_eq!(provider.requests()[0].params, params);
    assert_eq!(kernel.params()["thinking"]["type"], "enabled");
}

#[tokio::test]
async fn the_budget_carries_the_providers_own_numbers_next_to_the_estimate() {
    let usage = nachalnik::Usage {
        input_tokens: Some(901),
        output_tokens: Some(8),
        reasoning_tokens: Some(139),
        cached_input_tokens: Some(16),
    };
    let (kernel, _) = permissive([ModelResponse {
        usage: Some(usage),
        ..ModelResponse::text("hi")
    }]);
    kernel.push(ContextItem::user("hi"));

    assert_eq!(kernel.budget().reported, None, "nothing has been sent yet");

    kernel.turn().await.unwrap();

    let budget = kernel.budget();
    assert_eq!(budget.reported, Some(usage));
    assert!(
        budget.used() < usage.input_tokens.unwrap() as usize,
        "the estimate is a floor, and a compactor can now see by how much"
    );
}

#[tokio::test]
async fn reasoning_is_recorded_on_its_turn_and_offered_back() {
    let (kernel, provider) = permissive([
        ModelResponse {
            content: Some("the answer is 4".into()),
            reasoning: Some("2 and 2 is 4".into()),
            tool_calls: Vec::new(),
            stop: StopReason::EndTurn,
            usage: None,
            raw: None,
        },
        ModelResponse::text("still 4"),
    ]);
    kernel.push(ContextItem::user("what is 2 + 2?"));
    kernel.turn().await.unwrap();

    let turn = kernel
        .items()
        .into_iter()
        .find(|item| item.source == "model")
        .unwrap();
    assert_eq!(
        turn.reasoning()
            .map(|r| r.to_text().into_owned())
            .as_deref(),
        Some("2 and 2 is 4")
    );

    // the next request carries it back, so a provider whose API verifies its own thinking can
    // hand it over verbatim
    kernel.push(ContextItem::user("are you sure?"));
    kernel.turn().await.unwrap();

    let second = &provider.requests()[1];
    let assistant = second
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert_eq!(
        assistant
            .reasoning
            .as_ref()
            .map(|r| r.to_text().into_owned())
            .as_deref(),
        Some("2 and 2 is 4")
    );

    // ... and a projector told not to send it does not, while the record keeps it
    kernel.set_projector(Arc::new(nachalnik::LinearProjector {
        send_reasoning: false,
        ..Default::default()
    }));
    let quiet = kernel.preview_request().unwrap();
    assert!(quiet.messages.iter().all(|m| m.reasoning.is_none()));
    assert!(kernel.item(turn.id).unwrap().reasoning().is_some());
}

#[tokio::test]
async fn what_a_provider_attaches_to_a_call_comes_back_attached_to_it() {
    // Google's `thought_signature` is the case that made this necessary: the API hands one back
    // per function call and rejects the *next* request if it does not come back with the call it
    // belongs to. The kernel has no idea what it is, which is exactly the point
    let signature = json!({ "google": { "thought_signature": "El4KXAERTTIP" } });
    let (kernel, provider) = permissive([
        ModelResponse::tool_calls(vec![
            nachalnik::ToolCall::new("c1", "peek", json!({})).with_extra(signature.clone()),
        ]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.unwrap();

    let second = &provider.requests()[1];
    let assistant = second
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert_eq!(
        *assistant.tool_calls[0].extra, signature,
        "the call went back out without its signature"
    );

    // it is part of the turn, so it survives a session round trip and it is counted
    let turn = kernel
        .items()
        .into_iter()
        .find(|item| item.source == "model")
        .unwrap();
    let ContextKind::AssistantMessage { tool_calls, .. } = &turn.kind else {
        unreachable!()
    };
    assert_eq!(*tool_calls[0].extra, signature);
    assert!(turn.tokens > 0);

    let json = serde_json::to_string(&*turn).unwrap();
    let restored: ContextItem = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, *turn);
}

#[tokio::test]
async fn a_tool_definition_is_not_copied_for_every_request() {
    let kernel = Kernel::new(Config::default());
    let schema = json!({ "type": "object", "properties": { "value": { "type": "string" } } });
    kernel.add_tool(Arc::new(
        ConstTool::new("peek", "ok").with_schema(schema.clone()),
    ));

    // `Tool::spec` runs afresh for every request; the schema it hands back is shared, not rebuilt
    let first = kernel.tool_specs();
    let second = kernel.tool_specs();
    assert!(Arc::ptr_eq(&first[0].schema, &second[0].schema));

    let request = {
        kernel.push(ContextItem::user("hi"));
        kernel.preview_request().unwrap()
    };
    assert!(Arc::ptr_eq(&request.tools[0].schema, &first[0].schema));
}

/// A projector for the dialect in which the whole context is one user message - as different a
/// shape as there is, and still one method.
struct OneMessage;

impl Projector for OneMessage {
    fn project(&self, items: &[Arc<ContextItem>]) -> Projection {
        let (mut text, mut included, mut skipped) = (String::new(), Vec::new(), Vec::new());

        for item in items {
            if !item.is_projected() {
                skipped.push(Skipped {
                    id: item.id,
                    reason: item.state.to_string(),
                });
                continue;
            }
            text.push_str(&format!("{}: {}\n", item.label, item.content.to_text()));
            included.push(item.id);
        }

        Projection {
            messages: vec![Message::new(Role::User, text)],
            included,
            skipped,
            repairs: Vec::new(),
        }
    }
}

#[tokio::test]
async fn the_shape_of_a_request_is_the_projectors_and_the_kernel_sends_what_it_says() {
    let (kernel, provider) = permissive([ModelResponse::text("noted")]);
    kernel.set_projector(Arc::new(OneMessage));

    kernel.push(ContextItem::system("be terse"));
    let dropped = kernel.push(ContextItem::file("src/lexer.rs", "fn lex() {}"));
    kernel.push(ContextItem::user("what is wrong?"));
    kernel.set_state(
        [dropped],
        ContextState::Excluded,
        Some("not this one".into()),
    );

    // the preview is the projector's work, not a description of it
    let preview = kernel.preview_request().unwrap();
    assert_eq!(preview.messages.len(), 1, "this dialect has one message");
    assert_eq!(preview.messages[0].role, Role::User);

    let projection = kernel.project();
    assert_eq!(projection.included.len(), 2);
    assert_eq!(projection.skipped.len(), 1);
    assert_eq!(projection.skipped[0].id, dropped);

    kernel.turn().await.unwrap();

    // and what the provider was handed is that, byte for byte
    let sent = &provider.requests()[0];
    assert_eq!(sent.messages, preview.messages);
    let text = sent.messages[0]
        .content
        .clone()
        .unwrap()
        .to_text()
        .into_owned();
    assert!(text.contains("system: be terse"));
    assert!(text.contains("user: what is wrong?"));
    assert!(
        !text.contains("fn lex"),
        "an excluded item is excluded whatever the shape of the request"
    );
}
