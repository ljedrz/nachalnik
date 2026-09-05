//! Tests for counting: what a piece of content costs, and what happens when the provider says
//! otherwise.

use std::sync::Arc;

use nachalnik::{
    BytesPerToken, Calibrating, Config, Content, ContextItem, ContextKind, Kernel, ModelResponse,
    State, TokenCounter, Usage,
    test::{AllowAll, ConstTool, ScriptedProvider, call},
};
use serde_json::json;

fn kernel() -> Kernel {
    Kernel::new(Config::default())
}

fn blob() -> serde_json::Value {
    json!({
        "findings": [
            { "file": "src/parser.rs", "line": 13, "message": "unreachable code" },
            { "file": "src/lexer.rs", "line": 41, "message": "unused variable `x`" },
        ],
        "ok": false,
    })
}

// ------------------------------------------------------------------------------ structured content

#[test]
fn structured_content_is_measured_as_it_would_be_sent() {
    let content = Content::json(blob());

    // the measurement is of the bytes that would go on the wire, and it is made without building
    // them: whatever the counting sink does, it has to agree with rendering the thing
    assert_eq!(content.byte_len(), blob().to_string().len());
    assert_eq!(content.to_text(), blob().to_string());
    assert_eq!(
        content.as_text(),
        None,
        "it is not text, and does not claim to be"
    );
}

#[test]
fn structured_content_is_counted_like_everything_else() {
    let counter = BytesPerToken::default();
    let content = Content::json(blob());

    assert_eq!(
        counter.count(&content),
        content.byte_len().div_ceil(4),
        "the same rule as text, over the bytes it would occupy"
    );

    let item = ContextItem::new(ContextKind::Reference, "tool", "findings", content);
    assert_eq!(counter.count_item(&item), counter.count(&item.content));
}

#[test]
fn truncating_structured_content_turns_it_into_text_rather_than_broken_json() {
    let mut content = Content::json(blob());
    let whole = content.byte_len();

    let dropped = content.truncate_to(60).expect("it is longer than that");

    assert!(
        content.as_text().is_some(),
        "a cut JSON document is not JSON"
    );
    assert!(
        content.byte_len() <= 60,
        "a limit that is not a limit is no use in a budget"
    );
    assert!(
        content.to_text().contains("truncated by an output limit"),
        "and it says so, in the content itself"
    );

    // what was kept plus what was dropped is what there was: the note is the only thing added
    let text = content.to_text();
    let kept = text.split("\n[... ").next().unwrap().len();
    assert_eq!(kept + dropped, whole);
}

#[tokio::test]
async fn a_structured_tool_result_reaches_the_request_intact() {
    let kernel = kernel();
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("call_01", "scan", json!({}))]),
        ModelResponse::text("two findings"),
    ])));
    kernel.add_tool(Arc::new(ConstTool::new("scan", Content::json(blob()))));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.push(ContextItem::user("scan the crate"));

    let State::Finished { .. } = kernel.turn().await.unwrap() else {
        panic!("nothing needed deciding")
    };

    let result = kernel
        .items()
        .into_iter()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .expect("the tool ran");
    assert!(
        matches!(result.content, Content::Json(_)),
        "the kernel does not stringify what a tool handed it"
    );
    assert_eq!(result.tokens, blob().to_string().len().div_ceil(4));

    // and it is still structured when it reaches the message a provider would render
    let projected = kernel.project();
    let message = projected
        .messages
        .iter()
        .find(|message| message.tool_call_id.is_some())
        .expect("the result is in the request");
    assert_eq!(
        message.content.as_ref().map(|c| c.to_text()),
        Some(blob().to_string().into())
    );
}

// ---------------------------------------------------------------------------------- calibration

/// A counter wrapped so that its corrections can be inspected.
fn calibrating() -> Arc<Calibrating<BytesPerToken>> {
    Arc::new(Calibrating::new(BytesPerToken::default()))
}

#[test]
fn a_counter_that_has_been_told_nothing_corrects_nothing() {
    let counter = calibrating();
    let content = Content::text("a".repeat(400));

    assert_eq!(counter.calibration().scale, 1.0);
    assert_eq!(counter.calibration().observations, 0);
    assert_eq!(counter.count(&content), 100);
}

#[test]
fn one_response_is_enough_to_correct_the_estimate() {
    let counter = calibrating();
    let content = Content::text("a".repeat(400));
    assert_eq!(counter.count(&content), 100);

    // the provider charged a third more than was estimated for the request as a whole
    counter.observe(3_000, 4_000);

    let learned = counter.calibration();
    assert_eq!(learned.observations, 1);
    assert!((learned.scale - 4.0 / 3.0).abs() < 1e-9);
    assert_eq!(counter.count(&content), 133);
}

#[test]
fn the_correction_does_not_compound_on_itself() {
    let counter = calibrating();
    counter.observe(3_000, 4_000);
    let after_one = counter.calibration().scale;

    // the next request is estimated with the correction already applied; observing it again must
    // leave the ratio where it was rather than squaring it
    counter.observe(4_000, 4_000);

    let learned = counter.calibration();
    assert_eq!(learned.observations, 2);
    assert!(
        (learned.scale - after_one).abs() < 1e-9,
        "a stable provider should produce a stable correction, got {} then {}",
        after_one,
        learned.scale
    );
}

#[test]
fn the_correction_settles_rather_than_chasing_the_last_request() {
    let counter = calibrating();
    for _ in 0..8 {
        counter.observe(1_000, 1_300);
    }
    let settled = counter.calibration().scale;

    // one odd request out of nine barely moves a ratio drawn from all of them
    counter.observe(1_300, 1_300);

    assert!(
        (counter.calibration().scale - settled).abs() < 0.05,
        "one outlier moved the correction from {settled} to {}",
        counter.calibration().scale
    );
}

#[test]
fn a_nonsensical_report_cannot_turn_a_budget_into_a_fiction() {
    let counter = calibrating();

    counter.observe(1, 100_000_000);
    assert_eq!(counter.calibration().scale, 10.0, "clamped, not believed");

    counter.reset();
    counter.observe(100_000_000, 1_000);
    assert_eq!(counter.calibration().scale, 0.1);

    // and a report with nothing in it says nothing about the ratio
    counter.reset();
    counter.observe(0, 5_000);
    counter.observe(5_000, 0);
    assert_eq!(counter.calibration().observations, 0);
    assert_eq!(counter.calibration().scale, 1.0);
}

#[test]
fn a_request_too_small_to_have_a_bias_in_it_teaches_nothing() {
    let counter = calibrating();

    // four tokens estimated against five charged is a 20% error made of one token; a counter
    // that learned from it would arrive at a scale that is wrong for every request that matters
    for _ in 0..20 {
        counter.observe(4, 5);
    }

    assert_eq!(counter.calibration().observations, 0);
    assert_eq!(counter.calibration().scale, 1.0);

    // the same ratio, on a request big enough to mean it, is learned from
    counter.observe(4_000, 5_000);
    assert_eq!(counter.calibration().observations, 1);
    assert_eq!(counter.calibration().scale, 1.25);
}

#[test]
fn forgetting_is_one_call_because_another_model_tokenizes_differently() {
    let counter = calibrating();
    counter.observe(3_000, 4_000);
    assert_ne!(counter.calibration().scale, 1.0);

    counter.reset();

    assert_eq!(
        counter.calibration(),
        Calibrating::new(BytesPerToken::default()).calibration()
    );
}

#[tokio::test]
async fn the_kernel_tells_the_counter_what_the_request_actually_cost() {
    let kernel = kernel();
    let counter = calibrating();
    kernel.set_counter(counter.clone());
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(900),
            output_tokens: Some(12),
            ..Usage::default()
        }),
        ..ModelResponse::text("hello")
    }])));
    kernel.push(ContextItem::user("a".repeat(400)));

    let estimated = kernel.budget().used();
    kernel.turn().await.unwrap();

    let learned = counter.calibration();
    assert_eq!(learned.observations, 1, "one request, one observation");
    assert_eq!(
        learned.estimated, estimated as u64,
        "the estimate it is told about is the one it made"
    );
    assert_eq!(learned.reported, 900);
    assert!(
        learned.scale > 1.0,
        "this counter was low, as it usually is"
    );
}

#[tokio::test]
async fn a_counter_that_ignores_the_feedback_is_left_alone() {
    let kernel = kernel();
    // the default counter learns; this is the one that has been asked not to
    kernel.set_counter(Arc::new(BytesPerToken::default()));
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(900),
            ..Usage::default()
        }),
        ..ModelResponse::text("hello")
    }])));
    let item = kernel.push(ContextItem::user("a".repeat(400)));
    let before = kernel.item(item).unwrap().tokens;

    kernel.turn().await.unwrap();

    assert_eq!(
        kernel.item(item).unwrap().tokens,
        before,
        "a counter with no `observe` does nothing with it, and nothing is rewritten anyway"
    );
    let after = kernel.push(ContextItem::user("a".repeat(400)));
    assert_eq!(
        kernel.item(after).unwrap().tokens,
        before,
        "and it goes on estimating the same bytes at exactly what it did before"
    );
}

#[tokio::test]
async fn the_counter_a_kernel_starts_with_corrects_itself() {
    // nothing is set here: this is what somebody who never read the documentation gets
    let kernel = kernel();
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse {
            usage: Some(Usage {
                input_tokens: Some(900),
                ..Usage::default()
            }),
            ..ModelResponse::text("hello")
        },
        ModelResponse::text("again"),
    ])));
    let item = kernel.push(ContextItem::user("a".repeat(400)));
    let estimated = kernel.budget().used();

    kernel.turn().await.unwrap();

    assert_eq!(
        kernel.item(item).unwrap().tokens,
        estimated,
        "the figure already recorded is still the one that was recorded"
    );

    // ... but what is counted from now on carries the correction the provider's own number implies
    let after = kernel.push(ContextItem::user("a".repeat(400)));
    assert!(
        kernel.item(after).unwrap().tokens > kernel.item(item).unwrap().tokens,
        "the same bytes, counted after the lesson: {} vs {}",
        kernel.item(after).unwrap().tokens,
        kernel.item(item).unwrap().tokens
    );
}

#[tokio::test]
async fn corrected_figures_reach_the_context_only_when_asked_for() {
    let kernel = kernel();
    let counter = calibrating();
    kernel.set_counter(counter.clone());
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse {
        usage: Some(Usage {
            input_tokens: Some(900),
            ..Usage::default()
        }),
        ..ModelResponse::text("hello")
    }])));

    let item = kernel.push(ContextItem::user("a".repeat(400)));
    let before = kernel.item(item).unwrap().tokens;
    kernel.turn().await.unwrap();

    assert_eq!(
        kernel.item(item).unwrap().tokens,
        before,
        "a recorded number does not change by itself"
    );

    kernel.recount();

    assert!(
        kernel.item(item).unwrap().tokens > before,
        "and it does when the user says so"
    );
}

/// A counter that charges for a message's framing keeps that opinion through `Calibrating`.
///
/// note: `count_message` is the one the budget is counted over, and the one a real tokenizer
/// overrides precisely to add the per-message overhead an estimate cannot see. It used to be the
/// one method `Calibrating` did not delegate, so wrapping such a counter silently threw the
/// override away and counted the parts instead.
#[test]
fn calibrating_keeps_a_wrapped_counter_s_opinion_about_messages() {
    /// Ten tokens of framing on every message, on top of what the content costs.
    struct Framed;

    impl TokenCounter for Framed {
        fn count(&self, content: &Content) -> usize {
            content.byte_len()
        }

        fn count_message(&self, message: &nachalnik::Message) -> usize {
            10 + message.content.as_ref().map_or(0, |c| self.count(c))
        }
    }

    let message = nachalnik::Message::user("abcd");
    assert_eq!(Framed.count_message(&message), 14);

    let wrapped = Calibrating::new(Framed);
    assert_eq!(
        wrapped.count_message(&message),
        14,
        "with nothing learned, the wrapper is the counter it wraps"
    );

    // and once it has learned something, the opinion is scaled rather than discarded
    wrapped.observe(1_000, 2_000);
    assert_eq!(wrapped.calibration().scale, 2.0);
    assert_eq!(
        wrapped.count_message(&message),
        28,
        "the framing is doubled with everything else, not counted away"
    );
}
