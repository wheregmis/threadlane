//! Compatibility shim: updater services now live in
//! `threadlane-ui-state`. This module re-exports them so existing
//! `crate::services::updater::...` paths keep working during the split.
pub use threadlane_ui_state::updater::*;
