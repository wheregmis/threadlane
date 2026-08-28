mod app_state;

#[cfg(all(test, feature = "legacy-api-tests"))]
pub(crate) use app_state::reported_session_shape_state;
pub(crate) use app_state::{
    runtime_status_text, SessionHydrationRequest, SessionProjectionResult,
};

pub use app_state::{
    discover_sessions_in_project, load_session_messages, AppState, AttachedProject,
    ChatMessageInfo, ChatStreamEvent, MessageRole, ProjectInfo, RequestedEditorTarget,
    SessionHealth, SessionInfo, SubagentActivityInfo, SubagentActivityStatus, ToolActivityInfo,
    TrajectoryDiagnostics, TrajectoryEntry, WorkMode, WorkspacePage,
};
