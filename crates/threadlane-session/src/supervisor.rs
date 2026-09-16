//! Compatibility shim: the background-task supervisor now lives in
//! `threadlane-supervisor`. This module re-exports it so existing
//! `crate::supervisor::...` paths keep working during the split.
pub use threadlane_supervisor::*;
