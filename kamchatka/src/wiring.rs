//! Putting one together: a kernel, a policy, the tools, the sandbox and an [`App`] around them.
//!
//! note: this is here because it was written twice. `main.rs` and `examples/recorded.rs` each did
//! the same nine steps in the same order - a kernel, a subscription, a policy, the provider and
//! the projector it implies, a compactor, the confinement, the tools, the introspection handle,
//! and an `App` holding the first and the last of those - and anyone embedding this crate would
//! have written them a third time, out of reading `main.rs` and hoping.
//!
//! Two of the steps are not guessable, which is most of the reason this exists. The subscription
//! has to happen **before** anything is plugged in, or the trace is missing the wiring; and
//! [`introspect::install`] hands back a handle that the caller has to keep, because the tools hold
//! a weak reference to it and stop working the moment it is dropped.
//!
//! note: what it deliberately does not do is reach the network or read the environment.
//! [`Setup::wire`] takes the provider already connected, because where the requests go, which key
//! pays for them and what dialect they speak are the caller's to decide - `provider::connect` is
//! one line and `tests` hand in a scripted one.

use std::sync::Arc;

use nachalnik::{Config, ContextItem, Event, Kernel, Snapshot};
use nachalnik_providers::Endpoint;
use tokio::sync::{broadcast, mpsc};

use crate::{
    app::{App, Outcome},
    attach, introspect, sandbox,
    tools::{self, Careful, Limits, Subject},
};

/// Everything a session is set up with, and what each of them means.
///
/// note: a settings struct with a `Default` rather than a builder, the way
/// [`nachalnik::Config`] is. `Setup { confine: false, ..Default::default() }` is the shape, and
/// the defaults below are the ones the program uses - so a caller who wants what `kamchatka` does
/// writes almost nothing, and a caller who wants something else changes the field that says so.
pub struct Setup {
    /// A session to carry on from, read and parsed by whoever has the file.
    ///
    /// note: a [`Snapshot`] rather than a path, because reading one is where the error messages
    /// worth writing are - which file, and whether it was a session at all - and those belong to
    /// the program that was handed the path.
    pub resume: Option<Snapshot>,
    /// What to call the session, which is also what its files are named after.
    ///
    /// note: `None` leaves the runtime's own counter, which restarts at 1 with the process and is
    /// fine as an identity and useless as a filename. A resumed session keeps the name in its
    /// snapshot, so this is left empty when resuming.
    pub session_name: Option<String>,
    /// How many requests one turn may make before it stops; `None` is no limit.
    pub requests: Option<usize>,
    /// Whether a model's tool calls may run at the same time rather than in the order it asked.
    pub parallel: bool,
    /// Whether the whole of a shortened tool output is kept beside the copy the model was shown.
    pub keep_truncated: bool,
    /// Whether a request the counter puts over the model's context is refused rather than sent.
    ///
    /// note: reachable because the figure it acts on is an estimate and the limit it is checked
    /// against is whatever the endpoint chose to advertise, and either can be wrong. An
    /// aggregator that quotes one model's window while routing to a provider with another is not
    /// hypothetical - it is what `liquid/lfm-2.5-2.6b` does - and a session held to a limit
    /// smaller than the real one refuses requests that would have been answered. Turning this off
    /// costs a round trip and gets the endpoint's own count of the request, which is worth more
    /// than any guess made here.
    pub refuse_oversized: bool,
    /// How full the context may get before the oldest tool results are elided; `None` never
    /// compacts.
    pub compact: Option<f64>,
    /// How many tokens the provider may charge for the whole session before it stops; `None`
    /// never stops.
    ///
    /// note: the sibling of `requests` above, over a different unit and a different span. That one
    /// is the kernel's and bounds a turn; this one is the session's, and it is the guard for the
    /// failure nothing else here catches - a model that has found a loop and a caller that is not
    /// watching. See [`App::set_spend`](crate::app::App::set_spend).
    pub spend: Option<u64>,
    /// Which of this program's own tools to offer, by id; `None` is all of them.
    ///
    /// note: a list rather than a flag per family, because "which tools" is one question and it
    /// was being answered in two places that could disagree. What is offered is a property of the
    /// session, so it is one field, and the two answers people actually want - all of them, or
    /// these - are the two shapes an `Option<Vec<_>>` has.
    ///
    /// note: naming a subset still *builds* the others and shelves them, so
    /// [`App::toggle`](crate::app::App::toggle) can offer one mid-session. That is what makes this
    /// a starting position rather than a decision: a session that started without `shell` can be
    /// given one, and it is confined exactly as it would have been.
    ///
    /// note: an empty list is the exception, and builds none of them at all - which is what a
    /// caller whose tools all come from MCP servers, or who brings its own, is asking for. There
    /// is nothing to offer later either, which is what asking for none means. It also skips the
    /// question of what the sandbox will take, and that question costs a child process.
    ///
    /// note: this program's own tools, and not the ones an MCP server brings. Those arrive after
    /// the wiring, under names nothing here can know in advance, and they are offered as they
    /// arrive; `/tools toggle` turns one off like any other.
    pub tools: Option<Vec<String>>,
    /// Whether to confine what the tools can reach.
    ///
    /// note: *the tools*, not the `shell` alone, which is what this said and is the half that
    /// matters. It is `Reach::confined` as well as the Landlock ruleset - and an unconfined
    /// `Reach` hands back every path untouched, so `read` of `~/.ssh/id_rsa` is a file rather
    /// than a refusal. Somebody turning this off for one command should know they turned it off
    /// for everything `fs` does as well.
    pub confine: bool,
    /// Paths outside the working directory the tools may also read and write.
    pub reachable: Vec<std::path::PathBuf>,
    /// Paths outside the working directory the tools may read but not change.
    pub readable: Vec<std::path::PathBuf>,
    /// Capabilities and path rules to allow before anything runs.
    ///
    /// note: answered in advance is the only way to decide in advance - [`Careful`] asks about
    /// whatever nobody has mentioned, and a session with nobody at it cannot be asked.
    pub allow: Vec<Subject>,
    /// The same, refused. The strictest of everything consulted wins, so this beats `allow`.
    pub deny: Vec<Subject>,
    /// A system instruction, pinned. The runtime ships none of its own.
    pub system: Option<String>,
    /// Files to put in the context, pinned; a PDF or an image goes in as itself.
    pub files: Vec<String>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            resume: None,
            session_name: None,
            requests: Some(8),
            parallel: false,
            keep_truncated: true,
            refuse_oversized: true,
            compact: Some(0.8),
            spend: None,
            tools: None,
            confine: true,
            reachable: Vec::new(),
            readable: Vec::new(),
            allow: Vec::new(),
            deny: Vec::new(),
            system: None,
            files: Vec::new(),
        }
    }
}

/// A session, wired up, and the two things that report on it.
pub struct Wired {
    /// The session.
    pub app: App,
    /// Every event the kernel emits, subscribed before any of the wiring happened.
    pub events: broadcast::Receiver<Event>,
    /// Where a finished turn reports itself; the other half is inside the [`App`].
    pub finished: mpsc::UnboundedReceiver<Outcome>,
}

/// The identifiers of the tools a session starts with.
///
/// note: read off tools built and dropped rather than from a list written out here. A list is a
/// second thing to forget, and what it would drift from is exactly what the refusal above is
/// about - a name that is not a tool. Building them costs six schemas and happens once.
fn offered_ids() -> Vec<String> {
    let kernel = Kernel::new(Config::default());
    let policy = Arc::new(Careful::new());
    for tool in tools::builtin(
        tools::Shell {
            policy: policy.clone(),
            workdir: std::path::PathBuf::new(),
            extra: Vec::new(),
            readable: Vec::new(),
            confiner: None,
            limits: Limits::default(),
        },
        crate::sandbox::Reach {
            workdir: std::path::PathBuf::new(),
            extra: Vec::new(),
            readable: Vec::new(),
            confined: true,
        },
        Limits::default(),
    ) {
        kernel.add_tool(tool);
    }
    let _anchor = introspect::install(&kernel, policy, Limits::default());

    kernel.tool_specs().into_iter().map(|it| it.id).collect()
}

impl Setup {
    /// What can be said about a setup before anything is reached: a path rule nothing can match,
    /// a tool nobody offers.
    ///
    /// note: separate from [`Setup::wire`], and called by it, so that a program can find out its
    /// arguments are wrong before it builds a provider. `main` connects to an endpoint and asks it
    /// what the model holds, which is a round trip and an API key - neither of them anybody's idea
    /// of how to be told that a settings file names `contxt`.
    pub fn check(&self) -> Result<(), String> {
        for subject in self.allow.iter().chain(self.deny.iter()) {
            // a path rule that cannot match stops the session rather than being drawn on the
            // permissions tab like any other: a `--deny` that refuses nothing is worse than no
            // rule, because it reads as given
            if let tools::Subject::Path(pattern) = subject
                && let Some(objection) = tools::objection_to(pattern)
            {
                return Err(objection);
            }
        }

        // note: a name that is not a tool stops the session rather than being skipped, for the
        // reason `deny_unknown_fields` is on the settings struct. Asking for `contxt` and getting
        // a session with no context tool and nothing said about it is the failure this setting is
        // most likely to have
        if let Some(wanted) = &self.tools {
            let offered = offered_ids();
            if let Some(unknown) = wanted.iter().find(|it| !offered.contains(it)) {
                return Err(format!(
                    "`{unknown}` is not one of this program's tools; they are {}",
                    offered.join(", ")
                ));
            }
        }

        Ok(())
    }

    /// Wires one up around a provider that is already connected.
    pub fn wire(self, provider: Arc<dyn Endpoint>) -> Result<Wired, String> {
        // before anything is built, so that an embedder gets the same refusal `main` gets before
        // it reaches an endpoint at all
        self.check()?;

        let config = Config {
            session_name: self.session_name,
            max_requests_per_turn: self.requests,
            parallel_tool_calls: self.parallel,
            keep_truncated_output: self.keep_truncated,
            refuse_oversized_requests: self.refuse_oversized,
            // note: the floor under everything without a row in `Limits`, which is every tool
            // from an MCP server: those hold no handle to that table, so before this a session
            // started with `--mcp` had no ceiling at all on what somebody else's server could put
            // in its context. The runtime's own default is `None`, which is right for a runtime
            // and wrong for a program that offers to run other people's tools.
            //
            // note: it does not give those tools a `/limit` row. A row that listed a number
            // nothing consults would be worse than no row, and what would earn one is the tools
            // consulting this table rather than the table naming them.
            default_tool_output_limit: Some(tools::CEILING),
            ..Default::default()
        };
        let kernel = match self.resume {
            Some(snapshot) => Kernel::resume(config, snapshot),
            None => Kernel::new(config),
        };

        // note: subscribed before anything is plugged in, so that the wiring is on the trace like
        // everything else. Setting the provider, the policy, the compactor and each tool are all
        // events, and a client that started listening afterwards gets a session whose first few
        // facts are only in the log
        let events = kernel.subscribe();

        let policy = Arc::new(Careful::new());
        for (subjects, verdict) in [
            (self.allow, nachalnik::Verdict::Allow),
            (self.deny, nachalnik::Verdict::Deny),
        ] {
            for subject in subjects {
                policy.set(&subject, verdict);
            }
        }
        kernel.set_provider(provider.clone());
        kernel.set_policy(policy.clone());
        // the projector decides the shape of a turn on the wire, so the provider that owns that
        // wire is the thing asked what it can carry - rather than a caller deciding a second time
        // from the same flag, which is how the two came apart in the first place
        kernel.set_projector(Arc::new(provider.projection()));
        if let Some(threshold) = self.compact.filter(|it| *it < 1.0) {
            kernel.set_compactor(Some(Arc::new(tools::Trim::under(threshold))));
        }

        // one table, shared by the tools that declare a limit and the `/limit` that changes them
        let limits = Limits::new();
        // note: asked only when there is a `shell` to confine, because asking is not free: finding
        // out what Landlock will take means applying a ruleset in a child process. It is settled
        // once here rather than per command either way - see the note on `Shell::confiner` - and a
        // caller that turns the built-in tools off and brings its own shell is the one building
        // the confiner, so it is the one that asks
        //
        // note: asked whether or not `shell` is among the tools *offered*, because a shelved one
        // can be offered later and it has to be the same shell. Deciding this from the starting
        // list would make `/tools toggle shell` in a session that started without one an unconfined
        // shell, with nothing on the screen saying so
        let mut confinement = sandbox::Confinement::Unsupported;
        let building = self.tools.as_ref().is_none_or(|it| !it.is_empty());
        if building {
            let program =
                std::env::current_exe().map_err(|e| format!("could not find myself: {e}"))?;
            if self.confine {
                confinement = sandbox::available(&program);
            }
            let reach = sandbox::Reach {
                workdir: std::env::current_dir()
                    .map_err(|e| format!("could not find the working directory: {e}"))?,
                extra: self.reachable,
                readable: self.readable,
                confined: self.confine,
            };
            for tool in tools::builtin(
                tools::Shell {
                    policy: policy.clone(),
                    workdir: reach.workdir.clone(),
                    extra: reach.extra.clone(),
                    readable: reach.readable.clone(),
                    // only when it would actually confine anything. A binary that has been
                    // replaced since this one started, or a kernel with no Landlock, is a `shell`
                    // that runs unconfined and a permissions view that says so - rather than one
                    // whose every command comes back with an error nobody can account for
                    confiner: confinement.is_confined().then(|| program.clone()),
                    limits: limits.clone(),
                },
                reach,
                limits.clone(),
            ) {
                kernel.add_tool(tool);
            }
        }

        // the handle the tools reach the kernel through, which `App` then holds for the rest of
        // the session; see `introspect::install` for why it is a weak handle to something out here
        // rather than a kernel the tools hold
        let introspect =
            building.then(|| introspect::install(&kernel, policy.clone(), limits.clone()));

        if let Some(system) = self.system {
            kernel.push(ContextItem::system(system).pinned());
        }
        for path in &self.files {
            kernel.push(
                attach::attached(path)
                    .map_err(|e| format!("{path}: {e}"))?
                    .because("named on the command line")
                    .pinned(),
            );
        }

        let (outcomes, finished) = mpsc::unbounded_channel();
        let mut app = App::new(kernel, policy, provider, limits, outcomes);
        app.confinement = confinement;
        app.introspect = introspect;
        app.set_spend(self.spend);

        // note: everything is built and then what was not asked for is turned off, rather than
        // only the named ones being built. That is what makes the list a starting position: the
        // rest are on the shelf, `/tools toggle` reaches them, and a `shell` offered later is
        // the one the confiner above was decided for.
        //
        // note: a name that is not a tool stops the session rather than being skipped, for the
        // reason `deny_unknown_fields` is on the settings struct. Asking for `contxt` and getting
        // a session with no context tool and nothing said about it is the failure this setting is
        // most likely to have
        if let Some(wanted) = self.tools {
            // the names were answered for by `Setup::check` before anything was built; what is
            // left is the position itself - everything is here, and what was not asked for goes
            // on the shelf
            let offered: Vec<String> = app
                .kernel
                .tool_specs()
                .into_iter()
                .map(|it| it.id)
                .collect();
            for id in offered.iter().filter(|it| !wanted.contains(it)) {
                app.toggle(id);
            }
        }

        Ok(Wired {
            app,
            events,
            finished,
        })
    }
}
