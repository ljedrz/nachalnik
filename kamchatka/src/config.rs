//! Settings read from a file, for the ones somebody would otherwise type every time.
//!
//! note: JSON, and it is the whole format. A project's settings are a handful of strings, a
//! handful of numbers and a few lists; there is a parser for that in the dependency tree already,
//! every editor knows it, and the alternative is a second grammar for this program to have opinions
//! about. What it buys over a shell alias is that the file can live next to the thing it describes
//! and be read by somebody who is not running it.
//!
//! note: every field is optional and every one of them is a *default*. The command line is the
//! thing in front of somebody's hands, so it wins, and it wins for a list by replacing it rather
//! than adding to it - one rule for every key, which is the only kind anybody can predict. See
//! `Args::under` in `args.rs`, which is where the two meet, because the merge has to know which
//! arguments were actually typed and only clap can say.
//!
//! note: unknown keys are refused rather than skipped. A settings file whose `modle` key does
//! nothing is the failure this format is most likely to have, and serde names the offending field
//! and lists the ones it knows - which is a better error than anything worth writing by hand.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a settings file may say, one field per argument it stands in for.
///
/// note: the names are the arguments' own, with `-` where the argument has one, so that reading a
/// file is reading `--help`. The ones left out are the ones that are not settings: a message, a
/// session to resume and a file to attach belong to an invocation rather than to a project, and
/// `--headless` decides itself from whether stdout is a terminal. `border-color` and `tools` go
/// the other way - settings with no argument - and the notes on them say why.
///
/// note: it serializes as well as deserializes, and every field is written even when it is
/// `null` - which is what makes a settings file something a program can produce rather than only
/// consume. `kamchatka.json` beside this crate is the shipped one, and the suite holds it to
/// having a key for every field here by writing this struct out and comparing the two sets: a
/// starting point missing the setting somebody is looking for is worth less than no starting
/// point, because they stop looking.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Settings {
    /// The model to talk to.
    pub model: Option<String>,
    /// Whether to speak Google's own dialect rather than an OpenAI-compatible one.
    pub gemini: Option<bool>,
    /// Whether to ask a second model to rate the shell commands a question is about.
    ///
    /// note: not behind the `shell-advisor` feature, for the reason `mcp` is not: one file works
    /// for every build of this program, and a build without it refuses the key rather than
    /// ignoring it - see `Args::under`.
    pub advise: Option<bool>,
    /// A system instruction, pinned, for a session that is starting - a resumed one already has
    /// the one it started with.
    pub system: Option<String>,
    /// MCP servers to run, as `[name=]command`.
    ///
    /// note: not behind the `mcp` feature, so that one file works for every build of this program.
    /// A build without it refuses the key rather than ignoring it - see `Args::under`.
    pub mcp: Option<Vec<String>>,
    /// How many requests one turn may make before it stops; `0` is no limit.
    pub requests: Option<usize>,
    /// How full the context may get before its oldest exchanges are excluded, and how far down it
    /// is taken once they are: `0.8`, or `[0.8, 0.6]`; `1` never compacts.
    pub compact: Option<Compact>,
    /// Whether tool calls may run at the same time rather than in the order they were asked for.
    pub parallel: Option<bool>,
    /// Which of this program's tools to offer, by id; left out, all of them are.
    ///
    /// note: a key with no argument behind it, for the reason `border-color` is one: which tools a
    /// project wants its agent to have is settled once and then not thought about again.
    /// `/tools toggle` changes it at any point in a session, and this says where the toggles start.
    ///
    /// note: an empty list offers none of them, which is a session with whatever an MCP server
    /// brought and nothing else. A name that is not a tool is refused at startup, like an unknown
    /// key - a settings file asking for `contxt` and quietly getting a session with no context
    /// tool is the failure worth naming.
    ///
    /// note: `null` is refused rather than read as "all of them". Beside `[]` for none, a `null`
    /// reads as none just as well, and a key that can be read both ways is read the wrong way by
    /// somebody; the shipped file lists every tool instead, and all of them is leaving the key out.
    #[serde(default, deserialize_with = "not_null")]
    pub tools: Option<Vec<String>>,
    /// Whether to run with no confinement at all, as `--no-sandbox` does: the shell unconfined,
    /// and `fs` no longer held to the working directory.
    pub no_sandbox: Option<bool>,
    /// Paths outside the working directory the tools may also read and write.
    pub sandbox_allow: Option<Vec<PathBuf>>,
    /// Paths outside the working directory the tools may read but not change.
    pub sandbox_read: Option<Vec<PathBuf>>,
    /// The devices under `/dev` the shell may read and write, replacing the usual list.
    pub sandbox_device: Option<Vec<PathBuf>>,
    /// Capabilities and path rules to allow before anything runs.
    pub allow: Option<Vec<String>>,
    /// The same, refused.
    pub deny: Option<Vec<String>>,
    /// MCP servers whose tools may run, by the name each was given.
    pub allow_server: Option<Vec<String>>,
    /// The same, refused.
    pub deny_server: Option<Vec<String>>,
    /// What a question nobody is there to answer gets: `deny` or `allow`.
    pub on_ask: Option<String>,
    /// Seconds after which a headless run stops, however far it has got; `0` never does.
    pub deadline: Option<u64>,
    /// Tokens the provider may charge for the session before it stops.
    pub spend: Option<u64>,
    /// Whether to drop the whole of a tool's output once it has been shortened.
    pub forget_truncated: Option<bool>,
    /// Whether to send a request that looks too long for the model rather than refusing it here.
    pub send_oversized: Option<bool>,
    /// Whether to leave the session unwritten when it ends.
    pub no_record: Option<bool>,
    /// The colour the window's frame is drawn in, as `#rrggbb`: left out, [`BORDER_COLOR`]; `null`,
    /// the terminal's own foreground colour.
    ///
    /// note: a key with no argument behind it, which is a decision rather than an oversight. Nearly
    /// every setting stands in for something somebody would otherwise type, and nobody types a
    /// colour twice - it is picked once to sit beside a terminal theme and then never thought about
    /// again, which is the thing a file is for and the command line is not. A `--border-color`
    /// would also have to be `tui`-gated, and would put a colour in the `--help` of a program half of whose
    /// runs have no screen.
    ///
    /// note: not gated here, for the reason `mcp` is not: one file works for every build. A
    /// headless build reads this and has nothing to draw with it, which costs nothing and grants
    /// nothing - unlike an MCP server it cannot run, which is worth refusing over.
    ///
    /// note: the one key where leaving it out and saying `null` are two different things, because
    /// they are two different colours: the outer `Option` is whether the file said anything, the
    /// inner one what it said.
    #[serde(default, deserialize_with = "stated")]
    pub border_color: Option<Option<String>>,
}

/// The colour a window's frame is drawn in when nothing says otherwise.
pub const BORDER_COLOR: &str = "#1A936F";

/// How full the context may get before its oldest exchanges go, and how far down it is taken once
/// they start going: `--compact 0.8` or `--compact 0.8,0.6`, and `0.8` or `[0.8, 0.6]` in a file.
///
/// note: one setting and not two, because the second number is about the first. As two keys, a
/// settings file that named both - the shipped one does, to say what the target is - held a
/// `--compact` typed beside it to a target written for another threshold: `--compact 0.5` was
/// refused for a target above it, and `--compact 0.9` aimed at the file's `0.6` rather than the
/// `0.7` it derives on its own. One value is replaced whole, as a list on the command line replaces
/// a file's, so a file that says what the program does unasked changes nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Compact {
    /// How full the context may get, as a fraction of the limit.
    pub threshold: f64,
    /// How far down it is taken once exchanges start going; `None` is what `Shedder::under`
    /// derives from the threshold.
    pub target: Option<f64>,
}

impl Compact {
    /// The threshold alone, with the target derived from it.
    pub const fn at(threshold: f64) -> Self {
        Self {
            threshold,
            target: None,
        }
    }
}

impl std::str::FromStr for Compact {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let fraction = |part: &str| {
            part.trim().parse::<f64>().map_err(|_| {
                format!(
                    "`{text}` is not a fraction, or two of them: `0.8`, or `0.8,0.6` for where \
                     compaction starts and how far down it takes the context"
                )
            })
        };
        match text.split_once(',') {
            Some((threshold, target)) => Ok(Self {
                threshold: fraction(threshold)?,
                target: Some(fraction(target)?),
            }),
            None => Ok(Self::at(fraction(text)?)),
        }
    }
}

impl std::fmt::Display for Compact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.target {
            Some(target) => write!(f, "{},{target}", self.threshold),
            None => write!(f, "{}", self.threshold),
        }
    }
}

impl Serialize for Compact {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.target {
            Some(target) => [self.threshold, target].serialize(serializer),
            None => self.threshold.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Compact {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            One(f64),
            Two([f64; 2]),
        }
        match Written::deserialize(deserializer).map_err(|_| {
            serde::de::Error::custom(
                "`compact` is a fraction, or two of them: `0.8`, or `[0.8, 0.6]` for where \
                 compaction starts and how far down it takes the context",
            )
        })? {
            Written::One(threshold) => Ok(Self::at(threshold)),
            Written::Two([threshold, target]) => Ok(Self {
                threshold,
                target: Some(target),
            }),
        }
    }
}

/// `tools`, which may be left out but not given as `null`.
///
/// note: the key is named in the refusal, since serde's own position is a line and a column.
fn not_null<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // note: only called for a key that is there, since `default` covers one that is not
    Option::<Vec<String>>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| {
            serde::de::Error::custom(
                "`tools` is a list of this program's tools, `[]` for none of them, and left out \
                 for all of them - it cannot be `null`",
            )
        })
}

/// A key whose `null` is a value of its own, told apart from the key being left out.
fn stated<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    // note: only called for a key that is there, since `default` covers one that is not
    Option::<T>::deserialize(deserializer).map(Some)
}

/// A `#rrggbb` colour, as the three bytes it names.
///
/// note: `#rrggbb` and nothing else. Not the sixteen colour *names*, because the terminal already
/// has those and this setting exists for somebody whose palette is not one of them; not `#rgb`,
/// because supporting two spellings of the same thing is two things to document and one more way
/// to typo. The error says the form rather than only that the value was wrong, since a settings
/// file has no `--help` beside it.
pub fn rgb(hex: &str) -> Result<(u8, u8, u8), String> {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "`{hex}` is not a colour; it should be six hex digits, as in `#7aa2f7`"
        ));
    }

    let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).expect("hex digits");

    Ok((byte(0), byte(2), byte(4)))
}

impl Settings {
    /// Reads one, or says what is wrong with it in a sentence naming the file.
    ///
    /// note: the path is in every error, including the parse ones. A program reading a file
    /// somebody named on the command line has no excuse for an error that could be about any file,
    /// and `serde_json`'s own message carries the line and column but not the name.
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let mut settings: Self = serde_json::from_str(&text).map_err(|e| {
            let e = e.to_string();
            // note: a file holding a list is the commonest near miss - `[]` is what an editor's
            // bracket pair and a hand-written file both produce first - and serde's own answer is
            // about a struct and the number of fields it has. A settings file has no `--help`
            // beside it, so what one is is said here instead of counted at it
            if text.trim_start().starts_with('[') {
                format!(
                    "{}: a settings file is an object of the arguments, one key each - {e}",
                    path.display()
                )
            } else {
                format!("{}: {e}", path.display())
            }
        })?;

        // note: the home directory is looked up once rather than per path, which is also what
        // makes the expansion itself a function of a home rather than of the environment - see
        // below. Nothing to expand against leaves every path exactly as it was written.
        if let Some(home) = home() {
            for paths in [&mut settings.sandbox_allow, &mut settings.sandbox_read]
                .into_iter()
                .flatten()
            {
                for path in paths.iter_mut() {
                    *path = expanded(std::mem::take(path), &home);
                }
            }
        }

        Ok(settings)
    }
}

/// What a settings file is called wherever this program looks for one without being told.
pub const FILE: &str = "kamchatka.json";

/// The settings file this crate ships, built into the binary so that `--print-config` can hand it
/// over.
///
/// note: `cargo install` copies no files. Without this, the starting point would reach whoever
/// cloned the repository, unpacked the `.crate` or downloaded a release archive, and nobody who
/// took the road the readme recommends.
///
/// note: the same bytes the file has rather than a copy written out from [`Settings::default`],
/// which would write `null` over every default the file states on purpose. The suite already holds
/// that file to naming every field of the struct, so this is exactly what the repository ships and
/// stays so.
pub const SHIPPED: &str = include_str!("../kamchatka.json");

/// The settings file to read when the command line named none: the working directory's, then the
/// one under this person's config directory.
///
/// note: the working directory first, because a file that sits next to the thing it describes is
/// the one somebody means. Nothing walks *up* from there - `.gitignore` does and a settings file
/// should not, because the surprise grows with the distance and the cost of typing
/// `--config-file` is one flag.
///
/// note: a file that applies because of where you are standing is a file that can surprise you,
/// and the answer to that is not to hide it: what is read is said out loud, into the conversation
/// every projection carries, before anything else happens. A session that picked something up is
/// a session that says which file and where from. The working directory's is also asked about
/// first; see [`underfoot`].
///
/// note: `XDG_CONFIG_HOME` and then `~/.config`, which is where the people who set the variable
/// expect it to be read from.
pub fn found() -> Option<PathBuf> {
    let beside = PathBuf::from(FILE);
    if beside.is_file() {
        return Some(beside);
    }

    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".config")))?
        .join(env!("CARGO_PKG_NAME"))
        .join(FILE);

    config.is_file().then_some(config)
}

/// Whether this is the settings file in the working directory, which is read only once somebody
/// says it may be.
///
/// note: that one and not the one under the config directory, because what the working
/// directory holds is whoever wrote the repository's, and the config directory is the person's
/// own. A file there that starts a server is a server the person asked for.
pub fn underfoot(path: &Path) -> bool {
    path == Path::new(FILE)
}

/// The keys a settings file may set that start a program or grant something, by the name the file
/// spells them with.
///
/// note: the list SECURITY.md gives for a file found underfoot. `deny` and `deny-server` are not
/// on it, because a file that only refuses is a file that can only take something away.
pub const GRANTING: [&str; 8] = [
    "mcp",
    "no-sandbox",
    "allow",
    "allow-server",
    "on-ask",
    "sandbox-allow",
    "sandbox-read",
    "sandbox-device",
];

/// The keys among [`GRANTING`] that these settings give a value this program would not have
/// chosen by itself.
///
/// note: measured against [`SHIPPED`] rather than against `null`, because the shipped file is
/// every key at the program's own default and a copy of it is the commonest settings file there
/// is. Its `sandbox-device` lists five devices and its `on-ask` says `deny`, and a question
/// naming those would be a question about nothing.
pub fn granting(settings: &Settings) -> Vec<&'static str> {
    let shipped: serde_json::Value =
        serde_json::from_str(SHIPPED).expect("the shipped settings are JSON");
    let Ok(these) = serde_json::to_value(settings) else {
        return GRANTING.to_vec();
    };

    GRANTING
        .into_iter()
        .filter(|key| !these[key].is_null() && these[key] != shipped[key])
        .collect()
}

/// Asks whoever is at the terminal whether the settings file found underfoot is read, and answers
/// `true` only for a yes.
///
/// note: what the file grants is named in the question, because a question with nothing in it
/// gets answered by reflex, and the keys are the difference between a file that picks a model and
/// one that starts somebody else's programs. The answer is read as the line it is: anything that
/// is not `y` or `yes`, the end of the input included, leaves the file unread.
pub fn trusted(
    path: &Path,
    granting: &[&str],
    input: &mut impl std::io::BufRead,
    prose: &mut impl std::io::Write,
) -> std::io::Result<bool> {
    let grants = match granting {
        [] => "starts nothing and grants nothing".to_owned(),
        keys => format!(
            "sets {}, which start programs or grant permissions",
            keys.join(", ")
        ),
    };
    write!(
        prose,
        "· {} is in the directory this was started in, and {grants}. Read it? [y/N] ",
        path.display()
    )?;
    prose.flush()?;

    let mut answer = String::new();
    input.read_line(&mut answer)?;
    let yes = matches!(answer.trim().to_lowercase().as_str(), "y" | "yes");
    if !yes {
        writeln!(
            prose,
            "· not read; `--config-file {}` reads it without asking",
            path.display()
        )?;
    }

    Ok(yes)
}

/// Where the home directory is, according to the environment and nothing else.
///
/// note: the environment and nothing else, for the same reason `~user` is left alone below: the
/// password database's answer and the one the person running this program is working from are
/// allowed to differ, and a path that quietly goes somewhere else is worse than one that fails.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// A leading `~`, made into the home directory it stands for.
///
/// note: this is the one place in this crate that expands one, and the exception is narrower than
/// it looks: every *other* way of giving these paths has a shell in front of it, which expanded
/// `~` before the program saw anything. A settings file has nothing in front of it, so not
/// expanding here would not be one rule applied evenly - it would be `--sandbox-read ~/.rustup`
/// working and the same path in a file silently reaching nothing. The tools refuse a leading `~`
/// rather than expanding it and that stays: those paths are written by a *model*, and expanding
/// one there is how `~/.ssh/id_rsa` becomes a real path on a string it made up. This one is
/// written by the person whose home it is.
///
/// note: `~user` is left alone. Resolving somebody else's home means asking the password database,
/// and a path that is quietly not what it says is worse than one that is obviously wrong.
///
/// note: the home is an argument rather than something this reads for itself, which is what lets
/// the test below say what it is rather than depend on the machine running it.
fn expanded(path: PathBuf, home: &Path) -> PathBuf {
    let Some(rest) = path.to_str().and_then(|text| text.strip_prefix('~')) else {
        return path;
    };
    let mut rest = rest.chars();
    match rest.next() {
        // `~` on its own
        None => home.to_path_buf(),
        // `~/x`
        Some('/') => home.join(rest.as_str()),
        // `~stuff`, `~root/.ssh`: a name that starts with the character, and not a home directory
        Some(_) => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A colour is six hex digits, and the error for anything else says so.
    ///
    /// note: the message is asserted rather than only the failure, because a settings file has no
    /// `--help` beside it: the error *is* the documentation, and one saying "invalid value" would
    /// leave somebody guessing between `#rgb`, `rgb()`, `yellow` and a bare number.
    #[test]
    fn a_border_is_six_hex_digits_and_says_so_when_it_is_not() {
        assert_eq!(rgb("#7aa2f7"), Ok((0x7a, 0xa2, 0xf7)));
        // the `#` is a convention rather than a requirement, and case is not a second spelling
        assert_eq!(rgb("7AA2F7"), Ok((0x7a, 0xa2, 0xf7)));
        assert_eq!(rgb("#000000"), Ok((0, 0, 0)));
        assert_eq!(rgb("#ffffff"), Ok((255, 255, 255)));

        // the near misses, which are what somebody actually types
        for wrong in [
            "#7af", "yellow", "#7aa2f", "#7aa2f77", "", "#nothex", "#7aa2f ",
        ] {
            let said = rgb(wrong).expect_err(&format!("`{wrong}` is not a colour"));
            assert!(said.contains("six hex digits"), "{wrong:?}: {said}");
            assert!(said.contains("#7aa2f7"), "the form is shown: {said}");
        }
    }

    #[test]
    fn a_leading_tilde_is_the_home_directory_and_nothing_else_is() {
        // a home this test says, rather than one the machine running it happens to have
        let home = PathBuf::from("/home/somebody");

        assert_eq!(expanded("~".into(), &home), home);
        assert_eq!(expanded("~/.rustup".into(), &home), home.join(".rustup"));
        // a backslash is a character a name may hold, not a separator
        assert_eq!(
            expanded(r"~\.rustup".into(), &home),
            PathBuf::from(r"~\.rustup")
        );
        // not a prefix, not somebody else's, and not one in the middle
        assert_eq!(expanded("~stuff".into(), &home), PathBuf::from("~stuff"));
        assert_eq!(
            expanded("~root/.ssh".into(), &home),
            PathBuf::from("~root/.ssh")
        );
        assert_eq!(
            expanded("/srv/~/x".into(), &home),
            PathBuf::from("/srv/~/x")
        );
    }

    /// What a file underfoot is asked about is what it grants, and a copy of the shipped file
    /// grants nothing.
    #[test]
    fn a_file_underfoot_is_asked_about_what_it_grants() {
        let shipped: Settings = serde_json::from_str(SHIPPED).expect("the shipped file");
        assert_eq!(granting(&shipped), Vec::<&str>::new());
        assert_eq!(granting(&Settings::default()), Vec::<&str>::new());

        let hostile: Settings = serde_json::from_str(
            r#"{ "model": "m", "mcp": ["x=y"], "no-sandbox": true, "on-ask": "allow",
                 "deny": ["exec"] }"#,
        )
        .expect("a settings file");
        assert_eq!(granting(&hostile), ["mcp", "no-sandbox", "on-ask"]);
    }

    /// Only a yes reads it, and the question says what it is asking about.
    #[test]
    fn only_a_yes_reads_a_file_underfoot() {
        let ask = |typed: &str| {
            let mut said = Vec::new();
            let yes = trusted(
                Path::new(FILE),
                &["mcp", "allow"],
                &mut typed.as_bytes(),
                &mut said,
            )
            .expect("asked");
            (yes, String::from_utf8(said).expect("text"))
        };

        for typed in ["y\n", "yes\n", " Y \n"] {
            let (yes, said) = ask(typed);
            assert!(yes, "{typed:?}: {said}");
            assert!(said.contains("kamchatka.json"), "{said}");
            assert!(said.contains("sets mcp, allow"), "{said}");
            assert!(!said.contains("not read"), "{said}");
        }
        for typed in ["\n", "n\n", "yep\n", ""] {
            let (yes, said) = ask(typed);
            assert!(!yes, "{typed:?}: {said}");
            assert!(
                said.contains("`--config-file kamchatka.json` reads it"),
                "{said}"
            );
        }
    }
}
