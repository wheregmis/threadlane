//! Compatibility shim: the coding-agent engine now lives in
//! `threadlane-coding-agent`. This module keeps the deep paths actually used
//! (`crate::coding_agent::harness::CodingSessionHarness`,
//! `crate::coding_agent::CodingAgentOptions`) working during the split.
pub use threadlane_coding_agent::{harness, options};
pub use threadlane_coding_agent::{
    cancel_open_subagent_operations, subagent_workspace, AgentRunTask, CodingAgent,
    CodingAgentCancellation, CodingAgentOptions, CodingAgentWorkHandle, HarnessCompositionSnapshot,
    SubagentCancellationGuard,
};
