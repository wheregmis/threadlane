//! Workspaces, channels, and sources (Plan.md §3.4, §5.5, §6.3, §10.2).

use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::ids::{RepositoryId, TaskId};
use crate::model::{EventKind, OutboxMessage, Trust};
use crate::time::Timestamp;

/// One repository a task touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRepository {
    pub repository_id: RepositoryId,
    pub name: String,
    pub remote_url: String,
    pub base_branch: String,
}

/// What a task's workspace must contain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSpec {
    pub task_id: TaskId,
    pub repositories: Vec<WorkspaceRepository>,
}

/// A prepared workspace: one worktree per repository, on the task branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedWorkspace {
    pub root: PathBuf,
    pub worktrees: Vec<(RepositoryId, PathBuf)>,
}

/// A saved checkpoint for one repository (Plan.md §5.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointBundle {
    pub repository_id: RepositoryId,
    pub base_commit: String,
    pub branch: String,
    pub bundle_path: PathBuf,
    pub sha256: String,
}

/// Why a workspace operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("workspace error: {0}")]
pub struct WorkspaceError(pub String);

/// Prepares, checkpoints, and restores a task's worktrees.
#[async_trait]
pub trait Workspace: Send + Sync + fmt::Debug {
    async fn prepare(&self, spec: &WorkspaceSpec) -> Result<PreparedWorkspace, WorkspaceError>;
    async fn checkpoint(
        &self,
        workspace: &PreparedWorkspace,
    ) -> Result<Vec<CheckpointBundle>, WorkspaceError>;
    async fn restore(
        &self,
        spec: &WorkspaceSpec,
        bundles: &[CheckpointBundle],
    ) -> Result<PreparedWorkspace, WorkspaceError>;
}

/// A successful delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    /// The channel's id for the message, used to route replies.
    pub channel_message_id: Option<String>,
}

/// Why a delivery failed (Plan.md §10.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryError {
    /// Retry with backoff.
    #[error("transient delivery failure: {0}")]
    Transient(String),
    /// Give up and tell the originating lane.
    #[error("permanent delivery failure: {0}")]
    Permanent(String),
}

/// Delivers a message on one channel.
#[async_trait]
pub trait ChannelAdapter: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;
    /// Delivers using the message's stable delivery key for idempotency.
    async fn deliver(&self, message: &OutboxMessage) -> Result<Delivered, DeliveryError>;
}

/// An event as a source adapter normalized it, before routing (Plan.md §6.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceEvent {
    pub source: String,
    pub source_event_id: String,
    pub kind: EventKind,
    pub payload: Json,
    pub trust: Trust,
    /// The repository's remote URL, if the event is about one.
    pub repository_remote: Option<String>,
    /// A task branch or pull request the event is about, if any.
    pub task_ref: Option<String>,
    pub received_at: Timestamp,
}

/// One batch pulled from a source, with the cursor to resume from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SourceBatch {
    pub events: Vec<SourceEvent>,
    pub cursor: Option<String>,
}

/// Why a source could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("source error: {0}")]
pub struct SourceError(pub String);

/// Verifies and normalizes one inbound event source. Never calls a model.
#[async_trait]
pub trait SourceAdapter: Send + Sync + fmt::Debug {
    fn source(&self) -> &str;
    /// Pulls what happened since `cursor` (Plan.md §6.6).
    async fn pull(&self, cursor: Option<&str>) -> Result<SourceBatch, SourceError>;
}
