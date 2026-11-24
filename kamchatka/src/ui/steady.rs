//! A backend wrapper that moves the cursor after every glyph whose width depends on the terminal.
//!
//! note: ratatui writes a run of changed cells and moves the cursor only where the run breaks, so
//! every cell in a run is drawn where the terminal's cursor happens to be. A cell ratatui counted
//! as two wide breaks the run, because the cell after it is skipped. A cell counted as one and
//! drawn as two does not, and from there to the end of the run everything is a column to the
//! right of where it was put, until the right edge wraps it onto the next row's first column - the
//! left border of the chat tab. A web search result with a bare `🌶` in it, which `unicode-width`
//! counts as one and some terminals draw as an emoji, turned that border into a column of `5`s
//! and chillies until the window was redrawn.
//!
//! note: the widths themselves cannot be fixed from here. Which glyphs a terminal draws wide
//! depends on the terminal, its version, its settings and its fonts, and nothing tells a program
//! which. What can be fixed is how far the error spreads: after any cell holding a glyph terminals
//! disagree about, the cursor is moved explicitly before the next cell. If the width was wrong, the
//! glyph overlaps the cell after it, and the rest of the row is unaffected.

use ratatui::{
    backend::{Backend, ClearType, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};

/// A [`Backend`] that moves the cursor explicitly after every glyph whose width depends on the
/// terminal; see the module.
#[derive(Debug)]
pub struct Steady<B>(pub B);

impl<B: Backend> Backend for Steady<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        // note: each call to the backend underneath starts by moving the cursor, so a call per run
        // is a move after every glyph that ends one, and the backend's own colour and style
        // tracking is left as it is
        let mut content = content.peekable();
        while content.peek().is_some() {
            self.0.draw(Through {
                inner: &mut content,
                ended: false,
            })?;
        }

        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.0.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.0.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.0.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.0.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.0.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.0.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.0.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush()
    }
}

/// The cells up to and including the next one that holds an unsteady glyph.
struct Through<'r, I: Iterator> {
    inner: &'r mut std::iter::Peekable<I>,
    ended: bool,
}

impl<'a, I> Iterator for Through<'_, I>
where
    I: Iterator<Item = (u16, u16, &'a Cell)>,
{
    type Item = (u16, u16, &'a Cell);

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        let cell = self.inner.next()?;
        self.ended = unsteady(cell.2.symbol());

        Some(cell)
    }
}

/// Whether terminals disagree about how wide `symbol` is.
///
/// note: by range rather than by property, because no crate here carries the emoji tables, and a
/// range that takes in a few glyphs every terminal agrees about only costs those a cursor move.
/// What it has to take in is everything that can be drawn as an emoji while counted as text, the
/// selectors and joiners that change that, the private use areas a patched font fills with icons,
/// and the line and paragraph separators. Box drawing is left out: it is every border on the
/// screen, nothing draws it wide, and a move after each of its cells would double what a redraw
/// writes.
pub(crate) fn unsteady(symbol: &str) -> bool {
    !symbol.is_ascii()
        && symbol.chars().any(|c| {
            matches!(
                c as u32,
                // zero width joiner, and the line and paragraph separators
                0x200D | 0x2028 | 0x2029
                // letterlike symbols, arrows, technical, enclosed alphanumerics: `ℹ`, `↔`, `⌚`, `Ⓜ`
                | 0x2100..=0x24FF
                // geometric shapes, miscellaneous symbols, dingbats, more arrows: `▶`, `☀`, `✔`, `⬆`
                | 0x25A0..=0x2BFF
                | 0x3030 | 0x303D | 0x3297 | 0x3299
                // private use
                | 0xE000..=0xF8FF
                | 0xF0000..
                // variation selectors
                | 0xFE00..=0xFE0F
                // every emoji block, and the mahjong and playing cards before them
                | 0x1F000..=0x1FAFF
            )
        })
}

#[cfg(test)]
mod tests {
    use ratatui::{
        Terminal,
        backend::TestBackend,
        buffer::Buffer,
        layout::Rect,
        widgets::{Paragraph, Widget as _},
    };

    use super::*;

    /// The glyphs of the result that broke the border, and the ones the screen draws itself.
    #[test]
    fn the_glyphs_terminals_disagree_about_are_unsteady() {
        for glyph in [
            "\u{1F336}",
            "\u{1F336}\u{FE0F}",
            "\u{2028}",
            "\u{F007}",
            "\u{2714}",
            "\u{1F607}",
        ] {
            assert!(unsteady(glyph), "{glyph:?}");
        }
        for glyph in ["a", "é", "€", "│", "─", "·", "…", "一"] {
            assert!(!unsteady(glyph), "{glyph:?}");
        }
    }

    /// Each unsteady glyph ends a run, so the cell after it is written at a position of its own,
    /// and every cell still arrives once and in order.
    #[test]
    fn a_run_ends_at_every_unsteady_glyph() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 1));
        Paragraph::new("a\u{1F336}b\u{F007}c").render(buffer.area, &mut buffer);
        let cells: Vec<_> = buffer
            .content
            .iter()
            .enumerate()
            .map(|(x, cell)| (x as u16, 0, cell))
            .collect();

        let mut runs: Vec<Vec<&str>> = Vec::new();
        let mut content = cells.iter().copied().peekable();
        while content.peek().is_some() {
            let through = Through {
                inner: &mut content,
                ended: false,
            };
            runs.push(through.map(|(_, _, cell)| cell.symbol()).collect());
        }

        assert_eq!(
            runs,
            [
                vec!["a", "\u{1F336}"],
                vec!["b", "\u{F007}"],
                vec!["c", " ", " ", " "],
            ]
        );
    }

    /// Drawn through it, a frame comes out the same as through the backend underneath.
    #[test]
    fn a_frame_drawn_through_it_is_the_same_frame() {
        let mut terminal = Terminal::new(Steady(TestBackend::new(12, 2))).unwrap();
        terminal
            .draw(|frame| {
                Paragraph::new("x\u{1F336}y\u{2028}z\n\u{1F336}\u{FE0F}ok")
                    .render(frame.area(), frame.buffer_mut())
            })
            .unwrap();

        let mut expected = Buffer::empty(Rect::new(0, 0, 12, 2));
        Paragraph::new("x\u{1F336}y\u{2028}z\n\u{1F336}\u{FE0F}ok")
            .render(expected.area, &mut expected);
        terminal.backend().0.assert_buffer(&expected);
    }
}
