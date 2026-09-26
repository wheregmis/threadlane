//! Durable UI state for the Threadlane desktop app.
//!
//! This crate owns `AppState` (the session/project/composer/projection store),
//! the session projection and discovery helpers, the chat turn services
//! (`execute_prompt`, title generation, ACP config), and the agent-event
//! adapter. It is intentionally GPUI-free: screens and the app shell import
//! its modules directly.

pub mod actions;
pub mod automation;
pub mod agent_events;
mod app_state;
pub mod chat;
pub mod controller;
mod discovery;
pub mod events;
pub mod projection;
pub mod provider_auth;
pub mod settings;
mod types;
pub mod updater;

#[cfg(any(test, feature = "test-support"))]
mod test_support;

pub use app_state::AppState;
pub use events::{next_event_batch, next_event_batch_capped};
pub use types::{
    hash_session_identity, ChatMessageInfo, ChatStreamEvent, MessageRole, RequestedComposerInsert,
    RequestedEditorTarget, SessionAttention, SessionHealth, SessionHydrationRequest, SessionInfo,
    SubagentActivityInfo, SubagentActivityStatus, ToolActivityInfo, TrajectoryEntry, WorkMode,
    GitHubTab, WorkspacePage,
};

#[cfg(any(test, feature = "test-support"))]
pub use test_support::{
    activate_test_session, generated_reported_session_path, reported_session_shape_state,
};
#[cfg(any(test, feature = "test-support"))]
pub use types::TrajectoryDiagnostics;
