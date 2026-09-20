//! A session in a browser, from one command.
//!
//! ```console
//! $ export KAMCHATKA_API_KEY=sk-or-...
//! $ cargo run --example phone
//! · a session of its own at tcp:127.0.0.1:7878
//! · a browser reaches tcp:127.0.0.1:7878 at http://127.0.0.1:8080/
//! ```
//!
//! ```console
//! $ cargo run --example phone -- 0.0.0.0:8080 qwen/qwen3-coder
//! ```
//!
//! What `gateway.rs` is missing is a session: it relays to one somebody else started, which is the
//! honest shape for a relay and two commands for a person. This is the two in one process - a
//! session wired the way `main.rs` wires one, a socket in front of it, and the same relay serving
//! the same page.
//!
//! note: the relay is `relay/mod.rs` and is shared rather than copied, which is the whole reason
//! this is a second example instead of a flag on the first. `gateway.rs` says what a relay is for
//! and does nothing else; this says what it is like to have one, and does not restate a word of it.
//!
//! note: it draws nothing, and that is not a limitation of the example. `kamchatka --serve` in a
//! terminal is a session with a screen *and* a socket, which is the better way to have both; what
//! this is for is the case with nobody at the machine - a session on a box somewhere, reached from
//! a phone.
//!
//! note: **no authentication and no encryption**, exactly as in `gateway.rs`, and the same sentence
//! applies with more force because this one starts the session too: whatever reaches the page runs
//! the `shell` tool as whoever ran this. The listen address is asked for outright rather than
//! defaulted to a wildcard, and a non-loopback one says what it means.

use std::sync::Arc;

use kamchatka::{
    endpoint, remote,
    wiring::{Setup, Wired},
};
use nachalnik_providers::Dialect;

mod relay;

/// Where the session listens, which is nobody's business but this process's.
///
/// note: loopback, always, and it is not a parameter. The session speaks a protocol with no
/// authentication in it, so the boundary is where it listens - and the thing anybody else reaches
/// here is the *page*, which is the one somebody chose to expose. `Server::bind` refuses anything
/// else anyway; this is the same decision said where a reader meets it.
///
/// note: port zero, so the kernel picks one nothing else is using and `Server::address` says which.
/// A fixed one is a second copy of this process failing to start because the first is still
/// running - or, worse, because a `kamchatka --serve` is - and nothing here needs the number to be
/// anything in particular: the relay is handed it.
const SESSION: &str = "tcp:127.0.0.1:0";

#[tokio::main]
async fn main() -> Result<(), String> {
    // this binary is its own confiner, the way the program is: the shell tool re-executes it with
    // `--confine-and-run` and it restricts itself before exec'ing the command. Without it the child
    // reads that flag as the address to listen on and starts a second session of its own, which is
    // an example that hangs on the first thing the model tries to run
    if let Some(code) = kamchatka::sandbox::run_if_asked() {
        std::process::exit(code);
    }

    let mut args = std::env::args().skip(1);
    let listen = args.next().unwrap_or_else(|| "127.0.0.1:8080".to_owned());
    let model = args
        .next()
        .or_else(|| std::env::var("KAMCHATKA_MODEL").ok())
        .unwrap_or_else(|| "openai/gpt-4o-mini".to_owned());

    // the same two variables the program reads, because this is the program's session and a second
    // way to say where the requests go would be a second thing to keep in step
    let provider: Arc<dyn Dialect> = endpoint::connect(&model)
        .await
        .map(|it| it as Arc<dyn Dialect>)
        .map_err(|e| format!("could not reach {model}: {e}"))?;
    let Wired {
        mut app,
        mut events,
        mut finished,
    } = Setup {
        // note: the runtime's own default is a counter that restarts with the process, which is an
        // identity and not a filename - and `/save` from the page files the session under this. The
        // program picks a timestamp for that reason and so does this
        session_name: Some(kamchatka::app::App::session_stamp(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or_default(),
        )),
        ..Default::default()
    }
    .wire(provider)?;

    let mut server = remote::Server::bind(SESSION).await?;
    let at = server.address();
    println!("· a session of its own at {at}");
    remote::opening(&mut app, &at);

    // note: the session is the task and the relay is what this waits on, rather than the other way
    // round. `Server::run` returns when the session ends - a `/quit` from the page - and the relay
    // never returns at all, so a `select!` over the two leaves by the door that has one
    let session = tokio::spawn(async move {
        let outcome = server.run(&mut app, &mut events, &mut finished).await;
        // the socket file, if this ever grows one, goes with `server` here; a port needs nothing
        drop(server);

        outcome
    });

    tokio::select! {
        ended = session => match ended {
            Ok(outcome) => outcome,
            Err(e) => Err(format!("the session stopped: {e}")),
        },
        served = relay::run(&at, &listen) => served,
    }
}
