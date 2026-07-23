use crate::types::{AgentMessage, AgentToolResult, TokenUsage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        usage: TokenUsage,
    },
    TurnStart {
        turn_number: usize,
    },
    TurnEnd {
        turn_number: usize,
        tool_results: Vec<AgentToolResult>,
    },
    MessageStart {
        role: String,
    },
    MessageUpdate {
        #[serde(skip_serializing_if = "Option::is_none")]
        text_delta: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_delta: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_name: Option<String>,
    },
    MessageEnd {
        message: AgentMessage,
    },
    ToolExecutionStart {
        tool_call_id: String,
        name: String,
        arguments: String,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        partial_result: String,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        name: String,
        result: AgentToolResult,
    },
    AgentError {
        error: String,
    },
}
