pub mod capability;
pub mod compaction;
pub mod config;
pub(crate) mod engine;
pub mod error;
pub mod events;
pub mod harness;
pub mod local_tool_router;
pub(crate) mod loop_engine;
pub mod model_metadata;
#[cfg(feature = "needle")]
pub mod needle_history_eval;
#[cfg(feature = "needle")]
pub mod needle_training;
pub mod provider;
pub mod rules;
pub mod tool_dispatcher;
pub mod tool_executor;
pub(crate) mod turn_driver;
pub mod types;
pub mod utils;

// ── AgentRuntime (sole runtime — replaces UnifiedAgent + ProviderRunExecutor) ──
mod runtime;
pub use runtime::{AgentRuntime, ModelContextProjector, ModelContextSource};

// ── Re-exports matching the old threadlane-agent public API ────────
pub use utils::{dirs_home, now_timestamp_ms, now_timestamp_secs, AbortOnDrop};

pub use capability::{Capability, CapabilityRegistry};
pub use compaction::{
    compact_messages, compact_messages_with_strategy, compaction_summary_text,
    prepare_token_optimal_context, prune_historical_tool_outputs, CompactionOptions,
    CompactionStrategy,
};
pub use config::{AgentConfig, AgentConfigBuilder};
pub use engine::get_runtime;
pub use error::AgentError;
pub use events::{
    AgentEvent, HarnessMetrics, PermissionRequest, PermissionScope, SubagentRecoveryStatus,
};
pub use harness::{
    has_open_subagent_lanes, interrupted_subagent_lanes, AcceptedRun, DurableEvent, DurablePayload,
    InterruptedSubagentLane, LaneQueue, OperationOutcome, QueueKind, Record, RecoveryResult,
    SteerItem, SteerPriority, ToolReplaySafety,
};
pub use loop_engine::repair_interrupted_tool_turn;
pub use provider::{
    convert_to_codex_llm, convert_to_llm, AssistantMessageRecorder, ChatCompletionsAdapter,
    CodexResponsesAdapter, ProviderAdapter, ProviderBoundaryPreparer, ProviderBoundaryRequest,
    ProviderBoundaryResult, ProviderDiscardedUsageRecorder, ProviderHookRecorder, ProviderMessages,
    ProviderRouter, ProviderTraceEvent, ProviderTraceRecorder, ProviderUsageRecorder,
    StreamingStateRecorder, ToolCompletionRecorder, ToolExecutionTraceEvent,
    ToolExecutionTraceRecorder, ToolIntentRecorder,
};
pub use rules::*;
pub use tool_dispatcher::ToolDispatcher;
pub use tool_executor::{BuiltinToolExecutor, ToolExecutor};
pub use types::*;
