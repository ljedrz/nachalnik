//! What happens when the model says nothing at all - before the answer, or during it.
//!
//! note: this is the one thing a scripted provider cannot imitate, and it is worth a socket: a
//! provider that only checks the interrupt *between* fragments is stuck for ever when no fragment
//! ever arrives, and the key somebody pressed to stop it does nothing whatever. From the outside
//! it looks identical to a program that is working.

#![cfg(any(feature = "openai", feature = "gemini"))]

use std::{sync::Arc, time::Duration};

use nachalnik::{Config, ContextItem, Kernel};
use tokio::{io::AsyncReadExt, net::TcpListener, sync::oneshot};

/// How long after a server has reached its point the interrupt waits, for the client to reach its
/// own: headers written are not yet headers read.
///
/// note: a margin on top of a signal rather than a sleep in place of one. The signal is what
/// stops an interrupt landing before the request has gone out, where it would be answered by the
/// kernel before the provider was ever asked and the test would pass without testing anything.
#[cfg(feature = "openai")]
const MARGIN: Duration = Duration::from_millis(100);

/// Accepts one request, answers it as a stream, and then holds the socket open saying nothing.
///
/// note: Not a closed connection and not an error - those are already handled. This is the
/// awkward case: a perfectly good response that never continues.
#[cfg(feature = "openai")]
async fn silent_server() -> (String, oneshot::Receiver<()>) {
    use tokio::io::AsyncWriteExt as _;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    let (reached, reaching) = oneshot::channel();

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
        let _ = reached.send(());

        // and now nothing, for longer than any test will wait
        tokio::time::sleep(Duration::from_secs(600)).await;
    });

    (format!("http://{address}"), reaching)
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn a_model_that_says_nothing_at_all_can_still_be_stopped() {
    let kernel = Kernel::new(Config::default());
    let (address, reached) = silent_server().await;
    kernel.set_provider(Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "silent",
        address,
        "no key needed",
    )));
    kernel.push(ContextItem::user("are you there?"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });

    // once the headers have gone back, so that the interrupt lands while the stream is waiting
    // rather than before it starts
    reached.await.expect("the server answered");
    tokio::time::sleep(MARGIN).await;
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

/// Accepts the connection, reads the request, and never answers it.
///
/// note: One step earlier than [`silent_server`], and it was the gap: the headers never arrive,
/// so there is no stream to watch and nothing to check the interrupt between. A request that
/// stalls here used to hold the terminal until the operating system's own timeout felt like
/// noticing, which in the worst measured case was eighteen minutes.
async fn deaf_server() -> String {
    heard_by_a_deaf_server().await.0
}

/// [`deaf_server`], and word once it has heard the request.
async fn heard_by_a_deaf_server() -> (String, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    let (heard, hearing) = oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("the request");
        let mut discard = [0u8; 4096];
        let _ = socket.read(&mut discard).await;
        let _ = heard.send(());

        // the request was heard in full and gets no reply, ever
        tokio::time::sleep(Duration::from_secs(600)).await;
    });

    (format!("http://{address}"), hearing)
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn a_model_that_never_answers_at_all_can_still_be_stopped() {
    let kernel = Kernel::new(Config::default());
    let (address, heard) = heard_by_a_deaf_server().await;
    kernel.set_provider(Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "deaf",
        address,
        "no key needed",
    )));
    kernel.push(ContextItem::user("are you there?"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });

    // once the request has been heard, so that the interrupt lands inside the send rather than
    // before it
    heard.await.expect("the server heard it");
    kernel.interrupt();

    let stopped = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the interrupt should reach a request whose answer has not started")
        .expect("the turn is not a panic");

    stopped.expect("an interrupted request is not a failed one");

    assert!(
        kernel.last_response().is_none_or(|r| r.content.is_none()),
        "a request that was never answered should not have produced any text"
    );
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn the_other_dialect_is_watched_the_same_way() {
    // the two providers in this crate send their requests down different URLs with different
    // headers, and the watching was written into one of them. This one's `send` was a bare `?`:
    // a stall got neither the doubling a busy server gets nor any of the noticing a stream gets
    let kernel = Kernel::new(Config::default());
    let (address, heard) = heard_by_a_deaf_server().await;
    kernel.set_provider(Arc::new(nachalnik_providers::Gemini::new(
        "deaf",
        address,
        "no key needed",
    )));
    kernel.push(ContextItem::user("are you there?"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });

    heard.await.expect("the server heard it");
    kernel.interrupt();

    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the interrupt should reach this dialect too")
        .expect("the turn is not a panic")
        .expect("an interrupted request is not a failed one");
}

/// A question about an endpoint that never answers gives up, in either dialect, rather than
/// holding whatever asked it.
///
/// note: a listing and a probe are not turns - nothing watches them for silence and no interrupt
/// reaches them - and they ran on a client with no timeout, so an endpoint that took the connection
/// and said nothing held a startup, a `/model` or a `/models` for ever. On a paused clock, so that
/// the bound is waited out without its real fifteen seconds; the guard around each is the same
/// clock, and without a bound the guard is what fires.
#[tokio::test(start_paused = true)]
async fn a_question_nobody_answers_gives_up() {
    #[cfg(feature = "openai")]
    use nachalnik::Provider as _;
    #[cfg(feature = "gemini")]
    use nachalnik_providers::Endpoint as _;

    #[cfg(feature = "openai")]
    {
        let asked = nachalnik_providers::OpenAiCompatible::new("deaf", deaf_server().await, "k");
        let listed = tokio::time::timeout(Duration::from_secs(60), asked.models())
            .await
            .expect("the listing waited for ever");
        assert!(listed.is_empty(), "{listed:?}");
        tokio::time::timeout(Duration::from_secs(600), asked.probe())
            .await
            .expect("the probe waited for ever");
        assert_eq!(asked.info().context_limit, None);
    }
    #[cfg(feature = "gemini")]
    {
        let asked = nachalnik_providers::Gemini::new("deaf", deaf_server().await, "k");
        let listed = tokio::time::timeout(Duration::from_secs(60), asked.models())
            .await
            .expect("the listing waited for ever");
        assert!(listed.is_empty(), "{listed:?}");
        tokio::time::timeout(Duration::from_secs(60), asked.probe())
            .await
            .expect("the probe waited for ever");
    }
}

/// Accepts one request and sends the headers of a whole answer, and then nothing of its body.
#[cfg(feature = "openai")]
async fn headers_only() -> (String, oneshot::Receiver<()>) {
    use tokio::io::AsyncWriteExt as _;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    let (reached, reaching) = oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("the request");
        let mut discard = [0u8; 4096];
        let _ = socket.read(&mut discard).await;
        let _ = socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                  Content-Length: 4096\r\n\r\n",
            )
            .await;
        let _ = socket.flush().await;
        let _ = reached.send(());

        tokio::time::sleep(Duration::from_secs(600)).await;
    });

    (format!("http://{address}"), reaching)
}

/// A whole answer whose headers arrived and whose body has not can still be stopped.
///
/// note: the body was read in one `text()`, with nothing watching it - so the wait for an answer
/// that is written before it is sent, which is minutes, could not be interrupted, and a server
/// that stopped after its headers held the turn for as long as the client's own timeout, which
/// the default client does not have.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_whole_answer_that_never_arrives_can_still_be_stopped() {
    let kernel = Kernel::new(Config::default());
    let (address, reached) = headers_only().await;
    kernel.set_provider(Arc::new(
        nachalnik_providers::OpenAiCompatible::new("slow", address, "no key needed")
            .streaming(false),
    ));
    kernel.push(ContextItem::user("are you there?"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });

    reached.await.expect("the server answered");
    tokio::time::sleep(MARGIN).await;
    kernel.interrupt();

    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the interrupt should reach a body that has not arrived")
        .expect("the turn is not a panic")
        .expect("an interrupted request is not a failed one");
}

/// Answers every request with a `429` asking to be left for half a minute, and counts them.
#[cfg(feature = "openai")]
async fn busy_server(
    requests: Arc<std::sync::atomic::AtomicUsize>,
) -> (String, oneshot::Receiver<()>) {
    use tokio::io::AsyncWriteExt as _;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    let (answered, answering) = oneshot::channel();
    let mut answered = Some(answered);

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut discard = [0u8; 4096];
            let _ = socket.read(&mut discard).await;
            let body = r#"{"error":{"message":"slow down","code":429}}"#;
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 30\r\n\
                         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = socket.shutdown().await;
            if let Some(answered) = answered.take() {
                let _ = answered.send(());
            }
        }
    });

    (format!("http://{address}"), answering)
}

/// A stop pressed while the provider waits out a busy server ends the wait, and sends nothing more.
///
/// note: the wait was one sleep for as long as the server asked - thirty seconds here - and the
/// request went out again at the end of it, stopped or not: answered, and paid for, by a turn that
/// somebody had already put down.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_stop_pressed_during_a_backoff_is_not_sent_again() {
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let kernel = Kernel::new(Config::default());
    let (address, answered) = busy_server(requests.clone()).await;
    kernel.set_provider(Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "busy",
        address,
        "no key needed",
    )));
    kernel.push(ContextItem::user("are you there?"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });

    // once the first answer has gone back, and the wait has begun
    answered.await.expect("the server answered");
    tokio::time::sleep(MARGIN).await;
    kernel.interrupt();

    let stopped = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the interrupt should end a backoff")
        .expect("the turn is not a panic");
    stopped.expect("an interrupted request is not a failed one");
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a stopped request was sent again"
    );
}

/// Takes every request, counts it, and never answers any.
#[cfg(feature = "openai")]
async fn counting_deaf_server(requests: Arc<std::sync::atomic::AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");

    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut socket, _)) = listener.accept().await {
            requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut discard = [0u8; 4096];
            let _ = socket.read(&mut discard).await;
            held.push(socket);
        }
    });

    format!("http://{address}")
}

/// A whole answer that has not arrived is not asked for again: its headers come with its last
/// token, so no answer yet is a model still writing one.
///
/// note: it was taken for a busy server and sent again three times, each a whole generation
/// billed, and failed anyway once the last had waited as long as the first. On a paused clock,
/// since what is waited out is `WHOLE_ANSWER`, ten minutes a try.
#[cfg(feature = "openai")]
#[tokio::test(start_paused = true)]
async fn a_whole_answer_still_being_written_is_asked_for_once() {
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(
        nachalnik_providers::OpenAiCompatible::new(
            "thinking",
            counting_deaf_server(requests.clone()).await,
            "no key needed",
        )
        .streaming(false),
    ));
    kernel.push(ContextItem::user("think hard"));

    let failed = tokio::time::timeout(Duration::from_secs(3600), kernel.turn())
        .await
        .expect("it gave up");
    assert!(failed.is_err(), "an answer that never came is a failure");
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "it was asked for again"
    );
}

/// Sends a stream's headers, and then a byte at a time with no newline for as long as it is read.
#[cfg(feature = "openai")]
async fn trickling_server() -> (String, oneshot::Receiver<()>) {
    use tokio::io::AsyncWriteExt as _;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("its own address");
    let (reached, reaching) = oneshot::channel();
    let mut reached = Some(reached);

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
        while socket.write_all(b"1\r\nx\r\n").await.is_ok() {
            let _ = socket.flush().await;
            if let Some(reached) = reached.take() {
                let _ = reached.send(());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    });

    (format!("http://{address}"), reaching)
}

/// A stream that trickles without ever ending a line can still be stopped.
///
/// note: the interrupt was asked in the quiet and between lines, and this is neither: every byte
/// resets the silence watch and none of them ends a line, so escape did nothing for as long as the
/// server went on trickling.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_stream_that_never_ends_a_line_can_still_be_stopped() {
    let kernel = Kernel::new(Config::default());
    let (address, reached) = trickling_server().await;
    kernel.set_provider(Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "trickle",
        address,
        "no key needed",
    )));
    kernel.push(ContextItem::user("are you there?"));

    let running = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.turn().await }
    });
    reached.await.expect("the server began trickling");
    tokio::time::sleep(MARGIN).await;
    kernel.interrupt();

    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the interrupt should reach a stream that trickles")
        .expect("the turn is not a panic")
        .expect("an interrupted request is not a failed one");
}

/// Sends a stream's headers, one event if asked for, and then more than this crate reads of one
/// line without ending it.
#[cfg(feature = "openai")]
async fn endless_line(first: bool) -> String {
    use tokio::io::AsyncWriteExt as _;

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
        if first {
            let event = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n";
            let framed = [
                format!("{:x}\r\n", event.len()).into_bytes(),
                event.to_vec(),
                b"\r\n".to_vec(),
            ]
            .concat();
            let _ = socket.write_all(&framed).await;
        }
        let chunk = vec![b'x'; 1 << 20];
        let framed = [
            format!("{:x}\r\n", chunk.len()).into_bytes(),
            chunk,
            b"\r\n".to_vec(),
        ]
        .concat();
        // a little past the ceiling and then held open, so that a reader with no ceiling waits
        // rather than reading until the machine runs out
        for _ in 0..80 {
            if socket.write_all(&framed).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_secs(600)).await;
    });

    format!("http://{address}")
}

/// A line that never ends is not held for ever: past the ceiling, a stream that has said nothing
/// is refused with a sentence, and one that has keeps what it said, as a stream cut off does.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_line_that_never_ends_is_not_read_for_ever() {
    for first in [false, true] {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(Arc::new(nachalnik_providers::OpenAiCompatible::new(
            "endless",
            endless_line(first).await,
            "no key needed",
        )));
        kernel.push(ContextItem::user("are you there?"));

        let ended = tokio::time::timeout(Duration::from_secs(60), kernel.turn())
            .await
            .unwrap_or_else(|_| panic!("it read for ever, with an event first: {first}"));
        match first {
            false => {
                let said = ended
                    .expect_err("nothing but a line is a failure")
                    .to_string();
                assert!(said.contains("MiB"), "{said}");
            }
            true => {
                ended.expect("what arrived before the line is kept");
                let kept = kernel.last_response().expect("a response");
                assert_eq!(
                    kept.content
                        .as_ref()
                        .map(|it| it.to_text().into_owned())
                        .as_deref(),
                    Some("hi")
                );
            }
        }
    }
}
