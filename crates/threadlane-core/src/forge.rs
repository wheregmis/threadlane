//! Shared repository effects that the runner performs after hub approval (Plan.md §9.2).

use std::fmt;
use std::path::Path;

use async_trait::async_trait;

/// The tool that pushes the task's branch (tier 2, task lanes only).
pub const PUBLISH_BRANCH: &str = "publish_branch";
/// The tool that opens a draft pull request from the task's branch,
/// publishing the branch in the same call (tier 2, task lanes only).
pub const OPEN_PULL_REQUEST: &str = "open_pull_request";

/// Post a comment on a registered repository's issue or pull request.
pub const COMMENT: &str = "comment";
/// Create an issue in a registered repository.
pub const CREATE_ISSUE: &str = "create_issue";
/// Report a product limitation to the Threadlane upstream during a project dream.
pub const REPORT_LIMITATION: &str = "report_threadlane_limitation";
/// Merge a registered repository's pull request.
pub const MERGE: &str = "merge";

/// A validated shared effect addressed to a registered repository.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RepositoryAction {
    /// Comment on the numbered issue or pull request.
    Comment {
        target: CommentTarget,
        number: std::num::NonZeroU64,
        body: String,
    },
    /// Create an issue with this title and body.
    CreateIssue { title: String, body: String },
    /// Merge the numbered pull request.
    Merge { number: std::num::NonZeroU64 },
}

/// Kind of numbered GitHub item to comment on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentTarget {
    /// A pull request.
    PullRequest,
    /// An issue.
    Issue,
}

/// Why a forge operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("forge: {0}")]
pub struct ForgeError(pub String);

/// Performs approved repository effects with the forge tools' existing logins (invariant 8).
#[async_trait]
pub trait Forge: Send + Sync + fmt::Debug {
    /// Pushes `branch` from the worktree at `worktree` to its remote. Safe to
    /// repeat.
    async fn push(&self, worktree: &Path, branch: &str) -> Result<(), ForgeError>;

    /// Pushes `branch`, then opens a draft pull request from it onto `base`.
    /// Returns the pull request's URL. Not safe to repeat: a second call may
    /// open a second pull request, so after an interruption ask
    /// [`Forge::find_pull_request`] first. `opened_by` names who opens it,
    /// as `<agent name>, <task id>`, for the body's attribution line.
    async fn open_draft_pull_request(
        &self,
        worktree: &Path,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
        opened_by: &str,
    ) -> Result<String, ForgeError>;

    /// Performs an approved repository action against the registered remote.
    async fn apply(&self, remote: &str, action: &RepositoryAction) -> Result<String, ForgeError>;

    /// Whether this machine is logged in to the forge, so a task's
    /// repositories can be reached from it (Plan.md §5.4 item 5).
    async fn logged_in(&self) -> bool;

    /// The URL of a pull request whose head is `branch`, in any state, if
    /// one exists.
    async fn find_pull_request(
        &self,
        worktree: &Path,
        branch: &str,
    ) -> Result<Option<String>, ForgeError>;
}
