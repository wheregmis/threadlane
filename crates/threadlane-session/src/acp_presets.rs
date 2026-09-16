//! Compatibility shim: ACP agent presets now live in
//! `threadlane-acp-engine`. This module re-exports them so existing
//! `crate::acp_presets::...` paths keep working during the split.
pub use threadlane_acp_engine::presets::*;
