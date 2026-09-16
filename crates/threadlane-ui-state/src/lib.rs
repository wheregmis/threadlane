//! Durable UI state for the Threadlane desktop app.
//!
//! This crate owns `AppState` (the session/project/composer/projection store),
//! the session projection and discovery helpers, the chat turn services
//! (`execute_prompt`, title generation, ACP config), and the agent-event
//! adapter. It is intentionally GPUI-free: screens and the app shell consume
//! it through `threadlane-gpui`'s compatibility shims.

pub mod actions;
pub mod agent_events;
mod app_state;
pub mod chat;
pub mod controller;
mod discovery;
pub mod events;
mod projection;
pub mod provider_auth;
pub mod settings;
mod types;
pub mod updater;
pub mod watcher;

#[cfg(any(test, feature = "test-support"))]
mod test_support;

pub use agent_events::{adapt_agent_event, ChatAgentUpdate};
pub use app_state::AppState;
pub use discovery::effective_session_work_dir;
pub use events::next_event_batch;
pub use projection::{
    coding_agent_options, compute_full_session_projection, compute_session_messages,
    runtime_status_text,
};
pub use types::{
    ChatMessageInfo, ChatStreamEvent, MessageRole, RequestedEditorTarget, SessionAttention,
    SessionHealth, SessionHydrationRequest, SessionInfo, SubagentActivityInfo,
    SubagentActivityStatus, ToolActivityInfo, TrajectoryEntry, WorkMode, WorkspacePage,
};

#[cfg(any(test, feature = "test-support"))]
pub use test_support::{
    activate_test_session, generated_reported_session_path, reported_session_shape_state,
};
#[cfg(any(test, feature = "test-support"))]
pub use types::TrajectoryDiagnostics;
