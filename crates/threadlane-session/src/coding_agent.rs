//! Compatibility shim: the coding-agent engine now lives in
//! `threadlane-coding-agent`. This module reconstructs the previous
//! `coding_agent::` subtree so existing deep paths (e.g.
//! `crate::coding_agent::harness::CodingSessionHarness`) keep working
//! during the split.
pub use threadlane_coding_agent::{
    broker, cancellation, capabilities, context_snapshots, durable, harness, mailbox, options,
    runtime, scheduler, subagents,
};
pub use threadlane_coding_agent::{
    cancel_open_subagent_operations, subagent_workspace, AgentRunTask, CodingAgent,
    CodingAgentCancellation, CodingAgentOptions, CodingAgentWorkHandle, HarnessCompositionSnapshot,
    SubagentCancellationGuard,
};
