//! What a session may spend: the ceiling, and what is charged against it and when.

use nachalnik::{Event, StopReason, Usage};

use super::{App, Speaker, text::thousands};

impl App {
    /// Adds what a response cost to the session's total, and stops the session if that was the
    /// last of what it was given.
    ///
    /// note: from the event rather than from [`nachalnik::Budget`], which is the kernel's estimate
    /// of the request it is *about to* build. What is added up here is what the provider charged
    /// for the ones already sent - `input + output`, which [`Usage`] defines to be the whole of a
    /// request's bill whichever dialect answered it - so this is a measurement rather than a
    /// conversion. Tokens rather than money because nothing here carries a price list, and a
    /// figure in money would be one: a table per model per endpoint, kept up to date by somebody,
    /// wrong quietly.
    ///
    /// note: a response the provider reported no figures for adds nothing, and the first time that
    /// happens it is said out loud. An endpoint that reports no usage is one this cannot see over,
    /// and a limit quietly never reached is worse than no limit at all: whoever set it would be
    /// reading the session as bounded when nothing is bounding it.
    ///
    /// note: except for a response whose stream never finished. It never reaches the chunk the
    /// figures ride on, so its silence is about the stream rather than the endpoint - and saying
    /// otherwise after a ctrl+c, a `--deadline` or a dropped connection tells whoever set the
    /// ceiling it has stopped meaning anything when it has not. `interrupted` and `cut off` are
    /// the stop reasons both of `nachalnik-providers`' dialects give a stream they were told to
    /// stop and one that ended mid-answer.
    ///
    /// note: a usage with neither figure in it is no usage, and one with only one of them is said
    /// once as well: what is added up is then a floor, and a ceiling counted against a floor is
    /// reached late.
    ///
    /// note: it stops *after* the response that crosses the line, because that is the first moment
    /// anybody knows what the response cost. A ceiling is a stopping rule, not a cap: the session
    /// ends having spent a little more than it, and the line says how much.
    ///
    /// note: a `fork`'s request is counted too, when the call that made it finishes: it is money
    /// the session spent, and a model drafting over and over would otherwise spend without limit
    /// under a ceiling that says there is one.
    pub(super) fn charge(&mut self, usage: Option<Usage>, stop: &StopReason) {
        let usage = usage.filter(|it| it.input_tokens.is_some() || it.output_tokens.is_some());
        // said only where it changes something: with no ceiling, a total nobody set a limit on
        // being short by one response is not news
        let telling = self.spend.is_some() && !self.unreported;
        let Some(usage) = usage else {
            let unfinished = matches!(
                stop,
                StopReason::Other(why) if why == "interrupted" || why == "cut off"
            );
            if telling && !unfinished {
                self.unreported = true;
                self.say(
                    Speaker::Note,
                    "this endpoint reports no usage, so nothing is counted against the ceiling; \
                     only a deadline can stop this session",
                );
            }

            return;
        };
        if telling && (usage.input_tokens.is_none() || usage.output_tokens.is_none()) {
            self.unreported = true;
            self.say(
                Speaker::Note,
                "this endpoint reports only half of what a response cost, so the ceiling is \
                 counted against less than was spent",
            );
        }

        self.count(
            usage
                .input_tokens
                .unwrap_or(0)
                .saturating_add(usage.output_tokens.unwrap_or(0)),
        );
    }

    /// Charges every response the log has recorded since the last one charged.
    ///
    /// note: called on every event rather than on `model.finished` alone, because the event
    /// that follows a lag is the first moment anybody here can know one was lost. That is always
    /// before another request goes out: a lag keeps the latest events and drops the oldest, so
    /// whatever follows the lost response is still on its way.
    pub(super) fn charge_since(&mut self) {
        // the cursor is moved in the same read, or a response recorded between two would be
        // passed over by both
        let (responses, last): (Vec<_>, _) = self.kernel.with_history(|log| {
            let responses = log
                .since(self.charged)
                .filter_map(|record| match &record.event {
                    Event::ModelFinished { usage, stop, .. } => Some((*usage, stop.clone())),
                    _ => None,
                })
                .collect();
            (responses, log.last_seq())
        });
        self.charged = last;
        for (usage, stop) in responses {
            self.charge(usage, &stop);
        }
    }

    /// Adds what a provider charged to what this session has spent, and stops the turn once that
    /// reaches the ceiling; see [`App::charge`].
    pub(super) fn count(&mut self, tokens: u64) {
        // counted whether or not anything is watching the figure. Added up only under a ceiling,
        // a session that set one half way through would begin from zero, and `/spend` would
        // answer `0 tokens spent` after a turn that plainly cost some
        self.spent = self.spent.saturating_add(tokens);
        let Some(limit) = self.spend else {
            return;
        };
        if self.spent < limit || self.overspent {
            return;
        }
        self.overspent = true;
        self.interrupt();
        self.say(
            Speaker::Note,
            format!(
                "spent {} tokens of {}; stopping. `/spend N` raises the ceiling",
                thousands(self.spent as usize),
                thousands(limit as usize)
            ),
        );
    }

    /// What the provider has charged for this session, as the responses have reported it.
    pub fn spent(&self) -> u64 {
        self.spent
    }

    /// The ceiling that stops it, if anything has set one.
    pub fn spend(&self) -> Option<u64> {
        self.spend
    }

    /// Whether the ceiling has been reached, so that nothing more will be sent.
    ///
    /// note: what a loop reads to decide whether to go on handing lines in. The refusal itself is
    /// [`App::start_turn`]'s, so a caller that does not ask still cannot spend anything; this is
    /// for the caller that would rather stop reading than be told `no` once a line.
    pub fn overspent(&self) -> bool {
        self.overspent
    }

    /// Whether the last turn was refused for a request longer than the model takes, and the next
    /// one would be too.
    ///
    /// note: both halves. The refusal alone stays true after `/exclude` or `/limit` has made room,
    /// and the size alone is true before the compactor has had its go - which runs before every
    /// request and may bring it under. A refusal that still stands is the one a message cannot get
    /// past.
    pub fn oversized(&self) -> bool {
        self.oversized && {
            let budget = self.kernel.budget();
            budget.limit.is_some_and(|limit| budget.used() > limit)
        }
    }

    /// Whether a turn is being asked for after the session has spent what it was given, and says
    /// so if it is.
    ///
    /// note: it speaks, because the alternative is a message that goes into the context and is
    /// never sent with nothing on the screen accounting for it - which is the failure the whole
    /// program is against. Said on each attempt rather than once: every line somebody hands in
    /// gets an answer, and the answer is the same one.
    pub(super) fn broke(&mut self) -> bool {
        if !self.overspent {
            return false;
        }
        let spent = thousands(self.spent as usize);
        let limit = thousands(self.spend.unwrap_or_default() as usize);
        self.say(
            Speaker::Note,
            format!(
                "nothing more is being sent: {spent} tokens spent of {limit}. `/spend N` \
                     raises the ceiling, and `/spend 0` takes it away"
            ),
        );

        true
    }

    /// Sets the ceiling, or takes it away, and lets a stopped session carry on under the new one.
    pub fn set_spend(&mut self, limit: Option<u64>) {
        self.spend = limit;
        self.overspent = limit.is_some_and(|limit| self.spent >= limit);
        if self.overspent {
            self.interrupt();
        }
    }
}
