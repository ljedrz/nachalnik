//! What the keys do.
//!
//! note: one handler per tab rather than one `match` over every key, because the same key means
//! different things on different tabs and a single table of them was the file's worst argument
//! with itself. [`super::App::on_key`] is the dispatcher, and it is next door with the rest of
//! the public surface.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nachalnik::{ContextItem, ContextState, Grant, Verdict};
use ratatui_textarea::CursorMove;

use super::{
    App, Focus, Overlay, Page, Search, Speaker, Tab,
    text::{beyond_a_prompt, projected, stored, whole},
};

/// How many lines `pgup` and `pgdn` move an overlay.
const PAGE: usize = 20;

impl App {
    /// The tab after the open one, wrapping round.
    pub(super) fn next_tab(&self) -> Tab {
        let at = Tab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);

        Tab::ALL[(at + 1) % Tab::ALL.len()]
    }

    /// Moves the keys between the prompt and whatever else is asking for them.
    pub(super) fn flip_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Input => Focus::Body,
            Focus::Body => Focus::Input,
        };
    }

    /// Keys that belong to the prompt.
    pub(super) async fn input_key(&mut self, key: KeyEvent) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            // the prompt is editing an item rather than composing a message; enter commits it and
            // escape puts it back the way it was
            KeyCode::Enter if !alt && self.editing.is_some() => {
                let text = self.input.lines().join("\n");
                self.clear_input();
                self.commit_edit(&text);
            }
            KeyCode::Esc if self.editing.is_some() => {
                self.clear_input();
                self.editing = None;
                self.focus = Focus::Body;
            }
            // enter sends, because that is what a prompt is for; a newline is alt+enter, which
            // is the one every terminal agrees on
            KeyCode::Enter if !alt => {
                let line = self.input.lines().join("\n").trim().to_owned();
                if line.is_empty() {
                    return;
                }
                self.clear_input();
                self.submit(&line).await;
            }
            KeyCode::Enter => {
                self.input.insert_newline();
            }
            // an empty prompt has nothing to move a cursor around in and nothing to lose, so
            // `up` there is the one that puts the last line back. Anywhere else it is still the
            // key that moves the cursor and scrolls at the top, which is what stops this from
            // taking a gesture away: nothing that used to do something does something else now
            KeyCode::Up if self.input.lines().iter().all(|line| line.is_empty()) => self.put_back(),
            // and `down` undoes it, while the prompt still holds exactly what `up` put there.
            // Typed over, it is a message somebody is writing and not a recall any more, so the
            // key goes back to being the one that moves the cursor
            KeyCode::Down if self.recalled.as_deref() == Some(self.draft().as_str()) => {
                self.clear_input()
            }
            // at the edges of the prompt, the arrows go on to the conversation
            KeyCode::Up if self.input.cursor().0 == 0 => self.scroll_by(-1),
            KeyCode::Down if self.input.cursor().0 + 1 == self.input.lines().len() => {
                self.scroll_by(1)
            }
            KeyCode::PageUp => self.scroll_by(-(self.viewport as isize / 2)),
            KeyCode::PageDown => self.scroll_by(self.viewport as isize / 2),
            _ => {
                self.input.input(key);
            }
        }
    }

    /// Puts the last line back in the prompt: the message still waiting, if one is, and otherwise
    /// a copy of the last line that was sent.
    ///
    /// note: two things reachable by one key, because to somebody pressing it they are one thing -
    /// *the last message* - and which of the two it is is not something to have to know. The
    /// difference is what happens to it: a message that is still waiting is **taken** out of the
    /// queue, since leaving it there would mean editing a copy of something that is still going to
    /// be sent as it was; one that has already gone is **copied**, since it is in the context and
    /// this is a way to read it back or send it again.
    ///
    /// note: one deep, deliberately. A prompt that walked back through a session is a second
    /// history beside the context tab, which already holds every message with more said about each
    /// of them than a prompt could show - and the thing that is actually wanted often enough to
    /// need a key is the last one.
    ///
    /// note: nothing is said out loud for the copy and a line is said for the take, because only
    /// one of them changes what the session is about to do. A message that stops waiting stops
    /// being drawn at the end of the conversation, and a row vanishing with no account of why is
    /// the thing this program does not do.
    pub(super) fn put_back(&mut self) {
        let waiting = self.typed_ahead.take();
        let Some(line) = waiting.clone().or_else(|| self.last_sent.clone()) else {
            return;
        };

        self.clear_input();
        self.input.insert_str(line.clone());
        self.input.move_cursor(CursorMove::End);
        // what `down` undoes, and only while the prompt still says exactly this
        self.recalled = Some(line);
        if waiting.is_some() {
            self.say(
                Speaker::Note,
                "taken back out of the queue; nothing is waiting now, and `enter` sends it again",
            );
        }
    }

    /// Empties the prompt, wherever what was in it has just gone.
    pub(super) fn clear_input(&mut self) {
        self.input.select_all();
        self.input.cut();
        self.input.move_cursor(CursorMove::End);
        // an empty prompt is not holding a recall, whatever it was holding a moment ago
        self.recalled = None;
    }

    /// Puts an edited item into the context in place of the one it came from.
    ///
    /// note: [`Kernel::replace`](nachalnik::Kernel::replace) rather than
    /// [`Kernel::supersede`](nachalnik::Kernel::supersede), so the item keeps its identifier, its
    /// kind, its state and its place in the conversation, and what it said before becomes a
    /// version page like every other rewrite. It used to supersede, which left a second row
    /// marked `~` saying what the `v1` page already says - and cost three things to keep upright.
    /// A state to carry over by hand, because a new item starts Active: an edit to a pruned one
    /// quietly came back into the request, and an archived one promoted the whole of an oversized
    /// output into it. A kind to rebuild whole, because an assistant turn carries its tool calls
    /// inside it and rebuilding it without them orphans their results. And a `replaces` hint, so
    /// the conversation could read the new words back into the old place. Replacing has none of
    /// those: there is nothing to carry over, because nothing moved.
    ///
    /// note: [`Kernel::supersede`](nachalnik::Kernel::supersede) is not the loser here. It is the
    /// right shape for a caller whose next round replaces the last while the earlier ones stay
    /// readable - `examples/panel.rs` in the runtime is exactly that - which is why
    /// [`super::App::in_order`] still places an item that carries the hint, and why a session
    /// saved before this changed still draws in the order the request has. It is not the shape of
    /// a person fixing a sentence.
    /// note: the whole of the act is [`App::revise`], which a client over [`crate::remote`] also
    /// commits through. What is left here is the half that belongs to the keys: taking the item
    /// out of `editing`, giving the focus back, and saying what happened on the chat - because a
    /// person who pressed `enter` is owed a sentence and a protocol is owed a return value.
    fn commit_edit(&mut self, text: &str) {
        let Some(id) = self.editing.take() else {
            return;
        };
        self.focus = Focus::Body;

        match self.revise(id, text, "edited at the terminal") {
            Ok(true) => {}
            Ok(false) => self.say(Speaker::Note, format!("[{id}] is unchanged")),
            Err(e) => self.say(Speaker::Error, e),
        }
    }

    /// Keys that belong to the context pane.
    pub(super) fn context_key(&mut self, key: KeyEvent, count: &str) {
        // what is on the screen, not what is in the context: with `f` on, the rows between two
        // items are gone and moving by one row has to mean the next row somebody can see
        let items = self.listed();
        if items.is_empty() {
            // there is nothing to pick, and the keys that are not about a row still have to work.
            // `f` puts the rows back; `u` and `U` are the way back from whatever emptied the pane,
            // which with `f` on is one keystroke away - hide the last row being sent and the row
            // goes, taking the key that would undo it. The way out was to press `f` first, which
            // nothing says anywhere
            match key.code {
                KeyCode::Char('f') => self.sending_only = false,
                KeyCode::Char('u') => {
                    let note = match self.kernel.undo() {
                        true => "undone",
                        false => "there is nothing to undo",
                    };
                    self.say(Speaker::Note, note);
                }
                KeyCode::Char('U') => {
                    let note = match self.kernel.redo() {
                        true => "redone",
                        false => "there is nothing to redo",
                    };
                    self.say(Speaker::Note, note);
                }
                _ => {}
            }

            return;
        }
        self.selected = self.selected.min(items.len() - 1);
        let picked = items[self.selected].clone();

        match key.code {
            KeyCode::Char('/') => self.search = Some(Search::new()),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(items.len() - 1)
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(self.viewport.max(2) / 2)
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + self.viewport.max(2) / 2).min(items.len() - 1)
            }
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            // `23G` goes to the item *numbered* 23 rather than the twenty-third row, because the
            // number in the first column is the one `/exclude` takes and the one every note names.
            // Bare `G` is the last item, as everywhere else
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = match count.parse::<u64>() {
                    Ok(id) => match items.iter().position(|item| item.id.0 == id) {
                        Some(at) => at,
                        // there is a difference between an item that does not exist and one this
                        // tab is not currently showing, and only one of them is somebody's typo
                        None => {
                            let note = match self.kernel.items().iter().any(|i| i.id.0 == id) {
                                true => {
                                    format!("item [{id}] is not being sent; `f` lists it again")
                                }
                                false => format!("there is no item [{id}]"),
                            };
                            self.say(Speaker::Note, note);
                            self.selected
                        }
                    },
                    Err(_) => items.len() - 1,
                }
            }
            KeyCode::Char(digit) if digit.is_ascii_digit() => {
                self.count = format!("{count}{digit}");
            }
            KeyCode::Esc => self.cancel_edit(),
            // changing what an item says, which is the verb the other keys were missing: `space`
            // and `p` decide whether the model reads it, and this decides what it reads
            //
            // note: what it reads, and only that. An item whose substance the prompt cannot hold
            // is refused here rather than opened - a tool call is on the item's kind and not in
            // its content, so a turn that is nothing but calls used to open an empty box titled
            // `editing [3]` over a row visibly holding one, and committing anything into it wrote
            // a sentence beside a call it had not touched. `beyond_a_prompt` is the whole of the
            // question; `enter` is still the way to read every face of an item this cannot rewrite
            KeyCode::Char('e') => match beyond_a_prompt(&picked) {
                // note: a panel over this tab rather than a line on the chat. Every other note
                // the pane raises goes to the conversation, which is right for something worth
                // finding later and wrong for the answer to a key just pressed: the tab it
                // appears on is not the tab somebody is looking at, so an `e` that refused read
                // as an `e` that did nothing at all. This one is about the row under it and is
                // gone on the next key
                Some(why) => self.preview(
                    format!("[{}] cannot be edited", picked.id),
                    format!(
                        "{why}\n\n`enter` reads every face of it · `space` takes it out of view"
                    ),
                ),
                None => {
                    self.clear_input();
                    self.input.insert_str(picked.content.to_text());
                    self.input.move_cursor(CursorMove::End);
                    self.editing = Some(picked.id);
                    self.focus = Focus::Input;
                }
            },
            // how much of an item the model gets, in three steps out and one back: all of it,
            // then a marker where it was, then nothing at all
            //
            // note: the middle step is the one worth having a key for. Taking a tool result out
            // makes the projector drop the call that asked for it, so the model reads a
            // conversation it never had; elided, the call keeps its answer and only the content
            // is gone. Which of the two somebody wants is not something this program can guess -
            // hiding a result outright is a fair thing to want - so it is a cycle rather than a
            // decision, the same way the permissions tab cycles a stance through three
            //
            // note: the ring itself is [`super::App::cycle`], because the page in
            // `examples/browser.html` puts a button on every row that does this and the notes it
            // writes are read by the model. Two callers writing their own words for one act is two
            // accounts of it in the context
            KeyCode::Char(' ') => {
                if let Err(e) = self.cycle(picked.id) {
                    self.say(Speaker::Error, e);
                }
            }
            KeyCode::Char('p') => {
                let to = match picked.state {
                    ContextState::Pinned => ContextState::Active,
                    _ => ContextState::Pinned,
                };
                self.kernel.set_state([picked.id], to, None);
            }
            // the row's own text, out of the window and onto the clipboard. A mouse over the chat
            // pane takes the frame with it, and `y` is the key a pager binds this to
            KeyCode::Char('y') => self.copy(picked.id),
            // a view, not a change: nothing is touched and nothing is logged. After a compaction
            // most of the list is items the model will never read again, and reading past them to
            // find the conversation is the thing this tab is for
            //
            // note: the selection follows the item rather than the row number, because the rows
            // under it have just moved. If what was picked is one of the ones now hidden, the
            // nearest row that is still there gets it
            KeyCode::Char('f') => {
                let was = picked.id;
                self.sending_only = !self.sending_only;
                let now = self.listed();
                self.selected = match now.iter().position(|item| item.id == was) {
                    Some(at) => at,
                    None => self.selected.min(now.len().saturating_sub(1)),
                };
            }
            KeyCode::Char('u') => {
                let note = match self.kernel.undo() {
                    true => "undone",
                    false => "there is nothing to undo",
                };
                self.say(Speaker::Note, note);
            }
            KeyCode::Char('U') => {
                let note = match self.kernel.redo() {
                    true => "redone",
                    false => "there is nothing to redo",
                };
                self.say(Speaker::Note, note);
            }
            KeyCode::Enter => {
                let title = format!(
                    "[{}] {} · {} · {} · {} tokens",
                    picked.id,
                    picked.label,
                    picked.kind.name(),
                    picked.state,
                    picked.tokens
                );
                let (pages, at) = self.faces(&picked);
                self.preview_pages(title, pages, at);
            }
            _ => {}
        }
    }

    /// Every face of a context item worth reading, and which of them to open on.
    ///
    /// note: the default is what the item says, except when that is not what the model reads. An
    /// elided item goes into the request as a marker, an excluded one does not go at all, and a
    /// tool result whose call has been taken out is repaired away though nothing on its row says
    /// so - all three are rows where the screen and the request disagree, and the disagreement is
    /// what somebody pressed enter to find. So the projection decides this, not the state.
    fn faces(&self, item: &ContextItem) -> (Vec<Page>, usize) {
        let projection = self.kernel.project();
        let reads_it = projection.included.contains(&item.id) && item.state.sends_content();
        let mut pages = vec![
            Page {
                name: "to the model".into(),
                body: projected(&projection, item.id),
            },
            Page {
                name: "as stored".into(),
                body: match item.meta.get("revised") {
                    // who rewrote it and why, which `context` records on the item itself; the trace
                    // has the rest, and this is the line that sends somebody to it
                    Some(revised) => format!(
                        "rewritten by `{}`: {}\n\n{}",
                        revised["by"].as_str().unwrap_or("something"),
                        revised["reason"].as_str().unwrap_or("no reason given"),
                        stored(item)
                    ),
                    None => stored(item),
                },
            },
        ];

        let history = self
            .versions
            .get(&item.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        // note: asked rather than worked out again. An undo puts an old content back and the
        // version it restored is then the current one too, so the last is dropped - and a browser
        // drawing a button that counts these has to reach the same number as the strip up here
        let keep = self.versions(item.id);
        for (n, was) in history.iter().enumerate().take(keep).rev() {
            pages.push(Page {
                name: format!("v{}", n + 1),
                body: format!(
                    "version {} of {}, before it was rewritten\n\n{}",
                    n + 1,
                    keep + 1,
                    whole(was)
                ),
            });
        }

        (pages, usize::from(reads_it))
    }

    /// Keys that belong to the permissions tab.
    pub(super) fn permissions_key(&mut self, key: KeyEvent) {
        let rows = self.permissions();
        if rows.is_empty() {
            return;
        }
        self.chosen = self.chosen.min(rows.len() - 1);
        let subject = rows[self.chosen].subject.clone();

        let decided = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.chosen = self.chosen.saturating_sub(1);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.chosen = (self.chosen + 1).min(rows.len() - 1);
                return;
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.chosen = 0;
                return;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.chosen = rows.len() - 1;
                return;
            }
            KeyCode::Char(' ') => self.policy.cycle(&subject),
            KeyCode::Char('a') => {
                self.policy.set(&subject, Verdict::Allow);
                Verdict::Allow
            }
            KeyCode::Char('n') => {
                self.policy.set(&subject, Verdict::Deny);
                Verdict::Deny
            }
            KeyCode::Char('r') | KeyCode::Backspace => {
                self.policy.set(&subject, Verdict::Ask);
                Verdict::Ask
            }
            _ => return,
        };

        // said out loud, because this is a decision about what may happen later and the tab it
        // was made on is not the one somebody will be looking at when it does
        self.say(
            Speaker::Note,
            match decided {
                Verdict::Allow => format!("`{subject}` runs without asking, from now on"),
                Verdict::Deny => format!("`{subject}` is refused, from now on"),
                Verdict::Ask => format!("`{subject}` is a question again"),
            },
        );
    }

    /// Keys that belong to the trace tab, which is a log and therefore worth reading backwards.
    /// Keys the search box wants while it is open; `false` to let the pane underneath have it.
    ///
    /// note: `up`, `down` and the paging are deliberately *not* taken. The point of filtering eight
    /// hundred events down to nine is to then read the nine, and a box that swallowed the scroll
    /// keys would mean closing the search - and so losing the filter - to look at what it found.
    /// `home` and `end` stay with the pane for the same reason and one more: `g` and `G`, which are
    /// what jumps to either end of a list everywhere else here, are letters, and while the box is
    /// open a letter is a letter. They are the only way left to reach the ends.
    ///
    /// note: `left` and `right` *are* taken, because neither pane uses them - the one place they
    /// mean something on the context tab is an open item, and an overlay takes the keys above
    /// this. So a query could only be amended by rubbing out everything back to the mistake, which
    /// is a poor trade for two keys that were doing nothing.
    pub(super) fn search_key(&mut self, key: KeyEvent) -> bool {
        let Some(search) = &mut self.search else {
            return false;
        };

        match key.code {
            KeyCode::Esc => {
                self.search = None;
                true
            }
            KeyCode::Backspace => {
                search.backspace();
                true
            }
            KeyCode::Delete => {
                search.delete();
                true
            }
            KeyCode::Left => {
                search.left();
                true
            }
            KeyCode::Right => {
                search.right();
                true
            }
            // a modifier means it is somebody reaching past the box for one of the keys that work
            // everywhere, not a character - `alt` as well as `ctrl`, which this said and did not
            // do: with a search open, `alt+2` typed a `2` into the query instead of going to the
            // context tab, and `/help` promises those four everywhere
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                search.push(c);
                true
            }
            _ => false,
        }
    }

    pub(super) fn trace_key(&mut self, key: KeyEvent) {
        // the pane draws the tail, so scrolling counts upwards from the newest line; the frame
        // clamps it to what there is
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.trace_scroll += 1,
            KeyCode::Down | KeyCode::Char('j') => {
                self.trace_scroll = self.trace_scroll.saturating_sub(1)
            }
            KeyCode::PageUp => self.trace_scroll += self.viewport.max(1),
            KeyCode::PageDown => {
                self.trace_scroll = self.trace_scroll.saturating_sub(self.viewport.max(1))
            }
            KeyCode::End | KeyCode::Char('G') => self.trace_scroll = 0,
            KeyCode::Home | KeyCode::Char('g') => self.trace_scroll = usize::MAX,
            KeyCode::Char('/') => self.search = Some(Search::new()),
            _ => {}
        }
    }

    /// Keys that belong to whatever is on top.
    pub(super) async fn overlay_key(&mut self, key: KeyEvent) {
        let Some(overlay) = &mut self.overlay else {
            return;
        };

        match overlay {
            Overlay::Text {
                pages,
                page,
                scroll,
                ..
            } => match key.code {
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll += 1,
                KeyCode::PageUp => *scroll = scroll.saturating_sub(PAGE),
                KeyCode::PageDown => *scroll += PAGE,
                // reading another face of the same thing is not leaving it. Round rather than
                // stop, so that two pages are one key apart in either direction
                KeyCode::Left | KeyCode::Right if pages.len() > 1 => {
                    let step = match key.code {
                        KeyCode::Left => pages.len() - 1,
                        _ => 1,
                    };
                    *page = (*page + step) % pages.len();
                    *scroll = 0;
                }
                // note: it used to go back to a permission overlay rather than close, because a
                // question was an overlay too and `[i]` had covered it over. A question is pinned
                // above the prompt now and was never covered, so there is nothing to go back to
                _ => self.overlay = None,
            },
        }
    }

    /// The keys while a question is on the screen and has not been given them.
    ///
    /// note: this is the guard, and it is a guard rather than a consequence of the layout. The
    /// question has the prompt's place, so there is nothing to type into and nothing for `enter`
    /// to send - and both of those matter. The answers are bare letters, and a question that took
    /// the keys on arrival read the `a` of "what" as `always, for shell` and kept it for the rest
    /// of a live session; an `enter` that still reached the prompt would send the half-written
    /// message the question interrupted, starting a turn nobody asked for on the way to answering.
    /// `tab` is the one gesture that gets past this, and the panel's title says so.
    ///
    /// note: what is left working is what moves the conversation, because reading is not
    /// answering. The question is pinned rather than modal so that somebody can go and look at
    /// what it is about before deciding, and a guard that also froze the transcript would be
    /// taking back the thing the pinning bought.
    pub(super) fn locked_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.scroll_by(-1),
            KeyCode::Down => self.scroll_by(1),
            KeyCode::PageUp => self.scroll_by(-(self.viewport as isize / 2)),
            KeyCode::PageDown => self.scroll_by(self.viewport as isize / 2),
            _ => {}
        }
    }

    /// Answers the question a tool is waiting on.
    ///
    /// note: reached only with the keys deliberately moved to it, which is the whole of what a
    /// settling timer used to be for. See the note on [`App::locked_key`] for what that is
    /// protecting against and why a timer could not do it.
    pub(super) async fn question_key(&mut self, key: KeyEvent) {
        // the compaction's own answers, where that is what is standing there. It scrolls with the
        // same keys, because it is the same panel showing a longer list than it has room for
        if self.asked().is_none() && self.proposed.is_some() {
            match key.code {
                KeyCode::PageUp => self.question_scroll = self.question_scroll.saturating_sub(PAGE),
                KeyCode::PageDown => self.question_scroll += PAGE,
                KeyCode::Up => self.question_scroll = self.question_scroll.saturating_sub(1),
                KeyCode::Down => self.question_scroll += 1,
                KeyCode::Char('y') => self.take_proposal(true).await,
                // `esc` as well as `n`, because a panel somebody opened and thought better of is
                // the one thing everybody tries `esc` on. There is no turn to interrupt here for
                // it to mean anything else: `esc` stops a run, and a question is the loop resting
                KeyCode::Char('n') | KeyCode::Esc => self.take_proposal(false).await,
                _ => {}
            }

            return;
        }

        let Some(request) = self.asked() else {
            // the question went away rather than being answered - the turn was stopped, or the
            // calls were dropped - so the prompt is back, and this key belongs to it instead of
            // being swallowed by a panel that is no longer there
            self.focus = Focus::Input;
            self.input_key(key).await;
            return;
        };

        // the arguments can be longer than the panel has room for - a `revise` carrying a
        // rewritten tool result is as long as the result - so the keys that scroll everything else
        // scroll them here too
        match key.code {
            KeyCode::PageUp => {
                self.question_scroll = self.question_scroll.saturating_sub(PAGE);
                return;
            }
            KeyCode::PageDown => {
                self.question_scroll += PAGE;
                return;
            }
            KeyCode::Up => {
                self.question_scroll = self.question_scroll.saturating_sub(1);
                return;
            }
            KeyCode::Down => {
                self.question_scroll += 1;
                return;
            }
            _ => {}
        }

        let mut remembered = false;
        let grant = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Grant::Allow,
            KeyCode::Char('a') | KeyCode::Char('A') => {
                remembered = true;
                Grant::Allow
            }
            KeyCode::Char('i') | KeyCode::Char('I') => {
                let spec = self
                    .kernel
                    .tool(&request.tool)
                    .map(|tool| serde_json::to_string_pretty(&tool.spec()).unwrap_or_default())
                    .unwrap_or_else(|| "this tool is not registered".into());
                let args = serde_json::to_string_pretty(&*request.args).unwrap_or_default();
                self.preview(
                    format!("{} · what it was asked to do", request.tool),
                    format!("{args}\n\n--- the tool ---\n{spec}"),
                );

                return;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Grant::Deny,
            KeyCode::Char('d') | KeyCode::Char('D') => {
                self.drop_pending();
                return;
            }
            // a key that is not one of the answers does nothing, and that is the point. It used to
            // fall through to the prompt, because a question took every key whether or not anybody
            // had given it one - so it had to hand back the ones that were not answers. The keys
            // are here because somebody put them here, and letting a stray one type into a message
            // would put the answers back into the middle of a sentence
            _ => return,
        };

        // note: everything an answer *is* - telling the sandbox about a granted `curl`, honouring
        // `always` over what the policy really consulted rather than over what the tool declared,
        // sweeping the questions already queued behind this one, and driving the turn on once none
        // are left - is [`App::decide`]. It is there rather than here because the keys are one of
        // three ways to answer, and the two that are not keys were each missing a different one of
        // those four
        if let Err(e) = self.decide(request.id, grant, remembered) {
            self.say(Speaker::Error, e);
        }
        // what is left is the screen's own half of it: the keys stay on the panel while there are
        // more questions, so three answers are three keystrokes rather than three rounds of `tab`
        if self.kernel.pending_permissions().is_empty() {
            self.focus = Focus::Input;
            self.question_scroll = 0;
        }
    }

    /// Drops every call the model is waiting on an answer for, and tells it so.
    ///
    /// note: Denying them one at a time says no to each; this says no to all of them with one
    /// reason, which is the answer when the model has gone off down the wrong path entirely.
    /// Either way the model is told - a call that simply vanished would leave it waiting.
    fn drop_pending(&mut self) {
        let dropped = self.kernel.cancel_pending_calls("dropped at the terminal");
        self.overlay = None;
        self.say(
            Speaker::Note,
            format!("{dropped} call(s) dropped; the model is told, and can try something else"),
        );

        // and then the model gets to say something about it. Answering `n` to every request ends
        // up at `start_turn`; dropping them all left the kernel idle with the refusals recorded
        // and nobody driving, so the session simply stopped until somebody typed `/continue`
        match self.stepping {
            true => self.say(Speaker::Note, "/step or /continue when you are ready"),
            false => self.start_turn(),
        }
    }

    /// Moves the transcript, and stops following the bottom if it moved up.
    fn scroll_by(&mut self, lines: isize) {
        let bottom = self.rendered.saturating_sub(self.viewport);
        let at = (self.scroll as isize + lines).clamp(0, bottom as isize) as usize;

        self.scroll = at;
        self.follow = at >= bottom;
    }
}
