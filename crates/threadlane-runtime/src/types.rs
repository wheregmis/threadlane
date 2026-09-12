use serde::{Deserialize, Serialize};

// Message and tool-schema contract types live in `threadlane-protocol` so the
// provider layer shares them without depending on the runtime. They are
// re-exported here so existing `threadlane_runtime::` paths keep working.
pub use threadlane_protocol::{
    AgentMessage, AgentToolCall, AgentToolDefinition, DeferredHandle, ImageAttachment,
    ReasoningEffort,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub step: String,
    pub status: PlanItemStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default)]
    pub items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    #[default]
    All,
    OneAtATime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentUsageSummary {
    input_tokens: u64,
    output_tokens: u64,
    total_subagents: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub total_tokens: u32,
}

impl TokenUsage {
    pub fn accumulate(&mut self, usage: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(usage.cache_write_tokens);
        self.total_tokens = self.total_tokens.saturating_add(usage.total_tokens);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
    pub(crate) terminate: bool,
    /// Model-visible images attached by the tool. Serialized inline so the
    /// durable transcript reproduces the exact provider-visible context.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
}

/// Rich tool output: text plus optional model-visible images. Executors keep
/// returning plain strings; only image-producing tools build this directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub images: Vec<ImageAttachment>,
}

impl From<String> for ToolOutput {
    fn from(content: String) -> Self {
        Self {
            content,
            images: Vec::new(),
        }
    }
}

impl AgentToolResult {
    pub fn terminates(&self) -> bool {
        self.terminate
    }

    /// Builds a tool result produced outside the built-in tool loop.
    ///
    /// External agents (ACP) report tool outcomes that need to reach the same
    /// transcript rendering as native tool calls, but they never terminate the
    /// loop, so `terminate` stays private and false.
    pub fn external(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            name: name.into(),
            content: content.into(),
            is_error,
            terminate: false,
            images: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelRoles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<String>,
    /// Ordered alternate models attempted after a pre-output quota/rate-limit failure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fallback_chain: Vec<String>,
    /// Persisted cooldown markers for temporarily exhausted provider/model routes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) cooldown_models: Vec<String>,
}

impl ModelRoles {
    pub fn resolve_fast<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.fast.as_deref().unwrap_or(fallback)
    }

    pub(crate) fn fallback_after<'a>(&'a self, current: &str) -> Option<&'a str> {
        self.fallback_chain
            .iter()
            .map(String::as_str)
            .find(|candidate| {
                *candidate != current
                    && !self
                        .cooldown_models
                        .iter()
                        .any(|cooldown| cooldown == candidate)
            })
    }
}

/// Orchestration mode governing explicit /prewalk engagement.
///
/// Prewalk is off by default (oh-my-pi parity): it is a one-shot handoff from
/// the active model to a faster/cheaper model after planning reaches
/// implementation. It is armed explicitly via `/prewalk` or `Always` mode;
/// there is no LLM intent classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMode {
    /// Deprecated: previously ran an LLM intent classifier. Now behaves as
    /// `Off` (direct execution) to preserve deserialization of old configs
    /// without paying classifier latency/cost.
    #[serde(alias = "auto")]
    Auto,
    /// Arm prewalk on all incoming prompts.
    Always,
    /// Direct execution only (explicit /prewalk command required).
    #[default]
    Off,
}

impl OrchestratorMode {
    pub fn label(&self) -> &'static str {
        match self {
            // Auto is retained only for backward compat; it no longer engages.
            Self::Auto => "Off (Manual /prewalk)",
            Self::Always => "Always",
            Self::Off => "Off (Manual /prewalk)",
        }
    }

    /// Whether this mode arms prewalk automatically. `Auto` is intentionally
    /// inert (see variant docs).
    pub fn arms_automatically(&self) -> bool {
        matches!(self, Self::Always)
    }
}

#[cfg(test)]
mod model_role_tests {
    use super::ModelRoles;

    #[test]
    fn fallback_skips_current_and_cooldown_routes() {
        let roles = ModelRoles {
            fallback_chain: vec!["primary".into(), "cooling".into(), "backup".into()],
            cooldown_models: vec!["cooling".into()],
            ..Default::default()
        };
        assert_eq!(roles.fallback_after("primary"), Some("backup"));
    }
}

#[derive(Debug, Clone)]
pub struct TurnState {
    pub system_prompt: String,
    pub messages: Vec<AgentMessage>,
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
    pub project_root: Option<std::path::PathBuf>,
}

impl TurnState {
    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }

    pub fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.reasoning_effort = effort;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_roles_resolve_fast_model_with_fallbacks() {
        let roles = ModelRoles {
            fast: Some("fast-model".into()),
            fallback_chain: vec!["primary".into(), "backup".into()],
            ..Default::default()
        };

        assert_eq!(roles.resolve_fast("base-model"), "fast-model");
        assert_eq!(roles.fallback_after("primary"), Some("backup"));
    }

    #[test]
    fn model_roles_are_backward_compatible_when_deserialized_without_fields() {
        let roles: ModelRoles = serde_json::from_str("{}").expect("default role config");
        assert_eq!(roles, ModelRoles::default());
        assert_eq!(roles.resolve_fast("base-model"), "base-model");
    }
}
