//! Compatibility shim: the terminal view now lives in
//! `threadlane-ui-terminal`. This module re-exports it so existing
//! `crate::screens::terminal::...` paths keep working during the split.
pub use threadlane_ui_terminal::*;
