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
/// `--headless` decides itself from whether stdout is a terminal.
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
    /// Whether to offer the two tools an agent reads and manages its own context with.
    pub introspect: Option<bool>,
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
    /// What a question nobody is there to answer gets: `deny` or `allow`.
    pub on_ask: Option<String>,
    /// Seconds after which a headless run stops, however far it has got.
    pub deadline: Option<u64>,
    /// Tokens the provider may charge for the session before it stops.
    pub spend: Option<u64>,
    /// Whether to drop the whole of a tool's output once it has been shortened.
    pub forget_truncated: Option<bool>,
    /// Whether to leave the session unwritten when it ends.
    pub no_record: Option<bool>,
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

        for paths in [&mut settings.sandbox_allow, &mut settings.sandbox_read]
            .into_iter()
            .flatten()
        {
            for path in paths.iter_mut() {
                *path = expanded(std::mem::take(path));
            }
        }

        Ok(settings)
    }
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
fn expanded(path: PathBuf) -> PathBuf {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return path;
    };
    match path.to_str() {
        Some("~") => home,
        Some(text) => match text.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => path,
        },
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_tilde_is_the_home_directory_and_nothing_else_is() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("a home directory"));

        assert_eq!(expanded("~".into()), home);
        assert_eq!(expanded("~/.rustup".into()), home.join(".rustup"));
        // not a prefix, not somebody else's, and not one in the middle
        assert_eq!(expanded("~stuff".into()), PathBuf::from("~stuff"));
        assert_eq!(expanded("~root/.ssh".into()), PathBuf::from("~root/.ssh"));
        assert_eq!(expanded("/srv/~/x".into()), PathBuf::from("/srv/~/x"));
    }
}
