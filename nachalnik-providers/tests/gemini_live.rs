//! Google's own API against the real endpoint, over the real wire.
//!
//! Skipped unless a key is in the environment:
//!
//! ```text
//! GEMINI_API_KEY=... \
//!   cargo test -p nachalnik-providers --test gemini_live -- --test-threads=1
//! ```
//!
//! note: the cheapest model that takes tools, and every request is small. What is checked is what
//! the offline suites in `gemini.rs` and `blobs.rs` cannot: that the shapes they pin are ones the
//! API accepts, and that the model reads what they carry.
//!
//! note: `GEMINI_API_KEY` and nothing borrowed, so a run pointed at another provider's address
//! never sends that provider's key here.

#![cfg(feature = "gemini")]

use std::{env, sync::Arc};

use nachalnik::{
    Block, Config, Content, ContextItem, Kernel, State,
    test::{AllowAll, ConstTool},
};
use nachalnik_providers::{Gemini, gemini::DEFAULT_BASE_URL};
use serde_json::json;

/// A kernel talking to the configured endpoint, or `None` where no key was given.
fn kernel() -> Option<Kernel> {
    let base = env::var("NACHALNIK_GEMINI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into());
    let Ok(key) = env::var("GEMINI_API_KEY") else {
        eprintln!("skipped: no key for {base}");
        return None;
    };
    let model =
        env::var("NACHALNIK_GEMINI_MODEL").unwrap_or_else(|_| "gemini-3.5-flash-lite".to_owned());

    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(Gemini::new(model, base, key)));
    kernel.set_policy(Arc::new(AllowAll));
    Some(kernel)
}

/// What the model said last, without its thinking.
fn said(kernel: &Kernel) -> String {
    kernel
        .last_response()
        .and_then(|response| response.content.clone())
        .map(|content| match content.as_blocks() {
            Some(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    Block::Text(part) => Some(part.content.to_text().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            None => content.to_text().into_owned(),
        })
        .unwrap_or_default()
}

/// A picture a tool returned is seen: carried in the `functionResponse`'s own `parts`, as
/// `nachalnik-mcp` hands one over.
///
/// note: a 16-pixel square of one colour, so that the answer is one word and cannot be guessed
/// from the text beside it, which does not say. Sent as a line naming it instead, the model
/// answers with some other colour.
#[tokio::test]
async fn a_picture_a_tool_returned_is_seen() {
    const RED_SQUARE: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR42mO4ICBAEmIY1TCqYfhqAABYoPABnlzdOQ",
        "AAAABJRU5ErkJggg==",
    );
    let Some(kernel) = kernel() else {
        return;
    };
    kernel.add_tool(Arc::new(ConstTool::new(
        "look",
        Content::blocks([
            Block::text("the picture:"),
            Block::text(Content::blob("image/png", RED_SQUARE)),
        ]),
    )));
    kernel.push(ContextItem::user(
        "Call the look tool once, then tell me in one word which colour the picture it returned \
         is.",
    ));

    let state = kernel.turn().await.expect("the turn is answered");
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    assert!(
        said(&kernel).to_lowercase().contains("red"),
        "{}",
        said(&kernel)
    );
}

/// A schema using what Google's own `Schema` has no field for - `$ref` into `$defs`,
/// `additionalProperties`, `const`, a list of types - is accepted, and the call follows it.
///
/// note: all of it is ordinary in an MCP server's `inputSchema`, and sent as `parameters` each
/// one is a 400 on every request while the tool is offered.
#[tokio::test]
async fn a_schema_in_json_schema_is_accepted_and_followed() {
    let Some(kernel) = kernel() else {
        return;
    };
    kernel.add_tool(Arc::new(ConstTool::new("locate", "noted").with_schema(
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "$defs": {
                "point": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"],
                    "additionalProperties": false,
                },
            },
            "properties": {
                "where": { "$ref": "#/$defs/point" },
                "units": { "const": "metric" },
                "zoom": { "type": ["integer", "null"] },
            },
            "required": ["where"],
            "additionalProperties": false,
        }),
    )));
    kernel.push(ContextItem::user(
        "Call the locate tool once for Warsaw, then say done.",
    ));

    let state = kernel.turn().await.expect("the turn is answered");
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    let call = kernel
        .items()
        .into_iter()
        .find_map(|item| {
            item.content
                .as_blocks()
                .and_then(|blocks| blocks.iter().find_map(|block| block.call().cloned()))
        })
        .expect("the tool was called");
    assert_eq!(call.tool, "locate");
    assert!(
        call.args["where"]["city"]
            .as_str()
            .is_some_and(|city| city.contains("Warsaw")),
        "{}",
        call.args
    );
}
