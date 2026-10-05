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

use nachalnik::{Config, ContextItem, Kernel, ModelResponse, Provider, StopReason};
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

/// A byte-order mark before the first event does not cost the event.
///
/// note: the mark is not whitespace to `trim`, so the first line began with it rather than with
/// `data:` and was skipped as a line that is not an event - and a stream whose first event is its
/// only one came back empty.
#[tokio::test]
async fn a_byte_order_mark_does_not_cost_the_first_event() {
    for (dialect, body) in [
        (
            "openai",
            "\u{feff}data: {\"choices\":[{\"delta\":{\"content\":\"all of it\"},\"finish_reason\":\"stop\"}]}\n\n",
        ),
        (
            "gemini",
            "\u{feff}data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"all of it\"}]},\
             \"finishReason\":\"STOP\"}]}\n\n",
        ),
    ] {
        let url = server(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            body,
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let Some((_, provider)) = dialects(&url).into_iter().find(|(d, _)| *d == dialect) else {
            continue;
        };

        let response = asked(provider).await.expect("an answer");
        assert_eq!(said(&response), "all of it", "{dialect}");
    }
}

/// An event spread over several `data:` lines is read as one, and a line that is not JSON does not
/// take the events after it with it.
///
/// note: each line was read as an event on its own, so an event the format allows to be written
/// over several lines was dropped a line at a time.
#[tokio::test]
async fn an_event_spread_over_several_lines_is_one_event() {
    for (dialect, body) in [
        (
            "openai",
            "data: not json\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"all \"}}]}\n\
             data: {\"choices\":[{\"delta\":\n\
             data:  {\"content\":\"of it\"},\n\
             data:  \"finish_reason\":\"stop\"}]}\n\n",
        ),
        (
            "gemini",
            "data: not json\n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"all \"}]}}]}\n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"of it\"}]},\n\
             data:  \"finishReason\":\"STOP\"}]}",
        ),
    ] {
        let url = server(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            body,
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let Some((_, provider)) = dialects(&url).into_iter().find(|(d, _)| *d == dialect) else {
            continue;
        };

        let response = asked(provider).await.expect("an answer");
        assert_eq!(said(&response), "all of it", "{dialect}");
        assert_eq!(response.stop, StopReason::EndTurn, "{dialect}");
    }
}

/// A comment between the lines of a spread event does not end it.
///
/// note: the format allows a comment anywhere, a keep-alive among them, and only a blank line ends
/// an event - so the lines either side of one are still one event.
#[tokio::test]
async fn a_comment_inside_a_spread_event_does_not_end_it() {
    for (dialect, body) in [
        (
            "openai",
            "data: {\"choices\":[{\"delta\":\n\
             : keep-alive\n\
             data:  {\"content\":\"all of it\"},\"finish_reason\":\"stop\"}]}\n\n",
        ),
        (
            "gemini",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"all of it\"}]},\n\
             : keep-alive\n\
             data:  \"finishReason\":\"STOP\"}]}\n\n",
        ),
    ] {
        let url = server(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            body,
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let Some((_, provider)) = dialects(&url).into_iter().find(|(d, _)| *d == dialect) else {
            continue;
        };

        let response = asked(provider).await.expect("an answer");
        assert_eq!(said(&response), "all of it", "{dialect}");
    }
}

/// A `[DONE]` written over several `data:` lines ends the stream like one written on a line of its
/// own, and a connection held open after it does not keep the read waiting.
///
/// note: the sentinel was matched on one line, while the event parse beside it had already learnt
/// that an event may be spread over several. A server that re-wraps a stream - which a proxy is
/// enough for - splits it, and the split was dropped as something that would not parse. The
/// answer had already arrived, and it sat out the whole of the stall bound to be reported as a
/// stall, and the turn that was complete was recorded as interrupted.
///
/// note: the connection is held open on purpose, and the bound is on the read. An ordinary
/// assertion on the answer would pass either way - the answer is right whichever way the
/// sentinel is read - and only the waiting shows the difference.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_done_spread_over_two_lines_ends_the_stream_while_the_connection_stays_open() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut discard = [0u8; 16384];
                let _ = socket.read(&mut discard).await;
                // the answer, then the sentinel in two pieces, and no finish_reason to say so
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n\
                          data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
                          data: [DO\ndata: NE]\n\n",
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
    // and the server's own end of the stream is an end of the turn, not a reason nobody gave
    assert_eq!(response.stop, StopReason::EndTurn);
}

/// Answers every request with `status` and a stream that promises more than `body` and then hangs
/// up, which is what the transport reads as a body broken off.
async fn broken_off(status: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut discard = [0u8; 16384];
            let _ = socket.read(&mut discard).await;
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\n\
                         Content-Length: {}\r\n\r\n{body}",
                        body.len() + 64
                    )
                    .as_bytes(),
                )
                .await;
            let _ = socket.shutdown().await;
        }
    });

    format!("http://{address}")
}

/// A stream broken off after the turn said why it ended is a whole turn; one broken off before
/// that is cut off.
///
/// note: the first is a complete answer that lost its trailing bytes, and calling it cut off
/// invents a fault the model did not have. The second is here so that the first is known to have
/// been broken off at all.
#[tokio::test]
async fn a_stream_broken_off_after_its_finish_is_a_whole_turn() {
    for (dialect, finished, unfinished) in [
        (
            "openai",
            "data: {\"choices\":[{\"delta\":{\"content\":\"all of it\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"all of it\"}}]}\n\n",
        ),
        (
            "gemini",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"all of it\"}]},\
             \"finishReason\":\"STOP\"}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"all of it\"}]}}]}\n\n",
        ),
    ] {
        for (body, stop) in [
            (finished, StopReason::EndTurn),
            (unfinished, StopReason::Other("cut off".to_owned())),
        ] {
            let url = broken_off("200 OK", body).await;
            let Some((_, provider)) = dialects(&url).into_iter().find(|(d, _)| *d == dialect)
            else {
                continue;
            };

            let response = asked(provider).await.expect("what arrived is kept");
            assert_eq!(said(&response), "all of it", "{dialect}");
            assert_eq!(response.stop, stop, "{dialect}");
        }
    }
}

/// A stream that closes cleanly before saying the turn is over is cut off; one that said so, by a
/// finish or by `[DONE]`, is whole.
///
/// note: a clean close was taken for the server's own end, so a stream that stopped after half an
/// answer - and after half a line of the next event - was recorded as finished for no reason
/// given, with nothing said about it. The close is the transport's word; the finish and `[DONE]`
/// are the answer's, and a server may send either without the other.
///
/// note: the second case is the other half of that. `unreported` is the word for a turn whose
/// reason nobody gave, and a server that sent `[DONE]` and no `finish_reason` gave one: the
/// marker is its own end of the turn. The two absences of a `finish_reason` are told apart by
/// what else arrived - the marker, or nothing.
#[tokio::test]
async fn a_stream_that_closes_before_saying_it_is_over_is_cut_off() {
    let cut_off = StopReason::Other("cut off".to_owned());
    for (dialect, body, stop) in [
        (
            "openai",
            "data: {\"choices\":[{\"delta\":{\"content\":\"all of it\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"and mo",
            cut_off.clone(),
        ),
        (
            "openai",
            "data: {\"choices\":[{\"delta\":{\"content\":\"all of it\"}}]}\n\ndata: [DONE]\n\n",
            StopReason::EndTurn,
        ),
        (
            "openai",
            "data: {\"choices\":[{\"delta\":{\"content\":\"all of it\"},\"finish_reason\":\"stop\"}]}\n\n",
            StopReason::EndTurn,
        ),
        (
            "gemini",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"all of it\"}]}}]}\n\n",
            cut_off.clone(),
        ),
        (
            "gemini",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"all of it\"}]}}]}\n\n\
             data: [DONE]\n\n",
            StopReason::EndTurn,
        ),
        (
            "gemini",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"all of it\"}]},\
             \"finishReason\":\"STOP\"}]}\n\n",
            StopReason::EndTurn,
        ),
    ] {
        let url = server(
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            body,
            Arc::new(AtomicUsize::new(0)),
        )
        .await;
        let Some((_, provider)) = dialects(&url).into_iter().find(|(d, _)| *d == dialect) else {
            continue;
        };

        let response = asked(provider).await.expect("what arrived is kept");
        assert_eq!(said(&response), "all of it", "{dialect}");
        assert_eq!(response.stop, stop, "{dialect}: {body}");
    }
}

/// A refusal whose body was broken off is still a refusal, reported by its status.
///
/// note: the body is only the refusal's explanation. The status is the answer, and it already says
/// whether the refusal is worth waiting out; the transport's complaint about the body says neither.
#[tokio::test]
async fn a_refusal_broken_off_is_reported_by_its_status() {
    let url = broken_off(
        "400 Bad Request",
        "{\"error\":{\"message\":\"no such parameter\"}}",
    )
    .await;
    for (dialect, provider) in dialects(&url) {
        let error = asked(provider).await.expect_err("a refusal");
        assert!(error.contains("400 Bad Request"), "{dialect}: {error}");
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

/// A stream whose every event is an empty `choices` is refused, as the same body is refused whole.
///
/// note: `completion()` refuses a whole body with no choice in it, and the streamed path was
/// checked by neither that nor anything else - so the same events that are an error in one
/// request became a turn that finished, an assistant item of nothing in the context and a
/// session that ended normally. A caller saw a run that worked and a model that said nothing,
/// the notice blamed the stream rather than the answer, and `raw` was the only place the truth
/// was.
///
/// note: the usage event is in the body, and it is there to be read: an endpoint that reports
/// the cost of a turn sends `choices: []` beside it as the last event of every turn, so the
/// refusal has to be about the stream as a whole rather than about the last event.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_stream_of_no_choices_is_not_an_answer() {
    let url = server(
        "200 OK",
        "Content-Type: text/event-stream\r\n",
        "data: {\"id\":\"gen\",\"choices\":[]}\n\n\
         data: {\"id\":\"gen\",\"choices\":[],\"usage\":{\"prompt_tokens\":9,\
         \"completion_tokens\":0,\"total_tokens\":9}}\n\n",
        Arc::new(AtomicUsize::new(0)),
    )
    .await;

    let error = asked(Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "m",
        url,
        "no key needed",
    )))
    .await
    .expect_err("the array and no answer is not an answer");
    assert!(
        error.contains("not a completion"),
        "and it is refused in the words the whole-answer path uses: {error}"
    );
    assert!(error.contains("choices"), "saying what came back: {error}");
}

/// A body the endpoint sent compressed is named as such rather than quoted.
///
/// note: the client asks for nothing it was not given, so what arrives against that ask is
/// bytes rather than words, and the first three hundred of them are a third of a header with a
/// replacement character wherever a byte was not UTF-8. Quoted into the conversation, the session
/// log and any file a user is invited to send on, that is not an account of anything - and
/// nothing on the screen told apart an endpoint that compressed its answer from one that sent
/// nonsense, which is the reading a person takes away.
#[tokio::test]
async fn a_body_the_client_cannot_read_is_named_rather_than_quoted() {
    // a body that is not the text: the first bytes of a gzip stream
    const GZIPPED: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xab, 0x4e, 0x4c,
    ];

    for (dialect, provider) in dialects(&encoded(GZIPPED).await) {
        let error = asked(provider).await.expect_err("bytes are not an answer");
        assert!(
            error.contains("gzip"),
            "{dialect}: the encoding is the diagnosis a reader needs: {error}"
        );
        assert!(
            !error.contains('\u{fffd}'),
            "{dialect}: and no third of a header is quoted into the log: {error}"
        );
    }
}

/// Answers every request with `body` and says it is `gzip`, which is what an intermediary does.
async fn encoded(body: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut discard = [0u8; 16384];
                let _ = socket.read(&mut discard).await;
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                             Content-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.write_all(body).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    format!("http://{address}")
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

/// And a whole body with no choice in it is not an answer, but the stream having carried none.
///
/// note: `choices: []` is the envelope of a completion and nothing in it, so read as the answer it
/// would be an empty turn - one the model never wrote, with nothing to say the server sent none.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_whole_body_with_no_choice_in_it_is_not_an_answer() {
    let requests = Arc::new(AtomicUsize::new(0));
    let url = server(
        "200 OK",
        "Content-Type: application/json\r\n",
        "{\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":0}}",
        requests,
    )
    .await;

    let refused = asked(Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "m",
        url,
        "no key needed",
    )))
    .await
    .expect_err("no answer");
    assert!(
        refused.to_string().contains("the stream carried no data"),
        "{refused}"
    );
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

    // and one event on its own is a list of one
    let url = server(
        "200 OK",
        "Content-Type: application/json\r\n",
        "{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"alone\"}]},\"finishReason\":\"STOP\"}]}",
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let response = asked(Arc::new(nachalnik_providers::Gemini::new(
        "m",
        url,
        "no key needed",
    )))
    .await
    .expect("an answer");
    assert_eq!(said(&response), "alone");
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

/// A busy server that asks to be left for a minute is waited out, and once the tries run out is not
/// said to have asked for longer than this waits.
///
/// note: only a `Retry-After` longer than a minute is refused at once, and a minute is what a
/// per-minute limit asks for. On a paused clock, so that the minutes are not sat through.
#[tokio::test(start_paused = true)]
async fn a_refusal_that_asks_for_a_minute_is_waited_out() {
    for (dialect, _) in dialects("http://127.0.0.1:1") {
        let requests = Arc::new(AtomicUsize::new(0));
        let url = server(
            "429 Too Many Requests",
            "Retry-After: 60\r\nContent-Type: application/json\r\n",
            "{\"error\":{\"code\":429,\"message\":\"Resource has been exhausted.\"}}",
            requests.clone(),
        )
        .await;
        let (_, provider) = dialects(&url)
            .into_iter()
            .find(|(d, _)| *d == dialect)
            .expect("built above");

        let error = asked(provider).await.expect_err("busy every time");
        assert!(
            error.contains("Resource has been exhausted"),
            "{dialect}: {error}"
        );
        assert!(!error.contains("longer than"), "{dialect}: {error}");
        assert_eq!(requests.load(Ordering::SeqCst), 4, "{dialect}");
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

/// Answers each request with the next of `bodies` as a 200 stream, the last one for ever after,
/// and counts the requests.
async fn in_turn(bodies: Vec<&'static str>, requests: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let n = requests.fetch_add(1, Ordering::SeqCst);
            let body = bodies[n.min(bodies.len() - 1)];
            let mut discard = [0u8; 16384];
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
        }
    });

    format!("http://{address}")
}

/// A refusal that arrives where the stream should have - its first event, or the whole of a body
/// that was not one - is waited out, as the same refusal as a status is; one that arrives after
/// the answer has started is not, and what arrived before it is kept.
///
/// note: OpenRouter sends its `200` before the model has produced anything, so a rate limit it
/// meets after that - once its own failover has run out - can only arrive inside the stream, and
/// the event below is the shape its documentation gives. It ended the turn as a provider failure,
/// where the same `429` as a status was waited out. After the answer has started, something has
/// been handed on, and a second attempt would say it again; ending the turn as a failure there
/// threw away what had been generated and billed.
#[tokio::test]
async fn a_refusal_as_the_first_event_of_a_stream_is_waited_out() {
    const LIMITED: &str = "data: {\"error\":{\"code\":429,\"message\":\"Rate limit exceeded\",\
         \"metadata\":{\"error_type\":\"rate_limit_exceeded\"}},\
         \"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n";
    const REFUSED: &str = "data: {\"error\":{\"code\":400,\"message\":\"no such parameter\"}}\n\n";
    // the same refusal as the whole of a body that was never a stream
    const WHOLE: &str = "{\"error\":{\"code\":429,\"message\":\"Rate limit exceeded\"}}";

    for (dialect, answer, partial) in [
        (
            "openai",
            "data: {\"choices\":[{\"delta\":{\"content\":\"here\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"he\"}}]}\n\n\
             data: {\"error\":{\"code\":429,\"message\":\"Rate limit exceeded\"}}\n\n",
        ),
        (
            "gemini",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"here\"}]},\
             \"finishReason\":\"STOP\"}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"he\"}]}}]}\n\n\
             data: {\"error\":{\"code\":429,\"message\":\"Rate limit exceeded\"}}\n\n",
        ),
    ] {
        let provider = |url: &str| dialects(url).into_iter().find(|(d, _)| *d == dialect);

        let requests = Arc::new(AtomicUsize::new(0));
        let url = in_turn(vec![LIMITED, answer], requests.clone()).await;
        let Some((_, waited)) = provider(&url) else {
            continue;
        };
        let response = asked(waited).await.expect("the second attempt is answered");
        assert_eq!(said(&response), "here", "{dialect}");
        assert_eq!(requests.load(Ordering::SeqCst), 2, "{dialect}");

        let requests = Arc::new(AtomicUsize::new(0));
        let url = in_turn(vec![WHOLE, answer], requests.clone()).await;
        let (_, whole) = provider(&url).expect("built above");
        let response = asked(whole).await.expect("the second attempt is answered");
        assert_eq!(
            said(&response),
            "here",
            "{dialect}: a refusal as a whole body"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2, "{dialect}");

        let requests = Arc::new(AtomicUsize::new(0));
        let url = in_turn(vec![partial, answer], requests.clone()).await;
        let (_, started) = provider(&url).expect("built above");
        let response = asked(started)
            .await
            .expect("what arrived before a failure mid-answer is kept");
        assert_eq!(said(&response), "he", "{dialect}");
        assert_eq!(
            response.stop,
            StopReason::Other("cut off".to_owned()),
            "{dialect}"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1, "{dialect}: sent once");

        let requests = Arc::new(AtomicUsize::new(0));
        let url = in_turn(vec![REFUSED, answer], requests.clone()).await;
        let (_, refused) = provider(&url).expect("built above");
        let error = asked(refused)
            .await
            .expect_err("a refusal that does not pass");
        assert!(error.contains("no such parameter"), "{dialect}: {error}");
        assert_eq!(requests.load(Ordering::SeqCst), 1, "{dialect}: sent once");
    }
}

/// Answers every request with a `429` that names no wait, and notes when each arrived.
async fn busy(arrived: Arc<std::sync::Mutex<Vec<tokio::time::Instant>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            arrived.lock().unwrap().push(tokio::time::Instant::now());
            let mut discard = [0u8; 16384];
            let _ = socket.read(&mut discard).await;
            let body = "{\"error\":{\"code\":429,\"message\":\"slow down\"}}";
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\n\r\n{body}",
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

/// A busy server that names no wait is left alone for longer each time before it is asked again.
///
/// note: asked again at once, a server that has just said it has too many requests gets another.
/// On a paused clock, so that the doublings are not sat through.
#[tokio::test(start_paused = true)]
async fn a_refusal_that_names_no_wait_is_waited_out_for_longer_each_time() {
    for (dialect, _) in dialects("http://127.0.0.1:1") {
        let arrived = Arc::new(std::sync::Mutex::new(Vec::new()));
        let url = busy(arrived.clone()).await;
        let (_, provider) = dialects(&url)
            .into_iter()
            .find(|(d, _)| *d == dialect)
            .expect("built above");

        asked(provider).await.expect_err("busy every time");
        let arrived = arrived.lock().unwrap().clone();
        let waits: Vec<_> = arrived.windows(2).map(|two| two[1] - two[0]).collect();
        assert!(
            waits.first().is_some_and(|wait| !wait.is_zero()),
            "{dialect}: {waits:?}"
        );
        assert!(
            waits.windows(2).all(|two| two[1] > two[0]),
            "{dialect}: {waits:?}"
        );
    }
}

/// A 404 says which address was asked, since a wrong address is what it is the answer to.
///
/// note: a server with no route at the path answers in its own words, and those are usually just
/// "Not Found" - which, for a base URL missing its `/v1`, was the whole of what a session was told.
#[tokio::test]
async fn a_request_that_finds_nothing_there_names_where_it_asked() {
    let requests = Arc::new(AtomicUsize::new(0));
    let url = server("404 Not Found", "", "Not Found", requests).await;

    for (dialect, provider) in dialects(&url) {
        let error = asked(provider).await.expect_err("nothing there");
        assert!(error.contains("404 Not Found"), "{dialect}: {error}");
        assert!(
            error.contains(&format!("asked at {url}/")),
            "{dialect}: {error}"
        );
    }
}

/// A request that never reached a server says why, not only that it failed.
///
/// note: the transport's own line is its category and the URL, and the reason - nothing listening,
/// an address that is not one - is further down its chain, where a recorded error never looks.
#[tokio::test]
async fn a_request_that_reached_nobody_says_why() {
    // note: port 1, which no test can be listening on: it takes privileges to bind, so it is
    // refused rather than filtered, and nothing can take it in between. A port bound here and let
    // go was free for whichever test bound one next, and with the suites run side by side one did
    // and answered; one held bound without listening is refused by Linux and never answered by
    // macOS, which makes it a timeout
    let nobody = "http://127.0.0.1:1";

    for (address, why) in [
        (nobody, "refused"),
        ("not-a-url", "relative URL without a base"),
    ] {
        for (dialect, provider) in dialects(address) {
            let error = asked(provider).await.expect_err("nobody there");
            assert!(error.contains(why), "{dialect}, {address}: {error}");
        }
    }
}
