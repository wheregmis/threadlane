use threadlane_provider::openai::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub messages: Vec<Message>,
    pub model: String,
}

impl Session {
    pub fn new(model: &str) -> Self {
        let system_message = Message {
            role: Role::System,
            content: Some(
                "You are threadlane, a minimal lightweight AI coding agent written in Rust. \
                You help developers inspect code, edit files, write new components, and run shell commands. \
                Always use the provided tools (read_file, write_file, edit_file, list_dir, run_command) \
                to interact with the workspace. Be concise, precise, and double check your work."
                    .to_string(),
            ),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };

        Self {
            messages: vec![system_message],
            model: model.to_string(),
        }
    }

    pub fn add_user_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::User,
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    pub fn add_assistant_message(
        &mut self,
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
    ) {
        self.messages.push(Message {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            name: None,
        });
    }

    pub fn add_tool_result(&mut self, tool_call_id: &str, tool_name: &str, result: &str) {
        self.messages.push(Message {
            role: Role::Tool,
            content: Some(result.to_string()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            name: Some(tool_name.to_string()),
        });
    }

    pub fn to_api_payload(&self, tools: Vec<Value>) -> Value {
        serde_json::json!({
            "model": self.model,
            "messages": self.messages,
            "tools": tools,
            "stream": true,
            "stream_options": { "include_usage": true }
        })
    }

    pub fn to_codex_payload(&self, tools: Vec<Value>) -> Value {
        serde_json::json!({
            "model": self.model,
            "input": self.messages,
            "store": false,
            "stream": true,
            "tools": tools
        })
    }
}
