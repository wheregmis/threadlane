//! Canonical UI-facing projections from the session journal.
//!
//! Host applications (such as `threadlane-gpui`) consume these projections
//! to render chat transcripts, tool activity, and reasoning blocks without
//! performing domain-level message reductions.

use serde::{Deserialize, Serialize};
use threadlane_protocol::{AgentMessage, RetryPrompt};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiMessageRole {
    User,
    Assistant,
    System,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiToolActivity {
    pub id: String,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub detail: String,
    /// Raw tool-call arguments JSON, preserved after the result lands in
    /// `detail` so UI surfaces can render the call (command, file path,
    /// edit payload) alongside its output.
    #[serde(default)]
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiChatMessage {
    pub id: String,
    pub role: UiMessageRole,
    pub content: String,
    pub tool_activities: Vec<UiToolActivity>,
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_prompt: Option<RetryPrompt>,
}

pub use threadlane_protocol::projection::{tool_activity_summary, tool_activity_display_summary};

/// Projects a sequence of [`AgentMessage`]s into canonical [`UiChatMessage`]s.
pub fn project_chat_messages(agent_messages: &[AgentMessage]) -> Vec<UiChatMessage> {
    let mut result = Vec::new();
    // Index of the latest activity per tool call id, replacing the previous
    // O(activities) reverse scan per tool result (worst-case O(n^2) on
    // tool-heavy transcripts). Overwriting on insert preserves the old
    // "latest match wins" semantics of the reverse search.
    let mut activity_index: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    let mut counter = 0usize;

    for msg in agent_messages {
        counter += 1;
        match msg {
            AgentMessage::User { content } => {
                result.push(UiChatMessage {
                    id: format!("msg_{counter}"),
                    role: UiMessageRole::User,
                    content: content.clone(),
                    tool_activities: Vec::new(),
                    reasoning_content: None,
                    retry_prompt: None,
                });
            }
            AgentMessage::UserWithImages { content, .. } => {
                result.push(UiChatMessage {
                    id: format!("msg_{counter}"),
                    role: UiMessageRole::User,
                    content: content.clone(),
                    tool_activities: Vec::new(),
                    reasoning_content: None,
                    retry_prompt: None,
                });
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut tool_activities = Vec::new();
                if let Some(calls) = tool_calls {
                    for call in calls {
                        let category = match call.function.name.as_str() {
                            "write_file"
                            | "replace_file_content"
                            | "multi_replace_file_content" => "Edited".into(),
                            "create_file" => "Created".into(),
                            "run_command" | "execute" => "Ran".into(),
                            "read_file" | "list_dir" => "Loaded".into(),
                            _ => "Explored".into(),
                        };
                        let detail = call.function.arguments.clone();
                        let title = call.function.name.clone();
                        let summary = tool_activity_summary(&title, &detail);
                        tool_activities.push(UiToolActivity {
                            id: call.id.clone(),
                            category,
                            summary,
                            title,
                            detail,
                            arguments: call.function.arguments.clone(),
                        });
                    }
                }
                let reasoning_content = result
                    .last()
                    .filter(|message| {
                        message.role == UiMessageRole::Assistant
                            && message.content.is_empty()
                            && message.tool_activities.is_empty()
                            && message.reasoning_content.is_some()
                    })
                    .and_then(|message| message.reasoning_content.clone());
                if reasoning_content.is_some() {
                    result.pop();
                }
                result.push(UiChatMessage {
                    id: format!("msg_{counter}"),
                    role: UiMessageRole::Assistant,
                    content: content.clone().unwrap_or_default(),
                    tool_activities,
                    reasoning_content,
                    retry_prompt: None,
                });
                // Index after the pop/push above: recording earlier would
                // capture a pre-pop position when a reasoning-only message
                // is merged into this one.
                if let Some(calls) = tool_calls {
                    let msg_idx = result.len() - 1;
                    for (act_idx, call) in calls.iter().enumerate() {
                        activity_index.insert(call.id.as_str(), (msg_idx, act_idx));
                    }
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                ..
            } => {
                let category = if *is_error { "Error" } else { "Result" };
                if let Some((msg_idx, act_idx)) = activity_index.get(tool_call_id.as_str()).copied()
                {
                    if let Some(activity) = result
                        .get_mut(msg_idx)
                        .and_then(|message| message.tool_activities.get_mut(act_idx))
                        .filter(|activity| activity.id == *tool_call_id)
                    {
                        activity.category = category.into();
                        activity.detail = content.clone();
                        continue;
                    }
                }
                let tool_info = UiToolActivity {
                    id: tool_call_id.clone(),
                    category: category.into(),
                    summary: tool_activity_summary(name, ""),
                    title: name.clone(),
                    detail: content.clone(),
                    arguments: String::new(),
                };
                if result
                    .last()
                    .is_some_and(|last| last.role == UiMessageRole::Assistant)
                {
                    let msg_idx = result.len() - 1;
                    let last = result.last_mut().expect("checked above");
                    let act_idx = last.tool_activities.len();
                    activity_index.insert(tool_call_id.as_str(), (msg_idx, act_idx));
                    last.tool_activities.push(tool_info);
                    continue;
                }
                let msg_idx = result.len();
                activity_index.insert(tool_call_id.as_str(), (msg_idx, 0));
                result.push(UiChatMessage {
                    id: format!("msg_{counter}"),
                    role: UiMessageRole::Assistant,
                    content: String::new(),
                    tool_activities: vec![tool_info],
                    reasoning_content: None,
                    retry_prompt: None,
                });
            }
            AgentMessage::System { content } => {
                let lowered = content.to_lowercase();
                let role = if lowered.contains("error") || lowered.contains("failed") {
                    UiMessageRole::Error
                } else {
                    UiMessageRole::System
                };
                result.push(UiChatMessage {
                    id: format!("msg_{counter}"),
                    role,
                    content: content.clone(),
                    tool_activities: Vec::new(),
                    reasoning_content: None,
                    retry_prompt: None,
                });
            }
            AgentMessage::Custom {
                custom_type,
                payload,
            } => {
                let text = payload
                    .get("text")
                    .or_else(|| payload.get("error"))
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| payload.to_string());
                if custom_type == "thinking" {
                    result.push(UiChatMessage {
                        id: format!("msg_{counter}"),
                        role: UiMessageRole::Assistant,
                        content: String::new(),
                        tool_activities: Vec::new(),
                        reasoning_content: Some(text),
                        retry_prompt: None,
                    });
                    continue;
                }
                if custom_type == "compaction_summary" {
                    let summary_text = payload
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Session history was compacted.")
                        .to_string();
                    result.push(UiChatMessage {
                        id: format!("msg_{counter}"),
                        role: UiMessageRole::System,
                        content: format!("Summary of prior conversation:\n{summary_text}"),
                        tool_activities: Vec::new(),
                        reasoning_content: None,
                        retry_prompt: None,
                    });
                    continue;
                }
                let is_error_type = custom_type == "error" || custom_type == "agent_error";
                let retry_prompt = if is_error_type {
                    payload
                        .get("retry_prompt")
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok())
                        .filter(RetryPrompt::is_sendable)
                } else {
                    None
                };
                result.push(UiChatMessage {
                    id: format!("msg_{counter}"),
                    role: if is_error_type {
                        UiMessageRole::Error
                    } else {
                        UiMessageRole::System
                    },
                    content: text,
                    tool_activities: Vec::new(),
                    reasoning_content: None,
                    retry_prompt,
                });
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_protocol::{RuntimeToolCall, RuntimeToolCallFunction};

    #[test]
    fn retry_prompt_is_owned_by_error_not_neighboring_user() {
        let retry = RetryPrompt {
            text: String::new(),
            images: vec![threadlane_protocol::ImageAttachment {
                display_name: "shot.png".into(),
                data_url: "data:image/png;base64,AA==".into(),
            }],
        };
        let rows = project_chat_messages(&[
            AgentMessage::user("old text", vec![]),
            AgentMessage::Custom {
                custom_type: "agent_error".into(),
                payload: serde_json::json!({"error":"failed", "retry_prompt": retry}),
            },
            AgentMessage::user("later queued prompt", vec![]),
            AgentMessage::Custom {
                custom_type: "agent_error".into(),
                payload: serde_json::json!({"error":"legacy"}),
            },
            AgentMessage::Custom {
                custom_type: "agent_error".into(),
                payload: serde_json::json!({"error":"malformed", "retry_prompt":{"text":"partial"}}),
            },
            AgentMessage::Custom {
                custom_type: "status".into(),
                payload: serde_json::json!({"text":"status", "retry_prompt":retry}),
            },
        ]);
        assert_eq!(rows[1].retry_prompt, Some(retry));
        for index in [0, 2, 3, 4, 5] {
            assert!(rows[index].retry_prompt.is_none());
        }
    }

    #[test]
    fn multiline_command_arguments_are_sanitized_to_single_line_summary() {
        let python_cmd = r#"{"CommandLine": "python3 - <<'PY'\nfrom pathlib import Path\np=Path('file.rs')\np.write_text('hello')\nPY"}"#;
        let messages = vec![AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![RuntimeToolCall {
                id: "call_123".into(),
                r#type: "function".into(),
                function: RuntimeToolCallFunction {
                    name: "run_command".into(),
                    arguments: python_cmd.into(),
                },
                thought_signature: None,
            }]),
            stop_reason: None,
            deferred_handle: None,
        }];

        let projected = project_chat_messages(&messages);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].tool_activities.len(), 1);
        let activity = &projected[0].tool_activities[0];
        assert_eq!(activity.summary, "run command: python3 - <<'PY' …");
        assert!(!activity.summary.contains('\n'));
    }
}
