use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::{Context, Kernel};
use crate::{
    context::{ContextId, ContextItem, ContextKind},
    model::{Content, Message, Role, ToolCallId},
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
pub trait Projector: Send + Sync {
    /// Projects the items - all of them, in insertion order, whatever their state - into the
    /// messages of a request.
    fn project(&self, items: &[Arc<ContextItem>]) -> Projection;
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
                ContextKind::AssistantMessage { tool_calls, .. } => {
                    for call in tool_calls {
                        *calls.entry(call.id.clone()).or_default() += 1;
                    }
                }
                _ => {}
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

            let message = match &item.kind {
                ContextKind::System => Message::new(Role::System, item.content.clone()),
                ContextKind::UserMessage => Message::new(Role::User, item.content.clone()),
                ContextKind::Reference => {
                    let content = match (self.label_references, item.content.as_text()) {
                        (true, Some(text)) => Content::text(format!("{}:\n{text}", item.label)),
                        _ => item.content.clone(),
                    };
                    Message::new(Role::User, content)
                }
                ContextKind::AssistantMessage {
                    tool_calls,
                    reasoning,
                } => {
                    let mut kept = Vec::with_capacity(tool_calls.len());
                    for call in tool_calls {
                        if !self.repair_orphans || claim(&mut answers, &call.id) {
                            kept.push(call.clone());
                        } else {
                            projection.repairs.push(format!(
                                "dropped the call `{}` ({}) from item {}: its result is not in the projection",
                                call.id, call.tool, item.id
                            ));
                        }
                    }

                    let content = match item.content.as_text() {
                        Some("") => None,
                        _ => Some(item.content.clone()),
                    };

                    if content.is_none() && kept.is_empty() {
                        // note: a turn that is *nothing but* reasoning goes too, because this
                        // projector speaks the dialect in which an assistant message with no
                        // content is rejected. A provider whose API keeps thinking-only turns -
                        // and some do - wants a projector of its own; the reasoning is still in
                        // the context either way, which is why this says so out loud
                        let reason = match reasoning {
                            Some(_) => {
                                "an assistant turn with no content and no answered calls, so its reasoning goes with it"
                            }
                            None => "an assistant turn with no content and no answered calls",
                        };
                        projection.skipped.push(Skipped {
                            id: item.id,
                            reason: reason.into(),
                        });
                        continue;
                    }

                    let reasoning = self.send_reasoning.then(|| reasoning.clone()).flatten();

                    Message::assistant(content, kept).with_reasoning(reasoning)
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
                    Message::tool_result(call.clone(), tool.clone(), item.content.clone())
                }
            };

            projection.included.push(item.id);
            projection.messages.push(message);
        }

        projection
    }
}
