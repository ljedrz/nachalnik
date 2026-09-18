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

use super::faint;
use crate::app::text::thousands;

/// The rows one logical line takes: fill with whole word-bound chunks, start a new row when the
/// next one will not fit, and split a chunk that will not fit on a row of its own.
pub(super) fn rows_for(line: &str, width: usize) -> usize {
    let mut closed = 0;
    let mut filled = 0;
    let mut started = false;

    for (_, text) in line.split_word_bound_indices() {
        let mut chunk = text;
        while !chunk.is_empty() {
            let chunk_width = Span::raw(chunk).width();
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
        let next = filled + Span::raw(grapheme).width();
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
        let next = filled + Span::raw(grapheme).width();
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
    if plain.chars().count() <= width {
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
    let hang = match hang.chars().count() + 8 < width {
        true => hang,
        false => String::new(),
    };

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut column = 0;
    for span in &line.spans {
        // `split_inclusive` keeps the spaces, so what is placed is a word and the gap after it
        for word in span.content.split_inclusive(' ') {
            for piece in split_to_fit(word, width - hang.chars().count()) {
                let length = piece.trim_end().chars().count();
                if column != 0 && column + length > width {
                    out.push(Line::from(std::mem::take(&mut current)).style(line.style));
                    column = hang.chars().count();
                    if !hang.is_empty() {
                        current.push(Span::raw(hang.clone()));
                    }
                }
                current.push(Span::styled(piece.clone(), span.style));
                column += piece.chars().count();
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

/// What a stage after the first is indented by, so that the joints stand in a column of their own
/// down the left of the block and the first stage is the only thing at the margin.
const UNDER: &str = "  ";

/// The same command line with each of its top-level joints - `|`, `||`, `&&`, `;` - at the head of
/// a line of its own; `None` when there is nothing to break at, or when the scan could not be
/// trusted.
///
/// note: the operator leads its line rather than trailing the one before it, which is the shape
/// rustfmt gives a long boolean expression. What it buys is that a reader going down the left edge
/// sees how each stage is joined to the last before reading the stage itself - and `&&` after a
/// `curl` that may or may not have worked is exactly the thing not to have to hunt for at the end
/// of a line somewhere off to the right.
///
/// note: what it costs is that the broken form is no longer something `sh` would take. A newline
/// *after* `|` continues the command and a newline *before* one ends it, so this is a rendering
/// and not a second spelling. That is a fair price for a panel - `[i]` is the byte-exact view and
/// is one key away - and the property actually worth keeping is narrower: every character drawn is
/// the command's own, in the command's own order, with nothing added but the breaks and the
/// indent, and nothing taken but the spaces around a joint.
///
/// note: quote-aware, and it gives up rather than guessing.
/// [`reaches_the_network`](crate::tools::reaches_the_network) splits a command on these same
/// characters without caring where in it they are, because a policy that over-reads a command asks
/// a question it need not have, and that is the right way for a policy to be wrong. A panel that
/// over-reads one *draws a command nobody wrote*, which is the one thing this screen cannot do. So
/// an unterminated quote, an unclosed `$(` or a trailing backslash leaves the command exactly as
/// it arrived.
///
/// note: a command that already has newlines in it is left alone without being scanned at all. It
/// has its structure already, and what is inside a heredoc is arbitrary text - the `|` in the
/// middle of a Python string is not a joint, and nothing readable from one line says so.
pub(super) fn joints(cmd: &str) -> Option<String> {
    if cmd.contains('\n') {
        return None;
    }

    // the operator that introduced each stage, and the stage itself; the first one has none
    let mut stages: Vec<(&str, &str)> = Vec::new();
    let mut operator = "";
    let mut start = 0;

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
        // a joint inside `$(…)` is the inner command's, and a line of the outer one is not where
        // it goes
        if depth != 0 || !matches!(byte, b'|' | b'&' | b';') {
            at += 1;
            continue;
        }

        let twice = bytes.get(at + 1) == Some(&byte);
        let width = match (byte, twice) {
            // a lone `&` is a job put in the background, and it is also the `&` of `2>&1`;
            // neither is a seam worth a line of its own
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

        stages.push((operator, cmd[start..at].trim()));
        operator = &cmd[at..at + width];
        at += width;
        start = at;
    }

    // a scan that ended in the middle of something did not understand the command, and a command
    // this cannot read is one it does not touch
    if quote.is_some() || escaped || depth != 0 {
        return None;
    }
    stages.push((operator, cmd[start..].trim()));

    // a command written with its separator at the end - `make;` - leaves a last stage with nothing
    // in it, and a line holding a lone `;` says nothing at all. It goes back on the end of the
    // stage it followed, which is where somebody wrote it
    let trailing = match stages.last() {
        Some((_, stage)) if stage.is_empty() && stages.len() > 1 => {
            stages.pop().map_or("", |(operator, _)| operator)
        }
        _ => "",
    };
    // one stage is a command with no joints in it, and an empty one anywhere else is a command
    // this did not understand after all
    if stages.len() < 2 || stages.iter().any(|(_, stage)| stage.is_empty()) {
        return None;
    }

    let mut out = String::new();
    for (nth, (operator, stage)) in stages.iter().enumerate() {
        match nth {
            0 => out.push_str(stage),
            _ => out.push_str(&format!("\n{UNDER}{operator} {stage}")),
        }
    }
    out.push_str(trailing);

    Some(out)
}

/// Splits a word that is wider than the line into pieces that are not.
pub(super) fn split_to_fit(word: &str, width: usize) -> Vec<String> {
    if word.chars().count() <= width {
        return vec![word.to_owned()];
    }
    let characters: Vec<char> = word.chars().collect();

    characters
        .chunks(width.max(1))
        .map(|piece| piece.iter().collect())
        .collect()
}

/// Breaks text into lines that fit, keeping the newlines it already had, hanging continuations
/// under the prefix, and leaving a line that already fits exactly as it was.
///
/// note: The last of those is what makes code and command output readable in here - a wrapper
/// that reflowed every line would turn an indented block into a paragraph. A line that has to be
/// broken keeps its own indentation on the pieces, so a list stays a list.
pub(super) fn wrapped(text: &str, width: usize, prefix: &str) -> Vec<String> {
    let head = prefix.chars().count();
    let width = width.max(head + 12);
    let hanging = " ".repeat(head);

    let mut out: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let lead: String = paragraph.chars().take_while(|c| *c == ' ').collect();
        let room = (width - head).saturating_sub(lead.chars().count()).max(12);

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
    if body.chars().count() <= room {
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
        if word.chars().count() > room {
            if !fresh {
                out.push(std::mem::take(&mut line));
            }
            let characters: Vec<char> = word.chars().collect();
            for piece in characters.chunks(room) {
                out.push(piece.iter().collect());
            }
            line = out.pop().unwrap_or_default();
            fresh = false;
            continue;
        }

        if !fresh && line.chars().count() + 1 + word.chars().count() > room {
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

/// Shortens text to a width, with an ellipsis if it had to.
pub(super) fn clip(text: &str, width: usize) -> String {
    match text.chars().count() > width {
        true if width > 1 => format!("{}…", text.chars().take(width - 1).collect::<String>()),
        true => text.chars().take(width).collect(),
        false => text.to_owned(),
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

    /// A pipeline is one stage to a line, with what joins it to the last in front of it.
    #[test]
    fn a_command_is_broken_at_its_joints() {
        assert_eq!(
            joints("cargo build --release 2>&1 | tail -5 && echo done").as_deref(),
            Some("cargo build --release 2>&1\n  | tail -5\n  && echo done")
        );
        assert_eq!(
            joints("cd src; ls || true").as_deref(),
            Some("cd src\n  ; ls\n  || true")
        );
    }

    /// And a command with no joints in it is left for the caller to draw as it arrived.
    #[test]
    fn a_command_with_nothing_to_break_at_is_left_alone() {
        assert_eq!(joints("cargo build --release"), None);
        assert_eq!(joints(""), None);
        // `2>&1` and a job in the background are both a lone `&`, and neither is a seam
        assert_eq!(joints("make 2>&1"), None);
        assert_eq!(joints("sleep 60 &"), None);
        // and one that has structure of its own is not given a second kind
        assert_eq!(joints("python3 - <<'PY'\nprint(1 | 2)\nPY"), None);
    }

    /// A separator that is part of an argument is not a joint, whichever way it was quoted.
    ///
    /// note: the case this function is for. Drawn a line at a time, `echo 'a | b'` would arrive on
    /// the screen as two commands, on the one screen that exists to say what is about to run - so
    /// a quoted separator has to be invisible to the scan, and a quote it cannot pair up has to
    /// stop it altogether.
    #[test]
    fn a_separator_inside_a_quote_is_not_a_joint() {
        assert_eq!(joints("echo 'a | b'"), None);
        assert_eq!(joints("echo \"a && b\""), None);
        assert_eq!(joints(r"echo a\;b"), None);
        assert_eq!(
            joints("grep -e '|' file | wc -l").as_deref(),
            Some("grep -e '|' file\n  | wc -l")
        );
        // the inner command's joint is the inner command's, and stays on the line it is in
        assert_eq!(joints("echo $(date | tr -d '\\n')"), None);
        assert_eq!(joints("echo `date | wc -c`"), None);
    }

    /// A command this cannot read to the end is one it does not touch.
    #[test]
    fn an_unreadable_command_is_drawn_as_it_arrived() {
        assert_eq!(joints("echo 'unterminated | still going"), None);
        assert_eq!(joints("echo \"unterminated && more"), None);
        assert_eq!(joints("make | tail -1 $(echo"), None);
        assert_eq!(joints("make | tail -1 \\"), None);
    }

    /// The separator somebody wrote at the end stays where they wrote it.
    #[test]
    fn a_trailing_separator_does_not_get_a_line_of_its_own() {
        assert_eq!(
            joints("cd src; make;").as_deref(),
            Some("cd src\n  ; make;")
        );
        // and a `case` arm's `;;` is not a separator at all
        assert_eq!(joints("case $x in a) echo one;; esac"), None);
    }
}
