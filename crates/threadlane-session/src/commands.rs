//! Compatibility shim: slash commands now live in
//! `threadlane-coding-agent`. This module re-exports them so existing
//! `crate::commands::...` paths keep working during the split.
pub use threadlane_coding_agent::commands::*;
