//! Centralized agent configuration.
//!
//! All tunable parameters for the agent execution loop, compaction, and
//! stream rules live here rather than as scattered `const` items.

use crate::types::{ModelRoles, OrchestratorMode, ReasoningEffort};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for the agent execution loop, compaction, and stream rules.
///
/// Every field has a sensible default. Use [`AgentConfig::builder()`] or
/// `AgentConfig::default()` as a starting point and override only what you
/// need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    // ── Compaction ──────────────────────────────────────────────────────
    /// Estimated token threshold above which auto-compaction triggers.
    pub(crate) auto_compaction_threshold_tokens: usize,

    /// Number of tokens to retain from the most recent messages during
    /// token-budget compaction.
    pub(crate) auto_compaction_keep_recent_tokens: usize,

    /// Maximum characters for a compaction checkpoint excerpt.
    pub(crate) max_checkpoint_chars: usize,

    /// Estimated tokens per image attachment (used for token counting).
    pub(crate) estimated_image_tokens: usize,

    pub(crate) unknown_model_context_limit: usize,
    pub(crate) context_minimum_headroom_tokens: usize,
    pub(crate) context_headroom_percent: usize,
    pub(crate) context_repeated_input_ceiling_tokens: usize,
    pub(crate) context_minimum_retained_tail_tokens: usize,
    pub(crate) context_maximum_retained_tail_tokens: usize,
    pub(crate) context_retained_tail_percent: usize,

    // ── Loop Guard ──────────────────────────────────────────────────────
    /// Master switch for the turn-level loop circuit breaker. When enabled,
    /// consecutive identical calls, ping-pong cycles, and same-error runs
    /// trip with a terminal message instead of burning to the context limit.
    #[serde(default = "default_loop_guard_enabled")]
    pub(crate) loop_guard_enabled: bool,
    /// Consecutive identical (tool+args+output) calls that trip the breaker.
    #[serde(default = "default_loop_identical_limit")]
    pub(crate) loop_identical_limit: usize,
    /// Repeated A→B→A… rounds (period 2-3) that trip the breaker.
    #[serde(default = "default_loop_pingpong_rounds")]
    pub(crate) loop_pingpong_rounds: usize,
    /// Consecutive same-error failures that trip the breaker.
    #[serde(default = "default_loop_error_limit")]
    pub(crate) loop_error_limit: usize,

    // ── Stream Rules ────────────────────────────────────────────────────
    /// Maximum bytes of accumulated streaming text to retain for regex
    /// matching. Text beyond this window is discarded.
    pub(crate) stream_rule_max_window_bytes: usize,

    // ── Provider ────────────────────────────────────────────────────────
    /// Default system prompt used when none is explicitly set.
    pub(crate) default_system_prompt: String,

    // ── Model Roles ─────────────────────────────────────────────────────
    /// Assigned models for specialized roles (Task, Plan, Advisor).
    #[serde(default)]
    pub model_roles: ModelRoles,

    /// Project-selected model for delegated subagents. `None` inherits the
    /// active parent session model.
    #[serde(default)]
    pub subagent_model: Option<String>,

    /// Project-selected reasoning effort for delegated subagents. `None`
    /// inherits the active parent turn's reasoning effort.
    #[serde(default)]
    pub subagent_reasoning_effort: Option<ReasoningEffort>,

    /// Project-selected reasoning effort for fast model execution (/prewalk). `None`
    /// inherits the active parent turn's reasoning effort.
    #[serde(default)]
    pub fast_reasoning_effort: Option<ReasoningEffort>,

    /// Orchestrator mode governing automatic /prewalk engagement.
    #[serde(default)]
    pub orchestrator_mode: OrchestratorMode,

    // ── Tool Execution ──────────────────────────────────────────────────
    /// Enable local Needle tool routing when compiled with the `needle` feature.
    #[serde(default)]
    pub needle_enabled: bool,

    /// Timeout for individual tool executions. `None` means no timeout.
    tool_execution_timeout: Option<Duration>,

    /// Maximum tool output length in bytes before truncation. `None` means
    /// no limit.
    max_tool_output_bytes: Option<usize>,

    /// When enabled, restricts the model-visible JSON tool schema to the essential core tools
    /// (read_file, edit_file_hashline, edit_files_hashline, write_file, run_command, subagent,
    /// plus the browser_* panel and computer_* native tools).
    /// Auxiliary tools remain executable directly or via the in-process `dyn` CLI.
    #[serde(default = "default_core_tool_schema_mode")]
    pub(crate) core_tool_schema_mode: bool,

    // ── Event Channel ───────────────────────────────────────────────────
    /// Capacity of the broadcast channel for [`AgentEvent`]s.
    pub(crate) event_channel_capacity: usize,
}

fn default_core_tool_schema_mode() -> bool {
    true
}

fn default_loop_guard_enabled() -> bool {
    true
}

fn default_loop_identical_limit() -> usize {
    5
}

fn default_loop_pingpong_rounds() -> usize {
    3
}

fn default_loop_error_limit() -> usize {
    3
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            auto_compaction_threshold_tokens: 96_000,
            auto_compaction_keep_recent_tokens: 20_000,
            max_checkpoint_chars: 12_000,
            estimated_image_tokens: 1_200,
            unknown_model_context_limit: 128_000,
            context_minimum_headroom_tokens: 32_000,
            context_headroom_percent: 20,
            context_repeated_input_ceiling_tokens: 256_000,
            context_minimum_retained_tail_tokens: 20_000,
            context_maximum_retained_tail_tokens: 64_000,
            context_retained_tail_percent: 25,
            stream_rule_max_window_bytes: 4096,
            default_system_prompt: "You are threadlane AI coding agent. Lead with answers and actions. Omit conversational filler, preambles, and recaps. Keep edits minimal, focused on root causes, and strictly avoid unrequested refactoring or speculative abstractions.".into(),
            model_roles: ModelRoles::default(),
            subagent_model: None,
            subagent_reasoning_effort: None,
            fast_reasoning_effort: None,
            orchestrator_mode: OrchestratorMode::default(),
            needle_enabled: false,
            core_tool_schema_mode: true,
            loop_guard_enabled: true,
            loop_identical_limit: 5,
            loop_pingpong_rounds: 3,
            loop_error_limit: 3,
            tool_execution_timeout: None,
            max_tool_output_bytes: None,
            event_channel_capacity: 500,
        }
    }
}

impl AgentConfig {
    pub fn default_system_prompt(&self) -> &str {
        &self.default_system_prompt
    }

    /// Creates a new [`AgentConfigBuilder`].
    pub fn builder() -> AgentConfigBuilder {
        AgentConfigBuilder::default()
    }
}

/// Builder for [`AgentConfig`].
///
/// # Example
///
/// ```ignore
/// let config = AgentConfig::builder()
///     .auto_compaction_threshold_tokens(128_000)
///     .tool_execution_timeout(Duration::from_secs(30))
///     .build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct AgentConfigBuilder {
    config: AgentConfig,
}

impl AgentConfigBuilder {
    #[cfg(test)]
    pub(crate) fn auto_compaction_threshold_tokens(mut self, value: usize) -> Self {
        self.config.auto_compaction_threshold_tokens = value;
        self
    }

    #[cfg(test)]
    pub(crate) fn auto_compaction_keep_recent_tokens(mut self, value: usize) -> Self {
        self.config.auto_compaction_keep_recent_tokens = value;
        self
    }

    pub fn max_checkpoint_chars(mut self, value: usize) -> Self {
        self.config.max_checkpoint_chars = value;
        self
    }

    #[cfg(test)]
    pub(crate) fn estimated_image_tokens(mut self, value: usize) -> Self {
        self.config.estimated_image_tokens = value;
        self
    }

    pub fn unknown_model_context_limit(mut self, value: usize) -> Self {
        self.config.unknown_model_context_limit = value;
        self
    }

    pub fn context_minimum_headroom_tokens(mut self, value: usize) -> Self {
        self.config.context_minimum_headroom_tokens = value;
        self
    }

    pub fn context_headroom_percent(mut self, value: usize) -> Self {
        self.config.context_headroom_percent = value;
        self
    }

    pub fn context_repeated_input_ceiling_tokens(mut self, value: usize) -> Self {
        self.config.context_repeated_input_ceiling_tokens = value;
        self
    }

    pub fn context_minimum_retained_tail_tokens(mut self, value: usize) -> Self {
        self.config.context_minimum_retained_tail_tokens = value;
        self
    }

    pub fn context_maximum_retained_tail_tokens(mut self, value: usize) -> Self {
        self.config.context_maximum_retained_tail_tokens = value;
        self
    }

    pub fn context_retained_tail_percent(mut self, value: usize) -> Self {
        self.config.context_retained_tail_percent = value;
        self
    }

    pub fn stream_rule_max_window_bytes(mut self, value: usize) -> Self {
        self.config.stream_rule_max_window_bytes = value;
        self
    }

    pub fn with_default_system_prompt(mut self, value: impl Into<String>) -> Self {
        self.config.default_system_prompt = value.into();
        self
    }

    #[cfg(test)]
    pub(crate) fn model_roles(mut self, value: ModelRoles) -> Self {
        self.config.model_roles = value;
        self
    }

    pub fn tool_execution_timeout(mut self, value: Duration) -> Self {
        self.config.tool_execution_timeout = Some(value);
        self
    }

    pub fn max_tool_output_bytes(mut self, value: usize) -> Self {
        self.config.max_tool_output_bytes = Some(value);
        self
    }

    pub fn event_channel_capacity(mut self, value: usize) -> Self {
        self.config.event_channel_capacity = value;
        self
    }

    pub fn core_tool_schema_mode(mut self, value: bool) -> Self {
        self.config.core_tool_schema_mode = value;
        self
    }

    pub fn loop_guard_enabled(mut self, value: bool) -> Self {
        self.config.loop_guard_enabled = value;
        self
    }

    pub fn loop_identical_limit(mut self, value: usize) -> Self {
        self.config.loop_identical_limit = value;
        self
    }

    pub fn loop_pingpong_rounds(mut self, value: usize) -> Self {
        self.config.loop_pingpong_rounds = value;
        self
    }

    pub fn loop_error_limit(mut self, value: usize) -> Self {
        self.config.loop_error_limit = value;
        self
    }

    pub fn build(self) -> AgentConfig {
        self.config
    }
}

/// Configuration for the coding agent harness (session-owned orchestration).
///
/// Moved from `threadlane_session::config` so all agent tuning lives in one
/// module: [`AgentConfig`] covers the execution loop, compaction, and stream
/// rules, while this covers capability/WASI limits, subagent bounds, and
/// recovery behavior. Every field has a sensible default matching the prior
/// hard-coded constants. Use [`CodingAgentConfig::builder()`] or
/// `CodingAgentConfig::default()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingAgentConfig {
    // ── Capability / WASI ───────────────────────────────────────────────
    /// Timeout for capability calls (network, process, etc.).
    capability_timeout: Duration,

    /// Maximum bytes buffered for a single capability response.
    max_capability_buffer_bytes: usize,

    /// Maximum timeout a WASI process can request (ms).
    max_process_timeout_ms: u64,

    /// Maximum output bytes a WASI process can produce.
    max_process_output_bytes: usize,

    /// Maximum number of concurrently managed processes.
    max_managed_processes: usize,

    /// Default recv timeout for managed processes (ms).
    default_recv_timeout_ms: u64,

    /// Maximum recv timeout for managed processes (ms).
    max_recv_timeout_ms: u64,

    /// Maximum stdout bytes buffered for a managed process.
    max_managed_stdout_bytes: usize,

    /// Maximum broker continuation rounds before failing with an error.
    max_broker_continuation_rounds: usize,

    // ── Subagents ───────────────────────────────────────────────────────
    /// Maximum number of tasks a subagent can accept.
    max_subagent_tasks: usize,

    /// Maximum characters for a subagent task description.
    max_subagent_task_chars: usize,

    /// Maximum concurrent subagents.
    pub subagent_concurrency_limit: usize,

    /// Overall timeout for a single subagent run.
    subagent_timeout: Duration,

    /// Prompt used when recovering a subagent from a checkpoint.
    subagent_recovery_prompt: String,
}

impl Default for CodingAgentConfig {
    fn default() -> Self {
        Self {
            capability_timeout: Duration::from_secs(2),
            max_capability_buffer_bytes: 64 * 1024,
            max_process_timeout_ms: 120_000,
            max_process_output_bytes: 8 * 1024 * 1024,
            max_managed_processes: 16,
            default_recv_timeout_ms: 5000,
            max_recv_timeout_ms: 30_000,
            max_managed_stdout_bytes: 16 * 1024 * 1024,
            max_broker_continuation_rounds: 4,
            max_subagent_tasks: 8,
            max_subagent_task_chars: 32_000,
            subagent_concurrency_limit: 4,
            subagent_timeout: Duration::from_secs(10 * 60),
            subagent_recovery_prompt:
                "Continue from the recovered checkpoint and finish the assigned task.".into(),
        }
    }
}

impl CodingAgentConfig {
    /// Creates a new [`CodingAgentConfigBuilder`].
    pub fn builder() -> CodingAgentConfigBuilder {
        CodingAgentConfigBuilder::default()
    }
}

/// Builder for [`CodingAgentConfig`].
#[derive(Debug, Clone, Default)]
pub struct CodingAgentConfigBuilder {
    config: CodingAgentConfig,
}

impl CodingAgentConfigBuilder {
    pub fn capability_timeout(mut self, value: Duration) -> Self {
        self.config.capability_timeout = value;
        self
    }

    pub fn max_capability_buffer_bytes(mut self, value: usize) -> Self {
        self.config.max_capability_buffer_bytes = value;
        self
    }

    pub fn max_process_timeout_ms(mut self, value: u64) -> Self {
        self.config.max_process_timeout_ms = value;
        self
    }

    pub fn max_process_output_bytes(mut self, value: usize) -> Self {
        self.config.max_process_output_bytes = value;
        self
    }

    pub fn max_managed_processes(mut self, value: usize) -> Self {
        self.config.max_managed_processes = value;
        self
    }

    pub fn default_recv_timeout_ms(mut self, value: u64) -> Self {
        self.config.default_recv_timeout_ms = value;
        self
    }

    pub fn max_recv_timeout_ms(mut self, value: u64) -> Self {
        self.config.max_recv_timeout_ms = value;
        self
    }

    pub fn max_managed_stdout_bytes(mut self, value: usize) -> Self {
        self.config.max_managed_stdout_bytes = value;
        self
    }

    pub fn max_broker_continuation_rounds(mut self, value: usize) -> Self {
        self.config.max_broker_continuation_rounds = value;
        self
    }

    pub fn max_subagent_tasks(mut self, value: usize) -> Self {
        self.config.max_subagent_tasks = value;
        self
    }

    pub fn max_subagent_task_chars(mut self, value: usize) -> Self {
        self.config.max_subagent_task_chars = value;
        self
    }

    pub fn subagent_concurrency_limit(mut self, value: usize) -> Self {
        self.config.subagent_concurrency_limit = value;
        self
    }

    pub fn subagent_timeout(mut self, value: Duration) -> Self {
        self.config.subagent_timeout = value;
        self
    }

    pub fn subagent_recovery_prompt(mut self, value: impl Into<String>) -> Self {
        self.config.subagent_recovery_prompt = value.into();
        self
    }

    pub fn build(self) -> CodingAgentConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_config_builder_overrides_survive_serde_round_trip() {
        let config = CodingAgentConfig::builder()
            .max_subagent_tasks(3)
            .subagent_concurrency_limit(2)
            .build();
        assert_eq!(config.max_subagent_tasks, 3);
        assert_eq!(config.subagent_concurrency_limit, 2);
        let json = serde_json::to_string(&config).expect("serializes");
        let back: CodingAgentConfig = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back.max_subagent_tasks, 3);
        assert_eq!(back.subagent_concurrency_limit, 2);
        assert_eq!(CodingAgentConfig::default().max_subagent_tasks, 8);
    }
}
