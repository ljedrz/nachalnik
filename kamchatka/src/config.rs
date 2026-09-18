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
//! `Args::under` in `main.rs`, which is where the two meet, because the merge has to know which
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
/// `--headless` decides itself from whether stdout is a terminal. `border` is the one that goes
/// the other way - a setting with no argument - and the note on it says why.
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
    /// Whether to ask a second model about tool calls the standing rules were going to allow.
    ///
    /// note: not behind the `advise` feature, for the reason `mcp` is not: one file works for
    /// every build of this program, and a build without it refuses the key rather than ignoring
    /// it - see `main.rs`. A setting that silently did nothing is worse here than almost
    /// anywhere else in this file, because what it would silently not be doing is checking
    /// permissions.
    pub advise: Option<bool>,
    /// A system instruction, pinned.
    pub system: Option<String>,
    /// MCP servers to run, as `[name=]command`.
    ///
    /// note: not behind the `mcp` feature, so that one file works for every build of this program.
    /// A build without it refuses the key rather than ignoring it - see `main.rs`.
    pub mcp: Option<Vec<String>>,
    /// How many requests one turn may make before it stops; `0` is no limit.
    pub requests: Option<usize>,
    /// How full the context may get before the oldest tool results are elided; `1` never compacts.
    pub compact: Option<f64>,
    /// Whether tool calls may run at the same time rather than in the order they were asked for.
    pub parallel: Option<bool>,
    /// Which of this program's tools to offer, by id; left out, all of them are.
    ///
    /// note: the second key with no argument behind it, and it is here for the reason `border` is:
    /// which tools a project wants its agent to have is settled once and then not thought about
    /// again. It replaced `--introspect`, which was a flag for four of the six and could only be
    /// answered before the session started; `/tools toggle` answers it at any point, and this
    /// says where the toggles start.
    ///
    /// note: an empty list offers none of them, which is a session with whatever an MCP server
    /// brought and nothing else. A name that is not a tool is refused at startup, like an unknown
    /// key - a settings file asking for `contxt` and quietly getting a session with no context
    /// tool is the failure worth naming.
    pub tools: Option<Vec<String>>,
    /// Whether to run the `shell` tool unconfined.
    pub no_sandbox: Option<bool>,
    /// Paths outside the working directory the tools may also read and write.
    pub sandbox_allow: Option<Vec<PathBuf>>,
    /// Paths outside the working directory the tools may read but not change.
    pub sandbox_read: Option<Vec<PathBuf>>,
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
    /// Seconds after which a headless run stops, however far it has got.
    pub deadline: Option<u64>,
    /// Tokens the provider may charge for the session before it stops.
    pub spend: Option<u64>,
    /// Whether to drop the whole of a tool's output once it has been shortened.
    pub forget_truncated: Option<bool>,
    /// Whether to send a request that looks too long for the model rather than refusing it here.
    pub send_oversized: Option<bool>,
    /// Whether to leave the session unwritten when it ends.
    pub no_record: Option<bool>,
    /// The colour the window's frame is drawn in, as `#rrggbb`.
    ///
    /// note: the one key here with no argument behind it, which is a decision rather than an
    /// oversight. Every other setting stands in for something somebody would otherwise type, and
    /// nobody types a colour twice - it is picked once to sit beside a terminal theme and then
    /// never thought about again, which is exactly the thing a file is for and the command line
    /// is not. A `--border` would also have to be `tui`-gated, and would put a colour in the
    /// `--help` of a program half of whose runs have no screen.
    ///
    /// note: not gated here, for the reason `mcp` is not: one file works for every build. A
    /// headless build reads this and has nothing to draw with it, which costs nothing and grants
    /// nothing - unlike an MCP server it cannot run, which is worth refusing over.
    pub border: Option<String>,
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
        let mut settings: Self =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;

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

/// Where the home directory is, according to the environment and nothing else.
///
/// note: `USERPROFILE` as well, because on Windows that is the variable with the answer in it and
/// `HOME` is usually not set at all - which made a `~` in a settings file there a directory of
/// that name, silently, on the one platform where nothing else in the program would have said so.
/// `HOME` is still asked first: a shell that sets it on Windows - an MSYS one does - is a shell
/// somebody is typing paths into, and that home is the one they mean.
///
/// note: the environment and nothing else, for the same reason `~user` is left alone below: the
/// password database's answer and the one the person running this program is working from are
/// allowed to differ, and a path that quietly goes somewhere else is worse than one that fails.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| match cfg!(windows) {
            true => std::env::var_os("USERPROFILE"),
            false => None,
        })
        .map(PathBuf::from)
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
/// the test below say what it is. A test that took the home from the environment was a test that
/// could only run where the environment has one, and it duly failed on Windows - asserting
/// nothing about this function on the platform where this function was in fact wrong.
///
/// note: `is_separator` rather than a literal `/`, so that `~\.cargo` is expanded on the platform
/// that spells it that way and stays a path called `~\.cargo` on the platform where a backslash
/// is a character a file may be named with.
fn expanded(path: PathBuf, home: &Path) -> PathBuf {
    let Some(rest) = path.to_str().and_then(|text| text.strip_prefix('~')) else {
        return path;
    };
    let mut rest = rest.chars();
    match rest.next() {
        // `~` on its own
        None => home.to_path_buf(),
        // `~/x`, and `~\x` where that is a separator too
        Some(sep) if std::path::is_separator(sep) => home.join(rest.as_str()),
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
        // and however this platform spells the separator, which on Windows is the other one
        let native = format!("~{}.rustup", std::path::MAIN_SEPARATOR);
        assert_eq!(expanded(native.into(), &home), home.join(".rustup"));
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
}
