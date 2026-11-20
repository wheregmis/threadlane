//! Shared repository effects that the runner performs after hub approval (Plan.md §9.2).

use std::fmt;
use std::path::Path;

use async_trait::async_trait;

/// The tool that pushes the task's branch (tier 2, task lanes only).
pub const PUBLISH_BRANCH: &str = "publish_branch";
/// `publish_branch` argument naming an ended task whose branch is pushed to
/// instead of the caller's own, which updates that task's pull request.
pub const ONTO_TASK: &str = "onto_task";
/// The tool that opens a draft pull request from the task's branch,
/// publishing the branch in the same call (tier 2, task lanes only).
pub const OPEN_PULL_REQUEST: &str = "open_pull_request";
/// Fast-forward an existing pull request from an approved task commit.
pub const FAST_FORWARD_PULL_REQUEST: &str = "fast_forward_pull_request";
/// The full commit ID the caller expects the pull request head to be at.
pub const EXPECTED_HEAD: &str = "expected_head";

/// Post a comment on a registered repository's issue or pull request.
pub const COMMENT: &str = "comment";
/// Create an issue in a registered repository.
pub const CREATE_ISSUE: &str = "create_issue";
/// Report a product limitation to the Threadlane upstream during a project dream.
pub const REPORT_LIMITATION: &str = "report_threadlane_limitation";
/// Merge a registered repository's pull request.
pub const MERGE: &str = "merge";
/// Mark a registered repository's draft pull request ready for review.
pub const MARK_READY: &str = "mark_ready";
/// Add or remove labels on a registered repository's issue (not a pull request).
pub const LABEL_ISSUE: &str = "label_issue";
/// The most labels one `label_issue` call may name.
pub const MAX_LABELS: usize = 20;

/// Whether `name` can be a GitHub label: at most 50 characters, no control
/// characters, and no surrounding whitespace.
#[must_use]
pub fn valid_label_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 50
        && name.trim() == name
        && !name.chars().any(char::is_control)
}
/// Close a registered repository's pull request, optionally deleting its head branch.
pub const CLOSE_PULL_REQUEST: &str = "close_pull_request";
/// Run one `gh` command against a registered repository, for a GitHub action
/// no dedicated tool covers yet. The user always decides it.
pub const GITHUB_CLI: &str = "github_cli";

/// Assign a registered repository's issue to the logged-in user.
pub const ASSIGN_ISSUE: &str = "assign_issue";

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
    /// Mark the numbered draft pull request ready for review.
    MarkReady { number: std::num::NonZeroU64 },
    /// Add and remove labels on the numbered issue. Labels the repository
    /// does not have are reported, never created.
    LabelIssue {
        number: std::num::NonZeroU64,
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// Close the numbered pull request if its head is still `expected_head`,
    /// then delete its head branch if asked.
    ClosePullRequest {
        number: std::num::NonZeroU64,
        expected_head: String,
        delete_branch: bool,
    },
    /// Run `gh` with these arguments, scoped to the repository with `--repo`.
    GhCli { args: Vec<String> },
    /// Assign the numbered issue to the logged-in user.
    AssignIssue { number: std::num::NonZeroU64 },
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

/// The approved commit and the branch it may update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovedBranch<'a> {
    /// The writable branch name.
    pub name: &'a str,
    /// The full commit ID to push.
    pub revision: &'a str,
}

/// The exact pull request and revisions authorized for a fast-forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullRequestUpdate<'a> {
    pub number: std::num::NonZeroU64,
    pub expected_head: &'a str,
    pub revision: &'a str,
}

/// Performs approved repository effects with the forge tools' existing logins (invariant 8).
#[async_trait]
pub trait Forge: Send + Sync + fmt::Debug {
    /// Pushes the approved commit to the named branch on the remote. Safe to repeat.
    async fn push(&self, worktree: &Path, branch: ApprovedBranch<'_>) -> Result<(), ForgeError>;

    /// Fast-forwards an open same-repository PR and returns its head branch.
    async fn fast_forward_pull_request(
        &self,
        worktree: &Path,
        repository: &crate::model::Repository,
        update: PullRequestUpdate<'_>,
    ) -> Result<String, ForgeError>;

    /// Pushes `branch`, then opens a draft pull request from it onto `base`.
    /// Returns the pull request's URL. Not safe to repeat: a second call may
    /// open a second pull request, so after an interruption ask
    /// [`Forge::find_pull_request`] first. `opened_by` names who opens it,
    /// as `<agent name>, <task id>`, for the body's attribution line.
    async fn open_draft_pull_request(
        &self,
        worktree: &Path,
        branch: ApprovedBranch<'_>,
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

#[cfg(test)]
mod tests {
    use super::valid_label_name;

    #[test]
    fn label_names_are_short_plain_text() {
        assert!(valid_label_name("bug"));
        assert!(valid_label_name("good first issue"));
        assert!(valid_label_name("area: hub/runner"));
        assert!(!valid_label_name(""));
        assert!(!valid_label_name(" bug"));
        assert!(!valid_label_name("bug\n"));
        assert!(!valid_label_name(&"x".repeat(51)));
    }
}
