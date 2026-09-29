//! The parts that have to be right before a run means anything: what an answer is read as, what
//! an intervention does to a copy, and what a set of comparisons comes to.
//!
//! note: All of it offline and none of it a model. Every figure a report quotes is produced by
//! this arithmetic, so it is checked against numbers worked out by hand rather than against
//! whatever it happened to print the first time.

mod fairness;
mod groupings;
mod handles;
mod instruments;
mod intervals;
mod interventions;
mod material;
mod readings;
mod scores;
mod statistics;

use material::report_of;
use scores::resolution;
use statistics::asked;
