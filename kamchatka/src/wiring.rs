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
/// [`nachalnik::Config`] is. `Setup { introspect: true, ..Default::default() }` is the shape, and
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
    /// How full the context may get before the oldest tool results are elided; `None` never
    /// compacts.
    pub compact: Option<f64>,
    /// Whether to offer `read`, `write`, `edit` and `shell`.
    ///
    /// note: a caller whose tools all come from MCP servers, or who brings its own, turns this
    /// off and adds them to `wired.app.kernel` afterwards. The registry is live, so there is no
    /// moment at which it is too late.
    pub builtin_tools: bool,
    /// Whether to confine the `shell` tool. Off means it reaches whatever the user running it can.
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
    /// Whether to offer the two tools an agent reads and manages its own context with.
    pub introspect: bool,
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
            compact: Some(0.8),
            builtin_tools: true,
            confine: true,
            reachable: Vec::new(),
            readable: Vec::new(),
            allow: Vec::new(),
            deny: Vec::new(),
            introspect: false,
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

impl Setup {
    /// Wires one up around a provider that is already connected.
    pub fn wire(self, provider: Arc<dyn Endpoint>) -> Result<Wired, String> {
        let config = Config {
            session_name: self.session_name,
            max_requests_per_turn: self.requests,
            parallel_tool_calls: self.parallel,
            keep_truncated_output: self.keep_truncated,
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
            kernel.set_compactor(Some(Arc::new(tools::Trim {
                threshold,
                target: (threshold - 0.2).max(0.1),
            })));
        }

        // one table, shared by the tools that declare a limit and the `/limit` that changes them
        let limits = Limits::new();
        // note: asked only when there is a `shell` to confine, because asking is not free: finding
        // out what Landlock will take means applying a ruleset in a child process. It is settled
        // once here rather than per command either way - see the note on `Shell::confiner` - and a
        // caller that turns the built-in tools off and brings its own shell is the one building
        // the confiner, so it is the one that asks
        let mut confinement = sandbox::Confinement::Unsupported;
        if self.builtin_tools {
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

        // the handle the two tools reach the kernel through, which `App` then holds so that
        // `/introspect` can turn them off again; see `introspect::install` for why it is a weak
        // handle to something out here rather than a kernel the tools hold
        let introspect = self
            .introspect
            .then(|| introspect::install(&kernel, limits.clone()));

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

        Ok(Wired {
            app,
            events,
            finished,
        })
    }
}
