#![deny(missing_docs)]
#![deny(unsafe_code)]

//! **nachalnik-eval** measures whether a model's claims about its own state are true.
//!
//! Ask a model why it thinks what it thinks and you get a story about itself, told after the
//! fact. Nobody can check that story - not you, and not it. This crate checks it: the model
//! commits to a claim about its own context, the harness then goes and moves the thing the claim
//! was about, and the two are compared. Nothing is scored that was not observed.
//!
//! # the loop
//!
//! ```text
//!   solve ──> introspect ──> predict ──> intervene ──> observe ──> score ──> tell ──┐
//!     ▲                                    (a copy)                                │
//!     └────────────────────────── a fresh question, a scored self-model ────────────┘
//! ```
//!
//! Every step of it is ordinary [`nachalnik`] usage. A [`Subject`] is a [`Kernel`] with a
//! [`Provider`] in it; an [`Origin`] is [`Kernel::snapshot`]; an [`Intervention`] is a state
//! change on the copy of a context; a copy is [`Kernel::resume`] with one request in it and no
//! tools; and what a run cost comes out of the session log. **Nothing was added to the runtime
//! for any of this**, which is the same test `nachalnik-mcp` and `kamchatka`'s introspection
//! tools were built to pass.
//!
//! # what makes a number here mean anything
//!
//! Four decisions, each of which a benchmark of this kind gets wrong by default:
//!
//! 1. **No model is in the scoring path.** A [`Probe`] declares the shape its answer comes back
//!    in and a [`Reading`] parses it; an answer that does not parse is [`Answer::Unreadable`] and
//!    is counted as such rather than being coerced into a score. There is no judge model, so
//!    nothing in a score depends on a second model's opinion.
//! 2. **Interventions run on a copy, never on the subject.** [`Ablation::observe`] resumes a
//!    [`Snapshot`] with items excluded, elided, revised or planted; the session under test is
//!    untouched and does not learn that it was measured. Which is also why every claim in the
//!    supplied experiments is elicited *before* any copy is run.
//! 3. **The control is a copy too, and the question says so.** "Did the answer change?" is the
//!    treated copies against control copies of the same context with nothing moved - not against
//!    what the subject said in the live session, which was said with tools, at a different point
//!    in the conversation. Which is why every counterfactual in [`suite`] asks about *two
//!    copies* rather than about "your answer": a subject can be exactly right about how the two
//!    copies will answer and be scored wrong for it, if the baseline it was shown is not the
//!    baseline the score used. Run more than one replicate and [`Change::instability`] reports
//!    how often the control disagreed with itself, which is the noise floor a change of one has
//!    to clear.
//! 4. **Accuracy is reported beside what guessing would score.** [`Scores::majority`] is what a
//!    subject that always gave the commonest answer would get, and [`Scores::skill`] is how much
//!    of the room above that the subject actually took. A battery of counterfactuals in which
//!    nothing ever moves is a battery on which "no" scores 100%.
//!
//! # what it measures
//!
//! | [`Kind`] | the claim | what it is scored against |
//! | --- | --- | --- |
//! | [`Kind::Counterfactual`] | "taking that away would/would not change my answer" | a copy with it taken away |
//! | [`Kind::Attribution`] | "this item is the one my answer rests on" | which items, ablated one at a time, actually moved the answer |
//! | [`Kind::Location`] | "that item is number 7" | which number it really is |
//! | [`Kind::Recursive`] | "a copy of me, asked that, would say yes" | a copy, asked that |
//!
//! Each comparison is one [`Resolution`], and [`Scores`] is computed over a set of them:
//! accuracy, the majority baseline, a Brier score, expected calibration error, an
//! over-confidence gap, and a calibration curve. [`Gain`] is those scores before and after the
//! subject was told how it had done, and [`Depths`] is them at each remove of self-reference.
//!
//! # a run
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use nachalnik::{Config, Kernel, Provider};
//! use nachalnik_eval::{Subject, evaluate, suite};
//!
//! # async fn go(provider: Arc<dyn Provider>) -> Result<(), Box<dyn std::error::Error>> {
//! let report = evaluate(suite::all(), |name| {
//!     // a fresh subject per experiment: a session that has already been introspected on is
//!     // not a clean subject
//!     let kernel = Kernel::new(Config {
//!         session_name: Some(name.to_owned()),
//!         ..Config::default()
//!     });
//!     kernel.set_provider(provider.clone());
//!     Ok(Subject::new(kernel))
//! })
//! .await;
//!
//! println!("{report}");
//! println!("{}", serde_json::to_string_pretty(&report)?);
//! # Ok(())
//! # }
//! ```
//!
//! # where the questions live
//!
//! There **is** prompt text in this crate, which is the one place it departs from the runtime's
//! rules, and it is deliberate: the questions are the instrument. They are all in [`suite`],
//! kept apart from the machinery in the modules above it, so that replacing them replaces the
//! benchmark and not the harness. Everything in [`suite`] is written in terms of the public
//! surface here and can be written again without touching it.
//!
//! [`Kernel`]: nachalnik::Kernel
//! [`Kernel::resume`]: nachalnik::Kernel::resume
//! [`Kernel::snapshot`]: nachalnik::Kernel::snapshot
//! [`Provider`]: nachalnik::Provider
//! [`Snapshot`]: nachalnik::Snapshot

mod error;
mod experiment;
mod fork;
mod intervene;
mod probe;
mod score;
mod subject;
mod trial;

pub mod suite;

pub use async_trait::async_trait;

pub use crate::{
    error::{Error, Result},
    experiment::{Experiment, Instrument, Outcome, Report, evaluate, per_model},
    fork::{Ablation, Change, Observation, Origin, PREAMBLE},
    intervene::{Applied, Intervention},
    probe::{Answer, Probe, Reading},
    score::{
        BINS, Bin, Cohort, Deference, Depth, Depths, Faced, Family, Gain, Interval, Paired,
        Reached, Scores, Stage, Surface,
    },
    subject::{Said, Spend, Subject},
    trial::{Act, Check, Journal, Kind, Labelled, Resolution, Step, Trial},
};
