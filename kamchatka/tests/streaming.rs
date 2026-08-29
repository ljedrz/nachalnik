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
