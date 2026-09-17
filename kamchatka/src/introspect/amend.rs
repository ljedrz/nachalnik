//! The half of `context` that changes it: prune, rewrite, write something down, and walk any of
//! it back.
//!
//! note: a file of its own and not a tool of its own. It was both until the two were merged, and
//! what stayed behind is everything that is really about *changing* a context - the journal
//! `undo` walks, the refusals, and the accounting that says what a change cost. The schema and
//! the dispatch are in `context.rs` with the reading half, because that is what a model sees.
//!
//! note: what it will not do is undo a person's decisions - a pinned item, a system instruction
//! and the assistant turn carrying the call in flight are refused, and `undo` walks its own
//! journal rather than the kernel's stack, which belongs to the person. Both rules are in the
//! doc comments below, where the code that enforces them is.

use std::{cmp::Ordering, collections::BTreeSet};

use nachalnik::{
    Content, ContextId, ContextItem, ContextKind, ContextState, Kernel, ToolCall, ToolCallId,
    ToolOutput,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::app::text::thousands;

use super::{Pinned, ids, named, protected};

/// How many of its own changes one call may walk back or forward.
///
/// note: a bound at all, because `undo` is a loop over a journal and a model that means "all of
/// it" writes a big number. Said in the schema and refused above it rather than clamped, which is
/// how a call asking for a hundred used to get sixty-four and read as though it had got a hundred.
const WALK: u64 = 64;

/// Changes the context: prunes it, rewrites an item, writes something down, walks its own
/// changes back.
///
/// note: it keeps the set of items it pinned itself, which is the whole of the mechanism that
/// stops a model quietly unpinning what a person pinned. A pin is a promise, and the promise was
/// not made to the model.
///
/// note: it also keeps a journal of what it has done, which is what `undo` walks - deliberately
/// *not* [`Kernel::undo`]. Two reasons, and either would be enough. The kernel's undo stack is the
/// person's, bound to the `u` key in the terminal, and a model walking it back would be undoing
/// their work rather than its own. And the top of that stack, at the moment a tool is running, is
/// always the assistant turn that asked for the call: one step would erase the model's own
/// question, orphan the answer it is waiting for, and leave the loop rebuilding a request from
/// before it asked. A journal of this tool's own amendments has neither problem, and it is the
/// honest scope of "undo my mistakes" - the mistakes being the ones it made.
pub(super) struct Amend {
    pinned: Pinned,
    journal: Mutex<Journal>,
}

impl Amend {
    /// Builds one; [`Context`](super::Context) is the only caller.
    ///
    /// note: the journal starts empty and is made here rather than handed in - what `undo` walks
    /// is what this session's model did, and a journal from anywhere else would be somebody
    /// else's work to walk back.
    ///
    /// note: the pinned set *is* handed in, because it is shared with the half that reports it:
    /// what was pinned here is what `budget` says the model may unpin, and a second set would
    /// have the two disagreeing about a promise.
    pub(super) fn new(pinned: Pinned) -> Self {
        Self {
            pinned,
            journal: Mutex::new(Journal::default()),
        }
    }
}

/// What [`Amend`] has done, and what it has walked back.
#[derive(Default)]
struct Journal {
    done: Vec<Undoing>,
    undone: Vec<Undoing>,
}

/// One amendment, recorded as the way back from it.
///
/// note: the way back rather than the change itself, because applying one returns the way back
/// from *that* - so undo and redo are the same operation run against two stacks, and there is no
/// second representation to keep in step with the first.
enum Undoing {
    /// Put these items into these states, with these notes.
    States(Vec<(ContextId, ContextState, Option<String>)>),
    /// Put this text and this metadata back on this item.
    Said(ContextId, Content, Value),
}

/// One step of a walk: the way back from it, what it did, and what it would not touch.
#[derive(Default)]
struct Applied {
    /// The way from where this left things to where they were, where anything moved.
    inverse: Option<Undoing>,
    /// What it did, in the words somebody reads.
    did: Option<String>,
    /// What it left alone, and why.
    left: Vec<String>,
}

impl Undoing {
    /// Applies it, and hands back the way from where that leaves things to where they were.
    ///
    /// note: `protected` is asked here too, which is the question every other move in this tool
    /// asks and this one did not. A person who pins an item after the model elided it has made a
    /// decision about that item; `undo` went straight to `set_state` and took the pin off again,
    /// silently, which is the one thing the word pin promises not to happen.
    ///
    /// note: grouped by the state they land in, rather than an item at a time. One operation is
    /// one undo, and walking back a move of three items left three checkpoints on the person's
    /// stack - so undoing what the model called one change took them three.
    ///
    /// note: `own_turn` is `None` because a change recorded earlier cannot be about the turn this
    /// call is speaking in: that item did not exist when the change was made, and identifiers are
    /// never reused.
    fn apply(self, kernel: &Kernel, mine: &BTreeSet<ContextId>) -> Applied {
        match self {
            Self::States(states) => {
                let mut back = Vec::new();
                let mut left = Vec::new();
                let mut groups: Vec<(ContextState, Option<String>, Vec<ContextId>)> = Vec::new();
                for (id, state, note) in states {
                    let Some(item) = kernel.item(id) else {
                        continue;
                    };
                    if let Some(why) = protected(&item, mine, None) {
                        left.push(format!("[{id}] {why}"));
                        continue;
                    }
                    back.push((id, item.state, item.note.clone()));
                    match groups
                        .iter_mut()
                        .find(|(known, said, _)| *known == state && *said == note)
                    {
                        Some((.., ids)) => ids.push(id),
                        None => groups.push((state, note, vec![id])),
                    }
                }

                let did = (!groups.is_empty()).then(|| {
                    groups
                        .iter()
                        .map(|(state, _, ids)| format!("{} now {state}", numbers(ids)))
                        .collect::<Vec<_>>()
                        .join(", ")
                });
                for (state, note, ids) in groups {
                    kernel.set_state(ids, state, note);
                }

                Applied {
                    inverse: (!back.is_empty()).then_some(Self::States(back)),
                    did,
                    left,
                }
            }
            Self::Said(id, content, meta) => {
                let Some(item) = kernel.item(id) else {
                    return Applied::default();
                };
                if let Some(why) = protected(&item, mine, None) {
                    return Applied {
                        left: vec![format!("[{id}] {why}")],
                        ..Applied::default()
                    };
                }
                let back = Self::Said(id, item.content.clone(), item.meta.clone());
                if kernel.replace(id, content).is_err() {
                    return Applied::default();
                }
                let _ = kernel.annotate(id, meta);

                Applied {
                    inverse: Some(back),
                    did: Some(format!("what [{id}] said")),
                    left: Vec::new(),
                }
            }
        }
    }
}

impl Amend {
    /// Runs one of the eight operations that change something.
    ///
    /// note: it is handed the operation and the reason rather than reading either, because both
    /// are the vocabulary's and the vocabulary is `context`'s: it is the tool a model called, it
    /// holds the list of twelve, and it is what says so when a call names none of them. What is
    /// in here is what changing a context *is*.
    pub(super) fn change(
        &self,
        kernel: &Kernel,
        call: &ToolCall,
        args: &Value,
        op: &str,
        reason: &str,
    ) -> ToolOutput {
        match op {
            "revise" => self.revise(kernel, call, args, reason),
            "note" => self.note(kernel, args, reason),
            "undo" => self.walk(kernel, args, reason, true),
            "redo" => self.walk(kernel, args, reason, false),
            // note: the four moves are operations of their own, named for what they do. They were
            // one `prune` action with a `state` argument once, which put the word for *one* of
            // them over all four - including `pin` and `restore`, which are its opposite, so
            // "prune to pin it" was the documented spelling of protecting something. It also
            // disagreed with every place the result is read back, all of which name the state.
            // Two models in a row spent a call each asking for `restore` and being told it was a
            // state and not an action; the answer was that they were right and the levels were
            // wrong.
            other => self.moved(kernel, call, args, reason, other),
        }
    }
}

/// What to say about the items already carrying the name a new note was just given, if any are.
///
/// note: bought by a live run, and the second time this tool's `label` has taught a model the
/// wrong thing. A session wrote five notes under one name - `in_progress`, then `step 2`, `step
/// 3`, `step 4`, `complete` - each plainly meant to replace the last, and pinned four of them, so
/// the context ended up asserting four different steps at once and compaction could clear none of
/// them. `note` appends: a label is how an item is found again, not a key that stands for one.
/// What the result said was `[17] experiment_status is in your context now`, five times, with a
/// different number each time and nothing to read the number against.
///
/// note: so the clash is said where the mistake is made, and `revise` is named, because the action
/// for what that session was actually doing was already in this tool's own enum. The `[id]` was
/// the only signal before this, and it is a constant-shaped one in a template that reads the same
/// every time - which is the shape a model learns to skim.
///
/// note: what still goes into the request, rather than everything with the name. An archived note
/// costs nothing and contradicts nothing, and a warning about one is a warning about nothing.
fn already(clashes: &[ContextId], label: &str) -> String {
    if clashes.is_empty() {
        return String::new();
    }

    // note: a few of them named and the rest counted, because the point of the sentence is that
    // there *are* others and what to do about it. Forty numbers would say the same thing in forty
    // times the tokens, and the one that matters is reachable by the name either way.
    let shown: Vec<String> = clashes.iter().take(4).map(|id| format!("[{id}]")).collect();

    format!(
        "{}{} {} that name too. A note is a new item every time - a label finds one again rather \
         than standing for one - so `label:{label}` now names {} and every one of them goes into \
         the request. `revise` rewrites the one you wrote before.\n",
        shown.join(", "),
        match clashes.len() - shown.len() {
            0 => String::new(),
            more => format!(" and {more} more"),
        },
        match clashes.len() {
            1 => "carries",
            _ => "carry",
        },
        clashes.len() + 1,
    )
}

/// Whether anything the agent wrote down for itself is still going into the request.
///
/// A note is the one thing in a context that is there because the agent decided a finding was
/// worth keeping. If none is, then everything the agent knows is in items it did not choose, and
/// hiding those is the whole of what it knew.
fn wrote_anything_down(kernel: &Kernel) -> bool {
    kernel.items().iter().any(|item| {
        matches!(item.kind, ContextKind::Reference)
            && item.source == "agent"
            && item.state.sends_content()
    })
}

impl Amend {
    /// Moves items to a state, refusing the ones that are not the model's to move.
    fn moved(
        &self,
        kernel: &Kernel,
        call: &ToolCall,
        args: &Value,
        reason: &str,
        action: &str,
    ) -> ToolOutput {
        // a selector, or a list of numbers, and never both. Naming a class of items is what makes
        // this usable for the job it is mostly for - "the tool results I am done with" is one
        // thought, and reading twelve numbers off a listing to say it is not
        let named = match named(&kernel.items(), args) {
            Ok(named) => named,
            Err(refusal) => return ToolOutput::error(refusal),
        };
        let (selected, ids) = (named.select, named.ids);
        if ids.is_empty() {
            // a selector that parsed and matched nothing is a different mistake from naming no
            // items at all, and telling them apart is the difference between trying again with a
            // better selector and trying again with the same one
            return ToolOutput::error(match selected {
                Some(input) => format!(
                    "`{input}` is a selector, and nothing in your context matches it; \
                     `context` with `look` lists what there is"
                ),
                // the mistake a live run actually made: `label` is in this schema, for naming a
                // `note`, and a model reaching for a way to say *which item* took it. The label
                // was a fine way to name one - `select` reads a bare word as a label - so the
                // useful answer is the spelling of what it meant rather than a list of arguments
                None => match args["label"].as_str() {
                    Some(label) => format!(
                        "`{action}` needs `ids`, or a `select` naming a class of them. `label` \
                         names a `note`, not the items to move - to move the items called \
                         `{label}`, say `select: \"label:{label}\"`"
                    ),
                    None => format!("`{action}` needs `ids`, or a `select` naming a class of them"),
                },
            });
        }
        let Some(state) = state_of(action) else {
            // what each one does rather than only what it is called: the choice between `elide`
            // and `exclude` is the one that decides whether a tool call keeps its answer, and a
            // list of five words does not help anybody make it
            //
            // note: `archive` said "keep it, do not send it, and stop counting it against the
            // budget", which is three things `exclude` also does - so the clause only meant
            // anything by implying that an excluded item is still charged for, and it is not.
            // Measured against a real endpoint, the two produce the same request to the token:
            // 3,451 active, 2,219 either way. What actually separates them is what the person
            // reading the pane is meant to conclude, so that is what the line says now
            return ToolOutput::error(
                "say which move you mean, as the `action`:\n  \
                 elide    - replace what it says with a marker; a tool call keeps its answer\n  \
                 exclude  - take it out of the request; a tool call loses its answer too\n  \
                 archive  - the same, for what you are done with rather than setting aside\n  \
                 pin      - protect it from compaction\n  \
                 restore  - put it back the way it was",
            );
        };

        let before = kernel.budget().used();
        let mine = self.pinned.lock().clone();
        let own = own_turn(kernel, &call.id);

        let (mut allowed, mut refused) = (Vec::new(), Vec::new());
        for id in ids {
            match kernel.item(id) {
                None => refused.push(format!("[{id}] there is no such item")),
                Some(item) => match protected(&item, &mine, own) {
                    Some(why) => refused.push(format!("[{id}] {why}")),
                    None => allowed.push(id),
                },
            }
        }

        // read before the change, because it is the way back from it
        let was: Vec<_> = allowed
            .iter()
            .filter_map(|id| kernel.item(*id))
            .map(|item| (item.id, item.state, item.note.clone()))
            .collect();
        let changed = kernel.set_state(allowed, state, Some(reason.to_owned()));
        for id in &changed.changed {
            self.note_pin(*id, state);
        }
        // note: `StateChange::unchanged` is "already in that state *with that note*", so an item
        // that was pinned and is being pinned again for a different reason comes back as changed -
        // which is true of the note and false of the item. The report was reading it as a move:
        // `pin [2]` on something already pinned said "1 item(s) are now pinned: 2" over figures
        // that had not moved, and put a step in the journal that `undo` then described as "2 back
        // to pinned" about an item that is still pinned. What actually happened is that the reason
        // was rewritten, so that is what it says. `was` is read before the change and already
        // holds the state each item had, so telling the two apart costs nothing.
        let (moved, restated): (Vec<_>, Vec<_>) = was
            .into_iter()
            .filter(|(id, ..)| changed.changed.contains(id))
            .partition(|(_, had, _)| *had != state);
        if !moved.is_empty() {
            self.record(Undoing::States(moved.clone()));
        }

        let numbers_of = |of: &[(ContextId, ContextState, Option<String>)]| {
            numbers(&of.iter().map(|(id, ..)| *id).collect::<Vec<_>>())
        };
        let mut out = format!("{} item(s) are now {state}", moved.len());
        if !moved.is_empty() {
            out.push_str(&format!(": {}", numbers_of(&moved)));
        }
        out.push('\n');
        // everything that did not move, said once and in one place. The kernel splits it in two -
        // `unchanged` is same state *and* same note, `restated` is same state with a new reason -
        // and that is a distinction about the record rather than about the item, so it goes in the
        // clause rather than in a second line of its own
        let still = [numbers_of(&restated), numbers(&changed.unchanged)];
        let still: Vec<&str> = still
            .iter()
            .map(String::as_str)
            .filter(|n| !n.is_empty())
            .collect();
        if !still.is_empty() {
            out.push_str(&format!(
                "{} item(s) were already {state} and did not move: {}",
                restated.len() + changed.unchanged.len(),
                still.join(", "),
            ));
            out.push_str(&match (restated.is_empty(), changed.unchanged.is_empty()) {
                (true, _) => "\n".to_owned(),
                // named only where there is something to tell them from; the reason was rewritten
                // on the ones the kernel reported as changed, and those are all of them here
                (false, true) => {
                    format!(" - what changed is the reason, which now reads `{reason}`\n")
                }
                (false, false) => format!(
                    " - what changed is the reason on {}, which now reads `{reason}`\n",
                    numbers_of(&restated)
                ),
            });
        }
        // the way back, at the moment it becomes worth knowing. A session that elided twenty-two
        // items spent its next two calls guessing at how to put them back and then gave up; six
        // words here are cheaper than that
        //
        // note: `action`, which is what `restore` is. It said `state: "restore"` from when the
        // four moves were a `state` argument, and went on saying it after they became four
        // actions of their own - so a model following the sentence sent an argument nothing
        // reads, which is now refused by name. The way back cost the call it was there to save
        if !moved.is_empty() && !state.sends_content() {
            out.push_str(
                "back: the same ids with `action: \"restore\"`, or `undo` for all of it\n",
            );
            // the failure this closes: a run gathered nineteen thousand tokens of evidence across
            // seventeen tool results, said nothing in its own turns, elided all seventeen at once,
            // and then answered all ten questions from nothing - confidently, and wrong on every
            // one. The tool told it what it had saved and nothing about what it had just spent
            // note: what this used to say was "what those items said survives only in what you
            // have already said", which was true when it was written and stopped being true the
            // day `context: search` arrived - an elided item keeps every byte and only projects
            // as a marker. A live model read the old sentence and told its user the content was
            // gone and no longer retrievable. The warning is still worth making; the claim under
            // it was not, and a warning that overstates its case is how a tool teaches a model
            // something false about its own context.
            if !wrote_anything_down(kernel) {
                out.push_str(
                    "you have no notes: nothing you are carrying says what those items said.\n",
                );
                // note: said outright rather than through `if_offered`, which is what this was
                // while `search` belonged to a tool next door that a session might not have.
                // The tool naming it is the tool that has it now
                out.push_str(
                    "The text is still in them - `search` reads a line of one without putting it \
                     back, and `restore` returns the whole - but a finding you have to go and look \
                     for again is not one you have.\n",
                );
                out.push_str("`note` writes a finding down where pruning cannot reach it.\n");
            }
        }
        for refusal in &refused {
            out.push_str(&format!("refused: {refusal}\n"));
        }
        out.push_str(&cost(
            kernel,
            before,
            match state {
                // the only move whose growth is not the content itself
                ContextState::Elided => Grew::Marker,
                _ => Grew::Content,
            },
        ));

        ToolOutput::new(out)
    }

    /// Rewrites what one item says.
    fn revise(&self, kernel: &Kernel, call: &ToolCall, args: &Value, reason: &str) -> ToolOutput {
        let ids = match ids(args, "ids") {
            Ok(ids) => ids,
            Err(why) => return ToolOutput::error(why),
        };
        let [id] = ids[..] else {
            return ToolOutput::error("`revise` takes exactly one id in `ids`");
        };
        let Some(content) = args["content"].as_str() else {
            return ToolOutput::error("`revise` needs the `content` to put there instead");
        };
        let Some(item) = kernel.item(id) else {
            return ToolOutput::error(format!("there is no context item {id}"));
        };
        if let Some(why) = protected(&item, &self.pinned.lock(), own_turn(kernel, &call.id)) {
            return ToolOutput::error(format!("[{id}] {why}"));
        }

        let before = kernel.budget().used();
        let was = item.tokens;
        if let Err(e) = kernel.replace(id, Content::text(content.to_owned())) {
            return ToolOutput::error(e.to_string());
        }

        // the note cannot be set without a second state change, and a second state change would
        // be a second thing to undo; the metadata slot exists for exactly this and costs no
        // checkpoint, so who rewrote this and why travels with the item either way
        let mut meta = match item.meta.is_object() {
            true => item.meta.clone(),
            false => json!({}),
        };
        // note: `by` is here because a reader of this item should be able to tell whose hand it
        // was, and there are two. This tool is one; the other is a person pressing `e` on the
        // context tab, which replaces in place as well and writes `by: user` for the same reason.
        // So the two paths must not look alike: a model reading its own metadata is never shown
        // its own tool as the editor of a sentence a person rewrote, and the person is never told
        // they did something `amend` did. The `unwrap_or` on the pane that draws this guards
        // somebody else's metadata, not either of those
        meta["revised"] = json!({ "by": "context", "reason": reason, "call": call.id.to_string() });
        let _ = kernel.annotate(id, meta);
        self.record(Undoing::Said(id, item.content.clone(), item.meta.clone()));

        let now = kernel.item(id).map(|item| item.tokens).unwrap_or_default();
        ToolOutput::new(format!(
            "[{id}] {} now says something else: ~{} tokens instead of ~{}. What it said before is \
             on the trace as `context.replaced`, is on the context pane under `enter`, and one \
             undo brings it back.\n{}",
            item.label,
            thousands(now),
            thousands(was),
            // the two figures above already say the new text is the longer one, so the sentence
            // this would add says it a second time
            cost(kernel, before, Grew::Asked),
        ))
    }

    /// Writes something into the context that will still be there later.
    ///
    /// note: the one thing here that *adds*, and it earns its place because everything else an
    /// agent knows is in a turn - and a turn is the first thing a compactor comes for. A plan, a
    /// conclusion, a thing that did not work: written down as an item of its own it can be pinned,
    /// and a pin is a promise the kernel keeps even against a `Compactor`. Saying the same thing
    /// out loud in a turn is not a promise about anything.
    ///
    /// note: the source is `agent`, not `memory` or `user`, so that "who put these 12,000 tokens
    /// in here?" has an answer on the context pane. It is the item's own field for exactly this,
    /// and a tool that attributed its writing to somebody else would be the one dishonest thing
    /// in a program built to show where everything came from.
    /// Writes something the agent decided into its own context, as an item of its own.
    ///
    /// note: worth being clear about what this is *for*, because the obvious objection is that a
    /// model can already think, and thinking is free. Four differences, and each of them is the
    /// reason somebody reaches for this rather than a reasoning block. Reasoning belongs to the
    /// turn that produced it, so pruning that turn prunes the thought - deliberately, since for a
    /// signed thinking block nothing else is safe. Reasoning is not addressable: it has no
    /// identifier, cannot be revised, cannot be pinned, and a compactor cannot be told to leave
    /// it alone. Reasoning is not reliably carried back either - this program's own
    /// OpenAI-compatible dialect has never put it on the wire and cannot, so on that endpoint a
    /// conclusion a model reached by thinking is gone by the next request. And reasoning is not
    /// on the context tab as a row of its own with a reason beside it, which is the difference
    /// between a person being able to see what the agent decided to keep and having to read a
    /// transcript for it.
    ///
    /// note: so a note is the one item in a context that is there because the agent judged a
    /// finding worth keeping, which is why [`wrote_anything_down`] asks about exactly this and
    /// why hiding everything while holding none of them is worth a sentence.
    fn note(&self, kernel: &Kernel, args: &Value, reason: &str) -> ToolOutput {
        let Some(content) = args["content"].as_str().filter(|c| !c.trim().is_empty()) else {
            return ToolOutput::error("`note` needs the `content` to write down");
        };
        // note: the name as it was given, kept apart from the one written on the item, because
        // only a name somebody *chose* can collide with one. Two notes nobody labelled are both
        // called `note` and telling the second that the first "carries that name too" would be
        // reporting a clash between two names the model never picked.
        let named = args["label"].as_str();
        let label = named.unwrap_or("note");
        let pin = args["pin"].as_bool().unwrap_or(false);

        // read before the push, so the new item is not one of its own clashes
        let clashes: Vec<ContextId> = match named {
            Some(name) => kernel
                .items()
                .iter()
                .filter(|item| item.label == name && item.state.sends_content())
                .map(|item| item.id)
                .collect(),
            None => Vec::new(),
        };

        let before = kernel.budget().used();
        // pinned as it is written rather than pinned afterwards: a push and a state change are two
        // checkpoints on the *person's* undo stack, and writing one note is one thing the model
        // did. It is the same reason `revise` puts its own account in the metadata instead of the
        // note, and `because` is already carrying the sentence a second checkpoint would have been
        // spent on
        let mut item = ContextItem::new(ContextKind::Reference, "agent", label, content.to_owned())
            .because(reason.to_owned());
        if pin {
            item = item.pinned();
        }
        let id = kernel.push(item);
        if pin {
            self.note_pin(id, ContextState::Pinned);
        }
        // the way back from having written it is to put it away; nothing here destroys anything,
        // so an undone note is archived and still listed rather than gone
        self.record(Undoing::States(vec![(
            id,
            ContextState::Archived,
            Some("a note this tool wrote, and then walked back".to_owned()),
        )]));

        ToolOutput::new(format!(
            "[{id}] {label} is in your context now, and goes into every request from here on. {}\n{}{}",
            match pin {
                true => "It is pinned, so compaction cannot take it.",
                false =>
                    "It is not pinned, so compaction may take it; say `pin` if it has to last.",
            },
            already(&clashes, label),
            cost(kernel, before, Grew::Asked),
        ))
    }

    /// Walks this tool's own amendments back, or forward again.
    ///
    /// note: the two directions are one loop over two stacks, because an [`Undoing`] applied hands
    /// back the way from where that left things to where they were. There is no separate "redo"
    /// representation to be written, or to fall out of step with the first one.
    fn walk(&self, kernel: &Kernel, args: &Value, reason: &str, back: bool) -> ToolOutput {
        // note: read rather than clamped. It took `as_u64().unwrap_or(1).clamp(1, 64)`, so a
        // `steps` of nought walked one change back, a negative number walked one back, a word
        // walked one back, and a hundred walked sixty-four - each of them a call that did
        // something other than what it said, and the schema advertised none of it
        let steps = match &args["steps"] {
            Value::Null => 1,
            Value::Number(given) if given.as_u64().is_some_and(|it| (1..=WALK).contains(&it)) => {
                given.as_u64().unwrap_or(1) as usize
            }
            given => {
                return ToolOutput::error(format!(
                    "`steps` is `{given}`, and nothing was done. It is how many of your own \
                     changes to walk, from 1 to {WALK}; left out, it is 1."
                ));
            }
        };
        let before = kernel.budget().used();

        let mut put_back = Vec::new();
        let mut left_alone = Vec::new();
        let mut touched = Vec::new();
        // taken before the journal, and copied, so that the two locks are never held in this
        // order anywhere - `note_pin` below holds the other one on its own
        let mine = self.pinned.lock().clone();
        {
            let mut journal = self.journal.lock();
            for _ in 0..steps {
                let taken = match back {
                    true => journal.done.pop(),
                    false => journal.undone.pop(),
                };
                let Some(change) = taken else {
                    break;
                };
                // an item that has since gone, or that is no longer this tool's to move, gives
                // nothing to walk to, and the entry is spent either way rather than left to be
                // retried against a context it no longer describes
                let step = change.apply(kernel, &mine);
                put_back.extend(step.did);
                left_alone.extend(step.left);
                let Some(inverse) = step.inverse else {
                    continue;
                };
                if let Undoing::States(states) = &inverse {
                    touched.extend(states.iter().map(|(id, ..)| *id));
                }
                match back {
                    true => journal.undone.push(inverse),
                    false => journal.done.push(inverse),
                }
            }
        }
        for id in touched {
            if let Some(item) = kernel.item(id) {
                self.note_pin(id, item.state);
            }
        }

        let (done, undone) = {
            let journal = self.journal.lock();
            (journal.done.len(), journal.undone.len())
        };
        let direction = match back {
            true => "back",
            false => "forward again",
        };

        let mut out = match put_back.is_empty() {
            true => format!(
                "there was nothing of yours to walk {direction}. `undo` and `redo` only move the \
                 changes this tool made; the person you work with has an undo of their own, and \
                 it is not this one.\n"
            ),
            false => format!(
                "walked {} of your own change(s) {direction}, because: {reason}\n  {}\n",
                put_back.len(),
                put_back.join("\n  "),
            ),
        };
        if !left_alone.is_empty() {
            out.push_str(&format!(
                "left alone, being no longer yours to move:\n  {}\n",
                left_alone.join("\n  ")
            ));
        }
        out.push_str(&format!(
            "{done} change(s) of yours can still be undone, {undone} redone.\n"
        ));
        // walking back an elision is content returning; walking one forward puts the marker back,
        // and a marker that grew the request is not what somebody undoing something is asking
        // about. Either way it is a state going back to what it was, which is the content
        out.push_str(&cost(kernel, before, Grew::Content));

        ToolOutput::new(out)
    }

    /// Records an amendment, and makes whatever had been walked back unreachable.
    ///
    /// note: the same rule the kernel's own redo stack follows, and for the same reason: a redo
    /// that reached across work done since would be overwriting it rather than restoring anything.
    fn record(&self, undoing: Undoing) {
        let mut journal = self.journal.lock();
        journal.undone.clear();
        journal.done.push(undoing);
    }

    /// Remembers whether this tool is the one holding an item pinned.
    fn note_pin(&self, id: ContextId, state: ContextState) {
        let mut mine = self.pinned.lock();
        match state {
            ContextState::Pinned => mine.insert(id),
            _ => mine.remove(&id),
        };
    }
}

/// The state each of the four moves leaves an item in.
///
/// note: four words for four states, and no second spelling of any of them. It took `excluded`,
/// `unpin`, `unelide`, `active` and `include` too, on the reasoning that accepting a word
/// somebody reached for costs nothing - which was true of the word and not of the program. The
/// schema advertises twelve operations; every place that had to answer "which operation is
/// this call" then needed a table of the words that are not in it, and `needs` was reduced to
/// declaring all twelve for a call it could not place. One list, in the schema, is the whole
/// of the vocabulary now.
fn state_of(word: &str) -> Option<ContextState> {
    Some(match word {
        "exclude" => ContextState::Excluded,
        "elide" => ContextState::Elided,
        "pin" => ContextState::Pinned,
        "restore" => ContextState::Active,
        _ => return None,
    })
}

/// The item holding the assistant turn that asked for this call, if it is still there.
pub(super) fn own_turn(kernel: &Kernel, call: &ToolCallId) -> Option<ContextId> {
    kernel.with_context(|context| {
        context
            .items()
            .iter()
            .rev()
            // `calls()`, or an ordered turn would look like a turn that asked for nothing and the
            // guard below it - that a call cannot excise the turn it is speaking in - would
            // quietly stop holding
            .find(|item| item.calls().any(|asked| &asked.id == call))
            .map(|item| item.id)
    })
}

/// Why a change might leave the next request bigger than it found it, which is what the figures
/// are worth saying beside.
///
/// note: named, and passed by the caller that made the change, because one explanation cannot
/// serve all four of them. It used to: every action reported growth as a marker left behind by an
/// elision, so a live run that wrote a *note* was told its ten extra tokens were the marker of an
/// elision it had not performed. A wrong account of a number is worse than the bare number - the
/// only reason this sentence exists is that models read the two figures and did not work out which
/// way they had gone.
#[derive(Debug, Clone, Copy)]
enum Grew {
    /// An elision replaced content with a marker carrying the reason given for it.
    Marker,
    /// Content that was not going into the request is going into it now.
    Content,
    /// Nothing to account for: the caller asked for something bigger and got it.
    Asked,
}

impl Grew {
    /// The sentence to put after the figures, once it is known that they went up.
    fn by(self, more: usize) -> String {
        match self {
            // an elision on a short item costs more than the content did: a live session elided
            // twenty-two items and added 162 tokens doing it, then went on to elide everything
            // else it had
            Self::Marker => format!(
                " That is {} more than before, not less: what an elided item leaves behind is a \
                 marker carrying your reason for eliding it, and on a short item that costs more \
                 than the content did.",
                thousands(more)
            ),
            Self::Content => format!(
                " That is {} more than before: content that was being left out of the request is \
                 going into it again, and it costs what it says.",
                thousands(more)
            ),
            // a note is content somebody asked to carry, so its cost is the answer to the request
            // rather than a surprise in it. `note` says on its own line that it goes into every
            // request from here on, which is the part worth knowing
            Self::Asked => String::new(),
        }
    }
}

/// What the next request costs now, beside what it cost before the change.
///
/// note: it says which way the figures went in *both* directions, which it did not used to. Growth
/// was accounted for and a drop was left as two numbers to subtract, on the reasoning that a drop
/// is what the caller asked for and needs no explaining. It does. A live session pruned three times
/// running, was told `~9,679, from ~10,273`, then `~9,810, from ~9,840`, then `~10,521, from
/// ~11,137` - three drops - and read all three as growth, because it was comparing each one against
/// a figure it remembered from a `budget` call several turns earlier rather than against the
/// `from ~` in the sentence it had just been handed. It concluded that pruning *adds* cost, acted
/// on the conclusion with `undo steps: 6`, and bought itself 8,619 tokens. Hence both halves of
/// what follows: the direction in words, and where the number it is measured against comes from.
fn cost(kernel: &Kernel, before: usize, grew: Grew) -> String {
    let budget = kernel.budget();
    let now = budget.used();
    format!(
        "the next request is now ~{} tokens{}, from ~{}.{}\n",
        thousands(now),
        budget
            .limit
            .map(|limit| format!(" of {}", thousands(limit)))
            .unwrap_or_default(),
        thousands(before),
        match now.cmp(&before) {
            Ordering::Greater => grew.by(now - before),
            Ordering::Less => format!(
                " That is {} less than before - and before is what the request cost when this \
                 change found it, not what an earlier `budget` said it would: everything that has \
                 landed since is in the figure too.",
                thousands(before - now)
            ),
            // note: said rather than left as two figures that happen to match. A live session
            // pinned an item, was told `now ~6,097 tokens, from ~6,097`, and answered "huh, pinning
            // increased the cost slightly?" - the same misreading as the one above, off two numbers
            // that were not even different. It says nothing about *why* it did not move, because
            // that differs by action and this arm serves all of them - a pin changes what
            // compaction may take rather than what the request carries, and a prune on something
            // already held back has nothing left to take out. What it does rule out is the other
            // reading of an unmoved figure, which is a change that never took
            Ordering::Equal => " That is the same figure as before, to the token, rather than a \
                                change that did not take."
                .to_owned(),
        },
    )
}

/// A list of item numbers, as somebody would read them out.
fn numbers(ids: &[ContextId]) -> String {
    ids.iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
