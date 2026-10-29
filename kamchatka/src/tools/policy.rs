//! `Careful`: what may run, what has to be asked about, and what is refused outright.
//!
//! note: it judges a command line by a heuristic, and does not claim to be more than that. It
//! reads the tool name and arguments as data; nothing a model says about its own permissions
//! reaches here. It produces a stance that can be read back on the permissions tab and changed.
//! `Shell` subsumes every other capability, so answers about the rest are reports rather than
//! boundaries. That is why the path rules exist, and why the tab says as much.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use nachalnik::{
    Capability, Domain, PermissionPolicy, PermissionRequest, ToolCallId, ToolSpec, Verdict,
    async_trait,
};
use parking_lot::Mutex;

use super::reaching::Reaching;

/// Something [`Careful`] holds an opinion about.
///
/// note: four questions about one call: what (operation and domain), to what (path), whose tool
/// (server). Only operation and domain are a hierarchy; [`Careful::stance`] resolves those two
/// alone. Path and server come from the arguments and registry and fold in with the strictest-wins
/// rule.
///
/// note: a capability is not fine enough on its own. `fs:read: allow` is reasonable;
/// `fs:read .env: allow` is not. The difference is a property of the file, not of the tool that
/// opened it, so a path rule is one subject rather than one per tool and binds every tool handed a
/// path as its `path` argument. A tool from an MCP server names its own arguments; one calling its
/// path `file_path` meets only the rules about its server. See [`Careful::judges`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Subject {
    /// One operation in one domain: `fs:read`.
    Capability(Capability),
    /// A whole domain, which is every operation in it: `fs`.
    Domain(Domain),
    /// A pattern the path a tool was handed is matched against.
    Path(String),
    /// The MCP server a tool came from, which is where it came from rather than what it does.
    Server(String),
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(capability) => write!(f, "{capability}"),
            Self::Domain(domain) => write!(f, "{domain}"),
            Self::Path(pattern) => write!(f, "{pattern}"),
            Self::Server(name) => write!(f, "server {name}"),
        }
    }
}

impl Subject {
    /// Reads one back: `fs`, `fs:read`, `.env*`, `secrets/`.
    ///
    /// note: `mcp:call` is the one subject a tool declares and is not always judged by; see
    /// [`Careful::decides`], which is what a table asks about it.
    ///
    /// note: anything holding a `/`, a `*` or a leading `.` is a path pattern; anything holding a
    /// `:` is one operation; anything else is a whole domain. It has to be a rule rather than a
    /// guess because all three are spelled as bare text, and this one is readable in a sentence: a
    /// colon means an operation, and the characters a path has and a name does not mean a path.
    ///
    /// note: a server is not in here, and cannot be: `files` is a server name and `fs` is a domain,
    /// and nothing in either string says which. It is named on its own argument instead - see
    /// `--auto-allow-servers`.
    ///
    /// note: `fs:*` is read as `fs`, the whole domain, and written back as `fs`.
    ///
    /// note: it is the inverse of [`fmt::Display`] above for the three it can produce, so
    /// `--auto-deny "$(read a row off the permissions tab)"` means what it says.
    pub fn parse(text: &str) -> Self {
        // `context:*` is the whole domain, and the way somebody used to globs would write it. It
        // holds a `*`, so it was a path rule matching files called `context:...`, which granted
        // nothing about `context` and covered every tool that takes a path
        if let Some(domain) = text.strip_suffix(":*")
            && !domain.is_empty()
            && !domain.contains([':', '/', '*'])
            && !domain.starts_with('.')
        {
            return Self::Domain(Domain::from(domain));
        }
        if text.contains('/') || text.contains('*') || text.starts_with('.') {
            return Self::Path(text.to_owned());
        }

        match Capability::parse(text) {
            Ok(capability) => Self::Capability(capability),
            Err(_) => Self::Domain(Domain::from(text)),
        }
    }
}

/// Actions of one tool that each need every one of several subjects; see
/// [`Careful::needed_together`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Together {
    /// The tool.
    pub tool: String,
    /// Its actions that need all of [`Together::needed`], in the order the tool offers them.
    pub actions: Vec<String>,
    /// The subjects they need, every one of them.
    pub needed: Vec<Capability>,
}

impl Together {
    /// The sentence every table puts under its rows: "`context`'s `undo` and `redo` need all of
    /// `context:move`, ..., so the strictest of those applies to `undo` and `redo`".
    pub fn sentence(&self) -> String {
        let actions: Vec<String> = self.actions.iter().map(|it| format!("`{it}`")).collect();
        let needed: Vec<String> = self.needed.iter().map(|it| format!("`{it}`")).collect();
        let need = match actions.len() {
            1 => "needs",
            _ => "need",
        };
        let actions = crate::reconcile::listed(&actions);
        format!(
            "`{}`'s {actions} {need} all of {}, so the strictest of those applies to {actions}",
            self.tool,
            crate::reconcile::listed(&needed),
        )
    }
}

/// `mcp:call`: the subject every tool from a server declares, meaning "somebody else's tool, and
/// nobody has vouched for what it does".
///
/// note: named once so the two places that report on it agree with the one that decides by it. See
/// [`Careful::judges`], which answers with the server's own name where it knows the server, and
/// [`Careful::decides`], which is how a table says so.
fn unvouched() -> Capability {
    Capability::of(Domain::Other("mcp".into()), "call")
}

/// The paths a fresh policy has something to say about.
///
/// note: a short list of names that are credentials by convention; every one is `ask` rather than
/// `deny`. When somebody answers `always` for `read`, the capability goes to `allow` and these stay
/// where they are, so reading `src/main.rs` goes silent and reading `.env` is still a question. A
/// rule finer than a capability is only worth having once the capability is open.
const SUSPECT: &[&str] = &[
    ".env*",
    "*.pem",
    "*.key",
    "*.p12",
    "id_rsa*",
    "id_ed25519*",
    "*credentials*",
    "secrets/",
    ".ssh/",
    ".aws/",
    ".gnupg/",
];

/// Whether a path matches this pattern.
///
/// note: a pattern ending in `/` is a directory: it matches any path containing that component.
/// Anything else is matched against the file name, with `*` standing for any run of characters.
/// That is less than a glob crate would give and is what these rules need: a pattern language
/// nobody can predict is worse on a permissions screen than a small one.
///
/// note: the path is read as a [`Path`] rather than split on `/`, so the name matched is the name
/// the file is opened under. It is the same string [`Reach::allows`](crate::sandbox::Reach::allows)
/// resolves, and the two must not disagree. Splitting on slashes would make `.env/` have no last
/// component and match nothing, while resolving it produces `.env` and reads it. A trailing slash,
/// a `.` in the middle, or a doubled separator changes neither the file nor the rule.
///
/// note: this cannot see a symlink. A rule is about a name, and a name that resolves elsewhere
/// resolves after this has answered. `fs` checks where the link led once resolved - see
/// `search::led_past` - and for a hard link, which has no target, where its other names are - see
/// `names` - and the sandbox boundary does not care about names - see [`crate::sandbox`].
///
/// note: names are compared exactly, as the filesystem here compares them: `.ENV` is another file
/// than `.env`, and folding a rule would reach files it was not written for. A backslash is a
/// character a name may hold, not a separator.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let path = Path::new(path);

    // note: not a `./` at the front, which `Path::components` keeps and which names no directory:
    // `./README.md` is `README.md`, and a rule that told the two apart would be the `.` that must
    // not change which rule applies
    if let Some(directory) = pattern.strip_suffix('/') {
        return path
            .components()
            .filter(|component| *component != std::path::Component::CurDir)
            .any(|component| component.as_os_str().to_str() == Some(directory));
    }

    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => glob(pattern, name),
        None => false,
    }
}

/// What a path rule may be, in the words an error has to say it in.
const GRAMMAR: &str = "a path rule is a file name in which `*` stands for any run of characters - \
                       `*.pem`, `.env*` - or one directory name with a slash after it - \
                       `secrets/` - which is about that directory wherever it sits in a path";

/// What is wrong with a path rule, if anything.
///
/// note: the grammar [`path_matches`] reads is small, and a pattern outside it would be taken all
/// the same: `--auto-allow 'src/**'` would go onto the permissions tab and be consulted about every
/// call, and no path can match it. A rule that cannot match is the worst way for one to be wrong,
/// because a `--auto-deny` that refuses nothing reads as given, so it is refused where it is
/// entered, and the refusal says what there is.
///
/// note: refused rather than taught to the matcher. Whole-path patterns would add anchoring,
/// absolute versus relative, and `**`; a permissions screen is worth more small than complete.
///
/// note: a `*` before the slash is refused too. A directory can really be called `sec*`, and the
/// directory branch compares the name as written, so `secrets*/` would silently fail to cover
/// `secrets-old/`.
pub fn objection_to(pattern: &str) -> Option<String> {
    let objection = |why: &str| Some(format!("`{pattern}` {why}; {GRAMMAR}"));

    if pattern.is_empty() {
        return objection("is not a rule at all");
    }
    // a rule without a slash is about the last name in a path, and no path's last name is `.` or
    // `..`. With a slash they are directory rules, and `../` matches every path that climbs out, so
    // only the bare two are refused; `./` is a way of spelling a directory, not a directory a path
    // is in
    if matches!(pattern, "." | "..") {
        return objection("cannot match: no file is called that");
    }
    if pattern == "./" {
        return objection("cannot match: `.` is how a path is spelled, not a directory in it");
    }
    match pattern.strip_suffix('/') {
        Some(directory) if directory.is_empty() || directory.contains('/') => {
            objection("cannot match: a directory rule is one name, and this is a path")
        }
        Some(directory) if directory.contains('*') => objection(
            "is read as the name it is written as: a directory rule is a name, not a pattern",
        ),
        Some(_) => None,
        None if pattern.contains('/') => {
            objection("cannot match: a rule about a directory is that directory's name and a slash")
        }
        None => None,
    }
}

/// Whether a name matches a pattern in which `*` stands for any run of characters.
///
/// note: it backtracks. Walking the pattern's literals with `find` and taking the first hit
/// refuses `abcbc` for `a*bc`: the `bc` found first is the one the star should swallow, and there
/// is no way back. A permission rule that silently fails to match is the worst way for one to be
/// wrong, and `*credentials*.json` is not an exotic thing to write.
///
/// note: over bytes rather than characters. Both sides are `str`, so equal bytes are equal
/// characters, and a `*` landing mid-character can only ever be a position the match moves past.
fn glob(pattern: &str, name: &str) -> bool {
    let (pattern, name) = (pattern.as_bytes(), name.as_bytes());
    let (mut p, mut n) = (0, 0);
    // where the last `*` was, and how much of the name it has been asked to swallow so far
    let (mut star, mut swallowed) = (None, 0);

    while n < name.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some(p);
                swallowed = n;
                p += 1;
            }
            Some(c) if *c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some(at) => {
                    p = at + 1;
                    swallowed += 1;
                    n = swallowed;
                }
                None => return false,
            },
        }
    }

    // whatever is left of the pattern has to be stars, which match the empty rest
    while pattern.get(p) == Some(&b'*') {
        p += 1;
    }

    p == pattern.len()
}

/// Everything is a question until the person at the terminal answers one - including reading, and
/// including a handful of paths that look like credentials.
///
/// note: capabilities are the unit rather than tool names, which makes this work for tools this
/// crate has never heard of. An MCP server's tools are judged as that server - see
/// [`Careful::judges`] - so answering "always" to one of them is answering for that server, and
/// only that server.
///
/// note: the state is a map from a subject to the [`Verdict`] this will return for it, rather than
/// a set of the allowed ones and a hard-coded refusal for the network, so every one of those
/// answers is a value somebody can look at and change, which is what the permissions tab draws. A
/// policy whose decisions can only be observed by triggering them would not show that a policy is
/// replaceable.
///
/// note: a domain is the whole of what is done in it, and a rule finer than one is about the
/// operation it names. `context` allows every operation in that domain; `context:move` allows
/// eliding, excluding and restoring and says nothing about the rest. Against that, the strictest of
/// everything consulted wins: `fs:read` stays `allow` while `.env` is a question, and a refused
/// domain stays refused however finely an operation in it is named. See [`Careful::judges`].
///
/// note: path rules - see [`Subject`] - bind `fs`, and not `shell`. A command names its files
/// inside a string, and `cat .env`, `sed -n 1p .env`, `python -c "open('.env')"` and `base64 <.env`
/// are the same act written four ways: a check over that string would refuse the first and wave the
/// rest through while looking like a rule. What binds a command is the kernel, and what the kernel
/// can express is a directory - see [`crate::sandbox`]. So `cat .env` works where `read .env` asks,
/// by design rather than by oversight.
pub struct Careful {
    stances: Mutex<BTreeMap<Subject, Verdict>>,
    /// Which MCP server each tool came from, for the tools that came from one.
    ///
    /// note: held here rather than read off a tool's name, because a name is the server's to
    /// choose and a prefix is optional and can be dropped when it will not fit. This program
    /// spawned the server, so it is the thing that knows; `Careful::came_from` is how it says so.
    servers: Mutex<BTreeMap<String, String>>,
    /// What it answers about paths matching a pattern, in the order they are consulted.
    ///
    /// note: ordered rather than a map, because these are read out on a screen and somebody
    /// adding one wants it where they put it. The strictest match wins regardless, so the order
    /// is for the reader rather than for the answer.
    paths: Mutex<Vec<(String, Verdict)>>,
    /// The calls a person was asked about and allowed, whose command reaches for the network.
    ///
    /// note: a stance of `ask` answered `yes, once` is permission for *that call*, and the
    /// sandbox has to know or the command runs with the network cut and fails in a way that
    /// contradicts what the person was just told. A stance is what the tab draws; this is the
    /// answer to a question, which the tab never sees.
    ///
    /// note: it holds one batch's answers and is emptied when the next request goes out - see
    /// [`Careful::forget_network_grants`] - rather than being bounded the way the refusals below
    /// are. A bound here throws away the entry most likely to be wanted: every call in a batch is
    /// decided before any of them runs, so one `yes` past the bound drops the first, and that
    /// command runs with the network cut after somebody allowed it.
    networked: Mutex<BTreeSet<ToolCallId>>,
    /// What each of the session's own tools declares, by its identifier.
    ///
    /// note: whoever installs the tools tells it here, so that a sentence sending a model to a
    /// capability can tell whether any tool on offer declares it; see [`Careful::reachable`].
    offered: Mutex<BTreeMap<String, ToolSpec>>,
    /// Which actions of a tool with several need each subject, by the subject: `fs:read` is needed
    /// by `fs`'s `read`, `glob` and `grep`.
    ///
    /// note: a subject names what a call does rather than which action asks for it, so one answers
    /// for several. Wherever one is put to somebody deciding - the question, the permissions tab,
    /// `setup`, a refusal - the actions are named beside it, or an `always` for `fs:read` given to
    /// a `grep` would allow `read` with nobody having said so. Each tool says which, through
    /// [`nachalnik::Tool::needs`]; see [`Careful::offers`].
    covered: Mutex<BTreeMap<Capability, Vec<(String, String)>>>,
    /// The tools among [`Careful::offered`] taken off offer for now, by `/tools toggle`.
    withdrawn: Mutex<BTreeSet<String>>,
    /// What a question comes to where nobody is there to answer it - a headless run's `--on-ask` -
    /// and `None` where somebody is.
    unanswered: Mutex<Option<Verdict>>,
    /// Why the last few refusals were refused, by the call they refused.
    ///
    /// note: only the policy knows this, and nothing carries it out: the kernel is handed a
    /// `Verdict` and records `the call was not permitted`, which is true and unhelpful when the
    /// tool's own capability is `allow` and something else refused it - the `shell: allow` /
    /// `network: deny` pair. A refusal should be accountable, so it is written down here, where it
    /// is known, and [`Careful::why`] hands it out.
    refusals: Mutex<VecDeque<(ToolCallId, String)>>,
    /// Whether a confined command's network is asked about when it tries, rather than read off its
    /// name; see [`Careful::gate_the_network`].
    gated: AtomicBool,
    /// The questions a running command is waiting on, which is where a held attempt is asked.
    reaching: Reaching,
}

/// How many refusals are kept for whoever asks why.
///
/// note: a queue rather than a map, and the oldest goes rather than all of them. Emptying it
/// outright when full would throw away the entry it is most likely to need: a refusal is written
/// down when the policy answers and read when the kernel builds the tool result, so the live one is
/// among the newest. A bound is right because nobody is obliged to read one at all - that is not
/// true of the grants above, every one of which is read by the call it was given for.
const REMEMBERED: usize = 64;

impl Default for Careful {
    fn default() -> Self {
        Self::new()
    }
}

impl Careful {
    /// Builds a policy that asks about everything, including a short list of paths that look like
    /// credentials.
    pub fn new() -> Self {
        Self {
            // note: nothing is decided on somebody's behalf - not `read`, which is the one people
            // would pick, and not `network`. Either would be a decision taken for the user about
            // something they may well want, and the sandbox is what makes either answer mean
            // something once they have given it.
            //
            // note: and no operation rules either. Seeding some of `context`'s at `ask`, so that
            // allowing the tool still left an `exclude` a question, would make
            // `--auto-allow context` mean something other than `context`, with no way to guess from
            // the words which ones. A domain is the whole of what is done in it; somebody who wants
            // less than that writes the operation they want.
            stances: Mutex::new(BTreeMap::new()),
            servers: Mutex::new(BTreeMap::new()),
            paths: Mutex::new(
                SUSPECT
                    .iter()
                    .map(|pattern| ((*pattern).to_owned(), Verdict::Ask))
                    .collect(),
            ),
            networked: Mutex::new(BTreeSet::new()),
            offered: Mutex::new(BTreeMap::new()),
            covered: Mutex::new(BTreeMap::new()),
            withdrawn: Mutex::new(BTreeSet::new()),
            unanswered: Mutex::new(None),
            refusals: Mutex::new(VecDeque::new()),
            gated: AtomicBool::new(false),
            reaching: Reaching::new(),
        }
    }

    /// Marks that a confined command's attempts at the network are held by the gate, so the
    /// network is asked about when the command tries, not from what it is called.
    ///
    /// note: set by whoever builds the shell, once [`crate::sandbox::available`] confirms the gate
    /// holds. From then on [`Careful::judges`] stops reading a command for program names, and an
    /// allowed `exec:run` runs without a question until a command reaches out.
    pub fn gate_the_network(&self) {
        self.gated.store(true, Ordering::Relaxed);
    }

    /// Whether [`Careful::gate_the_network`] was said.
    pub fn gates_the_network(&self) -> bool {
        self.gated.load(Ordering::Relaxed)
    }

    /// The questions a running command is waiting on.
    pub fn reaching(&self) -> &Reaching {
        &self.reaching
    }

    /// Everything this policy consults about one call, in the order it reads them out.
    ///
    /// note: the capabilities the call needs, which name the operation it picks since
    /// [`Tool::needs`](nachalnik::Tool::needs) is asked per call, plus two things only the
    /// arguments say: that a command reaches for the network, and that a path has a rule about it.
    /// One list rather than separate checks because everything downstream wants the same thing: the
    /// question asks about these, `always` answers for these, and a refusal is blamed on whichever
    /// said no. Several lists produce bugs like a one-off `yes` to `curl` that then runs with the
    /// network cut.
    pub fn judges(&self, request: &PermissionRequest) -> Vec<Subject> {
        // note: read the arguments as the tool reads them. This program's tools take theirs inside
        // a `call` object; reading outside it finds neither the `cmd` a network rule is about nor
        // the `path` a path rule is about, so both would quietly stop being consulted - the one
        // failure a permission policy must not have.
        //
        // note: a `call` object is read through the tool it belongs to. Another tool's `call`
        // argument may mean something else, and its `path` is then read as a path. That is the
        // direction to be wrong in: consulting a rule about a string that is not a path asks a
        // question nobody needed, where skipping it makes a rule stop being one. `inner` only reads
        // through a `call` that is the whole of the arguments, so nothing outside one is passed
        // over for it
        let args =
            super::ops::inner(&request.args).unwrap_or(std::borrow::Cow::Borrowed(&request.args));

        let mut judged: Vec<Subject> = request
            .capabilities
            .iter()
            .cloned()
            .map(Subject::Capability)
            .collect();

        // note: read the command's name only where the gate does not hold. Where it holds, the
        // question is the command's own attempt, asked while it runs - see `tools::Shell`.
        // Consulting `net:reach` here too would ask about `git status` a second time for nothing
        if request.capabilities.contains(&Capability::exec("run"))
            && !self.gates_the_network()
            && args
                .get("cmd")
                .and_then(serde_json::Value::as_str)
                .is_some_and(reaches_the_network)
        {
            judged.push(Subject::Capability(Capability::net("reach")));
        }
        // note: the path a tool was handed, not a path named inside a shell command; see the note
        // on `Careful` for why the second is not attempted
        if let Some(path) = args.get("path").and_then(|path| path.as_str()) {
            judged.extend(
                self.paths
                    .lock()
                    .iter()
                    .filter(|(pattern, _)| path_matches(pattern, path))
                    .map(|(pattern, _)| Subject::Path(pattern.clone())),
            );
        }

        // note: where the tool came from, the one thing about a call that is neither an act nor an
        // argument. A tool from an MCP server is answerable as that server whatever it claims, and
        // this program knows the server because it spawned it - not from a name, which a server can
        // choose.
        //
        // note: it answers for `mcp:call`, which every tool from a server declares and means
        // "somebody else's tool, and nobody has vouched for what it does". Naming the server is the
        // same statement made precisely, so consulting both would make `--auto-allow-servers files`
        // grant nothing until `mcp:call` had been answered too. Any other capability a server's
        // tool declares is still consulted, but under `nachalnik_mcp::Trust::Nothing`, which this
        // program installs servers with, there is none: what a tool's annotations claim is not
        // believed.
        //
        // note: except where somebody refused it. The server stands in for `mcp:call`'s question,
        // not for `--auto-deny mcp:call` or `--auto-deny mcp`. Dropping one would let
        // `--auto-allow-servers` bypass a refusal, which no rule may do
        let unvouched = Subject::Capability(unvouched());
        let refused = self.stance(&unvouched) == Verdict::Deny;
        if let Some(server) = self.servers.lock().get(&request.tool) {
            if !refused {
                judged.retain(|subject| *subject != unvouched);
            }
            judged.push(Subject::Server(server.clone()));
        }

        judged
    }

    /// Whether a rule about `subject` has anything to say about a call to `tool`, asked of the tool
    /// rather than of a call it has not made yet.
    ///
    /// note: for the two tables that say what a rule covers - the permissions tab and
    /// `setup: permissions` - which would otherwise answer from
    /// [`ToolSpec::capabilities`](nachalnik::ToolSpec) while [`Careful::judges`] answers from
    /// something narrower. `mcp:call` is the case: every tool from a server declares it, and
    /// `judges` takes it back out again for a server this program spawned, so a table read off the
    /// declarations would list a server's tools beside an `allow` for `mcp:call` that is never
    /// consulted about them. A coverage column that names tools a rule will not be consulted about
    /// is worse than an empty one, because somebody checks their own flags against it. A `deny` for
    /// it is consulted, and covers them.
    ///
    /// note: it takes a name and not a [`PermissionRequest`], so a path rule - which is about the
    /// argument of one call rather than about the tool - is not something it can answer. Those two
    /// tables have their own section for path rules, the place for a subject whose coverage is not
    /// a property of any tool.
    pub fn decides(&self, subject: &Subject, tool: &str) -> bool {
        match subject {
            Subject::Capability(capability) if *capability == unvouched() => {
                self.server_of(tool).is_none() || self.stance(subject) == Verdict::Deny
            }
            _ => true,
        }
    }

    /// What it would answer about a whole call, deciding nothing and recording nothing.
    ///
    /// note: [`Careful::evaluate`] is this and a note about whatever refused it, and the terminal
    /// asks the same question when it sweeps up the calls an `always` has just answered for. A
    /// second hand-written copy of the fold would be a second place to get it wrong.
    pub fn verdict(&self, request: &PermissionRequest) -> Verdict {
        // the strictest answer among the subjects wins, so a call that needs both an allowed one
        // and an unmentioned one is still a question. `Verdict::strictest` is the runtime's own
        // fold for exactly this. A call that needs nothing is allowed: the empty fold
        //
        // note: a call this program's own tools cannot read never gets here. Each tool says so
        // through `Tool::malformed`, and the kernel answers it before any policy is asked
        self.judges(request)
            .iter()
            .map(|subject| self.stance(subject))
            .fold(Verdict::Allow, Verdict::strictest)
    }

    /// What it answers about one subject; anything unmentioned gets `Ask`.
    ///
    /// note: the most specific rule that has an answer decides, and a `deny` above it overrules.
    /// `--auto-allow fs` covers `fs:read` because nothing finer was said;
    /// `--auto-allow fs --auto-deny fs:write` refuses changing files and allows the rest;
    /// `--auto-allow fs:read` allows reading while `fs` is still a question, which is what naming
    /// one operation plainly means. A domain somebody denied stays denied however finely
    /// an operation of it is named, so `--auto-deny` is the last word and the strictest of
    /// everything consulted wins.
    pub fn stance(&self, subject: &Subject) -> Verdict {
        match subject {
            Subject::Capability(capability) => {
                let stances = self.stances.lock();
                let domain = stances.get(&Subject::Domain(capability.domain.clone()));
                let exact = stances.get(subject);
                match (domain, exact) {
                    (Some(Verdict::Deny), _) | (_, Some(Verdict::Deny)) => Verdict::Deny,
                    _ => exact.or(domain).copied().unwrap_or(Self::untold()),
                }
            }
            Subject::Path(pattern) => self
                .paths
                .lock()
                .iter()
                .find(|(known, _)| known == pattern)
                .map(|(_, verdict)| *verdict)
                .unwrap_or(Self::untold()),
            Subject::Domain(_) | Subject::Server(_) => self
                .stances
                .lock()
                .get(subject)
                .copied()
                .unwrap_or(Self::untold()),
        }
    }

    /// What it answers about a subject nobody has told it anything about, which is everything until
    /// somebody answers a question.
    ///
    /// note: a function every arm of [`Careful::stance`] falls back to rather than a `Verdict::Ask`
    /// written into each of them, so that the sentence the permissions tab draws about this policy
    /// is read out of the policy and cannot come to disagree with it. A tab that listed only the
    /// answers somebody had given would say nothing about what decides in the meantime, which is
    /// the first thing a screen of permissions is asked.
    pub const fn untold() -> Verdict {
        Verdict::Ask
    }

    /// Decides what to answer about one subject from now on.
    pub fn set(&self, subject: &Subject, verdict: Verdict) {
        match subject {
            Subject::Capability(_) | Subject::Domain(_) | Subject::Server(_) => {
                self.stances.lock().insert(subject.clone(), verdict);
            }
            Subject::Path(pattern) => {
                let mut paths = self.paths.lock();
                match paths.iter_mut().find(|(known, _)| known == pattern) {
                    Some(rule) => rule.1 = verdict,
                    None => paths.push((pattern.clone(), verdict)),
                }
            }
        }
    }

    /// Moves one subject on to the next answer: ask, then allow, then deny, then ask again.
    pub fn cycle(&self, subject: &Subject) -> Verdict {
        let next = match self.stance(subject) {
            Verdict::Ask => Verdict::Allow,
            Verdict::Allow => Verdict::Deny,
            Verdict::Deny => Verdict::Ask,
        };
        self.set(subject, next);

        next
    }

    /// Remembers that these subjects may be used without asking again.
    pub fn always(&self, subjects: &[Subject]) {
        for subject in subjects {
            self.set(subject, Verdict::Allow);
        }
    }

    /// Records that a person, asked about this call, allowed it - and that it reaches the network.
    pub fn grant_the_network(&self, call: &ToolCallId) {
        self.networked.lock().insert(call.clone());
    }

    /// Whether [`Careful::grant_the_network`] was told about this call.
    pub fn was_granted_the_network(&self, call: &ToolCallId) -> bool {
        self.networked.lock().contains(call)
    }

    /// Forgets those answers, the batch they were given for being over.
    ///
    /// note: a one-off `yes` is permission for one call, and the moment nothing can still be
    /// waiting for one is the next request: the kernel decides a batch, runs it, and is back at
    /// `Idle` before it asks for anything again. Emptying this when a call finishes would take
    /// the batch's other answers with it, and bounding it drops a live one.
    pub fn forget_network_grants(&self) {
        self.networked.lock().clear();
    }

    /// Why the given call was refused, if this is what refused it.
    ///
    /// note: copied rather than taken out because two callers want different things: the screen
    /// tells the person, and the kernel puts it into the tool result the model reads - see
    /// [`PermissionPolicy::why`]. Handing it over once would give it to whichever asked first and
    /// tell the other nothing.
    pub fn why(&self, call: &ToolCallId) -> Option<String> {
        self.refusals
            .lock()
            .iter()
            .find(|(known, _)| known == call)
            .map(|(_, why)| why.clone())
    }

    /// The rule that keeps an `allow` for the file name `name` from ever deciding: another path
    /// rule, not itself an allow, that matches the name.
    ///
    /// note: every path rule that matches a call is consulted and the strictest wins, so beside
    /// `.env*: ask` an allow for `.env` changes nothing, and taken without a word it reads as
    /// given. Only a name is judged - no `*` and no `/` - because only for a name does matching it
    /// mean covering every path the allow could decide; `*.txt` and `vendor/` reach further than
    /// any one rule they meet.
    pub fn overruled(&self, name: &str) -> Option<(String, Verdict)> {
        if name.contains(['*', '/']) {
            return None;
        }
        self.paths
            .lock()
            .iter()
            .find(|(other, verdict)| {
                other != name && *verdict != Verdict::Allow && path_matches(other, name)
            })
            .cloned()
    }

    /// The path rules, in the order they are read out.
    pub fn paths(&self) -> Vec<(String, Verdict)> {
        self.paths.lock().clone()
    }

    /// Every capability this has been told about, and what it will answer, in a stable order.
    pub fn stances(&self) -> Vec<(Subject, Verdict)> {
        self.stances
            .lock()
            .iter()
            .map(|(subject, verdict)| (subject.clone(), *verdict))
            .collect()
    }

    /// Remembers what one of the session's own tools declares, so that a call to it that names no
    /// operation is refused saying so, and which of its actions need each subject it declares, as
    /// the tool answers for a call naming each one. A tool with one action has nothing to say
    /// beyond its subject, and is not asked.
    ///
    /// note: the tool rather than its spec, so nothing can tell the policy what a tool declares
    /// without it also learning which actions each subject covers; see [`Careful::named_for`].
    pub fn offers(&self, tool: &dyn nachalnik::Tool) {
        let spec = tool.spec();
        let actions = super::ops::offered(&spec.schema);
        {
            let mut covered = self.covered.lock();
            for actions in covered.values_mut() {
                actions.retain(|(id, _)| *id != spec.id);
            }
            if actions.len() > 1 {
                for action in actions {
                    let call = nachalnik::ToolCall::new(
                        "c",
                        spec.id.as_str(),
                        serde_json::json!({ super::ops::WRAPPER: { "action": action } }),
                    );
                    for capability in tool.needs(&call) {
                        covered
                            .entry(capability)
                            .or_default()
                            .push((spec.id.clone(), action.to_owned()));
                    }
                }
            }
            covered.retain(|_, actions| !actions.is_empty());
        }
        self.offered.lock().insert(spec.id.clone(), spec);
    }

    /// The actions of `tool` that need `capability`, where it has several; empty where it has one,
    /// or none that need it.
    pub fn actions_for(&self, capability: &Capability, tool: &str) -> Vec<String> {
        self.covered
            .lock()
            .get(capability)
            .into_iter()
            .flatten()
            .filter(|(id, _)| id == tool)
            .map(|(_, action)| action.clone())
            .collect()
    }

    /// What a rule about `capability` decides for in `tool`: the tool's name, and the actions of it
    /// that need the capability where it has several - `fs: read, glob, grep`.
    pub fn named_for(&self, capability: &Capability, tool: &str) -> String {
        let actions = self.actions_for(capability, tool);
        match actions.is_empty() {
            true => tool.to_owned(),
            false => format!("{tool}: {}", actions.join(", ")),
        }
    }

    /// The actions of these tools that need more than one subject, with the subjects each needs.
    ///
    /// note: for the tables that list actions under the subjects they need - `setup`'s and
    /// `/permissions` - where an action listed under several rows reads as allowed by any one of
    /// them. It needs all of them: `context`'s `undo` is under
    /// `context:move` and `context:revise` both, and a refused `context:revise` refuses it however
    /// `context:move` is answered. Worked out from what each tool says its actions need, so it
    /// changes when they do.
    pub fn needed_together(&self, specs: &[nachalnik::ToolSpec]) -> Vec<Together> {
        let mut together: Vec<Together> = Vec::new();
        for spec in specs {
            let mut by_action: Vec<(String, Vec<Capability>)> = Vec::new();
            for capability in &spec.capabilities {
                for action in self.actions_for(capability, &spec.id) {
                    match by_action.iter_mut().find(|(it, _)| *it == action) {
                        Some((_, needed)) => needed.push(capability.clone()),
                        None => by_action.push((action, vec![capability.clone()])),
                    }
                }
            }
            for (action, mut needed) in by_action {
                if needed.len() < 2 {
                    continue;
                }
                needed.sort();
                match together
                    .iter_mut()
                    .find(|it| it.tool == spec.id && it.needed == needed)
                {
                    Some(it) => it.actions.push(action),
                    None => together.push(Together {
                        tool: spec.id.clone(),
                        actions: vec![action],
                        needed,
                    }),
                }
            }
        }

        together
    }

    /// The actions that need `capability`, said for a model to read - "`fs`'s `read`, `glob` and
    /// `grep`" - or `None` where no tool with several actions declares it.
    pub fn needed_by(&self, capability: &Capability) -> Option<String> {
        let covered = self.covered.lock();
        let actions = covered.get(capability)?;
        let mut tools: Vec<&str> = actions.iter().map(|(id, _)| id.as_str()).collect();
        tools.dedup();
        let said: Vec<String> = tools
            .into_iter()
            .map(|tool| {
                let named: Vec<String> = actions
                    .iter()
                    .filter(|(id, _)| id == tool)
                    .map(|(_, action)| format!("`{action}`"))
                    .collect();
                format!("`{tool}`'s {}", crate::reconcile::listed(&named))
            })
            .collect();

        Some(crate::reconcile::listed(&said))
    }

    /// Says that a tool [`Careful::offers`] was told about is off offer for now, or back on it.
    pub fn withdraw(&self, tool: &str, withdrawn: bool) {
        let mut off = self.withdrawn.lock();
        match withdrawn {
            true => off.insert(tool.to_owned()),
            false => off.remove(tool),
        };
    }

    /// The suggestions among `ways` a model could act on now, each a clause naming what it would
    /// call, joined into one; `None` where there is none to make.
    pub(crate) fn ways(&self, ways: &[(Capability, &str)]) -> Option<String> {
        let open: Vec<&str> = ways
            .iter()
            .filter(|(capability, _)| self.reachable(capability))
            .map(|(_, said)| *said)
            .collect();

        match open.as_slice() {
            [] => None,
            [one] => Some((*one).to_owned()),
            [rest @ .., last] => Some(format!("{}, and {last}", rest.join(", "))),
        }
    }

    /// Says what a question comes to where nobody is there to answer it, which is what a headless
    /// run's `--on-ask` is; `None` is somebody being there.
    pub fn unanswered(&self, verdict: Option<Verdict>) {
        *self.unanswered.lock() = verdict;
    }

    /// Whether a model sent to `capability` could use it now: a tool on offer declares it, and it
    /// would not be refused - by a rule, or by a question nobody is there to answer.
    ///
    /// note: what every sentence that sends a model to a tool asks first, so that none of them
    /// names one the model does not have, or one it would be refused. A suggestion is read as
    /// something to try: a model sent to a tool that is off, or that a headless run refuses every
    /// call to, makes the call, is refused, and has spent a request on being misdirected; sent to
    /// one it cannot see at all, it names a tool that does not exist.
    ///
    /// note: a policy told nothing about what is offered - one an embedder built without
    /// [`Careful::offers`] - takes everything to be, and goes by its rules alone.
    pub fn reachable(&self, capability: &Capability) -> bool {
        let named = capability.to_string();
        let offered = {
            let offered = self.offered.lock();
            let withdrawn = self.withdrawn.lock();
            offered.is_empty()
                || offered.values().any(|spec| {
                    !withdrawn.contains(&spec.id)
                        && spec.capabilities.iter().any(|it| it.to_string() == named)
                })
        };

        offered && self.permits(capability)
    }

    /// The policy's own half of [`Careful::reachable`]: a call needing `capability` would not be
    /// refused, by a rule or by an unanswered question, for a caller that knows better than this
    /// what is on offer.
    pub fn permits(&self, capability: &Capability) -> bool {
        match self.stance(&Subject::Capability(capability.clone())) {
            Verdict::Allow => true,
            Verdict::Deny => false,
            Verdict::Ask => *self.unanswered.lock() != Some(Verdict::Deny),
        }
    }

    /// Records that a tool came from an MCP server, so calls to it are answerable as that server as
    /// well as by what they do.
    ///
    /// note: told rather than worked out. A server chooses its tools' names and the prefix is
    /// optional, so only whatever installed the tool reliably knows where it came from.
    pub fn came_from(&self, tool: impl Into<String>, server: impl Into<String>) {
        self.servers.lock().insert(tool.into(), server.into());
    }

    /// Which MCP server a tool came from, if it came from one.
    ///
    /// note: the permissions tab needs it: a row about a server has to say which tools it covers,
    /// and a tool's id cannot be asked for a server's name.
    pub fn server_of(&self, tool: &str) -> Option<String> {
        self.servers.lock().get(tool).cloned()
    }
}

#[async_trait]
impl PermissionPolicy for Careful {
    /// The reason this policy wrote down when it refused, handed to the kernel so that it reaches
    /// the model rather than only the screen.
    ///
    /// note: the same sentence both of them read. A model told it was refused by the rule for
    /// `.env*` can do something with that - stop asking for it, ask what to use instead - and
    /// one told only that a call was not permitted cannot tell a standing rule from a bad moment.
    fn why(&self, request: &PermissionRequest) -> Option<String> {
        Careful::why(self, &request.call)
    }

    /// note: a question nobody is here to answer, in a run whose `--on-ask` answers every one
    /// `deny`, is refused here rather than asked. Asked, it was answered by the run and reached the
    /// model as one person's answer to one call, with a different approach maybe allowed - when the
    /// same call, and every call the same rule asks about, would be refused for the rest of the
    /// session. Refused here, it is the standing rule it is, and says which rule would have asked:
    /// under `--no-sandbox`, `net:reach` judged from the command's name. `--on-ask allow` is still
    /// asked and answered, because granting a command the network is more than a verdict.
    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        let mut verdict = self.verdict(request);
        let unasked = verdict == Verdict::Ask && *self.unanswered.lock() == Some(Verdict::Deny);
        if unasked {
            verdict = Verdict::Deny;
        }

        if verdict == Verdict::Deny {
            // which subject did it, so that a refused `shell` in a session where `shell` is
            // allowed can say what actually refused it
            let deciding = match unasked {
                true => Verdict::Ask,
                false => Verdict::Deny,
            };
            let blamed: Vec<String> = self
                .judges(request)
                .iter()
                .filter(|subject| self.stance(subject) == deciding)
                .map(|subject| match subject {
                    Subject::Capability(capability) if *capability == Capability::net("reach") => {
                        "`net:reach`, which this command reaches for".to_owned()
                    }
                    Subject::Path(pattern) => format!("the rule for `{pattern}`"),
                    Subject::Server(name) => format!("the rule for the `{name}` server"),
                    // the actions it is the rule for, which a model would otherwise try one by one
                    Subject::Capability(capability) => match self.needed_by(capability) {
                        Some(actions) => format!("`{subject}`, which {actions} need"),
                        None => format!("`{subject}`"),
                    },
                    subject => format!("`{subject}`"),
                })
                .collect();

            let why = match (blamed.is_empty(), unasked) {
                (true, false) => "the policy refused it".to_owned(),
                (false, false) => format!("refused by {}", blamed.join(" and ")),
                // note: "for as long as it lasts", so the kernel's "a standing rule" after it is
                // true of something. Nothing ruled `deny` here: the rule asks, and this run has
                // nobody to ask, so it is the run's refusal that stands. A model told only that
                // the call was asked about and then that a standing rule refused it could not tell
                // which of the two it was dealing with
                (true, true) => "it would be asked about, and nobody is here to answer, so this \
                                 run refuses it, and every call like it, for as long as it lasts"
                    .to_owned(),
                (false, true) => format!(
                    "it is asked about by {}, and nobody is here to answer, so this run refuses it, \
                     and every other call that rule asks about, for as long as it lasts",
                    blamed.join(" and ")
                ),
            };
            // nobody is obliged to read these; a session that never does should not grow a queue
            let mut refusals = self.refusals.lock();
            match refusals
                .iter_mut()
                .find(|(known, _)| known == &request.call)
            {
                Some(known) => known.1 = why,
                None => {
                    if refusals.len() == REMEMBERED {
                        refusals.pop_front();
                    }
                    refusals.push_back((request.call.clone(), why));
                }
            }
        }

        verdict
    }
}

/// The programs counted as going out to the network.
///
/// note: short on purpose, and made of what a model actually writes. Every entry here is a program
/// whose whole point is the network, so a false positive is nearly impossible; misses are false
/// negatives, and are the subject of the note on [`reaches_the_network`].
const NETWORKED: &[&str] = &[
    "curl",
    "wget",
    "aria2c",
    "http",
    "https",
    "xh",
    "httpie",
    "nc",
    "ncat",
    "netcat",
    "telnet",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "ftp",
    "ping",
    "dig",
    "nslookup",
    "host",
    "whois",
    "git",
    "gh",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "npx",
    "pip",
    "pip3",
    "uv",
    "poetry",
    "gem",
    "go",
    "brew",
    "apt",
    "apt-get",
    "dnf",
    "pacman",
    "apk",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "aws",
    "gcloud",
    "az",
    "terraform",
];

/// Whether a shell command names one of them.
///
/// note: consulted only where the gate does not hold: under `--no-sandbox`, and on a kernel the
/// sandbox does not start on. Where the gate holds, a command is asked about when it tries. See
/// [`Careful::gate_the_network`].
///
/// note: a heuristic over the command as it was written. It catches `curl https://…`,
/// `pip install x` and `git push`, which is what a model writes when it wants the network, and the
/// same behind a program that runs another (`timeout 30 curl …`, `sh -c 'curl …'`; see
/// `WRAPPERS`). It does not catch a script that curls, a binary that opens a socket of its own,
/// or `$(echo cur)l`.
/// It is not a sandbox and this program does not claim it is one - `Capability::exec("run")`
/// subsumes every other capability, and the runtime's own documentation says so.
///
/// note: a model with its `curl` refused reaches for another spelling, such as
/// `python3 -c "import urllib.request; urllib.request.urlopen(...)"`, which this allows. Trying to
/// win that would be an arms race with a model's vocabulary. What this *does* do is make the
/// refusal real and visible for the command that was actually written.
///
/// note: its purpose is that `network` on the permissions tab should mean something. A row that
/// reads `deny` beside a `shell` the model uses for `curl` all day is worse than no row: it reports
/// a restriction that is not there. Several of these - `git`, `cargo`, `go`, `docker` - also do
/// plenty offline, so the answer will sometimes be a question about `git status`, and a policy
/// called `Careful` errs that way on purpose.
pub fn reaches_the_network(cmd: &str) -> bool {
    cmd.split([';', '|', '&', '\n', '(', ')', '`'])
        .filter_map(program)
        .any(|program| NETWORKED.contains(&program))
}

/// Programs that run the program named after them, which is then the one judged.
///
/// note: a model puts these in front of the command it means as a matter of course - a `timeout`
/// so a fetch cannot hang the turn, an `sh -c` round a pipeline - so reading only the first word
/// let `timeout 30 curl …` past a refused network while `curl …` was refused.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "time", "nohup", "timeout", "nice", "ionice", "exec", "command",
    "stdbuf", "setsid", "xargs", "sh", "bash", "dash", "zsh",
];

/// The program one segment of a command runs: its first word that is not an assignment
/// (`FOO=bar curl …`), and after one of [`WRAPPERS`], the first word after it that is not an option
/// or a number (`nice -n 10 curl …`).
fn program(segment: &str) -> Option<&str> {
    let mut wrapped = false;
    for word in segment.split_whitespace() {
        if word.contains('=') {
            continue;
        }
        let word = word.trim_matches(['"', '\'']);
        let word = word.rsplit('/').next().unwrap_or(word);
        if wrapped && word.starts_with(|c: char| c == '-' || c.is_ascii_digit()) {
            continue;
        }
        if WRAPPERS.contains(&word) {
            wrapped = true;
            continue;
        }
        return Some(word);
    }
    None
}
