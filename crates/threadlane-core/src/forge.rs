//! Shared effects on a repository's forge (Plan.md §9.2, docs/milestone-3.md
//! §1 item 4): push a branch, open a draft pull request.
//!
//! Only the runner calls a [`Forge`], and only to perform a stored call you
//! approved (Plan.md §9.4). The agent never reaches it.

use std::fmt;
use std::path::Path;

use async_trait::async_trait;

/// The tool that pushes the task's branch (tier 3, task lanes only).
pub const PUBLISH_BRANCH: &str = "publish_branch";
/// The tool that opens a draft pull request from the task's branch,
/// publishing the branch in the same call (tier 3, task lanes only).
pub const OPEN_PULL_REQUEST: &str = "open_pull_request";

/// Why a forge operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("forge: {0}")]
pub struct ForgeError(pub String);

/// Pushes branches and opens draft pull requests, from a task's worktree,
/// with the logins of the tools that hold them (invariant 8).
#[async_trait]
pub trait Forge: Send + Sync + fmt::Debug {
    /// Pushes `branch` from the worktree at `worktree` to its remote. Safe to
    /// repeat.
    async fn push(&self, worktree: &Path, branch: &str) -> Result<(), ForgeError>;

    /// Pushes `branch`, then opens a draft pull request from it onto `base`.
    /// Returns the pull request's URL. Not safe to repeat: a second call may
    /// open a second pull request, so after an interruption ask
    /// [`Forge::find_pull_request`] first.
    async fn open_draft_pull_request(
        &self,
        worktree: &Path,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, ForgeError>;

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
