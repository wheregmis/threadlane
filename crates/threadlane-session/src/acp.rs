//! Session-owned ACP adapter.
//!
//! The protocol implementation lives in [`threadlane_acp`]; this module
//! re-exports it so existing `crate::acp::` paths keep working, and adds the
//! one piece that belongs to the host application: the workspace path guard.
//! Agent filesystem access must follow the same policy as the rest of the
//! session, so [`session_workspace_client`] injects
//! `threadlane_tools::validate_path_in_workspace` rather than letting the
//! standalone crate's default validator drift from it.

pub use threadlane_acp::*;

use std::path::PathBuf;
use std::sync::Arc;

/// Builds the session's [`AcpWorkspaceClient`]: workspace-scoped filesystem
/// access through the single shared [`threadlane_tools`] path guard, with the
/// caller chaining permission/update wiring afterwards.
pub fn session_workspace_client(workspace_root: PathBuf) -> AcpWorkspaceClient {
    AcpWorkspaceClient::new(workspace_root).with_path_validator(Arc::new(
        |requested: &str, root: &std::path::Path| {
            threadlane_tools::validate_path_in_workspace(requested, root)
        },
    ))
}
