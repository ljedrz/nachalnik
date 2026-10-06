//! What each dialect asks the projector for, and whether the budget then matches the request.
//!
//! note: these two used to be settled in different places. The projector was chosen by the
//! client from whichever flag picked the dialect, the wire format was written in the provider,
//! and nothing connected them - so `send_reasoning` stayed on for a `to_wire` that has never sent
//! reasoning, and the budget charged for every turn of thinking the context was holding.
//! `Dialect::projection` is the connection, and these are the tests that the two agree.

#![cfg(any(feature = "anthropic", feature = "openai", feature = "gemini"))]

use std::sync::Arc;

#[cfg(any(feature = "openai", feature = "gemini"))]
use nachalnik::ContextKind;
#[cfg(any(feature = "anthropic", feature = "gemini"))]
use nachalnik::{Block, Part, ToolCall, ToolCallId};
use nachalnik::{Config, Content, ContextItem, Kernel, Provider};
use nachalnik_providers::Dialect;
#[cfg(any(feature = "anthropic", feature = "gemini"))]
use serde_json::json;

/// A session holding one assistant turn: a short answer and a long think, which is the usual
/// ratio for a reasoning model and the reason this matters at all.
#[cfg(any(feature = "openai", feature = "gemini"))]
fn thinking_out_loud() -> (Kernel, String) {
    let kernel = Kernel::new(Config::default());
    let thinking = "let me work through what the stack trace is saying. ".repeat(60);

    kernel.push(ContextItem::new(
        ContextKind::UserMessage,
        "user",
        "user",
        "why does it fail?",
    ));
    kernel.push(
        ContextItem::new(
            ContextKind::AssistantMessage {
                reasoning: None,
                tool_calls: Vec::new(),
            },
            "assistant",
            "assistant",
            "the parser is the problem",
        )
        .with_reasoning(Some(Content::text(thinking.as_str()))),
    );

    (kernel, thinking)
}

/// What the projector produced is what the budget is counted over, which is what makes it the
/// size of the *request* rather than the size of the context. So a projector handing over
/// something the wire format drops is not a wasted copy - it is a charge for bytes that never
/// leave the process, repeated for as long as the turn is in the context.
#[cfg(feature = "openai")]
#[test]
fn the_conventional_dialect_does_not_pay_for_thinking_it_cannot_send() {
    let (kernel, thinking) = thinking_out_loud();
    let provider =
        nachalnik_providers::OpenAiCompatible::new("m", "https://example.invalid/v1", "k");
    kernel.set_projector(Arc::new(provider.projection()));

    let request = kernel.preview_request().expect("a request");
    let payload = provider.render(&request).expect("it renders").to_string();
    assert!(
        !payload.contains(&thinking),
        "this dialect has never carried an assistant turn's reasoning"
    );

    // and the estimate agrees: the whole context now costs less than the thinking alone would,
    // which it cannot do while the thinking is in it
    let thinking_costs = kernel.counter().count(&Content::text(thinking.as_str()));
    assert!(thinking_costs > 500, "the fixture is too small to see it");
    assert!(
        kernel.budget().context_tokens < thinking_costs,
        "the estimate is carrying reasoning that the request will not"
    );
}

/// The other half, and the reason this is a property of the dialect rather than a setting: Gemini
/// takes a turn's thinking back as a part marked `thought`, and for a signed one it has to. Here
/// the reasoning really is in the request, so the budget is right to charge for it.
#[cfg(feature = "gemini")]
#[test]
fn the_gemini_dialect_pays_for_the_thinking_it_does_send() {
    let (kernel, thinking) = thinking_out_loud();
    let provider = nachalnik_providers::Gemini::new("m", "https://example.invalid", "k");
    kernel.set_projector(Arc::new(provider.projection()));

    let request = kernel.preview_request().expect("a request");
    let payload = provider.render(&request).expect("it renders").to_string();
    assert!(
        payload.contains(&thinking),
        "this dialect sends it, as a part marked `thought`"
    );

    let thinking_costs = kernel.counter().count(&Content::text(thinking.as_str()));
    assert!(
        kernel.budget().context_tokens > thinking_costs,
        "the request carries the thinking, so the estimate has to as well"
    );
}

/// Anthropic's dialect sends the thinking it signed, and the budget pays for it.
///
/// note: the thinking has to be this dialect's own - a block with a signature on it - because that
/// is the only kind this API takes back. The case the projector cannot see is the other kind, a
/// thinking turn another provider recorded, which is projected and then not sent; see
/// `Anthropic::projection`.
#[cfg(feature = "anthropic")]
#[test]
fn the_anthropic_dialect_pays_for_the_thinking_it_signed_and_sends() {
    let thinking = "let me work through what the stack trace is saying. ".repeat(60);
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("why does it fail?"));
    kernel.push(ContextItem::assistant(
        Content::blocks([
            Block::Reasoning(
                Part::new(thinking.as_str()).with_extra(json!({ "signature": "SIG" })),
            ),
            Block::Call(ToolCall::new("toolu_1", "read", json!({}))),
        ]),
        Vec::new(),
    ));
    kernel.push(ContextItem::tool_result(
        ToolCallId::from("toolu_1"),
        "read",
        "the trace",
        false,
    ));
    let provider = nachalnik_providers::Anthropic::new("m", "https://example.invalid/v1", "k");
    kernel.set_projector(Arc::new(provider.projection()));

    let request = kernel.preview_request().expect("a request");
    let payload = provider.render(&request).expect("it renders");
    assert_eq!(
        payload["messages"][1]["content"][0]["thinking"], thinking,
        "this dialect sends it, signed"
    );

    let thinking_costs = kernel.counter().count(&Content::text(thinking.as_str()));
    assert!(
        kernel.budget().context_tokens > thinking_costs,
        "the request carries the thinking, so the estimate has to as well"
    );
}

/// A turn recorded as an order goes back out as one, with what was attached to each of its parts.
///
/// note: the half of this dialect's projection the budget cannot see, since a turn flattened into
/// three slots costs the same. The request can: flattened, the signature on a text part has
/// nowhere to go, and this API answers the next request with `400 Function call is missing a
/// thought_signature`.
#[cfg(feature = "gemini")]
#[test]
fn the_gemini_dialect_is_handed_a_turn_in_the_order_it_was_recorded() {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("what is the weather?"));
    kernel.push(ContextItem::assistant(
        Content::blocks([
            Block::Text(
                Part::new("Checking.").with_extra(json!({ "thoughtSignature": "SIG-TEXT" })),
            ),
            Block::Call(ToolCall::new("call_1", "weather", json!({}))),
        ]),
        Vec::new(),
    ));
    kernel.push(ContextItem::tool_result(
        ToolCallId::from("call_1"),
        "weather",
        "sunny",
        false,
    ));
    let provider = nachalnik_providers::Gemini::new("m", "https://example.invalid", "k");
    kernel.set_projector(Arc::new(provider.projection()));

    let request = kernel.preview_request().expect("a request");
    let payload = provider.render(&request).expect("it renders");
    let parts = &payload["contents"][1]["parts"];
    assert_eq!(parts[0]["text"], "Checking.");
    assert_eq!(parts[0]["thoughtSignature"], "SIG-TEXT", "{parts:#?}");
}
