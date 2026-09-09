//! `Careful`: what may run, what has to be asked about, and what is refused outright.
//!
//! note: a heuristic over a command line, and honest about it. What it reads is the tool name and
//! the arguments, both as data - nothing a model says about its own permissions reaches here -
//! and what it produces is a stance somebody can read back on the permissions tab and change.
//! `Shell` subsumes every other capability, so the answers about the rest are reports rather than
//! boundaries, which is why the path rules exist and why the tab says as much.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    path::Path,
};

use nachalnik::{
    Capability, PermissionPolicy, PermissionRequest, ToolCallId, Verdict, async_trait,
};
use parking_lot::Mutex;

/// Something [`Careful`] holds an opinion about.
///
/// note: two kinds, because a capability is not fine enough on its own. `read: allow` is a
/// reasonable thing to want and `read .env: allow` is not, and the difference is a property of the
/// *file* rather than of the tool that opened it - which is why a path rule is one subject rather
/// than three, and binds `read`, `write` and `edit` alike.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subject {
    /// A class of side effect a tool declares.
    Capability(Capability),
    /// A pattern the path a tool was handed is matched against.
    Path(String),
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(capability) => write!(f, "{capability}"),
            Self::Path(pattern) => write!(f, "{pattern}"),
        }
    }
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
/// [`Reach::allows`](crate::sandbox::Reach::allows) resolves, and the two used to disagree about
/// the simplest thing there is: `.env/` has no last component when it is split on slashes, so no
/// rule matched it, while resolving it produced `.env` and read it. A trailing slash, a `.` in the
/// middle, a doubled separator - none of them changes which file is meant, and none of them may
/// change which rule applies.
///
/// note: what this cannot see is a *symlink*. A rule is about a name, and a name that resolves
/// somewhere else resolves after this has answered. The boundary that does not care about names
/// is the sandbox, which is the kernel's - see [`crate::sandbox`].
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let path = path.replace('\\', "/");
    let path = Path::new(&path);

    if let Some(directory) = pattern.strip_suffix('/') {
        return path
            .components()
            .any(|component| component.as_os_str() == directory);
    }

    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => glob(pattern, name),
        None => false,
    }
}

/// Whether a name matches a pattern in which `*` stands for any run of characters.
///
/// note: it backtracks, which the first version did not: it walked the pattern's literals with
/// `find` and took the first hit, so `a*bc` refused `abcbc` - the `bc` it found was the one the
/// star should have swallowed, and there was no way back. A permission rule that silently fails to
/// match is the worst way for one to be wrong, and `*credentials*.json` is not an exotic thing to
/// write.
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
/// note: Capabilities are the unit rather than tool names, which is what makes this work for
/// tools this crate has never heard of. An MCP server's tools all carry a `mcp:<server>`
/// capability, so answering "always" to one of them is answering for that server, and only that
/// server.
///
/// note: The state is a map from a capability to the [`Verdict`] this will return for it, rather
/// than a set of the allowed ones and a hard-coded refusal for the network. Same behaviour, but
/// every one of those answers is now a value somebody can look at and change - which is what the
/// permissions tab is drawing. A policy whose decisions can only be observed by triggering them
/// is not much of a demonstration of a replaceable policy.
///
/// note: a capability is not fine enough on its own. `read: allow` is a reasonable thing to want
/// and `read .env: allow` is not, so there is a second kind of [`Subject`]: a pattern the *path* a
/// tool was handed is matched against. The strictest of everything consulted wins, so a rule can
/// only tighten what a capability allows - `read` stays `allow` and `.env` becomes a question.
///
/// note: those rules bind `read`, `write` and `edit`, and deliberately not `shell`. A command
/// names its files inside a string, and `cat .env`, `sed -n 1p .env`, `python -c "open('.env')"`
/// and `base64 <.env` are the same act written four ways: a check over that string would refuse
/// the first and wave the rest through while looking like a rule. What binds a command is the
/// kernel, and what the kernel can express is a directory - see [`crate::sandbox`]. So `cat .env`
/// works where `read .env` asks, and that is the honest shape of it rather than an oversight.
pub struct Careful {
    stances: Mutex<BTreeMap<Capability, Verdict>>,
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
    networked: Mutex<VecDeque<ToolCallId>>,
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

/// How many of each of the two per-call notes above are kept.
///
/// note: a queue rather than a map, and the oldest goes rather than all of them. Both of these
/// used to be cleared outright when they got past thirty-two, which is a bound that throws away
/// the entry it is most likely to need: an answer is written down when the person gives it and
/// read when the call runs, so the live one is among the newest. Sixty-four is past what one turn
/// can produce, and the linear scan over that is nothing beside spawning a process.
const REMEMBERED: usize = 64;

impl Default for Careful {
    fn default() -> Self {
        Self::new()
    }
}

impl Careful {
    /// Builds a policy that allows reads and asks about everything else, including a short list of
    /// paths that look like credentials.
    pub fn new() -> Self {
        Self {
            // note: empty, and `stance` answers `ask` for anything it does not find. Nothing is
            // decided on somebody's behalf - not `read`, which was `allow` here and is the one
            // people would have picked, and not `network`, which was `deny`. Both were decisions
            // taken for the user about things they may perfectly well want, and the sandbox is
            // what makes either answer mean something once they have given it
            stances: Mutex::new(BTreeMap::new()),
            paths: Mutex::new(
                SUSPECT
                    .iter()
                    .map(|pattern| ((*pattern).to_owned(), Verdict::Ask))
                    .collect(),
            ),
            networked: Mutex::new(VecDeque::new()),
            refusals: Mutex::new(VecDeque::new()),
        }
    }

    /// Everything this policy consults about one call, in the order it reads them out.
    ///
    /// note: the declared capabilities, plus the two things only the arguments can say - that a
    /// command reaches for the network, and that a path is one there is a rule about. It is one
    /// list rather than three checks because everything downstream wants the same thing: the
    /// question asks about these, `always` answers for these, and a refusal is blamed on whichever
    /// of these said no. A one-off `yes` to a `curl` that then ran with the network cut is the
    /// shape of bug that comes of having three of them.
    pub fn judges(&self, request: &PermissionRequest) -> Vec<Subject> {
        let mut judged: Vec<Subject> = request
            .capabilities
            .iter()
            .cloned()
            .map(Subject::Capability)
            .collect();

        if request.capabilities.contains(&Capability::Shell)
            && command(&request.args).is_some_and(reaches_the_network)
        {
            judged.push(Subject::Capability(Capability::Network));
        }
        // note: the path a *tool* was handed, which is not the same as a path named inside a shell
        // command; see the note on `Careful` for why the second is not attempted
        if let Some(path) = request.args.get("path").and_then(|path| path.as_str()) {
            judged.extend(
                self.paths
                    .lock()
                    .iter()
                    .filter(|(pattern, _)| path_matches(pattern, path))
                    .map(|(pattern, _)| Subject::Path(pattern.clone())),
            );
        }

        judged
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
    pub fn stance(&self, subject: &Subject) -> Verdict {
        match subject {
            Subject::Capability(capability) => self
                .stances
                .lock()
                .get(capability)
                .copied()
                .unwrap_or(Self::untold()),
            Subject::Path(pattern) => self
                .paths
                .lock()
                .iter()
                .find(|(known, _)| known == pattern)
                .map(|(_, verdict)| *verdict)
                .unwrap_or(Self::untold()),
        }
    }

    /// What it answers about a subject nobody has told it anything about, which is everything
    /// until somebody answers a question.
    ///
    /// note: a function the two arms of [`Careful::stance`] fall back to rather than a
    /// `Verdict::Ask` written into each of them, so that the sentence the permissions tab draws
    /// about this policy is read out of the policy and cannot come to disagree with it. The tab
    /// listed the answers somebody had given and said nothing about what was deciding in the
    /// meantime, which is the first thing a screen of permissions is asked.
    pub const fn untold() -> Verdict {
        Verdict::Ask
    }

    /// Decides what to answer about one subject from now on.
    pub fn set(&self, subject: &Subject, verdict: Verdict) {
        match subject {
            Subject::Capability(capability) => {
                self.stances.lock().insert(capability.clone(), verdict);
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
        let mut networked = self.networked.lock();
        if networked.iter().any(|known| known == call) {
            return;
        }
        if networked.len() == REMEMBERED {
            networked.pop_front();
        }
        networked.push_back(call.clone());
    }

    /// Whether [`Careful::grant_the_network`] was told about this call.
    pub fn was_granted_the_network(&self, call: &ToolCallId) -> bool {
        self.networked.lock().iter().any(|known| known == call)
    }

    /// Why the given call was refused, if this is what refused it.
    ///
    /// note: it used to be taken out rather than copied, on the grounds that the one caller
    /// rendered it and nothing was gained by holding it. There are two callers now and they want
    /// different things with it: the screen tells the person, and the kernel puts it into the
    /// tool result the *model* reads - see [`PermissionPolicy::why`]. Handing it over once meant
    /// whichever asked first got it and the other was told nothing.
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
    pub fn stances(&self) -> Vec<(Capability, Verdict)> {
        self.stances
            .lock()
            .iter()
            .map(|(capability, verdict)| (capability.clone(), *verdict))
            .collect()
    }
}

#[async_trait]
impl PermissionPolicy for Careful {
    /// The reason this policy wrote down when it refused, handed to the kernel so that it reaches
    /// the model rather than only the screen.
    ///
    /// note: the same sentence both of them read. A model told `refused by the rule for
    /// `**/.env`` can do something with that - stop asking for it, ask what to use instead - and
    /// one told only that a call was not permitted cannot tell a standing rule from a bad moment.
    fn why(&self, call: &ToolCallId) -> Option<String> {
        Careful::why(self, call)
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
                    Subject::Capability(Capability::Network) => {
                        "`network`, which this command reaches for".to_owned()
                    }
                    Subject::Path(pattern) => format!("the rule for `{pattern}`"),
                    subject => format!("`{subject}`"),
                })
                .collect();

            let why = match blamed.is_empty() {
                true => "the policy refused it".to_owned(),
                false => format!("refused by {}", blamed.join(" and ")),
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
/// note: a heuristic over the command as it was written, and it is worth being exact about what
/// that is worth. It catches `curl https://…`, `pip install x` and `git push`, which is what a
/// model writes when it wants the network, and it does not catch a script that curls, a binary
/// that opens a socket of its own, or `$(echo cur)l`. It is not a sandbox and this program does
/// not pretend it is one - `Capability::Shell` subsumes every other capability, and the runtime's
/// own documentation says so.
///
/// note: that is not hypothetical. Asked for a URL against a live model with `network` refused,
/// the `curl` was refused - and the next call was
/// `python3 -c "import urllib.request; urllib.request.urlopen(...)"`, which was allowed and
/// fetched it. Nothing here is going to win that argument, and trying to would be an arms race
/// with a model's vocabulary. What this *does* do is make the refusal real and visible for the
/// command that was actually written, which is the difference between a policy and a decoration.
///
/// note: what it *is* for is that `network` on the permissions tab should mean something. A row
/// that reads `deny` beside a `shell` the model uses for `curl` all day is worse than no row: it
/// reports a restriction that is not there. Several of these - `git`, `cargo`, `go`, `docker` -
/// also do plenty offline, so the answer will sometimes be a question about `git status`. Erring
/// that way is the point of a policy called `Careful`.
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
