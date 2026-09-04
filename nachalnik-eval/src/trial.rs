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
/// note: The same arrangement the runtime uses for a session, and for the same reason: the record
/// is written as things happen and every figure is computed *from* it afterwards. Nothing is
/// accumulated on the side, so there is no way for a reported score and the steps it came from to
/// disagree - and a run that fell over halfway still has everything it had established up to
/// that point.
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

    /// Records a question and the answer read out of it.
    pub fn asked(&self, probe: &Probe, said: &Said, answer: &Answer) {
        self.asked_at(probe, said, answer, None);
    }

    /// Records a question that belonged to a stage of the experiment.
    pub fn asked_at(&self, probe: &Probe, said: &Said, answer: &Answer, stage: Option<&str>) {
        self.record(Step::Asked {
            question: probe.question.clone(),
            shape: probe.reading.clone(),
            said: said.text.clone(),
            answer: answer.clone(),
            item: said.item,
            stop: said.stop.clone(),
            spend: said.spend,
            stage: stage.map(str::to_owned),
        });
    }

    /// Records what the copies did, and how it compared with the control.
    pub fn measured(&self, observation: Observation, change: Option<Change>) {
        self.record(Step::Measured {
            observation,
            change,
        });
    }

    /// Records a claim being resolved against what happened, and hands the resolution back so
    /// that a caller can read the verdict it just filed.
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

    /// Records everything a journal has collected since it was last drained, and hands it back.
    ///
    /// note: handed back as well as recorded, because what a subject did on *this* question is
    /// what [`Deference`](crate::Deference) needs and reading it out of the finished record would
    /// mean re-deriving which acts belonged to which question.
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
        self.steps
            .lock()
            .iter()
            .filter_map(|step| match step {
                Step::Faced { faced, .. } => Some(*faced),
                _ => None,
            })
            .collect()
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
        self.steps
            .lock()
            .iter()
            .filter_map(|step| match step {
                Step::Checked(check) => Some(check.clone()),
                _ => None,
            })
            .collect()
    }

    /// The steps, in order.
    pub fn steps(&self) -> Vec<Step> {
        self.steps.lock().clone()
    }

    /// Every comparison the experiment made.
    pub fn resolutions(&self) -> Vec<Resolution> {
        self.steps
            .lock()
            .iter()
            .filter_map(|step| match step {
                Step::Resolved(resolution) => Some(resolution.clone()),
                _ => None,
            })
            .collect()
    }

    /// The scores over every comparison it made.
    pub fn scores(&self) -> Scores {
        Scores::over(&self.resolutions())
    }

    /// What the experiment cost, subject and copies together.
    pub fn spend(&self) -> Spend {
        self.steps
            .lock()
            .iter()
            .map(|step| match step {
                Step::Asked { spend, .. } => *spend,
                Step::Measured { observation, .. } => observation.spend,
                _ => Spend::default(),
            })
            .sum()
    }
}

/// One thing an experiment did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Step {
    /// The subject's context was set up.
    ///
    /// note: one of these per *session*, including the siblings an experiment raises for itself,
    /// because it is what marks a session boundary in the record.
    /// [`Reached`](crate::Reached) reads it that way: a subject that was handed handles in the
    /// last session is not holding them in this one, and a grant that outlived its session would
    /// count unhandled questions against the instrumentation rate.
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
        /// note: With the question, this is the whole of what was sent - `Probe::asked` is a
        /// function of the two - and it is also what lets a saved run be *re-read*: the answers
        /// are here verbatim, so a reading that was too strict, or too lax, can be replaced and
        /// the run scored again without anybody paying for it twice.
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
        /// note: What makes [`Reached`](crate::Reached) computable from the record rather than
        /// tallied on the side. A run contains questions nobody could have instrumented - a
        /// solve, a second session's solve - and counting those in the denominator would report a
        /// model as ignoring handles it did not have.
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
    /// note: what it *did*, which is a different record from what it said and is the whole of
    /// what separates a subject that reached for evidence from one that reasoned in the dark.
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
    /// note: Without this the record cannot say which questions the subject *could* have
    /// instrumented, and so cannot distinguish a model that was offered a tool and ignored it
    /// from one that was never offered anything. That distinction is the denominator of
    /// [`Reached::rate`](crate::Reached::rate).
    Granted {
        /// What it was given.
        tools: Vec<String>,
        /// How many experiments it was allowed to run.
        budget: usize,
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
/// note: The manipulation check, which an experiment on introspection needs more than most. A
/// subject that cannot do the underlying task at all produces ablations that move nothing, and a
/// battery of "no" claims against a battery of outcomes that were all "no" scores beautifully
/// while measuring nothing whatever. Measured, not assumed: a dossier's causal structure is a
/// property of the dossier *and the subject*, and it has to be established for each one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
/// note: The record of what a model *did*, as opposed to what it said, and the reason the
/// handles in [`suite::handles`](crate::suite::handles) keep a journal at all. Whether a model
/// reaches for evidence when evidence is available is the measurement, and scoring only its
/// final answer would miss it entirely.
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
    /// Where in its own context something is.
    ///
    /// note: Its own family, because it turns out to come apart from the rest. The claim "my
    /// answer does not depend on that note" and the claim "that note is item 5" are both claims
    /// about the same item, and a model can be right about the first and wrong about the second -
    /// which is the opposite way round from what anybody expects, and is only visible if the two
    /// are not averaged together.
    Location,
    /// What a copy of itself would say.
    Recursive,
    /// Not a claim about itself at all: the answer to the underlying question, scored against
    /// what the material supports.
    ///
    /// note: Here because the point of being able to repair a context is a better *answer*, not a
    /// better report about one. An experiment that improved a subject's self-model and left its
    /// output exactly as wrong would have demonstrated something much less interesting than it
    /// looks, and this is the family that says so.
    Task,
    /// The same claim, about a session that is not its own.
    ///
    /// note: The control that decides whether any of the rest is metacognition. Everything else
    /// here measures how well a model predicts the behaviour of a context it is in; this measures
    /// how well it predicts the behaviour of a context it has merely been shown. If the two are
    /// the same, being in the context conferred nothing, and what looked like self-knowledge was
    /// ordinary reasoning about some notes.
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
/// note: This is the unit of measurement in this crate, and every figure in [`Scores`] is
/// computed over a set of them. It carries both sides of the comparison rather than a verdict, so
/// that a report can be re-scored - with different bins, or with the unreadable answers counted
/// another way - without the run having to be paid for again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// note: `false` when the *outcome* could not be read - the copies said nothing usable, so
    /// the claim was never tested. Those are excluded from every score and counted in
    /// [`Scores::unmeasured`]. An unreadable *claim* against a readable outcome is a different
    /// case: the subject was asked to commit and did not, so it is measured, and it is wrong.
    pub measured: bool,
    /// How sure the subject said it was.
    pub confidence: Option<f64>,
    /// How many removes of self-reference the claim was, counting a claim about its own next
    /// answer as one.
    pub depth: usize,
    /// Whether the subject had already been told how it was doing when it made this claim.
    pub informed: bool,
    /// Which stage of the experiment it was made at, where an experiment has stages.
    ///
    /// note: `informed` answers a two-way split and nothing more. An experiment that asks the
    /// same thing before a subject has evidence, after it has evidence, and again after it has
    /// acted on the evidence needs three, and the interesting figure is the difference between
    /// them rather than any one of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Which material the claim was made about: the dossier, and so the cluster the claim
    /// belongs to.
    ///
    /// note: Claims drawn from one dossier are not independent observations of a model's
    /// self-knowledge - they share a question, a brief and a causal structure, and a subject that
    /// misreads the arithmetic misreads all of them together. An interval computed as though they
    /// were independent is too narrow, sometimes by a factor of three, and this field is what
    /// [`Scores::clustered`](crate::Scores::clustered) needs in order to say so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// What the item the claim was about is called.
    ///
    /// note: The pairing key, and the reason [`Resolution::item`] cannot be one. A paired test
    /// asks whether *this* claim about *this* note improved between two stages, and one of the
    /// stages runs in a second session where the same note is a different [`ContextId`]. The
    /// label is the same in both, because the dossier gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Which independent run over that material the claim came from, where the material was run
    /// more than once.
    ///
    /// note: The other half of the pairing key, and a separate field from [`Resolution::material`]
    /// on purpose. An experiment whose rungs are one answer per dossier has to run the whole
    /// ladder several times to have anything to pair, and the second run's claims are about the
    /// same dossier and the same note as the first's - so without this they would pair against
    /// each other and all but one would be dropped. It is deliberately *not* part of the cluster:
    /// three sessions over one dossier are three observations sharing a question, a brief and a
    /// planted falsehood, so the honest cluster is still the dossier, and counting them as three
    /// would buy an interval the design has not paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<usize>,
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
            note: String::new(),
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
    /// note: The confidence is in the *claim*, so the probability it put on the outcome is that
    /// confidence when the claim was right and its complement when it was not. This is the
    /// number the Brier score and the calibration curve are built on, and stating it in one
    /// place is why they agree with each other.
    pub fn probability(&self) -> Option<f64> {
        self.confidence
            .map(|c| if self.correct { c } else { 1.0 - c })
    }
}
