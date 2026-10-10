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
//! it too. [`Recorder`] writes a session down as it goes, [`record`] finishes that record - or
//! writes the whole session out where nobody started one - and [`Setup::relaunch`] is `/restart`:
//! the same settings wired a second time, with the first session written out on the way. Without
//! them a custom loop, such as a session with a socket in front of it, ends the process
//! on `/quit` or `/restart` with the session in memory and nothing on disk. A loop that ends a
//! session owes it the same safety net the program's loops give one, and gets it from here.

use std::sync::Arc;

use nachalnik::{Capability, Config, ContextItem, Event, Kernel, Snapshot};
use nachalnik_providers::Dialect;
use tokio::sync::{broadcast, mpsc};

use crate::{
    app::{App, Outcome},
    attach, introspect, sandbox,
    tools::{self, Careful, Limits, Subject},
};

mod record;

pub(crate) use record::unclaimed;
pub use record::{Recorded, Recorder, record};

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
    /// Whether the session is written down: a log and a snapshot under the temporary directory,
    /// kept as it goes by a [`Recorder`] and finished by [`record`], which [`Setup::relaunch`]
    /// does for the session a restart replaces. `--no-record` is this, off.
    ///
    /// note: [`Setup::wire`] does not read it. Starting a record and finishing one are the acts
    /// of the loop driving the session, which is the caller's - so this is the setting and
    /// [`Recorder::start`] and [`record`] are the acts, and a caller driving an [`App`] with a loop
    /// of its own honours the one with the others, the way `main.rs` does at both ends of a run.
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
    /// How full the context may get before its oldest exchanges are excluded; `None` never
    /// compacts.
    pub compact: Option<f64>,
    /// How far down a full context is taken once they are; `None` is what `Shedder::under`
    /// derives from `compact`.
    pub compact_target: Option<f64>,
    /// How many tokens the provider may charge for the whole session before it stops; `None`
    /// never stops.
    ///
    /// note: the sibling of `requests` above, over a different unit and a different span. That one
    /// is the kernel's and bounds a turn; this one is the session's, and it is the guard for the
    /// failure nothing else here catches - a model that has found a loop and a caller that is not
    /// watching. See [`App::set_spend`](crate::app::App::set_spend).
    pub spend: Option<u64>,
    /// Whether what a command left running is left running when the session ends, rather than
    /// stopped; see [`stragglers_at_end`].
    pub leave_running: bool,
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
    /// The devices under `/dev` a confined command may read and write, and no others;
    /// [`sandbox::DEVICES`] unless somebody says otherwise.
    pub devices: Vec<std::path::PathBuf>,
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
            compact_target: None,
            spend: None,
            leave_running: false,
            tools: None,
            confine: true,
            reachable: Vec::new(),
            readable: Vec::new(),
            devices: sandbox::DEVICES
                .iter()
                .map(std::path::PathBuf::from)
                .collect(),
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

/// What the model is told as the context becomes full: that the compactor has nothing more it may
/// take, and what it can do about the rest.
///
/// note: `Shedder` never takes the turn in progress, what is pinned, or a note the model wrote for
/// itself, so once the older exchanges are gone that is what is left, and only the model or the
/// person can say what of it may go.
/// The kernel places this once per fill, between a turn's results and its next request, so a
/// tool loop that fills the context hears it inside the same turn - see
/// `nachalnik::Kernel::set_full_notice`.
///
/// note: the `context` tool is named only where the model could use it to make room now - offered,
/// and with `exclude` or `elide` not refused, by a rule or by a question nobody is there to answer -
/// and `look` only where that is too. A notice sending the model to a tool it does not have was
/// followed to one that does not exist; the other one asks it to tell the person, which is the one
/// thing left that it can do. Worked out again whenever what the model could use changes - see
/// `App::refresh_full_notice`.
pub(crate) fn full_notice(kernel: &Kernel, policy: &Careful) -> ContextItem {
    let can = |op: &str| crate::introspect::reachable(kernel, policy, &format!("context:{op}"));
    // note: both say what is left - this turn, what is pinned and the notes - because a model
    // told only that the context was full went looking for tool results to elide, and one told
    // to ask the person wrote a file larger than the room there was. Both say what not to do,
    // since a request over the limit is not sent at all
    let said = match (can("exclude") || can("elide"), can("look")) {
        (true, looks) => format!(
            "The context is full, and nothing more will be taken from it automatically: what \
             fills it now is this turn, what is pinned and the notes you kept. Before anything \
             else, {} what you no longer need, and say why. Add nothing large until there is \
             room - a request over the limit is not sent.",
            match looks {
                true =>
                    "`look` with the `context` tool to see what there is, then `exclude` or \
                         `elide` by id",
                false => "`exclude` or `elide` with the `context` tool, by id,",
            }
        ),
        (false, _) => "The context is full, and what fills it now is this turn and what is \
                       pinned. Add nothing large to it - no long answers, no large files written \
                       or read - because a request over the limit is not sent. Tell the person \
                       you are working with that the context is full, so they can exclude what is \
                       no longer needed, and keep to short answers until there is room."
            .to_owned(),
    };

    ContextItem::new(
        nachalnik::ContextKind::Reference,
        "kamchatka",
        "context full",
        said,
    )
}

/// Everything this program's own tools declare, for the names `Setup::check` holds `tools` to and
/// the rules checked against what the tools do.
///
/// note: read off tools built and dropped rather than from a list written out here. A list is a
/// second thing to forget, and what it would drift from is exactly what the refusal in
/// `Setup::check` is about - a name that is not a tool. Building them costs six schemas, at
/// startup, once for `tools` and once for each rule checked.
fn offered() -> Vec<nachalnik::ToolSpec> {
    let kernel = Kernel::new(Config::default());
    let policy = Arc::new(Careful::new());
    for tool in tools::builtin(
        tools::Shell {
            policy: policy.clone(),
            workdir: std::path::PathBuf::new(),
            extra: Vec::new(),
            readable: Vec::new(),
            devices: crate::sandbox::DEVICES.iter().map(Into::into).collect(),
            confiner: None,
            limits: Limits::default(),
            stragglers: tools::Stragglers::default(),
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

    kernel.tool_specs()
}

/// What is wrong with a rule about a capability or a domain no tool here has, where something is.
///
/// note: the path rules' objection, for the other two kinds. `--deny shell` parsed as a domain
/// called `shell`, and the shell is judged as `exec:run` - so it refused nothing, and a headless
/// run given `--on-ask allow` ran every command unasked under a rule that read as given. So did a
/// typo like `fs:writ`. The domains a call here can be judged under are the ones this program's
/// tools declare, `net:reach`, which `shell` asks about per command, and `mcp:call`, which every
/// tool from a server declares; anything else can match nothing.
///
/// note: and the one refusal of that answer is about a file. A word holding a dot or a path is
/// read as a domain by [`Subject::parse`], so `b.txt` and `main.rs` got here, and a sentence about
/// domains tells somebody who wanted a path rule nothing. The rule is left as it is - taking a
/// plain name as a path rule would answer a different question with a different subject, and see
/// [`named`] - and what the refusal does is say how to write the rule instead.
fn unreached(subject: &Subject) -> Option<String> {
    let (domain, op) = match subject {
        Subject::Capability(capability) => (capability.domain.to_string(), Some(&capability.op)),
        Subject::Domain(domain) => (domain.to_string(), None),
        Subject::Path(_) | Subject::Server(_) => return None,
    };
    let specs = offered();
    let mut declared: Vec<Capability> = specs
        .iter()
        .flat_map(|spec| spec.capabilities.iter().cloned())
        .collect();
    declared.push(Capability::net("reach"));
    declared.extend(Capability::parse("mcp:call").ok());

    let ops: Vec<&str> = declared
        .iter()
        .filter(|it| it.domain.to_string() == domain)
        .map(|it| it.op.as_str())
        .collect();
    if ops.is_empty() {
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        return Some(match named(&domain, &ids) {
            // a file somebody meant to rule about, and the grammar `objection_to` holds the other
            // kind to - a rule about a path is a file name in which `*` stands for any run of
            // characters, so `b.txt*` is one and `b.txt` is not what a path rule is written as
            Some(_) => format!(
                "`{subject}` is read as a whole domain, and no domain here is called `{domain}`. A \
                 path rule is a file name in which `*` stands for any run of characters - \
                 `{domain}*` - or one directory name with a slash after it - `{domain}/`"
            ),
            // a tool, and what a call is judged by instead
            None => match specs.iter().find(|spec| spec.id == domain) {
                Some(tool) => format!(
                    "`{subject}` names a tool, and a rule names what a call needs: `{domain}` is \
                     judged as {}",
                    tool.capabilities
                        .iter()
                        .map(|it| format!("`{it}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => {
                    let mut domains: Vec<String> =
                        declared.iter().map(|it| it.domain.to_string()).collect();
                    domains.sort();
                    domains.dedup();
                    format!(
                        "`{subject}` is about `{domain}`, which no call here is judged under: the \
                         domains are {}",
                        domains.join(", ")
                    )
                }
            },
        });
    }
    match op {
        Some(op) if !ops.contains(&op.as_str()) => {
            let mut ops = ops;
            ops.sort();
            ops.dedup();
            Some(format!(
                "`{subject}` is not something a call here does: `{domain}` is {}",
                ops.join(", ")
            ))
        }
        _ => None,
    }
}

/// Whether `domain` is the name of a file rather than of anything a rule here is written about.
///
/// note: the characters a file name may hold and a domain may not, read by that alone. A domain
/// and a tool's id are both bare words and nothing in either says which it is - which is why a
/// server is named on an argument of its own - so this is the one place a rule about a file can
/// be recognised at all, and the refusal above is where it says so.
///
/// note: a path is read as a name too, since a path rule is a *file name* compared with the last
/// name in a path: `main.rs*` is a rule about that file, and `src/main.rs` is not what a path
/// rule is written as. `b.txt` is accepted here and refused, rather than taken: what a plain name
/// means is a question about what a subject is, and the two ways of answering it - read every
/// plain name as a file, or refuse one no domain claims - leave `shell` and `files` reading as
/// files as well as domains nobody declared.
fn named(domain: &str, ids: &[&str]) -> Option<String> {
    if ids.contains(&domain) {
        return None;
    }
    (domain.contains(['.', '/', '*']) || domain.starts_with('.') || domain.ends_with('/'))
        .then(|| domain.to_owned())
}

/// Where a provider was pointed when the session began: the address and the model the flags gave
/// it, for a `/restart` to put back.
///
/// note: a restart carries the provider over rather than connecting again, because the connection
/// is what is not cheap to rebuild - and `/model` and `/endpoint` switch that provider in place, so
/// the model on it is this session's drift and not the flags'. Taken once, before the first
/// session is wired, and put back before every relaunch.
#[derive(Debug, Clone)]
pub struct Flagged {
    endpoint: String,
    model: String,
}

impl Flagged {
    /// What `provider` is pointed at now.
    pub fn of(provider: &dyn Dialect) -> Self {
        Self {
            endpoint: provider.endpoint(),
            model: provider.model(),
        }
    }

    /// Points `provider` back there, where a `/model` or an `/endpoint` has moved it; nothing is
    /// asked of the endpoint where nothing moved.
    pub async fn restore(&self, provider: &dyn Dialect) {
        if provider.endpoint() != self.endpoint || provider.model() != self.model {
            provider
                .set_endpoint(self.endpoint.clone(), Some(self.model.clone()))
                .await;
        }
    }
}

/// Why [`Setup::check`] refused a setup, and which setting said what it refused.
///
/// note: the setting as a settings file spells its key - `sandbox-device`, `allow-server` - since
/// that is the one spelling every setting has, and a flag has it too with `--` in front. It is
/// `None` for a refusal no setting said, like a working directory that cannot be found.
///
/// note: it exists for the caller holding a settings file, which names the file beside a value the
/// file said and says nothing about one somebody typed. Without it that caller had one string and
/// no way to tell, and named the file for a `--sandbox-device` typed on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Refused {
    /// The setting whose value is refused, by its key in a settings file.
    pub setting: Option<&'static str>,
    /// What is wrong with it, as a sentence for a person.
    pub said: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.said)
    }
}

impl std::error::Error for Refused {}

impl Setup {
    /// What can be said about a setup before anything is reached: a path rule nothing can match,
    /// a tool nobody offers.
    ///
    /// note: separate from [`Setup::wire`], and called by it, so that a program can find out its
    /// arguments are wrong before it builds a provider. `main` connects to an endpoint and asks it
    /// what the model holds, which is a round trip and an API key - neither of them anybody's idea
    /// of how to be told that a settings file names `contxt`.
    ///
    /// note: what is refused says which setting said it, so that a caller holding a settings file
    /// can name the file where the file is what said it - and only there. See [`Refused`].
    pub fn check(&self) -> Result<(), Refused> {
        let refused = |setting: &'static str, said: String| Refused {
            setting: Some(setting),
            said,
        };
        for (list, subjects) in [("allow", &self.allow), ("deny", &self.deny)] {
            for subject in subjects {
                // a path rule that cannot match stops the session rather than being drawn on the
                // permissions tab like any other: a `--deny` that refuses nothing is worse than no
                // rule, because it reads as given
                if let tools::Subject::Path(pattern) = subject
                    && let Some(objection) = tools::objection_to(pattern)
                {
                    return Err(refused(list, objection));
                }
            }
        }

        for (list, subjects) in [("allow", &self.allow), ("deny", &self.deny)] {
            for subject in subjects {
                if let Some(objection) = unreached(subject) {
                    // a server's rule came in as `allow-server` or `deny-server`, which is where it
                    // was written; every other one is the list's own
                    let setting = match (subject, list) {
                        (tools::Subject::Server(_), "allow") => "allow-server",
                        (tools::Subject::Server(_), _) => "deny-server",
                        _ => list,
                    };
                    return Err(refused(setting, objection));
                }
            }
        }

        // note: a name that is not a tool stops the session rather than being skipped, for the
        // reason `deny_unknown_fields` is on the settings struct. Asking for `contxt` and getting
        // a session with no context tool and nothing said about it is the failure this setting is
        // most likely to have
        if let Some(wanted) = &self.tools {
            let offered: Vec<String> = offered().into_iter().map(|it| it.id).collect();
            if let Some(unknown) = wanted.iter().find(|it| !offered.contains(it)) {
                return Err(refused(
                    "tools",
                    format!(
                        "`{unknown}` is not one of this program's tools; they are {}",
                        offered.join(", ")
                    ),
                ));
            }
        }

        // note: the stem of every file `record` and `/save DIR/` write, and a snapshot is a file
        // anybody could have written. A name that is not one file name puts them somewhere else -
        // `../../elsewhere` outside the private directory the record is meant to go in
        let name = self
            .session_name
            .as_deref()
            .or(self.resume.as_ref().map(|it| it.session.as_str()));
        if let Some(name) = name
            && std::path::Path::new(name).file_name() != Some(std::ffi::OsStr::new(name))
        {
            return Err(refused(
                "session",
                format!(
                    "`{name}` is not a session name a file can be written under; a session is \
                     named by one file name, with no directory in it"
                ),
            ));
        }

        // note: refused because it cannot be kept. Landlock only ever adds to what a process may
        // do, so a read-only path inside a writable one is writable to `shell`, and `fs` checks
        // the writable roots first - while every screen would say it was read-only
        //
        // note: resolved through its parent where it is not there yet, since it can be made
        // afterwards and is then just as writable - by the tools themselves, among others
        if self.confine {
            let workdir = std::env::current_dir().map_err(|e| Refused {
                setting: None,
                said: format!("could not find the working directory: {e}"),
            })?;
            let writable: Vec<_> = std::iter::once(&workdir)
                .chain(self.reachable.iter())
                .filter_map(|path| sandbox::resolve(&workdir.join(path)))
                .collect();
            if let Some(nested) = self.readable.iter().find(|path| {
                sandbox::resolve(&workdir.join(path))
                    .is_some_and(|path| writable.iter().any(|root| path.starts_with(root)))
            }) {
                return Err(refused(
                    "sandbox-read",
                    format!(
                        "{}: `--sandbox-read` cannot make a path read-only inside one the tools \
                         may already write in",
                        nested.display()
                    ),
                ));
            }
        }

        // note: a device is granted reading and writing whatever is beneath it, which is what
        // `--sandbox-allow` is for anywhere else. Named here, it would be a writable path the
        // screens never mention - and `/dev/../home` is one, so the path is resolved first.
        //
        // note: asked of the list whether or not a confinement is being built, since a path that
        // is not a device is one whatever the sandbox is doing. Under `--no-sandbox` the list is
        // not used, so the harm is nothing granted - but a value nobody ever checked is a value
        // somebody finds out about at startup by being refused for it, after a run with
        // `--no-sandbox` in front of it had the list work
        if let Some(stray) = self
            .devices
            .iter()
            .find(|device| sandbox::device(device).is_none())
        {
            return Err(refused(
                "sandbox-device",
                format!(
                    "{}: `--sandbox-device` names a device under `/dev`; a path anywhere else is \
                     `--sandbox-allow`",
                    stray.display()
                ),
            ));
        }

        Ok(())
    }

    /// Wires one up around a provider that is already connected.
    pub fn wire(self, provider: Arc<dyn Dialect>) -> Result<Wired, String> {
        // before anything is built, so that an embedder gets the same refusal `main` gets before
        // it reaches an endpoint at all
        self.check().map_err(|refused| refused.to_string())?;

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
                // at the start of the record, so that a call allowed by one of these later says
                // where its permission came from, whether a flag or a settings file set it
                kernel.record_rule(subject.to_string(), verdict, None, false);
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
            .map(|engine| Arc::new(tools::Advised::new(policy.clone(), engine)));
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
        let shed = self.compact.filter(|it| *it < 1.0).map(|threshold| {
            let derived = tools::Shedder::under(threshold);
            tools::Shedder {
                target: self.compact_target.unwrap_or(derived.target),
                ..derived
            }
        });
        let compact_target = shed.as_ref().map(|shed| shed.target);
        let compact_threshold = shed.as_ref().map(|shed| shed.threshold);
        if let Some(shed) = shed {
            kernel.set_compactor(Some(Arc::new(shed)));
            // worked out again once the tools not asked for are off; see below
            kernel.set_full_notice(Some(full_notice(&kernel, &policy)));
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
        let mut confinement = sandbox::Confinement::Off;
        // what the shell's commands leave running, which `App` holds for whoever ends the session
        let stragglers = tools::Stragglers::default();
        let mut unconfined_because = None;
        let building = self.tools.as_ref().is_none_or(|it| !it.is_empty());
        if building {
            let program =
                std::env::current_exe().map_err(|e| format!("could not find myself: {e}"))?;
            if self.confine {
                let probed = sandbox::available(&program);
                confinement = probed.confinement;
                unconfined_because = probed.why;
                // note: only where the shell is confined, which `available` already folds in: the
                // gate is installed by the child that confines itself, so an unconfined shell has
                // nothing to hold a call with and goes on being asked about by name
                if probed.gated {
                    policy.gate_the_network();
                }
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
                    devices: self.devices,
                    // only when it would actually confine anything. A binary that has been
                    // replaced since this one started, or a kernel with no Landlock, is a `shell`
                    // that runs unconfined and a permissions view that says so - rather than one
                    // whose every command comes back with an error nobody can account for
                    confiner: confinement.is_confined().then(|| program.clone()),
                    limits: limits.clone(),
                    stragglers: stragglers.clone(),
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
        for spec in kernel.tool_specs() {
            policy.offers(spec);
        }

        if let Some(system) = self.system {
            kernel.push(ContextItem::system(system).pinned());
        }
        for path in &self.files {
            kernel.push(
                // note: `{e:#}` for the whole chain, as `/attach` prints it and for the reason its
                // note gives: what could not be done, and then the operating system's own account
                // of why. Without the `#` the cause is dropped and the refusal reads as
                // `missing.png: could not read missing.png` - the file named twice and no word
                // about why - or as `fifo: fifo is not a file`, with `os error` nowhere
                attach::attached(path)
                    .map_err(|e| format!("{e:#}"))?
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
        app.stragglers = stragglers;
        app.unconfined_because = unconfined_because;
        app.introspect = introspect;
        app.set_spend(self.spend);
        app.compact_target = compact_target;
        app.compact_threshold = compact_threshold;
        // what `/attach` says about a file it took, said about the ones `-f` took
        for item in app.kernel.items() {
            if let Some(caution) = attach::caution(&item) {
                app.say(crate::app::Speaker::Error, caution);
            }
        }

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
        app.refresh_full_notice();

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
        // the old session's stragglers are the old session's: the fresh one has a list of its own
        let stragglers = stragglers_at_end(app, self.leave_running);

        let name = app.kernel.session_name();
        let said = match self.record {
            false => format!("{name} ended; `--no-record`, so nothing was written"),
            true => match record(app) {
                Ok(written) => format!(
                    "{name} ended: {written} (`kamchatka -r {}` carries on from it)",
                    written.state.display()
                ),
                // the restart still happens: a session nobody could write down is a worse reason
                // to refuse somebody a fresh one than it is to carry on with the old
                Err(e) => format!("{name} ended, and could not be written down: {e}"),
            },
        };
        let said = std::iter::once(said)
            .chain(stragglers)
            .collect::<Vec<_>>()
            .join("\n");

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

/// Stops what the session's commands left running, or with `leave` only names it, and hands back
/// the lines saying which.
///
/// note: stopped by default, because the session that started them is over: a server or a
/// `sleep 300 &` the model put in the background ran on reparented to init, with nothing left to
/// answer its network questions or record what it did, and usually nobody had asked for it to
/// outlive anything. `--leave-running` is for somebody who did - a server started on purpose - and
/// is still told what it is leaving.
pub fn stragglers_at_end(app: &App, leave: bool) -> Vec<String> {
    let (commands, done) = match leave {
        true => (
            app.stragglers.running(),
            "left running, as `--leave-running` asked",
        ),
        false => (app.stragglers.stop(), "stopped what it had left running"),
    };

    commands
        .into_iter()
        .map(|cmd| format!("· `{cmd}` {done}"))
        .collect()
}

/// Whether the session has already said it is over.
///
/// note: the log rather than a flag on [`App`], because the loops that end a session end it on the
/// kernel and a flag would be a second place to keep the same fact. Reading the last record is
/// also the only answer that stays right for a loop nobody here has written: an embedder that ends
/// its own session gets one `session.finished`, and one that leaves it to [`Setup::relaunch`] gets
/// one too.
///
/// note: *any* record rather than the last. They differ after a second `ctrl+c`, which leaves a
/// headless run without waiting for the turn: the session is ended while the turn is still
/// running, and what it records after that is later than `session.finished`. Asking of the last
/// record then says the session never ended, and it is ended a second time. The program's own
/// parting asks this too, so the two can no longer disagree.
pub fn ended(kernel: &Kernel) -> bool {
    kernel.with_history(|log| {
        log.records()
            .any(|record| record.event == Event::SessionFinished)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped settings file lists every tool this program offers, and nothing else.
    ///
    /// note: the file lists them rather than leaving the key out, since `null` is refused and all
    /// of them is otherwise nothing a reader can see. A list that is written out is a list that
    /// goes stale, and a tool added without it is a tool a copied file quietly turns off.
    #[test]
    fn the_shipped_file_lists_every_tool() {
        let shipped: crate::config::Settings =
            serde_json::from_str(crate::config::SHIPPED).expect("the shipped settings parse");
        let mut listed = shipped.tools.expect("the shipped file lists the tools");
        let mut offered: Vec<String> = offered().into_iter().map(|it| it.id).collect();
        listed.sort();
        offered.sort();

        assert_eq!(listed, offered);
    }

    /// A session is over once anything said so, whatever was recorded after.
    ///
    /// note: the case a second `ctrl+c` makes - ended while a turn is still running, which goes
    /// on recording. Reading the last record, this said the session had never ended, and it was
    /// ended again.
    #[test]
    fn a_session_that_said_it_was_over_is_over() {
        let kernel = Kernel::new(Config::default());
        assert!(!ended(&kernel));

        kernel.finish();
        assert!(ended(&kernel));

        // something recorded after the end, as a turn still running would
        kernel.push(ContextItem::user("late"));
        assert!(
            kernel.with_history(|log| log
                .records()
                .last()
                .is_some_and(|record| record.event != Event::SessionFinished)),
            "the push recorded nothing, so this checks nothing"
        );
        assert!(ended(&kernel), "a record after the end undid it");
    }

    /// A session wired the way the tests here wire one: no tools, so nothing is built and
    /// nothing is asked of the sandbox, and a provider that reaches nothing.
    fn wired(setup: Setup) -> Wired {
        setup
            .wire(Arc::new(nachalnik_providers::OpenAiCompatible::new(
                "scripted",
                "http://127.0.0.1:1",
                "",
            )))
            .expect("the wiring failed")
    }

    /// A snapshot of a session that has said something, which is what `-r` hands the wiring.
    fn said_something() -> Snapshot {
        let app = wired(Setup {
            tools: Some(Vec::new()),
            compact: None,
            ..Default::default()
        })
        .app;
        app.kernel
            .push(ContextItem::user("something said before the restart"));
        app.kernel.snapshot()
    }

    /// A restart starts a session of its own rather than carrying the old one on a second time.
    ///
    /// note: `resume` and `session_name` are the two settings a restart does not take at their
    /// word, and this is the half that is about the context. A snapshot is where the *run*
    /// started and a restart is somebody leaving it, so a session begun out of one is the same
    /// conversation twice - the old session's items, its parameters and its spent ceiling in it,
    /// and a second `session.resumed` in the log where a session that has never been resumed
    /// belongs.
    #[test]
    fn a_restart_starts_a_session_rather_than_carrying_the_last_one_on() {
        let base = Setup {
            resume: Some(said_something()),
            record: false,
            tools: Some(Vec::new()),
            compact: None,
            ..Default::default()
        };
        let old = wired(base.clone()).app;
        let (fresh, _) = base.relaunch(&old, provider()).expect("a fresh session");

        assert_eq!(
            fresh.app.kernel.items().len(),
            0,
            "the session before the restart is in the one after it"
        );
        assert!(
            !fresh.app.kernel.with_history(|log| log
                .records()
                .any(|record| matches!(record.event, Event::SessionResumed { .. }))),
            "the fresh session is a resumption of the one before it"
        );
    }

    /// The session a restart starts is stamped fresh, and does not carry the name of the one it
    /// replaces.
    ///
    /// note: a name carried over would give two sessions of one run the same identity and put
    /// both records under one stem, where `record` settles that by writing the second beside the
    /// first - so the two logs are told apart by a `-2` rather than by the session they belong
    /// to, and the trace shows one session name for both. `None` would be worse and is why the
    /// stamp is set rather than the name cleared: the runtime's own counter restarts at 0 with
    /// the process, which is fine as an identity and useless as a filename.
    #[test]
    fn the_session_a_restart_starts_is_not_the_one_it_replaced() {
        let base = Setup {
            session_name: Some("before-the-restart".to_owned()),
            record: false,
            tools: Some(Vec::new()),
            compact: None,
            ..Default::default()
        };
        let old = wired(base.clone()).app;
        let (fresh, _) = base.relaunch(&old, provider()).expect("a fresh session");

        let name = fresh.app.kernel.session_name();
        assert_ne!(name, "before-the-restart", "the name carried over");
        assert!(
            name.contains('T') && name.ends_with('Z'),
            "{name} is not a session's own stamp"
        );
    }

    /// The provider the wiring is handed, which reaches nothing: what these tests assert is about
    /// the session the wiring builds, not about an answer.
    fn provider() -> Arc<dyn Dialect> {
        Arc::new(nachalnik_providers::OpenAiCompatible::new(
            "scripted",
            "http://127.0.0.1:1",
            "",
        ))
    }
}
