//! "Which of the things you are carrying is your answer made of?" - asked, then measured.

use nachalnik::ContextId;

use crate::{
    async_trait,
    error::Result,
    experiment::{Experiment, Instrument},
    fork::{Ablation, Change, Origin},
    intervene::Intervention,
    probe::{Answer, Probe, Reading},
    subject::Subject,
    suite::{
        counterfactual,
        dossier::{self, Dossier, Expected, id_of},
        instrument, note_drift, script,
    },
    trial::{Kind, Resolution, Step, Trial},
};

/// Elicits an attribution, an item number and a counterfactual, then ablates every note in the
/// context one at a time and scores all three against what the ablations did.
///
/// note: This is the experiment the other three are variations on, and the order of it is the
/// methodology: **every claim is elicited before any copy is run**. A subject that had seen one
/// ablation before making its next claim would be reasoning from evidence rather than from
/// itself, which is a different and much easier thing to be right about.
///
/// note: over every dossier by default, and that is why it costs what it costs. It is the
/// cheapest experiment that produces the primary endpoint - no tools, no stages, no ladder - and
/// the endpoint's denominator is *inert items*, of which one dossier yields about seven. Measured
/// on `deepseek/deepseek-v4-flash-0731`, `depot` alone gave four numeric and three plain and a
/// difference with an interval a hundred points wide. Six dossiers give roughly forty, which is
/// what a preregistered item count needs; it is a property of the material rather than of any
/// one study, which is why the default lives here.
///
/// note: It ablates *every* note rather than only the ones it asked about, and that is most of
/// what the experiment costs. It is not optional: "was the note it named the most influential
/// one?" cannot be answered without knowing what the others do, and an attribution scored against
/// a single measurement would be scoring a guess against a guess.
pub struct Attribution {
    dossiers: Vec<&'static Dossier>,
    replicates: usize,
    locating: bool,
}

impl Default for Attribution {
    fn default() -> Self {
        Self {
            dossiers: dossier::ALL.to_vec(),
            replicates: 1,
            // off, because the question it asks cannot be answered from where the subject sits:
            // see `locating`
            locating: false,
        }
    }
}

impl Attribution {
    /// The experiment on its default material.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to ask the subject what number a note is in its own context.
    ///
    /// note: off by default, and it was on for v4. The probe asks for the kernel's item id, which
    /// appears nowhere in what the subject reads - the projector renders an item as its label and
    /// then its content - and this experiment installs no handles, so there is no `look` either.
    /// Six models answered it 2 times in 108, an order of magnitude *below* the majority baseline
    /// for the row, which is what an unanswerable question scores rather than what an absent
    /// faculty does. The count is real and its reading was withdrawn; see the study write-up.
    ///
    /// note: kept rather than deleted, because the question becomes a fair one the moment the
    /// subject can see the numbering - and finding out whether a model can locate an item when it
    /// is allowed to look is worth an experiment. Anything turning this on should install
    /// [`handles`](crate::suite::handles) as well, which is the rule
    /// `tests/machinery.rs` holds the suite to.
    ///
    /// note: it also ran *before* the counterfactual battery, so every claim v4 measured was made
    /// in a context where the subject had just invented three item numbers. Turning this off
    /// removes that as well, which is the point of the re-run.
    #[must_use]
    pub fn locating(mut self, locating: bool) -> Self {
        self.locating = locating;
        self
    }

    /// Runs it on one dossier only.
    ///
    /// note: for a probe. One dossier is nine items, of which the seven or so that turn out to be
    /// inert are the whole denominator of the primary endpoint - measured, `depot` on
    /// `deepseek/deepseek-v4-flash-0731` gave four numeric and three plain, and a difference over
    /// seven items carries an interval a hundred points wide.
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

    /// How many copies each condition gets.
    #[must_use]
    pub fn replicates(mut self, replicates: usize) -> Self {
        self.replicates = replicates.max(1);
        self
    }
}

impl Attribution {
    /// The whole battery over one dossier, in one session.
    async fn battery(&self, dossier: &Dossier, subject: &Subject, trial: &Trial) -> Result<()> {
        let notes = dossier.install(subject);
        trial.record(Step::Briefed {
            items: notes.clone(),
        });

        // ------------------------------------------------------------------------------ solve
        let question = dossier.probe();
        let (said_solve, answer) = subject.probe(&question).await?;
        trial.asked(&question, &said_solve, &answer);
        trial.note(match answer.key().as_deref() {
            Some(key) if key == dossier.answer => {
                format!("it answered `{key}`, which the notes support")
            }
            Some(key) => format!(
                "it answered `{key}`; the notes support `{}`",
                dossier.answer
            ),
            None => "it did not answer the question readably".to_owned(),
        });

        // frozen here, before a single claim has been made: every copy below is made from the
        // context as it stood when the subject had answered and had not yet been asked about
        // itself
        let origin = Origin::of(subject)?;

        // ------------------------------------------------------------------------- introspect
        let labels: Vec<String> = notes.iter().map(|note| note.label.clone()).collect();
        let attribution = Probe::new(
            script::fill(
                script::ATTRIBUTION,
                &[
                    ("question", dossier.question),
                    ("answer", &answer.key().unwrap_or_default()),
                ],
            ),
            Reading::Choice(labels.clone()),
        );
        let (said, named) = subject.probe(&attribution).await?;
        trial.asked(&attribution, &said, &named);

        let claimed_label = named.key().map(|key| key.into_owned());
        // a subject that did not name anything is still asked the rest, about the note the
        // dossier was built around, so that the run produces the other measurements rather than
        // stopping on the first unreadable answer
        let about = claimed_label
            .clone()
            .unwrap_or_else(|| dossier.decisive.to_owned());

        // a battery rather than one claim, and at three different offsets on purpose: an error
        // that is always the note's own ordinal is a different finding from an error that
        // wanders, and one claim cannot tell them apart
        let mut placed: Vec<(String, Answer)> = vec![];
        let mut wanted = vec![about.clone()];
        for label in [notes.first(), notes.get(notes.len() / 2), notes.last()]
            .into_iter()
            .flatten()
            .map(|note| note.label.clone())
        {
            if !wanted.contains(&label) {
                wanted.push(label);
            }
        }
        for label in wanted.iter().take(3).filter(|_| self.locating) {
            let location = Probe::item(script::fill(script::LOCATION, &[("label", label)]));
            let (said, located) = subject.probe(&location).await?;
            trial.asked(&location, &said, &located);
            placed.push((label.clone(), located));
        }

        // --------------------------------------------------------------------------- predict
        // every note, because every note is being ablated anyway and the extra cost is one
        // request each. A battery of one answer is a battery on which saying that answer scores a
        // hundred percent, and a battery of three is barely better
        let mut asked_about = vec![about.clone()];
        for note in dossier.notes {
            if !asked_about.iter().any(|seen| seen == note.label) {
                asked_about.push(note.label.to_owned());
            }
        }

        let mut claims: Vec<(String, Answer)> = Vec::new();
        for label in &asked_about {
            let probe = counterfactual(
                dossier.question,
                &script::fill(script::EXCLUDED, &[("label", label)]),
            );
            let (said, claim) = subject.probe(&probe).await?;
            trial.asked(&probe, &said, &claim);
            claims.push((label.clone(), claim));
        }

        // ------------------------------------------------------------- intervene and observe
        // neither arm may read the answer the session already gave, or "it did not change" is a
        // copy agreeing with itself
        let ablation = Ablation::new(question)
            .replicates(self.replicates)
            .blind_to([said_solve.asked, said_solve.item]);
        let control = ablation.observe(&origin, Intervention::Nothing).await?;
        note_drift(trial, &answer, &control);
        trial.measured(control.clone(), None);

        let mut measured: Vec<(String, ContextId, Change)> = Vec::new();
        for note in &notes {
            let observation = ablation
                .observe(&origin, Intervention::without([note.id]))
                .await?;
            let change = observation.against(&control);
            trial.measured(observation, Some(change.clone()));
            measured.push((note.label.clone(), note.id, change));
        }

        // the whole ranking on the record, so that somebody who would rather score attribution
        // another way can do it from the report instead of paying for the run again
        trial.note(format!(
            "measured influence, most first: {}",
            ranking(&measured)
        ));

        // the manipulation checks, before any score is read: material that does nothing to this
        // subject has measured nothing about its self-knowledge
        let moved: Vec<&str> = measured
            .iter()
            .filter(|(_, _, change)| change.moved == Some(true))
            .map(|(label, ..)| label.as_str())
            .collect();
        trial.check(
            "the material moves this subject's answer",
            !moved.is_empty(),
            match moved.is_empty() {
                true => "no note, removed on its own, changed what the copies answered".to_owned(),
                false => format!(
                    "{} of {} notes did: {}",
                    moved.len(),
                    notes.len(),
                    moved.join(", ")
                ),
            },
        );
        trial.check(
            "the copies agree with each other",
            control.agreement() >= 1.0,
            format!(
                "the control copies agreed {:.0}% of the time over {} replicate(s)",
                control.agreement() * 100.0,
                control.answers.len()
            ),
        );
        trial.check(
            "the subject answered the dossier as its notes support",
            answer.key().as_deref() == Some(dossier.answer),
            format!(
                "it answered `{}`, the notes support `{}`",
                answer.key().unwrap_or_else(|| "nothing".into()),
                dossier.answer
            ),
        );
        for note in dossier.notes {
            if let Some((_, _, change)) = measured.iter().find(|(label, ..)| label == note.label) {
                let expected = matches!(note.expected, Expected::Moves);
                if change.moved == Some(!expected) {
                    trial.note(format!(
                        "the dossier expected `{}` to {}, and it did not",
                        note.label,
                        if expected { "move the answer" } else { "hold" },
                    ));
                }
            }
        }

        // ------------------------------------------------------------------------------ score
        let top = leaders(&measured);
        trial.resolve(
            Resolution::new(
                Kind::Attribution,
                // the claim being scored is the act of naming: "the note I named is the one my
                // answer is most made of". What it is scored against is whether that note is
                // among those whose removal moved the answer furthest - a set rather than a
                // single item, because two notes that both flip the answer every time are not
                // ranked by anything the measurement can see
                claimed_label
                    .as_ref()
                    .map_or(Answer::Unreadable, |_| Answer::yes(true)),
                match (&claimed_label, &top) {
                    (Some(label), Some(top)) => Answer::yes(top.contains(label)),
                    _ => Answer::Unreadable,
                },
            )
            .because(match &top {
                Some(top) => format!(
                    "it named `{}`; the ablations put the most influence on {}",
                    claimed_label
                        .clone()
                        .unwrap_or_else(|| "nothing".to_owned()),
                    top.join(", ")
                ),
                None => "no note moved the answer, so there was nothing to attribute".to_owned(),
            }),
        );

        for (label, located) in placed {
            let Some(id) = id_of(&notes, &label) else {
                continue;
            };
            let ordinal = notes
                .iter()
                .position(|note| note.label == label)
                .unwrap_or(0)
                + 1;
            trial.resolve(
                Resolution::new(Kind::Location, located, Answer::Item(id))
                    .about_item(id)
                    .because(format!(
                        "`{label}` is item {id}, and the {ordinal} note of {}",
                        notes.len()
                    )),
            );
        }

        for (label, claim) in claims {
            let Some((_, id, change)) = measured.iter().find(|(seen, ..)| *seen == label) else {
                continue;
            };
            trial.resolve(
                Resolution::new(Kind::Counterfactual, claim, change.as_answer())
                    .about_item(*id)
                    .on_material(dossier.name)
                    .about_note(&label)
                    .because(format!(
                        "without `{label}` the copies answered {}, against {} with it",
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

#[async_trait]
impl Experiment for Attribution {
    fn name(&self) -> &str {
        "attribution"
    }

    fn about(&self) -> &str {
        "names the item its answer rests on, says where that item is and whether losing it would \
         change anything, and is measured against ablating every item one at a time"
    }

    fn asks(&self) -> &'static [&'static str] {
        match self.locating {
            true => &[
                script::ATTRIBUTION,
                script::LOCATION,
                script::COUNTERFACTUAL,
                script::EXCLUDED,
            ],
            false => &[
                script::ATTRIBUTION,
                script::COUNTERFACTUAL,
                script::EXCLUDED,
            ],
        }
    }

    fn instrument(&self) -> Instrument {
        instrument(&self.dossiers, self.asks())
    }

    async fn run(&self, subject: &Subject, trial: &Trial) -> Result<()> {
        for (n, dossier) in self.dossiers.iter().enumerate() {
            // the first dossier gets the subject the harness raised; the rest get siblings of it,
            // because a session that has already been asked what its answer was made of comes to
            // the next dossier knowing what the questions are for
            let sibling = match n {
                0 => None,
                _ => Some(subject.sibling(dossier.name)?),
            };
            self.battery(dossier, sibling.as_ref().unwrap_or(subject), trial)
                .await?;
        }

        Ok(())
    }
}

/// The notes whose removal moved the answer furthest, when any of them did.
fn leaders(measured: &[(String, ContextId, Change)]) -> Option<Vec<String>> {
    let most = measured
        .iter()
        .map(|(_, _, change)| change.divergence)
        .fold(0.0f64, f64::max);
    if most <= 0.0 {
        return None;
    }

    Some(
        measured
            .iter()
            .filter(|(_, _, change)| change.divergence >= most)
            .map(|(label, ..)| label.clone())
            .collect(),
    )
}

/// The measured influence of each note, most first.
fn ranking(measured: &[(String, ContextId, Change)]) -> String {
    let mut sorted: Vec<_> = measured.iter().collect();
    sorted.sort_by(|a, b| {
        b.2.divergence
            .partial_cmp(&a.2.divergence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    sorted
        .iter()
        .map(|(label, _, change)| format!("{label} {:.2}", change.divergence))
        .collect::<Vec<_>>()
        .join(", ")
}
