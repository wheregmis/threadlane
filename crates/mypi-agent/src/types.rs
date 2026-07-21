use mypi_provider::openai::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum AgentMessage {
    System {
        content: String,
    },
    User {
        content: String,
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
    pub fn role_str(&self) -> &'static str {
        match self {
            AgentMessage::System { .. } => "system",
            AgentMessage::User { .. } => "user",
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
            tools: Vec::new(),
            messages: Vec::new(),
            is_streaming: false,
            pending_tool_calls: Vec::new(),
            metadata: HashMap::new(),
        }
    }
}
