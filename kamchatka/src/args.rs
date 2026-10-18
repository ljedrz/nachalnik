//! Command-line arguments, and the session assembled from them.
//!
//! note: these live in the library rather than in `main.rs` because a session assembled from these
//! arguments is what this crate is for, and embedders build sessions from them too instead of
//! keeping a second set of flags in sync.
//!
//! note: `main.rs` keeps the part that is the program's own: which loop drives the session, where
//! the record is written, and the refusal that `--connect` assembles nothing.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::parser::ValueSource;
use clap::{CommandFactory as _, FromArgMatches as _, Parser, ValueEnum as _};
use nachalnik::Grant;
use nachalnik_providers::Dialect;

use crate::{
    app::App,
    config::{self, Api, Compact, Settings},
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
/// has an `--rate-shell-commands` to use them and by no other. A variable named in the help of a
/// program that reads it nowhere is the same failure as a settings key nothing consults.
pub fn environment() -> String {
    /// The advisor's three, listed by a build that has an `--rate-shell-commands` and empty in one
    /// that does not.
    #[cfg(feature = "shell-advisor")]
    const ADVISOR: &str = "

The advisor, which is only ever asked when --rate-shell-commands is given:
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
  ANTHROPIC_API_KEY        the key for Anthropic's own API, with --api anthropic, and only there
  KAMCHATKA_BASE_URL       where the requests go, e.g. http://localhost:11434/v1 for ollama;
                           OpenRouter by default, Google's own v1beta with --api gemini, or
                           Anthropic's own with --api anthropic; with --api responses,
                           OpenRouter or https://api.openai.com/v1 for OpenAI's own
  KAMCHATKA_CONTEXT_LIMIT  the model's context size, for a provider that will not say
  KAMCHATKA_NO_ATTRIBUTION set to stop naming this program to OpenRouter{ADVISOR}

Exit status of a headless run: 0 done, 1 failed, 3 --max-spend-tokens reached, 4 paused at
--max-requests-per-turn, 5 done but the record could not be written, 124 --max-run-seconds
passed, 129/130/143 SIGHUP, ctrl+c or SIGTERM - the last three for a served one too"
    )
}

/// A terminal agent that shows its context.
///
/// note: `#[non_exhaustive]` because [`Args::given`] constructs it, so adding a flag later is a
/// patch rather than a breaking change for anyone assembling sessions.
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

    /// Which API the requests speak. `chat-completions` is OpenAI's, which OpenRouter and most
    /// local servers speak too. `responses` is OpenAI's Responses API, so a turn keeps its order
    /// and a reasoning model's thinking goes back sealed rather than being redone every turn.
    /// OpenRouter answers it as well as OpenAI; for OpenAI's own use
    /// `KAMCHATKA_BASE_URL=https://api.openai.com/v1`. `anthropic` is Anthropic's Messages API, so
    /// a turn keeps its order and its signed thinking goes back as it came. OpenRouter speaks it
    /// too, at `https://openrouter.ai/api/v1`. `gemini` is Google's own, so a turn keeps the order
    /// it was produced in: thinking, a sentence, a tool call, more thinking.
    #[arg(long, value_enum, value_name = "API", default_value_t = Api::ChatCompletions)]
    pub api: Api,

    /// Ask a second model where each shell command a question is about lands on a three-level
    /// rubric, and colour the question by the answer - a command joined at its `|`, `&&` or `;`
    /// is asked about stage by stage and rated by its worst one, underlined where it is worse than
    /// the rest. The rating decides nothing: what the rules allow runs and what they refuse is
    /// refused. Sends the call's tool name, capabilities and arguments to the advisor, a System
    /// One model named by KAMCHATKA_SYSTEM1_MODEL; see KAMCHATKA_SYSTEM1_BASE_URL for one on this
    /// machine, where nothing leaves it.
    #[cfg(feature = "shell-advisor")]
    #[arg(long)]
    pub rate_shell_commands: bool,

    /// A file to put in the context, pinned; a PDF or an image goes in as itself. May be
    /// repeated, and `/attach` is the same thing at the prompt.
    #[arg(short, long, value_name = "PATH")]
    pub file: Vec<String>,

    /// A system prompt, pinned. The runtime ships none of its own.
    #[arg(short, long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// Carry on from a session written by `/save`.
    #[arg(short, long, value_name = "PATH")]
    pub resume: Option<String>,

    /// An MCP server to run, offering its tools, as `[name=]command`. May be repeated. The command
    /// is split on whitespace, with no quoting, so none of its arguments can hold a space.
    #[cfg(feature = "mcp")]
    #[arg(long, value_name = "COMMAND")]
    pub mcp: Vec<String>,

    /// How many requests one turn may make before it stops and asks; `0` is no limit at all.
    /// `/requests N` changes it while the session runs.
    #[arg(long, value_name = "N", default_value_t = 8)]
    pub max_requests_per_turn: usize,

    /// How full the context may get before its oldest exchanges are excluded, which they can be
    /// restored from; `1` never compacts at all. A second fraction after a comma is how far down a
    /// full context is taken once they start going, at most the first; left out, twenty points
    /// under it or half of it, whichever is more.
    #[arg(long, value_name = "FRACTION[,TARGET]", default_value = "0.8")]
    pub compact_context_at: Compact,

    /// Let the model's tool calls run at the same time, instead of in the order it asked.
    #[arg(long)]
    pub parallel_tool_calls: bool,

    /// Offer this program's own tools with every argument at the top level of a call, rather than
    /// inside one `call` object. For a model whose own call format is flat, which can slip back
    /// into it partway through a nested object and send a call that cannot be read. Tools from an
    /// MCP server are offered as the server wrote them.
    #[arg(long)]
    pub flat_tool_arguments: bool,

    /// Send each of the model's turns back with the thinking that produced it, as `reasoning`,
    /// with `--api chat-completions`. For a model that refuses every request after its first tool
    /// call otherwise. Off by default: most endpoints refuse a field they do not know, and the
    /// thinking costs its tokens on every request.
    #[arg(long)]
    pub send_reasoning: bool,

    /// Run with no confinement at all: the shell reaches whatever the user running this can, and
    /// `fs`, which is not a process, is not confined to the working directory.
    #[arg(long)]
    pub no_sandbox: bool,

    /// Do not write the session out when it ends. It is written to a temporary directory
    /// otherwise, and the path is the last thing printed.
    #[arg(long)]
    pub no_record: bool,

    /// A path outside the working directory the tools may also read and write. May be repeated,
    /// and takes a comma-separated list.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    pub sandbox_write: Vec<std::path::PathBuf>,

    /// A path outside the working directory the tools may read but not change. The same, and a
    /// toolchain is the usual one: `cargo` cannot start without `~/.rustup`.
    #[arg(long, value_name = "PATH", value_delimiter = ',')]
    pub sandbox_read: Vec<std::path::PathBuf>,

    /// A `/dev` device the shell may read and write. The list replaces the usual one rather than
    /// adding to it. Everything else under `/dev` stays out of reach.
    #[arg(long, value_name = "PATH", value_delimiter = ',', default_values = crate::sandbox::DEVICES)]
    pub sandbox_device: Vec<std::path::PathBuf>,

    /// Drop the whole of a tool's output once it has been shortened, rather than keeping it as an
    /// excluded item that can still be read.
    #[arg(long)]
    pub discard_cut_off_output: bool,

    /// Send a request even if the counter says it is over the model's context, and let the endpoint
    /// say no. For a wrongly advertised limit or a drifted counter, the endpoint's own count is
    /// worth more than any guess made here.
    #[arg(long)]
    pub send_oversized_requests: bool,

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
    pub auto_allow: Vec<String>,

    /// Refuse one, the same way. The strictest of everything consulted wins, so this beats
    /// `--auto-allow`.
    #[arg(long, value_name = "SUBJECT", value_delimiter = ',')]
    pub auto_deny: Vec<String>,

    /// Allow every tool an MCP server offers, by name.
    ///
    /// It is a separate argument because a server name and a domain are both bare words.
    #[arg(long, value_name = "NAME", value_delimiter = ',')]
    pub auto_allow_server: Vec<String>,

    /// Refuse one, the same way.
    #[arg(long, value_name = "NAME", value_delimiter = ',')]
    pub auto_deny_server: Vec<String>,

    /// What to do with a question nobody is there to answer: in a headless run, and in a
    /// `--connect` run once its input has closed. `--connect` leaves it for somebody else unless
    /// this is given; `leave` is for `--connect` alone.
    #[arg(long, value_name = "ANSWER", default_value = "deny")]
    pub on_ask: OnAsk,

    /// Stop a headless run after this many seconds, however far it has got; `0` is no deadline.
    /// What has arrived is kept and the session is written out as usual.
    #[arg(long, value_name = "SECONDS")]
    pub max_run_seconds: Option<u64>,

    /// How many seconds the model's answer may say nothing - before its first word or between two -
    /// before the request is given up on; 150 unless given. A free endpoint under load can hold a
    /// request in its queue longer than that and then answer it normally.
    #[arg(long, value_name = "SECONDS")]
    pub max_silence_seconds: Option<u64>,

    /// How many seconds a call to an MCP server's tool may wait for its answer before it is given
    /// up on. 600 unless given. `0` waits as long as the server takes.
    #[arg(long, value_name = "SECONDS")]
    pub mcp_timeout_seconds: Option<u64>,

    /// Stop the session once the provider has charged this many tokens for it, in and out. `0`
    /// means no ceiling. `/spend` changes it while it runs.
    #[arg(long, value_name = "TOKENS")]
    pub max_spend_tokens: Option<u64>,

    /// Leave background `shell` commands running when the session ends instead of stopping them.
    /// The end of the session names each one either way.
    #[arg(long)]
    pub leave_running: bool,

    /// A JSON file of settings for things you would otherwise type every time. Command-line values
    /// win over the file. Given none, `./kamchatka.json` is read if it exists and you agree when
    /// asked, and the one under your config directory if it does not exist.
    #[arg(long, value_name = "PATH")]
    pub config_file: Option<std::path::PathBuf>,

    /// Print the settings file this program ships with and stop, for editing into one of your
    /// own: `kamchatka --print-config > kamchatka.json`.
    #[arg(long)]
    pub print_config: bool,

    /// Read a session's record, report what does not add up, and stop. PATH is the log, the
    /// snapshot, or their name without the suffix; whichever of the pair exists is read: lines that
    /// are not records, events this version does not know, records missing or numbered twice, calls
    /// asked for and never finished, and a snapshot that disagrees with its log. Nothing is
    /// started. Anything found makes the exit status non-zero.
    #[arg(long, value_name = "PATH")]
    pub check: Option<String>,

    /// The frame's colour, which only a settings file can say; `None` is the terminal's own
    /// foreground colour. See `Settings::border_color`.
    ///
    /// note: `skip` because nobody would type it twice, and it stays on `Args` so the one merge in
    /// `under` remains the one place the file meets the command line. A second path from settings
    /// to the program is a second set of rules about which wins.
    #[arg(skip = Some(config::BORDER_COLOR.to_owned()))]
    pub border_color: Option<String>,

    /// Which tools to offer at startup, which only a settings file can say. See `Settings::tools`.
    ///
    /// note: `skip` for the reason above, and because which tools are worth their tokens is a fact
    /// about the project, which belongs in the file beside it rather than on the command line.
    /// Turn one off for one session with `/tools toggle`, when wanted rather than before the
    /// session starts.
    #[arg(skip)]
    pub tools: Option<Vec<String>>,
}

impl Args {
    /// Fills in everything the command line did not say from a settings file.
    ///
    /// note: it asks clap which arguments were typed, rather than comparing against the defaults,
    /// because those are not the same question: `--max-requests-per-turn 8` is the default and
    /// somebody meant it, and a merge that could not tell them apart would let a file quietly
    /// override what was asked for. `ValueSource::CommandLine` is the only answer that counts, and
    /// it is why [`Args::given`] hands back the matches as well as the struct.
    ///
    /// note: a list from the command line replaces the file's rather than adding to it. One rule
    /// for every key is the only kind anybody can predict, and the other way round there is no way
    /// to ask for fewer than the file says.
    pub fn under(mut self, settings: Settings, matches: &clap::ArgMatches) -> Result<Self> {
        let typed =
            |name: &str| matches.value_source(name) == Some(clap::parser::ValueSource::CommandLine);
        // one macro rather than an `if` per field, and it names the argument once: clap knows the
        // field by its own name, so renaming a field without its entry here stops compiling rather
        // than silently misbehaving
        macro_rules! fill {
            ($($field:ident),* $(,)?) => {$(
                if !typed(stringify!($field)) && let Some(value) = settings.$field {
                    self.$field = value.into();
                }
            )*};
        }

        fill!(
            api,
            max_requests_per_turn,
            compact_context_at,
            parallel_tool_calls,
            flat_tool_arguments,
            send_reasoning,
            no_sandbox,
            sandbox_write,
            sandbox_read,
            sandbox_device,
            auto_allow,
            auto_deny,
            auto_allow_server,
            auto_deny_server,
            max_run_seconds,
            max_silence_seconds,
            mcp_timeout_seconds,
            max_spend_tokens,
            discard_cut_off_output,
            send_oversized_requests,
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
        // note: not into a resumed session, whose context already holds the pinned system prompt
        // from the snapshot's first run. Pushed again on every `-r`, it would stack a copy per
        // resume beyond compaction's reach. A typed `-s` beside `-r` is still honoured
        if self.system_prompt.is_none() && self.resume.is_none() {
            self.system_prompt = settings.system_prompt;
        }
        // parsed here rather than where it is drawn, so a colour nobody can parse is a startup
        // error naming the file rather than a quietly default frame. There is no argument to lose
        // to, so nothing to ask `typed` about.
        //
        // note: a `null` is the terminal's colour, not a missing key
        match settings.border_color {
            None => {}
            Some(None) => self.border_color = None,
            Some(Some(border)) => {
                config::rgb(&border)
                    .map_err(|e| anyhow::anyhow!("`border-color` in the settings file: {e}"))?;
                self.border_color = Some(border);
            }
        }
        // the other one has no argument to lose to. `Setup::wire` checks the names, since the tools
        // it would check against are the ones it is about to build
        self.tools = settings.tools;
        if let Some(on_ask) = settings.on_ask.filter(|_| !typed("on_ask")) {
            self.on_ask = OnAsk::from_str(&on_ask, true).map_err(|_| {
                // validated strictly, not with clap's "invalid variant: maybe". A settings file has
                // no `--help` beside it, and `deny` and `allow` are the only answers
                anyhow::anyhow!(
                    "`on-ask` in the settings file is what a question nobody is there to answer \
                     gets, and this is `{on_ask}`: it is `deny` or `allow`"
                )
            })?;
            // note: runs that have nobody else to ask read the file, and `leave` there means a
            // question nobody will ever answer. `--connect` does not read the file's `on-ask` at
            // all; see `Args::given`
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
            // every key, `mcp` included, so a stricter rule would make the shipped starting point
            // unusable in a build with no MCP
            #[cfg(not(feature = "mcp"))]
            Some(mcp) if !mcp.is_empty() => anyhow::bail!(
                "this build has no MCP support, so `mcp` in the settings file cannot be honoured"
            ),
            _ => {}
        }

        // the same rule for the same reason: a settings file that asked for its commands rated, in
        // a build with no advisor in it, would run every question uncoloured and say nothing
        match settings.rate_shell_commands {
            #[cfg(feature = "shell-advisor")]
            Some(rate) if !typed("rate_shell_commands") => self.rate_shell_commands = rate,
            #[cfg(not(feature = "shell-advisor"))]
            Some(true) => anyhow::bail!(
                "this build has no advisor in it, so `rate-shell-commands` in the settings file \
                 cannot be honoured"
            ),
            _ => {}
        }

        Ok(self)
    }

    /// The command line as typed, with unset arguments filled in from a settings file.
    ///
    /// note: the matches come back with the arguments because [`Args::under`] needs to know which
    /// were typed; the struct cannot tell a value that equals its default from one somebody wrote.
    ///
    /// note: a typed path is not announced and a found path is. Someone who wrote `--config-file`
    /// knows; someone who walked into a directory with one does not. A file that applies because of
    /// where you are standing must say so rather than be discovered later by its effects. Saying so
    /// is the caller's, because there is no session to say it into yet.
    ///
    /// note: nothing is looked for under `--connect`, which takes nothing else: the model, key,
    /// tools and sandbox belong to whoever is serving, and a file here would be settings for a
    /// session this process is not assembling.
    pub fn given() -> Result<Given> {
        let matches = Self::command().get_matches();
        let mut args = Self::from_arg_matches(&matches)
            .map_err(|e| e.exit())
            .unwrap();
        // note: before anything is looked for, because `--print-config > kamchatka.json` means the
        // shell has emptied that file before this runs, so reading it first is a parse error about
        // the file this was about to write. `--check` and a command run no session for a file's
        // settings to be about
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
            // the path the way `Settings::read` puts it on its own errors: `under` refuses a value
            // in this file, and there are two places a file can be found.
            //
            // the two a served session does not read, taken before `under` moves them into the
            // struct, where nothing can tell a file's value from a default.
            //
            // note: only where the file's value is one this run would not otherwise have had.
            // `--print-config` writes every key, so the file this program hands out to be edited
            // carries `on-ask: deny` - the default, which changes nothing, and refusing every
            // `--serve` over it would break the file the documentation tells a reader to write. A
            // file saying `allow` is somebody's decision, and that is refused. Comparison is
            // case-insensitive, or `Deny` would be read as the default and refused as something
            // else
            let unserved = [
                ("max-run-seconds", settings.max_run_seconds.is_some()),
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
            // every key the file gave a value, spelled as the argument it stands in for: what says
            // a refused value was the file's rather than a default's or a snapshot's
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

            // note: asked after the file is read and merged, which only look at it, so a file this
            // program would refuse is refused with what is wrong with it rather than asked about
            // first
            if found.as_deref().is_some_and(config::underfoot) && !asked(&path, &granting)? {
                args = Self::from_arg_matches(&matches)
                    .map_err(|e| e.exit())
                    .unwrap();
                found = None;
                filed = None;
            }
        }

        // note: last of all, so that `-m`, `KAMCHATKA_MODEL` and a settings file's `model` still
        // win: the record is what a session was talking to, and any of those is somebody saying
        // what this one should. A snapshot holds the conversation and not the model, so `-r`
        // without this started a session with none, and a headless one given a message sent nothing
        // and said so to nobody who was looking
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
    /// note: the resumed session is read here rather than in [`Setup`], because the error messages
    /// worth writing - which path, and whether it was a session at all - belong to whoever was
    /// handed the path.
    pub fn setup(&self) -> Result<Setup> {
        self.setup_from(None)
    }

    /// [`Args::setup`], for a run that read a settings file, and the file's own refusals with it.
    ///
    /// note: where the merge's refusals ended and these began. `under` holds a value to the file
    /// and names the key. `Setup::check` and the wiring hold them to what this program can do, and
    /// say nothing about where a value came from. A file found underfoot is announced on standard
    /// error, where a terminal's screen covers it and a failure has already ended the run, so the
    /// two run together. Anything out of `self` rather than out of the file is left as it was said;
    /// somebody who typed `--auto-deny` is looking at it.
    ///
    /// note: a parameter rather than a field on [`Args`] because the file is not a setting: it
    /// holds which path and which arguments were typed, and the latter tells a file's value from a
    /// flag's.
    pub fn setup_from(&self, filed: Option<&Filed>) -> Result<Setup> {
        // one function for refusals about the arguments: a file value is answered with the file
        // beside it, and a value off the command line is answered as it always was
        let at = |field: &str| filed_at(filed, field);
        // note: here rather than where the limit is read, because that is a round trip away and a
        // session measuring itself against nothing is not a session anybody finds out about. Every
        // other figure this program settles at startup is refused here, and this one used to be
        // read out of the environment and dropped when it did not parse
        endpoint::checked_limit().map_err(|e| anyhow::anyhow!("{e}"))?;
        // note: refused rather than read as no limit, as `0` is for `--max-run-seconds`: a stream
        // given no silence at all is given up on at its first, which is no session anybody meant
        anyhow::ensure!(
            self.max_silence_seconds != Some(0),
            "{}`max-silence-seconds` is how many seconds an answer may say nothing, and `0` would \
             give every request up at once; leave it out for the default of 150",
            at("max_silence_seconds").unwrap_or_default()
        );
        // note: here rather than in the parser, so a settings file's `compact-context-at` is held
        // to it too. It is a fraction above 0 and at most 1: `80`, meant as a percentage, compacts
        // nothing, and anything at or below zero compacts every tool result
        anyhow::ensure!(
            self.compact_context_at.threshold > 0.0 && self.compact_context_at.threshold <= 1.0,
            "{}`compact-context-at` is how full the context may get, as a fraction above 0 and at \
             most 1 - `0.8` rather than `80`, and `1` never compacts - and this was `{}`",
            at("compact_context_at").unwrap_or_default(),
            self.compact_context_at.threshold
        );
        // note: at most the threshold, because a target above it means the context is over the
        // threshold and already under the target, so a pass drops nothing before every request
        // while the context stays between the two. Equal is allowed: the pass drops just enough
        // each time
        if let Some(target) = self.compact_context_at.target {
            anyhow::ensure!(
                target > 0.0 && target <= self.compact_context_at.threshold,
                "{}`compact-context-at`'s second fraction is how far down a full context is taken, \
                 above 0 and at most its first, which is `{}` - and this was `{target}`",
                at("compact_context_at").unwrap_or_default(),
                self.compact_context_at.threshold
            );
        }
        // note: refused for the reason `wiring::unreached` refuses a domain no tool declares. A
        // deny naming a server this run does not start matches nothing, so a misspelled
        // `--auto-deny-server` read as given and left the server it meant to the question - which a
        // headless run with `--on-ask allow` answers yes. An auto-allow of one grants nothing, and
        // is only said; see `Args::unstarted`
        let servers = self.servers();
        if let Some(unknown) = self
            .auto_deny_server
            .iter()
            .find(|name| !servers.contains(name))
        {
            anyhow::bail!(
                "{}`{unknown}` is not a server this run starts; {}",
                at("auto_deny_server").unwrap_or_default(),
                these(&servers)
            );
        }
        // note: an `allow` of `mcp:call` or `mcp` matches no call and is refused for the same
        // reason. A server's tools are judged under its name instead of `mcp:call`; see
        // `Careful::judges`. Every server here is one this program started, so the rule read as
        // given and every call still went to the question. A `deny` of either is consulted
        if let Some(granted) = self.auto_allow.iter().find(|rule| {
            matches!(
                Subject::parse(rule).to_string().as_str(),
                "mcp" | "mcp:call"
            )
        }) {
            let at = at("auto_allow").unwrap_or_default();
            anyhow::bail!(
                "{at}`--auto-allow {granted}` grants nothing: a tool from an MCP server is judged \
                 under the server's name, so `--auto-allow-server NAME` is what lets one through"
            );
        }
        let resume = match &self.resume {
            Some(path) => {
                let snapshot: nachalnik::Snapshot = serde_json::from_slice(
                    &std::fs::read(path).with_context(|| format!("could not read {path}"))?,
                )
                .with_context(|| format!("{path} is not a session"))?;
                // refused rather than repaired. The runtime would renumber or reserve its way
                // around most of these, and a session carried on from a record that had to be
                // changed to be read no longer has a record that says what happened
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
            // the runtime's default counter restarts at 0 with the process. It is fine as an
            // identity but useless as a filename: every session would overwrite the last record. A
            // resumed session keeps the name in its snapshot, so resuming appends to the same
            // session instead of starting a second, unrelated one
            session_name: self.resume.is_none().then(|| {
                let started = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|since| since.as_secs())
                    .unwrap_or_default();

                App::session_stamp(started)
            }),
            requests: (self.max_requests_per_turn > 0).then_some(self.max_requests_per_turn),
            parallel: self.parallel_tool_calls,
            // what the runtime keeps is a decision about retention, and retention here is a file
            // somebody has to store: `/save` writes the snapshot, and an excluded output goes into
            // it whole. One `grep` that wanders into `./target/` can be megabytes of build noise
            // nobody will read, and it is in every save of that session from then on
            keep_truncated: !self.discard_cut_off_output,
            refuse_oversized: !self.send_oversized_requests,
            record: !self.no_record,
            compact: Some(self.compact_context_at.threshold),
            compact_target: self.compact_context_at.target,
            // note: `0` is no ceiling, as `/spend 0` and `--max-requests-per-turn 0` say it. A
            // ceiling of nothing would be a session that refuses its first turn without saying why
            spend: self.max_spend_tokens.filter(|it| *it > 0),
            leave_running: self.leave_running,
            confine: !self.no_sandbox,
            reachable: self.sandbox_write.clone(),
            readable: self.sandbox_read.clone(),
            devices: self.sandbox_device.clone(),
            layout: match self.flat_tool_arguments {
                true => crate::tools::Layout::Flat,
                false => crate::tools::Layout::Wrapped,
            },
            allow: self
                .auto_allow
                .iter()
                .map(|it| Subject::parse(it))
                .chain(self.auto_allow_server.iter().cloned().map(Subject::Server))
                .collect(),
            deny: self
                .auto_deny
                .iter()
                .map(|it| Subject::parse(it))
                .chain(self.auto_deny_server.iter().cloned().map(Subject::Server))
                .collect(),
            tools: self.tools.clone(),
            system: self.system_prompt.clone(),
            files: self.file.clone(),
            #[cfg(feature = "shell-advisor")]
            advisor: None,
        };

        // the last of the refusals, and the first one a value out of a file can reach without
        // saying which file it came from. What this program offers and what a rule can match are
        // both decided here, so a tool, a server and a path rule are all refused after the merge
        // rather than in it. The refusal says which setting it was, so the file is named for what
        // the file said and for nothing else
        setup.check().map_err(|refused| {
            let at = refused
                .setting
                .and_then(|setting| at(&setting.replace('-', "_")))
                .unwrap_or_default();
            anyhow::anyhow!("{at}{refused}")
        })?;

        Ok(setup)
    }

    /// What to say about the servers `--auto-allow-server` names that this run does not start;
    /// `None` where it names none.
    ///
    /// note: said rather than refused, unlike a deny. Which servers run is up to the command line,
    /// so a settings file that allows the person's usual servers is right in general and was
    /// refused whenever `--mcp` left one out. An allow of a server that is not there grants
    /// nothing, which is the safe way for it to be wrong.
    pub fn unstarted(&self, filed: Option<&Filed>) -> Option<String> {
        let servers = self.servers();
        let absent: Vec<&str> = self
            .auto_allow_server
            .iter()
            .filter(|name| !servers.contains(name))
            .map(String::as_str)
            .collect();
        (!absent.is_empty()).then(|| {
            format!(
                "{}`--auto-allow-server` names {}, which this run does not start, so it grants \
                 nothing there; {}",
                filed_at(filed, "auto_allow_server").unwrap_or_default(),
                absent
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                these(&servers)
            )
        })
    }

    /// How long a call to an MCP server's tool may wait: `--mcp-timeout-seconds`, ten minutes by
    /// default, and no limit for `0`.
    ///
    /// note: ten minutes because a tool may be a build or a search that takes a while. The timeout
    /// is for a tool that never answers; a turn can still be interrupted sooner.
    pub fn mcp_within(&self) -> Option<std::time::Duration> {
        match self.mcp_timeout_seconds.unwrap_or(MCP_TIMEOUT) {
            0 => None,
            seconds => Some(std::time::Duration::from_secs(seconds)),
        }
    }

    /// The names of the servers this run starts, as `attach` names them.
    fn servers(&self) -> Vec<String> {
        #[cfg(feature = "mcp")]
        return self
            .mcp
            .iter()
            .map(|spec| crate::mcp::named(spec).0)
            .collect();
        #[cfg(not(feature = "mcp"))]
        Vec::new()
    }

    /// The same setup, with the advisor attached where `--rate-shell-commands` asked for one.
    ///
    /// note: this runs after [`Setup::check`] and before the provider. A missing advisor key is a
    /// fact about the arguments, so finding it should not cost a round trip to the other endpoint.
    /// A session asked for with `--rate-shell-commands` that cannot reach an advisor is refused,
    /// rather than quietly run with every question uncoloured. Someone who turned this on is
    /// entitled to have it on or to be told it is not.
    ///
    /// note: nothing the advisor has to say is drained here. The session reports all of it through
    /// `App::on_event` and the drawn loop's tick. A queue that somebody else popped is missing its
    /// first line; for a local advisor, that line says it is not ready yet. Printing to stderr here
    /// would not help: the screen arrives immediately and clears it.
    #[cfg(feature = "shell-advisor")]
    pub async fn advised(&self, setup: Setup) -> Result<Setup> {
        if !self.rate_shell_commands {
            return Ok(setup);
        }

        let engine = endpoint::advise::connect(&endpoint::session_endpoint(self.wire()))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("could not reach the advisor")?;
        Ok(Setup {
            advisor: Some(engine),
            ..setup
        })
    }

    /// The wire format this session speaks, as `--api` names it.
    pub fn wire(&self) -> endpoint::Wire {
        match self.api {
            Api::ChatCompletions => endpoint::Wire::OpenAi,
            Api::Responses => endpoint::Wire::Responses,
            Api::Anthropic => endpoint::Wire::Anthropic,
            Api::Gemini => endpoint::Wire::Gemini,
        }
    }

    /// Where this session's requests go, in the dialect asked for.
    ///
    /// note: there are four wire formats and one trait. `--api` selects the format. The kernel, the
    /// screen, `/model`, and `/endpoint` hold a `Dialect` and never find out which one it is.
    ///
    /// note: no model is used unless one was named. A default would make a session started without
    /// `-m` talk to whatever this program's author picked, at the person's expense and without
    /// saying the choice was not theirs. Without a model the session still starts: the address and
    /// key are settled, `/models` lists what is served, and `/model` picks one. Until a model is
    /// picked the kernel holds no provider, so nothing can be sent. See `Setup::wire`.
    pub async fn provider(&self) -> Result<Arc<dyn Dialect>> {
        let model = self.model.as_deref();
        let provider = match self.wire() {
            endpoint::Wire::Gemini => endpoint::gemini::connect(model)
                .await
                .map(|it| it as Arc<dyn Dialect>),
            endpoint::Wire::Anthropic => endpoint::anthropic::connect(model)
                .await
                .map(|it| it as Arc<dyn Dialect>),
            endpoint::Wire::Responses => endpoint::responses::connect(model)
                .await
                .map(|it| it as Arc<dyn Dialect>),
            endpoint::Wire::OpenAi => endpoint::connect_with(model, self.send_reasoning)
                .await
                .map(|it| it as Arc<dyn Dialect>),
        }
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("could not reach the model")?;
        if let Some(seconds) = self.max_silence_seconds {
            provider.set_patience(std::time::Duration::from_secs(seconds));
        }

        Ok(provider)
    }
}

/// Moves a snapshot's numbering past everything the log beside it numbered after the snapshot was
/// taken.
///
/// note: the snapshot is rewritten when a turn begins and when the session rests, and the log is
/// appended to as each event happens. A run killed in the middle of a turn therefore leaves a log
/// that runs past its snapshot. If the snapshot were carried on as it stands, the next session
/// would number records, items, calls and permissions again from where it was taken, and two files
/// of one session would say different things under one identifier. `-r` already reads this log for
/// `App::recall`; one that is missing or cut off mid-line costs what it cannot say.
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
/// note: this is the last `model.changed` or `model.requested` in the record, whichever came later,
/// so a `/model` switch the session made is the one it carries on with. A log that runs past the
/// snapshot, as a killed run leaves, is read to its end like everything else here. A switch to no
/// model is `None`, and so is a record that is missing or names none.
fn last_model(path: &std::path::Path) -> Option<String> {
    last_talked_to(path)
        .map(|info| info.model)
        .filter(|model| !model.is_empty())
}

/// How many seconds a call to an MCP server's tool waits unless `--mcp-timeout-seconds` says
/// otherwise.
const MCP_TIMEOUT: u64 = 600;

/// Where a refusal about `field` names the settings file: only where the file said it - carried the
/// key, and was not overruled by a flag. Asked the other way round, "not typed" named the file for
/// every default.
fn filed_at(filed: Option<&Filed>, field: &str) -> Option<String> {
    match filed {
        Some(filed) if !filed.typed(field) && filed.carried.iter().any(|key| key == field) => {
            Some(format!("{}: ", filed.at.display()))
        }
        _ => None,
    }
}

/// The servers a run starts, as a clause for a sentence about one it does not.
fn these(servers: &[String]) -> String {
    match servers.is_empty() {
        true => "it starts none".to_owned(),
        false => format!("they are {}", servers.join(", ")),
    }
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
/// note: refuse rather than run without the file, because a file carrying `max-run-seconds` or
/// `max-spend-tokens` is a script's bounds, and a script that quietly lost them would run
/// unbounded. A run nobody can ask names the file with `--config-file`.
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

/// The arguments, what clap matched, and the settings file they were filled in from where one was
/// found rather than named.
///
/// note: not `#[non_exhaustive]`, which every other struct this crate answers with carries; `Wired`
/// beside it is the same exception for the same reason. This struct exists to be taken apart at the
/// call site. The attribute would make every caller write `..`, the one spelling that *hides* a
/// field added later rather than stopping the build over it.
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
/// note: a refusal about somebody else's value has to answer two things the struct cannot. The path
/// is what a value has to name to be found, and the matches say whether it came from the file or
/// from a flag on the command line; that is the difference between naming the file and leaving
/// somebody to wonder. A value that failed after the merge is a value somebody wrote down, and a
/// file applies because of where they are standing as much as anything else.
pub struct Filed {
    /// The file read, named the way `Settings::read` names it on its own errors.
    pub at: std::path::PathBuf,
    /// What clap matched, which is the only thing that can say which arguments were *typed*.
    pub matches: clap::ArgMatches,
    /// The keys this file carried that a served session does not read, as the argument's own name.
    ///
    /// note: asked here rather than read back off the struct, because the struct cannot say where a
    /// value came from: `--on-ask` has a default, so a merged `Args` looks the same whether the
    /// file set it or nobody did. What the file carried is the fact the refusal needs; a settings
    /// file is a person writing `max-run-seconds` down once rather than typing it every day, which
    /// is the case that gets dropped without a word. See [`Given::unserved`].
    pub unserved: Vec<&'static str>,
    /// Every key this file gave a value, as the argument's own name.
    ///
    /// note: what says a value is the file's. Not being typed does not: a value nobody typed may be
    /// a default, or a resumed session's own name, and a refusal naming the file for one of those
    /// sends somebody to read a file that does not say it.
    pub carried: Vec<String>,
}

impl Filed {
    /// Whether this argument was given on the command line rather than read from a settings file.
    ///
    /// note: returns `false` for a name clap knows nothing of, rather than panicking. Some settings
    /// are `#[arg(skip)]` - file only, with no argument behind them - so a refusal about one must
    /// be able to ask where the value came from like any other.
    pub fn typed(&self, name: &str) -> bool {
        self.matches.ids().any(|id| id.as_str() == name)
            && self.matches.value_source(name) == Some(ValueSource::CommandLine)
    }
}

impl Given {
    /// What this command line carries that a served session does not read, and where each came
    /// from.
    ///
    /// note: a served session has no loop to read `--max-run-seconds`, which ends a headless run,
    /// and `--on-ask`, which answers a question nobody is there to answer. Read off the arguments
    /// rather than clap's matches alone, because a value from a settings file is written into
    /// [`Args`] by [`Args::under`] and leaves no trace it was not typed - so the matches see a
    /// file's `max-run-seconds` as absent, and a run serving for ever beside a file saying when it
    /// should end says nothing at all.
    ///
    /// note: a file's value names the file, as every value from one does. A person writing
    /// `max-run-seconds` into `kamchatka.json` is not looking at this run's command line, so a
    /// refusal naming only `--max-run-seconds` names a flag they never typed.
    ///
    /// note: a file's `on-ask: deny` is not among them, because it is the default and a served run
    /// would have had it anyway. `--print-config` writes it, and refusing every `--serve` over the
    /// file this program hands out would break the file it tells a reader to write. See
    /// `Args::given`.
    pub fn unserved(&self) -> String {
        let typed = |id: &str| {
            self.matches
                .value_source(id)
                .is_some_and(|source| source == ValueSource::CommandLine)
        };
        // the argument and the key it stands for, because one is `on_ask` and the other `on-ask`,
        // and a refusal that mixed them up would name a setting nobody wrote
        let mut said = Vec::new();
        let mut filed = None;
        for (id, key) in [("max_run_seconds", "max-run-seconds"), ("on_ask", "on-ask")] {
            // typed first: a command-line value means somebody is looking at this run; a file's
            // value only says where it was written down
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
/// note: a command rather than a flag, unlike `--check`, because it takes a list and an output and
/// is a program of its own. `--check` reads; this writes a session to resume.
#[derive(clap::Subcommand)]
#[non_exhaustive]
pub enum Command {
    /// Fold several hard forks of one session into one session to carry on from, and stop.
    ///
    /// The items the forks share are kept whole. From each fork's own part only the notes the agent
    /// wrote for itself are carried; two that disagree are both kept and named. Nothing is sent to
    /// a model: `kamchatka -r` on what it wrote carries on, and `/request` shows what the first
    /// request would be.
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
/// note: `deny` is the default. `examples/recorded.rs` grants every question because a recording is
/// watched; a program is not, and an unwatched run must not do anything that has not been allowed.
/// Pass `--auto-allow exec` to allow them.
///
/// note: every public enum in this workspace carries `#[non_exhaustive]`. It was missing here, and
/// `Leave` found that out.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[non_exhaustive]
pub enum OnAsk {
    /// Refuse it. The model is told, and told that it was this call rather than a standing rule.
    Deny,
    /// Grant it.
    Allow,
    /// Answer nothing, leaving it for the next client to attach to the same session, or for this
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
