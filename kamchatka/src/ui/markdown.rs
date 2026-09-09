//! What a model wrote, turned into styled lines: markdown, fenced code, and the chunking that
//! tells one from the other.
//!
//! note: a model's answer is markdown whether or not anybody asked for it, so the choice is
//! between rendering it and showing the punctuation. What is rendered is deliberately narrow -
//! emphasis, headings, rules, lists, code - and a fenced block is handed to the highlighter
//! rather than to the markdown renderer, which is why the two live in one file. Tables are the
//! third case and are big enough to have their own: see [`super::table`].

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use crate::ui::table::is_delimiter;

use super::faint;

/// Puts a blank line in, unless there is one there already or there is nothing to separate from.
pub(super) fn separate(lines: &mut Vec<Line<'static>>) {
    if lines.last().is_some_and(|line| !line.spans.is_empty()) {
        lines.push(Line::default());
    }
}

/// What a fence's info string means to the highlighter, which thinks in file extensions.
///
/// note: models write `rust` and `python` far more often than `rs` and `py`, and a block that
/// silently came out uncoloured because of that would look like the highlighter had failed. The
/// list is short on purpose: anything not here is passed through as-is, which already covers `rs`,
/// `go`, `sql`, `toml` and the rest.
fn extension(language: &str) -> &str {
    match language.to_ascii_lowercase().as_str() {
        "rust" => "rs",
        "python" => "py",
        "javascript" | "node" => "js",
        "typescript" => "ts",
        "shell" | "console" | "zsh" | "fish" => "sh",
        "yaml" => "yml",
        "c++" => "cpp",
        "c#" | "csharp" => "cs",
        "golang" => "go",
        "rb" | "ruby" => "rb",
        "makefile" | "make" => "sh",
        _ => language,
    }
}

/// The colour a token of that name is drawn in.
///
/// note: by name rather than by theme, which is the whole reason the highlighting is done here
/// instead of by the markdown renderer. A theme is a set of 24-bit colours chosen against a known
/// background, and this program does not know the background - the same argument that put a rule
/// down the left of a code block instead of a slab behind it. Named colours are the terminal's
/// own, so they are legible in whatever the user has set up.
fn token(name: &str) -> Style {
    let colour = match name {
        "comment" => Color::Gray,
        "string" | "character" => Color::Green,
        "keyword" | "kw" => Color::Magenta,
        "digit" | "boolean" => Color::Yellow,
        "function" | "macro" | "tag" => Color::Blue,
        "struct" | "namespace" | "type" | "attribute" | "key" => Color::Cyan,
        "operator" | "reference" => Color::Reset,
        // markdown, diffs and the rest of what synoptic knows about; whatever is left is code
        "heading" | "header" | "bold" => return Style::default().bold(),
        "italic" | "quote" => return Style::default().italic(),
        "insertion" => Color::Green,
        "deletion" => Color::Red,
        "link" | "list" => Color::Cyan,
        _ => Color::Reset,
    };

    Style::default().fg(colour)
}

/// Draws a fenced block: a rule down the left, and its tokens in this program's own colours.
///
/// note: the block is handed to the highlighter whole, so that a string or a comment running
/// across lines is one token rather than several guesses. A language nothing here recognises still
/// gets the rule and the wrapping - it is a code block whether or not anybody can colour it.
pub(super) fn highlighted(language: &str, body: &str, width: usize) -> Vec<Line<'static>> {
    let bar = Span::styled("│ ", faint());
    let room = width.saturating_sub(2).max(8);
    let source: Vec<String> = body.lines().map(str::to_owned).collect();

    let mut highlighter = synoptic::from_extension(extension(language), 4);
    if let Some(highlighter) = &mut highlighter {
        highlighter.run(&source);
    }

    let mut drawn = Vec::new();
    for (y, line) in source.iter().enumerate() {
        let spans: Vec<(String, Style)> = match &highlighter {
            Some(highlighter) => highlighter
                .line(y, line)
                .into_iter()
                .map(|piece| match piece {
                    synoptic::TokOpt::Some(text, name) => (text, token(&name)),
                    synoptic::TokOpt::None(text) => (text, Style::default()),
                })
                .collect(),
            None => vec![(line.clone(), Style::default().fg(Color::Cyan))],
        };

        for row in fit(spans, room) {
            let mut cells = vec![bar.clone()];
            cells.extend(row);
            drawn.push(Line::from(cells));
        }
    }

    drawn
}

/// Breaks a run of styled pieces into rows no wider than `room`, keeping every style.
///
/// note: code is not prose and is not wrapped like it. A line that is too long is cut where the
/// room runs out rather than at a word boundary, because the alternative - reflowing - would be
/// showing something the model did not write.
fn fit(spans: Vec<(String, Style)>, room: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows = vec![Vec::new()];
    let mut used = 0;

    for (text, style) in spans {
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let left = room - used;
            let taken: String = rest.chars().take(left).collect();
            let bytes = taken.len();
            if !taken.is_empty() {
                used += taken.chars().count();
                rows.last_mut()
                    .expect("there is always a row")
                    .push(Span::styled(taken, style));
            }
            rest = &rest[bytes..];
            if used >= room && !rest.is_empty() {
                rows.push(Vec::new());
                used = 0;
            }
        }
    }

    // a block ending in a newline gives a last row with nothing in it, which is a blank line the
    // model did not write
    if rows.last().is_some_and(Vec::is_empty) && rows.len() > 1 {
        rows.pop();
    }

    rows
}

/// One stretch of a model's answer: either prose, or a fenced block with the language it claimed.
#[derive(Debug, PartialEq)]
pub(super) enum Chunk<'a> {
    /// Everything that is not a fenced block, markdown and all.
    Prose(&'a str),
    /// A pipe table, with its delimiter row.
    Table(&'a str),
    /// A fenced block, without its fences.
    Code {
        /// The info string the fence carried, e.g. `rust`; empty if it carried none.
        language: &'a str,
        /// What is between the fences.
        body: &'a str,
    },
}

/// Splits an answer at its fences.
///
/// note: done here rather than left to the markdown renderer, which is otherwise perfectly able
/// to handle a code block, because colouring one needs three things the renderer does not hand
/// back: the language the fence claimed, the whole block at once - a string or a comment can run
/// across lines, and a highlighter shown one line at a time gets those wrong - and the fact that
/// it *is* a block, which is what earns it the rule down its left.
///
/// note: an unterminated fence runs to the end of the answer rather than being read as prose,
/// because every code block is unterminated for as long as it is still arriving.
pub(super) fn chunks(text: &str) -> Vec<Chunk<'_>> {
    /// The fence a line opens or closes with: its character and how many of them.
    fn fence(line: &str) -> Option<(char, usize)> {
        // up to three spaces of indent, per CommonMark; four would make it an indented block
        let trimmed = line.trim_start_matches(' ');
        if line.len() - trimmed.len() > 3 {
            return None;
        }
        let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
        let run = trimmed.chars().take_while(|c| *c == marker).count();

        (run >= 3).then_some((marker, run))
    }

    let mut chunks = Vec::new();
    let mut prose = 0;
    let mut open: Option<(char, usize, usize)> = None;
    let mut at = 0;
    // where the row above the one being read starts, so that a delimiter row can hand back the
    // header it belongs to; a table is only a table because of the line *after* its first
    let mut previous: Option<usize> = None;
    let mut table: Option<usize> = None;

    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let bare = line.trim_end_matches(['\n', '\r']);
        let was = previous.replace(start);

        match open {
            None => {
                // a table runs until a line with no pipe in it, which is every way one can end:
                // a blank line, a heading, a paragraph
                if let Some(head) = table {
                    if bare.contains('|') {
                        continue;
                    }
                    chunks.push(Chunk::Table(&text[head..start]));
                    (table, prose) = (None, start);
                } else if is_delimiter(bare)
                    && let Some(head) = was
                    && text[head..start].contains('|')
                {
                    if head > prose {
                        chunks.push(Chunk::Prose(&text[prose..head]));
                    }
                    table = Some(head);
                    continue;
                }

                let Some((marker, run)) = fence(bare) else {
                    continue;
                };
                if start > prose {
                    chunks.push(Chunk::Prose(&text[prose..start]));
                }
                open = Some((marker, run, at));
            }
            // a closing fence is the same character, at least as long, and says nothing else
            Some((marker, run, body)) => {
                let closes = fence(bare).is_some_and(|(c, n)| c == marker && n >= run)
                    && bare.trim().chars().all(|c| c == marker);
                if !closes {
                    continue;
                }
                let language = text[start_of_line(text, body)..body].trim();
                let language = language.trim_start_matches([marker]).trim();
                chunks.push(Chunk::Code {
                    language,
                    body: &text[body..start],
                });
                open = None;
                prose = at;
            }
        }
    }

    match open {
        // still arriving: what there is of it is a block, not prose
        Some((marker, _, body)) => {
            let language = text[start_of_line(text, body)..body].trim();
            chunks.push(Chunk::Code {
                language: language.trim_start_matches([marker]).trim(),
                body: &text[body..],
            });
        }
        // a table that runs to the end of what has arrived: still a table
        None if table.is_some() => chunks.push(Chunk::Table(&text[table.unwrap_or(prose)..])),
        None if text.len() > prose => chunks.push(Chunk::Prose(&text[prose..])),
        None => {}
    }

    chunks
}

/// Where the line ending at `end` began.
fn start_of_line(text: &str, end: usize) -> usize {
    text[..end]
        .trim_end_matches('\n')
        .rfind('\n')
        .map(|at| at + 1)
        .unwrap_or(0)
}

/// How markdown is dressed for a terminal that does not know what colour the background is.
///
/// note: The defaults put a cyan slab behind an H1 and a black one behind every code block, which
/// looks like a redaction on a dark theme and a bruise on a light one. Weight and colour say the
/// same thing and survive both. The `#` and the ``` are dropped: they are the punctuation of the
/// format, and what is wanted is what they mean.
#[derive(Clone)]
pub(super) struct Markdown;

impl tui_markdown::StyleSheet for Markdown {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::default().fg(Color::Yellow).bold().underlined(),
            2 => Style::default().fg(Color::Yellow).bold(),
            _ => Style::default().bold(),
        }
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn code(&self) -> Style {
        Style::default().fg(Color::Cyan)
    }

    fn code_block_fence(&self) -> &str {
        ""
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Gray).italic()
    }

    fn link(&self) -> Style {
        Style::default().fg(Color::Blue).underlined()
    }
}

/// The options every model answer is rendered with.
pub(super) fn markdown() -> tui_markdown::Options<Markdown> {
    tui_markdown::Options::new(Markdown)
}

/// Whether a line is a horizontal rule, which markdown spells in punctuation.
///
/// note: Only outside a fenced block, where `---` is three characters a tool printed rather than
/// a divider a model asked for; the caller has already sent those the other way.
pub(super) fn rule(line: &Line<'_>) -> bool {
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    let text = text.trim();

    text.len() >= 3
        && (text.chars().all(|c| c == '-')
            || text.chars().all(|c| c == '*')
            || text.chars().all(|c| c == '_'))
}
