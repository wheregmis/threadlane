//! Compatibility shim: credential resolution now lives in
//! `threadlane-coding-agent`. This module re-exports it so existing
//! `crate::credentials::...` paths keep working during the split.
pub use threadlane_coding_agent::credentials::*;
