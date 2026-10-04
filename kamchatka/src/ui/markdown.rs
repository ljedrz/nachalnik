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
use crate::tools::joints;
use crate::ui::text::{columns, prefix_within, refit};

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
/// note: by name rather than by theme, which is why the highlighting is done here instead of by the
/// markdown renderer. A theme is a set of 24-bit colours chosen against a known background, and
/// this program does not know the background - the same argument that put a rule down the left of a
/// code block instead of a slab behind it. Named colours are the terminal's own, so they are
/// legible in whatever the user has set up.
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

/// How many spaces the highlighter draws a tab as.
const TAB: usize = 4;

/// Each line of `text`, in pieces coloured the way [`token`] colours them - or all of it in cyan,
/// where nothing recognises `extension`.
///
/// note: the text is handed to the highlighter whole, so that a string or a comment running
/// across lines is one token rather than several guesses.
///
/// note: the pieces are not the text byte for byte - a tab comes back as [`TAB`] spaces - so an
/// offset into `text` is moved by [`drawn_at`] before it is used on them.
fn tokens(extension: &str, text: &str) -> Vec<Vec<(String, Style)>> {
    let source: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut highlighter = synoptic::from_extension(extension, TAB);
    if let Some(highlighter) = &mut highlighter {
        highlighter.run(&source);
    }

    source
        .iter()
        .enumerate()
        .map(|(y, line)| match &highlighter {
            Some(highlighter) => highlighter
                .line(y, line)
                .into_iter()
                .map(|piece| match piece {
                    synoptic::TokOpt::Some(text, name) => (text, token(&name)),
                    synoptic::TokOpt::None(text) => (text, Style::default()),
                })
                .collect(),
            None => vec![(line.clone(), Style::default().fg(Color::Cyan))],
        })
        .collect()
}

/// Draws a fenced block: a rule down the left, and its tokens in this program's own colours.
///
/// note: a language nothing here recognises still gets the rule and the wrapping - it is a code
/// block whether or not anybody can colour it.
pub(super) fn highlighted(language: &str, body: &str, width: usize) -> Vec<Line<'static>> {
    let bar = Span::styled("│ ", faint());
    let room = width.saturating_sub(2).max(8);

    let mut drawn = Vec::new();
    for spans in tokens(extension(language), body) {
        for row in fit(spans, room) {
            let mut cells = vec![bar.clone()];
            cells.extend(row);
            drawn.push(Line::from(cells));
        }
    }

    drawn
}

/// The colour a command's own joints are picked out in.
///
/// note: cyan, which in the table above belongs to `struct`, `namespace`, `type` and `key` -
/// none of which a shell produces, so in this one context it is a colour nothing else is using.
/// It is also none of green, yellow or red, which the panel above this has spent on the advisor's
/// rating and cannot afford to have echoed by punctuation.
fn joint() -> Style {
    Style::default().fg(Color::Cyan).bold()
}

/// A shell command, drawn as code with its own joints picked out.
///
/// note: the same rule, the same highlighter and the same wrapping a fenced ```sh block gets, and
/// one thing on top: the `|`, `&&`, `||` and `;` that join one stage to the next are coloured.
/// They are what a person scanning the command is looking for - where it stops doing one thing and
/// starts doing another - and unpicked out they are two grey characters in the middle of a run of
/// flags and paths.
///
/// note: the joints come from [`joints`], which reads the command, rather than from the
/// highlighter, which does not read it well enough. See the note there.
///
/// note: `worst` is the stage the advisor liked least, and it is underlined rather than painted.
/// The band above the panel is already saying how bad the command is; what this answers is
/// *where*, which is the question a colour cannot answer and the one a long `&&` chain actually
/// raises. Underlining adds a channel instead of spending one - the highlighter's colours still
/// say which part of the stage is a path and which is a flag, and none of green, yellow or red
/// is repeated down here where it would compete with the line that owns it.
pub(super) fn command(
    cmd: &str,
    width: usize,
    worst: Option<(usize, usize)>,
) -> Vec<Line<'static>> {
    let bar = Span::styled("│ ", faint());
    let room = width.saturating_sub(2).max(8);
    // note: over the whole command, and so only where the whole command is one line. `joints`
    // declines a command with newlines in it and this asks it once, so a heredoc is drawn with
    // nothing picked out rather than with the offsets of a line other than the one being drawn
    //
    // note: `worst` is safe in the same row-by-row loop for the same reason and not by luck - the
    // advisor takes a command apart with this function, so a command it declines is one with no
    // stage to be worst
    let picked: Vec<(usize, usize)> = joints(cmd)
        .into_iter()
        .map(|range| drawn_at(cmd, range))
        .collect();
    let worst = worst.map(|range| drawn_at(cmd, range));

    let mut drawn = Vec::new();
    for spans in tokens("sh", cmd) {
        // note: `refit` rather than `fit`, which is the one place this parts company with a fenced
        // block. `fit` cuts where the room runs out, because reflowing a block of code would be
        // showing something the model did not write - and a command is one logical line, so
        // breaking it at a space is wrapping rather than reflowing. Cut, `cargo build` would
        // arrive as `carg` at the end of one row and `o build` at the start of the next, which is
        // a worse thing to put in front of somebody deciding whether to run it than any argument
        // for fidelity supports. The rule down the left is what makes this safe: it says the
        // second row is a continuation, which the margin cannot say
        let styled = Line::from(
            accented(spans, &picked, worst)
                .into_iter()
                .map(|(text, style)| Span::styled(text, style))
                .collect::<Vec<_>>(),
        );
        for row in refit(&styled, room) {
            let mut cells = vec![bar.clone()];
            cells.extend(row.spans);
            drawn.push(Line::from(cells));
        }
    }

    drawn
}

/// A byte range into `cmd` as a range into what the highlighter made of it, where every tab is
/// [`TAB`] spaces.
///
/// note: without it every joint and underline after a tab lands `TAB - 1` bytes early for each
/// one, and `ls\t&& rm -rf target` underlines `&& rm -rf tar`.
fn drawn_at(cmd: &str, (from, to): (usize, usize)) -> (usize, usize) {
    let tabs = |at: usize| cmd.bytes().take(at).filter(|byte| *byte == b'\t').count();
    let moved = |at: usize| at + (TAB - 1) * tabs(at);
    (moved(from), moved(to))
}

/// The same pieces, cut where a joint or the worst stage starts and ends, and restyled there.
///
/// note: the highlighter's spans and the joints are two readings of one string and neither is
/// built from the other, so this walks the pieces keeping a byte offset rather than trusting them
/// to line up. A piece that straddles the start of a joint is split; what is inside a joint's
/// range takes [`joint`]'s style whatever the highlighter made of it, because the highlighter's
/// opinion of `|` is that it is not a token at all.
///
/// note: the worst stage is a third reading, and it is cut at the same time as the other two rather
/// than in a pass of its own. A stage is a run *between* joints, so its edges fall where no joint's
/// do, and two passes would each split pieces the other had already split - the offsets are what
/// everything here is keyed on, and re-walking them is where they would come apart. So every offset
/// a style can change at is collected first and one walk honours all of them.
///
/// note: the worst stage takes an underline on top of whatever it already had rather than instead
/// of it. What it marks is a *run* of the command, several tokens long, and a run repainted in
/// one colour would take away the highlighting that says which of those tokens is the path.
fn accented(
    spans: Vec<(String, Style)>,
    picked: &[(usize, usize)],
    worst: Option<(usize, usize)>,
) -> Vec<(String, Style)> {
    if picked.is_empty() && worst.is_none() {
        return spans;
    }

    let mut edges: Vec<usize> = picked
        .iter()
        .chain(worst.iter())
        .flat_map(|(from, to)| [*from, *to])
        .collect();
    edges.sort_unstable();

    let inside = |range: &(usize, usize), at: usize| at >= range.0 && at < range.1;

    let mut out = Vec::with_capacity(spans.len());
    let mut at = 0;
    for (text, style) in spans {
        let mut rest = text.as_str();
        while !rest.is_empty() {
            // how far this piece can go before the styling of it could change
            let until = edges
                .iter()
                .copied()
                .find(|edge| *edge > at)
                .unwrap_or(usize::MAX);
            let take = rest.len().min(until.saturating_sub(at));
            // a boundary inside a character cannot happen - every joint is ASCII and a stage's
            // edges are trimmed to one - but slicing on a guess is not worth the certainty
            let take = match rest.is_char_boundary(take) {
                true => take,
                false => rest.len(),
            };

            let mut styled = match picked.iter().any(|range| inside(range, at)) {
                true => joint(),
                false => style,
            };
            if worst.is_some_and(|range| inside(&range, at)) {
                styled = styled.underlined();
            }

            out.push((rest[..take].to_owned(), styled));
            at += take;
            rest = &rest[take..];
        }
    }

    out
}

/// Breaks a run of styled pieces into rows no wider than `room`, keeping every style.
///
/// note: code is not prose and is not wrapped like it. A line that is too long is cut where the
/// room runs out rather than at a word boundary, because the alternative - reflowing - would be
/// showing something the model did not write.
///
/// note: the room is columns and the cut is between graphemes, so a row of CJK holds half as many
/// characters as a row of Latin and every one of them arrives. Cut by character count instead,
/// the rows would be twice as wide as the pane, and a `Paragraph` that does not wrap drops the
/// right-hand end - out of a block whose whole claim is that it is what the model wrote.
fn fit(spans: Vec<(String, Style)>, room: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows = vec![Vec::new()];
    let mut used = 0;

    for (text, style) in spans {
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let left = room.saturating_sub(used);
            let take = prefix_within(rest, left);
            // what is left of the row is narrower than the next grapheme, which is not the same
            // as the row being full: one cell can be free and the character need two. Close the
            // row rather than let it over-run by a cell
            if columns(&rest[..take]) > left && used != 0 {
                rows.push(Vec::new());
                used = 0;
                continue;
            }
            let piece = &rest[..take];
            used += columns(piece);
            rows.last_mut()
                .expect("there is always a row")
                .push(Span::styled(piece.to_owned(), style));
            rest = &rest[take..];
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What is underlined in a drawn command is the stage it was handed, tabs or no tabs.
    #[test]
    fn the_underlined_run_is_the_stage_it_was_handed() {
        for cmd in [
            "ls && rm -rf target",
            "ls\t&& rm -rf target",
            "ls\t&&\trm -rf\ttarget",
        ] {
            let from = cmd.find("rm").expect("a stage");
            let under: String = command(cmd, 200, Some((from, cmd.len())))
                .iter()
                .flat_map(|line| line.spans.iter())
                .filter(|span| {
                    span.style
                        .add_modifier
                        .contains(ratatui::style::Modifier::UNDERLINED)
                })
                .map(|span| span.content.as_ref())
                .collect();
            assert_eq!(
                under,
                cmd[from..].replace('\t', &" ".repeat(TAB)),
                "{cmd:?}"
            );
        }
    }

    /// A line of a block with nothing in it is drawn as the rule and nothing beside it.
    ///
    /// note: a model puts an empty line inside a block to space two pieces of code apart, and
    /// dropping the row would show the block one line shorter than the model wrote it. An empty
    /// line elsewhere in the same block arriving whole is what says it was not a row that fell off
    /// the end.
    #[test]
    fn a_blank_line_inside_a_block_is_drawn() {
        let drawn: Vec<String> = highlighted("rust", "fn main() {\n\n    let x = 1;\n}\n", 40)
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();

        assert_eq!(drawn, ["│ fn main() {", "│ ", "│     let x = 1;", "│ }"]);
    }

    /// Three spaces of indent is a fence; four is an indented block, and nothing here.
    ///
    /// note: CommonMark's boundary, and it is a real one - a list's contents are indented four and
    /// their fences are not fences. Read the other way round, a block the model indented by three
    /// arrives as prose, so the answer loses its rule down the left and its colours, and the code
    /// is set as an indented block instead.
    #[test]
    fn a_fence_indented_by_three_spaces_is_still_a_fence() {
        assert_eq!(
            chunks("intro\n\n   ```rust\nfn main() {}\n   ```\n"),
            [
                Chunk::Prose("intro\n\n"),
                Chunk::Code {
                    language: "rust",
                    body: "fn main() {}\n",
                },
            ],
            "three spaces of indent",
        );
        assert_eq!(
            chunks("intro\n\n    ```rust\nfn main() {}\n    ```\n"),
            [Chunk::Prose(
                "intro\n\n    ```rust\nfn main() {}\n    ```\n",
            )],
            "four spaces of indent",
        );
    }

    /// The prose in front of a table is a chunk, and the table starts at the header's own line.
    ///
    /// note: a table is only recognised because of the delimiter row under its header, so the
    /// header is a line back from the line that makes the decision - and the prose before it has
    /// to be handed over as text of its own. Losing it leaves the answer starting at the table,
    /// which is the model reading a document rather than a person being answered.
    #[test]
    fn the_prose_in_front_of_a_table_is_a_chunk_of_its_own() {
        assert_eq!(
            chunks("what it does:\n\n| a | b |\n| - | - |\n| 1 | 2 |\n"),
            [
                Chunk::Prose("what it does:\n\n"),
                Chunk::Table("| a | b |\n| - | - |\n| 1 | 2 |\n"),
            ],
        );
        assert_eq!(
            chunks("| a | b |\n| - | - |\n| 1 | 2 |\n"),
            [Chunk::Table("| a | b |\n| - | - |\n| 1 | 2 |\n")],
            "a table with nothing in front of it",
        );
    }

    /// A drawn block's tokens wear the colours [`token`] gives those names.
    ///
    /// note: through [`highlighted`] rather than by asking `token` directly, because a colour the
    /// highlighter never emits is a colour nothing can observe - the names here are chosen to be
    /// the ones a fence declaring that language actually produces, and the claim under test is
    /// that each of them arrives in its own colour on the row it is drawn in.
    #[test]
    fn a_fenced_blocks_tokens_are_drawn_in_their_own_colours() {
        for (language, body, token, colour) in [
            // a diff's removed line: the only name that means red
            ("diff", "-removed", "-removed", Color::Red),
            // and its added line, so the two halves of a diff are told apart
            ("diff", "+added", "+added", Color::Green),
            // markdown's link and list markers: the names that mean cyan here
            ("md", "- item", "-", Color::Cyan),
            ("md", "[text](url)", "[text]", Color::Cyan),
        ] {
            let drawn: Vec<(String, Style)> = highlighted(language, body, 80)
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| (span.content.to_string(), span.style))
                .collect();
            let hit = drawn
                .iter()
                .find(|(text, _)| text == token)
                .unwrap_or_else(|| panic!("{token:?} was not drawn out of {body:?}"));
            assert_eq!(hit.1, Style::default().fg(colour), "{token:?} in {body:?}");
        }
    }

    /// A grapheme that will not fit in what is left of the row closes the row rather than
    /// over-running it.
    ///
    /// note: `used != 0` is the whole of the second half of that condition. On an empty row there
    /// is nothing to over-run - the row has all the room there is - so the character goes in at
    /// its full width and the row is a cell over rather than the character being dropped.
    #[test]
    fn a_wide_character_never_starts_a_row_it_does_not_fit_in() {
        // eight cells of room, three taken: `de` fits, and `一` needs two more than are left
        assert_eq!(rows_of(&["abc", "de一丁"], 8), ["abcde一", "丁"]);
        // the same text a cell wider and it is one row, which is what the room is for
        assert_eq!(rows_of(&["abc", "de一丁"], 9), ["abcde一丁"]);
    }

    /// A row is closed when it is full and there is more to come, and not one character sooner.
    ///
    /// note: the zero-width space is the case here. It is a character that occupies no cells, so
    /// `used >= room` is false after it and `used < room` is true - the row must *not* be closed
    /// on it, or a row with a cell free comes out empty and the text after it is pushed down a
    /// row for nothing.
    #[test]
    fn a_row_is_not_closed_before_it_is_full() {
        // eight cells exactly, then a character that takes none of them, then two more
        assert_eq!(
            rows_of(&["abcdefgh", "\u{200b}ij"], 8),
            ["abcdefgh\u{200b}", "ij"],
            "a row with a cell free is not full"
        );
        // and a zero-width character after a full row of wide ones keeps the row it is on
        assert_eq!(
            rows_of(&["ab", "de一丁", "\u{200b}"], 8),
            ["abde一丁\u{200b}"],
            "nothing is pushed down a row for a character that takes no cells"
        );
    }

    /// What [`fit`] draws for one body at one width, as its rows of text.
    ///
    /// note: the pieces go in as one call rather than several, because a row can be filled across
    /// a boundary between two of the highlighter's tokens, and a helper that fitted each piece on
    /// its own would never put them in the same row.
    fn rows_of(pieces: &[&str], room: usize) -> Vec<String> {
        let style = Style::default();
        let spans: Vec<(String, Style)> = pieces
            .iter()
            .map(|text| (text.to_string(), style))
            .collect();
        fit(spans, room)
            .into_iter()
            .map(|row| row.iter().map(|span| span.content.to_string()).collect())
            .collect()
    }

    /// A divider is three of one mark and nothing else, whichever of the three marks it is.
    ///
    /// note: markdown spells a rule as `-`, `*` or `_`, and a model can reach for any of the
    /// three - an escaped run is a divider to it and punctuation to the reader - so all three have
    /// to arrive as a rule rather than only the one.
    #[test]
    fn a_line_of_marks_is_a_rule() {
        for text in ["---", "***", "___"] {
            assert!(
                rule(&Line::from(text)),
                "{text:?} is three characters of one mark and nothing else"
            );
        }

        for text in ["--", "* *", "**bold**", "___a divider___", "a-b-c"] {
            assert!(!rule(&Line::from(text)), "{text:?} is not a divider");
        }
    }

    /// What one stretch of an answer is, and the words of it.
    fn split(text: &str) -> Vec<String> {
        chunks(text)
            .into_iter()
            .map(|chunk| match chunk {
                Chunk::Prose(prose) => format!("prose {prose:?}"),
                Chunk::Table(block) => format!("table {block:?}"),
                Chunk::Code { language, body } => format!("code {language:?} {body:?}"),
            })
            .collect()
    }

    /// A closing fence is the character the block opened with, and no shorter than the one that
    /// opened it.
    ///
    /// note: CommonMark says a fence closes on a run of the same character *at least as long*, so
    /// the run's length is half of what closes a block and the other half is that the rest of the
    /// line is nothing but that character. Read as either half alone, the ``` inside a ```` block
    /// would end it - and a block the model indented with four backticks to hold a three-backtick
    /// snippet is the ordinary way of saying that, not a corner of the format. The tilde fence is
    /// the other direction: it is not the character the block opened with, so it is content.
    #[test]
    fn a_fence_closes_a_block_only_if_it_is_the_same_character_and_no_shorter() {
        assert_eq!(
            split("````rust\nfn a() {}\nstill inside\n```\n"),
            [r#"code "rust" "fn a() {}\nstill inside\n```\n""#],
            "a shorter run of the same character is not a closing fence",
        );
        assert_eq!(
            split("```rust\nfn a() {}\n~~~\n```py\nb = 1\n```\n"),
            [r#"code "rust" "fn a() {}\n~~~\n```py\nb = 1\n""#],
            "another character's fence is content, and the block runs on past it",
        );
    }

    /// A fence right under a line of prose still names its language.
    ///
    /// note: the language is read off the fence's own line, and a fence need not have a blank row
    /// above it. Read from one character too early, the line it starts on is the prose's, and a
    /// block of Rust arrives named after the end of the sentence in front of it.
    #[test]
    fn a_fence_right_under_prose_still_names_its_language() {
        assert_eq!(
            split("intro\n```rust\nfn main() {}\n```\n"),
            ["prose \"intro\\n\"", "code \"rust\" \"fn main() {}\\n\""]
        );
    }
}
