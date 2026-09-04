//! The experiments, and the material they are run on.
//!
//! note: This module is where the prompt text is, and it is the only place in the crate that has
//! any. Everything above it is machinery that does not know what a question is about; everything
//! in here is a question, a planted note, or a sentence explaining to a copy that it has no
//! tools. The line between them is worth keeping: a benchmark is its questions, and questions go
//! stale - a model that has seen [`DEPOT`] in a training set is a model this
//! suite can no longer measure, and replacing the suite should not mean replacing the harness.
//!
//! note: The four are in the order they are worth reading rather than in the order they cost.
//! [`Attribution`] is the one the other three are variations on; [`Recursion`] is the one that
//! goes deeper; [`Lie`] is the one whose ground truth does not depend on a fork at all; and
//! [`Feedback`] is the only one that asks whether any of this can be learnt, and costs about as
//! much as the other three together.

use std::sync::Arc;

use crate::{
    experiment::{Experiment, Instrument},
    fork::Observation,
    probe::{Answer, Probe},
    trial::Trial,
};

pub mod dossier;
pub mod handles;
pub mod script;

mod attribution;
mod feedback;
mod instrumented;
mod lie;
mod privilege;
mod recursion;
mod repair;

pub use crate::suite::{
    attribution::Attribution,
    dossier::{ALL, DEPOT, Dossier, Expected, FERRY, FOUNDRY, KILN, MILL, Note, ORCHARD},
    feedback::Feedback,
    instrumented::{Instrumented, REPORTED, RETESTED, TESTED},
    lie::{CANCELLED, Lie, NEVER_RESTARTED, PLANTED, Plant, REASSIGNED, REOPENED, REPRIEVED},
    privilege::Privilege,
    recursion::Recursion,
    repair::{AGAIN, CARRYING, LADDERS, REPAIRED, Repair, TOLD_SO, UNPROMPTED},
};

/// The question every counterfactual claim in the suite is put as: two copies, and whether they
/// will answer differently.
///
/// note: Two copies rather than "would *your* answer change", and the difference is not pedantry.
/// What the harness measures is a treated copy against a control copy - it has to be, or the
/// intervention is confounded with the whole business of being a copy at all - so that is what
/// the question has to ask about. Asked the other way, a subject can be exactly right about how
/// the two copies will answer and be scored wrong because the live session, which has the
/// elicitation in its context and its tools in its request, answered differently from both.
///
/// note: found by a real model rather than by reasoning: `gemini-3.7-flash` answered a dossier
/// correctly, a copy of the same context answered it wrongly, and the claim in between was
/// graded against a baseline it had never been shown. See the saved runs of any study, or run
/// the `lie` experiment and read `it answered X, and a copy of it answered Y` in the record.
pub(crate) fn counterfactual(question: &str, difference: &str) -> Probe {
    Probe::claim(script::fill(
        script::COUNTERFACTUAL,
        &[("question", question), ("difference", difference)],
    ))
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

    Instrument::of(
        script::VERSION,
        dossiers.iter().map(|dossier| dossier.name),
        text,
    )
}

/// Records whether a copy of a session answers the way the session itself did.
///
/// note: Worth a line in every record, because it is the caveat every other figure is read
/// under. A copy is not the session: it has one question at the end of it, no tools in its
/// request, and a sentence explaining that it has none. Where the two agree, the copies are
/// standing in for the subject and the ablations mean what they look like. Where they do not,
/// the ablations are still sound - they are copies measured against copies - and the subject's
/// own answer was reached some other way, which is a thing worth knowing before quoting a
/// number at anybody.
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

/// The seven experiments, at their default settings.
///
/// note: One copy per condition, which is the cheap end. It is enough to run the whole thing for
/// about sixty requests and enough to produce every figure in a report; it is *not* enough for
/// [`Change::instability`](crate::Change), which needs at least two and is reported as zero
/// without them. Raise the replicates before quoting a number at anybody.
pub fn all() -> Vec<Arc<dyn Experiment>> {
    vec![
        Arc::new(Attribution::new()),
        Arc::new(Recursion::new()),
        Arc::new(Lie::new()),
        Arc::new(Privilege::new()),
        Arc::new(Instrumented::new()),
        Arc::new(Repair::new()),
        Arc::new(Feedback::new()),
    ]
}

/// The same seven, with every condition run `replicates` times and every ladder run `ladders`
/// times.
///
/// note: two numbers, because they buy different things and only one of them is cheap. A
/// *replicate* is another copy of an [`Ablation`](crate::Ablation) - the harness re-running its
/// own measurement to put a noise floor under it - and the preregistration justifies one, because
/// measured instability across copies was zero. A *ladder* is another pass over the same dossier
/// by a fresh subject, which is the only way [`Repair`] gets more than one observation per rung,
/// and the [`AGAIN`] control has already shown that a subject asked the same question twice does
/// not always answer it the same way. Conflating them would either replicate the copies three
/// times for nothing or leave the rungs with five observations each.
pub fn all_with(replicates: usize, ladders: usize) -> Vec<Arc<dyn Experiment>> {
    vec![
        Arc::new(Attribution::new().replicates(replicates)),
        Arc::new(Recursion::new().replicates(replicates)),
        Arc::new(Lie::new().replicates(replicates)),
        Arc::new(Privilege::new().replicates(replicates)),
        Arc::new(Instrumented::new().replicates(replicates)),
        Arc::new(Repair::new().replicates(ladders)),
        Arc::new(Feedback::new().replicates(replicates)),
    ]
}
