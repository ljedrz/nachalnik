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
    io::{IsTerminal as _, Write as _, stdout},
    path::Path,
};

use anyhow::Result;
// the drawing loop's, like the two imports under it: the one `context` left in here is on a
// terminal that stopped talking, and a screenless build has no terminal
#[cfg(feature = "tui")]
use anyhow::Context as _;
use clap::CommandFactory as _;
#[cfg(feature = "tui")]
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event as TerminalEvent, EventStream,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};

use kamchatka::{
    app::{App, Speaker},
    args::{Args, Command, Given},
    headless, remote, sandbox,
    wiring::Wired,
};
// the drawing loop's own: the channel payload it has to name in a signature, and the screen
#[cfg(feature = "tui")]
use kamchatka::{app::Outcome, ui};
#[cfg(feature = "tui")]
use nachalnik::Event;

/// How often the loop looks for what arrives with no event: a notice, the busy line's clock, an
/// advisor's rating.
#[cfg(feature = "tui")]
const TICK: std::time::Duration = std::time::Duration::from_millis(120);

/// How many ticks go by with nothing to draw before the screen is drawn anyway.
///
/// note: a frame is drawn only when something changed; this is a floor under that rule for changes
/// that announce nothing. Missing one leaves the screen stale for a second instead of waiting for a
/// key.
#[cfg(feature = "tui")]
const HEARTBEAT: u32 = 8;

fn main() -> Result<std::process::ExitCode> {
    // before anything else, and before a runtime exists: `--confine-and-run` confines one command
    // and runs it, as a confinement somebody uses by hand, and makes a runtime of its own for the
    // sandbox's supervisor
    if let Some(code) = sandbox::run_if_asked() {
        std::process::exit(code);
    }

    // and still before a runtime exists, for a reason of the same shape as above. The local time
    // of day comes from libc, which reads the process environment; a thread setting a variable
    // while another reads it is undefined behaviour, so `time` refuses once a program is threaded.
    // Here nothing races, and the answer stays valid for the rest of the run: the trace pane needs
    // an offset, not a calendar, and a build without the pane needs neither
    #[cfg(feature = "tui")]
    kamchatka::app::when::note_local_offset(
        time::UtcOffset::current_local_offset()
            .ok()
            .map(time::UtcOffset::whole_seconds),
    );

    // and still before a runtime exists, for the reason the first of these gives. The arguments
    // decide whether this run assembles a session with confined commands; such a run puts itself
    // where nothing it starts may signal past it - see `sandbox::scope_signals` - which has to
    // happen while this is the only thread and nothing has been started
    let given = Args::given()?;
    let assembles = !given.args.print_config
        && given.args.check.is_none()
        && given.args.command.is_none()
        && given.args.connect.is_none();
    if assembles && !given.args.no_sandbox {
        sandbox::scope_signals();
    }

    // note: two workers rather than one per core. What they run waits on the network, a child or a
    // socket; the reading, writing and searching the file tools do go to the blocking pool, which
    // this does not limit. Every worker is another glibc malloc arena holding memory freed into it:
    // on a long headless session, two workers used clearly less memory at peak and afterwards than
    // the default, and ran a little faster. One worker uses less still, but then a request being
    // built holds up every stream and client until it is done
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(session(given));
    // note: the runtime is let go of rather than dropped, because otherwise the program *hangs*
    // after every early stop. `tokio::io::stdin` reads on a blocking thread; a blocking read on a
    // pipe nobody is writing to does not return; and dropping a runtime waits for its blocking
    // threads. So a headless run that ends by `ctrl+c`, a deadline or `/quit` - anything but the
    // input closing - would write its session out, print where it had gone, and then sit there
    // until somebody killed it.
    //
    // note: it is safe here because it is the last statement. The session has been written, the MCP
    // servers were dropped with the scope that held them - which is what kills their child
    // processes - and what is being abandoned is a thread waiting on a pipe.
    runtime.shutdown_background();

    // note: a headless run cut short says so in its status as well as on standard error, because a
    // script reads the status; see `headless::Stop`. Every other run that ends without an error is
    // a `0`, and an error is `anyhow`'s `1`
    match outcome {
        Ok(stopped) => Ok(std::process::ExitCode::from(
            stopped.map_or(0, headless::Stop::code),
        )),
        // said the way `main` returning an error says it, and then left with a deadline's status
        Err(e) if e.is::<OutOfTime>() => {
            let _ = writeln!(std::io::stderr(), "Error: {e:?}");
            Ok(std::process::ExitCode::from(
                headless::Stop::Deadline.code(),
            ))
        }
        Err(e) => Err(e),
    }
}

/// How a run with no screen stopped short, if it did, or why it failed.
///
/// note: a stop by the clock or a signal decides the status even when the last turn had already
/// failed. A script tells a timeout from a failure by the status; the failure has already been
/// reported on standard error and is in the record, and is reported again here, as a failure that
/// ends the run is. A run that reached its spend ceiling or `--max-requests-per-turn` after a
/// failed turn is still a failure: those stops are read off the session, not made by anything
/// outside it.
fn stopped_over_failed(
    ran: std::result::Result<(), String>,
    stopped: Option<headless::Stop>,
) -> Result<Option<headless::Stop>> {
    use headless::Stop;

    match (ran, stopped) {
        (Ok(()), stopped) => Ok(stopped),
        (
            Err(e),
            Some(stop @ (Stop::Deadline | Stop::Interrupted | Stop::Terminated | Stop::HungUp)),
        ) => {
            let _ = writeln!(std::io::stderr(), "Error: {e}");
            Ok(Some(stop))
        }
        (Err(e), _) => Err(anyhow::anyhow!("{e}")),
    }
}

/// How many times a headless run sends a request for a turn while the endpoint says it is busy;
/// see `Dialect::set_tries`.
const UNWATCHED_TRIES: usize = 10;

/// Whether this run is driven by lines rather than by keys.
///
/// note: three ways in: `asked`, stdout not being a terminal so there is nowhere to draw, or **this
/// build having no screen in it**. The third has to be a decision and not only a notice: without
/// it, `--no-default-features` in a terminal with no flag would report a headless run and then hit
/// the `unreachable!` below.
///
/// note: a function so that it can be tested. Every test of this binary pipes its stdout, so
/// `piped` is true in all of them and none can reach the case where it is false; the four cases are
/// below.
fn headless(asked: bool, piped: bool) -> bool {
    asked || piped || cfg!(not(feature = "tui"))
}

/// A deadline that passed before the session had started.
///
/// note: a type of its own so that `main` can find it under `anyhow` and exit with the status a
/// deadline has, `124`, as a deadline in the middle of a run does, rather than a failure's `1`. A
/// script asking whether a run was out of time should not have to know how far it had got.
#[derive(Debug)]
struct OutOfTime(String);

impl std::fmt::Display for OutOfTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "out of time {}: `--max-run-seconds` passed before the session started, and nothing \
             was run",
            self.0
        )
    }
}

impl std::error::Error for OutOfTime {}

/// Awaits one step of starting a run, unless the deadline passes first.
///
/// note: an error rather than the driver's "out of time", which ends a session and writes it out.
/// Here there is no session yet and nothing was run to keep.
async fn starting<T>(
    ends: Option<tokio::time::Instant>,
    step: &str,
    doing: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match ends {
        None => doing.await,
        Some(ends) => tokio::time::timeout_at(ends, doing)
            .await
            .unwrap_or_else(|_| Err(OutOfTime(step.to_owned()).into())),
    }
}

/// What else was typed on a command line that also has `--connect`.
///
/// note: these arguments are refused rather than ignored, and named rather than counted. A client
/// assembles nothing, so each one would be dropped; `-m "a question"` beside `--connect` is a
/// natural thing to type, and would otherwise connect and say nothing about the message.
///
/// note: read off the matches rather than declared as `conflicts_with_all`, because the list would
/// be every argument this program has and two of them are behind features. `--serve` can use the
/// short way because it conflicts with two.
fn also_typed(matches: &clap::ArgMatches, besides: &[&str]) -> Vec<String> {
    // the declared arguments rather than `ArgMatches::ids`, which also hands back the group clap's
    // derive makes for the struct itself. That group reads as typed whenever anything in it is
    Args::command()
        .get_arguments()
        .map(|arg| arg.get_id().as_str().to_owned())
        // note: `on_ask` is the one exception because it is not an argument a client would drop.
        // Everything else here assembles a session for whoever is serving. This says what this
        // client does with a question left open when its input closes, which is nobody else's
        // business. See `Client::settle`
        .filter(|id| {
            !besides.contains(&id.as_str())
                && matches.value_source(id) == Some(clap::parser::ValueSource::CommandLine)
        })
        .map(|id| match id.as_str() {
            "message" => "the message".to_owned(),
            id => format!("`--{}`", id.replace('_', "-")),
        })
        .collect()
}

/// `--check`: reads the pair `path` names, says what it found, and fails if it found anything.
fn checked(path: &str) -> Result<()> {
    let (log, state) = kamchatka::check::pair(path);
    let read = |file: &Path| match std::fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!("could not read {}: {e}", file.display())),
    };
    let (logged, snapshot) = (read(&log)?, read(&state)?);
    if logged.is_none() && snapshot.is_none() {
        anyhow::bail!(
            "there is no session at {path}: neither {} nor {} is there",
            log.display(),
            state.display()
        );
    }

    let checked = kamchatka::check::check(logged.as_deref(), snapshot.as_deref());
    let mut out = stdout();
    for finding in &checked.findings {
        let _ = writeln!(out, "{finding}");
    }
    let read = [
        logged
            .is_some()
            .then(|| format!("{} records in {}", checked.records, log.display())),
        snapshot
            .is_some()
            .then(|| format!("the snapshot in {}", state.display())),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" and ");
    match checked.findings.len() {
        0 => {
            let _ = writeln!(out, "read {read}: nothing that does not add up");
            Ok(())
        }
        found => Err(anyhow::anyhow!(
            "read {read}: {found} thing(s) that do not add up"
        )),
    }
}

/// `reconcile`: folds the forks into one session, writes it under `output`, and says what it
/// decided on the way.
fn reconciled(paths: &[String], output: &str) -> Result<()> {
    use kamchatka::reconcile::{Fork, reconcile, session_name, write};

    let mut seen = std::collections::HashSet::new();
    for path in paths {
        let (_, state) = kamchatka::check::pair(path);
        let same = std::fs::canonicalize(&state).unwrap_or(state);
        anyhow::ensure!(
            seen.insert(same),
            "{path} is named twice, and a fork reconciled with itself is the fork"
        );
    }
    // as `/load` says it, rather than as a missing `DIR/.json`
    if let Some(dir) = paths.iter().find(|path| Path::new(path).is_dir()) {
        anyhow::bail!("{dir} is a directory, and a fork is a session's file: `/save` names them");
    }
    let forks = paths
        .iter()
        .map(|path| Fork::read(path))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!(e))?;
    let reconciled = reconcile(&forks, &session_name()).map_err(|e| anyhow::anyhow!(e))?;
    let (log, state) = write(&reconciled, output).map_err(|e| anyhow::anyhow!(e))?;

    let mut out = stdout();
    let _ = writeln!(
        out,
        "the forks agree up to item {}",
        reconciled.shared_through
    );
    for line in &reconciled.said {
        let _ = writeln!(out, "  {line}");
    }
    // the model is in the log where the forks agree on one, and `-r` reads it from there
    let _ = writeln!(
        out,
        "{} records in {log}, and a session in {state}: `kamchatka -r {state}` carries on from it{}",
        reconciled.records.len(),
        match &reconciled.model {
            Some(model) => format!(", talking to {model}"),
            None => ", with the model `-m` names: the forks' logs do not name one model".to_owned(),
        }
    );

    Ok(())
}

/// The program proper: wired the same way whichever of the two drives it.
async fn session(given: Given) -> Result<Option<headless::Stop>> {
    let begun = tokio::time::Instant::now();
    // whole, rather than taken apart here, because `--serve` asks the arguments which of the two
    // it does not read and a settings file's answer is not in the matches; see
    // `Given::unserved`
    let Given {
        args,
        matches,
        found,
        filed,
    } = &given;
    if args.print_config {
        // note: written rather than `print!`ed, which panics when the write fails. This is all the
        // flag asks for. A stream nobody can write to is as much a place for a script redirect as
        // for a person typing
        let _ = write!(stdout(), "{}", kamchatka::config::SHIPPED);

        return Ok(None);
    }
    if let Some(path) = &args.check {
        return checked(path).map(|()| None);
    }
    if let Some(command) = &args.command {
        // refused rather than ignored, as beside `--connect`: a command runs no session, so a
        // flag for one typed before it would be dropped on the floor
        let ignored = also_typed(matches, &[]);
        if !ignored.is_empty() {
            return Err(anyhow::anyhow!(
                "a command takes only its own arguments, after its name. Drop {}",
                ignored.join(", ")
            ));
        }
        return match command {
            Command::Reconcile { forks, output } => reconciled(forks, output).map(|()| None),
            _ => unreachable!("every command this build has is matched above"),
        };
    }
    // note: printed before anything the file asked for is done, because it can ask for MCP servers
    // started below and a sandbox turned off. The conversation exists only once the session is
    // wired, too late to be the first place this is said. A screen covers the line while it is up
    // and leaves it behind when it goes; the conversation says it again for the screen and a served
    // session's clients. Written rather than `eprintln!`ed, which panics with nobody reading and
    // takes the run with it
    if let Some(path) = &found {
        let _ = writeln!(std::io::stderr(), "· settings read from {}", path.display());
    }

    // note: before any of the wiring below, so it is here rather than beside the three loops at the
    // bottom. A client assembles nothing: the model, the key that pays for it, the tools, the
    // sandbox and the context belong to whoever is serving. A client attached to somebody else's
    // session should not fail for not reaching a provider, which was never its job
    if let Some(address) = args.connect.clone() {
        let ignored = also_typed(matches, &["connect", "on_ask"]);
        if !ignored.is_empty() {
            return Err(anyhow::anyhow!(
                "`--connect` takes nothing else but `--on-ask`: the model, the key, the tools, \
                 the sandbox and the context all belong to whoever is serving. Drop {}",
                ignored.join(", ")
            ));
        }
        // note: `leave` unless this command line says otherwise, including a settings file's
        // `on-ask`, which is written for runs that have nobody else to ask. A client whose input
        // closes is usually watching a session somebody else is driving. Answering their questions
        // for them - `deny` by default - would be this client deciding what another person was
        // about to do
        let typed = matches.value_source("on_ask") == Some(clap::parser::ValueSource::CommandLine);
        let on_ask = match typed {
            true => args.on_ask,
            false => kamchatka::args::OnAsk::Leave,
        };
        let (mut records, mut prose) = (stdout(), std::io::stderr());
        let mut client = remote::Client::new(
            on_ask.grant().unwrap_or(nachalnik::Grant::Deny),
            &mut records,
            &mut prose,
        );
        if on_ask.grant().is_none() {
            client = client.leaves_questions();
        }
        if !std::io::stdin().is_terminal() {
            client = client.waits_for_turns();
        }
        return client
            .run(&address, tokio::io::BufReader::new(tokio::io::stdin()))
            .await
            .map(|()| None)
            .map_err(|e| anyhow::anyhow!("{e}"));
    }

    // refused before anything is started - a socket, a provider or an MCP server: `leave` is
    // someone else's to answer, and a run that is not `--connect` has nobody else
    let Some(on_ask) = args.on_ask.grant() else {
        return Err(anyhow::anyhow!(
            "`--on-ask leave` is for `--connect`, where another client can answer what this one \
             leaves; here nobody else can, so it is `deny` or `allow`"
        ));
    };

    // note: bind before the provider is reached, so an address that cannot be listened on fails in
    // the first second rather than after a round trip to an API. `Server`'s `Drop` removes any
    // socket file it made, so failing after this point leaves nothing behind.
    //
    // `--web` without `--serve` still needs a served session for the page to relay to: a loopback
    // port picked by the kernel
    #[cfg(feature = "webui")]
    let served = args
        .serve
        .clone()
        .or_else(|| args.web.as_ref().map(|_| "tcp:127.0.0.1:0".to_owned()));
    #[cfg(not(feature = "webui"))]
    let served = args.serve.clone();
    let mut server = match &served {
        Some(address) => {
            // note: before the bind: a refused argument must leave nothing listening. Check the
            // arguments, not the matches, because the merge writes a settings file's
            // `max-run-seconds` and `on-ask` into the arguments and leaves no trace of their
            // source. A check on the matches would read a file-supplied deadline as nothing, and
            // the session would serve for ever without a word
            let unserved = given.unserved();
            let flag = match args.serve {
                Some(_) => "--serve",
                None => "--web",
            };
            anyhow::ensure!(
                unserved.is_empty(),
                "`{flag}` does not read {unserved}: a served session is this program's and goes \
                 on for as long as somebody wants it, while `--max-run-seconds` ends a headless \
                 run and `--on-ask` answers a question nobody is there to answer",
            );
            Some(
                remote::Server::bind(address)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?,
            )
        }
        None => None,
    };
    // bind the page now too, so a bad address fails before anything else starts
    #[cfg(feature = "webui")]
    let web = match (&args.web, &server) {
        (Some(listen), Some(server)) => Some(
            kamchatka::web::Web::bind(listen, &server.address())
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        ),
        _ => None,
    };
    // the screen is drawn to stdout, so a stdout that is not a terminal cannot have one. The mode
    // is announced rather than silently chosen: a program that draws or does not draw depending on
    // what is on the other end of a pipe should say which it decided, and `--headless` is how
    // somebody says it themselves.
    //
    // note: the notice is not said for a served session. The line driver is never its loop, so a
    // notice about headless runs would be about a pipe nobody mentioned
    let piped = !std::io::stdout().is_terminal();
    // note: the question is asked of a served session too. A session with a socket in front of it
    // draws as well, where there is anything to draw on, so the person running it can drive it from
    // the desk it is on and from a browser. For a served run the question decides only whether
    // there is a screen, since `--serve` conflicts with `--headless`
    let headless = headless(args.headless, piped);
    // a headless run nobody is attached to, which is most of what `headless` is asked for
    let unattended = headless && server.is_none();
    if unattended && !args.headless {
        // note: written rather than `eprintln!`ed, which panics when nobody is reading and takes
        // the run with it. Saying which mode was chosen must not be able to end the run, and this
        // is said on the way into the mode where an unread stream is the ordinary case
        let _ = match piped {
            true => writeln!(
                std::io::stderr(),
                "· stdout is not a terminal, so this is a headless run"
            ),
            false => writeln!(
                std::io::stderr(),
                "· built without the `tui` feature, so this is a headless run"
            ),
        };
    }

    // note: the run's, not the session's, and counted from the start. `--max-run-seconds` stops a
    // run however far it has got. The endpoint's probe and an MCP server's handshake wait on
    // somebody else's program, and nothing else bounds them - see POSTPONED.md for why a server's
    // has no timeout of its own. `/restart` does not start the clock again.
    //
    // note: `0` is none, as for `--max-spend-tokens` and `--max-requests-per-turn`: a deadline of
    // nothing stops the run before the first line is read. A duration too far off to add to the
    // clock is none as well, rather than a panic
    let ends = args
        .max_run_seconds
        .filter(|seconds| *seconds > 0 && unattended)
        .and_then(|seconds| begun.checked_add(std::time::Duration::from_secs(seconds)));

    // note: the checks run before the provider, which is a round trip and an API key away.
    // Everything they answer is answerable from the arguments alone - a tool nobody offers, a path
    // rule nothing can match - and hearing about one of those through an endpoint's refusal to talk
    // is hearing about the wrong thing. They are asked here rather than by a separate `check`,
    // because here a value from a settings file can still be named; `wire` asks again for whoever
    // is not `main`
    let setup = args.setup_from(filed.as_ref())?;
    // said the way the kernel's gaps are below: once on standard error, and into each session
    let cautions: Vec<String> = args
        .unstarted(filed.as_ref())
        .into_iter()
        .chain(args.shell_cautions(filed.as_ref()))
        .collect();
    for caution in &cautions {
        let _ = writeln!(std::io::stderr(), "· {caution}");
    }

    #[cfg(feature = "shell-advisor")]
    let setup = starting(ends, "reaching the advisor", args.advised(setup)).await?;

    let provider = starting(ends, "reaching the model", args.provider()).await?;
    // note: a run nobody watches waits out a busy endpoint for about five minutes rather than
    // fourteen seconds, since there is nobody to tell sooner and a limit several sessions share
    // lasts longer than that; `--max-run-seconds` still bounds the whole run. A restart keeps the
    // provider, and with it this
    if unattended {
        provider.set_tries(UNWATCHED_TRIES);
    }
    let flagged = kamchatka::wiring::Flagged::of(&*provider);

    // note: kept so `/restart` can wire a second session from the same settings. The new session is
    // the one the flags describe, not a copy of this session's drift - a model switched with
    // `/model`, a capability answered `always`, a tool toggled off are all this session's, and none
    // of them survive being started again
    let base = setup.clone();

    let Wired {
        mut app,
        mut events,
        mut finished,
    } = setup
        .wire(provider.clone())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // the record beside the snapshot is read before this session's own is started. That one
    // begins with what this reads, so it is the whole session rather than the part this run saw,
    // and `App::replay` below says how much of it came back. The path is the one typed, so the log
    // it finds is the one belonging to the session asked for
    if let Some(path) = &args.resume {
        app.recall(Path::new(path));
    }
    start_recording(&mut app, base.record);

    // the servers must outlive every session this run has, not just the first; see `mcp::attach`.
    // A restart installs the tools they already offer into the new kernel rather than spawning them
    // again, because the handshake is a round trip and a `npx` server is seconds of it.
    //
    // note: each one is named before it is started, because an unbounded handshake is otherwise a
    // run that says nothing while it waits - and a server named by a file found underfoot is a
    // program somebody may not know they asked for
    #[cfg(feature = "mcp")]
    let servers = {
        for spec in &args.mcp {
            let _ = writeln!(std::io::stderr(), "· starting the MCP server `{spec}`");
        }
        starting(ends, "starting the MCP servers", async {
            kamchatka::mcp::attach_within(&app.kernel, &app.policy, &args.mcp, args.mcp_within())
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
        })
        .await?
    };

    // note: one reader for the whole run rather than one per session, because of `/restart` in a
    // piped run. A `BufReader` has read ahead by the time a line is handed over, so building a
    // second one for the second session drops whatever was already in the first one's buffer -
    // `do this / restart / do that` would run the restart and never see the third line. The reader
    // is the run's input; the sessions take turns on it
    let mut input = tokio::io::BufReader::new(tokio::io::stdin());

    if let Some(server) = &server {
        // note: printed once for the whole run rather than said into each session, and the two
        // halves of that pair are addressed to different people. `remote::opening` goes into the
        // conversation through `App::say`, so a client attaching an hour later finds out what it
        // has joined, and a restarted session owes a newcomer that as much as the first one did.
        // This line is for whoever typed the flag, and the address does not move
        //
        // note: written rather than `println!`ed, because `println!` panics if nobody is reading. A
        // served run's stdout is often a supervisor's, and if it has closed, the run should keep
        // serving rather than panic on its first line
        let _ = writeln!(stdout(), "· serving on {}", server.address());
    }
    // the page runs for the whole run, across `/restart`; browsers reconnect by themselves.
    // Connection errors go to stderr, except under a drawn screen, which they would corrupt. A LAN
    // warning goes to stderr and onto the screen of each session, because a screen hides stderr.
    // Not to the browsers: it is for whoever started this, who is the one who can do something
    // about it
    #[cfg(feature = "webui")]
    let exposure = web.as_ref().and_then(kamchatka::web::Web::exposure);
    #[cfg(not(feature = "webui"))]
    let exposure: Option<String> = None;
    #[cfg(feature = "webui")]
    let page = web.as_ref().map(kamchatka::web::Web::address);
    #[cfg(not(feature = "webui"))]
    let page: Option<String> = None;
    #[cfg(feature = "webui")]
    if let Some(web) = web {
        let _ = writeln!(
            stdout(),
            "· a browser reaches the session at {}",
            web.address()
        );
        // note: the code only where a person is looking at it. A piped stdout is a program reading
        // the address off the line above; half a screen of blocks is nothing it can use
        if std::io::stdout().is_terminal()
            && let Some(code) = kamchatka::qr::Code::new(&web.address())
        {
            // white on black, whatever the terminal's colours, for the reason the overlay gives
            let lines = code.lines();
            for line in &lines {
                let _ = writeln!(stdout(), "\x1b[97;40m{line}\x1b[0m");
            }
        }
        if let Some(exposure) = &exposure {
            let _ = writeln!(std::io::stderr(), "· {exposure}");
        }
        tokio::spawn(web.run(move |said| {
            if headless {
                let _ = writeln!(std::io::stderr(), "· {said}");
            }
        }));
    }

    // what this kernel leaves out of the sandbox, once on standard error and into each session,
    // since a screen hides the first. Asked whatever tools are offered at first, because a shell
    // can be toggled on later and it is the same sandbox
    let weaker = (!args.no_sandbox).then(sandbox::weaker_here).flatten();
    if let Some(weaker) = &weaker {
        let _ = writeln!(std::io::stderr(), "· {weaker}");
    }

    // note: a loop because `/restart` writes this session out and asks for another. Inside is
    // everything a session is: the opening lines, the driving, and the ending. Above is everything
    // the run is: the socket, the servers, the provider, and the settings the next one is built
    // from
    let mut first = true;
    let outcome = loop {
        // note: after the wiring rather than a field in `Setup`, because `Setup` is what an
        // embedder fills in to get a session and this is a fact about a window. An embedder draws
        // it and sets this themselves, as they do with `App::accent`'s neighbours.
        //
        // note: parsed again rather than carried through as three bytes, and it cannot fail here:
        // `under` refused the file if it would. Parsing it there makes a bad colour an error about
        // a settings file instead of a frame that is silently the default colour
        #[cfg(feature = "tui")]
        {
            app.accent = match args
                .border_color
                .as_deref()
                .and_then(|it| kamchatka::config::rgb(it).ok())
            {
                Some((r, g, b)) => ratatui::style::Color::Rgb(r, g, b),
                None => ratatui::style::Color::Reset,
            };
        }

        // note: three statements rather than one match over the pair, because a resumed headless
        // run wants both of the first two and a match gives only one. The replay line says what was
        // picked up; the opening says how this run is driven and what a question nobody can answer
        // gets. A session carried on from a file needs to know that as much as a fresh one.
        //
        // note: not for a served session, whose loop is `Server::run` even with no screen. That
        // loop reads no lines of this program's and never consults `--on-ask`, so the opening would
        // tell every attaching client that a question is answered `deny`, beside a line saying the
        // session waits for one
        if unattended {
            headless::opening(&mut app, on_ask);
        }
        if let Some(server) = &server {
            remote::opening(&mut app, &server.address());
        }
        if let Some(exposure) = &exposure {
            app.tell(exposure.clone());
        }
        app.web = page.clone();
        // note: shown as the screen opens where the page is for a phone, because the screen covers
        // the code printed above it; on loopback the browser is on this machine and the address can
        // be copied rather than photographed. First session only: a `/restart` means somebody is
        // already using it
        if first && !headless && exposure.is_some() {
            app.web_page();
        }
        if let Some(weaker) = &weaker {
            app.say(Speaker::Note, weaker.clone());
        }
        for caution in &cautions {
            app.say(Speaker::Note, caution.clone());
        }
        // a resumed session already has a conversation; replay it instead of reading it one item at
        // a time.
        //
        // note: the items' previous text is not in the snapshot; it is in the log `/save` wrote
        // beside it. `App::recall` read it when the session was wired, and `App::replay` reports
        // its size.
        //
        // note: first session of the run only. A snapshot is where somebody asked to start, and
        // `/restart` says they are done with where they started. A restart that read it back in
        // would leave no way to reach an empty session from a resumed one, which is most of what
        // `/restart` is for
        if first && let Some(path) = &args.resume {
            app.replay();
            // the `context` tool's undo history, carried from the snapshot so the model can still
            // walk back what it changed. See `introspect::History`
            if let (Some(installed), Some(saved)) = (
                &app.introspect,
                kamchatka::introspect::saved_history(Path::new(path)),
            ) && let Err(e) = installed.history().carry(&saved)
            {
                app.say(Speaker::Note, e);
            }
            // and where it was talking, where that is not where this run is pointed. Said rather
            // than followed; see `args::last_endpoint`
            if let Some(was) = kamchatka::args::last_endpoint(Path::new(path))
                && let Some(now) = app.provider.info().endpoint
                && now != was
            {
                app.say(
                    Speaker::Note,
                    format!(
                        "the record says this session was last talking to {was}, and this run is \
                         pointed at {now}; `/endpoint {was}` carries on there"
                    ),
                );
            }
        }
        // only for the screen: a served session's clients never see it; see `App::greet`
        #[cfg(feature = "tui")]
        if !headless && !(first && args.resume.is_some()) {
            app.greet();
        }
        // said again here, where a screen and a served session's clients read, because this is the
        // line a person needs before wondering where a setting came from. Not in a headless run
        // with nobody attached: its conversation is printed to the stream the line above already
        // went to
        if let Some(path) = found.as_ref().filter(|_| !unattended) {
            app.say(
                Speaker::Note,
                format!("settings read from {}", path.display()),
            );
        }
        // note: said into the conversation rather than printed, because all three loops read it and
        // a session with no model has the same thing to say to each of them. The corner says it
        // too, for as long as it is true, and `start_turn` says it again to anything that tries to
        // send. This is the orientation, not the enforcement
        if app.kernel.model_info().is_none() {
            app.say(
                Speaker::Note,
                format!(
                    "no model yet: `/model ID` picks one, and `/models` lists what {} serves",
                    app.provider.host()
                ),
            );
        }
        // note: first session only. Asking `--message` again on every restart would make `/restart`
        // a way of putting the same question to the model for ever, the opposite of what somebody
        // types it for.
        //
        // note: the text, not the arity, decides: an empty line is nothing, and a script that hands
        // over a variable it never set would otherwise pay a round trip and a failed turn for a
        // request the provider refuses
        if first
            && let Some(message) = (!args.message.is_empty())
                .then(|| args.message.join(" "))
                .filter(|message| !message.trim().is_empty())
        {
            app.ask(&message);
            app.start_turn();
        }
        first = false;

        // note: a served session with a screen runs the drawn loop with a socket beside it, not a
        // fourth loop or a precedence rule. Only one loop can own the `App`, so the loop with the
        // keys keeps it and answers clients through `remote::Serving`, using the same calls as
        // `Server::run`. A served session with no screen uses `Server::run` as its loop, and
        // `Server::run` ends the session
        let ran = match &mut server {
            Some(server) => match headless {
                #[cfg(feature = "tui")]
                false => drawn(&mut app, &mut events, &mut finished, Some(server))
                    .await
                    .map(|()| None),
                #[cfg(not(feature = "tui"))]
                false => unreachable!("there is no screen in this build"),
                true => {
                    let ran = server.run(&mut app, &mut events, &mut finished).await;
                    stopped_over_failed(ran, server.stopped())
                }
            },
            None => match headless {
                true => {
                    let (mut records, mut prose) = (stdout(), std::io::stderr());
                    let mut driver = headless::Headless::new(on_ask, &mut records, &mut prose)
                        // here rather than in the library's default: taking a process-wide signal
                        // is the program's decision, and this is the program
                        .stops_on_ctrl_c()
                        .leaves_when_terminated();
                    if let Some(ends) = ends {
                        driver = driver
                            .deadline(ends.saturating_duration_since(tokio::time::Instant::now()));
                    }
                    let ran = driver
                        .run(&mut app, &mut events, &mut finished, &mut input)
                        .await;
                    stopped_over_failed(ran, driver.stopped())
                }
                #[cfg(feature = "tui")]
                false => drawn(&mut app, &mut events, &mut finished, None)
                    .await
                    .map(|()| None),
                #[cfg(not(feature = "tui"))]
                false => unreachable!("there is no screen in this build"),
            },
        };

        if !app.restarting() {
            break ran;
        }

        // note: the loop that just returned runs one extra iteration after the session is over.
        // That is the cost of `App::leaving` asking one question, *stop holding this*; this code
        // reads which of the two reasons applied, because here there is somewhere to go with the
        // answer
        flagged.restore(&*provider).await;
        let (wired, said) = base
            .relaunch(&app, provider.clone())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let Wired {
            app: fresh,
            events: replaced,
            finished: reported,
        } = wired;
        (app, events, finished) = (fresh, replaced, reported);

        // the first thing the new session says, because only here are the old session's name and
        // the file it went to still written down
        app.say(Speaker::Note, said);
        start_recording(&mut app, base.record);

        // note: the servers keep running and their tools are installed into the new kernel, so
        // `attach` is above the loop and this is not. Re-spawning them would cost seconds of
        // handshake for tools the process already holds open.
        //
        // note: after the line that says the old session ended. Otherwise a server that would not
        // list its tools comes first, and the first thing the new session says is about the old one
        #[cfg(feature = "mcp")]
        for left_out in kamchatka::mcp::reinstall(&app.kernel, &app.policy, &servers).await {
            app.say(Speaker::Error, left_out);
        }
    };

    finish(&app, base.record, unattended, outcome)
}

/// Starts recording the session as it goes, unless `--no-record` was given.
///
/// note: if the record cannot be claimed, the failure is reported and is not fatal. `--no-record`
/// is the flag for wanting no record, and a temporary directory made unusable by someone else
/// should not stop the program from running. `finish` tries once more at the end, which is how a
/// session with no recorder was always written.
fn start_recording(app: &mut App, record: bool) {
    if !record {
        return;
    }
    match kamchatka::wiring::Recorder::start(app) {
        Ok(recorder) => app.recorder = Some(recorder),
        Err(e) => app.say(
            Speaker::Error,
            format!("the session is not being written down as it goes: {e}"),
        ),
    }
}

/// Ends the session if nothing else has, says where it got to, and writes it down.
///
/// note: three loops and one `finish`, because everything here concerns the run rather than how it
/// was driven. The only thing it asks about the driving is `logged`: whether stdout carries the
/// record stream. A headless run does; a served run does not, even with no screen.
fn finish(
    app: &App,
    record: bool,
    logged: bool,
    outcome: Result<Option<headless::Stop>>,
) -> Result<Option<headless::Stop>> {
    // note: the headless driver and the server each end the session, so the record saying so goes
    // down their own stream with the rest, rather than being an event no stream carries.
    // `Kernel::finish` emits an event every time it is called, so this asks the log whether the
    // session has ended rather than which loop ran. A loop meant to end the session can exit before
    // doing so: the headless driver returns on a line it cannot read, or when stdout goes away. The
    // record written below must still say the session ended
    if !kamchatka::wiring::ended(&app.kernel) {
        app.kernel.finish();
    }
    // note: written to stderr in a headless run, because stdout is the log there. A line of prose
    // in a stream of JSON makes it unparseable, and this is the last line written.
    //
    // note: written without the macros, which panic when the write fails. A run piped into `head`
    // has nobody reading either stream by the time it gets here, and a panic on the first line
    // would take the record down
    let say = |line: &str| {
        let _ = match logged {
            true => writeln!(std::io::stderr(), "{line}"),
            false => writeln!(stdout(), "{line}"),
        };
    };
    // written whether or not anybody typed `/save`: a session that ended badly is the one worth
    // reading afterwards, and the one nobody thinks to save. It is written before anything is said
    // about it, because saying is the part that can fail
    let written = record.then(|| kamchatka::wiring::record(app));
    say(&format!(
        "{} · {} events recorded",
        app.kernel.session_name(),
        app.kernel.history().len()
    ));
    // a status of its own for a run that did everything but this, since a script reads `0` as a
    // session it can find afterwards
    let unrecorded = matches!(written, Some(Err(_)));
    match written {
        Some(Ok(written)) => say(&format!(
            "{written}\n`kamchatka -r {}` carries on from it",
            written.state.display()
        )),
        Some(Err(e)) => {
            let _ = writeln!(std::io::stderr(), "the session was not written: {e}");
        }
        // the fact `/restart` says at the other door, in the same words: the count above reads as a
        // pointer to a record, and under this flag there is no file to point at. It does not say
        // where the records went instead: a piped run writes them to stdout whatever else was asked
        // for, and a drawn or served one has nowhere
        //
        // note: "not written when it ended" rather than "nothing was written": a `/save` typed
        // during the session wrote whatever it was asked to, and a model reading these lines took
        // the one to deny the other
        None => say("`--no-record`, so the session was not written when it ended"),
    }
    // last, because it is the one thing here that waits: a straggler is given a moment to leave on
    // `SIGTERM`. A record written first is a record a slow straggler cannot keep from being kept
    for line in kamchatka::wiring::stragglers_at_end(app) {
        say(&line);
    }

    match outcome {
        Ok(None) if unrecorded => Ok(Some(headless::Stop::Unrecorded)),
        outcome => outcome,
    }
}

/// Takes the terminal, draws until there is nothing left to draw, and gives it back.
///
/// note: the terminal is taken here rather than in `session` so that the headless path never
/// touches it.
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
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags, DisableBracketedPaste);
        previous(info);
    }));

    // taken before the terminal, because it is the `App`'s half rather than the screen's, and kept
    // here rather than inside `run` so that the last of it can be said once the loop is over
    let mut serving = server.is_some().then(|| remote::Serving::new(app));

    let mut terminal = ratatui::init();
    // note: the kitty keyboard protocol, asked for rather than checked for, is the only way a
    // terminal sends `shift+enter` as anything but `enter`. One that does not speak it ignores
    // the request, and `alt+enter` is the newline there. Only the flag that disambiguates:
    // releases and repeats are further flags, and nothing here wants them
    let _ = execute!(
        stdout(),
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let outcome = run(
        &mut terminal,
        app,
        events,
        finished,
        server,
        serving.as_mut(),
    )
    .await;
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags, DisableBracketedPaste);
    ratatui::restore();
    // A turn still running is stopped and waited for before anybody writes `session.finished`,
    // however the loop was left: `/quit`, a signal, or a terminal that stopped answering
    app.wait_for_turn(events, finished, |_| {}).await;

    // note: a drawn session ends itself only where it was also served. `session.finished` is a
    // record like any other, and the clients still attached are owed it and the lines under it.
    // `finish` could not send them, because the voice they arrive on is this loop's. A drawn
    // session with no socket leaves it to `finish`
    if let Some(serving) = serving {
        app.kernel.finish();
        serving.last(app).await;
    }

    outcome
}

/// Draws, waits for whichever of the things that can happen happens first, and does it again.
///
/// note: `server` makes this the same session from two places at once. The keys and the socket are
/// two ways into one [`App`], and only one loop can own it, so this one does, and `remote::Serving`
/// is how the connections reach it: `pump` says what the session has said, `arrived` and `attend`
/// take on whoever connected, and `asked` and `answered` do what they ask. They are calls rather
/// than a loop so that `remote::Server::run` can make the same ones, and what both loops do with an
/// event, an outcome or an arrival is a function of `remote::server`'s that both call.
#[cfg(feature = "tui")]
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    finished: &mut tokio::sync::mpsc::UnboundedReceiver<Outcome>,
    server: Option<&mut remote::Server>,
    mut serving: Option<&mut remote::Serving>,
) -> Result<()> {
    use kamchatka::remote::server::{apply_arrival, apply_event, apply_outcome, released};
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};
    use tokio_stream::StreamExt as _;

    let mut keys = EventStream::new();
    let mut ticks = tokio::time::interval(TICK);
    // raw mode makes `ctrl+c` a key, so these are the only signals this loop is sent
    let mut terminations =
        kamchatka::stopping::Terminated::new().context("could not listen for a request to end")?;
    // note: a frame only when something changed. Drawing is the most expensive thing this loop
    // does: drawn on every tick, an idle session with a long chat kept a core busy; drawn once per
    // event, a stream arriving faster than frames fell behind the broadcast and lost events. Every
    // branch below but the tick can change something, so every branch but the tick says so
    let mut stale = true;
    let mut quiet_ticks = 0;

    loop {
        // before the frame, so that what a client is told and what the screen shows are one look at
        // the `App` rather than two. Nothing here draws
        stale |= released(app, serving.as_deref_mut()).await;
        if stale {
            terminal.draw(|frame| ui::draw(frame, app))?;
            (stale, quiet_ticks) = (false, 0);
        }
        // after the frame rather than before it, so the hand-over line is on the screen by the time
        // the terminal has it
        if let Some(text) = app.clipboard.take()
            && let Err(why) = kamchatka::clipboard::hand_over(&text)
        {
            app.not_copied(&why);
            stale = true;
        }
        if app.leaving() {
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
            event = events.recv() => {
                // with whatever else is already queued behind it, so that one frame shows all of
                // it. Only what is queued now: a stream that keeps coming still gets a frame
                let queued = events.len();
                let mut event = event;
                for _ in 0..=queued {
                    if !apply_event(app, event) {
                        return Ok(());
                    }
                    match events.try_recv() {
                        Ok(next) => event = Ok(next),
                        Err(TryRecvError::Lagged(missed)) => event = Err(RecvError::Lagged(missed)),
                        Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                    }
                }
            },
            Some(outcome) = finished.recv() => apply_outcome(app, events, outcome),
            // and the two that exist only when this session is also served. They are the same pair
            // `remote::Server::run` selects on, in a loop that happens to have a screen
            arrived = async {
                match &server {
                    Some(server) => server.arrived().await,
                    None => std::future::pending().await,
                }
            } => {
                // the two are made together and there is no listener without one, so a connection
                // with nobody serving is a shape the types ask for and nothing reaches. Dropping
                // it is what that would mean if anything ever did
                if let Some(serving) = &mut serving {
                    apply_arrival(serving, app, arrived);
                }
            }
            Some(ask) = async {
                match &mut serving {
                    Some(serving) => serving.asked().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(serving) = &mut serving
                    && !serving.answered(app, ask, events).await
                {
                    return Ok(());
                }
            }
            // a served session's own, which was subscribed before its socket existed
            _ = async {
                match &server {
                    Some(server) => {
                        server.terminated().await;
                    }
                    None => terminations.arrived().await,
                }
            } => app.quit = true,
            _ = ticks.tick() => {
                // note: on the tick as well as on an event, because a provider and an advisor
                // produce notices when nothing is happening - a retry's wait, a local advisor
                // loading a checkpoint before the first question - and no event carries those
                stale |= app.take_notices();
                // two things move on screen without announcing themselves: the busy line's clock,
                // and a question a running command asks at the network gate, read from
                // `Careful::reaching` rather than arriving as an event. An advisor's rating is not
                // one of them: the kernel awaits the policy before raising a question, so the
                // rating is there before the panel is
                quiet_ticks += 1;
                stale |= app.busy || app.asking() || quiet_ticks >= HEARTBEAT;
                continue;
            }
        }
        stale = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four ways a run can end up with or without a screen.
    ///
    /// note: the third case is the one no other test reaches. A build with no `tui` has nothing
    /// to draw with, so a run of it is headless whatever stdout is; without that, it panics. Every
    /// piped test in this crate covers the other three, and a test that pipes stdout cannot reach
    /// this one.
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
