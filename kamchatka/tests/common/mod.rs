//! What the test suites share: a place to put files that is not `/tmp`, the tools a session is
//! given, and the two things it takes to drive the *program* rather than the library - the binary,
//! and something for it to talk to.
//!
//! note: `dead_code` is allowed, and it has to be. A `mod common;` is compiled afresh into every
//! suite that declares it, so anything here that one suite does not call is unused *in that
//! binary* - which is a warning in every other suite for a helper two suites share, under a
//! `RUSTFLAGS` that makes a warning a failure.

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
///
/// note: under nextest, in a directory of the test's own. There every test is a process, and the
/// processes of every suite run side by side - so a directory a whole suite shares, like
/// [`nowhere`], was emptied by each test that started while the others were standing in it, and
/// their spawns failed for want of a working directory. The directory is named by a hash of the
/// suite and the test nextest hands each process, which keeps it the same from run to run, rather
/// than by the names themselves: a socket is made in some of these, a socket's path is at most 107
/// bytes, and a test's name can be most of that on its own.
///
/// note: what an earlier run left is opened up before it is removed, and a removal that fails is a
/// panic that names the directory. A test that takes a directory's permissions away puts them back
/// after the call, but a panic or a timeout in between leaves them gone; removing that directory
/// then fails, and with the error ignored every later run of the test failed at its first line, in
/// a way that said nothing about why.
pub fn scratch(name: &str) -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    if let (Ok(suite), Ok(test)) = (
        std::env::var("NEXTEST_BINARY_ID"),
        std::env::var("NEXTEST_TEST_NAME"),
    ) {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (suite, test).hash(&mut hasher);
        dir = dir.join(format!("n{:08x}", hasher.finish() as u32));
    }
    let dir = dir.join(name);
    open_up(&dir);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            panic!(
                "{} is left from an earlier run and cannot be removed: {e}",
                dir.display()
            )
        }
        _ => {}
    }
    std::fs::create_dir_all(&dir).expect("a directory to work in");

    dir
}

/// Gives the owner back read, write and search on `path` and every directory under it, so that it
/// can be removed. Symbolic links are not followed.
fn open_up(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700));
    }
    for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
        open_up(&entry.path());
    }
}

/// A scratch directory with one file in it, `inside.txt`, for a test that reads, writes or confines
/// a workspace of its own.
pub fn workdir(name: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::write(dir.join("inside.txt"), "hello").expect("a file in it");

    dir
}

/// `fs` and `shell` as a session gets them, under a `Careful` of their own: held to `dir`, or, as
/// under `--no-sandbox`, not held at all, with `limits` on what `fs` hands back.
pub fn builtin(
    dir: &std::path::Path,
    confined: bool,
    limits: kamchatka::tools::Limits,
) -> Vec<std::sync::Arc<dyn nachalnik::Tool>> {
    builtin_under(
        dir,
        confined,
        limits,
        std::sync::Arc::new(kamchatka::tools::Careful::new()),
    )
}

/// [`builtin`], under `policy` rather than a fresh one.
pub fn builtin_under(
    dir: &std::path::Path,
    confined: bool,
    limits: kamchatka::tools::Limits,
    policy: std::sync::Arc<kamchatka::tools::Careful>,
) -> Vec<std::sync::Arc<dyn nachalnik::Tool>> {
    use kamchatka::{
        sandbox::Reach,
        tools::{Limits, Shell},
    };

    kamchatka::tools::builtin(
        Shell {
            workdir: dir.to_path_buf(),
            writable: Vec::new(),
            readable: Vec::new(),
            shell_readable: kamchatka::sandbox::SYSTEM.iter().map(Into::into).collect(),
            devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
            policy,
            confiner: None,
            limits: Limits::default(),
            stragglers: Default::default(),
            layout: kamchatka::tools::Layout::Wrapped,
        },
        Reach {
            workdir: dir.to_path_buf(),
            writable: Vec::new(),
            readable: Vec::new(),
            shell_readable: kamchatka::sandbox::SYSTEM.iter().map(Into::into).collect(),
            confined,
        },
        limits,
    )
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
pub async fn endpoint(answers: Vec<String>) -> String {
    hearing(answers).await.0
}

/// [`endpoint`], and the body of every request for an answer it was sent, in order.
///
/// note: a request is read whole, to the length its headers give, since the body can arrive in a
/// read of its own after them.
pub async fn hearing(
    answers: Vec<String>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    let at = listener.local_addr().expect("its address");
    let answers = Arc::new(answers);
    let nth = Arc::new(AtomicUsize::new(0));
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (answers, nth, log) = (answers.clone(), nth.clone(), log.clone());
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

                let mut buf = Vec::new();
                let mut chunk = vec![0u8; 65536];
                while let Ok(read) = socket.read(&mut chunk).await
                    && read > 0
                {
                    buf.extend_from_slice(&chunk[..read]);
                    if whole(&buf) {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                if !head.contains("/models")
                    && let Some((_, body)) = head.split_once("\r\n\r\n")
                {
                    log.lock().expect("not poisoned").push(body.to_owned());
                }

                let (kind, body) = match head.contains("/models") {
                    true => (
                        "application/json",
                        r#"{"data":[{"id":"nothing","context_length":128000}]}"#.to_owned(),
                    ),
                    false => match answers.get(nth.fetch_add(1, SeqCst)) {
                        Some(whole) if whole.starts_with("HTTP/") => {
                            let _ = socket.write_all(whole.as_bytes()).await;
                            let _ = socket.shutdown().await;
                            return;
                        }
                        Some(sse) => ("text/event-stream", format!("{sse}\n\ndata: [DONE]\n\n")),
                        // a request nobody wrote an answer for is held open rather than refused,
                        // which is a model that has gone quiet - and the one thing a test must not
                        // do here is end the turn by accident
                        None => return std::future::pending().await,
                    },
                };
                // `Connection: close` because the socket is shut after one answer. Without it the
                // client keeps the connection for the next request, and a request that follows a
                // tool that finishes at once goes out on it before the close arrives, and fails
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (format!("http://{at}/v1"), heard)
}

/// Whether `request` holds its headers and as many bytes of body as they say it has.
fn whole(request: &[u8]) -> bool {
    let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let length = String::from_utf8_lossy(&request[..end])
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    request.len() - (end + 4) >= length
}

/// A refusal for [`endpoint`] to hand out in place of an answer: busy, and asking for a second's
/// wait, which the provider takes and then asks again.
///
/// note: an answer that begins as a response is sent as it is, which is what makes this one of
/// the bodies rather than a second endpoint.
pub const BUSY: &str =
    "HTTP/1.1 502 Bad Gateway\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// One streamed answer for [`endpoint`], with what it cost on the end of it.
pub fn answer(text: &str) -> String {
    use serde_json::json;

    format!(
        "data: {}\n\ndata: {}",
        json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant", "content": text},
               "finish_reason": null}]}),
        json!({"id": "1", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
               "usage": {"prompt_tokens": 700, "completion_tokens": 500, "total_tokens": 1200}})
    )
}

/// The binary under test.
///
/// note: what cargo sets for exactly this, rather than the test binary's own path with `deps`
/// taken off it. It knows the extension, it knows where the profile put the binary, and it cannot
/// be wrong about either.
pub fn program() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_kamchatka"))
}

/// A directory with nothing in it, for the program to stand in.
///
/// note: the suites run from the crate root, which is where the shipped `kamchatka.json` is, and a
/// settings file in the working directory is read only once somebody at a terminal says so. A test
/// that means a directory of its own sets one over this.
pub fn nowhere() -> &'static std::path::Path {
    static NOWHERE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    NOWHERE.get_or_init(|| scratch("nowhere"))
}

/// The binary, to be run standing in [`nowhere`].
pub fn command() -> std::process::Command {
    let mut command = std::process::Command::new(program());
    command.current_dir(nowhere());

    command
}

/// Has this thread, and every process it starts, refused `landlock_create_ruleset` with `ENOSYS`,
/// as a kernel built without Landlock refuses it, and every other system call let through. It is
/// how a test sees what is said on a kernel with no Landlock while running on one that has it.
///
/// note: a seccomp filter cannot be taken off, so this is for a process of its own: a child the
/// test runs, or the program between `fork` and `exec` in `CommandExt::pre_exec`, where nothing
/// may allocate and nothing here does. The call is matched by its native number, which is the
/// only one the code under test makes.
pub fn without_landlock() -> std::io::Result<()> {
    let op = |code: u32, jf: u8, k: u32| libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf,
        k,
    };
    let mut filter = [
        // the call's number
        op(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0),
        op(
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
            1,
            libc::SYS_landlock_create_ruleset as u32,
        ),
        op(
            libc::BPF_RET | libc::BPF_K,
            0,
            libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
        ),
        op(libc::BPF_RET | libc::BPF_K, 0, libc::SECCOMP_RET_ALLOW),
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: two `prctl` calls, the second given a filter that outlives it on this stack.
    // `no_new_privs` is what lets a process without privileges install a filter at all
    let installed = unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
            && libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &raw const program,
            ) == 0
    };
    match installed {
        true => Ok(()),
        false => Err(std::io::Error::last_os_error()),
    }
}
