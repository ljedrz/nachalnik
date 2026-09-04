//! What an experiment is, and what running a set of them produces.

use std::{
    collections::BTreeMap,
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use nachalnik::{ModelInfo, Params};
use serde::{Deserialize, Serialize};

use crate::{
    error::Result,
    score::{Deference, Depths, Family, Gain, Paired, Reached, Scores, Stage, Surface},
    subject::{Spend, Subject},
    trial::{Check, Step, Trial},
};

/// What identifies the material an experiment used, so that two runs are known to be comparable
/// or known not to be.
///
/// note: The thing a benchmark of this kind most easily gets wrong, and the reason it is a field
/// on every [`Outcome`] rather than a line in somebody's notes: a question edited between two
/// runs makes them two measurements, and nothing in a score shows it. The [`Instrument::digest`]
/// is computed from the exact text the experiment says, so a bumped version is a claim anybody can
/// check and a *forgotten* bump is caught anyway.
///
/// note: An experiment that states nothing gets [`Instrument::unstated`], and the report says so
/// rather than pretending. That is the honest answer for a third-party experiment this crate has
/// never seen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instrument {
    /// The version of the material, as its author states it.
    pub version: String,
    /// What the material is called.
    pub material: Vec<String>,
    /// A fingerprint of the exact text, as hex.
    pub digest: String,
}

impl Instrument {
    /// Names a version and the material, and fingerprints every sentence of it.
    pub fn of<'a>(
        version: impl Into<String>,
        material: impl IntoIterator<Item = &'a str>,
        text: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for piece in text {
            for byte in piece.as_ref().as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            // a separator, so that two pieces cannot be rearranged into the same digest
            hash ^= 0xff;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }

        Self {
            version: version.into(),
            material: material.into_iter().map(str::to_owned).collect(),
            digest: format!("{hash:016x}"),
        }
    }

    /// The identity of an experiment that does not state one.
    ///
    /// note: also the [`Default`], which is what lets a report written before this field existed
    /// still be read back - see the `serde(default)` on [`Outcome::instrument`]. A record that
    /// cannot be re-read is not a record, and that includes records written by an older version
    /// of the thing reading them.
    ///
    /// note: FNV-1a, which is neither cryptographic nor meant to be: what is wanted is that a
    /// changed question changes the number, deterministically, on every platform and every
    /// compiler, forever. `DefaultHasher` is documented as giving none of those guarantees across
    /// Rust versions, which would make a digest incomparable with one taken last year - the exact
    /// failure this field exists to prevent.
    pub fn unstated() -> Self {
        Self::default()
    }

    /// Whether the experiment said what it was asking.
    pub fn is_stated(&self) -> bool {
        !self.digest.is_empty()
    }
}

impl fmt::Display for Instrument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_stated() {
            return f.write_str("an unstated instrument");
        }
        write!(f, "v{}", self.version)?;
        if !self.material.is_empty() {
            write!(f, "/{}", self.material.join("+"))?;
        }

        write!(f, " #{}", self.digest)
    }
}

/// One measurement, from setting a subject up to filing the last comparison.
///
/// note: An experiment is given a [`Subject`] and a [`Trial`] and nothing else: the subject is
/// what it may ask, and the trial is where what it finds out goes. It returns no figures, because
/// figures are computed from the record afterwards - an experiment that returned its own score
/// could return one the record does not support.
#[async_trait]
pub trait Experiment: Send + Sync {
    /// What this experiment is called.
    fn name(&self) -> &str;

    /// What it measures, in one line.
    fn about(&self) -> &str {
        ""
    }

    /// What identifies the material it asks with.
    ///
    /// note: Defaulted to [`Instrument::unstated`] so that a third-party experiment compiles
    /// without it, and reported as unstated rather than as nothing, because "these two runs may
    /// or may not have been asked the same thing" is information.
    fn instrument(&self) -> Instrument {
        Instrument::unstated()
    }

    /// The question templates it puts to a subject.
    ///
    /// note: the same list [`Experiment::instrument`] fingerprints, so the two cannot drift: a
    /// template missing from here is missing from the digest, and the digest tests would catch
    /// it. Declared separately because a question's *text* decides what a subject needs in order
    /// to answer it, and that is checkable before a run rather than after - see
    /// `a_question_that_needs_an_address_comes_with_a_way_to_look` in `tests/machinery.rs`.
    fn asks(&self) -> &'static [&'static str] {
        &[]
    }

    /// Runs it.
    async fn run(&self, subject: &Subject, trial: &Trial) -> Result<()>;
}

/// Everything one experiment found out about one subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    /// The experiment.
    pub experiment: String,
    /// What it asked with.
    ///
    /// note: `serde(default)` so that a report written before instruments were recorded still
    /// reads back, as an unstated one. Which is the honest answer for it: nobody wrote down what
    /// that run was asked, and a tool that guessed would be inventing the comparability this
    /// field exists to establish.
    #[serde(default)]
    pub instrument: Instrument,
    /// The preconditions it tested rather than assumed.
    ///
    /// note: read these before the scores. An experiment whose material turned out not to do
    /// anything to *this* subject has measured nothing about its self-knowledge, however
    /// confident its claims were, and a check that did not hold says so where a footnote would
    /// not.
    #[serde(default)]
    pub checks: Vec<Check>,
    /// What the subject reported itself to be.
    pub model: Option<ModelInfo>,
    /// The parameters it was run with.
    pub params: Params,
    /// Everything that happened, in order.
    pub steps: Vec<Step>,
    /// What it cost.
    pub spend: Spend,
    /// What every comparison it made came to.
    pub scores: Scores,
    /// The same, by family of claim.
    pub families: Vec<Family>,
    /// The same, by remove of self-reference.
    pub depths: Depths,
    /// The same, by stage of the experiment, where it has stages.
    #[serde(default)]
    pub stages: Vec<Stage>,
    /// The same, by stage, paired item by item - the primary endpoint.
    ///
    /// note: every ordered pair of stages, because the interesting contrast is not always the
    /// adjacent one: `reported` to `retested` is what a handle bought the subject that had
    /// already guessed, and `reported` to `tested` is what it bought one that never did. The
    /// difference between *those two* is the order effect, and it is only readable if both are
    /// present.
    #[serde(default)]
    pub paired: Vec<Paired>,
    /// What it did when its own experiment contradicted its own claim.
    #[serde(default)]
    pub deference: Option<Deference>,
    /// Whether it reached for the handles it was given.
    ///
    /// What it claimed about items that provably do nothing, split by whether they read numeric.
    ///
    /// note: the primary endpoint. `None` where the experiment resolved no counterfactual claims
    /// about a dossier's own notes, which is every experiment that asks something else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<Surface>,
    /// note: read this before the stages. Below the preregistered gate the instrumented stages
    /// are measuring a model that does not use tools, which is worth knowing and is not what the
    /// ladder claims to measure.
    #[serde(default)]
    pub reached: Option<Reached>,
    /// The same, on either side of the feedback, where the experiment gave any.
    pub gain: Option<Gain>,
    /// Why it stopped early, if it did.
    ///
    /// note: An experiment that fails partway keeps its steps and its scores, and says what went
    /// wrong beside them. Thirty requests into a run is the wrong moment to discover that a
    /// harness treats a timeout as a reason to throw the afternoon away - and a run of this kind
    /// is nearly all requests to somebody else's API, so the timeout is not hypothetical.
    pub failed: Option<String>,
}

impl Outcome {
    /// Reads a trial's record into a scored outcome.
    pub fn of(trial: &Trial, failed: Option<String>) -> Self {
        let resolutions = trial.resolutions();
        let gain = Gain::over(&resolutions);
        let steps = trial.steps();
        let stages = Stage::over(&resolutions);

        // every ordered pair, earlier stage first, and only the ones that actually paired
        let mut paired = Vec::new();
        for (n, before) in stages.iter().enumerate() {
            for after in stages.iter().skip(n + 1) {
                let contrast = Paired::over(&resolutions, &before.name, &after.name);
                if contrast.is_measurable() {
                    paired.push(contrast);
                }
            }
        }

        let deference = Deference::over(&trial.faceds());
        let reached = Reached::over(&steps);
        let surface = Surface::over(&resolutions, crate::suite::dossier::surface);

        Self {
            experiment: trial.experiment().to_owned(),
            instrument: trial.instrument().clone(),
            checks: trial.checks(),
            model: trial.model().cloned(),
            params: trial.params().clone(),
            spend: trial.spend(),
            scores: Scores::over(&resolutions),
            families: Family::over(&resolutions),
            depths: Depths::over(&resolutions),
            stages,
            paired,
            deference: deference.is_measurable().then_some(deference),
            reached: reached.is_measurable().then_some(reached),
            surface: surface.is_measurable().then_some(surface),
            gain: gain.is_measurable().then_some(gain),
            steps,
            failed,
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}  [{}]", self.experiment, self.instrument)?;
        for check in self.checks.iter().filter(|check| !check.held) {
            writeln!(f, "  {:<16}{}: {}", "unmet:", check.what, check.detail)?;
        }
        writeln!(f, "  {:<16}{}", "overall:", self.scores)?;
        for family in &self.families {
            writeln!(f, "  {:<16}{}", format!("{}:", family.kind), family.scores)?;
        }
        for stage in &self.stages {
            writeln!(f, "  {:<16}{}", format!("{}:", stage.name), stage.scores)?;
        }
        for contrast in &self.paired {
            writeln!(f, "  {:<16}{contrast}", "paired:")?;
        }
        // above the handles and the stages, because it is the endpoint they are read against
        if let Some(surface) = &self.surface {
            writeln!(f, "  {:<16}{surface}", "surface:")?;
        }
        if let Some(reached) = &self.reached {
            writeln!(f, "  {:<16}{reached}", "handles:")?;
        }
        if let Some(deference) = &self.deference {
            writeln!(f, "  {:<16}{deference}", "deference:")?;
        }
        if self.depths.is_recursive() {
            writeln!(f, "{}", self.depths)?;
        }
        if let Some(gain) = &self.gain {
            writeln!(f, "  {:<16}{gain}", "told:")?;
        }
        write!(
            f,
            "  {:<16}{} requests, {} in / {} out",
            "cost:", self.spend.requests, self.spend.input, self.spend.output
        )?;
        if let Some(failed) = &self.failed {
            write!(f, "\n  {:<16}{failed}", "stopped:")?;
        }

        Ok(())
    }
}

/// What a whole run came to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// When the run started, in milliseconds since the Unix epoch.
    pub at: u64,
    /// One entry per experiment.
    pub outcomes: Vec<Outcome>,
}

impl Report {
    /// Every comparison every experiment made, pooled.
    ///
    /// note: Pooled, and worth being careful with, for two reasons. The experiments ask different
    /// numbers of questions, so a pooled figure is weighted by how talkative each one is rather
    /// than by how much each one is worth. And [`Scores::majority`] is taken over outcomes that
    /// are not all of a kind - a yes-or-no from one experiment beside a note's label from
    /// another - so the pooled baseline is a mixture rather than a baseline. Read the
    /// per-experiment scores first; this is for a headline.
    pub fn scores(&self) -> Scores {
        let resolutions: Vec<_> = self
            .outcomes
            .iter()
            .flat_map(|outcome| {
                outcome.steps.iter().filter_map(|step| match step {
                    Step::Resolved(resolution) => Some(resolution.clone()),
                    _ => None,
                })
            })
            .collect();

        Scores::over(&resolutions)
    }

    /// The primary endpoint over every claim the run made, pooled across its experiments.
    ///
    /// note: pooled here and not in [`Outcome`], because H1 is a claim about a *model* and the
    /// experiments are only different ways of putting the same counterfactual to it. `attribution`
    /// asks about one dossier's notes, `feedback` about two, `privilege` about its own and someone
    /// else's - and every one of those claims is about an item whose ablation either moved the
    /// answer or did not. Reading them apart would leave the endpoint computed over forty items in
    /// one column and nine in another, when the model is the unit.
    pub fn surface(&self) -> Surface {
        let resolutions: Vec<_> = self
            .outcomes
            .iter()
            .flat_map(|outcome| {
                outcome.steps.iter().filter_map(|step| match step {
                    Step::Resolved(resolution) => Some(resolution.clone()),
                    _ => None,
                })
            })
            .collect();

        Surface::over(&resolutions, crate::suite::dossier::surface)
    }

    /// What the run says it measured, where its outcomes agree about it.
    ///
    /// note: `None` when the outcomes disagree, which would mean a report holding two models -
    /// something the runner cannot produce and a hand-edited file could.
    pub fn model(&self) -> Option<&str> {
        let named: Vec<&str> = self
            .outcomes
            .iter()
            .filter_map(|outcome| outcome.model.as_ref())
            .map(|model| model.model.as_str())
            .collect();
        let first = named.first()?;

        named.iter().all(|seen| seen == first).then_some(*first)
    }

    /// What the whole run cost.
    pub fn spend(&self) -> Spend {
        self.outcomes.iter().map(|outcome| outcome.spend).sum()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let model = self
            .outcomes
            .iter()
            .find_map(|outcome| outcome.model.as_ref())
            .map(|model| format!("{}/{}", model.provider, model.model))
            .unwrap_or_else(|| "an unnamed model".to_owned());
        writeln!(f, "{model}")?;

        for outcome in &self.outcomes {
            writeln!(f, "\n{outcome}")?;
        }

        let spend = self.spend();
        write!(
            f,
            "\npooled: {}\ntotal:  {} requests, {} in / {} out",
            self.scores(),
            spend.requests,
            spend.input,
            spend.output
        )
    }
}

/// Picks one report per model: the newest of those that produced a usable primary endpoint.
///
/// note: the rule that matters is not recency, it is that **a report which measured nothing never
/// displaces one that did**. Recency only breaks ties among reports that did produce the endpoint.
///
/// note: written after the naive version cost a model. A re-run of `x-ai/grok-4.6` stopped after
/// nine requests on a provider budget limit, and being that model's newest report it replaced a
/// completed run of two hundred and fifty-eight - so the model left the pooled table as a dash,
/// the cohort silently became five, and the only sign of it was a parenthesis reading "1 not
/// measured". A sweep exists to be re-run in pieces when cells fail, which makes a failed re-run
/// the *expected* case rather than an odd one, and the analysis has to survive it.
pub fn per_model<T>(reports: impl IntoIterator<Item = (Report, T)>) -> Vec<(String, Report, T)> {
    let mut best: BTreeMap<String, (Report, T, bool)> = BTreeMap::new();
    for (report, tag) in reports {
        let model = report.model().unwrap_or("(unnamed)").to_owned();
        let measured = report.surface().is_measurable();
        let better = match best.get(&model) {
            None => true,
            Some((_, _, seen)) if *seen != measured => measured,
            Some((seen, ..)) => report.at > seen.at,
        };
        if better {
            best.insert(model, (report, tag, measured));
        }
    }

    best.into_iter()
        .map(|(model, (report, tag, _))| (model, report, tag))
        .collect()
}

/// Runs a set of experiments, each on a subject of its own, and collects what they found.
///
/// note: `make` is called once per experiment rather than once per run, because a session that
/// has already been asked about itself is not a clean subject: it has learnt that it is being
/// measured, and nothing in a score would show the difference between the first experiment and
/// the fourth. The name it is handed is the experiment's, so that the sessions can be told apart
/// in a log.
///
/// note: Nothing here is run concurrently. Every request in an evaluation goes to the same
/// endpoint, and a harness that fanned four experiments out at once would be measuring a rate
/// limiter as much as a model.
pub async fn evaluate(
    experiments: impl IntoIterator<Item = Arc<dyn Experiment>>,
    make: impl Fn(&str) -> Result<Subject>,
) -> Report {
    let mut outcomes = Vec::new();

    for experiment in experiments {
        let subject = match make(experiment.name()) {
            Ok(subject) => subject,
            Err(e) => {
                outcomes.push(Outcome {
                    experiment: experiment.name().to_owned(),
                    instrument: experiment.instrument(),
                    checks: Vec::new(),
                    model: None,
                    params: Params::new(),
                    steps: Vec::new(),
                    spend: Spend::default(),
                    scores: Scores::default(),
                    families: Vec::new(),
                    depths: Depths::default(),
                    stages: Vec::new(),
                    paired: Vec::new(),
                    deference: None,
                    reached: None,
                    surface: None,
                    gain: None,
                    failed: Some(e.to_string()),
                });
                continue;
            }
        };

        let trial = Trial::new(experiment.name(), &subject).asking(experiment.instrument());
        let failed = experiment
            .run(&subject, &trial)
            .await
            .err()
            .map(|e| e.to_string());
        outcomes.push(Outcome::of(&trial, failed));
    }

    Report {
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or_default(),
        outcomes,
    }
}
