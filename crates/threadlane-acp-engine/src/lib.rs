//! External ACP agent engine for Threadlane.
//!
//! [`AcpEngine`] drives a turn against a third-party agent subprocess
//! (protocol in `threadlane-acp`), while [`bridge`] maps the agent's
//! `session/update` notifications onto the shared `AgentEvent` contract so
//! external turns render through the existing transcript pipeline.
//! Filesystem access the agent requests is workspace-scoped through
//! [`session_workspace_client`], which injects the single shared
//! `threadlane_tools` path guard.

mod bridge;
pub mod presets;
mod runtime;

pub use bridge::{acp_agent_id, acp_model_id, agent_events_for, is_acp_model};
pub use presets::{
    add_acp_agent, configured_acp_agents, remove_acp_agent, set_acp_enabled,
    set_acp_preset_enabled, upgrade_acp_presets, AcpPreset, ACP_PRESETS,
};
pub use runtime::{generate_title, AcpEngine};
pub use threadlane_acp::AcpWorkspaceClient;

use std::path::PathBuf;
use std::sync::Arc;

/// Builds an [`AcpWorkspaceClient`] with workspace-scoped filesystem access
/// through the single shared [`threadlane_tools`] path guard, with the caller
/// chaining permission/update wiring afterwards.
pub fn session_workspace_client(workspace_root: PathBuf) -> AcpWorkspaceClient {
    AcpWorkspaceClient::new(workspace_root).with_path_validator(Arc::new(
        |requested: &str, root: &std::path::Path| {
            threadlane_tools::validate_path_in_workspace(requested, root)
        },
    ))
}
