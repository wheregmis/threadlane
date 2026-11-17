//! Errors of the hub's own loops, and how store errors cross the link.

use threadlane_core::client::ApiError;
use threadlane_core::link::LinkError;
use threadlane_core::runtime::Killed;
use threadlane_core::store::StoreError;
use threadlane_core::tools::ToolError;

/// Why a hub operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HubError {
    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Crash injection stopped the hub at a step boundary.
    #[error(transparent)]
    Killed(#[from] Killed),
    /// The database has no machine row for the hub's own runner.
    #[error("the hub has no machine row of its own")]
    NoHubMachine,
}

/// A store error as a runner sees it: a moved-on epoch is a stale lease,
/// anything else a rejection with the store's message.
pub(crate) fn link_error(error: StoreError) -> LinkError {
    match error {
        StoreError::StaleLease { lane_id, .. } => LinkError::StaleLease { lane_id },
        other => LinkError::Rejected(other.to_string()),
    }
}

/// A store error as a tool call sees it.
pub(crate) fn tool_error(error: StoreError) -> ToolError {
    match error {
        StoreError::StaleLease { .. } => ToolError::LeaseLost,
        other => ToolError::Failed(other.to_string()),
    }
}

/// A link error inside a tool call: the lease is gone, or it failed.
pub(crate) fn link_to_tool(error: LinkError) -> ToolError {
    match error {
        LinkError::StaleLease { .. } => ToolError::LeaseLost,
        LinkError::Killed(killed) => ToolError::Killed(killed),
        LinkError::Unreachable => ToolError::HubUnreachable,
        other => ToolError::Failed(other.to_string()),
    }
}

/// A store error as a client sees it.
pub(crate) fn api_error(error: StoreError) -> ApiError {
    ApiError::Internal(error.to_string())
}
