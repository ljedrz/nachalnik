//! The half of `context` that changes it: change items' states, rewrite one, write something down,
//! and walk any of it back.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::Arc;

use nachalnik::{
    Capability, Content, ContextId, ContextItem, ContextKind, ContextState, Kernel, ToolCall,
    ToolCallId, ToolOutput,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    app::{
        Going,
        text::{beyond_a_prompt, reasoning_beyond, thousands},
    },
    introspect::{ids, named, protected, unknown},
    tools::{Careful, yes_or_no},
};

use super::{CHANGES, Pinned};

/// Changes the context: changes items' states, rewrites one, writes something down, walks its
/// own changes back.
///
/// note: it keeps the set of items it pinned itself, stopping a model from unpinning what a person
/// pinned. A pin is a promise to the person, not the model.
///
/// note: it also keeps a journal of what it has done, which is what `undo` walks, rather than
/// [`Kernel::undo`]. The kernel's undo stack is the person's, bound to the `u` key in the terminal,
/// and a model walking it back would undo the person's work, not its own. While a tool is running,
/// the top of that stack is always the assistant turn that asked for the call: one step would erase
/// the model's question, orphan the answer it waits for, and leave the loop rebuilding a request
/// from before it asked. A journal of this tool's own changes has neither problem.
pub(super) struct Changes {
    pinned: Pinned,
    /// What its answers may send the model to; see `introspect::if_reachable`.
    policy: Arc<Careful>,
    journal: Arc<Mutex<Journal>>,
    /// Held for the whole of one operation; see [`Changes::make`].
    one_at_a_time: Mutex<()>,
}

impl Changes {
    /// Builds one; [`Context`](super::Context) is the only caller.
    ///
    /// note: the journal is handed in, shared with whoever writes the session down, so a snapshot
    /// carries it and a resumed session's model can walk back what it did before - see
    /// [`crate::introspect::History`]. `undo` walks only what this session's model did: the journal
    /// starts empty, and only a snapshot of this session fills it.
    ///
    /// note: the pinned set is handed in too, because it is shared with the half that reports it:
    /// what was pinned here is what `budget` says the model may unpin. A second set would make the
    /// two disagree about a promise.
    pub(super) fn new(pinned: Pinned, policy: Arc<Careful>, journal: Arc<Mutex<Journal>>) -> Self {
        Self {
            pinned,
            policy,
            journal,
            one_at_a_time: Mutex::new(()),
        }
    }
}

/// How many of its own changes [`Changes`] keeps to walk back; the oldest go first.
///
/// note: the kernel's own undo depth, `Config::context_undo_depth`'s default. Each entry holds the
/// items as they were, old content included, and the journal goes into every snapshot, so without
/// a limit a session that revises large items writes every old version at the end of every turn.
/// `undo`'s description in `context/mod.rs` gives the number.
const DEPTH: usize = 16;

/// What [`Changes`] has made and not yet walked back.
///
/// note: a snapshot written while `context` still had `redo` also carries an `undone` list beside
/// this one. It is ignored on reading: there is no operation left to walk it.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Journal {
    done: Vec<Entry>,
    /// Whether this was read back out of a snapshot, rather than started empty.
    #[serde(skip)]
    pub(crate) carried: bool,
}

impl Journal {
    /// Takes what this tool just did to these items as the way it left them, in every entry.
    ///
    /// note: every change this tool makes, not only the ones it journals. Pinning a pinned item for
    /// a new reason rewrites its note and changes no state, so it is no step of its own. An entry
    /// still holding the old note would read the model's own restating as somebody else's change,
    /// and refuse to walk back a pin the model had made.
    fn refresh(&mut self, kernel: &Kernel, ids: &[ContextId]) {
        for entry in &mut self.done {
            for (id, left) in &mut entry.left {
                if ids.contains(id)
                    && let Some(item) = kernel.item(*id)
                {
                    *left = Left::of(&item);
                }
            }
        }
    }
}

/// One change in the journal: the way back from it, and what it left each touched item as.
///
/// note: what it left them as, because somebody else may have moved one since. The model's undo put
/// an item back where the model had it, whatever happened in between. An item that no longer looks
/// the way this change left it is left alone, and the answer says so.
#[derive(Serialize, Deserialize)]
struct Entry {
    undoing: Undoing,
    left: Vec<(ContextId, Left)>,
}

/// What an item looked like: its state, its note, what it says and what it thought.
///
/// note: not its metadata, which this tool writes beside a pin, so a pin's own bookkeeping is not
/// read as somebody else's change.
///
/// note: `serde(default)` on the reasoning, so a journal written before it was kept reads as one
/// whose turns had none. An entry about a turn that did have some then reads as moved since and is
/// left alone, which is the safe way to be wrong.
#[derive(PartialEq, Serialize, Deserialize)]
struct Left {
    state: ContextState,
    note: Option<String>,
    content: Content,
    #[serde(default)]
    reasoning: Option<Content>,
}

impl Left {
    fn of(item: &ContextItem) -> Self {
        Self {
            state: item.state,
            note: item.note.clone(),
            content: item.content.clone(),
            reasoning: item.reasoning().cloned(),
        }
    }
}

impl Entry {
    /// The change, and each item it touched as it is now.
    fn now(kernel: &Kernel, undoing: Undoing) -> Self {
        let left = undoing
            .ids()
            .into_iter()
            .filter_map(|id| kernel.item(id).map(|item| (id, Left::of(&item))))
            .collect();

        Self { undoing, left }
    }
}

/// Why an item is no longer a change's to walk back, given what the change left it as.
fn moved_since(left: &[(ContextId, Left)], item: &ContextItem) -> Option<String> {
    let (_, left) = left.iter().find(|(id, _)| *id == item.id)?;
    (*left != Left::of(item)).then(|| {
        "has changed since this tool changed it - by the person you are working with, or by \
         compaction - so it stays as it is now"
            .to_owned()
    })
}

/// One change, recorded as the way back from it.
///
/// note: the way back rather than the change itself, because that is the one thing an entry is ever
/// used for.
#[derive(Serialize, Deserialize)]
enum Undoing {
    /// Put these items into these states, with these notes.
    States(Vec<(ContextId, ContextState, Option<String>)>),
    /// Put this text and this metadata back on this item.
    Said(ContextId, Content, Value),
    /// Put this reasoning and this metadata back on this turn.
    Reasoned(ContextId, Option<Content>, Value),
}

impl Undoing {
    /// The items whose state it would change.
    fn ids(&self) -> Vec<ContextId> {
        match self {
            Self::States(states) => states.iter().map(|(id, ..)| *id).collect(),
            Self::Said(id, ..) | Self::Reasoned(id, ..) => vec![*id],
        }
    }
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
    /// Applies it, and returns the way back from where that leaves things to where they were.
    ///
    /// note: `protected` is checked here too, as in every other state change. Pinning an item after
    /// the model elided it is a decision about that item. An `undo` that went straight to
    /// `set_state` would remove the pin silently, which a pin promises will not happen.
    ///
    /// note: one [`Kernel::set_states`] for the whole operation, not one `set_state` per item or
    /// state. One operation is one undo; otherwise walking back three items into two states would
    /// leave two checkpoints on the stack - two undos for one change. Items are grouped by the
    /// state they land in, to say what was done.
    ///
    /// note: `own_turn` is `None` because a change recorded earlier cannot be about this call's
    /// turn: the item did not exist when the change was made, and identifiers are never reused.
    fn apply(
        self,
        kernel: &Kernel,
        mine: &crate::introspect::Mine,
        left_as: &[(ContextId, Left)],
    ) -> Applied {
        match self {
            Self::States(states) => {
                let mut back = Vec::new();
                let mut left = Vec::new();
                let mut groups: Vec<(ContextState, Option<String>, Vec<ContextId>)> = Vec::new();
                for (id, state, note) in states {
                    let Some(item) = kernel.item(id) else {
                        continue;
                    };
                    if let Some(why) =
                        protected(&item, mine, None).or_else(|| moved_since(left_as, &item))
                    {
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
                kernel.set_states(groups.into_iter().flat_map(|(state, note, ids)| {
                    ids.into_iter().map(move |id| (id, state, note.clone()))
                }));

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
                if let Some(why) =
                    protected(&item, mine, None).or_else(|| moved_since(left_as, &item))
                {
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
            Self::Reasoned(id, reasoning, meta) => {
                let Some(item) = kernel.item(id) else {
                    return Applied::default();
                };
                if let Some(why) =
                    protected(&item, mine, None).or_else(|| moved_since(left_as, &item))
                {
                    return Applied {
                        left: vec![format!("[{id}] {why}")],
                        ..Applied::default()
                    };
                }
                let back = Self::Reasoned(id, item.reasoning().cloned(), item.meta.clone());
                if kernel.replace_reasoning(id, reasoning).is_err() {
                    return Applied::default();
                }
                let _ = kernel.annotate(id, meta);

                Applied {
                    inverse: Some(back),
                    did: Some(format!("the reasoning of [{id}]")),
                    left: Vec::new(),
                }
            }
        }
    }
}

impl Changes {
    /// Runs one of the eight operations that change something.
    ///
    /// note: the operation and reason are handed in rather than read here, because the vocabulary
    /// belongs to the tool a model called. That tool holds the list of twelve and says when a call
    /// names none of them.
    ///
    /// note: one at a time, because under `--parallel-tool-calls` two calls run on two threads. An
    /// operation is a kernel change and then an entry describing it. A walkback between the two
    /// would take the entry before the one it is about, and an entry made after another call's
    /// change describes items as that call left them, not as this one did. The order of parallel
    /// calls is unspecified; each is whole.
    pub(super) fn make(
        &self,
        kernel: &Kernel,
        call: &ToolCall,
        args: &Value,
        op: &str,
        reason: &str,
    ) -> ToolOutput {
        // the outermost lock, taken before `pinned` and the journal wherever either is taken
        let _one = self.one_at_a_time.lock();
        match op {
            "revise" => self.revise(kernel, call, args, reason),
            "revise_reasoning" => self.revise_reasoning(kernel, call, args, reason),
            "note" => self.note(kernel, args, reason),
            "undo" => self.walk(kernel, reason),
            // note: the four state changes are separate operations named for what they do, not one
            // `prune` action with a `state` argument. That would put one word over all four,
            // including `pin` and `reset_state`, which are its opposite, so "prune to pin it" would
            // mean protecting something. It would also disagree with every place the result is
            // read back, which name the state.
            //
            // note: the state is read here, where the four are named, not inside the change. Inside
            // it, an unknown word would need a listing of what to say instead, kept current in a
            // branch nothing can reach. `CHANGES` gets a call this far; the four state changes are
            // the rest.
            other => match state_of(other) {
                Some(state) => self.moved(kernel, call, args, reason, other, state),
                None => ToolOutput::error(unknown(other, &CHANGES)),
            },
        }
    }
}

/// What to say about the items already carrying the name a new note was just given, if any are.
///
/// note: `note` appends: a label is how an item is found again, not a key that stands for one. A
/// model that reads it as a key writes a run of notes under one name, each meant to replace the
/// last, and pins them, so the context asserts several different steps at once and compaction can
/// clear none of them. Without this, all the result says is `[17] <label> is in your context now`,
/// each time, with a different number and nothing to read the number against.
///
/// note: so the clash is said where the mistake is made, and `revise` is named, because it is the
/// action for what such a model is doing. The `[id]` alone is a constant-shaped signal in a
/// template that reads the same every time, which a model learns to skim.
///
/// note: what still goes into the request, rather than everything with the name. An excluded note
/// costs nothing and contradicts nothing, so it needs no warning.
fn already(clashes: &[ContextId], label: &str) -> String {
    if clashes.is_empty() {
        return String::new();
    }

    // note: a few are named and the rest counted, because the point is that others exist and what
    // to do about them. Forty numbers would say the same in forty times the tokens; the one that
    // matters is reachable by the name either way.
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

impl Changes {
    /// Puts items into a state, refusing the ones that are not the model's to change.
    fn moved(
        &self,
        kernel: &Kernel,
        call: &ToolCall,
        args: &Value,
        reason: &str,
        action: &str,
        state: ContextState,
    ) -> ToolOutput {
        // a selector, or a list of numbers, and never both. A selector lets a model name a class
        // of items, such as "the tool results I am done with", instead of reading twelve numbers
        // off a listing
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
                    "`{input}` is a selector, and nothing in your context matches it.{}{}",
                    crate::introspect::if_reachable(kernel, &self.policy, "context:read", || {
                        " `context` with `look` lists what there is.".to_owned()
                    }),
                    crate::introspect::unmatched(kernel, input)
                ),
                // the mistake a model actually makes: `label` is in this schema, for naming a
                // `note`, and a model reaching for a way to say *which item* takes it. The label
                // is a fine way to name one - `select` reads a bare word as a label - so the
                // useful answer is the spelling of what it meant rather than a list of arguments
                None => match args["label"].as_str() {
                    Some(label) => format!(
                        "`{action}` needs `ids`, or a `select` naming a class of them. `label` \
                         names a `note`, not the items to change - to change the items called \
                         `{label}`, say `select: \"label:{label}\"`"
                    ),
                    None => format!("`{action}` needs `ids`, or a `select` naming a class of them"),
                },
            });
        }
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
            self.note_pin(kernel, *id, state, Some(reason.to_owned()));
        }
        self.journal.lock().refresh(kernel, &changed.changed);
        // note: `StateChange::unchanged` is "already in that state *with that note*", so an item
        // that was pinned and is being pinned again for a different reason comes back as changed,
        // which is true of the note and false of the item. Read as a state change, `pin [2]` on
        // something already pinned would say "1 item(s) are now pinned: 2" over figures that had
        // not moved, and put a step in the journal that `undo` would describe as "2 back to pinned"
        // about an item that is still pinned. What happened is that the reason was rewritten, so
        // that is what it says. `was` is read before the change and already holds the state each
        // item had, so telling the two apart costs nothing.
        let (moved, restated): (Vec<_>, Vec<_>) = was
            .into_iter()
            .filter(|(id, ..)| changed.changed.contains(id))
            .partition(|(_, had, _)| *had != state);
        if !moved.is_empty() {
            self.record(kernel, Undoing::States(moved.clone()));
        }

        let numbers_of = |of: &[(ContextId, ContextState, Option<String>)]| {
            numbers(&of.iter().map(|(id, ..)| *id).collect::<Vec<_>>())
        };
        let mut out = format!("{} item(s) are now {state}", moved.len());
        if !moved.is_empty() {
            out.push_str(&format!(": {}", numbers_of(&moved)));
        }
        out.push('\n');
        // everything whose state did not change, said once and in one place. The kernel splits it
        // in two - `unchanged` is same state *and* same note, `restated` is same state with a new
        // reason - and that is a distinction about the record rather than about the item, so it
        // goes in the clause rather than in a second line of its own
        let still = [numbers_of(&restated), numbers(&changed.unchanged)];
        let still: Vec<&str> = still
            .iter()
            .map(String::as_str)
            .filter(|n| !n.is_empty())
            .collect();
        if !still.is_empty() {
            out.push_str(&format!(
                "{} item(s) were already {state}: {}",
                restated.len() + changed.unchanged.len(),
                still.join(", "),
            ));
            out.push_str(&match (restated.is_empty(), changed.unchanged.is_empty()) {
                (true, _) => "\n".to_owned(),
                // Show numbers only when they distinguish items. The reason was rewritten on every
                // changed item here
                (false, true) => {
                    format!(" - what changed is the reason, which now reads `{reason}`\n")
                }
                (false, false) => format!(
                    " - what changed is the reason on {}, which now reads `{reason}`\n",
                    numbers_of(&restated)
                ),
            });
        }
        // the way back, at the moment it becomes worth knowing. A model that has just put a batch
        // of items away otherwise spends calls guessing at how to put them back; a line here is
        // cheaper than that
        //
        // note: `action`, which is what `reset_state` is. A `state` argument is one nothing reads
        // and is refused by name, so a sentence that named one would cost the call it is there to
        // save
        if !moved.is_empty() && !state.sends_content() {
            out.push_str(
                "back: the same ids with `action: \"reset_state\"`, or `undo` for all of it\n",
            );
            // the failure this closes: a model can gather its evidence in tool results, say nothing
            // about it in its own turns, put all of it away at once, and then answer from nothing -
            // confidently, and wrong. Without this the tool tells it what it saved and nothing
            // about what it just spent
            //
            // note: the warning does not say the content is gone, because it is not: an elided item
            // keeps every byte and only projects as a marker, and `search` reads it. A model told
            // the content is gone tells its user so, and an overstated warning teaches a model
            // something false about its own context.
            if !wrote_anything_down(kernel) {
                out.push_str(
                    "you have no notes: nothing you are carrying says what those items said.\n",
                );
                // note: through the policy even here, where the tool naming `search` is the tool
                // that has it: a rule may be about one of its operations and not the others
                let ways = self.policy.ways(&[
                    (
                        Capability::parse("context:read").expect("a capability"),
                        "`search` reads a line of one without putting it back",
                    ),
                    (
                        Capability::parse("context:state").expect("a capability"),
                        "`reset_state` returns the whole",
                    ),
                ]);
                out.push_str(&format!(
                    "The text is still in them{} - but a finding you have to go and look for \
                     again is not one you have.\n",
                    ways.map(|ways| format!(" - {ways}")).unwrap_or_default()
                ));
                out.push_str(&crate::introspect::if_reachable(
                    kernel,
                    &self.policy,
                    "context:note",
                    || {
                        "`note` writes a finding down in an item of its own, which this change \
                         does not touch.\n"
                            .to_owned()
                    },
                ));
            }
        }
        for refusal in &refused {
            out.push_str(&format!("refused: {refusal}\n"));
        }
        out.push_str(&cost(
            kernel,
            before,
            match state {
                // the only state change whose growth is not the content itself
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
        // note: said to be a refusal, as a state change's is. `[11] pinned by the person ..., and a
        // pin is a promise` on its own read as a remark about the item, and a model that got it
        // told the person its revision had gone through
        if let Some(why) = protected(&item, &self.pinned.lock(), own_turn(kernel, &call.id)) {
            return ToolOutput::error(format!("refused: [{id}] {why} - nothing was revised"));
        }
        // note: here rather than in `protected`, because it is the one change that puts words in
        // somebody's mouth. A message excluded or elided is still what the person said, and the
        // pane shows it so; one revised has every later request read them as having said the
        // model's text. A model meaning to revise its note and handed the id of the message asking
        // for it did exactly that. What it means can go beside the message instead
        if matches!(item.kind, ContextKind::UserMessage) {
            return ToolOutput::error(format!(
                "refused: [{id}] is a message the person wrote, and their words are theirs to \
                 change - nothing was revised. `note` puts what you meant to say beside it"
            ));
        }
        // note: a tool result is what the tool answered, not something the model wrote down, so
        // rewriting it would put output in the context that no tool produced - and every later
        // request, and every fork, would read it as the tool's. It is refused here rather than
        // only discouraged in the description, which a model can ignore. What the model wants to
        // keep of a result goes in a note, and the result itself can be elided or excluded
        if matches!(item.kind, ContextKind::ToolResult { .. }) {
            return ToolOutput::error(format!(
                "refused: [{id}] is a tool result, which is what the tool answered - rewriting it \
                 would put output in your context that no tool produced. Nothing was revised. \
                 `note` writes down what you want to keep of it, and `elide` or `exclude` puts \
                 the result away"
            ));
        }
        // note: the three shapes a person's edit is refused for, refused here for the same reason:
        // text written over a picture is the picture gone, and over a turn whose calls live beside
        // or among its words it is the calls gone - and their results with them, orphaned and
        // silently repaired out of the request. The first paragraph is the part that is not about
        // an editor on a screen
        if let Some(why) = beyond_a_prompt(&item) {
            let why = why.split("\n\n").next().unwrap_or(why);
            return ToolOutput::error(format!(
                "[{id}] {why} `revise` writes text, and would write it over the whole item."
            ));
        }

        // the kernel takes this as no operation, and so does this: no metadata, no journal entry
        let content = Content::text(content.to_owned());
        if item.content == content {
            return ToolOutput::new(format!(
                "[{id}] already says exactly that, so nothing was changed"
            ));
        }

        let before = kernel.budget().used();
        let was = item.tokens;
        if let Err(e) = kernel.replace(id, content) {
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
        // The two paths must not look alike: a model reading its own metadata is never shown its
        // own tool as the editor of a sentence a person rewrote, and the person is never told they
        // did something `context` did. The `unwrap_or` on the pane that draws this guards somebody
        // else's metadata, not either of those
        meta["revised"] = json!({ "by": "context", "reason": reason, "call": call.id.to_string() });
        let _ = kernel.annotate(id, meta);
        self.journal.lock().refresh(kernel, &[id]);
        self.record(
            kernel,
            Undoing::Said(id, item.content.clone(), item.meta.clone()),
        );

        let now = kernel.item(id).map(|item| item.tokens).unwrap_or_default();
        // note: the write goes through whatever the item's state, and an item the request does not
        // carry is one whose new words nobody reads until it is put back, which the figures below
        // cannot say, since they are the same either way
        let going = Going::of(kernel);
        let unread = match kernel.item(id) {
            Some(now) if !going.sends_content(&now) => match going.left_out.get(&id) {
                Some(why) => format!(
                    " It is not going into the next request ({why}), so that request does not \
                     read what it says now."
                ),
                None => format!(
                    " It is `{}`, so the next request does not read what it says now; \
                     `reset_state` puts it back.",
                    now.state
                ),
            },
            _ => String::new(),
        };
        ToolOutput::new(format!(
            "[{id}] {} now says something else: ~{} tokens instead of ~{}.{unread} What it said \
             before is in the session's record as `context.replaced`{}, the person sees it on \
             their context pane, and one undo brings it back.\n{}",
            item.label,
            thousands(now),
            thousands(was),
            reads_back(kernel, &self.policy, id),
            // the two figures above already say the new text is the longer one, so the sentence
            // this would add says it a second time
            cost(kernel, before, Grew::Asked),
        ))
    }

    /// Rewrites what one of the model's replies thought, or takes it away.
    ///
    /// note: [`Changes::revise`] for the reasoning slot, refused for the same items and for the
    /// ones `reasoning_beyond` names. The person's messages and tool results need no refusal of
    /// their own here: neither is a reply, so neither has reasoning.
    ///
    /// note: allowed at all because a reply corrected with `revise` otherwise keeps the reasoning
    /// that led to the mistake, and a later request that carries reasoning reads the two as
    /// disagreeing. It is a subject of its own, `context:revise_reasoning`, so a person can allow
    /// one without the other, and it leaves `context.reasoning_replaced` on the trace with what the
    /// reasoning was.
    fn revise_reasoning(
        &self,
        kernel: &Kernel,
        call: &ToolCall,
        args: &Value,
        reason: &str,
    ) -> ToolOutput {
        let ids = match ids(args, "ids") {
            Ok(ids) => ids,
            Err(why) => return ToolOutput::error(why),
        };
        let [id] = ids[..] else {
            return ToolOutput::error("`revise_reasoning` takes exactly one id in `ids`");
        };
        let Some(content) = args["content"].as_str() else {
            return ToolOutput::error(
                "`revise_reasoning` needs the `content` to put there instead; an empty string \
                 removes the reasoning",
            );
        };
        let Some(item) = kernel.item(id) else {
            return ToolOutput::error(format!("there is no context item {id}"));
        };
        if let Some(why) = protected(&item, &self.pinned.lock(), own_turn(kernel, &call.id)) {
            return ToolOutput::error(format!("refused: [{id}] {why} - nothing was revised"));
        }
        if let Some(why) = reasoning_beyond(&item) {
            return ToolOutput::error(format!("refused: [{id}] {why} Nothing was revised."));
        }

        let reasoning = (!content.trim().is_empty()).then(|| Content::text(content.to_owned()));
        let had = item.reasoning().map(|was| was.to_text());
        if had == reasoning.as_ref().map(|now| now.to_text()) {
            return ToolOutput::new(format!(
                "[{id}]'s reasoning is already exactly that, so nothing was changed"
            ));
        }

        let before = kernel.budget().used();
        let was = item.tokens;
        if let Err(e) = kernel.replace_reasoning(id, reasoning.clone()) {
            return ToolOutput::error(e.to_string());
        }

        // under a key of its own, for the reason `App::revise_reasoning` gives
        let mut meta = match item.meta.is_object() {
            true => item.meta.clone(),
            false => json!({}),
        };
        meta["reasoning_revised"] =
            json!({ "by": "context", "reason": reason, "call": call.id.to_string() });
        let _ = kernel.annotate(id, meta);
        self.journal.lock().refresh(kernel, &[id]);
        self.record(
            kernel,
            Undoing::Reasoned(id, item.reasoning().cloned(), item.meta.clone()),
        );

        let now = kernel.item(id).map(|item| item.tokens).unwrap_or_default();
        // note: whether anything reads it, which is a real question here rather than an edge case:
        // most endpoints are not sent reasoning back at all. A model told only that the rewrite
        // went through spends its next turns correcting thinking nobody will read
        let going = Going::of(kernel);
        let unread = match kernel.item(id) {
            Some(now) if !going.sends_content(&now) => match going.left_out.get(&id) {
                Some(why) => format!(
                    " The reply is not going into the next request ({why}), so that request \
                     does not read it."
                ),
                None => format!(
                    " The reply is `{}`, so the next request does not read it.",
                    now.state
                ),
            },
            Some(now) if reasoning.is_some() && going.sends_reasoning(&now) == Some(false) => {
                " Reasoning is not sent back to the model in this session, so no later request \
                 reads it; what changed is the record the person reads."
                    .to_owned()
            }
            _ => String::new(),
        };
        let did = match reasoning {
            Some(_) => "now holds other reasoning",
            None => "no longer has any reasoning",
        };
        ToolOutput::new(format!(
            "[{id}] {did}: ~{} tokens instead of ~{}.{unread} What it was before is in the \
             session's record as `context.reasoning_replaced`{}, and one undo brings it back.\n{}",
            thousands(now),
            thousands(was),
            reads_back(kernel, &self.policy, id),
            cost(kernel, before, Grew::Asked),
        ))
    }

    /// Writes something into the context that will still be there later.
    ///
    /// note: the one thing here that *adds*, and it earns its place because everything else an
    /// agent knows is in a turn, and a turn is the first thing a compactor comes for. A plan, a
    /// conclusion, a thing that did not work: written down as an item of its own it can be pinned,
    /// and a pin is a promise the kernel keeps even against a `Compactor`. Saying the same thing in
    /// a turn is not a promise about anything.
    ///
    /// note: the source is `agent`, not `memory` or `user`, so that "who put these 12,000 tokens in
    /// here?" has an answer, on the chat line that records the item and in `look`. It is the item's
    /// own field for this, and a tool that attributed its writing to somebody else would be the one
    /// dishonest thing in a program built to show where everything came from.
    ///
    /// note: a note rather than a reasoning block, although a model can already think and thinking
    /// is free. Reasoning belongs to the turn that produced it, so excluding that turn takes the
    /// thought, necessarily, since for a signed thinking block nothing else is safe. Reasoning is
    /// not addressable: it has no identifier of its own, cannot be pinned apart from its turn, and
    /// a compactor cannot be told to leave it alone. Nor is it reliably carried back: this program's own
    /// OpenAI-compatible dialect does not put it on the wire and cannot, so on that endpoint a
    /// conclusion a model reached by thinking is gone by the next request. And reasoning is not on
    /// the context tab as a row of its own with a reason beside it, so a person cannot see what the
    /// agent decided to keep without reading a transcript.
    ///
    /// note: so a note is the one item in a context that is there because the agent judged a
    /// finding worth keeping, which is why [`wrote_anything_down`] asks about this and why hiding
    /// everything while holding none of them is worth a sentence.
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
        let pin = match yes_or_no(args, "pin") {
            Ok(pin) => pin,
            Err(refusal) => {
                return ToolOutput::error(format!("{refusal} Nothing was written down."));
            }
        };

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
        // pin as written rather than afterwards. A push and a state change are two undo
        // checkpoints; one note is one thing the model did. `revise` puts its account in metadata
        // instead of the note for the same reason, and `because` already carries the sentence a
        // second checkpoint would have been spent on
        let mut item = ContextItem::new(ContextKind::Reference, "agent", label, content.to_owned())
            .because(reason.to_owned());
        if pin {
            // with the reason as the note, which is what says the pin is the model's; see `Mine`
            item = item.pinned();
            item.note = Some(reason.to_owned());
        }
        let id = kernel.push(item);
        if pin {
            self.note_pin(kernel, id, ContextState::Pinned, Some(reason.to_owned()));
        }
        // to undo having written it, put it away. Nothing here destroys anything, so an undone note
        // is excluded and still listed rather than gone
        self.record(
            kernel,
            Undoing::States(vec![(
                id,
                ContextState::Excluded,
                Some("a note this tool wrote, and then walked back".to_owned()),
            )]),
        );

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

    /// Walks this tool's last change back.
    fn walk(&self, kernel: &Kernel, reason: &str) -> ToolOutput {
        let before = kernel.budget().used();

        // taken before the journal and copied, so the two locks are never held in this order
        // anywhere; `note_pin` holds the other one on its own
        let mine = self.pinned.lock().clone();
        let (step, touched) = {
            let mut journal = self.journal.lock();
            match journal.done.pop() {
                None => (None, Vec::new()),
                Some(Entry { undoing, left }) => {
                    // an item that has since gone, or is no longer this tool's to change, gives
                    // nothing to walk to. The entry is spent either way rather than retried against
                    // a context it no longer describes
                    let step = undoing.apply(kernel, &mine, &left);
                    let touched = match &step.inverse {
                        Some(inverse) => {
                            // what this left items as, for every other entry naming the same items
                            journal.refresh(kernel, &inverse.ids());
                            match inverse {
                                Undoing::States(states) => {
                                    states.iter().map(|(id, ..)| *id).collect()
                                }
                                Undoing::Said(..) | Undoing::Reasoned(..) => Vec::new(),
                            }
                        }
                        None => Vec::new(),
                    };
                    (Some(step), touched)
                }
            }
        };
        for id in touched {
            if let Some(item) = kernel.item(id) {
                self.note_pin(kernel, id, item.state, item.note.clone());
            }
        }

        let (done, carried) = {
            let journal = self.journal.lock();
            (journal.done.len(), journal.carried)
        };

        let (did, left_alone) = match step {
            Some(step) => (Some(step.did), step.left),
            None => (None, Vec::new()),
        };
        let mut out = match did {
            Some(Some(did)) => {
                format!("walked back your last change, because: {reason}\n  {did}\n")
            }
            // note: which of the two it was, because the model reads this sentence and the two are
            // opposites. An entry spent on a refusal existed; telling the model there was nothing
            // of yours to walk back reads as never having made the change. The only other evidence
            // is the counter moving under it
            Some(None) => "your last change could not be walked back, and `undo` will not try \
                           it again. `undo` only takes back the changes this tool made; the \
                           person you work with has an undo of their own, and it is not this \
                           one.\n"
                .to_owned(),
            None => {
                let mut said = "there was nothing of yours to walk back. `undo` only takes \
                                back the changes this tool made; the person you work with has an \
                                undo of their own, and it is not this one.\n"
                    .to_owned();
                // note: a snapshot written before the journal went into it carries none, so after a
                // resume from one the model's earlier changes are not in it either. Without this,
                // the answer reads as though they had been the person's
                if super::reads::resumed(kernel) && !carried {
                    said.push_str(
                        "This session was resumed from a snapshot, and the changes this tool made \
                         before that were not carried over; `reset_state` puts an item back by \
                         name.\n",
                    );
                }
                said
            }
        };
        if !left_alone.is_empty() {
            out.push_str(&format!(
                "left alone, being no longer yours to change:\n  {}\n",
                left_alone.join("\n  ")
            ));
        }
        out.push_str(&format!("{done} change(s) of yours can still be undone.\n"));
        // walking back an elision returns content, and a marker that grew the request is not what
        // an undo asks about: it is a state going back to what it was, which is the content
        out.push_str(&cost(kernel, before, Grew::Content));

        ToolOutput::new(out)
    }

    /// Records a change.
    fn record(&self, kernel: &Kernel, undoing: Undoing) {
        let entry = Entry::now(kernel, undoing);
        let mut journal = self.journal.lock();
        journal.done.push(entry);
        let over = journal.done.len().saturating_sub(DEPTH);
        journal.done.drain(..over);
    }

    /// Remembers whether this tool holds an item pinned, and with what note.
    ///
    /// note: in the item's metadata as well as in `Mine`, because `Mine` is the process's and the
    /// metadata is the snapshot's. Without it, a resumed session had every pin as the person's,
    /// which fails safe and refused the model a pin it had made itself. `annotate` takes no
    /// checkpoint, so a pin is still one undo on the person's stack.
    fn note_pin(&self, kernel: &Kernel, id: ContextId, state: ContextState, note: Option<String>) {
        if state == ContextState::Pinned
            && let Some(item) = kernel.item(id)
        {
            let mut meta = match item.meta.is_object() {
                true => item.meta.clone(),
                false => json!({}),
            };
            meta[crate::introspect::PINNED] = json!({ "by": "context", "note": note });
            let _ = kernel.annotate(id, meta);
        }
        hold(&mut self.pinned.lock(), id, state, note);
    }
}

/// Writes down whether the model holds an item pinned, in whichever copy of the set is at hand.
fn hold(
    mine: &mut crate::introspect::Mine,
    id: ContextId,
    state: ContextState,
    note: Option<String>,
) {
    match state {
        ContextState::Pinned => mine.insert(id, note),
        _ => mine.remove(&id),
    };
}

/// The state each of the four state changes leaves an item in.
///
/// note: four words for four states, and no second spelling of any of them. Accepting a synonym
/// costs nothing for the word and something for the program: the schema advertises twelve
/// operations, so every place that answers "which operation is this call" would need a table of the
/// words that are not in it, and `needs` would be reduced to declaring all twelve for a call it
/// could not place. The one list in the schema is the whole vocabulary.
fn state_of(word: &str) -> Option<ContextState> {
    Some(match word {
        "exclude" => ContextState::Excluded,
        "elide" => ContextState::Elided,
        "pin" => ContextState::Pinned,
        "reset_state" => ContextState::Active,
        _ => return None,
    })
}

/// Every item of the turn this call is speaking in, from the assistant turn that asked to the
/// message that started it.
///
/// note: `own_turn` answers for one item, but a turn is more than the item holding the call. The
/// user message is its own context item, and the question the model is asking sits in it. Skipping
/// the assistant turn and nothing else told a model searching for a name, identifier or error
/// string it was just handed that the context already held it, when the only place it was is the
/// question.
///
/// note: the walk goes to the boundary, not one item, because a turn's assistant turns are not
/// adjacent: each result lands between them. A call in the fourth assistant turn has two results
/// and three turns in front of it. A turn is the message that started it, its assistant turns and
/// their results; the first item that is none of those ends the walk. A session past the request
/// cap is a turn with no new message, so a walk from a later call reaches the message the person
/// wrote instead of stopping at the turn boundary the loop can see.
pub(super) fn asking_turn(kernel: &Kernel, call: &ToolCallId) -> BTreeSet<ContextId> {
    let Some(asked) = own_turn(kernel, call) else {
        return BTreeSet::new();
    };

    kernel.with_context(|context| {
        let mut turn = BTreeSet::from([asked]);
        for item in context
            .items()
            .iter()
            .rev()
            .skip_while(|item| item.id != asked)
            .skip(1)
        {
            turn.insert(item.id);
            if !matches!(
                item.kind,
                ContextKind::AssistantMessage { .. } | ContextKind::ToolResult { .. }
            ) {
                break;
            }
        }

        turn
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

/// Why a change might make the next request bigger than before.
///
/// note: the caller that made the change passes this value, because one explanation cannot serve
/// all four variants: it would tell a model that wrote a *note* that its extra tokens were the
/// marker of an elision it had not performed, and a wrong account of a number is worse than the
/// bare number. The sentence is there at all because models read the two figures and do not work
/// out which way they went.
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
            // an elision on a short item costs more than the content did, and a model not told so
            // goes on eliding to save tokens it is spending
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
            // request from here on
            Self::Asked => String::new(),
        }
    }
}

/// What the next request costs now, compared with what it cost before the change.
///
/// note: it says which way the figure went, up or down. A drop still needs explaining, even though
/// the caller asked for it. A model compares the new figure with one it remembers from a `budget`
/// call several turns earlier, not with the `from ~` in the sentence it has just been handed. It
/// reads a drop as growth, concludes that excluding *adds* cost, and walks its work back. So the
/// sentence states the direction in words and says where the compared number comes from.
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
            // note: this says the figures are the same instead of leaving two matching numbers,
            // because a model reads growth even into two numbers that do not differ, the same
            // misreading as above. It does not say why the figure did not move, because the reason
            // differs by action and this arm serves all actions: a pin changes what compaction may
            // take rather than what the request carries, and an exclusion of something already held
            // back has nothing left to take out. It rules out the other reading of an unmoved
            // figure: a change that never took
            Ordering::Equal => " That is the same figure as before, to the token, rather than a \
                                change that did not take."
                .to_owned(),
        },
    )
}

/// Where the model reads back what a rewrite of `id` overwrote, if it can: `log`, by the item's
/// number.
///
/// note: the record rather than "the trace", which is the person's tab and nothing the model can
/// open. Said only where `log:read` is reachable, as every pointer to another tool is.
fn reads_back(kernel: &Kernel, policy: &Careful, id: ContextId) -> String {
    crate::introspect::if_reachable(kernel, policy, "log:read", || {
        format!(" (`log` with `ids: [{id}]` reads it)")
    })
}

/// A list of item numbers, as somebody would read them out.
fn numbers(ids: &[ContextId]) -> String {
    ids.iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
