use mypi_provider::openai::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
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
    pub fn to_chat_completions_tool(&self) -> Value {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub fn as_api_str(self) -> Option<&'static str> {
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

impl Default for ReasoningEffort {
    fn default() -> Self {
        Self::Medium
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolExecutionMode {
    Sequential,
    Parallel,
}

impl Default for ToolExecutionMode {
    fn default() -> Self {
        Self::Parallel
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    All,
    OneAtATime,
}

impl Default for QueueMode {
    fn default() -> Self {
        Self::All
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub display_name: String,
    pub data_url: String,
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
    },
    Tool {
        tool_call_id: String,
        name: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    Custom {
        custom_type: String,
        payload: Value,
    },
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

    pub fn is_user(&self) -> bool {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
    pub terminate: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BeforeToolCallResult {
    pub block: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AfterToolCallResult {
    pub override_content: Option<String>,
    pub override_is_error: Option<bool>,
    pub terminate: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct AgentState {
    pub system_prompt: String,
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
    pub tools: Vec<Value>,
    pub messages: Vec<AgentMessage>,
    pub is_streaming: bool,
    pub pending_tool_calls: Vec<String>,
    pub metadata: HashMap<String, Value>,
}

impl AgentState {
    pub fn new(model: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            model: model.into(),
            reasoning_effort: ReasoningEffort::default(),
            tools: Vec::new(),
            messages: Vec::new(),
            is_streaming: false,
            pending_tool_calls: Vec::new(),
            metadata: HashMap::new(),
        }
    }
}
