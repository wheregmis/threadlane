//! Compatibility shim: the ACP event bridge now lives in
//! `threadlane-acp-engine`. This module re-exports it so existing
//! `crate::acp_bridge::...` paths keep working during the split.
pub use threadlane_acp_engine::{acp_agent_id, acp_model_id, agent_events_for, is_acp_model};
