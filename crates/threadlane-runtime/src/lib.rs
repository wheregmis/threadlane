pub mod capability;
pub mod compaction;
pub mod config;
pub mod error;
pub mod events;
pub mod harness;
pub mod local_tool_router;
pub(crate) mod loop_detector;
pub(crate) mod loop_engine;
pub mod model_metadata;
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
pub use utils::{AbortOnDrop, dirs_home, now_timestamp_ms, now_timestamp_secs};

pub use capability::{Capability, CapabilityRegistry};
pub use compaction::{
    CompactionOptions, CompactionStrategy, compact_messages, compact_messages_with_strategy,
    compaction_summary_text, prepare_token_optimal_context, prune_historical_tool_outputs,
};
pub use config::{AgentConfig, AgentConfigBuilder};
pub use error::AgentError;
pub use events::{
    AgentEvent, HarnessMetrics, PermissionRequest, PermissionScope, QuestionAnswer, QuestionItem,
    QuestionItemAnswer, QuestionRequest, SubagentIsolation, SubagentProgressUpdate,
    SubagentRecoveryStatus,
};
pub use harness::{
    AcceptedRun, DurableEvent, DurablePayload, InterruptedSubagentLane, LaneQueue,
    OperationOutcome, QueueKind, Record, RecoveryResult, SteerItem, SteerPriority,
    ToolReplaySafety, has_open_subagent_lanes, interrupted_subagent_lanes,
};
pub use loop_engine::repair_interrupted_tool_turn;
pub use provider::{
    AssistantMessageRecorder, ChatCompletionsAdapter, CodexResponsesAdapter, ProviderAdapter,
    ProviderBoundaryPreparer, ProviderBoundaryRequest, ProviderBoundaryResult,
    ProviderDiscardedUsageRecorder, ProviderHookRecorder, ProviderMessages, ProviderRouter,
    ProviderTraceEvent, ProviderTraceRecorder, ProviderUsageRecorder, StreamingStateRecorder,
    ToolCompletionRecorder, ToolExecutionTraceEvent, ToolExecutionTraceRecorder,
    ToolIntentRecorder,
};
// Message translation, model registry, and the shared reactor live in
// `threadlane-provider` (the dependency arrow points runtime → provider);
// re-exported here so existing paths keep working.
pub use threadlane_provider::{
    convert_to_codex_llm, convert_to_llm, get_runtime,
};
pub use threadlane_provider::model_registry;
pub use rules::*;
pub use tool_dispatcher::ToolDispatcher;
pub use tool_executor::{BuiltinToolExecutor, ToolExecutor};
pub use types::*;
