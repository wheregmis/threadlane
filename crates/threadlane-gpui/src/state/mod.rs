//! Compatibility shim: durable UI state now lives in
//! `threadlane-ui-state`. This module re-exports it so existing
//! `crate::state::...` paths keep working during the split.
pub use threadlane_ui_state::*;
