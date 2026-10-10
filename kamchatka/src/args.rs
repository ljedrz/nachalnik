//! The command line: what the program accepts, and what a session made of it looks like.
//!
//! note: in the library rather than in `main.rs`. A session assembled from these arguments is the
//! thing this crate is for, and embedders build sessions from it too, rather than keeping a second
//! set of flags in sync.
//!
//! note: what `main.rs` keeps is the part that is the program's own: which loop drives the session,
//! where the record is written, and the refusal that `--connect` assembles nothing.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::parser::ValueSource;
use clap::{CommandFactory as _, FromArgMatches as _, Parser, ValueEnum as _};
use nachalnik::Grant;
use nachalnik_providers::Dialect;

use crate::{
    app::App,
    config::{self, Compact, Settings},
    endpoint,
    tools::Subject,
    wiring::Setup,
};

/// The variables `--help` lists, and what each of them is for - and the exit statuses, which are
/// the other thing a script reads and a flag cannot say.
///
/// note: they are read by [`Args::provider`], and the advisor's by `Args::advised`, rather than
/// declared as arguments, so clap cannot list them the way it lists `KAMCHATKA_MODEL` beside
/// `--model`. A setting nothing on the screen mentions is a setting nobody finds, and `--help` is
/// where a person looks for the list.
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
  KAMCHATKA_SYSTEM1_MODEL     which System One model answers; required, and any of
                              openrouter.ai/api/v1/models?output_modalities=decisions
  KAMCHATKA_SYSTEM1_API_KEY   its key. Without one it borrows an OpenRouter key -
                              this session's own where it talks to OpenRouter, or
                              OPENROUTER_API_KEY - but only for questions that go
                              to OpenRouter; anywhere else it asks with none
  KAMCHATKA_SYSTEM1_BASE_URL  where its questions go; OpenRouter, or an engine of
                              your own that answers the same route";
    #[cfg(not(feature = "shell-advisor"))]
    const ADVISOR: &str = "";

    format!(
        "\
Environment:
  KAMCHATKA_API_KEY        the key, sent wherever the requests go. Needed for OpenRouter,
                           OpenAI, Google and Anthropic; a model served locally takes none
  OPENROUTER_API_KEY       the key for OpenRouter, and only there
  OPENAI_API_KEY           the key for OpenAI's own API, and only there
  ANTHROPIC_API_KEY        the key for Anthropic's own API, with --anthropic, and only there
  KAMCHATKA_BASE_URL       where the requests go, e.g. http://localhost:11434/v1 for ollama;
                           OpenRouter by default, Google's own v1beta with --gemini, or
                           Anthropic's own with --anthropic; with --responses, OpenRouter or
                           https://api.openai.com/v1 for OpenAI's own
  KAMCHATKA_CONTEXT_LIMIT  the model's context size, for a provider that will not say
  KAMCHATKA_NO_ATTRIBUTION set to stop naming this program to OpenRouter{ADVISOR}

Exit status of a headless run: 0 done, 1 failed, 3 --spend reached, 4 paused at --requests,
5 done but the record could not be written, 124 --deadline passed, 129/130/143 SIGHUP, ctrl+c or
SIGTERM - the last three for a served one too"
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
// note: no `help` command beside `reconcile`: `--help` is the one spelling, and every name taken
// by a command is a first message that stops being one
#[command(disable_help_subcommand = true)]
pub struct Args {
    /// Something to do other than run a session.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// A first message, sent as soon as it starts.
    pub message: Vec<String>,

    /// The model to talk to. Without one the session starts with none and sends nothing until
    /// `/model` picks one; `/models` lists what the endpoint serves.
    #[arg(short, long, env = "KAMCHATKA_MODEL")]
    pub model: Option<String>,

    /// Talk to Google's own API rather than an OpenAI-compatible one, so that a turn keeps the
    /// order it was produced in: thinking, a sentence, a tool call, more thinking.
    #[arg(long, conflicts_with_all = ["anthropic", "responses"])]
    pub gemini: bool,

    /// Talk to Anthropic's own Messages API rather than an OpenAI-compatible one, so that a
    /// turn keeps its order and its signed thinking goes back as it came. OpenRouter speaks it
    /// too: point `KAMCHATKA_BASE_URL` at `https://openrouter.ai/api/v1`.
    #[arg(long, conflicts_with = "responses")]
    pub anthropic: bool,

    /// Ask OpenAI's Responses API rather than chat completions, so that a turn keeps its order
    /// and a reasoning model's thinking goes back sealed rather than being redone every turn.
    /// OpenRouter answers it as well as OpenAI: `KAMCHATKA_BASE_URL=https://api.openai.com/v1`
    /// for OpenAI's own.
    #[arg(long)]
    pub responses: bool,

    /// Ask a second model where each shell command a question is about lands on a three-level
    /// rubric, and colour the question by the answer - a command joined at its `|`, `&&` or `;`
    /// is asked about stage by stage and rated by its worst one, underlined where it is worse than
    /// the rest. The rating decides nothing: what the rules allow runs and what they refuse is
    /// refused. Sends the call's tool name, capabilities and arguments to the advisor, a System
    /// One model named by KAMCHATKA_SYSTEM1_MODEL; see KAMCHATKA_SYSTEM1_BASE_URL for one on this
    /// machine, where nothing leaves it.
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

    /// An MCP server to run, and offer the tools of, as `[name=]command`. May be repeated. The
    /// command is split on whitespace, with no quoting, so none of its arguments can hold a space.
    #[cfg(feature = "mcp")]
    #[arg(long, value_name = "COMMAND")]
    pub mcp: Vec<String>,

    /// How many requests one turn may make before it stops and asks; `0` is no limit at all.
    #[arg(long, value_name = "N", default_value_t = 8)]
    pub requests: usize,

    /// How full the context may get before its oldest exchanges are excluded, which they can be
    /// restored from; `1` never compacts at all. A second fraction after a comma is how far down a
    /// full context is taken once they start going, at most the first; left out, twenty points
    /// under it or half of it, whichever is more.
    #[arg(long, value_name = "FRACTION[,TARGET]", default_value = "0.8")]
    pub compact: Compact,

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

    /// A device under `/dev` the shell may read and write, and the list replaces the usual one
    /// rather than adding to it. Everything else under `/dev` - another terminal, shared memory, a
    /// camera - stays out of reach.
    #[arg(long, value_name = "PATH", value_delimiter = ',', default_values = crate::sandbox::DEVICES)]
    pub sandbox_device: Vec<std::path::PathBuf>,

    /// Drop the whole of a tool's output once it has been shortened, rather than keeping it as an
    /// excluded item that can still be read.
    #[arg(long)]
    pub forget_truncated: bool,

    /// Send a request the counter puts over the model's context anyway, and let the endpoint be
    /// the one that says no. For a limit that is advertised wrongly, or a counter that has
    /// drifted, the endpoint's own count is worth more than any guess made here.
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

    /// Also serve the session as a web page, e.g. at `127.0.0.1:8080`. With `--serve`, the page
    /// relays to that socket; otherwise the session is served on a loopback port of its own. The
    /// page has no authentication. This machine's address on a private network (e.g.
    /// `192.168.1.5:8080`) makes it reachable from that network, with a warning; from further away,
    /// use a tunnel such as `ssh -L`.
    #[cfg(feature = "webui")]
    #[arg(long, value_name = "ADDRESS", conflicts_with_all = ["headless", "connect"])]
    pub web: Option<String>,

    /// Attach to a session somebody else is serving and drive it from lines on stdin, in the same
    /// two streams `--headless` writes. Nothing else is accepted on this command line but
    /// `--on-ask`: anything else is an error naming it, because the model, the key, the tools and
    /// the sandbox are all the host's, and what a detaching client does with a question still open
    /// is the one thing that is the client's.
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
    /// `--connect` one once its input has closed. `--connect` leaves it for somebody else unless
    /// this is given; `leave` is for `--connect` alone.
    #[arg(long, value_name = "ANSWER", default_value = "deny")]
    pub on_ask: OnAsk,

    /// Stop a headless run after this many seconds, however far it has got; `0` is no deadline.
    /// What has arrived is kept and the session is written out as usual.
    #[arg(long, value_name = "SECONDS")]
    pub deadline: Option<u64>,

    /// Stop the session once the provider has charged this many tokens for it, in and out; `0` is
    /// no ceiling. Time is not the only thing a run nobody is watching can spend; `/spend` changes
    /// it while it runs.
    #[arg(long, value_name = "TOKENS")]
    pub spend: Option<u64>,

    /// Leave what a `shell` command started in the background running when the session ends,
    /// rather than stopping it. Either way, the end of the session names each one.
    #[arg(long)]
    pub leave_running: bool,

    /// A JSON file of settings, for the ones you would otherwise type every time. Anything given
    /// here on the command line wins over what it says. Given none, `./kamchatka.json` is read if
    /// it is there and you say yes when asked, and the one under your config directory if it is
    /// not there.
    #[arg(long, value_name = "PATH")]
    pub config_file: Option<std::path::PathBuf>,

    /// Print the settings file this program ships with and stop, for editing into one of your
    /// own: `kamchatka --print-config > kamchatka.json`.
    #[arg(long)]
    pub print_config: bool,

    /// Read a session's record and say what does not add up, and stop. PATH is the log, the
    /// snapshot, or their name without the suffix, and whichever of the pair is there is read:
    /// lines that are not records, events this version does not know, records missing or
    /// numbered twice, calls asked for and never finished, and a snapshot that disagrees with its
    /// log. Nothing is started, and anything found makes the exit status non-zero.
    #[arg(long, value_name = "PATH")]
    pub check: Option<String>,

    /// The frame's colour, which only a settings file can say; `None` is the terminal's own
    /// foreground colour. See `Settings::border_color`.
    ///
    /// note: `skip` rather than an argument nobody would type twice, and it lives on `Args` all
    /// the same so that the one merge in `under` stays the one place the file meets the command
    /// line. A second path from a settings file to the program is a second set of rules about
    /// which wins.
    #[arg(skip = Some(config::BORDER_COLOR.to_owned()))]
    pub border_color: Option<String>,

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
            anthropic,
            responses,
            requests,
            compact,
            parallel,
            no_sandbox,
            sandbox_allow,
            sandbox_read,
            sandbox_device,
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
        // error naming the file rather than a frame that is quietly the default. There is no
        // argument to lose to, so there is nothing to ask `typed` about
        //
        // note: a `null` is a colour of its own - the terminal's - and not the key left out
        match settings.border_color {
            None => {}
            Some(None) => self.border_color = None,
            Some(Some(border)) => {
                config::rgb(&border)
                    .map_err(|e| anyhow::anyhow!("`border-color` in the settings file: {e}"))?;
                self.border_color = Some(border);
            }
        }
        // the other one with no argument to lose to. Checking the names is `Setup::wire`'s, since
        // the tools it would be checking against are the ones it is about to build
        self.tools = settings.tools;
        if let Some(on_ask) = settings.on_ask.filter(|_| !typed("on_ask")) {
            self.on_ask = OnAsk::from_str(&on_ask, true).map_err(|_| {
                // what the word is held to, rather than clap's own "invalid variant: maybe": a
                // settings file has no `--help` beside it, and these two are the only answers
                anyhow::anyhow!(
                    "`on-ask` in the settings file is what a question nobody is there to answer \
                     gets, and this is `{on_ask}`: it is `deny` or `allow`"
                )
            })?;
            // note: a file is read by the runs that have nobody else to ask, and `leave` there is
            // a question nobody will ever answer. `--connect` does not read the file's `on-ask`
            // at all; see `Args::given`
            if self.on_ask == OnAsk::Leave {
                return Err(anyhow::anyhow!(
                    "`on-ask` in the settings file is `leave`, which only `--connect` can do - \
                     and `--connect` takes `--on-ask` from its own command line. It is `deny` or \
                     `allow`"
                ));
            }
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
        // so reading it first is a parse error about the file this was about to write. `--check`
        // and a command run no session for a file's settings to be about
        if args.print_config || args.check.is_some() || args.command.is_some() {
            return Ok(Given {
                args,
                matches,
                found: None,
                filed: None,
            });
        }
        let mut found = args
            .config_file
            .is_none()
            .then(|| args.connect.is_none().then(config::found))
            .flatten()
            .flatten();
        let mut filed = None;
        if let Some(path) = args.config_file.clone().or_else(|| found.clone()) {
            let settings = Settings::read(&path).map_err(|e| anyhow::anyhow!("{e}"))?;
            // the path the way `Settings::read` puts it on its own errors: what `under` refuses
            // is a value in this file, and there are two places a file can be found
            //
            // the two a served session does not read, taken before `under` moves them into the
            // struct, where nothing can tell a file's value from a default
            //
            // note: and only where the file's value is one this run would not otherwise have had.
            // `--print-config` writes every key, so the file this program hands out to be edited
            // carries `on-ask: deny` - which is the default, changes nothing, and refusing every
            // `--serve` over it would break the very file the documentation tells a reader to
            // write. A file saying `allow` is somebody's decision, and that is refused. Compared
            // as the value is parsed, without regard to case, or `Deny` would be read as the
            // default and refused as something else
            let unserved = [
                ("deadline", settings.deadline.is_some()),
                (
                    "on-ask",
                    settings
                        .on_ask
                        .as_deref()
                        .is_some_and(|asked| !asked.eq_ignore_ascii_case("deny")),
                ),
            ]
            .into_iter()
            .filter_map(|(id, carried)| carried.then_some(id))
            .collect();
            let granting = config::granting(&settings);
            // every key the file gave a value, as the argument it stands in for spells it: what
            // says a refused value was the file's rather than a default's, or a snapshot's
            let carried = serde_json::to_value(&settings)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .map(|keys| {
                    keys.into_iter()
                        .filter(|(_, value)| !value.is_null())
                        .map(|(key, _)| key.replace('-', "_"))
                        .collect()
                })
                .unwrap_or_default();
            args = args
                .under(settings, &matches)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            filed = Some(Filed {
                at: path.clone(),
                matches: matches.clone(),
                unserved,
                carried,
            });

            // note: asked after the file is read and merged, both of which only look at it, so a
            // file this program would refuse is refused with what is wrong with it rather than
            // asked about first
            if found.as_deref().is_some_and(config::underfoot) && !asked(&path, &granting)? {
                args = Self::from_arg_matches(&matches)
                    .map_err(|e| e.exit())
                    .unwrap();
                found = None;
                filed = None;
            }
        }

        // note: last of all, so that `-m`, `KAMCHATKA_MODEL` and a settings file's `model` each
        // still win: the record is what a session was talking to, and any of those is somebody
        // saying what this one should. A snapshot holds the conversation and not the model, so a
        // `-r` without this started a session with none - and a headless one given a message sent
        // nothing and said so to nobody who was looking
        if args.model.is_none()
            && let Some(path) = &args.resume
        {
            args.model = last_model(std::path::Path::new(path));
        }

        Ok(Given {
            args,
            matches,
            found,
            filed,
        })
    }

    /// The session these arguments ask for, short of the provider that answers it.
    ///
    /// note: the resumed session is read here rather than in [`Setup`], because the two error
    /// messages worth writing - which path, and whether it was a session at all - belong to
    /// whoever was handed the path.
    pub fn setup(&self) -> Result<Setup> {
        self.setup_from(None)
    }

    /// [`Args::setup`], for a run that read a settings file, and the file's own refusals with it.
    ///
    /// note: where the refusals of the merge ended and these began. `under` holds a value to the
    /// file and says which key, `Setup::check` and the wiring hold them to what this program can
    /// do and say nothing about where a value came from - and a file found underfoot is announced
    /// on standard error, where a terminal's screen covers it and a failure has already ended the
    /// run. So the two run together, and anything out of `self` rather than out of the file is
    /// left as it would have been said: somebody who typed `--deny` is looking at it.
    ///
    /// note: a parameter rather than a field on [`Args`] because the file is not a setting: it is
    /// two facts - which path, and which arguments were *typed* - and the second is what tells a
    /// file's value from a flag's.
    pub fn setup_from(&self, filed: Option<&Filed>) -> Result<Setup> {
        // one function for the refusals that are about the arguments, so that a value out of a
        // file is answered with the file beside it and a value off the command line is answered
        // as it always was
        //
        // note: the file only where the file said it - carried the key, and was not overruled by a
        // flag. Asked the other way round, "not typed" named the file for every default
        let at = |field: &str| match filed {
            Some(filed) if !filed.typed(field) && filed.carried.iter().any(|key| key == field) => {
                Some(format!("{}: ", filed.at.display()))
            }
            _ => None,
        };
        // note: here rather than where the limit is read, because that is a round trip away and a
        // session measuring itself against nothing is not a session anybody finds out about. Every
        // other figure this program settles at startup is refused here, and this one used to be
        // read out of the environment and dropped when it did not parse
        endpoint::checked_limit().map_err(|e| anyhow::anyhow!("{e}"))?;
        // note: here rather than in the parser, so that a settings file's `compact` is held to it
        // too. A fraction, and said so: `80`, meant as a percentage, would be no compactor at all,
        // and anything at or below zero one that took every tool result
        anyhow::ensure!(
            self.compact.threshold > 0.0 && self.compact.threshold <= 1.0,
            "{}`compact` is how full the context may get, as a fraction above 0 and at most 1 - \
             `0.8` rather than `80`, and `1` never compacts - and this was `{}`",
            at("compact").unwrap_or_default(),
            self.compact.threshold
        );
        // note: at most the threshold, because a target above it is a context that is over the
        // threshold and already under the target, which a pass answers by dropping nothing - before
        // every request, for as long as the context stays between the two. Equal is allowed: it is
        // the pass that drops just enough each time, which is a choice and not a mistake
        if let Some(target) = self.compact.target {
            anyhow::ensure!(
                target > 0.0 && target <= self.compact.threshold,
                "{}`compact`'s second fraction is how far down a full context is taken, above 0 \
                 and at most its first, which is `{}` - and this was `{target}`",
                at("compact").unwrap_or_default(),
                self.compact.threshold
            );
        }
        // note: refused for the reason `wiring::unreached` refuses a domain no tool declares. A
        // server rule naming a server this run does not start matches nothing, so a misspelled
        // `--deny-server` read as given and left the server it meant to the question - which a
        // headless run with `--on-ask allow` answers yes
        #[cfg(feature = "mcp")]
        let servers: Vec<String> = self
            .mcp
            .iter()
            .map(|spec| crate::mcp::named(spec).0)
            .collect();
        #[cfg(not(feature = "mcp"))]
        let servers: Vec<String> = Vec::new();
        // with the list it is in, so that the file is named only where the file gave it: the
        // shipped one carries both lists, and asked of either, it was named for a name typed
        let listed = [
            ("allow_server", &self.allow_server),
            ("deny_server", &self.deny_server),
        ];
        if let Some((field, unknown)) = listed
            .into_iter()
            .flat_map(|(field, names)| names.iter().map(move |name| (field, name)))
            .find(|(_, name)| !servers.contains(name))
        {
            anyhow::bail!(
                "{}`{unknown}` is not a server this run starts; {}",
                at(field).unwrap_or_default(),
                match servers.is_empty() {
                    true => "it starts none".to_owned(),
                    false => format!("they are {}", servers.join(", ")),
                }
            );
        }
        // note: an `allow` of `mcp:call` or `mcp` matches no call, and is refused for the same
        // reason. A server's tools are judged under its name in place of `mcp:call` - see
        // `Careful::judges` - and every server here is one this program started, so the rule read
        // as given and every call still went to the question. A `deny` of either is consulted
        if let Some(granted) = self.allow.iter().find(|rule| {
            matches!(
                Subject::parse(rule).to_string().as_str(),
                "mcp" | "mcp:call"
            )
        }) {
            let at = at("allow").unwrap_or_default();
            anyhow::bail!(
                "{at}`--allow {granted}` grants nothing: a tool from an MCP server is judged \
                 under the server's name, so `--allow-server NAME` is what lets one through"
            );
        }
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
                let mut snapshot = snapshot;
                past_the_snapshot(&mut snapshot, std::path::Path::new(path));

                Some(snapshot)
            }
            None => None,
        };
        let setup = Setup {
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
            // somebody has to store: `/save` writes the snapshot, and an excluded output goes into
            // it whole. One `grep` that wanders into `./target/` can be megabytes of build noise
            // nobody will read, and it is in every save of that session from then on
            keep_truncated: !self.forget_truncated,
            refuse_oversized: !self.send_oversized,
            record: !self.no_record,
            compact: Some(self.compact.threshold),
            compact_target: self.compact.target,
            // note: `0` is no ceiling, as `/spend 0` and `--requests 0` say it: a ceiling of nothing
            // would be a session that refuses its first turn without saying why
            spend: self.spend.filter(|it| *it > 0),
            leave_running: self.leave_running,
            confine: !self.no_sandbox,
            reachable: self.sandbox_allow.clone(),
            readable: self.sandbox_read.clone(),
            devices: self.sandbox_device.clone(),
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
        };

        // the last of the refusals, and the first one a value out of a file can reach without
        // having said which file it came from: what this program offers and what a rule can match
        // are both decided here, so a tool, a server and a path rule are all refused after the
        // merge rather than in it - and the refusal says which setting it was, so that the file is
        // named for what the file said and for nothing else
        setup.check().map_err(|refused| {
            let at = refused
                .setting
                .and_then(|setting| at(&setting.replace('-', "_")))
                .unwrap_or_default();
            anyhow::anyhow!("{at}{refused}")
        })?;

        Ok(setup)
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

        let engine = endpoint::advise::connect(&endpoint::session_endpoint(self.wire()?))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("could not reach the advisor")?;
        Ok(Setup {
            advisor: Some(engine),
            ..setup
        })
    }

    /// The wire format this session speaks.
    ///
    /// note: clap refuses the two flags together, and this is for the settings file, which can
    /// name one dialect while the command line names the other - two dialects asked for are a
    /// mistake to say, not one to settle by picking either.
    pub fn wire(&self) -> Result<endpoint::Wire> {
        let asked: Vec<&str> = [
            (self.gemini, "--gemini"),
            (self.anthropic, "--anthropic"),
            (self.responses, "--responses"),
        ]
        .into_iter()
        .filter_map(|(on, flag)| on.then_some(flag))
        .collect();

        match asked[..] {
            [] => Ok(endpoint::Wire::OpenAi),
            ["--gemini"] => Ok(endpoint::Wire::Gemini),
            ["--anthropic"] => Ok(endpoint::Wire::Anthropic),
            ["--responses"] => Ok(endpoint::Wire::Responses),
            _ => Err(anyhow::anyhow!(
                "{} are {} dialects, and a session speaks one: at least one of them is on in the \
                 settings file",
                asked.join(" and "),
                match asked.len() {
                    2 => "two",
                    _ => "three",
                }
            )),
        }
    }

    /// Where this session's requests go, in whichever dialect was asked for.
    ///
    /// note: four wire formats, one trait. `--gemini`, `--anthropic` or `--responses` is what a
    /// person picks, and everything
    /// downstream - the kernel, the screen, `/model`, `/endpoint` - holds a `Dialect` and never
    /// finds out which one it got.
    ///
    /// note: no model unless one was named. A default means that a session started without `-m`
    /// talks to whatever this program's author picked, at the person's expense and with nothing
    /// saying the choice was not theirs. Without one the session still starts - the address and
    /// the key are settled, `/models` lists what is served and `/model` picks one - and until it
    /// is picked the kernel holds no provider, so nothing can be sent. See `Setup::wire`.
    pub async fn provider(&self) -> Result<Arc<dyn Dialect>> {
        let model = self.model.as_deref();
        match self.wire()? {
            endpoint::Wire::Gemini => endpoint::gemini::connect(model)
                .await
                .map(|it| it as Arc<dyn Dialect>),
            endpoint::Wire::Anthropic => endpoint::anthropic::connect(model)
                .await
                .map(|it| it as Arc<dyn Dialect>),
            endpoint::Wire::Responses => endpoint::responses::connect(model)
                .await
                .map(|it| it as Arc<dyn Dialect>),
            endpoint::Wire::OpenAi => endpoint::connect(model)
                .await
                .map(|it| it as Arc<dyn Dialect>),
        }
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("could not reach the model")
    }
}

/// Moves a snapshot's numbering past everything the log beside it numbered after it was taken.
///
/// note: the snapshot is rewritten when a turn begins and when the session rests, and the log is
/// appended to as each event happens, so a run killed in the middle of a turn leaves a log that runs
/// past its snapshot. Carried on from as it stands, the snapshot would have the next session number
/// records, items, calls and permissions again from where it was taken - and two files of one
/// session would say different things under one identifier. The log is the one `-r` already reads
/// for `App::recall`, and one that is missing or cut off mid-line costs what it cannot say.
fn past_the_snapshot(snapshot: &mut nachalnik::Snapshot, path: &std::path::Path) {
    use nachalnik::{Event, Record};

    let Ok(log) = std::fs::read_to_string(path.with_extension("jsonl")) else {
        return;
    };
    let taken = snapshot.last_seq;
    let past = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .filter(|record| record.seq > taken);
    for record in past {
        snapshot.last_seq = snapshot.last_seq.max(record.seq);
        let permission = match record.event {
            Event::ContextAdded { id, .. } => {
                snapshot.next_item = snapshot.next_item.max(id.0 + 1);
                None
            }
            Event::ToolRequested { call, .. } | Event::ToolCallRepaired { call, .. } => {
                snapshot.used_calls.push(call);
                None
            }
            Event::PermissionRequested { request } => Some(request.id),
            Event::PermissionDecided { id, .. } => Some(id),
            _ => None,
        };
        if let Some(id) = permission {
            snapshot.next_permission = snapshot.next_permission.max(id.0 + 1);
        }
    }
    snapshot.used_calls.sort();
    snapshot.used_calls.dedup();
}

/// The model the record beside a snapshot says the session was last talking to.
///
/// note: the last `model.changed` or `model.requested` in it, whichever came later, so that a
/// `/model` switch the session made is the one it carries on with - and a log running past the
/// snapshot, as one a killed run leaves does, is read to its end like everything else here. A
/// switch to no model is `None`, and so is a record that is missing or names none.
fn last_model(path: &std::path::Path) -> Option<String> {
    last_talked_to(path)
        .map(|info| info.model)
        .filter(|model| !model.is_empty())
}

/// Where the record beside a snapshot says the session was last sending its requests, where it
/// says.
///
/// note: read and said, never followed. The address is where this run's key would go, and a
/// snapshot is a file anybody can hand somebody: `-r` on one whose record named an address of the
/// sender's choosing would post the reader's key there. So a session resumed somewhere other than
/// where it was is told so, and `/endpoint` is the person deciding to go back.
pub fn last_endpoint(path: &std::path::Path) -> Option<String> {
    last_talked_to(path).and_then(|info| info.endpoint)
}

/// The last thing the record beside a snapshot says about the model the session was talking to.
fn last_talked_to(path: &std::path::Path) -> Option<nachalnik::ModelInfo> {
    use nachalnik::Record;

    let log = std::fs::read_to_string(path.with_extension("jsonl")).ok()?;
    let records: Vec<Record> = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .collect();

    talked_to(&records)
}

/// The last thing a log says about the model its session was talking to.
pub(crate) fn talked_to(records: &[nachalnik::Record]) -> Option<nachalnik::ModelInfo> {
    use nachalnik::Event;

    records
        .iter()
        .filter_map(|record| match &record.event {
            Event::ModelChanged { to, .. } => Some(to.clone()),
            Event::ModelRequested { model, .. } => Some(Some(model.clone())),
            _ => None,
        })
        .next_back()
        .flatten()
}

/// Whether the settings file found underfoot may be read, asked of the terminal, and a refusal
/// where there is nobody at one.
///
/// note: a refusal rather than a run without the file, because a file carrying `deadline` or
/// `spend` is a script's bounds, and a script that quietly lost them would run unbounded. Naming
/// the file with `--config-file` is how a run nobody can ask says it is meant.
fn asked(path: &std::path::Path, granting: &[&str]) -> Result<bool> {
    use std::io::IsTerminal as _;

    anyhow::ensure!(
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        "{} is in the directory this was started in, and a settings file found there is read \
         only when somebody at a terminal says it may be. `--config-file {}` reads it",
        path.display(),
        path.display()
    );

    config::trusted(
        path,
        granting,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
    )
    .context("could not ask about the settings file")
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
    /// The file the arguments were filled in from, where there was one at all; see
    /// [`Args::setup_from`].
    pub filed: Option<Filed>,
}

/// The settings file these arguments were filled in from, and which of them were *typed*.
///
/// note: the two things a refusal about somebody else's value has to answer and the struct cannot.
/// The path is what a value has to name to be found, and the matches are what says whether it came
/// from the file or from a flag on the command line - which is the difference between naming the
/// file and leaving somebody to wonder. A value that failed after the merge is a value somebody
/// wrote down, and a file applies because of where they are standing as much as anything else.
pub struct Filed {
    /// The file read, named the way `Settings::read` names it on its own errors.
    pub at: std::path::PathBuf,
    /// What clap matched, which is the only thing that can say which arguments were *typed*.
    pub matches: clap::ArgMatches,
    /// The keys this file carried that a served session does not read, as the argument's own name.
    ///
    /// note: asked here rather than read back off the struct, because the struct cannot say where
    /// a value came from: `--on-ask` has a default, so a merged `Args` looks the same whether the
    /// file set it or nobody did. What the file carried is the fact, and it is the fact the refusal
    /// needs - a settings file is a person writing `deadline` down once rather than typing it every
    /// day, which is exactly the case that gets dropped without a word. See
    /// [`Given::unserved`].
    pub unserved: Vec<&'static str>,
    /// Every key this file gave a value, as the argument's own name.
    ///
    /// note: what says a value is the file's. Not being typed does not: a value nobody typed may be
    /// a default, or a resumed session's own name, and a refusal naming the file for one of those
    /// sends somebody to read a file that does not say it.
    pub carried: Vec<String>,
}

impl Filed {
    /// Whether this argument was written on the command line, which is what says a value was not
    /// read out of the file.
    ///
    /// note: `false` for a name clap knows nothing of, rather than a panic. Some settings are
    /// `#[arg(skip)]` - a file only, with no argument behind them - and a refusal about one of
    /// those has to be able to ask where the value came from like any other.
    pub fn typed(&self, name: &str) -> bool {
        self.matches.ids().any(|id| id.as_str() == name)
            && self.matches.value_source(name) == Some(ValueSource::CommandLine)
    }
}

impl Given {
    /// What this command line carries that a served session does not read, and where each came
    /// from.
    ///
    /// note: the two a served session has no loop to read - `--deadline`, which ends a headless
    /// run, and `--on-ask`, which answers a question nobody is there to answer. Read off the
    /// arguments rather than off clap's matches alone, because a value out of a settings file is
    /// written into [`Args`] by [`Args::under`] and leaves nothing behind saying it was not typed -
    /// so a check on the matches sees a file's `deadline` as absent, and a run that serves for
    /// ever beside a file saying when it should have ended says nothing at all.
    ///
    /// note: a file's value names the file, the way every other value out of one does. A person
    /// writing `deadline` into a project's `kamchatka.json` is not looking at this run's command
    /// line at all, so a refusal naming only `--deadline` would be a flag they never typed.
    ///
    /// note: a file's `on-ask: deny` is not among them, because that is the default and a served
    /// run would have had it anyway - `--print-config` writes it, and refusing every `--serve`
    /// over the file this program hands out would break the one it tells a reader to write. See
    /// `Args::given`.
    pub fn unserved(&self) -> String {
        let typed = |id: &str| {
            self.matches
                .value_source(id)
                .is_some_and(|source| source == ValueSource::CommandLine)
        };
        // the argument and the key it stands for beside it, because one is `on_ask` and the other
        // is `on-ask`, and a refusal that mixed them up would name a setting nobody wrote
        let mut said = Vec::new();
        let mut filed = None;
        for (id, key) in [("deadline", "deadline"), ("on_ask", "on-ask")] {
            // typed first, because a value on the command line is somebody looking at this run,
            // and a file's would only say where it was written down
            if typed(id) {
                said.push(format!("`--{}`", id.replace('_', "-")));
            } else if let Some(from) = self.filed.as_ref()
                && from.unserved.contains(&key)
            {
                said.push(format!("`{key}`"));
                filed = Some(from.at.display().to_string());
            }
        }

        match (said.is_empty(), filed) {
            (true, _) => String::new(),
            // the file named once however many of its keys were dropped, at the end of the clause
            // rather than beside each: a path is long, and what somebody reads is what it is for
            (false, Some(path)) => format!("{} in {path}", said.join(" or ")),
            (false, None) => said.join(" or "),
        }
    }
}

/// What this program does instead of running a session.
///
/// note: a command rather than a flag, unlike `--check`, because it takes a list and an output
/// and is a program of its own. `--check` reads; this writes a session somebody then resumes.
#[derive(clap::Subcommand)]
#[non_exhaustive]
pub enum Command {
    /// Fold several hard forks of one session into one session to carry on from, and stop.
    ///
    /// The items the forks share are kept whole. From each fork's own part only the notes the
    /// agent wrote down for itself are carried, and two that disagree are both kept and named.
    /// Nothing is sent to a model: `kamchatka -r` on what it wrote carries on, and `/request`
    /// shows what the first request would be.
    Reconcile {
        /// The forks: sessions `/save` wrote, each with the log beside it where there is one.
        #[arg(value_name = "PATH", num_args = 2.., required = true)]
        forks: Vec<String>,
        /// Where to write the session it makes, as a pair `/load` reads. Nothing there is
        /// written over.
        #[arg(short, long, value_name = "PATH")]
        output: String,
    },
}

/// What an unanswerable question is answered with.
///
/// note: `deny` is the default, and that is the difference between this and `examples/recorded.rs`,
/// which grants every question it is asked. That is right for a recording somebody is watching and
/// wrong for a program: a run nobody is watching should not be able to do a thing nobody has
/// allowed, and `--allow exec` is one flag away for anyone who means it.
///
/// note: `#[non_exhaustive]`, which is what every public enum in this workspace carries - it was
/// missing, and `Leave` is what found out.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[non_exhaustive]
pub enum OnAsk {
    /// Refuse it. The model is told, and told that it was this call rather than a standing rule.
    Deny,
    /// Grant it.
    Allow,
    /// Answer nothing, and leave it for the next client to attach to the same session, or for this
    /// one coming back. `--connect` only: a headless run has nobody else to leave it to.
    Leave,
}

impl OnAsk {
    /// The answer itself, as the kernel spells it; `None` for [`OnAsk::Leave`], which gives none.
    pub fn grant(self) -> Option<Grant> {
        match self {
            Self::Deny => Some(Grant::Deny),
            Self::Allow => Some(Grant::Allow),
            Self::Leave => None,
        }
    }
}
