//! What a kernel is assembled from, and swapping any of it while it runs: the provider, the tools,
//! the policy, the projector, the counter, the compactor, the full notice and the parameters.

use std::sync::Arc;

#[cfg(doc)]
use crate::session::Snapshot;
use crate::{
    compaction::Compactor,
    context::ContextItem,
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
    pub fn set_provider(&self, provider: Arc<dyn Provider>) -> Option<Arc<dyn Provider>> {
        let to = provider.info();
        let mut held = self.0.provider.write();
        let previous = held.replace(provider);
        *self.0.announced.lock() = Some(to.clone());
        // still under the lock: see the note on `Kernel::emit`
        self.emit(Event::ModelChanged {
            from: previous.as_ref().map(|p| p.info()),
            to: Some(to),
        });

        previous
    }

    /// Removes the provider, returning it.
    pub fn clear_provider(&self) -> Option<Arc<dyn Provider>> {
        let mut held = self.0.provider.write();
        let previous = held.take();
        *self.0.announced.lock() = None;
        self.emit(Event::ModelChanged {
            from: previous.as_ref().map(|p| p.info()),
            to: None,
        });

        previous
    }

    /// Says that the provider this kernel holds now answers as a different model, and announces it
    /// as [`Event::ModelChanged`]; returns whether anything had changed.
    ///
    /// note: for a provider that switches its model in place, which is the only way a client can
    /// switch one it shares with something else. [`Kernel::set_provider`] with the same provider
    /// would ask it what it was *after* the switch and report `from` and `to` as the same model,
    /// so the kernel remembers what it last announced and compares against that instead.
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
        // the list is read off the guard rather than through `tool_ids`, which would ask for a
        // read lock on the one this guard holds for writing
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
        // the recount waits until the lock is let go, because it reads the counter through it.
        // What has to be under one lock is the change and the announcement of it; the figures
        // the recount brings into line are announced as an event of their own
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
        // the correction and the recount it causes under one lock, as a change and its
        // announcement are: applied first and recounted after, a snapshot in between held the
        // new scale beside figures counted on the old one
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

    /// Sets (or, with `None`, removes) the compactor, returning the previous one.
    ///
    /// note: With no compactor set, the context only ever changes because somebody asked it to.
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

    /// Sets (or, with `None`, removes) what is put into the context as it becomes full, returning
    /// the previous one.
    ///
    /// note: how the model is told, and the words are the caller's, since the kernel ships no text
    /// the model reads - what the model can do about a full context depends on the tools it has,
    /// which only the caller knows. The kernel owns *when* and *where*: the notice is pushed as
    /// [`Event::ContextFull`] says the context is full, which is before a request, between a
    /// turn's tool results and the next request - so a tool loop that fills the context hears it
    /// in the same turn, while there is still room under the limit for the request that carries
    /// it - and only where there is: a notice that would take that request over the limit is left
    /// out, since the kernel would refuse the request and nobody would read it. It is pushed at
    /// most once per fill, and excluded again as the context has room.
    ///
    /// note: the notice in the context is recognised by what it is - its kind, source, label and
    /// content - rather than by an identifier remembered here, so that a session resumed while
    /// full does not stack a second copy on the first. A notice the person or the model excluded
    /// stays excluded for the rest of that fill, and one they pinned is left where it is.
    ///
    /// note: each placing and each retiring is an ordinary change to the context, so each is an
    /// undo step, and neither is taken more than once per fill.
    pub fn set_full_notice(&self, notice: Option<ContextItem>) -> Option<ContextItem> {
        std::mem::replace(&mut *self.0.full_notice.write(), notice)
    }

    /// Returns what is put into the context as it becomes full, if anything is.
    pub fn full_notice(&self) -> Option<ContextItem> {
        self.0.full_notice.read().clone()
    }

    /// Returns the parameters that will be sent with the next request.
    pub fn params(&self) -> Params {
        self.0.params.read().clone()
    }

    /// Sets the parameters sent with every request, returning the previous ones.
    ///
    /// note: parameters equal to the ones in force are not an operation, which is the rule
    /// [`Kernel::replace`] and [`Kernel::set_state`] already follow. `model.params` over a request
    /// that will go out byte for byte the same is a change somebody reading the log goes looking
    /// for and cannot find. This is the one component setter that can tell: the rest hold a trait
    /// object, and two `Arc<dyn Provider>` that would behave alike are not comparable.
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
