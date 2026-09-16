//! Compatibility shim: bundled icon assets now live in
//! `threadlane-ui-theme`. This module re-exports them so existing
//! `crate::assets::...` paths keep working during the split.
pub use threadlane_ui_theme::assets::*;
