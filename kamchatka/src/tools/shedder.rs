//! `Shedder`: the compactor, which sheds what the conversation is done with and says exactly what
//! it took.
//!
//! note: two rules, and they answer different questions. The first is about *what has served its
//! purpose*, and it runs before every request whether the context is full or not: a tool result
//! the model has read and whose turn is over goes to a marker, and so does a picture the model has
//! been shown, in an exchange before the one in progress. The second is about *room*: once the
//! context reaches the threshold, the oldest exchanges with the person go whole - the message, the
//! turns answering it and their results - until it is down to the target. The pair is a high- and
//! a low-water mark, so the second rule runs in bursts rather than an exchange at a time before
//! every turn.
//!
//! note: neither rule touches the turn in progress. What the model is in the middle of is what it
//! has not finished using, and a result taken from under it - read once, never answered with -
//! was read again, and elided again on the next step. A turn that fills the context is told so by
//! the full notice, in the same turn, and what goes is for the model or the person to say.
//!
//! note: and neither takes anything the model has not been shown. That is what lets the marker
//! say the model had read what it stands in for, which it needs to say: a model reading a marker
//! that said only "compacted" decided it had never seen the files it had just summarised, called
//! its own summaries fabricated and withdrew them.
//!
//! note: what it frees has to beat what it costs, and both sides are counted on the counter's own
//! scale. An elided item leaves behind a sentence carrying the reason for eliding it, so a result
//! smaller than that sentence is not worth taking - and a counter that has corrected itself
//! upwards prices the sentence higher than a plain bytes-over-four does.
//!
//! note: with one exception, which is a blob. A [`Content::Blob`](nachalnik::Content::Blob) is
//! counted at `0` by every counter in this workspace, because what a picture costs is a formula
//! over its dimensions and no byte length reaches one - so that same check reads a picture as
//! recovering nothing, and the largest thing in the context would be the one thing this could
//! never take. Where it cannot judge, it takes.

use std::{collections::HashSet, sync::Arc};

use nachalnik::{
    Budget, CompactionPlan, Compactor, Content, ContextId, ContextItem, ContextKind, ContextState,
    ToolCallId, async_trait,
};

use crate::app::named_calls;

/// Elides what the conversation is done with as it goes, and drops its oldest exchanges once the
/// context gets full - and says so.
///
/// note: it does not summarize what it takes, and nothing it leaves behind claims more than that
/// the content existed, was read, and is gone - a compactor that invented a paraphrase of output
/// it never read would be putting words in a tool's mouth. Every change is reversible: the items
/// keep what they hold, they stay in the context pane with it counted as held back, and restoring
/// one is a keystroke. Anything pinned is left where it is.
///
/// note: blobs go as soon as they have been shown, on principle rather than on measurement, and
/// the principle is that this is a *terminal* client. It renders no pictures and is not going to,
/// so a blob here is a cost the person cannot see, paid out of a budget that cannot count it, for
/// a payload the screen will only ever name. A client that shows pictures should want a different
/// rule, which is why this one is `kamchatka`'s and not the runtime's.
pub struct Shedder {
    /// How full the context has to be before exchanges start going.
    pub threshold: f64,
    /// How empty it is trying to get it once they do.
    ///
    /// note: at most `threshold`. Equal to it, every pass that drops anything drops just enough to
    /// be under the threshold again, which is an exchange or so before nearly every turn once the
    /// context is full - and each of those moves the start of the request, which is what a
    /// provider's prompt cache keys on. Below it, the drops come in bursts and the cache survives
    /// the turns between them.
    pub target: f64,
}

impl Shedder {
    /// One that starts dropping at this fraction of the limit and aims below it.
    ///
    /// note: the target is twenty points under where it starts, or half of it, whichever leaves
    /// more. Below a fifth the subtraction goes negative and the target becomes no context at all,
    /// so something has to catch it; a fraction of the threshold catches it without ever rising
    /// above it.
    #[must_use]
    pub fn under(threshold: f64) -> Self {
        Self {
            threshold,
            target: (threshold - 0.2).max(threshold / 2.0),
        }
    }
}

#[async_trait]
impl Compactor for Shedder {
    /// note: always, because the first rule is not about the budget. A result is done with once
    /// its turn is over however empty the context is, and the only one who can tell that is
    /// `plan`, which has the items. What it costs is one walk of them per request, which is what
    /// the projection already does twice; and `plan` answers `None` when there is nothing to do,
    /// which is most requests inside a turn.
    fn should_compact(&self, _: &Budget) -> bool {
        true
    }

    /// note: the threshold alone. Something unpriced is a reason to look, and every request is
    /// looked at anyway; it is not a sign the context is full: a pinned picture is unpriced for
    /// the whole session, and a context holding one at five percent is not full
    fn wants_room(&self, budget: &Budget) -> bool {
        budget
            .fraction_used()
            .is_some_and(|used| used >= self.threshold)
    }

    async fn plan(&self, items: &[Arc<ContextItem>], budget: &Budget) -> Option<CompactionPlan> {
        // what the counter active here calls four bytes, taken from the items in hand, so that a
        // marker and a summary are priced on the same scale as the results they stand in for
        let scale = scale(items);
        let full = self.wants_room(budget);

        // written before anything is chosen, because what one elision recovers depends on them:
        // the kernel makes the pass's reason the note on every item it elides, and the note is
        // the marker. The model reads it too, in the brackets the projector puts round it, so each
        // is written to be read by both - what happened, why, and what to do about it
        //
        // note: one reason for every item in a pass, so it has to be true of all of them. Both say
        // the model had read what they stand in for, which no pass takes anything but: a model
        // reading a marker that said only "compacted" decided it had never seen the files it had
        // just summarised, and withdrew its summaries
        //
        // note: the one about room asks for the part rather than the whole, because a model told
        // only that it may read something again reads a file too big for the context again, and
        // again, every read discarded on arrival - and the part it needs is what `grep` is for
        //
        // note: the percentage is of the *context* - `context_tokens`, the figure the report's
        // `tokens_before` and `tokens_after` are counted on - rather than `fraction_used`, which has
        // the tool definitions in it, and a share that rounds to nothing is said as under one
        // percent rather than as `0%`, which reads as a pass run on an empty context
        let limit = budget.limit.unwrap_or_default();
        let reached = match limit {
            0 => "0%".to_owned(),
            limit => match (budget.context_tokens as f64 / limit as f64 * 100.0).round() as usize {
                0 if budget.context_tokens > 0 => "under 1%".to_owned(),
                share => format!("{share}%"),
            },
        };
        let served = format!(
            "{MARK} after you had read it, to keep the context short. Read it again if you still \
             need it"
        );
        let room_made = format!(
            "{MARK} after you had read it, because the context had reached {reached} of the \
             {limit}-token limit. If you still need it, ask for the part you need rather than the \
             whole - there is little room for it"
        );
        // the longer of the two, so that whichever one the pass ends up with, nothing it takes is
        // smaller than the line standing in for it
        let marker = [&served, &room_made]
            .into_iter()
            .map(|reason| marker_tokens(reason, scale))
            .max()
            .unwrap_or_default();

        // note: `sends_content` rather than `is_projected`. An elided item *is* projected - as a
        // marker - so with `is_projected` every item a pass had already elided would come back as
        // a candidate on the next one, and the plan would never be `None`
        //
        // note: and not a pinned one, which `sends_content` says yes to. The kernel refuses those
        // - a pin is a promise - so proposing one produces a plan that moves nothing and still
        // costs a turn of the screen saying it refused
        //
        // note: and not one whose call is no longer sent, which the projector leaves out and the
        // kernel will not elide
        let asked: HashSet<&ToolCallId> = items
            .iter()
            .filter(|item| item.state.is_projected())
            .flat_map(|item| item.calls().map(|call| &call.id))
            .collect();
        // what the request carries of a turn: its words, or a call something still answers. A turn
        // whose only call lost its result is no turn at all to the projector, which leaves it out
        // - and the kernel will not move what the request is not carrying, so named, it is named
        // again by every pass after, each of them crediting itself with what it never sent
        let answered: HashSet<&ToolCallId> = items
            .iter()
            .filter(|item| item.state.is_projected())
            .filter_map(|item| match &item.kind {
                // whether its call is asked is not asked here: `answered` is only read for turns that
                // are being sent, and a call in one of those is asked by definition
                ContextKind::ToolResult { call, .. } => Some(call),
                _ => None,
            })
            .collect();
        let carried = |item: &ContextItem| {
            item.state.is_projected()
                && match &item.kind {
                    ContextKind::ToolResult { call, .. } => asked.contains(call),
                    ContextKind::AssistantMessage { .. } => {
                        item.state.is_elided()
                            || item.content.as_text() != Some("")
                            || item.calls().any(|call| answered.contains(&call.id))
                    }
                    _ => true,
                }
        };
        let sheddable = |item: &ContextItem| {
            item.state.sends_content()
                && item.state != ContextState::Pinned
                && match &item.kind {
                    ContextKind::ToolResult { call, .. } => asked.contains(call),
                    // an attachment, which is a picture or a document brought in at the prompt;
                    // one of text goes with its exchange rather than on its own
                    ContextKind::Reference => carries_blob(&item.content),
                    _ => false,
                }
        };

        // what the model has been shown: everything before its last turn, which was answering a
        // request that carried it. Taken before then, a search that fitted the limit was elided on
        // its way in - the model ran it again for a marker, and again
        let seen = items
            .iter()
            .rposition(|item| matches!(item.kind, ContextKind::AssistantMessage { .. }))
            .unwrap_or(0);
        let exchanges = exchanges(items);
        // where the turn in progress starts, which neither rule reaches into; with no message from
        // the person at all, everything is that turn
        let current = exchanges.last().map_or(0, |turn| turn.start);

        let mut used = budget.used();
        let mut routine = Vec::new();

        // the first rule: what is done with. A result whose turn is over, and a picture that has
        // been shown wherever it is
        //
        // note: not one carrying a note. The kernel sets none on a result, so a note is somebody
        // having said something about this item - most often a person restoring it from the tab -
        // and eliding it again before the next request would undo what they just did. The room
        // rule still takes it with its exchange; a pin is what keeps something
        for (index, item) in items.iter().enumerate() {
            if index >= seen || !sheddable(item) || item.note.is_some() {
                continue;
            }
            // nothing of the turn in progress, a picture included: it is what the model is still
            // working from
            if index >= current {
                continue;
            }
            match carries_blob(&item.content) {
                // what a blob recovers is credited as the counter has it - nothing, today - and is
                // here for the counter behind `set_counter` that does know what a picture costs
                true => used -= item.tokens.saturating_sub(marker).min(used),
                false => {
                    // a result no bigger than the marker is not worth eliding at all: the pass
                    // would spend the person's undo and a line of the model's attention to make
                    // the request bigger
                    let Some(net) = item.tokens.checked_sub(marker).filter(|net| *net != 0) else {
                        continue;
                    };
                    used -= net.min(used);
                }
            }
            routine.push(item.id);
        }

        let mut remove = Vec::new();
        let mut dropped = 0usize;
        // what the room half of the pass has freed, on the counter's scale and before its own
        // summary is paid for; the half that has not freed enough to be worth that is not run
        let mut recovered = 0usize;
        if let Some(limit) = budget.limit.filter(|_| full) {
            let target = (limit as f64 * self.target) as usize;
            // what a pin holds on to: the kernel refuses to exclude either half of a pinned call
            // and its result, so naming one would be a refusal on the screen and nothing moved
            //
            // note: and the whole of the turn a pinned result answers, not only its own call. The
            // turn is kept for the pin's sake, so a result of it dropped beside the pinned one
            // leaves a call in it with no answer, which the projector then has to take out of the
            // turn on every request from then on - the same shape `/load` keeps whole for the same
            // reason
            let mut pinned: HashSet<&ToolCallId> = items
                .iter()
                .filter(|item| item.state == ContextState::Pinned)
                .flat_map(|item| named_calls(item))
                .collect();
            let turns: Vec<&ToolCallId> = items
                .iter()
                .filter(|item| item.calls().any(|call| pinned.contains(&call.id)))
                .flat_map(|item| item.calls().map(|call| &call.id))
                .collect();
            pinned.extend(turns);

            // the second rule: the oldest exchanges, whole, and never the one in progress
            //
            // note: whole, because half of one is a conversation nobody had - a question with no
            // answer, or a result whose call has gone - and removed rather than elided, because
            // a marker for every message of a forgotten exchange is the exchange's length in
            // lines saying there was something there. The summary says it once
            for exchange in &exchanges[..exchanges.len().saturating_sub(1)] {
                if used <= target {
                    break;
                }
                let mut took = false;
                for item in &items[exchange.start..exchange.end] {
                    // note: and only what the request is carrying: a result whose call is out
                    // already, or a turn with nothing left in it, is left out by the projector -
                    // counted, it is a saving the request never sees
                    if !goes_with_its_exchange(item)
                        || !carried(item)
                        || item.state == ContextState::Pinned
                        || named_calls(item).any(|call| pinned.contains(call))
                    {
                        continue;
                    }
                    // what it is sending now, which for anything elided is the line standing in for
                    // it - this pass's marker for what this pass elided, and the item's own note for
                    // what was elided before, which may be a few words somebody wrote rather than
                    // a sentence of this file's. Priced at this pass's marker, three of those read
                    // as a hundred and fifty tokens freed, and a drop of short lines that freed
                    // almost nothing passed for one worth its summary and grew the request
                    let sending = match (item.state.is_elided(), routine.contains(&item.id)) {
                        (_, true) => marker,
                        (true, false) => {
                            marker_tokens(item.note.as_deref().unwrap_or_default(), scale)
                        }
                        (false, false) => item.tokens,
                    };
                    used -= sending.min(used);
                    recovered += sending;
                    took = true;
                    remove.push(item.id);
                }
                dropped += took as usize;
            }
        }

        // the room half leaves a summary behind, because what it removes leaves nothing behind of
        // its own; the first rule does not, because every item it takes is a marker in its own
        // place, saying what happened to it
        let summary = (dropped != 0).then(|| summary(items, dropped));

        // note: what the room half is worth is what it freed less what its summary costs, and one
        // that comes out under is not run. The margin is the summary again, so it has to free
        // twice what its own bookkeeping costs before it spends a turn of the model's attention on
        // it - otherwise an exchange of two short lines goes for a paragraph saying it went
        let (summary, elide, remove): (_, Vec<ContextId>, Vec<ContextId>) = match summary {
            Some(said) if recovered >= summary_tokens(&said, scale).saturating_mul(2) => (
                Some(said),
                // what this pass was eliding and has dropped with its exchange instead is not
                // elided as well
                routine
                    .into_iter()
                    .filter(|id| !remove.contains(id))
                    .collect(),
                remove,
            ),
            _ => (None, routine, Vec::new()),
        };
        if elide.is_empty() && remove.is_empty() {
            return None;
        }

        // note: the last pass's summary goes out as this one goes in, so that a long session does
        // not pile up copies of the same sentence in a context the pass was called on to make
        // room in. `remove` and not `elide`, because a marker where a summary was is a line of text
        // saying a line of text has been taken away; and not the pinned ones, which the kernel
        // refuses. A pass that leaves no summary leaves the last one standing, and it is still
        // true: it counts every exchange a pass has dropped, and none has come back since
        let remove = match &summary {
            Some(_) => remove
                .into_iter()
                .chain(
                    items
                        .iter()
                        .filter(|item| {
                            item.source == "compaction"
                                && item.state.sends_content()
                                && item.state != ContextState::Pinned
                        })
                        .map(|item| item.id),
                )
                .collect(),
            None => remove,
        };

        let reason = match summary.is_some() || full {
            true => room_made,
            false => served,
        };

        Some(CompactionPlan {
            summary: summary.map(ContextItem::summary),
            reason,
            elide,
            remove,
        })
    }
}

/// The first words of a compaction reason, and how this compactor's own work is told from anyone
/// else's.
///
/// note: one string rather than a comparison against the sentence it begins, so that rewording the
/// marker and telling a marker from a model's own note cannot come apart - the two answers this
/// gives are the same sentence, read two ways. The kernel puts it at the start of the note on an
/// elided item and after `compaction: ` on a removed one.
const MARK: &str = "compacted";

/// One exchange with the person: a message from them, everything answering it, and the files they
/// brought in with it - as a range of indices into the items.
struct Exchange {
    start: usize,
    end: usize,
}

/// The conversation as exchanges, oldest first; what comes before the person's first message - the
/// system instruction and what the session was set up with - is in none of them.
///
/// note: an exchange starts at the person's message, or at the files attached just before it,
/// since `/attach` puts the file in and then the question. Starting it at the message would leave
/// each attachment at the end of the exchange *before* the one it was brought in for, and dropping
/// that one would take the file the current question is about.
fn exchanges(items: &[Arc<ContextItem>]) -> Vec<Exchange> {
    let mut starts = Vec::new();
    let mut floor = 0;
    for (index, item) in items.iter().enumerate() {
        if item.kind != ContextKind::UserMessage {
            continue;
        }
        let mut start = index;
        while start > floor && brought_in(&items[start - 1]) {
            start -= 1;
        }
        starts.push(start);
        floor = index + 1;
    }

    starts
        .iter()
        .enumerate()
        .map(|(n, start)| Exchange {
            start: *start,
            end: starts.get(n + 1).copied().unwrap_or(items.len()),
        })
        .collect()
}

/// Whether this is a file the person brought into the conversation, rather than one the session
/// was set up with or a note somebody wrote.
fn brought_in(item: &ContextItem) -> bool {
    item.kind == ContextKind::Reference && matches!(item.source.as_str(), "file" | "selection")
}

/// Whether this goes when the exchange it is in goes.
///
/// note: the conversation's own items and the files brought into it, and nothing else. A note the
/// model wrote for itself through `context` is the one thing it wrote to outlast the exchange it
/// wrote it in, a summary of an earlier pass is replaced rather than dropped, and the notice that
/// the context is full is the kernel's to retire. All of them are in some exchange or other, and
/// none of them is that exchange's.
fn goes_with_its_exchange(item: &ContextItem) -> bool {
    matches!(
        item.kind,
        ContextKind::UserMessage
            | ContextKind::AssistantMessage { .. }
            | ContextKind::ToolResult { .. }
    ) || brought_in(item)
}

/// What the room half of a pass leaves behind: how much of the conversation has gone, counting
/// every pass and not only this one, since it replaces the summary the last one left.
///
/// note: only what a *compactor* took, told apart by the note. A message the person excluded by
/// hand is gone too, and counting it here would tell the model more had been dropped to make room
/// than any pass had dropped. The kernel sets a removal's note to `compaction: ` and the reason,
/// and the reason begins with [`MARK`].
fn summary(items: &[Arc<ContextItem>], dropped: usize) -> String {
    let ours = |item: &ContextItem| {
        item.note
            .as_deref()
            .and_then(|note| note.strip_prefix("compaction: "))
            .is_some_and(|note| note.starts_with(MARK))
    };
    let exchanges = dropped
        + items
            .iter()
            .filter(|item| {
                item.kind == ContextKind::UserMessage
                    && item.state == ContextState::Excluded
                    && ours(item)
            })
            .count();

    format!(
        "The {exchanges} earliest exchange(s) of this conversation - each a message from the \
         person, your turns answering it and their tool results - have been excluded to make \
         room. You can no longer read them, and nothing else stands in their place."
    )
}

/// How many tokens the counter active here gives four bytes, taken from the items in hand.
///
/// note: a [`Calibrating`](nachalnik::Calibrating) counter learns from what providers charge and
/// is routinely above `1.0`, so a marker priced at a plain four bytes a token comes out under what
/// it really costs. A result smaller than its own replacement then reads as worth eliding, and the
/// pass reports tokens recovered while the request grows.
///
/// note: every kind but the assistant's turns, because those are the one kind whose figure is more
/// than its content - a turn counts its calls and its thinking on top, so including one would read
/// that overhead as a bigger ratio. Anything carrying a picture is left out for the reason it is
/// everywhere else in this file: the counter prices a blob at nothing.
///
/// note: not the tool results alone, which is what this read once. A conversation that has
/// mostly talked has none, so the scale fell back to `1.0` under a counter reading forty percent
/// high, and the summary a drop leaves was priced at the plain estimate - an exchange worth twice
/// that went for a sentence that cost more than half of it.
///
/// note: not the budget. Its `context_tokens` is what the *projection* costs, which carries every
/// label and bracket the projector adds and cannot be divided by a byte count this does not have.
///
/// note: the ratio rather than an average of per-item ones, so one short result cannot set the
/// scale for the rest; and `1.0` where there is nothing to derive it from, which is the plain
/// estimate this file made before a counter could be anything else.
fn scale(items: &[Arc<ContextItem>]) -> f64 {
    let (counted, bytes) = items
        .iter()
        .filter(|item| {
            !matches!(item.kind, ContextKind::AssistantMessage { .. })
                && !carries_blob(&item.content)
        })
        .fold((0, 0), |(counted, bytes), item| {
            (
                counted + item.tokens,
                bytes + item.content.byte_len().div_ceil(4),
            )
        });

    match bytes {
        0 => 1.0,
        _ => counted as f64 / bytes as f64,
    }
}

/// Estimates what one elision marker costs: the pass's reason, in the brackets
/// [`LinearProjector`](nachalnik::LinearProjector) puts round it, at the scale [`scale`] has
/// derived.
///
/// note: the same scale as the [`ContextItem::tokens`] it is subtracted from, which is the whole
/// of it. An estimate of a marker's size measured on a different ruler from the result it replaces
/// is not a small inaccuracy: it decides which side of the line a result falls on.
///
/// note: taken from the reason rather than fixed, because the reason is what the marker says. A
/// constant here would be a second place to remember when that sentence is reworded.
fn marker_tokens(reason: &str, scale: f64) -> usize {
    // `[... ` and ` ...]`, which the projector supplies and this does not get to choose
    ((reason.len() + 10).div_ceil(4) as f64 * scale).round() as usize
}

/// Estimates what the summary a pass leaves behind costs: its own text, behind the label the
/// projector puts in front of a reference, at the scale [`scale`] has derived.
///
/// note: the label, because a summary is a `Reference` and a reference projects as
/// `label:\ntext`; a sentence priced without it is a couple of tokens short, which is nothing
/// next to being priced without the scale.
fn summary_tokens(text: &str, scale: f64) -> usize {
    let label = ContextItem::summary(Content::text("")).label.len();
    let bytes = label + 2 + text.len();

    (bytes.div_ceil(4) as f64 * scale).round() as usize
}

/// Whether the content is a blob or has one somewhere inside it.
///
/// note: [`Content::blobs`] does the walking rather than this. The nesting is the hard part - a
/// sentence-and-a-screenshot turn is a blob one level down inside `Content::Blocks` - and
/// `Content` and `Block` are both `#[non_exhaustive]`, so a walk written out here would answer
/// `false` for a variant added later and a picture would quietly stop going first, with no arm
/// this file could have written to catch it. The runtime's own counter needs the same walk for
/// the same reason, and a second copy here would go stale without anybody noticing.
fn carries_blob(content: &Content) -> bool {
    !content.blobs().is_empty()
}
