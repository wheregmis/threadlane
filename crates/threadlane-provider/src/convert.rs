//! Pure message translation: model-visible messages to provider payloads.
//!
//! These functions operate only on the shared contract types from
//! `threadlane-protocol`, so payload translation never depends on the
//! execution engine. The runtime's `ProviderAdapter` implementations delegate
//! to them; `threadlane-runtime` re-exports them for backward compatibility.

use serde_json::Value;
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

pub fn normalized_tool_call_id(id: &str, empty_index: usize) -> String {
    if id.is_empty() {
        format!("call_{empty_index}")
    } else {
        id.to_string()
    }
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
                Some(serde_json::json!({
                    "role": "user",
                    "content": parts
                }))
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
                Some(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id_str,
                    "name": name,
                    "content": content
                }))
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

    for msg in &messages {
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
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": parts
                }));
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
                items.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": output
                }));
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

fn normalize_tool_call_ids(messages: &[AgentMessage]) -> Vec<AgentMessage> {
    let mut tool_index = 0;
    messages
        .iter()
        .map(|message| match message {
            AgentMessage::Assistant {
                content,
                tool_calls: Some(tool_calls),
                stop_reason,
                deferred_handle,
            } => {
                tool_index = 0;
                AgentMessage::Assistant {
                    content: content.clone(),
                    tool_calls: Some(
                        tool_calls
                            .iter()
                            .enumerate()
                            .map(|(idx, call)| {
                                let mut call = call.clone();
                                call.id = normalized_tool_call_id(&call.id, idx);
                                call
                            })
                            .collect(),
                    ),
                    stop_reason: stop_reason.clone(),
                    deferred_handle: deferred_handle.clone(),
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                terminate,
                images,
            } => {
                let normalized = normalized_tool_call_id(tool_call_id, tool_index);
                tool_index += 1;
                AgentMessage::Tool {
                    tool_call_id: normalized,
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    terminate: *terminate,
                    images: images.clone(),
                }
            }
            other => {
                tool_index = 0;
                other.clone()
            }
        })
        .collect()
}
