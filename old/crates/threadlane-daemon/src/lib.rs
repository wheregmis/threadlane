//! Headless Threadlane services (the in-process daemon core).
//!
//! This crate owns the non-UI half of the old `threadlane-ui-state` surface:
//! session discovery and store projections, the chat turn services (prompt
//! execution, title generation, ACP config), the agent-event adapter, the
//! background services (`provider_auth`, `settings`, `updater`), worktree
//! setup, the automation application service, and the provider/model catalog
//! (`catalog`, formerly `threadlane-ui-catalog`). It is intentionally
//! GPUI-free so any client — today `threadlane-ui-state`/`AppState`, later a
//! real `threadlane-daemon` process speaking `threadlane-protocol::daemon` —
//! can embed it.

pub mod agent_events;
pub mod automation;
pub mod catalog;
pub mod chat;
pub mod core;
pub mod discovery;
pub mod events;
pub mod pairing;
pub mod project_io;
pub mod conversation_search;
pub mod file_search;
pub mod projection;
pub mod provider_auth;
pub mod runtimes;
pub mod server;
pub mod settings;
pub mod terminal;
pub mod types;
pub mod updater;
pub mod worktree_setup;

pub use events::{next_event_batch, next_event_batch_capped};
pub use terminal::TerminalManager;
pub use types::{
    derive_session_attention, hash_session_identity, AttachedProject, ChatMessageInfo,
    CommandReply, CommandRequest, CommandResponse, ContextWindowInfo, HydrationRuntimeOptions,
    MessageRole, PendingComposerMessage, ProjectInfo, RunCompletionToken, RunTiming,
    SessionAttention, SessionCommand, SessionCompletionSummary, SessionDiscoveryCache,
    SessionDiscoveryCacheEntry, SessionEvent, SessionHealth, SessionHydrationRequest,
    SessionInfo, SessionMetricsInfo, SessionProjectionKey, SessionProjectionResult,
    SessionSnapshot, SubagentActivityInfo, SubagentActivityStatus, ToolActivityInfo,
    TrajectoryDiagnostics, TrajectoryEntry, WorkMode,
};
