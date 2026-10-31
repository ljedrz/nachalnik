//! Building a request, sending it, and handling the response.
//!
//! note: the private half of [`super::Kernel`]'s request path, in a file of its own because the
//! public surface next door is long enough without it. A caller sees
//! [`super::Kernel::preview_request`] and [`super::Kernel::step`]; this is what they are made of.
//! The only public operations here are the full notice's setter and getter, beside the code that
//! places and retires the notice.

use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
};

use crate::{
    context::{ContextId, ContextItem, ContextState},
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
        let mut restore = Restore::new(self);

        // the counter that makes the estimate is the one told what it came to, whatever is
        // installed by the time the answer arrives: one swapped in meanwhile never counted this
        let counter = self.counter();
        let built = self.compacted(&*counter).await;

        // a step that gets this far and cannot proceed reports why instead of appearing on the
        // stream as two state changes with nothing between them
        let prepared = self
            .provider()
            .ok_or(Error::NoProvider)
            .and_then(|provider| built.map(|built| (provider, built)));
        let (
            provider,
            Built {
                request,
                projection,
                context,
                tools,
            },
        ) = match prepared {
            Ok(prepared) => prepared,
            Err(e) => {
                self.emit(Event::StepFailed {
                    overrun: None,
                    error: e.to_string(),
                });
                return Err(e);
            }
        };
        let cost = context + tools;

        // the compactor has had its turn above; this is for when it could not get the request under
        // the limit, because what it might take is pinned or of no kind it takes. Sending anyway
        // buys one round trip and the endpoint's account of a figure already on the screen. See
        // `Config::refuse_oversized_requests` for why this is a setting
        if let Some(overrun) = self.oversized(provider.info().context_limit, &cost) {
            self.emit(Event::StepFailed {
                overrun: Some(overrun),
                error: Error::TooLong(overrun).to_string(),
            });
            return Err(Error::TooLong(overrun));
        }

        // the provider's own account of what it is about to send, when it can give one and the user
        // has asked for it to be kept; see `Config::record_payloads` for why it is not free
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
            items: projection.included.iter().copied().collect(),
            skipped: projection.skipped,
            repairs: projection.repairs,
        });

        // the counter learns from what the provider says this request cost, beside its estimate,
        // and decides for itself what that is worth.
        //
        // note: not when the counter could not price part of the request: the two numbers are then
        // about different things. `Calibrating` corrects with one cumulative multiplier, so a
        // picture it priced at nothing would be spread over the bytes it could see, every later
        // estimate of prose would read high, and deleting the picture would not undo it. Respecting
        // what the counter declined is the kernel's half, and both figures below go through this
        // one door so neither can skip it.
        let observe = |reported: usize| {
            if cost.uncounted == 0 {
                counter.observe(cost.tokens, reported);
            }
        };

        let mut response = match provider
            .respond(request, DeltaSink::new(self.clone()))
            .await
        {
            Ok(response) => response,
            Err(e) => {
                // a refusal for being too long is the only failure that carries a measurement, and
                // the only one in the limit's units. A counter that learned only from answered
                // requests would learn nothing where every request fails, which is where it most
                // needs correcting: the figure on screen would stay under a limit the model is
                // already over, and the same request would go out again. It goes through the same
                // door as reported usage.
                let overrun = TooLong::of(&*e).map(|too_long| too_long.overrun);
                if let Some(overrun) = overrun {
                    observe(overrun.tokens as usize);
                }

                self.emit(Event::ModelFailed {
                    error: e.to_string(),
                    overrun,
                });
                return Err(Error::Provider(e));
            }
        };

        // the dialect says a cached prefix is part of the prompt figure. An endpoint that disagrees
        // is read the way the dialect meant, before either the counter or a client sees it, so both
        // look at the same repaired number.
        response.usage = response.usage.map(Usage::settled);
        // the provider has just said what the request it was handed actually cost
        if let Some(reported) = response.usage.and_then(|usage| usage.input_tokens) {
            observe(reported as usize);
        }

        // a model's tool calls are only useful if their identifiers are, and in practice they
        // sometimes are not (a streamed call whose first fragment carried no id, a provider that
        // numbers them all `0`)
        self.repair_call_ids(&mut response);

        let response = Arc::new(response);
        *self.0.last_response.write() = Some(response.clone());
        // gathered once, because a turn recorded as ordered blocks keeps its calls in its content
        // and leaves `response.tool_calls` empty
        let calls: Vec<ToolCall> = response.calls().cloned().collect();

        // note: the reasoning is recorded on the turn that produced it, so it is counted and
        // excludable like anything else, and a provider whose API wants its own thinking back can
        // get it. Whether it is sent is the projector's call.
        //
        // note: recorded as the first of a batch, so the checkpoint it takes is written down under
        // the lock that took it. An answer to a call nobody can run joins that checkpoint; read
        // again afterwards, the number could belong to a push that landed in between, and one
        // `undo` would take the answer with the push and leave the turn's call unanswered.
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
    pub(super) fn build_request(&self, counter: &dyn TokenCounter) -> Result<Built> {
        // note: the tools, the projector with the context, and the parameters are read with their
        // locks held together, because each setter announces while holding its own. Read one after
        // another, a client that added a tool and then set parameters could have a request go out
        // with the parameters but not the tool, which no prefix of the log describes. The order is
        // the one on `InnerKernel`
        let (tools, projection, params) = {
            let tools = self.0.tools.read();
            let projector = self.0.projector.read();
            let params = self.0.params.read();
            let context = self.0.context.read();
            let specs: Vec<ToolSpec> = tools.values().map(|tool| tool.spec()).collect();
            let projection = projector.project(context.items());
            (specs, projection, params.clone())
        };
        let tool_cost = tool_cost(&tools, counter);
        let context = projection_cost(&projection, counter);

        if projection.messages.is_empty() {
            return Err(Error::EmptyProjection);
        }

        let request = ModelRequest {
            messages: projection.messages.clone(),
            tools,
            params,
        };
        Ok(Built {
            request,
            projection,
            context,
            tools: tool_cost,
        })
    }

    /// Whether a request this size is one the model will refuse to read, and by how much.
    ///
    /// note: `>` rather than a fraction of the limit: the endpoint refuses at the limit, and a
    /// margin would be the kernel refusing requests on its own account, a policy that is not this
    /// crate's.
    ///
    /// note: a request carrying something the counter could not price is larger than its figure,
    /// never smaller, so a request this calls oversized really is. That the figure is an estimate
    /// is why the check can be turned off.
    ///
    /// note: a limit of `0` means no limit here, as for
    /// [`Budget::fraction_used`](crate::Budget::fraction_used): it is what an endpoint that does
    /// not know says, and taken literally it refused every request while the budget called the
    /// limit unknown.
    fn oversized(&self, limit: Option<usize>, cost: &Cost) -> Option<Overrun> {
        let limit = limit.filter(|limit| {
            *limit != 0 && self.0.config.refuse_oversized_requests && cost.tokens > *limit
        })?;

        Some(Overrun {
            tokens: cost.tokens as u64,
            limit: Some(limit as u64),
        })
    }

    /// Asks the compactor whether the context needs managing, applies whatever it says, and builds
    /// the request that follows.
    ///
    /// note: it measures the result rather than being told: the context is full when the compactor
    /// still wants room after its pass, and only somebody else can do anything about that.
    /// [`Event::ContextFull`] says when that starts and stops. With no compactor nothing wants
    /// room, so the context is never full.
    ///
    /// note: the request is built first and the compactor is shown what that request costs, so a
    /// step projects and prices the context once where nothing needs managing, which is nearly
    /// every step. Each projection is linear in the context, and a step used to make two of them
    /// with nothing between them that could change the answer: one for the compactor's budget and
    /// one for the request. The request is built again only where something may have changed it:
    /// a compactor that was asked for a plan, which is awaited and so leaves room for anybody to
    /// change the context, and a full notice placed or retired.
    async fn compacted(&self, counter: &dyn TokenCounter) -> Result<Built> {
        let mut built = self.build_request(counter);
        let Some(compactor) = self.compactor() else {
            if self.mark_full(false) {
                built = self.build_request(counter);
            }
            return built;
        };

        let budget = self.budget_of(&built);
        let full = match compactor.should_compact(&budget) {
            false => false,
            true => {
                let items = self.items();
                if let Some(plan) = compactor.plan(&items, &budget).await {
                    self.apply_compaction(plan);
                }
                built = self.build_request(counter);
                compactor.wants_room(&self.budget_of(&built))
            }
        };
        if self.mark_full(full) {
            built = self.build_request(counter);
        }

        built
    }

    /// What a built request comes to as a [`Budget`], as [`Kernel::budget`] would count it.
    ///
    /// note: a request that could not be built has no figure of its own, and is counted afresh.
    /// That is a context that projects to nothing, so it is cheap.
    fn budget_of(&self, built: &Result<Built>) -> crate::Budget {
        match built {
            Ok(built) => self.budget_from(built.context, built.tools),
            Err(_) => self.budget(),
        }
    }

    /// Sets (or, with `None`, removes) what is put into the context as it becomes full, returning
    /// the previous one.
    ///
    /// note: the words are the caller's, because the kernel ships no text the model reads, and what
    /// the model can do about a full context depends on tools only the caller knows. The kernel
    /// decides when and where. The notice is pushed as [`Event::ContextFull`] reports the context
    /// full, which is before a request and between a turn's tool results and the next request, so a
    /// tool loop that fills the context hears it in the same turn. It is pushed only if the request
    /// still fits with it, since a request over the limit is refused unread; at most once per fill;
    /// and it is excluded again once the context has room.
    ///
    /// note: the notice is recognised by its kind, source, label and content rather than by a
    /// remembered identifier, so a session resumed while full does not stack a second copy. One the
    /// person or the model excluded stays excluded for the rest of that fill, and one they pinned
    /// is left alone.
    ///
    /// note: each placing and retiring is an ordinary context change and so an undo step, and
    /// neither happens more than once per fill.
    pub fn set_full_notice(&self, notice: Option<ContextItem>) -> Option<ContextItem> {
        std::mem::replace(&mut *self.0.full_notice.write(), notice)
    }

    /// Returns what is put into the context as it becomes full, if anything is.
    pub fn full_notice(&self) -> Option<ContextItem> {
        self.0.full_notice.read().clone()
    }

    /// Records whether the context is full, says so where that changed, and places or retires the
    /// caller's notice; see [`Kernel::set_full_notice`]. Returns whether it changed, which is when
    /// the context may have changed with it.
    fn mark_full(&self, full: bool) -> bool {
        if self.0.full.swap(full, Ordering::SeqCst) == full {
            return false;
        }

        let budget = self.budget();
        self.emit(Event::ContextFull {
            full,
            used: budget.used(),
            limit: budget.limit,
        });

        let Some(notice) = self.full_notice() else {
            return true;
        };
        let standing = |item: &ContextItem| {
            item.kind == notice.kind
                && item.source == notice.source
                && item.label == notice.label
                && item.content == notice.content
        };
        let items = self.items();
        if full {
            // note: any copy that still sends, pinned included, counts as the notice already given;
            // one excluded by hand does not, and a later fill is told again.
            //
            // note: only where the request still fits with it: one that would take the request over
            // the limit would get the request refused, and the turn would end with nothing sent.
            if !items
                .iter()
                .any(|item| standing(item) && item.state.sends_content())
                && self.carries(&notice)
            {
                self.push(notice.clone());
            }
        } else {
            // note: `Active` alone - a pinned copy is somebody's decision to keep it
            let retired: Vec<_> = items
                .iter()
                .filter(|item| standing(item) && item.state == ContextState::Active)
                .map(|item| item.id)
                .collect();
            self.set_state(retired, ContextState::Excluded, None);
        }

        true
    }

    /// Whether the next request would still be under the limit with `item` at the end of the
    /// context, by the same check that refuses one that is not.
    fn carries(&self, item: &ContextItem) -> bool {
        let counter = self.counter();
        let tools = tool_cost(&self.tool_specs(), &*counter);
        let projection = {
            let projector = self.projector();
            let context = self.0.context.read();
            let mut item = item.clone();
            item.id = ContextId(context.next_id());
            let mut items = context.items().to_vec();
            items.push(Arc::new(item));
            projector.project(&items)
        };
        let cost = projection_cost(&projection, &*counter);
        let limit = self.model_info().and_then(|info| info.context_limit);

        self.oversized(limit, &(cost + tools)).is_none()
    }

    /// Projects the context and reports what the projection costs.
    ///
    /// note: [`Kernel::budget`] goes through here, and the request builder projects the same way
    /// under its locks and counts with the same [`projection_cost`], so the number a client sees is
    /// the number about to be sent.
    ///
    /// note: counted over the messages produced, not the items: a reference gains its label, and an
    /// elided item is a one-line marker for what may be ten thousand tokens. Summing items would
    /// report what the context holds rather than what the request costs, and a compactor that
    /// elides would see no change and elide again.
    pub(super) fn projected_with(&self, counter: &dyn TokenCounter) -> (Projection, Cost) {
        let projector = self.projector();
        let context = self.0.context.read();
        let projection = projector.project(context.items());
        let cost = projection_cost(&projection, counter);

        (projection, cost)
    }

    /// Gives every tool call a usable identifier unique within the session, announcing each change.
    ///
    /// note: run before the model's turn is recorded, so a call and its result always agree; a pair
    /// that cannot be matched is rejected by most providers.
    ///
    /// note: every identifier the session has used is remembered, not just those in this response.
    /// A provider that numbers its calls from zero every turn would otherwise produce a request
    /// with one `tool_call_id` twice, in which excluding one result would silently leave a call
    /// unanswered.
    ///
    /// note: a turn recorded as ordered blocks keeps its calls in its content, so a repair rewrites
    /// the sequence. That is rare, so an ordinary turn pays only a walk over its blocks.
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
            // Held until each repair has been announced, as `Kernel::reserve_calls` holds it;
            // otherwise a reservation made in between would be recorded ahead of a change to the
            // set made before it.
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

/// A request ready to send, with the projection it was made from and what its two halves cost.
///
/// note: the halves apart, so a [`Budget`](crate::Budget) can be read off a built request rather
/// than by projecting the context again; see [`Kernel::compacted`].
pub(super) struct Built {
    /// The request.
    pub request: ModelRequest,
    /// The projection its messages came from.
    pub projection: Projection,
    /// What the messages cost.
    pub context: Cost,
    /// What the tool definitions cost.
    pub tools: Cost,
}

/// Counts what a projection costs, which is what a request carrying it would cost.
///
/// note: the one definition of the projected total, so [`Kernel::projected_with`], the request
/// builder and [`Kernel::apply_compaction`] cannot disagree. It counts the messages produced, not
/// the items, for the reason on [`Kernel::projected_with`].
///
/// note: one walk gives both figures. Counted over the items, a picture inside an elided item would
/// be reported unpriced though what goes out is a marker with no picture: a hole in a request that
/// has none.
pub(super) fn projection_cost(projection: &Projection, counter: &dyn TokenCounter) -> Cost {
    projection
        .messages
        .iter()
        .map(|message| Cost {
            tokens: counter.count_message(message),
            uncounted: counter.uncounted_message(message),
        })
        .sum()
}

/// What a projection costs: the estimate, and how much of the request the estimate does not cover.
///
/// note: the two travel together everywhere, so the pair is named rather than two bare `usize`s
/// whose order readers must remember.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Cost {
    /// The estimated tokens.
    pub tokens: usize,
    /// The pieces of content the counter would not put a number on, so that `tokens` can be read
    /// as the floor it is.
    pub uncounted: usize,
}

impl std::ops::Add for Cost {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            tokens: self.tokens + other.tokens,
            uncounted: self.uncounted + other.uncounted,
        }
    }
}

impl std::iter::Sum for Cost {
    fn sum<I: Iterator<Item = Self>>(costs: I) -> Self {
        costs.fold(Self::default(), std::ops::Add::add)
    }
}

/// Estimates the size of the given tool definitions, the schemas plus the descriptions, and how
/// many pieces of them the counter would not count.
///
/// note: a counter is not obliged to price JSON, so a schema it abstains on is counted as unpriced,
/// as a message's content is; otherwise a request with tools it could not price would reach
/// [`TokenCounter::observe`] as if it had priced all of it.
pub(super) fn tool_cost(specs: &[ToolSpec], counter: &dyn TokenCounter) -> Cost {
    specs
        .iter()
        .map(|spec| {
            let named = Content::text(format!("{} {}", spec.id, spec.description));
            Cost {
                tokens: counter.count_schema(&spec.schema) + counter.count(&named),
                uncounted: counter.uncounted(&Content::Json(spec.schema.clone()))
                    + counter.uncounted(&named),
            }
        })
        .sum()
}
