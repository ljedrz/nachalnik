//! What a provider tells an endpoint about the program calling it, and which endpoint it tells.
//!
//! note: what a provider makes of a stream is in the conformance suite, which both dialects here
//! are held to. This is the part that is shared with nothing: a `HTTP-Referer` volunteered to
//! whatever address a caller has pointed this at is something the person running it did not ask
//! to send, and the address is a setting.

#![cfg(feature = "openai")]

use std::{net::SocketAddr, sync::Arc};

use nachalnik::{Config, ContextItem, Kernel};
use nachalnik_providers::{OpenAiCompatible, install_crypto};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Answers one request as `server` does, and hands back the request line and headers it was sent,
/// lowercased - which is how they arrive, and how the assertions read.
async fn overheard(provider: impl FnOnce(SocketAddr) -> OpenAiCompatible) -> String {
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
    kernel.set_provider(Arc::new(provider(address)));
    kernel.push(ContextItem::user("go"));
    let _ = kernel.step().await;

    listening
        .recv()
        .await
        .expect("the request was overheard")
        .to_lowercase()
}

/// A provider that believes it is talking to OpenRouter and is in fact talking to `address`.
///
/// note: the name in the URL is what decides whether the headers go out, and the resolver is what
/// decides where the socket goes, so the two can disagree - which is the only way to see the
/// headers that are sent *only* to OpenRouter without sending anything to OpenRouter.
///
/// note: [`install_crypto`] first, because this builds a [`reqwest::Client`] of its own and reqwest
/// is built here with `rustls-no-provider` - so `build()` panics unless the process default is
/// already in. Every constructor in the crate installs it, and this helper reaches `build()` before
/// it reaches one of them. That made three of the four tests in this file fail *sometimes*: the
/// fourth builds no client of its own, and whether the other three panicked came down to whether
/// its `OpenAiCompatible::new` happened to win the race and install the provider first. Which is
/// why the fix belongs here rather than in a test-ordering flag - a caller building its own client
/// is exactly the case the function is public for.
fn as_if_openrouter(address: SocketAddr) -> OpenAiCompatible {
    install_crypto();
    let client = reqwest::Client::builder()
        .resolve("openrouter.ai", address)
        .build()
        .expect("a client that resolves one name itself");
    OpenAiCompatible::new(
        "m",
        format!("http://openrouter.ai:{}/api/v1", address.port()),
        "k",
    )
    .with_client(client)
}

#[tokio::test]
async fn openrouter_is_told_the_app_the_name_and_what_kind_of_program_it_is() {
    let seen = overheard(|address| {
        as_if_openrouter(address)
            .on_behalf_of("https://example.invalid/app", "kamchatka")
            .filed_under(["cli-agent", "programming-app"])
    })
    .await;

    assert!(
        seen.contains("referer: https://example.invalid/app"),
        "{seen}"
    );
    assert!(seen.contains("x-openrouter-title: kamchatka"), "{seen}");
    // comma-separated and in the order they were given, which is the documented shape
    assert!(
        seen.contains("x-openrouter-categories: cli-agent,programming-app"),
        "{seen}"
    );
    // public is the absence of the header rather than a value, so an app that said nothing about
    // visibility must send nothing about it
    assert!(!seen.contains("visibility"), "{seen}");
}

#[tokio::test]
async fn an_app_that_asked_to_stay_off_the_listings_says_so() {
    let seen = overheard(|address| {
        as_if_openrouter(address)
            .on_behalf_of("https://internal.invalid", "telemetry")
            .unlisted(true)
    })
    .await;

    assert!(
        seen.contains("x-openrouter-app-visibility: hidden"),
        "{seen}"
    );
}

#[tokio::test]
async fn a_category_with_no_app_to_put_it_on_is_not_sent() {
    // OpenRouter builds the page against the referer, so a category on its own describes nothing:
    // it is a header that says what the caller is and buys them not one thing
    let seen = overheard(|address| {
        as_if_openrouter(address)
            .filed_under(["cli-agent"])
            .unlisted(true)
    })
    .await;

    assert!(!seen.contains("x-openrouter-"), "{seen}");
    assert!(!seen.contains("referer"), "{seen}");
}

#[tokio::test]
async fn the_app_headers_go_to_openrouter_and_nowhere_else() {
    // an endpoint that is not OpenRouter is whatever `KAMCHATKA_BASE_URL` was pointed at, and a
    // `HTTP-Referer` volunteered to it is something nobody asked to send
    let elsewhere = overheard(|address| {
        OpenAiCompatible::new("m", format!("http://{address}"), "k")
            .on_behalf_of("https://example.invalid", "kamchatka")
            .filed_under(["cli-agent"])
            .unlisted(true)
    })
    .await;
    assert!(
        !elsewhere.contains("referer") && !elsewhere.contains("x-openrouter-"),
        "a local endpoint is told nothing about the app: {elsewhere}"
    );

    // and one with no attribution at all sends none wherever it is pointed
    let silent =
        overheard(|address| OpenAiCompatible::new("m", format!("http://{address}"), "k")).await;
    assert!(!silent.contains("referer"), "{silent}");
}
