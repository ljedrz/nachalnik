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
//! NACHALNIK_ANTHROPIC_MODEL=anthropic/claude-haiku-5.5 \
//! NACHALNIK_ANTHROPIC_PARAMS='{"provider":{"order":["anthropic"],"allow_fallbacks":false}}' \
//!   cargo test -p nachalnik-providers --features anthropic --test anthropic_live -- --test-threads=1
//! ```
//!
//! note: the cheapest model by default, and every request but two is small: a run is eight
//! requests, and the two that check the cache carry a prompt long enough to be cached - about a
//! cent between them. What is checked is what the offline suites cannot: that the requests this
//! dialect builds are accepted - the tool results in a user turn, a signed thinking block sent
//! back the way it came - and that the cache asked for by default is really read.
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

/// A kernel talking to the configured endpoint, or `None` where no key was given, sending the
/// parameters `params` makes of the provider once it has asked the endpoint about the model.
async fn kernel(params: impl FnOnce(&Anthropic) -> Params) -> Option<Kernel> {
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
            true => "anthropic/claude-haiku-5.5",
            false => "claude-haiku-5-5",
        }
        .to_owned()
    });

    let provider = Arc::new(Anthropic::new(model, base, key));
    provider.probe().await;

    let mut sent: Params = env::var("NACHALNIK_ANTHROPIC_PARAMS")
        .ok()
        .and_then(|params| serde_json::from_str(&params).ok())
        .unwrap_or_default();
    sent.extend(params(&provider));

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
    let Some(kernel) = kernel(|_| Params::new()).await else {
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
    let Some(kernel) = kernel(|_| Params::new()).await else {
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
    let Some(kernel) = kernel(|provider| {
        let mut params = thinking(provider.capabilities());
        params.insert("max_tokens".to_owned(), json!(4096));
        params
    })
    .await
    else {
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

/// The second turn of a conversation reads its start from the cache, with nothing asked for.
///
/// note: the instructions are long enough to be cached at all - every model has a smallest prefix
/// it caches, and Haiku 4.5's is 4096 tokens - which makes this the dearest request here, at about
/// a cent.
#[tokio::test]
async fn the_second_turn_reads_the_first_from_the_cache() {
    let Some(kernel) = kernel(|_| Params::new()).await else {
        return;
    };
    let rules: String = (1..=600)
        .map(|n| format!("Rule {n}: the duty officer at post {n} answers in one word.\n"))
        .collect();
    kernel.push(ContextItem::system(rules));
    kernel.push(ContextItem::user("Reply with the word pong."));
    kernel.turn().await.expect("the first turn is answered");

    kernel.push(ContextItem::user("Reply with the word ping."));
    kernel.turn().await.expect("the second turn is answered");
    let usage = kernel
        .last_response()
        .and_then(|response| response.usage)
        .expect("the cost is reported");
    assert!(
        usage
            .cached_input_tokens
            .is_some_and(|cached| cached > 4096),
        "{usage:?}"
    );
}

/// Thinking turned on the way the model takes it, as the endpoint says it does.
///
/// note: models differ - Haiku 4.5 takes only `enabled` with a budget, Haiku 5.5 only `adaptive`,
/// and each refuses the other with a 400 - and Anthropic's `/models` says which, so nothing here
/// names a model. Adaptive thinking is the model's to skip on a question this easy, so it is asked
/// for at the most effort.
///
/// note: where the endpoint said nothing, it is the adaptive kind at the most effort. OpenRouter
/// lists `capabilities` as `null`, and takes both kinds for either model, turning each into what
/// the model takes - but a budget it turns into adaptive thinking at no effort in particular,
/// which Haiku 5.5 skips here, and no signature comes back to check.
fn thinking(capabilities: Option<Value>) -> Params {
    let supported = |path: &[&str]| {
        let Some(capabilities) = &capabilities else {
            return true;
        };
        path.iter()
            .fold(capabilities, |value, key| &value[key])
            .get("supported")
            == Some(&Value::Bool(true))
    };
    let mut params = Params::new();
    match supported(&["thinking", "types", "adaptive"]) {
        true => {
            params.insert("thinking".to_owned(), json!({ "type": "adaptive" }));
            if supported(&["effort"]) {
                params.insert("output_config".to_owned(), json!({ "effort": "max" }));
            }
        }
        false => {
            let budget = json!({ "type": "enabled", "budget_tokens": 1024 });
            params.insert("thinking".to_owned(), budget);
        }
    }
    params
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
