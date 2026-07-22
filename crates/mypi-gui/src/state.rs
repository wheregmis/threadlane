//! Shared GUI state: chat transcript, plan panel, activity feed, sessions
//! sidebar, and the event type used to pump agent/background-task results
//! onto the UI thread.
//!
//! The chat/plan/activity/session data lives in `static RwLock`s so the
//! custom list widgets (see `chat.rs`) can read it during their draw pass
//! without fighting `Scope` lifetimes — same pattern as makepad's aichat example.

use mypi_agent::{AgentEvent, AgentMessage, SessionTree, TaskAgentEvent};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Events sent from background tokio tasks to the UI thread.
pub enum GuiAgentEvent {
    TaskEvent(TaskAgentEvent),
    Agent(AgentEvent),
    DeviceCodePrompt { user_code: String, url: String },
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
    Tool,
}

#[derive(Clone)]
pub struct ChatMessage {
    pub role: MsgRole,
    pub text: String,
}

pub struct ChatData {
    pub messages: Vec<ChatMessage>,
    pub streaming_text: String,
    pub is_streaming: bool,
}

pub static CHAT_DATA: RwLock<ChatData> = RwLock::new(ChatData {
    messages: Vec::new(),
    streaming_text: String::new(),
    is_streaming: false,
});

/// Append a finished message; returns its index (stable — messages are only appended).
pub fn push_chat(role: MsgRole, text: impl Into<String>) -> usize {
    let mut data = CHAT_DATA.write().unwrap();
    data.messages.push(ChatMessage {
        role,
        text: text.into(),
    });
    data.messages.len() - 1
}

/// Replace the text of an existing message (used to flip tool lines to done/failed).
pub fn set_chat_text(index: usize, text: impl Into<String>) {
    let mut data = CHAT_DATA.write().unwrap();
    if let Some(msg) = data.messages.get_mut(index) {
        msg.text = text.into();
    }
}

/// Append a streamed text delta to the in-progress assistant message.
pub fn push_stream_delta(delta: &str) {
    let mut data = CHAT_DATA.write().unwrap();
    if !data.is_streaming {
        data.is_streaming = true;
        data.streaming_text.clear();
    }
    data.streaming_text.push_str(delta);
}

/// Finalize the streaming buffer into a proper assistant message (if any).
pub fn flush_streaming() {
    let mut data = CHAT_DATA.write().unwrap();
    let text = std::mem::take(&mut data.streaming_text);
    data.is_streaming = false;
    if !text.is_empty() {
        data.messages.push(ChatMessage {
            role: MsgRole::Assistant,
            text,
        });
    }
}

// ---------------------------------------------------------------------------
// Plan panel (.mypi/state/extensions/plan_mode_ext.json)
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

/// Re-read the plan extension state file into `PLAN_DATA`. Returns `enabled`.
pub fn refresh_plan(work_dir: &Path) -> bool {
    let state_path = work_dir.join(".mypi/state/extensions/plan_mode_ext.json");
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
// Activity feed (tool executions)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum ActivityStatus {
    Info,
    Requested,
    Running,
    Done,
    Error,
}

impl ActivityStatus {
    pub fn glyph(self) -> &'static str {
        match self {
            ActivityStatus::Info => "·",
            ActivityStatus::Requested => "→",
            ActivityStatus::Running => "…",
            ActivityStatus::Done => "✓",
            ActivityStatus::Error => "✗",
        }
    }
}

pub struct ActivityEntry {
    /// tool_call_id when this row tracks a tool execution.
    pub id: Option<String>,
    pub name: String,
    pub status: ActivityStatus,
    pub detail: String,
}

pub static ACTIVITY_DATA: RwLock<Vec<ActivityEntry>> = RwLock::new(Vec::new());

pub fn push_activity(id: Option<String>, name: impl Into<String>, status: ActivityStatus, detail: impl Into<String>) {
    ACTIVITY_DATA.write().unwrap().push(ActivityEntry {
        id,
        name: name.into(),
        status,
        detail: detail.into(),
    });
}

/// Update the most recent activity entry with the given tool_call_id.
pub fn update_activity(id: &str, status: Option<ActivityStatus>, detail: Option<String>) {
    let mut data = ACTIVITY_DATA.write().unwrap();
    if let Some(entry) = data
        .iter_mut()
        .rev()
        .find(|entry| entry.id.as_deref() == Some(id))
    {
        if let Some(status) = status {
            entry.status = status;
        }
        if let Some(detail) = detail {
            entry.detail = detail;
        }
    }
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
    ProjectHeader { project_idx: usize },
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
        let resolved = if p.is_absolute() {
            p
        } else {
            work_dir.join(p)
        };
        if resolved != work_dir && resolved.is_dir() {
            dirs.push(resolved);
        }
    }
    dirs
}

/// Rescan session files into `SESSIONS_DATA`. Preserves the active selection
/// when still present; otherwise selects the newest session in `work_dir`.
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

    let still_active = projects.iter().any(|p| {
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
    data.is_streaming = false;
    for msg in messages {
        match msg {
            AgentMessage::User { content } => {
                data.messages.push(ChatMessage {
                    role: MsgRole::User,
                    text: content.clone(),
                });
            }
            AgentMessage::Assistant { content, .. } => {
                if let Some(text) = content {
                    if !text.is_empty() {
                        data.messages.push(ChatMessage {
                            role: MsgRole::Assistant,
                            text: text.clone(),
                        });
                    }
                }
            }
            AgentMessage::Tool { name, is_error, .. } => {
                let glyph = if *is_error { "✗" } else { "✓" };
                data.messages.push(ChatMessage {
                    role: MsgRole::Tool,
                    text: format!("{glyph} {name}"),
                });
            }
            AgentMessage::System { .. } | AgentMessage::Custom { .. } => {}
        }
    }
}

pub fn clear_activity() {
    ACTIVITY_DATA.write().unwrap().clear();
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
