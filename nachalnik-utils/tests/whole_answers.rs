//! The path a *whole* answer takes, which is this provider's alone.
//!
//! note: what every provider here has in common is in the conformance suite, and `streaming(false)`
//! is not part of it: `kamchatka`'s two always ask for a stream, so there is no second
//! implementation for them to agree with. This is what is left over - the answer that arrives in
//! one piece, parsed by `parse` rather than by `parse_stream`.

use std::sync::Arc;

use nachalnik::{Config, ContextItem, Kernel};
use nachalnik_utils::OpenAiCompatible;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

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
