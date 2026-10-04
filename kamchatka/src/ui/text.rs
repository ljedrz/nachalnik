//! Measuring text, and making it fit.
//!
//! note: nothing in here draws. They are the functions that answer "how many rows will this take"
//! and "what does this look like at 34 columns", which is what a screen made of nested
//! [`Rect`](ratatui::layout::Rect)s spends most of its time asking. Kept apart from the drawing for
//! the ordinary reason: they are the part that can be reasoned about without a terminal.

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
    // a word can be the empty string - that is what a run of spaces is made of - so "have I
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
/// `keep_truncated_output` keeps the whole of it - and a `grep` that wanders into a build
/// directory holds millions. Seven columns stop at `999,999`, and a wider figure does not widen
/// the column - `{:>7}` pads and never truncates, so it takes the columns it needs from its
/// neighbours: `0` and `1,400,000` would arrive as `01,400,000`, and the row would lose its last
/// two characters to the clip. Precision is the right thing to give up instead, because nobody
/// acts on the last three digits of something that is not going anywhere.
pub(super) fn fitted(n: usize, width: usize) -> String {
    match thousands(n) {
        exact if exact.len() <= width => exact,
        _ => compact(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drawn row as the one string a terminal is handed.
    fn drawn(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
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

        // the gap between two words is a column like any other, so a pair that fills the room
        // exactly stays on one row and a pair one column over does not - and the pair is not the
        // whole paragraph, so this is the arithmetic in the loop rather than the way out of it
        assert_eq!(fold("aaaaa bbbb", 10), ["aaaaa bbbb"]);
        assert_eq!(fold("aaaaa bbbbb", 10), ["aaaaa", "bbbbb"]);
        assert_eq!(fold("aaaaa bbbb cc", 10), ["aaaaa bbbb", "cc"]);
        assert_eq!(fold("aaaaa bbbbb cc", 10), ["aaaaa", "bbbbb cc"]);

        // a word too long for the row is broken rather than allowed to overflow, and the row it
        // lands on keeps what was already on it - the two halves of that are separate claims, and
        // a row written before a broken word is easy to lose
        let said = "alpha beta gamma deltadeltadelta epsilon zeta";
        let rows = fold(said, 12);
        assert_eq!(
            rows,
            ["alpha beta", "gamma", "deltadeltade", "lta epsilon", "zeta"]
        );
        assert!(rows.iter().all(|row| columns(row) <= 12), "{rows:?}");

        // a word as wide as the row is a row of its own, and there is no blank row above it: a
        // blank one is a line of nothing, and `wrapped` gives the first row a prefix rather than
        // the hanging indent, so it moves a whole paragraph across by the width of a marker
        assert_eq!(fold("aaaaaaaaaaaa bb", 12), ["aaaaaaaaaaaa", "bb"]);
        // two of them break at the room rather than run together and overflow it
        assert_eq!(
            fold("aaaaaaaaaaaa bbbbbbbbbbbb", 12),
            ["aaaaaaaaaaaa", "bbbbbbbbbbbb"]
        );
        // and a word that fits the row exactly is left whole rather than broken and rejoined
        assert_eq!(fold("abcde fghij", 5)[..], ["abcde", "fghij"]);
        assert!(fold("abcde fghij", 5).iter().all(|row| columns(row) <= 5));
        // and what was already on the row when a long word arrives is kept, not overwritten
        assert_eq!(
            fold("alpha deltadeltadelta", 12)[..],
            ["alpha", "deltadeltade", "lta"]
        );
    }

    /// A count is said in the band it falls in: as it is under a thousand, to a tenth of a thousand
    /// under ten thousand, in whole thousands under a million, and to a tenth of a million under ten
    /// million, where a `.0` is dropped.
    ///
    /// note: each band is here because the ones either side of it say the same count differently,
    /// so a band that goes missing is a number said in some other band's words.
    #[test]
    fn a_round_figure_is_said_in_as_few_characters_as_it_can_be() {
        for (n, said) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1.0k"),
            (4_100, "4.1k"),
            (9_999, "10.0k"),
            (10_000, "10k"),
            (131_000, "131k"),
            (999_999, "999k"),
            (1_048_576, "1M"),
            (4_100_000, "4.1M"),
            (12_000_000, "12M"),
        ] {
            assert_eq!(compact(n), said, "{n}");
        }
    }

    /// A list item's continuation hangs under its marker, and text that only looks like one does not.
    ///
    /// note: `- ` and `1. ` are indentation as far as reading it goes, so the marker is as wide as
    /// it is drawn, however many digits the number runs to. The two that are *not* markers are
    /// worth naming, because a rule that says yes to either hangs a row of ordinary prose under
    /// something that is not there: digits with no `. ` or `) ` behind them, and a `. ` that never
    /// had a number in front of it.
    #[test]
    fn a_list_items_continuation_hangs_under_its_marker() {
        let rest = |line: &str| -> Vec<String> {
            refit(&Line::raw(line), 20)
                .iter()
                .map(|row| drawn(row).trim_end().to_owned())
                .skip(1)
                .collect()
        };
        let said = "alpha bravo charlie delta echo foxtrot";

        // The hang is as wide as the marker is drawn, and these are the markers plus the
        // two things that only look like one.
        let hang = |marker: &str| -> usize {
            refit(&Line::raw(format!("{marker}{said}")), 20)[1]
                .spans
                .iter()
                .take_while(|span| span.content.as_ref().trim().is_empty())
                .map(|span| columns(span.content.as_ref()))
                .sum()
        };
        for marker in ["- ", "* ", "+ "] {
            assert_eq!(hang(marker), 2, "{marker}");
        }
        for (marker, wide) in [("1. ", 3), ("12. ", 4), ("12) ", 4), ("100. ", 5)] {
            assert_eq!(hang(marker), wide, "{marker}");
        }

        // and it is drawn: the continuation of `1. ` sits under its item rather than
        // back at the margin
        assert_eq!(
            rest(&format!("1. {said}"))[..],
            ["   charlie delta", "   echo foxtrot"]
        );
        assert_eq!(
            rest(&format!("12. {said}"))[..],
            ["    charlie delta", "    echo foxtrot"]
        );

        // digits that are not a marker, and a marker that is not numbered, both continue at the
        // margin: a rule that says yes to either hangs a row of ordinary prose under nothing
        assert_eq!(
            rest("2024 is the year alpha bravo charlie")[..],
            ["alpha bravo charlie"]
        );
        assert_eq!(
            rest(&format!(". {said}"))[..],
            ["charlie delta echo", "foxtrot"]
        );
    }

    /// A pane is wrapped to the width it was given, not to the floor under it.
    ///
    /// note: `width` is the pane and the prefix is drawn in front of the first row, so the floor
    /// is the prefix's own width plus twelve - the width of the pane is never below it, and a
    /// prefix of two cells and one of twenty are floored differently.
    #[test]
    fn a_narrow_pane_is_wrapped_to_its_own_width() {
        let said = "alpha bravo charlie delta echo foxtrot";

        assert_eq!(
            wrapped(said, 20, "│ ")[..],
            ["│ alpha bravo", "  charlie delta echo", "  foxtrot"]
        );
        // the same text in a pane past the floor, which is where the floor stops being the answer
        assert_eq!(
            wrapped(said, 30, "│ ")[..],
            ["│ alpha bravo charlie delta", "  echo foxtrot"]
        );
    }

    /// The rows `refit` gives back, as the strings a screen would draw.
    fn refit_rows(line: &Line<'_>, width: usize) -> Vec<String> {
        refit(line, width)
            .iter()
            .map(|row| row.to_string())
            .collect()
    }

    /// A word wider than the room the hang leaves is cut to that room, not to the whole pane.
    ///
    /// note: the continuation carries the indent, so a word cut to the full width would overflow
    /// the row by however much the indent is. Twelve columns with two of them spent leaves ten,
    /// and twelve is what `width + hang` would have cut it to instead.
    #[test]
    fn a_refit_line_cuts_a_word_to_the_room_left_by_the_hang() {
        assert_eq!(
            refit_rows(&Line::raw("  alpha aaaaaaaaaaaa beta"), 12),
            ["  alpha ", "  aaaaaaaaaa", "  aa beta"]
        );
    }

    /// A word that lands a row on the width exactly stays on it, and the word after it starts the
    /// row below.
    ///
    /// note: a word is measured with its trailing space trimmed off, because that space is the
    /// gap to what follows and a row ending on one is not full. `abcdef` is six and `ghijk` is
    /// six, so the row fills twelve exactly - which is where `>` and `>=` part company.
    #[test]
    fn a_refit_line_leaves_a_row_alone_when_the_next_word_fills_it_exactly() {
        assert_eq!(
            refit_rows(&Line::raw("abcdef ghijk mno"), 12),
            ["abcdef ghijk ", "mno"]
        );
    }
}
