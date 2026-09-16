//! Compatibility shim: the session controller now lives in
//! `threadlane-coding-agent`. This module re-exports it so existing
//! `crate::controller::...` paths keep working during the split.
pub use threadlane_coding_agent::controller::*;
