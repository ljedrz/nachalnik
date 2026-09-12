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
            pattern: Pattern::default(),
            matcher: Mutex::new(Matcher::new(Config::DEFAULT)),
        }
    }

    /// Adds a character to the query.
    pub fn push(&mut self, c: char) {
        self.query.push(c);
        self.reparse();
    }

    /// Removes the last character; leaves an empty query empty.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.reparse();
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
