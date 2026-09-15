//! The `/` filter over a pane's rows.
//!
//! note: fuzzy rather than substring, and the matcher is Helix's, because that is where the
//! expectation comes from: people type `mreq` for `model.requested` and `attr/dep` for a path, and
//! a substring search answers neither. `nucleo-matcher` rather than `nucleo` - the full crate is a
//! worker pool and an injector for streaming millions of candidates into a picker, and what is
//! being filtered here is a few hundred rows that are already in memory.
//!
//! note: the filter only exists while the box is on the screen. Closing it clears it, which is the
//! one rule that keeps a pane from lying: a filter that outlived its box would leave a window
//! quietly showing four of eight hundred events with nothing on screen saying why.

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use parking_lot::Mutex;

/// What has been typed into the search box, and the matcher that answers with it.
pub struct Search {
    /// The query, as typed.
    pub query: String,
    /// Where the next character goes, as a byte offset into [`Search::query`].
    ///
    /// note: a byte offset rather than a character index, because every use of it is a slice of
    /// the query and a character index would be converted at each one. It is kept on a character
    /// boundary by the only four methods that move it, which is what makes those slices safe.
    at: usize,
    /// The parsed form of it, rebuilt whenever the query changes.
    pattern: Pattern,
    /// note: kept rather than built per candidate, because a `Matcher` owns the slabs the
    /// algorithm works in and building one per row would be the allocation the whole crate exists
    /// to avoid. Behind a lock because scoring takes `&mut` and the panes only ever ask through
    /// `&App`; it is uncontended - one thread draws.
    matcher: Mutex<Matcher>,
}

impl Search {
    /// An open, empty search box.
    pub fn new() -> Self {
        Self {
            query: String::new(),
            at: 0,
            pattern: Pattern::default(),
            matcher: Mutex::new(Matcher::new(Config::DEFAULT)),
        }
    }

    /// Adds a character where the cursor is.
    pub fn push(&mut self, c: char) {
        self.query.insert(self.at, c);
        self.at += c.len_utf8();
        self.reparse();
    }

    /// Removes the character before the cursor; leaves an empty query empty.
    pub fn backspace(&mut self) {
        let Some(start) = self.before() else {
            return;
        };
        self.query.remove(start);
        self.at = start;
        self.reparse();
    }

    /// Removes the character under the cursor; at the end of the query, nothing.
    ///
    /// note: the key that only makes sense once there is a cursor to be behind something. Before
    /// that, `delete` and `backspace` would have been two names for the same rub-out.
    pub fn delete(&mut self) {
        if self.at < self.query.len() {
            self.query.remove(self.at);
            self.reparse();
        }
    }

    /// Moves the cursor one character towards the start.
    pub fn left(&mut self) {
        if let Some(start) = self.before() {
            self.at = start;
        }
    }

    /// Moves the cursor one character towards the end.
    pub fn right(&mut self) {
        if let Some(c) = self.query[self.at..].chars().next() {
            self.at += c.len_utf8();
        }
    }

    /// The query on either side of the cursor, for whatever is drawing it.
    ///
    /// note: two slices rather than the offset, so that the one place that knows the cursor is a
    /// byte index is this file. A caller handed the number would have to trust it lands on a
    /// character boundary; handed the halves, it cannot get that wrong.
    pub fn parts(&self) -> (&str, &str) {
        self.query.split_at(self.at)
    }

    /// Where the character before the cursor starts, if there is one.
    fn before(&self) -> Option<usize> {
        self.query[..self.at]
            .chars()
            .next_back()
            .map(|c| self.at - c.len_utf8())
    }

    fn reparse(&mut self) {
        // note: `Smart` on both, which is what makes a lowercase query case-insensitive and an
        // uppercase one exact. Typing `Model` to mean `model.requested` and getting nothing would
        // be the surprise; typing `EndTurn` and getting every `endturn` would be the other one
        self.pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
    }

    /// Whether a row matches. An empty query matches everything, so opening the box hides nothing.
    pub fn matches(&self, haystack: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }

        let mut buf = Vec::new();
        self.pattern
            .score(Utf32Str::new(haystack, &mut buf), &mut self.matcher.lock())
            .is_some()
    }
}

impl Default for Search {
    fn default() -> Self {
        Self::new()
    }
}
