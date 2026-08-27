use std::fmt;

#[cfg(doc)]
use crate::{Kernel, Provider, State, Tool};
use crate::{context::ContextId, permissions::PermissionId};

/// An error produced by user-supplied code (a [`Provider`] or a [`Tool`]) and carried by the
/// kernel without being interpreted.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The result type used by the kernel.
pub type Result<T> = std::result::Result<T, Error>;

/// Things that can go wrong in the kernel itself.
///
/// note: This list is deliberately short. A failing [`Tool`] is *not* a kernel error - the
/// failure is recorded as an error tool result and handed back to the model, since that is
/// information the model needs. Only conditions that make the loop unable to proceed appear
/// here.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The kernel is in the middle of a request or of running tools, and cannot start another.
    ///
    /// note: See [`Kernel::state`]: [`State::Requesting`] and [`State::Executing`] are the two
    /// states in which somebody else is already driving the loop.
    Busy,
    /// [`Kernel::step`] was called without a [`Provider`] being set.
    NoProvider,
    /// The [`Provider`] returned an error.
    Provider(BoxError),
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
