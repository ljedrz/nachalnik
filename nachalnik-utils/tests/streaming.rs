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
