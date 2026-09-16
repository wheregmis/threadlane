//! Compatibility shim: the project watcher now lives in
//! `threadlane-ui-state`. This module re-exports it so existing
//! `crate::services::watcher::...` paths keep working during the split.
pub use threadlane_ui_state::watcher::*;
