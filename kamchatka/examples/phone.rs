//! A session in a browser, from one command.
//!
//! ```console
//! $ export KAMCHATKA_API_KEY=sk-or-...
//! $ cargo run --example phone -- -m qwen/qwen3-coder
//! ```
//!
//! It prints where its session listens and the address a browser reaches the page at.
//!
//! ```console
//! $ KAMCHATKA_PHONE_LISTEN=0.0.0.0:8080 cargo run --example phone -- \
//!     --advise --allow fs:read "have a look around"
//! ```
//!
//! note: **the program's own arguments**, every one of them, because this assembles the program's
//! own session - see [`kamchatka::args`]. `--advise`, a system instruction, a tool and a path rule
//! all mean here what they mean there. A second, smaller vocabulary in an example is two sets of
//! flags to keep in step and one of them always behind.
//!
//! note: where the *page* listens is the one thing that is not an argument, and it is a variable
//! rather than a positional because the positional is the first message. `--serve` is not it
//! either: that is the session's own socket, which this one holds on loopback and a port nothing
//! else is using, and giving the flag a second meaning here would be the kind of overload a reader
//! has to be told about.
//!
//! What `gateway.rs` is missing is a session: it relays to one somebody else started, which is the
//! honest shape for a relay and two commands for a person. This is the two in one process - a
//! session wired the way `main.rs` wires one, a socket in front of it, and the same relay serving
//! the same page.
//!
//! A session here ends the way the program's does. `/quit` from the page writes it out under the
//! temporary directory and says where, and `/restart` writes it out and wires another from the
//! same arguments behind the same socket, which the page comes back into by itself. Both are
//! [`kamchatka::wiring`]'s, the same calls `main.rs` makes.
//!
//! note: the relay is `relay/mod.rs`, shared rather than copied, which is what lets this be a
//! second example instead of a flag on the first. `gateway.rs` says what a relay is for and does
//! nothing else; this says what it is like to have one, and does not restate a word of it.
//!
//! note: it draws nothing, and that is not a limitation of the example. `kamchatka --serve` in a
//! terminal is a session with a screen *and* a socket, which is the better way to have both; what
//! this is for is the case with nobody at the machine - a session on a box somewhere, reached from
//! a phone.
//!
//! note: **no authentication and no encryption**, exactly as in `gateway.rs`, and the same sentence
//! applies with more force because this one starts the session too: whatever reaches the page runs
//! the `shell` tool as whoever ran this. The page listens on loopback unless
//! `KAMCHATKA_PHONE_LISTEN` says otherwise, and a non-loopback address says what it means.

use kamchatka::{
    app::Speaker,
    args::{Args, Given},
    remote,
    wiring::{self, Wired},
};

mod relay;

/// Where the page listens, for whoever is not standing at this machine.
///
/// note: loopback by default, and a wildcard has to be asked for outright. See the note about
/// authentication at the top: whatever reaches the page runs the `shell` tool as whoever ran this.
const LISTEN: &str = "KAMCHATKA_PHONE_LISTEN";

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
    // hands that flag to the argument parser, which refuses it, and nothing the model tries to run
    // runs
    if let Some(code) = kamchatka::sandbox::run_if_asked() {
        std::process::exit(code);
    }

    let listen = std::env::var(LISTEN).unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    // the program's own, settings file and all, so that every flag means here what it means there
    let Given { args, found, .. } = Args::given().map_err(|e| format!("{e:#}"))?;
    if args.print_config {
        print!("{}", kamchatka::config::SHIPPED);

        return Ok(());
    }
    // note: refused rather than ignored, which is the rule `--connect` follows in the program. This
    // starts a session and puts a socket in front of it, so a second address is not a setting it
    // can honour - and an example that quietly did nothing with one somebody typed would be worse
    // than one that stops
    for (given, flag) in [
        (args.serve.is_some(), "--serve"),
        (args.connect.is_some(), "--connect"),
    ] {
        if given {
            return Err(format!(
                "`{flag}` is not this example's to take: it serves a session of its own on \
                 loopback and puts the page in front of it. {LISTEN} says where the page listens"
            ));
        }
    }

    let provider = args.provider().await.map_err(|e| format!("{e:#}"))?;
    let flagged = kamchatka::wiring::Flagged::of(&*provider);
    let setup = args.setup().map_err(|e| format!("{e:#}"))?;
    setup.check()?;
    #[cfg(feature = "shell-advisor")]
    let setup = args.advised(setup).await.map_err(|e| format!("{e:#}"))?;

    let mut server = remote::Server::bind(SESSION).await?;
    let at = server.address();
    println!("· a session of its own at {at}");

    // note: the session is the task and the relay is what this waits on, rather than the other way
    // round. The task ends with the last session - a `/quit` from the page, since a `/restart` is
    // taken inside it - and the relay returns only where it could not listen, so a `select!` over
    // the two leaves by the session's door
    //
    // note: a loop inside the task, for the reason `main.rs` has one: `/restart` writes this
    // session out and asks for another, built from the arguments this run started with rather
    // than from where the first one had got to. The task owns the `App`, so the loop and the
    // record at the end of it have to be in here with it
    let message = (!args.message.is_empty()).then(|| args.message.join(" "));
    let bound = at.clone();
    let session = tokio::spawn(async move {
        let base = setup.clone();
        let Wired {
            mut app,
            mut events,
            mut finished,
        } = setup.wire(provider.clone())?;

        let mut first = true;
        let outcome = loop {
            remote::opening(&mut app, &bound);
            // the three lines the program says into a session before anything is driving it, for
            // the same reasons it says them: where a setting nobody typed came from, that there
            // is no model yet, and the message that was handed in on the command line. Every one
            // of them reaches the page, because the conversation is what a projection carries
            if let Some(path) = &found {
                app.say(
                    Speaker::Note,
                    format!("settings read from {}", path.display()),
                );
            }
            if app.kernel.model_info().is_none() {
                app.say(
                    Speaker::Note,
                    format!(
                        "no model yet: `/model ID` picks one, and `/models` lists what {} serves",
                        app.provider.host()
                    ),
                );
            }
            // the first session only: a message asked again on every restart would make
            // `/restart` a way of putting the same question to the model for ever
            if first && let Some(message) = &message {
                app.ask(message);
                app.start_turn();
            }
            first = false;

            let outcome = server.run(&mut app, &mut events, &mut finished).await;
            if !app.restart {
                break outcome;
            }

            flagged.restore(&*provider).await;
            let (wired, said) = base.relaunch(&app, provider.clone())?;
            let Wired {
                app: fresh,
                events: replaced,
                finished: reported,
            } = wired;
            (app, events, finished) = (fresh, replaced, reported);
            // the first thing the new session says, because it is the only place the old one's
            // name and the file it went to are still written down
            app.say(Speaker::Note, said);
        };

        // the last session of the run, written out the way every one before it was.
        // `Server::run` has ended it already, so the record's last line is `session.finished`
        println!(
            "{} · {} events recorded",
            app.kernel.session_name(),
            app.kernel.history().len()
        );
        if base.record {
            match wiring::record(&app) {
                Ok(written) => println!(
                    "{written}\n`kamchatka -r {}` carries on from it",
                    written.state
                ),
                Err(e) => eprintln!("the session was not written: {e}"),
            }
        }

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
