//! What the keys do.
//!
//! note: one handler per tab rather than one `match` over every key, because the same key means
//! different things on different tabs and a single table of them was the file's worst argument
//! with itself. [`super::App::on_key`] is the dispatcher, and it is next door with the rest of
//! the public surface.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nachalnik::{Capability, ContextItem, ContextState, Grant, Verdict};
use ratatui_textarea::CursorMove;
use serde_json::json;

use super::{
    App, Focus, Overlay, Page, Speaker, Tab,
    text::{projected, stored, whole},
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

    /// Empties the prompt, wherever what was in it has just gone.
    pub(super) fn clear_input(&mut self) {
        self.input.select_all();
        self.input.cut();
        self.input.move_cursor(CursorMove::End);
    }

    /// Puts an edited item into the context in place of the one it came from.
    ///
    /// note: [`Kernel::supersede`] rather than a replacement in place, so the old one is still
    /// there to be read and put back - marked `~`, with a note saying which item replaced it.
    /// The kind is carried over whole, which matters for an assistant turn: its tool calls live
    /// inside the kind, and rebuilding it without them would orphan their results.
    fn commit_edit(&mut self, text: &str) {
        let Some(id) = self.editing.take() else {
            return;
        };
        self.focus = Focus::Body;

        let Some(old) = self.kernel.item(id) else {
            self.say(Speaker::Error, format!("[{id}] is no longer there"));
            return;
        };
        if old.content.to_text() == text {
            self.say(Speaker::Note, format!("[{id}] is unchanged"));
            return;
        }

        let mut edited = ContextItem::new(
            old.kind.clone(),
            old.source.clone(),
            old.label.clone(),
            text.to_owned(),
        )
        .because("edited at the terminal");
        // where it belongs in the conversation, which is not where its identifier puts it. A
        // superseding item is appended, so it is the newest thing in the context and the chat
        // would draw it last - an edit to a turn from twenty exchanges ago landing after
        // everything that followed it, which describes an order no request ever had. The
        // request has the new words in the old place, so this says which place that is.
        //
        // note: on `meta`, which is the field for exactly this - a hint the runtime never reads
        // - and it rides in the snapshot, so a resumed session draws the edit where it was too
        edited.meta = json!({ "replaces": id.0 });
        // editing something decides what it says, not whether it is sent - so whatever it was
        // doing, it goes on doing. `ContextItem::new` starts out Active, and carrying over only
        // the pin meant editing a pruned item quietly put it back into the next request, and
        // editing an *archived* one promoted the whole of an oversized tool output into it. An
        // elided one is the same trap in a quieter form: the row says a marker is being sent, so
        // an edit that came back Active would be sending the new text against what the screen
        // says. It stays elided, and `space` round to active is how you say you meant it read
        edited.state = match old.state {
            ContextState::Pinned
            | ContextState::Excluded
            | ContextState::Elided
            | ContextState::Archived => old.state,
            _ => ContextState::Active,
        };

        match self.kernel.supersede(id, edited) {
            Ok(new) => {
                // the old item keeps its own row, but the new one is where somebody will be
                // looking, so what it used to say follows it there. Copied rather than moved:
                // both rows are real, and both can answer "what did this say before?"
                let history = self.versions.get(&id).cloned().unwrap_or_default();
                self.versions.insert(new, history);
                self.remember(new, old.content.clone());
                // and nothing has to be told about the conversation. The superseded item stops
                // being projected and the new one takes its place in the context, so the next
                // frame draws the edit where the turn was - which is also why an `undo` of it
                // reaches the screen with nothing here keeping a second account of what to
                // put back
            }
            Err(e) => self.say(Speaker::Error, e.to_string()),
        }
    }

    /// Keys that belong to the context pane.
    pub(super) fn context_key(&mut self, key: KeyEvent, count: &str) {
        // what is on the screen, not what is in the context: with `f` on, the rows between two
        // items are gone and moving by one row has to mean the next row somebody can see
        let items = self.listed();
        if items.is_empty() {
            // there is nothing to pick, but the key that puts the rows back must still work
            if matches!(key.code, KeyCode::Char('f')) {
                self.sending_only = false;
            }
            return;
        }
        self.selected = self.selected.min(items.len() - 1);
        let picked = items[self.selected].clone();

        match key.code {
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
            // number in the first column is the one `/prune` takes and the one every note names.
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
            KeyCode::Char('?') => self.preview("the keys", crate::help::HELP),
            KeyCode::Esc => self.cancel_edit(),
            // changing what an item says, which is the verb the other keys were missing: `space`
            // and `p` decide whether the model reads it, and this decides what it reads
            KeyCode::Char('e') => {
                self.clear_input();
                self.input.insert_str(picked.content.to_text());
                self.input.move_cursor(CursorMove::End);
                self.editing = Some(picked.id);
                self.focus = Focus::Input;
            }
            // how much of an item the model gets, in three steps out and one back: all of it,
            // then a marker where it was, then nothing at all
            //
            // note: the middle step is the one worth having a key for. Taking a tool result out
            // makes the projector drop the call that asked for it, so the model reads a
            // conversation it never had; elided, the call keeps its answer and only the content
            // is gone. Which of the two somebody wants is not something this program can guess -
            // hiding a result outright is a fair thing to want - so it is a cycle rather than a
            // decision, the same way the permissions tab cycles a stance through three
            KeyCode::Char(' ') => {
                let (to, note) = match picked.state {
                    // note: this one is read by the model, in the brackets the projector puts
                    // round it, so it is written for somebody who has never heard of this
                    // program: no "at the terminal", which is this codebase's own idiom for
                    // "a person did it here" and reads to a model like a shell or a state. It
                    // does not invite the model to ask for it back either - the thing hidden may
                    // be the thing that should not be asked for
                    ContextState::Active | ContextState::Pinned => (
                        ContextState::Elided,
                        Some("removed from view by the user".into()),
                    ),
                    ContextState::Elided => (
                        ContextState::Excluded,
                        Some("taken out at the terminal".into()),
                    ),
                    _ => (ContextState::Active, None),
                };
                self.kernel.set_state([picked.id], to, note);
            }
            KeyCode::Char('p') => {
                let to = match picked.state {
                    ContextState::Pinned => ContextState::Active,
                    _ => ContextState::Pinned,
                };
                self.kernel.set_state([picked.id], to, None);
            }
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
                    // who rewrote it and why, which `amend` records on the item itself; the trace
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
        // an undo puts an old content back, and the version it restored is then the current one
        // too; two identical pages side by side would be saying nothing twice
        let keep = history.len() - usize::from(history.last() == Some(&item.content));
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
            KeyCode::Char('?') => {
                self.preview("the keys", crate::help::HELP);
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
            KeyCode::Char('?') => self.preview("the keys", crate::help::HELP),
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
        let Some(request) = self.asked() else {
            // the question went away rather than being answered - the turn was stopped, or the
            // calls were dropped - so the prompt is back, and this key belongs to it instead of
            // being swallowed by a panel that is no longer there
            self.focus = Focus::Input;
            self.input_key(key).await;
            return;
        };

        // the arguments can be longer than the panel has room for - an `amend` carrying a
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
                // everything the policy actually consulted, not just what the tool declared: a
                // `yes, always` to a `curl` that left `network` on `ask`, or to a `.env` that left
                // its rule on `ask`, would ask again on the very next call
                self.policy.always(&self.policy.judges(&request));
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

        // saying yes to a command that reaches for the network is permission for *that* command,
        // and the sandbox has to hear about it. Without this the call runs with the network cut
        // and fails, one keystroke after somebody was told it would run
        if grant == Grant::Allow
            && request.capabilities.contains(&Capability::Shell)
            && request
                .args
                .get("cmd")
                .and_then(|cmd| cmd.as_str())
                .is_some_and(crate::tools::reaches_the_network)
        {
            self.policy.grant_the_network(&request.call);
        }

        if let Err(e) = self.kernel.decide(request.id, grant) {
            self.say(Speaker::Error, e.to_string());
        }
        // note: `always` is a promise about what happens next, and what happens next is often
        // already in the queue. A model that asks for three commands in one answer produces three
        // questions, all of them decided before the first was shown - so answering `a` to the
        // first asked about the second one keystroke later, having just been told it would not.
        // Everything still waiting that the policy would now let through is let through
        if remembered {
            for waiting in self.kernel.pending_permissions() {
                if self.policy.verdict(&waiting) == Verdict::Allow
                    && let Err(e) = self.kernel.decide(waiting.id, Grant::Allow)
                {
                    self.say(Speaker::Error, e.to_string());
                }
            }
        }
        // the model may have asked for several things at once, and each is its own question. The
        // keys stay on the panel while there are more, so three answers are three keystrokes
        // rather than three rounds of `tab`
        if self.kernel.pending_permissions().is_empty() {
            self.focus = Focus::Input;
            self.question_scroll = 0;
            // somebody driving this a transition at a time did not ask for the rest of the turn,
            // and running it here would be the harness taking the wheel back
            match self.stepping {
                true => self.say(
                    Speaker::Note,
                    "decided; /step runs the calls, /continue runs the rest of the turn",
                ),
                false => self.start_turn(),
            }
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
