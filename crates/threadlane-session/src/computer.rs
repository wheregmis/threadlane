//! Compatibility shim: the computer capability adapter now lives in
//! `threadlane-coding-agent`. This module re-exports it so existing
//! `crate::computer::...` paths keep working during the split.
pub use threadlane_coding_agent::computer::*;
