//! Whether being told how it did makes a subject better at saying how it will do.

use nachalnik::{ContextItem, ContextState};

use crate::{
    async_trait,
    error::Result,
    experiment::{Experiment, Instrument},
    fork::{Ablation, Origin},
    intervene::Intervention,
    probe::Answer,
    subject::Subject,
    suite::{
        counterfactual,
        dossier::{DEPOT, Dossier, ORCHARD, id_of},
        instrument, note_drift, script,
    },
    trial::{Kind, Labelled, Resolution, Step, Trial},
};

/// How many notes each battery asks about.
const BATTERY: usize = 6;

/// Runs a battery of counterfactual claims, measures every one of them, tells the subject what
/// happened, and runs a second battery of the same shape over different material.
///
/// note: The two batteries are over two dossiers rather than over the same one twice, and that
/// is what the experiment turns on. Asking again about notes it has just been told the answers
/// for measures memory; asking about different notes of the same kind measures whether anything
/// transferred. The dossiers are built to the same design for exactly this comparison - see
/// [`ORCHARD`].
///
/// note: The feedback is outcomes and nothing else: which way each claim went, what the copies
/// actually did, and how many it got right. No advice, no explanation of what it should have
/// noticed, and no hint that its confidence was the part that was off. What is being measured is
/// whether a subject can improve its own model of itself from results, which is a much weaker
/// and more interesting claim than whether it can follow instructions about how to answer.
pub struct Feedback {
    first: &'static Dossier,
    second: &'static Dossier,
    battery: usize,
    replicates: usize,
}

impl Default for Feedback {
    fn default() -> Self {
        Self {
            first: &DEPOT,
            second: &ORCHARD,
            battery: BATTERY,
            replicates: 1,
        }
    }
}

impl Feedback {
    /// The experiment on its default material.
    pub fn new() -> Self {
        Self::default()
    }

    /// The two dossiers to use, in order.
    #[must_use]
    pub fn between(mut self, first: &'static Dossier, second: &'static Dossier) -> Self {
        self.first = first;
        self.second = second;
        self
    }

    /// How many notes each battery asks about; at least two, or there is nothing to be
    /// calibrated about.
    #[must_use]
    pub fn battery(mut self, battery: usize) -> Self {
        self.battery = battery.max(2);
        self
    }

    /// How many copies each condition gets.
    #[must_use]
    pub fn replicates(mut self, replicates: usize) -> Self {
        self.replicates = replicates.max(1);
        self
    }

    /// One battery: plant a dossier, answer its question, claim what each note is doing, then
    /// measure every claim.
    async fn round(
        &self,
        subject: &Subject,
        trial: &Trial,
        dossier: &'static Dossier,
        informed: bool,
    ) -> Result<(Vec<Labelled>, Vec<Verdict>)> {
        let notes = match informed {
            false => dossier.install(subject),
            // the brief is already in the context and is the same sentence
            true => dossier.plant(subject),
        };
        trial.record(Step::Briefed {
            items: notes.clone(),
        });

        let question = dossier.probe();
        let (said_solve, answer) = subject.probe(&question).await?;
        trial.asked(&question, &said_solve, &answer);

        let origin = Origin::of(subject)?;
        let battery = dossier.battery(self.battery);

        let mut claims = Vec::with_capacity(battery.len());
        for label in &battery {
            let probe = counterfactual(
                dossier.question,
                &script::fill(script::EXCLUDED, &[("label", label)]),
            );
            let (said, claim) = subject.probe(&probe).await?;
            trial.asked(&probe, &said, &claim);
            claims.push(claim);
        }

        let ablation = Ablation::new(question)
            .replicates(self.replicates)
            .blind_to([said_solve.asked, said_solve.item]);
        let control = ablation.observe(&origin, Intervention::Nothing).await?;
        note_drift(trial, &answer, &control);
        trial.measured(control.clone(), None);

        trial.check(
            "the copies agree with each other",
            control.agreement() >= 1.0,
            format!(
                "on `{}` the control copies agreed {:.0}% of the time over {} replicate(s)",
                dossier.name,
                control.agreement() * 100.0,
                control.answers.len()
            ),
        );

        let mut verdicts = Vec::with_capacity(battery.len());
        for (label, claim) in battery.iter().zip(claims) {
            let Some(id) = id_of(&notes, label) else {
                continue;
            };
            let observation = ablation
                .observe(&origin, Intervention::without([id]))
                .await?;
            let change = observation.against(&control);
            trial.measured(observation, Some(change.clone()));

            let resolution = trial.resolve(
                Resolution::new(Kind::Counterfactual, claim.clone(), change.as_answer())
                    .about_item(id)
                    .on_material(dossier.name)
                    .about_note(*label)
                    .informed(informed)
                    .because(format!(
                        "without `{label}` the copies answered {}, against {} with it",
                        change.after.clone().unwrap_or_else(|| "nothing".to_owned()),
                        change
                            .before
                            .clone()
                            .unwrap_or_else(|| "nothing".to_owned()),
                    )),
            );

            verdicts.push(Verdict {
                label: (*label).to_owned(),
                said: claim,
                moved: change.moved,
                correct: resolution.correct,
                measured: resolution.measured,
            });
        }

        trial.check(
            format!("the `{}` battery is not degenerate", dossier.name),
            verdicts.iter().any(|v| v.moved == Some(true))
                && verdicts.iter().any(|v| v.moved == Some(false)),
            format!(
                "{} of {} notes moved the answer; a battery in which none or all do is one where \
                 a single word scores everything",
                verdicts.iter().filter(|v| v.moved == Some(true)).count(),
                verdicts.len()
            ),
        );

        Ok((notes, verdicts))
    }
}

#[async_trait]
impl Experiment for Feedback {
    fn name(&self) -> &str {
        "feedback"
    }

    fn about(&self) -> &str {
        "scores a battery of counterfactual claims, tells the subject how it did, and scores a \
         second battery of the same shape over fresh material"
    }

    fn asks(&self) -> &'static [&'static str] {
        &[
            script::COUNTERFACTUAL,
            script::EXCLUDED,
            script::TOLD,
            script::TOLD_LINE,
            script::TOLD_TALLY,
            script::SAID_DIFFERENT,
            script::SAID_SAME,
            script::SAID_NEITHER,
            script::DID_DIFFER,
            script::DID_AGREE,
            script::DID_NEITHER,
        ]
    }

    fn instrument(&self) -> Instrument {
        instrument(&[self.first, self.second], self.asks())
    }

    async fn run(&self, subject: &Subject, trial: &Trial) -> Result<()> {
        let (planted, verdicts) = self.round(subject, trial, self.first, false).await?;

        // -------------------------------------------------------------------------------- tell
        let feedback = report(&verdicts);
        subject.kernel().push(
            ContextItem::instruction("what actually happened", feedback.clone())
                .pinned()
                .because("the measured outcome of the subject's own predictions"),
        );
        trial.record(Step::Told { feedback });

        // the first round's notes go out of the request rather than out of the context: the
        // second question is about different material, and a copy of this session still lists
        // them and can bring them back. What stays is the exchange and the feedback, which is
        // the thing being tested
        subject.kernel().set_state(
            planted.iter().map(|note| note.id),
            ContextState::Excluded,
            Some("the first round is over".to_owned()),
        );

        // ------------------------------------------------------------------------------- again
        self.round(subject, trial, self.second, true).await?;

        Ok(())
    }
}

/// What one claim in a battery came to.
struct Verdict {
    label: String,
    said: Answer,
    moved: Option<bool>,
    correct: bool,
    measured: bool,
}

/// The feedback a subject is given: what it said, what happened, and how many it got right.
fn report(verdicts: &[Verdict]) -> String {
    let mut out = String::from(script::TOLD);

    for verdict in verdicts {
        let said = match verdict.said {
            Answer::Claim { yes: true, .. } => script::SAID_DIFFERENT,
            Answer::Claim { yes: false, .. } => script::SAID_SAME,
            _ => script::SAID_NEITHER,
        };
        let happened = match verdict.moved {
            Some(true) => script::DID_DIFFER,
            Some(false) => script::DID_AGREE,
            None => script::DID_NEITHER,
        };
        out.push_str(&script::fill(
            script::TOLD_LINE,
            &[
                ("label", &verdict.label),
                ("said", said),
                ("happened", happened),
            ],
        ));
    }

    let measured = verdicts.iter().filter(|v| v.measured).count();
    let correct = verdicts.iter().filter(|v| v.correct).count();
    out.push_str(&script::fill(
        script::TOLD_TALLY,
        &[
            ("correct", &correct.to_string()),
            ("measured", &measured.to_string()),
        ],
    ));

    out
}
