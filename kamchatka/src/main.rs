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

use std::{io::stdout, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use clap::Parser;
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste, Event as TerminalEvent, EventStream},
    execute,
};
use nachalnik::{Config, ContextItem, Event, Kernel};
use ratatui::DefaultTerminal;
use tokio::sync::{broadcast::error::RecvError, mpsc};
use tokio_stream::StreamExt;

use kamchatka::{
    app::{App, Outcome, Speaker},
    mind, provider, sandbox, tools, ui,
};

/// How often the screen is redrawn when nothing at all is happening.
const TICK: Duration = Duration::from_millis(120);

/// A terminal agent that shows you its context.
///
/// note: the three environment variables below are read by [`provider`] rather than declared as
/// arguments, so clap cannot list them the way it lists `KAMCHATKA_MODEL` beside `--model`. A
/// setting nothing on the screen mentions is a setting nobody finds, and `--help` is where a
/// person looks for the list.
#[derive(Parser)]
#[command(
    version,
    about,
    long_about = None,
    after_help = "\
Environment:
  KAMCHATKA_API_KEY        the key; or OPENROUTER_API_KEY, or OPENAI_API_KEY
  KAMCHATKA_BASE_URL       where the requests go, e.g. http://localhost:11434/v1 for ollama;
                           OpenRouter by default
  KAMCHATKA_CONTEXT_LIMIT  the model's context size, for a provider that will not say"
)]
struct Args {
    /// A first message, sent as soon as it starts.
    message: Vec<String>,

    /// The model to talk to.
    #[arg(
        short,
        long,
        env = "KAMCHATKA_MODEL",
        default_value = "openai/gpt-4o-mini"
    )]
    model: String,

    /// A file to put in the context, pinned. May be repeated.
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

    /// How many requests one turn may make before it stops and asks.
    #[arg(long, value_name = "N", default_value_t = 8)]
    requests: usize,

    /// How full the context may get before the oldest tool results are dropped; `1` never does.
    #[arg(long, value_name = "FRACTION", default_value_t = 0.8)]
    compact: f64,

    /// Let the model's tool calls run at the same time, instead of in the order it asked.
    #[arg(long)]
    parallel: bool,

    /// Run the shell tool with no sandbox, reaching whatever the user running this can reach.
    #[arg(long)]
    no_sandbox: bool,

    /// Offer the model the two tools that read and change its own context: `mind` and `amend`.
    #[arg(long)]
    mind: bool,

    /// A path outside the working directory the shell tool may also read and write. May be
    /// repeated.
    #[arg(long, value_name = "PATH")]
    sandbox_allow: Vec<std::path::PathBuf>,
}

fn main() -> Result<()> {
    // before anything else, and before a runtime exists: this is the mode the `shell` tool
    // re-executes this program in, and its whole job is to confine itself and run one command.
    // Landlock restricts the calling thread, so the one shape that needs no thought about which
    // thread that was is a program with only one
    if let Some(code) = sandbox::run_if_asked() {
        std::process::exit(code);
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(terminal())
}

/// The program proper.
async fn terminal() -> Result<()> {
    let args = Args::parse();

    let provider = provider::connect(&args.model)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("could not reach the model")?;

    let config = Config {
        max_requests_per_turn: (args.requests > 0).then_some(args.requests),
        parallel_tool_calls: args.parallel,
        ..Default::default()
    };
    let kernel = match &args.resume {
        Some(path) => {
            let snapshot = serde_json::from_slice(
                &std::fs::read(path).with_context(|| format!("could not read {path}"))?,
            )
            .with_context(|| format!("{path} is not a session"))?;
            Kernel::resume(config, snapshot)
        }
        None => Kernel::new(config),
    };

    // note: subscribed before anything is plugged in, so that the wiring is on the trace like
    // everything else. Setting the provider, the policy, the compactor and each tool are all
    // events, and a screen that started listening afterwards drew a session whose first few facts
    // were only in the log
    let mut events = kernel.subscribe();

    let policy = Arc::new(tools::Careful::new());
    kernel.set_provider(provider.clone());
    kernel.set_policy(policy.clone());
    if args.compact < 1.0 {
        kernel.set_compactor(Some(Arc::new(tools::Trim {
            threshold: args.compact,
            target: (args.compact - 0.2).max(0.1),
        })));
    }
    // settled once, here, rather than asked per command: see the note on `Shell::confiner`
    let program = std::env::current_exe()?;
    let confinement = match args.no_sandbox {
        true => sandbox::Confinement::Unsupported,
        false => sandbox::available(&program),
    };
    let reach = sandbox::Reach {
        workdir: std::env::current_dir()?,
        extra: args.sandbox_allow.clone(),
        confined: !args.no_sandbox,
    };
    for tool in tools::builtin(
        tools::Shell {
            policy: policy.clone(),
            workdir: reach.workdir.clone(),
            extra: reach.extra.clone(),
            // only when it would actually confine anything. A binary that has been replaced since
            // this one started, or a kernel with no Landlock, is a `shell` that runs unconfined
            // and a permissions tab that says so - rather than one whose every command comes back
            // with an error nobody can account for
            confiner: confinement.is_confined().then(|| program.clone()),
        },
        reach,
    ) {
        kernel.add_tool(tool);
    }

    // the handle the two tools reach the kernel through, which `App` then holds so that `/mind`
    // can turn them off again; see `mind::install` for why it is a weak handle to something out
    // here rather than a kernel the tools hold
    let mind = args.mind.then(|| mind::install(&kernel));

    // the servers have to outlive this scope: dropping one takes its child process, and its
    // tools, with it
    #[cfg(feature = "mcp")]
    let _servers = attach_mcp(&kernel, &args.mcp).await?;

    if let Some(system) = &args.system {
        kernel.push(ContextItem::system(system.clone()).pinned());
    }
    for path in &args.file {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("could not read {path}"))?;
        kernel.push(
            ContextItem::file(path, content)
                .because("named on the command line")
                .pinned(),
        );
    }

    let (outcomes, mut finished) = mpsc::unbounded_channel();
    let mut app = App::new(kernel, policy, provider, outcomes);
    app.confinement = confinement;
    app.mind = mind;
    match args.resume.is_some() {
        // a resumed session has a conversation in it already, and it would be strange to have to
        // read it out of the context pane one item at a time
        true => app.replay(),
        false => app.say(
            Speaker::Note,
            "tab moves to the context · ctrl+t swaps it for the trace · ctrl+p shows the \
             next request · F1 lists the keys",
        ),
    }
    if let Some(message) = (!args.message.is_empty()).then(|| args.message.join(" ")) {
        app.say(Speaker::User, &message);
        app.kernel.push(ContextItem::user(message));
        app.start_turn();
    }

    // ratatui installs a hook of its own that restores the terminal and then calls this one
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableBracketedPaste);
        previous(info);
    }));

    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    let outcome = run(&mut terminal, &mut app, &mut events, &mut finished).await;
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();

    app.kernel.finish();
    println!(
        "{} · {} events recorded",
        app.kernel.session_name(),
        app.kernel.history().len()
    );

    outcome
}

/// Draws, waits for whichever of the three things happens first, and does it again.
async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    finished: &mut mpsc::UnboundedReceiver<Outcome>,
) -> Result<()> {
    let mut keys = EventStream::new();
    let mut ticks = tokio::time::interval(TICK);

    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
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

/// Starts the MCP servers that were asked for, and puts their tools in the same list as the
/// built-in ones.
///
/// note: The name matters more than it looks: it prefixes every tool the server offers and it is
/// what "always, for `mcp:<name>`" grants permission to. Taken from the program it would be
/// `npx` or `python3` for most of the servers people actually run, so `name=command` is accepted
/// and worth using.
#[cfg(feature = "mcp")]
async fn attach_mcp(kernel: &Kernel, commands: &[String]) -> Result<Vec<nachalnik_mcp::Server>> {
    let mut servers = Vec::new();

    for spec in commands {
        // `env FOO=bar cmd` is a command rather than a name, which is what the guard is for
        let (name, line) = match spec.split_once('=') {
            Some((name, rest))
                if !name.is_empty()
                    && !name.contains(char::is_whitespace)
                    && !name.contains('/') =>
            {
                (name.to_owned(), rest.trim())
            }
            _ => (String::new(), spec.as_str()),
        };

        let mut words = line.split_whitespace();
        let program = words.next().context("--mcp needs a command to run")?;
        let name = match name.is_empty() {
            false => name,
            true => std::path::Path::new(program)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(program)
                .to_owned(),
        };

        let mut command = tokio::process::Command::new(program);
        command.args(words);

        let server = nachalnik_mcp::Server::spawn(name, command)
            .await
            .with_context(|| format!("`{line}` did not answer the handshake"))?;
        server
            .install(kernel)
            .await
            .with_context(|| format!("`{line}` would not list its tools"))?;
        servers.push(server);
    }

    Ok(servers)
}
