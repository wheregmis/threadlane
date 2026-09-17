//! Agent lifecycle events: the engine-to-surface stream contract.
//!
//! Moved from `threadlane-runtime::events` (body verbatim): every turn,
//! tool, subagent, permission, and question update the engine emits is
//! defined here so session orchestration, daemon/transport mirrors, and UI
//! rendering share one definition without depending on the runtime.
//! `threadlane-runtime` re-exports this module for compatibility.

use crate::interaction::{PermissionRequest, QuestionRequest};
use crate::messages::{AgentMessage, AgentToolResult, SessionPlan, TokenUsage};
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
    pub(crate) active_lanes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::{PermissionScope, QuestionItem};
    use crate::messages::ImageAttachment;
    use crate::{RuntimeToolCall, RuntimeToolCallFunction};

    fn sample_result() -> AgentToolResult {
        AgentToolResult {
            tool_call_id: "call_1".to_string(),
            name: "read_file".to_string(),
            content: "contents".to_string(),
            is_error: false,
            terminate: false,
            images: vec![ImageAttachment {
                display_name: "shot.jpg".to_string(),
                data_url: "data:image/jpeg;base64,AAA".to_string(),
            }],
        }
    }

    fn round_trip(event: &AgentEvent) -> AgentEvent {
        let json = serde_json::to_string(event).expect("event serializes");
        serde_json::from_str::<AgentEvent>(&json).expect("event deserializes")
    }

    #[test]
    fn agent_event_wire_shape_survives_json_round_trip() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 30,
            cache_write_tokens: 40,
            total_tokens: 100,
        };
        let plan = SessionPlan {
            explanation: Some("why".to_string()),
            items: vec![crate::messages::PlanItem {
                step: "do it".to_string(),
                status: crate::messages::PlanItemStatus::InProgress,
            }],
        };
        let events = vec![
            AgentEvent::AgentStart,
            AgentEvent::AgentEnd {
                usage: usage.clone(),
            },
            AgentEvent::TurnStart { turn_number: 2 },
            AgentEvent::TurnEnd {
                turn_number: 2,
                tool_results: vec![sample_result()],
            },
            AgentEvent::MessageStart {
                role: "assistant".to_string(),
            },
            AgentEvent::MessageUpdate {
                text_delta: Some("hi".to_string()),
                reasoning_delta: None,
                tool_call_name: None,
            },
            AgentEvent::MessageEnd {
                message: AgentMessage::Assistant {
                    content: Some("done".to_string()),
                    tool_calls: Some(vec![RuntimeToolCall {
                        id: "a".to_string(),
                        r#type: "function".to_string(),
                        function: RuntimeToolCallFunction {
                            name: "read_file".to_string(),
                            arguments: "{}".to_string(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
            AgentEvent::ToolExecutionStart {
                tool_call_id: "a".to_string(),
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
            },
            AgentEvent::ToolExecutionUpdate {
                tool_call_id: "a".to_string(),
                partial_result: "part".to_string(),
            },
            AgentEvent::ToolExecutionEnd {
                tool_call_id: "a".to_string(),
                name: "read_file".to_string(),
                result: sample_result(),
            },
            AgentEvent::SubagentQueued {
                run_id: 1,
                task_index: 0,
                agent: "explore".to_string(),
                task: "look".to_string(),
            },
            AgentEvent::SubagentStarted {
                run_id: 1,
                task_index: 0,
                journal_run_id: "j1".to_string(),
                lane: "child".to_string(),
                agent: "explore".to_string(),
                task: "look".to_string(),
                model: "m".to_string(),
                isolation: Some(SubagentIsolation {
                    workspace: std::path::PathBuf::from("/tmp/w"),
                    branch: "b".to_string(),
                }),
            },
            AgentEvent::SubagentUpdate {
                run_id: 1,
                task_index: 0,
                journal_run_id: "j1".to_string(),
                lane: "child".to_string(),
                update: SubagentProgressUpdate::Usage {
                    usage: usage.clone(),
                },
            },
            AgentEvent::SubagentFinished {
                run_id: 1,
                task_index: 0,
                journal_run_id: "j1".to_string(),
                succeeded: true,
                error: None,
            },
            AgentEvent::SubagentRecovery {
                run_id: "j1".to_string(),
                status: SubagentRecoveryStatus::Recovered,
                detail: None,
            },
            AgentEvent::PlanUpdated { plan },
            AgentEvent::AgentError {
                error: "boom".to_string(),
            },
            AgentEvent::PermissionRequested {
                request: PermissionRequest {
                    id: "p1".to_string(),
                    capability: "network".to_string(),
                    title: "t".to_string(),
                    detail: "d".to_string(),
                    scopes: vec![PermissionScope::Once],
                },
            },
            AgentEvent::QuestionRequested {
                request: QuestionRequest {
                    id: "q1".to_string(),
                    questions: vec![QuestionItem {
                        id: "q".to_string(),
                        header: "h".to_string(),
                        question: "which?".to_string(),
                        options: vec![],
                        allow_custom: false,
                    }],
                },
            },
            AgentEvent::StreamRuleTriggered {
                rule_id: "r".to_string(),
                rule_name: "n".to_string(),
                matched_text: "m".to_string(),
                reminder: "rem".to_string(),
            },
            AgentEvent::PrewalkCompleted {
                model: "fast".to_string(),
                message: "ok".to_string(),
            },
        ];
        for event in &events {
            assert_eq!(&round_trip(event), event);
        }
    }

    #[test]
    fn token_usage_accumulates_saturating() {
        let mut total = TokenUsage::default();
        total.accumulate(&TokenUsage {
            input_tokens: 5,
            output_tokens: 7,
            cache_read_tokens: 1,
            cache_write_tokens: 2,
            total_tokens: 15,
        });
        assert_eq!(total.input_tokens, 5);
        assert_eq!(total.total_tokens, 15);
    }
}
