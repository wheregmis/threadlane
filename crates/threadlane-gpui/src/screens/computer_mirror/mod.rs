//! Compatibility shim: the computer-use mirror now lives in
//! `threadlane-ui-mirror`. This module re-exports it so existing
//! `crate::screens::computer_mirror::...` paths keep working during the split.
pub use threadlane_ui_mirror::*;
