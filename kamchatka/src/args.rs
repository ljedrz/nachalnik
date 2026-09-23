//! The command line: what the program accepts, and what a session made of it looks like.
//!
//! note: in the library rather than in `main.rs`. A session assembled from these arguments is the
//! thing this crate is for, and `examples/phone.rs` builds one too - so a second, smaller
//! vocabulary in the example would be two sets of flags to keep in step and one of them always
//! behind.
//!
//! note: what `main.rs` keeps is the part that is the program's own: which loop drives the session,
//! where the record is written, and the refusal that `--connect` assembles nothing.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{CommandFactory as _, FromArgMatches as _, Parser, ValueEnum as _};
use nachalnik::Grant;
use nachalnik_providers::Dialect;

use crate::{
    app::App,
    config::{self, Settings},
    endpoint,
    tools::Subject,
    wiring::Setup,
};

/// The variables `--help` lists, and what each of them is for.
///
/// note: they are read by [`Args::provider`] rather than declared as arguments, so clap cannot
/// list them the way it lists `KAMCHATKA_MODEL` beside `--model`. A setting nothing on the screen
/// mentions is a setting nobody finds, and `--help` is where a person looks for the list.
///
/// note: a function rather than a literal, so that the advisor's three are listed by a build that
/// has an `--advise` to use them and by no other. A variable named in the help of a program that
/// reads it nowhere is the same failure as a settings key nothing consults.
pub fn environment() -> String {
    /// The advisor's three, listed by a build that has an `--advise` and empty in one that does
    /// not.
    #[cfg(feature = "shell-advisor")]
    const ADVISOR: &str = "

The advisor, which is only ever asked when --advise is given:
  SYSTEM1_ADVISOR_COMMAND     an engine to run on this machine, as a command line.
                              Takes precedence over the three below, and nothing
                              leaves the machine when it is set
  KAMCHATKA_SYSTEM1_API_KEY   its key; or TYPESAFE_API_KEY. Without one it borrows
                              KAMCHATKA_API_KEY, but only where this session already
                              talks to OpenRouter, which serves jev too
  KAMCHATKA_SYSTEM1_BASE_URL  where its questions go; the endpoint of whichever of
                              those two keys was found, or any other service that
                              answers the same typed questions
  KAMCHATKA_SYSTEM1_MODEL     which model answers them; jev-latest at TypeSafe,
                              typesafe/jev-1.13 through OpenRouter";
    #[cfg(not(feature = "shell-advisor"))]
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

/// A terminal agent that shows you its context.
///
/// note: `#[non_exhaustive]`, because this is answered with rather than built: [`Args::given`] is
/// where one comes from, and a flag added later should be a patch rather than a break for anybody
/// assembling a session of their own out of it.
#[derive(Parser)]
#[non_exhaustive]
#[command(version, about, long_about = None, after_help = environment())]
pub struct Args {
    /// A first message, sent as soon as it starts.
    pub message: Vec<String>,

    /// The model to talk to. Without one the session starts with none and sends nothing until
    /// `/model` picks one; `/models` lists what the endpoint serves.
    #[arg(short, long, env = "KAMCHATKA_MODEL")]
    pub model: Option<String>,

    /// Talk to Google's own API rather than an OpenAI-compatible one, so that a turn keeps the
    /// order it was produced in: thinking, a sentence, a tool call, more thinking.
    #[arg(long)]
    pub gemini: bool,

    /// Ask a second model where each shell command a question is about lands on a three-level
    /// rubric, and colour the question by the answer - a command joined at its `|`, `&&` or `;`
    /// is asked about stage by stage and rated by its worst one, which is underlined. The rating
    /// decides nothing: what the rules allow runs and what they refuse is refused. Sends the
    /// call's tool name, capabilities and arguments to the advisor; see SYSTEM1_ADVISOR_COMMAND
    /// for one on this machine, where nothing is sent, and KAMCHATKA_SYSTEM1_API_KEY for the
    /// hosted default.
    #[cfg(feature = "shell-advisor")]
    #[arg(long)]
    pub advise: bool,

    /// A file to put in the context, pinned; a PDF or an image goes in as itself. May be
    /// repeated, and `/attach` is the same thing at the prompt.
    #[arg(short, long, value_name = "PATH")]
    pub file: Vec<String>,

    /// A system instruction. The runtime ships none of its own.
    #[arg(short, long, value_name = "TEXT")]
    pub system: Option<String>,

    /// Carry on from a session written by `/save`.
    #[arg(short, long, value_name = "PATH")]
    pub resume: Option<String>,

    /// An MCP server to run, and offer the tools of, as `[name=]command`. May be repeated.
    #[cfg(feature = "mcp")]
    #[arg(long, value_name = "COMMAND")]
    pub mcp: Vec<String>,

    /// How many requests one turn may make before it stops and asks; `0` is no limit at all.
    #[arg(long, value_name = "N", default_value_t = 8)]
    pub requests: usize,

    /// How full the context may get before the oldest tool results are elided to a marker they
    /// can be restored from; `1` never does.
    #[arg(long, value_name = "FRACTION", default_value_t = 0.8)]
    pub compact: f64,

    /// Let the model's tool calls run at the same time, instead of in the order it asked.
    #[arg(long)]
    pub parallel: bool,

    /// Run with no confinement at all: the shell reaches whatever the user running this can, and
    /// `fs`, which is not a process, stops holding itself to the working directory.
    #[arg(long)]
    pub no_sandbox: bool,

    /// Do not write the session out when it ends. It is written to a temporary directory
    /// otherwise, and the path is the last thing printed.
    #[arg(long)]
    pub no_record: bool,

    /// A path outside the working directory the tools may also read and write. May be repeated,
    /// and takes a comma-separated list.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    pub sandbox_allow: Vec<std::path::PathBuf>,

    /// A path outside the working directory the tools may read but not change. The same, and a
    /// toolchain is the usual one: `cargo` cannot start without `~/.rustup`.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    pub sandbox_read: Vec<std::path::PathBuf>,

    /// Drop the whole of a tool's output once it has been shortened, rather than keeping it as an
    /// archived item that can still be read.
    #[arg(long)]
    pub forget_truncated: bool,

    /// Send a request the counter puts over the model's context anyway, and let the endpoint be
    /// the one that says no. For a limit that is advertised wrongly, or a counter that is.
    #[arg(long)]
    pub send_oversized: bool,

    /// Drive the session from lines on stdin instead of from a screen: the session log goes to
    /// stdout, one JSON record per line, and what the model says goes to stderr. Implied when
    /// stdout is not a terminal.
    #[arg(long)]
    pub headless: bool,

    /// Put a socket in front of the session so it can be driven from elsewhere as well as from
    /// here: `unix:PATH`, or `tcp:127.0.0.1:PORT`. The screen stays where there is one to draw on.
    /// The session is this program's - it carries on when a client detaches, and waits when a tool
    /// needs an answer.
    #[arg(long, value_name = "ADDRESS", conflicts_with_all = ["headless", "connect"])]
    pub serve: Option<String>,

    /// Attach to a session somebody else is serving and drive it from lines on stdin, in the same
    /// two streams `--headless` writes. Nothing else on this command line applies except
    /// `--on-ask`: the model, the key, the tools and the sandbox are all the host's, and what a
    /// detaching client does with a question still open is not.
    #[arg(long, value_name = "ADDRESS")]
    pub connect: Option<String>,

    /// Allow a whole domain, one operation in one, or a path, as `fs`, `fs:read`, `exec:run`,
    /// `.env*`. May be repeated, and takes a comma-separated list. Answering at the prompt writes
    /// the same table.
    #[arg(long, value_name = "SUBJECT", value_delimiter = ',')]
    pub allow: Vec<String>,

    /// Refuse one, the same way. The strictest of everything consulted wins, so this beats
    /// `--allow`.
    #[arg(long, value_name = "SUBJECT", value_delimiter = ',')]
    pub deny: Vec<String>,

    /// Allow every tool an MCP server offers, by the name it was given. Its own argument because
    /// a server name and a domain are both bare words and nothing in either says which it is.
    #[arg(long, value_name = "NAME", value_delimiter = ',')]
    pub allow_server: Vec<String>,

    /// Refuse one, the same way.
    #[arg(long, value_name = "NAME", value_delimiter = ',')]
    pub deny_server: Vec<String>,

    /// What to do with a question nobody is there to answer: in a headless run, and in a
    /// `--connect` one once its input has closed.
    #[arg(long, value_name = "ANSWER", default_value = "deny")]
    pub on_ask: OnAsk,

    /// Stop a headless run after this many seconds, however far it has got. What has arrived is
    /// kept and the session is written out as usual.
    #[arg(long, value_name = "SECONDS")]
    pub deadline: Option<u64>,

    /// Stop the session once the provider has charged this many tokens for it, in and out. Time is
    /// not the only thing a run nobody is watching can spend; `/spend` changes it while it runs.
    #[arg(long, value_name = "TOKENS")]
    pub spend: Option<u64>,

    /// A JSON file of settings, for the ones you would otherwise type every time. Anything given
    /// here on the command line wins over what it says. Given none, `./kamchatka.json` is read if
    /// it is there, and the one under your config directory if it is not.
    #[arg(long, value_name = "PATH")]
    pub config_file: Option<std::path::PathBuf>,

    /// Print the settings file this program ships with and stop, for editing into one of your
    /// own: `kamchatka --print-config > kamchatka.json`.
    #[arg(long)]
    pub print_config: bool,

    /// The frame's colour, which only a settings file can say; see `Settings::border`.
    ///
    /// note: `skip` rather than an argument nobody would type twice, and it lives on `Args` all
    /// the same so that the one merge in `under` stays the one place the file meets the command
    /// line. A second path from a settings file to the program is a second set of rules about
    /// which wins.
    #[arg(skip)]
    pub border: Option<String>,

    /// Which tools to offer at startup, which only a settings file can say; see `Settings::tools`.
    ///
    /// note: `skip` for the reason above, and for one of its own. Which tools are worth their
    /// tokens is a fact about the project, and belongs in the file beside it rather than in front
    /// of somebody's hands. Turning one off for one session is `/tools toggle`, at the moment
    /// somebody wants it rather than before the session starts.
    #[arg(skip)]
    pub tools: Option<Vec<String>>,
}

impl Args {
    /// Fills in everything the command line did not say from a settings file.
    ///
    /// note: it asks clap which arguments were *typed*, rather than comparing against the
    /// defaults, because those are not the same question: `--requests 8` is the default value and
    /// somebody meant it, and a merge that could not tell them apart would let a file quietly
    /// override what was asked for. `ValueSource::CommandLine` is the only answer that counts, and
    /// it is why [`Args::given`] hands back the matches as well as the struct.
    ///
    /// note: a list from the command line *replaces* the file's rather than adding to it. One rule
    /// for every key is the only kind anybody can predict, and the other way round there is no way
    /// to ask for fewer than the file says.
    pub fn under(mut self, settings: Settings, matches: &clap::ArgMatches) -> Result<Self> {
        let typed =
            |name: &str| matches.value_source(name) == Some(clap::parser::ValueSource::CommandLine);
        // one macro rather than an `if` per field, and it names the argument once: the string clap
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
        // note: and not at all into a resumed session, whose context already holds the one the
        // snapshot's first run was given, pinned. Pushed again on every `-r`, it would stack a copy
        // per resume, each beyond compaction's reach; a typed `-s` beside `-r` is somebody asking
        // for one, and is still honoured
        if self.system.is_none() && self.resume.is_none() {
            self.system = settings.system;
        }
        // read here rather than where it is drawn, so that a colour nobody can parse is a startup
        // error naming the file rather than a frame that is quietly still yellow. There is no
        // argument to lose to, so there is nothing to ask `typed` about
        if let Some(border) = &settings.border {
            config::rgb(border)
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
            // every key, `mcp` among them, so a stricter rule would make the shipped starting
            // point unusable in a build that has no MCP
            #[cfg(not(feature = "mcp"))]
            Some(mcp) if !mcp.is_empty() => anyhow::bail!(
                "this build has no MCP support, so `mcp` in the settings file cannot be honoured"
            ),
            _ => {}
        }

        // the same rule for the same reason: a settings file that asked for its commands rated, in
        // a build with no advisor in it, would run every question uncoloured and say nothing
        match settings.advise {
            #[cfg(feature = "shell-advisor")]
            Some(advise) if !typed("advise") => self.advise = advise,
            #[cfg(not(feature = "shell-advisor"))]
            Some(true) => anyhow::bail!(
                "this build has no advisor in it, so `advise` in the settings file cannot be \
                 honoured"
            ),
            _ => {}
        }

        Ok(self)
    }

    /// The command line as it was typed, with everything it did not say filled in from a settings
    /// file.
    ///
    /// note: the matches come back with the arguments, because [`Args::under`] needs to know which
    /// of them were *typed* and the struct cannot say - a value that equals its default and a
    /// value somebody wrote out are the same field.
    ///
    /// note: a path that was typed is not announced and a path that was found is. Somebody who
    /// wrote `--config-file` knows; somebody who walked into a directory with one in it does not,
    /// and a file that applies because of where you are standing has to say so rather than be
    /// discovered later by its effects. Saying it is the caller's, because there is no session to
    /// say it into yet.
    ///
    /// note: nothing is looked for under `--connect`, which takes nothing else on principle: the
    /// model, the key, the tools and the sandbox all belong to whoever is serving, and a file
    /// picked up here would be a set of settings for a session this process is not assembling.
    pub fn given() -> Result<Given> {
        let matches = Self::command().get_matches();
        let mut args = Self::from_arg_matches(&matches)
            .map_err(|e| e.exit())
            .unwrap();
        // note: before anything is looked for, because what this flag is for is
        // `--print-config > kamchatka.json` - and the shell has emptied that file before this runs,
        // so reading it first is a parse error about the file this was about to write
        if args.print_config {
            return Ok(Given {
                args,
                matches,
                found: None,
            });
        }
        let found = args
            .config_file
            .is_none()
            .then(|| args.connect.is_none().then(config::found))
            .flatten()
            .flatten();
        if let Some(path) = args.config_file.clone().or_else(|| found.clone()) {
            let settings = Settings::read(&path).map_err(|e| anyhow::anyhow!("{e}"))?;
            args = args.under(settings, &matches)?;
        }

        Ok(Given {
            args,
            matches,
            found,
        })
    }

    /// The session these arguments ask for, short of the provider that answers it.
    ///
    /// note: the resumed session is read here rather than in [`Setup`], because the two error
    /// messages worth writing - which path, and whether it was a session at all - belong to
    /// whoever was handed the path.
    pub fn setup(&self) -> Result<Setup> {
        // note: here rather than in the parser, so that a settings file's `compact` is held to it
        // too. A fraction, and said so: `80`, meant as a percentage, would be no compactor at all,
        // and anything at or below zero one that took every tool result
        anyhow::ensure!(
            self.compact > 0.0 && self.compact <= 1.0,
            "`compact` is how full the context may get, as a fraction above 0 and at most 1 - \
             `0.8` rather than `80`, and `1` never compacts - and this was `{}`",
            self.compact
        );
        let resume = match &self.resume {
            Some(path) => {
                let snapshot: nachalnik::Snapshot = serde_json::from_slice(
                    &std::fs::read(path).with_context(|| format!("could not read {path}"))?,
                )
                .with_context(|| format!("{path} is not a session"))?;
                // refused rather than repaired: the runtime would renumber or reserve its way round
                // most of these, and a session carried on from a record that had to be changed to
                // be read is one whose record no longer says what happened
                let problems = snapshot.problems();
                anyhow::ensure!(
                    problems.is_empty(),
                    "{path} is a session this will not carry on from: {}",
                    problems.join("; ")
                );

                Some(snapshot)
            }
            None => None,
        };

        Ok(Setup {
            resume,
            // the runtime's own default is a counter that restarts at 0 with the process, which is
            // fine as an identity and useless as a filename: every session would write over the
            // last one's record. A resumed session keeps the name in its snapshot, so carrying on
            // appends to the same session rather than starting a second one that looks unrelated
            session_name: self.resume.is_none().then(|| {
                let started = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|since| since.as_secs())
                    .unwrap_or_default();

                App::session_stamp(started)
            }),
            requests: (self.requests > 0).then_some(self.requests),
            parallel: self.parallel,
            // what the runtime keeps is a decision about retention, and retention here is a file
            // somebody has to store: `/save` writes the snapshot, and an archived output goes into
            // it whole. One `grep` that wanders into `./target/` can be megabytes of build noise
            // nobody will read, and it is in every save of that session from then on
            keep_truncated: !self.forget_truncated,
            refuse_oversized: !self.send_oversized,
            record: !self.no_record,
            compact: Some(self.compact),
            spend: self.spend,
            confine: !self.no_sandbox,
            reachable: self.sandbox_allow.clone(),
            readable: self.sandbox_read.clone(),
            allow: self
                .allow
                .iter()
                .map(|it| Subject::parse(it))
                .chain(self.allow_server.iter().cloned().map(Subject::Server))
                .collect(),
            deny: self
                .deny
                .iter()
                .map(|it| Subject::parse(it))
                .chain(self.deny_server.iter().cloned().map(Subject::Server))
                .collect(),
            tools: self.tools.clone(),
            system: self.system.clone(),
            files: self.file.clone(),
            #[cfg(feature = "shell-advisor")]
            advisor: None,
        })
    }

    /// The same, with the advisor attached where `--advise` asked for one.
    ///
    /// note: after [`Setup::check`] and before the provider. A missing advisor key is a fact about
    /// the arguments and should not cost a round trip to the *other* endpoint to find out about;
    /// and a session that was asked for with `--advise` and could not reach an advisor is refused
    /// rather than quietly run with every question uncoloured. Somebody who turned this on is
    /// entitled to have it on or be told it is not.
    ///
    /// note: nothing the advisor has to say is drained here. The session reports all of it,
    /// through `App::on_event` and the drawn loop's tick, and a queue somebody else popped is a
    /// queue missing its first line - for a local advisor, that it is not ready yet. Printing it
    /// to stderr here would not help either: the screen arrives immediately and clears it.
    #[cfg(feature = "shell-advisor")]
    pub async fn advised(&self, setup: Setup) -> Result<Setup> {
        if !self.advise {
            return Ok(setup);
        }

        let jev = endpoint::advise::connect(&endpoint::session_endpoint(self.gemini))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("could not reach the advisor")?;
        Ok(Setup {
            advisor: Some(jev),
            ..setup
        })
    }

    /// Where this session's requests go, in whichever dialect was asked for.
    ///
    /// note: two wire formats, one trait. `--gemini` is what a person picks, and everything
    /// downstream - the kernel, the screen, `/model`, `/provider` - is written against `Endpoint`
    /// and never finds out which one it got.
    ///
    /// note: no model unless one was named. A default means that a session started without `-m`
    /// talks to whatever this program's author picked, at the person's expense and with nothing
    /// saying the choice was not theirs. Without one the session still starts - the address and
    /// the key are settled, `/models` lists what is served and `/model` picks one - and until it
    /// is picked the kernel holds no provider, so nothing can be sent. See `Setup::wire`.
    pub async fn provider(&self) -> Result<Arc<dyn Dialect>> {
        let model = self.model.as_deref();
        match self.gemini {
            true => endpoint::gemini::connect(model)
                .await
                .map(|it| it as Arc<dyn Dialect>),
            false => endpoint::connect(model)
                .await
                .map(|it| it as Arc<dyn Dialect>),
        }
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("could not reach the model")
    }
}

/// A command line, read: the arguments, what clap matched, and the settings file they were filled
/// in from where one was found rather than named.
///
/// note: not `#[non_exhaustive]`, which every other struct this crate answers with carries, and
/// `Wired` beside it is the same exception for the same reason: this exists to be taken apart at
/// the call site. The attribute would make every caller write `..`, which is the one spelling that
/// *hides* a field added later rather than stopping the build over it.
pub struct Given {
    /// The arguments, with anything the command line did not say filled in from the file.
    pub args: Args,
    /// What clap matched, which is the only thing that can say which arguments were *typed*.
    pub matches: clap::ArgMatches,
    /// The settings file found where somebody was standing, where one was.
    ///
    /// note: never one named with `--config-file`: this is what has to be said out loud, and a
    /// path somebody typed does not.
    pub found: Option<std::path::PathBuf>,
}

/// What an unanswerable question is answered with.
///
/// note: `deny` is the default, and that is the difference between this and `examples/recorded.rs`,
/// which grants every question it is asked. That is right for a recording somebody is watching and
/// wrong for a program: a run nobody is watching should not be able to do a thing nobody has
/// allowed, and `--allow exec` is one flag away for anyone who means it.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OnAsk {
    /// Refuse it. The model is told, and told that it was this call rather than a standing rule.
    Deny,
    /// Grant it.
    Allow,
}

impl OnAsk {
    /// The answer itself, as the kernel spells it.
    pub fn grant(self) -> Grant {
        match self {
            Self::Deny => Grant::Deny,
            Self::Allow => Grant::Allow,
        }
    }
}
