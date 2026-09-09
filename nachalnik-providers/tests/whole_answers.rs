//! The path a *whole* answer takes, which only one of the two dialects has.
//!
//! note: what the dialects have in common is in the conformance suite, and `streaming(false)` is
//! not part of it - `Gemini` always asks for a stream, so there is nothing for this to agree
//! with. This is what is left over: the answer that arrives in one piece, read by `whole` rather
//! than assembled from fragments.

#![cfg(feature = "openai")]

use std::sync::Arc;

use nachalnik::{Config, ContextItem, Kernel};
use nachalnik_providers::OpenAiCompatible;
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
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(
        OpenAiCompatible::new("streaming", whole_server(body).await, "no key needed")
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

/// A rate limit as this dialect actually reports it to a whole-answer request: an `error` object
/// with a status in it, inside a perfectly good 200.
const LIMITED: &str = concat!(
    "{\"error\":{\"code\":429,\"message\":\"Provider returned error\",",
    "\"metadata\":{\"raw\":\"rate limited upstream\"}}}",
);

/// The answer once it stops being busy.
const ANSWERED: &str = concat!(
    "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"here\"},",
    "\"finish_reason\":\"stop\"}]}",
);

/// A rate limit is waited out whether it arrives as a status or inside a 200.
///
/// note: the streamed path never had to know, because a `429` there is a `429`. This dialect
/// answers a *whole* request that its upstream refused with a 200 carrying an `error` object, and
/// reading only the status makes that a hard failure - the run stops on something that would have
/// worked in two seconds. Found against OpenRouter; the body below is one of theirs.
#[tokio::test]
async fn a_rate_limit_inside_a_good_status_is_waited_out_like_any_other() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(
        OpenAiCompatible::new("busy", busy_then(LIMITED, ANSWERED).await, "no key needed")
            .streaming(false),
    ));
    kernel.push(ContextItem::user("go"));
    kernel.step().await.expect("the second attempt is answered");

    let response = kernel.last_response().expect("the model answered");
    assert_eq!(
        response.content.as_ref().map(|c| c.to_text().into_owned()),
        Some("here".to_owned()),
        "the answer after the wait is the one that counts"
    );
}

/// A spent daily quota is not waited out, because it will still be spent in a minute.
#[tokio::test]
async fn a_daily_quota_is_told_apart_from_a_server_that_is_merely_busy() {
    const SPENT: &str =
        "{\"error\":{\"code\":429,\"message\":\"Rate limit exceeded: free-models-per-day\"}}";

    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(
        OpenAiCompatible::new("spent", busy_then(SPENT, ANSWERED).await, "no key needed")
            .streaming(false),
    ));
    kernel.push(ContextItem::user("go"));

    let why = kernel.step().await.expect_err("a spent quota is an error");
    assert!(
        nachalnik_providers::out_of_quota(&why.to_string()),
        "and one a caller can recognise: {why}"
    );
}

/// Answers one request with `body`, in one write.
async fn whole_server(body: &'static str) -> String {
    busy_then(body, body).await
}

/// Answers the first request with `first` and every one after it with `then`.
async fn busy_then(first: &'static str, then: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        let mut answered = 0;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let body = match answered {
                0 => first,
                _ => then,
            };
            answered += 1;

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
        }
    });

    format!("http://{address}")
}
