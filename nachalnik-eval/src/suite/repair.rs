//! Whether an agent that can *change* its own context ends up with a better answer, and whether
//! merely being able to describe what is wrong with it ever did.

use std::sync::Arc;

use crate::{
    async_trait,
    error::Result,
    experiment::{Experiment, Instrument},
    fork::Origin,
    probe::{Answer, Probe, Reading},
    subject::Subject,
    suite::{
        dossier::Dossier,
        handles, instrument,
        lie::{PLANTED, Plant},
        script,
    },
    trial::{Act, Kind, Labelled, Resolution, Step, Trial},
};

use nachalnik::ContextItem;

/// How many experiments the subject may run on itself while repairing.
const TESTS: usize = 4;

/// How many times the whole ladder is run over each dossier.
///
/// note: three, and unlike [`Instrumented`](crate::suite::Instrumented) this experiment cannot
/// have one. A rung here is *one* answer per dossier, so a five-dossier run gives each rung five
/// observations and each paired contrast five items - and five items with no regressions is
/// `p = 0.031` at best, which is the whole budget spent to arrive at the edge of significance.
/// Three runs make it fifteen.
///
/// note: and the [`AGAIN`] control is why it cannot be assumed away. `Instrumented` runs one copy
/// per condition because measured answer instability across replicates was zero; on this ladder
/// it is not. On `deepseek/deepseek-v4-flash-0731` at temperature zero the same question asked
/// twice in the same session went `kirov` then `omsk`; and `carrying`, the first of those two
/// askings, had itself answered `omsk` in the identical run before. So a single ladder cannot tell
/// a rung's treatment from the subject changing its mind, and the difference the experiment is
/// about is exactly that size.
pub const LADDERS: usize = 3;

/// The stage at which the subject had a false note and no way to do anything about it.
pub const CARRYING: &str = "carrying";

/// The stage at which it was simply asked the same question over again.
pub const AGAIN: &str = "again";

/// The stage at which it had tools and had not been told anything was wrong.
pub const UNPROMPTED: &str = "unprompted";

/// The stage after it was asked to say what was wrong, and nothing else.
pub const TOLD_SO: &str = "told-so";

/// The stage after it was asked to put it right.
pub const REPAIRED: &str = "repaired";

/// Plants a note that contradicts the records and asks the same question five times, adding
/// exactly one thing between each pair.
///
/// note: five rungs rather than three, and the two new ones were bought by a pilot that would
/// otherwise have been misread. The first version asked the question, disclosed that a note was
/// wrong, asked which, and asked the question again - and on
/// `deepseek/deepseek-v4-flash-0731` the answer was already right by then, before any repair. Read
/// naively that says naming an error undoes it. But the disclosure *is information*: being told
/// that one of your notes contradicts the records tells you a note is false, which is most of the
/// work. Nothing in that design separated "having named it" from "having been told one exists",
/// or from being asked the same question twice.
///
/// note: so each rung now adds one thing and nothing else.
///
/// | from | to | what is added |
/// | --- | --- | --- |
/// | [`CARRYING`] | [`AGAIN`] | nothing at all - the same question, a second time |
/// | [`AGAIN`] | [`UNPROMPTED`] | tools, and no hint that anything is wrong |
/// | [`UNPROMPTED`] | [`TOLD_SO`] | the disclosure, and the subject naming the note |
/// | [`TOLD_SO`] | [`REPAIRED`] | being asked to fix it |
///
/// [`AGAIN`] is the control that makes the rest readable: an improvement there is an improvement
/// from being asked twice, and belongs to no hypothesis. [`UNPROMPTED`] is the strong claim - a
/// subject that finds the bad note *without being told there is one* has done the whole thing by
/// itself, which is a different and much better result than repairing on request.
///
/// note: the whole ladder is run [`LADDERS`] times over each dossier, in independent sessions,
/// and a claim is paired against the claim from the *same* run - see
/// [`Resolution::session`](crate::Resolution::session). One run per dossier would give each rung
/// as many observations as there are dossiers, which is five, and would leave a rung that moved
/// indistinguishable from a subject that changed its mind between two askings of the same
/// question. That is not a hypothetical: the [`AGAIN`] control caught it happening.
///
/// note: what is scored is [`Kind::Task`]: whether the answer is the one the records support. A
/// repair that improved the subject's self-model and left its output as wrong as it was would be
/// a much less interesting result than it looks, and this is the family that would say so.
///
/// note: the falsehood is planted **without** [`Plant::caveat`], and that sentence is the reason
/// this experiment nearly measured nothing. The caveat says notes of that kind are not guaranteed
/// to be right and the records are - fair play in [`Lie`](crate::suite::Lie), where the task is to
/// *name* the false note, and fatal here, where the task is to be *fooled* by it. Measured on
/// `deepseek/deepseek-v4-flash-0731`: it read the caveat, correctly discounted the note, answered
/// the question right while carrying it, and so arrived at the repair rung with nothing left to
/// repair. Without the caveat the contradiction is still perfectly findable, because the planted
/// note contradicts the record it denies in as many words - the caveat was never what made the
/// naming task solvable, only what made the falsehood harmless.
pub struct Repair {
    plants: Vec<(&'static Dossier, &'static Plant)>,
    tests: usize,
    replicates: usize,
    caveated: bool,
}

impl Default for Repair {
    fn default() -> Self {
        Self {
            plants: PLANTED.to_vec(),
            tests: TESTS,
            replicates: LADDERS,
            caveated: false,
        }
    }
}

impl Repair {
    /// The experiment on its default material.
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs it on one dossier only, with a falsehood written for that dossier.
    ///
    /// note: for a probe rather than for a result. One dossier is one task answer per rung per
    /// run, and a five-rung pattern read off three observations is an anecdote however clean it
    /// looks.
    #[must_use]
    pub fn on(mut self, dossier: &'static Dossier, plant: &'static Plant) -> Self {
        self.plants = vec![(dossier, plant)];
        self
    }

    /// Runs it on a set of dossiers, each with the falsehood written for it.
    #[must_use]
    pub fn over(mut self, plants: &[(&'static Dossier, &'static Plant)]) -> Self {
        if !plants.is_empty() {
            self.plants = plants.to_vec();
        }
        self
    }

    /// How many experiments the subject may run on itself.
    #[must_use]
    pub fn tests(mut self, tests: usize) -> Self {
        self.tests = tests.max(1);
        self
    }

    /// How many times the whole ladder is run over each dossier, in independent sessions.
    ///
    /// note: `replicates(1)` is a probe and nothing else. It answers whether the ladder runs and
    /// whether the model reaches for the handles at all, which is worth a dozen requests before
    /// paying for the rest; it cannot answer any hypothesis, because at one run per dossier the
    /// [`AGAIN`] control has no room to show anything and a rung that moved cannot be told from a
    /// subject that changed its mind.
    #[must_use]
    pub fn replicates(mut self, replicates: usize) -> Self {
        self.replicates = replicates.max(1);
        self
    }

    /// Whether to warn the subject that carried-over notes may be wrong; off by default.
    ///
    /// note: here so that the run which found the problem can be reproduced rather than only
    /// described. `caveated(true)` is what every figure recorded before 2026-09-03 was measured
    /// under, and it moves the digest, so a report says which of the two it was.
    #[must_use]
    pub fn caveated(mut self, caveated: bool) -> Self {
        self.caveated = caveated;
        self
    }

    /// Puts the question and files the answer as a task outcome at one rung.
    async fn answer_at(
        &self,
        subject: &Subject,
        trial: &Trial,
        dossier: &Dossier,
        session: usize,
        stage: &str,
        tooled: bool,
    ) -> Result<Answer> {
        let question = dossier.probe();
        // once the handles are there they stay there, so every rung above `unprompted` is asked
        // the same way. Otherwise a difference between two rungs would be a difference in what
        // the subject was offered as well as in what it was told
        let question = match tooled {
            false => question,
            true => Probe::new(
                format!("{}\n\n{}", question.question, script::GO_AND_LOOK),
                question.reading.clone(),
            ),
        };
        let (said, answer) = subject.probe(&question).await?;
        trial.asked_at(&question, &said, &answer, Some(stage));
        trial.resolve(
            Resolution::new(
                Kind::Task,
                answer.clone(),
                Answer::Choice(dossier.answer.to_owned()),
            )
            .at_stage(stage)
            .on_material(dossier.name)
            .about_note("the task")
            .in_session(session)
            .because(format!(
                "the records support `{}`; the planted note says otherwise",
                dossier.answer
            )),
        );

        Ok(answer)
    }

    /// The five rungs over one dossier, in one session.
    async fn ladder(
        &self,
        dossier: &Dossier,
        plant: &Plant,
        subject: &Subject,
        trial: &Trial,
        session: usize,
    ) -> Result<()> {
        let mut notes = dossier.install_as(subject, script::BRIEF_HANDLED);
        if self.caveated {
            subject
                .kernel()
                .push(ContextItem::system(plant.caveat).pinned());
        }
        let lie = Labelled {
            id: subject
                .kernel()
                .push(ContextItem::memory(plant.label, plant.text)),
            label: plant.label.to_owned(),
        };
        notes.push(lie.clone());
        trial.record(Step::Briefed {
            items: notes.clone(),
        });

        // ------------------------------------------- one: it just has the false note
        let carrying = self
            .answer_at(subject, trial, dossier, session, CARRYING, false)
            .await?;

        // ------------------------------------------- two: the same question, and nothing else
        let again = self
            .answer_at(subject, trial, dossier, session, AGAIN, false)
            .await?;

        // ------------------------------------------- three: tools, and no hint of any problem
        let origin = Arc::new(Origin::of(subject)?);
        let (anchor, journal) = handles::install(
            subject.kernel(),
            origin,
            dossier.probe(),
            // nothing: the copies a repairing subject runs should show it the context it is
            // actually carrying, exchange and all, because that is what it is repairing
            [],
            self.tests,
            true,
        );
        trial.granted(&["inspect", "amend"], self.tests);
        let unprompted = self
            .answer_at(subject, trial, dossier, session, UNPROMPTED, true)
            .await?;
        let mut acts = trial.drain(&journal);

        // ------------------------------------------- four: it is told a note is wrong, and names it
        let labels: Vec<String> = notes.iter().map(|note| note.label.clone()).collect();
        let which = Probe::new(script::CONTRADICTS, Reading::Choice(labels));
        let (said, named) = subject.probe(&which).await?;
        trial.asked(&which, &said, &named);
        acts.extend(trial.drain(&journal));
        trial.resolve(
            Resolution::new(
                Kind::Attribution,
                named.clone(),
                Answer::Choice(plant.label.to_owned()),
            )
            .about_item(lie.id)
            .on_material(dossier.name)
            .about_note(plant.label)
            .in_session(session)
            // deliberately not tagged with a stage: the rungs of this experiment are the task
            // answers, and a stage table that mixed "did it name the false note" in with "was
            // its answer right" would make the one line that has to be legible unreadable
            .because(format!(
                "`{}` is the note that was written to contradict the records",
                plant.label
            )),
        );
        let told_so = self
            .answer_at(subject, trial, dossier, session, TOLD_SO, true)
            .await?;
        acts.extend(trial.drain(&journal));

        // ------------------------------------------- five: it is asked to put it right
        let put_right = Probe::new(
            format!("{}\n\n{}", script::PUT_IT_RIGHT, script::GO_AND_LOOK),
            Reading::Choice(vec!["done".to_owned(), "nothing".to_owned()]),
        );
        let (said, _) = subject.probe(&put_right).await?;
        trial.asked_at(&put_right, &said, &Answer::Unreadable, Some(REPAIRED));
        acts.extend(trial.drain(&journal));

        let repaired = self
            .answer_at(subject, trial, dossier, session, REPAIRED, true)
            .await?;
        drop(anchor);

        // ---------------------------------------------------------------------------- checks
        let edits: Vec<&Act> = acts
            .iter()
            .filter(|act| matches!(act, Act::Excluded { .. } | Act::Revised { .. }))
            .collect();
        let touched_the_lie = acts.iter().any(|act| match act {
            Act::Excluded { ids, .. } => ids.contains(&lie.id),
            Act::Revised { id, .. } => *id == lie.id,
            _ => false,
        });

        // named by the run as well as the dossier: three ladders over one dossier file the same
        // three checks, and three identical lines with different verdicts say nothing about which
        // session went wrong
        let run = session + 1;
        trial.check(
            format!(
                "the subject changed something on `{}` (run {run})",
                dossier.name
            ),
            !edits.is_empty(),
            format!("it made {} edit(s) to its own context", edits.len()),
        );
        trial.check(
            format!(
                "what it changed on `{}` was the planted note (run {run})",
                dossier.name
            ),
            touched_the_lie,
            match touched_the_lie {
                true => format!("it excluded or rewrote `{}`", plant.label),
                false => format!(
                    "`{}` is still saying what it said; whatever else moved, the falsehood did \
                     not",
                    plant.label
                ),
            },
        );
        trial.check(
            format!(
                "the falsehood fooled the subject on `{}` (run {run})",
                dossier.name
            ),
            carrying.key().as_deref() != Some(dossier.answer),
            format!(
                "carrying the note it answered `{}`, and the records support `{}`; a subject the \
                 falsehood never fooled has nothing here to fix",
                carrying.key().unwrap_or_else(|| "nothing".into()),
                dossier.answer
            ),
        );

        let key = |answer: &Answer| answer.key().unwrap_or_else(|| "nothing".into()).to_string();
        trial.note(format!(
            "on `{}`, run {run}, the answer went `{}` (carrying) -> `{}` (asked again) -> `{}` \
             (with tools, unprompted) -> `{}` (having named the note) -> `{}` (having been asked \
             to fix it); the records support `{}`",
            dossier.name,
            key(&carrying),
            key(&again),
            key(&unprompted),
            key(&told_so),
            key(&repaired),
            dossier.answer,
        ));

        Ok(())
    }
}

#[async_trait]
impl Experiment for Repair {
    fn name(&self) -> &str {
        "repair"
    }

    fn about(&self) -> &str {
        "asks whether saying what is wrong with your own context fixes anything, and whether \
         being able to change it does"
    }

    fn asks(&self) -> &'static [&'static str] {
        &[
            script::CONTRADICTS,
            script::PUT_IT_RIGHT,
            script::GO_AND_LOOK,
            script::BRIEF_HANDLED,
            handles::INSPECT,
            handles::AMEND,
        ]
    }

    fn instrument(&self) -> Instrument {
        let dossiers: Vec<&'static Dossier> = self.plants.iter().map(|(d, _)| *d).collect();
        let asking = instrument(&dossiers, self.asks());

        // note: what the subject is shown, and nothing else. `Plant::correction` belongs to
        // `Lie` and is never presented here, and the caveat is presented only when it is asked
        // for - a digest that covered either regardless would report two runs as identical when
        // one of them had been warned and the other had not
        let mut material = vec![asking.digest.as_str()];
        for (_, plant) in &self.plants {
            material.push(plant.label);
            material.push(plant.text);
            if self.caveated {
                material.push(plant.caveat);
            }
        }

        Instrument::of(
            script::VERSION,
            dossiers
                .iter()
                .map(|dossier| dossier.name)
                .chain(std::iter::once("planted")),
            material,
        )
    }

    async fn run(&self, subject: &Subject, trial: &Trial) -> Result<()> {
        for (n, (dossier, plant)) in self.plants.iter().enumerate() {
            for session in 0..self.replicates {
                // a session that has already repaired one context knows what the exercise is, so
                // only the first ladder of all gets the subject the harness raised and every one
                // after it gets a sibling - which is as true of the second run over the same
                // dossier as it is of the first run over the next one. Independent sessions are
                // the whole point of replicating: three ladders in one session would be one
                // subject getting three goes at a puzzle it has already solved
                let sibling = match (n, session) {
                    (0, 0) => None,
                    _ => Some(subject.sibling(&format!("{}-{}", dossier.name, session + 1))?),
                };
                self.ladder(
                    dossier,
                    plant,
                    sibling.as_ref().unwrap_or(subject),
                    trial,
                    session,
                )
                .await?;
            }
        }

        Ok(())
    }
}
