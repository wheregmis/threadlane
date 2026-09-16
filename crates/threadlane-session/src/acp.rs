//! Session-owned ACP adapter.
//!
//! The protocol implementation lives in [`threadlane_acp`]; this module
//! re-exports it so existing `crate::acp::` paths keep working. The
//! workspace path guard ([`session_workspace_client`]) now lives in
//! `threadlane-acp-engine` alongside its only caller, re-exported here for
//! compatibility.

pub use threadlane_acp::*;
pub use threadlane_acp_engine::session_workspace_client;
