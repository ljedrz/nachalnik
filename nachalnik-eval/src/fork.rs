//! Copies of a context, asked one question, with one thing moved.

use std::{collections::BTreeMap, sync::Arc};

use nachalnik::{Config, ContextId, ContextItem, Kernel, Projector, Provider, Snapshot};
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    intervene::{Applied, Intervention},
    probe::{Answer, Probe},
    subject::{Spend, Subject},
};

/// What every copy is told before its question.
///
/// note: Said out loud, because the copy cannot work it out. It inherits a conversation full of
/// tool calls and their results and no tool definitions at all, and a model reading that asks for
/// a tool - which nothing here can run, so the answer comes back as a call and no words. Measured
/// against a real model that is not a corner case; it is what happens every time.
pub const PREAMBLE: &str = "You are a copy of this session, made to think and not to act. You \
                            have no tools here, and nothing you ask for can be run: answer in \
                            words, from what is already in front of you.";

/// A share, rounded the way every figure in a report is; see `score::PLACES`.
fn round(share: f64) -> f64 {
    (share * 1e6).round() / 1e6
}

/// The point every copy is made from.
///
/// note: A frozen context, and it is frozen on purpose rather than read from the live session
/// each time. The claims an experiment elicits go into the subject's context as it makes them, so
/// a copy taken afterwards would be a copy that has read the subject's own answer and can agree
/// with it - which is a measurement of nothing. One origin, taken before anything is asked, is
/// what makes the copies comparable with each other and independent of the claims being scored.
pub struct Origin {
    snapshot: Snapshot,
    provider: Arc<dyn Provider>,
    projector: Arc<dyn Projector>,
}

impl Origin {
    /// Freezes a subject's context as it stands.
    pub fn of(subject: &Subject) -> Result<Self> {
        let kernel = subject.kernel();

        Ok(Self {
            snapshot: kernel.snapshot(),
            provider: kernel.provider().ok_or(nachalnik::Error::NoProvider)?,
            // the same projector, because a copy projected a different way is not a copy: the
            // shape of the request is part of what is being held constant
            projector: kernel.projector(),
        })
    }

    /// The context every copy starts from.
    pub fn items(&self) -> &[ContextItem] {
        &self.snapshot.items
    }

    /// The context as a snapshot, for an experiment that wants to look at it before moving
    /// anything.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
}

/// One question, put to copies of a context with one thing moved at a time.
///
/// note: The question is fixed and the intervention varies, which is the only arrangement in
/// which the answers mean anything: two copies asked two different questions differ for two
/// reasons, and nothing in the result says which.
#[derive(Debug, Clone)]
pub struct Ablation {
    probe: Probe,
    replicates: usize,
    preamble: String,
    blind: Vec<ContextId>,
}

impl Ablation {
    /// Builds an ablation around the question every copy will be asked.
    pub fn new(probe: Probe) -> Self {
        Self {
            probe,
            replicates: 1,
            preamble: PREAMBLE.to_owned(),
            blind: Vec::new(),
        }
    }

    /// How many copies each condition gets; one by default.
    ///
    /// note: One is enough to say what a copy answered and not enough to say what a copy
    /// answers. Two or three make [`Change::instability`] a real figure - the share of control
    /// copies that disagreed with their own majority - and that figure is the noise floor a
    /// single flipped answer has to clear before it means anything. It is also what multiplies
    /// the cost of a run, which is why the default is the honest small number rather than a
    /// respectable-looking large one.
    #[must_use]
    pub fn replicates(mut self, replicates: usize) -> Self {
        self.replicates = replicates.max(1);
        self
    }

    /// What each copy is told before the question.
    #[must_use]
    pub fn preamble(mut self, preamble: impl Into<String>) -> Self {
        self.preamble = preamble.into();
        self
    }

    /// Items every copy is made without - the treated ones and the control alike.
    ///
    /// note: For holding a nuisance constant rather than for testing one, and the nuisance it
    /// exists for is sharp. A copy taken after the session answered can *read that answer* a few
    /// items above the question it is being asked again, so "the answer did not change" may be a
    /// copy agreeing with itself rather than a context determining an answer. Leaving the whole
    /// exchange out of every copy makes each of them answer the question once, from the material,
    /// which is what an ablation is supposed to compare.
    ///
    /// note: it belongs here rather than in the [`Intervention`] because it is *not* the
    /// intervention: it applies identically to both arms and so cannot be what moved the answer.
    /// A caller who wants to know what the exchange itself is worth should ablate it as a
    /// treatment instead, against a control that keeps it.
    #[must_use]
    pub fn blind_to(mut self, ids: impl IntoIterator<Item = ContextId>) -> Self {
        self.blind = ids.into_iter().collect();
        self
    }

    /// What every copy is made without.
    pub fn blind(&self) -> &[ContextId] {
        &self.blind
    }

    /// The question the copies are asked.
    pub fn probe(&self) -> &Probe {
        &self.probe
    }

    /// Runs the copies under one condition and reads what they said.
    ///
    /// note: The copies run one after another, not at once. Not for the runtime's reason - these
    /// are separate kernels and could not interfere with each other - but for the provider's: a
    /// fan-out of identical requests is what a rate limiter is for, and a run whose figures came
    /// back differently depending on how many retries the third replicate needed would be
    /// measuring somebody's traffic policy.
    pub async fn observe(
        &self,
        origin: &Origin,
        intervention: Intervention,
    ) -> Result<Observation> {
        let mut observation = Observation {
            intervention: intervention.describe(),
            applied: Applied::default(),
            repairs: Vec::new(),
            items: 0,
            answers: Vec::new(),
            said: Vec::new(),
            spend: Spend::default(),
        };

        // what every copy is held constant on, applied before what is being tested, so that the
        // record shows both and the two cannot be confused
        let intervention = match self.blind.is_empty() {
            true => intervention,
            false => Intervention::Compound(vec![
                Intervention::Without(self.blind.clone()),
                intervention,
            ]),
        };
        observation.intervention = intervention.describe();

        for _ in 0..self.replicates {
            let mut snapshot = origin.snapshot.clone();
            let name = format!("{}#copy", snapshot.session);
            observation.applied = intervention.apply(&mut snapshot);

            let copy = Kernel::resume(
                Config {
                    session_name: Some(name),
                    // it answers once and is thrown away: there is nothing for an undo stack to
                    // be for, and nothing after the first request for a second one to build on
                    context_undo_depth: 0,
                    max_requests_per_turn: Some(1),
                    ..Config::default()
                },
                snapshot,
            );
            copy.set_provider(origin.provider.clone());
            copy.set_projector(origin.projector.clone());
            // note: no tools, and that is the whole of the isolation. A copy that could run a
            // command could go and find out what the answer is, and a measurement of what a
            // context supports would become a measurement of what a shell can reach.
            copy.push(ContextItem::system(self.preamble.clone()).pinned());
            copy.push(
                ContextItem::user(self.probe.asked()).because("put to a copy of this context"),
            );

            // what the copy will actually read, rather than what it was handed: the projector
            // still has to repair the calls whose results were just excluded out from under
            // them, and a count taken before it did would be one no copy ever saw
            let projection = copy.project();
            observation.items = projection.included.len();
            observation.repairs = projection.repairs;

            copy.turn().await?;
            let Some(response) = copy.last_response() else {
                return Err(Error::Silent);
            };
            let said = response
                .content
                .as_ref()
                .map(|content| content.to_text().into_owned())
                .unwrap_or_default();

            observation.answers.push(self.probe.read(&said));
            observation.said.push(said);
            observation.spend += Spend::since(&copy, 0);
        }

        Ok(observation)
    }
}

/// What the copies said under one condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// What was moved, in words.
    pub intervention: String,
    /// What the intervention actually did.
    pub applied: Applied,
    /// What the projector had to repair to keep the request valid.
    ///
    /// note: On the record because it is a confound, and the only one the harness can see.
    /// Excluding a tool result takes its call with it, so an ablation of one item can remove two
    /// messages; an experiment whose answer moved and whose repairs list is not empty has not
    /// measured what it thinks it measured.
    pub repairs: Vec<String>,
    /// How many items each copy actually read.
    pub items: usize,
    /// What each copy answered.
    pub answers: Vec<Answer>,
    /// What each copy said, verbatim.
    pub said: Vec<String>,
    /// What the copies cost.
    pub spend: Spend,
}

impl Observation {
    /// The answer more of the copies gave than any other, where any of them could be read.
    ///
    /// note: A plurality rather than a majority, and ties go to nobody: two copies that
    /// disagreed have no answer between them, and inventing one would turn a coin toss into a
    /// finding. [`Observation::agreement`] is how much of a plurality it was.
    pub fn majority(&self) -> Option<String> {
        let mut tally: BTreeMap<String, usize> = BTreeMap::new();
        for answer in &self.answers {
            if let Some(key) = answer.key() {
                *tally.entry(key.into_owned()).or_default() += 1;
            }
        }

        let best = tally.values().copied().max()?;
        let mut leaders = tally.into_iter().filter(|(_, count)| *count == best);
        let leader = leaders.next()?;

        leaders.next().is_none().then_some(leader.0)
    }

    /// The share of copies that gave the commonest answer; `1.0` when they all agreed.
    ///
    /// note: Unreadable answers count in the denominator. A condition under which a third of the
    /// copies could not be read is one this crate should report as unreliable rather than as
    /// unanimous.
    pub fn agreement(&self) -> f64 {
        if self.answers.is_empty() {
            return 0.0;
        }
        let Some(majority) = self.majority() else {
            return 0.0;
        };
        let agreed = self
            .answers
            .iter()
            .filter(|answer| answer.key().as_deref() == Some(majority.as_str()))
            .count();

        round(agreed as f64 / self.answers.len() as f64)
    }

    /// How this condition compares with a control.
    pub fn against(&self, control: &Observation) -> Change {
        let before = control.majority();
        let after = self.majority();
        let divergence = match &before {
            Some(before) if !self.answers.is_empty() => {
                let differed = self
                    .answers
                    .iter()
                    .filter(|answer| answer.key().as_deref() != Some(before.as_str()))
                    .count();
                round(differed as f64 / self.answers.len() as f64)
            }
            _ => 0.0,
        };

        Change {
            moved: before
                .as_ref()
                .zip(after.as_ref())
                .map(|(before, after)| before != after),
            before,
            after,
            divergence,
            instability: round(1.0 - control.agreement()),
        }
    }
}

/// What one intervention did to the answer, measured against a control.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// What the control copies answered.
    pub before: Option<String>,
    /// What the treated copies answered.
    pub after: Option<String>,
    /// Whether the answer moved; `None` when either side could not be read, because "the copies
    /// did not answer" is not the same finding as "the answer held".
    pub moved: Option<bool>,
    /// The share of treated copies that differed from the control's answer.
    ///
    /// note: the continuous version of [`Change::moved`], and the one worth ranking items by:
    /// with three replicates an item that moved two copies out of three is doing more work than
    /// one that moved one, and a binary verdict cannot tell them apart.
    pub divergence: f64,
    /// The share of *control* copies that differed from their own commonest answer: the noise
    /// this change was measured against.
    ///
    /// note: `0.0` with one replicate, which is not a claim that the answer is stable - it is
    /// the honest figure for "nothing was measured". See [`Ablation::replicates`].
    pub instability: f64,
}

impl Change {
    /// Whether the answer moved, as an [`Answer`] that can be compared with a claim about it.
    pub fn as_answer(&self) -> Answer {
        self.moved.map_or(Answer::Unreadable, Answer::yes)
    }

    /// Whether the change is larger than the disagreement the control showed with itself.
    ///
    /// note: The one guard this crate offers against reading noise as an effect, and it is a weak
    /// one by construction: with a single replicate the instability is zero and everything clears
    /// it. It is worth something from two replicates up.
    pub fn clears_the_noise(&self) -> bool {
        self.divergence > self.instability
    }
}
