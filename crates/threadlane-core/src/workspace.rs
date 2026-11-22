//! A task's working copies (Plan.md §5.5): one worktree per task per
//! repository, on the task's own branch.
//!
//! The runner sets them up and saves the work; the agent only works in them.

use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;

use serde::{Deserialize, Serialize};

use crate::ids::{RepositoryId, RunId, TaskId};
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

/// One file's line counts in a task branch diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFileStat {
    /// Repository-relative path of the changed file.
    pub path: String,
    /// Added lines.
    pub insertions: u64,
    /// Deleted lines.
    pub deletions: u64,
    /// Whether git reported the file as binary.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
}

/// A bounded file-stat summary and unified diff preview of a task branch
/// against its repository's default branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffPreview {
    /// The repository's name.
    pub repository: String,
    /// The base branch the diff was computed against.
    pub base_branch: String,
    /// Per-file insertion and deletion counts.
    #[serde(default)]
    pub files: Vec<DiffFileStat>,
    /// Total added lines across all files.
    pub insertions: u64,
    /// Total deleted lines across all files.
    pub deletions: u64,
    /// Bounded unified diff lines (`git diff --unified=3 --no-color`).
    #[serde(default)]
    pub patch: String,
    /// Whether `patch` was clipped to stay within the preview budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// The commit to publish and its diff, which is absent when there are no changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishPreview {
    /// The full commit ID of the task branch.
    pub revision: String,
    /// The bounded diff against the repository's default branch.
    pub diff: Option<DiffPreview>,
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
    /// The full commit from the task branch that was checked. Older results omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The files that conflict; empty when it merges cleanly.
    pub conflicts: Vec<String>,
    /// Optional bounded diff preview against the base branch. Older results omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffPreview>,
}

/// Identifies a checkout reused when a review resumes on the same machine.
#[derive(Debug, Clone, Copy)]
pub enum ReviewCheckout {
    Submission(RunId),
    PullRequest(u64),
}

/// An implementation commit for a review, keyed by the project's repository ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRevision {
    pub repository: RepositoryId,
    pub commit: String,
}

/// Whether an ID names a complete Git SHA-1 or SHA-256 object.
#[must_use]
pub fn valid_commit_id(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64)
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
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

    /// Checks out the submitted commits in separate worktrees for the task's review round.
    async fn prepare_review(
        &self,
        task: TaskId,
        checkout: ReviewCheckout,
        repositories: &[Repository],
        revisions: &[ReviewRevision],
    ) -> Result<Vec<PathBuf>, WorkspaceError>;

    /// Confirms that the Engineer ended the run on the commits submitted for review.
    async fn verify_submission(
        &self,
        task: TaskId,
        repositories: &[Repository],
        revisions: &[MergeCheck],
    ) -> Result<(), WorkspaceError>;

    /// Commits whatever is uncommitted in the task's worktrees on the task
    /// branch. A clean worktree gets no commit; one never set up is skipped.
    async fn checkpoint(
        &self,
        task: TaskId,
        repositories: &[Repository],
        message: &str,
    ) -> Result<(), WorkspaceError>;

    /// Like [`Self::checkpoint`], but commits only changes to tracked files,
    /// for a run that failed, timed out or was stopped: its untracked files
    /// may be scratch it meant to delete, so they stay in the worktree.
    async fn checkpoint_tracked(
        &self,
        task: TaskId,
        repositories: &[Repository],
        message: &str,
    ) -> Result<(), WorkspaceError>;

    /// Returns what [`Self::checkpoint_tracked`] committed and left out for
    /// each repository, to tell the task's next run.
    async fn checkpoint_notes(&self, task: TaskId, repositories: &[Repository]) -> String;

    /// Forgets the notes once a run that was told them has started.
    async fn clear_checkpoint_notes(&self, task: TaskId, repositories: &[Repository]);

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

    /// Resolves the task branch and computes its diff against the expected PR
    /// head, or against the default branch when publishing without a PR target.
    async fn preview_publish(
        &self,
        task: TaskId,
        repository: &Repository,
        expected_head: Option<&str>,
    ) -> Result<PublishPreview, WorkspaceError>;
}

/// The branch a task works on: `threadlane/T-0142`.
#[must_use]
pub fn task_branch(task: TaskId) -> String {
    format!("threadlane/{task}")
}

/// The task whose [`task_branch`] `branch` is, spelled exactly so.
#[must_use]
pub fn branch_task(branch: &str) -> Option<TaskId> {
    let id = task_ref(&serde_json::Value::String(
        branch.strip_prefix("threadlane/")?.to_owned(),
    ))?;
    (task_branch(id) == branch).then_some(id)
}

/// A task id given as `T-0142`, `142`, or the number 142.
#[must_use]
pub fn task_ref(value: &serde_json::Value) -> Option<TaskId> {
    let parsed = match value {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(text) => {
            let text = text.trim();
            let digits = text
                .strip_prefix("T-")
                .or_else(|| text.strip_prefix("t-"))
                .unwrap_or(text);
            digits.parse().ok()
        }
        _ => None,
    };
    parsed.filter(|id| *id > 0).map(TaskId)
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
        for id in [1, 7, 142, 12345] {
            assert_eq!(branch_task(&task_branch(TaskId(id))), Some(TaskId(id)));
        }
        for other in [
            "threadlane/7",
            "threadlane/t-0007",
            "threadlane/T-7",
            "threadlane/T-0000",
            "threadlane/T-0007x",
            "other/T-0007",
            "T-0007",
            "main",
        ] {
            assert_eq!(branch_task(other), None, "{other}");
        }
    }
}
