//! Compatibility shim: the visual language now lives in
//! `threadlane-ui-theme`. This module re-exports it so existing
//! `crate::theme::...` paths keep working during the split.
pub use threadlane_ui_theme::theme::*;
