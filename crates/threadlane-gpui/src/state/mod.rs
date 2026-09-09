mod app_state;
mod discovery;
mod projection;
mod types;

pub(crate) use app_state::AppState;
pub(crate) use discovery::discover_sessions_in_project;
pub(crate) use projection::{
    coding_agent_options, compute_full_session_projection, compute_session_messages,
    provider_credentials, runtime_status_text,
};
pub(crate) use types::{
    ChatMessageInfo, ChatStreamEvent, MessageRole, ProjectInfo, RequestedEditorTarget,
    SessionAttention, SessionHealth, SessionHydrationRequest, SessionInfo,
    SubagentActivityInfo, SubagentActivityStatus, ToolActivityInfo, TrajectoryEntry, WorkMode,
    WorkspacePage,
};

#[cfg(test)]
pub(crate) use app_state::reported_session_shape_state;
#[cfg(test)]
pub(crate) use types::TrajectoryDiagnostics;
