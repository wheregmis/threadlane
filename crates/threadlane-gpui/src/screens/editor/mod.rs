//! Compatibility shim: the editor view now lives in
//! `threadlane-ui-editor`. This module re-exports it so existing
//! `crate::screens::editor::...` paths keep working during the split.
pub use threadlane_ui_editor::*;
