//! Building a request, sending it, and making sense of what came back.
//!
//! note: the private half of [`super::Kernel`]'s request path, in a file of its own because the
//! public surface next door is long enough without it. Nothing in here is reachable from outside
//! the crate: what a caller gets is [`super::Kernel::preview_request`] and
//! [`super::Kernel::step`], and this is what those two are made of.

use std::{collections::HashSet, sync::Arc};

use crate::{
    context::ContextItem,
    error::{Error, Result},
    event::{DeltaSink, Event},
    model::{Block, Content, ModelRequest, ModelResponse, ToolCall, ToolCallId},
    projection::Projection,
    tokens::TokenCounter,
    tool::ToolSpec,
};

use super::{Kernel, Restore, State};

impl Kernel {
    /// Builds and sends a request, records the answer, and prepares whatever it asked for.
    pub(super) async fn request(&self) -> Result<State> {
        // whatever happens - an error, or this future being dropped - the kernel does not stay
        // in `Requesting`
        let mut restore = Restore::new(self, State::Idle);

        self.maybe_compact().await;

        // a step that gets this far and then cannot proceed says why, rather than showing up on
        // the stream as a pair of state changes with nothing between them
        let prepared = self
            .provider()
            .ok_or(Error::NoProvider)
            .and_then(|provider| self.build_request().map(|built| (provider, built)));
        let (provider, (request, projection, tokens)) = match prepared {
            Ok(prepared) => prepared,
            Err(e) => {
                self.emit(Event::StepFailed {
                    error: e.to_string(),
                });
                return Err(e);
            }
        };

        // the provider's own account of what it is about to send, when it can give one and the
        // user has asked for it to be kept; see `Config::record_payloads` for why it is not free
        if self.0.config.record_payloads
            && let Some(payload) = provider.render(&request)
        {
            self.emit(Event::ModelPayload { payload });
        }

        self.emit(Event::ModelRequested {
            model: provider.info(),
            messages: request.messages.len(),
            tools: request.tools.len(),
            tokens,
            items: projection.included,
            skipped: projection.skipped,
            repairs: projection.repairs,
        });

        let mut response = match provider
            .respond(request, DeltaSink::new(self.clone()))
            .await
        {
            Ok(response) => response,
            Err(e) => {
                self.emit(Event::ModelFailed {
                    error: e.to_string(),
                });
                return Err(Error::Provider(e));
            }
        };
        // the provider has just said what the request it was handed actually cost, beside the
        // estimate that was made of it; the counter is told, and decides for itself whether that
        // is worth anything to it
        if let Some(reported) = response.usage.and_then(|usage| usage.input_tokens) {
            self.counter().observe(tokens, reported as usize);
        }

        // a model's tool calls are only useful if their identifiers are, and in practice they
        // sometimes are not (a streamed call whose first fragment carried no id, a provider that
        // numbers them all `0`)
        self.repair_call_ids(&mut response);

        let response = Arc::new(response);
        *self.0.last_response.write() = Some(response.clone());
        // gathered once, because a turn recorded as ordered blocks keeps its calls inside its
        // content and `response.tool_calls` is empty for it; everything below wants the calls
        // themselves rather than where they happen to be written down
        let calls: Vec<ToolCall> = response.calls().cloned().collect();

        // note: the reasoning is recorded on the turn that produced it, so that it is counted
        // and prunable like everything else, and so that a provider whose API insists on seeing
        // its own thinking again can get it back; whether it is *sent* is the projector's call
        let content = response.content.clone().unwrap_or_default();
        let item = self.add_item(
            ContextItem::assistant(content, response.tool_calls.clone())
                .with_reasoning(response.reasoning.clone()),
            true,
        );
        self.emit(Event::ModelFinished {
            stop: response.stop.clone(),
            usage: response.usage,
            tool_calls: calls.iter().map(|c| c.id.clone()).collect(),
            item,
        });

        let to = if calls.is_empty() {
            let to = State::Finished {
                item,
                stop: response.stop.clone(),
            };
            self.transition(&mut self.0.machine.lock(), to.clone());
            to
        } else {
            self.prepare_calls(&calls).await
        };
        restore.disarm();

        Ok(to)
    }

    /// Builds the next request, along with the projection it came from and its estimated size.
    pub(super) fn build_request(&self) -> Result<(ModelRequest, Projection, usize)> {
        let counter = self.counter();
        let tools = self.tool_specs();
        let tool_tokens = tool_tokens(&tools, &*counter);

        let (projection, context_tokens) = self.projected();

        if projection.messages.is_empty() {
            return Err(Error::EmptyProjection);
        }

        let request = ModelRequest {
            messages: projection.messages.clone(),
            tools,
            params: self.params(),
        };

        Ok((request, projection, context_tokens + tool_tokens))
    }

    /// Asks the compactor whether the context needs managing, and applies whatever it says.
    async fn maybe_compact(&self) {
        let Some(compactor) = self.compactor() else {
            return;
        };

        let budget = self.budget();
        if !compactor.should_compact(&budget) {
            return;
        }

        let items = self.items();
        if let Some(plan) = compactor.plan(&items, &budget).await {
            self.apply_compaction(plan);
        }
    }

    /// Projects the context and reports what the projection costs.
    ///
    /// note: Both [`Kernel::budget`] and the request builder go through here, so that the number
    /// a client is shown is the number that is about to be sent.
    ///
    /// note: counted over the messages that came out, not over the items that went in. They are
    /// not the same figure: a reference is labelled on its way out, and an elided item is a marker
    /// the size of a line where the item behind it may be ten thousand tokens. Summing the items
    /// would have the budget report what the context is holding, which is not what the request
    /// costs - and a compactor that elides would watch the total refuse to move and elide again.
    pub(super) fn projected(&self) -> (Projection, usize) {
        let projector = self.projector();
        let counter = self.counter();
        let context = self.0.context.read();
        let projection = projector.project(context.items());
        let tokens = projection_tokens(&projection, &*counter);

        (projection, tokens)
    }

    /// Returns the estimated size of the tool definitions.
    pub(super) fn tool_tokens(&self) -> usize {
        let counter = self.counter();

        tool_tokens(&self.tool_specs(), &*counter)
    }

    /// Gives every tool call a usable identifier that is unique *within the session*, announcing
    /// each change.
    ///
    /// note: This runs before the model's turn is recorded, so the call and its result always
    /// agree. Doing nothing instead would mean recording a pair that cannot be matched up -
    /// which most providers reject, and which is very hard to see afterwards.
    ///
    /// note: The identifiers a whole session has used are remembered, not just the ones in the
    /// response being repaired. A provider that numbers its calls from zero on every turn - and
    /// they exist - would otherwise produce a request carrying the same `tool_call_id` twice,
    /// and, worse, one in which pruning a single result silently leaves a call unanswered,
    /// because a set of identifiers cannot tell the two apart.
    ///
    /// note: a turn recorded as ordered blocks keeps its calls in its content, so repairing one
    /// means rewriting the sequence. That is only done when something actually needed repairing -
    /// which is almost never - so the ordinary turn pays a walk over its own blocks and nothing
    /// else.
    fn repair_call_ids(&self, response: &mut ModelResponse) {
        let Some(blocks) = response.content.as_ref().and_then(Content::as_blocks) else {
            self.rename_calls(&mut response.tool_calls);
            return;
        };

        let mut calls: Vec<ToolCall> = blocks.iter().filter_map(Block::call).cloned().collect();
        if !self.rename_calls(&mut calls) {
            return;
        }

        let mut renamed = calls.into_iter();
        let rebuilt: Vec<Block> = blocks
            .iter()
            .map(|block| match block {
                Block::Call(_) => match renamed.next() {
                    Some(call) => Block::Call(call),
                    // unreachable: as many went in as came out
                    None => block.clone(),
                },
                _ => block.clone(),
            })
            .collect();
        response.content = Some(Content::Blocks(rebuilt.into()));
    }

    /// Renames whatever needs renaming, returning whether anything did.
    fn rename_calls(&self, calls: &mut [ToolCall]) -> bool {
        let mut repairs = Vec::new();
        {
            let mut seen = self.0.seen_calls.lock();
            let mut in_response: HashSet<ToolCallId> = HashSet::with_capacity(calls.len());

            for (index, call) in calls.iter_mut().enumerate() {
                let reason = if call.id.0.is_empty() {
                    "the provider left the identifier empty"
                } else if in_response.contains(&call.id) {
                    "the provider used the identifier twice in one response"
                } else if seen.contains(&call.id) {
                    "the provider reused an identifier from earlier in the session"
                } else {
                    seen.insert(call.id.clone());
                    in_response.insert(call.id.clone());
                    continue;
                };

                let was = std::mem::take(&mut call.id.0);
                let mut attempt = 0;
                call.id = loop {
                    let candidate = match attempt {
                        0 => ToolCallId(format!("call_{index}")),
                        n => ToolCallId(format!("call_{index}_{n}")),
                    };
                    if !seen.contains(&candidate) {
                        break candidate;
                    }
                    attempt += 1;
                };
                seen.insert(call.id.clone());
                in_response.insert(call.id.clone());

                repairs.push(Event::ToolCallRepaired {
                    call: call.id.clone(),
                    was,
                    reason: reason.to_owned(),
                });
            }
        }

        let repaired = !repairs.is_empty();
        for repair in repairs {
            self.emit(repair);
        }

        repaired
    }
}

/// Counts what a projection costs, which is what a request carrying it would cost.
///
/// note: the one definition of "the projected total", because there are two callers and they were
/// not agreeing. Counted over the messages that came out rather than the items that went in: a
/// reference is labelled on its way out, and an elided item is a marker the size of a line where
/// the item behind it may be ten thousand tokens.
pub(super) fn projection_tokens(projection: &Projection, counter: &dyn TokenCounter) -> usize {
    projection
        .messages
        .iter()
        .map(|message| counter.count_message(message))
        .sum()
}

/// Estimates the size of the given tool definitions: the schemas plus the descriptions.
fn tool_tokens(specs: &[ToolSpec], counter: &dyn TokenCounter) -> usize {
    specs
        .iter()
        .map(|spec| {
            counter.count_schema(&spec.schema)
                + counter.count(&Content::text(format!("{} {}", spec.id, spec.description)))
        })
        .sum()
}
