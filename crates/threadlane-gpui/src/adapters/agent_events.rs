//! Compatibility shim: the agent-event adapter now lives in
//! `threadlane-ui-state`. This module re-exports it so existing
//! `crate::adapters::agent_events::...` paths keep working during the split.
pub use threadlane_ui_state::agent_events::*;
