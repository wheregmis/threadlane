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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolIcon {
    ReadFile,
    WriteFile,
    EditFile,
    ListDirectory,
    Terminal,
    Skill,
    Subagent,
    Generic,
}

#[derive(Clone, Debug)]
pub struct ToolPresentation {
    pub icon: ToolIcon,
    pub title: String,
    pub primary: String,
    pub metadata: String,
    pub arguments_detail: String,
    pub output_markdown: bool,
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
            result_metadata.clear();
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
            result_metadata: String::new(),
            started_at: Instant::now(),
        });
    }

    pub fn update_tool(&mut self, id: &str, output: String, status: Option<ToolStatus>) {
        if let Some(ChatMessage::Tool {
            name,
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
            *result_metadata = result_metadata_for_tool(
                name,
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
                AgentMessage::UserWithImages { content, images } => {
                    let names = images
                        .iter()
                        .map(|image| image.display_name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let text = if content.trim().is_empty() {
                        format!("Attached: {names}")
                    } else {
                        format!("{content}\n\nAttached: {names}")
                    };
                    self.push_chat(MsgRole::User, text);
                }
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
                                result_metadata: String::new(),
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
                            result_preview: tool_result_preview(content, 800), result_metadata: result_metadata_for_tool(name, content, status, Duration::ZERO), started_at: Instant::now(),
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
    let incoming = text.trim();
    if incoming.is_empty() {
        return;
    }
    if let Some(ChatMessage::Thinking { text: existing }) = data.messages.last_mut() {
        if existing.trim() == incoming {
            return;
        }
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

pub fn tool_icon(name: &str) -> ToolIcon {
    match name {
        "read_file" => ToolIcon::ReadFile,
        "write_file" => ToolIcon::WriteFile,
        "edit_file" => ToolIcon::EditFile,
        "list_dir" => ToolIcon::ListDirectory,
        "run_command" => ToolIcon::Terminal,
        "load_skill" => ToolIcon::Skill,
        "subagent" => ToolIcon::Subagent,
        _ => ToolIcon::Generic,
    }
}

pub fn tool_title(name: &str) -> String {
    match name {
        "run_command" => "Run command".into(),
        "read_file" => "Read file".into(),
        "write_file" => "Write file".into(),
        "edit_file" => "Edit file".into(),
        "list_dir" => "List directory".into(),
        "load_skill" => "Load skill".into(),
        "subagent" => "Delegate".into(),
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
    let pretty_arguments = parsed
        .as_ref()
        .and_then(|value| serde_json::to_string_pretty(value).ok())
        .unwrap_or_else(|| arguments.to_string());

    let (primary, metadata, arguments_detail, output_markdown) = match name {
        "run_command" => (
            compact_command(get_str("command").unwrap_or(arguments)),
            String::new(),
            pretty_arguments.clone(),
            false,
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
            (path.clone(), range, pretty_arguments.clone(), false)
        }
        "write_file" => {
            let content = get_str("content").unwrap_or_default();
            (
                path.clone(),
                text_size_label(content),
                pretty_arguments.clone(),
                false,
            )
        }
        "edit_file" => {
            let old = get_str("target").unwrap_or_default();
            let new = get_str("replacement").unwrap_or_default();
            (
                path.clone(),
                format!("−{} +{} lines", line_count(old), line_count(new)),
                pretty_arguments.clone(),
                false,
            )
        }
        "list_dir" => (
            if path.is_empty() {
                ".".into()
            } else {
                path.clone()
            },
            String::new(),
            pretty_arguments.clone(),
            false,
        ),
        "load_skill" => {
            let skill_id = get_str("name").unwrap_or_default();
            (
                truncate_chars(skill_id, 96),
                "skill instructions".into(),
                format!("Skill ID: {}", truncate_chars(skill_id, 128)),
                true,
            )
        }
        "subagent" => {
            let (primary, metadata, detail) = subagent_presentation(args, arguments);
            (primary, metadata, detail, true)
        }
        _ => (
            truncate_chars(arguments, 120),
            String::new(),
            pretty_arguments,
            false,
        ),
    };

    ToolPresentation {
        icon: tool_icon(name),
        title: tool_title(name),
        primary,
        metadata,
        arguments_detail,
        output_markdown,
    }
}

fn subagent_presentation(
    args: Option<&serde_json::Value>,
    fallback: &str,
) -> (String, String, String) {
    const MAX_VISIBLE_TASKS: usize = 8;
    const MAX_VISIBLE_AGENT_CHARS: usize = 128;
    const MAX_VISIBLE_TASK_CHARS: usize = 240;

    let Some(tasks) = args
        .and_then(|value| value.get("tasks"))
        .and_then(serde_json::Value::as_array)
    else {
        return (
            truncate_chars(fallback, 120),
            String::new(),
            truncate_chars(fallback, 2_000),
        );
    };
    let visible_tasks: Vec<_> = tasks.iter().take(MAX_VISIBLE_TASKS).collect();
    let parallel = args
        .and_then(|value| value.get("parallel"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mode = if parallel { "parallel" } else { "sequential" };
    let agents: Vec<String> = visible_tasks
        .iter()
        .filter_map(|task| task.get("agent").and_then(serde_json::Value::as_str))
        .map(|agent| truncate_chars(agent, MAX_VISIBLE_AGENT_CHARS))
        .collect();
    let task_summary = |task: &serde_json::Value| {
        task.get("task")
            .and_then(serde_json::Value::as_str)
            .map(|task| normalize_whitespace_bounded(task, MAX_VISIBLE_TASK_CHARS))
            .unwrap_or_default()
    };

    let primary = if tasks.len() == 1 {
        let agent = agents.first().map(String::as_str).unwrap_or("subagent");
        let summary = task_summary(visible_tasks[0]);
        format!(
            "{} · {}",
            truncate_chars(agent, 64),
            truncate_chars(&summary, 84)
        )
    } else {
        format!("{} tasks", tasks.len())
    };
    let metadata = {
        let agent_list = truncate_chars(&agents.join(", "), 160);
        if agent_list.is_empty() {
            mode.to_string()
        } else {
            format!("{mode} · {agent_list}")
        }
    };
    let mut detail = format!(
        "Mode: {}\nTasks:",
        if parallel { "Parallel" } else { "Sequential" }
    );
    for (index, task) in visible_tasks.iter().enumerate() {
        let agent = task
            .get("agent")
            .and_then(serde_json::Value::as_str)
            .map(|agent| truncate_chars(agent, MAX_VISIBLE_AGENT_CHARS))
            .unwrap_or_else(|| "subagent".into());
        let summary = task_summary(task);
        detail.push_str(&format!("\n{}. {} — {}", index + 1, agent, summary));
    }
    if tasks.len() > visible_tasks.len() {
        detail.push_str(&format!(
            "\n… {} additional tasks omitted",
            tasks.len() - visible_tasks.len()
        ));
    }
    (primary, metadata, detail)
}

fn normalize_whitespace_bounded(text: &str, max_chars: usize) -> String {
    text.chars()
        .take(max_chars)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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

pub fn tool_result_detail(output: &str, max_chars: usize) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "(empty output)".into();
    }
    truncate_chars(trimmed, max_chars)
}

pub fn result_metadata_for(output: &str, status: ToolStatus, duration: Duration) -> String {
    result_metadata_for_tool("", output, status, duration)
}

fn result_metadata_for_tool(
    name: &str,
    output: &str,
    status: ToolStatus,
    duration: Duration,
) -> String {
    let secs = duration.as_secs_f32();
    let time_label = if secs < 0.1 {
        format!("<0.1s")
    } else {
        format!("{secs:.1}s")
    };
    match status {
        ToolStatus::Running => String::new(),
        ToolStatus::Error => format!("Failed · {time_label}"),
        ToolStatus::Done if name == "load_skill" => format!("Loaded · {time_label}"),
        ToolStatus::Done if name == "subagent" => format!("Completed · {time_label}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_consecutive_thinking_is_not_duplicated() {
        let mut data = ChatData::default();

        data.push_thinking("Listing repository files and status".into());
        data.push_thinking("Listing repository files and status".into());

        assert_eq!(data.messages.len(), 1);
        assert!(matches!(
            &data.messages[0],
            ChatMessage::Thinking { text }
                if text == "Listing repository files and status"
        ));
    }

    #[test]
    fn distinct_consecutive_thinking_is_preserved() {
        let mut data = ChatData::default();

        data.push_thinking("Listing repository files".into());
        data.push_thinking("Reviewing manifests".into());

        assert!(matches!(
            &data.messages[0],
            ChatMessage::Thinking { text }
                if text == "Listing repository files\n\nReviewing manifests"
        ));
    }

    #[test]
    fn load_skill_has_a_compact_markdown_presentation() {
        let presentation = tool_presentation("load_skill", r#"{"name":"rust-review"}"#);

        assert_eq!(presentation.icon, ToolIcon::Skill);
        assert_eq!(presentation.title, "Load skill");
        assert_eq!(presentation.primary, "rust-review");
        assert_eq!(presentation.metadata, "skill instructions");
        assert_eq!(presentation.arguments_detail, "Skill ID: rust-review");
        assert!(presentation.output_markdown);
    }

    #[test]
    fn subagent_presentation_summarizes_parallel_tasks() {
        let arguments = serde_json::json!({
            "tasks": [
                {"agent": "scout", "task": "Inspect the repository structure"},
                {"agent": "reviewer", "task": "Review the security boundaries"}
            ],
            "parallel": true
        })
        .to_string();
        let presentation = tool_presentation("subagent", &arguments);

        assert_eq!(presentation.icon, ToolIcon::Subagent);
        assert_eq!(presentation.title, "Delegate");
        assert_eq!(presentation.primary, "2 tasks");
        assert_eq!(presentation.metadata, "parallel · scout, reviewer");
        assert!(presentation.arguments_detail.contains("Mode: Parallel"));
        assert!(presentation
            .arguments_detail
            .contains("1. scout — Inspect the repository structure"));
        assert!(presentation.output_markdown);
    }

    #[test]
    fn specialized_tool_completion_metadata_is_action_oriented() {
        assert_eq!(
            result_metadata_for_tool(
                "load_skill",
                "instructions",
                ToolStatus::Done,
                Duration::from_secs(2)
            ),
            "Loaded · 2.0s"
        );
        assert_eq!(
            result_metadata_for_tool(
                "subagent",
                "report",
                ToolStatus::Done,
                Duration::from_secs(3)
            ),
            "Completed · 3.0s"
        );
    }
}
