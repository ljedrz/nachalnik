//! The chat as a person reads it: what is said, what is still arriving, and what the lines add
//! up to once the kernel has recorded them.
//!
//! note: an `impl App` of its own, the way `keys.rs` is, because this is one concern with one
//! rule at the bottom of it - everything on the chat tab either *is* a context item or is one of
//! these, and the two have to keep their order among each other. [`Entry`] is what is not an
//! item, [`App::conversation`] is what builds the drawn line out of both, and [`Said`] is what it
//! builds. What the screen does with them is `ui/tabs.rs`, which decides nothing.

use std::{
    borrow::Cow,
    collections::BTreeMap,
    fs::File,
    io::{BufRead as _, BufReader},
    path::Path,
    sync::Arc,
    time::{Instant, SystemTime},
};

use nachalnik::{ContextId, ContextItem, ContextKind, Event, Overrun, Record};

use super::{
    App, HOPS, LIVE_OUTPUT, TRACE_DEPTH, Traced,
    text::{head, one_line, plural, thousands, unpadded},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who produced a line of the transcript.
///
/// note: serializable because a line of the chat is what attaching to a session answers with, and
/// whoever reads it is not always in this process - see [`crate::remote`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    /// The person at the terminal.
    User,
    /// The model's answer.
    Model,
    /// The model's reasoning, where the provider exposes it.
    Reasoning,
    /// A tool the model asked for.
    Call,
    /// What that tool said.
    Result,
    /// The runtime, saying what it did.
    Note,
    /// Something went wrong.
    Error,
}

/// One line of the chat that is not a context item.
///
/// note: everything else on the chat *is* one, and is read off the context every frame by
/// [`App::conversation`]. What is left over is two kinds of line, and they are one type because
/// they have to keep their order among each other: the fragments arriving between a model
/// starting to speak and the kernel recording what it said, and the chrome that is nobody's
/// context at all - a slash command's output, a compaction notice, "stopped", a provider's
/// error.
///
/// note: the first kind is [`Entry::transient`] and is dropped the moment the item exists; the
/// second stays for the session. Neither carries an identifier, because neither has one.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Who is saying it.
    pub speaker: Speaker,
    /// What it says.
    pub text: String,
    /// Whether more of it is still arriving.
    pub open: bool,
    /// Whether it arrived as fragments, and so is a context item's to say once there is one.
    ///
    /// note: not the same question as [`Entry::open`], which is only whether *more* is coming. A
    /// streamed answer stops being open the moment the stream ends and is still the item's a
    /// beat later, when the kernel records it.
    pub streamed: bool,
    /// The newest context item that existed when it was said, if any did.
    ///
    /// note: what puts it back in its place. A line like this belongs *between* two turns rather
    /// than at an index, because the turns around it can be excluded, edited, undone or
    /// compacted and it still happened where it happened. Anchoring to an identifier survives
    /// all of that, the anchor itself going away included: the line simply renders before
    /// whatever the next surviving item is.
    pub after: Option<ContextId>,
    /// Whether it was said while something was still arriving, and so belongs after whatever
    /// that arrival turns into.
    ///
    /// note: the one case an identifier cannot answer on its own. "stopped" is said while a
    /// model is mid-sentence, so the newest item at the time is the *question*, and anchoring
    /// there puts the interruption above the half-answer it interrupted. The turn is recorded a
    /// moment later with a higher identifier; this is what re-anchors to it. Without it the
    /// conversation reads in an order the session never had.
    pub arriving: bool,
}

impl Entry {
    /// Whether the context is going to say this line itself, once it catches up.
    ///
    /// note: whether it *streamed*, and not whether its speaker is one that gets context items.
    /// The difference is a message typed into a running turn: said by a person, so it will be an
    /// item eventually, but not until the turn it was typed into has ended - and dropping it
    /// when some other turn was recorded took it off the screen for as long as that took. What
    /// an arriving item replaces is what was arriving.
    pub fn transient(&self) -> bool {
        self.streamed
    }
}

/// One line of the conversation as it stands now, ready to be drawn.
///
/// note: built per frame by [`App::conversation`] and held by nobody. A line that *is* a context
/// item carries it, so the drawing can ask the one question that is about the request rather
/// than about the text - is this going, and if not why - without a second lookup and a second
/// chance to disagree with itself.
pub struct Said<'a> {
    /// Who said it.
    pub speaker: Speaker,
    /// What it says now.
    pub text: Cow<'a, str>,
    /// The context item this line is part of, where it is part of one.
    pub item: Option<&'a Arc<ContextItem>>,
}

impl App {
    /// Hands what an item says to the loop, for the terminal to put on the clipboard.
    ///
    /// note: the item's own text, which is not what is on the screen. A selection dragged across
    /// the chat pane takes the frame down both sides of every line, the wrapping of whatever
    /// width the window was, and none of what has scrolled past - and a model's answer is the
    /// thing people most often want out of here whole. This is that answer as the context holds
    /// it; see [`crate::clipboard`] for what the loop then does with it, and for the one thing
    /// neither can find out, which is whether the terminal took it.
    ///
    /// note: it says how many bytes, because that is the only receipt there is. A line reading
    /// "copied" would be claiming a thing this program cannot see; the figure is what somebody
    /// compares against what turns up in their paste.
    ///
    /// note: and it does not name the terminal, because the loop is what finds out whether there
    /// is one. Said here, `handed to the terminal` was followed down a pipe by `there is no
    /// terminal here for it to go to` - two lines about one act, the second contradicting the
    /// first.
    pub fn copy(&mut self, id: ContextId) {
        let Some(item) = self.kernel.item(id) else {
            self.say(Speaker::Note, format!("there is no item {id}"));
            return;
        };

        let text = item.content.to_text().into_owned();
        let said = format!("[{id}] to the clipboard: {} bytes", thousands(text.len()));
        self.clipboard = Some(text);
        self.say(Speaker::Note, said);
    }

    /// Adds a finished entry to the transcript, ending whatever was still arriving.
    ///
    /// note: only the person's own line takes the view back to the bottom. Everything else that
    /// arrives leaves the scroll where somebody put it - a model writing four hundred lines used
    /// to yank the window back to the newest of them on every fragment, so reading anything it
    /// had said thirty seconds ago was impossible until the turn ended.
    pub fn say(&mut self, speaker: Speaker, text: impl Into<String>) {
        let text = unpadded(&text.into()).to_owned();
        // whether something was arriving is read *before* closing it, because closing is what
        // makes it stop arriving and this line was said while it still was
        let arriving = self.arriving();
        self.close();
        self.loose.push(Entry {
            speaker,
            text,
            open: false,
            streamed: false,
            after: self.kernel.items().last().map(|item| item.id),
            arriving,
        });
        if speaker == Speaker::User {
            self.follow = true;
        }
    }

    /// Says a message of the person's own and puts it in the context.
    ///
    /// note: it does not say it on the chat, and that is the whole of what this method is now.
    /// The item *is* the line: the conversation is read off the context every frame, so pushing
    /// is saying. It used to be three statements - say it, push it, tie the two together - and
    /// the third one is the one `--message` forgot, which left the opening line of every `-m`
    /// session unable to be hidden, updated or undone for the rest of it. There is no third
    /// statement left to forget.
    pub fn ask(&mut self, text: &str) -> ContextId {
        self.follow = true;

        self.kernel.push(ContextItem::user(text))
    }

    /// Says an error, unless this turn has already said the same one in a smaller envelope.
    ///
    /// note: one provider failure is reported twice - once as the event the kernel emitted and
    /// once as the outcome the turn came to, the second wrapping the first - and two red lines
    /// saying the same thing is one more than the news warrants; the trace pane has both either
    /// way.
    ///
    /// note: within *this turn*, and the distinction is the whole of [`App::failed`]. Asking
    /// whether the last loose line was this error answers a different question, because a loose
    /// line outlives the turn that said it: a message and an answer are both drawn from the
    /// context and neither leaves one, so the first red line of a session stays the last loose
    /// line until something else is said out loud. A live run against a model with one canned
    /// refusal failed three times and showed it once - twice in silence, with the person typing
    /// into what looked like a working session. A failure is news every time it happens.
    pub(super) fn say_error(&mut self, error: String) {
        let said = unpadded(&error).to_owned();
        if self
            .failed
            .as_deref()
            .is_some_and(|last| said.contains(last))
        {
            return;
        }

        self.failed = Some(said);
        self.say(Speaker::Error, error);
    }

    /// Says what a refusal for length means here: how much has to go before the next request is
    /// one that gets sent, and who counted.
    ///
    /// note: the sentence above this one is the server's or the kernel's, and both of them say the
    /// same two numbers in a different order. This says what they mean *here*. Without it the only
    /// figure on the screen is the corner - which is the estimate that just turned out to be
    /// wrong, and which is now being corrected by this very event.
    ///
    /// note: `counted` is the difference between a measurement and a guess, and it is worth a
    /// different sentence rather than a different word. A refusal from the endpoint is the model's
    /// own tokenizer reporting on the request it read, and there is nothing to argue with; a
    /// refusal from here is this counter's estimate of a request nobody has read yet, which can be
    /// wrong in either direction and is wrong by enough to matter on anything that is not prose.
    /// Telling somebody "the model read that request as" a figure the model never saw is the kind
    /// of confident wrong sentence this whole corner of the program exists to stop.
    pub(super) fn overran(&mut self, overrun: Option<Overrun>, counted: bool) {
        let Some(overrun) = overrun else {
            return;
        };

        let tokens = thousands(overrun.tokens as usize);
        let over = overrun
            .limit
            .map(|limit| thousands(overrun.tokens.saturating_sub(limit) as usize));
        let said = match (counted, over) {
            (true, Some(over)) => format!(
                "the model read that request as {tokens} tokens: ~{over} more than it takes. \
                 Nothing is sent until that much goes - `/compact` says what a pass would take \
                 before it takes it, and `/exclude` is the same decision made by hand",
            ),
            (true, None) => format!(
                "the model read that request as {tokens} tokens, which is more than it takes",
            ),
            (false, Some(over)) => format!(
                "~{over} tokens have to go before it is sent - `/compact` says what a pass would \
                 take before it takes it, and `/exclude` is the same decision made by hand. \
                 Nothing has read that request: {tokens} is this counter's own estimate of it, \
                 and `/budget` says how far it has been corrected",
            ),
            (false, None) => format!(
                "nothing has read that request: {tokens} is this counter's own estimate of it, \
                 and `/budget` says how far it has been corrected",
            ),
        };

        self.say(Speaker::Error, said);
    }

    /// Whether something is part-way through arriving.
    fn arriving(&self) -> bool {
        self.loose.last().is_some_and(|entry| entry.open)
    }

    /// Appends to the line still arriving from this speaker, starting one if there is none.
    ///
    /// note: the bound is on a tool's output and on nothing else, which it did not used to be. A
    /// `find /` should not be able to fill the screen up, and the whole of it is in the context
    /// either way - but a model writing a long answer had its first paragraphs eaten while it
    /// was still writing the last one. A message is what somebody came here to read, and it is
    /// never shortened; the moment the turn is recorded the line is dropped and the item is what
    /// gets drawn.
    pub(super) fn append(&mut self, speaker: Speaker, fragment: &str) {
        let after = self.kernel.items().last().map(|item| item.id);
        match self.loose.last_mut() {
            Some(entry) if entry.open && entry.speaker == speaker => {
                entry.text.push_str(fragment);
                if speaker == Speaker::Result && entry.text.len() > LIVE_OUTPUT {
                    let cut = entry
                        .text
                        .char_indices()
                        .nth(entry.text.chars().count() - LIVE_OUTPUT / 2)
                        .map(|(at, _)| at)
                        .unwrap_or(0);
                    entry.text = format!(
                        "[... the earlier output is not repeated here; the whole of it is in the \
                         context ...]\n{}",
                        &entry.text[cut..]
                    );
                }
            }
            _ => {
                self.close();
                self.loose.push(Entry {
                    speaker,
                    text: fragment.to_owned(),
                    open: true,
                    streamed: true,
                    after,
                    arriving: false,
                });
            }
        }
    }

    /// Closes whatever was still arriving, and drops it if it turned out to be nothing.
    pub(super) fn close(&mut self) {
        let Some(entry) = self.loose.last_mut() else {
            return;
        };

        entry.open = false;
        entry.text = unpadded(entry.text.trim_end()).to_owned();
        if entry.text.is_empty() {
            self.loose.pop();
        }
    }

    /// Hands the lines that were arriving over to the item that now holds them.
    ///
    /// note: called with no thought about *what* arrived, which is the point. The old code
    /// remembered whether a provider had streamed so it would not print a non-streaming answer
    /// twice, popped the open tool result so the recorded one could take its place, and walked
    /// the tail backwards stamping identifiers onto lines. All three answered the same question -
    /// which lines has the context caught up with - and the answer is now always "all of them".
    ///
    /// note: what was said *while* they were arriving is re-anchored to the item rather than
    /// dropped, because it is not the item's and it did not happen before it. "stopped" is said
    /// mid-sentence, so the newest item at the time was the question; left there it would read
    /// above the half-answer it interrupted.
    pub(super) fn caught_up(&mut self, item: ContextId) {
        self.loose.retain(|entry| !entry.transient());
        for entry in &mut self.loose {
            if entry.arriving {
                entry.after = Some(item);
                entry.arriving = false;
            }
        }
    }

    /// Fills in what the resumed items used to say, out of the record saved beside the snapshot.
    ///
    /// note: a [`nachalnik::Snapshot`] carries items and not events, so the text a rewrite
    /// replaced is in the session's log and nowhere else - [`Event::ContextReplaced`] is the one
    /// event that carries content, which is the whole reason it does. `/save` writes that log
    /// beside the snapshot under the same name, so this looks for it there: `<name>.json` is
    /// what `-r` was handed, `<name>.jsonl` is what this reads. What it finds goes through the
    /// same `App::remember` the live path uses, in the order it was recorded, so a resumed
    /// `v1` is the `v1` the session had - eight deep, and an undone rewrite deduped by the same
    /// line that dedupes it live.
    ///
    /// note: best effort, and deliberately not an error. The session has resumed by the time
    /// this runs, and a record that is absent, unreadable or half-written costs a page under
    /// `enter` rather than a session. A line that will not parse is skipped rather than ending
    /// the walk: the last line of a log from a run that was killed is the one most likely to be
    /// half a record, and the ones before it are fine.
    ///
    /// note: only for items that came back. A rewrite of something an `undo` took away before
    /// the snapshot was written is history for an item this session does not have, and a resumed
    /// kernel has no undo stack for it to come back on.
    ///
    /// note: what this does *not* do is carry forward. The resumed session's own log starts at
    /// `session.resumed`, so a `/save` of it writes a record with none of these rewrites in it
    /// and a resume of *that* file reads nothing. Two hops back is the earlier `.jsonl`, which is
    /// why an append-only log is worth keeping rather than overwriting.
    pub fn recall(&mut self, state: &Path) -> usize {
        let Ok(log) = File::open(state.with_extension("jsonl")) else {
            return 0;
        };

        let mut recalled = 0;
        for line in BufReader::new(log).lines().map_while(Result::ok) {
            let Ok(record) = serde_json::from_str::<Record>(&line) else {
                continue;
            };
            let Event::ContextReplaced { id, was, .. } = record.event else {
                continue;
            };
            if self.kernel.item(id).is_some() {
                self.remember(id, was);
                recalled += 1;
            }
        }

        recalled
    }

    /// Says what a resumed session picked up.
    ///
    /// note: it says it and nothing else, which is the whole of what a resume needs now. It used
    /// to walk the context turning every item into a transcript line, because the transcript was
    /// a log and a resumed session had no log to show - and that walk was a *second*
    /// implementation of "what does this item look like as a conversation", beside the one the
    /// live path built event by event. They disagreed, as two of anything do: a resumed turn
    /// showed none of its thinking, and a resumed tool result's line left out what the output
    /// limit had taken. There is one implementation now, [`App::conversation`], and a resumed
    /// session is drawn by it without being told that it was resumed.
    pub fn replay(&mut self) {
        let items = self.kernel.items();
        let withheld = items.iter().filter(|item| !item.is_projected()).count();

        // read off `versions` rather than handed in, so the line says what is actually there to
        // read: `App::recall` runs before this and an unread record leaves it at nothing
        let recalled: usize = self.versions.values().map(Vec::len).sum();

        self.say(
            Speaker::Note,
            format!(
                "resumed session {}: {}, ~{} tokens{}{}",
                self.kernel.session_name(),
                plural(items.len(), "item"),
                thousands(self.kernel.budget().context_tokens),
                match withheld {
                    0 => String::new(),
                    n => format!(", {n} of which the pane says are not being sent"),
                },
                match recalled {
                    0 => String::new(),
                    n => format!(
                        "; {n} earlier version(s) of what items said, off the record beside it"
                    ),
                }
            ),
        );
    }

    /// The conversation as it stands: every context item that is going, in order, with the
    /// asides that were said between them and whatever is arriving at the end.
    ///
    /// note: **the one place a context item becomes a line of chat.** Everything the screen
    /// shows of the conversation is worked out here, from the context, every frame - so an item
    /// that was excluded is not in it, an item that was rewritten reads as it is now, an item an
    /// `undo` took away is gone and one a `redo` brought back is there, and none of those needed
    /// an event, a back-pointer or a second copy of the words. What used to do this was a log
    /// with three patches on it: a live text lookup for edits, a withheld lookup for exclusions,
    /// and a backwards walk stamping identifiers onto lines so the other two could find them.
    ///
    /// note: `is_projected` and not `sends_content`, so an *elided* item keeps its place. It is
    /// in the request as a marker, which is what the model reads there; the drawing marks it.
    /// A turn whose call has not come back yet also stays: the projector repairs one of those
    /// out of the request, and that is a momentary, mechanical absence rather than anything
    /// anybody decided - hiding on it blanked a call out of the conversation at the moment a
    /// permission question was asking about it.
    ///
    /// note: the items are the caller's, because they are `Arc`s the kernel hands out by clone
    /// and the lines borrow their text rather than copying it. A frame that copied every word it
    /// was about to draw would copy the whole conversation to show what it was already showing.
    pub fn conversation<'a>(&'a self, items: &'a [Arc<ContextItem>]) -> Vec<Said<'a>> {
        let mut said = Vec::new();
        let mut loose = self.loose.iter().peekable();
        let loosed = |entry: &'a Entry| Said {
            speaker: entry.speaker,
            text: Cow::Borrowed(&entry.text),
            item: None,
        };

        for (at, item) in Self::in_order(items) {
            // what was said before this item existed goes before it, in the order it was said
            while let Some(entry) = loose.next_if(|entry| entry.after < Some(at)) {
                said.push(loosed(entry));
            }
            if item.state.is_projected() {
                Self::as_conversation(item, &mut said);
            }
        }
        said.extend(loose.map(loosed));
        // and last, the message typed into a turn that is still running. It is drawn from the
        // field holding it rather than said onto the screen, for the same reason the fragments
        // are drawn from the context: it is state waiting to become an item, and the moment it
        // becomes one the item is what gets drawn, in the same place, with nothing to clean up
        if let Some(waiting) = &self.typed_ahead {
            said.push(Said {
                speaker: Speaker::User,
                text: Cow::Borrowed(waiting),
                item: None,
            });
        }

        said
    }

    /// The same text without the blank lines a provider put in front of it, and without
    /// copying it to find out there were none.
    ///
    /// note: applied to what an item says rather than to what the item holds. The item keeps
    /// what arrived - a record of "what arrived, tidied up" cannot answer what arrived - and the
    /// screen declines to spend rows on it. `App::say` has always done this to a line said
    /// outright, and the lines are read off items now, so this is where it moved to.
    fn trimmed(text: Cow<'_, str>) -> Cow<'_, str> {
        match text {
            Cow::Borrowed(text) => Cow::Borrowed(unpadded(text)),
            Cow::Owned(text) => Cow::Owned(unpadded(&text).to_owned()),
        }
    }

    /// The items in the order the conversation had them, each with the place it occupies.
    ///
    /// note: not the order the context holds them in, and the difference is a *supersession*:
    /// the new words are a new item, appended, so its identifier is the highest in the context
    /// and reading the context in order puts a replacement for a turn from twenty exchanges ago
    /// after everything that followed it. That is an order no request ever had - the request has
    /// the new words where the old ones were - so an item that says which one it replaces takes
    /// that one's place, and its identifier is only the tie-break between two that claim the
    /// same one.
    ///
    /// note: `e` no longer makes one of those. A terminal edit replaces in place, keeps its
    /// identifier and is therefore already where it belongs; see [`App::commit_edit`]. This
    /// stays for a context that arrives with a supersession in it - a session saved before that
    /// changed, or one written by another client, since [`Kernel::supersede`] is the runtime's
    /// and is the right shape for a caller whose next round replaces the last. The hint it
    /// reads lives on `meta`, which is where this program wrote it and where such a client
    /// would: the field exists for exactly this, and the runtime never reads it.
    ///
    /// note: the chain is followed rather than the one hop, because an item can be superseded
    /// twice and the second replaces the first. Bounded, because nothing here wrote the number
    /// it is following: `meta` is a free-form value and a hand-written snapshot could point one
    /// item at another in a circle.
    pub(super) fn in_order(items: &[Arc<ContextItem>]) -> Vec<(ContextId, &Arc<ContextItem>)> {
        let replaces = |item: &ContextItem| {
            item.meta
                .get("replaces")
                .and_then(Value::as_u64)
                .map(ContextId)
        };
        let by_id: BTreeMap<_, _> = items.iter().map(|item| (item.id, item)).collect();

        let mut placed: Vec<_> = items
            .iter()
            .map(|item| {
                let mut at = item.id;
                for _ in 0..HOPS {
                    match by_id.get(&at).and_then(|item| replaces(item)) {
                        Some(prior) if by_id.contains_key(&prior) => at = prior,
                        _ => break,
                    }
                }

                (at, item)
            })
            .collect();
        // stable, so two items in the same place keep the order the context has them in
        placed.sort_by_key(|(at, _)| *at);

        placed
    }

    /// Turns one context item into the lines it reads as.
    ///
    /// note: a turn is more than one line - what it thought, what it said, and each tool it
    /// asked for - and all of them are the same item. That is why the drawing dedupes the
    /// withheld mark by identifier rather than by line: one turn says once why it is not going.
    pub(super) fn as_conversation<'a>(item: &'a Arc<ContextItem>, said: &mut Vec<Said<'a>>) {
        let mut line = |speaker, text| {
            said.push(Said {
                speaker,
                text,
                item: Some(item),
            })
        };

        match &item.kind {
            ContextKind::System => {}
            ContextKind::UserMessage => line(Speaker::User, Self::trimmed(item.content.to_text())),
            ContextKind::AssistantMessage { .. } => {
                // the thinking first, because that is the order it happened in and the order a
                // turn recorded as ordered blocks holds it in
                for thought in item.thinking() {
                    let text = Self::trimmed(thought.to_text());
                    if !text.trim().is_empty() {
                        line(Speaker::Reasoning, text);
                    }
                }
                let text = Self::trimmed(item.content.to_text());
                if !text.trim().is_empty() {
                    line(Speaker::Model, text);
                }
                // `calls()`, so a turn a provider recorded as ordered blocks reads back with the
                // tools it asked for rather than as bare text
                for call in item.calls() {
                    let args = one_line(&call.args.to_string());
                    line(Speaker::Call, Cow::Owned(format!("{}({args})", call.tool)));
                }
            }
            ContextKind::ToolResult { tool, is_error, .. } => {
                line(
                    Speaker::Result,
                    Cow::Owned(head(&item.content.to_text(), 6)),
                );
                // note: what a tool cost and what the output limit took are *not* here, and
                // used to be. Both are facts about an item rather than anything said, both are
                // a column on the context tab already, and a conversation with a line of
                // accountancy under every tool call is one somebody has to read around. What
                // survives is the one thing that changes how the turn above and below it reads
                if *is_error {
                    line(
                        Speaker::Note,
                        Cow::Owned(format!("{tool} reported an error")),
                    );
                }
            }
            // note: this line is what `/attach` and `-f` say for themselves, and neither of them
            // says anything else. A command that pushed an item and then announced it would be
            // describing the context from beside the context, which is the arrangement the whole
            // chat was rewritten to get rid of: two accounts of one item, and only one of them
            // able to be wrong. What a person needs to see is here because it is *read off* the
            // item - which file, what it is, what it costs
            //
            // note: the `+` is the context pane's, for the same reason: a file attached as bytes
            // is counted at `0` by everything in this workspace, and a line saying `0 tokens`
            // about the largest thing in the request is the one number on this screen that reads
            // as good news when it is the opposite
            _ => line(
                Speaker::Note,
                Cow::Owned(format!(
                    "[{}] {} ({}){}, {} tokens{}",
                    item.id,
                    item.label,
                    item.source,
                    match crate::attach::describe(item) {
                        Some(what) => format!(" {what}"),
                        None => String::new(),
                    },
                    thousands(item.tokens),
                    match item.uncounted {
                        0 => String::new(),
                        n => format!(" and {n} piece(s) nothing here can price"),
                    }
                )),
            ),
        }
    }

    /// Adds an event to the trace pane.
    pub(super) fn trace(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        if self.trace.len() == TRACE_DEPTH {
            self.trace.pop_front();
        }
        self.trace.push_back(Traced {
            name: name.into(),
            detail: detail.into(),
            at: Instant::now(),
            wall: SystemTime::now(),
            // taken rather than read, so that exactly one line is marked: the first thing that
            // happens after somebody acts is the line whose gap spans their thinking, and every
            // line after it is the program working again
            after_a_person: std::mem::take(&mut self.acted),
        });
    }
}
