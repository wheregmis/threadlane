//! Bridges an ACP session onto the shared `AgentEvent` stream.
//!
//! Threadlane already renders streamed text, reasoning, tool activity, and
//! plans from `AgentEvent`. Mapping ACP's `session/update` notifications onto
//! that same contract means an external agent's turn renders through the
//! existing transcript pipeline rather than a parallel one, which is also why
//! the mapping lives here rather than in the UI crate: it is pure and testable
//! without a `Cx`.

use crate::acp::{
    AcpContentBlock, AcpPlanEntry, AcpPlanEntryStatus, AcpSessionUpdate, AcpToolCall,
    AcpToolCallStatus, AcpToolKind,
};
use threadlane_runtime::types::{AgentToolResult, PlanItem, PlanItemStatus, SessionPlan};
use threadlane_runtime::AgentEvent;

/// Model id prefix that selects an external ACP agent.
///
/// Mirrors the `antigravity/` convention so ACP agents flow through the
/// existing model picker, `/model` command, and per-session model persistence
/// without a second selection mechanism.
const ACP_MODEL_PREFIX: &str = "acp/";

/// Builds the model id for a configured agent.
pub fn acp_model_id(agent_id: &str) -> String {
    format!("{ACP_MODEL_PREFIX}{agent_id}")
}

/// Returns the agent id when `model` selects an ACP agent.
pub fn acp_agent_id(model: &str) -> Option<&str> {
    model
        .strip_prefix(ACP_MODEL_PREFIX)
        .filter(|id| !id.is_empty())
}

pub fn is_acp_model(model: &str) -> bool {
    acp_agent_id(model).is_some()
}

/// Names a tool call for the transcript.
///
/// ACP titles are human-facing ("Read main.rs"); the kind is the closest thing
/// to a stable tool name, so it is used when a title is absent.
fn tool_display_name(call: &AcpToolCall) -> String {
    if let Some(title) = call
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return title.to_string();
    }
    match call.kind {
        Some(AcpToolKind::Read) => "read",
        Some(AcpToolKind::Edit) => "edit",
        Some(AcpToolKind::Delete) => "delete",
        Some(AcpToolKind::Move) => "move",
        Some(AcpToolKind::Search) => "search",
        Some(AcpToolKind::Execute) => "execute",
        Some(AcpToolKind::Think) => "think",
        Some(AcpToolKind::Fetch) => "fetch",
        Some(AcpToolKind::SwitchMode) => "switch_mode",
        Some(AcpToolKind::Other) | None => "tool",
    }
    .to_string()
}

/// Flattens ACP tool content into displayable text.
fn tool_content_text(call: &AcpToolCall) -> String {
    let Some(items) = call.content.as_ref() else {
        return String::new();
    };
    let mut out = String::new();
    for item in items {
        // Content entries wrap a block under `content`; diffs and terminals
        // carry their own shapes, which are surfaced as their raw JSON rather
        // than dropped.
        let text = item
            .get("content")
            .and_then(|inner| inner.get("text"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                item.get("text")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| item.to_string());
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&text);
    }
    out
}

fn plan_status(status: AcpPlanEntryStatus) -> PlanItemStatus {
    match status {
        AcpPlanEntryStatus::Pending => PlanItemStatus::Pending,
        AcpPlanEntryStatus::InProgress => PlanItemStatus::InProgress,
        AcpPlanEntryStatus::Completed => PlanItemStatus::Completed,
    }
}

fn plan_from_entries(entries: Vec<AcpPlanEntry>) -> SessionPlan {
    SessionPlan {
        explanation: None,
        items: entries
            .into_iter()
            .map(|entry| PlanItem {
                step: entry.content,
                status: plan_status(entry.status),
            })
            .collect(),
    }
}

/// Translates one ACP update into zero or more `AgentEvent`s.
///
/// Updates with no transcript meaning (the echo of our own prompt, command
/// lists, mode changes) map to nothing rather than to placeholder output.
pub(crate) fn agent_events_for(update: AcpSessionUpdate) -> Vec<AgentEvent> {
    match update {
        AcpSessionUpdate::AgentMessageChunk(block) => text_of(&block)
            .map(|text| {
                vec![AgentEvent::MessageUpdate {
                    text_delta: Some(text),
                    reasoning_delta: None,
                    tool_call_name: None,
                }]
            })
            .unwrap_or_default(),
        AcpSessionUpdate::AgentThoughtChunk(block) => text_of(&block)
            .map(|text| {
                vec![AgentEvent::MessageUpdate {
                    text_delta: None,
                    reasoning_delta: Some(text),
                    tool_call_name: None,
                }]
            })
            .unwrap_or_default(),
        AcpSessionUpdate::ToolCall(call) => {
            let name = tool_display_name(&call);
            let arguments = call
                .raw_input
                .as_ref()
                .map(|input| input.to_string())
                .unwrap_or_else(|| "{}".to_string());
            // The transcript flushes buffered assistant text into the tool
            // activity group when it sees `tool_call_name`. Without this, an
            // agent's preamble ("Let me read that file…") stays stranded in the
            // assistant stream and ACP tool calls group differently from
            // native ones.
            vec![
                AgentEvent::MessageUpdate {
                    text_delta: None,
                    reasoning_delta: None,
                    tool_call_name: Some(name.clone()),
                },
                AgentEvent::ToolExecutionStart {
                    tool_call_id: call.tool_call_id.clone(),
                    name,
                    arguments,
                },
            ]
        }
        AcpSessionUpdate::ToolCallUpdate(call) => tool_update_events(call),
        AcpSessionUpdate::Plan(entries) => vec![AgentEvent::PlanUpdated {
            plan: plan_from_entries(entries),
        }],
        // The user's own message is already in the transcript, and command or
        // mode metadata has nowhere meaningful to render yet.
        AcpSessionUpdate::UserMessageChunk(_)
        | AcpSessionUpdate::AvailableCommandsUpdate(_)
        | AcpSessionUpdate::CurrentModeUpdate { .. }
        | AcpSessionUpdate::Other { .. } => Vec::new(),
    }
}

fn tool_update_events(call: AcpToolCall) -> Vec<AgentEvent> {
    let name = tool_display_name(&call);
    let content = tool_content_text(&call);
    match call.status {
        Some(AcpToolCallStatus::Completed) => {
            let output = if content.is_empty() {
                "(no output)".to_string()
            } else {
                content
            };
            vec![AgentEvent::ToolExecutionEnd {
                tool_call_id: call.tool_call_id.clone(),
                name: name.clone(),
                result: AgentToolResult::external(call.tool_call_id, name, output, false),
            }]
        }
        Some(AcpToolCallStatus::Failed) => {
            let output = if content.is_empty() {
                "Tool call failed.".to_string()
            } else {
                content
            };
            vec![AgentEvent::ToolExecutionEnd {
                tool_call_id: call.tool_call_id.clone(),
                name: name.clone(),
                result: AgentToolResult::external(call.tool_call_id, name, output, true),
            }]
        }
        // Pending/in-progress updates only matter when they carry new output.
        _ if !content.is_empty() => vec![AgentEvent::ToolExecutionUpdate {
            tool_call_id: call.tool_call_id,
            partial_result: content,
        }],
        _ => Vec::new(),
    }
}

fn text_of(block: &AcpContentBlock) -> Option<String> {
    block
        .as_text()
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_call(value: serde_json::Value) -> AcpToolCall {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn model_ids_round_trip_through_the_prefix() {
        assert_eq!(acp_model_id("claude_code"), "acp/claude_code");
        assert_eq!(acp_agent_id("acp/claude_code"), Some("claude_code"));
        assert!(is_acp_model("acp/gemini"));

        // Must not swallow ordinary models or a bare prefix.
        assert_eq!(acp_agent_id("gpt-5"), None);
        assert_eq!(acp_agent_id("antigravity/gemini-3.1-pro"), None);
        assert_eq!(acp_agent_id("acp/"), None);
        assert!(!is_acp_model("acp/"));
    }

    #[test]
    fn message_chunks_become_text_deltas() {
        let events = agent_events_for(AcpSessionUpdate::AgentMessageChunk(AcpContentBlock::text(
            "hello",
        )));
        let [AgentEvent::MessageUpdate {
            text_delta,
            reasoning_delta,
            ..
        }] = events.as_slice()
        else {
            panic!("expected one message update, got {events:?}");
        };
        assert_eq!(text_delta.as_deref(), Some("hello"));
        assert!(reasoning_delta.is_none());
    }

    #[test]
    fn thought_chunks_become_reasoning_deltas() {
        let events = agent_events_for(AcpSessionUpdate::AgentThoughtChunk(AcpContentBlock::text(
            "thinking",
        )));
        let [AgentEvent::MessageUpdate {
            text_delta,
            reasoning_delta,
            ..
        }] = events.as_slice()
        else {
            panic!("expected one message update, got {events:?}");
        };
        assert!(text_delta.is_none());
        assert_eq!(reasoning_delta.as_deref(), Some("thinking"));
    }

    #[test]
    fn empty_and_non_text_chunks_emit_nothing() {
        assert!(
            agent_events_for(AcpSessionUpdate::AgentMessageChunk(AcpContentBlock::text(
                ""
            )))
            .is_empty()
        );
        assert!(agent_events_for(AcpSessionUpdate::AgentMessageChunk(
            AcpContentBlock::Unknown
        ))
        .is_empty());
    }

    #[test]
    fn a_tool_call_starts_tool_activity() {
        let events = agent_events_for(AcpSessionUpdate::ToolCall(tool_call(json!({
            "toolCallId": "call_1",
            "title": "Read main.rs",
            "kind": "read",
            "rawInput": { "path": "src/main.rs" },
        }))));
        let [AgentEvent::MessageUpdate {
            tool_call_name: Some(boundary),
            ..
        }, AgentEvent::ToolExecutionStart {
            tool_call_id,
            name,
            arguments,
        }] = events.as_slice()
        else {
            panic!("expected a preamble flush then a tool start, got {events:?}");
        };
        // The flush must precede the tool row or the preamble is stranded.
        assert_eq!(boundary, "Read main.rs");
        assert_eq!(tool_call_id, "call_1");
        assert_eq!(name, "Read main.rs");
        assert!(arguments.contains("src/main.rs"));
    }

    #[test]
    fn a_tool_call_without_a_title_falls_back_to_its_kind() {
        let events = agent_events_for(AcpSessionUpdate::ToolCall(tool_call(json!({
            "toolCallId": "call_2",
            "kind": "execute",
        }))));
        let [_flush, AgentEvent::ToolExecutionStart {
            name, arguments, ..
        }] = events.as_slice()
        else {
            panic!("expected a preamble flush then a tool start, got {events:?}");
        };
        assert_eq!(name, "execute");
        // No rawInput must still produce valid arguments rather than empty text.
        assert_eq!(arguments, "{}");
    }

    #[test]
    fn a_completed_tool_call_ends_with_its_output() {
        let events = agent_events_for(AcpSessionUpdate::ToolCallUpdate(tool_call(json!({
            "toolCallId": "call_1",
            "title": "Read main.rs",
            "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": "fn main() {}" } }],
        }))));
        let [AgentEvent::ToolExecutionEnd { result, .. }] = events.as_slice() else {
            panic!("expected a tool end, got {events:?}");
        };
        assert_eq!(result.content, "fn main() {}");
        assert!(!result.is_error);
    }

    #[test]
    fn a_failed_tool_call_is_marked_as_an_error() {
        let events = agent_events_for(AcpSessionUpdate::ToolCallUpdate(tool_call(json!({
            "toolCallId": "call_1",
            "status": "failed",
        }))));
        let [AgentEvent::ToolExecutionEnd { result, .. }] = events.as_slice() else {
            panic!("expected a tool end, got {events:?}");
        };
        assert!(result.is_error);
        assert_eq!(result.content, "Tool call failed.");
    }

    #[test]
    fn a_completed_tool_call_without_output_still_reports_completion() {
        let events = agent_events_for(AcpSessionUpdate::ToolCallUpdate(tool_call(json!({
            "toolCallId": "call_1",
            "status": "completed",
        }))));
        let [AgentEvent::ToolExecutionEnd { result, .. }] = events.as_slice() else {
            panic!("expected a tool end, got {events:?}");
        };
        assert_eq!(result.content, "(no output)");
        assert!(!result.is_error);
    }

    #[test]
    fn in_progress_updates_only_surface_when_they_carry_output() {
        let with_output = agent_events_for(AcpSessionUpdate::ToolCallUpdate(tool_call(json!({
            "toolCallId": "call_1",
            "status": "in_progress",
            "content": [{ "type": "content", "content": { "type": "text", "text": "line 1" } }],
        }))));
        let [AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            partial_result,
        }] = with_output.as_slice()
        else {
            panic!("expected a partial update, got {with_output:?}");
        };
        assert_eq!(tool_call_id, "call_1");
        assert_eq!(partial_result, "line 1");

        // A bare status change would otherwise render as an empty update.
        let bare = agent_events_for(AcpSessionUpdate::ToolCallUpdate(tool_call(json!({
            "toolCallId": "call_1",
            "status": "in_progress",
        }))));
        assert!(bare.is_empty());
    }

    #[test]
    fn plans_map_onto_the_session_plan() {
        let events = agent_events_for(AcpSessionUpdate::Plan(
            serde_json::from_value(json!([
                { "content": "Read the code", "priority": "high", "status": "completed" },
                { "content": "Fix the bug", "priority": "high", "status": "in_progress" },
                { "content": "Run tests", "priority": "medium", "status": "pending" },
            ]))
            .unwrap(),
        ));
        let [AgentEvent::PlanUpdated { plan }] = events.as_slice() else {
            panic!("expected a plan update, got {events:?}");
        };
        assert_eq!(plan.items.len(), 3);
        assert_eq!(plan.items[0].step, "Read the code");
        assert_eq!(plan.items[0].status, PlanItemStatus::Completed);
        assert_eq!(plan.items[1].status, PlanItemStatus::InProgress);
        assert_eq!(plan.items[2].status, PlanItemStatus::Pending);
    }

    #[test]
    fn transcript_irrelevant_updates_are_dropped() {
        assert!(
            agent_events_for(AcpSessionUpdate::UserMessageChunk(AcpContentBlock::text(
                "echo of our own prompt"
            )))
            .is_empty()
        );
        assert!(agent_events_for(AcpSessionUpdate::CurrentModeUpdate {
            current_mode_id: "ask".into()
        })
        .is_empty());
        assert!(agent_events_for(AcpSessionUpdate::Other {
            kind: "usage_update".into(),
            payload: json!({}),
        })
        .is_empty());
    }
}
