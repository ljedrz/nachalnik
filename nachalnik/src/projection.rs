//! Turns context items into the messages of a request.
//!
//! note: the trait is one method because a real API may disagree with the default shape, and a
//! projector that disagrees replaces the whole shape rather than overriding a piece of it. The
//! default makes three private passes: match calls to results, build a message per item, and put
//! them in the order the wire needs. [`Projection`] is what is about to be sent, available before
//! anything is; [`Skipped`] lists the items left out of it and why.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::Kernel;
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

/// The messages a context projects to, plus how they came about.
///
/// note: available before anything is sent, via [`Kernel::preview_request`] and
/// [`Kernel::project`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    /// The messages, in the order they will be sent.
    pub messages: Vec<Message>,
    /// The items that contributed, in order.
    pub included: Vec<ContextId>,
    /// The items that did not, and why.
    pub skipped: Vec<Skipped>,
    /// What the projector took out to keep the request valid: a call whose result is missing, a
    /// result whose call is missing, an ordered turn a flat shape cannot carry.
    ///
    /// note: each is content the model would have had and will not, so they are put in front of
    /// somebody rather than left on a page they would have to open. See [`Projection::reordered`]
    /// for the adjustments that are not.
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
    /// Projects all items, in insertion order whatever their state, into the messages of a request.
    ///
    /// note: called with the context locked, so it must not call back into the kernel it is
    /// installed in; the items it needs are the ones it is handed.
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

/// Puts several blocks into the one slot a conventional message has.
///
/// note: one block is carried through untouched: a [`Content::Json`] thinking block is how a signed
/// or encrypted one travels, and a provider that received it as a string of its own serialization
/// could not send it back. Several blocks can only be joined as text, which is why
/// [`LinearProjector::send_blocks`] exists and why joining is reported.
///
/// note: [`Part::extra`] cannot survive either way: a conventional message has a [`Content`] in
/// each slot and nowhere to put what a provider attached to it. The caller reports it instead,
/// because a repair reads better naming the item it happened to.
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
/// note: This projector speaks the dialect where a tool result is its own message and an assistant
/// message must carry content. A provider whose format disagrees - tool results as blocks in a user
/// turn, thinking-only turns that must be echoed - wants its own projector, which is why this is a
/// trait.
#[derive(Debug, Clone, Copy)]
pub struct LinearProjector {
    /// Whether a reference's label is prepended to its text, as `label:\n<content>`.
    ///
    /// note: With this off, the model receives a file's contents without being told which file it
    /// is - rarely what anyone wants. It is a prompt decision, so it is visible and optional rather
    /// than baked in.
    pub label_references: bool,
    /// Whether tool calls and tool results whose counterpart is not in the projection are dropped
    /// from it.
    ///
    /// note: This makes excluding safe. Most providers reject a request where an assistant turn
    /// asks for a tool with no result (or vice versa), so dropping one half of a pair would cause a
    /// request error. Every drop is listed in [`Projection::repairs`].
    ///
    /// note: Calls are paired to results one for one, in order, not by set membership. Call
    /// identifiers are meant to be unique - the kernel enforces it for everything a provider
    /// produces (see [`Event::ToolCallRepaired`](crate::Event::ToolCallRepaired)) - but a context
    /// assembled by hand or restored from elsewhere can repeat one. Counting keeps calls and
    /// results equal even then.
    pub repair_orphans: bool,
    /// What a [`ContextState::Elided`](crate::ContextState::Elided) item is shown as when it
    /// carries no note of its own.
    ///
    /// note: The words are the item's `note` where it has one: whoever elided it knows why, and
    /// this projector does not. All this supplies is the brackets around it and this fallback. It
    /// reads like [`Content::truncate_to`](crate::Content::truncate_to)'s marker on purpose - the
    /// same promise to the model at two different limits, so a model that has learnt to read one
    /// reads the other.
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
    /// note: on by default. A provider that does not want the field ignores it. A provider that
    /// needs it, such as a reasoning model whose API verifies a signed thinking block against the
    /// turn it belongs to, cannot invent it. Turn it off to keep reasoning in the record without
    /// sending it on every subsequent request.
    ///
    /// note: an elided turn's thinking is dropped whatever this says. Eliding means the user or a
    /// [`Compactor`](crate::Compactor) says the turn is no longer to be read. A marker that still
    /// carried every token the model had thought would make
    /// [`ContextState::Elided`](crate::ContextState::Elided) free nothing. The signature gives a
    /// stricter rule: a thinking block verified against the words it was produced with must not be
    /// sent beside a marker that replaced them.
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

        // three passes. The middle pass is the only place a decision about an item is made.
        // Counting gives that pass what it cannot work out as it goes: whether a call has a result
        // is a fact about the whole projection, not the item in hand. Ordering answers the same
        // kind of question for the wire.
        let (mut answers, mut calls) = pairings(items);
        let built = self.build(items, &mut answers, &mut calls, &mut projection);
        for (id, message) in in_wire_order(built, &mut projection.reordered) {
            projection.included.push(id);
            projection.messages.push(message);
        }

        projection
    }

    /// The type's path and the two switches a provider sets, as
    /// `nachalnik::projection::LinearProjector (blocks, no reasoning)`.
    ///
    /// note: the switches are included because the type alone cannot tell a change from no change.
    /// Every provider ships one of these, and the kernel starts with the default - so a session
    /// announced `LinearProjector → LinearProjector` while the shape of every assistant turn on the
    /// wire changed. The other fields stay out: nothing sets them, and a name listing them all
    /// would be a debug print.
    fn name(&self) -> &'static str {
        match (self.send_blocks, self.send_reasoning) {
            (false, true) => concat!(module_path!(), "::LinearProjector"),
            (true, true) => concat!(module_path!(), "::LinearProjector (blocks)"),
            (false, false) => concat!(module_path!(), "::LinearProjector (no reasoning)"),
            (true, false) => concat!(module_path!(), "::LinearProjector (blocks, no reasoning)"),
        }
    }
}

impl LinearProjector {
    /// Every item as the message it projects to, in the order the context has them.
    ///
    /// note: the request order is decided by [`in_wire_order`], a separate pass. This function
    /// decides everything about one item on its own: what it says, which role says it, and whether
    /// it is included.
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
            // the message changes: same role, and a tool result still answers its call. It differs
            // from excluding it: the turn keeps its shape, so the repair never has to take the call
            // down and rewrite history as if the call was never made. The words are the item's own
            // note; this only supplies the brackets
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
                        // note: the two ways to fail to claim a call need different messages. A
                        // call this projection does not carry is an orphan. A call it carries that
                        // already has a result is a second result for it: restoring the whole of a
                        // truncated output beside the copy the model was shown produces that second
                        // result, and the pairing is working as intended, not failing. The
                        // missing-call message would send whoever just produced that second result
                        // looking for a call that is on their screen
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

    /// The message one assistant turn projects to, or nothing when it projects to none.
    ///
    /// note: the turn is treated as one ordered sequence however it was recorded, so the repair,
    /// the elision, and the skip rule are each written once. Only the last step chooses which of
    /// the two shapes it goes out in.
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

        // the turn as one ordered sequence, whichever way it was recorded. For a conventional turn,
        // that is the order every provider has been assuming - what it thought, what it said, what
        // it asked for - so flattening it back reproduces exactly what came in. For one recorded as
        // blocks, it is the order the model actually produced
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
                // content is a one-line marker still costing every token it ever thought, so
                // eliding a turn would free nothing and a compactor would watch the total refuse to
                // move and elide it again. Worse under `send_blocks`: a signed thinking block would
                // go out beside a marker that is not the words it was signed over
                Block::Reasoning(_) if elided || !self.send_reasoning => {}
                // an elided turn loses its text and keeps everything else: the calls still answer
                // their results, so the turn keeps its shape
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

        // note: in either output shape, the message is returned rather than pushed anywhere here,
        // because the caller is where the repair above has finished taking calls down. The
        // ordering pass reads the calls a message actually kept
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

/// What a turn recorded as ordered blocks loses by going out in three slots, if anything: two
/// thinking blocks joined into one destroys a signature, a sentence after a call arrives before it,
/// and thinking after a sentence arrives before that.
///
/// note: only for a turn recorded as blocks. A conventional turn is put back into the shape it
/// arrived in, so nothing is lost or reported. A turn with one of everything, in slot order, loses
/// nothing.
///
/// note: a signature on text or thinking has nowhere to go in a conventional message, and losing it
/// is the most important thing reported. An API rejects the next request over it, and it was lost
/// because this projector was asked for a shape that cannot hold it.
fn flattening_lost(item: &ContextItem, kept: &[Block], spoke: usize) -> Option<String> {
    // only a turn that was recorded as blocks has an order to lose
    item.content.as_blocks()?;

    let thoughts = kept.iter().filter(|b| b.thought().is_some()).count();
    let interleaved = kept
        .iter()
        .skip_while(|block| block.call().is_none())
        .any(|block| block.call().is_none());
    // the reasoning slot is read first, so thinking recorded after a sentence is moved ahead of it
    let rethought = kept
        .iter()
        .skip_while(|block| block.said().is_none())
        .any(|block| block.thought().is_some());
    let signed = kept
        .iter()
        .filter_map(Block::part)
        .any(|part| !part.extra.is_null());
    if !(interleaved || rethought || spoke > 1 || thoughts > 1 || signed) {
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

/// How many results can answer each call, and how many calls each result can answer, among the
/// items a projection carries.
///
/// note: both are consumed as messages are built, so a call and result pair one for one. Counting
/// identifiers, rather than checking presence, keeps this true when one arrives twice - see
/// [`LinearProjector::repair_orphans`].
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
                // `calls()` rather than the kind's list: a turn recorded as ordered blocks keeps
                // its calls in its content. Pairing that found none there would repair away every
                // result it ever got
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
/// A result must reach the wire immediately after the call it answers: the dialect specifies it,
/// and a strict endpoint refuses the whole request, naming the unanswered `tool_call_id`. Each
/// turn's results are gathered to it; everything else keeps the order the context had.
///
/// note: a strict endpoint, not every endpoint. Inception Labs' `mercury-2.5` accepts the malformed
/// order without complaint, so a test that only checks acceptance passes on the broken order there.
/// The property in `tests/invariants.rs` asserts adjacency itself rather than trusting a provider
/// to complain, and the live test asserts the result's position, not merely that the request went
/// through.
///
/// note: a separate pass rather than bookkeeping in the `build` loop. Bookkeeping there means a
/// count of what the current turn is waiting for, reset on the next turn: a result recorded after a
/// later turn has nothing anchoring it and goes out several messages away from its call, and any
/// result can decrement the count, including one answering somebody else. Reading the whole list at
/// once costs one walk and gets neither wrong: a result goes where its own call is, and a call is a
/// place in a list rather than a number to keep.
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
    // the calls this request actually makes, which decides whether a result has somebody to wait
    // for. `results` cannot answer that: it is built from every result's identifier, so it says yes
    // for all of them, and the branch that keeps an unasked-for result where it was would never run
    let asked: HashSet<&ToolCallId> = built
        .iter()
        .flat_map(|(_, message)| message.calls())
        .map(|call| &call.id)
        .collect();

    let mut placed = vec![false; built.len()];
    let mut order: Vec<usize> = Vec::with_capacity(built.len());
    for (at, (_, message)) in built.iter().enumerate() {
        // a result waits for the call it answers to place it, unless nothing in the request asks
        // for it. `repair_orphans` normally handles that, and a caller can turn it off; then the
        // result keeps its place rather than being lost
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
            // Use one result per call, the next unplaced one. `build` claims calls and results one
            // for one, in order, not by set membership. Taking every result that shares an
            // identifier would break that pairing for two calls carrying one identifier, the case
            // `repair_orphans` counts for. Both answers would go behind the first call, the second
            // would be sent with nothing after it, and there would be no report because nothing was
            // dropped
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
    // nothing is dropped to achieve an order. A result whose call is in the request is placed by
    // that call above; one that got here another way keeps its place at the end instead of going
    // missing. `Projection::included` is read for that guarantee
    order.extend((0..built.len()).filter(|at| !placed[*at]));

    // taken rather than cloned: a projection is built for every budget and every preview, and an
    // order is a permutation - each message is wanted exactly once, somewhere else
    let mut built: Vec<Option<(ContextId, Message)>> = built.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|at| built[at].take().expect("an order places each message once"))
        .collect()
}
