//! The Responses API against a real endpoint, over the real wire.
//!
//! Skipped unless a key is in the environment. OpenAI's own API is the default:
//!
//! ```text
//! OPENAI_API_KEY=sk-... \
//!   cargo test -p nachalnik-providers --test responses_live -- --test-threads=1
//! ```
//!
//! OpenRouter answers the same API at `/api/v1/responses`. Pinned to OpenAI, with no fallback to
//! another provider, it is the same API with OpenRouter's fields added:
//!
//! ```text
//! OPENROUTER_API_KEY=sk-or-... \
//! NACHALNIK_RESPONSES_BASE_URL=https://openrouter.ai/api/v1 \
//! NACHALNIK_RESPONSES_PARAMS='{"provider":{"order":["openai"],"allow_fallbacks":false}}' \
//!   cargo test -p nachalnik-providers --test responses_live -- --test-threads=1
//! ```
//!
//! note: the cheapest model that reasons by default, and every request is small: a run is eight
//! requests and well under a cent. What is checked is what the offline suites cannot: that the
//! requests this mode builds are accepted - a call's result as its own item, and above all the
//! sealed reasoning sent back the way it came - and that a conversation's start is read from the
//! cache.
//!
//! note: `OPENROUTER_API_KEY` only for OpenRouter and `OPENAI_API_KEY` only for anything else, so
//! neither key is ever sent to the other's address.

#![cfg(feature = "openai")]

use std::{env, sync::Arc};

use nachalnik::{
    Block, Config, Content, ContextItem, ContextKind, Kernel, Params, State, StopReason,
    test::{AllowAll, EchoTool},
};
use nachalnik_providers::{OpenAiCompatible, is_openrouter};
use serde_json::{Value, json};

/// A kernel talking to the configured endpoint, or `None` where no key was given.
async fn kernel(params: Params) -> Option<Kernel> {
    let base = env::var("NACHALNIK_RESPONSES_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let key = match is_openrouter(&base) {
        true => env::var("OPENROUTER_API_KEY"),
        false => env::var("OPENAI_API_KEY"),
    };
    let Ok(key) = key else {
        eprintln!("skipped: no key for {base}");
        return None;
    };
    let model = env::var("NACHALNIK_RESPONSES_MODEL").unwrap_or_else(|_| {
        match is_openrouter(&base) {
            true => "openai/gpt-5-nano",
            false => "gpt-5-nano",
        }
        .to_owned()
    });

    let provider = Arc::new(OpenAiCompatible::new(model, base, key).responses(true));
    provider.probe().await;

    let mut sent: Params = env::var("NACHALNIK_RESPONSES_PARAMS")
        .ok()
        .and_then(|params| serde_json::from_str(&params).ok())
        .unwrap_or_default();
    sent.insert(
        "reasoning".to_owned(),
        json!({ "effort": "low", "summary": "auto" }),
    );
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

/// A call and its result, carried there and back, with the turn's sealed reasoning in front of
/// the call - which is the request this mode exists to get right.
#[tokio::test]
async fn a_tool_is_called_and_its_answer_read_with_the_reasoning_carried() {
    let Some(kernel) = kernel(Params::new()).await else {
        return;
    };
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
    assert!(said(&kernel).contains("kotelnaya"), "{}", said(&kernel));

    // the turn that asked for the call thought first, and kept what it thought in the form this
    // API takes back: the second request of the turn carried it, or was refused
    let sealed = kernel
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
            block.extra()["encrypted_content"]
                .as_str()
                .is_some_and(|sealed| !sealed.is_empty())
        });
    assert!(sealed, "a turn that thought kept its reasoning sealed");
}

/// The same answer, asked for whole rather than streamed.
#[tokio::test]
async fn a_whole_answer_is_read_as_a_stream_would_have_been() {
    let mut params = Params::new();
    params.insert("stream".to_owned(), json!(false));
    let Some(kernel) = kernel(params).await else {
        return;
    };
    kernel.push(ContextItem::user("Reply with the word pong."));

    kernel.turn().await.expect("the request is answered");
    assert!(
        said(&kernel).to_lowercase().contains("pong"),
        "{}",
        said(&kernel)
    );
    let response = kernel.last_response().expect("answered");
    assert_eq!(response.stop, StopReason::EndTurn);
    assert!(response.usage.is_some(), "{response:?}");
}

/// The second turn of a conversation reads its start from the cache, with nothing asked for.
///
/// note: OpenAI caches a prompt by its prefix, from 1024 tokens up, on its own. What this checks is
/// that nothing here breaks the prefix between two turns - and that what is cached is reported.
#[tokio::test]
async fn the_second_turn_reads_the_first_from_the_cache() {
    let Some(kernel) = kernel(Params::new()).await else {
        return;
    };
    let rules: String = (1..=300)
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
            .is_some_and(|cached| cached >= 1024),
        "{usage:?}"
    );
}
