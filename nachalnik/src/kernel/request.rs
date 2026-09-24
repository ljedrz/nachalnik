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
    model::{
        Block, Content, ModelRequest, ModelResponse, Overrun, TooLong, ToolCall, ToolCallId, Usage,
    },
    projection::Projection,
    tokens::TokenCounter,
    tool::ToolSpec,
};

use super::{Batch, Kernel, Restore, State};

impl Kernel {
    /// Builds and sends a request, records the answer, and prepares whatever it asked for.
    pub(super) async fn request(&self) -> Result<State> {
        // whatever happens - an error, or this future being dropped - the kernel does not stay
        // in `Requesting`
        let mut restore = Restore::new(self, State::Idle);

        self.maybe_compact().await;

        // the counter that makes the estimate is the one told what it came to, whatever is
        // installed by the time the answer arrives: one swapped in meanwhile never counted this
        let counter = self.counter();

        // a step that gets this far and then cannot proceed says why, rather than showing up on
        // the stream as a pair of state changes with nothing between them
        let prepared = self
            .provider()
            .ok_or(Error::NoProvider)
            .and_then(|provider| self.build_request(&*counter).map(|built| (provider, built)));
        let (provider, (request, projection, cost)) = match prepared {
            Ok(prepared) => prepared,
            Err(e) => {
                self.emit(Event::StepFailed {
                    overrun: None,
                    error: e.to_string(),
                });
                return Err(e);
            }
        };

        // the compactor has already had its turn, above, and this check is for when it could not
        // get the request under the limit - everything it might have taken is pinned, or there
        // was nothing of the kind it takes. Sending anyway buys one round trip and the endpoint's
        // own account of a figure that is already on the screen; see
        // `Config::refuse_oversized_requests` for the half of this that is a judgement rather
        // than arithmetic
        if let Some(overrun) = self.oversized(provider.info().context_limit, &cost) {
            self.emit(Event::StepFailed {
                overrun: Some(overrun),
                error: Error::TooLong(overrun).to_string(),
            });
            return Err(Error::TooLong(overrun));
        }

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
            tokens: cost.tokens,
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
                // a refusal for being too long is the one failure carrying a measurement, and
                // the only one taken in the units the *limit* is enforced in. A counter that
                // learns only from answered requests learns nothing from the point where every
                // request fails, which is the point it most needs correcting at: the figure on
                // screen stays under a limit the model is already over, and the same request
                // goes out again. It goes through the same door a reported usage does, with the
                // same condition on it - a counter that disowned part of what went out is not
                // being told about the same bytes
                let overrun = TooLong::of(&*e).map(|too_long| too_long.overrun);
                if let Some(overrun) = overrun
                    && cost.uncounted == 0
                {
                    counter.observe(cost.tokens, overrun.tokens as usize);
                }

                self.emit(Event::ModelFailed {
                    error: e.to_string(),
                    overrun,
                });
                return Err(Error::Provider(e));
            }
        };

        // the dialect says a cached prefix is part of the prompt figure; an endpoint that
        // disagrees is read the way the dialect meant before either the counter or a client
        // sees it, so that both are looking at the same repaired number
        response.usage = response.usage.map(Usage::settled);
        // the provider has just said what the request it was handed actually cost, beside the
        // estimate that was made of it; the counter is told, and decides for itself whether that
        // is worth anything to it.
        //
        // note: unless the counter said it could not price part of what went out, in which case
        // the two numbers are not about the same thing and the difference between them is not an
        // error to learn from. `Calibrating` corrects with a single multiplier, so a screenshot
        // it estimated at nothing and the provider billed a thousand tokens for gets spread over
        // the bytes it *could* see: two thousand tokens of prose beside one picture settles on a
        // scale of about 1.5, and from then on the prose reads three thousand while the picture
        // still reads nothing. The ratio is cumulative, so deleting the picture does not undo it.
        // The counter disowned that content; respecting the disownment is the kernel's half
        if let Some(reported) = response.usage.and_then(|usage| usage.input_tokens)
            && cost.uncounted == 0
        {
            counter.observe(cost.tokens, reported as usize);
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
        //
        // note: recorded as the first of a batch, so the checkpoint it takes is written down under
        // the lock that took it. An answer to a call nobody can run joins that checkpoint, and read
        // again afterwards the number could be a push that landed in between - one `undo` would
        // then take the answer with the push and leave the turn's call unanswered
        let content = response.content.clone().unwrap_or_default();
        let mut turn = Batch::default();
        let item = self.add_in(
            ContextItem::assistant(content, response.tool_calls.clone())
                .with_reasoning(response.reasoning.clone()),
            &mut turn,
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
            self.prepare_calls(&calls, turn).await
        };
        restore.disarm();

        Ok(to)
    }

    /// Builds the next request, along with the projection it came from and what it costs.
    pub(super) fn build_request(
        &self,
        counter: &dyn TokenCounter,
    ) -> Result<(ModelRequest, Projection, Cost)> {
        let tools = self.tool_specs();
        let tool_tokens = tool_tokens(&tools, counter);

        let (projection, context) = self.projected_with(counter);

        if projection.messages.is_empty() {
            return Err(Error::EmptyProjection);
        }

        let request = ModelRequest {
            messages: projection.messages.clone(),
            tools,
            params: self.params(),
        };
        // a tool schema is JSON and a counter always has a number for one, so the tool side
        // contributes tokens and never an abstention
        let cost = Cost {
            tokens: context.tokens + tool_tokens,
            uncounted: context.uncounted,
        };

        Ok((request, projection, cost))
    }

    /// Whether a request this size is one the model will refuse to read, and by how much.
    ///
    /// note: `>` rather than a fraction of the limit. What is being decided is whether the
    /// endpoint will refuse this, and it refuses at the limit - so a margin here would be the
    /// kernel refusing requests on its own account, which is a policy and not this crate's.
    ///
    /// note: a request carrying something the counter could not price is *bigger* than the
    /// figure, never smaller, so the comparison holds in the direction that matters. The estimate
    /// being an estimate is why the whole check is a knob.
    ///
    /// note: a limit of `0` is no limit here, as it is to
    /// [`Budget::fraction_used`](crate::Budget::fraction_used): it is what an endpoint that does
    /// not know says, and taken at its word it refused every request there was while the budget
    /// beside it reported the limit as unknown.
    fn oversized(&self, limit: Option<usize>, cost: &Cost) -> Option<Overrun> {
        let limit = limit.filter(|limit| {
            *limit != 0 && self.0.config.refuse_oversized_requests && cost.tokens > *limit
        })?;

        Some(Overrun {
            tokens: cost.tokens as u64,
            limit: Some(limit as u64),
        })
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
    pub(super) fn projected_with(&self, counter: &dyn TokenCounter) -> (Projection, Cost) {
        let projector = self.projector();
        let context = self.0.context.read();
        let projection = projector.project(context.items());
        let cost = projection_cost(&projection, counter);

        (projection, cost)
    }

    /// Returns the estimated size of the tool definitions, priced by the counter given.
    pub(super) fn tool_tokens_with(&self, counter: &dyn TokenCounter) -> usize {
        tool_tokens(&self.tool_specs(), counter)
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
    /// means rewriting the sequence. That is only done when something actually needs repairing -
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
        let mut repaired = false;
        {
            // held until each repair has been announced, as `Kernel::reserve_calls` holds it: a
            // reservation made in between would otherwise be recorded ahead of a change to the
            // set that was made before it
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

                repaired = true;
                self.emit(Event::ToolCallRepaired {
                    call: call.id.clone(),
                    was,
                    reason: reason.to_owned(),
                });
            }
        }

        repaired
    }
}

/// Counts what a projection costs, which is what a request carrying it would cost.
///
/// note: the one definition of "the projected total", so that [`Kernel::projected`] and
/// [`Kernel::apply_compaction`] cannot disagree about it. It is counted over the messages that
/// came out rather than the items that went in, for the reason on [`Kernel::projected`].
///
/// note: the same walk answers both figures. An abstention counted over the *items* would report a
/// picture inside an elided item as unpriced, when what goes out in its place is a one-line marker
/// with no picture in it - the budget would name a hole in a request that does not have one.
pub(super) fn projection_cost(projection: &Projection, counter: &dyn TokenCounter) -> Cost {
    projection
        .messages
        .iter()
        .fold(Cost::default(), |cost, message| Cost {
            tokens: cost.tokens + counter.count_message(message),
            uncounted: cost.uncounted + counter.uncounted_message(message),
        })
}

/// What a projection costs: the estimate, and how much of the request the estimate does not
/// cover.
///
/// note: the two travel together everywhere, so the pair has a name rather than being two bare
/// `usize`s in the same order, for every reader to get right by remembering which was which.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Cost {
    /// The estimated tokens.
    pub tokens: usize,
    /// The pieces of content the counter would not put a number on, so that `tokens` can be read
    /// as the floor it is.
    pub uncounted: usize,
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
