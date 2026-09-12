use crate::types::{AgentMessage, AgentToolResult, SessionPlan, TokenUsage};
// Interaction contracts (permission prompts, clarifying questions) are
// canonical in `threadlane_protocol::interaction`; re-exported here so
// existing `threadlane_runtime::…` paths keep working.
pub use threadlane_protocol::interaction::{
    PermissionRequest, PermissionScope, QuestionAnswer, QuestionItem, QuestionItemAnswer,
    QuestionRequest,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentIsolation {
    pub workspace: std::path::PathBuf,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    SubagentQueued {
        run_id: u64,
        task_index: usize,
        agent: String,
        task: String,
    },
    SubagentStarted {
        run_id: u64,
        task_index: usize,
        journal_run_id: String,
        lane: String,
        agent: String,
        task: String,
        model: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        isolation: Option<SubagentIsolation>,
    },
    SubagentUpdate {
        run_id: u64,
        task_index: usize,
        journal_run_id: String,
        lane: String,
        update: SubagentProgressUpdate,
    },
    SubagentFinished {
        run_id: u64,
        task_index: usize,
        journal_run_id: String,
        succeeded: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    SubagentRecovery {
        run_id: String,
        status: SubagentRecoveryStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    PlanUpdated {
        plan: SessionPlan,
    },
    AgentError {
        error: String,
    },
    PermissionRequested {
        request: PermissionRequest,
    },
    QuestionRequested {
        request: QuestionRequest,
    },
    StreamRuleTriggered {
        rule_id: String,
        rule_name: String,
        matched_text: String,
        reminder: String,
    },
    PrewalkCompleted {
        model: String,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentRecoveryStatus {
    Started,
    Recovered,
    Retrying,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubagentProgressUpdate {
    TextDelta {
        delta: String,
    },
    ReasoningDelta {
        delta: String,
    },
    ToolStarted {
        tool_call_id: String,
        name: String,
        arguments: String,
    },
    ToolUpdated {
        tool_call_id: String,
        partial_result: String,
    },
    ToolFinished {
        tool_call_id: String,
        name: String,
        result: AgentToolResult,
    },
    Usage {
        usage: TokenUsage,
    },
    Error {
        error: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessMetrics {
    total_runs: u64,
    total_tools_executed: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    pub active_lanes: usize,
}
