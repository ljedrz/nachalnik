//! The program: the arguments, the wiring, and the loop that draws and waits.
//!
//! ```text
//! export KAMCHATKA_API_KEY=sk-or-...
//! kamchatka -m qwen/qwen3-coder
//! ```
//!
//! Everything it is made of lives in the library beside it; see the crate documentation for what
//! is on the screen and why.

#![deny(unsafe_code)]

use std::{
    io::{IsTerminal as _, stdout},
    path::Path,
};

use anyhow::{Context as _, Result};
use clap::CommandFactory as _;
#[cfg(feature = "tui")]
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste, Event as TerminalEvent, EventStream},
    execute,
};

use kamchatka::{
    app::{App, Speaker},
    args::{Args, Given},
    headless, remote, sandbox,
    wiring::Wired,
};
// the drawing loop's own: the channel payload it has to name in a signature, and the screen
#[cfg(feature = "tui")]
use kamchatka::{app::Outcome, ui};
#[cfg(feature = "tui")]
use nachalnik::Event;

/// How often the screen is redrawn when nothing at all is happening.
#[cfg(feature = "tui")]
const TICK: std::time::Duration = std::time::Duration::from_millis(120);

fn main() -> Result<()> {
    // before anything else, and before a runtime exists: this is the mode the `shell` tool
    // re-executes this program in, and its whole job is to confine itself and run one command.
    // Landlock restricts the calling thread, so the one shape that needs no thought about which
    // thread that was is a program with only one
    if let Some(code) = sandbox::run_if_asked() {
        std::process::exit(code);
    }

    // and still before a runtime exists, for a reason of the same shape as the one above. Working
    // out the local time of day means asking libc, which reads the process environment, and a
    // thread setting a variable while another reads one is undefined behaviour - so `time`
    // refuses to answer once a program is threaded. Here there is nobody to race, and the answer
    // is good for the rest of the run: the trace pane needs an offset, not a calendar.
    kamchatka::app::when::note_local_offset(
        time::UtcOffset::current_local_offset()
            .ok()
            .map(time::UtcOffset::whole_seconds),
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(session());
    // note: the runtime is let go of rather than dropped, and without this the program *hangs*
    // after every early stop there is. `tokio::io::stdin` reads on a blocking thread; a blocking
    // read on a pipe nobody is writing to does not return; and dropping a runtime waits for its
    // blocking threads. So a headless run that ended by `ctrl+c`, a deadline, a spend ceiling or
    // `/quit` - anything but the input closing - wrote its session out, printed where it had gone,
    // and then sat there until somebody killed it.
    //
    // note: it is safe here precisely because it is the last statement. The session has been
    // written, the MCP servers were dropped with the scope that held them - which is what kills
    // their child processes - and what is being abandoned is a thread waiting on a pipe.
    runtime.shutdown_background();

    outcome
}

/// Whether this run is driven by lines rather than by keys.
///
/// note: three ways into it and the third is the one that was missing. Somebody says so; or
/// stdout is not a terminal, so there is nowhere to draw; or **this build has no screen in it**,
/// which was written as a notice and not as a decision. `--no-default-features` in a terminal with
/// no flag therefore printed `built without the tui feature, so this is a headless run` and then
/// walked into the `unreachable!` below, which is the crate's own headline configuration failing
/// at the first thing anybody would do with it.
///
/// note: no test caught it and none could have, as the suite is written: every test of this
/// binary pipes its stdout, so `piped` is true in all of them and the missing case is the one
/// where it is false. It was found by running the thing in a terminal. What is testable is this
/// decision, which is why it is a function rather than an expression - the four cases are below.
fn headless(asked: bool, piped: bool) -> bool {
    asked || piped || cfg!(not(feature = "tui"))
}

/// What else was typed on a command line that also has `--connect`.
///
/// note: refused rather than ignored, and named rather than counted. A client assembles nothing,
/// so every one of these is an argument that would be dropped on the floor - and `-m "a question"`
/// beside `--connect` is a natural thing to type, which used to connect and say nothing at all
/// about the message.
///
/// note: read off the matches rather than declared as `conflicts_with_all`, because the list would
/// be every argument this program has and two of them are behind features. `--serve` can say it
/// the short way because it conflicts with two.
fn also_typed(matches: &clap::ArgMatches) -> Vec<String> {
    // the declared arguments rather than `ArgMatches::ids`, which also hands back the group clap's
    // derive makes for the struct itself - and that group reads as typed whenever anything in it is
    Args::command()
        .get_arguments()
        .map(|arg| arg.get_id().as_str().to_owned())
        // note: `on_ask` is the one exception, and it is one because it is not an argument a
        // client would have dropped on the floor. Everything else here assembles a session that
        // belongs to whoever is serving; this says what *this* client does with a question left
        // open when its input closes, which is nobody else's business. See `Client::settle`
        .filter(|id| {
            !matches!(id.as_str(), "connect" | "on_ask")
                && matches.value_source(id) == Some(clap::parser::ValueSource::CommandLine)
        })
        .map(|id| match id.as_str() {
            "message" => "the message".to_owned(),
            id => format!("`--{}`", id.replace('_', "-")),
        })
        .collect()
}

/// The program proper: wired the same way whichever of the two drives it.
async fn session() -> Result<()> {
    let Given {
        args,
        matches,
        found,
    } = Args::given()?;
    if args.print_config {
        print!("{}", kamchatka::config::SHIPPED);

        return Ok(());
    }

    // note: before any of the wiring below, and that is the whole reason it is here rather than
    // beside the three loops at the bottom. A client assembles nothing: the model, the key that
    // pays for it, the tools, the sandbox and the context all belong to whoever is serving, and a
    // client that attached to somebody else's session and then failed because *it* could not reach
    // a provider would be failing about a job that was never its own
    if let Some(address) = args.connect.clone() {
        let ignored = also_typed(&matches);
        if !ignored.is_empty() {
            return Err(anyhow::anyhow!(
                "`--connect` takes nothing else but `--on-ask`: the model, the key, the tools, \
                 the sandbox and the context all belong to whoever is serving. Drop {}",
                ignored.join(", ")
            ));
        }
        let (mut records, mut prose) = (stdout(), std::io::stderr());
        return remote::Client::new(args.on_ask.grant(), &mut records, &mut prose)
            .run(&address, tokio::io::BufReader::new(tokio::io::stdin()))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"));
    }

    // note: bound before the provider is reached, so that an address nobody can listen on is a
    // refusal in the first second rather than after a round trip to somebody's API. The socket
    // file it may have made is taken away again by `Server`'s own `Drop`, so failing after this
    // point leaves nothing behind
    let mut server = match &args.serve {
        Some(address) => Some(
            remote::Server::bind(address)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        ),
        None => None,
    };
    // the screen is drawn to stdout, so a stdout that is nobody's terminal cannot have one. It is
    // announced rather than silently chosen: a program that draws or does not draw depending on
    // what is on the other end of a pipe should say which it decided, and `--headless` is how
    // somebody says it themselves
    //
    // note: a served session is neither. It has no screen and it is not driven by lines either -
    // the decision below is about which of the *local* two is running, and asking it of a run that
    // is neither produced a notice about a pipe nobody had mentioned
    let piped = !std::io::stdout().is_terminal();
    // note: asked of a served session too, where it used to be skipped. `--serve` is no longer
    // "instead of a screen": a session with a socket in front of it draws as well, where there is
    // anything to draw on, so that the person running it can drive it from the desk it is on and
    // from a phone in the same breath. What the question decides for a served run is only whether
    // there is a screen, since `--serve` conflicts with `--headless` and the line driver is not one
    // of its answers
    let headless = headless(args.headless, piped);
    if server.is_none() && headless && !args.headless {
        match piped {
            true => eprintln!("· stdout is not a terminal, so this is a headless run"),
            false => eprintln!("· built without the `tui` feature, so this is a headless run"),
        }
    }

    let setup = args.setup()?;

    // note: before the provider, which is a round trip and an API key away. Everything `check`
    // answers is answerable from the arguments alone - a tool nobody offers, a path rule nothing
    // can match - and being told about one of those by an endpoint's refusal to talk is being
    // told about the wrong thing. `wire` asks it again for whoever is not `main`
    setup.check().map_err(|e| anyhow::anyhow!("{e}"))?;

    #[cfg(feature = "advise")]
    let setup = args.advised(setup).await?;

    let provider = args.provider().await?;

    let Wired {
        mut app,
        mut events,
        mut finished,
    } = setup.wire(provider).map_err(|e| anyhow::anyhow!("{e}"))?;

    // note: after the wiring rather than a field in `Setup`, because `Setup` is what an embedder
    // fills in to get a session and this is a fact about a window. Somebody embedding `App` draws
    // it themselves and sets this themselves, the way they already set `App::accent`'s neighbours
    //
    // note: parsed again rather than carried through as three bytes, and it cannot fail here -
    // `under` refused the file if it would. Doing it there is what makes a bad colour an error
    // about a settings file instead of a frame that is silently still yellow
    #[cfg(feature = "tui")]
    if let Some((r, g, b)) = args
        .border
        .as_deref()
        .and_then(|it| kamchatka::config::rgb(it).ok())
    {
        app.accent = ratatui::style::Color::Rgb(r, g, b);
    }

    // the servers have to outlive this scope: dropping one takes its child process, and its
    // tools, with it
    #[cfg(feature = "mcp")]
    let _servers = kamchatka::mcp::attach(&app.kernel, &app.policy, &args.mcp)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let on_ask = args.on_ask.grant();
    // note: three statements rather than one match over the pair, because a resumed headless run
    // wants both of the first two and the match gave it one. The replay line says what was picked
    // up; the opening says how this run is driven and what a question nobody can answer gets,
    // which a session carried on from a file needs to know exactly as much as a fresh one
    if headless {
        headless::opening(&mut app, on_ask);
    }
    if let Some(server) = &server {
        remote::opening(&mut app, &server.address());
        // note: said here *as well*, and the two are addressed to different people. The one above
        // goes into the conversation through `App::say`, which is how a client attaching an hour
        // later finds out what it has joined; this one is for whoever typed the flag, and without
        // it a served run is a terminal that prints nothing at all between starting and being
        // stopped - including on the interesting case, `tcp:127.0.0.1:0`, where the port is the
        // one thing the person does not already know
        println!("· serving on {}", server.address());
    }
    // a resumed session has a conversation in it already, and it would be strange to have to read
    // it out of the context pane one item at a time
    //
    // note: the record first, because what the items used to say is not in the snapshot - it is
    // in the log `/save` wrote beside it, which `App::recall` goes looking for and `App::replay`
    // then says the size of. The path is the one the person typed, so the log it finds is the
    // one belonging to the session they asked for
    if let Some(path) = &args.resume {
        app.recall(Path::new(path));
        app.replay();
    }
    // note: `server.is_none()` as well as `!headless`, because those are two different questions
    // and this greeting is about the third answer to neither. `headless` says the session is driven
    // by lines *here*; a served one is driven by neither lines nor keys, and told every client that
    // `ctrl+p` shows the next request and `F1` lists the keys - into a browser, which has no keys of
    // this program's to press. It said it once per session and then to everybody who ever attached,
    // because the greeting goes into the conversation and the conversation is in every projection
    #[cfg(feature = "tui")]
    if !headless && server.is_none() && args.resume.is_none() {
        app.say(Speaker::Note, ui::GREETING);
    }
    // said here rather than where it was read, because there is no screen at that point and this
    // is the one line a person needs before they wonder where a setting came from
    if let Some(path) = &found {
        app.say(
            Speaker::Note,
            format!("settings read from {}", path.display()),
        );
    }
    // note: said into the conversation rather than printed, because all three loops read that one
    // and a session with no model has the same thing to say to each of them. The corner says it
    // too, for as long as it is true, and `start_turn` says it again to anything that tries to
    // send - this is the orientation, not the enforcement
    if app.kernel.model_info().is_none() {
        app.say(
            Speaker::Note,
            format!(
                "no model yet: `/model ID` picks one, and `/models` lists what {} serves",
                app.provider.host()
            ),
        );
    }
    if let Some(message) = (!args.message.is_empty()).then(|| args.message.join(" ")) {
        app.ask(&message);
        app.start_turn();
    }

    // note: a served session with a screen is the *drawn* loop with a socket beside it, rather than
    // a fourth loop or a precedence somebody has to know. Only one loop can own the `App`, so the
    // one with the keys keeps it and `remote::Serving` is what it answers clients through - which is
    // the same pair of calls `Server::run` makes, from the other side. A served session with no
    // screen is unchanged: `Server::run` is the loop, and it is the one that ends the session
    #[cfg(feature = "tui")]
    if let Some(server) = &mut server
        && !headless
    {
        let outcome = drawn(&mut app, &mut events, &mut finished, Some(server)).await;

        return finish(&app, &args, Ending::Served, outcome);
    }
    if let Some(server) = &mut server {
        let outcome = server
            .run(&mut app, &mut events, &mut finished)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"));

        return finish(&app, &args, Ending::Served, outcome);
    }

    let outcome = match headless {
        true => {
            let (mut records, mut prose) = (stdout(), std::io::stderr());
            let mut driver = headless::Headless::new(on_ask, &mut records, &mut prose)
                // here rather than in the library's default: taking a process-wide signal is the
                // program's decision, and here this *is* the program
                .stops_on_ctrl_c();
            if let Some(seconds) = args.deadline {
                driver = driver.deadline(std::time::Duration::from_secs(seconds));
            }
            driver
                .run(
                    &mut app,
                    &mut events,
                    &mut finished,
                    tokio::io::BufReader::new(tokio::io::stdin()),
                )
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        #[cfg(feature = "tui")]
        false => drawn(&mut app, &mut events, &mut finished, None).await,
        #[cfg(not(feature = "tui"))]
        false => unreachable!("there is no screen in this build"),
    };

    let ending = match headless {
        true => Ending::Logged,
        false => Ending::Spoken,
    };

    finish(&app, &args, ending, outcome)
}

/// How a run that is over says so.
///
/// note: an enum over two `bool`s, because both of the questions it answers are decisions rather
/// than formalities and neither of them is "was there a screen". Which stream the closing lines go
/// to is whether *stdout* is carrying the record stream, and whether the session still wants ending
/// is whether the loop that just returned ended it - which two of the three do, deliberately.
enum Ending {
    /// Driven by lines: stdout is the log, so a person reads stderr, and the driver has already
    /// ended the session.
    Logged,
    /// Driven by keys: stdout is free, and nothing has ended the session yet.
    Spoken,
    /// Driven from a socket, and possibly from keys as well: stdout is free, and the loop that just
    /// returned has already ended the session.
    ///
    /// note: the two questions this enum is about are both the same for a served session whether or
    /// not it also had a screen, which is why there is no fourth. A loop that is serving ends the
    /// session itself, because the clients still attached are owed `session.finished` and the lines
    /// under it, and the voice they arrive on is that loop's.
    Served,
}

/// Ends the session if nothing else has, says where it got to, and writes it down.
///
/// note: three loops and one of these, because everything in here is about the run rather than
/// about how it was driven - and while it was inline at the bottom of `session` a third loop meant
/// a third copy of the two decisions [`Ending`] names.
fn finish(app: &App, args: &Args, ending: Ending, outcome: Result<()>) -> Result<()> {
    // note: the headless driver and the server have each ended the session themselves, so that the
    // record saying so goes down their own stream with the rest rather than being the one nobody
    // was sent. `Kernel::finish` emits an event every time it is called, so this is an either-or
    // rather than a belt and braces
    if matches!(ending, Ending::Spoken) {
        app.kernel.finish();
    }
    // note: on stderr in a headless run, because stdout is the log there. A line of prose in the
    // middle of a stream of JSON is the one thing that would make it unparseable, and this is the
    // last line written - so it would be the one nobody noticed until a reader fell over the end
    let logged = matches!(ending, Ending::Logged);
    let say = |line: &str| match logged {
        true => eprintln!("{line}"),
        false => println!("{line}"),
    };
    say(&format!(
        "{} · {} events recorded",
        app.kernel.session_name(),
        app.kernel.history().len()
    ));
    // the record was only ever written if somebody thought to type `/save`, which is exactly the
    // wrong condition: a session that ended badly is the one worth reading afterwards, and it was
    // the one that left nothing. Nine runs against a provider that timed out left no trace of how
    // far any of them had got
    if !args.no_record {
        match record(app) {
            Ok((records, log, state)) => say(&format!(
                "{records} records in {log}, and a session in {state}\n\
                 `kamchatka -r {state}` carries on from it"
            )),
            Err(e) => eprintln!("the session was not written: {e}"),
        }
    }

    outcome
}

/// Writes the session where nobody has to have asked for it, and says where that was.
///
/// note: a temporary directory, because this is a safety net rather than an archive - `/save`
/// remains the way to put a session somewhere it will still be next week. `--no-record` turns it
/// off for anyone who would rather a transcript did not outlive the terminal.
///
/// note: and the directory is the user's own, `0700`. What goes in it is a whole conversation and
/// every byte of output every tool produced, written without anybody asking for it; under the
/// default umask that is a world-readable file in a directory everyone on the machine can list.
/// Nobody would type `/save /tmp/everyone/notes.jsonl`, and this should not do it for them.
fn record(app: &App) -> Result<(usize, String, String)> {
    let mut dir = std::env::temp_dir();
    dir.push("kamchatka");
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        // it may already exist from an earlier run, made before this did it; either way, this is
        // the run that is about to write a transcript into it
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }

    let (log, state) = unclaimed(&dir.join(app.kernel.session_name()))?;
    let records = app
        .write_session(&log, &state)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok((records, log, state))
}

/// A `.jsonl` and `.json` pair under `stem` that no other session has written.
///
/// note: the name is a session's own, and a session's own name is not unique enough to be a
/// filename. Two of them collide in two ways, and both were silent. Two runs started inside one
/// second share a stamp, so the second to finish wrote over the first - the case this was written
/// for, found by starting two and reading one back. And **a resumed session keeps the name of the
/// session it resumed**, which is right for what a name is for and means `-r` wrote over the very
/// file it had just read: a hundred and twelve records of what happened replaced by the twelve of
/// a sitting that did nothing. The snapshot survived that one by luck, because a resumed context
/// renders to nearly the same bytes; the log did not, and the log is the half that says what
/// happened rather than where things ended up.
///
/// note: so the name stays what it is and the *file* moves - `…Z-2.jsonl` beside `…Z.jsonl`,
/// which sorts next to its sibling and reads as the second sitting of one session. Renaming the
/// session instead would put a process identifier in every filename to fix something rare, and
/// the name is what `#fork` derives from and what the trace shows.
///
/// note: `create_new` rather than asking whether the file is there, because between asking and
/// writing is exactly where the first of those two collisions lives. The `.json` is checked
/// before the `.jsonl` is claimed, so a suffix this passes over leaves nothing of its own behind.
fn unclaimed(stem: &std::path::Path) -> Result<(String, String)> {
    // bounded, so that a directory nothing can be written in is an error rather than a loop
    (1..1_000)
        .find_map(|nth| {
            let stem = match nth {
                1 => stem.display().to_string(),
                nth => format!("{}-{nth}", stem.display()),
            };
            let (log, state) = (format!("{stem}.jsonl"), format!("{stem}.json"));
            if std::path::Path::new(&state).exists() {
                return None;
            }

            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&log)
                .ok()
                .map(|_| (log, state))
        })
        .with_context(|| {
            format!(
                "could not find an unused name for the record beside {}",
                stem.display()
            )
        })
}

/// Takes the terminal, draws until there is nothing left to draw, and gives it back.
///
/// note: the terminal is taken here rather than in `session` so that the headless path never
/// touches it. It used to be set up before either loop ran, which was harmless only for as long
/// as there was one loop.
#[cfg(feature = "tui")]
async fn drawn(
    app: &mut App,
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    finished: &mut tokio::sync::mpsc::UnboundedReceiver<Outcome>,
    server: Option<&mut remote::Server>,
) -> Result<()> {
    // ratatui installs a hook of its own that restores the terminal and then calls this one
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableBracketedPaste);
        previous(info);
    }));

    // taken before the terminal, because it is the `App`'s half rather than the screen's, and kept
    // here rather than inside `run` so that the last of it can be said once the loop is over
    let mut serving = server.is_some().then(|| remote::Serving::new(app));

    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    let outcome = run(
        &mut terminal,
        app,
        events,
        finished,
        server,
        serving.as_mut(),
    )
    .await;
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();

    // note: a drawn session ends itself only where it was also served, and that is the difference
    // `Ending::Served` names. `session.finished` is a record like any other, and the clients still
    // attached are owed it and the lines under it - which `finish` could not send, because the
    // voice they arrive on is this loop's. A drawn session with no socket leaves it to `finish`,
    // where it has always been
    if let Some(serving) = &mut serving {
        app.kernel.finish();
        serving.last(app);
    }

    outcome
}

/// Draws, waits for whichever of the things that can happen happens first, and does it again.
///
/// note: `server` is the half that makes this the same session from two places at once. The keys
/// and the socket are two ways into one [`App`], and only one loop can own it - so this one does,
/// and `remote::Serving` is how the connections reach it: `pump` says what the session has said,
/// `arrived` and `attend` take on whoever connected, and `asked` and `answer` do what they ask.
/// `remote::Server::run` makes the same three calls from the other side, which is the point of
/// their being three calls rather than a loop.
#[cfg(feature = "tui")]
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    finished: &mut tokio::sync::mpsc::UnboundedReceiver<Outcome>,
    server: Option<&mut remote::Server>,
    mut serving: Option<&mut remote::Serving>,
) -> Result<()> {
    use tokio::sync::broadcast::error::RecvError;
    use tokio_stream::StreamExt as _;

    let mut keys = EventStream::new();
    let mut ticks = tokio::time::interval(TICK);

    loop {
        // before the frame, so that what a client is told and what the screen shows are one look at
        // the `App` rather than two. Nothing here draws
        if let Some(serving) = &mut serving {
            serving.pump(app);
        }
        terminal.draw(|frame| ui::draw(frame, app))?;
        // after the frame rather than before it, so the line saying what was handed over is on
        // the screen by the time the terminal has it
        if let Some(text) = app.clipboard.take()
            && let Err(why) = kamchatka::clipboard::hand_over(&text)
        {
            app.say(Speaker::Note, why);
        }
        if app.quit {
            return Ok(());
        }

        tokio::select! {
            key = keys.next() => match key {
                Some(Ok(TerminalEvent::Key(key))) => app.on_key(key).await,
                // without this, a pasted newline would be an enter press and would send half of
                // what was pasted
                Some(Ok(TerminalEvent::Paste(text))) => app.paste(&text),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e).context("the terminal stopped talking"),
                None => return Ok(()),
            },
            event = events.recv() => match event {
                Ok(event) => app.on_event(event),
                // the screen is the live view; the session log is the one that keeps everything
                Err(RecvError::Lagged(missed)) => app.say(
                    Speaker::Note,
                    format!("{missed} events went by too fast to draw; /save has them all"),
                ),
                Err(RecvError::Closed) => return Ok(()),
            },
            Some(outcome) = finished.recv() => {
                // the turn's last events are still in the queue behind this, and `select!` picks
                // whichever branch is ready rather than whichever happened first - so an outcome
                // drawn now would put "the turn paused after 3 requests" *above* the result it
                // paused after. The transcript is meant to be what happened, in that order
                while let Ok(event) = events.try_recv() {
                    app.on_event(event);
                }
                app.on_outcome(outcome);
            }
            // and the two that are only there where this session is also served. They are the same
            // pair `remote::Server::run` selects on, in a loop that happens to have a screen
            arrived = async {
                match &server {
                    Some(server) => server.arrived().await,
                    None => std::future::pending().await,
                }
            } => match (arrived, &mut serving) {
                (Ok(arrived), Some(serving)) => serving.attend(app, arrived),
                // the two are made together and there is no listener without one, so this is a
                // shape the types ask for and nothing reaches. Dropping the connection is what it
                // would mean if anything ever did
                (Ok(_), None) => {}
                // one connection failing to arrive is not a reason to end a session that may have
                // a turn running in it, and the person at the screen is the one who can see this
                (Err(e), _) => app.say(Speaker::Note, format!("a client could not connect: {e}")),
            },
            Some(ask) = async {
                match &mut serving {
                    Some(serving) => serving.asked().await,
                    None => std::future::pending().await,
                }
            } => {
                // note: the same hole `remote::Server::run` has, in the loop that also draws, and
                // closed the same way: the command holds the `App` and the loop waits, but the
                // kernel's broadcast is read meanwhile, because it is the one channel here that
                // drops what nobody took rather than queueing it. The keys are in a stream, the
                // outcome in an unbounded channel, a connection in the listen backlog; all of
                // those arrive late. A lagged subscription is a hole in what the screen shows and
                // in the trace every later client is handed.
                //
                // note: the screen does not redraw while it waits, and that is visible and
                // explains itself. What was not visible is the losing.
                let mut held = Vec::new();
                if let Some(serving) = &mut serving {
                    let doing = serving.answer(app, ask);
                    tokio::pin!(doing);
                    loop {
                        tokio::select! {
                            () = &mut doing => break,
                            event = events.recv() => held.push(event),
                        }
                    }
                }
                for event in held {
                    match event {
                        Ok(event) => app.on_event(event),
                        Err(RecvError::Lagged(missed)) => app.say(
                            Speaker::Note,
                            format!("{missed} events went by too fast to draw; /save has them all"),
                        ),
                        Err(RecvError::Closed) => return Ok(()),
                    }
                }
            }
            _ = ticks.tick() => {
                if let Some(notice) = app.provider.take_notice() {
                    app.say(Speaker::Note, notice);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session writes beside a record rather than over it, however it came by the same name.
    ///
    /// note: the second half is the case that matters, and it is not the exotic one: `-r` is the
    /// line this program prints at the end of every run, and a resumed session keeps the name of
    /// the session it resumed. Every resume wrote over the log it had just read.
    #[test]
    fn a_record_never_writes_over_one_that_is_already_there() {
        // note: not `tests/common`'s `scratch`, which builds under `CARGO_TARGET_TMPDIR` -
        // cargo hands that to integration tests and not to a unit test inside a binary. One
        // fixed name, emptied on the way in, so nothing accumulates either
        let dir = std::env::temp_dir().join("kamchatka-unclaimed");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to work in");
        let stem = dir.join("2026-09-15T13-34-29Z");

        let (log, state) = unclaimed(&stem).expect("nothing is there yet");
        assert!(log.ends_with("2026-09-15T13-34-29Z.jsonl"), "{log}");
        assert!(state.ends_with("2026-09-15T13-34-29Z.json"), "{state}");
        // what a session that got this far would leave behind
        std::fs::write(&log, "one").expect("written");
        std::fs::write(&state, "{}").expect("written");

        // the same name again - two runs in one second, or a resume - lands beside it
        let (again, beside) = unclaimed(&stem).expect("a second name");
        assert!(again.ends_with("2026-09-15T13-34-29Z-2.jsonl"), "{again}");
        assert!(beside.ends_with("2026-09-15T13-34-29Z-2.json"), "{beside}");
        assert_eq!(
            std::fs::read_to_string(&log).expect("still there"),
            "one",
            "the first record is untouched"
        );

        // and the claim is the file itself, so a third does not get the second's name back
        std::fs::write(&beside, "{}").expect("written");
        let (third, _) = unclaimed(&stem).expect("a third name");
        assert!(third.ends_with("2026-09-15T13-34-29Z-3.jsonl"), "{third}");
    }

    /// The four ways a run can end up with or without a screen.
    ///
    /// note: the third case is a bug that shipped. A build with no `tui` has nothing to draw with,
    /// so a run of it is headless whatever stdout is - and until this was written that was a
    /// notice printed beside a decision that had not been made, followed by a panic. The other
    /// three have been exercised by every piped test in this crate since the mode existed, which
    /// is exactly why the fourth went unnoticed: a test that pipes stdout cannot reach it.
    #[test]
    fn a_build_with_no_screen_is_headless_wherever_its_output_goes() {
        assert!(headless(true, false), "somebody asked for it");
        assert!(headless(false, true), "nowhere to draw");
        assert_eq!(
            headless(false, false),
            cfg!(not(feature = "tui")),
            "a terminal and no flag: a screen if this build has one, and headless if it has not"
        );
        assert!(headless(true, true));
    }
}
