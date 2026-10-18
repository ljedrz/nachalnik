//! A markdown table, measured and drawn.
//!
//! note: separate from [`super::markdown`] because a table's width must be decided before anything
//! is drawn: every cell in a column is measured, the columns are fitted to the available width, and
//! only then is a row a row. The chunker decides whether a block is a table.

use ratatui::text::{Line, Span};

use crate::ui::{markdown::markdown, text::refit};

use super::faint;

/// A markdown pipe table, as the cells it is made of.
pub(super) struct Table {
    /// The header row.
    head: Vec<String>,
    /// Everything under it.
    body: Vec<Vec<String>>,
    /// Which way each column is aligned, as its delimiter's colons asked.
    align: Vec<Align>,
}

/// Which end of its column a cell sits at.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Align {
    /// The default, and where prose belongs.
    Left,
    /// `:---:`.
    Centre,
    /// `---:`, which is where a column of numbers belongs.
    Right,
}

/// Reads a pipe table from lines, or gives up.
///
/// note: the second line, a row of dashes between pipes, marks the table. The header decides how
/// many columns there are. A row with more cells has the extras dropped; a row with fewer is
/// padded, which keeps the box rectangular.
pub(super) fn table(block: &str) -> Option<Table> {
    /// The cells of one row, without the delimiter pipes.
    ///
    /// note: one delimiter off each end rather than every pipe there, and a split that knows what
    /// `\|` is. Trimming them all would take the empty first cell of `|| a |` away with the
    /// delimiter, and splitting on every pipe would read an escaped one as a column - so a row
    /// with a pipe in a cell would move every value after it one column left, and `resize` would
    /// drop whatever fell off the end. A table drawn from the model's own answer has to say what
    /// the answer said.
    ///
    /// note: a trailing `\|` is a pipe in the last cell, not the delimiter, so the end is stripped
    /// only when the pipe is not escaped.
    fn cells(line: &str) -> Vec<String> {
        let line = line.trim();
        let line = line.strip_prefix('|').unwrap_or(line);
        let line = match line.ends_with("\\|") {
            true => line,
            false => line.strip_suffix('|').unwrap_or(line),
        };

        let mut cells = Vec::new();
        let mut cell = String::new();
        let mut escaped = false;
        for c in line.chars() {
            match (escaped, c) {
                (true, '|') => cell.push('|'),
                (true, other) => {
                    cell.push('\\');
                    cell.push(other);
                }
                (false, '\\') => {
                    escaped = true;
                    continue;
                }
                (false, '|') => cells.push(std::mem::take(&mut cell).trim().to_owned()),
                (false, other) => cell.push(other),
            }
            escaped = false;
        }
        if escaped {
            cell.push('\\');
        }
        cells.push(cell.trim().to_owned());

        cells
    }

    let mut lines = block.lines().filter(|line| !line.trim().is_empty());
    let head = cells(lines.next()?);
    let delimiter = lines.next()?;
    if !is_delimiter(delimiter) {
        return None;
    }
    // the colons say which way a column reads, and a column of numbers read from the left is the
    // one thing a table can get wrong that a list of lines does not
    let mut align: Vec<Align> = cells(delimiter)
        .iter()
        .map(|rule| match (rule.starts_with(':'), rule.ends_with(':')) {
            (true, true) => Align::Centre,
            (false, true) => Align::Right,
            _ => Align::Left,
        })
        .collect();
    align.resize(head.len(), Align::Left);

    let body = lines
        .map(|line| {
            let mut row = cells(line);
            row.resize(head.len(), String::new());

            row
        })
        .collect();

    Some(Table { head, body, align })
}

/// Whether a line is the `| --- | :-: |` row that makes the line above it a header.
pub(super) fn is_delimiter(line: &str) -> bool {
    let trimmed = line.trim();

    trimmed.contains('-')
        && trimmed.starts_with(['|', ':', '-'])
        && trimmed
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

/// Draws a table into the width there is, wrapping inside the cells rather than around them.
///
/// note: this is why tables are chunked out rather than left to the markdown renderer. The renderer
/// lays a table out at whatever width its contents want and returns rows of box characters; a row
/// wider than the window would wrap like a sentence, with half a border on the next line, and the
/// table would come apart. A table is a fixed shape; when it does not fit, the columns give way,
/// and only this end knows what they have to fit into.
///
/// note: the cells go through the markdown renderer first, so `\`Tool\`` is measured and drawn as
/// `Tool` in the inline-code colour. Measuring the raw text would spend two columns per cell on
/// punctuation that is never drawn.
pub(super) fn draw_table(table: &Table, width: usize) -> Vec<Line<'static>> {
    /// The narrowest a column may be before another column gives way.
    ///
    /// note: eight, because that is the floor `refit` wraps to; a smaller value would make a column
    /// narrower than the lines in it.
    const FLOOR: usize = 8;

    let columns = table.head.len();
    if columns == 0 {
        return Vec::new();
    }
    let head: Vec<Line<'static>> = table.head.iter().map(|cell| inline(cell)).collect();
    let body: Vec<Vec<Line<'static>>> = table
        .body
        .iter()
        .map(|row| row.iter().map(|cell| inline(cell)).collect())
        .collect();

    // every column starts as wide as its widest cell; the widest columns are shaved until the table
    // fits. Shaving evenly would squeeze a `yes`/`no` column for a sentence column
    let mut widths: Vec<usize> = (0..columns)
        .map(|n| {
            std::iter::once(&head)
                .chain(body.iter())
                .filter_map(|row| row.get(n))
                .map(Line::width)
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    let room = width.saturating_sub(columns * 3 + 1);
    while widths.iter().sum::<usize>() > room {
        let widest = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| **w)
            .map(|(n, _)| n)
            .unwrap_or(0);
        if widths[widest] <= FLOOR {
            break;
        }
        widths[widest] -= 1;
    }

    let rule = |left: &str, mid: &str, right: &str| {
        let mut out = left.to_owned();
        for (n, w) in widths.iter().enumerate() {
            out.push_str(&"─".repeat(w + 2));
            out.push_str(match n + 1 == columns {
                true => right,
                false => mid,
            });
        }

        Line::styled(out, faint())
    };

    let mut lines = vec![rule("┌", "┬", "┐")];
    lines.extend(row(&head, &widths, &table.align, true));
    lines.push(rule("├", "┼", "┤"));
    for cells in &body {
        lines.extend(row(cells, &widths, &table.align, false));
    }
    lines.push(rule("└", "┴", "┘"));

    lines
}

/// One cell's markdown, rendered to the spans it is drawn as.
fn inline(cell: &str) -> Line<'static> {
    tui_markdown::from_str_with_options(cell, &markdown())
        .lines
        .first()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| Span::styled(span.content.to_string(), span.style))
                .collect::<Vec<_>>()
                .into()
        })
        .unwrap_or_default()
}

/// One row of a table, as the several screen lines its cells may need.
pub(super) fn row(
    cells: &[Line<'static>],
    widths: &[usize],
    align: &[Align],
    head: bool,
) -> Vec<Line<'static>> {
    let wrapped: Vec<Vec<Line<'static>>> = widths
        .iter()
        .enumerate()
        .map(|(n, w)| match cells.get(n) {
            Some(cell) => refit(cell, *w),
            None => vec![Line::default()],
        })
        .collect();
    let tall = wrapped.iter().map(Vec::len).max().unwrap_or(1);

    (0..tall)
        .map(|at| {
            let mut spans = Vec::with_capacity(widths.len() * 3 + 1);
            for (n, w) in widths.iter().enumerate() {
                spans.push(Span::styled("│ ", faint()));
                let line = wrapped[n].get(at);
                let used = line.map(Line::width).unwrap_or(0);
                let spare = w.saturating_sub(used);
                let before = match align.get(n).copied().unwrap_or(Align::Left) {
                    Align::Left => 0,
                    Align::Centre => spare / 2,
                    Align::Right => spare,
                };
                spans.push(Span::raw(" ".repeat(before)));
                spans.extend(line.into_iter().flat_map(|line| &line.spans).map(|span| {
                    Span::styled(
                        span.content.to_string(),
                        match head {
                            true => span.style.bold(),
                            false => span.style,
                        },
                    )
                }));
                spans.push(Span::raw(" ".repeat(spare - before + 1)));
            }
            spans.push(Span::styled("│", faint()));

            Line::from(spans)
        })
        .collect()
}
