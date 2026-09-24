//! What is read off the socket, whichever dialect is reading it: a body that ends without a
//! newline, one that was never a stream, and a refusal that says how long to wait.
//!
//! note: the conformance suite is shapes a server really sent, and these are not all that. They
//! are the edges of the one reader both dialects share, where the two copies it replaced had each
//! drifted, and each case is asked of both.

#![cfg(any(feature = "openai", feature = "gemini"))]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use nachalnik::{Config, ContextItem, Kernel, ModelResponse, Provider};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Answers every request with `status`, `headers` and `body`, and counts the requests.
async fn server(
    status: &'static str,
    headers: &'static str,
    body: &'static str,
    requests: Arc<AtomicUsize>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            requests.fetch_add(1, Ordering::SeqCst);
            let mut discard = [0u8; 16384];
            let _ = socket.read(&mut discard).await;
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\n\r\n{body}",
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

/// Puts one question to `provider` and hands back what the kernel ended up holding.
async fn asked(provider: Arc<dyn Provider>) -> Result<Arc<ModelResponse>, String> {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider);
    kernel.push(ContextItem::user("go"));
    kernel.step().await.map_err(|e| e.to_string())?;

    kernel
        .last_response()
        .ok_or_else(|| "the provider answered with nothing".to_owned())
}

/// What the turn said, whichever shape it came back in.
fn said(response: &ModelResponse) -> String {
    response
        .content
        .as_ref()
        .map(|content| content.to_text().into_owned())
        .unwrap_or_default()
}

/// The dialects this crate was built with, each asking the server at `url`.
fn dialects(url: &str) -> Vec<(&'static str, Arc<dyn Provider>)> {
    vec![
        #[cfg(feature = "openai")]
        (
            "openai",
            Arc::new(nachalnik_providers::OpenAiCompatible::new(
                "m",
                url,
                "no key needed",
            )) as Arc<dyn Provider>,
        ),
        #[cfg(feature = "gemini")]
        (
            "gemini",
            Arc::new(nachalnik_providers::Gemini::new("m", url, "no key needed"))
                as Arc<dyn Provider>,
        ),
    ]
}

/// The last event of a body is read when nothing follows it - no blank line, no newline at all.
///
/// note: the reader took a line to be what ends in a newline, so the bytes after the last one
/// waited for the rest of their line until the body ended, and were then dropped with it. Where
/// the last event is the one carrying the answer, the turn came back empty.
#[tokio::test]
async fn a_last_event_with_nothing_after_it_is_still_read() {
    for (dialect, body) in [
        (
            "openai",
            "data: {\"choices\":[{\"delta\":{\"content\":\"all of it\"},\"finish_reason\":\"stop\"}]}",
        ),
        (
            "gemini",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"all of it\"}]},\
             \"finishReason\":\"STOP\"}]}",
        ),
    ] {
        let requests = Arc::new(AtomicUsize::new(0));
        let url = server(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            body,
            requests,
        )
        .await;
        let Some((_, provider)) = dialects(&url).into_iter().find(|(d, _)| *d == dialect) else {
            continue;
        };

        let response = asked(provider).await.expect("an answer");
        assert_eq!(said(&response), "all of it", "{dialect}");
    }
}

/// A body that was never a stream is reported by what it says, not by its last line.
///
/// note: what was left to report was whatever followed the last newline - for a page of HTML,
/// its closing tag - because each whole line had been read as a candidate event and dropped.
#[tokio::test]
async fn a_body_that_is_not_a_stream_is_reported_whole() {
    let requests = Arc::new(AtomicUsize::new(0));
    let url = server(
        "200 OK",
        "Content-Type: text/html\r\n",
        "<html>\n<body>the upstream went away</body>\n</html>\n",
        requests,
    )
    .await;

    for (dialect, provider) in dialects(&url) {
        let error = asked(provider).await.expect_err("a page is not an answer");
        assert!(
            error.contains("the upstream went away"),
            "{dialect}: the page's own words are the account: {error}"
        );
    }
}

/// A server that ignored the request for a stream and answered whole has still answered.
///
/// note: one line of JSON and no `data:` in front of it, so there was nothing for a stream reader
/// to find, and a finished answer was reported as the stream having carried no data.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_whole_answer_to_a_request_for_a_stream_is_an_answer() {
    let requests = Arc::new(AtomicUsize::new(0));
    let url = server(
        "200 OK",
        "Content-Type: application/json\r\n",
        "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"whole\"},\
         \"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}",
        requests,
    )
    .await;

    let response = asked(Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "m",
        url,
        "no key needed",
    )))
    .await
    .expect("an answer");
    assert_eq!(said(&response), "whole");
    assert_eq!(response.usage.and_then(|usage| usage.input_tokens), Some(3));
}

/// Google's unstreamed answer - a list of the events a stream would have carried - is read as
/// those events.
///
/// note: what `streamGenerateContent` sends where `alt=sse` did not arrive, which a proxy that
/// drops the query string is enough to cause.
#[cfg(feature = "gemini")]
#[tokio::test]
async fn a_list_of_events_is_read_as_the_stream_it_would_have_been() {
    let requests = Arc::new(AtomicUsize::new(0));
    let url = server(
        "200 OK",
        "Content-Type: application/json\r\n",
        "[{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"one \"}]}}]},\n\
         {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"two\"}]},\"finishReason\":\"STOP\"}],\
         \"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":2}}]",
        requests,
    )
    .await;

    let response = asked(Arc::new(nachalnik_providers::Gemini::new(
        "m",
        url,
        "no key needed",
    )))
    .await
    .expect("an answer");
    assert_eq!(said(&response), "one two");
    assert_eq!(response.stop, nachalnik::StopReason::EndTurn);
}

/// A busy server that asks to be left longer than the provider waits is told so at once, in its
/// own words, whichever dialect asked.
///
/// note: only one of the two read `Retry-After`. The other doubled its way through four attempts
/// regardless of what it was asked, and then put the whole error body - status, details and all -
/// into the turn's error.
#[tokio::test]
async fn a_refusal_that_asks_for_a_long_wait_is_reported_rather_than_sat_through() {
    for (dialect, _) in dialects("http://127.0.0.1:1") {
        let requests = Arc::new(AtomicUsize::new(0));
        let url = server(
            "429 Too Many Requests",
            "Retry-After: 120\r\nContent-Type: application/json\r\n",
            "{\"error\":{\"code\":429,\"message\":\"Resource has been exhausted.\",\
             \"status\":\"RESOURCE_EXHAUSTED\",\"details\":[]}}",
            requests.clone(),
        )
        .await;
        let (_, provider) = dialects(&url)
            .into_iter()
            .find(|(d, _)| *d == dialect)
            .expect("built above");

        let error = tokio::time::timeout(std::time::Duration::from_secs(5), asked(provider))
            .await
            .unwrap_or_else(|_| panic!("{dialect}: a two-minute wait was sat through"))
            .expect_err("a refusal");
        assert!(
            error.contains("Resource has been exhausted") && error.contains("longer than"),
            "{dialect}: {error}"
        );
        assert!(
            !error.contains("RESOURCE_EXHAUSTED"),
            "{dialect}: the sentence, not the envelope: {error}"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1, "{dialect}");
    }
}

/// `[DONE]` ends the stream, whether or not the server closes the connection after it.
///
/// note: the connection here stays open once the answer is out. Read past `[DONE]` as a line that
/// is not JSON, a finished answer waited out the whole of the stall bound and was then reported as
/// a stall rather than as the answer it was.
#[cfg(feature = "openai")]
#[tokio::test]
async fn done_ends_the_stream_while_the_connection_stays_open() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut discard = [0u8; 16384];
                let _ = socket.read(&mut discard).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n\
                          data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\
                          \"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                    )
                    .await;
                // and nothing more: not a byte, and not a close
                tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                drop(socket);
            });
        }
    });
    let provider = Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "m",
        format!("http://{address}"),
        "",
    ));

    let response = tokio::time::timeout(std::time::Duration::from_secs(10), asked(provider))
        .await
        .expect("the answer was over and the read went on waiting")
        .expect("the question failed");

    assert_eq!(said(&response), "ok");
}

/// A line that arrives over many chunks is read in a moment, however long it is.
///
/// note: a Gemini call's arguments, or an image, arrive as one event on one line. Searched for its
/// newline from the start at every chunk, a line of megabytes in pieces the size of a TLS record
/// took time quadratic in its length. The reader is shared, so one dialect asks it.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_long_line_in_many_pieces_is_read_in_a_moment() {
    const LONG: usize = 12 << 20;
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = socket.set_nodelay(true);
                let mut discard = [0u8; 16384];
                let _ = socket.read(&mut discard).await;
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
                let text = "a".repeat(LONG);
                let body = format!(
                    "{head}data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\
                     \"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
                );
                // a piece at a time, handing over between them: written at once, the whole body is
                // in the socket before the reader looks, and arrives as a few large chunks
                for piece in body.as_bytes().chunks(16 << 10) {
                    if socket.write_all(piece).await.is_err() {
                        return;
                    }
                    let _ = socket.flush().await;
                    tokio::task::yield_now().await;
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    let provider = Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "m",
        format!("http://{address}"),
        "",
    ));

    let started = std::time::Instant::now();
    let response = asked(provider).await.expect("an answer");
    assert_eq!(said(&response).len(), LONG);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
