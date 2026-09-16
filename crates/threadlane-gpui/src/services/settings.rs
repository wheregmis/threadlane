//! Compatibility shim: settings services now live in
//! `threadlane-ui-state`. This module re-exports them so existing
//! `crate::services::settings::...` paths keep working during the split.
pub use threadlane_ui_state::settings::*;
