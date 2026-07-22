//! Shared GUI state: chat transcript, plan panel, sessions sidebar, and the
//! event type used to pump agent/background-task results onto the UI thread.
//!
//! The chat/plan/session data lives in `static RwLock`s so the custom list
//! widgets (see `chat.rs`) can read it during their draw pass
//! without fighting `Scope` lifetimes — same pattern as makepad's aichat example.

use mypi_agent::{AgentEvent, AgentMessage, SessionTree};
use mypi_coding_agent::{TaskAgentEvent, WasiExtensionManager};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Events sent from background tokio tasks to the UI thread.
pub enum GuiAgentEvent {
    TaskEvent(TaskAgentEvent),
    Agent(AgentEvent),
    DeviceCodePrompt {
        user_code: String,
        url: String,
    },
    DeviceLoginSuccess,
    AvailableModelsLoaded(Vec<String>),
    CommandOutput(String),
    /// Session file swap finished; UI should rebuild chat from these messages.
    SessionSwitched {
        session_id: String,
        title: String,
        work_dir: PathBuf,
        messages: Vec<AgentMessage>,
    },
}

// ---------------------------------------------------------------------------
// Chat transcript
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum MsgRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, Copy, PartialEq)]
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

#[derive(Clone)]
pub struct ToolPresentation {
    pub title: String,
    pub primary: String,
    pub metadata: String,
    pub arguments_detail: String,
}

#[derive(Clone)]
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

#[derive(Clone, Copy, PartialEq)]
pub enum StreamingKind {
    Assistant,
    Thinking,
}

pub struct ChatData {
    pub messages: Vec<ChatMessage>,
    pub streaming_text: String,
    pub streaming_kind: Option<StreamingKind>,
}

pub static CHAT_DATA: RwLock<ChatData> = RwLock::new(ChatData {
    messages: Vec::new(),
    streaming_text: String::new(),
    streaming_kind: None,
});

pub fn push_chat(role: MsgRole, text: impl Into<String>) {
    CHAT_DATA.write().unwrap().messages.push(ChatMessage::Text {
        role,
        text: text.into(),
    });
}

fn flush_streaming_locked(data: &mut ChatData) {
    let text = std::mem::take(&mut data.streaming_text);
    let kind = data.streaming_kind.take();
    if text.trim().is_empty() {
        return;
    }
    data.messages.push(match kind {
        Some(StreamingKind::Thinking) => ChatMessage::Thinking { text },
        _ => ChatMessage::Text {
            role: MsgRole::Assistant,
            text,
        },
    });
}

fn push_stream_delta_for(kind: StreamingKind, delta: &str) {
    if delta.is_empty() {
        return;
    }
    let mut data = CHAT_DATA.write().unwrap();
    if data.streaming_kind != Some(kind) {
        flush_streaming_locked(&mut data);
        data.streaming_kind = Some(kind);
    }
    data.streaming_text.push_str(delta);
}

pub fn push_stream_delta(delta: &str) {
    push_stream_delta_for(StreamingKind::Assistant, delta);
}

pub fn push_reasoning_delta(delta: &str) {
    push_stream_delta_for(StreamingKind::Thinking, delta);
}

pub fn flush_streaming() {
    flush_streaming_locked(&mut CHAT_DATA.write().unwrap());
}

pub fn push_tool(id: String, name: String, arguments: String) {
    flush_streaming();
    let presentation = tool_presentation(&name, &arguments);
    let mut data = CHAT_DATA.write().unwrap();
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
    }) = data.messages.iter_mut().rev().find(|message| {
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
    data.messages.push(ChatMessage::Tool {
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

pub fn update_tool(id: &str, output: String, status: Option<ToolStatus>) {
    let mut data = CHAT_DATA.write().unwrap();
    if let Some(ChatMessage::Tool {
        output: existing_output,
        status: existing_status,
        result_preview,
        result_metadata,
        started_at,
        ..
    }) = data.messages.iter_mut().rev().find(
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
            truncate_chars(get_str("command").unwrap_or(arguments), 300),
            get_str("cwd")
                .filter(|cwd| !cwd.is_empty())
                .map(|cwd| format!("in {cwd}"))
                .unwrap_or_default(),
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
        _ => (truncate_chars(arguments, 220), String::new()),
    };

    let arguments_detail = parsed
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| arguments.to_string());
    ToolPresentation {
        title: tool_title(name),
        primary,
        metadata,
        arguments_detail,
    }
}

/// Backwards-compatible compact preview used by tests and other callers.
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

fn compact_path(path: &str) -> String {
    let candidate = Path::new(path);
    if let Ok(work_dir) = std::env::current_dir() {
        if let Ok(relative) = candidate.strip_prefix(&work_dir) {
            return relative.display().to_string();
        }
    }
    // Absolute paths outside the active project are shortened without hiding
    // their filename, while relative paths remain untouched.
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

fn result_metadata_for(output: &str, status: ToolStatus, elapsed: Duration) -> String {
    if status == ToolStatus::Running {
        return format!("Running… · {}", byte_size_label(output.len()));
    }
    let outcome = if status == ToolStatus::Error {
        "Failed"
    } else {
        "Done"
    };
    format!(
        "{outcome} · {} · {:.1}s",
        byte_size_label(output.len()),
        elapsed.as_secs_f64()
    )
}

/// Preserve both the beginning and the diagnostically useful tail of output.
pub fn tool_result_preview(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let head_len = max_chars * 3 / 4;
    let tail_len = max_chars - head_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(count - tail_len).collect();
    format!(
        "{head}\n\n… {} characters omitted …\n\n{tail}",
        count - max_chars
    )
}

// ---------------------------------------------------------------------------
// Plan panel (session-scoped plan_mode_ext extension state)
// ---------------------------------------------------------------------------

pub struct PlanItem {
    pub index: u64,
    pub description: String,
    pub completed: bool,
}

pub struct PlanData {
    /// Whether a plan state file was found at all.
    pub available: bool,
    pub enabled: bool,
    pub items: Vec<PlanItem>,
}

pub static PLAN_DATA: RwLock<PlanData> = RwLock::new(PlanData {
    available: false,
    enabled: false,
    items: Vec::new(),
});

/// Re-read the selected session's plan extension state into `PLAN_DATA`.
/// Returns whether that session has plan mode enabled.
pub fn refresh_plan(work_dir: &Path, session_id: &str) -> bool {
    let state_path =
        WasiExtensionManager::session_state_path(work_dir, session_id, "plan_mode_ext");
    let state = std::fs::read_to_string(state_path)
        .ok()
        .and_then(|contents| serde_json::from_str::<serde_json::Value>(&contents).ok());

    let mut data = PLAN_DATA.write().unwrap();
    let Some(state) = state else {
        data.available = false;
        data.enabled = false;
        data.items.clear();
        return false;
    };

    data.available = true;
    data.enabled = state
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    data.items.clear();
    if let Some(items) = state.get("items").and_then(serde_json::Value::as_array) {
        for item in items {
            if let (Some(index), Some(description)) = (
                item.get("index").and_then(serde_json::Value::as_u64),
                item.get("description").and_then(serde_json::Value::as_str),
            ) {
                data.items.push(PlanItem {
                    index,
                    description: description.to_string(),
                    // `completed` is optional — the extension currently omits it.
                    completed: item
                        .get("completed")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                });
            }
        }
    }
    data.enabled
}

// ---------------------------------------------------------------------------
// Sessions sidebar (projects → session rows)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SessionEntry {
    pub id: String,
    pub title: String,
    pub work_dir: PathBuf,
    pub session_file: PathBuf,
    /// Unix seconds of the latest session activity.
    pub updated_at: u64,
}

#[derive(Clone)]
pub struct ProjectGroup {
    pub name: String,
    pub work_dir: PathBuf,
    pub sessions: Vec<SessionEntry>,
}

/// Flattened PortalList rows for the sessions sidebar.
#[derive(Clone, Copy, Debug)]
pub enum SessionListRow {
    ProjectHeader {
        project_idx: usize,
    },
    Session {
        project_idx: usize,
        session_idx: usize,
    },
    EmptyProject,
}

pub struct SessionsData {
    pub projects: Vec<ProjectGroup>,
    pub active_session_id: Option<String>,
    pub active_work_dir: PathBuf,
    /// Cached flat rows matching `projects` (rebuilt on refresh).
    pub rows: Vec<SessionListRow>,
}

pub static SESSIONS_DATA: RwLock<SessionsData> = RwLock::new(SessionsData {
    projects: Vec::new(),
    active_session_id: None,
    active_work_dir: PathBuf::new(),
    rows: Vec::new(),
});

fn project_display_name(work_dir: &Path) -> String {
    work_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| work_dir.display().to_string())
}

fn session_title_from_tree(tree: &SessionTree, fallback_id: &str) -> String {
    if let Some(name) = tree.name.as_ref().filter(|n| !n.trim().is_empty()) {
        return name.clone();
    }
    // Prefer the earliest user message on the active branch as the title.
    for msg in tree.get_active_branch_messages() {
        if let AgentMessage::User { content } = msg {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                continue;
            }
            let cleaned = trimmed
                .strip_prefix("/plan ")
                .or_else(|| trimmed.strip_prefix("/plan"))
                .unwrap_or(trimmed)
                .trim();
            if !cleaned.is_empty() {
                return truncate_chars(cleaned, 42);
            }
        }
    }
    fallback_id.to_string()
}

fn session_updated_at(tree: &SessionTree, path: &Path) -> u64 {
    let from_nodes = tree.nodes.values().map(|n| n.timestamp).max().unwrap_or(0);
    if from_nodes > 0 {
        return from_nodes;
    }
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn discover_sessions_in_project(work_dir: &Path) -> Vec<SessionEntry> {
    let sessions_dir = work_dir.join(".mypi/sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());
        let tree = SessionTree::load_from_file(&path).unwrap_or_else(|_| {
            let mut t = SessionTree::new(id.clone());
            t.file_path = Some(path.clone());
            t
        });
        sessions.push(SessionEntry {
            id: id.clone(),
            title: session_title_from_tree(&tree, &id),
            work_dir: work_dir.to_path_buf(),
            session_file: path.clone(),
            updated_at: session_updated_at(&tree, &path),
        });
    }
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.title.cmp(&b.title)));
    sessions
}

fn rebuild_session_rows(projects: &[ProjectGroup]) -> Vec<SessionListRow> {
    let mut rows = Vec::new();
    for (project_idx, project) in projects.iter().enumerate() {
        rows.push(SessionListRow::ProjectHeader { project_idx });
        if project.sessions.is_empty() {
            rows.push(SessionListRow::EmptyProject);
        } else {
            for session_idx in 0..project.sessions.len() {
                rows.push(SessionListRow::Session {
                    project_idx,
                    session_idx,
                });
            }
        }
    }
    rows
}

/// Optional extra project roots listed in `.mypi/gui/sidebar_projects.json`
/// (JSON array of absolute/relative paths). Current `work_dir` is always first.
fn load_extra_project_dirs(work_dir: &Path) -> Vec<PathBuf> {
    let path = work_dir.join(".mypi/gui/sidebar_projects.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(list) = serde_json::from_str::<Vec<String>>(&text) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for item in list {
        let p = PathBuf::from(item);
        let resolved = if p.is_absolute() { p } else { work_dir.join(p) };
        if resolved != work_dir && resolved.is_dir() {
            dirs.push(resolved);
        }
    }
    dirs
}

/// Rescan session files into `SESSIONS_DATA`. Preserves an active selection
/// when still present, while deliberately preserving no selection at startup.
pub fn refresh_sessions(work_dir: &Path) -> Vec<SessionListRow> {
    let mut projects = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut push_project = |dir: PathBuf| {
        if !seen.insert(dir.clone()) {
            return;
        }
        let sessions = discover_sessions_in_project(&dir);
        projects.push(ProjectGroup {
            name: project_display_name(&dir),
            work_dir: dir,
            sessions,
        });
    };

    push_project(work_dir.to_path_buf());
    for extra in load_extra_project_dirs(work_dir) {
        push_project(extra);
    }

    let rows = rebuild_session_rows(&projects);

    let mut data = SESSIONS_DATA.write().unwrap();
    let prev_id = data.active_session_id.clone();
    let prev_dir = data.active_work_dir.clone();

    let still_active = prev_id.is_none()
        || projects.iter().any(|p| {
            p.work_dir == prev_dir && p.sessions.iter().any(|s| Some(&s.id) == prev_id.as_ref())
        });

    if !still_active {
        if let Some(session) = projects
            .iter()
            .find(|p| p.work_dir == work_dir)
            .and_then(|p| p.sessions.first())
        {
            data.active_session_id = Some(session.id.clone());
            data.active_work_dir = session.work_dir.clone();
        } else {
            data.active_session_id = None;
            data.active_work_dir = work_dir.to_path_buf();
        }
    }

    data.projects = projects;
    data.rows = rows.clone();
    if data.active_work_dir.as_os_str().is_empty() {
        data.active_work_dir = work_dir.to_path_buf();
    }
    rows
}

pub fn set_active_session(work_dir: &Path, session_id: &str) {
    let mut data = SESSIONS_DATA.write().unwrap();
    data.active_work_dir = work_dir.to_path_buf();
    data.active_session_id = Some(session_id.to_string());
}

pub fn active_session_entry() -> Option<SessionEntry> {
    let data = SESSIONS_DATA.read().unwrap();
    let id = data.active_session_id.as_ref()?;
    for project in &data.projects {
        if project.work_dir != data.active_work_dir {
            continue;
        }
        if let Some(session) = project.sessions.iter().find(|s| &s.id == id) {
            return Some(session.clone());
        }
    }
    None
}

pub fn session_entry_at_row(row_idx: usize) -> Option<SessionEntry> {
    let data = SESSIONS_DATA.read().unwrap();
    match data.rows.get(row_idx)? {
        SessionListRow::Session {
            project_idx,
            session_idx,
        } => data
            .projects
            .get(*project_idx)
            .and_then(|p| p.sessions.get(*session_idx))
            .cloned(),
        _ => None,
    }
}

/// Create an empty session jsonl under `work_dir/.mypi/sessions/`.
/// Does not change the active selection — caller should activate it.
pub fn create_new_session(work_dir: &Path) -> Option<SessionEntry> {
    let sessions_dir = work_dir.join(".mypi/sessions");
    std::fs::create_dir_all(&sessions_dir).ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let id = format!("session_{now}");
    let path = sessions_dir.join(format!("{id}.jsonl"));
    // Touch an empty file so discovery picks it up.
    std::fs::File::create(&path).ok()?;
    let entry = SessionEntry {
        id,
        title: "New session".to_string(),
        work_dir: work_dir.to_path_buf(),
        session_file: path,
        updated_at: now,
    };
    refresh_sessions(work_dir);
    Some(entry)
}

pub fn archive_session(entry: &SessionEntry) -> bool {
    let archive_dir = entry.work_dir.join(".mypi/sessions/archive");
    if std::fs::create_dir_all(&archive_dir).is_err() {
        return false;
    }
    let Some(file_name) = entry.session_file.file_name() else {
        return false;
    };
    std::fs::rename(&entry.session_file, archive_dir.join(file_name)).is_ok()
}

pub fn delete_session(entry: &SessionEntry) -> bool {
    std::fs::remove_file(&entry.session_file).is_ok()
}

/// Human relative timestamp matching the sidebar mockup (`1m`, `2h`, `6d`).
pub fn relative_time_label(updated_at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(updated_at);
    let secs = now.saturating_sub(updated_at);
    if secs < 60 {
        return "now".to_string();
    }
    if secs < 3600 {
        return format!("{}m", secs / 60);
    }
    if secs < 86400 {
        return format!("{}h", secs / 3600);
    }
    format!("{}d", secs / 86400)
}

/// Replace the chat transcript with messages from a session branch.
pub fn replace_chat_from_agent_messages(messages: &[AgentMessage]) {
    let mut data = CHAT_DATA.write().unwrap();
    data.messages.clear();
    data.streaming_text.clear();
    data.streaming_kind = None;
    for msg in messages {
        match msg {
            AgentMessage::User { content } => data.messages.push(ChatMessage::Text {
                role: MsgRole::User,
                text: content.clone(),
            }),
            AgentMessage::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(text) = content {
                    if !text.is_empty() {
                        data.messages.push(ChatMessage::Text {
                            role: MsgRole::Assistant,
                            text: text.clone(),
                        });
                    }
                }
                if let Some(tool_calls) = tool_calls {
                    for call in tool_calls {
                        let presentation =
                            tool_presentation(&call.function.name, &call.function.arguments);
                        data.messages.push(ChatMessage::Tool {
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
                if let Some(ChatMessage::Tool {
                    output,
                    status,
                    result_preview,
                    result_metadata,
                    started_at,
                    ..
                }) = data.messages.iter_mut().rev().find(
                    |message| matches!(message, ChatMessage::Tool { id, .. } if id == tool_call_id),
                ) {
                    *output = content.clone();
                    *status = if *is_error {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Done
                    };
                    *result_preview = tool_result_preview(content, 800);
                    *result_metadata = result_metadata_for(content, *status, started_at.elapsed());
                } else {
                    let status = if *is_error {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Done
                    };
                    let presentation = tool_presentation(name, "");
                    data.messages.push(ChatMessage::Tool {
                        id: tool_call_id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                        output: content.clone(),
                        status,
                        presentation,
                        result_preview: tool_result_preview(content, 800),
                        result_metadata: result_metadata_for(content, status, Duration::ZERO),
                        started_at: Instant::now(),
                    });
                }
            }
            AgentMessage::System { .. } | AgentMessage::Custom { .. } => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CommandInfo {
    /// Command name without the leading slash.
    pub name: String,
    pub description: String,
}

/// Built-in slash commands (kept in sync with mypi-agent/src/commands.rs).
pub fn builtin_commands() -> Vec<CommandInfo> {
    [
        ("model", "Switch model, or show the current one"),
        ("compact", "Compact the conversation context"),
        ("session", "Show session info"),
        ("name", "Name this session"),
        ("tree", "Switch session tree branch"),
        ("fork", "Fork a session tree branch"),
        ("clone", "Clone the active session tree"),
        ("clear-plan", "Clear active plan items"),
        ("quit", "Quit mypi agent"),
    ]
    .into_iter()
    .map(|(name, description)| CommandInfo {
        name: name.to_string(),
        description: description.to_string(),
    })
    .collect()
}

/// Truncate long tool output for compact display, on a char boundary.
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_kind_switch_preserves_assistant_reasoning_order() {
        let mut data = ChatData {
            messages: Vec::new(),
            streaming_text: "answer prefix".into(),
            streaming_kind: Some(StreamingKind::Assistant),
        };
        flush_streaming_locked(&mut data);
        data.streaming_text = "reasoning".into();
        data.streaming_kind = Some(StreamingKind::Thinking);
        flush_streaming_locked(&mut data);

        assert!(matches!(
            &data.messages[0],
            ChatMessage::Text { role: MsgRole::Assistant, text } if text == "answer prefix"
        ));
        assert!(matches!(
            &data.messages[1],
            ChatMessage::Thinking { text } if text == "reasoning"
        ));
    }

    #[test]
    fn result_preview_keeps_head_and_tail() {
        let text = "abcdefghij";
        assert_eq!(
            tool_result_preview(text, 6),
            "abcd\n\n… 4 characters omitted …\n\nij"
        );
    }

    #[test]
    fn formats_structured_write_and_edit_metadata() {
        let write = tool_presentation("write_file", r#"{"path":"src/a.rs","content":"one\ntwo"}"#);
        assert_eq!(write.primary, "src/a.rs");
        assert_eq!(write.metadata, "2 lines · 7 B");

        let edit = tool_presentation(
            "edit_file",
            r#"{"path":"src/a.rs","target":"one\ntwo","replacement":"three"}"#,
        );
        assert_eq!(edit.metadata, "−2 +1 lines");
    }

    #[test]
    fn formats_core_tool_previews() {
        assert_eq!(
            tool_preview(
                "read_file",
                r#"{"path":"src/main.rs","start_line":10,"end_line":20}"#,
            ),
            "src/main.rs  (lines 10–20)"
        );
        assert_eq!(
            tool_preview("run_command", r#"{"command":"cargo test","cwd":"."}"#),
            "cargo test"
        );
    }
}
