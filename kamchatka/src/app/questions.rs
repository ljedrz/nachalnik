//! Answering what stands in the prompt's place: a tool's call waiting on a decision, a running
//! command that reached for the network, and a compaction waiting to be taken or left.

#[cfg(doc)]
use nachalnik::State;
use nachalnik::{Capability, Grant, PermissionId, PermissionRequest, Verdict};

use super::{App, Focus, Speaker};
use crate::tools::Subject;

impl App {
    /// Drops the calls a turn resting in [`State::Ready`] has decided and not run, and tells the
    /// model so.
    ///
    /// note: what stopping means where nothing is running. The calls run at the next step whatever
    /// is done first: excluding the turn that asked for them, or taking their tool out of the
    /// registry, changes the next request and not a call already decided. Dropped, each becomes a
    /// refusal the model reads, as `d` at a question does. Nothing carries the turn on: stopping
    /// is the opposite of asking for the rest, and `/continue` or a message is how it goes on.
    pub fn drop_decided(&mut self) {
        let dropped = self.kernel.cancel_pending_calls("dropped before it ran");
        self.say(
            Speaker::Note,
            format!(
                "{dropped} call(s) dropped without running; the model is told when the turn goes \
                 on - /continue, or a message"
            ),
        );
    }

    /// Answers one of the questions the kernel is waiting on, and carries the turn on if that was
    /// the last of them.
    ///
    /// note: here rather than in `keys.rs`, because three loops answer questions - the keys,
    /// `headless.rs` and [`crate::remote`] - and each doing it in its own words would leave each
    /// missing a different part. An answer is four things: telling the sandbox about a granted
    /// command that reaches the network, which is [`App::answer`]; honouring `always` over what
    /// the policy consulted; sweeping the questions queued behind this one; and driving the turn
    /// on, because a decision leaves the kernel resting with nobody driving it. Without the last,
    /// a headless run answers and then sits there.
    ///
    /// note: `remember` is what the `a` key means - *always* - and what it remembers is everything
    /// the policy actually consulted rather than what the tool declared, so a `yes, always` to a
    /// `curl` that left `network` on `ask` does not ask again on the next call. It then sweeps what
    /// is already in the queue, because a model asking for three things at once produces three
    /// questions before the first is shown, and a promise about what happens next has to cover
    /// what is already waiting.
    ///
    /// note: what it returns is about the question the caller asked about, and nothing else. The
    /// sweep's own failures go to [`App::say`], because they are the program reporting something
    /// nobody asked it to do - which is what that list is - and a caller handed them as *its*
    /// error would be told its own answer failed when it did not.
    pub fn decide(&mut self, id: PermissionId, grant: Grant, remember: bool) -> Result<(), String> {
        let Some(request) = self
            .kernel
            .pending_permissions()
            .into_iter()
            .find(|pending| pending.id == id)
        else {
            return Err(format!("there is no question {id} waiting to be answered"));
        };
        rememberable(grant, remember)?;

        if remember {
            // everything the policy actually consulted, not just what the tool declared - and
            // each one recorded against the question it answered, so that the calls it lets through
            // later say where their permission came from
            let judged = self.policy.judges(&request);
            self.policy.always(&judged);
            for subject in &judged {
                self.kernel
                    .record_rule(subject.to_string(), Verdict::Allow, Some(id), false);
            }
        }
        // the other thing a session waits on somebody for. Whatever the question cost in wall time
        // was spent reading it, and `permission.decided` is the line it lands on
        self.acted = true;
        self.answer(&request, grant)?;
        if remember {
            for waiting in self.kernel.pending_permissions() {
                // note: `App::answer` rather than `Kernel::decide`, because a swept question is
                // answered rather than merely decided. A model that asks for `ls` and `curl` in one
                // breath produces two questions, and `a` on the first is what lets the second
                // through - so the second has to be let through the same door, network grant and
                // all. Decided straight into the kernel, it would run with the network cut and
                // nothing anywhere saying why
                if self.policy.verdict(&waiting) == Verdict::Allow
                    && let Err(e) = self.answer(&waiting, Grant::Allow)
                {
                    self.say(Speaker::Error, e);
                }
            }
        }
        // the model may have asked for several things at once, and each is its own question
        if self.kernel.pending_permissions().is_empty() {
            match self.stepping {
                // somebody driving this a transition at a time did not ask for the rest of the
                // turn, and running it here would be the harness taking the wheel back
                true => self.say(
                    Speaker::Note,
                    "decided; /step runs the calls, /continue runs the rest of the turn",
                ),
                false => self.start_turn(),
            }
        }

        Ok(())
    }

    /// Answers a running command that reached for the network, and writes the answer down.
    ///
    /// note: the counterpart of [`App::decide`] for the question the kernel does not ask, and it is
    /// where all three loops answer one for the same reason that one exists: an answer is more
    /// than the command hearing it. It is a `policy.ruled` in the record - the kernel has no event
    /// for this question, so the rule is the paper trail - and with `remember` it is `net:reach`
    /// allowed from here on, which lets through every command already waiting on the same
    /// question.
    ///
    /// note: the command reads the answer, not the model: its sockets open or come back
    /// `Permission denied`, and the result it hands the model says which was answered.
    pub fn decide_reach(&mut self, id: u64, grant: Grant, remember: bool) -> Result<(), String> {
        rememberable(grant, remember)?;
        let allow = grant == Grant::Allow;
        self.policy.reaching().answer(id, allow)?;

        let subject = Subject::Capability(Capability::net("reach"));
        if remember {
            self.policy.set(&subject, Verdict::Allow);
            self.refresh_full_notice();
            for waiting in self.policy.reaching().waiting() {
                let _ = self.policy.reaching().answer(waiting.id, true);
            }
        }
        self.kernel.record_rule(
            subject.to_string(),
            match allow {
                true => Verdict::Allow,
                false => Verdict::Deny,
            },
            None,
            !remember,
        );
        self.acted = true;

        Ok(())
    }

    /// The question a tool is waiting on, if one is.
    ///
    /// note: asked of the kernel every time rather than held here. What stands in the prompt's
    /// place is a *rendering* of the kernel's state, so it cannot be open when there is nothing to
    /// answer, or shut when there is - and [`App::prompted`] reads it for the same reason.
    pub fn asked(&self) -> Option<PermissionRequest> {
        self.kernel.pending_permissions().into_iter().next()
    }

    /// The running command waiting to hear whether it may reach the network, if one is.
    ///
    /// note: the other kind of question a tool waits on, and the one the kernel knows nothing
    /// about: it is asked while the call runs rather than before, by the gate holding the
    /// command's first attempt at an internet socket. See [`crate::gate`] and
    /// [`App::decide_reach`].
    pub fn reached(&self) -> Option<crate::tools::Reached> {
        self.policy.reaching().first()
    }

    /// Whether anything is standing in the prompt's place, waiting to be answered.
    ///
    /// note: the two kinds are a tool waiting on a decision and a compaction waiting on one, and
    /// everything about the screen that cares - what has the keys, what `tab` reaches, whether
    /// there is a prompt at all - cares only that there is one. Which it is, is a question for
    /// the panel that draws it and the key that answers it.
    pub fn asking(&self) -> bool {
        self.asked().is_some() || self.reached().is_some() || self.proposed.is_some()
    }

    /// Answers the compaction standing in the prompt's place.
    ///
    /// note: `take` works the pass out again rather than applying what was listed. The list is a
    /// snapshot of a context somebody has just been invited to change, so applying it would take
    /// exactly what they had protected while reading it. The kernel refuses a pinned item and
    /// says so, which would catch it - afterwards, in a report, which is what this question
    /// exists to avoid.
    ///
    /// note: public because the screen is not the only thing entitled to answer. `--headless` has
    /// no keys and answers this itself; see the note there for why it takes it rather than
    /// refusing it the way it refuses a tool's question.
    pub fn take_proposal(&mut self, take: bool) {
        self.proposed = None;
        self.focus = Focus::Input;
        // the next question starts at the top of itself, whatever was being read in this one
        self.question_scroll = 0;
        if !take {
            self.say(Speaker::Note, "left alone; nothing was compacted");
            return;
        }

        let Some(compactor) = self.kernel.compactor() else {
            return;
        };
        // worked out again rather than applied as listed, and sent out as `/compact` sends its
        // own: the pass can be a request of its own, and nothing holds the loop for one
        let (items, budget) = (self.kernel.items(), self.kernel.budget());
        self.errand(
            "the compaction pass",
            async move { compactor.plan(&items, &budget).await },
            |app, plan| match plan {
                // the report is said by `Event::Compacted`, like any other pass: one account of a
                // compaction, whoever asked for it
                Some(plan) => {
                    app.kernel.apply_compaction(plan);
                }
                None => app.say(
                    Speaker::Note,
                    "nothing left to take: everything the pass had listed is pinned now",
                ),
            },
        );
    }
}

/// Refuses `remember` on anything but an allow, for both kinds of question.
///
/// note: `always` is an allow. What it remembers is every subject the policy consulted, because
/// that is what it takes to let such a call through; a refusal has no such set, and refusing
/// every one of them would refuse every call sharing any. Taken as it came, a client asking never
/// to allow this wrote the rules that allow it.
fn rememberable(grant: Grant, remember: bool) -> Result<(), String> {
    match remember && grant != Grant::Allow {
        true => Err(
            "only an allow is remembered; a standing refusal is a rule, on the permissions tab \
             or `--deny`"
                .to_owned(),
        ),
        false => Ok(()),
    }
}
