//! Settings read from a file, for the ones somebody would otherwise type every time.
//!
//! note: JSON, and it is the whole format. A project's settings are a handful of strings, a handful
//! of numbers and a few lists; there is a parser for that in the dependency tree already, every
//! editor knows it, and the alternative is a second grammar to maintain. What it buys over a shell
//! alias is that the file can live next to the thing it describes and be read by somebody who is
//! not running it.
//!
//! note: every field is optional and every one of them is a *default*. The command line is the
//! thing in front of somebody's hands, so it wins, a list included. See `Args::under` in `args.rs`,
//! where the two meet, because the merge has to know which arguments were typed and only clap can
//! say.
//!
//! note: unknown keys are refused rather than skipped. A settings file whose `modle` key does
//! nothing is the failure this format is most likely to have, and serde names the offending field
//! and lists the ones it knows, which is a better error than anything worth writing by hand.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a settings file may say, one field per argument it stands in for.
///
/// note: the names are the arguments' own, with `-` where the argument has one, so that reading a
/// file is reading `--help`. The ones left out are the ones that are not settings: a message, a
/// session to resume and a file to attach belong to an invocation rather than to a project, and
/// `--headless` decides itself from whether stdout is a terminal. `border-color` and `tools` go the
/// other way - settings with no argument - and the notes on them say why.
///
/// note: it serializes as well as deserializes, and every field is written even when it is `null`,
/// so a settings file is something a program can produce rather than only consume. `kamchatka.json`
/// beside this crate is the shipped one, and the suite holds it to having a key for every field
/// here by writing this struct out and comparing the two sets: a starting point missing the setting
/// somebody is looking for is worth less than none, because they stop looking.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct Settings {
    /// The model to talk to.
    pub model: Option<String>,
    /// Which API the requests speak.
    pub api: Option<Api>,
    /// Whether to ask a second model to rate the shell commands a question is about.
    ///
    /// note: not behind the `shell-advisor` feature, for the same reason as `mcp`: one file works
    /// for every build, and a build without it refuses the key rather than ignoring it. See
    /// `Args::under`.
    pub rate_shell_commands: Option<bool>,
    /// A system prompt, pinned, for a session that is starting - a resumed one already has the
    /// one it started with.
    pub system_prompt: Option<String>,
    /// MCP servers to run, as `[name=]command`.
    ///
    /// note: not behind the `mcp` feature: one file works for every build, and a build without it
    /// refuses the key rather than ignoring it. See `Args::under`.
    pub mcp: Option<Vec<String>>,
    /// How many requests one turn may make before it stops; `0` is no limit.
    pub max_requests_per_turn: Option<usize>,
    /// How full the context may get before its oldest exchanges are excluded, and how far down it
    /// is taken once they are: `0.8`, or `[0.8, 0.6]`; `1` never compacts.
    pub compact_context_at: Option<Compact>,
    /// Whether tool calls may run at the same time rather than in the order they were asked for.
    pub parallel_tool_calls: Option<bool>,
    /// Whether this program's own tools are offered with every argument at the top level of a
    /// call, as `--flat-tool-arguments` does, rather than inside one `call` object.
    pub flat_tool_arguments: Option<bool>,
    /// Whether the model's turns go back with their thinking, as `--send-reasoning` does.
    pub send_reasoning: Option<bool>,
    /// Which of this program's tools to offer, by id. Left out, all of them are offered.
    ///
    /// note: a key with no argument behind it, for the reason `border-color` is one: which tools a
    /// project wants its agent to have is settled once and then not thought about again.
    /// `/tools toggle` changes it during a session, and this says where the toggles start.
    ///
    /// note: an empty list offers none of them, leaving only whatever an MCP server brought. A name
    /// that is not a tool is refused at startup, like an unknown key: a settings file asking for
    /// `contxt` and silently getting a session with no context tool is the failure worth naming.
    ///
    /// note: `null` is refused rather than read as "all of them". `[]` means none, and `null` could
    /// be read as none too, and a key that can be read both ways will be misread by somebody. The
    /// shipped file lists every tool instead; all of them is leaving the key out.
    #[serde(default, deserialize_with = "not_null")]
    pub tools: Option<Vec<String>>,
    /// Whether to run with no confinement at all, as `--no-sandbox` does: the shell unconfined,
    /// and `fs` no longer held to the working directory.
    pub no_sandbox: Option<bool>,
    /// Paths outside the working directory the tools may also read and write.
    pub sandbox_write: Option<Vec<PathBuf>>,
    /// Paths outside the working directory the tools may read but not change.
    pub sandbox_read: Option<Vec<PathBuf>>,
    /// The devices under `/dev` the shell may read and write, replacing the usual list.
    pub sandbox_devices: Option<Vec<PathBuf>>,
    /// Capabilities and path rules allowed without asking.
    pub auto_allow: Option<Vec<String>>,
    /// The same, refused without asking.
    pub auto_deny: Option<Vec<String>>,
    /// MCP servers whose tools run without asking, by the name each was given.
    pub auto_allow_servers: Option<Vec<String>>,
    /// The same, refused without asking.
    pub auto_deny_servers: Option<Vec<String>>,
    /// What a question nobody is there to answer gets: `deny` or `allow`.
    pub on_ask: Option<String>,
    /// Seconds after which a headless run stops, however far it has got; `0` never does.
    pub max_run_seconds: Option<u64>,
    /// Seconds a model's answer may say nothing before the request is given up on.
    pub max_silence_seconds: Option<u64>,
    /// Seconds a call to an MCP server's tool may wait for its answer; `0` waits for ever.
    pub mcp_timeout_seconds: Option<u64>,
    /// Tokens the provider may charge for the session before it stops.
    pub max_spend_tokens: Option<u64>,
    /// Whether to drop the whole of a tool's output once it has been shortened.
    pub discard_cut_off_output: Option<bool>,
    /// Whether to send a request that looks too long for the model rather than refusing it here.
    pub send_oversized_requests: Option<bool>,
    /// Whether to leave the session unwritten when it ends.
    pub no_record: Option<bool>,
    /// The colour the window's frame is drawn in, as `#rrggbb`. Left out, it is [`BORDER_COLOR`];
    /// `null`, the terminal's own foreground colour.
    ///
    /// note: a key with no argument behind it, by decision. A colour is picked once to sit beside a
    /// terminal theme and never typed again, so it belongs in a file, not on the command line. A
    /// `--border-color` would also have to be `tui`-gated, and would put a colour in the `--help`
    /// of a program half of whose runs have no screen.
    ///
    /// note: not gated here, for the reason `mcp` is not: one file works for every build. A
    /// headless build reads this and has nothing to draw with it, which costs nothing and grants
    /// nothing, unlike an MCP server it cannot run, which is worth refusing over.
    ///
    /// note: the one key where leaving it out and saying `null` are two different things, because
    /// they are two different colours: the outer `Option` is whether the file said anything, the
    /// inner one what it said.
    #[serde(default, deserialize_with = "stated")]
    pub border_color: Option<Option<String>>,
}

/// Which API a session's requests speak: `--api`, and `api` in a settings file.
///
/// note: one setting with four values rather than a switch per dialect. A session speaks exactly
/// one, and three switches could say two at once, which had to be refused after the fact.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Api {
    /// OpenAI's chat completions, which OpenRouter and most local servers speak.
    #[default]
    ChatCompletions,
    /// OpenAI's Responses API.
    Responses,
    /// Anthropic's Messages API.
    Anthropic,
    /// Google's own `generateContent`.
    Gemini,
}

/// The colour a window's frame is drawn in when nothing says otherwise.
pub const BORDER_COLOR: &str = "#1A936F";

/// How full the context may get before its oldest exchanges go, and how far down it is taken once
/// they start going: `--compact-context-at 0.8` or `--compact-context-at 0.8,0.6`, and `0.8` or
/// `[0.8, 0.6]` in a file.
///
/// note: one setting and not two, because the second number is about the first. As two keys, a
/// settings file that named both - the shipped one does, to say what the target is - once held a
/// `--compact-context-at` typed beside it to a target written for another threshold:
/// `--compact-context-at 0.5` was refused for a target above it, and `--compact-context-at 0.9`
/// aimed at the file's `0.6` rather than the `0.7` it derives on its own. One value is replaced
/// whole, as a list on the command line replaces a file's, so a file that says what the program
/// does unasked changes nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
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
                "`compact-context-at` is a fraction, or two of them: `0.8`, or `[0.8, 0.6]` for \
                 where compaction starts and how far down it takes the context",
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
/// note: only `#rrggbb`. The sixteen colour names are not accepted because the terminal already has
/// them and this setting is for a palette that is not one of them. `#rgb` is not accepted because
/// two spellings of the same colour are two things to document and one more way to typo. The error
/// states the form, not only that the value was wrong, because a settings file has no `--help`
/// beside it.
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
    /// Reads the settings file, or returns an error naming the file and what is wrong.
    ///
    /// note: every error names the path, including parse errors. `serde_json`'s message gives the
    /// line and column but not the file name, and an error about a file named on the command line
    /// must say which file it is about.
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

        // note: the home directory is looked up once, not per path, so expansion depends on the
        // home directory rather than the environment. If there is nothing to expand against, every
        // path is left exactly as written.
        if let Some(home) = home() {
            for paths in [&mut settings.sandbox_write, &mut settings.sandbox_read]
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

/// The settings file this crate ships, built into the binary so `--print-config` can output it.
///
/// note: `cargo install` copies no files, so without this only repository clones, unpacked `.crate`
/// files and release archives would carry the starting point, not the install the readme
/// recommends.
///
/// note: the same bytes as the file, not a copy generated from [`Settings::default`], which would
/// write `null` over every default the file states on purpose. The test suite holds the file to
/// naming every field of the struct, so this is what the repository ships.
pub const SHIPPED: &str = include_str!("../kamchatka.json");

/// The settings file read when the command line names none: the working directory's, then the one
/// under this person's config directory.
///
/// note: the working directory first, because a file next to the thing it describes is the one
/// somebody means. Nothing walks up from there; `.gitignore` does and a settings file should not,
/// because surprise grows with distance and `--config-file` costs one flag.
///
/// note: a file that applies because of where you are standing can surprise you, so what is read is
/// said out loud, into the conversation every projection carries, before anything else happens. A
/// session that picked something up says which file and where from. The working directory's is also
/// asked about first; see [`underfoot`].
///
/// note: `XDG_CONFIG_HOME` and then `~/.config`, where the people who set the variable expect it to
/// be read from.
///
/// note: only where it is an absolute path. An empty or relative one names a directory under
/// wherever the program was started, so the file found there is the repository's and not the
/// person's, and it would be read without the question [`underfoot`] asks. The XDG spec says an
/// empty value is unset and a relative one is ignored.
pub fn found() -> Option<PathBuf> {
    let beside = PathBuf::from(FILE);
    if beside.is_file() {
        return Some(beside);
    }

    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home().map(|home| home.join(".config")))
        .filter(|path| path.is_absolute())?
        .join(env!("CARGO_PKG_NAME"))
        .join(FILE);

    config.is_file().then_some(config)
}

/// Whether this is the working directory's settings file, read only once somebody says it may be.
///
/// note: that one and not the config directory one, because the working directory holds the
/// repository's and the config directory is the person's own. A file there that starts a server is
/// a server the person asked for.
pub fn underfoot(path: &Path) -> bool {
    path == Path::new(FILE)
}

/// The keys a settings file may set that start a program or grant something, by the name the file
/// spells them with.
///
/// note: the list SECURITY.md gives for a file found underfoot. `auto-deny` and `auto-deny-servers`
/// are not on it, because a file that only refuses can only take something away.
pub const GRANTING: [&str; 8] = [
    "mcp",
    "no-sandbox",
    "auto-allow",
    "auto-allow-servers",
    "on-ask",
    "sandbox-write",
    "sandbox-read",
    "sandbox-devices",
];

/// The keys among [`GRANTING`] that these settings give a value this program would not have chosen
/// by itself.
///
/// note: measured against [`SHIPPED`] rather than `null`, because the shipped file is every key at
/// the program's own default and a copy of it is the commonest settings file. Its `sandbox-devices`
/// lists five devices and its `on-ask` says `deny`, and a question naming those would be a question
/// about nothing.
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

/// Asks the person at the terminal whether the settings file found underfoot is read, and answers
/// `true` only for a yes.
///
/// note: what the file grants is named in the question, because a question with nothing in it gets
/// answered by reflex, and the keys are the difference between a file that picks a model and one
/// that starts somebody else's programs. The answer is read as the line it is: anything that is not
/// `y` or `yes`, end of input included, leaves the file unread.
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
/// password database's answer and the one the person running this program is working from may
/// differ, and a path that silently goes somewhere else is worse than one that fails.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Expands a leading `~` into the home directory.
///
/// note: this is the only place in this crate that expands `~`. Every other way of giving these
/// paths has a shell in front of it, which expands `~` before the program sees anything. A settings
/// file has nothing in front of it, so not expanding here would mean `--sandbox-read ~/.rustup`
/// works while the same path in a file silently reaches nothing. The tools still refuse a leading
/// `~`: those paths are written by a model, and expanding one there is how `~/.ssh/id_rsa` becomes
/// a real path on a string it made up. This path is written by the person whose home it is.
///
/// note: `~user` is left alone. Resolving another user's home means asking the password database,
/// and a path that is silently wrong is worse than one that is obviously wrong.
///
/// note: the home is an argument so the test can say what it is instead of depending on the machine
/// running it.
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
    /// note: the message is asserted, not only the failure, because a settings file has no `--help`
    /// beside it: the error is the documentation. An error saying "invalid value" would leave
    /// somebody guessing between `#rgb`, `rgb()`, `yellow` and a bare number.
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

    /// A compaction setting prints as the spelling that reads it back.
    ///
    /// note: the round trip, because one value has three spellings - `0.8` on the command line,
    /// `0.8` or `[0.8, 0.6]` in a file - and this is the spelling a caller holding the value gets.
    /// It also carries the target, which the threshold alone would lose; printed without it, the
    /// session compacts somewhere else the next time it is written down.
    #[test]
    fn a_compaction_setting_prints_the_way_it_is_typed() {
        assert_eq!(Compact::at(0.8).to_string(), "0.8");
        assert_eq!(
            Compact {
                threshold: 0.8,
                target: Some(0.6),
            }
            .to_string(),
            "0.8,0.6"
        );

        for text in ["0.8", "0.8,0.6"] {
            assert_eq!(
                text.parse::<Compact>().expect("a fraction").to_string(),
                text,
                "what is printed has to be what is read"
            );
        }
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
                 "auto-allow": ["exec"], "auto-allow-servers": ["x"], "auto-deny": ["fs"] }"#,
        )
        .expect("a settings file");
        assert_eq!(
            granting(&hostile),
            [
                "mcp",
                "no-sandbox",
                "auto-allow",
                "auto-allow-servers",
                "on-ask"
            ]
        );

        // every name on the list is a key a file can have: one renamed without it would be a key
        // that grants and is never asked about
        let keys = serde_json::to_value(Settings::default()).expect("it writes");
        for key in GRANTING {
            assert!(keys.get(key).is_some(), "`{key}` is not a settings key");
        }
    }

    /// Only a yes reads it, and the question says what it is asking about.
    #[test]
    fn only_a_yes_reads_a_file_underfoot() {
        let ask = |typed: &str| {
            let mut said = Vec::new();
            let yes = trusted(
                Path::new(FILE),
                &["mcp", "auto-allow"],
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
            assert!(said.contains("sets mcp, auto-allow"), "{said}");
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
