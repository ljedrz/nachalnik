//! What a kernel is assembled from, and swapping any of it while it runs: the provider, the tools,
//! the policy, the projector, the counter, the compactor and the parameters.

use std::sync::Arc;

#[cfg(doc)]
use crate::session::Snapshot;
use crate::{
    compaction::Compactor,
    event::Event,
    model::{ModelInfo, Params, Provider},
    permissions::{PermissionId, PermissionPolicy, Verdict},
    projection::Projector,
    tokens::{Calibration, TokenCounter},
    tool::{Tool, ToolSpec},
};

use super::Kernel;

impl Kernel {
    /// Sets the provider, returning the previous one.
    ///
    /// note: `from` is the last announced model, not what the provider now says it is, as in
    /// [`Kernel::provider_changed`]. A provider that switched in place unannounced, or was set
    /// again after switching, would otherwise be named as the model it became, and the record would
    /// never say it had been the one before.
    pub fn set_provider(&self, provider: Arc<dyn Provider>) -> Option<Arc<dyn Provider>> {
        let mut held = self.0.provider.write();
        let previous = held.replace(provider);
        // what was installed, asked under the lock that installed it
        let to = held.as_ref().map(|provider| provider.info());
        let from = std::mem::replace(&mut *self.0.announced.lock(), to.clone());
        // still under the lock: see the note on `Kernel::emit`
        self.emit(Event::ModelChanged { from, to });

        previous
    }

    /// Removes the provider, returning it, and announces it as gone from the model last announced;
    /// see [`Kernel::set_provider`].
    pub fn clear_provider(&self) -> Option<Arc<dyn Provider>> {
        let mut held = self.0.provider.write();
        let previous = held.take();
        let from = self.0.announced.lock().take();
        self.emit(Event::ModelChanged { from, to: None });

        previous
    }

    /// Says the held provider now answers as a different model, announces [`Event::ModelChanged`],
    /// and returns whether anything changed.
    ///
    /// note: for a provider that switches its model in place, the only way a client can switch one
    /// it shares with something else. [`Kernel::set_provider`] with the same provider would ask
    /// what it is after the switch and report `from` and `to` as the same model, so the kernel
    /// compares against what it last announced instead.
    pub fn provider_changed(&self) -> bool {
        // the component's lock, held while the change is announced, for the reason `emit` gives
        let held = self.0.provider.write();
        let to = held.as_ref().map(|provider| provider.info());
        let mut announced = self.0.announced.lock();
        if *announced == to {
            return false;
        }
        let from = std::mem::replace(&mut *announced, to.clone());
        self.emit(Event::ModelChanged { from, to });

        true
    }

    /// Records that the policy was told something that changes what it answers, as
    /// [`Event::PolicyRuled`].
    ///
    /// note: a record and nothing more. The kernel holds no rules and cannot see a policy's, so
    /// whoever changed one says what it changed, and the record is as good as what it is told -
    /// which is why the event names the rule in the policy's own words rather than in a form the
    /// kernel checks. `answering` is the question whose answer made it, where one did, and `once`
    /// that it holds for that question's call alone.
    pub fn record_rule(
        &self,
        subject: impl Into<String>,
        verdict: Verdict,
        answering: Option<PermissionId>,
        once: bool,
    ) {
        self.emit(Event::PolicyRuled {
            subject: subject.into(),
            verdict,
            answering,
            once,
        });
    }

    /// Returns the provider, if one is set.
    pub fn provider(&self) -> Option<Arc<dyn Provider>> {
        self.0.provider.read().clone()
    }

    /// Returns the identity and capabilities of the model in use.
    pub fn model_info(&self) -> Option<ModelInfo> {
        self.0.provider.read().as_ref().map(|p| p.info())
    }

    /// Registers a tool, returning the one it replaced, if any.
    pub fn add_tool(&self, tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
        let id = tool.spec().id;
        let mut held = self.0.tools.write();
        let previous = held.insert(id, tool);
        // Read from the guard: `tool_ids` would take a read lock on a lock already held for writing
        self.emit(Event::ToolsChanged {
            tools: held.keys().cloned().collect(),
        });

        previous
    }

    /// Unregisters a tool, returning it.
    pub fn remove_tool(&self, id: &str) -> Option<Arc<dyn Tool>> {
        let mut held = self.0.tools.write();
        let previous = held.remove(id);
        if previous.is_some() {
            self.emit(Event::ToolsChanged {
                tools: held.keys().cloned().collect(),
            });
        }

        previous
    }

    /// Returns the tool with the given identifier.
    pub fn tool(&self, id: &str) -> Option<Arc<dyn Tool>> {
        self.0.tools.read().get(id).cloned()
    }

    /// Returns the identifiers of the registered tools, in the order they are offered to the
    /// model.
    pub fn tool_ids(&self) -> Vec<String> {
        self.0.tools.read().keys().cloned().collect()
    }

    /// Returns the definitions of the registered tools, exactly as they will be sent.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.0.tools.read().values().map(|t| t.spec()).collect()
    }

    /// Sets the permission policy, returning the previous one.
    pub fn set_policy(&self, policy: Arc<dyn PermissionPolicy>) -> Arc<dyn PermissionPolicy> {
        let to = policy.name().to_owned();
        let mut held = self.0.policy.write();
        let previous = std::mem::replace(&mut *held, policy);
        self.emit(Event::PolicyChanged {
            from: previous.name().to_owned(),
            to,
        });

        previous
    }

    /// Returns the permission policy.
    pub fn policy(&self) -> Arc<dyn PermissionPolicy> {
        self.0.policy.read().clone()
    }

    /// Sets the projector, returning the previous one.
    pub fn set_projector(&self, projector: Arc<dyn Projector>) -> Arc<dyn Projector> {
        let to = projector.name().to_owned();
        let mut held = self.0.projector.write();
        let previous = std::mem::replace(&mut *held, projector);
        self.emit(Event::ProjectorChanged {
            from: previous.name().to_owned(),
            to,
        });

        previous
    }

    /// Returns the projector.
    pub fn projector(&self) -> Arc<dyn Projector> {
        self.0.projector.read().clone()
    }

    /// Sets the token counter and recounts the context, returning the previous counter.
    pub fn set_counter(&self, counter: Arc<dyn TokenCounter>) -> Arc<dyn TokenCounter> {
        let to = counter.name().to_owned();
        let previous = {
            let mut held = self.0.counter.write();
            let previous = std::mem::replace(&mut *held, counter);
            self.emit(Event::CounterChanged {
                from: previous.name().to_owned(),
                to,
            });

            previous
        };
        // the recount waits for the lock to be released, because it reads the counter through it.
        // The change and its announcement must be under one lock; the figures the recount brings
        // into line are announced as their own event
        self.recount();

        previous
    }

    /// Returns the token counter.
    pub fn counter(&self) -> Arc<dyn TokenCounter> {
        self.0.counter.read().clone()
    }

    /// Tells the counter what a previous one had learned, recounts, and returns what it knew
    /// before.
    ///
    /// note: the front door for [`TokenCounter::recalibrate`], and it exists because reaching
    /// past the kernel for it is a trap. A correction changes what is counted *from then on*,
    /// like [`TokenCounter::observe`] - so a caller who applies one to a context that is already
    /// counted has every stored figure on the old scale and every projected figure on the new
    /// one, which is two budgets for the same bytes. [`Kernel::resume`] avoids it by
    /// recalibrating before the items are counted; anything reading a
    /// [`Snapshot::calibration`] into a session that is already running wants this instead.
    ///
    /// note: it recounts for the same reason [`Kernel::set_counter`] does. What changed is what
    /// the kernel's numbers mean, and leaving the old ones to be read is the quiet rewrite this
    /// crate does not do - so the correction is applied and the figures are brought into line, out
    /// loud, as [`Event::ContextRecounted`].
    ///
    /// note: `None`, and nothing done at all, when the counter does not learn. The kernel only
    /// ever offers back what a counter gave it, which is the rule [`TokenCounter::calibration`]
    /// states; a counter that never changes its mind has nothing to be told and nothing to
    /// recount for. A correction identical to the one in force is not a change either, and takes
    /// no recount - judged by what the counter reports afterwards rather than by what it was
    /// handed, because a counter may apply less than it is offered.
    pub fn recalibrate(&self, calibration: Calibration) -> Option<Calibration> {
        let held = self.0.counter.read();
        let counter = &**held;
        // the correction and its recount happen under one lock, as a change and its announcement
        // do: applied first and recounted after, a snapshot in between would hold the new scale
        // beside figures counted on the old one
        let mut context = self.0.context.write();
        let previous = counter.calibration()?;
        counter.recalibrate(calibration);
        // a counter is entitled to hold a correction to what it can actually apply, and
        // `Calibrating` does - so offering it a scale it refuses is not a change to recount for,
        // and a scale it takes in part is a recount against the part it took
        if counter.calibration() != Some(previous) {
            self.recount_in(&mut context, counter);
        }

        Some(previous)
    }

    /// Sets or removes the compactor, returning the previous one.
    ///
    /// note: with no compactor set, the context only changes when somebody asks it to.
    pub fn set_compactor(
        &self,
        compactor: Option<Arc<dyn Compactor>>,
    ) -> Option<Arc<dyn Compactor>> {
        let to = compactor.as_ref().map(|c| c.name().to_owned());
        let mut held = self.0.compactor.write();
        let previous = std::mem::replace(&mut *held, compactor);
        self.emit(Event::CompactorChanged {
            from: previous.as_ref().map(|c| c.name().to_owned()),
            to,
        });

        previous
    }

    /// Returns the compactor, if one is set.
    pub fn compactor(&self) -> Option<Arc<dyn Compactor>> {
        self.0.compactor.read().clone()
    }

    /// Sets how many of one answer's tool calls are run, returning the previous value; `None`, the
    /// default, runs every call.
    ///
    /// note: a model in a repetition loop can answer with thousands of calls, mostly the same one,
    /// and every one is prepared, run and recorded, filling the context with copies. Calls past the
    /// ceiling get an error saying how many ran and to ask for the rest in another answer; every
    /// call still has its result, and nothing past the ceiling runs or is asked about. A ceiling
    /// rather than one run of a repeated call, because a model may mean a repetition (a read after
    /// a write), and a loop of distinct calls is still a loop.
    ///
    /// note: a setter rather than a field of [`Config`](crate::Config), which a caller builds from
    /// its fields and which would break every one of them; it is a setting, not session state, so a
    /// snapshot does not carry it.
    pub fn set_call_ceiling(&self, ceiling: Option<usize>) -> Option<usize> {
        std::mem::replace(&mut *self.0.call_ceiling.write(), ceiling)
    }

    /// Returns how many of one answer's tool calls are run, where there is a ceiling.
    pub fn call_ceiling(&self) -> Option<usize> {
        *self.0.call_ceiling.read()
    }

    /// Sets the maximum number of requests a turn may send, returning the previous limit; `None`
    /// means no limit, so the turn continues until the model stops asking for tools. It starts at
    /// [`Config::max_requests_per_turn`](crate::Config::max_requests_per_turn).
    ///
    /// note: read before each request, so a change made during a turn applies from the next
    /// request. Raising it lets the turn continue; lowering it to the number already sent or fewer
    /// pauses the turn. A paused turn stays paused until [`Kernel::turn`] is called again, which
    /// resumes counting from where it stopped.
    ///
    /// note: like [`Kernel::set_call_ceiling`], it is a setting and not session state, so a
    /// snapshot does not carry it and a resumed kernel starts from its own `Config`.
    pub fn set_max_requests_per_turn(&self, max: Option<usize>) -> Option<usize> {
        std::mem::replace(&mut *self.0.max_requests.write(), max)
    }

    /// Returns how many requests a turn may send, where there is a limit.
    pub fn max_requests_per_turn(&self) -> Option<usize> {
        *self.0.max_requests.read()
    }

    /// Sets the maximum size of a picture in a tool's output that is kept whole beside text cut to
    /// the output limit, returning the previous value; `None`, the default, measures the picture
    /// with the rest of the output, so a picture over the limit is named instead.
    ///
    /// note: an output limit is bytes the model reads; a picture is not read by the byte. An MCP
    /// screenshot is a few megabytes of base64, over any text limit, and without this the model saw
    /// only the picture's name. Set it to the most the APIs in use take in one picture.
    /// [`Content::truncate_around_pictures`](crate::Content::truncate_around_pictures) shows what
    /// is kept and what is cut.
    ///
    /// note: a setting rather than a field of [`Config`](crate::Config), as
    /// [`Kernel::set_call_ceiling`] is, for the same reasons.
    pub fn set_picture_limit(&self, bytes: Option<usize>) -> Option<usize> {
        std::mem::replace(&mut *self.0.picture_limit.write(), bytes)
    }

    /// Returns how large a picture in a tool's output may be and still be kept whole, where one
    /// may.
    pub fn picture_limit(&self) -> Option<usize> {
        *self.0.picture_limit.read()
    }

    /// Sets the byte limit at which the output of a tool that sets no limit of its own is cut,
    /// returning the previous value; `None` cuts nothing. It starts at
    /// [`Config::default_tool_output_limit`](crate::Config::default_tool_output_limit).
    ///
    /// note: a setter as well as the field, because what is worth showing the model depends on its
    /// context window, which changes during the session. A tool from an MCP server says nothing
    /// about its limit and was held to the number the kernel was built with, whatever model came
    /// after.
    ///
    /// note: read as each call's output is recorded, so a change holds from the next call. Like
    /// [`Kernel::set_call_ceiling`], it is not part of the session's state, so a snapshot does not
    /// carry it.
    pub fn set_default_tool_output_limit(&self, bytes: Option<usize>) -> Option<usize> {
        std::mem::replace(&mut *self.0.output_limit.write(), bytes)
    }

    /// Returns what the output of a tool that sets no limit of its own is cut at, where it is cut.
    pub fn default_tool_output_limit(&self) -> Option<usize> {
        *self.0.output_limit.read()
    }

    /// Returns the parameters that will be sent with the next request.
    pub fn params(&self) -> Params {
        self.0.params.read().clone()
    }

    /// Sets the parameters sent with every request, returning the previous ones.
    ///
    /// note: parameters equal to the ones in force are not an operation, the rule
    /// [`Kernel::replace`] and [`Kernel::set_state`] already follow. A `model.params` event for a
    /// request that goes out byte for byte the same is a change a log reader looks for and cannot
    /// find. This is the only component setter that can tell: the rest hold a trait object, and two
    /// `Arc<dyn Provider>` that behave alike are not comparable.
    pub fn set_params(&self, params: Params) -> Params {
        let mut held = self.0.params.write();
        if *held == params {
            return params;
        }
        let previous = std::mem::replace(&mut *held, params.clone());
        self.emit(Event::ModelParamsChanged { params });

        previous
    }
}
