//! What the test suites share: a place to put files that is not `/tmp`, and the two things it
//! takes to drive the *program* rather than the library - the binary, and something for it to talk
//! to.
//!
//! note: `dead_code` is allowed, and it has to be. A `mod common;` is compiled afresh into every
//! suite that declares it, so anything here that one suite does not call is unused *in that
//! binary* - which is seven warnings for a helper two suites share, under a `RUSTFLAGS` that makes
//! a warning a failure.

#![allow(dead_code)]

use std::path::PathBuf;

/// A directory of this test's own, emptied first, under the one cargo hands the test binaries.
///
/// note: `CARGO_TARGET_TMPDIR` rather than `std::env::temp_dir()`, which is what these used to
/// use. Every one of them cleared its directory on the way in rather than on the way out - a test
/// that fails is a test whose leavings you want to look at - and the name carried the process
/// identifier, so a fresh one arrived with every run and none of them ever left. Thirty runs of
/// this suite had put eight hundred and ten directories in `/tmp`, which is a real cost to
/// somebody else's machine and an odd thing for the tidy program to do. Cargo hands integration
/// tests a directory for exactly this, it is under `target/`, and `cargo clean` sweeps it.
///
/// note: no process identifier in the name, now that they are not accumulating. Cargo runs one
/// test binary at a time, so a name only has to be unique within its own suite, and a stable name
/// is one somebody debugging a failure can find - which was the reason for clearing on the way in
/// rather than the way out, and is worth keeping.
pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a directory to work in");

    dir
}

/// An endpoint the program can be pointed at, which answers the model listing and then hands out
/// these bodies, one per request, as a stream.
///
/// note: a socket rather than a scripted provider, because what is under test here is the
/// *program*: it builds its own provider out of two environment variables, in a process of its
/// own, and nothing this test holds can be swapped into that. A listener is the only seam a child
/// process has.
///
/// note: the bodies are SSE because the provider asks for a stream unless told not to, and the
/// point of these tests is the path the program actually takes. `[DONE]` is appended here so that
/// a case reads as what the model said rather than as protocol.
///
/// note: the imports are the function's rather than the file's, because the function is. Everything
/// under this `cfg` is gone on Windows, and a `use` at the top that only this reaches is an unused
/// import there - which under the `RUSTFLAGS` this workspace builds with is a failed build on the
/// one platform nobody here runs.
#[cfg(unix)]
pub async fn endpoint(answers: Vec<String>) -> String {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    let answers = Arc::new(answers);
    let nth = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (answers, nth) = (answers.clone(), nth.clone());
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

                let mut buf = vec![0u8; 65536];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..read]).into_owned();

                let (kind, body) = match head.contains("/models") {
                    true => (
                        "application/json",
                        r#"{"data":[{"id":"nothing","context_length":128000}]}"#.to_owned(),
                    ),
                    false => match answers.get(nth.fetch_add(1, SeqCst)) {
                        Some(sse) => ("text/event-stream", format!("{sse}\n\ndata: [DONE]\n\n")),
                        // a request nobody wrote an answer for is held open rather than refused,
                        // which is a model that has gone quiet - and the one thing a test must not
                        // do here is end the turn by accident
                        None => return std::future::pending().await,
                    },
                };
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });

    format!("http://{at}/v1")
}

/// The binary under test.
///
/// note: what cargo sets for exactly this, rather than the test binary's own path with `deps`
/// taken off it. It knows the extension, it knows where the profile put the binary, and it cannot
/// be wrong about either.
#[cfg(unix)]
pub fn program() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_kamchatka"))
}

/// An example built beside the binary, by name.
///
/// note: cargo sets no variable for an example the way it does for a binary, so this is the one
/// path in here that is derived: a build's examples are in `examples/` under the directory its
/// binary is in. `cargo test -p kamchatka` builds them; a run of one suite alone may not, which is
/// for the caller to say before spawning.
#[cfg(unix)]
pub fn example(name: &str) -> std::path::PathBuf {
    program()
        .parent()
        .expect("the binary is in a directory")
        .join("examples")
        .join(name)
}
