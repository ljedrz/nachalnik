//! Thinking a model wrote into its own content, and getting it back out of there.
//!
//! note: this dialect carries the thinking in `reasoning`, beside the content. A model whose chat
//! template ends the prompt *inside* a thinking block never writes the `<think>` that opened it,
//! and an endpoint serving that model with no reasoning parser of its own passes the lot through as
//! content - so the answer arrives with its own thinking on the front and a bare `</think>` in the
//! middle. Seen against `poolside/laguna-xs-2.1:free` on nine turns of one conversation: the tag
//! went into the context, the transcript and the log, and `reasoning` was `None` every time while
//! the reasoning sat in the text.
//!
//! note: both wire paths, because they read the same answer by different routes and the whole point
//! of sharing the reader is that they must agree about it. The streamed one is the one that found
//! it; the whole-answer one would have failed the same way and nobody had looked.

#![cfg(feature = "openai")]

use std::sync::Arc;

use nachalnik::{Config, ContextItem, Kernel, ModelResponse};
use nachalnik_providers::OpenAiCompatible;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// What the provider made of one answer, asked for whichever way.
async fn answered(provider: OpenAiCompatible) -> Arc<ModelResponse> {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(provider));
    kernel.push(ContextItem::user("go"));
    kernel.step().await.expect("the request is answered");

    kernel.last_response().expect("the model answered")
}

/// The thinking, and the answer, as the two of them come back.
fn read(response: &ModelResponse) -> (Option<String>, Option<String>) {
    (
        response
            .reasoning
            .as_ref()
            .map(|c| c.to_text().into_owned()),
        response.content.as_ref().map(|c| c.to_text().into_owned()),
    )
}

/// The shape that is actually sent: no opener, and the delimiter split across two chunks, because
/// a fragment boundary falls wherever the server felt like putting it.
const FRAGMENTS: &[&str] = &[
    "I should look at the budget first.",
    "</th",
    "ink>",
    "Let me check the budget:",
];

#[tokio::test]
async fn a_streamed_answer_gives_up_the_thinking_written_into_it() {
    let response = answered(OpenAiCompatible::new(
        "laguna",
        streaming_server(FRAGMENTS, None).await,
        "no key needed",
    ))
    .await;

    let (thought, said) = read(&response);
    assert_eq!(
        thought.as_deref(),
        Some("I should look at the budget first.")
    );
    assert_eq!(said.as_deref(), Some("Let me check the budget:"));
    assert!(
        !said.unwrap_or_default().contains("think"),
        "and the tag is in neither half"
    );
}

#[tokio::test]
async fn a_whole_answer_gives_up_the_same_thinking_the_same_way() {
    const BODY: &str = concat!(
        "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":",
        "\"I should look at the budget first.</think>Let me check the budget:\"},",
        "\"finish_reason\":\"stop\"}]}",
    );

    let response = answered(
        OpenAiCompatible::new("laguna", whole_server(BODY).await, "no key needed").streaming(false),
    )
    .await;

    let (thought, said) = read(&response);
    assert_eq!(
        thought.as_deref(),
        Some("I should look at the budget first.")
    );
    assert_eq!(said.as_deref(), Some("Let me check the budget:"));
}

/// An endpoint that fills `reasoning` itself has a parser of its own, so a `</think>` in the content
/// it sends is a model writing the characters - and the answer keeps every word of itself.
#[tokio::test]
async fn an_endpoint_that_reports_its_own_reasoning_keeps_its_content_whole() {
    let said = "a `</think>` closes the block";
    let response = answered(OpenAiCompatible::new(
        "careful",
        streaming_server(&["a `</think>` closes the block"], Some("weighed it up")).await,
        "no key needed",
    ))
    .await;

    let (thought, content) = read(&response);
    assert_eq!(content.as_deref(), Some(said), "not a word of it was moved");
    assert_eq!(thought.as_deref(), Some("weighed it up"));
}

/// And the guard that matters most: a model that closes no thinking is a model whose answer is its
/// answer. Reading content as thinking by default would empty the turn of every model that has none.
#[tokio::test]
async fn an_answer_with_no_tag_in_it_arrives_exactly_as_it_was_sent() {
    let response = answered(OpenAiCompatible::new(
        "plain",
        streaming_server(&["just", " an ordinary ", "answer"], None).await,
        "no key needed",
    ))
    .await;

    let (thought, said) = read(&response);
    assert_eq!(said.as_deref(), Some("just an ordinary answer"));
    assert_eq!(thought, None, "there was nothing to report as thinking");
}

/// Turned off, the content is whatever the wire said it was - tag and all.
#[tokio::test]
async fn nothing_is_moved_when_the_reading_is_turned_off() {
    let response = answered(
        OpenAiCompatible::new("laguna", streaming_server(FRAGMENTS, None).await, "no key")
            .thinking_in_content(false),
    )
    .await;

    let (thought, said) = read(&response);
    assert_eq!(
        said.as_deref(),
        Some("I should look at the budget first.</think>Let me check the budget:")
    );
    assert_eq!(thought, None);
}

/// Streams `content` one chunk per fragment, with `reasoning` on the first of them if there is any.
async fn streaming_server(
    content: &'static [&'static str],
    reasoning: Option<&'static str>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut discard = [0u8; 8192];
        let _ = socket.read(&mut discard).await;

        let mut body = String::new();
        if let Some(reasoning) = reasoning {
            body.push_str(&format!(
                "data: {}\n\n",
                serde_json::json!({ "choices": [{ "delta": { "reasoning": reasoning } }] })
            ));
        }
        for fragment in content {
            body.push_str(&format!(
                "data: {}\n\n",
                serde_json::json!({ "choices": [{ "delta": { "content": fragment } }] })
            ));
        }
        body.push_str(&format!(
            "data: {}\n\n",
            serde_json::json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] })
        ));
        body.push_str("data: [DONE]\n\n");

        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = socket.shutdown().await;
    });

    format!("http://{address}")
}

/// Answers one request with `body`, in one write.
async fn whole_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut discard = [0u8; 8192];
        let _ = socket.read(&mut discard).await;
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = socket.shutdown().await;
    });

    format!("http://{address}")
}
