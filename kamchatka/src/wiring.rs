//! Putting one together: a kernel, a policy, the tools, the sandbox and an [`App`] around them.
//!
//! note: one place for the nine steps `main.rs` and `examples/recorded.rs` both take in the same
//! order - a kernel, a subscription, a policy, the provider and the projector it implies, a
//! compactor, the confinement, the tools, the introspection handle, and an `App` holding the first
//! and the last of those - so that anyone embedding this crate does not write them a third time,
//! out of reading `main.rs` and hoping.
//!
//! Two of the steps are not guessable. The subscription has to happen **before** anything is
//! plugged in, or the trace is missing the wiring; and
//! [`introspect::install`] hands back a handle that the caller has to keep, because the tools hold
//! a weak reference to it and stop working the moment it is dropped.
//!
//! note: what it deliberately does not do is reach the network or read the environment.
//! [`Setup::wire`] takes the provider already connected, because where the requests go, which key
//! pays for them and what dialect they speak are the caller's to decide - `endpoint::connect` is
//! one line and `tests` hand in a scripted one.
//!
//! note: and where a session goes when it is over, so that an embedder with a loop of its own has
//! it too. [`record`] writes a session out where nobody has to have asked for it, and
//! [`Setup::relaunch`] is `/restart`: the same settings wired a second time, with the first
//! session written out on the way. Without them a loop like `examples/phone.rs`, a session with a
//! socket in front of it, ends the process on `/quit` or `/restart` with the session in memory and
//! nothing on disk. A loop that ends a session owes it the same safety net the program's loops
//! give one, and gets it from here.

use std::sync::Arc;

use nachalnik::{Config, ContextItem, Event, Kernel, Snapshot};
use nachalnik_providers::Dialect;
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
///
/// note: `Clone`, because [`Setup::wire`] consumes one and a session can be asked to start again.
/// `/restart` is the same settings wired a second time - which is what makes the second session
/// the one the flags describe rather than a copy of the first session's drift.
#[derive(Clone)]
pub struct Setup {
    /// A session to carry on from, read and parsed by whoever has the file.
    ///
    /// note: a [`Snapshot`] rather than a path, because reading one is where the error messages
    /// worth writing are - which file, and whether it was a session at all - and those belong to
    /// the program that was handed the path.
    pub resume: Option<Snapshot>,
    /// What to call the session, which is also what its files are named after.
    ///
    /// note: `None` leaves the runtime's own counter, which restarts at 0 with the process and is
    /// fine as an identity and useless as a filename. A resumed session keeps the name in its
    /// snapshot, so this is left empty when resuming.
    pub session_name: Option<String>,
    /// Whether the session is written out when it is over: a log and a snapshot under the
    /// temporary directory, by [`record`], which [`Setup::relaunch`] does for the session a
    /// restart replaces. `--no-record` is this, off.
    ///
    /// note: [`Setup::wire`] does not read it. A session is written when it ends, and the loop
    /// that ends it is the caller's - so this is the setting and [`record`] is the act, and a
    /// caller driving an [`App`] with a loop of its own honours the one with the other, the way
    /// `main.rs` and `examples/phone.rs` do at the end of a run.
    pub record: bool,
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
    /// note: a list rather than a flag per family, because "which tools" is one question, and a
    /// flag per family answers it in places that can disagree. What is offered is a property of the
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
    /// note: *the tools*, not the `shell` alone. It is `Reach::confined` as well as the Landlock
    /// ruleset - and an unconfined
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
    /// A model to ask where each shell command a question is about lands on a rubric.
    ///
    /// note: `None` is the default and is a session that sends nothing about its tool calls
    /// anywhere. Handing one over turns on the rating in [`tools::Advised`], which sends the
    /// tool's id, its declared capabilities and its arguments to that endpoint and decides
    /// nothing. The module says what leaves.
    ///
    /// note: the connected client rather than a key or a flag, for the reason `wire` takes a
    /// connected provider: this function reaches no network and reads no environment, so whether
    /// to have an advisor at all - and which endpoint it is - is the caller's to decide and its
    /// business to say out loud.
    #[cfg(feature = "shell-advisor")]
    pub advisor: Option<Arc<dyn nachalnik_providers::system1::SystemOne>>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            resume: None,
            session_name: None,
            record: true,
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
            #[cfg(feature = "shell-advisor")]
            advisor: None,
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
/// second thing to forget, and what it would drift from is exactly what the refusal in
/// `Setup::check` is about - a name that is not a tool. Building them costs six schemas and
/// happens once.
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
    pub fn wire(self, provider: Arc<dyn Dialect>) -> Result<Wired, String> {
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
            // from an MCP server: those hold no handle to that table, so without this a session
            // started with `--mcp` has no ceiling at all on what somebody else's server can put
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
        // note: only where the provider names a model, because a session started without `-m` has
        // an address and a key and nothing to ask - and the runtime already has a state for that.
        // With no provider, `model_info()` answers `None`, every screen that draws a model draws
        // the gap instead, and a turn is `Error::NoProvider` rather than a request naming nothing.
        // `/model` hands it over, which is the moment there is something to hand over
        if !provider.model().is_empty() {
            kernel.set_provider(provider.clone());
        }

        // note: the kernel gets the advisor wrapped around the standing rules where there is one,
        // and the rules themselves where there is not. Everything else keeps holding `Careful`
        // directly - the `shell` tool asks it what a command may reach, the permissions tab draws
        // its stances, and `/allow` changes them - because those are all about the standing rules,
        // which are the whole of what decides. What the wrapper adds is a rating of a command
        // somebody is about to be asked about
        //
        // note: built once and kept as itself as well as handed over, because the screen reads
        // the rating off it and `Arc<dyn PermissionPolicy>` cannot be asked for
        // one. Two of them would be two memories of what the advisor said, one of them always
        // empty - and the empty one is the one the panel would be holding
        #[cfg(feature = "shell-advisor")]
        let advisor = self
            .advisor
            .map(|jev| Arc::new(tools::Advised::new(policy.clone(), jev)));
        #[cfg(feature = "shell-advisor")]
        let decides: Arc<dyn nachalnik::PermissionPolicy> = match &advisor {
            Some(advised) => advised.clone(),
            None => policy.clone(),
        };
        #[cfg(not(feature = "shell-advisor"))]
        let decides: Arc<dyn nachalnik::PermissionPolicy> = policy.clone();
        // one call rather than one per branch: setting the policy is an `Event`, and a session
        // whose trace said it twice would be a session that had two
        kernel.set_policy(decides);
        // the projector decides the shape of a turn on the wire, so the provider that owns that
        // wire is the thing asked what it can carry - rather than a caller deciding a second time
        // from the same flag, which is how the two come apart
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
        #[cfg(feature = "shell-advisor")]
        {
            app.advisor = advisor;
        }
        app.confinement = confinement;
        app.introspect = introspect;
        app.set_spend(self.spend);

        // note: everything is built and then what was not asked for is turned off, rather than
        // only the named ones being built. That is what makes the list a starting position: the
        // rest are on the shelf, `/tools toggle` reaches them, and a `shell` offered later is
        // the one the confiner above was decided for.
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

    /// Writes a session out and wires the one that takes its place, out of these settings.
    ///
    /// note: what `/restart` is. The session handed in is ended here rather than left to whoever
    /// writes it, because a record whose last line is not `session.finished` reads as a run that
    /// was killed, and this one was asked to stop. Its record is the same one the end of a run
    /// writes, and deliberately: a session somebody restarted is a session that ended, and a run
    /// abandoned halfway is the case that safety net is most for.
    ///
    /// note: and only where the loop that handed it over has not ended it already. A served loop
    /// and a headless one each owe their clients the last record on the stream they are writing,
    /// so each emits it itself, and [`Kernel::finish`] emits every time it is called - so a second
    /// one here would put `session.finished` in the log twice, under a line that says nothing more
    /// will be recorded. The question is asked of the log because the log is what
    /// this is about: the last line of the record it is going to write.
    ///
    /// note: it hands back the sentence rather than saying it, because the thing to say it to
    /// does not exist until this returns. The old [`App`] is about to be dropped and is the only
    /// place the name and the paths are written down; the caller says it into the new one.
    ///
    /// note: `resume` and `session_name` are the two settings not taken at their word. A snapshot
    /// is where the *run* started and a restart is somebody leaving it; and a name carried over
    /// would give two sessions of one run the same identity, while `None` would fall back to the
    /// runtime's counter, which is fine as an identity and useless as a filename. So a fresh
    /// session is stamped fresh, the way the first one was, and [`record`] settles two of them
    /// landing in the same second.
    pub fn relaunch(
        &self,
        app: &App,
        provider: Arc<dyn Dialect>,
    ) -> Result<(Wired, String), String> {
        if !ended(&app.kernel) {
            app.kernel.finish();
        }

        let name = app.kernel.session_name();
        let said = match self.record {
            false => format!("{name} ended; `--no-record`, so nothing was written"),
            true => match record(app) {
                Ok(written) => format!(
                    "{name} ended: {written} (`kamchatka -r {}` carries on from it)",
                    written.state
                ),
                // the restart still happens: a session nobody could write down is a worse reason
                // to refuse somebody a fresh one than it is to carry on with the old
                Err(e) => format!("{name} ended, and could not be written down: {e}"),
            },
        };

        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default();
        let fresh = Setup {
            resume: None,
            session_name: Some(App::session_stamp(started)),
            ..self.clone()
        };
        // the old session's line rides on the error, since it is the one pointer to where that
        // session went and the caller is about to report this instead of saying it
        let wired = fresh
            .wire(provider)
            .map_err(|e| format!("{said}\nand no fresh session could be started: {e}"))?;

        Ok((wired, said))
    }
}

/// Whether the session has already said it is over.
///
/// note: the log rather than a flag on [`App`], because the loops that end a session end it on the
/// kernel and a flag would be a second place to keep the same fact. Reading the last record is
/// also the only answer that stays right for a loop nobody here has written: an embedder that ends
/// its own session gets one `session.finished`, and one that leaves it to [`Setup::relaunch`] gets
/// one too.
fn ended(kernel: &Kernel) -> bool {
    kernel.with_history(|log| {
        log.records()
            .last()
            .is_some_and(|record| record.event == Event::SessionFinished)
    })
}

/// Where a session went when it was written out: how many records, and the two files.
#[derive(Debug)]
#[non_exhaustive]
pub struct Recorded {
    /// How many records the log holds.
    pub records: usize,
    /// The event log, one record per line.
    pub log: String,
    /// The snapshot `kamchatka -r` starts from.
    pub state: String,
}

impl std::fmt::Display for Recorded {
    // the line every run ends with, and the one `/save` says
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} records in {}, and a session in {}",
            self.records, self.log, self.state
        )
    }
}

/// Writes the session where nobody has to have asked for it, and says where that was.
///
/// note: a temporary directory, because this is a safety net rather than an archive - `/save`
/// remains the way to put a session somewhere it will still be next week. [`Setup::record`] is
/// the setting that turns it off, and the caller is what reads it: this writes.
///
/// note: and the directory is the user's own, `0700`. What goes in it is a whole conversation and
/// every byte of output every tool produced, written without anybody asking for it; under the
/// default umask that is a world-readable file in a directory everyone on the machine can list.
/// Nobody would type `/save /tmp/everyone/notes.jsonl`, and this should not do it for them.
pub fn record(app: &App) -> Result<Recorded, String> {
    let mut dir = std::env::temp_dir();
    dir.push("kamchatka");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        // it may already exist from an earlier run, made before this did it; either way, this is
        // the run that is about to write a transcript into it
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));

        // note: and then looked at, because the temporary directory is everybody's and this name
        // is fixed. One somebody else made there first is one the line above cannot make private
        // - it fails, and is ignored - so without this the transcript would go into a directory
        // they can read, and a link they left at a predictable name would be followed. A real
        // directory that nobody but its owner can enter is one this user owns, or one it cannot
        // write in at all
        let private = std::fs::symlink_metadata(&dir)
            .is_ok_and(|meta| meta.is_dir() && meta.permissions().mode() & 0o077 == 0);
        if !private {
            return Err(format!(
                "{} is not a directory only you can enter, so the session was not recorded \
                 there; remove it, or use `--no-record` and `/save`",
                dir.display()
            ));
        }
    }

    let (log, state) = unclaimed(&dir.join(app.kernel.session_name()))?;
    let records = app.write_session(&log, &state)?;

    Ok(Recorded {
        records,
        log,
        state,
    })
}

/// A `.jsonl` and `.json` pair under `stem` that no other session has written.
///
/// note: the name is a session's own, and a session's own name is not unique enough to be a
/// filename. Two of them collide in two ways, both silently. Two runs started inside one second
/// share a stamp, so the second to finish would write over the first. And **a resumed session
/// keeps the name of the session it resumed**, which is right for what a name is for and means
/// `-r` would write over the very file it had just read: the log of what happened replaced by that
/// of a sitting that did nothing. The snapshot would come through nearly intact, because a resumed
/// context renders to nearly the same bytes; the log would not, and the log is the half that says
/// what happened rather than where things ended up.
///
/// note: so the name stays what it is and the *file* moves - `…Z-2.jsonl` beside `…Z.jsonl`,
/// which sorts next to its sibling and reads as the second sitting of one session. Renaming the
/// session instead would put a process identifier in every filename to fix something rare, and
/// the name is what `#fork` derives from and what the trace shows.
///
/// note: `create_new` rather than asking whether the file is there, because between asking and
/// writing is exactly where the first of those two collisions lives. The `.json` is checked
/// before the `.jsonl` is claimed, so a suffix this passes over leaves nothing of its own behind.
///
/// note: a name that is taken is passed over and nothing else is: a directory that cannot be
/// written in, or a full disk, is the reason the record is not there, and saying "no unused name"
/// a thousand tries later would be the wrong one.
fn unclaimed(stem: &std::path::Path) -> Result<(String, String), String> {
    // bounded, so that a directory full of these is an error rather than a loop
    for nth in 1..1_000 {
        let stem = match nth {
            1 => stem.display().to_string(),
            nth => format!("{}-{nth}", stem.display()),
        };
        let (log, state) = (format!("{stem}.jsonl"), format!("{stem}.json"));
        // anything there at all, a link to nothing included, which `exists` answers no to and
        // a write would follow
        if std::fs::symlink_metadata(&state).is_ok() {
            continue;
        }

        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log)
        {
            Ok(_) => return Ok((log, state)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not write {log}: {e}")),
        }
    }

    Err(format!(
        "could not find an unused name for the record beside {}",
        stem.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session writes beside a record rather than over it, however it came by the same name.
    ///
    /// note: the second half is the case that matters, and it is not the exotic one: `-r` is the
    /// line this program prints at the end of every run, and a resumed session keeps the name of
    /// the session it resumed, so every resume would otherwise write over the log it had just read.
    #[test]
    fn a_record_never_writes_over_one_that_is_already_there() {
        // note: not `tests/common`'s `scratch`, which builds under `CARGO_TARGET_TMPDIR` -
        // cargo hands that to integration tests and not to a unit test. One fixed name, emptied
        // on the way in, so nothing accumulates either
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

    /// A link at the snapshot's name is a name that is taken, even when it points at nothing.
    ///
    /// note: `exists` follows a link and answers no for one to a file that is not there, and the
    /// write after it follows the link too - so a link left at a predictable name would be a file
    /// created wherever it pointed.
    #[cfg(unix)]
    #[test]
    fn a_link_at_the_snapshots_name_is_not_written_through() {
        let dir = std::env::temp_dir().join("kamchatka-unclaimed-link");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory to work in");
        let stem = dir.join("2026-09-22T12-00-00Z");
        std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("2026-09-22T12-00-00Z.json"))
            .expect("a link");

        let (log, state) = unclaimed(&stem).expect("a name beside it");
        assert!(log.ends_with("2026-09-22T12-00-00Z-2.jsonl"), "{log}");
        assert!(state.ends_with("2026-09-22T12-00-00Z-2.json"), "{state}");
    }
}
