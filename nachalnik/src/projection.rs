use std::{collections::HashMap, sync::Arc};

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
    /// Adjustments the projector made to keep the request valid, in plain words.
    pub repairs: Vec<String>,
}

/// Turns context items into the messages of a request.
///
/// note: This is where the shape of a request lives, and it is replaceable. The kernel has no
/// opinion about whether a file belongs in a user message, a system message or a preamble; it
/// asks the projector, and shows you the result.
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
///         }
///     }
/// }
/// ```
///
/// note: What a projector decides is which items become which messages - and, for an assistant
/// turn, which of the two shapes it goes out in. A turn can be a content slot, a reasoning slot
/// and a flat list of calls, or it can be an ordered sequence of [`Block`]s where thinking, text
/// and calls interleave and the order is part of the message;
/// [`LinearProjector::send_blocks`] picks. What no projector can do is recover an order that was
/// never recorded, which is why this is a property of [`Content`] rather than of the projection:
/// a turn keeps its order from the wire, through the context, to the next request.
pub trait Projector: Send + Sync {
    /// Projects the items - all of them, in insertion order, whatever their state - into the
    /// messages of a request.
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
/// note: one of them is carried through untouched, which matters more than it looks: a
/// [`Content::Json`] thinking block is how a signed or encrypted one travels, and a provider that
/// received it as a string of its own serialization could not send it back. Several can only be
/// joined as text, which is why [`LinearProjector::send_blocks`] exists and why joining is
/// reported.
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
/// note: The three flags below are the only judgement calls this projector makes, and every one
/// of them can be turned off.
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
    /// *after* a call - it says so in [`Projection::repairs`] rather than doing it quietly. That
    /// is the honest version of what a provider used to have to do for itself, and the reason
    /// this flag exists is that for a signed thinking block it is not good enough.
    pub send_blocks: bool,
    /// Whether an assistant turn's reasoning is carried back into
    /// [`Message::reasoning`](crate::Message::reasoning).
    ///
    /// note: On by default, because a provider that does not want it simply ignores the field,
    /// whereas a provider that needs it - a reasoning model whose API verifies a signed thinking
    /// block against the turn it belongs to - cannot invent it. Turn it off to keep reasoning in
    /// the record without spending it on every subsequent request.
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
        };

        // how many results are available to answer each call, and how many calls are available
        // to be answered by each result, among the projected items; both are consumed as the
        // messages are built, so a call and a result are paired one for one
        let mut answers: HashMap<ToolCallId, usize> = HashMap::new();
        let mut calls: HashMap<ToolCallId, usize> = HashMap::new();
        for item in items.iter().filter(|i| i.is_projected()) {
            match &item.kind {
                ContextKind::ToolResult { call, .. } => {
                    *answers.entry(call.clone()).or_default() += 1;
                }
                ContextKind::AssistantMessage { .. } => {
                    // `calls()` rather than the kind's own list: a turn recorded as ordered
                    // blocks keeps its calls in its content, and pairing that found none there
                    // would repair away every result it ever got
                    for call in item.calls() {
                        *calls.entry(call.id.clone()).or_default() += 1;
                    }
                }
                _ => {}
            }
        }

        /// Puts back whatever was held out of a turn, in the order it arrived.
        fn flush(projection: &mut Projection, held: &mut Vec<(ContextId, Message)>) {
            for (id, message) in held.drain(..) {
                projection.included.push(id);
                projection.messages.push(message);
            }
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

        // what arrived in the middle of a turn, and how many of that turn's calls are still
        // waiting to be answered
        let mut held: Vec<(ContextId, Message)> = Vec::new();
        let mut outstanding = 0usize;

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
            // the whole difference from excluding it - the turn keeps its shape, so the repair
            // below never has to take the call down and rewrite history into one where it was
            // never made. The words are the item's own note; this only supplies the brackets
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
                } => {
                    let elided = item.state.is_elided();

                    // the turn as one ordered sequence, whichever way it was recorded. For a
                    // conventional one that is the order every provider has been assuming
                    // anyway - what it thought, what it said, what it asked for - so flattening
                    // it back below reproduces exactly what came in; for one recorded as blocks
                    // it is the order the model actually produced, which is the whole point.
                    // Doing it in two steps rather than four arms is what keeps the repair, the
                    // elision and the skip rule from being written twice
                    let recorded: Vec<Block> = match item.content.as_blocks() {
                        Some(blocks) => blocks.to_vec(),
                        None => {
                            let mut assembled = Vec::with_capacity(tool_calls.len() + 2);
                            assembled.extend(reasoning.clone().map(Block::reasoning));
                            // an empty text is no text at all - but an elided turn still gets
                            // its marker, which is the whole of what elision leaves behind
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
                                if !self.repair_orphans || claim(&mut answers, &call.id) {
                                    kept.push(block);
                                } else {
                                    projection.repairs.push(format!(
                                        "dropped the call `{}` ({}) from item {}: its result is not in the projection",
                                        call.id, call.tool, item.id
                                    ));
                                }
                            }
                            Block::Reasoning(_) if !self.send_reasoning => {}
                            // an elided turn loses what it *said* and keeps everything else: the
                            // calls still answer their results, so the turn keeps its shape
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

                    let calls: Vec<ToolCall> =
                        kept.iter().filter_map(Block::call).cloned().collect();
                    let spoke: Vec<&Part> = kept.iter().filter_map(Block::said).collect();

                    if spoke.is_empty() && calls.is_empty() {
                        // note: a turn that is *nothing but* reasoning goes too, because this
                        // projector speaks the dialect in which an assistant message with no
                        // content is rejected. A provider whose API keeps thinking-only turns -
                        // and some do - wants a projector of its own; the reasoning is still in
                        // the context either way, which is why this says so out loud
                        let reason = match kept.is_empty() {
                            false => {
                                "an assistant turn with no content and no answered calls, so its reasoning goes with it"
                            }
                            true => "an assistant turn with no content and no answered calls",
                        };
                        projection.skipped.push(Skipped {
                            id: item.id,
                            reason: reason.into(),
                        });
                        continue;
                    }

                    if self.send_blocks {
                        projection.included.push(item.id);
                        projection.messages.push(Message::assistant(
                            Some(Content::Blocks(kept.into())),
                            Vec::new(),
                        ));
                        continue;
                    }

                    // flattening into the three slots, and saying so where it costs something:
                    // two thinking blocks joined into one is a signature destroyed, and a
                    // sentence that came after a call arrives before it
                    if item.content.as_blocks().is_some() {
                        let thoughts = kept.iter().filter(|b| b.thought().is_some()).count();
                        let interleaved = kept
                            .iter()
                            .skip_while(|block| block.call().is_none())
                            .any(|block| block.call().is_none());
                        // a signature on a text or a thinking part has nowhere to go in a
                        // conventional message, and going missing is the thing it is most
                        // important to say out loud: it is what an API rejects the next request
                        // over, and the reason it went is that this projector was asked for a
                        // shape that cannot hold it
                        let signed = kept
                            .iter()
                            .filter_map(Block::part)
                            .any(|part| !part.extra.is_null());
                        if interleaved || spoke.len() > 1 || thoughts > 1 || signed {
                            let also = match signed {
                                true => ", and what the provider had attached to them",
                                false => "",
                            };
                            projection.repairs.push(format!(
                                "flattened item {} out of {} ordered block(s): this projector \
                                 sends one content slot, one reasoning slot and a list of calls, \
                                 so their order is not carried{also}",
                                item.id,
                                kept.len(),
                            ));
                        }
                    }

                    let content = join(spoke);
                    let reasoning = join(kept.iter().filter_map(Block::thought).collect());

                    Message::assistant(content, calls).with_reasoning(reasoning)
                }
                ContextKind::ToolResult { call, tool, .. } => {
                    if self.repair_orphans && !claim(&mut calls, call) {
                        projection.repairs.push(format!(
                            "dropped item {} (a result of `{tool}`): the call `{call}` is not in the projection",
                            item.id
                        ));
                        projection.skipped.push(Skipped {
                            id: item.id,
                            reason: "an orphaned tool result".into(),
                        });
                        continue;
                    }
                    Message::tool_result(call.clone(), tool.clone(), said)
                }
            };

            // a result has to reach the wire immediately after the call it answers: an
            // OpenAI-compatible API refuses the whole request otherwise, naming the
            // `tool_call_id` that went unanswered, and a tool that writes into the context - a
            // note the model asks for while the rest of its calls are still running - lands
            // exactly there. So anything that is not a result waits until the turn has had them
            match &item.kind {
                ContextKind::ToolResult { .. } => outstanding = outstanding.saturating_sub(1),
                ContextKind::AssistantMessage { .. } => {
                    // a new turn ends the last one, whatever is still unanswered in it
                    flush(&mut projection, &mut held);
                    // `calls()` rather than the kind's own list: with `send_blocks` a turn keeps
                    // its calls in its content, and the repair above may have taken some down
                    outstanding = message.calls().count();
                }
                _ if outstanding > 0 => {
                    projection.repairs.push(format!(
                        "held item {} back until the turn it landed in had its results: a tool \
                         result has to follow the call it answers",
                        item.id
                    ));
                    held.push((item.id, message));
                    continue;
                }
                _ => {}
            }

            projection.included.push(item.id);
            projection.messages.push(message);
            if outstanding == 0 {
                flush(&mut projection, &mut held);
            }
        }
        // a turn whose results never arrived still must not swallow what came after it
        flush(&mut projection, &mut held);

        projection
    }
}
