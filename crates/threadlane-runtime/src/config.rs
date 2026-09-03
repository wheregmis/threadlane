//! Centralized agent configuration.
//!
//! All tunable parameters for the agent execution loop, compaction, and
//! stream rules live here rather than as scattered `const` items.

use crate::types::{ModelRoles, ReasoningEffort};
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
    /// (read_file, edit_file_hashline, edit_files_hashline, write_file, run_command, subagent).
    /// Auxiliary tools remain executable directly or via the in-process `dyn` CLI.
    #[serde(default = "default_core_tool_schema_mode")]
    pub core_tool_schema_mode: bool,

    // ── Event Channel ───────────────────────────────────────────────────
    /// Capacity of the broadcast channel for [`AgentEvent`]s.
    pub(crate) event_channel_capacity: usize,
}

fn default_core_tool_schema_mode() -> bool {
    true
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
            default_system_prompt: "You are threadlane AI coding agent.".into(),
            model_roles: ModelRoles::default(),
            subagent_model: None,
            subagent_reasoning_effort: None,
            fast_reasoning_effort: None,
            needle_enabled: false,
            core_tool_schema_mode: true,
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
    pub fn auto_compaction_threshold_tokens(mut self, value: usize) -> Self {
        self.config.auto_compaction_threshold_tokens = value;
        self
    }

    pub fn auto_compaction_keep_recent_tokens(mut self, value: usize) -> Self {
        self.config.auto_compaction_keep_recent_tokens = value;
        self
    }

    pub fn max_checkpoint_chars(mut self, value: usize) -> Self {
        self.config.max_checkpoint_chars = value;
        self
    }

    pub fn estimated_image_tokens(mut self, value: usize) -> Self {
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

    pub fn model_roles(mut self, value: ModelRoles) -> Self {
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

    pub fn build(self) -> AgentConfig {
        self.config
    }
}
