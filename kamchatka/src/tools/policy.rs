//! `Careful`: what may run, what has to be asked about, and what is refused outright.
//!
//! note: a heuristic over a command line, and honest about it. What it reads is the tool name and
//! the arguments, both as data - nothing a model says about its own permissions reaches here -
//! and what it produces is a stance somebody can read back on the permissions tab and change.
//! `Shell` subsumes every other capability, so the answers about the rest are reports rather than
//! boundaries, which is why the path rules exist and why the tab says as much.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    path::Path,
};

use nachalnik::{
    Capability, Domain, PermissionPolicy, PermissionRequest, ToolCallId, ToolSpec, Verdict,
    async_trait,
};
use parking_lot::Mutex;

/// Something [`Careful`] holds an opinion about.
///
/// note: four kinds, and they are four different questions about one call. *What* is being done
/// is the operation (`fs:read`) and the domain it is in (`fs`); *to what* is the path; *whose
/// tool* is the server. Only the first two are a hierarchy, and the resolution rule in
/// [`Careful::stance`] is about those two alone - a path and a server are facts the arguments and
/// the registry carry, and they fold in with the strictest-wins rule like anything else.
///
/// note: a capability is not fine enough on its own. `fs:read: allow` is a reasonable thing to
/// want and `fs:read .env: allow` is not, and the difference is a property of the *file* rather
/// than of the tool that opened it - which is why a path rule is one subject rather than one per
/// tool, and binds every tool that is handed a path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
    /// guess because all three are spelled as bare text, and this one is readable in a sentence:
    /// a colon means an operation, and the characters a path has and a name does not mean a path.
    ///
    /// note: a server is not in here, and cannot be: `files` is a server name and `fs` is a
    /// domain, and nothing in either string says which. It is named on its own argument instead -
    /// see `--allow-server` - which is the honest answer to a spelling that cannot be told apart.
    ///
    /// note: it is the inverse of [`fmt::Display`] above for the three it can produce, which is
    /// what makes `--deny "$(read a row off the permissions tab)"` mean what it says.
    pub fn parse(text: &str) -> Self {
        if text.contains('/') || text.contains('*') || text.starts_with('.') {
            return Self::Path(text.to_owned());
        }

        match Capability::parse(text) {
            Ok(capability) => Self::Capability(capability),
            Err(_) => Self::Domain(Domain::from(text)),
        }
    }
}

/// `mcp:call`: the subject every tool from a server declares, meaning "somebody else's tool, and
/// nobody has vouched for what it does".
///
/// note: named once rather than spelled in each of the three places that need it, because the two
/// of them that *report* on it have to agree with the one that decides by it. See
/// [`Careful::judges`], which answers for it with the server's own name where it knows the server,
/// and [`Careful::decides`], which is how a table says so.
fn unvouched() -> Capability {
    Capability::of(Domain::Other("mcp".into()), "call")
}

/// The paths a fresh policy has something to say about.
///
/// note: a short list of the names that are credentials by convention, and every one of them is
/// `ask` rather than `deny`, like everything else here. What they are for is the moment somebody
/// answers `always` for `read`: the capability goes to `allow` and these stay where they are, so
/// reading `src/main.rs` goes silent and reading `.env` is still a question. A rule finer than a
/// capability is only worth having once the capability is open.
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

/// Whether a path is one this pattern is about.
///
/// note: a pattern ending in `/` is a directory: it matches a path with that component anywhere in
/// it. Anything else is matched against the file name, with `*` standing for any run of
/// characters. That is less than a glob crate would give and it is what these rules need; a
/// pattern language nobody can predict is worse on a permissions screen than a small one.
///
/// note: the path is read as a [`Path`] rather than split on `/`, so that the name a rule is
/// matched against is the name the file will actually be opened under. It is the same string
/// [`Reach::allows`](crate::sandbox::Reach::allows) resolves, and the two must not disagree. Split
/// on slashes, `.env/` has no last component and no rule would match it, while resolving it
/// produces `.env` and reads it. A trailing slash, a `.` in the middle, a doubled separator - none
/// of them changes which file is meant, and none of them may change which rule applies.
///
/// note: what this cannot see is a *symlink*. A rule is about a name, and a name that resolves
/// somewhere else resolves after this has answered. The boundary that does not care about names
/// is the sandbox, which is the kernel's - see [`crate::sandbox`].
///
/// note: and a name is compared the way the filesystem here compares it: case-blind on macOS and
/// Windows, and on Windows as the name the file is opened under, without trailing dots or a
/// stream. See `Spelling` for why only there.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    matches_as(pattern, path, Spelling::HERE)
}

/// [`path_matches`], for a filesystem that spells names this way.
fn matches_as(pattern: &str, path: &str, spelling: Spelling) -> bool {
    let path = path.replace('\\', "/");
    let path = Path::new(&path);

    if let Some(directory) = pattern.strip_suffix('/') {
        return path.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| spelling.same(spelling.opened(name), directory))
        });
    }

    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => glob(pattern, spelling.opened(name), spelling),
        None => false,
    }
}

/// How the filesystem a rule is checked on compares names.
///
/// note: a rule is about the file that gets opened, so it has to agree with the filesystem about
/// which names are the same file. On macOS and Windows `.ENV` opens `.env`, and on Windows so do
/// `.env.` and `.env::$DATA` - so a rule compared byte for byte let `fs read .ENV` open the file
/// `.env*` asks about, without asking. Folding only there rather than everywhere, because where
/// names differ in case they are different files, and an `allow` rule folded on Linux would
/// reach files it was not written for. The folding is ASCII: a filesystem folds more than that,
/// and every rule this program ships is ASCII.
#[derive(Debug, Clone, Copy)]
struct Spelling {
    /// Whether two names that differ only in case are one file.
    case_blind: bool,
    /// Whether a name's trailing dots and spaces, and anything from a `:`, are not part of it.
    windows: bool,
}

impl Spelling {
    /// This platform's.
    const HERE: Self = Self {
        case_blind: cfg!(any(target_os = "macos", windows)),
        windows: cfg!(windows),
    };

    /// The name the filesystem opens for this one.
    fn opened(self, name: &str) -> &str {
        match self.windows {
            // a stream of a file is the file, and a trailing dot or space is dropped on the way in
            true => name
                .split(':')
                .next()
                .unwrap_or(name)
                .trim_end_matches(['.', ' ']),
            false => name,
        }
    }

    /// Whether two names are one.
    fn same(self, a: &str, b: &str) -> bool {
        match self.case_blind {
            true => a.eq_ignore_ascii_case(b),
            false => a == b,
        }
    }

    /// Whether two bytes of a name are one.
    fn same_byte(self, a: u8, b: u8) -> bool {
        match self.case_blind {
            true => a.eq_ignore_ascii_case(&b),
            false => a == b,
        }
    }
}

/// What a path rule may be, in the words an error has to say it in.
const GRAMMAR: &str = "a path rule is a file name in which `*` stands for any run of characters - \
                       `*.pem`, `.env*` - or one directory name with a slash after it - \
                       `secrets/` - which is about that directory wherever it sits in a path";

/// What is wrong with a path rule, where something is.
///
/// note: the grammar [`path_matches`] reads is small, and a pattern outside it would be taken all
/// the same: `--allow 'src/**'` would go onto the permissions tab and be consulted about every
/// call, and no path can match it. A rule that cannot match is the worst way for one to be wrong,
/// because a `--deny` that refuses nothing reads as given - so it is refused where it is entered,
/// and the refusal says what there is.
///
/// note: refused rather than taught to the matcher. Whole-path patterns bring anchoring, absolute
/// against relative, `**`, and a separator that means something on one platform - and a pattern
/// language on a permissions screen is worth more small than complete.
///
/// note: a `*` before the slash is refused too, and that one is legal rather than impossible: a
/// directory really can be called `sec*`, and the directory branch compares the name as it is
/// written. `secrets*/` is somebody expecting `secrets-old/` to be covered, which is the same
/// silence by another route.
pub fn objection_to(pattern: &str) -> Option<String> {
    let objection = |why: &str| Some(format!("`{pattern}` {why}; {GRAMMAR}"));

    if pattern.is_empty() {
        return objection("is not a rule at all");
    }
    // a rule without a slash is about the last name in a path, and no path's last name is either
    // of these. With the slash they are directory rules and do match - `../` is every path that
    // climbs out - so it is only the bare two that are refused
    if matches!(pattern, "." | "..") {
        return objection("cannot match: no file is called that");
    }
    if pattern.contains('\\') {
        return objection(
            "cannot match: a path is read with `/` between its names, whatever was typed",
        );
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
fn glob(pattern: &str, name: &str, spelling: Spelling) -> bool {
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
            Some(c) if spelling.same_byte(*c, name[n]) => {
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
/// note: capabilities are the unit rather than tool names, which is what makes this work for
/// tools this crate has never heard of. An MCP server's tools are judged as that server - see
/// [`Careful::judges`] - so answering "always" to one of them is answering for that server, and
/// only that server.
///
/// note: the state is a map from a subject to the [`Verdict`] this will return for it, rather
/// than a set of the allowed ones and a hard-coded refusal for the network, so every one of those
/// answers is a value somebody can look at and change - which is what the permissions tab is
/// drawing. A policy whose decisions can only be observed by triggering them is not much of a
/// demonstration of a replaceable policy.
///
/// note: a domain is not fine enough on its own. `fs:read: allow` is a reasonable thing to want
/// and `fs:read .env: allow` is not, so there is a kind of [`Subject`] finer than either: a
/// pattern the *path* a tool was handed is matched against.
///
/// note: a domain is the whole of what is done in it, and a rule finer than one is about the
/// operation it names. `context` allows every operation in that domain; `context:note` allows a
/// note and says nothing about the rest. Against that, the strictest of everything consulted
/// wins: `fs:read` stays `allow` while `.env` is a question, and a refused domain stays refused
/// however finely an operation in it is named. See [`Careful::judges`].
///
/// note: those rules bind `fs`, and deliberately not `shell`. A command names its files inside a
/// string, and `cat .env`, `sed -n 1p .env`, `python -c "open('.env')"` and `base64 <.env` are the
/// same act written four ways: a check over that string would refuse the first and wave the rest
/// through while looking like a rule. What binds a command is the kernel, and what the kernel can
/// express is a directory - see [`crate::sandbox`]. So `cat .env` works where `read .env` asks,
/// and that is the honest shape of it rather than an oversight.
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
    /// note: for the one refusal a rule is the wrong thing to blame. A call whose operation cannot
    /// be read declares everything its tool does, and a rule about one of those then refuses a
    /// call that meant to do another; saying only the rule sends a model to retry the same call,
    /// when what is wrong is its arguments. Knowing the tool's declaration is how a refusal can
    /// tell - see `ops::unnamed_operation` - and it is told here by whoever installs the tools.
    offered: Mutex<BTreeMap<String, ToolSpec>>,
    /// Why the last few refusals were refused, by the call they refused.
    ///
    /// note: the policy is the only thing that knows this, and nothing carries it out: the
    /// kernel is handed a `Verdict` and records `the call was not permitted`, which is true and
    /// unhelpful when the tool's own capability is `allow` and something else refused it. That is
    /// exactly the `shell: allow` / `network: deny` pair, and a refusal nobody can account for is
    /// the one thing this program is not for. So it is written down here, where it is known, and
    /// [`Careful::why`] hands it out.
    refusals: Mutex<VecDeque<(ToolCallId, String)>>,
}

/// How many refusals are kept for whoever asks why.
///
/// note: a queue rather than a map, and the oldest goes rather than all of them. Emptying it
/// outright when full would throw away the entry it is most likely to need: a refusal is written
/// down when the policy answers and read when the kernel builds the tool result, so the live one
/// is among the newest. What makes a bound the right shape here is that nobody is obliged to read
/// one at all - which is exactly what is not true of the grants above, every one of which is read
/// by the call it was given for.
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
            // something they may perfectly well want, and the sandbox is what makes either answer
            // mean something once they have given it.
            //
            // note: and no operation rules either. Seeding some of `context`'s at `ask`, so that
            // allowing the tool still left an `exclude` a question, would make `--allow context`
            // mean something other than `context`, with no way to guess from the words which ones.
            // A domain is the whole of what is done in it; somebody who wants less than that
            // writes the operation they want.
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
            refusals: Mutex::new(VecDeque::new()),
        }
    }

    /// Everything this policy consults about one call, in the order it reads them out.
    ///
    /// note: the capabilities the call needs - which name the operation it picks, since
    /// [`Tool::needs`](nachalnik::Tool::needs) is asked per call - plus the two things only the
    /// arguments can say: that a command reaches for the network, and that a path is one there is
    /// a rule about. It is one list rather than separate checks because everything downstream
    /// wants the same thing: the question asks about these, `always` answers for these, and a
    /// refusal is blamed on whichever of these said no. A one-off `yes` to a `curl` that then runs
    /// with the network cut is the shape of bug that comes of having several of them.
    pub fn judges(&self, request: &PermissionRequest) -> Vec<Subject> {
        // note: the arguments as the tool will read them. This program's own tools take theirs
        // inside a `call` object, and reading the outside of that finds neither the `cmd` a
        // network rule is about nor the `path` a path rule is about - so both would quietly stop
        // being consulted, which is the one failure a permission policy does not get to have.
        //
        // note: a `call` object is read through whoever's tool it belongs to. Somebody else's
        // tool may take an argument called `call` and mean something else by it, and its `path`
        // is then read as a path. That is the direction to be wrong in: a rule consulted about a
        // string that is not a path asks a question nobody needed, where skipping it is a rule
        // that stops being one. `inner` only reads through a `call` that is the whole of the
        // arguments, so nothing on the outside of one is passed over for it
        let args =
            super::ops::inner(&request.args).unwrap_or(std::borrow::Cow::Borrowed(&request.args));

        let mut judged: Vec<Subject> = request
            .capabilities
            .iter()
            .cloned()
            .map(Subject::Capability)
            .collect();

        if request.capabilities.contains(&Capability::exec("run"))
            && command(&args).is_some_and(reaches_the_network)
        {
            judged.push(Subject::Capability(Capability::net("reach")));
        }
        // note: the path a *tool* was handed, which is not the same as a path named inside a shell
        // command; see the note on `Careful` for why the second is not attempted
        if let Some(path) = args.get("path").and_then(|path| path.as_str()) {
            judged.extend(
                self.paths
                    .lock()
                    .iter()
                    .filter(|(pattern, _)| path_matches(pattern, path))
                    .map(|(pattern, _)| Subject::Path(pattern.clone())),
            );
        }

        // note: where the tool came from, which is the one thing about a call that is neither an
        // act nor an argument. A tool from an MCP server is answerable as that server whatever it
        // claims to do, and it is a fact this program holds because it spawned the server - not
        // one read off a name, which a server can choose.
        //
        // note: and it answers for `mcp:call`, which is the subject every tool from a server
        // declares and means "somebody else's tool, and nobody has vouched for what it does".
        // Naming the server is the same statement made precisely, so consulting both would make
        // `--allow-server files` grant nothing until `mcp:call` had been answered too - two flags
        // for one decision. Whatever the server's *annotations* claimed is untouched: a tool that
        // says it writes is still judged against `fs:write`.
        //
        // note: except where somebody refused it. The server stands in for `mcp:call`'s question,
        // not for a `--deny mcp:call` or a `--deny mcp`, and dropping one of those would let
        // `--allow-server` talk its way past a refusal - which no rule here may do
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

    /// Whether a rule about `subject` has anything to say about a call to `tool`, asked of the
    /// tool rather than of a call it has not made yet.
    ///
    /// note: for the two tables that say what a rule covers - the permissions tab and
    /// `setup: permissions` - which would otherwise answer from
    /// [`ToolSpec::capabilities`](nachalnik::ToolSpec) while [`Careful::judges`] answers from
    /// something narrower. `mcp:call` is the case: every tool from a server declares it, and
    /// `judges` takes it back out again for a server this program spawned, so a table read off the
    /// declarations would list a server's tools beside an `allow` for `mcp:call` that is never
    /// consulted about them. A coverage column that names tools a rule will not be consulted about
    /// is worse than an empty one: it is the answer somebody checks their own flags against. A
    /// `deny` for it is consulted, and covers them.
    ///
    /// note: it takes a name and not a [`PermissionRequest`], so a path rule - which is about the
    /// argument of one call rather than about the tool - is not something it can answer. Those
    /// two tables have their own section for path rules, which is the honest place for a subject
    /// whose coverage is not a property of any tool.
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
    /// second hand-written copy of the fold is a second place to get it wrong.
    pub fn verdict(&self, request: &PermissionRequest) -> Verdict {
        // the strictest answer among the subjects wins, so a call that needs both an allowed one
        // and an unmentioned one is still a question. `Verdict::strictest` is the runtime's own
        // fold for exactly this. A call that needs nothing is allowed: the empty fold
        self.judges(request)
            .iter()
            .map(|subject| self.stance(subject))
            .fold(Verdict::Allow, Verdict::strictest)
    }

    /// What it answers about one subject; asking is what it does about anything unmentioned.
    ///
    /// note: **the most specific rule that has an answer decides, and a `deny` above it
    /// overrules.** `--allow fs` covers `fs:read` because nothing finer was said;
    /// `--allow fs --deny fs:edit` refuses the edit and allows the rest; `--allow fs:read` allows
    /// reading while `fs` is still a question, which is what naming one operation plainly means.
    /// What it cannot do is talk its way past a refusal - a domain somebody denied stays denied
    /// however finely an operation of it is named - so `--deny` is still the last word and the
    /// strictest of everything consulted still wins.
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

    /// What it answers about a subject nobody has told it anything about, which is everything
    /// until somebody answers a question.
    ///
    /// note: a function every arm of [`Careful::stance`] falls back to rather than a `Verdict::Ask`
    /// written into each of them, so that the sentence the permissions tab draws
    /// about this policy is read out of the policy and cannot come to disagree with it. A tab that
    /// listed only the answers somebody had given would say nothing about what decides in the
    /// meantime, which is the first thing a screen of permissions is asked.
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
    /// note: copied rather than taken out, because there are two callers and they want different
    /// things with it: the screen tells the person, and the kernel puts it into the tool result
    /// the *model* reads - see [`PermissionPolicy::why`]. Handing it over once would give it to
    /// whichever asked first and tell the other nothing.
    pub fn why(&self, call: &ToolCallId) -> Option<String> {
        self.refusals
            .lock()
            .iter()
            .find(|(known, _)| known == call)
            .map(|(_, why)| why.clone())
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
    /// operation is refused saying so.
    pub fn offers(&self, spec: ToolSpec) {
        self.offered.lock().insert(spec.id.clone(), spec);
    }

    /// Records that a tool came from an MCP server, so that calls to it are answerable as that
    /// server as well as by what they do.
    ///
    /// note: told rather than worked out. A server chooses the names its tools carry and the
    /// prefix is optional, so the only thing that reliably knows where a tool came from is
    /// whatever installed it.
    pub fn came_from(&self, tool: impl Into<String>, server: impl Into<String>) {
        self.servers.lock().insert(tool.into(), server.into());
    }

    /// Which MCP server a tool came from, if it came from one.
    ///
    /// note: the inverse of what [`Careful::servers`] answers, and the half the permissions tab
    /// needs: a row about a server has to say which tools it covers, and a server's name is not
    /// something a tool's id can be asked for.
    pub fn server_of(&self, tool: &str) -> Option<String> {
        self.servers.lock().get(tool).cloned()
    }

    /// Every MCP server whose tools are installed, and what this answers about each.
    pub fn servers(&self) -> Vec<(String, Verdict)> {
        let mut names: Vec<String> = self.servers.lock().values().cloned().collect();
        names.sort_unstable();
        names.dedup();

        names
            .into_iter()
            .map(|name| {
                let verdict = self.stance(&Subject::Server(name.clone()));
                (name, verdict)
            })
            .collect()
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

    async fn evaluate(&self, request: &PermissionRequest) -> Verdict {
        let verdict = self.verdict(request);

        if verdict == Verdict::Deny {
            // which subject did it, so that a refused `shell` in a session where `shell` is
            // allowed can say what actually refused it
            let blamed: Vec<String> = self
                .judges(request)
                .iter()
                .filter(|subject| self.stance(subject) == Verdict::Deny)
                .map(|subject| match subject {
                    Subject::Capability(capability) if *capability == Capability::net("reach") => {
                        "`net:reach`, which this command reaches for".to_owned()
                    }
                    Subject::Path(pattern) => format!("the rule for `{pattern}`"),
                    Subject::Server(name) => format!("the rule for the `{name}` server"),
                    subject => format!("`{subject}`"),
                })
                .collect();

            let why = match blamed.is_empty() {
                true => "the policy refused it".to_owned(),
                false => format!("refused by {}", blamed.join(" and ")),
            };
            // the arguments first, where they are why the rule applied at all
            let widened = self
                .offered
                .lock()
                .get(&request.tool)
                .and_then(|spec| super::ops::unnamed_operation(spec, request));
            let why = match widened {
                Some(widened) => format!("{widened}; {why}"),
                None => why,
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

/// The command a `shell` call was asked to run, if it was asked to run one.
fn command(args: &serde_json::Value) -> Option<&str> {
    args.get("cmd")?.as_str()
}

/// The programs this counts as going out to the network.
///
/// note: short on purpose, and made of what a model actually writes. Every entry here is a
/// program whose *whole point* is the network, so a false positive is nearly impossible; the
/// misses are the other way round, and are the subject of the note on
/// [`reaches_the_network`].
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
/// note: a heuristic over the command as it was written. It catches `curl https://…`,
/// `pip install x` and `git push`, which is what a model writes when it wants the network, and it
/// does not catch a script that curls, a binary that opens a socket of its own, or
/// `$(echo cur)l`. It is not a sandbox and this program does not pretend it is one -
/// `Capability::exec("run")` subsumes every other capability, and the runtime's own documentation
/// says so.
///
/// note: a model with its `curl` refused reaches for another spelling, such as
/// `python3 -c "import urllib.request; urllib.request.urlopen(...)"`, which this allows. Nothing
/// here is going to win that argument, and trying to would be an arms race with a model's
/// vocabulary. What this *does* do is make the refusal real and visible for the command that was
/// actually written.
///
/// note: what it *is* for is that `network` on the permissions tab should mean something. A row
/// that reads `deny` beside a `shell` the model uses for `curl` all day is worse than no row: it
/// reports a restriction that is not there. Several of these - `git`, `cargo`, `go`, `docker` -
/// also do plenty offline, so the answer will sometimes be a question about `git status`, and a
/// policy called `Careful` errs that way on purpose.
pub fn reaches_the_network(cmd: &str) -> bool {
    cmd.split([';', '|', '&', '\n', '(', ')', '`'])
        .filter_map(|segment| {
            // `FOO=bar curl …`: the assignments come first, and none of them is the program
            segment.split_whitespace().find(|word| !word.contains('='))
        })
        .any(|program| {
            let program = program.trim_matches(['"', '\'']);
            let program = program.rsplit('/').next().unwrap_or(program);

            NETWORKED.contains(&program)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIX: Spelling = Spelling {
        case_blind: false,
        windows: false,
    };
    const MACOS: Spelling = Spelling {
        case_blind: true,
        windows: false,
    };
    const WINDOWS: Spelling = Spelling {
        case_blind: true,
        windows: true,
    };

    /// A rule catches every spelling that opens the file it is about, where the filesystem makes
    /// them one - and no more than that where it does not.
    ///
    /// note: compared byte for byte, `.ENV` got past `.env*` on macOS and Windows, where it opens
    /// `.env`, and on Windows so did `key.pem.` and `key.pem::$DATA`. Each spelling is tried on
    /// all three, since the platform a test runs on is one of them.
    #[test]
    fn a_rule_catches_every_name_that_opens_its_file() {
        for (pattern, path, unix, macos, windows) in [
            (".env*", ".ENV", false, true, true),
            ("*.pem", "keys/Key.PEM", false, true, true),
            ("id_rsa*", "ID_RSA", false, true, true),
            ("secrets/", "Secrets/x", false, true, true),
            ("*.pem", "key.pem.", false, false, true),
            ("*.pem", "key.pem::$DATA", false, false, true),
            ("secrets/", "secrets. /x", false, false, true),
            (".env*", ".env", true, true, true),
            ("*.pem", "key.txt", false, false, false),
        ] {
            assert_eq!(
                matches_as(pattern, path, UNIX),
                unix,
                "{pattern} {path} on unix"
            );
            assert_eq!(
                matches_as(pattern, path, MACOS),
                macos,
                "{pattern} {path} on macos"
            );
            assert_eq!(
                matches_as(pattern, path, WINDOWS),
                windows,
                "{pattern} {path} on windows"
            );
        }
    }
}
