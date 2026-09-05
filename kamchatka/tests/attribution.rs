//! What this program tells an endpoint about itself, and which endpoint it tells.
//!
//! note: what a provider makes of a stream is in the conformance suite, which both of this
//! crate's providers are held to alongside `nachalnik-utils`'s. This is the part that is not
//! shared with anything: a `HTTP-Referer` volunteered to whatever address somebody has pointed
//! `KAMCHATKA_BASE_URL` at is something they did not ask to send.

use std::sync::Arc;

use kamchatka::provider::OpenAiCompatible;
use nachalnik::{Config, ContextItem, Kernel};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

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
