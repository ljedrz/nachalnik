//! Where each dialect puts bytes that are not text.
//!
//! note: `Content::Blob` holds its payload already base64, which is the form both of these APIs
//! take it in, so what is under test here is placement rather than encoding. The two disagree
//! about everything else: this dialect wants a `data:` URI inside a list of typed content parts,
//! that one wants an `inline_data` part beside the text ones.
//!
//! note: and they agree about one thing, which is the case worth pinning. Neither accepts a
//! picture in a *tool result* - `tool` content is a string in one and a `functionResponse` object
//! in the other - so a tool that returned one sends the sentence naming it instead. That is the
//! answer `nachalnik-mcp` has always given, and it beats a 400 by enough to be worth being
//! deliberate about.

#![cfg(any(feature = "openai", feature = "gemini"))]

use std::sync::Arc;

use nachalnik::{Block, Config, Content, ContextItem, Kernel, Provider, ToolCall};
use serde_json::{Value, json};

/// A one-pixel PNG, base64, which is what a caller would have handed over.
const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUg==";

/// What `provider` would send for a context holding `items`.
fn rendered(provider: Arc<dyn Provider>, items: Vec<ContextItem>) -> Value {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider.clone());
    kernel.push_all(items);

    provider
        .render(&kernel.preview_request().expect("a request"))
        .expect("this provider always renders")
}

/// A user turn carrying a picture, and a tool result carrying one.
fn looking_at_a_picture() -> Vec<ContextItem> {
    let call = ToolCall::new("c1", "screenshot", json!({}));

    vec![
        ContextItem::user(Content::blob("image/png", PIXEL)),
        ContextItem::assistant(Content::text("let me look"), vec![call.clone()]),
        ContextItem::tool_result(
            call.id.clone(),
            "screenshot",
            Content::blob("image/png", PIXEL),
            false,
        ),
    ]
}

/// The conventional dialect carries it as a `data:` URI in a content part.
///
/// note: a *list* of parts only where there is one to carry. A plain string is what every endpoint
/// speaking this dialect accepts and some of the smaller ones accept nothing else, so a turn with
/// no picture in it must go out looking exactly as it did before this variant existed.
#[cfg(feature = "openai")]
#[test]
fn the_conventional_dialect_sends_a_data_uri_in_a_content_part() {
    use nachalnik_providers::OpenAiCompatible;

    let provider = Arc::new(OpenAiCompatible::new(
        "m",
        "https://example.invalid/v1",
        "k",
    ));
    let body = rendered(provider, looking_at_a_picture());
    let messages = body["messages"].as_array().expect("messages");

    assert_eq!(
        messages[0]["content"][0]["image_url"]["url"],
        json!(format!("data:image/png;base64,{PIXEL}")),
        "the user turn: {}",
        messages[0]["content"]
    );
    assert_eq!(messages[0]["content"][0]["type"], "image_url");

    // the assistant turn has no picture in it and is a plain string, as it always was
    assert_eq!(messages[1]["content"], json!("let me look"));

    // and the tool result names what it was rather than being refused for its shape
    let result = messages.last().expect("the result");
    assert_eq!(result["role"], "tool");
    assert_eq!(
        result["content"],
        json!(format!("[image/png, {} bytes]", PIXEL.len())),
        "a picture cannot be a `tool` message's content in this dialect"
    );
}

/// Google's dialect carries it as an `inline_data` part.
#[cfg(feature = "gemini")]
#[test]
fn googles_dialect_sends_inline_data_beside_the_text_parts() {
    use nachalnik_providers::Gemini;

    let provider = Arc::new(Gemini::new("m", "https://example.invalid", "k"));
    let body = rendered(provider, looking_at_a_picture());
    let contents = body["contents"].as_array().expect("contents");

    assert_eq!(contents[0]["role"], "user");
    assert_eq!(
        contents[0]["parts"][0]["inline_data"],
        json!({ "mime_type": "image/png", "data": PIXEL }),
        "the user turn: {}",
        contents[0]["parts"]
    );

    // the model's own turn is text and a call, and goes out unchanged
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[1]["parts"][0]["text"], "let me look");

    // and the result is a `functionResponse`, which has nowhere to put a picture
    let answer = contents.last().expect("the result");
    assert_eq!(answer["role"], "user");
    assert_eq!(
        answer["parts"][0]["functionResponse"]["response"]["result"],
        json!(format!("[image/png, {} bytes]", PIXEL.len()))
    );
}

/// A turn that is a sentence *and* a picture goes out as both.
///
/// note: the shape a caller building a multimodal client reaches for, and the one a byte-for-byte
/// reading of "content is one thing" would lose: `to_text` on a block sequence names the picture
/// rather than carrying it, which is right for a transcript and wrong for a request. Both dialects
/// read the blocks instead, so the model gets the words and the image in the order they were put
/// in.
#[test]
fn a_turn_that_is_a_sentence_and_a_picture_carries_both() {
    let asked = Content::blocks([
        Block::text(Content::text("what is wrong with this?")),
        Block::text(Content::blob("image/png", PIXEL)),
    ]);

    #[cfg(feature = "openai")]
    {
        let provider = Arc::new(nachalnik_providers::OpenAiCompatible::new(
            "m",
            "https://example.invalid/v1",
            "k",
        ));
        let body = rendered(provider, vec![ContextItem::user(asked.clone())]);
        let parts = &body["messages"][0]["content"];

        assert_eq!(
            parts[0],
            json!({ "type": "text", "text": "what is wrong with this?" })
        );
        assert_eq!(
            parts[1]["image_url"]["url"],
            json!(format!("data:image/png;base64,{PIXEL}")),
            "{parts}"
        );
    }

    #[cfg(feature = "gemini")]
    {
        let provider = Arc::new(nachalnik_providers::Gemini::new(
            "m",
            "https://example.invalid",
            "k",
        ));
        let body = rendered(provider, vec![ContextItem::user(asked)]);
        let parts = &body["contents"][0]["parts"];

        assert_eq!(parts[0]["text"], "what is wrong with this?");
        assert_eq!(
            parts[1]["inline_data"],
            json!({ "mime_type": "image/png", "data": PIXEL }),
            "{parts}"
        );
    }
}
