//! Native computer-use tools: window introspection, screenshots, input control.
//!
//! Extracted from `threadlane-session` so OS-level input automation lives in
//! its own crate. The crate never touches session state: user approval flows
//! through the [`ComputerApproval`] trait, which the session implements with
//! its permission manager. The live frame/overlay feed is the shared
//! [`threadlane_protocol::live`] contract.

pub mod computer;
mod stream;

pub use computer::{
    global_previews_dir, watch_display_for_debug, ComputerAct, ComputerCapability,
    ComputerToolExecutor, TargetedAct, COMPUTER_ACT_TOOL, COMPUTER_SCREENSHOT_TOOL,
    COMPUTER_STATUS_TOOL, COMPUTER_UNAVAILABLE, COMPUTER_WINDOWS_TOOL,
};

/// User decision for a computer-use approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerDecision {
    AllowOnce,
    AllowAlways,
    Deny,
}

/// Approval channel for computer-use actions.
///
/// Implemented by the session with its permission manager (Once/Always
/// scopes, per-project persisted grant, default-deny unattended). The
/// executor calls it before every screenshot and input action.
#[async_trait::async_trait]
pub trait ComputerApproval: Send + Sync {
    async fn request_computer(&self, title: &str, detail: &str) -> ComputerDecision;
}
