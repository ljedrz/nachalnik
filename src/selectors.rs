//! A small language for naming context items, behind the `selectors` feature.
//!
//! note: This is not part of the runtime - the kernel takes [`ContextId`]s and knows nothing
//! about selectors. It lives here because every client ends up wanting the same thing: a way to
//! turn `tool:grep:latest` into a list of identifiers, and to show the user what it resolved to
//! before anything happens.
//!
//! ```
//! use nachalnik::{ContextItem, Kernel, Config, ContextState, selectors::Selector};
//!
//! let kernel = Kernel::new(Config::default());
//! kernel.push(ContextItem::file("src/parser.rs", "fn parse() {}"));
//!
//! let selector: Selector = "file:src/parser.rs".parse().unwrap();
//! let hits = selector.matches(&kernel.items());
//! kernel.set_state(hits, ContextState::Excluded, Some("too big".into()));
//! ```

use std::{fmt, str::FromStr, sync::Arc};

use crate::context::{ContextId, ContextItem, ContextKind, ContextState};

/// Which of the matching items a [`Selector::Tool`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    /// All of them.
    All,
    /// The earliest one.
    First,
    /// The most recent one.
    Latest,
}

/// Why a selector could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorError(pub String);

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid selector: {}", self.0)
    }
}

impl std::error::Error for SelectorError {}

/// A way of naming context items so they can be acted upon.
///
/// # Syntax
///
/// ```text
/// 17                     the item with that identifier
/// all                    every item
/// files                  every item from that source; also: tool_results, diagnostics,
///                        selections, memories, instructions, system, user, model, compaction
/// all:tool_results       the same, spelled out
/// source:helix           every item from a source with that name, extensions included
/// kind:assistant_message every item of that kind
/// state:excluded         every item in that state
/// file:src/parser.rs     the file with that path
/// tool:grep              every result produced by the `grep` tool
/// tool:grep:latest       the most recent one; also: tool:grep:first
/// tool_result:1842       the tool result with that identifier
/// label:cargo test       every item with that exact label
/// src/parser.rs          anything else is taken as a label
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum Selector {
    /// A single item, by identifier.
    Id(ContextId),
    /// Every item, whatever its state.
    All,
    /// Every item from a given source.
    Source(String),
    /// Every item of a given kind, named as by [`ContextKind::name`].
    Kind(String),
    /// Every item in a given state.
    State(ContextState),
    /// Every item with exactly this label.
    Label(String),
    /// A file, by path.
    File(String),
    /// The results of a given tool.
    Tool {
        /// The tool's identifier.
        name: String,
        /// Which of its results.
        which: Which,
    },
}

impl Selector {
    /// Parses a selector; see the [type-level docs](Selector) for the syntax.
    pub fn parse(input: &str) -> Result<Self, SelectorError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(SelectorError("empty selector".into()));
        }

        if let Ok(id) = input.parse::<u64>() {
            return Ok(Self::Id(ContextId(id)));
        }
        if input == "all" {
            return Ok(Self::All);
        }

        let Some((prefix, rest)) = input.split_once(':') else {
            return Ok(match source_name(input) {
                Some(source) => Self::Source(source.to_owned()),
                None => Self::Label(input.into()),
            });
        };

        let rest = rest.trim();
        match prefix.trim() {
            "all" | "source" => Ok(Self::Source(source_name(rest).unwrap_or(rest).to_owned())),
            "kind" => {
                if !KINDS.contains(&rest) {
                    return Err(SelectorError(format!(
                        "unknown kind `{rest}`; expected one of {}",
                        KINDS.join(", ")
                    )));
                }
                Ok(Self::Kind(rest.to_owned()))
            }
            "state" => match state_by_name(rest) {
                Some(state) => Ok(Self::State(state)),
                None => Err(SelectorError(format!("unknown state `{rest}`"))),
            },
            "label" => Ok(Self::Label(rest.to_owned())),
            "file" => Ok(Self::File(rest.to_owned())),
            "tool" => {
                let (name, which) = match rest.split_once(':') {
                    Some((name, "latest")) => (name, Which::Latest),
                    Some((name, "first")) => (name, Which::First),
                    Some((_, other)) => {
                        return Err(SelectorError(format!(
                            "unknown tool qualifier `{other}`; expected `first` or `latest`"
                        )));
                    }
                    None => (rest, Which::All),
                };
                Ok(Self::Tool {
                    name: name.trim().to_owned(),
                    which,
                })
            }
            "tool_result" => match rest.parse::<u64>() {
                Ok(id) => Ok(Self::Id(ContextId(id))),
                Err(_) => Err(SelectorError(format!(
                    "`tool_result:` expects an item identifier, got `{rest}`"
                ))),
            },
            // a label is the fallback; paths and commands are allowed to contain colons
            _ => Ok(Self::Label(input.to_owned())),
        }
    }

    /// Resolves the selector against a list of items, in insertion order.
    pub fn matches(&self, items: &[Arc<ContextItem>]) -> Vec<ContextId> {
        let mut hits: Vec<ContextId> = items
            .iter()
            .filter(|item| self.matches_item(item))
            .map(|item| item.id)
            .collect();

        if let Self::Tool { which, .. } = self {
            match which {
                Which::All => {}
                Which::First => hits.truncate(1),
                Which::Latest => {
                    if let Some(last) = hits.pop() {
                        hits.clear();
                        hits.push(last);
                    }
                }
            }
        }

        hits
    }

    /// Returns whether the selector matches a single item.
    pub fn matches_item(&self, item: &ContextItem) -> bool {
        match self {
            Self::Id(id) => item.id == *id,
            Self::All => true,
            Self::Source(source) => item.source == *source,
            Self::Kind(kind) => item.kind.name() == kind,
            Self::State(state) => item.state == *state,
            Self::Label(label) => item.label == *label,
            Self::File(path) => item.source == "file" && item.label == *path,
            Self::Tool { name, .. } => {
                matches!(&item.kind, ContextKind::ToolResult { tool, .. } if tool == name)
            }
        }
    }
}

impl FromStr for Selector {
    type Err = SelectorError;

    fn from_str(s: &str) -> Result<Self, SelectorError> {
        Self::parse(s)
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::All => f.write_str("all"),
            Self::Source(source) => write!(f, "source:{source}"),
            Self::Kind(kind) => write!(f, "kind:{kind}"),
            Self::State(state) => write!(f, "state:{state}"),
            Self::Label(label) => write!(f, "label:{label}"),
            Self::File(path) => write!(f, "file:{path}"),
            Self::Tool { name, which } => match which {
                Which::All => write!(f, "tool:{name}"),
                Which::First => write!(f, "tool:{name}:first"),
                Which::Latest => write!(f, "tool:{name}:latest"),
            },
        }
    }
}

/// The kind names a selector accepts.
const KINDS: [&str; 5] = [
    "system",
    "user_message",
    "assistant_message",
    "tool_result",
    "reference",
];

/// Maps a bare or plural word to the source name the constructors in [`ContextItem`] use.
fn source_name(name: &str) -> Option<&str> {
    let source = match name {
        "user" | "users" => "user",
        "system" => "system",
        "file" | "files" => "file",
        "selection" | "selections" => "selection",
        "diagnostic" | "diagnostics" => "diagnostic",
        "tool_result" | "tool_results" => "tool_result",
        "model" => "model",
        "memory" | "memories" => "memory",
        "instruction" | "instructions" => "instruction",
        "compaction" => "compaction",
        _ => return None,
    };

    Some(source)
}

fn state_by_name(name: &str) -> Option<ContextState> {
    let state = match name {
        "active" => ContextState::Active,
        "excluded" => ContextState::Excluded,
        "pinned" => ContextState::Pinned,
        "archived" => ContextState::Archived,
        "superseded" => ContextState::Superseded,
        _ => return None,
    };

    Some(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Selector {
        Selector::parse(input).unwrap()
    }

    #[test]
    fn parses_identifiers() {
        assert_eq!(parse("17"), Selector::Id(ContextId(17)));
        assert_eq!(parse(" 8 "), Selector::Id(ContextId(8)));
        assert_eq!(parse("tool_result:1842"), Selector::Id(ContextId(1842)));
    }

    #[test]
    fn parses_sources() {
        assert_eq!(parse("all"), Selector::All);
        assert_eq!(parse("files"), Selector::Source("file".into()));
        assert_eq!(
            parse("all:tool_results"),
            Selector::Source("tool_result".into())
        );
        assert_eq!(parse("source:helix"), Selector::Source("helix".into()));
    }

    #[test]
    fn parses_tools() {
        assert_eq!(
            parse("tool:grep"),
            Selector::Tool {
                name: "grep".into(),
                which: Which::All
            }
        );
        assert_eq!(
            parse("tool:grep:latest"),
            Selector::Tool {
                name: "grep".into(),
                which: Which::Latest
            }
        );
        assert!(Selector::parse("tool:grep:oldest").is_err());
    }

    #[test]
    fn falls_back_to_labels() {
        assert_eq!(
            parse("src/parser.rs"),
            Selector::Label("src/parser.rs".into())
        );
        assert_eq!(
            parse("file:src/foo.rs"),
            Selector::File("src/foo.rs".into())
        );
        assert_eq!(
            parse("label:cargo test"),
            Selector::Label("cargo test".into())
        );
        assert_eq!(parse("C:/tmp/x.rs"), Selector::Label("C:/tmp/x.rs".into()));
        assert!(Selector::parse("  ").is_err());
    }

    #[test]
    fn round_trips_through_display() {
        for input in [
            "17",
            "all",
            "source:file",
            "kind:tool_result",
            "state:excluded",
            "label:cargo test",
            "file:src/foo.rs",
            "tool:grep",
            "tool:grep:first",
            "tool:grep:latest",
        ] {
            let selector = parse(input);
            assert_eq!(parse(&selector.to_string()), selector, "{input}");
        }
    }
}
