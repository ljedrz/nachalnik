//! What each dialect asks the projector for, and whether the budget then matches the request.
//!
//! note: these two were settled in different places. The projector was chosen in `main.rs` from
//! the `--gemini` flag, the wire format was written in the provider, and nothing connected them -
//! so `send_reasoning` stayed on for a `to_wire` that has never sent reasoning, and the budget
//! charged for every turn of thinking the context was holding. `Endpoint::projection` is the
//! connection, and these are the tests that the two agree.

use std::sync::Arc;

use kamchatka::{
    gemini::Gemini,
    provider::{Endpoint, OpenAiCompatible},
};
use nachalnik::{Config, Content, ContextItem, ContextKind, Kernel, Provider};

/// A session holding one assistant turn: a short answer and a long think, which is the usual
/// ratio for a reasoning model and the reason this matters at all.
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
#[test]
fn the_conventional_dialect_does_not_pay_for_thinking_it_cannot_send() {
    let (kernel, thinking) = thinking_out_loud();
    let provider = OpenAiCompatible::new("m", "https://example.invalid/v1", "k");
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
#[test]
fn the_gemini_dialect_pays_for_the_thinking_it_does_send() {
    let (kernel, thinking) = thinking_out_loud();
    let provider = Gemini::new("m", "https://example.invalid", "k");
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
