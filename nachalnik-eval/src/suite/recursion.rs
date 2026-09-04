//! Introspection at increasing removes: a claim, a claim about that claim, a claim about that.

use crate::{
    async_trait,
    error::Result,
    experiment::{Experiment, Instrument},
    fork::{Ablation, Origin},
    intervene::Intervention,
    probe::{Answer, Probe},
    subject::Subject,
    suite::{
        counterfactual,
        dossier::{DEPOT, Dossier, Expected, id_of},
        instrument, note_drift, script,
    },
    trial::{Kind, Resolution, Step, Trial},
};

/// How deep to go unless told otherwise.
const DEPTH: usize = 3;

/// Asks a subject what it would do, then what a copy of it would say it would do, then what a
/// copy asked *that* would say - and runs a copy at every level to find out.
///
/// note: What makes this a measurement rather than a hall of mirrors is that every level has a
/// ground truth, and every ground truth is a request somebody actually paid for. At one remove
/// the claim is about behaviour and is scored against a copy with the note taken out. At two, the
/// claim is about a claim, and is scored against a copy asked the level-one question. At three,
/// against a copy asked the level-two question. The recursion is in the questions; the answers
/// are all observations.
///
/// note: The subject makes its claims in one session, in order, so by the time it is asked the
/// level-three question it can see what it said at levels one and two. That is not a leak - it
/// is what "predict your own prediction" has to mean for a stateless model - but it is the
/// reason the copies are all made from an [`Origin`] taken before any of it: the copy answering
/// the level-one question must not be able to read the subject's answer to it.
pub struct Recursion {
    dossier: &'static Dossier,
    depth: usize,
    replicates: usize,
}

impl Default for Recursion {
    fn default() -> Self {
        Self {
            dossier: &DEPOT,
            depth: DEPTH,
            replicates: 1,
        }
    }
}

impl Recursion {
    /// The experiment on its default material, three removes deep.
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs it on another dossier.
    #[must_use]
    pub fn on(mut self, dossier: &'static Dossier) -> Self {
        self.dossier = dossier;
        self
    }

    /// How many removes to go to; at least one.
    ///
    /// note: each one past the first costs a copy and an ask, and the answers get shorter and
    /// less readable as the question gets more convoluted. Three is where the curve stops being
    /// about self-knowledge and starts being about how much nesting a model can parse, which is
    /// worth knowing and is not what this measures.
    #[must_use]
    pub fn depth(mut self, depth: usize) -> Self {
        self.depth = depth.max(1);
        self
    }

    /// How many copies each condition gets.
    #[must_use]
    pub fn replicates(mut self, replicates: usize) -> Self {
        self.replicates = replicates.max(1);
        self
    }
}

#[async_trait]
impl Experiment for Recursion {
    fn name(&self) -> &str {
        "recursion"
    }

    fn about(&self) -> &str {
        "predicts its own behaviour, then a copy's prediction of that, then a copy's prediction \
         of the copy's - with a copy actually run at every level"
    }

    fn asks(&self) -> &'static [&'static str] {
        &[script::COUNTERFACTUAL, script::EXCLUDED, script::DEEPER]
    }

    fn instrument(&self) -> Instrument {
        instrument(&[self.dossier], self.asks())
    }

    async fn run(&self, subject: &Subject, trial: &Trial) -> Result<()> {
        let notes = self.dossier.install(subject);
        trial.record(Step::Briefed {
            items: notes.clone(),
        });

        let question = self.dossier.probe();
        let (said_solve, answer) = subject.probe(&question).await?;
        trial.asked(&question, &said_solve, &answer);

        let origin = Origin::of(subject)?;
        // no copy may read the answer the session already gave: see `Ablation::blind_to`
        let blind = [said_solve.asked, said_solve.item];

        // two ladders, over a note the dossier is built around and a note it expects to be doing
        // nothing. One ladder answers `yes` all the way down whatever the model is doing, so a
        // depth curve built on it alone cannot tell a subject from one that always says yes -
        // which is most of what a depth curve is for
        let mut pivots = vec![self.dossier.decisive];
        if let Some(quiet) = self
            .dossier
            .notes
            .iter()
            .find(|note| note.expected == Expected::Holds)
        {
            pivots.push(quiet.label);
        }

        let ablation = Ablation::new(question)
            .replicates(self.replicates)
            .blind_to(blind);
        let control = ablation.observe(&origin, Intervention::Nothing).await?;
        note_drift(trial, &answer, &control);
        trial.measured(control.clone(), None);
        trial.check(
            "the copies agree with each other",
            control.agreement() >= 1.0,
            format!(
                "the control copies agreed {:.0}% of the time over {} replicate(s)",
                control.agreement() * 100.0,
                control.answers.len()
            ),
        );

        // ---------------------------------------------------------------------------- predict
        let mut ladders = Vec::new();
        for pivot in &pivots {
            let Some(id) = id_of(&notes, pivot) else {
                return Err(crate::Error::Setup(format!(
                    "the dossier's note `{pivot}` was not planted"
                )));
            };

            // the ladder of questions, built before any of them is asked so that level n is
            // exactly the text level n+1 quotes
            let mut ladder = vec![counterfactual(
                self.dossier.question,
                &script::fill(script::EXCLUDED, &[("label", pivot)]),
            )];
            while ladder.len() < self.depth {
                ladder.push(deeper(ladder.last().expect("the ladder starts with one")));
            }

            let mut claims = Vec::with_capacity(ladder.len());
            for probe in &ladder {
                let (said, claim) = subject.probe(probe).await?;
                trial.asked(probe, &said, &claim);
                claims.push(claim);
            }
            ladders.push((*pivot, id, ladder, claims));
        }

        // ---------------------------------------------------------------------------- observe
        for (pivot, id, ladder, claims) in &ladders {
            let without = ablation
                .observe(&origin, Intervention::without([*id]))
                .await?;
            let ground = without.against(&control);
            trial.measured(without, Some(ground.clone()));

            trial.resolve(
                Resolution::new(Kind::Counterfactual, claims[0].clone(), ground.as_answer())
                    .about_item(*id)
                    .on_material(self.dossier.name)
                    .about_note(*pivot)
                    .at_depth(1)
                    .because(format!(
                        "the copies answered {} without `{pivot}`, against {} with it",
                        ground.after.clone().unwrap_or_else(|| "nothing".to_owned()),
                        ground
                            .before
                            .clone()
                            .unwrap_or_else(|| "nothing".to_owned()),
                    )),
            );

            for (level, claim) in claims.iter().enumerate().skip(1) {
                // what a copy really says when asked the question one level down, which is
                // exactly what the claim at this level is a claim about
                let asked = Ablation::new(ladder[level - 1].clone())
                    .replicates(self.replicates)
                    .blind_to(blind);
                let observation = asked.observe(&origin, Intervention::Nothing).await?;
                let happened = observation
                    .majority()
                    .map_or(Answer::Unreadable, |key| Answer::yes(key == "yes"));
                trial.measured(observation, None);

                trial.resolve(
                    Resolution::new(Kind::Recursive, claim.clone(), happened.clone())
                        .about_item(*id)
                        .at_depth(level + 1)
                        .because(format!(
                            "on `{pivot}`, a copy asked the level-{level} question answered {}",
                            happened.key().unwrap_or_else(|| "nothing".into())
                        )),
                );

                // the quantity nobody asked for and everybody wants: whether a copy of it, asked
                // what it was just asked, gives the answer it gave
                if level == 1 {
                    trial.note(format!(
                        "on `{pivot}`, the subject said {} at level 1 and a copy of it said {}",
                        claims[0].key().unwrap_or_else(|| "nothing".into()),
                        happened.key().unwrap_or_else(|| "nothing".into()),
                    ));
                }
            }
        }

        trial.check(
            "the depth curve is built on more than one outcome",
            ladders.len() > 1,
            format!(
                "the ladders run over {} note(s): {}",
                ladders.len(),
                pivots.join(", ")
            ),
        );

        Ok(())
    }
}

/// The same question one remove out: what a copy would answer to the one below it.
fn deeper(inner: &Probe) -> Probe {
    Probe::claim(script::fill(script::DEEPER, &[("inner", &inner.asked())]))
}
