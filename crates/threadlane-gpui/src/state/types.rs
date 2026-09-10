use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use threadlane_session::{AcpConfigOption, AgentEvent, ImageAttachment, SessionPlan, TokenUsage};

use super::AppState;
use crate::services::sessions::{SessionRuntime, SessionRuntimeStatus};

pub type AttachedProject = threadlane_session::ProjectRecord;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SessionHealth {
    Healthy,
    Working,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SessionAttention {
    NeedsYou,
    Working,
    Ready,
    Idle,
}

impl SessionAttention {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Ready => "Ready",
            Self::Idle => "Idle",
        }
    }
}

pub(crate) fn derive_session_attention(
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
    pub(crate) id: String,
    pub(crate) title: String,
    /// Canonical attached project that owns this session file.
    pub(crate) work_dir: PathBuf,
    /// Effective directory used for agent execution.
    pub(crate) runtime_work_dir: PathBuf,
    pub(crate) session_file: PathBuf,
    pub(crate) updated_at: u64,
    pub(crate) health: SessionHealth,
    pub(crate) git_branch: Option<String>,
    pub(crate) github_issue: Option<threadlane_git::GitHubIssueRef>,
    pub(crate) is_worktree: bool,
    pub(crate) worktree_available: bool,
}

#[derive(Clone, Debug)]
pub struct ProjectInfo {
    pub(crate) name: String,
    pub(crate) work_dir: PathBuf,
    pub(crate) sessions: Vec<SessionInfo>,
    pub(crate) is_expanded: bool,
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
    pub(crate) id: String,
    pub(crate) category: String,
    pub(crate) title: String,
    pub(crate) display_summary: String,
    pub(crate) detail: String,
    pub(crate) is_expanded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TrajectoryDiagnostics {
    pub(crate) status: Option<String>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) model_visible: bool,
    pub(crate) source: Option<String>,
    pub(crate) raw: Option<String>,
    pub(crate) parent_id: Option<String>,
    pub(crate) result_id: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) output_bytes: Option<u64>,
    pub(crate) files_mutated: Vec<String>,
    pub(crate) commands_executed: Vec<String>,
    pub(crate) error_summary: Option<String>,
    pub(crate) items_count: Option<usize>,
    pub(crate) token_estimate: Option<u32>,
    pub(crate) is_anomaly: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TrajectoryEntry {
    pub(crate) seq: Option<u64>,
    pub(crate) run_id: Option<String>,
    pub(crate) turn: Option<u32>,
    /// The user-facing request this entry belongs to, when it can be inferred
    /// from the canonical transcript. Runtime records inherit the active request.
    pub(crate) request: Option<u32>,
    pub(crate) category: String,
    pub(crate) summary: String,
    pub(crate) detail: String,
    pub(crate) lane: Option<String>,
    pub(crate) correlation_id: Option<String>,
    pub(crate) diagnostics: TrajectoryDiagnostics,
}

#[derive(Clone, Debug, Default)]
pub struct SessionMetricsInfo {
    pub(crate) turns: usize,
    pub(crate) tool_calls: usize,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cache_read_tokens: u64,
    pub(crate) cache_write_tokens: u64,
}

impl SessionMetricsInfo {
    pub(crate) fn billed_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    pub(crate) fn cache_hit_percent(&self) -> Option<u64> {
        let billed_input = self.billed_input_tokens();
        (billed_input > 0).then(|| {
            (((self.cache_read_tokens as u128) * 100 + (billed_input as u128) / 2)
                / billed_input as u128) as u64
        })
    }

    pub(crate) fn accumulate_usage(&mut self, usage: &TokenUsage) {
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
pub(crate) struct ContextWindowInfo {
    pub(crate) current_tokens: u64,
    pub(crate) context_limit: u64,
    pub(crate) context_limit_is_estimate: bool,
    pub(crate) effective_model: String,
    pub(crate) compaction_generation: u64,
    pub(crate) last_compaction_seq: Option<u64>,
    pub(crate) provisional: bool,
    pub(crate) estimating: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SessionProjectionKey {
    pub(crate) session_id: String,
    pub(crate) session_file: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ChatMessageInfo {
    pub(crate) id: String,
    pub(crate) role: MessageRole,
    pub(crate) content: String,
    pub(crate) tool_activities: Vec<ToolActivityInfo>,
    pub(crate) streaming: bool,
    pub(crate) reasoning_content: Option<String>,
    pub(crate) reasoning_expanded: bool,
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
    pub(crate) batch_run_id: u64,
    pub(crate) task_index: usize,
    pub(crate) journal_run_id: Option<String>,
    pub(crate) lane: Option<String>,
    pub(crate) agent: String,
    pub(crate) task: String,
    pub(crate) model: Option<String>,
    pub(crate) status: SubagentActivityStatus,
    pub(crate) messages: Vec<ChatMessageInfo>,
    pub(crate) isolation: Option<threadlane_runtime::SubagentIsolation>,
    pub(crate) error: Option<String>,
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
pub(crate) struct PendingComposerMessage {
    pub(crate) text: String,
    pub(crate) images: Vec<ImageAttachment>,
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
pub(crate) struct SessionHydrationRequest {
    pub(crate) session_id: String,
    pub(crate) session_file: PathBuf,
    pub(crate) reload_messages: bool,
    /// The first tuple item is the effective worktree directory for agent execution.
    pub(crate) runtime_options: Option<(
        PathBuf,
        String,
        threadlane_session::ModelRoles,
        threadlane_session::BrowserBridge,
    )>,
}

/// The complete durable UI projection built from one JSONL store parse.
pub(crate) struct SessionProjectionResult {
    pub(crate) plan: SessionPlan,
    pub(crate) trajectory: Vec<TrajectoryEntry>,
    pub(crate) subagents: Vec<SubagentActivityInfo>,
    pub(crate) diagnostics: threadlane_session::harness::SessionDiagnostics,
    pub(crate) metrics: SessionMetricsInfo,
    pub(crate) token_usage: TokenUsage,
    pub(crate) context_window: Option<ContextWindowInfo>,
}

#[derive(Default)]
pub(crate) struct SessionDiscoveryCache {
    pub(crate) entries: HashMap<PathBuf, SessionDiscoveryCacheEntry>,
}

pub(crate) struct SessionDiscoveryCacheEntry {
    pub(crate) len: u64,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) info: SessionInfo,
}

#[derive(Clone)]
pub(crate) struct IssueWorkSelection {
    pub(crate) active_work_dir: Option<PathBuf>,
    pub(crate) active_session_id: Option<String>,
    pub(crate) is_new_task: bool,
    pub(crate) draft_work_mode: WorkMode,
    pub(crate) workspace_page: WorkspacePage,
    pub(crate) messages: Arc<Vec<ChatMessageInfo>>,
    pub(crate) active_plan: SessionPlan,
    pub(crate) is_generating: bool,
    pub(crate) session_status: Option<String>,
    pub(crate) pending_hydrations: Vec<SessionHydrationRequest>,
    pub(crate) available_models: Vec<crate::model_catalog::ModelOption>,
}

impl IssueWorkSelection {
    pub(crate) fn capture(state: &AppState) -> Self {
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

    pub(crate) fn restore(self, state: &mut AppState) {
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
