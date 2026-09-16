//! Compatibility shim: application actions now live in
//! `threadlane-ui-state`. This module re-exports them so existing
//! `crate::app::actions::...` paths keep working during the split.
pub use threadlane_ui_state::actions::*;
