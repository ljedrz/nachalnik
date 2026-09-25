//! A provider read from one thread while it answers on another.
//!
//! note: what a screen does to one. The model's name and its context limit are drawn every frame,
//! on the thread that draws, while the turn's request is built on another, and `info` and
//! `respond` read the same two locks. Taken in one order by the first and in the other by the
//! second, the two threads waited on each other for ever at the start of a request, which froze
//! the screen of the program built on this. The reader here asks as fast as it can rather than
//! once a frame, so the window a frame hits now and then is hit on most requests.
//!
//! note: the server refuses everything, because the locks are taken before anything is sent and a
//! 400 is not retried - so each request costs a round trip and nothing more.

#![cfg(any(feature = "gemini", feature = "openai"))]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use nachalnik::{Config, ContextItem, Kernel, Provider};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// How many requests each dialect makes while it is being read.
const REQUESTS: usize = 200;

/// Refuses every request at once, for as long as anybody keeps asking, and counts them.
async fn refusing() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let at = listener.local_addr().expect("its address");
    let answered = Arc::new(AtomicUsize::new(0));
    let counted = answered.clone();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"error":{"message":"no"}}"#;
            let mut discard = [0u8; 4096];
            let _ = socket.read(&mut discard).await;
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = socket.shutdown().await;
        }
    });

    (format!("http://{at}"), answered)
}

/// Makes request after request through a kernel while another thread reads its model, and says
/// whether they all came back.
///
/// note: on a thread and a runtime of their own, waited on from here. A thread stuck on a lock
/// cannot be stopped, and a runtime that owns one cannot be dropped, so a deadlock inside the
/// test's own runtime is a test that hangs rather than one that fails. Out here it is a thread
/// left where it is, and an answer that never arrives.
fn asked_while_read(provider: impl FnOnce(String) -> Arc<dyn Provider> + Send + 'static) -> bool {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async move {
            let (at, answered) = refusing().await;
            let kernel = Kernel::new(Config::default());
            kernel.set_provider(provider(at));

            let reading = Arc::new(AtomicBool::new(true));
            let reader = std::thread::spawn({
                let (kernel, reading) = (kernel.clone(), reading.clone());
                move || {
                    while reading.load(Ordering::Relaxed) {
                        std::hint::black_box(kernel.model_info());
                    }
                }
            });
            for _ in 0..REQUESTS {
                kernel.push(ContextItem::user("go"));
                let _ = kernel.step().await;
            }
            reading.store(false, Ordering::Relaxed);
            reader.join().expect("the reader panicked");

            assert!(
                answered.load(Ordering::SeqCst) >= REQUESTS,
                "the requests were not made"
            );
            let _ = done.send(());
        });
    });

    finished.recv_timeout(Duration::from_secs(30)).is_ok()
}

#[cfg(feature = "openai")]
#[test]
fn the_conventional_dialect_answers_while_it_is_read() {
    assert!(
        asked_while_read(|at| Arc::new(
            nachalnik_providers::OpenAiCompatible::new("read", at, "no key needed")
                .with_context_limit(Some(8192))
        )),
        "a request and a reader waited on each other"
    );
}

#[cfg(feature = "gemini")]
#[test]
fn the_native_dialect_answers_while_it_is_read() {
    assert!(
        asked_while_read(|at| Arc::new(
            nachalnik_providers::Gemini::new("read", at, "no key needed")
                .with_context_limit(Some(8192))
        )),
        "a request and a reader waited on each other"
    );
}
