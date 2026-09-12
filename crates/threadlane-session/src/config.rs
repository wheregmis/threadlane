//! Coding-agent harness configuration: compatibility re-exports.
//!
//! `CodingAgentConfig` is canonical in `threadlane_runtime::config` alongside
//! `AgentConfig`, so all agent tuning lives in one module. Re-exported here
//! for compatibility; new code should import from `threadlane_runtime`
//! directly.

pub use threadlane_runtime::config::{CodingAgentConfig, CodingAgentConfigBuilder};
