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
    provider, tools, ui,
};

/// How often the screen is redrawn when nothing at all is happening.
const TICK: Duration = Duration::from_millis(120);

/// A terminal agent that shows you its context.
#[derive(Parser)]
#[command(version, about, long_about = None)]
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
}

#[tokio::main]
async fn main() -> Result<()> {
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

    let policy = Arc::new(tools::Careful::new());
    kernel.set_provider(provider.clone());
    kernel.set_policy(policy.clone());
    if args.compact < 1.0 {
        kernel.set_compactor(Some(Arc::new(tools::Trim {
            threshold: args.compact,
            target: (args.compact - 0.2).max(0.1),
        })));
    }
    for tool in tools::builtin() {
        kernel.add_tool(tool);
    }

    // the servers have to outlive this scope: dropping one takes its child process, and its
    // tools, with it
    #[cfg(feature = "mcp")]
    let _servers = attach_mcp(&kernel, &args.mcp).await?;

    let mut events = kernel.subscribe();
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
    match args.resume.is_some() {
        // a resumed session has a conversation in it already, and it would be strange to have to
        // read it out of the context pane one item at a time
        true => app.replay(),
        false => app.say(
            Speaker::Note,
            "tab moves to the context · F1 lists the keys · ctrl+p shows the next request",
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
                Some(Ok(TerminalEvent::Paste(text))) => { app.input.insert_str(text); }
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
            Some(outcome) = finished.recv() => app.on_outcome(outcome),
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
