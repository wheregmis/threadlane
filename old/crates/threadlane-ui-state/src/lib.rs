//! Durable UI state for the Threadlane desktop app.
//!
//! This crate owns `AppState` (the client-side session/project/composer store)
//! and the app-intent boundary (`actions`/`controller`). The headless service
//! layer it is built from — session discovery/projection, chat turn services,
//! background services, worktree setup, automation, and the model catalog —
//! lives in `threadlane-daemon`; the re-exports below keep the historical
//! `threadlane_ui_state::` paths working for existing consumers, and new code
//! should import `threadlane_daemon` directly.

pub mod actions;
pub mod completion_alerts;
pub mod completion_alert_preferences;
pub mod automation_io;
pub mod controller;
mod app_state;
pub mod project_io;
pub mod session_seen;
pub mod session_snooze;
pub mod types;

#[cfg(any(test, feature = "test-support"))]
mod test_support;

pub use app_state::{close_work_needs_refresh, ActiveCloseWork, AppState, TerminalBus};
pub use session_seen::{SessionSeenStore, SessionSeenWriter};
pub use session_snooze::{
    snooze_return_label, SessionSnooze, SessionSnoozeStore, SnoozeRecord, SNOOZE_OPTIONS,
};
pub use threadlane_daemon::{
    agent_events, automation, chat, discovery, events, projection, provider_auth, runtimes,
    settings, updater, worktree_setup,
};
pub use threadlane_daemon::{
    derive_session_attention, hash_session_identity, next_event_batch, next_event_batch_capped,
    AttachedProject, ChatMessageInfo, SessionEvent, ContextWindowInfo, HydrationRuntimeOptions,
    MessageRole, PendingComposerMessage, ProjectInfo, RunCompletionToken, RunTiming,
    SessionAttention, SessionCompletionSummary, SessionDiscoveryCache, SessionDiscoveryCacheEntry,
    SessionHealth, SessionHydrationRequest, SessionInfo, SessionMetricsInfo, SessionProjectionKey,
    SessionProjectionResult, SubagentActivityInfo, SubagentActivityStatus, ToolActivityInfo,
    TrajectoryEntry, WorkMode,
};
pub use types::{
    GitHubTab, IssueWorkSelection, RequestedComposerInsert, RequestedEditorTarget, WorkspacePage,
};

#[cfg(any(test, feature = "test-support"))]
pub use test_support::{
    activate_test_session, generated_reported_session_path, reported_session_shape_state,
};
#[cfg(any(test, feature = "test-support"))]
pub use threadlane_daemon::TrajectoryDiagnostics;
