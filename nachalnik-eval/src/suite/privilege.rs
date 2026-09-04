//! The control that decides whether any of the rest is metacognition: the same claim, about its
//! own context and about somebody else's.

use nachalnik::{Config, ContextItem, Kernel};

use crate::{
    async_trait,
    error::{Error, Result},
    experiment::{Experiment, Instrument},
    fork::{Ablation, Origin},
    intervene::Intervention,
    probe::{Answer, Probe},
    subject::Subject,
    suite::{
        counterfactual,
        dossier::{DEPOT, Dossier, ORCHARD, id_of},
        instrument, note_drift, script,
    },
    trial::{Kind, Resolution, Step, Trial},
};

/// How many notes each battery asks about.
const BATTERY: usize = 5;

/// Asks a subject to predict ablations on its own context and on a context it has only been
/// shown, and compares how well it does at each.
///
/// note: Every other experiment here measures how well a model predicts the behaviour of a
/// context it is *in*. None of them can tell that apart from ordinary reasoning about some notes,
/// because a model good at working out what a dossier determines will look good at working out
/// what its own dossier determines. This one puts the two side by side: same model, same session,
/// matched material, batteries interleaved, and the only thing that differs is whose context is
/// being predicted. If the two accuracies are the same, being in the context conferred nothing.
///
/// note: the foreign session is real rather than described. A second [`Subject`] is built on the
/// same provider, given the other dossier as its own notes, and asked the question; the copies
/// that settle the foreign battery are made from *its* context. Predicting a session that was
/// never run would be predicting a story about a session.
///
/// note: what cannot be held constant is that the foreign context arrives as quoted text while
/// the subject's own arrives as its context. That is the difference under test and also,
/// unavoidably, a difference in presentation - so a study runs [`Privilege::swapped`] as well and
/// reports both. A study's own methods document is the place that difference gets argued.
pub struct Privilege {
    own: &'static Dossier,
    foreign: &'static Dossier,
    battery: usize,
    replicates: usize,
}

impl Default for Privilege {
    fn default() -> Self {
        Self {
            own: &DEPOT,
            foreign: &ORCHARD,
            battery: BATTERY,
            replicates: 1,
        }
    }
}

impl Privilege {
    /// The experiment on its default material.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same, with the dossiers the other way round.
    ///
    /// note: the counterbalance. The two dossiers are built to one design and are still two
    /// different sets of numbers, so a difference between the arms of a single run is confounded
    /// with a difference between the materials. Running both orders and pooling is what separates
    /// them, and it is the only reason this method exists.
    #[must_use]
    pub fn swapped(mut self) -> Self {
        std::mem::swap(&mut self.own, &mut self.foreign);
        self
    }

    /// How many notes each battery asks about.
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
}

#[async_trait]
impl Experiment for Privilege {
    fn name(&self) -> &str {
        "privilege"
    }

    fn about(&self) -> &str {
        "predicts ablations on its own context and on another session's, and compares the two - \
         the control that separates privileged self-access from reasoning about some notes"
    }

    fn asks(&self) -> &'static [&'static str] {
        &[
            script::COUNTERFACTUAL,
            script::EXCLUDED,
            script::FOREIGN,
            script::QUOTED,
        ]
    }

    fn instrument(&self) -> Instrument {
        instrument(&[self.own, self.foreign], self.asks())
    }

    async fn run(&self, subject: &Subject, trial: &Trial) -> Result<()> {
        // ------------------------------------------------------------------------ its own
        let mine = self.own.install(subject);
        trial.record(Step::Briefed {
            items: mine.clone(),
        });
        let question = self.own.probe();
        let (said, answer) = subject.probe(&question).await?;
        trial.asked(&question, &said, &answer);

        // frozen before the other session's notes are quoted into this one, so that the copies
        // settling this battery never see the foreign material
        let origin = Origin::of(subject)?;
        let blind = [said.asked, said.item];

        // --------------------------------------------------------------------- somebody else's
        let provider = subject.kernel().provider().ok_or_else(|| {
            Error::Setup("there is no provider to run a second session on".into())
        })?;
        let elsewhere = Kernel::new(Config {
            session_name: Some(format!("{}#elsewhere", subject.kernel().session_name())),
            ..Config::default()
        });
        elsewhere.set_provider(provider);
        elsewhere.set_params(subject.kernel().params());
        let elsewhere = Subject::new(elsewhere);

        let theirs = self.foreign.install(&elsewhere);
        let their_question = self.foreign.probe();
        let (their_said, their_answer) = elsewhere.probe(&their_question).await?;
        trial.note(format!(
            "another session, on `{}`, answered `{}`",
            self.foreign.name,
            their_answer.key().unwrap_or_else(|| "nothing".into())
        ));
        let their_origin = Origin::of(&elsewhere)?;
        let their_blind = [their_said.asked, their_said.item];

        let quoted = script::fill(
            script::QUOTED,
            &[
                ("brief", self.foreign.brief),
                (
                    "notes",
                    &self
                        .foreign
                        .notes
                        .iter()
                        .map(|note| format!("{}:\n{}\n\n", note.label, note.text))
                        .collect::<String>(),
                ),
            ],
        );
        subject.kernel().push(
            ContextItem::memory("another-session/context", quoted)
                .because("the material of the foreign arm"),
        );

        // ---------------------------------------------------------------------------- predict
        // interleaved, so that neither arm is systematically asked while the subject is fresher
        // or more practised than it is for the other
        let ours = self.own.battery(self.battery);
        let others = self.foreign.battery(self.battery);
        let mut claims: Vec<(Kind, String, Answer)> = Vec::new();
        for round in 0..ours.len().max(others.len()) {
            if let Some(label) = ours.get(round) {
                let probe = counterfactual(
                    self.own.question,
                    &script::fill(script::EXCLUDED, &[("label", label)]),
                );
                let (said, claim) = subject.probe(&probe).await?;
                trial.asked(&probe, &said, &claim);
                claims.push((Kind::Counterfactual, (*label).to_owned(), claim));
            }
            if let Some(label) = others.get(round) {
                let probe = Probe::claim(script::fill(
                    script::FOREIGN,
                    &[
                        ("question", self.foreign.question),
                        (
                            "answer",
                            &their_answer.key().unwrap_or_else(|| "nothing".into()),
                        ),
                        (
                            "difference",
                            &script::fill(script::EXCLUDED, &[("label", label)]),
                        ),
                    ],
                ));
                let (said, claim) = subject.probe(&probe).await?;
                trial.asked(&probe, &said, &claim);
                claims.push((Kind::Foreign, (*label).to_owned(), claim));
            }
        }

        // ---------------------------------------------------------------------------- observe
        let here = Ablation::new(question)
            .replicates(self.replicates)
            .blind_to(blind);
        let control = here.observe(&origin, Intervention::Nothing).await?;
        note_drift(trial, &answer, &control);
        trial.measured(control.clone(), None);

        let there = Ablation::new(their_question)
            .replicates(self.replicates)
            .blind_to(their_blind);
        let their_control = there.observe(&their_origin, Intervention::Nothing).await?;
        note_drift(trial, &their_answer, &their_control);
        trial.measured(their_control.clone(), None);

        for (kind, label, claim) in claims {
            let (notes, origin, ablation, control, dossier) = match kind {
                Kind::Foreign => (&theirs, &their_origin, &there, &their_control, self.foreign),
                _ => (&mine, &origin, &here, &control, self.own),
            };
            let Some(id) = id_of(notes, &label) else {
                continue;
            };
            let observation = ablation
                .observe(origin, Intervention::without([id]))
                .await?;
            let change = observation.against(control);
            trial.measured(observation, Some(change.clone()));

            trial.resolve(
                Resolution::new(kind, claim, change.as_answer())
                    .about_item(id)
                    // the dossier the note came from, which is the foreign one on that arm: a
                    // surface feature is a property of the text, and the text is theirs
                    .on_material(dossier.name)
                    .about_note(&label)
                    .because(format!(
                        "on `{}`, without `{label}` the copies answered {}, against {} with it",
                        dossier.name,
                        change.after.clone().unwrap_or_else(|| "nothing".to_owned()),
                        change
                            .before
                            .clone()
                            .unwrap_or_else(|| "nothing".to_owned()),
                    )),
            );
        }

        Ok(())
    }
}
