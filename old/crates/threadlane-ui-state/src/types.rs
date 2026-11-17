use std::path::PathBuf;
use std::sync::Arc;
use threadlane_protocol::{ImageAttachment, SessionPlan};

use crate::AppState;

pub use threadlane_daemon::types::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestedEditorTarget {
    /// Host-validated saved-file navigation, rechecked again at consumption.
    SearchFile {
        project: PathBuf,
        path: String,
        line: usize,
        owner_project: Option<PathBuf>,
        owner_session: Option<String>,
        daemon_identity: usize,
        connection_epoch: u64,
    },
    File {
        project: PathBuf,
        path: String,
        line: Option<usize>,
    },
    Diff {
        project: PathBuf,
        path: String,
        content: String,
    },
}

/// Text (and optional images) another surface asked to append to the
/// composer, e.g. a browser annotation. Applied by the chat view without
/// disturbing already-typed input. `session_id` scopes the insert: `None`
/// applies to whatever composer is on screen, `Some` is held until that
/// session is active so a reply for one session never lands in another's
/// draft. `work_dir` carries the project the session belongs to — drafts
/// key on `(work_dir, session_id)`, so a scoped insert only applies when
/// both match, never to a same-named session in another project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestedComposerInsert {
    pub text: String,
    pub images: Vec<ImageAttachment>,
    pub session_id: Option<String>,
    pub work_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GitHubTab {
    #[default]
    Issues,
    PullRequests,
}

impl GitHubTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Issues => "Issues",
            Self::PullRequests => "Pull requests",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WorkspacePage {
    #[default]
    Chat,
    GitHub,
    Automations,
    Settings,
}

#[derive(Clone)]
pub struct IssueWorkSelection {
    pub selected_model: String,
    pub reasoning_effort: threadlane_protocol::ReasoningEffort,
    pub orchestrator_mode: threadlane_protocol::OrchestratorMode,
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
    pub available_models: Vec<threadlane_daemon::catalog::ModelOption>,
}

impl IssueWorkSelection {
    pub fn capture(state: &AppState) -> Self {
        Self {
            selected_model: state.selected_model.clone(),
            reasoning_effort: state.reasoning_effort,
            orchestrator_mode: state.orchestrator_mode,
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
        state.selected_model = self.selected_model;
        state.reasoning_effort = self.reasoning_effort;
        state.orchestrator_mode = self.orchestrator_mode;
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
