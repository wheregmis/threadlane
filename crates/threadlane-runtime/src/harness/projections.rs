//! Canonical UI-facing projections from the session journal.
//!
//! Host applications (such as `threadlane-gpui`) consume these projections
//! to render chat transcripts, tool activity, and reasoning blocks without
//! performing domain-level message reductions.

use crate::types::AgentMessage;
use serde::{Deserialize, Serialize};

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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiChatMessage {
    pub id: String,
    pub role: UiMessageRole,
    pub content: String,
    pub tool_activities: Vec<UiToolActivity>,
    pub reasoning_content: Option<String>,
}

fn tool_activity_summary(name: &str, arguments: &str) -> String {
    let display_name = name.replace('_', " ");
    let Ok(args_val) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return display_name;
    };
    let context = [
        "path",
        "file_path",
        "FilePath",
        "TargetFile",
        "command",
        "CommandLine",
        "query",
        "Query",
        "regex",
        "glob",
        "pattern",
        "Pattern",
        "prompt",
        "Prompt",
        "description",
        "Description",
    ]
    .iter()
    .find_map(|key| args_val.get(key).and_then(|v| v.as_str()));

    if let Some(ctx) = context {
        let trimmed = ctx.trim();
        if !trimmed.is_empty() {
            let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
            let has_more_lines = trimmed.lines().nth(1).is_some();
            let mut summary_ctx = first_line.to_string();
            if has_more_lines && !summary_ctx.ends_with('…') && !summary_ctx.ends_with("...") {
                summary_ctx.push_str(" …");
            }
            return format!("{display_name}: {summary_ctx}");
        }
    }
    display_name
}

/// Projects a sequence of [`AgentMessage`]s into canonical [`UiChatMessage`]s.
pub fn project_chat_messages(agent_messages: &[AgentMessage]) -> Vec<UiChatMessage> {
    let mut result = Vec::new();
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
                });
            }
            AgentMessage::UserWithImages { content, .. } => {
                result.push(UiChatMessage {
                    id: format!("msg_{counter}"),
                    role: UiMessageRole::User,
                    content: content.clone(),
                    tool_activities: Vec::new(),
                    reasoning_content: None,
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
                });
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                ..
            } => {
                let category = if *is_error { "Error" } else { "Result" };
                if let Some(activity) = result
                    .iter_mut()
                    .rev()
                    .flat_map(|message| message.tool_activities.iter_mut().rev())
                    .find(|activity| activity.id == *tool_call_id)
                {
                    activity.category = category.into();
                    activity.detail = content.clone();
                    continue;
                }
                let tool_info = UiToolActivity {
                    id: tool_call_id.clone(),
                    category: category.into(),
                    summary: tool_activity_summary(name, ""),
                    title: name.clone(),
                    detail: content.clone(),
                };
                if let Some(last) = result.last_mut() {
                    if last.role == UiMessageRole::Assistant {
                        last.tool_activities.push(tool_info);
                        continue;
                    }
                }
                result.push(UiChatMessage {
                    id: format!("msg_{counter}"),
                    role: UiMessageRole::Assistant,
                    content: String::new(),
                    tool_activities: vec![tool_info],
                    reasoning_content: None,
                });
            }
            AgentMessage::System { content } => {
                let role = if content.to_lowercase().contains("error")
                    || content.to_lowercase().contains("failed")
                {
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
                    });
                    continue;
                }
                let is_error_type = custom_type == "error" || custom_type == "agent_error";
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
