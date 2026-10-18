//! The append-only record of one experiment, and the comparisons the scores are computed from.

use std::sync::Arc;

use nachalnik::{ContextId, ModelInfo, Params, StopReason};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{
    experiment::Instrument,
    fork::{Change, Observation},
    probe::{Answer, Probe, Reading},
    score::{Faced, Scores},
    subject::{Said, Spend, Subject},
};

/// Everything one experiment did, in the order it did it.
///
/// note: as with a runtime session, the record is written as things happen and every figure is
/// computed from it afterwards. Nothing is accumulated on the side, so a reported score cannot
/// disagree with the steps it came from, and a run that failed halfway keeps everything established
/// up to that point.
pub struct Trial {
    experiment: String,
    instrument: Instrument,
    model: Option<ModelInfo>,
    params: Params,
    steps: Mutex<Vec<Step>>,
}

impl Trial {
    /// Opens a record for an experiment about to be run on a subject.
    pub fn new(experiment: impl Into<String>, subject: &Subject) -> Self {
        Self {
            experiment: experiment.into(),
            instrument: Instrument::unstated(),
            model: subject.model(),
            // recorded because they change the answers: a benchmark run at one temperature is
            // not comparable with one run at another, and a report that did not say which it was
            // would be inviting the comparison
            params: subject.kernel().params(),
            steps: Mutex::new(Vec::new()),
        }
    }

    /// Records what the experiment is asking with.
    #[must_use]
    pub fn asking(mut self, instrument: Instrument) -> Self {
        self.instrument = instrument;
        self
    }

    /// The experiment this is a record of.
    pub fn experiment(&self) -> &str {
        &self.experiment
    }

    /// What it asked with.
    pub fn instrument(&self) -> &Instrument {
        &self.instrument
    }

    /// What the subject reported itself to be.
    pub fn model(&self) -> Option<&ModelInfo> {
        self.model.as_ref()
    }

    /// The parameters the subject was run with.
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Appends a step.
    pub fn record(&self, step: Step) {
        self.steps.lock().push(step);
    }

    /// Records a question and its answer, and returns where in the record it went, for
    /// [`Resolution::answering`].
    pub fn asked(&self, probe: &Probe, said: &Said, answer: &Answer) -> usize {
        self.asked_at(probe, said, answer, None)
    }

    /// Records a question that belonged to a stage of the experiment, and hands back where in the
    /// record it went.
    pub fn asked_at(
        &self,
        probe: &Probe,
        said: &Said,
        answer: &Answer,
        stage: Option<&str>,
    ) -> usize {
        let mut steps = self.steps.lock();
        steps.push(Step::Asked {
            question: probe.question.clone(),
            shape: probe.reading.clone(),
            said: said.text.clone(),
            answer: answer.clone(),
            item: said.item,
            stop: said.stop.clone(),
            spend: said.spend,
            stage: stage.map(str::to_owned),
        });

        steps.len() - 1
    }

    /// Records what the copies did, and how it compared with the control.
    pub fn measured(&self, observation: Observation, change: Option<Change>) {
        self.record(Step::Measured {
            observation,
            change,
        });
    }

    /// Records a resolution against what happened, and returns it so the caller can read the
    /// verdict just filed.
    pub fn resolve(&self, resolution: Resolution) -> Resolution {
        self.record(Step::Resolved(resolution.clone()));

        resolution
    }

    /// Records something worth keeping that is none of the above.
    pub fn note(&self, note: impl Into<String>) {
        self.record(Step::Noted { note: note.into() });
    }

    /// Records a precondition the experiment tested rather than assumed.
    pub fn check(&self, what: impl Into<String>, held: bool, detail: impl Into<String>) {
        self.record(Step::Checked(Check {
            what: what.into(),
            held,
            detail: detail.into(),
        }));
    }

    /// Everything the subject did with the handles it was given, in order.
    pub fn acts(&self) -> Vec<Act> {
        self.steps
            .lock()
            .iter()
            .filter_map(|step| match step {
                Step::Acted(act) => Some(act.clone()),
                _ => None,
            })
            .collect()
    }

    /// Records everything a journal has collected since it was last drained, and returns it.
    ///
    /// note: returned as well as recorded because [`Deference`](crate::Deference) needs what a
    /// subject did on this question; reading it from the finished record would mean re-deriving
    /// which acts belonged to which question.
    pub fn drain(&self, journal: &Journal) -> Vec<Act> {
        let acts: Vec<Act> = journal.lock().drain(..).collect();
        for act in &acts {
            self.record(Step::Acted(act.clone()));
        }

        acts
    }

    /// Records a claim, the subject's own measurement of it, and what it said afterwards.
    pub fn faced(&self, label: impl Into<String>, material: Option<&str>, faced: Faced) {
        self.record(Step::Faced {
            label: label.into(),
            material: material.map(str::to_owned),
            faced,
        });
    }

    /// Every case where the subject's own evidence bore on its own claim.
    pub fn faceds(&self) -> Vec<Faced> {
        faceds_of(&self.steps.lock())
    }

    /// Records the stages that ask the same claims again, earliest first: the ones that are
    /// paired.
    pub fn ladder(&self, stages: &[&str]) {
        self.record(Step::Ladder {
            stages: stages.iter().map(|stage| (*stage).to_owned()).collect(),
        });
    }

    /// Records that the subject was given handles.
    pub fn granted(&self, tools: &[&str], budget: usize) {
        self.record(Step::Granted {
            tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
            budget,
        });
    }

    /// Every precondition it tested.
    pub fn checks(&self) -> Vec<Check> {
        checks_of(&self.steps.lock())
    }

    /// The steps, in order.
    pub fn steps(&self) -> Vec<Step> {
        self.steps.lock().clone()
    }

    /// Every comparison the experiment made.
    pub fn resolutions(&self) -> Vec<Resolution> {
        resolutions_of(&self.steps.lock())
    }

    /// The scores over every comparison it made.
    pub fn scores(&self) -> Scores {
        Scores::over(&self.resolutions())
    }

    /// What the experiment cost, subject and copies together.
    pub fn spend(&self) -> Spend {
        spend_of(&self.steps.lock())
    }
}

/// Every case in a record where the subject's own evidence bore on its own claim.
pub(crate) fn faceds_of(steps: &[Step]) -> Vec<Faced> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Faced { faced, .. } => Some(*faced),
            _ => None,
        })
        .collect()
}

/// Every precondition a record tested.
pub(crate) fn checks_of(steps: &[Step]) -> Vec<Check> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Checked(check) => Some(check.clone()),
            _ => None,
        })
        .collect()
}

/// Every comparison a record made.
pub(crate) fn resolutions_of(steps: &[Step]) -> Vec<Resolution> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Resolved(resolution) => Some(resolution.clone()),
            _ => None,
        })
        .collect()
}

/// What a record cost, subject and copies together.
pub(crate) fn spend_of(steps: &[Step]) -> Spend {
    steps
        .iter()
        .map(|step| match step {
            Step::Asked { spend, .. } => *spend,
            Step::Measured { observation, .. } => observation.spend,
            Step::Acted(Act::Tested { spend, .. }) => *spend,
            _ => Spend::default(),
        })
        .sum()
}

/// One thing an experiment did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Step {
    /// The subject's context was set up.
    ///
    /// note: one per session, including the siblings an experiment raises for itself, because it
    /// marks a session boundary in the record. [`Reached`](crate::Reached) reads it that way:
    /// handles granted in the last session are not held in this one, and a grant that outlived its
    /// session would count unhandled questions against the instrumentation rate.
    Briefed {
        /// The items that were planted, and what they were called.
        items: Vec<Labelled>,
    },
    /// The subject was asked something and answered.
    Asked {
        /// What it was asked, without the answer-shape instructions.
        question: String,
        /// The shape the answer was asked for in.
        ///
        /// note: with the question, this is all that was sent (`Probe::asked` is a function of the
        /// two). It also lets a saved run be re-read: the answers are verbatim, so a reading that
        /// was too strict or too lax can be replaced and the run scored again without paying twice,
        /// with [`Outcome::reread`](crate::Outcome::reread).
        shape: Reading,
        /// What it said, verbatim.
        ///
        /// note: kept in full, always, and not only when the reading failed. Every score in a
        /// report is a comparison of parsed answers, and the raw text is the only thing anybody
        /// can check the parsing against.
        said: String,
        /// What was read out of it.
        answer: Answer,
        /// The context item the turn was recorded as.
        item: ContextId,
        /// Why it stopped.
        stop: StopReason,
        /// What the exchange cost.
        spend: Spend,
        /// Which stage of the experiment the question belonged to, where it belonged to one.
        ///
        /// note: this makes [`Reached`](crate::Reached) computable from the record rather than
        /// tallied on the side. A run contains questions nobody could have instrumented (a solve, a
        /// second session's solve), and counting those in the denominator would report a model as
        /// ignoring handles it did not have.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<String>,
    },
    /// Copies were run under one condition.
    Measured {
        /// What they said.
        observation: Observation,
        /// How it compared with the control, where there was one to compare with.
        change: Option<Change>,
    },
    /// A claim was compared with what happened.
    Resolved(Resolution),
    /// A precondition was tested.
    Checked(Check),
    /// The subject used one of the handles it was given.
    ///
    /// note: what it did, a different record from what it said, and what separates a subject that
    /// reached for evidence from one that did not.
    Acted(Act),
    /// The subject's own experiment bore on a claim it had already made.
    ///
    /// note: the three readings rather than the verdict, so that
    /// [`Deference`](crate::Deference) is arithmetic over the record like every other figure
    /// here, and a saved run can be re-scored without being re-paid for.
    Faced {
        /// Which note it was about.
        label: String,
        /// Which material the note belonged to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        material: Option<String>,
        /// What it claimed, what its test showed, and what it claimed afterwards.
        faced: Faced,
    },
    /// The subject was given handles onto its own context.
    ///
    /// note: without this, the record cannot say which questions the subject could have
    /// instrumented, so it cannot tell a model that ignored a tool from one never offered one. That
    /// distinction is the denominator of [`Reached::rate`](crate::Reached::rate).
    Granted {
        /// What it was given.
        tools: Vec<String>,
        /// How many experiments it was allowed to run.
        budget: usize,
    },
    /// The stages that put the same claims to the same respondent again, in the order they are
    /// climbed.
    ///
    /// note: [`Outcome::paired`](crate::Outcome::paired) pairs these stages and no others. A stage
    /// pair is a paired contrast only when one subject answers one claim twice. `conflict`'s three
    /// stages are the subject's own unprompted claim and two sets of copies, one with the opposite
    /// truth, so they are not a ladder; pairing every stage an experiment had once gave such pairs
    /// a McNemar test that did not apply. It is declared in the record rather than on
    /// [`Experiment`](crate::Experiment), so the run and any later re-read of what it saved pair
    /// the same declared stages.
    Ladder {
        /// The stages, earliest first.
        stages: Vec<String>,
    },
    /// The subject was told how it had been doing.
    Told {
        /// What it was told, verbatim.
        feedback: String,
    },
    /// Something else worth keeping.
    Noted {
        /// The note.
        note: String,
    },
}

/// A precondition an experiment tested rather than assumed.
///
/// note: this is the manipulation check. A subject that cannot do the underlying task produces
/// ablations that move nothing, and a battery of "no" claims against outcomes that were all "no"
/// scores well while measuring nothing. A dossier's causal structure is a property of the dossier
/// *and the subject*, so it has to be established for each pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Check {
    /// What was being checked, in a few words.
    pub what: String,
    /// Whether it held.
    pub held: bool,
    /// The figures behind the verdict.
    pub detail: String,
}

/// One thing a subject did with the handles it was given.
///
/// note: this records what a model did, not what it said, which is why the handles in
/// [`suite::handles`](crate::suite::handles) keep a journal. Whether a model reaches for evidence
/// when it is available is the measurement, and scoring only the final answer would miss it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Act {
    /// It listed its own context.
    Looked {
        /// How many items it was shown.
        items: usize,
    },
    /// It ran an experiment on a copy of itself.
    Tested {
        /// What it took out of the second copy.
        without: Vec<ContextId>,
        /// What the untouched copy answered.
        before: Option<String>,
        /// What the treated copy answered.
        after: Option<String>,
        /// Whether the two differed.
        moved: Option<bool>,
        /// What the copies cost.
        ///
        /// note: in [`Trial::spend`], beside the subject's own requests and the harness's copies.
        /// The untouched copy is run once and kept, so it is in the first test's spend and not
        /// in the ones after it.
        #[serde(default)]
        spend: Spend,
        /// Why the copies could not be run, where they could not.
        ///
        /// note: journaled all the same: the subject reached for the handle and a test came off its
        /// budget, and a record without it would show a question the subject declined to
        /// instrument. It counts as a test in [`Reached`](crate::Reached) and shows nothing about
        /// any item.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failed: Option<String>,
    },
    /// It took items out of its own context.
    Excluded {
        /// Which.
        ids: Vec<ContextId>,
        /// Why, in its own words.
        reason: String,
    },
    /// It made an item say something else.
    Revised {
        /// Which.
        id: ContextId,
        /// What it used to say.
        was: String,
        /// What it says now.
        now: String,
        /// Why, in its own words.
        reason: String,
    },
    /// It asked for something it was not allowed to have.
    Refused {
        /// What it asked for.
        what: String,
        /// Why it was refused.
        why: String,
    },
}

/// What the handles have been used for, shared with whoever installed them.
pub type Journal = Arc<Mutex<Vec<Act>>>;

/// A context item and what the experiment calls it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Labelled {
    /// The item.
    pub id: ContextId,
    /// Its label.
    pub label: String,
}

/// What a claim was about, which is what decides which score it lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Kind {
    /// Whether moving something would change its answer.
    Counterfactual,
    /// Which of the things it is carrying its answer rests on.
    Attribution,
    /// Where something is in its own context.
    ///
    /// note: a family of its own because it comes apart from the rest: "my answer does not depend
    /// on that note" and "that note is item 5" are about the same item, and a model can be right
    /// about the first and wrong about the second. Averaging the two would hide that.
    Location,
    /// What a copy of itself would say.
    Recursive,
    /// Whether what it is reading is the whole of what happened.
    ///
    /// note: a family of its own for the same reason as [`Kind::Location`]. Every other claim is
    /// answerable from the record the subject holds, and the question is whether the model reads it
    /// well. A provenance claim is about whether the record is whole, and a context with something
    /// taken out of it supports the wrong answer. Averaged with the others, it would look like the
    /// model got worse at reasoning about notes, when the notes changed.
    Provenance,
    /// Whether the things it is carrying agree with each other.
    ///
    /// note: not [`Kind::Provenance`]. Provenance is about whether the record is whole: something
    /// was taken out, and what is left supports the wrong answer. This is about whether it hangs
    /// together: nothing was taken out, and what is there supports two answers at once. The first
    /// is noticing an absence, the second a collision, and a model can be good at one and poor at
    /// the other. Averaged into one figure, they would report a model that got worse at reading
    /// notes.
    Consistency,
    /// Not a claim about itself: the answer to the underlying question, scored against what the
    /// material supports.
    ///
    /// note: repairing a context should give a better answer, not only a better report about one.
    /// An experiment that improved a subject's self-model but left its output wrong would show less
    /// than it seems, and this family shows that.
    Task,
    /// The same claim, about a session that is not its own.
    ///
    /// note: the control that decides whether any of the rest is metacognition. Everything else
    /// measures how well a model predicts the behaviour of a context it is in; this measures how
    /// well it predicts one it has only been shown. If the two are the same, being in the context
    /// added nothing, and what looked like self-knowledge was ordinary reasoning about some notes.
    Foreign,
}

impl Kind {
    /// The name of the family, e.g. `counterfactual`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Counterfactual => "counterfactual",
            Self::Attribution => "attribution",
            Self::Location => "location",
            Self::Recursive => "recursive",
            Self::Provenance => "provenance",
            Self::Consistency => "consistency",
            Self::Foreign => "foreign",
            Self::Task => "task",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One claim, beside what actually happened.
///
/// note: the unit of measurement in this crate; every figure in [`Scores`] is computed over a set
/// of them. It carries both sides of the comparison rather than a verdict, so a report can be
/// re-scored (with different bins, or with unreadable answers counted another way) without paying
/// for the run again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Resolution {
    /// Which family of claim this is.
    pub about: Kind,
    /// The item the claim was about, where it was about one.
    pub item: Option<ContextId>,
    /// What the subject said.
    pub claimed: Answer,
    /// What was observed.
    pub happened: Answer,
    /// Whether the two agree.
    pub correct: bool,
    /// Whether there was anything to be right or wrong about.
    ///
    /// note: `false` when the outcome could not be read: the copies said nothing usable, so the
    /// claim was never tested. Those are excluded from every score and counted in
    /// [`Scores::unmeasured`]. An unreadable claim against a readable outcome is a different case:
    /// the subject was asked to commit and did not, so it is measured, and it is wrong.
    pub measured: bool,
    /// How sure the subject said it was.
    pub confidence: Option<f64>,
    /// How many removes of self-reference the claim was, counting a claim about its own next
    /// answer as one.
    pub depth: usize,
    /// Whether the subject had already been told how it was doing when it made this claim.
    pub informed: bool,
    /// The stage of the experiment it was made at, where an experiment has stages.
    ///
    /// note: `informed` answers a two-way split. An experiment that asks the same thing before a
    /// subject has evidence, after it has evidence, and after it has acted on the evidence needs
    /// three, and the interesting figure is the difference between them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// The material the claim was made about: the dossier, which is the cluster the claim belongs
    /// to.
    ///
    /// note: claims drawn from one dossier are not independent: they share a question, a brief and
    /// a causal structure, and a subject that misreads the arithmetic misreads all of them. An
    /// interval computed as though they were independent is too narrow, and
    /// [`Scores::clustered`](crate::Scores::clustered) needs this field to say so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// The name of the item the claim was about.
    ///
    /// note: the pairing key, and the reason [`Resolution::item`] cannot be one. A paired test asks
    /// whether this claim about this note improved between two stages, and one of the stages runs
    /// in a second session where the same note has a different [`ContextId`]. The label, given by
    /// the dossier, is the same in both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The independent run over that material the claim came from, where the material was run more
    /// than once.
    ///
    /// note: the other half of the pairing key, kept apart from [`Resolution::material`]. An
    /// experiment with one answer per dossier has to run several times to have anything to pair,
    /// and the second run's claims are about the same dossier and note as the first's; without this
    /// they would pair against each other and all but one would be dropped. It is not part of the
    /// cluster: three sessions over one dossier share a question, a brief and a planted falsehood,
    /// so the cluster is still the dossier, and counting them as three would give an interval the
    /// design does not support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<usize>,
    /// Where in the record the claim was read: the index of that [`Step::Asked`] among the
    /// outcome's steps.
    ///
    /// note: lets [`Outcome::reread`](crate::Outcome::reread) carry a new reading of an answer into
    /// the claim made of it. Left empty where the claim is worked out from an answer (a label
    /// mapped to the note it names, or several answers put together), because a new reading of the
    /// words is not a new reading of that, and a claim linked to its answer would be replaced by
    /// the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<usize>,
    /// What was compared with what, in one line.
    pub note: String,
}

impl Resolution {
    /// Compares a claim with what happened.
    pub fn new(about: Kind, claimed: Answer, happened: Answer) -> Self {
        // a claim that never arrived is not a claim: see `Answer::Cut`
        let measured = happened.is_readable() && !claimed.is_cut();

        Self {
            about,
            item: None,
            confidence: claimed.confidence(),
            correct: measured && claimed.agrees_with(&happened),
            claimed,
            happened,
            measured,
            depth: 1,
            informed: false,
            stage: None,
            material: None,
            label: None,
            session: None,
            asked: None,
            note: String::new(),
        }
    }

    /// Names the question the claim is the answer to, by where [`Trial::asked`] recorded it.
    #[must_use]
    pub fn answering(mut self, step: usize) -> Self {
        self.asked = Some(step);
        self
    }

    /// The same comparison, with the claim read again.
    ///
    /// note: what [`Resolution::new`] works out from the claim is worked out again - whether it was
    /// measured, whether it was right, how sure it was - and nothing else is touched.
    pub(crate) fn reclaimed(self, claimed: Answer) -> Self {
        let measured = self.happened.is_readable() && !claimed.is_cut();

        Self {
            confidence: claimed.confidence(),
            correct: measured && claimed.agrees_with(&self.happened),
            measured,
            claimed,
            ..self
        }
    }

    /// Names the item the claim was about.
    #[must_use]
    pub fn about_item(mut self, id: ContextId) -> Self {
        self.item = Some(id);
        self
    }

    /// Names the material the claim was made about, which is the cluster it is counted in.
    #[must_use]
    pub fn on_material(mut self, material: impl Into<String>) -> Self {
        self.material = Some(material.into());
        self
    }

    /// Names the item the claim was about by its label, which is what pairs it across stages.
    #[must_use]
    pub fn about_note(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Numbers the run over the material the claim came from, where the material is run repeatedly.
    #[must_use]
    pub fn in_session(mut self, session: usize) -> Self {
        self.session = Some(session);
        self
    }

    /// Sets how many removes of self-reference the claim was.
    #[must_use]
    pub fn at_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// Marks the claim as one the subject made after being told how it was doing.
    #[must_use]
    pub fn informed(mut self, informed: bool) -> Self {
        self.informed = informed;
        self
    }

    /// Names the stage of the experiment the claim was made at.
    #[must_use]
    pub fn at_stage(mut self, stage: impl Into<String>) -> Self {
        self.stage = Some(stage.into());
        self
    }

    /// Says what was compared with what.
    #[must_use]
    pub fn because(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }

    /// The probability the subject put on what actually happened.
    ///
    /// note: Confidence is in the claim, so the probability is that confidence when the claim was
    /// right and its complement when it was not. The Brier score and calibration curve are built on
    /// this number, so stating it once is why they agree.
    pub fn probability(&self) -> Option<f64> {
        self.confidence
            .map(|c| if self.correct { c } else { 1.0 - c })
    }
}

#[cfg(test)]
mod tests {
    use nachalnik::{Config, Kernel};

    use super::*;

    /// What a run cost includes the copies the subject's own tests ran.
    ///
    /// note: `Act::Tested` carried no spend and `Trial::spend` summed only the subject's requests
    /// and the harness's copies, so an instrumented run was reported as costing less than it was
    /// billed - by every test the subject chose to run.
    #[test]
    fn a_test_the_subject_ran_is_part_of_what_the_run_cost() {
        let trial = Trial::new(
            "instrumented",
            &Subject::new(Kernel::new(Config::default())),
        );
        let spent = |requests: usize| Spend {
            requests,
            ..Spend::default()
        };
        trial.record(Step::Acted(Act::Tested {
            without: vec![ContextId(2)],
            before: None,
            after: None,
            moved: None,
            spend: spent(2),
            failed: None,
        }));
        trial.record(Step::Acted(Act::Looked { items: 7 }));

        assert_eq!(trial.spend().requests, 2);
    }

    /// A family is named by its name, and shown by it too.
    #[test]
    fn a_family_is_named_and_shown_by_its_name() {
        assert_eq!(Kind::Counterfactual.name(), "counterfactual");
        assert_eq!(Kind::Recursive.to_string(), "recursive");
    }

    /// The record keeps the parameters the subject was run with, and every comparison made in it.
    #[test]
    fn a_trial_keeps_its_parameters_and_its_comparisons() {
        let kernel = Kernel::new(Config::default());
        let params = serde_json::json!({ "temperature": 0.25 })
            .as_object()
            .expect("an object")
            .clone();
        kernel.set_params(params.clone());
        let trial = Trial::new("kept", &Subject::new(kernel));
        assert_eq!(trial.params(), &params);

        let resolution =
            Resolution::new(Kind::Counterfactual, Answer::yes(true), Answer::yes(true));
        trial.record(Step::Resolved(resolution.clone()));
        assert_eq!(trial.resolutions(), [resolution]);
        assert_eq!(trial.scores().n, 1);
    }

    /// The probability a claim put on what happened is its confidence when it was right and the
    /// rest when it was wrong.
    #[test]
    fn a_wrong_claim_put_the_rest_of_its_confidence_on_what_happened() {
        let sure = |yes| Answer::Claim {
            yes,
            confidence: Some(0.75),
        };
        let right = Resolution::new(Kind::Counterfactual, sure(true), Answer::yes(true));
        let wrong = Resolution::new(Kind::Counterfactual, sure(false), Answer::yes(true));
        assert_eq!(right.probability(), Some(0.75));
        assert_eq!(wrong.probability(), Some(0.25));
    }

    /// A claim taken again works out afresh how sure it was and whether it was measured.
    #[test]
    fn a_claim_taken_again_is_measured_and_weighed_afresh() {
        let first = Resolution::new(
            Kind::Counterfactual,
            Answer::Claim {
                yes: true,
                confidence: Some(0.75),
            },
            Answer::yes(true),
        );
        assert!(
            first.measured && first.confidence == Some(0.75),
            "{first:?}"
        );

        let again = first.reclaimed(Answer::Cut);
        assert!(
            !again.measured,
            "a claim that never arrived is not measured: {again:?}"
        );
        assert_eq!(again.confidence, None, "nor sure of anything: {again:?}");
    }
}
