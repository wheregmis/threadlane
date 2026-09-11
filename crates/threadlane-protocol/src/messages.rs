//! Model-visible message and tool-schema contract types.
//!
//! These types describe what the model sees (messages, tool definitions,
//! reasoning effort) independent of any execution engine. They live here so
//! both the provider layer (payload translation, model registry) and the
//! runtime layer (turn driver, harness, persistence) share one definition;
//! `threadlane-runtime` re-exports them for backward compatibility.

use crate::RuntimeToolCall as ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningEffort {
    Off,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
    /// Provider/model-defined effort level loaded at runtime (e.g. from
    /// `models.json` or an ACP `thought_level` option). Stored as a leaked
    /// lowercase API value so the enum stays `Copy` and wire-compatible.
    Other(&'static str),
}

impl ReasoningEffort {
    /// All built-in levels in picker order. Custom levels from the model
    /// registry are appended by callers.
    pub fn known_levels() -> [Self; 7] {
        [
            Self::Off,
            Self::Minimal,
            Self::Low,
            Self::Medium,
            Self::High,
            Self::XHigh,
            Self::Max,
        ]
    }

    pub fn is_custom(self) -> bool {
        matches!(self, Self::Other(_))
    }

    /// Builds a custom level from a runtime string. Empty strings return
    /// `None`; known names resolve to their built-in variant.
    pub fn custom(value: &str) -> Option<Self> {
        Self::from_label(value)
    }

    pub fn as_api_str(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
            Self::Other(value) => {
                let normalized = value.trim().to_ascii_lowercase();
                if normalized.is_empty() || normalized == "off" || normalized == "none" {
                    None
                } else {
                    Some(value)
                }
            }
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
            Self::Other(value) => value,
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        let label = label.strip_prefix("Thinking: ").unwrap_or(label).trim();
        match label.to_ascii_lowercase().as_str() {
            "" => None,
            "off" | "none" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => {
                let normalized = label.to_ascii_lowercase();
                let leaked: &'static str = Box::leak(normalized.into_boxed_str());
                Some(Self::Other(leaked))
            }
        }
    }
}

impl serde::Serialize for ReasoningEffort {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Off => serializer.serialize_str("off"),
            Self::Minimal => serializer.serialize_str("minimal"),
            Self::Low => serializer.serialize_str("low"),
            Self::Medium => serializer.serialize_str("medium"),
            Self::High => serializer.serialize_str("high"),
            Self::XHigh => serializer.serialize_str("xhigh"),
            Self::Max => serializer.serialize_str("max"),
            Self::Other(value) => serializer.serialize_str(value),
        }
    }
}

impl<'de> serde::Deserialize<'de> for ReasoningEffort {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_label(&value).ok_or_else(|| serde::de::Error::custom("empty reasoning effort"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub display_name: String,
    pub data_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredHandle {
    pub handle_id: String,
    pub provider: String,
    pub model: String,
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
        /// Model-visible images attached by the tool (e.g. screenshots).
        /// Empty for text-only results; serialized inline so durable reload
        /// reproduces the exact provider-visible context.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageAttachment>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}
