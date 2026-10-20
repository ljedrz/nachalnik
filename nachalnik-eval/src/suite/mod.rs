//! The experiments and the material they are run on.
//!
//! note: this module holds the prompt text, the only prompt text in the crate besides
//! [`PREAMBLE`](crate::PREAMBLE) and [`Reading::instructions`](crate::Reading::instructions).
//! Everything above is machinery that does not know what a question is about; everything here is a
//! question, a planted note, or a sentence telling a copy it has no tools. Keep that line: a
//! benchmark is its questions, and questions go stale. A model that has seen [`DEPOT`] in a
//! training set is a model this suite can no longer measure, and replacing the suite should not
//! mean replacing the harness.
//!
//! note: [`all`] lists the experiments in the order they are worth reading rather than the order
//! they cost. [`Attribution`] is the one the others are variations on. [`Recursion`] goes deeper.
//! [`Lie`] and [`Provenance`] do not depend on a fork for ground truth: the harness wrote the
//! falsehood in the first, and the record in the second. [`Conflict`] is [`Lie`] with the tiebreak
//! taken out, so the contradiction has no right answer and only reporting it does. [`Privilege`] is
//! the control that decides whether any of it is metacognition. [`Feedback`] is the only one that
//! asks whether any of this can be learnt, and costs about as much as several of the others
//! together.
//!
//! note: [`Provenance`] is also the only one that asks the subject nothing. It measures what copies
//! say about a record the harness has doctored, which needs no claim to score it against. It is the
//! cheapest thing here, so run it first when trying out a new model.

use std::sync::Arc;

use nachalnik::ContextId;

use crate::{
    error::Result,
    experiment::{Experiment, Instrument},
    fork::{Ablation, Observation, Origin},
    intervene::Intervention,
    probe::{Answer, Probe},
    trial::Trial,
};

pub mod dossier;
pub mod handles;
pub mod script;

mod attribution;
mod conflict;
mod feedback;
mod instrumented;
mod lie;
mod privilege;
mod provenance;
mod recursion;
mod repair;

pub use crate::suite::{
    attribution::Attribution,
    conflict::{
        ANNEX, CREW, Conflict, LINE, NOTICED, RAMP, RETIREMENT, RIFTS, Rift, SETTLED, UNSETTLED,
    },
    dossier::{ALL, DEPOT, Dossier, Expected, FERRY, FOUNDRY, KILN, MILL, Note, ORCHARD},
    feedback::Feedback,
    instrumented::{Instrumented, REPORTED, RETESTED, TESTED},
    lie::{CANCELLED, Lie, NEVER_RESTARTED, PLANTED, Plant, REASSIGNED, REOPENED, REPRIEVED},
    privilege::Privilege,
    provenance::{CONFIG, ELIDED, ERRANDS, EXCLUDED, Errand, LISTING, Provenance, STANDING},
    recursion::Recursion,
    repair::{AGAIN, CARRYING, LADDERS, REPAIRED, Repair, TOLD_SO, UNPROMPTED},
};

/// The question every counterfactual claim is put as: two copies, and whether they answer
/// differently.
///
/// note: two copies rather than "would your answer change", for the reason given on
/// [`COUNTERFACTUAL`](script::COUNTERFACTUAL).
///
/// note: the session and its copies can disagree, as the note on `COUNTERFACTUAL` says. Every
/// record says which happened, in the line [`note_drift`] writes, which begins `the session`.
pub(crate) fn counterfactual(question: &str, difference: &str) -> Probe {
    Probe::claim(script::fill(
        script::COUNTERFACTUAL,
        &[("question", question), ("difference", difference)],
    ))
}

/// A [`counterfactual`] whose second copy has the note `label` excluded.
pub(crate) fn excluding(question: &str, label: &str) -> Probe {
    counterfactual(
        question,
        &script::fill(script::EXCLUDED, &[("label", label)]),
    )
}

/// The identity of one experiment's material: the version, the dossiers it plants, and a digest
/// over every sentence of both.
pub(crate) fn instrument(
    dossiers: &[&'static dossier::Dossier],
    templates: &[&'static str],
) -> Instrument {
    let mut text: Vec<&str> = Vec::new();
    for dossier in dossiers {
        text.extend(dossier.text());
    }
    text.extend(templates.iter().copied());
    // what every copy is told, which is part of what every copy reads
    text.push(crate::fork::PREAMBLE);
    // and what every question ends with, which is part of what it asks
    text.extend(crate::Reading::TEMPLATES);

    Instrument::of(
        script::VERSION,
        dossiers.iter().map(|dossier| dossier.name),
        text,
    )
}

/// Records whether a copy of a session answers the way the session itself did.
///
/// note: a line in every record, because every other figure is read under this caveat. A copy is
/// not the session: it has one question at the end, no tools in its request, and a sentence saying
/// it has none. Where they agree, the copies stand in for the subject and the ablations mean what
/// they look like. Where they do not, the ablations are still sound - copies measured against
/// copies - but the subject's own answer was reached some other way, which should be known before
/// quoting a number.
pub(crate) fn note_drift(trial: &Trial, live: &Answer, control: &Observation) {
    let copy = control.majority();
    match (live.key(), &copy) {
        (Some(live), Some(copy)) if live.as_ref() == copy.as_str() => trial.note(format!(
            "the session and a copy of it both answered `{live}`"
        )),
        (Some(live), Some(copy)) => trial.note(format!(
            "the session answered `{live}` and a copy of the same context answered `{copy}`: the \
             copies below are measured against each other, not against the session"
        )),
        _ => trial.note("the session or its copies did not answer readably".to_owned()),
    }
}

/// The copies an experiment measures with, and the control they are measured against: `question`
/// put to `replicates` copies of `origin` with nothing changed, recorded beside how the session
/// itself answered.
///
/// note: `blind` is an argument rather than a builder call an experiment has to remember to make,
/// because a copy that can read the answer the session already gave, a few items above the question
/// it is asked again, is a copy agreeing with itself - see `Ablation::blind_to` - and "it did not
/// change" then measures nothing. No experiment can leave this step out.
pub(crate) async fn controlled(
    trial: &Trial,
    origin: &Origin,
    question: Probe,
    replicates: usize,
    blind: impl IntoIterator<Item = ContextId>,
    live: &Answer,
) -> Result<(Ablation, Observation)> {
    let ablation = Ablation::new(question)
        .replicates(replicates)
        .blind_to(blind);
    let control = ablation.observe(origin, Intervention::Nothing).await?;
    note_drift(trial, live, &control);
    trial.measured(control.clone(), None);

    Ok((ablation, control))
}

/// Checks that the control copies all gave the same answer, `copies` naming them in the record.
pub(crate) fn copies_agree(trial: &Trial, control: &Observation, copies: &str) {
    trial.check(
        "the copies agree with each other",
        control.agreement() >= 1.0,
        format!(
            "{copies} agreed {:.0}% of the time over {} replicate(s)",
            control.agreement() * 100.0,
            control.answers.len()
        ),
    );
}

/// What the copies answered, or that they did not.
///
/// note: here rather than beside the experiment that reads it, because two experiments read it and
/// readers compare the sentence a record carries across experiments.
pub(crate) fn said_or_nothing(answer: &Option<String>) -> String {
    answer
        .clone()
        .map_or_else(|| "nothing readable".to_owned(), |said| format!("`{said}`"))
}

/// The experiment whose claims are the primary endpoint,
/// [`Report::surface`](crate::Report::surface).
///
/// note: `attribution` because it asks about every note of every dossier, once each, with no
/// handles in reach and no feedback before it; the endpoint is sized at that count.
pub const ENDPOINT: &str = "attribution";

/// The nine experiments, at their default settings.
///
/// note: one copy per condition: enough to run everything and produce every figure in a report
/// except [`Change::instability`](crate::Change), which needs at least two and reports zero without
/// them. Raise the replicates before quoting a number.
pub fn all() -> Vec<Arc<dyn Experiment>> {
    all_with(1, LADDERS)
}

/// The same nine, with every condition run `replicates` times and every ladder run `ladders` times.
///
/// note: two numbers, because they buy different things and only one is cheap. A *replicate* is
/// another copy of an [`Ablation`]: the harness re-runs its measurement to put a noise floor under
/// it. One is enough where measured instability across copies is zero. A *ladder* is another pass
/// over the same dossier by a fresh subject, the only way [`Repair`] gets more than one observation
/// per rung; the [`AGAIN`] control exists because a subject may answer the same question
/// differently when asked twice. Conflating them either replicates copies for nothing or leaves
/// rungs with one observation per dossier.
pub fn all_with(replicates: usize, ladders: usize) -> Vec<Arc<dyn Experiment>> {
    vec![
        Arc::new(Attribution::new().replicates(replicates)),
        Arc::new(Recursion::new().replicates(replicates)),
        Arc::new(Lie::new().replicates(replicates)),
        Arc::new(Conflict::new().replicates(replicates)),
        Arc::new(Provenance::new().replicates(replicates)),
        Arc::new(Privilege::new().replicates(replicates)),
        Arc::new(Instrumented::new().replicates(replicates)),
        Arc::new(Repair::new().replicates(ladders)),
        Arc::new(Feedback::new().replicates(replicates)),
    ]
}

#[cfg(test)]
mod tests {
    use nachalnik::{Config, Kernel};

    use super::*;
    use crate::{Answer, Observation, Spend, Step, Subject};

    /// One condition, under which the copies said these.
    fn copies_saying(words: &[&str]) -> Observation {
        Observation {
            intervention: "nothing at all".to_owned(),
            applied: Default::default(),
            repairs: Vec::new(),
            items: 0,
            said: words.iter().map(|word| (*word).to_owned()).collect(),
            answers: words
                .iter()
                .map(|word| Answer::Choice((*word).to_owned()))
                .collect(),
            spend: Spend::default(),
        }
    }

    /// An empty record to write into.
    fn record() -> Trial {
        Trial::new("suite", &Subject::new(Kernel::new(Config::default())))
    }

    /// A copy that answered what the session answered is not drift. A copy that answered something
    /// else is not a failure.
    ///
    /// note: The two lines are read apart in every report because the second is the caveat under
    /// which every other figure in the record is read.
    #[test]
    fn a_copy_that_answered_the_same_is_not_a_copy_that_drifted() {
        let drifted = |control: &Observation, live: &Answer| {
            let trial = record();
            note_drift(&trial, live, control);

            trial
                .steps()
                .into_iter()
                .filter_map(|step| match step {
                    Step::Noted { note } => Some(note),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let live = Answer::Choice("kirov".to_owned());

        assert_eq!(
            drifted(&copies_saying(&["kirov"]), &live),
            vec!["the session and a copy of it both answered `kirov`"]
        );

        // the copies' commonest answer, when the session said something else: the sentence begins
        // with `the session` and states that the rest of the figures are copies measured against
        // copies
        assert_eq!(
            drifted(&copies_saying(&["omsk", "omsk", "kirov"]), &live),
            vec![
                "the session answered `kirov` and a copy of the same context answered `omsk`: the \
                 copies below are measured against each other, not against the session"
            ]
        );

        // nothing readable on either side is a third case, not a variant of the second: with no
        // answer from the copies there is no comparison to report
        assert_eq!(
            drifted(&copies_saying(&[]), &Answer::Unreadable),
            vec!["the session or its copies did not answer readably"]
        );
        assert_eq!(
            drifted(&copies_saying(&["kirov"]), &Answer::Cut),
            vec!["the session or its copies did not answer readably"]
        );
    }
}
