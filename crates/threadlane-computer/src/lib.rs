//! Computer use through the CUA driver: window introspection, accessibility
//! snapshots, screenshots, and input control.
//!
//! Extracted from `threadlane-session` so OS-level automation lives in its
//! own crate. The crate never touches session state: user approval flows
//! through the [`ComputerApproval`] trait, which the session implements with
//! its permission manager. Mirror frames and input overlays are published on
//! the shared [`threadlane_protocol::live`] contract; screenshots also land
//! on disk for the user and the GPUI mirror fallback.

pub mod computer;
pub mod driver;
mod mirror;

pub use computer::{
    global_previews_dir, parse_act_target, parse_computer_act, resolve_previews_dir, ComputerAct,
    ComputerToolExecutor, COMPUTER_ACT_TOOL, COMPUTER_AX_TOOL, COMPUTER_SCREENSHOT_TOOL,
    COMPUTER_STATUS_TOOL, COMPUTER_WINDOWS_TOOL, CUA_CALL_TOOL, MAX_IMAGE_BYTES,
};
pub use driver::{driver_available, driver_binary, driver_call, driver_version, DRIVER_MISSING_HINT};

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
/// executor calls it before every screenshot, accessibility snapshot with
/// pixels, input action, and mutating `cua_call`.
#[async_trait::async_trait]
pub trait ComputerApproval: Send + Sync {
    async fn request_computer(&self, title: &str, detail: &str) -> ComputerDecision;
}
