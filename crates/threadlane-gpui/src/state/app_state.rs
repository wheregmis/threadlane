use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use threadlane_protocol::git::{GitHubPrInfo, GitStatus};
use threadlane_protocol::harness::SessionDiagnostics;
use threadlane_protocol::permission::{PermissionDecision, PermissionRequest};
use threadlane_protocol::{
    AcpConfigOption, ImageAttachment, ModelRoles, ProjectRecord, ReasoningEffort, SessionEvent,
    SessionPlan, TokenUsage,
};

use crate::adapters::agent_events::{adapt_agent_event, ChatAgentUpdate};

use crate::services::sessions::{SessionRuntime, SessionRuntimeStatus};

pub type AttachedProject = ProjectRecord;

pub use threadlane_protocol::session::{
    ChatMessageInfo, ContextWindowInfo, MessageRole, SessionHealth, SessionInfo,
    SessionMetricsInfo, SubagentActivityInfo, SubagentActivityStatus, ToolActivityInfo,
    TrajectoryDiagnostics, TrajectoryEntry,
};

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
pub struct ProjectInfo {
    pub(crate) name: String,
    pub(crate) work_dir: PathBuf,
    pub(crate) sessions: Vec<SessionInfo>,
    pub(crate) is_expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SessionProjectionKey {
    session_id: String,
    session_file: PathBuf,
}

#[derive(Clone, Debug)]
pub enum ChatStreamEvent {
    Agent {
        session_id: String,
        event: SessionEvent,
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
        options: Vec<AcpConfigOption>,
        error: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestedEditorTarget {
    File(String),
    Diff {
        project: PathBuf,
        path: String,
        content: String,
    },
}

#[derive(Clone, Debug)]
struct PendingComposerMessage {
    text: String,
    images: Vec<ImageAttachment>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspacePage {
    #[default]
    Chat,
    Settings,
}

/// A session whose durable UI projections need to be computed off the UI thread.
#[derive(Clone)]
pub(crate) struct SessionHydrationRequest {
    pub(crate) session_id: String,
    pub(crate) session_file: PathBuf,
    pub(crate) reload_messages: bool,
    /// The first tuple item is the effective worktree directory for agent execution.
    pub(crate) runtime_options: Option<(PathBuf, String, ModelRoles)>,
}

/// The complete durable UI projection built from one JSONL store parse.
pub(crate) struct SessionProjectionResult {
    pub(crate) plan: SessionPlan,
    pub(crate) trajectory: Vec<TrajectoryEntry>,
    pub(crate) subagents: Vec<SubagentActivityInfo>,
    pub(crate) diagnostics: SessionDiagnostics,
    pub(crate) metrics: SessionMetricsInfo,
    pub(crate) token_usage: TokenUsage,
    pub(crate) context_window: Option<ContextWindowInfo>,
}

impl From<threadlane_protocol::session::HydrateSessionResponse> for SessionProjectionResult {
    fn from(res: threadlane_protocol::session::HydrateSessionResponse) -> Self {
        Self {
            plan: res.plan,
            trajectory: res.trajectory,
            subagents: res.subagents,
            diagnostics: res.diagnostics,
            metrics: res.metrics,
            token_usage: res.token_usage,
            context_window: res.context_window,
        }
    }
}

pub struct AppState {
    pub(crate) projects: Vec<ProjectInfo>,
    pub(crate) active_work_dir: Option<PathBuf>,
    pub(crate) active_session_id: Option<String>,
    pub(crate) is_new_task: bool,
    pub(crate) draft_work_mode: WorkMode,
    /// Presentation-only sidebar filter. `None` keeps the flat list scoped to all projects.
    pub(crate) sidebar_project_filter: Option<PathBuf>,
    pub(crate) search_query: String,
    pub(crate) messages: Arc<Vec<ChatMessageInfo>>,
    pub(crate) available_models: Vec<crate::model_catalog::ModelOption>,
    pub(crate) active_plan: SessionPlan,
    pub(crate) is_generating: bool,
    composer_text: String,
    pub(crate) session_status: Option<String>,
    pending_composer_messages: HashMap<String, PendingComposerMessage>,
    session_token_usage: HashMap<SessionProjectionKey, TokenUsage>,
    trajectory_by_session: HashMap<SessionProjectionKey, Vec<TrajectoryEntry>>,
    subagents_by_session: HashMap<SessionProjectionKey, Vec<SubagentActivityInfo>>,
    trajectory_revision: u64,
    trajectory_epoch: u64,
    diagnostics_revision: u64,
    diagnostics_by_session: HashMap<SessionProjectionKey, SessionDiagnostics>,
    session_metrics: HashMap<SessionProjectionKey, SessionMetricsInfo>,
    context_windows: HashMap<SessionProjectionKey, ContextWindowInfo>,
    /// Settings each ACP session's agent exposes, keyed by session id.
    ///
    /// Keyed by session rather than by model id because two sessions on the
    /// same configured agent can hold different settings.
    acp_config_options: HashMap<String, Vec<AcpConfigOption>>,
    stashed_prompts: HashMap<String, String>,
    pub(crate) pending_permissions: HashMap<String, PermissionRequest>,
    pub(crate) pending_hydrations: Vec<SessionHydrationRequest>,
    pub(crate) git_statuses: HashMap<PathBuf, GitStatus>,
    pub(crate) git_prs: HashMap<(PathBuf, String), Option<GitHubPrInfo>>,

    pub(crate) selected_model: String,
    pub(crate) model_roles: ModelRoles,
    pub(crate) reasoning_effort: ReasoningEffort,
    pub(crate) workspace_page: WorkspacePage,
    pub(crate) needle_enabled: bool,
    pub(crate) auth_status_msg: Option<String>,
    pub(crate) update_status: threadlane_protocol::UpdateStatus,
    pub(crate) update_notice_dismissed: bool,
    pub(crate) requested_editor_target: Option<RequestedEditorTarget>,
    pub(crate) requested_composer_prompt: Option<String>,
    pub(crate) requested_terminal_command: Option<String>,
    pub(crate) requested_project_picker: bool,
    stream_tx: tokio::sync::mpsc::UnboundedSender<ChatStreamEvent>,
    pub(crate) stream_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ChatStreamEvent>>,
    session_refresh_tx: Sender<PathBuf>,
    pub(crate) session_refresh_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<(PathBuf, Vec<SessionInfo>)>>,
    pub(crate) session_runtimes: HashMap<PathBuf, Arc<SessionRuntime>>,
    /// Transcript paths the daemon itself returned, keyed by session id. The
    /// client never derives session paths from a presumed on-disk layout.
    daemon_session_files: HashMap<String, PathBuf>,
    deferred_stream_events: HashMap<String, Vec<ChatStreamEvent>>,
}

pub fn discover_sessions_in_project(work_dir: &Path) -> Vec<SessionInfo> {
    let Ok(executor) = crate::services::chat::executor() else {
        return Vec::new();
    };
    let work_dir = work_dir.to_string_lossy().into_owned();
    executor
        .block_on(async move {
            let client = crate::services::daemon_client::get_daemon_client().await?;
            client.list_session_infos(&work_dir).await
        })
        .unwrap_or_default()
}

fn tool_activity_summary(name: &str, arguments: &str) -> String {
    let display_name = name.replace('_', " ");
    let Ok(arguments) = serde_json::from_str::<serde_json::Value>(arguments) else {
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
    .find_map(|key| arguments.get(key).and_then(|value| value.as_str()));

    if let Some(value) = context {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
            let has_more_lines = trimmed.lines().nth(1).is_some();
            let mut summary_ctx = first_line.to_string();
            if has_more_lines && !summary_ctx.ends_with('…') && !summary_ctx.ends_with("...") {
                summary_ctx.push_str(" …");
            }
            return format!("{display_name} {summary_ctx}");
        }
    }
    display_name
}

fn tool_activity_display_summary(summary: &str) -> String {
    let first_line = summary.lines().next().unwrap_or(summary).trim();
    if summary.lines().nth(1).is_some()
        && !first_line.ends_with('…')
        && !first_line.ends_with("...")
    {
        format!("{first_line} …")
    } else {
        first_line.to_string()
    }
}

pub(crate) fn runtime_status_text(status: SessionRuntimeStatus) -> Option<String> {
    match status {
        SessionRuntimeStatus::Ready => None,
        SessionRuntimeStatus::Working => Some("Working…".into()),
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::load()
    }
}

fn normalize_project_relative_path(path: &str) -> Result<String, String> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err("File path must be relative to the project".into());
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::ParentDir if normalized.pop() => {}
            std::path::Component::ParentDir => {
                return Err("File path is outside the project".into());
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err("File path must be relative to the project".into());
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err("File path is empty".into());
    }
    Ok(normalized.to_string_lossy().into_owned())
}

impl AppState {
    pub(crate) fn load() -> Self {
        Self::loading()
    }

    /// Constructs the UI before the daemon snapshot arrives. This deliberately
    /// contains no local project, session, or model discovery.
    pub(crate) fn loading() -> Self {
        let mut state = Self::load_from_registry_with_options(Vec::new(), false);
        state.projects.clear();
        state.active_work_dir = None;
        state.active_session_id = None;
        state.is_new_task = true;
        state.available_models.clear();
        state.selected_model.clear();
        state.session_status = Some("Connecting to daemon…".into());
        state
    }

    fn load_from_registry_with_options(
        registry_projects: Vec<AttachedProject>,
        _allow_current_directory_fallback: bool,
    ) -> Self {
        let mut project_infos = Vec::new();
        let mut active_work_dir = None;
        let mut active_session_id = None;
        let mut active_session_file = None;
        let mut active_runtime_work_dir = None;
        let mut active_project_index = 0;
        for index in 1..registry_projects.len() {
            if registry_projects[index].last_opened_at
                > registry_projects[active_project_index].last_opened_at
            {
                active_project_index = index;
            }
        }

        for (i, p) in registry_projects.iter().enumerate() {
            let path = PathBuf::from(&p.path);
            let sessions = discover_sessions_in_project(&path);
            let is_active = i == active_project_index;

            if is_active {
                active_work_dir = Some(path.clone());
                if let Some(target_session) = p
                    .last_session_id
                    .as_deref()
                    .and_then(|id| sessions.iter().find(|s| s.id == id))
                    .or_else(|| sessions.first())
                {
                    active_session_id = Some(target_session.id.clone());
                    active_session_file = Some(target_session.session_file.clone());
                    active_runtime_work_dir = Some(target_session.runtime_work_dir.clone());
                }
            }

            project_infos.push(ProjectInfo {
                name: p.name.clone(),
                work_dir: path,
                sessions,
                is_expanded: true,
            });
        }
        let (stream_tx, stream_rx) = tokio::sync::mpsc::unbounded_channel();
        let (session_refresh_tx, session_refresh_requests) = mpsc::channel::<PathBuf>();
        let (session_refresh_results_tx, session_refresh_rx) =
            tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            while let Ok(work_dir) = session_refresh_requests.recv() {
                let sessions = discover_sessions_in_project(&work_dir);
                if session_refresh_results_tx
                    .send((work_dir, sessions))
                    .is_err()
                {
                    break;
                }
            }
        });
        for project in &project_infos {
            let _ = session_refresh_tx.send(project.work_dir.clone());
        }
        let selected_model = String::new();

        let model_roles = ModelRoles::default();
        let session_runtimes = HashMap::new();
        let daemon_session_files: HashMap<String, PathBuf> = project_infos
            .iter()
            .flat_map(|project| project.sessions.iter())
            .map(|session| (session.id.clone(), session.session_file.clone()))
            .collect();
        let session_status = active_session_id
            .as_ref()
            .map(|_| "Loading session…".to_string());
        let messages = match (active_work_dir.as_ref(), active_session_file.as_ref()) {
            (Some(_), Some(_)) => Vec::new(),
            _ => Vec::new(),
        };

        let available_models = Vec::new();

        let mut state = Self {
            projects: project_infos,
            active_work_dir,
            is_new_task: active_session_id.is_none(),
            draft_work_mode: WorkMode::Local,
            active_session_id,
            sidebar_project_filter: None,
            search_query: String::new(),
            messages: Arc::new(messages),
            available_models,
            active_plan: SessionPlan::default(),
            is_generating: false,
            composer_text: String::new(),
            session_status,
            pending_composer_messages: HashMap::new(),
            session_token_usage: HashMap::new(),
            trajectory_by_session: HashMap::new(),
            subagents_by_session: HashMap::new(),
            trajectory_revision: 0,
            trajectory_epoch: 0,
            diagnostics_revision: 0,
            diagnostics_by_session: HashMap::new(),
            session_metrics: HashMap::new(),
            context_windows: HashMap::new(),
            acp_config_options: HashMap::new(),
            stashed_prompts: HashMap::new(),
            selected_model,
            model_roles,
            reasoning_effort: ReasoningEffort::default(),
            workspace_page: WorkspacePage::Chat,
            needle_enabled: crate::services::settings::load_needle_enabled(),
            auth_status_msg: None,
            update_status: threadlane_protocol::UpdateStatus::Idle,
            update_notice_dismissed: false,
            requested_editor_target: None,
            requested_composer_prompt: None,
            requested_terminal_command: None,
            requested_project_picker: false,
            stream_tx,
            stream_rx: Some(stream_rx),
            session_refresh_tx,
            session_refresh_rx: Some(session_refresh_rx),
            session_runtimes,
            daemon_session_files,
            deferred_stream_events: HashMap::new(),
            pending_permissions: HashMap::new(),
            pending_hydrations: Vec::new(),
            git_statuses: HashMap::new(),
            git_prs: HashMap::new(),
        };
        if let (Some(session_id), Some(session_file)) = (
            state.active_session_id.clone(),
            active_session_file.as_deref(),
        ) {
            state.pending_hydrations.push(SessionHydrationRequest {
                session_id,
                session_file: session_file.to_path_buf(),
                reload_messages: true,
                runtime_options: active_runtime_work_dir.map(|work_dir| {
                    (
                        work_dir,
                        state.selected_model.clone(),
                        state.model_roles.clone(),
                    )
                }),
            });
        }
        state
    }

    pub(crate) fn messages_mut(&mut self) -> &mut Vec<ChatMessageInfo> {
        Arc::make_mut(&mut self.messages)
    }

    pub(crate) fn available_models(&self) -> &[crate::model_catalog::ModelOption] {
        &self.available_models
    }

    /// Replaces the startup snapshot with the daemon's authoritative project,
    /// session, and model catalog. The daemon owns discovery and persistence;
    /// GPUI only keeps the data needed to render the views.
    pub(crate) fn apply_daemon_snapshot(
        &mut self,
        projects: Vec<threadlane_protocol::ProjectRecord>,
        sessions: Vec<(String, Vec<threadlane_protocol::SessionInfo>)>,
        models: Vec<threadlane_protocol::ModelDescriptor>,
    ) {
        let mut project_infos = Vec::with_capacity(projects.len());
        for project in projects {
            let work_dir = PathBuf::from(&project.path);
            let project_sessions = sessions
                .iter()
                .find(|(path, _)| path == &project.path)
                .map(|(_, infos)| infos.clone())
                .unwrap_or_default();
            project_infos.push(ProjectInfo {
                name: project.name,
                work_dir,
                sessions: project_sessions,
                is_expanded: true,
            });
        }

        for project in &project_infos {
            self.note_daemon_sessions(&project.sessions);
        }
        self.projects = project_infos;
        self.available_models = models
            .into_iter()
            .map(|model| crate::model_catalog::ModelOption {
                id: model.id,
                label: model.name,
                provider: match model.provider.as_str() {
                    "antigravity" => crate::model_catalog::ModelProvider::Antigravity,
                    "opencode" | "opencode-go" => crate::model_catalog::ModelProvider::OpenCode,
                    "acp" => crate::model_catalog::ModelProvider::Acp,
                    _ => crate::model_catalog::ModelProvider::OpenAi,
                },
            })
            .collect();

        if let Some(project) = self.projects.first() {
            self.active_work_dir = Some(project.work_dir.clone());
            self.active_session_id = project.sessions.first().map(|session| session.id.clone());
            self.is_new_task = self.active_session_id.is_none();
        } else {
            self.active_work_dir = None;
            self.active_session_id = None;
            self.is_new_task = true;
        }
        if !self
            .available_models
            .iter()
            .any(|model| model.id == self.selected_model)
        {
            self.selected_model = self
                .available_models
                .first()
                .map(|model| model.id.clone())
                .unwrap_or_default();
        }
    }

    pub(crate) fn set_needle_enabled(&mut self, enabled: bool) -> Result<(), String> {
        crate::services::settings::save_needle_enabled(enabled)?;
        self.needle_enabled = enabled;
        Ok(())
    }

    pub(crate) fn stash_prompt(&mut self, session_id: &str, text: String) {
        if !text.trim().is_empty() {
            self.stashed_prompts.insert(session_id.to_string(), text);
        }
    }

    pub(crate) fn pop_stashed_prompt(&mut self, session_id: &str) -> Option<String> {
        self.stashed_prompts.remove(session_id)
    }

    pub(crate) fn get_stashed_prompt(&self, session_id: &str) -> Option<&String> {
        self.stashed_prompts.get(session_id)
    }

    pub(crate) fn clear_stashed_prompt(&mut self, session_id: &str) {
        self.stashed_prompts.remove(session_id);
    }

    fn invalidate_idle_runtimes(&mut self) {
        self.session_runtimes
            .retain(|_, runtime| runtime.is_generating());
    }

    pub(crate) fn invalidate_capability_runtimes(&mut self) {
        self.invalidate_idle_runtimes();
    }

    pub(crate) fn save_openai_key(&mut self, key: String) -> Result<(), String> {
        let key = key.trim().to_string();
        crate::services::provider_auth::save_api_key(
            threadlane_protocol::ProviderKind::OpenAi,
            &key,
        )?;
        self.auth_status_msg = Some(if key.is_empty() {
            "OpenAI API key removed.".into()
        } else {
            "OpenAI API key saved successfully!".into()
        });
        self.invalidate_idle_runtimes();
        self.reconcile_selected_model();
        Ok(())
    }

    pub(crate) fn save_opencode_key(&mut self, key: String) -> Result<(), String> {
        let key = key.trim().to_string();
        crate::services::provider_auth::save_api_key(
            threadlane_protocol::ProviderKind::OpenCode,
            &key,
        )?;
        self.auth_status_msg = Some(if key.is_empty() {
            "Opencode API key removed.".into()
        } else {
            "Opencode API key saved successfully!".into()
        });
        self.invalidate_idle_runtimes();
        self.reconcile_selected_model();
        Ok(())
    }

    pub(crate) fn reconcile_selected_model(&mut self) {
        if !self
            .available_models
            .iter()
            .any(|model| model.id == self.selected_model)
        {
            self.selected_model = self
                .available_models
                .first()
                .map(|model| model.id.clone())
                .unwrap_or_default();
        }
        self.invalidate_idle_runtimes();
    }

    pub(crate) fn set_selected_model(&mut self, model: String) {
        if !self.available_models.iter().any(|m| m.id == model) {
            return;
        }
        self.selected_model = model.clone();
        if let (Some(work_dir), Some(session_id)) = (
            self.active_work_dir.as_ref(),
            self.active_session_id.as_ref(),
        ) {
            if let Some(session_file) = self.session_file(work_dir, session_id) {
                if self
                    .session_runtimes
                    .get(&session_file)
                    .is_some_and(|runtime| !runtime.is_generating())
                {
                    self.session_runtimes.remove(&session_file);
                }
            }
        }
        self.auth_status_msg = Some(format!("Model switched to {model}"));
        // The runtime above was dropped, taking the old agent connection with
        // it, so anything cached about the previous agent is now stale.
        if let Some(session_id) = self.active_session_id.clone() {
            self.acp_config_options.remove(&session_id);
        }
        self.request_acp_config_options();
    }

    pub(crate) fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.reasoning_effort = effort;
    }

    pub(crate) fn open_settings(&mut self) {
        self.workspace_page = WorkspacePage::Settings;
        self.auth_status_msg = None;
    }

    pub(crate) fn close_settings(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
        self.auth_status_msg = None;
    }

    fn request_session_refresh(&self, work_dir: &Path) {
        let _ = self.session_refresh_tx.send(work_dir.to_path_buf());
    }

    pub(crate) fn apply_session_refresh(
        &mut self,
        work_dir: PathBuf,
        sessions: Vec<SessionInfo>,
    ) -> bool {
        self.note_daemon_sessions(&sessions);
        let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        else {
            return false;
        };
        project.sessions = sessions;
        true
    }

    fn refresh_active_session(&mut self) {
        if let (Some(work_dir), Some(session_id)) = (
            &self.active_work_dir.clone(),
            &self.active_session_id.clone(),
        ) {
            if let Some(session_file) = self.session_file(work_dir, session_id) {
                let is_generating = self
                    .session_runtimes
                    .get(&session_file)
                    .is_some_and(|runtime| runtime.is_generating());
                if !is_generating {
                    self.pending_hydrations.push(SessionHydrationRequest {
                        session_id: session_id.clone(),
                        session_file,
                        reload_messages: true,
                        runtime_options: None,
                    });
                }
            }
            self.request_session_refresh(work_dir);
        }
    }

    pub(crate) fn begin_new_task(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
        if let Some(project_work_dir) = self.active_session_id.as_ref().and_then(|session_id| {
            self.projects.iter().find_map(|project| {
                project
                    .sessions
                    .iter()
                    .any(|session| &session.id == session_id)
                    .then(|| project.work_dir.clone())
            })
        }) {
            self.active_work_dir = Some(project_work_dir);
        }
        self.active_session_id = None;
        self.is_new_task = true;
        self.draft_work_mode = WorkMode::Local;
        self.messages = Arc::new(Vec::new());
        self.active_plan = SessionPlan::default();
        self.is_generating = false;
        self.session_status = None;
        if self.active_work_dir.is_none() {
            self.active_work_dir = self
                .projects
                .first()
                .map(|project| project.work_dir.clone());
        }
    }

    pub(crate) fn set_work_mode(&mut self, mode: WorkMode) {
        self.draft_work_mode = mode;
    }

    pub(crate) fn set_sidebar_project_filter(&mut self, work_dir: Option<PathBuf>) {
        self.sidebar_project_filter = work_dir.filter(|candidate| {
            self.projects
                .iter()
                .any(|project| project.work_dir == *candidate)
        });
    }

    fn persist_project_selection(&self, work_dir: &Path, session_id: Option<&str>) {
        let work_dir_str = work_dir.to_string_lossy().to_string();
        let session_id_opt = session_id.map(String::from);
        if let Ok(executor) = crate::services::chat::executor() {
            executor.spawn(async move {
                if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                    let _ = client
                        .select_project(&work_dir_str, session_id_opt.as_deref())
                        .await;
                }
            });
        }
    }

    pub(crate) fn select_draft_project(&mut self, work_dir: PathBuf) {
        if self
            .projects
            .iter()
            .any(|project| project.work_dir == work_dir)
        {
            self.active_work_dir = Some(work_dir.clone());
            self.active_session_id = None;
            self.is_new_task = true;
            self.draft_work_mode = WorkMode::Local;
            self.messages = Arc::new(Vec::new());
            self.active_plan = SessionPlan::default();
            self.is_generating = false;
            self.session_status = None;
            self.persist_project_selection(&work_dir, None);
            self.request_session_refresh(&work_dir);
        }
    }

    pub(crate) fn request_open_file(&mut self, relative_path: String) {
        if self.active_work_dir.is_none() {
            return;
        }
        let relative = match normalize_project_relative_path(&relative_path) {
            Ok(relative) => relative,
            Err(error) => {
                self.session_status = Some(error);
                return;
            }
        };
        self.requested_editor_target = Some(RequestedEditorTarget::File(relative));
    }

    pub(crate) fn request_open_diff(
        &mut self,
        project: PathBuf,
        relative_path: String,
        content: String,
    ) {
        self.requested_editor_target = Some(RequestedEditorTarget::Diff {
            project,
            path: relative_path,
            content,
        });
    }

    pub(crate) fn request_composer_prompt(&mut self, prompt: String) {
        self.requested_composer_prompt = Some(prompt);
    }

    pub(crate) fn request_run_terminal_command(&mut self, command: String) {
        self.requested_terminal_command = Some(command);
    }

    pub(crate) fn select_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
    ) -> Option<SessionHydrationRequest> {
        self.workspace_page = WorkspacePage::Chat;
        let session = self
            .projects
            .iter()
            .find(|project| project.work_dir == work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
            });
        let runtime_work_dir = session
            .map(|session| session.runtime_work_dir.clone())
            .unwrap_or_else(|| work_dir.clone());
        let Some(session_file) = session
            .map(|session| session.session_file.clone())
            .or_else(|| self.daemon_session_files.get(&session_id).cloned())
        else {
            return None;
        };
        self.active_work_dir = Some(work_dir.clone());
        self.active_session_id = Some(session_id.clone());
        self.is_new_task = false;
        let project_work_dir = self
            .projects
            .iter()
            .find(|project| {
                project
                    .sessions
                    .iter()
                    .any(|session| session.id == session_id && session.work_dir == work_dir)
            })
            .map(|project| project.work_dir.as_path())
            .unwrap_or(&work_dir);
        self.persist_project_selection(project_work_dir, Some(&session_id));
        let completed_events = self
            .deferred_stream_events
            .remove(&session_id)
            .unwrap_or_default();
        for event in completed_events {
            if let ChatStreamEvent::Agent { event, .. } = event {
                self.record_trajectory(&session_id, &event);
            }
        }
        self.messages = Arc::new(Vec::new());
        self.active_plan = SessionPlan::default();
        self.is_generating = false;
        self.session_status = Some("Loading session…".into());
        let request = SessionHydrationRequest {
            session_id,
            session_file,
            reload_messages: true,
            runtime_options: Some((
                runtime_work_dir,
                self.selected_model.clone(),
                self.model_roles.clone(),
            )),
        };
        self.pending_hydrations.push(request.clone());
        Some(request)
    }

    pub(crate) fn settle_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
    ) -> Result<(), String> {
        let session_file = self.session_file(&work_dir, &session_id);
        if session_file.is_some_and(|session_file| {
            self.session_runtimes
                .get(&session_file)
                .is_some_and(|runtime| runtime.is_generating())
        }) {
            return Err("Stop the running generation before archiving this session".into());
        }
        let work_dir_str = work_dir.to_string_lossy().to_string();
        let session_id_clone = session_id.clone();
        if let Ok(executor) = crate::services::chat::executor() {
            executor.spawn(async move {
                if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                    let _ = client
                        .archive_session(threadlane_protocol::session::ArchiveSessionRequest {
                            session_id: session_id_clone,
                            project_path: work_dir_str,
                        })
                        .await;
                }
            });
        }
        self.finish_session_removal(&work_dir, &session_id);
        Ok(())
    }

    pub(crate) fn remove_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
    ) -> Result<(), String> {
        let session_file = self.session_file(&work_dir, &session_id);
        if session_file.is_some_and(|session_file| {
            self.session_runtimes
                .get(&session_file)
                .is_some_and(|runtime| runtime.is_generating())
        }) {
            return Err("Stop the running generation before deleting this session".into());
        }
        let session_id_clone = session_id.clone();
        if let Ok(executor) = crate::services::chat::executor() {
            executor.spawn(async move {
                if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                    let _ = client
                        .delete_session(threadlane_protocol::session::DeleteSessionRequest {
                            session_id: session_id_clone,
                        })
                        .await;
                }
            });
        }
        self.finish_session_removal(&work_dir, &session_id);
        Ok(())
    }

    pub(crate) fn ensure_session_runtime(
        &mut self,
        work_dir: PathBuf,
        session_file: PathBuf,
    ) -> Arc<SessionRuntime> {
        if let Some(runtime) = self.session_runtimes.get(&session_file) {
            return runtime.clone();
        }
        let session_id = session_file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".to_string());
        let runtime = Arc::new(SessionRuntime::new(
            session_id,
            work_dir,
            session_file.clone(),
        ));
        self.session_runtimes.insert(session_file, runtime.clone());
        runtime
    }

    pub(crate) fn resolve_active_permission(
        &mut self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> bool {
        let Some(session_id) = self.active_session_id.clone() else {
            return false;
        };
        self.pending_permissions.remove(&session_id);
        let req_id = request_id.to_string();
        if let Ok(executor) = crate::services::chat::executor() {
            executor.spawn(async move {
                if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                    let _ = client
                        .submit_permission(
                            threadlane_protocol::permission::SubmitPermissionRequest {
                                request_id: req_id,
                                decision,
                            },
                        )
                        .await;
                }
            });
        }
        true
    }

    /// Records transcript paths exactly as the daemon reported them.
    fn note_daemon_sessions(&mut self, sessions: &[SessionInfo]) {
        self.daemon_session_files.extend(
            sessions
                .iter()
                .map(|session| (session.id.clone(), session.session_file.clone())),
        );
    }

    /// Resolves the transcript path for a session from daemon-returned data
    /// only. Returns `None` when the daemon has not yet reported the session;
    /// the client never guesses the daemon's on-disk layout.
    fn session_file(&self, work_dir: &Path, session_id: &str) -> Option<PathBuf> {
        self.projects
            .iter()
            .flat_map(|project| project.sessions.iter())
            .find(|session| {
                session.id == session_id
                    && (session.work_dir == work_dir || session.session_file.starts_with(work_dir))
            })
            .map(|session| session.session_file.clone())
            .or_else(|| self.daemon_session_files.get(session_id).cloned())
    }

    fn session_runtime_work_dir(&self, work_dir: &Path, session_id: &str) -> PathBuf {
        self.projects
            .iter()
            .find(|project| project.work_dir == work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
            })
            .map(|session| session.runtime_work_dir.clone())
            .unwrap_or_else(|| work_dir.to_path_buf())
    }

    fn projection_key(session_id: &str, session_file: &Path) -> SessionProjectionKey {
        SessionProjectionKey {
            session_id: session_id.to_owned(),
            session_file: session_file.to_path_buf(),
        }
    }

    fn session_projection_key(
        &self,
        work_dir: &Path,
        session_id: &str,
    ) -> Option<SessionProjectionKey> {
        Some(Self::projection_key(
            session_id,
            &self.session_file(work_dir, session_id)?,
        ))
    }

    fn active_session_projection_key(&self) -> Option<SessionProjectionKey> {
        let work_dir = self.active_work_dir.as_deref()?;
        let session_id = self.active_session_id.as_deref()?;
        self.session_projection_key(work_dir, session_id)
    }

    pub(crate) fn active_session_matches(&self, session_id: &str, session_file: &Path) -> bool {
        self.active_session_projection_key()
            .is_some_and(|active| active == Self::projection_key(session_id, session_file))
    }

    fn finish_session_removal(&mut self, work_dir: &Path, session_id: &str) {
        if let Some(session_file) = self.session_file(work_dir, session_id) {
            self.session_runtimes.remove(&session_file);
        }
        self.daemon_session_files.remove(session_id);
        self.pending_composer_messages.remove(session_id);
        self.acp_config_options.remove(session_id);
        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        {
            project.sessions = discover_sessions_in_project(work_dir);
        }

        let removed_active = self.active_work_dir.as_deref() == Some(work_dir)
            && self.active_session_id.as_deref() == Some(session_id);
        if !removed_active {
            return;
        }

        self.active_session_id = None;
        self.is_new_task = true;
        self.messages = Arc::new(Vec::new());
        self.active_plan = SessionPlan::default();
        self.is_generating = false;
        self.session_status = None;
        let next_session = self
            .projects
            .iter()
            .flat_map(|project| project.sessions.iter())
            .next()
            .map(|session| (session.work_dir.clone(), session.id.clone()));
        if let Some((next_work_dir, next_session_id)) = next_session {
            let _ = self.select_session(next_work_dir, next_session_id);
        }
    }

    pub(crate) fn session_is_generating(&self, session_file: &Path) -> bool {
        self.session_runtimes
            .get(session_file)
            .is_some_and(|runtime| runtime.is_generating())
    }

    pub(crate) fn toggle_project_expanded(&mut self, work_dir: &Path) {
        if let Some(proj) = self.projects.iter_mut().find(|p| p.work_dir == work_dir) {
            proj.is_expanded = !proj.is_expanded;
        }
    }

    pub(crate) fn toggle_tool_activity(&mut self, tool_call_id: &str) {
        if let Some(activity) = self
            .messages_mut()
            .iter_mut()
            .flat_map(|message| message.tool_activities.iter_mut())
            .find(|activity| activity.id == tool_call_id)
        {
            activity.is_expanded = !activity.is_expanded;
        }
    }

    pub(crate) fn attach_project(&mut self, raw_path: PathBuf) -> Result<(), String> {
        let canonical = raw_path;
        let canonical_str = canonical.to_string_lossy().to_string();
        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());

        if let Ok(executor) = crate::services::chat::executor() {
            let path_for_daemon = canonical_str.clone();
            executor.spawn(async move {
                if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                    let _ = client.register_project(&path_for_daemon).await;
                }
            });
        }

        let discovered_sessions = discover_sessions_in_project(&canonical);
        let session_to_restore = discovered_sessions
            .first()
            .map(|session| session.id.clone());

        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| project.work_dir == canonical)
        {
            project.name = name;
            project.sessions = discovered_sessions;
            project.is_expanded = true;
        } else {
            self.projects.push(ProjectInfo {
                name,
                sessions: discovered_sessions,
                work_dir: canonical.clone(),
                is_expanded: true,
            });
        }

        if let Some(session_id) = session_to_restore {
            self.select_session(canonical, session_id);
        } else {
            self.active_work_dir = Some(canonical);
            self.active_session_id = None;
            self.is_new_task = true;
            self.messages = Arc::new(Vec::new());
            self.active_plan = SessionPlan::default();
            self.is_generating = false;
            self.session_status = None;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn detach_project(&mut self, work_dir: &Path) -> Result<(), String> {
        let canonical = work_dir.to_path_buf();
        let canonical_str = canonical.to_string_lossy().to_string();

        if let Ok(executor) = crate::services::chat::executor() {
            let path_for_daemon = canonical_str.clone();
            executor.spawn(async move {
                if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                    let _ = client.unregister_project(&path_for_daemon).await;
                }
            });
        }

        self.projects.retain(|p| p.work_dir != canonical);
        if self.active_work_dir.as_deref() == Some(&canonical) {
            if let Some(first) = self.projects.first() {
                let first_dir = first.work_dir.clone();
                let first_session = first.sessions.first().map(|s| s.id.clone());
                if let Some(session_id) = first_session {
                    self.select_session(first_dir, session_id);
                } else {
                    self.active_work_dir = Some(first_dir);
                    self.active_session_id = None;
                    self.is_new_task = true;
                    self.messages = Arc::new(Vec::new());
                    self.active_plan = SessionPlan::default();
                }
            } else {
                self.active_work_dir = None;
                self.active_session_id = None;
                self.is_new_task = true;
                self.messages = Arc::new(Vec::new());
                self.active_plan = SessionPlan::default();
            }
        }
        Ok(())
    }

    fn create_new_session(&mut self) -> Result<(), String> {
        let Some(work_dir) = self.active_work_dir.clone() else {
            return Err("No active project directory".into());
        };
        let now_nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let session_id = format!("session_{now_nanos}");
        let model = self.selected_model.clone();
        let project_path = work_dir.to_string_lossy().into_owned();

        // The daemon owns the on-disk transcript layout, so create the session
        // first and record the transcript path it returns. The client never
        // derives session paths itself.
        let summary = if let Ok(executor) = crate::services::chat::executor() {
            executor.block_on(async {
                let client = crate::services::daemon_client::get_daemon_client().await?;
                client
                    .create_session(threadlane_protocol::session::CreateSessionRequest {
                        project_path,
                        session_id: Some(session_id.clone()),
                        model: Some(model),
                        title: None,
                    })
                    .await
            })
        } else {
            Err("No async executor available".into())
        }?;
        let session_file = summary
            .session_file
            .map(PathBuf::from)
            .ok_or_else(|| "Daemon did not return a session transcript path".to_string())?;

        self.activate_new_session(session_id, session_file);
        Ok(())
    }

    fn activate_new_session(&mut self, session_id: String, session_file: PathBuf) {
        self.daemon_session_files
            .insert(session_id.clone(), session_file);
        self.active_session_id = Some(session_id.clone());
        self.is_new_task = false;
        self.messages = Arc::new(Vec::new());
        self.active_plan = SessionPlan::default();
        self.is_generating = false;
        self.session_status = None;
        if let Some(work_dir) = self.active_work_dir.clone() {
            self.persist_project_selection(&work_dir, Some(&session_id));
        }
    }

    /// Applies a completed background projection if its session remains active.
    pub(crate) fn session_status_for_file(&self, session_file: &Path) -> Option<String> {
        self.session_runtimes
            .get(session_file)
            .and_then(|runtime| runtime_status_text(runtime.status()))
    }

    pub(crate) fn apply_session_messages(
        &mut self,
        session_id: &str,
        session_file: &Path,
        messages: Vec<ChatMessageInfo>,
    ) {
        if self.active_session_matches(session_id, session_file) {
            self.messages = Arc::new(messages);
        }
    }

    pub(crate) fn apply_session_hydration(
        &mut self,
        session_id: &str,
        session_file: &Path,
        result: SessionProjectionResult,
    ) {
        if !self.active_session_matches(session_id, session_file) {
            return;
        }
        let key = Self::projection_key(session_id, session_file);
        self.active_plan = result.plan;
        self.trajectory_by_session
            .insert(key.clone(), result.trajectory);
        self.trajectory_epoch = self.trajectory_epoch.wrapping_add(1);
        self.subagents_by_session
            .insert(key.clone(), result.subagents);
        self.trajectory_revision = self.trajectory_revision.wrapping_add(1);
        self.diagnostics_by_session
            .insert(key.clone(), result.diagnostics);
        self.diagnostics_revision = self.diagnostics_revision.wrapping_add(1);
        self.session_metrics.insert(key.clone(), result.metrics);
        if let Some(context_window) = result.context_window {
            self.context_windows.insert(key.clone(), context_window);
        } else {
            self.context_windows.remove(&key);
        }
        self.session_token_usage.insert(key, result.token_usage);
    }

    fn record_subagent_activity(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::SubagentStarted {
                run_id,
                lane,
                agent,
                task,
            } => {
                let Some(subagents) = self.active_subagents_mut() else {
                    return;
                };
                if subagents.iter().any(|subagent| {
                    subagent.batch_run_id == *run_id && subagent.lane.as_ref() == Some(lane)
                }) {
                    return;
                }
                subagents.push(SubagentActivityInfo {
                    batch_run_id: *run_id,
                    task_index: subagents.len(),
                    journal_run_id: Some(lane.clone()),
                    lane: Some(lane.clone()),
                    agent: agent.clone(),
                    task: task.clone(),
                    model: None,
                    status: SubagentActivityStatus::Running,
                    messages: Vec::new(),
                    error: None,
                });
            }
            SessionEvent::SubagentUpdated {
                run_id,
                lane,
                delta,
                tool_name,
            } => {
                let Some(subagents) = self.active_subagents_mut() else {
                    return;
                };
                let Some(subagent) = subagents
                    .iter_mut()
                    .find(|s| s.batch_run_id == *run_id && s.lane.as_ref() == Some(lane))
                else {
                    return;
                };
                if let Some(delta) = delta {
                    if let Some(message) = subagent
                        .messages
                        .last_mut()
                        .filter(|m| m.role == MessageRole::Assistant && m.streaming)
                    {
                        message.content.push_str(delta);
                    } else {
                        subagent.messages.push(ChatMessageInfo {
                            id: format!("subagent-{lane}-{}", subagent.messages.len()),
                            role: MessageRole::Assistant,
                            content: delta.clone(),
                            tool_activities: Vec::new(),
                            streaming: true,
                            reasoning_content: None,
                            reasoning_expanded: false,
                        });
                    }
                }
                if let Some(tool_name) = tool_name {
                    let activity = ToolActivityInfo {
                        id: format!("subagent-tool-{}", subagent.messages.len()),
                        category: "Working".into(),
                        title: tool_name.clone(),
                        display_summary: tool_name.clone(),
                        detail: String::new(),
                        is_expanded: false,
                    };
                    if let Some(message) = subagent.messages.last_mut() {
                        message.tool_activities.push(activity);
                    }
                }
            }
            SessionEvent::SubagentFinished {
                run_id,
                lane,
                succeeded,
                error,
            } => {
                let Some(subagents) = self.active_subagents_mut() else {
                    return;
                };
                if let Some(subagent) = subagents
                    .iter_mut()
                    .find(|s| s.batch_run_id == *run_id && s.lane.as_ref() == Some(lane))
                {
                    subagent.status = if *succeeded {
                        SubagentActivityStatus::Completed
                    } else {
                        SubagentActivityStatus::Failed
                    };
                    subagent.error = error.clone();
                    for message in &mut subagent.messages {
                        message.streaming = false;
                    }
                }
            }
            _ => {}
        }
    }

    fn record_trajectory(&mut self, session_id: &str, event: &SessionEvent) {
        let entry = match event {
            SessionEvent::ToolCallStarted {
                tool_call_id: _,
                name,
                arguments,
            } => Some(("Tool", format!("{name} running"), arguments.clone(), None)),
            SessionEvent::ToolCallFinished {
                tool_call_id: _,
                name,
                result,
            } => Some((
                "Tool",
                format!(
                    "{name} {}",
                    if result.is_error {
                        "failed"
                    } else {
                        "finished"
                    }
                ),
                result.content.clone(),
                None,
            )),
            SessionEvent::SubagentStarted {
                lane, agent, task, ..
            } => Some((
                "Subagent",
                format!("{agent} started"),
                format!("{lane}: {task}"),
                Some(lane.clone()),
            )),
            SessionEvent::SubagentFinished {
                lane,
                succeeded,
                error,
                ..
            } => Some((
                "Subagent",
                format!(
                    "Subagent {}",
                    if *succeeded { "finished" } else { "failed" }
                ),
                error.clone().unwrap_or_else(|| lane.clone()),
                Some(lane.clone()),
            )),
            SessionEvent::Error { message } => {
                Some(("Error", "Agent error".into(), message.clone(), None))
            }
            _ => None,
        };

        if let Some((category, summary, detail, lane)) = entry {
            self.append_trajectory_entry(session_id, category, summary, detail, lane);
        }
    }

    fn append_trajectory_entry(
        &mut self,
        session_id: &str,
        category: &'static str,
        summary: String,
        detail: String,
        lane: Option<String>,
    ) {
        let Some(key) = self
            .active_session_projection_key()
            .filter(|key| key.session_id == session_id)
        else {
            return;
        };
        self.trajectory_by_session
            .entry(key)
            .or_default()
            .push(TrajectoryEntry {
                seq: None,
                run_id: lane.clone(),
                turn: None,
                request: None,
                category: category.into(),
                summary,
                detail,
                lane,
                correlation_id: None,
                diagnostics: TrajectoryDiagnostics::default(),
            });
        self.trajectory_revision = self.trajectory_revision.wrapping_add(1);
    }

    pub(crate) fn active_model_context_diagnostics(&self) -> Vec<TrajectoryEntry> {
        let Some(projection) = self
            .active_session_projection_key()
            .and_then(|key| self.diagnostics_by_session.get(&key))
        else {
            return Vec::new();
        };
        projection
            .model_context
            .iter()
            .map(|entry| {
                let json_text = serde_json::to_string_pretty(&entry.message)
                    .unwrap_or_else(|_| format!("{:?}", entry.message));
                TrajectoryEntry {
                    seq: Some(entry.seq),
                    run_id: None,
                    turn: None,
                    request: None,
                    category: "Model Context".into(),
                    summary: format!("{} · {}", entry.id, entry.message.role_str()),
                    detail: format!(
                        "**Entry ID**: `{}`\n**Role**: `{}`\n**Lane**: `{}`\n\n```json\n{}\n```",
                        entry.id,
                        entry.message.role_str(),
                        entry.lane,
                        json_text
                    ),
                    lane: Some(entry.lane.clone()),
                    correlation_id: Some(entry.id.clone()),
                    diagnostics: TrajectoryDiagnostics {
                        model_visible: true,
                        source: Some("Model context projection".into()),
                        raw: Some(json_text),
                        ..Default::default()
                    },
                }
            })
            .collect()
    }

    pub(crate) fn active_durable_event_diagnostics(&self) -> Vec<TrajectoryEntry> {
        let Some(projection) = self
            .active_session_projection_key()
            .and_then(|key| self.diagnostics_by_session.get(&key))
        else {
            return Vec::new();
        };
        projection
            .durable_events
            .iter()
            .map(|event| {
                let (category, summary, detail) = match &event.kind {
                    threadlane_protocol::harness::DurableEventKind::Entry { role, parent_id } => (
                        "Entry",
                        format!("{} · {role}", event.id),
                        format!("parent={parent_id:?}"),
                    ),
                    threadlane_protocol::harness::DurableEventKind::Record => (
                        "Record",
                        format!("{} · durable record", event.id),
                        format!(
                            "seq={} lane={} run={}",
                            event.seq,
                            event.lane,
                            event.run_id.as_deref().unwrap_or("—")
                        ),
                    ),
                };
                TrajectoryEntry {
                    seq: Some(event.seq),
                    run_id: event.run_id.clone(),
                    turn: event.turn,
                    request: None,
                    category: category.into(),
                    summary,
                    detail: detail.clone(),
                    lane: Some(event.lane.clone()),
                    correlation_id: Some(event.id.clone()),
                    diagnostics: TrajectoryDiagnostics {
                        source: Some("Canonical durable event".into()),
                        raw: Some(detail.clone()),
                        ..Default::default()
                    },
                }
            })
            .collect()
    }

    pub(crate) fn active_recovery_diagnostics(&self) -> Vec<TrajectoryEntry> {
        let Some(projection) = self
            .active_session_projection_key()
            .and_then(|key| self.diagnostics_by_session.get(&key))
        else {
            return Vec::new();
        };
        project_recovery_diagnostics(&projection.recovery)
    }

    pub(crate) fn active_trajectory(&self) -> &[TrajectoryEntry] {
        self.active_session_projection_key()
            .and_then(|key| self.trajectory_by_session.get(&key))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn trajectory_revision(&self) -> u64 {
        self.trajectory_revision
    }

    pub(crate) fn trajectory_epoch(&self) -> u64 {
        self.trajectory_epoch
    }

    pub(crate) fn diagnostics_revision(&self) -> u64 {
        self.diagnostics_revision
    }

    pub(crate) fn session_trajectory(&self, session_id: &str) -> &[TrajectoryEntry] {
        self.active_work_dir
            .as_deref()
            .and_then(|work_dir| self.session_projection_key(work_dir, session_id))
            .and_then(|key| self.trajectory_by_session.get(&key))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn active_subagents(&self) -> &[SubagentActivityInfo] {
        self.active_session_projection_key()
            .and_then(|key| self.subagents_by_session.get(&key))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn active_subagents_mut(&mut self) -> Option<&mut Vec<SubagentActivityInfo>> {
        let key = self.active_session_projection_key()?;
        Some(self.subagents_by_session.entry(key).or_default())
    }

    pub(crate) fn active_session_metrics(&self) -> SessionMetricsInfo {
        self.active_session_projection_key()
            .and_then(|key| self.session_metrics.get(&key))
            .cloned()
            .unwrap_or_default()
    }

    /// Asks the selected external agent what settings it offers.
    ///
    /// A no-op for a provider model, which has no agent to ask.
    pub(crate) fn request_acp_config_options(&mut self) {
        if !threadlane_protocol::is_acp_model(&self.selected_model) {
            return;
        }
        let Some((runtime, session_id)) = self.active_session_runtime() else {
            return;
        };
        // A refusal here is not worth interrupting the user: this is a
        // background question, and the picker simply stays as it was.
        if let Err(error) = crate::services::chat::load_acp_config_options(
            runtime,
            session_id,
            self.stream_tx.clone(),
        ) {
            tracing::debug!("Could not load ACP agent settings: {error}");
        }
    }

    /// Applies one of the selected external agent's settings.
    pub(crate) fn set_acp_config_option(&mut self, config_id: String, value: String) {
        let Some((runtime, session_id)) = self.active_session_runtime() else {
            self.session_status = Some("Open a session before changing agent settings".into());
            return;
        };
        // A refusal here *is* worth surfacing: the user picked something and
        // it did not take effect.
        if let Err(error) = crate::services::chat::set_acp_config_option(
            runtime,
            session_id,
            config_id,
            value,
            self.stream_tx.clone(),
        ) {
            self.session_status = Some(error);
        }
    }

    /// The active session's runtime, creating it if this is its first use.
    ///
    /// Settings are held by the agent inside the runtime, so reaching them
    /// means having one — the same runtime a turn would use, so asking about
    /// settings and then sending a prompt talk to one agent, not two.
    fn active_session_runtime(&mut self) -> Option<(Arc<SessionRuntime>, String)> {
        let work_dir = self.active_work_dir.clone()?;
        let session_id = self.active_session_id.clone()?;
        let session_file = self.session_file(&work_dir, &session_id)?;
        let runtime_work_dir = self.session_runtime_work_dir(&work_dir, &session_id);
        let runtime = self.ensure_session_runtime(runtime_work_dir, session_file);
        Some((runtime, session_id))
    }

    /// Settings the active session's external agent exposes.
    pub(crate) fn active_acp_config_options(&self) -> &[AcpConfigOption] {
        self.active_session_id
            .as_deref()
            .and_then(|session_id| self.acp_config_options.get(session_id))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Short name of the model the active session's agent is running.
    ///
    /// The control-sized form ("Opus", "Sonnet") for a button, as distinct
    /// from [`Self::active_acp_model_label`], which is the fuller phrase the
    /// status bar has room for.
    pub(crate) fn active_acp_model_name(&self) -> Option<String> {
        None
    }

    pub(crate) fn active_acp_model_label(&self) -> Option<String> {
        None
    }

    pub(crate) fn active_context_window(&self) -> Option<&ContextWindowInfo> {
        self.active_session_projection_key()
            .and_then(|key| self.context_windows.get(&key))
    }

    pub(crate) fn drain_chat_stream(&mut self, events: Vec<ChatStreamEvent>) -> bool {
        let active_session_id = self.active_session_id.clone();
        let deferred = active_session_id
            .as_ref()
            .and_then(|session_id| self.deferred_stream_events.remove(session_id))
            .unwrap_or_default()
            .into_iter();
        let mut active_changed = false;

        for event in deferred.chain(events) {
            match event {
                ChatStreamEvent::Agent { session_id, event }
                    if self.active_session_id.as_deref() == Some(&session_id) =>
                {
                    if matches!(&event, SessionEvent::TurnStarted { .. }) {
                        if let Some(message) = self
                            .messages_mut()
                            .last_mut()
                            .filter(|message| message.role == MessageRole::Assistant)
                        {
                            message.streaming = false;
                        }
                    }
                    self.record_trajectory(&session_id, &event);
                    self.record_subagent_activity(&event);
                    let key = self
                        .active_session_projection_key()
                        .expect("active stream event must have a projection key");
                    let metrics = self.session_metrics.entry(key.clone()).or_default();
                    match &event {
                        SessionEvent::TurnStarted { .. } => {
                            metrics.turns = metrics.turns.saturating_add(1)
                        }
                        SessionEvent::ToolCallStarted { .. } => {
                            metrics.tool_calls = metrics.tool_calls.saturating_add(1)
                        }
                        SessionEvent::TurnCompleted { usage, .. } => {
                            metrics.accumulate_usage(usage)
                        }
                        _ => {}
                    }
                    match adapt_agent_event(event) {
                        ChatAgentUpdate::TextDelta(delta) => {
                            active_changed = true;
                            let stream_prefix = format!("streaming-{session_id}-");
                            if let Some(message) =
                                self.messages_mut().last_mut().filter(|message| {
                                    message.role == MessageRole::Assistant
                                        && message.id.starts_with(&stream_prefix)
                                        && message.tool_activities.is_empty()
                                })
                            {
                                message.content.push_str(&delta);
                            } else {
                                let new_len = self.messages.len();
                                self.messages_mut().push(ChatMessageInfo {
                                    id: format!("streaming-{session_id}-{new_len}"),
                                    role: MessageRole::Assistant,
                                    content: delta,
                                    tool_activities: Vec::new(),
                                    streaming: true,
                                    reasoning_content: None,
                                    reasoning_expanded: false,
                                });
                            }
                        }
                        ChatAgentUpdate::ReasoningDelta(delta) => {
                            active_changed = true;
                            if let Some(message) = self
                                .messages_mut()
                                .last_mut()
                                .filter(|m| m.role == MessageRole::Assistant && m.streaming)
                            {
                                match &mut message.reasoning_content {
                                    Some(content) => content.push_str(&delta),
                                    None => message.reasoning_content = Some(delta),
                                }
                            } else {
                                let segment = self.messages.len();
                                self.messages_mut().push(ChatMessageInfo {
                                    id: format!("streaming-{session_id}-{segment}"),
                                    role: MessageRole::Assistant,
                                    content: String::new(),
                                    tool_activities: Vec::new(),
                                    streaming: true,
                                    reasoning_content: Some(delta),
                                    reasoning_expanded: false,
                                });
                            }
                        }
                        ChatAgentUpdate::ToolStarted {
                            tool_call_id,
                            name,
                            arguments,
                        } => {
                            active_changed = true;
                            let summary = tool_activity_summary(&name, &arguments);
                            let display_summary = tool_activity_display_summary(&summary);
                            let activity = ToolActivityInfo {
                                id: tool_call_id,
                                category: "Working".into(),
                                display_summary,
                                title: name,
                                detail: arguments,
                                is_expanded: false,
                            };
                            if let Some(message) =
                                self.messages_mut().last_mut().filter(|message| {
                                    message.role == MessageRole::Assistant
                                        && message.content.is_empty()
                                })
                            {
                                message.tool_activities.push(activity);
                            } else {
                                let new_len = self.messages.len();
                                self.messages_mut().push(ChatMessageInfo {
                                    id: format!("streaming-{session_id}-{new_len}"),
                                    role: MessageRole::Assistant,
                                    content: String::new(),
                                    tool_activities: vec![activity],
                                    streaming: true,
                                    reasoning_content: None,
                                    reasoning_expanded: false,
                                });
                            }
                        }
                        ChatAgentUpdate::ToolUpdated {
                            tool_call_id,
                            partial_result,
                        } => {
                            active_changed = true;
                            if let Some(activity) = self
                                .messages_mut()
                                .iter_mut()
                                .rev()
                                .flat_map(|message| message.tool_activities.iter_mut().rev())
                                .find(|activity| activity.id == tool_call_id)
                            {
                                activity.detail = partial_result;
                            }
                        }
                        ChatAgentUpdate::ToolFinished {
                            tool_call_id,
                            content,
                            is_error,
                        } => {
                            active_changed = true;
                            if let Some(activity) = self
                                .messages_mut()
                                .iter_mut()
                                .rev()
                                .flat_map(|message| message.tool_activities.iter_mut().rev())
                                .find(|activity| activity.id == tool_call_id)
                            {
                                activity.category = if is_error {
                                    "Error".into()
                                } else {
                                    "Completed".into()
                                };
                                activity.detail = content;
                            }
                        }
                        ChatAgentUpdate::PlanUpdated(plan) => {
                            active_changed = true;
                            self.active_plan = plan;
                        }
                        ChatAgentUpdate::AdvisorNote(note) => {
                            active_changed = true;
                            let note_id =
                                format!("advisor-note-{session_id}-{}", self.messages.len());
                            self.messages_mut().push(ChatMessageInfo {
                                id: note_id,
                                role: MessageRole::Advisor(note.severity),
                                content: format!("**{}**\n\n{}", note.summary, note.details),
                                tool_activities: Vec::new(),
                                streaming: false,
                                reasoning_content: None,
                                reasoning_expanded: false,
                            });
                        }
                        ChatAgentUpdate::ModelRolesUpdated(roles) => {
                            active_changed = true;
                            self.model_roles = roles;
                        }
                        ChatAgentUpdate::Usage(usage) => {
                            let entry = self.session_token_usage.entry(key.clone()).or_default();
                            entry.accumulate(&usage);
                        }
                        ChatAgentUpdate::PermissionRequested(request) => {
                            active_changed = true;
                            self.pending_permissions.insert(session_id.clone(), request);
                        }
                        ChatAgentUpdate::Error(error) => {
                            active_changed = true;
                            self.messages_mut().push(ChatMessageInfo {
                                id: format!("stream-error-{session_id}"),
                                role: MessageRole::Error,
                                content: error.clone(),
                                tool_activities: Vec::new(),
                                streaming: false,
                                reasoning_content: None,
                                reasoning_expanded: false,
                            });
                            self.is_generating = false;
                            self.session_status = Some(error);
                        }
                        ChatAgentUpdate::Ignore => {}
                    }
                }
                ChatStreamEvent::Finished {
                    session_id,
                    session_file,
                } => {
                    if self.active_session_id.as_deref() != Some(&session_id) {
                        self.deferred_stream_events
                            .entry(session_id.clone())
                            .or_default()
                            .push(ChatStreamEvent::Finished {
                                session_id,
                                session_file,
                            });
                        continue;
                    }
                    active_changed = true;
                    self.pending_permissions.remove(&session_id);
                    self.is_generating = false;
                    if let Some(subagents) = self.active_subagents_mut() {
                        for subagent in subagents.iter_mut().filter(|subagent| {
                            matches!(
                                subagent.status,
                                SubagentActivityStatus::Queued | SubagentActivityStatus::Running
                            )
                        }) {
                            subagent.status = SubagentActivityStatus::Cancelled;
                            if subagent.error.is_none() {
                                subagent.error =
                                    Some("Parent generation stopped before completion.".into());
                            }
                            for message in &mut subagent.messages {
                                message.streaming = false;
                            }
                        }
                    }
                    self.session_status = Some("Reconciling session…".into());
                    self.pending_hydrations.push(SessionHydrationRequest {
                        session_id: session_id.clone(),
                        session_file: session_file.clone(),
                        reload_messages: true,
                        runtime_options: None,
                    });
                    let runtime_is_stale = self
                        .session_runtimes
                        .get(&session_file)
                        .is_some_and(|runtime| !runtime.is_generating());
                    if runtime_is_stale {
                        self.session_runtimes.remove(&session_file);
                    }
                    if let Some(work_dir) = session_file
                        .parent()
                        .and_then(Path::parent)
                        .and_then(Path::parent)
                    {
                        self.request_session_refresh(work_dir);
                    }
                }
                ChatStreamEvent::AcpConfigOptions {
                    session_id,
                    options,
                    error,
                } => {
                    if let Some(error) = error {
                        self.session_status = Some(error);
                        active_changed = true;
                    }
                    if options.is_empty() {
                        if self.acp_config_options.remove(&session_id).is_some()
                            && self.active_session_id.as_deref() == Some(&session_id)
                        {
                            active_changed = true;
                        }
                    } else if self.acp_config_options.get(&session_id) != Some(&options) {
                        self.acp_config_options.insert(session_id.clone(), options);
                        if self.active_session_id.as_deref() == Some(&session_id) {
                            active_changed = true;
                        }
                    }
                }
                ChatStreamEvent::TitleGenerated {
                    session_id,
                    session_file,
                } => {
                    if let Some(work_dir) = session_file
                        .parent()
                        .and_then(Path::parent)
                        .and_then(Path::parent)
                    {
                        self.request_session_refresh(work_dir);
                    }
                    if self.active_session_id.as_deref() == Some(&session_id) {
                        active_changed = true;
                        self.refresh_active_session();
                    }
                }
                ChatStreamEvent::Agent { session_id, event } => {
                    self.deferred_stream_events
                        .entry(session_id.clone())
                        .or_default()
                        .push(ChatStreamEvent::Agent { session_id, event });
                }
            }
        }
        active_changed
    }

    pub(crate) fn active_pending_composer_message(&self) -> Option<&str> {
        self.active_session_id
            .as_ref()
            .and_then(|session_id| self.pending_composer_messages.get(session_id))
            .map(|message| message.text.as_str())
    }

    pub(crate) fn stage_busy_message(
        &mut self,
        text: String,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        let session_id = self
            .active_session_id
            .clone()
            .ok_or_else(|| "No active session".to_string())?;
        if !self.is_generating {
            return Err("The session is no longer generating".into());
        }
        self.pending_composer_messages
            .insert(session_id, PendingComposerMessage { text, images });
        Ok(())
    }

    pub(crate) fn queue_pending_message(&mut self) -> Result<(), String> {
        let (_runtime, session_id, text, images) = self.pending_runtime_message()?;
        let session_id_clone = session_id.clone();
        let text_clone = text.clone();
        if let Ok(executor) = crate::services::chat::executor() {
            executor.spawn(async move {
                if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                    let _ = client
                        .queue_follow_up(threadlane_protocol::session::QueueFollowUpRequest {
                            session_id: session_id_clone,
                            prompt: text_clone,
                            images,
                        })
                        .await;
                }
            });
        }
        self.pending_composer_messages.remove(&session_id);
        self.push_optimistic_follow_up(&session_id, text, "queued-user");
        self.session_status = Some("Message queued…".into());
        Ok(())
    }

    pub(crate) fn steer_pending_message(&mut self) -> Result<(), String> {
        let (_runtime, session_id, text, images) = self.pending_runtime_message()?;
        let session_id_clone = session_id.clone();
        let text_clone = text.clone();
        if let Ok(executor) = crate::services::chat::executor() {
            executor.spawn(async move {
                if let Ok(client) = crate::services::daemon_client::get_daemon_client().await {
                    let _ = client
                        .queue_steer(threadlane_protocol::session::QueueSteerRequest {
                            session_id: session_id_clone,
                            prompt: text_clone,
                            images,
                        })
                        .await;
                }
            });
        }
        self.pending_composer_messages.remove(&session_id);
        self.push_optimistic_follow_up(&session_id, text, "steered-user");
        self.session_status = Some("Steering current turn…".into());
        Ok(())
    }

    pub(crate) fn dismiss_pending_message(&mut self) {
        if let Some(session_id) = self.active_session_id.as_ref() {
            self.pending_composer_messages.remove(session_id);
        }
    }

    fn pending_runtime_message(
        &self,
    ) -> Result<(Arc<SessionRuntime>, String, String, Vec<ImageAttachment>), String> {
        let session_id = self
            .active_session_id
            .clone()
            .ok_or_else(|| "No active session".to_string())?;
        let work_dir = self
            .active_work_dir
            .as_ref()
            .ok_or_else(|| "No active project".to_string())?;
        let session_file = self
            .session_file(work_dir, &session_id)
            .ok_or_else(|| "Session runtime is unavailable".to_string())?;
        let runtime = self
            .session_runtimes
            .get(&session_file)
            .cloned()
            .ok_or_else(|| "Session runtime is unavailable".to_string())?;
        let pending = self
            .pending_composer_messages
            .get(&session_id)
            .cloned()
            .ok_or_else(|| "No pending composer message".to_string())?;
        Ok((runtime, session_id, pending.text, pending.images))
    }

    fn push_optimistic_follow_up(&mut self, session_id: &str, text: String, prefix: &str) {
        if self.active_session_id.as_deref() == Some(session_id) {
            let new_len = self.messages.len();
            self.messages_mut().push(ChatMessageInfo {
                id: format!("{prefix}-{session_id}-{new_len}"),
                role: MessageRole::User,
                content: text,
                tool_activities: Vec::new(),
                streaming: false,
                reasoning_content: None,
                reasoning_expanded: false,
            });
        }
    }

    pub(crate) fn send_prompt(&mut self, text: String) -> Result<(), String> {
        self.send_prompt_with_images(text, Vec::new())
    }

    pub(crate) fn send_prompt_with_images(
        &mut self,
        text: String,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() && images.is_empty() {
            return Ok(());
        }

        if self.active_session_id.is_none() || self.active_work_dir.is_none() {
            self.create_new_session()?;
        }

        let (work_dir, session_id) =
            match (self.active_work_dir.clone(), self.active_session_id.clone()) {
                (Some(w), Some(s)) => (w, s),
                _ => return Err("Failed to ensure active session".into()),
            };
        let Some(session_file) = self.session_file(&work_dir, &session_id) else {
            return Err("Session transcript path is unavailable".into());
        };
        let runtime_work_dir = self.session_runtime_work_dir(&work_dir, &session_id);
        if self
            .session_runtimes
            .get(&session_file)
            .is_some_and(|runtime| runtime.is_generating())
        {
            return Err("A generation is already running for this session".into());
        }

        let model = self.selected_model.clone();
        let runtime = self.ensure_session_runtime(runtime_work_dir, session_file.clone());
        crate::services::chat::execute_prompt(
            runtime,
            session_id.clone(),
            text.clone(),
            images.clone(),
            self.reasoning_effort,
            self.stream_tx.clone(),
        )?;
        let prompt_detail = if images.is_empty() {
            text.clone()
        } else if text.is_empty() {
            format!("[{} image attachment(s)]", images.len())
        } else {
            format!("{text}\n[{} image attachment(s)]", images.len())
        };
        self.trajectory_by_session
            .entry(Self::projection_key(&session_id, &session_file))
            .or_default()
            .push(TrajectoryEntry {
                seq: None,
                run_id: None,
                turn: None,
                request: None,
                category: "Input".into(),
                summary: "User input".into(),
                detail: prompt_detail.clone(),
                lane: Some("main".into()),
                correlation_id: None,
                diagnostics: TrajectoryDiagnostics::default(),
            });
        self.trajectory_revision = self.trajectory_revision.wrapping_add(1);
        if !model.starts_with("antigravity/") {
            crate::services::chat::maybe_generate_session_title(
                session_file,
                session_id.clone(),
                text.clone(),
                work_dir.clone(),
                self.stream_tx.clone(),
            );
        }

        // Present the accepted prompt immediately. CodingAgent owns durable
        // persistence; writing it directly here would duplicate it.
        let new_len = self.messages.len();
        self.messages_mut().push(ChatMessageInfo {
            id: format!("pending-user-{session_id}-{new_len}"),
            role: MessageRole::User,
            content: prompt_detail,
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        });

        self.is_generating = true;
        self.session_status = Some("Working…".into());

        // Refresh project sessions without blocking the UI thread.
        self.request_session_refresh(&work_dir);
        self.composer_text.clear();
        Ok(())
    }

    pub(crate) fn cancel_generation(&mut self) -> Result<(), String> {
        let (Some(work_dir), Some(session_id)) = (
            self.active_work_dir.as_ref(),
            self.active_session_id.as_ref(),
        ) else {
            return Ok(());
        };
        let Some(session_file) = self.session_file(work_dir, session_id) else {
            return Ok(());
        };
        let Some(runtime) = self.session_runtimes.get(&session_file).cloned() else {
            return Ok(());
        };
        crate::services::chat::cancel_prompt(runtime, session_id.clone(), self.stream_tx.clone())?;
        self.is_generating = false;
        self.session_status = Some("Generation cancelled".into());
        Ok(())
    }
}

fn project_recovery_diagnostics(
    lanes: &[threadlane_protocol::harness::LaneRecoveryDiagnostic],
) -> Vec<TrajectoryEntry> {
    let mut rows = Vec::new();
    for lane in lanes {
        let decision = match lane.decision {
            threadlane_protocol::harness::RecoveryDecision::None => "No recovery required",
            threadlane_protocol::harness::RecoveryDecision::ResumeFromLeaf => {
                "Resume interrupted operation from durable leaf"
            }
            threadlane_protocol::harness::RecoveryDecision::ReplaySafeToolsThenResume => {
                "Replay safe interrupted tools, then resume"
            }
            threadlane_protocol::harness::RecoveryDecision::AbortUnsafeTool => {
                "Abort interrupted run; unsafe tool cannot be replayed"
            }
            threadlane_protocol::harness::RecoveryDecision::WaitForDeferredResult => {
                "Wait for deferred provider result"
            }
            threadlane_protocol::harness::RecoveryDecision::ExplicitRetryRequired => {
                "Keep failed; require explicit retry"
            }
        };
        rows.push(TrajectoryEntry {
            seq: None,
            run_id: lane.open_operation.clone(),
            turn: None,
            request: None,
            category: "Decision".into(),
            summary: format!("{} · {decision}", lane.lane),
            detail: format!(
                "status={:?} attempts={} abort_requested={} leaf={}",
                lane.status,
                lane.attempts,
                lane.abort_requested,
                lane.leaf_id.as_deref().unwrap_or("—")
            ),
            lane: Some(lane.lane.clone()),
            correlation_id: lane.open_operation.clone(),
            diagnostics: TrajectoryDiagnostics::default(),
        });
        for tool in &lane.interrupted_tools {
            rows.push(TrajectoryEntry {
                seq: None,
                run_id: Some(tool.run_id.clone()),
                turn: None,
                request: None,
                category: "Interrupted Tool".into(),
                summary: format!("{} · replay {:?}", tool.name, tool.replay),
                detail: format!(
                    "call={} result_entry={:?}",
                    tool.call_id, tool.result_entry_id
                ),
                lane: Some(lane.lane.clone()),
                correlation_id: Some(tool.call_id.clone()),
                diagnostics: TrajectoryDiagnostics::default(),
            });
        }
        for queued in &lane.queued_work {
            rows.push(TrajectoryEntry {
                seq: None,
                run_id: lane.open_operation.clone(),
                turn: None,
                request: None,
                category: "Queued Work".into(),
                summary: format!("{:?} · {}", queued.queue, queued.entry_id),
                detail: String::new(),
                lane: Some(lane.lane.clone()),
                correlation_id: Some(queued.entry_id.clone()),
                diagnostics: TrajectoryDiagnostics::default(),
            });
        }
    }
    rows
}

#[cfg(test)]
mod optimistic_prompt_tests {
    use super::*;

    #[test]
    fn new_session_does_not_queue_stale_hydration() {
        let mut state = AppState::load_from_registry_with_options(Vec::new(), false);
        state.active_work_dir = Some(std::env::temp_dir());

        // Activation is the post-create state transition; the daemon RPC in
        // create_new_session is exercised only in the running app.
        state.activate_new_session(
            "session_test".into(),
            std::env::temp_dir().join("session_test.jsonl"),
        );

        assert_eq!(state.active_session_id.as_deref(), Some("session_test"));
        assert!(state.pending_hydrations.is_empty());
    }

    #[test]
    fn session_file_resolution_uses_only_daemon_returned_paths() {
        let mut state = AppState::load_from_registry_with_options(Vec::new(), false);
        let work_dir = std::env::temp_dir();
        state.active_work_dir = Some(work_dir.clone());

        // Unknown sessions resolve to None; the client never guesses a path.
        assert!(state.session_file(&work_dir, "session_missing").is_none());

        let daemon_path = work_dir.join("custom").join("layout.jsonl");
        state.activate_new_session("session_test".into(), daemon_path.clone());
        assert_eq!(
            state.session_file(&work_dir, "session_test"),
            Some(daemon_path)
        );
    }
}

#[cfg(test)]
mod file_path_tests {
    use super::normalize_project_relative_path;

    #[test]
    fn normalizes_relative_paths_without_touching_the_client_filesystem() {
        assert_eq!(
            normalize_project_relative_path("src/../main.rs").unwrap(),
            "main.rs"
        );
        assert!(normalize_project_relative_path("../outside.txt").is_err());
        assert!(normalize_project_relative_path("/tmp/outside.txt").is_err());
    }
}
