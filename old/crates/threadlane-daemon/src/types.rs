//! Boundary types shared by the daemon core and its clients.
//!
//! Every type that crosses the service→UI boundary is canonical in
//! `threadlane_protocol::daemon` — serde-clean and ID-referenced so a remote
//! client can speak the same contract. This module re-exports them (keeping
//! `crate::types::*` and `threadlane_ui_state::*` paths stable) and defines
//! only the genuinely process-local types a remote client cannot use:
//! projections carrying engine diagnostics, discovery cache internals, and
//! helpers that inspect live runtime handles.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use threadlane_coding_agent::controller::SessionRuntimeStatus;
use threadlane_protocol::{SessionPlan, TokenUsage};

pub use threadlane_protocol::daemon::{
    ChatMessageInfo, CommandReply, CommandRequest, CommandResponse, ContextWindowInfo,
    GitHubIssueRef, HydrationRuntimeOptions, MessageRole, PendingComposerMessage,
    PermissionDecision, ProjectInfo, RunCompletionToken, RunTiming, SessionAttention,
    SessionCommand, SessionCompletionSummary, SessionEvent, SessionHealth,
    SessionHydrationRequest, SessionInfo, SessionMetricsInfo, SessionProjectionKey,
    SessionSnapshot, SetupStage, SubagentActivityInfo, SubagentActivityStatus, TerminalEvent,
    ToolActivityInfo, TrajectoryDiagnostics, TrajectoryEntry, WorkMode, WorktreeSetup,
};

pub type AttachedProject = threadlane_project::ProjectRecord;

pub fn derive_session_attention(
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

/// Hash of the session-identity fields every session list renders (id, title,
/// health, worktree flags, branch). Shared by the sidebar and GitHub views so
/// the two fingerprints cannot drift on identity; each view hashes its own
/// extras (attention, issue URL, PR state, project scope) on top. Fingerprints
/// are in-memory only, so field order here carries no stability contract.
pub fn hash_session_identity(hasher: &mut impl std::hash::Hasher, session: &SessionInfo) {
    use std::hash::Hash;
    session.id.hash(hasher);
    session.title.hash(hasher);
    session.health.hash(hasher);
    session.worktree_available.hash(hasher);
    session.is_worktree.hash(hasher);
    session.git_branch.hash(hasher);
}

/// The complete durable projection built from one JSONL store parse.
///
/// Process-local: `diagnostics` and `token_efficiency` carry engine-owned
/// types that do not cross the wire; [`SessionSnapshot`] is the wire-clean
/// projection a remote client receives instead.
pub struct SessionProjectionResult {
    pub run_timing: Option<RunTiming>,
    pub plan: SessionPlan,
    pub trajectory: Vec<TrajectoryEntry>,
    pub subagents: Vec<SubagentActivityInfo>,
    /// `None` on snapshots that came over the wire: diagnostics are a
    /// daemon-local panel the `SessionEvent::SessionSnapshot` contract
    /// deliberately does not carry, so `None` means "keep what the client
    /// already computed" rather than "empty".
    pub diagnostics: Option<threadlane_runtime::harness::SessionDiagnostics>,
    pub metrics: SessionMetricsInfo,
    /// `None` for the same reason as `diagnostics`.
    pub token_efficiency: Option<threadlane_runtime::harness::TokenEfficiencyReport>,
    pub token_usage: TokenUsage,
    pub context_window: Option<ContextWindowInfo>,
}

#[derive(Default)]
pub struct SessionDiscoveryCache {
    pub entries: HashMap<PathBuf, SessionDiscoveryCacheEntry>,
}

pub struct SessionDiscoveryCacheEntry {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub info: SessionInfo,
}
