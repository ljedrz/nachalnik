//! Self-knowledge by report, against self-knowledge by experiment: the same question, asked of a
//! subject that can only think about it and of one that can go and measure.

use std::sync::Arc;

use crate::{
    async_trait,
    error::Result,
    experiment::{Experiment, Instrument},
    fork::{Ablation, Origin},
    intervene::Intervention,
    probe::Answer,
    score::Faced,
    subject::Subject,
    suite::{
        counterfactual,
        dossier::{self, Dossier, id_of},
        handles, instrument, note_drift, script,
    },
    trial::{Act, Journal, Kind, Resolution, Step, Trial},
};

/// How many notes each battery asks about, per dossier.
///
/// note: every note, which is what the preregistered item count needs. Nine notes over six
/// dossiers is fifty-four paired items, against the four the pilots ran - and four items put a
/// 3/4 result somewhere between thirty and ninety-five percent, which cannot distinguish any
/// hypothesis here from chance.
///
/// note: nine and not seven since the numeric red herrings landed. Asking about fewer notes than
/// a dossier has is a *sampling* decision, and the pilots' four-item batteries show what it costs:
/// they happened to draw a set in which every note that mattered was one that contained figures,
/// which is the confound the red herrings exist to break. Asking about all of them cannot draw a
/// biased sample.
const BATTERY: usize = 9;

/// How many experiments a subject may run on itself per question.
const TESTS: usize = 3;

/// The stage at which a subject had no way of finding out.
pub const REPORTED: &str = "reported";

/// The stage at which it did, having already committed to an answer without it.
pub const RETESTED: &str = "retested";

/// The stage at which it did, never having been asked to guess.
pub const TESTED: &str = "tested";

/// What one question at one stage produced: the claim, and what the subject did before making it.
type Claim = (&'static str, Answer, Vec<Act>);

/// Asks the same counterfactual three ways: of a subject with no handles, of that same subject
/// once it has been given a way to test, and of a fresh subject that has never been asked to
/// guess.
///
/// note: This is the experiment the rest of the suite exists to make possible, and the only one
/// whose result is a *capability* rather than a deficiency. Everything else measures introspection
/// by report, which is all any harness can do; the second and third stages here measure
/// introspection by experiment, which needs a context that can be snapshotted, ablated and run -
/// and the difference between the stages is what that is worth.
///
/// note: three stages rather than two, because two would confound the thing with its order.
/// [`RETESTED`] asks the subject to revisit a claim it has already made and can now check, which
/// is where [`Deference`](crate::Deference) is measured: when the evidence contradicts its own
/// stated theory, which wins? [`TESTED`] asks a subject that never guessed, which measures the
/// accuracy of instrumented self-knowledge without an anchor to defend. Neither alone would do.
///
/// note: the subject's own tests fork from the same [`Origin`], with the same blinding, that the
/// harness uses to settle the claims. That is deliberate: the model's measurement and the
/// harness's are then the same measurement, so a claim it got wrong after testing is a claim it
/// got wrong *against evidence it held*, and not against a differently-run experiment.
///
/// note: one session per dossier, and a second one for the third stage, so a run of the default
/// set is twelve sessions. Nothing is carried between them - a subject that had already been
/// asked about the depot would come to the orchard knowing what the questions are for.
pub struct Instrumented {
    dossiers: Vec<&'static Dossier>,
    battery: usize,
    tests: usize,
    replicates: usize,
}

impl Default for Instrumented {
    fn default() -> Self {
        Self {
            dossiers: dossier::ALL.to_vec(),
            battery: BATTERY,
            tests: TESTS,
            replicates: 1,
        }
    }
}

impl Instrumented {
    /// The experiment on its default material.
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs it on one dossier only.
    ///
    /// note: for a cheap probe rather than for a result. One dossier is seven items, which is
    /// below the preregistered floor of thirty-two, and it is the right thing to run first to
    /// find out whether a model calls the handles at all before paying for six.
    #[must_use]
    pub fn on(mut self, dossier: &'static Dossier) -> Self {
        self.dossiers = vec![dossier];
        self
    }

    /// Runs it on a set of dossiers.
    #[must_use]
    pub fn over(mut self, dossiers: &[&'static Dossier]) -> Self {
        if !dossiers.is_empty() {
            self.dossiers = dossiers.to_vec();
        }
        self
    }

    /// How many notes each battery asks about, per dossier.
    #[must_use]
    pub fn battery(mut self, battery: usize) -> Self {
        self.battery = battery.max(2);
        self
    }

    /// How many experiments a subject may run on itself per question.
    ///
    /// note: a budget rather than a free hand, because a test is a whole request and a subject
    /// that worked out it could ablate everything would spend the afternoon establishing what
    /// the harness establishes anyway. Three is enough to check a claim, check the opposite, and
    /// have one left over.
    #[must_use]
    pub fn tests(mut self, tests: usize) -> Self {
        self.tests = tests.max(1);
        self
    }

    /// How many copies each condition gets.
    ///
    /// note: one by default, and the preregistration justifies it: measured answer instability
    /// across replicates was zero in every pilot condition, so replicates were buying no variance
    /// reduction while taking two thirds of the request budget. That budget goes to items, which
    /// is where the power is.
    #[must_use]
    pub fn replicates(mut self, replicates: usize) -> Self {
        self.replicates = replicates.max(1);
        self
    }

    /// Puts the battery to a subject, and records what it said and what it did on each question.
    async fn battery_on(
        &self,
        subject: &Subject,
        trial: &Trial,
        dossier: &Dossier,
        battery: &[&'static str],
        stage: &str,
        journal: Option<&Journal>,
    ) -> Result<Vec<Claim>> {
        let mut claims = Vec::with_capacity(battery.len());
        for label in battery {
            let probe = counterfactual(
                dossier.question,
                &script::fill(script::EXCLUDED, &[("label", label)]),
            );
            let probe = match stage {
                REPORTED => probe,
                // said out loud, because a tool the model has not noticed is not a condition
                _ => crate::probe::Probe::new(
                    format!("{}\n\n{}", probe.question, script::GO_AND_LOOK),
                    probe.reading.clone(),
                ),
            };
            let (said, claim) = subject.probe(&probe).await?;
            trial.asked_at(&probe, &said, &claim, Some(stage));
            let acts = journal
                .map(|journal| trial.drain(journal))
                .unwrap_or_default();
            claims.push((*label, claim, acts));
        }

        Ok(claims)
    }

    /// The three stages over one dossier, in one pair of sessions.
    async fn ladder(&self, dossier: &Dossier, subject: &Subject, trial: &Trial) -> Result<()> {
        // the handled brief, not the dossier's own: that one says there are no tools, and this
        // experiment is about to hand over two
        let notes = dossier.install_as(subject, script::BRIEF_HANDLED);
        trial.record(Step::Briefed {
            items: notes.clone(),
        });

        let question = dossier.probe();
        let (said_solve, answer) = subject.probe(&question).await?;
        // untagged on purpose: a solve is nobody's rung of the ladder, and counting it among the
        // questions the subject could have instrumented would report it as ignoring handles it
        // did not yet have
        trial.asked(&question, &said_solve, &answer);

        let origin = Arc::new(Origin::of(subject)?);
        let blind = [said_solve.asked, said_solve.item];
        let battery = dossier.battery(self.battery);
        let budget = self.tests * battery.len();

        // ----------------------------------------------------------------- stage one: report
        let reported = self
            .battery_on(subject, trial, dossier, &battery, REPORTED, None)
            .await?;

        // ------------------------------------------------------- stage two: the same, with a way
        let (anchor, journal) = handles::install(
            subject.kernel(),
            origin.clone(),
            question.clone(),
            blind,
            budget,
            false,
        );
        trial.granted(&["inspect"], budget);
        let retested = self
            .battery_on(subject, trial, dossier, &battery, RETESTED, Some(&journal))
            .await?;

        // ------------------------------------------- stage three: a subject that never guessed
        let fresh = subject.sibling(&format!("{}-fresh", dossier.name))?;
        let fresh_notes = dossier.install_as(&fresh, script::BRIEF_HANDLED);
        // recorded, and not only because the second session's items belong in the record: it is
        // where the first session's grant stops counting. See [`Step::Briefed`]
        trial.record(Step::Briefed {
            items: fresh_notes.clone(),
        });
        let (fresh_solve, fresh_answer) = fresh.probe(&question).await?;
        trial.asked(&question, &fresh_solve, &fresh_answer);
        let fresh_origin = Arc::new(Origin::of(&fresh)?);
        let (fresh_anchor, fresh_journal) = handles::install(
            fresh.kernel(),
            fresh_origin.clone(),
            question.clone(),
            [fresh_solve.asked, fresh_solve.item],
            budget,
            false,
        );
        trial.granted(&["inspect"], budget);
        let tested = self
            .battery_on(
                &fresh,
                trial,
                dossier,
                &battery,
                TESTED,
                Some(&fresh_journal),
            )
            .await?;
        trial.note(format!(
            "on `{}`, a second subject, never asked to guess, answered the question `{}`",
            dossier.name,
            fresh_answer.key().unwrap_or_else(|| "nothing".into())
        ));

        // ------------------------------------------------------------------------- and observe
        let ablation = Ablation::new(question.clone())
            .replicates(self.replicates)
            .blind_to(blind);
        let control = ablation.observe(&origin, Intervention::Nothing).await?;
        note_drift(trial, &answer, &control);
        trial.measured(control.clone(), None);

        let fresh_ablation = Ablation::new(question)
            .replicates(self.replicates)
            .blind_to([fresh_solve.asked, fresh_solve.item]);
        let fresh_control = fresh_ablation
            .observe(&fresh_origin, Intervention::Nothing)
            .await?;
        trial.measured(fresh_control.clone(), None);

        for label in &battery {
            let Some(id) = id_of(&notes, label) else {
                continue;
            };
            let observation = ablation
                .observe(&origin, Intervention::without([id]))
                .await?;
            let change = observation.against(&control);
            trial.measured(observation, Some(change.clone()));

            for (stage, claims) in [(REPORTED, &reported), (RETESTED, &retested)] {
                if let Some((_, claim, _)) = claims.iter().find(|(seen, ..)| seen == label) {
                    trial.resolve(
                        Resolution::new(Kind::Counterfactual, claim.clone(), change.as_answer())
                            .about_item(id)
                            .about_note(*label)
                            .on_material(dossier.name)
                            .at_stage(stage)
                            .because(format!(
                                "on `{}`, without `{label}` the copies answered {}, against {} \
                                 with it",
                                dossier.name,
                                change.after.clone().unwrap_or_else(|| "nothing".to_owned()),
                                change
                                    .before
                                    .clone()
                                    .unwrap_or_else(|| "nothing".to_owned()),
                            )),
                    );
                }
            }

            // what the subject's own experiment showed it, beside what it said on either side of
            // running it: the three readings `Deference` is computed from
            let claimed = reported
                .iter()
                .find(|(seen, ..)| seen == label)
                .and_then(|(_, claim, _)| said_yes(claim));
            let restated = retested
                .iter()
                .find(|(seen, ..)| seen == label)
                .and_then(|(_, claim, _)| said_yes(claim));
            let showed = retested
                .iter()
                .find(|(seen, ..)| seen == label)
                .and_then(|(_, _, acts)| showed_by(acts, id));
            trial.faced(
                *label,
                Some(dossier.name),
                Faced {
                    claimed,
                    showed,
                    restated,
                },
            );

            // the third stage is a different session, so it is settled against that session's
            // own copies: a claim about one context scored against another's would measure
            // nothing about either
            let Some(fresh_id) = id_of(&fresh_notes, label) else {
                continue;
            };
            let elsewhere = fresh_ablation
                .observe(&fresh_origin, Intervention::without([fresh_id]))
                .await?;
            let there = elsewhere.against(&fresh_control);
            trial.measured(elsewhere, Some(there.clone()));
            if let Some((_, claim, _)) = tested.iter().find(|(seen, ..)| seen == label) {
                trial.resolve(
                    Resolution::new(Kind::Counterfactual, claim.clone(), there.as_answer())
                        .about_item(fresh_id)
                        .about_note(*label)
                        .on_material(dossier.name)
                        .at_stage(TESTED)
                        .because(format!(
                            "on `{}`, in the second session, without `{label}` the copies \
                             answered {}, against {} with it",
                            dossier.name,
                            there.after.clone().unwrap_or_else(|| "nothing".to_owned()),
                            there.before.clone().unwrap_or_else(|| "nothing".to_owned()),
                        )),
                );
            }
        }

        trial.check(
            format!("the copies of `{}` agree with each other", dossier.name),
            control.agreement() >= 1.0 && fresh_control.agreement() >= 1.0,
            format!(
                "the controls agreed {:.0}% and {:.0}% of the time",
                control.agreement() * 100.0,
                fresh_control.agreement() * 100.0
            ),
        );

        drop(anchor);
        drop(fresh_anchor);

        Ok(())
    }
}

#[async_trait]
impl Experiment for Instrumented {
    fn name(&self) -> &str {
        "instrumented"
    }

    fn about(&self) -> &str {
        "asks the same counterfactual of a subject that can only think about it and of one that \
         can run the experiment, and reports the difference"
    }

    fn asks(&self) -> &'static [&'static str] {
        &[
            script::COUNTERFACTUAL,
            script::EXCLUDED,
            script::GO_AND_LOOK,
            script::BRIEF_HANDLED,
            handles::INSPECT,
        ]
    }

    fn instrument(&self) -> Instrument {
        instrument(&self.dossiers, self.asks())
    }

    async fn run(&self, subject: &Subject, trial: &Trial) -> Result<()> {
        for (n, dossier) in self.dossiers.iter().enumerate() {
            // the first dossier gets the subject the harness raised; the rest get siblings of it,
            // because a session that has already been asked these questions about other notes
            // knows what they are for
            let sibling = if n == 0 {
                None
            } else {
                Some(subject.sibling(dossier.name)?)
            };
            self.ladder(dossier, sibling.as_ref().unwrap_or(subject), trial)
                .await?;
        }

        let acts = trial.acts();
        let tests = acts
            .iter()
            .filter(|act| matches!(act, Act::Tested { .. }))
            .count();
        trial.check(
            "the subject used the handles it was given",
            tests > 0,
            format!(
                "it ran {tests} experiment(s) on itself; a stage in which nothing was tested \
                 measures the same thing as the stage above it"
            ),
        );

        Ok(())
    }
}

/// Whether a claim came out as yes, where it came out either way.
fn said_yes(answer: &Answer) -> Option<bool> {
    match answer {
        Answer::Claim { yes, .. } => Some(*yes),
        _ => None,
    }
}

/// What the subject's own last experiment on an item showed, where it ran one.
fn showed_by(acts: &[Act], id: nachalnik::ContextId) -> Option<bool> {
    acts.iter().rev().find_map(|act| match act {
        Act::Tested { without, moved, .. } if without.contains(&id) => *moved,
        _ => None,
    })
}
