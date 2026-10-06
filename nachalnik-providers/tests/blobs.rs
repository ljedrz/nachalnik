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

#![cfg(any(feature = "anthropic", feature = "openai", feature = "gemini"))]

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
        json!(format!("[image/png, {}B]", PIXEL.len())),
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
        json!(format!("[image/png, {}B]", PIXEL.len()))
    );
}

/// Anthropic's carries it as an `image` block, in the user turn and inside the tool's result alike.
///
/// note: the one dialect here whose tool result can hold a picture: a `tool_result` takes the same
/// blocks a user turn does, so a screenshot a tool took reaches the model as a screenshot rather
/// than as its name.
#[cfg(feature = "anthropic")]
#[test]
fn anthropics_dialect_sends_an_image_block_wherever_the_picture_is() {
    use nachalnik_providers::Anthropic;

    let provider = Arc::new(Anthropic::new("m", "https://example.invalid/v1", "k"));
    let body = rendered(provider, looking_at_a_picture());
    let messages = body["messages"].as_array().expect("messages");
    let image = json!({
        "type": "image",
        "source": { "type": "base64", "media_type": "image/png", "data": PIXEL },
    });

    assert_eq!(messages[0]["content"][0], image, "the user turn");
    assert_eq!(messages[1]["role"], "assistant");
    let result = &messages[2]["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["content"], json!([image]), "the result: {result}");
}

/// A document is not a picture, and the conventional dialect has a separate part for saying so.
///
/// note: the case `/attach` in `kamchatka` exists for. `image_url` means an image in this dialect
/// rather than "an attachment", so a PDF sent that way is a 400 from anything implementing the
/// spec - and until something that was not a picture had to go out, every blob went that way.
/// Google's dialect has no equivalent split: `inline_data` takes a mime type and carries whatever
/// it names, which is why only one of these two tests is about placement.
#[cfg(feature = "openai")]
#[test]
fn a_document_goes_out_as_a_file_part_and_not_as_a_picture() {
    use nachalnik::Blob;
    use nachalnik_providers::OpenAiCompatible;

    let provider = || {
        Arc::new(OpenAiCompatible::new(
            "m",
            "https://example.invalid/v1",
            "k",
        ))
    };
    let named = Blob::new("application/pdf", PIXEL).with_meta(json!({ "name": "results.pdf" }));
    let body = rendered(
        provider(),
        vec![ContextItem::user(Content::Blob(Arc::new(named)))],
    );
    let parts = &body["messages"][0]["content"];

    assert_eq!(parts[0]["type"], "file", "{parts}");
    assert_eq!(
        parts[0]["file"]["file_data"],
        json!(format!("data:application/pdf;base64,{PIXEL}"))
    );
    // what the producer called it, because it is the one thing that knew
    assert_eq!(parts[0]["file"]["filename"], "results.pdf");

    // and with nobody to say, a name derived from the media type - the part is refused without
    // one, so there is no option of leaving it out
    let bare = Content::blob("application/pdf", PIXEL);
    let body = rendered(provider(), vec![ContextItem::user(bare)]);

    assert_eq!(
        body["messages"][0]["content"][0]["file"]["filename"],
        "file.pdf"
    );
}

/// A recording goes out in the part that says it is one, and a film in the part that takes a URL.
///
/// note: a `file` part is refused for neither, and refused is what a caller needs: a recording sent
/// that way is dropped, and the model answers a question about bytes it never received. An audio
/// type the table says nothing about still goes out as a `file`, since guessing a container for it
/// is worse than an endpoint naming the one it wants.
#[cfg(feature = "openai")]
#[test]
fn a_recording_goes_out_as_audio_and_a_film_as_a_video() {
    use nachalnik_providers::OpenAiCompatible;

    let provider = || {
        Arc::new(OpenAiCompatible::new(
            "m",
            "https://example.invalid/v1",
            "k",
        ))
    };
    let spoken = |media_type: &str| {
        let body = rendered(
            provider(),
            vec![ContextItem::user(Content::blob(media_type, PIXEL))],
        );
        body["messages"][0]["content"][0].clone()
    };

    // a media type, and the word this dialect's `input_audio` wants for it
    for (media_type, format) in [
        ("audio/wav", "wav"),
        ("audio/x-wav", "wav"),
        ("audio/mpeg", "mp3"),
        ("audio/mp3", "mp3"),
    ] {
        let part = spoken(media_type);
        assert_eq!(part["type"], "input_audio", "{media_type}: {part}");
        assert_eq!(part["input_audio"]["data"], PIXEL, "{media_type}: {part}");
        assert_eq!(
            part["input_audio"]["format"], format,
            "{media_type}: {part}"
        );
    }

    // a film is a data URL under `video_url`, and the data URL is the whole of it
    let part = spoken("video/mp4");
    assert_eq!(part["type"], "video_url", "{part}");
    assert_eq!(
        part["video_url"]["url"],
        json!(format!("data:video/mp4;base64,{PIXEL}"))
    );

    // and an audio type the table says nothing about stays a file, rather than being sent as
    // something it is not
    let part = spoken("audio/ogg");
    assert_eq!(part["type"], "file", "{part}");
    assert_eq!(part["file"]["filename"], "file.ogg");
}

/// A turn that is a question about a recording carries the question and the recording.
///
/// note: the shape `/attach` builds, and the reason the placement above is a one-liner rather than
/// a conversion anywhere: a blob reached through `nachalnik` is a turn of `Blocks` with the
/// payload first, and the parts go out in the order they were put in.
#[test]
fn a_sentence_about_a_recording_carries_both() {
    let asked = Content::blocks([
        Block::text(Content::text("what is said in this?")),
        Block::text(Content::blob("audio/wav", PIXEL)),
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
            json!({ "type": "text", "text": "what is said in this?" })
        );
        assert_eq!(parts[1]["type"], "input_audio", "{parts}");
        assert_eq!(parts[1]["input_audio"]["format"], "wav", "{parts}");
    }

    // Google's dialect has one part for a payload of any media type, and it already carries
    // recordings; a sentence and a recording are two parts there for the same reason as here
    #[cfg(feature = "gemini")]
    {
        let provider = Arc::new(nachalnik_providers::Gemini::new(
            "m",
            "https://example.invalid",
            "k",
        ));
        let body = rendered(provider, vec![ContextItem::user(asked.clone())]);
        let parts = &body["contents"][0]["parts"];

        assert_eq!(parts[0]["text"], "what is said in this?");
        assert_eq!(
            parts[1]["inline_data"],
            json!({ "mime_type": "audio/wav", "data": PIXEL }),
            "{parts}"
        );
    }

    // this API takes pictures and PDFs and no recordings; it goes out as a document, the one
    // block a payload that is not a picture can be, and the API refuses it in its own words
    #[cfg(feature = "anthropic")]
    {
        let provider = Arc::new(nachalnik_providers::Anthropic::new(
            "m",
            "https://example.invalid/v1",
            "k",
        ));
        let body = rendered(provider, vec![ContextItem::user(asked)]);
        let parts = &body["messages"][0]["content"];

        assert_eq!(parts[0]["text"], "what is said in this?");
        assert_eq!(parts[1]["type"], "document", "{parts}");
        assert_eq!(parts[1]["source"]["media_type"], "audio/wav", "{parts}");
    }
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
        let body = rendered(provider, vec![ContextItem::user(asked.clone())]);
        let parts = &body["contents"][0]["parts"];

        assert_eq!(parts[0]["text"], "what is wrong with this?");
        assert_eq!(
            parts[1]["inline_data"],
            json!({ "mime_type": "image/png", "data": PIXEL }),
            "{parts}"
        );
    }

    #[cfg(feature = "anthropic")]
    {
        let provider = Arc::new(nachalnik_providers::Anthropic::new(
            "m",
            "https://example.invalid/v1",
            "k",
        ));
        let body = rendered(provider, vec![ContextItem::user(asked)]);
        let parts = &body["messages"][0]["content"];

        assert_eq!(parts[0]["text"], "what is wrong with this?");
        assert_eq!(
            parts[1]["source"],
            json!({ "type": "base64", "media_type": "image/png", "data": PIXEL }),
            "{parts}"
        );
    }
}
