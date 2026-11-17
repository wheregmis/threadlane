//! [`RunnerError`].

use std::io;

use threadlane_core::backend::BackendError;
use threadlane_core::link::LinkError;
use threadlane_core::runtime::Killed;

/// Why the runner stopped.
///
/// Failures inside a lane are reported to the hub and do not end the runner;
/// only process death ([`RunnerError::Killed`]) and errors outside any lane
/// reach the caller.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// An injected crash stopped the runner. Treat it as process death.
    #[error(transparent)]
    Killed(#[from] Killed),
    /// A hub request outside any lane failed.
    #[error("hub request failed: {0}")]
    Link(#[source] LinkError),
    /// A backend operation outside any run failed.
    #[error("backend failed: {0}")]
    Backend(#[from] BackendError),
    /// The lanes directory could not be prepared.
    #[error("i/o failed: {0}")]
    Io(#[from] io::Error),
}

impl From<LinkError> for RunnerError {
    /// [`LinkError::Killed`] becomes [`RunnerError::Killed`]: it is process death.
    fn from(error: LinkError) -> Self {
        match error {
            LinkError::Killed(killed) => Self::Killed(killed),
            other => Self::Link(other),
        }
    }
}
