//! Shared GUI state: chat transcript, plan panel, activity feed, and the
//! event type used to pump agent/background-task results onto the UI thread.
//!
//! The chat/plan/activity data lives in `static RwLock`s so the custom list
//! widgets (see `chat.rs`) can read it during their draw pass without
//! fighting `Scope` lifetimes — same pattern as makepad's aichat example.

use mypi_agent::AgentEvent;
use std::path::Path;
use std::sync::RwLock;

/// Events sent from background tokio tasks to the UI thread.
pub enum GuiAgentEvent {
    Agent(AgentEvent),
    DeviceCodePrompt { user_code: String, url: String },
    DeviceLoginSuccess,
    AvailableModelsLoaded(Vec<String>),
    CommandOutput(String),
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
