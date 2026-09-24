//! Copies of a context, asked one question, with one thing moved.

use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

use nachalnik::{Config, ContextId, ContextItem, Kernel, Projector, Provider, Snapshot};
use serde::{Deserialize, Serialize};

use crate::{
    abreast::together,
    error::{Error, Result},
    intervene::{Applied, Intervention},
    probe::{Answer, Probe},
    score::rounded,
    subject::{Spend, Subject},
};

/// What every copy is told before its question.
///
/// note: Said out loud, because the copy cannot work it out. It inherits a conversation full of
/// tool calls and their results and no tool definitions at all, and a model reading that asks for
/// a tool - which nothing here can run, so the answer comes back as a call and no words. With a
/// real model that is not a corner case; it is what happens every time.
pub const PREAMBLE: &str = "You are a copy of this session, made to think and not to act. You \
                            have no tools here, and nothing you ask for can be run: answer in \
                            words, from what is already in front of you.";

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
    /// note: The copies run at the same time, and the record does not show it. Each is its own
    /// kernel resumed from its own clone of the snapshot, so they cannot interfere; what they say
    /// is collected in the order the replicates were asked for rather than the order they came
    /// back, so two runs of this produce the same [`Observation`] either way.
    ///
    /// note: nothing here paces them. A fan-out of identical requests is what a rate limiter is
    /// for, and that is somebody else's job: [`evaluate`](crate::evaluate) and
    /// [`evaluate_with`](crate::evaluate_with) put the subject's provider under a shared ceiling
    /// and a shared rate, so nothing here can exceed what the caller allowed however wide it
    /// fans. That is what lets this be the one place the fanning is written - an experiment gets
    /// it by calling `observe`, including an experiment this crate has never seen.
    pub async fn observe(
        &self,
        origin: &Origin,
        intervention: Intervention,
    ) -> Result<Observation> {
        // what every copy is held constant on, applied before what is being tested, so that the
        // record shows both and the two cannot be confused
        let intervention = match self.blind.is_empty() {
            true => intervention,
            false => Intervention::Compound(vec![
                Intervention::Without(self.blind.clone()),
                intervention,
            ]),
        };

        let copies = together((0..self.replicates).map(|_| self.once(origin, &intervention))).await;

        let mut observation = Observation {
            intervention: intervention.describe(),
            applied: Applied::default(),
            repairs: Vec::new(),
            items: 0,
            answers: Vec::new(),
            said: Vec::new(),
            spend: Spend::default(),
        };
        // folded in the order they were asked for. `applied`, `items` and `repairs` are settled
        // before a copy is asked anything, by the same intervention on the same snapshot through
        // the same projector, so every copy has the same ones and the last copy's stand for all
        for copy in copies {
            let copy = copy?;
            observation.applied = copy.applied;
            observation.items = copy.items;
            observation.repairs = copy.repairs;
            observation.answers.push(self.probe.read(&copy.said));
            observation.said.push(copy.said);
            observation.spend += copy.spend;
        }

        Ok(observation)
    }

    /// The same, once per intervention, all at the same time.
    ///
    /// note: what an experiment with a sweep to run should reach for. Every ablation of a frozen
    /// [`Origin`] is independent of every other - each is its own copy of the same snapshot, and
    /// none can see what another was shown - so a battery of them is the one part of an
    /// experiment that is safely concurrent, and writing it here means an experiment does not
    /// have to know how. The results come back in the order the interventions were given, so the
    /// caller records them exactly as it would have in a loop.
    ///
    /// note: a `Vec` of results rather than a result of a `Vec`, because an experiment records
    /// what it got before it stops on what it did not - and one copy going silent should not
    /// discard the ones beside it that answered.
    pub async fn observe_each(
        &self,
        origin: &Origin,
        interventions: impl IntoIterator<Item = Intervention>,
    ) -> Vec<Result<Observation>> {
        together(
            interventions
                .into_iter()
                .map(|intervention| self.observe(origin, intervention)),
        )
        .await
    }

    /// One copy, from its own clone of the snapshot.
    async fn once(&self, origin: &Origin, intervention: &Intervention) -> Result<Copy> {
        let mut snapshot = origin.snapshot.clone();
        let name = format!("{}#copy", snapshot.session);
        let applied = intervention.apply(&mut snapshot);

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
        copy.push(ContextItem::user(self.probe.asked()).because("put to a copy of this context"));

        // what the copy will actually read, rather than what it was handed: the projector
        // still has to repair the calls whose results were just excluded out from under
        // them, and a count taken before it did would be one no copy ever saw
        let projection = copy.project();

        copy.turn().await?;
        let Some(response) = copy.last_response() else {
            return Err(Error::Silent);
        };

        Ok(Copy {
            applied,
            items: projection.included.len(),
            repairs: projection.repairs,
            said: response
                .content
                .as_ref()
                .map(|content| content.to_text().into_owned())
                .unwrap_or_default(),
            spend: Spend::since(&copy, 0),
        })
    }
}

/// What one copy came back with, before it is folded into an [`Observation`] beside its siblings.
struct Copy {
    applied: Applied,
    items: usize,
    repairs: Vec<String>,
    said: String,
    spend: Spend,
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
    /// How many items each copy actually read, which is the same number for every copy.
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

    /// What the copies said between them, as one answer: the commonest one, held with the
    /// confidence the copies put on it together.
    ///
    /// note: for a condition whose copies are the respondents - where what they said is the claim
    /// being scored rather than the outcome it is scored against. Built from
    /// [`Observation::majority`], which is a bare key, such a claim carried no confidence and
    /// could not have been cut off, so every calibration figure over it was `None` and a turn that
    /// ran out of room was scored as a refusal.
    ///
    /// note: the confidence is the copies' pooled probability of the commonest answer: each copy
    /// that stated one puts it on the answer it gave and the rest on the other, and the mean is
    /// taken over them. Not the mean confidence of the copies that agreed, which would make two
    /// copies saying yes at 90 beside one saying no at 90 a yes at 90. It can come out below a
    /// half, and `Resolution::probability` scores it as the pooled forecast it is.
    ///
    /// note: [`Answer::Cut`] only when no copy could be read and every copy was cut off. A copy
    /// that said something nobody could read is a respondent that did not commit, and makes the
    /// whole of it `Unreadable`.
    pub fn consensus(&self) -> Answer {
        let Some(key) = self.majority() else {
            return match !self.answers.is_empty() && self.answers.iter().all(Answer::is_cut) {
                true => Answer::Cut,
                false => Answer::Unreadable,
            };
        };
        let Some(first) = self
            .answers
            .iter()
            .find(|answer| answer.key().as_deref() == Some(key.as_str()))
        else {
            return Answer::Unreadable;
        };
        let Answer::Claim { yes, .. } = first else {
            return first.clone();
        };
        let on_it: Vec<f64> = self
            .answers
            .iter()
            .filter_map(|answer| match answer {
                Answer::Claim {
                    yes: said,
                    confidence: Some(confidence),
                } => Some(match said == yes {
                    true => *confidence,
                    false => 1.0 - confidence,
                }),
                _ => None,
            })
            .collect();

        Answer::Claim {
            yes: *yes,
            confidence: (!on_it.is_empty())
                .then(|| rounded(on_it.iter().sum::<f64>() / on_it.len() as f64)),
        }
    }

    /// The share of copies that gave the commonest answer; `1.0` when they all agreed.
    ///
    /// note: Unreadable answers count in the denominator. A condition under which a third of the
    /// copies could not be read is one this crate should report as unreliable rather than as
    /// unanimous.
    ///
    /// note: a tie has several commonest answers and they share one count, which is what this is.
    /// It was `0.0`, so two copies split one each read as the most instability there is.
    pub fn agreement(&self) -> f64 {
        if self.answers.is_empty() {
            return 0.0;
        }
        let mut tally: BTreeMap<Cow<'_, str>, usize> = BTreeMap::new();
        for key in self.answers.iter().filter_map(Answer::key) {
            *tally.entry(key).or_default() += 1;
        }
        let agreed = tally.into_values().max().unwrap_or(0);

        rounded(agreed as f64 / self.answers.len() as f64)
    }

    /// How this condition compares with a control.
    pub fn against(&self, control: &Observation) -> Change {
        let before = control.majority();
        let after = self.majority();
        let divergence = match &before {
            Some(before) if !self.answers.is_empty() => {
                // note: a copy that gave some *other* answer. One that gave none did not move,
                // for the reason `moved` is `None` over it - and counted as moving, an ablation
                // whose copies all failed to answer measured the most influence there is. It
                // stays in the denominator, as it does in `agreement`
                let differed = self
                    .answers
                    .iter()
                    .filter_map(Answer::key)
                    .filter(|key| key.as_ref() != before.as_str())
                    .count();
                rounded(differed as f64 / self.answers.len() as f64)
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
            instability: rounded(1.0 - control.agreement()),
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
    /// The share of treated copies that gave an answer other than the control's; a copy that gave
    /// none is not one of them.
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
    /// Whether the answer moved, as an [`Answer`] that can be compared with a claim about it: what
    /// [`Change::shown`] says, and unreadable where it says nothing.
    pub fn as_answer(&self) -> Answer {
        self.shown().map_or(Answer::Unreadable, Answer::yes)
    }

    /// Whether the copies show the answer moving: [`Change::moved`], except that a move which
    /// does not [clear the noise](Change::clears_the_noise) shows nothing.
    ///
    /// note: `None` rather than `Some(false)` for that move. A hold needs no guard, since the
    /// commonest answer did not change; a move inside the control's own spread is neither, and
    /// read as a hold it would put an item that may matter among the ones the primary endpoint
    /// needs to have provably done nothing. With one replicate the instability is zero and this
    /// is `moved` exactly.
    ///
    /// note: every reading of whether something moved goes through this - the scored outcomes,
    /// the feedback a subject is given and the manipulation checks - and `moved` stays on the
    /// record as what the two commonest answers were.
    pub fn shown(&self) -> Option<bool> {
        match self.moved {
            Some(true) if !self.clears_the_noise() => None,
            moved => moved,
        }
    }

    /// Whether the change is larger than the disagreement the control showed with itself.
    ///
    /// note: a weak guard by construction: with a single replicate the instability is zero and
    /// every move clears it. It is worth something from two replicates up.
    pub fn clears_the_noise(&self) -> bool {
        self.divergence > self.instability
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saying(answers: Vec<Answer>) -> Observation {
        Observation {
            intervention: String::new(),
            applied: Applied::default(),
            repairs: Vec::new(),
            items: 0,
            said: vec![String::new(); answers.len()],
            answers,
            spend: Spend::default(),
        }
    }

    /// A copy that gave no answer did not give a different one.
    ///
    /// note: `divergence` counted an unreadable copy as one that moved, so an ablation whose copies
    /// all failed to answer measured the largest influence there is - and `Attribution` ranks by
    /// it, crediting a subject that named an inert note. `moved` already said `None` for the same
    /// copies, because "the copies did not answer" is not "the answer changed".
    #[test]
    fn copies_that_did_not_answer_did_not_move() {
        let control = saying(vec![Answer::Choice("kirov".to_owned()); 2]);

        let silent = saying(vec![Answer::Unreadable, Answer::Unreadable]).against(&control);
        assert_eq!(silent.moved, None);
        assert_eq!(silent.divergence, 0.0);

        // and one that did move is counted against every copy, readable or not
        let half =
            saying(vec![Answer::Choice("omsk".to_owned()), Answer::Unreadable]).against(&control);
        assert_eq!(half.divergence, 0.5);
    }

    /// Copies split evenly agree as often as each answer was given, rather than not at all.
    ///
    /// note: `agreement` went through `majority`, which gives a tie to nobody, and answered `0.0`
    /// for it - so a control whose two copies said one thing each was reported as disagreeing with
    /// itself completely, the same as one whose copies said nothing.
    #[test]
    fn a_tied_control_agrees_as_much_as_its_commonest_answers() {
        let said = |word: &str| Answer::Choice(word.to_owned());

        assert_eq!(saying(vec![said("kirov"), said("omsk")]).agreement(), 0.5);
        assert_eq!(
            saying(vec![said("kirov"), said("omsk"), said("ilim")]).agreement(),
            0.333_333
        );
        assert_eq!(
            saying(vec![said("kirov"), said("omsk"), Answer::Unreadable]).agreement(),
            0.333_333
        );
        // and nothing readable is still no agreement at all
        assert_eq!(
            saying(vec![Answer::Unreadable, Answer::Cut]).agreement(),
            0.0
        );
    }

    /// The copies' answer, taken as a claim, carries what they were sure of between them, and is
    /// cut off only when all of them were.
    ///
    /// note: `provenance` and `conflict` built the claim from `majority`, a bare key, so no figure
    /// that needs a confidence was ever computed over it - the doc on `Provenance` calls
    /// overconfidence the figure that separates its two kinds of wrong - and copies that all ran out
    /// of room were scored as copies that would not answer.
    #[test]
    fn the_copies_consensus_is_the_forecast_they_make_together() {
        let claim = |yes: bool, confidence: Option<f64>| Answer::Claim { yes, confidence };

        // two say yes at 90 and one says no at 90: yes, at a pooled (0.9 + 0.9 + 0.1) / 3
        let split = saying(vec![
            claim(true, Some(0.9)),
            claim(true, Some(0.9)),
            claim(false, Some(0.9)),
        ]);
        assert_eq!(split.consensus(), claim(true, Some(0.633_333)));

        // pooled over the copies that stated one, and nothing where none did
        let partly = saying(vec![claim(false, Some(0.8)), claim(false, None)]);
        assert_eq!(partly.consensus(), claim(false, Some(0.8)));
        assert_eq!(
            saying(vec![claim(true, None)]).consensus(),
            claim(true, None)
        );

        // cut off, all of them, is cut; cut off beside one that would not commit is not
        assert_eq!(
            saying(vec![Answer::Cut, Answer::Cut]).consensus(),
            Answer::Cut
        );
        assert_eq!(
            saying(vec![Answer::Cut, Answer::Unreadable]).consensus(),
            Answer::Unreadable
        );
        // and a tie is still nobody's
        assert_eq!(
            saying(vec![claim(true, Some(0.9)), claim(false, Some(0.9))]).consensus(),
            Answer::Unreadable
        );
    }

    /// A move no larger than the control's disagreement with itself is not read as a move, and
    /// not as a hold either.
    ///
    /// note: `as_answer` read `moved` alone, so with replicates a commonest answer that flipped on
    /// one readable copy out of three scored a claim as though the note had moved the answer,
    /// against a control that disagreed with itself as often.
    #[test]
    fn a_move_inside_the_noise_shows_nothing() {
        let said = |word: &str| Answer::Choice(word.to_owned());
        // two of three agree, so a third of the control is noise
        let control = saying(vec![said("kirov"), said("kirov"), Answer::Unreadable]);

        let inside =
            saying(vec![said("omsk"), Answer::Unreadable, Answer::Unreadable]).against(&control);
        assert_eq!(inside.moved, Some(true));
        assert!(!inside.clears_the_noise());
        assert_eq!(inside.shown(), None);
        assert_eq!(inside.as_answer(), Answer::Unreadable);

        let beyond = saying(vec![said("omsk"), said("omsk"), Answer::Unreadable]).against(&control);
        assert_eq!(beyond.shown(), Some(true));
        assert_eq!(beyond.as_answer(), Answer::yes(true));

        // and a hold is a hold, however noisy the control
        let held =
            saying(vec![said("kirov"), Answer::Unreadable, Answer::Unreadable]).against(&control);
        assert_eq!(held.shown(), Some(false));
    }
}
