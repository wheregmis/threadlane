//! Chat panel state: chat messages, tool call presentations, and streaming status.

use mypi_agent::AgentMessage;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MsgRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ToolStatus {
    Running,
    Done,
    Error,
}

impl ToolStatus {
    pub fn glyph(self) -> &'static str {
        match self {
            ToolStatus::Running => "◌",
            ToolStatus::Done => "✓",
            ToolStatus::Error => "✗",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolPresentation {
    pub icon: String,
    pub title: String,
    pub primary: String,
    pub metadata: String,
    pub arguments_detail: String,
}

#[derive(Clone, Debug)]
pub enum ChatMessage {
    Text {
        role: MsgRole,
        text: String,
    },
    Thinking {
        text: String,
    },
    Tool {
        id: String,
        name: String,
        arguments: String,
        output: String,
        status: ToolStatus,
        presentation: ToolPresentation,
        result_preview: String,
        result_metadata: String,
        started_at: Instant,
    },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StreamingKind {
    Assistant,
    Thinking,
}

#[derive(Clone, Debug, Default)]
pub struct ChatData {
    pub messages: Vec<ChatMessage>,
    pub streaming_text: String,
    pub streaming_kind: Option<StreamingKind>,
}

impl ChatData {
    pub fn push_chat(&mut self, role: MsgRole, text: impl Into<String>) {
        self.messages.push(ChatMessage::Text {
            role,
            text: text.into(),
        });
    }

    pub fn push_thinking(&mut self, text: String) {
        push_thinking_locked(self, text);
    }

    pub fn push_stream_delta(&mut self, kind: StreamingKind, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.streaming_kind != Some(kind) {
            flush_streaming_locked(self);
            self.streaming_kind = Some(kind);
        }
        self.streaming_text.push_str(delta);
    }

    pub fn flush_streaming(&mut self) {
        flush_streaming_locked(self);
    }

    pub fn flush_tool_call_preamble(&mut self) {
        let text = std::mem::take(&mut self.streaming_text);
        self.streaming_kind = None;
        self.push_thinking(text);
    }

    pub fn push_tool(&mut self, id: String, name: String, arguments: String) {
        self.flush_streaming();
        let presentation = tool_presentation(&name, &arguments);
        if let Some(ChatMessage::Tool {
            name: existing_name,
            arguments: existing_arguments,
            status,
            presentation: existing_presentation,
            output,
            result_preview,
            result_metadata,
            started_at,
            ..
        }) = self.messages.iter_mut().rev().find(|message| {
            matches!(message, ChatMessage::Tool { id: existing_id, .. } if existing_id == &id)
        }) {
            *existing_name = name;
            *existing_arguments = arguments;
            *existing_presentation = presentation;
            *output = String::new();
            *result_preview = String::new();
            *result_metadata = "Running…".into();
            *status = ToolStatus::Running;
            *started_at = Instant::now();
            return;
        }
        self.messages.push(ChatMessage::Tool {
            id,
            name,
            arguments,
            output: String::new(),
            status: ToolStatus::Running,
            presentation,
            result_preview: String::new(),
            result_metadata: "Running…".into(),
            started_at: Instant::now(),
        });
    }

    pub fn update_tool(&mut self, id: &str, output: String, status: Option<ToolStatus>) {
        if let Some(ChatMessage::Tool {
            output: existing_output,
            status: existing_status,
            result_preview,
            result_metadata,
            started_at,
            ..
        }) = self.messages.iter_mut().rev().find(
            |message| matches!(message, ChatMessage::Tool { id: existing_id, .. } if existing_id == id),
        ) {
            *existing_output = output;
            *result_preview = tool_result_preview(existing_output, 800);
            *result_metadata = result_metadata_for(
                existing_output,
                status.unwrap_or(*existing_status),
                started_at.elapsed(),
            );
            if let Some(status) = status {
                *existing_status = status;
            }
        }
    }

    pub fn replace_from_agent_messages(&mut self, messages: &[AgentMessage]) {
        self.messages.clear();
        self.streaming_text.clear();
        self.streaming_kind = None;
        for msg in messages {
            match msg {
                AgentMessage::User { content } => self.push_chat(MsgRole::User, content.clone()),
                AgentMessage::Assistant {
                    content,
                    tool_calls,
                } => {
                    if let Some(text) = content {
                        if !text.is_empty() {
                            if tool_calls.is_some() {
                                self.push_thinking(text.clone());
                            } else {
                                self.push_chat(MsgRole::Assistant, text.clone());
                            }
                        }
                    }
                    if let Some(tool_calls) = tool_calls {
                        for call in tool_calls {
                            let presentation =
                                tool_presentation(&call.function.name, &call.function.arguments);
                            self.messages.push(ChatMessage::Tool {
                                id: call.id.clone(),
                                name: call.function.name.clone(),
                                arguments: call.function.arguments.clone(),
                                output: String::new(),
                                status: ToolStatus::Running,
                                presentation,
                                result_preview: String::new(),
                                result_metadata: "Awaiting result…".into(),
                                started_at: Instant::now(),
                            });
                        }
                    }
                }
                AgentMessage::Tool {
                    tool_call_id,
                    name,
                    content,
                    is_error,
                } => {
                    let status = if *is_error {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Done
                    };
                    self.update_tool(tool_call_id, content.clone(), Some(status));
                    if !self.messages.iter().any(|message| matches!(message, ChatMessage::Tool { id, .. } if id == tool_call_id)) {
                        let presentation = tool_presentation(name, "");
                        self.messages.push(ChatMessage::Tool {
                            id: tool_call_id.clone(), name: name.clone(), arguments: String::new(), output: content.clone(), status, presentation,
                            result_preview: tool_result_preview(content, 800), result_metadata: result_metadata_for(content, status, Duration::ZERO), started_at: Instant::now(),
                        });
                    }
                }
                AgentMessage::Custom {
                    custom_type,
                    payload,
                } if custom_type == "thinking" => {
                    if let Some(text) = payload.get("text").and_then(serde_json::Value::as_str) {
                        self.push_thinking(text.to_string());
                    }
                }
                AgentMessage::System { .. } | AgentMessage::Custom { .. } => {}
            }
        }
    }
}

fn push_thinking_locked(data: &mut ChatData, text: String) {
    if text.trim().is_empty() {
        return;
    }
    if let Some(ChatMessage::Thinking { text: existing }) = data.messages.last_mut() {
        if !existing.is_empty() {
            existing.push_str("\n\n");
        }
        existing.push_str(&text);
    } else {
        data.messages.push(ChatMessage::Thinking { text });
    }
}

fn flush_streaming_locked(data: &mut ChatData) {
    let text = std::mem::take(&mut data.streaming_text);
    let kind = data.streaming_kind.take();
    if text.trim().is_empty() {
        return;
    }
    match kind {
        Some(StreamingKind::Thinking) => push_thinking_locked(data, text),
        _ => data.messages.push(ChatMessage::Text {
            role: MsgRole::Assistant,
            text,
        }),
    }
}

pub fn tool_icon(name: &str) -> &'static str {
    match name {
        "read_file" => "⌕",
        "write_file" => "+",
        "edit_file" => "✎",
        "list_dir" => "□",
        "run_command" => "›_",
        _ => "•",
    }
}

pub fn tool_title(name: &str) -> String {
    match name {
        "run_command" => "Run command".into(),
        "read_file" => "Read file".into(),
        "write_file" => "Write file".into(),
        "edit_file" => "Edit file".into(),
        "list_dir" => "List directory".into(),
        _ => name.replace('_', " "),
    }
}

pub fn tool_presentation(name: &str, arguments: &str) -> ToolPresentation {
    let parsed = serde_json::from_str::<serde_json::Value>(arguments).ok();
    let args = parsed.as_ref();
    let get_str = |key: &str| {
        args.and_then(|value| value.get(key))
            .and_then(serde_json::Value::as_str)
    };
    let path = get_str("path").map(compact_path).unwrap_or_default();

    let (primary, metadata) = match name {
        "run_command" => (
            compact_command(get_str("command").unwrap_or(arguments)),
            String::new(),
        ),
        "read_file" => {
            let start = args
                .and_then(|value| value.get("start_line"))
                .and_then(serde_json::Value::as_u64);
            let end = args
                .and_then(|value| value.get("end_line"))
                .and_then(serde_json::Value::as_u64);
            let range = match (start, end) {
                (Some(start), Some(end)) => format!("lines {start}–{end}"),
                (Some(start), None) => format!("from line {start}"),
                _ => String::new(),
            };
            (path.clone(), range)
        }
        "write_file" => {
            let content = get_str("content").unwrap_or_default();
            (path.clone(), text_size_label(content))
        }
        "edit_file" => {
            let old = get_str("target").unwrap_or_default();
            let new = get_str("replacement").unwrap_or_default();
            (
                path.clone(),
                format!("−{} +{} lines", line_count(old), line_count(new)),
            )
        }
        "list_dir" => (
            if path.is_empty() {
                ".".into()
            } else {
                path.clone()
            },
            String::new(),
        ),
        _ => (truncate_chars(arguments, 120), String::new()),
    };

    let arguments_detail = parsed
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| arguments.to_string());
    ToolPresentation {
        icon: tool_icon(name).into(),
        title: tool_title(name),
        primary,
        metadata,
        arguments_detail,
    }
}

pub fn tool_preview(name: &str, arguments: &str) -> String {
    let presentation = tool_presentation(name, arguments);
    if presentation.metadata.is_empty() {
        presentation.primary
    } else if name == "read_file" {
        format!("{}  ({})", presentation.primary, presentation.metadata)
    } else {
        presentation.primary
    }
}

fn compact_command(command: &str) -> String {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&normalized, 48)
}

fn compact_path(path: &str) -> String {
    let candidate = Path::new(path);
    if let Ok(work_dir) = std::env::current_dir() {
        if let Ok(relative) = candidate.strip_prefix(&work_dir) {
            return relative.display().to_string();
        }
    }
    if candidate.is_absolute() {
        let parts: Vec<_> = candidate.components().collect();
        if parts.len() > 4 {
            return format!(
                "…/{}/{}/{}",
                parts[parts.len() - 3].as_os_str().to_string_lossy(),
                parts[parts.len() - 2].as_os_str().to_string_lossy(),
                parts[parts.len() - 1].as_os_str().to_string_lossy()
            );
        }
    }
    path.to_string()
}

fn line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

fn text_size_label(text: &str) -> String {
    format!(
        "{} lines · {}",
        line_count(text),
        byte_size_label(text.len())
    )
}

fn byte_size_label(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn tool_result_preview(output: &str, max_chars: usize) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "(empty output)".into();
    }
    let first_line = trimmed.lines().next().unwrap_or(trimmed);
    truncate_chars(first_line, max_chars)
}

pub fn result_metadata_for(output: &str, status: ToolStatus, duration: Duration) -> String {
    let secs = duration.as_secs_f32();
    let time_label = if secs < 0.1 {
        format!("<0.1s")
    } else {
        format!("{secs:.1}s")
    };
    match status {
        ToolStatus::Running => "Running…".into(),
        ToolStatus::Error => format!("Failed · {time_label}"),
        ToolStatus::Done => {
            let lines = line_count(output);
            let bytes = output.len();
            format!("{lines} lines · {} · {time_label}", byte_size_label(bytes))
        }
    }
}

pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
