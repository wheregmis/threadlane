//! Compatibility shim: the intent controller now lives in
//! `threadlane-ui-state`. This module re-exports it so existing
//! `crate::app::controller::...` paths keep working during the split.
pub use threadlane_ui_state::controller::*;
