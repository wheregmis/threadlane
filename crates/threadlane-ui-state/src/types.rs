use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use threadlane_session::{AcpConfigOption, AgentEvent, ImageAttachment, SessionPlan, TokenUsage};

use crate::AppState;
use threadlane_session::{SessionRuntime, SessionRuntimeStatus};

pub type AttachedProject = threadlane_project::ProjectRecord;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SessionHealth {
    Healthy,
    Working,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionAttention {
    NeedsYou,
    Working,
    Ready,
    Idle,
}

impl SessionAttention {
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Ready => "Ready",
            Self::Idle => "Idle",
        }
    }
}

pub fn derive_session_attention(
    has_blocking_request: bool,
    health: &SessionHealth,
    runtime_status: Option<&SessionRuntimeStatus>,
    is_generating: bool,
    has_ready_work: bool,
) -> SessionAttention {
    if has_blocking_request
        || *health == SessionHealth::Warning
        || matches!(
            runtime_status,
            Some(SessionRuntimeStatus::Interrupted | SessionRuntimeStatus::Error(_))
        )
    {
        SessionAttention::NeedsYou
    } else if is_generating
        || *health == SessionHealth::Working
        || matches!(runtime_status, Some(SessionRuntimeStatus::Working))
    {
        SessionAttention::Working
    } else if has_ready_work {
        SessionAttention::Ready
    } else {
        SessionAttention::Idle
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum WorkMode {
    #[default]
    Local,
    Worktree,
}

impl WorkMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Worktree => "Worktree",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    /// Canonical attached project that owns this session file.
    pub work_dir: PathBuf,
    /// Effective directory used for agent execution.
    pub runtime_work_dir: PathBuf,
    pub session_file: PathBuf,
    pub updated_at: u64,
    pub health: SessionHealth,
    pub git_branch: Option<String>,
    pub github_issue: Option<threadlane_git::GitHubIssueRef>,
    pub is_worktree: bool,
    pub worktree_available: bool,
}

#[derive(Clone, Debug)]
pub struct ProjectInfo {
    pub name: String,
    pub work_dir: PathBuf,
    pub sessions: Vec<SessionInfo>,
    pub is_expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Error,
    ContextMarker,
}

#[derive(Clone, Debug)]
pub struct ToolActivityInfo {
    pub id: String,
    pub category: String,
    pub title: String,
    pub display_summary: String,
    pub detail: String,
    pub is_expanded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TrajectoryDiagnostics {
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
    pub model_visible: bool,
    pub source: Option<String>,
    pub raw: Option<String>,
    pub parent_id: Option<String>,
    pub result_id: Option<String>,
    pub exit_code: Option<i32>,
    pub output_bytes: Option<u64>,
    pub files_mutated: Vec<String>,
    pub commands_executed: Vec<String>,
    pub error_summary: Option<String>,
    pub items_count: Option<usize>,
    pub token_estimate: Option<u32>,
    pub is_anomaly: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TrajectoryEntry {
    pub seq: Option<u64>,
    pub run_id: Option<String>,
    pub turn: Option<u32>,
    /// The user-facing request this entry belongs to, when it can be inferred
    /// from the canonical transcript. Runtime records inherit the active request.
    pub request: Option<u32>,
    pub category: String,
    pub summary: String,
    pub detail: String,
    pub lane: Option<String>,
    pub correlation_id: Option<String>,
    pub diagnostics: TrajectoryDiagnostics,
}

#[derive(Clone, Debug, Default)]
pub struct SessionMetricsInfo {
    pub turns: usize,
    pub tool_calls: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl SessionMetricsInfo {
    pub fn billed_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    pub fn cache_hit_percent(&self) -> Option<u64> {
        let billed_input = self.billed_input_tokens();
        (billed_input > 0).then(|| {
            (((self.cache_read_tokens as u128) * 100 + (billed_input as u128) / 2)
                / billed_input as u128) as u64
        })
    }

    pub fn accumulate_usage(&mut self, usage: &TokenUsage) {
        self.input_tokens = self
            .input_tokens
            .saturating_add(u64::from(usage.input_tokens));
        self.output_tokens = self
            .output_tokens
            .saturating_add(u64::from(usage.output_tokens));
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(u64::from(usage.cache_read_tokens));
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(u64::from(usage.cache_write_tokens));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextWindowInfo {
    pub current_tokens: u64,
    pub context_limit: u64,
    pub context_limit_is_estimate: bool,
    pub effective_model: String,
    pub compaction_generation: u64,
    pub last_compaction_seq: Option<u64>,
    pub provisional: bool,
    pub estimating: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionProjectionKey {
    pub session_id: String,
    pub session_file: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ChatMessageInfo {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub tool_activities: Vec<ToolActivityInfo>,
    pub streaming: bool,
    pub reasoning_content: Option<String>,
    pub reasoning_expanded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentActivityStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct SubagentActivityInfo {
    pub batch_run_id: u64,
    pub task_index: usize,
    pub journal_run_id: Option<String>,
    pub lane: Option<String>,
    pub agent: String,
    pub task: String,
    pub model: Option<String>,
    pub status: SubagentActivityStatus,
    pub messages: Vec<ChatMessageInfo>,
    pub isolation: Option<threadlane_runtime::SubagentIsolation>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum ChatStreamEvent {
    Agent {
        session_id: String,
        event: AgentEvent,
    },
    Finished {
        session_id: String,
        session_file: PathBuf,
    },
    TitleGenerated {
        session_id: String,
        session_file: PathBuf,
    },
    /// Settings an external ACP agent exposes, as it reports them.
    ///
    /// Unlike a provider model these are not known from the selection alone —
    /// the agent defines them and names its own current values — so they
    /// arrive once it has connected.
    AcpConfigOptions {
        session_id: String,
        source: std::sync::Weak<SessionRuntime>,
        options: Vec<AcpConfigOption>,
        error: Option<String>,
        /// Restores a New-task picker selection when applying it failed.
        failed_config: Option<(String, String)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestedEditorTarget {
    File {
        project: PathBuf,
        path: String,
    },
    Diff {
        project: PathBuf,
        path: String,
        content: String,
    },
}

#[derive(Clone, Debug)]
pub struct PendingComposerMessage {
    pub text: String,
    pub images: Vec<ImageAttachment>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WorkspacePage {
    #[default]
    Chat,
    GitHub,
    Settings,
}

/// A session whose durable UI projections need to be computed off the UI thread.
#[derive(Clone)]
pub struct SessionHydrationRequest {
    pub session_id: String,
    pub session_file: PathBuf,
    pub reload_messages: bool,
    /// The first tuple item is the effective worktree directory for agent execution.
    pub runtime_options: Option<(
        PathBuf,
        String,
        threadlane_session::ModelRoles,
        threadlane_protocol::browser::BrowserBridge,
    )>,
}

/// The complete durable UI projection built from one JSONL store parse.
pub struct SessionProjectionResult {
    pub plan: SessionPlan,
    pub trajectory: Vec<TrajectoryEntry>,
    pub subagents: Vec<SubagentActivityInfo>,
    pub diagnostics: threadlane_session::harness::SessionDiagnostics,
    pub metrics: SessionMetricsInfo,
    pub token_usage: TokenUsage,
    pub context_window: Option<ContextWindowInfo>,
}

#[derive(Default)]
pub struct SessionDiscoveryCache {
    pub entries: HashMap<PathBuf, SessionDiscoveryCacheEntry>,
}

pub struct SessionDiscoveryCacheEntry {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub info: SessionInfo,
}

#[derive(Clone)]
pub struct IssueWorkSelection {
    pub active_work_dir: Option<PathBuf>,
    pub active_session_id: Option<String>,
    pub is_new_task: bool,
    pub draft_work_mode: WorkMode,
    pub workspace_page: WorkspacePage,
    pub messages: Arc<Vec<ChatMessageInfo>>,
    pub active_plan: SessionPlan,
    pub is_generating: bool,
    pub session_status: Option<String>,
    pub pending_hydrations: Vec<SessionHydrationRequest>,
    pub available_models: Vec<threadlane_ui_catalog::ModelOption>,
}

impl IssueWorkSelection {
    pub fn capture(state: &AppState) -> Self {
        Self {
            active_work_dir: state.active_work_dir.clone(),
            active_session_id: state.active_session_id.clone(),
            is_new_task: state.is_new_task,
            draft_work_mode: state.draft_work_mode,
            workspace_page: state.workspace_page,
            messages: state.messages.clone(),
            active_plan: state.active_plan.clone(),
            is_generating: state.is_generating,
            session_status: state.session_status.clone(),
            pending_hydrations: state.pending_hydrations.clone(),
            available_models: state.available_models.clone(),
        }
    }

    pub fn restore(self, state: &mut AppState) {
        state.active_work_dir = self.active_work_dir;
        state.active_session_id = self.active_session_id;
        state.is_new_task = self.is_new_task;
        state.draft_work_mode = self.draft_work_mode;
        state.workspace_page = self.workspace_page;
        state.messages = self.messages;
        state.active_plan = self.active_plan;
        state.is_generating = self.is_generating;
        state.session_status = self.session_status;
        state.pending_hydrations = self.pending_hydrations;
        state.available_models = self.available_models;
    }
}
