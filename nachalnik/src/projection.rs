//! Context items in, wire messages out.
//!
//! note: the seam where the shape of a request is decided, and the one seam whose default a real
//! API may well disagree with - which is why the trait is one method: a projector that disagrees
//! replaces the whole shape rather than overriding a piece of it. What the default does behind
//! that one method is three passes - count the calls against the results, build a message per
//! item, put them in the order the wire needs - and none of that is anybody else's to reach.
//! [`Projection`] is the answer to "what is about to be sent", available before anything is, and
//! the [`Skipped`] list beside it is why that answer is shorter than the context it came from.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::{Context, Kernel};
use crate::{
    context::{ContextId, ContextItem, ContextKind},
    model::{Block, Content, Message, Part, Role, ToolCall, ToolCallId},
};

/// An item that did not make it into the request, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    /// The item's identifier.
    pub id: ContextId,
    /// Why it was left out.
    pub reason: String,
}

/// The messages a context projects to, plus the paper trail of how they came about.
///
/// note: This is the answer to "what will be sent in the next request?", and it is available
/// *before* anything is sent, via [`Kernel::preview_request`] and [`Kernel::project`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    /// The messages, in the order they will be sent.
    pub messages: Vec<Message>,
    /// The items that contributed, in order.
    pub included: Vec<ContextId>,
    /// The items that did not, and why.
    pub skipped: Vec<Skipped>,
    /// What the projector had to **take out** to keep the request valid, in plain words: a call
    /// whose result is not here, a result whose call is not, an ordered turn a flat shape cannot
    /// carry.
    ///
    /// note: every one of these is content the model would have had and will not, which is why
    /// they are worth putting in front of somebody rather than leaving on a page they have to go
    /// and open. See [`Projection::reordered`] for the adjustments that are not.
    pub repairs: Vec<String>,
    /// What it had to **move**, in plain words. Nothing is lost by one of these.
    ///
    /// note: a separate list from [`Projection::repairs`] because it is a different piece of news.
    /// A tool result has to reach the wire immediately after the call it answers, so an item
    /// pushed between the two sends it down the list and the projector puts it back - the layout
    /// rule working, not a fault. A tool that writes to the context, such as `kamchatka`'s
    /// `context: note`, does this on every call, because its item is written while the call that
    /// writes it is still in flight. The order really is not the context's order, and saying so
    /// belongs in a request preview; it does not belong in a line that reads like something went
    /// wrong, and that stays in the conversation for the rest of the session.
    pub reordered: Vec<String>,
}

/// Turns context items into the messages of a request.
///
/// note: This is where the shape of a request lives. The kernel has no opinion about whether a
/// file belongs in a user message, a system message or a preamble; it asks the projector, and
/// shows you the result.
///
/// [`LinearProjector`] speaks the dialect in which a tool result is a message of its own. Not
/// every API agrees - some want tool results as blocks inside a user turn, some keep
/// thinking-only turns, some take a single string - and none of that belongs in a kernel. A whole
/// projector is one method:
///
/// ```
/// use std::sync::Arc;
///
/// use nachalnik::{ContextItem, Message, Projection, Projector, Role, Skipped};
///
/// /// The dialect in which the entire context is one user message.
/// struct OneMessage;
///
/// impl Projector for OneMessage {
///     fn project(&self, items: &[Arc<ContextItem>]) -> Projection {
///         let (mut text, mut included, mut skipped) = (String::new(), Vec::new(), Vec::new());
///
///         for item in items {
///             // whatever the shape, an item that is not sent is reported rather than dropped
///             if !item.is_projected() {
///                 skipped.push(Skipped { id: item.id, reason: item.state.to_string() });
///                 continue;
///             }
///             text.push_str(&format!("{}: {}\n", item.label, item.content.to_text()));
///             included.push(item.id);
///         }
///
///         Projection {
///             messages: vec![Message::new(Role::User, text)],
///             included,
///             skipped,
///             repairs: Vec::new(),
///             reordered: Vec::new(),
///         }
///     }
/// }
/// ```
///
/// note: A projector also decides which of two shapes an assistant turn goes out in: a content
/// slot, a reasoning slot and a flat list of calls, or an ordered sequence of [`Block`]s where
/// the order is part of the message. [`LinearProjector::send_blocks`] picks. What no projector
/// can do is recover an order that was never recorded, which is why the order is a property of
/// [`Content`] rather than of the projection.
pub trait Projector: Send + Sync {
    /// Projects the items - all of them, in insertion order, whatever their state - into the
    /// messages of a request.
    ///
    /// note: called with the context locked, so it must not call back into the kernel it is
    /// installed in: the items it needs are the ones it is handed.
    fn project(&self, items: &[Arc<ContextItem>]) -> Projection;

    /// What this is, for a client that wants to say which one is installed.
    ///
    /// note: The default is the implementing type's own path, which costs an implementor nothing
    /// and is right often enough to be worth having. Override it to say something friendlier. It
    /// is for showing a person, not for matching on: `type_name` makes no stability promise.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// Puts several blocks' worth of content into the one slot a conventional message has.
///
/// note: one of them is carried through untouched, because a [`Content::Json`] thinking block is
/// how a signed or encrypted one travels, and a provider that received it as a string of its own
/// serialization could not send it back. Several can only be joined as text, which is why
/// [`LinearProjector::send_blocks`] exists and why joining is reported.
///
/// note: what cannot survive either way is [`Part::extra`] - a conventional message has a
/// [`Content`] in each slot and nowhere to put what a provider attached to it. The caller reports
/// that rather than this, because a repair reads better naming the item it happened to.
fn join(parts: Vec<&Part>) -> Option<Content> {
    match parts.len() {
        0 => None,
        1 => Some(parts[0].content.clone()),
        _ => Some(Content::text(
            parts
                .iter()
                .map(|part| part.content.to_text())
                .collect::<Vec<_>>()
                .join("\n"),
        )),
    }
}

/// The default [`Projector`]: one message per item, in insertion order.
///
/// The mapping is:
///
/// | kind | message |
/// | --- | --- |
/// | [`ContextKind::System`] | [`Role::System`] |
/// | [`ContextKind::UserMessage`] | [`Role::User`] |
/// | [`ContextKind::AssistantMessage`] | [`Role::Assistant`], carrying its tool calls and reasoning |
/// | [`ContextKind::ToolResult`] | [`Role::Tool`], carrying its call identifier |
/// | [`ContextKind::Reference`] | [`Role::User`] |
///
/// note: The flags below are the only judgement calls this projector makes, and every one of them
/// can be turned off.
///
/// note: This is the projector for the dialect in which a tool result is a message of its own
/// and an assistant message must carry content. A provider whose format disagrees - tool results
/// as blocks inside a user turn, thinking-only turns that have to be echoed - wants a projector
/// of its own, which is why this is a trait.
#[derive(Debug, Clone, Copy)]
pub struct LinearProjector {
    /// Whether a reference's label is prepended to its text, as `label:\n<content>`.
    ///
    /// note: With this off, the model is handed a file's contents without being told which file
    /// it is looking at, which is rarely what anyone wants - but it is a decision about the
    /// prompt, so it is visible and optional rather than baked in.
    pub label_references: bool,
    /// Whether tool calls and tool results whose counterpart is not in the projection are
    /// dropped from it.
    ///
    /// note: This is what makes pruning safe. Most providers reject a request in which an
    /// assistant turn asks for a tool that never gets a result (or vice versa), so removing one
    /// half of a pair would otherwise turn into a request error at the worst possible moment.
    /// Every drop is listed in [`Projection::repairs`].
    ///
    /// note: Calls are paired to results one for one, in order, rather than by set membership.
    /// Call identifiers are meant to be unique - the kernel enforces it for everything a
    /// provider produces (see [`Event::ToolCallRepaired`](crate::Event::ToolCallRepaired)) - but
    /// a context assembled by hand or restored from elsewhere can still repeat one, and counting
    /// keeps the number of calls and the number of results equal even then.
    pub repair_orphans: bool,
    /// What a [`ContextState::Elided`](crate::ContextState::Elided) item is shown as when it
    /// carries no note of its own.
    ///
    /// note: the words are the item's `note` where it has one, because whoever elided it knows
    /// why and this projector does not; all this supplies is the brackets around it and this
    /// fallback. It reads like [`Content::truncate_to`](crate::Content::truncate_to)'s marker on
    /// purpose - they are the same promise to the model, made at two different limits, and a
    /// model that has learnt to read one reads the other.
    pub elision: &'static str,
    /// Whether an assistant turn is projected as an ordered sequence of
    /// [`Block`](crate::Block)s rather than as a content slot, a reasoning slot and a list of
    /// calls.
    ///
    /// note: off by default, because this projector speaks the dialect in which a turn *is* those
    /// three slots, and that is what every provider built against it reads. Turn it on for an API
    /// whose assistant turn is a list of typed blocks - thinking, text, a tool call, more thinking
    /// before the next one - where the order is part of the message.
    ///
    /// note: it applies to every assistant turn, not only to the ones that were recorded as
    /// blocks. A turn recorded the conventional way is assembled into the conventional order -
    /// thinking, then what was said, then what was asked for - which is the order a provider was
    /// assuming anyway; so a context holding some of each projects to one shape rather than two.
    ///
    /// note: with it off, a turn recorded as blocks is flattened back into the three slots, and
    /// where that loses something - two thinking blocks joined into one, a sentence that came
    /// *after* a call - it says so in [`Projection::repairs`] rather than doing it quietly. The
    /// flag exists because for a signed thinking block that is not good enough.
    pub send_blocks: bool,
    /// Whether an assistant turn's reasoning is carried back into
    /// [`Message::reasoning`](crate::Message::reasoning).
    ///
    /// note: On by default, because a provider that does not want it simply ignores the field,
    /// whereas a provider that needs it - a reasoning model whose API verifies a signed thinking
    /// block against the turn it belongs to - cannot invent it. Turn it off to keep reasoning in
    /// the record without spending it on every subsequent request.
    ///
    /// note: an elided turn's thinking goes whatever this says. Eliding is the user (or a
    /// [`Compactor`](crate::Compactor)) saying the turn is no longer to be read, and a marker
    /// that still cost every token the model had thought would make
    /// [`ContextState::Elided`](crate::ContextState::Elided) free nothing at all. The signature
    /// is the sharper half of it: a thinking block verified against the words it was produced
    /// with must not go out beside a marker that replaced them.
    pub send_reasoning: bool,
}

impl Default for LinearProjector {
    fn default() -> Self {
        Self {
            label_references: true,
            repair_orphans: true,
            send_reasoning: true,
            send_blocks: false,
            elision: "elided by nachalnik",
        }
    }
}

impl Projector for LinearProjector {
    fn project(&self, items: &[Arc<ContextItem>]) -> Projection {
        let mut projection = Projection {
            messages: Vec::with_capacity(items.len()),
            included: Vec::with_capacity(items.len()),
            skipped: Vec::new(),
            repairs: Vec::new(),
            reordered: Vec::new(),
        };

        // three passes, and the middle one is the only place a decision about an item is made.
        // The counting is what that pass needs and cannot work out as it goes, since whether a
        // call has a result is a fact about the whole projection rather than about the item in
        // hand; the ordering is the wire's answer to the same kind of question.
        let (mut answers, mut calls) = pairings(items);
        let built = self.build(items, &mut answers, &mut calls, &mut projection);
        for (id, message) in in_wire_order(built, &mut projection.reordered) {
            projection.included.push(id);
            projection.messages.push(message);
        }

        projection
    }
}

impl LinearProjector {
    /// Every item as the message it projects to, in the order the context has them.
    ///
    /// note: the order the *request* has them is [`in_wire_order`], a pass of its own. What is
    /// decided here is everything about one item on its own - what it says, which role says it,
    /// and whether it goes at all.
    fn build(
        &self,
        items: &[Arc<ContextItem>],
        answers: &mut HashMap<ToolCallId, usize>,
        calls: &mut HashMap<ToolCallId, usize>,
        projection: &mut Projection,
    ) -> Vec<(ContextId, Message)> {
        let mut built: Vec<(ContextId, Message)> = Vec::with_capacity(items.len());

        for item in items {
            if !item.is_projected() {
                let reason = match &item.note {
                    Some(note) => format!("{}: {note}", item.state),
                    None => item.state.to_string(),
                };
                projection.skipped.push(Skipped {
                    id: item.id,
                    reason,
                });
                continue;
            }

            // an elided item goes in as a marker in place of what it says, and nothing else about
            // the message changes: same role, and a tool result still answers its call. That is
            // how it differs from excluding it - the turn keeps its shape, so the repair below
            // never has to take the call down and rewrite history into one where it was never
            // made. The words are the item's own note; this only supplies the brackets
            let said = match item.state.is_elided() {
                true => Content::text(format!(
                    "[... {} ...]",
                    item.note.as_deref().unwrap_or(self.elision)
                )),
                false => item.content.clone(),
            };

            let message = match &item.kind {
                ContextKind::System => Message::new(Role::System, said),
                ContextKind::UserMessage => Message::new(Role::User, said),
                ContextKind::Reference => {
                    // the label goes on an elided reference too: which file is gone is most of
                    // what is worth knowing about it
                    let content = match (self.label_references, said.as_text()) {
                        (true, Some(text)) => Content::text(format!("{}:\n{text}", item.label)),
                        _ => said,
                    };
                    Message::new(Role::User, content)
                }
                ContextKind::AssistantMessage {
                    tool_calls,
                    reasoning,
                } => match self.turn(item, tool_calls, reasoning, &said, answers, projection) {
                    Some(message) => message,
                    None => continue,
                },
                ContextKind::ToolResult { call, tool, .. } => {
                    if self.repair_orphans && !claim(calls, call) {
                        // note: there are two ways to fail to claim a call and they do not read
                        // the same. One this projection does not carry is an orphan; one it
                        // carries whose answers are all spoken for is a *second* result for it -
                        // which is what restoring the whole of a truncated output beside the copy
                        // the model was shown produces, and it is the pairing working rather than
                        // anything going wrong. Saying the call was missing there sends whoever
                        // just did it looking for a call that is on their screen
                        let answered = calls.contains_key(call);
                        let why = match answered {
                            true => format!("the call `{call}` already has a result"),
                            false => format!("the call `{call}` is not in the projection"),
                        };
                        projection.repairs.push(format!(
                            "dropped item {} (a result of `{tool}`): {why}",
                            item.id
                        ));
                        projection.skipped.push(Skipped {
                            id: item.id,
                            reason: match answered {
                                true => "a second result for one call".into(),
                                false => "an orphaned tool result".into(),
                            },
                        });
                        continue;
                    }
                    Message::tool_result(call.clone(), tool.clone(), said)
                }
            };

            built.push((item.id, message));
        }

        built
    }

    /// One assistant turn as the message it projects to, or nothing where it projects to none.
    ///
    /// note: the turn is read as one ordered sequence whichever way it was recorded, so the
    /// repair, the elision and the skip rule are each written once and only the last step asks
    /// which of the two shapes it goes out in.
    fn turn(
        &self,
        item: &ContextItem,
        tool_calls: &[ToolCall],
        reasoning: &Option<Content>,
        said: &Content,
        answers: &mut HashMap<ToolCallId, usize>,
        projection: &mut Projection,
    ) -> Option<Message> {
        let elided = item.state.is_elided();

        // the turn as one ordered sequence, whichever way it was recorded. For a conventional one
        // that is the order every provider has been assuming anyway - what it thought, what it
        // said, what it asked for - so flattening it back below reproduces exactly what came in;
        // for one recorded as blocks it is the order the model actually produced
        let recorded: Vec<Block> = match item.content.as_blocks() {
            Some(blocks) => blocks.to_vec(),
            None => {
                let mut assembled = Vec::with_capacity(tool_calls.len() + 2);
                assembled.extend(reasoning.clone().map(Block::reasoning));
                // an empty text is no text at all - but an elided turn still gets its marker, which
                // is what elision leaves behind
                if elided || item.content.as_text() != Some("") {
                    assembled.push(Block::text(item.content.clone()));
                }
                assembled.extend(tool_calls.iter().cloned().map(Block::Call));

                assembled
            }
        };

        let mut kept: Vec<Block> = Vec::with_capacity(recorded.len());
        let mut marked = false;
        for block in recorded {
            match &block {
                Block::Call(call) => {
                    if !self.repair_orphans || claim(answers, &call.id) {
                        kept.push(block);
                    } else {
                        projection.repairs.push(format!(
                            "dropped the call `{}` ({}) from item {}: its result is not in the \
                             projection",
                            call.id, call.tool, item.id
                        ));
                    }
                }
                // an elided turn's thinking goes with its words. Keeping it would mean a turn whose
                // content is a one-line marker still costing every token it ever thought - so
                // eliding a turn would free nothing, and a compactor would watch the total refuse
                // to move and elide it again. Worse under `send_blocks`: a signed thinking block
                // would go out beside a marker that is not the words it was signed over
                Block::Reasoning(_) if elided || !self.send_reasoning => {}
                // an elided turn loses what it *said* and keeps everything else: the calls still
                // answer their results, so the turn keeps its shape
                Block::Text(_) if elided => {
                    if !marked {
                        kept.push(Block::text(said.clone()));
                        marked = true;
                    }
                }
                _ => kept.push(block),
            }
        }
        // a turn recorded as blocks need not have had any text to mark
        if elided && !marked {
            kept.insert(0, Block::text(said.clone()));
        }

        let calls: Vec<ToolCall> = kept.iter().filter_map(Block::call).cloned().collect();
        let spoke: Vec<&Part> = kept.iter().filter_map(Block::said).collect();

        if spoke.is_empty() && calls.is_empty() {
            // note: a turn that is *nothing but* reasoning goes too, because this projector speaks
            // the dialect in which an assistant message with no content is rejected. A provider
            // whose API keeps thinking-only turns - and some do - wants a projector of its own; the
            // reasoning is still in the context either way, which is why this says so out loud
            let reason = match kept.is_empty() {
                false => {
                    "an assistant turn with no content and no answered calls, so its reasoning \
                     goes with it"
                }
                true => "an assistant turn with no content and no answered calls",
            };
            projection.skipped.push(Skipped {
                id: item.id,
                reason: reason.into(),
            });
            return None;
        }

        // note: whichever shape it goes out in, the message is handed back rather than pushed
        // anywhere here, because the caller is where the repair above has finished taking calls
        // down - and the ordering pass reads the calls a message actually kept
        if self.send_blocks {
            Some(Message::assistant(
                Some(Content::Blocks(kept.into())),
                Vec::new(),
            ))
        } else {
            // flattening into the three slots, and saying so where it costs something
            if let Some(lost) = flattening_lost(item, &kept, spoke.len()) {
                projection.repairs.push(lost);
            }

            let content = join(spoke);
            let reasoning = join(kept.iter().filter_map(Block::thought).collect());

            Some(Message::assistant(content, calls).with_reasoning(reasoning))
        }
    }
}

/// What a turn recorded as ordered blocks loses by going out in three slots, where it loses
/// anything: two thinking blocks joined into one is a signature destroyed, and a sentence that
/// came after a call arrives before it.
///
/// note: asked of a turn that *was* recorded as blocks and of no other, because a conventional
/// one is being put back into the shape it arrived in - there is nothing to lose and nothing to
/// report. A turn with one of everything, in the order the slots are in, loses nothing either.
///
/// note: a signature on a text or a thinking part has nowhere to go in a conventional message,
/// and losing it is the most important thing reported here. An API rejects the next request over
/// it, and it went because this projector was asked for a shape that cannot hold it.
fn flattening_lost(item: &ContextItem, kept: &[Block], spoke: usize) -> Option<String> {
    // only a turn that was recorded as blocks has an order to lose
    item.content.as_blocks()?;

    let thoughts = kept.iter().filter(|b| b.thought().is_some()).count();
    let interleaved = kept
        .iter()
        .skip_while(|block| block.call().is_none())
        .any(|block| block.call().is_none());
    let signed = kept
        .iter()
        .filter_map(Block::part)
        .any(|part| !part.extra.is_null());
    if !(interleaved || spoke > 1 || thoughts > 1 || signed) {
        return None;
    }

    let also = match signed {
        true => ", and what the provider had attached to them",
        false => "",
    };

    Some(format!(
        "flattened item {} out of {} ordered block(s): this projector sends one content slot, one \
         reasoning slot and a list of calls, so their order is not carried{also}",
        item.id,
        kept.len(),
    ))
}

/// How many results are available to answer each call, and how many calls are available to be
/// answered by each result, among the items a projection will carry.
///
/// note: both are consumed as the messages are built, so a call and a result are paired one for
/// one. Counting rather than asking whether the identifier is present is what keeps that true of
/// a context in which one arrives twice - see [`LinearProjector::repair_orphans`].
fn pairings(
    items: &[Arc<ContextItem>],
) -> (HashMap<ToolCallId, usize>, HashMap<ToolCallId, usize>) {
    let mut answers: HashMap<ToolCallId, usize> = HashMap::new();
    let mut calls: HashMap<ToolCallId, usize> = HashMap::new();
    for item in items.iter().filter(|i| i.is_projected()) {
        match &item.kind {
            ContextKind::ToolResult { call, .. } => {
                *answers.entry(call.clone()).or_default() += 1;
            }
            ContextKind::AssistantMessage { .. } => {
                // `calls()` rather than the kind's own list: a turn recorded as ordered blocks
                // keeps its calls in its content, and pairing that found none there would repair
                // away every result it ever got
                for call in item.calls() {
                    *calls.entry(call.id.clone()).or_default() += 1;
                }
            }
            _ => {}
        }
    }

    (answers, calls)
}

/// Claims one of the remaining counterparts for a call identifier, if there is one left.
fn claim(remaining: &mut HashMap<ToolCallId, usize>, id: &ToolCallId) -> bool {
    match remaining.get_mut(id) {
        Some(left) if *left > 0 => {
            *left -= 1;
            true
        }
        _ => false,
    }
}

/// The messages in the order the request needs them, and a line for everything that had to move.
///
/// A result has to reach the wire immediately after the call it answers: it is what the
/// dialect specifies, and a strict endpoint refuses the whole request otherwise, naming
/// the `tool_call_id` that went unanswered. So each turn's results are gathered to it, and
/// everything else keeps the order the context had it in.
///
/// note: *a* strict endpoint, not every endpoint. Inception Labs' `mercury-2.5` accepts the
/// malformed order without complaint, so a test that only checks "the API accepted it" passes
/// on the broken order there. That is why the property in `tests/invariants.rs` asserts the
/// adjacency itself rather than trusting a provider to complain, and why the live test asserts
/// the *position* of the result and not merely that the request went through.
///
/// note: a pass of its own rather than bookkeeping inside the loop in `build`. Bookkeeping
/// there means a *count* of what the current turn is waiting for, reset on the next turn: a
/// result recorded after a later turn has nothing anchoring it and goes out several messages
/// away from its call, and any result can decrement the count, including one answering
/// somebody else. Reading the whole list at once costs one walk and gets neither wrong: a
/// result goes where its own call is, and a call is a place in a list rather than a number
/// that has to be kept.
fn in_wire_order(
    built: Vec<(ContextId, Message)>,
    reordered: &mut Vec<String>,
) -> Vec<(ContextId, Message)> {
    let mut results: HashMap<&ToolCallId, Vec<usize>> = HashMap::new();
    for (at, (_, message)) in built.iter().enumerate() {
        if let Some(answers) = &message.tool_call_id {
            results.entry(answers).or_default().push(at);
        }
    }
    // the calls this request actually makes, which is what decides whether a result has somebody
    // to wait for. `results` cannot answer that: it is built from every result's own identifier,
    // so it says yes for all of them and the branch below that keeps an unasked-for result where
    // it was would never run
    let asked: HashSet<&ToolCallId> = built
        .iter()
        .flat_map(|(_, message)| message.calls())
        .map(|call| &call.id)
        .collect();

    let mut placed = vec![false; built.len()];
    let mut order: Vec<usize> = Vec::with_capacity(built.len());
    for (at, (_, message)) in built.iter().enumerate() {
        // a result waits for the call it answers to place it - unless nothing in the request asks
        // for it, which `repair_orphans` normally takes care of and a caller can turn off; then it
        // keeps the place it had rather than being lost
        let deferred = message
            .tool_call_id
            .as_ref()
            .is_some_and(|answers| asked.contains(answers));
        if placed[at] || deferred {
            continue;
        }

        placed[at] = true;
        order.push(at);

        // `calls()` rather than the kind's own list: with `send_blocks` a turn keeps its calls in
        // its content, and the repair above may have taken some down
        let mut adjacent = at + 1;
        for call in message.calls() {
            // one result per call, and the next unplaced one, because that is the pairing `build`
            // made: calls and results are claimed one for one, in order, rather than by set
            // membership. Taking every result that shares the identifier would undo that for two
            // calls carrying one identifier - the case `repair_orphans` counts for - putting both
            // answers behind the first call and sending the second with nothing after it, and
            // saying nothing, because nothing was dropped
            let answer = results
                .get(&call.id)
                .into_iter()
                .flatten()
                .copied()
                .find(|answer| !placed[*answer]);
            let Some(answer) = answer else {
                continue;
            };

            if answer != adjacent {
                reordered.push(format!(
                    "moved item {} up behind the call `{}` it answers: a tool result has to \
                     reach the wire immediately after the call it answers",
                    built[answer].0, call.id
                ));
            }
            placed[answer] = true;
            order.push(answer);
            adjacent += 1;
        }
    }
    // nothing is dropped to achieve an order. A result whose call is in the request is placed by it
    // above; one that got here another way keeps its place at the end rather than going missing,
    // which is the guarantee `Projection::included` is read for
    order.extend((0..built.len()).filter(|at| !placed[*at]));

    // taken rather than cloned: a projection is built for every budget and every preview, and an
    // order is a permutation - each message is wanted exactly once, somewhere else
    let mut built: Vec<Option<(ContextId, Message)>> = built.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|at| built[at].take().expect("an order places each message once"))
        .collect()
}
