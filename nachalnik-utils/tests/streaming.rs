//! What this provider makes of a stream the network broke in an awkward place.
//!
//! note: a socket rather than a unit test over a parser, because there is no parser to call: the
//! assembly of a streamed answer happens inside `respond`, between reading bytes off a response
//! and handing back a `ModelResponse`. A test that reached in beside it would be testing a copy of
//! the code under test.

use std::sync::Arc;

use nachalnik::{Config, ContextItem, Kernel};
use nachalnik_utils::OpenAiCompatible;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// A chunk boundary is not a character boundary.
///
/// note: the bytes off the socket were decoded as they arrived, lossily. A character split between
/// two reads was therefore decoded twice - once with its tail missing and once with its head - and
/// became two replacement characters, which then went into the context and into whatever record
/// was kept of the run. It depends only on where the network happened to break the stream, so it
/// is a coin toss on every chunk of every answer in a language with diacritics.
#[tokio::test]
async fn a_character_split_between_two_reads_survives() {
    const SAID: &str = "zażółć — 大";
    const BODY: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"zażółć — 大\"},\"index\":0}]}\n\n\
                        data: [DONE]\n\n";

    // every byte through the awkward text is tried as the place the stream breaks, so the split
    // lands inside each multi-byte character in turn rather than wherever one run happened to
    // put it
    // reqwest is built with `rustls-no-provider`, so a client cannot be made until one is
    // named; the callers of this crate do it in `main`
    let _ = rustls::crypto::ring::default_provider().install_default();

    let text = BODY.find("za").expect("the text is in there");
    for at in text..text + SAID.len() + 1 {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(Arc::new(OpenAiCompatible::new(
            reqwest::Client::new(),
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

/// Google's compatible endpoint, which sends whole calls and no `index` at all.
///
/// note: this is the shape that makes the bug: an absent index read as zero, so the second call
/// lands on the first - `write` and `write` become `writewrite`, the arguments become two JSON
/// objects run together, and the model is told there is no tool by that name. It had asked for
/// two perfectly ordinary calls. Seven of this study's exploratory runs went through this
/// endpoint.
const NO_INDEX: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_1\",\"type\":\"function\",",
    "\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}]},\"index\":0}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_2\",\"type\":\"function\",",
    "\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"b.txt\\\"}\"}}]},\"index\":0,",
    "\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
);

/// A provider that numbers its calls from one, which leaves nothing at zero.
const FROM_ONE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_a\",",
    "\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"index\":0,",
    "\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
);

/// A model that wrote arguments which are not JSON.
const BROKEN_ARGS: &str = concat!(
    "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"tool_calls\":[{\"id\":\"c1\",",
    "\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{path: notes\"}}]},",
    "\"finish_reason\":\"tool_calls\"}]}",
);

/// Asks once, and hands back what the provider made of the answer.
async fn answered(body: &'static str, streaming: bool) -> std::sync::Arc<nachalnik::ModelResponse> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(
        OpenAiCompatible::new(
            reqwest::Client::new(),
            "streaming",
            whole_server(body).await,
            "no key needed",
        )
        .streaming(streaming),
    ));
    kernel.push(ContextItem::user("go"));
    kernel.step().await.expect("the request is answered");

    kernel.last_response().expect("the model answered")
}

/// Two calls in one turn are two calls, whether or not the server numbered them.
#[tokio::test]
async fn calls_are_told_apart_however_the_server_files_them() {
    let response = answered(NO_INDEX, true).await;
    let named: Vec<&str> = response
        .tool_calls
        .iter()
        .map(|call| call.tool.as_str())
        .collect();
    assert_eq!(named, ["write", "write"], "{:?}", response.tool_calls);
    assert_eq!(response.tool_calls[0].args["path"], "a.txt");
    assert_eq!(response.tool_calls[1].args["path"], "b.txt");

    // and an index that starts at one does not leave an unfilled call behind it
    let response = answered(FROM_ONE, true).await;
    assert_eq!(response.tool_calls.len(), 1, "{:?}", response.tool_calls);
    assert_eq!(response.tool_calls[0].tool, "read");
    assert_eq!(response.tool_calls[0].id.0, "call_a");
}

/// Arguments that are not JSON are handed over as what they were, not as nothing.
///
/// note: the streamed path has always done this and the whole-answer path did not - it swallowed
/// the failure and produced `{}`, so a tool ran with no arguments and neither the tool nor the
/// model was told why.
#[tokio::test]
async fn a_model_that_writes_broken_arguments_is_shown_that_it_did() {
    let response = answered(BROKEN_ARGS, false).await;

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(
        response.tool_calls[0].args["_unparsed"], "{path: notes",
        "{:?}",
        response.tool_calls[0].args
    );
}

/// Answers one request with `body`, in one write.
async fn whole_server(body: &'static str) -> String {
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
        let _ = socket.shutdown().await;
    });

    format!("http://{address}")
}

/// Answers one request with `body`, in two writes split at `at` bytes.
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
