use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use threadlane_protocol::RuntimeToolCall as ToolCall;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

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

impl AgentToolDefinition {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: Some(description.into()),
            parameters,
            strict: None,
        }
    }

    /// Renders the nested function schema expected by Chat Completions.
    pub(crate) fn to_chat_completions_tool(&self) -> Value {
        let mut function = Map::new();
        function.insert("name".into(), self.name.clone().into());
        if let Some(description) = &self.description {
            function.insert("description".into(), description.clone().into());
        }
        function.insert("parameters".into(), self.parameters.clone());
        if let Some(strict) = self.strict {
            function.insert("strict".into(), strict.into());
        }

        serde_json::json!({
            "type": "function",
            "function": function,
        })
    }

    /// Renders the flat function schema expected by the Codex Responses API.
    pub fn to_codex_responses_tool(&self) -> Value {
        let mut tool = Map::new();
        tool.insert("type".into(), "function".into());
        tool.insert("name".into(), self.name.clone().into());
        if let Some(description) = &self.description {
            tool.insert("description".into(), description.clone().into());
        }
        tool.insert("parameters".into(), self.parameters.clone());
        if let Some(strict) = self.strict {
            tool.insert("strict".into(), strict.into());
        }
        Value::Object(tool)
    }

    /// Accepts either the nested Chat Completions shape or the flat Responses shape.
    pub fn from_provider_schema(schema: &Value) -> Result<Self, String> {
        let schema = schema
            .as_object()
            .ok_or_else(|| "Tool schema must be a JSON object".to_string())?;
        if schema.get("type").and_then(Value::as_str) != Some("function") {
            return Err("Tool schema type must be 'function'".to_string());
        }

        let function = match schema.get("function") {
            Some(value) => value
                .as_object()
                .ok_or_else(|| "Tool schema 'function' must be an object".to_string())?,
            None => schema,
        };
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| "Tool schema requires a non-empty name".to_string())?;
        let parameters = function
            .get("parameters")
            .cloned()
            .ok_or_else(|| format!("Tool schema '{name}' requires parameters"))?;
        let description = function
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let strict = function.get("strict").and_then(Value::as_bool);

        Ok(Self {
            name: name.to_string(),
            description,
            parameters,
            strict,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Off,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub(crate) fn as_api_str(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Minimal => "Minimal",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::XHigh => "XHigh",
            Self::Max => "Max",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        let label = label.strip_prefix("Thinking: ").unwrap_or(label).trim();
        match label.to_ascii_lowercase().as_str() {
            "off" | "none" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub display_name: String,
    pub data_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredHandle {
    handle_id: String,
    provider: String,
    model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentUsageSummary {
    input_tokens: u64,
    output_tokens: u64,
    total_subagents: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum AgentMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    UserWithImages {
        content: String,
        images: Vec<ImageAttachment>,
    },
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deferred_handle: Option<DeferredHandle>,
    },
    Tool {
        tool_call_id: String,
        name: String,
        content: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        terminate: bool,
    },
    Custom {
        custom_type: String,
        payload: Value,
    },
}

impl PartialEq for AgentMessage {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(self).ok() == serde_json::to_value(other).ok()
    }
}

impl AgentMessage {
    pub fn user(content: impl Into<String>, images: Vec<ImageAttachment>) -> Self {
        let content = content.into();
        if images.is_empty() {
            Self::User { content }
        } else {
            Self::UserWithImages { content, images }
        }
    }

    pub(crate) fn is_user(&self) -> bool {
        matches!(self, Self::User { .. } | Self::UserWithImages { .. })
    }

    pub fn same_user_message(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::User { content: left }, Self::User { content: right }) => left == right,
            (
                Self::UserWithImages {
                    content: left_content,
                    images: left_images,
                },
                Self::UserWithImages {
                    content: right_content,
                    images: right_images,
                },
            ) => left_content == right_content && left_images == right_images,
            _ => false,
        }
    }

    pub fn role_str(&self) -> &'static str {
        match self {
            AgentMessage::System { .. } => "system",
            AgentMessage::User { .. } | AgentMessage::UserWithImages { .. } => "user",
            AgentMessage::Assistant { .. } => "assistant",
            AgentMessage::Tool { .. } => "tool",
            AgentMessage::Custom { .. } => "custom",
        }
    }
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
pub struct AgentToolCall {
    pub id: String,
    pub name: String,
    pub(crate) arguments: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
    pub(crate) terminate: bool,
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

/// Orchestration mode governing automatic /prewalk engagement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMode {
    /// Automatically engage /prewalk on actionable coding tasks if a fast model is available.
    #[default]
    Auto,
    /// Always engage /prewalk on all incoming prompts.
    Always,
    /// Direct execution only (explicit /prewalk command required).
    Off,
}

impl OrchestratorMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Auto => "Auto (Complex Tasks)",
            Self::Always => "Always",
            Self::Off => "Off (Manual /prewalk)",
        }
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
