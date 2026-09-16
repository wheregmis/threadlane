mod app_state;
mod discovery;
mod projection;
mod types;

pub(crate) use app_state::AppState;
pub(crate) use discovery::effective_session_work_dir;
pub(crate) use projection::{
    coding_agent_options, compute_full_session_projection, compute_session_messages,
    runtime_status_text,
};
pub(crate) use types::{
    ChatMessageInfo, ChatStreamEvent, MessageRole, RequestedEditorTarget, SessionAttention,
    SessionHealth, SessionHydrationRequest, SessionInfo, SubagentActivityInfo,
    SubagentActivityStatus, ToolActivityInfo, TrajectoryEntry, WorkMode, WorkspacePage,
};

#[cfg(test)]
pub(crate) use app_state::reported_session_shape_state;
#[cfg(test)]
pub(crate) use types::TrajectoryDiagnostics;
