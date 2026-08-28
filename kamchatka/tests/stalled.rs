//! What happens when the model answers the connection and then says nothing at all.
//!
//! note: This is the one thing a scripted provider cannot imitate, and it is worth a socket: a
//! provider that only checks the interrupt *between* fragments is stuck for ever when no fragment
//! ever arrives, and `esc` does nothing whatever. It looks identical to a working program.

use std::{sync::Arc, time::Duration};

use kamchatka::provider::OpenAiCompatible;
use nachalnik::{Config, ContextItem, Kernel};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Accepts one request, answers it as a stream, and then holds the socket open saying nothing.
///
/// note: Not a closed connection and not an error - those are already handled. This is the
/// awkward case: a perfectly good response that never continues.
async fn silent_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("the request");
        let mut discard = [0u8; 4096];
        let _ = socket.read(&mut discard).await;
        let _ = socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                  Transfer-Encoding: chunked\r\n\r\n",
            )
            .await;
        let _ = socket.flush().await;

        // and now nothing, for longer than any test will wait
        tokio::time::sleep(Duration::from_secs(600)).await;
    });

    format!("http://{address}")
}

#[tokio::test]
async fn a_model_that_says_nothing_at_all_can_still_be_stopped() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(OpenAiCompatible::new(
        "silent",
        silent_server().await,
        "no key needed",
    )));
    kernel.push(ContextItem::user("are you there?"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });

    // long enough for the request to be sent and the headers to come back, so that the interrupt
    // lands while the stream is waiting rather than before it starts
    tokio::time::sleep(Duration::from_millis(500)).await;
    kernel.interrupt();

    let stopped = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the interrupt should reach a stream that has not said anything")
        .expect("the turn is not a panic");

    // and it stops as a turn that was stopped, rather than as a provider that went wrong: an
    // error here would put a red line on the screen for doing exactly what it was told
    stopped.expect("an interrupted request is not a failed one");

    // nothing was said, so nothing was recorded as having been said
    assert!(
        kernel.last_response().is_none_or(|r| r.content.is_none()),
        "a stream that carried no text should not have produced any"
    );
}
