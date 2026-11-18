//! Pure message translation: model-visible messages to provider payloads.
//!
//! These functions operate only on the shared contract types from
//! `threadlane-protocol`, so payload translation never depends on the
//! execution engine. The runtime's `ProviderAdapter` implementations delegate
//! to them; `threadlane-runtime` re-exports them for backward compatibility.

use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashSet;
use threadlane_protocol::AgentMessage;

pub const MAX_CONTEXT_SNAPSHOT_INDEX_ENTRIES: usize = 20;
pub const MAX_CONTEXT_SNAPSHOT_INDEX_CHARS: usize = 4_000;
const CONTEXT_SNAPSHOT_INDEX_HEADING: &str = "## Available context snapshots";

/// Extracts the summary text from a `compaction_summary` custom message.
pub fn compaction_summary_text(message: &AgentMessage) -> Option<&str> {
    let AgentMessage::Custom {
        custom_type,
        payload,
    } = message
    else {
        return None;
    };
    if custom_type != "compaction_summary" {
        return None;
    }
    payload.get("summary").and_then(serde_json::Value::as_str)
}

/// Renders a checkpoint message as provider-visible text, appending the
/// context-snapshot index when present. Public for the runtime's compaction
/// pass, which projects the same text into retained history.
pub fn compaction_checkpoint_text(message: &AgentMessage) -> Option<String> {
    let summary = compaction_summary_text(message)?;
    let AgentMessage::Custom { payload, .. } = message else {
        unreachable!();
    };
    let Some(entries) = payload
        .get("context_snapshot_index")
        .and_then(serde_json::Value::as_array)
    else {
        return Some(summary.to_owned());
    };
    let mut index = CONTEXT_SNAPSHOT_INDEX_HEADING.to_owned();
    let mut included = 0;
    for entry in entries.iter().take(MAX_CONTEXT_SNAPSHOT_INDEX_ENTRIES) {
        let (Some(context_id), Some(path), Some(file_sha256)) = (
            entry.get("context_id").and_then(serde_json::Value::as_str),
            entry.get("path").and_then(serde_json::Value::as_str),
            entry.get("file_sha256").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        let location = match (
            entry.get("start_line").and_then(serde_json::Value::as_u64),
            entry.get("end_line").and_then(serde_json::Value::as_u64),
        ) {
            (None, None) => path.to_owned(),
            (start, end) => format!(
                "{path}:{}-{}",
                start.map_or_else(String::new, |line| line.to_string()),
                end.map_or_else(String::new, |line| line.to_string())
            ),
        };
        let line = format!("- {context_id} {location} sha256={file_sha256}");
        if index.chars().count() + 1 + line.chars().count() > MAX_CONTEXT_SNAPSHOT_INDEX_CHARS {
            break;
        }
        index.push('\n');
        index.push_str(&line);
        included += 1;
    }
    Some(if included == 0 {
        summary.to_owned()
    } else {
        format!("{summary}\n\n{index}")
    })
}

pub(crate) fn normalized_tool_call_id(id: &str, empty_index: usize) -> String {
    if id.is_empty() {
        format!("call_{empty_index}")
    } else {
        id.to_string()
    }
}

/// Removes incomplete or ambiguous tool-call turns and orphaned results before provider
/// conversion. Provider APIs reject replaying either shape.
///
/// Moved from `threadlane-runtime::loop_engine`: it sits with
/// `normalized_tool_call_id`, which it uses to match calls to results, and
/// only touches the shared `AgentMessage` contract — never engine state.
pub(crate) fn repair_interrupted_tool_turn(messages: &mut Cow<'_, [AgentMessage]>) -> bool {
    let mut repaired = false;
    let mut index = 0;
    while index < messages.len() {
        if matches!(messages[index], AgentMessage::Tool { .. }) {
            messages.to_mut().remove(index);
            repaired = true;
            continue;
        }
        let AgentMessage::Assistant {
            tool_calls: Some(tool_calls),
            ..
        } = &messages[index]
        else {
            index += 1;
            continue;
        };
        if tool_calls.is_empty() {
            index += 1;
            continue;
        }

        let expected_ids: HashSet<String> = tool_calls
            .iter()
            .enumerate()
            .map(|(idx, call)| normalized_tool_call_id(&call.id, idx))
            .collect();
        let mut completed_ids = HashSet::new();
        let mut next = index + 1;
        let mut tool_index = 0;
        while let Some(AgentMessage::Tool { tool_call_id, .. }) = messages.get(next) {
            let id = normalized_tool_call_id(tool_call_id, tool_index);
            tool_index += 1;
            completed_ids.insert(id);
            next += 1;
        }

        if expected_ids.len() == tool_calls.len()
            && expected_ids == completed_ids
            && next - index - 1 == tool_calls.len()
        {
            index = next;
            continue;
        }

        let replacement = match &messages[index] {
            AgentMessage::Assistant {
                content: Some(content),
                stop_reason,
                deferred_handle,
                ..
            } if !content.trim().is_empty() => Some(AgentMessage::Assistant {
                content: Some(content.clone()),
                tool_calls: None,
                stop_reason: stop_reason.clone(),
                deferred_handle: deferred_handle.clone(),
            }),
            _ => None,
        };
        messages.to_mut().splice(index..next, replacement);
        repaired = true;
    }
    repaired
}

/// Converts agent messages into the standard Chat Completions message array.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Value> {
    let messages = normalize_tool_call_ids(messages);
    messages
        .iter()
        .filter_map(|msg| match msg {
            AgentMessage::System { content } => Some(serde_json::json!({
                "role": "system",
                "content": content
            })),
            AgentMessage::User { content } => Some(serde_json::json!({
                "role": "user",
                "content": content
            })),
            AgentMessage::UserWithImages { content, images } => {
                let mut parts = Vec::new();
                if !content.trim().is_empty() {
                    parts.push(serde_json::json!({
                        "type": "text",
                        "text": content
                    }));
                }
                parts.extend(images.iter().map(|image| {
                    serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": image.data_url,
                            "detail": "auto"
                        }
                    })
                }));
                let mut message = serde_json::json!({"role": "user"});
                message["content"] = parts.into();
                Some(message)
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut map = serde_json::Map::new();
                map.insert("role".into(), "assistant".into());
                if let Some(c) = content {
                    map.insert("content".into(), c.clone().into());
                }
                if let Some(t) = tool_calls {
                    map.insert(
                        "tool_calls".into(),
                        serde_json::to_value(t).unwrap_or_default(),
                    );
                }
                Some(Value::Object(map))
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                images,
                ..
            } => {
                let id_str = if tool_call_id.is_empty() {
                    "call_0"
                } else {
                    tool_call_id
                };
                // Chat Completions accepts content parts in tool messages, so
                // screenshots ride alongside the text result.
                let content = if images.is_empty() {
                    serde_json::Value::String(content.clone())
                } else {
                    let mut parts = Vec::new();
                    if !content.trim().is_empty() {
                        parts.push(serde_json::json!({
                            "type": "text",
                            "text": content
                        }));
                    }
                    parts.extend(images.iter().map(|image| {
                        serde_json::json!({
                            "type": "image_url",
                            "image_url": {
                                "url": image.data_url,
                                "detail": "auto"
                            }
                        })
                    }));
                    serde_json::Value::Array(parts)
                };
                let mut message = serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id_str,
                    "name": name
                });
                message["content"] = content;
                Some(message)
            }
            AgentMessage::Custom { .. } => compaction_checkpoint_text(msg).map(|checkpoint| {
                serde_json::json!({
                    "role": "user",
                    "content": format!("<context-checkpoint>\n{checkpoint}\n</context-checkpoint>")
                })
            }),
        })
        .collect()
}

/// Converts agent messages into the Codex Responses (instructions, input items) format.
pub fn convert_to_codex_llm(messages: &[AgentMessage]) -> (String, Vec<Value>) {
    let messages = normalize_tool_call_ids(messages);
    let mut instructions = String::new();
    let mut items = Vec::new();

    for msg in messages.iter() {
        match msg {
            AgentMessage::System { content } => {
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(content);
            }
            AgentMessage::User { content } => {
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": content }]
                }));
            }
            AgentMessage::UserWithImages { content, images } => {
                let mut parts = Vec::new();
                if !content.trim().is_empty() {
                    parts.push(serde_json::json!({
                        "type": "input_text",
                        "text": content
                    }));
                }
                parts.extend(images.iter().map(|image| {
                    serde_json::json!({
                        "type": "input_image",
                        "image_url": image.data_url,
                        "detail": "auto"
                    })
                }));
                let mut message = serde_json::json!({
                    "type": "message",
                    "role": "user"
                });
                message["content"] = parts.into();
                items.push(message);
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if let Some(c) = content {
                    if !c.trim().is_empty() {
                        items.push(serde_json::json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": c }]
                        }));
                    }
                }
                if let Some(t_calls) = tool_calls {
                    for tc in t_calls {
                        items.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.function.name,
                            "arguments": tc.function.arguments
                        }));
                    }
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                content,
                images,
                ..
            } => {
                // Responses function outputs accept a mixed text/image list,
                // so screenshots ride alongside the text result. Text-only
                // results keep the legacy string shape.
                if images.is_empty() {
                    items.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": tool_call_id,
                        "output": content
                    }));
                    continue;
                }
                let mut output = Vec::new();
                if !content.trim().is_empty() {
                    output.push(serde_json::json!({
                        "type": "input_text",
                        "text": content
                    }));
                }
                output.extend(images.iter().map(|image| {
                    serde_json::json!({
                        "type": "input_image",
                        "image_url": image.data_url,
                        "detail": "auto"
                    })
                }));
                let mut message = serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id
                });
                message["output"] = output.into();
                items.push(message);
            }
            AgentMessage::Custom { .. } => {
                if let Some(checkpoint) = compaction_checkpoint_text(msg) {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": format!("<context-checkpoint>\n{checkpoint}\n</context-checkpoint>")
                        }]
                    }));
                }
            }
        }
    }

    (instructions, items)
}

fn normalize_tool_call_ids(messages: &[AgentMessage]) -> Cow<'_, [AgentMessage]> {
    let mut normalized = Cow::Borrowed(messages);
    let mut tool_index = 0;
    for (index, message) in messages.iter().enumerate() {
        match message {
            AgentMessage::Assistant {
                tool_calls: Some(tool_calls),
                ..
            } => {
                tool_index = 0;
                if tool_calls.iter().any(|call| call.id.is_empty()) {
                    let AgentMessage::Assistant {
                        tool_calls: Some(calls),
                        ..
                    } = &mut normalized.to_mut()[index]
                    else {
                        unreachable!();
                    };
                    for (call_index, call) in calls.iter_mut().enumerate() {
                        if call.id.is_empty() {
                            call.id = normalized_tool_call_id("", call_index);
                        }
                    }
                }
            }
            AgentMessage::Tool { tool_call_id, .. } => {
                if tool_call_id.is_empty() {
                    let AgentMessage::Tool { tool_call_id, .. } = &mut normalized.to_mut()[index]
                    else {
                        unreachable!();
                    };
                    *tool_call_id = normalized_tool_call_id("", tool_index);
                }
                tool_index += 1;
            }
            _ => tool_index = 0,
        }
    }
    repair_interrupted_tool_turn(&mut normalized);
    normalized
}

#[cfg(test)]
mod repair_tests {
    use super::*;
    use threadlane_protocol::{RuntimeToolCall, RuntimeToolCallFunction};

    fn assistant_with_calls(ids: &[&str]) -> AgentMessage {
        AgentMessage::Assistant {
            content: None,
            tool_calls: Some(
                ids.iter()
                    .map(|id| RuntimeToolCall {
                        id: id.to_string(),
                        r#type: "function".to_string(),
                        function: RuntimeToolCallFunction {
                            name: "read_file".to_string(),
                            arguments: "{}".to_string(),
                        },
                        thought_signature: None,
                    })
                    .collect(),
            ),
            stop_reason: None,
            deferred_handle: None,
        }
    }

    fn tool_result(id: &str) -> AgentMessage {
        AgentMessage::Tool {
            tool_call_id: id.to_string(),
            name: "read_file".to_string(),
            content: "ok".to_string(),
            is_error: false,
            terminate: false,
            images: Vec::new(),
        }
    }

    #[test]
    fn complete_history_is_borrowed_before_provider_payload_allocation() {
        let messages = vec![assistant_with_calls(&["a"]), tool_result("a")];
        let normalized = normalize_tool_call_ids(&messages);
        assert_eq!(normalized.as_ptr(), messages.as_ptr());
    }

    #[test]
    fn image_parts_preserve_both_provider_payload_shapes() {
        let image = threadlane_protocol::ImageAttachment {
            data_url: "data:image/jpeg;base64,AA==".into(),
            display_name: "image.jpg".into(),
        };
        let mut tool = tool_result("a");
        if let AgentMessage::Tool { images, .. } = &mut tool {
            images.push(image.clone());
        }
        let messages = vec![
            AgentMessage::user("see", vec![image.clone()]),
            assistant_with_calls(&["a"]),
            tool,
        ];
        let chat = convert_to_llm(&messages);
        let chat_parts = |text| {
            serde_json::json!([
                {"type": "text", "text": text},
                {"type": "image_url", "image_url": {"url": image.data_url, "detail": "auto"}}
            ])
        };
        assert_eq!(
            chat[0],
            serde_json::json!({"role": "user", "content": chat_parts("see")})
        );
        assert_eq!(
            chat[2],
            serde_json::json!({
                "role": "tool", "tool_call_id": "a", "name": "read_file", "content": chat_parts("ok")
            })
        );
        let (_, responses) = convert_to_codex_llm(&messages);
        let response_parts = |text| {
            serde_json::json!([
                {"type": "input_text", "text": text},
                {"type": "input_image", "image_url": image.data_url, "detail": "auto"}
            ])
        };
        assert_eq!(
            responses[0],
            serde_json::json!({
                "type": "message", "role": "user", "content": response_parts("see")
            })
        );
        assert_eq!(
            responses[2],
            serde_json::json!({
                "type": "function_call_output", "call_id": "a", "output": response_parts("ok")
            })
        );
    }

    #[test]
    fn complete_turn_is_left_alone() {
        let mut messages = Cow::Owned(vec![
            assistant_with_calls(&["a", "b"]),
            tool_result("a"),
            tool_result("b"),
        ]);
        assert!(!repair_interrupted_tool_turn(&mut messages));
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn interrupted_turn_is_removed_without_dropping_later_messages() {
        let thinking = AgentMessage::Custom {
            custom_type: "thinking".to_string(),
            payload: serde_json::json!({}),
        };
        let mut messages = Cow::Owned(vec![
            AgentMessage::user("go", Vec::new()),
            thinking,
            assistant_with_calls(&["a", "b"]),
            tool_result("a"),
            AgentMessage::user("later", Vec::new()),
        ]);
        assert!(repair_interrupted_tool_turn(&mut messages));
        assert_eq!(messages.len(), 3);
        assert!(messages[0].is_user());
        assert!(matches!(messages[1], AgentMessage::Custom { .. }));
        assert!(messages[2].is_user());
    }

    #[test]
    fn duplicate_call_ids_are_repaired_only_in_the_provider_projection() {
        for ids in [["duplicate", "duplicate"], ["call_1", ""]] {
            let mut assistant = assistant_with_calls(&ids);
            if let AgentMessage::Assistant { content, .. } = &mut assistant {
                *content = Some("The operation was interrupted.".into());
            }
            let messages = vec![
                AgentMessage::user("go", Vec::new()),
                assistant,
                tool_result(ids[0]),
                tool_result(ids[1]),
                AgentMessage::user("continue", Vec::new()),
            ];
            let original = serde_json::to_value(&messages).unwrap();
            let chat = convert_to_llm(&messages);
            assert_eq!(chat.len(), 3);
            assert_eq!(chat[1]["content"], "The operation was interrupted.");
            assert!(chat[1].get("tool_calls").is_none());
            let (_, responses) = convert_to_codex_llm(&messages);
            assert_eq!(responses.len(), 3);
            assert!(responses.iter().all(|item| item["type"] == "message"));
            assert_eq!(serde_json::to_value(&messages).unwrap(), original);
        }
    }

    #[test]
    fn codex_conversion_drops_replayed_call_without_a_second_result() {
        let messages = vec![
            assistant_with_calls(&["call-1"]),
            tool_result("call-1"),
            assistant_with_calls(&["call-1"]),
            AgentMessage::user("continue", Vec::new()),
        ];

        let (_, items) = convert_to_codex_llm(&messages);
        assert_eq!(
            items
                .iter()
                .filter(|item| item["type"] == "function_call")
                .count(),
            1
        );
        assert_eq!(
            items
                .iter()
                .filter(|item| item["type"] == "function_call_output")
                .count(),
            1
        );
    }
}
