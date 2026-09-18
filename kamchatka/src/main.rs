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
    sync::Arc,
};

use anyhow::{Context as _, Result};
use clap::{CommandFactory as _, FromArgMatches as _, Parser, ValueEnum as _};
#[cfg(feature = "tui")]
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste, Event as TerminalEvent, EventStream},
    execute,
};
use nachalnik::Grant;
use nachalnik_providers::Dialect;

use kamchatka::{
    app::App,
    config::Settings,
    endpoint, headless, sandbox,
    tools::Subject,
    wiring::{Setup, Wired},
};
// the drawing loop's own: the two channel payloads it has to name in a signature, and the screen
#[cfg(feature = "tui")]
use kamchatka::{
    app::{Outcome, Speaker},
    ui,
};
#[cfg(feature = "tui")]
use nachalnik::Event;

/// How often the screen is redrawn when nothing at all is happening.
#[cfg(feature = "tui")]
const TICK: std::time::Duration = std::time::Duration::from_millis(120);

/// A terminal agent that shows you its context.
///
/// note: the three environment variables below are read by [`provider`] rather than declared as
/// arguments, so clap cannot list them the way it lists `KAMCHATKA_MODEL` beside `--model`. A
/// setting nothing on the screen mentions is a setting nobody finds, and `--help` is where a
/// person looks for the list.
/// The variables `--help` lists, and what each of them is for.
///
/// note: a function rather than the literal it was, so that the advisor's three are listed by a
/// build that has an `--advise` to use them and by no other. A variable named in the help of a
/// program that reads it nowhere is the same failure as a settings key nothing consults.
fn environment() -> String {
    /// The advisor's three, listed by a build that has an `--advise` and empty in one that does
    /// not.
    #[cfg(feature = "advise")]
    const ADVISOR: &str = "

The advisor, which is only ever asked when --advise is given:
  KAMCHATKA_TYPESAFE_API_KEY  its key; or TYPESAFE_API_KEY. No fallback to the above -
                              it is a different service, and a different account
  KAMCHATKA_TYPESAFE_BASE_URL where its questions go; TypeSafe's own by default
  KAMCHATKA_TYPESAFE_MODEL    which model answers them; jev-latest by default";
    #[cfg(not(feature = "advise"))]
    const ADVISOR: &str = "";

    format!(
        "\
Environment:
  KAMCHATKA_API_KEY        the key; or OPENROUTER_API_KEY, or OPENAI_API_KEY
  KAMCHATKA_BASE_URL       where the requests go, e.g. http://localhost:11434/v1 for ollama;
                           OpenRouter by default, or Google's own v1beta with --gemini
  KAMCHATKA_CONTEXT_LIMIT  the model's context size, for a provider that will not say
  KAMCHATKA_NO_ATTRIBUTION set to stop naming this program to OpenRouter{ADVISOR}"
    )
}

#[derive(Parser)]
#[command(version, about, long_about = None, after_help = environment())]
struct Args {
    /// A first message, sent as soon as it starts.
    message: Vec<String>,

    /// The model to talk to. [default: openai/gpt-4o-mini, or gemini-3.6-flash with --gemini]
    #[arg(short, long, env = "KAMCHATKA_MODEL")]
    model: Option<String>,

    /// Talk to Google's own API rather than an OpenAI-compatible one, so that a turn keeps the
    /// order it was produced in: thinking, a sentence, a tool call, more thinking.
    #[arg(long)]
    gemini: bool,

    /// Ask a second model about every tool call the rules were going to allow, and take the
    /// stricter of the two answers. It can refuse a call and never permit one. Sends the call's
    /// tool name, capabilities and arguments to TypeSafe; see KAMCHATKA_TYPESAFE_API_KEY.
    #[cfg(all(feature = "advise", not(feature = "assisted-shell")))]
    #[arg(long)]
    advise: bool,

    /// The same, and this build also asks it where each command a question is about lands on a
    /// three-level rubric, and colours the question by the answer. The rating decides nothing.
    /// That sends every command the model writes to TypeSafe, and not only the allowed ones.
    #[cfg(feature = "assisted-shell")]
    #[arg(long)]
    advise: bool,

    /// A file to put in the context, pinned; a PDF or an image goes in as itself. May be
    /// repeated, and `/attach` is the same thing at the prompt.
    #[arg(short, long, value_name = "PATH")]
    file: Vec<String>,

    /// A system instruction. The runtime ships none of its own.
    #[arg(short, long, value_name = "TEXT")]
    system: Option<String>,

    /// Carry on from a session written by `/save`.
    #[arg(short, long, value_name = "PATH")]
    resume: Option<String>,

    /// An MCP server to run, and offer the tools of, as `[name=]command`. May be repeated.
    #[cfg(feature = "mcp")]
    #[arg(long, value_name = "COMMAND")]
    mcp: Vec<String>,

    /// How many requests one turn may make before it stops and asks; `0` is no limit at all.
    #[arg(long, value_name = "N", default_value_t = 8)]
    requests: usize,

    /// How full the context may get before the oldest tool results are elided to a marker they
    /// can be restored from; `1` never does.
    #[arg(long, value_name = "FRACTION", default_value_t = 0.8)]
    compact: f64,

    /// Let the model's tool calls run at the same time, instead of in the order it asked.
    #[arg(long)]
    parallel: bool,

    /// Run with no confinement at all: the shell reaches whatever the user running this can, and
    /// `fs`, which is not a process, stops holding itself to the working directory.
    #[arg(long)]
    no_sandbox: bool,

    /// Do not write the session out when it ends. It is written to a temporary directory
    /// otherwise, and the path is the last thing printed.
    #[arg(long)]
    no_record: bool,

    /// A path outside the working directory the tools may also read and write. May be repeated,
    /// and takes a comma-separated list.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    sandbox_allow: Vec<std::path::PathBuf>,

    /// A path outside the working directory the tools may read but not change. The same, and a
    /// toolchain is the usual one: `cargo` cannot start without `~/.rustup`.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    sandbox_read: Vec<std::path::PathBuf>,

    /// Drop the whole of a tool's output once it has been shortened, rather than keeping it as an
    /// archived item that can still be read.
    #[arg(long)]
    forget_truncated: bool,

    /// Send a request the counter puts over the model's context anyway, and let the endpoint be
    /// the one that says no. For a limit that is advertised wrongly, or a counter that is.
    #[arg(long)]
    send_oversized: bool,

    /// Drive the session from lines on stdin instead of from a screen: the session log goes to
    /// stdout, one JSON record per line, and what the model says goes to stderr. Implied when
    /// stdout is not a terminal.
    #[arg(long)]
    headless: bool,

    /// Allow a whole domain, one operation in one, or a path, as `fs`, `fs:read`, `exec:run`,
    /// `.env*`. May be repeated, and takes a comma-separated list. Answering at the prompt writes
    /// the same table.
    #[arg(long, value_name = "SUBJECT", value_delimiter = ',')]
    allow: Vec<String>,

    /// Refuse one, the same way. The strictest of everything consulted wins, so this beats
    /// `--allow`.
    #[arg(long, value_name = "SUBJECT", value_delimiter = ',')]
    deny: Vec<String>,

    /// Allow every tool an MCP server offers, by the name it was given. Its own argument because
    /// a server name and a domain are both bare words and nothing in either says which it is.
    #[arg(long, value_name = "NAME", value_delimiter = ',')]
    allow_server: Vec<String>,

    /// Refuse one, the same way.
    #[arg(long, value_name = "NAME", value_delimiter = ',')]
    deny_server: Vec<String>,

    /// What to do with a question nobody is there to answer, in a headless run.
    #[arg(long, value_name = "ANSWER", default_value = "deny")]
    on_ask: OnAsk,

    /// Stop a headless run after this many seconds, however far it has got. What has arrived is
    /// kept and the session is written out as usual.
    #[arg(long, value_name = "SECONDS")]
    deadline: Option<u64>,

    /// Stop the session once the provider has charged this many tokens for it, in and out. Time is
    /// not the only thing a run nobody is watching can spend; `/spend` changes it while it runs.
    #[arg(long, value_name = "TOKENS")]
    spend: Option<u64>,

    /// A JSON file of settings, for the ones you would otherwise type every time. Anything given
    /// here on the command line wins over what it says.
    #[arg(long, value_name = "PATH")]
    config_file: Option<std::path::PathBuf>,

    /// The frame's colour, which only a settings file can say; see `Settings::border`.
    ///
    /// note: `skip` rather than an argument nobody would type twice, and it lives on `Args` all
    /// the same so that the one merge in `under` stays the one place the file meets the command
    /// line. A second path from a settings file to the program is a second set of rules about
    /// which wins.
    #[arg(skip)]
    border: Option<String>,

    /// Which tools to offer at startup, which only a settings file can say; see `Settings::tools`.
    ///
    /// note: `skip` for the reason above, and for one of its own. This was `--introspect`, a flag
    /// for four of the tools, and what it was really being used for was a project where those four
    /// were worth the tokens - which is a fact about the project and belongs in the file beside it
    /// rather than in front of somebody's hands. What it was *also* used for was turning them off
    /// for one session, and that is `/tools toggle` now, at the moment somebody wants it rather
    /// than before the session starts.
    #[arg(skip)]
    tools: Option<Vec<String>>,
}

impl Args {
    /// Fills in everything the command line did not say from a settings file.
    ///
    /// note: it asks clap which arguments were *typed*, rather than comparing against the
    /// defaults, because those are not the same question: `--requests 8` is the default value and
    /// somebody meant it, and a merge that could not tell them apart would let a file quietly
    /// override what was asked for. `ValueSource::CommandLine` is the only answer that counts, and
    /// it is why `session` parses the matches as well as the struct.
    ///
    /// note: a list from the command line *replaces* the file's rather than adding to it. One rule
    /// for every key is the only kind anybody can predict, and the other way round there is no way
    /// to ask for fewer than the file says.
    fn under(mut self, settings: Settings, matches: &clap::ArgMatches) -> Result<Self> {
        let typed =
            |name: &str| matches.value_source(name) == Some(clap::parser::ValueSource::CommandLine);
        // one macro rather than eighteen `if`s, and it names the argument once: the string clap
        // knows it by is the field's own name, so a field renamed without its entry here stops
        // compiling rather than stopping working
        macro_rules! fill {
            ($($field:ident),* $(,)?) => {$(
                if !typed(stringify!($field)) && let Some(value) = settings.$field {
                    self.$field = value.into();
                }
            )*};
        }

        fill!(
            gemini,
            requests,
            compact,
            parallel,
            no_sandbox,
            sandbox_allow,
            sandbox_read,
            allow,
            deny,
            allow_server,
            deny_server,
            deadline,
            spend,
            forget_truncated,
            send_oversized,
            no_record,
        );
        // the four that are not a plain assignment: two are already `Option`, one is a list this
        // build may not have, and one is a word that has to be a `ValueEnum`
        //
        // note: these two ask whether the field is empty rather than whether it was typed, and for
        // `model` that is a decision rather than a shorthand. It is the one argument with an
        // environment variable behind it, so "not typed" and "not set" differ - and a set
        // `KAMCHATKA_MODEL` should beat a file the way a typed `-m` does. Command line, then
        // environment, then file, which is the order everything else in this workspace reads in
        if self.model.is_none() {
            self.model = settings.model;
        }
        if self.system.is_none() {
            self.system = settings.system;
        }
        // read here rather than where it is drawn, so that a colour nobody can parse is a startup
        // error naming the file rather than a frame that is quietly still yellow. There is no
        // argument to lose to, so there is nothing to ask `typed` about
        if let Some(border) = &settings.border {
            kamchatka::config::rgb(border)
                .map_err(|e| anyhow::anyhow!("`border` in the settings file: {e}"))?;
            self.border = settings.border;
        }
        // the other one with no argument to lose to. Checking the names is `Setup::wire`'s, since
        // the tools it would be checking against are the ones it is about to build
        self.tools = settings.tools;
        if let Some(on_ask) = settings.on_ask.filter(|_| !typed("on_ask")) {
            self.on_ask = OnAsk::from_str(&on_ask, true)
                .map_err(|e| anyhow::anyhow!("`on-ask` in the settings file: {e}"))?;
        }
        match settings.mcp {
            #[cfg(feature = "mcp")]
            Some(mcp) if !typed("mcp") => self.mcp = mcp,
            // note: a *server* that cannot be run is worth refusing over; an empty list asks for
            // nothing and is honoured by doing nothing. The crate's own `kamchatka.json` carries
            // every key, `mcp` among them, so the stricter rule made the shipped starting point
            // unusable in the one build that has no MCP - which is how this was found
            #[cfg(not(feature = "mcp"))]
            Some(mcp) if !mcp.is_empty() => anyhow::bail!(
                "this build has no MCP support, so `mcp` in the settings file cannot be honoured"
            ),
            _ => {}
        }

        // the same rule for the same reason, and it matters more here: a settings file that asked
        // for a second opinion on every tool call, in a build with no advisor in it, would run
        // the session with its permissions decided by the heuristic alone and say nothing
        match settings.advise {
            #[cfg(feature = "advise")]
            Some(advise) if !typed("advise") => self.advise = advise,
            #[cfg(not(feature = "advise"))]
            Some(true) => anyhow::bail!(
                "this build has no advisor in it, so `advise` in the settings file cannot be \
                 honoured"
            ),
            _ => {}
        }

        Ok(self)
    }
}

/// What an unanswerable question is answered with.
///
/// note: `deny` is the default, and it is the whole of the difference between this and
/// `examples/recorded.rs`, which grants every question it is asked. That is right for a recording
/// somebody is watching and wrong for a program: a run nobody is watching should not be able to do
/// a thing nobody has allowed, and `--allow exec` is one flag away for anyone who means it.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OnAsk {
    /// Refuse it. The model is told, and told that it was this call rather than a standing rule.
    Deny,
    /// Grant it.
    Allow,
}

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

/// The program proper: wired the same way whichever of the two drives it.
async fn session() -> Result<()> {
    // note: the matches as well as the struct, because the settings file needs to know which
    // arguments were typed and the struct cannot say - a value that equals its default and a value
    // somebody wrote out are the same field. See `Args::under`
    let matches = Args::command().get_matches();
    let mut args = Args::from_arg_matches(&matches)
        .map_err(|e| e.exit())
        .unwrap();
    if let Some(path) = args.config_file.clone() {
        let settings = Settings::read(&path).map_err(|e| anyhow::anyhow!("{e}"))?;
        args = args.under(settings, &matches)?;
    }
    // the screen is drawn to stdout, so a stdout that is nobody's terminal cannot have one. It is
    // announced rather than silently chosen: a program that draws or does not draw depending on
    // what is on the other end of a pipe should say which it decided, and `--headless` is how
    // somebody says it themselves
    let piped = !std::io::stdout().is_terminal();
    let headless = headless(args.headless, piped);
    if headless && !args.headless {
        match piped {
            true => eprintln!("· stdout is not a terminal, so this is a headless run"),
            false => eprintln!("· built without the `tui` feature, so this is a headless run"),
        }
    }

    // note: the reading of the file is here rather than in `Setup`, because the two error
    // messages worth writing - which path, and whether it was a session at all - belong to
    // whoever was handed the path
    let resume = match &args.resume {
        Some(path) => Some(
            serde_json::from_slice(
                &std::fs::read(path).with_context(|| format!("could not read {path}"))?,
            )
            .with_context(|| format!("{path} is not a session"))?,
        ),
        None => None,
    };

    let setup = Setup {
        resume,
        // the runtime's own default is a counter that restarts at 1 with the process, which is
        // fine as an identity and useless as a filename: every session would write over the last
        // one's record. A resumed session keeps the name in its snapshot, so carrying on appends
        // to the same session rather than starting a second one that looks unrelated
        session_name: args.resume.is_none().then(|| {
            let started = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_secs())
                .unwrap_or_default();

            App::session_stamp(started)
        }),
        requests: (args.requests > 0).then_some(args.requests),
        parallel: args.parallel,
        // what the runtime keeps is a decision about retention, and retention here is a file
        // somebody has to store: `/save` writes the snapshot, and an archived output goes into it
        // whole. One `grep` that wandered into `./target/` is 11MB of build noise nobody will
        // read, and it is in every save of that session from then on
        keep_truncated: !args.forget_truncated,
        refuse_oversized: !args.send_oversized,
        compact: Some(args.compact),
        spend: args.spend,
        confine: !args.no_sandbox,
        reachable: args.sandbox_allow.clone(),
        readable: args.sandbox_read.clone(),
        allow: args
            .allow
            .iter()
            .map(|it| Subject::parse(it))
            .chain(args.allow_server.iter().cloned().map(Subject::Server))
            .collect(),
        deny: args
            .deny
            .iter()
            .map(|it| Subject::parse(it))
            .chain(args.deny_server.iter().cloned().map(Subject::Server))
            .collect(),
        tools: args.tools.clone(),
        system: args.system.clone(),
        files: args.file.clone(),
        #[cfg(feature = "advise")]
        advisor: None,
    };

    // note: before the provider, which is a round trip and an API key away. Everything `check`
    // answers is answerable from the arguments alone - a tool nobody offers, a path rule nothing
    // can match - and being told about one of those by an endpoint's refusal to talk is being
    // told about the wrong thing. `wire` asks it again for whoever is not `main`
    setup.check().map_err(|e| anyhow::anyhow!("{e}"))?;

    // note: after `check` and before the provider, and the order is the point. A missing advisor
    // key is a fact about the arguments and should not cost a round trip to the *other* endpoint
    // to find out about; and a session that was asked for with `--advise` and could not reach an
    // advisor is refused rather than quietly run with its permissions decided by the heuristic
    // alone. Somebody who turned this on is entitled to have it on or be told it is not
    #[cfg(feature = "advise")]
    let setup = match args.advise {
        false => setup,
        true => {
            let jev = endpoint::advise::connect()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("could not reach the advisor")?;
            // what the probe had to say about the model it was asked for - a name the endpoint
            // does not serve, most likely. On stderr because there is no screen yet and this is
            // the last moment it can be read before one covers it
            if let Some(notice) = jev.take_notice() {
                eprintln!("advisor: {notice}");
            }

            Setup {
                advisor: Some(jev),
                ..setup
            }
        }
    };

    // two wire formats, one trait. `--gemini` is what a person picks, and everything downstream -
    // the kernel, the screen, `/model`, `/provider` - is written against `Endpoint` and never
    // finds out which one it got
    let model = args.model.clone().unwrap_or_else(|| {
        match args.gemini {
            true => "gemini-3.6-flash",
            false => "openai/gpt-4o-mini",
        }
        .to_owned()
    });
    let provider: Arc<dyn Dialect> = match args.gemini {
        true => endpoint::gemini::connect(&model)
            .await
            .map(|it| it as Arc<dyn Dialect>),
        false => endpoint::connect(&model)
            .await
            .map(|it| it as Arc<dyn Dialect>),
    }
    .map_err(|e| anyhow::anyhow!("{e}"))
    .context("could not reach the model")?;

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

    let on_ask = match args.on_ask {
        OnAsk::Deny => Grant::Deny,
        OnAsk::Allow => Grant::Allow,
    };
    // note: three statements rather than one match over the pair, because a resumed headless run
    // wants both of the first two and the match gave it one. The replay line says what was picked
    // up; the opening says how this run is driven and what a question nobody can answer gets,
    // which a session carried on from a file needs to know exactly as much as a fresh one
    if headless {
        headless::opening(&mut app, on_ask);
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
    #[cfg(feature = "tui")]
    if !headless && args.resume.is_none() {
        app.say(Speaker::Note, ui::GREETING);
    }
    if let Some(message) = (!args.message.is_empty()).then(|| args.message.join(" ")) {
        app.ask(&message);
        app.start_turn();
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
        false => drawn(&mut app, &mut events, &mut finished).await,
        #[cfg(not(feature = "tui"))]
        false => unreachable!("there is no screen in this build"),
    };

    // note: the headless driver has ended the session itself, so that the record saying so goes
    // down the stream with the rest rather than being the one nobody was sent. `Kernel::finish`
    // emits an event every time it is called, so this is an either-or rather than a belt and
    // braces
    if !headless {
        app.kernel.finish();
    }
    // note: on stderr in a headless run, because stdout is the log. A line of prose in the middle
    // of a stream of JSON is the one thing that would make it unparseable, and this is the last
    // line written - so it would be the one nobody noticed until a reader fell over the end
    let ending = format!(
        "{} · {} events recorded",
        app.kernel.session_name(),
        app.kernel.history().len()
    );
    match headless {
        true => eprintln!("{ending}"),
        false => println!("{ending}"),
    }
    // the record was only ever written if somebody thought to type `/save`, which is exactly the
    // wrong condition: a session that ended badly is the one worth reading afterwards, and it was
    // the one that left nothing. Nine runs against a provider that timed out left no trace of how
    // far any of them had got
    if !args.no_record {
        match record(&app) {
            Ok((records, log, state)) => {
                let where_it_went = format!(
                    "{records} records in {log}, and a session in {state}\n\
                     `kamchatka -r {state}` carries on from it"
                );
                match headless {
                    true => eprintln!("{where_it_went}"),
                    false => println!("{where_it_went}"),
                }
            }
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
) -> Result<()> {
    // ratatui installs a hook of its own that restores the terminal and then calls this one
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableBracketedPaste);
        previous(info);
    }));

    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    let outcome = run(&mut terminal, app, events, finished).await;
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();

    outcome
}

/// Draws, waits for whichever of the three things happens first, and does it again.
#[cfg(feature = "tui")]
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    finished: &mut tokio::sync::mpsc::UnboundedReceiver<Outcome>,
) -> Result<()> {
    use tokio::sync::broadcast::error::RecvError;
    use tokio_stream::StreamExt as _;

    let mut keys = EventStream::new();
    let mut ticks = tokio::time::interval(TICK);

    loop {
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
