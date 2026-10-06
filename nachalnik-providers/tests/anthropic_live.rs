//! The Anthropic dialect against a real endpoint, over the real wire.
//!
//! Skipped unless a key is in the environment. Anthropic's own API is the default:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-ant-... \
//!   cargo test -p nachalnik-providers --features anthropic --test anthropic_live -- --test-threads=1
//! ```
//!
//! OpenRouter answers the same dialect at `/api/v1/messages`. Pinned to Anthropic, with no
//! fallback to another provider, it is the same API with OpenRouter's fields added:
//!
//! ```text
//! OPENROUTER_API_KEY=sk-or-... \
//! NACHALNIK_ANTHROPIC_BASE_URL=https://openrouter.ai/api/v1 \
//! NACHALNIK_ANTHROPIC_MODEL=anthropic/claude-haiku-4.5 \
//! NACHALNIK_ANTHROPIC_PARAMS='{"provider":{"order":["anthropic"],"allow_fallbacks":false}}' \
//!   cargo test -p nachalnik-providers --features anthropic --test anthropic_live -- --test-threads=1
//! ```
//!
//! note: the cheapest model by default, and every request is small: a run is six requests and a
//! few thousand tokens. What is checked is what the offline suites cannot: that the requests this
//! dialect builds are accepted - the tool results in a user turn, and above all a signed thinking
//! block sent back the way it came.
//!
//! note: `OPENROUTER_API_KEY` only for OpenRouter and `ANTHROPIC_API_KEY` only for anything else,
//! so neither key is ever sent to the other's address.

#![cfg(feature = "anthropic")]

use std::{env, sync::Arc};

use nachalnik::{
    Block, Config, Content, ContextItem, ContextKind, Kernel, Params, State, StopReason,
    test::{AllowAll, EchoTool},
};
use nachalnik_providers::{Anthropic, anthropic::DEFAULT_BASE_URL, is_openrouter};
use serde_json::{Value, json};

/// A kernel talking to the configured endpoint, or `None` where no key was given.
async fn kernel(params: Params) -> Option<Kernel> {
    let base = env::var("NACHALNIK_ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into());
    let key = match is_openrouter(&base) {
        true => env::var("OPENROUTER_API_KEY"),
        false => env::var("ANTHROPIC_API_KEY"),
    };
    let Ok(key) = key else {
        eprintln!("skipped: no key for {base}");
        return None;
    };
    let model = env::var("NACHALNIK_ANTHROPIC_MODEL").unwrap_or_else(|_| {
        match is_openrouter(&base) {
            true => "anthropic/claude-haiku-4.5",
            false => "claude-haiku-4-5",
        }
        .to_owned()
    });

    let provider = Arc::new(Anthropic::new(model, base, key));
    provider.probe().await;

    let mut sent: Params = env::var("NACHALNIK_ANTHROPIC_PARAMS")
        .ok()
        .and_then(|params| serde_json::from_str(&params).ok())
        .unwrap_or_default();
    sent.extend(params);

    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider);
    kernel.set_params(sent);
    kernel.set_policy(Arc::new(AllowAll));
    Some(kernel)
}

/// What the model said last, as plain text.
fn said(kernel: &Kernel) -> String {
    kernel
        .last_response()
        .and_then(|response| {
            response
                .content
                .as_ref()
                .map(|said| said.to_text().into_owned())
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn an_answer_comes_back_with_what_it_cost() {
    let Some(kernel) = kernel(Params::new()).await else {
        return;
    };
    kernel.push(ContextItem::system("You answer in one word."));
    kernel.push(ContextItem::user("Reply with the word pong."));

    let state = kernel.turn().await.expect("the request is answered");
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    assert!(
        said(&kernel).to_lowercase().contains("pong"),
        "{}",
        said(&kernel)
    );

    let response = kernel.last_response().expect("answered");
    assert_eq!(response.stop, StopReason::EndTurn);
    let usage = response.usage.expect("the cost is reported");
    assert!(
        usage.input_tokens.is_some_and(|input| input > 0),
        "{usage:?}"
    );
    assert!(
        usage.output_tokens.is_some_and(|output| output > 0),
        "{usage:?}"
    );
}

/// A call and its result, carried there and back: the `tool_result` the second request carries is
/// what this API is strict about.
#[tokio::test]
async fn a_tool_is_called_and_its_answer_read() {
    let Some(kernel) = kernel(Params::new()).await else {
        return;
    };
    round_trip(&kernel).await;
}

/// Thinking, signed, sent back with the call it came before.
///
/// note: the request this API refuses when a dialect gets it wrong. A turn that thought and then
/// asked for a tool has to send that thinking back - signature and all - with the tool's result,
/// so the second request of this turn is the check: it is answered only if the block went back
/// as it came.
#[tokio::test]
async fn signed_thinking_goes_back_with_the_call_it_came_before() {
    let thinking = json!({ "thinking": { "type": "enabled", "budget_tokens": 1024 } });
    let mut params = Params::new();
    params.insert("max_tokens".to_owned(), json!(4096));
    params.extend(thinking.as_object().cloned().unwrap_or_default());
    let Some(kernel) = kernel(params).await else {
        return;
    };
    round_trip(&kernel).await;

    let signed = kernel
        .items()
        .into_iter()
        .filter(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
        .filter_map(|item| match &item.content {
            Content::Blocks(blocks) => Some(blocks.clone()),
            _ => None,
        })
        .flat_map(|blocks| blocks.to_vec())
        .filter(|block| matches!(block, Block::Reasoning(_)))
        .any(|block| {
            block.extra()["signature"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        });
    assert!(signed, "a turn that thought kept its signature");
}

/// Asks for one call to `echo`, and checks the answer used what it returned.
async fn round_trip(kernel: &Kernel) {
    kernel.add_tool(Arc::new(EchoTool::new("echo", [])));
    kernel.push(ContextItem::user(
        "Call the echo tool once with the value \"kotelnaya\", then tell me the value it \
         returned.",
    ));

    let state = kernel.turn().await.expect("the turn is answered");
    assert!(matches!(state, State::Finished { .. }), "{state:?}");

    let asked: Vec<Value> = kernel
        .items()
        .into_iter()
        .flat_map(|item| {
            item.calls()
                .map(|call| (*call.args).clone())
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        asked.iter().any(|args| args["value"] == "kotelnaya"),
        "the call carried its argument: {asked:?}"
    );
    assert!(said(kernel).contains("kotelnaya"), "{}", said(kernel));
}
