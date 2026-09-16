//! Compatibility shim: the MCP tool adapter now lives in
//! `threadlane-coding-agent`. This module re-exports it so existing
//! `crate::mcp::...` paths keep working during the split.
pub use threadlane_coding_agent::mcp::*;
