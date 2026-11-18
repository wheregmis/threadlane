//! A task's working copies (Plan.md §5.5): one worktree per task per
//! repository, on the task's own branch.
//!
//! The runner sets them up and saves the work; the agent only works in them.

use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;

use serde::{Deserialize, Serialize};

use crate::ids::{RepositoryId, TaskId};
use crate::model::Repository;

/// Why setting up or saving a workspace failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("workspace: {0}")]
pub struct WorkspaceError(pub String);

/// Local workspace paths to inspect or remove after deleting a project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRemoval {
    /// Owned task directories.
    pub tasks: Vec<TaskId>,
    /// Project repository IDs and directory names, including shared remotes.
    pub repositories: Vec<(RepositoryId, String)>,
    /// Clones to delete because no surviving project has their remote.
    pub clones: Vec<RepositoryId>,
}

/// Actual directories found under a hub workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceCounts {
    /// Task worktrees.
    pub worktrees: u64,
    /// Complete and partial clones.
    pub clones: u64,
}

/// Where a task branch stands against its repository's default branch
/// (docs/merge-help.md): checked when the task finishes, and when a run
/// starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeCheck {
    /// The repository's name.
    pub repository: String,
    /// Its default branch, which the task branch merges into.
    pub branch: String,
    /// The commit of the default branch it was checked against.
    pub base: String,
    /// The files that conflict; empty when it merges cleanly.
    pub conflicts: Vec<String>,
}

impl MergeCheck {
    /// Whether the task branch merges into the base cleanly.
    #[must_use]
    pub fn clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Sets up and saves the worktrees a task works in.
#[async_trait]
pub trait Workspaces: Send + Sync + fmt::Debug {
    /// Makes sure the task has one worktree per repository, on the branch
    /// [`task_branch`], and returns their paths in the order given. Safe to
    /// repeat, also after a crash at any point: an existing worktree is
    /// reused as it is, and a half-made one is made again.
    async fn prepare(
        &self,
        task: TaskId,
        repositories: &[Repository],
    ) -> Result<Vec<PathBuf>, WorkspaceError>;

    /// Commits whatever is uncommitted in the task's worktrees on the task
    /// branch. A clean worktree gets no commit; one never set up is skipped.
    async fn checkpoint(
        &self,
        task: TaskId,
        repositories: &[Repository],
        message: &str,
    ) -> Result<(), WorkspaceError>;

    /// Fetches each repository's default branch and checks whether the task
    /// branch merges into it cleanly, touching no worktree. A worktree never
    /// set up is skipped.
    async fn check_merge(
        &self,
        task: TaskId,
        repositories: &[Repository],
    ) -> Result<Vec<MergeCheck>, WorkspaceError>;

    /// Fetches each repository's default branch and rebases the task branch
    /// onto it when that rebases cleanly; when it would conflict the branch
    /// is left as it is. Returns where each set-up worktree stands after.
    async fn bring_in_base(
        &self,
        task: TaskId,
        repositories: &[Repository],
    ) -> Result<Vec<MergeCheck>, WorkspaceError>;

    /// Counts the on-disk directories affected by a removal.
    async fn removal_counts(&self, removal: &WorkspaceRemoval) -> WorkspaceCounts;

    /// Removes local worktrees and clones, reporting paths it could not remove.
    async fn remove(&self, removal: &WorkspaceRemoval) -> Vec<String>;
}

/// The branch a task works on: `threadlane/T-0142`.
#[must_use]
pub fn task_branch(task: TaskId) -> String {
    format!("threadlane/{task}")
}

/// Whether `name` may name a repository: it becomes a directory name and a
/// command argument, so letters, digits, `.`, `_` and `-` only, starting
/// with a letter or digit.
#[must_use]
pub fn valid_repository_name(name: &str) -> bool {
    name.len() <= 100
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Whether `name` is a plain branch name that `git` cannot read as an
/// option or a revision expression.
#[must_use]
pub fn valid_branch_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("/.")
        && !name.contains("@{")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_git_could_misread_are_refused() {
        assert!(valid_repository_name("app"));
        assert!(valid_repository_name("my-app.v2_x"));
        for bad in ["", "-x", ".git", "a/b", "..", "a b", "ä"] {
            assert!(!valid_repository_name(bad), "{bad:?}");
        }
        assert!(valid_branch_name("main"));
        assert!(valid_branch_name("release/2.1"));
        for bad in [
            "", "-f", "--force", "/x", "x/", "a..b", "a//b", "x.lock", "a@{1}", "a b", ".x",
            "a/.b", "x.",
        ] {
            assert!(!valid_branch_name(bad), "{bad:?}");
        }
        assert_eq!(task_branch(TaskId(7)), "threadlane/T-0007");
    }
}
