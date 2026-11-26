//! The `/` filter over a pane's rows.
//!
//! note: fuzzy rather than substring, and the matcher is Helix's, because that is where the
//! expectation comes from: people type `mreq` for `model.requested` and `attr/dep` for a path, and
//! a substring search answers neither. `nucleo-matcher` rather than `nucleo` - the full crate is a
//! worker pool and an injector for streaming millions of candidates into a picker, and what is
//! being filtered here is a few hundred rows that are already in memory.
//!
//! note: built without the terminal too, because a served session answers a browser's filter with
//! it (see `Command::Search`), and two filters over one context that disagreed about what matches
//! would leave somebody unsure which of them to believe.
//!
//! note: the filter only exists while the box is on the screen. Closing it clears it, which keeps
//! a pane from lying: a filter that outlived its box would leave a window quietly showing four of
//! hundreds of rows with nothing on screen saying why.

use std::{collections::HashMap, sync::Arc};

use nachalnik::{ContextId, ContextItem};
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
    /// note: a byte offset rather than a character index, because every use of it is a slice of the
    /// query and a character index would be converted at each one. The six methods that move it
    /// keep it on a character boundary, which is what makes those slices safe.
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    at: usize,
    /// The parsed form of it, rebuilt whenever the query changes.
    pattern: Pattern,
    /// note: kept rather than built per candidate, because a `Matcher` owns the slabs the
    /// algorithm works in and building one per row would be the allocation the whole crate exists
    /// to avoid. Behind a lock because scoring takes `&mut` and the panes only ever ask through
    /// `&App`; it is uncontended - one thread draws.
    matcher: Mutex<Matcher>,
    /// What this query said about each context item, and the item it said it about.
    ///
    /// note: the context pane is filtered on every frame, and matching an item means the whole of
    /// its text - which, over a long session redrawn several times a second while a turn runs,
    /// cost more than the rest of the frame. An answer stands for as long as the query and the
    /// item do: the item is held and compared by pointer, since a changed item is a new one, and
    /// the query changing empties this.
    matched: Mutex<HashMap<ContextId, (Arc<ContextItem>, bool)>>,
}

impl Search {
    /// An open, empty search box.
    pub fn new() -> Self {
        Self {
            query: String::new(),
            at: 0,
            pattern: Pattern::default(),
            matcher: Mutex::new(Matcher::new(Config::DEFAULT)),
            matched: Mutex::default(),
        }
    }

    /// A search for `query`, as though it had been typed into the box.
    pub fn of(query: &str) -> Self {
        let mut search = Self::new();
        search.query = query.to_owned();
        search.at = search.query.len();
        search.reparse();

        search
    }

    /// The items of these that match, by identifier, in the order they came.
    pub fn found<'a>(
        &self,
        items: impl IntoIterator<Item = &'a Arc<ContextItem>>,
    ) -> Vec<ContextId> {
        items
            .into_iter()
            .filter(|item| self.matches_item(item, item_text))
            .map(|item| item.id)
            .collect()
    }

    #[cfg(feature = "tui")]
    /// Adds a character where the cursor is.
    pub fn push(&mut self, c: char) {
        self.query.insert(self.at, c);
        self.at += c.len_utf8();
        self.reparse();
    }

    #[cfg(feature = "tui")]
    /// Removes the character before the cursor; leaves an empty query empty.
    pub fn backspace(&mut self) {
        let Some(start) = self.before() else {
            return;
        };
        self.query.remove(start);
        self.at = start;
        self.reparse();
    }

    #[cfg(feature = "tui")]
    /// Removes the character under the cursor; at the end of the query, nothing.
    ///
    /// note: distinct from `backspace` only because there is a cursor for something to be under.
    pub fn delete(&mut self) {
        if self.at < self.query.len() {
            self.query.remove(self.at);
            self.reparse();
        }
    }

    #[cfg(feature = "tui")]
    /// Moves the cursor one character towards the start.
    pub fn left(&mut self) {
        if let Some(start) = self.before() {
            self.at = start;
        }
    }

    #[cfg(feature = "tui")]
    /// Moves the cursor one character towards the end.
    pub fn right(&mut self) {
        if let Some(c) = self.query[self.at..].chars().next() {
            self.at += c.len_utf8();
        }
    }

    #[cfg(feature = "tui")]
    /// Moves the cursor to the start of the query.
    pub fn home(&mut self) {
        self.at = 0;
    }

    #[cfg(feature = "tui")]
    /// Moves the cursor to the end of the query.
    pub fn end(&mut self) {
        self.at = self.query.len();
    }

    #[cfg(feature = "tui")]
    /// The query on either side of the cursor, for drawing.
    ///
    /// note: two slices rather than the offset, so that the only place that knows the cursor is a
    /// byte index is this file. A caller given the offset would have to trust that it lands on a
    /// character boundary; given the halves, it cannot get that wrong.
    pub fn parts(&self) -> (&str, &str) {
        self.query.split_at(self.at)
    }

    #[cfg(feature = "tui")]
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
        self.matched.get_mut().clear();
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

    /// Whether a context item matches, on the text `text` makes of it, asked once per item.
    ///
    /// note: `item_text` is the text everything here matches on; the parameter is for tests.
    pub fn matches_item(
        &self,
        item: &Arc<ContextItem>,
        text: impl FnOnce(&ContextItem) -> String,
    ) -> bool {
        if let Some((seen, matched)) = self.matched.lock().get(&item.id)
            && Arc::ptr_eq(seen, item)
        {
            return *matched;
        }
        let matched = self.matches(&text(item));
        self.matched
            .lock()
            .insert(item.id, (Arc::clone(item), matched));

        matched
    }
}

/// What a search over the context matches an item on.
///
/// note: the whole of what the item holds, not just the preview line, which would miss a word
/// rarely on the first line. The row still shows its preview; the match may be about more than the
/// row can show. That includes what a reply thought and the calls it made, wherever the turn keeps
/// them: they are drawn when the row is opened, and a reply found by its reasoning is the one
/// somebody remembered the model working something out in.
///
/// note: the kind column, too. Without it, `/tool_result` would match only words in somebody's
/// *content*. `ContextKind::name` is used, so what is typed is what the column shows, and it is
/// matched whether or not the column is drawn: it is dropped below 84 columns, and a filter that
/// found less on a narrow terminal would be a worse surprise.
///
/// note: the state is not in here. It is a mark on the row, not a word, so nothing would be typed
/// to match it; `/exclude state:excluded` is the language for that question.
pub fn item_text(item: &ContextItem) -> String {
    let mut text = format!(
        "{} {} {}",
        item.label,
        item.kind.name(),
        item.content.to_text()
    );
    for thought in item.thinking() {
        text.push('\n');
        text.push_str(&thought.to_text());
    }
    for call in item.calls() {
        text.push('\n');
        text.push_str(&format!("{}({})", call.tool, call.args));
    }

    text
}

impl Default for Search {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "tui"))]
mod tests {
    use super::*;

    /// An item changed under a standing query is matched again, not answered from before.
    #[test]
    fn a_changed_item_is_matched_again() {
        let mut search = Search::new();
        for c in "needle".chars() {
            search.push(c);
        }
        let text = |item: &ContextItem| item.content.to_text().into_owned();

        let mut item = ContextItem::user("a needle in here");
        item.id = ContextId(1);
        let before = Arc::new(item.clone());
        assert!(search.matches_item(&before, text));

        item.content = "nothing now".into();
        let after = Arc::new(item);
        assert!(!search.matches_item(&after, text));

        // and a new query forgets what the old one said
        for _ in 0.."needle".len() {
            search.backspace();
        }
        for c in "nothing".chars() {
            search.push(c);
        }
        assert!(search.matches_item(&after, text));
    }
}
