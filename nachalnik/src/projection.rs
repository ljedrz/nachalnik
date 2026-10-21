//! Turns context items into the messages of a request.
//!
//! note: the trait is one method, so a projector that disagrees with the default replaces the whole
//! shape rather than overriding a piece of it. The default's three passes (pair calls with results,
//! build a message per item, order them for the wire) are private. [`Projection`] is what will be
//! sent, available before anything is; [`Skipped`] lists what was left out and why.

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
    /// What the projector took out to keep the request valid: a call without its result, a result
    /// without its call, an ordered turn a flat shape cannot carry.
    ///
    /// note: each is content the model will not see, so it is reported here rather than only in a
    /// log. Moves that lose nothing are in [`Projection::reordered`].
    pub repairs: Vec<String>,
    /// What the projector moved, in plain words; a move loses nothing.
    ///
    /// note: separate from [`Projection::repairs`] because a move is not a fault. A tool result
    /// must follow its call on the wire, so an item written between them (as `kamchatka`'s
    /// `context: note` is, on every call) makes the projector put the result back after the call. A
    /// request preview should show that; a repair line, which stays in the conversation for the
    /// rest of the session, should not.
    pub reordered: Vec<String>,
}

/// Turns context items into the messages of a request.
///
/// note: the kernel has no opinion about whether content goes in a user message, a system message
/// or a preamble; the projector decides.
///
/// [`LinearProjector`] speaks the dialect in which a tool result is a message of its own. An API
/// that wants tool results as blocks in a user turn, keeps thinking-only turns, or takes a single
/// string needs its own projector, which is one method:
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
/// note: a projector also decides whether an assistant turn goes out as three slots (content,
/// reasoning, calls) or as an ordered sequence of [`Block`]s; see [`LinearProjector::send_blocks`].
/// No projector can recover an order that was never recorded, which is why the order belongs to
/// [`Content`] rather than to the projection.
pub trait Projector: Send + Sync {
    /// Projects all items, in insertion order whatever their state, into the messages of a request.
    ///
    /// note: called with the context locked, so it must not call back into the kernel it is
    /// installed in; the items it needs are the ones it is handed.
    fn project(&self, items: &[Arc<ContextItem>]) -> Projection;

    /// Name of this projector, for clients that display it.
    ///
    /// note: The default is the implementing type's path. Override to be friendlier. It is for
    /// showing a person, not for matching: `type_name` makes no stability promise.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// Puts several blocks into the one slot a conventional message has.
///
/// note: a single block passes through untouched, because a [`Content::Json`] thinking block is how
/// a signed or encrypted one travels and a provider handed its own serialization as a string could
/// not send it back. Several blocks can only be joined as text; joining is reported, and
/// [`LinearProjector::send_blocks`] avoids it.
///
/// note: [`Part::extra`] is lost either way, since a conventional message has nowhere to put it.
/// The caller reports that, so the repair names the item.
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
/// note: the flags below are its only judgement calls, and each can be turned off. It speaks the
/// dialect in which a tool result is its own message and an assistant message must carry content; a
/// provider whose format differs needs its own projector.
#[derive(Debug, Clone, Copy)]
pub struct LinearProjector {
    /// Whether a reference's label is prepended to its text, as `label:\n<content>`.
    ///
    /// note: Without this, the model sees content without knowing which file it's from. This is
    /// a prompt decision, kept visible and optional.
    pub label_references: bool,
    /// Whether tool calls and tool results whose counterpart is not in the projection are dropped
    /// from it.
    ///
    /// note: this is what makes excluding safe: most providers reject a call without its result or
    /// a result without its call. Every drop is listed in [`Projection::repairs`].
    ///
    /// note: pairing is one for one, in order, by count rather than by set membership, so it stays
    /// right when a context assembled by hand or restored from elsewhere repeats a call identifier.
    /// The kernel never lets a provider repeat one; see
    /// [`Event::ToolCallRepaired`](crate::Event::ToolCallRepaired).
    pub repair_orphans: bool,
    /// What a [`ContextState::Elided`](crate::ContextState::Elided) item is shown as when it
    /// carries no note of its own.
    ///
    /// note: the item's own `note` is used where it has one, since whoever elided it knows why;
    /// this supplies the brackets and the fallback. It matches
    /// [`Content::truncate_to`](crate::Content::truncate_to)'s marker on purpose, so a model that
    /// has learnt one reads the other.
    pub elision: &'static str,
    /// Whether an assistant turn is projected as an ordered sequence of [`Block`](crate::Block)s
    /// rather than as a content slot, a reasoning slot and a list of calls.
    ///
    /// note: off by default, because every provider built against this projector reads the three
    /// slots. Turn it on for an API whose assistant turn is an ordered list of typed blocks. It
    /// then applies to every assistant turn: a conventional one is assembled as thinking, text,
    /// calls, so a mixed context projects to one shape.
    ///
    /// note: with it off, a turn recorded as blocks is flattened, and whatever that loses (thinking
    /// blocks joined, a sentence that came after a call) is reported in [`Projection::repairs`].
    /// For a signed thinking block, reporting the loss is not enough; that is what this flag is
    /// for.
    pub send_blocks: bool,
    /// Whether an assistant turn's reasoning is carried back into
    /// [`Message::reasoning`](crate::Message::reasoning).
    ///
    /// note: on by default. A provider that does not want the field ignores it; one whose API
    /// verifies a signed thinking block against its turn cannot invent it. Turn it off to keep
    /// reasoning in the record without resending it on every request.
    ///
    /// note: an elided turn's thinking is dropped regardless. Otherwise eliding a turn would free
    /// none of its thinking tokens, and a signed thinking block would go out beside a marker that
    /// replaced the words it was signed over.
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

        // three passes, and only the middle one decides anything about an item. Counting first
        // tells it whether a call has a result, which depends on the whole projection rather than
        // the item in hand; ordering last does the same for the wire
        let (mut answers, mut calls) = pairings(items);
        let built = self.build(items, &mut answers, &mut calls, &mut projection);
        for (id, message) in in_wire_order(built, &mut projection.reordered) {
            projection.included.push(id);
            projection.messages.push(message);
        }

        projection
    }

    /// The type's path and the two switches a provider sets, as
    /// `nachalnik::projection::LinearProjector (blocks, reasoning not sent back)`.
    ///
    /// note: the switches are included because the type alone cannot tell a change from no change:
    /// every shipped provider installs a `LinearProjector`, so a session once announced
    /// `LinearProjector → LinearProjector` while the wire shape of every assistant turn changed.
    /// Nothing sets the other fields.
    fn name(&self) -> &'static str {
        match (self.send_blocks, self.send_reasoning) {
            (false, true) => concat!(module_path!(), "::LinearProjector"),
            (true, true) => concat!(module_path!(), "::LinearProjector (blocks)"),
            (false, false) => concat!(module_path!(), "::LinearProjector (reasoning not sent back)"),
            (true, false) => concat!(module_path!(), "::LinearProjector (blocks, reasoning not sent back)"),
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

            // an elided item goes in as a marker in place of its content, with the same role, and a
            // tool result still answers its call. Unlike exclusion, the turn keeps its shape, so no
            // call has to be repaired away. The words are the item's own note; this supplies the
            // brackets
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
                        // note: two failures, two messages. A call this projection does not carry
                        // is an orphan. A call that already has a result is getting a second one,
                        // which is what restoring the whole of a truncated output beside the
                        // model's copy does, and is working as intended; the orphan's message would
                        // send its author looking for a call that is on their screen
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

        // the turn as one ordered sequence, however it was recorded. For a conventional turn that
        // is the order every provider assumes (thinking, text, calls), so flattening it back
        // reproduces what came in; for one recorded as blocks it is the order the model produced
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
                // an elided turn's thinking goes with its words. Kept, it would make eliding free
                // nothing (a compactor would see no change and elide again), and under
                // `send_blocks` a signed thinking block would go out beside a marker that is not
                // the words it was signed over
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
            // note: a turn that is only reasoning is dropped too, because this dialect rejects an
            // assistant message with no content; a provider whose API keeps such turns needs its
            // own projector. The reason says so because the reasoning is still in the context
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
/// note: only for a turn recorded as blocks; a conventional turn, or one with one of everything in
/// slot order, loses nothing.
///
/// note: a lost signature is the most important thing reported: it has nowhere to go in a
/// conventional message, and the API rejects the next request over it.
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
                // `calls()` finds calls in content for ordered blocks. Using the kind's list would
                // miss them, repairing away their results
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
/// note: only a strict endpoint refuses it; Inception Labs' `mercury-2.5` accepts the wrong order.
/// So `tests/invariants.rs` asserts adjacency itself, and the live test asserts the result's
/// position rather than only that the request went through.
///
/// note: a separate pass, because counting in the `build` loop what the current turn waits for goes
/// wrong twice: a result recorded after a later turn has nothing anchoring it, and any result can
/// decrement the count, including one answering another call. Reading the whole list puts each
/// result where its own call is.
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
    // the calls this request actually makes, which decide whether a result has a call to wait for.
    // `results` cannot answer that: it holds every result's identifier, so it would say yes for all
    // of them and the branch that leaves an unasked-for result in place would never run
    let asked: HashSet<&ToolCallId> = built
        .iter()
        .flat_map(|(_, message)| message.calls())
        .map(|call| &call.id)
        .collect();

    let mut placed = vec![false; built.len()];
    let mut order: Vec<usize> = Vec::with_capacity(built.len());
    for (at, (_, message)) in built.iter().enumerate() {
        // a result waits for its call to place it, unless nothing in the request asks for it.
        // `repair_orphans` normally drops such a result; with it turned off, the result keeps its
        // place rather than being lost
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
            // one result per call, the next unplaced one, as `build` pairs them one for one in
            // order. Taking every result with the call's identifier would, for two calls sharing
            // one (the case `repair_orphans` counts for), put both answers behind the first call
            // and send the second with nothing after it, unreported because nothing was dropped
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
    // nothing is dropped to achieve an order. A result whose call is in the request was placed by
    // that call above; one that got here another way keeps its place at the end, which
    // `Projection::included` relies on
    order.extend((0..built.len()).filter(|at| !placed[*at]));

    // taken rather than cloned: a projection is built for every budget and every preview, and an
    // order is a permutation - each message is wanted exactly once, somewhere else
    let mut built: Vec<Option<(ContextId, Message)>> = built.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|at| built[at].take().expect("an order places each message once"))
        .collect()
}
