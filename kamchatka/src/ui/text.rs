//! Measuring text, and making it fit.
//!
//! note: nothing in here draws. They are the functions that answer "how many rows will this
//! take" and "what does this look like at 34 columns", which is what a screen made of nested
//! [`Rect`]s spends most of its time asking. Kept apart from the drawing for the ordinary
//! reason: they are the part that can be reasoned about without a terminal.

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use super::faint;
use crate::app::text::thousands;

/// How many terminal columns `text` occupies.
///
/// note: the one place in this program that answers it, and everything deciding what fits goes
/// through it. A character is not a column - a CJK character is given two cells and so is an
/// emoji - so a row measured in characters holds twice what it is told it holds, and the overflow
/// is clipped at the right edge by a `Paragraph` that does not wrap. That clipped end is not
/// reachable by scrolling either, which is what makes it worse than a wrong count.
///
/// note: it does not follow that everything counting characters here is wrong. A cut made *in*
/// the text - the first ninety-six characters of a summary, a window into a long line - is about
/// the text and stays in characters. What belongs here is every question of the form "will this
/// fit", and the answer to that is columns.
pub(super) fn columns(text: &str) -> usize {
    text.width()
}

/// `text` clipped to `width` columns and padded out to exactly that many.
///
/// note: `{:<width$}` is the obvious way and it pads by *characters*, so a label of three CJK
/// characters in a column ten wide is given seven spaces and takes thirteen cells - which pushes
/// every column to the right of it off the end of the row. Nothing in `std`'s formatting knows
/// what a column is, so a padded cell of text somebody else wrote is built here instead.
pub(super) fn pad(text: &str, width: usize) -> String {
    let clipped = clip(text, width);
    let spare = width.saturating_sub(columns(&clipped));

    format!("{clipped}{}", " ".repeat(spare))
}

/// The rows one logical line takes: fill with whole word-bound chunks, start a new row when the
/// next one will not fit, and split a chunk that will not fit on a row of its own.
pub(super) fn rows_for(line: &str, width: usize) -> usize {
    let mut closed = 0;
    let mut filled = 0;
    let mut started = false;

    for (_, text) in line.split_word_bound_indices() {
        let mut chunk = text;
        while !chunk.is_empty() {
            let chunk_width = columns(chunk);
            if filled + chunk_width <= width {
                filled += chunk_width;
                started = true;
                break;
            }
            // there is something on this row already, so end it and try the chunk on the next
            if started {
                closed += 1;
                filled = 0;
                started = false;
                continue;
            }
            // an empty row and it still does not fit: this is one long word, and it is cut
            let take = prefix_within(chunk, width);
            closed += 1;
            chunk = &chunk[take..];
            filled = 0;
        }
    }

    // the row still being filled counts, and a line with nothing on it is a row of its own
    closed + usize::from(started || closed == 0)
}

/// The longest prefix of `text` that fits in `width` columns, and never nothing - a grapheme
/// wider than the whole box would otherwise loop forever.
pub(super) fn prefix_within(text: &str, width: usize) -> usize {
    let mut end = 0;
    let mut filled = 0;

    for (offset, grapheme) in text.grapheme_indices(true) {
        let next = filled + columns(grapheme);
        if end != 0 && next > width {
            break;
        }
        end = offset + grapheme.len();
        filled = next;
        if filled >= width {
            break;
        }
    }

    end.max(text.chars().next().map_or(1, char::len_utf8))
        .min(text.len())
}

/// The longest suffix of `text` that fits in `width` columns, as a length in bytes.
pub(super) fn suffix_within(text: &str, width: usize) -> usize {
    let mut len = 0;
    let mut filled = 0;

    for (offset, grapheme) in text.grapheme_indices(true).rev() {
        let next = filled + columns(grapheme);
        if len != 0 && next > width {
            break;
        }
        len = text.len() - offset;
        filled = next;
        if filled >= width {
            break;
        }
    }

    len
}

pub(super) fn gutter(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let bar = Span::styled("│ ", faint());
    let code = Style::default().fg(Color::Cyan);
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();

    split_to_fit(&text, width.saturating_sub(2).max(8))
        .into_iter()
        .map(|piece| Line::from(vec![bar.clone(), Span::styled(piece, code)]))
        .collect()
}

/// Breaks a line of already-styled fragments into lines that fit, keeping every fragment's style
/// and hanging the continuations under whatever the line was indented by.
///
/// note: `Paragraph` can wrap, but it wraps at render time, and this program counts lines to know
/// where it is scrolled to - a widget that quietly turned forty lines into sixty would put the
/// scrolling out by twenty. So the wrapping happens here, where the count is taken.
pub(super) fn refit(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(8);
    let plain: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    if columns(&plain) <= width {
        return vec![
            Line::from(
                line.spans
                    .iter()
                    .map(|span| Span::styled(span.content.to_string(), span.style))
                    .collect::<Vec<_>>(),
            )
            .style(line.style),
        ];
    }

    // an indented line's continuations belong under it rather than back at the margin, and so do
    // a list item's: `- ` and `1. ` are indentation as far as reading it goes
    let spaces = plain.chars().take_while(|c| *c == ' ').count();
    let hang = " ".repeat(spaces + bullet(&plain[spaces..]));
    let hang = match hang.len() + 8 < width {
        true => hang,
        false => String::new(),
    };

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut column = 0;
    for span in &line.spans {
        // `split_inclusive` keeps the spaces, so what is placed is a word and the gap after it
        for word in span.content.split_inclusive(' ') {
            for piece in split_to_fit(word, width - hang.len()) {
                let length = columns(piece.trim_end());
                if column != 0 && column + length > width {
                    out.push(Line::from(std::mem::take(&mut current)).style(line.style));
                    column = hang.len();
                    if !hang.is_empty() {
                        current.push(Span::raw(hang.clone()));
                    }
                }
                current.push(Span::styled(piece.clone(), span.style));
                column += columns(&piece);
            }
        }
    }
    out.push(Line::from(current).style(line.style));

    out
}

/// The width of a list marker at the start of a line, or nought if there is not one.
fn bullet(text: &str) -> usize {
    if let Some(rest) = text.strip_prefix(['-', '*', '+'])
        && rest.starts_with(' ')
    {
        return 2;
    }

    // `12. ` and `12) `, however many digits it runs to
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    match digits != 0 && matches!(text.get(digits..digits + 2), Some(". ") | Some(") ")) {
        true => digits + 2,
        false => 0,
    }
}

/// Where a command line's own top-level joints are - `|`, `||`, `&&`, `;` - as byte ranges into
/// it; empty when there are none, or when the scan could not be trusted.
///
/// note: ranges rather than a rewritten command. This started out breaking the line at each of
/// them, one stage to a row, which read well and cost a row per stage on a panel whose rows are
/// its scarcest thing - and a broken command is not something `sh` would take back, so the panel
/// was showing a spelling nobody could act on. Colouring the joints in place says the same thing:
/// where one stage ends and the next begins, at a glance, in a command still written the way the
/// model wrote it.
///
/// note: worked out here rather than taken from the highlighter, which is the obvious free option
/// and is wrong twice over. `synoptic`'s `sh` mode calls every flag's hyphen an operator - `-n`,
/// `-u`, `-5` - and does not tokenise `|` or `;` at all, so painting its operators would colour
/// the noise and miss the joints. These are the joints.
///
/// note: quote-aware, and it gives up rather than guessing.
/// [`reaches_the_network`](crate::tools::reaches_the_network) splits a command on these same
/// characters without caring where in it they are, because a policy that over-reads a command asks
/// a question it need not have, and that is the right way for a policy to be wrong. A panel that
/// over-reads one *tells somebody a quoted `|` is a pipe* on the screen where they decide whether
/// to run it. So an unterminated quote, an unclosed `$(` or a trailing backslash comes back empty
/// and the command is drawn with nothing picked out.
///
/// note: a command that already has newlines in it is left alone without being scanned at all.
/// What is inside a heredoc is arbitrary text - the `|` in the middle of a Python string is not a
/// joint, and nothing readable from one line says so.
pub(super) fn joints(cmd: &str) -> Vec<(usize, usize)> {
    if cmd.contains('\n') {
        return Vec::new();
    }

    let mut found: Vec<(usize, usize)> = Vec::new();

    // note: bytes rather than characters, which is safe because everything looked at here is
    // ASCII: every byte of a multi-byte character is `0x80` or above, so it can match none of
    // them, and every index taken is one of theirs
    let bytes = cmd.as_bytes();
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if escaped {
            escaped = false;
            at += 1;
            continue;
        }
        match quote {
            // nothing inside these is an escape, not even a backslash
            Some(b'\'') => {
                if byte == b'\'' {
                    quote = None;
                }
                at += 1;
                continue;
            }
            Some(mark) => {
                match byte {
                    b'\\' => escaped = true,
                    it if it == mark => quote = None,
                    _ => {}
                }
                at += 1;
                continue;
            }
            None => {}
        }

        match byte {
            b'\\' => {
                escaped = true;
                at += 1;
                continue;
            }
            // a backtick closes with the character it opened with, so it counts here as a quote
            // rather than as a depth
            b'\'' | b'"' | b'`' => {
                quote = Some(byte);
                at += 1;
                continue;
            }
            b'(' => {
                depth += 1;
                at += 1;
                continue;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                at += 1;
                continue;
            }
            _ => {}
        }
        // a joint inside `$(…)` joins the inner command's stages, not this one's
        if depth != 0 || !matches!(byte, b'|' | b'&' | b';') {
            at += 1;
            continue;
        }

        let twice = bytes.get(at + 1) == Some(&byte);
        let width = match (byte, twice) {
            // a lone `&` is a job put in the background, and it is also the `&` of `2>&1`;
            // neither is a seam worth picking out
            (b'&', false) => {
                at += 1;
                continue;
            }
            // and `;;` ends a `case` arm rather than separating two commands
            (b';', true) => {
                at += 2;
                continue;
            }
            (_, true) => 2,
            (_, false) => 1,
        };

        found.push((at, at + width));
        at += width;
    }

    // a scan that ended in the middle of something did not understand the command, and a command
    // this cannot read is one it picks nothing out of
    match quote.is_some() || escaped || depth != 0 {
        true => Vec::new(),
        false => found,
    }
}

/// Splits a word that is wider than the line into pieces that are not.
///
/// note: between graphemes rather than between characters, and by column rather than by count.
/// Half of a wide character is not a character, and an `é` written as `e` and a combining accent
/// is two characters and one cell - so a chunking that counted either would put the accent on the
/// row below the letter it belongs to. [`prefix_within`] answers both, and it never answers
/// nothing, which is what stops a grapheme wider than the whole box looping here for ever.
pub(super) fn split_to_fit(word: &str, width: usize) -> Vec<String> {
    if columns(word) <= width {
        return vec![word.to_owned()];
    }

    let width = width.max(1);
    let mut out = Vec::new();
    let mut rest = word;
    while !rest.is_empty() {
        let take = prefix_within(rest, width);
        out.push(rest[..take].to_owned());
        rest = &rest[take..];
    }

    out
}

/// Breaks text into lines that fit, keeping the newlines it already had, hanging continuations
/// under the prefix, and leaving a line that already fits exactly as it was.
///
/// note: The last of those is what makes code and command output readable in here - a wrapper
/// that reflowed every line would turn an indented block into a paragraph. A line that has to be
/// broken keeps its own indentation on the pieces, so a list stays a list.
pub(super) fn wrapped(text: &str, width: usize, prefix: &str) -> Vec<String> {
    let head = columns(prefix);
    let width = width.max(head + 12);
    let hanging = " ".repeat(head);

    let mut out: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let lead: String = paragraph.chars().take_while(|c| *c == ' ').collect();
        let room = (width - head).saturating_sub(lead.len()).max(12);

        for line in fold(paragraph.trim_start(), room) {
            let start = match out.is_empty() {
                true => prefix,
                false => &hanging,
            };
            out.push(format!("{start}{lead}{line}"));
        }
    }

    out
}

/// Breaks one paragraph into pieces no wider than `room`.
///
/// note: `split(' ')` rather than `split_whitespace`, because the latter collapses a run of
/// spaces into one and this is what draws `/payload` - a panel whose whole claim is that it shows
/// the bytes that would go out. A file's indentation inside a JSON string is spaces in a row, and
/// re-flowing them away would be quietly answering a different question. It keeps the help's
/// columns lined up in a narrow pane, too.
pub(super) fn fold(body: &str, room: usize) -> Vec<String> {
    if columns(body) <= room {
        return vec![body.to_owned()];
    }

    let mut out = Vec::new();
    let mut line = String::new();
    // a word can now be the empty string - that is what a run of spaces is made of - so "have I
    // put anything on this line yet" is its own question rather than `line.is_empty()`
    let mut fresh = true;

    for word in body.split(' ') {
        // a single word longer than the pane is broken rather than allowed to overflow; the last
        // piece stays open, so that whatever follows can share the line with it
        if columns(word) > room {
            if !fresh {
                out.push(std::mem::take(&mut line));
            }
            out.extend(split_to_fit(word, room));
            line = out.pop().unwrap_or_default();
            fresh = false;
            continue;
        }

        if !fresh && columns(&line) + 1 + columns(word) > room {
            out.push(std::mem::take(&mut line));
            fresh = true;
        }
        if !fresh {
            line.push(' ');
        }
        line.push_str(word);
        fresh = false;
    }
    out.push(line);

    out
}

/// Shortens text to a width in columns, with an ellipsis if it had to.
///
/// note: the ellipsis is a column of its own and is counted as one, so what comes back is never
/// wider than it was asked for. There is a case with no good answer - one grapheme two cells wide
/// in a box two cells wide, where the grapheme and the ellipsis do not both fit - and it goes to
/// the ellipsis: a box this narrow can say *something was cut* or it can say one character of
/// what, and the first is the one a reader can act on.
pub(super) fn clip(text: &str, width: usize) -> String {
    if columns(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }

    // the ellipsis takes the last column, so what goes in front of it has the rest
    let head = &text[..prefix_within(text, width - 1)];
    match columns(head) < width {
        true => format!("{head}…"),
        false => "…".to_owned(),
    }
}

/// A round number in as few characters as it can be said in: `1M`, `131k`, `4.1k`.
///
/// note: For the limit rather than the count. Nobody reads `1,048,576` as anything but "a lot",
/// and it is the shape of the number that matters when it is sitting next to a percentage.
pub(super) fn compact(n: usize) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1_000.0),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        1_000_000..10_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0).replace(".0M", "M"),
        _ => format!("{}M", n / 1_000_000),
    }
}

/// A token count for a column that cannot grow: the exact figure while it fits, [`compact`]'s
/// abbreviation when it does not.
///
/// note: `held` is where an unbounded number can land. What is being *sent* is bounded by the
/// window it is being sent to; what is being *held* is whatever a tool actually produced, since
/// `keep_truncated_output` archives the whole of it - and a `grep` that wandered into a build
/// directory holds millions. Seven columns stop at `999,999`, and a wider figure did not widen the
/// column - `{:>7}` pads and never truncates, so it took the columns it needed from its
/// neighbours: `0` and `1,400,000` arrived as `01,400,000`, and the row lost its last two
/// characters to the clip. Precision is the right thing to give up instead, because nobody acts on
/// the last three digits of something that is not going anywhere.
pub(super) fn fitted(n: usize, width: usize) -> String {
    match thousands(n) {
        exact if exact.len() <= width => exact,
        _ => compact(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pieces a set of joint ranges picks out of a command, for reading a test by.
    fn picked(cmd: &str) -> Vec<&str> {
        joints(cmd)
            .into_iter()
            .map(|(from, to)| &cmd[from..to])
            .collect()
    }

    /// Every joint, and only the joints.
    #[test]
    fn a_commands_own_joints_are_found() {
        assert_eq!(
            picked("cargo build --release 2>&1 | tail -5 && echo done"),
            ["|", "&&"]
        );
        assert_eq!(picked("cd src; ls || true"), [";", "||"]);
        // and they are where they are, not merely how many: the offsets are what gets coloured
        let cmd = "a | b";
        assert_eq!(joints(cmd), vec![(2, 3)]);
        assert_eq!(&cmd[2..3], "|");
    }

    /// What looks like a joint and is not.
    #[test]
    fn what_is_not_a_joint_is_not_picked_out() {
        assert!(joints("cargo build --release").is_empty());
        assert!(joints("").is_empty());
        // `2>&1` and a job in the background are both a lone `&`, and neither is a seam
        assert!(joints("make 2>&1").is_empty());
        assert!(joints("sleep 60 &").is_empty());
        // a `case` arm's `;;` ends an arm rather than separating two commands
        assert!(joints("case $x in a) echo one;; esac").is_empty());
        // and a command with newlines of its own is not scanned at all: what is inside a heredoc
        // is arbitrary text, and the `|` in a Python expression is not a pipe
        assert!(joints("python3 - <<'PY'\nprint(1 | 2)\nPY").is_empty());
    }

    /// A separator that is part of an argument is not a joint, whichever way it was quoted.
    ///
    /// note: the case this function is for. Coloured as a joint, the `|` in `echo 'a | b'` would
    /// be the panel telling somebody a quoted character is a pipe, on the screen where they decide
    /// whether to run it - so a quoted separator has to be invisible to the scan.
    #[test]
    fn a_separator_inside_a_quote_is_not_a_joint() {
        assert!(joints("echo 'a | b'").is_empty());
        assert!(joints("echo \"a && b\"").is_empty());
        assert!(joints(r"echo a\;b").is_empty());
        // the inner command's joint is the inner command's
        assert!(joints("echo $(date | tr -d '\\n')").is_empty());
        assert!(joints("echo `date | wc -c`").is_empty());

        // and the real one beside a quoted one is still found
        assert_eq!(picked("grep -e '|' file | wc -l"), ["|"]);
    }

    /// Eight characters and sixteen cells, cut at the cell.
    ///
    /// note: the box widths here are the ones a screen cannot be driven to. A pane is never two
    /// columns wide in practice, and the arithmetic that has to survive it is the same arithmetic
    /// a narrow one uses - so this is where the case with no good answer is pinned, rather than
    /// left to be discovered by somebody dragging a window edge.
    #[test]
    fn a_clip_counts_cells_rather_than_characters() {
        assert_eq!(clip("一丁丂七丄丅丆万", 9), "一丁丂七…");
        assert!(columns(&clip("一丁丂七丄丅丆万", 9)) <= 9);

        // no room for a grapheme and an ellipsis both: the ellipsis is what a reader can act on
        assert_eq!(clip("一丁", 2), "…");
        assert_eq!(clip("一丁", 0), "");
        // and nothing changes for text a cell wide
        assert_eq!(clip("hello", 4), "hel…");
        assert_eq!(clip("hi", 5), "hi");
    }

    /// A padded cell is as many columns as it was asked for, whatever is in it.
    #[test]
    fn a_padded_cell_is_the_columns_it_was_given() {
        for (text, width) in [("一丁", 8), ("ab", 5), ("一丁丂七丄", 6), ("", 3)] {
            assert_eq!(columns(&pad(text, width)), width, "`{text}` at {width}");
        }
        assert_eq!(pad("ab", 5), "ab   ");
    }

    /// A word too wide for the row is broken between graphemes, and every piece fits.
    #[test]
    fn a_long_word_is_split_by_column_and_between_graphemes() {
        let pieces = split_to_fit("一丁丂七丄丅", 5);
        assert!(pieces.iter().all(|piece| columns(piece) <= 5), "{pieces:?}");
        assert_eq!(pieces.concat(), "一丁丂七丄丅");

        // a combining accent is two characters and one cell, and it stays on its letter
        let pieces = split_to_fit("e\u{0301}e\u{0301}e\u{0301}", 2);
        assert_eq!(pieces, ["e\u{0301}e\u{0301}", "e\u{0301}"]);
    }

    /// And a paragraph is folded by the same count, losing nothing.
    #[test]
    fn folding_a_paragraph_counts_cells() {
        let rows = fold("一丁丂七丄丅丆万", 8);
        assert!(rows.iter().all(|row| columns(row) <= 8), "{rows:?}");
        assert_eq!(rows.concat(), "一丁丂七丄丅丆万");
    }

    /// A command this cannot read to the end has nothing picked out of it.
    #[test]
    fn an_unreadable_command_is_left_plain() {
        assert!(joints("echo 'unterminated | still going").is_empty());
        assert!(joints("echo \"unterminated && more\" | wc -l\"").is_empty());
        assert!(joints("make | tail -1 $(echo").is_empty());
        assert!(joints("make | tail -1 \\").is_empty());
    }
}
