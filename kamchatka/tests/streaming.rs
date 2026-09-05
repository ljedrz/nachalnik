//! What the provider makes of the shapes a chat-completions stream actually arrives in.
//!
//! note: worth a socket rather than a unit test over a parser, because there is no parser to call:
//! the assembly of a streamed answer happens inside `respond`, between reading bytes off a
//! response and handing back a `ModelResponse`. A test that reached in beside it would be testing
//! a copy of the code under test.
//!
//! note: the dialect is "OpenAI-compatible" the way English is spoken in two countries. Every
//! server here agrees on the envelope and disagrees about the details, and the details are where
//! a model's request quietly turns into something it did not ask for.

use std::sync::Arc;

use kamchatka::provider::OpenAiCompatible;
use nachalnik::{Config, ContextItem, Kernel};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Answers one request with this body, as a stream, and closes.
async fn server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("the request");
        let mut discard = [0u8; 8192];
        let _ = socket.read(&mut discard).await;
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
        let _ = socket.flush().await;
        let _ = socket.shutdown().await;
    });

    format!("http://{address}")
}

/// Answers one request as `server` does, and hands back the request line and headers it was sent.
async fn overheard(provider: impl Fn(String) -> OpenAiCompatible) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    let (heard, mut listening) = tokio::sync::mpsc::channel(1);

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("the request");
        let mut seen = Vec::new();
        let mut buffer = [0u8; 4096];
        // just the head: the body follows the blank line, and the headers are the whole question
        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => seen.extend_from_slice(&buffer[..n]),
            }
        }
        let _ = heard
            .send(String::from_utf8_lossy(&seen).into_owned())
            .await;
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"index\":0}]}\n\n\
                    data: [DONE]\n\n";
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

    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(provider(format!("http://{address}"))));
    kernel.push(ContextItem::user("go"));
    let _ = kernel.step().await;

    listening.recv().await.expect("the request was overheard")
}

#[tokio::test]
async fn the_app_headers_go_to_openrouter_and_nowhere_else() {
    // an endpoint that is not OpenRouter is whatever `KAMCHATKA_BASE_URL` was pointed at, and a
    // `HTTP-Referer` volunteered to it is something nobody asked to send
    let elsewhere = overheard(|url| {
        OpenAiCompatible::new("m", url, "k").on_behalf_of("https://example.invalid", "kamchatka")
    })
    .await
    .to_lowercase();
    assert!(
        !elsewhere.contains("referer") && !elsewhere.contains("x-openrouter-title"),
        "a local endpoint is told nothing about the app: {elsewhere}"
    );

    // and one with no attribution at all sends none wherever it is pointed
    let silent = overheard(|url| OpenAiCompatible::new("m", url, "k"))
        .await
        .to_lowercase();
    assert!(!silent.contains("referer"), "{silent}");
}

/// Asks the model once, and hands back what the provider made of the answer.
async fn answered(body: &'static str) -> Arc<nachalnik::ModelResponse> {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(OpenAiCompatible::new(
        "streaming",
        server(body).await,
        "no key needed",
    )));
    kernel.push(ContextItem::user("go"));
    kernel.step().await.expect("the request is answered");

    kernel.last_response().expect("the model answered")
}

/// Google's compatible endpoint, which sends whole calls and no `index` at all.
///
/// note: this is the shape that made the bug: an absent index read as zero, so the second call
/// landed on the first - `write` and `write` became `writewrite`, its arguments became two JSON
/// objects run together, and the model was told there was no tool by that name. It had asked for
/// two perfectly ordinary calls.
const NO_INDEX: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_1\",\"type\":\"function\",",
    "\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}]},",
    "\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_2\",\"type\":\"function\",",
    "\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"b.txt\\\"}\"}}]},",
    "\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\",\"index\":0}]}\n\n",
    "data: [DONE]\n\n",
);

/// OpenAI's own, which numbers the calls and streams the arguments in fragments.
const INDEXED: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",",
    "\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\"}}]},\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_2\",",
    "\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\"}}]},\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,",
    "\"function\":{\"arguments\":\"\\\"a.txt\\\"}\"}}]},\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,",
    "\"function\":{\"arguments\":\"\\\"b.txt\\\"}\"}}]},\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\",\"index\":0}]}\n\n",
    "data: [DONE]\n\n",
);

/// MiniMax, which numbers its calls from one.
///
/// note: found by pointing this at `minimax/minimax-m3` on OpenRouter. The index says which call
/// a fragment belongs to; it does not say where in a list to put it. Read as a position, a first
/// call at index 1 left an unfilled call at index 0 - no identifier and no name - which the kernel
/// repaired and then reported as `tool.unknown` for a tool called "". One wasted round trip per
/// turn, and an error the model had to read and work around.
const FROM_ONE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_a\",",
    "\"type\":\"function\",\"function\":{\"name\":\"write\",\"arguments\":\"\"}}]},",
    "\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,",
    "\"function\":{\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}]},\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":2,\"id\":\"call_b\",",
    "\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]},",
    "\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":2,",
    "\"function\":{\"arguments\":\"{\\\"path\\\":\\\"b.txt\\\"}\"}}]},\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\",\"index\":0}]}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn calls_numbered_from_one_do_not_leave_an_empty_call_at_zero() {
    let response = answered(FROM_ONE).await;

    let named: Vec<&str> = response
        .tool_calls
        .iter()
        .map(|call| call.tool.as_str())
        .collect();
    assert_eq!(named, ["write", "read"], "{:?}", response.tool_calls);
    assert_eq!(response.tool_calls[0].id.0, "call_a");
    assert_eq!(response.tool_calls[1].id.0, "call_b");
    assert_eq!(response.tool_calls[0].args["path"], "a.txt");
    assert_eq!(response.tool_calls[1].args["path"], "b.txt");
}

#[tokio::test]
async fn two_calls_with_no_index_between_them_are_still_two_calls() {
    let response = answered(NO_INDEX).await;

    let named: Vec<&str> = response
        .tool_calls
        .iter()
        .map(|call| call.tool.as_str())
        .collect();
    assert_eq!(named, ["write", "write"], "{:?}", response.tool_calls);
    assert_eq!(response.tool_calls[0].args["path"], "a.txt");
    assert_eq!(response.tool_calls[1].args["path"], "b.txt");
    assert_eq!(response.tool_calls[0].id.0, "call_1");
    assert_eq!(response.tool_calls[1].id.0, "call_2");
}

#[tokio::test]
async fn arguments_streamed_against_an_index_are_assembled_by_it() {
    let response = answered(INDEXED).await;

    assert_eq!(response.tool_calls.len(), 2, "{:?}", response.tool_calls);
    assert_eq!(response.tool_calls[0].args["path"], "a.txt");
    assert_eq!(response.tool_calls[1].args["path"], "b.txt");
}

/// A chunk boundary is not a character boundary.
///
/// note: the bytes off the socket were decoded as they arrived, lossily. A two-byte character
/// split between two reads was therefore decoded twice - once with its tail missing and once with
/// its head missing - and became two replacement characters, which then went into the context, the
/// transcript and the session log with nothing to say they had ever been anything else. Every
/// model that answers in a language with diacritics, or with a typographic dash, hits it; it
/// depends only on where the network happened to break the stream.
#[tokio::test]
async fn a_character_split_between_two_reads_survives() {
    const SAID: &str = "zażółć — 大";
    const BODY: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"zażółć — 大\"},\
                        \"index\":0}]}\n\ndata: [DONE]\n\n";

    // every byte of the body is tried as the place the stream breaks, so the split lands inside
    // each of the multi-byte characters in turn rather than wherever one run happened to put it
    // the splits that can land inside a multi-byte character, plus the whole run around them
    let text = BODY.find("za").expect("the text is in there");
    for at in text..text + SAID.len() + 1 {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(Arc::new(OpenAiCompatible::new(
            "streaming",
            split_server(BODY, at).await,
            "no key needed",
        )));
        kernel.push(ContextItem::user("go"));
        kernel.step().await.expect("the request is answered");

        let said = kernel
            .last_response()
            .and_then(|response| response.content.clone())
            .map(|content| content.to_text().into_owned())
            .unwrap_or_default();
        assert_eq!(said, SAID, "the stream was broken after {at} bytes");
    }
}

/// Answers with `body` in two writes, split at `at` bytes.
async fn split_server(body: &'static str, at: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("the request");
        let mut discard = [0u8; 8192];
        let _ = socket.read(&mut discard).await;
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;

        let bytes = body.as_bytes();
        let _ = socket.write_all(&bytes[..at]).await;
        let _ = socket.flush().await;
        // long enough for the first half to be read on its own, which is the whole point
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let _ = socket.write_all(&bytes[at..]).await;
        let _ = socket.flush().await;
        let _ = socket.shutdown().await;
    });

    format!("http://{address}")
}
