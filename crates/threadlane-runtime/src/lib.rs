pub mod capability;
pub mod compaction;
pub mod config;
pub mod error;
pub mod harness;
pub mod model_metadata;
pub mod plan;
pub mod provider;
pub mod rules;
pub mod tool_dispatcher;
pub mod tool_executor;
pub mod titles;
pub(crate) mod turn_driver;
pub mod types;
pub(crate) mod utils;

// ── AgentRuntime (sole runtime — replaces UnifiedAgent + ProviderRunExecutor) ──
mod runtime;
pub use runtime::AgentRuntime;

pub use capability::{Capability, CapabilityRegistry, ToolPolicy};
pub use config::{AgentConfig, AgentConfigBuilder, CodingAgentConfig, CodingAgentConfigBuilder};
pub use error::AgentError;
pub use harness::{
    has_open_subagent_lanes, interrupted_subagent_lanes, AcceptedRun,
    InterruptedSubagentLane, OperationOutcome, QueueKind, Record, SteerPriority,
    ToolReplaySafety,
};
// Turn-repair helper lives in `threadlane-provider::convert`, next to the
// id normalization it relies on; re-exported here so existing paths work.
pub use provider::{
    AssistantMessageRecorder,
    ProviderBoundaryPreparer, ProviderBoundaryRequest, ProviderBoundaryResult,
    ProviderDiscardedUsageRecorder, ProviderHookRecorder,
    ProviderTraceEvent, ProviderTraceRecorder, ProviderUsageRecorder, StreamingStateRecorder,
    ToolCompletionRecorder, ToolExecutionTraceEvent, ToolExecutionTraceRecorder,
    ToolIntentRecorder,
};
#[cfg(test)]
pub use provider::{ProviderAdapter, ProviderMessages, ProviderRouter};
// Message translation, model registry, and the shared reactor live in
// `threadlane-provider` (the dependency arrow points runtime → provider);
// import them from there directly.
pub use rules::*;
pub use tool_dispatcher::ToolDispatcher;
pub use tool_executor::BuiltinToolExecutor;
pub use types::*;
