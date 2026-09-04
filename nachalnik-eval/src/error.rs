use std::fmt;

#[cfg(doc)]
use crate::{Experiment, Subject, evaluate};

/// The result type used by the harness.
pub type Result<T> = std::result::Result<T, Error>;

/// Things that can stop a measurement.
///
/// note: Deliberately short, and none of it is a *finding*. A subject that answers unreadably, a
/// copy that says nothing, a claim that turns out to be false: those are results, and they are
/// recorded as results. What is here is only the conditions under which there is nothing to
/// record - and even those do not end a run, because [`evaluate`] catches an
/// [`Experiment`]'s error and keeps the steps it had already taken.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The runtime could not carry out a step.
    Runtime(nachalnik::Error),
    /// A copy was run and its provider returned nothing at all - not an empty answer, no answer.
    Silent,
    /// A tool call is waiting on a permission decision, and there is nobody here to ask.
    ///
    /// note: An evaluation has no user to put the question to, and answering on their behalf is
    /// the one thing this workspace exists not to do. Give the subject's kernel a policy that
    /// decides for itself - `nachalnik::test::AllowAll` is one - or no tools.
    Undecided,
    /// The subject used up its request budget without ending its turn.
    ///
    /// note: See `Config::max_requests_per_turn` and [`Subject::rounds`]: between them they
    /// bound how long one question may run for, and a subject that keeps calling tools instead
    /// of answering hits that bound rather than the harness waiting for it forever.
    Exhausted,
    /// An experiment could not set itself up, and says why.
    Setup(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(e) => write!(f, "the runtime failed: {e}"),
            Self::Silent => write!(f, "a copy was asked and answered nothing at all"),
            Self::Undecided => write!(
                f,
                "a tool call is waiting on a permission decision and there is nobody to ask"
            ),
            Self::Exhausted => write!(f, "the subject ran out of requests before it answered"),
            Self::Setup(what) => write!(f, "the experiment could not be set up: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runtime(e) => Some(e),
            _ => None,
        }
    }
}

impl From<nachalnik::Error> for Error {
    fn from(e: nachalnik::Error) -> Self {
        Self::Runtime(e)
    }
}
