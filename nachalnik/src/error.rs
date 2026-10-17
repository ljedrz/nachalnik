//! What can go wrong in the kernel, and what deliberately cannot.

use std::fmt;

#[cfg(doc)]
use crate::{Config, Kernel, Provider, State, Tool};
use crate::{context::ContextId, model::Overrun, permissions::PermissionId};

/// An error from user-supplied code (a [`Provider`] or a [`Tool`]), carried by the kernel without
/// being interpreted.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The result type used by the kernel.
pub type Result<T> = std::result::Result<T, Error>;

/// Things that can go wrong in the kernel itself.
///
/// note: only conditions that make the loop unable to proceed appear here. A failing [`Tool`] is
/// not a kernel error. Its failure is recorded as an error tool result and handed back to the model
/// because the model needs that information.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The kernel is mid-turn. It cannot start another turn or undo its current action.
    ///
    /// note: See [`Kernel::state`]. [`State::Requesting`] and [`State::Executing`] are the two
    /// states in which someone else is already driving the loop, and [`Kernel::step`] is refused in
    /// them.
    /// [`Kernel::undo`] and [`Kernel::redo`] are refused in [`State::Deciding`] and
    /// [`State::Ready`] too, for the reason `undo` gives. Only a decision, a step or
    /// [`Kernel::cancel_pending_calls`] takes the machine out of those two states.
    ///
    /// note: Nothing here waits on a free kernel. A "tell me when you are free" future would choose
    /// a queueing policy, and no one policy suits every client. Instead, every transition is an
    /// [`Event::StateChanged`](crate::Event::StateChanged). To wait, use
    /// [`Kernel::subscribe`](crate::Kernel::subscribe) and loop until the state allows the call;
    /// for a step, until [`State::is_busy`] is false. A client that would rather refuse than queue
    /// reports this and moves on.
    Busy,
    /// [`Kernel::step`], or [`Kernel::preview_payload`](crate::Kernel::preview_payload), was
    /// called without a [`Provider`] being set.
    NoProvider,
    /// The [`Provider`] returned an error.
    Provider(BoxError),
    /// The request is longer than the model will read, and was not sent.
    ///
    /// note: the kernel's own arithmetic, not an endpoint's verdict.
    /// [`Config::refuse_oversized_requests`] turns it off. The numbers have the same shape either
    /// way, so a client reports them the same way for both.
    TooLong(Overrun),
    /// The projection of the current context contains no messages, so there is nothing to send.
    EmptyProjection,
    /// The referenced context item does not exist.
    UnknownItem(ContextId),
    /// The referenced permission request does not exist, or has already been decided.
    UnknownPermission(PermissionId),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => write!(f, "the kernel is busy"),
            Self::NoProvider => write!(f, "no provider is set"),
            Self::Provider(e) => write!(f, "the provider failed: {e}"),
            Self::TooLong(overrun) => match overrun.limit {
                Some(limit) => write!(
                    f,
                    "the request is about {} tokens and the model takes {limit}, so it was not \
                     sent",
                    overrun.tokens,
                ),
                None => write!(
                    f,
                    "the request is about {} tokens, which is more than the model takes, so it \
                     was not sent",
                    overrun.tokens,
                ),
            },
            Self::EmptyProjection => write!(f, "the context projects to an empty request"),
            Self::UnknownItem(id) => write!(f, "there is no context item {id}"),
            Self::UnknownPermission(id) => write!(f, "there is no pending permission request {id}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Provider(e) => Some(&**e),
            _ => None,
        }
    }
}
