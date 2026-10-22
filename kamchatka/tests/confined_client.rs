//! A command this session confined is not a client of it, and nothing else in this binary is.
//!
//! note: a binary of its own because of how the check works. sandlock connects a confined command
//! to a unix socket from its supervisor, in this process, so while a command runs, a connection
//! this process makes to its own session's socket is taken for the command's. The program reaches
//! its own session without the socket; a test of the remote suite connecting by hand beside this
//! one was refused.

use std::{sync::Arc, time::Duration};

use kamchatka::{remote::Server, wiring::Wired};

mod common;

/// How long anything here is given.
const PATIENCE: Duration = Duration::from_secs(5);

/// A session with no tools and a provider nothing is asked of.
fn wired() -> Wired {
    kamchatka::wiring::Setup {
        tools: Some(Vec::new()),
        compact: None,
        ..Default::default()
    }
    .wire(Arc::new(nachalnik_providers::OpenAiCompatible::new(
        "scripted",
        "http://127.0.0.1:1",
        "",
    )))
    .expect("the wiring failed")
}

/// A command the session confined is not a client: it is hung up on, where the same client run
/// unconfined is answered.
///
/// note: a socket file in the working directory, which a confined command may connect to since it
/// may write there - and sandlock makes that connection from this process, so the session is told
/// it is this process at the other end. A client answers permission questions, so this one would
/// be answering its own. The probe sends a
/// line that is not a message, which a session answers with a failure before it closes; a refused
/// connection closes with nothing.
#[tokio::test]
async fn a_command_the_session_confined_is_not_a_client() {
    use kamchatka::{
        sandbox::{Confinement, available},
        tools::{Careful, Limits, Shell},
    };
    use nachalnik::{OutputSink, Tool, test::call};

    if available().confinement != Confinement::Full {
        eprintln!("skipped: this machine cannot confine a command");
        return;
    }
    let dir = common::workdir("remote-confined-client");
    let socket = dir.join("s");
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired();
    let mut server = Server::bind(&format!("unix:{}", socket.display()))
        .await
        .expect("it listens");
    let loop_ = tokio::spawn(async move {
        let _ = server.run(&mut app, &mut events, &mut finished).await;
    });
    // a session that refuses the connection may have closed it before the line is written, or
    // leave the line unread and close it with a reset rather than an end, and each of those is a
    // hang-up. The connect stays outside, so a command that could not reach the socket at all
    // fails here rather than passing as refused
    let probe = format!(
        r#"python3 -c "
import socket
s = socket.socket(socket.AF_UNIX)
s.connect('{}')
try:
    s.sendall(b'{{\n')
    got = s.recv(4096)
except OSError:
    got = b''
print('answered' if got else 'hung up')
""#,
        socket.display()
    );

    let control = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&probe)
        .output()
        .await
        .expect("sh is here");
    let control = String::from_utf8_lossy(&control.stdout);
    assert!(control.contains("answered"), "{control}");

    let shell = Shell {
        workdir: dir.clone(),
        writable: Vec::new(),
        readable: Vec::new(),
        shell_readable: kamchatka::sandbox::SYSTEM.iter().map(Into::into).collect(),
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        policy: std::sync::Arc::new(Careful::new()),
        confined: true,
        limits: Limits::default(),
        stragglers: Default::default(),
        layout: kamchatka::tools::Layout::Wrapped,
    };
    let said = tokio::time::timeout(
        PATIENCE,
        shell.invoke(
            &call("1", "shell", serde_json::json!({ "cmd": probe })),
            OutputSink::disconnected(),
        ),
    )
    .await
    .expect("the command never answered")
    .expect("the tool answers either way")
    .content
    .to_text()
    .into_owned();
    assert!(said.contains("hung up"), "{said}");
    assert!(!said.contains("answered"), "{said}");

    loop_.abort();
}

/// A client in this process still reaches the session while a confined command runs, which is when
/// the session's socket takes this process for the command.
///
/// note: the web page's relay is such a client. It reaches a session served here without the
/// socket, so a command left running in the background does not shut the page out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_in_this_process_is_let_in_while_a_command_runs() {
    use kamchatka::{
        sandbox::{Confinement, available},
        tools::{Careful, Limits, Shell},
    };
    use nachalnik::{Grant, OutputSink, Tool, test::call};

    if available().confinement != Confinement::Full {
        eprintln!("skipped: this machine cannot confine a command");
        return;
    }
    let dir = common::workdir("remote-local-client");
    let at = format!("unix:{}", dir.join("s").display());
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = wired();
    let mut server = Server::bind(&at).await.expect("it listens");
    let loop_ = tokio::spawn(async move {
        let _ = server.run(&mut app, &mut events, &mut finished).await;
    });

    let shell = Shell {
        workdir: dir.clone(),
        writable: Vec::new(),
        readable: Vec::new(),
        shell_readable: kamchatka::sandbox::SYSTEM.iter().map(Into::into).collect(),
        devices: kamchatka::sandbox::DEVICES.iter().map(Into::into).collect(),
        policy: Arc::new(Careful::new()),
        confined: true,
        limits: Limits::default(),
        stragglers: Default::default(),
        layout: kamchatka::tools::Layout::Wrapped,
    };
    // left running past its call, so its sandbox is alive while the client comes in
    let said = shell
        .invoke(
            &call("1", "shell", serde_json::json!({ "cmd": "sleep 5 &" })),
            OutputSink::disconnected(),
        )
        .await
        .expect("the tool answers")
        .content
        .to_text()
        .into_owned();
    assert!(said.starts_with("exit: 0"), "{said}");

    // the control: the socket takes this process for the command now, and hangs up on it where a
    // client would be answered - so the client below is let in by the other way, and only by it
    let refused = {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let mut raw = tokio::net::UnixStream::connect(dir.join("s"))
            .await
            .expect("the socket is there");
        let _ = raw.write_all(b"{\n").await;
        let mut got = Vec::new();
        let _ = tokio::time::timeout(PATIENCE, raw.read_to_end(&mut got)).await;
        got.is_empty()
    };
    assert!(
        refused,
        "this process was answered on the socket while a command ran"
    );

    let (mut records, mut prose) = (Vec::new(), Vec::new());
    tokio::time::timeout(
        PATIENCE,
        kamchatka::remote::Client::new(Grant::Deny, &mut records, &mut prose)
            .run(&at, "".as_bytes()),
    )
    .await
    .expect("the client never left")
    .expect("the client was not let in");
    let prose = String::from_utf8(prose).expect("the prose is text");
    assert!(prose.contains("record(s)"), "{prose}");

    loop_.abort();
}
