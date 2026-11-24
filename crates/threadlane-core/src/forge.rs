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
/// The most pull requests one `merge` call may name in `pulls`.
pub const MAX_MERGE_BATCH: usize = 20;
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
/// Replace the title and/or body of a registered repository's pull request.
pub const EDIT_PULL_REQUEST: &str = "edit_pull_request";
/// Reply to and resolve one review thread of a pull request, naming the fixing commit
/// or why its finding was declined.
pub const RESOLVE_REVIEW_THREAD: &str = "resolve_review_thread";
/// Attach a local image to one of the task's pull requests, as a comment
/// showing it (#735). The runner performs it in the task's worktree.
pub const ATTACH_IMAGE: &str = "attach_image";
/// `attach_image` argument the runner sets: the image's Git blob ID, so the
/// approval covers those exact bytes.
pub const IMAGE_BLOB: &str = "image_blob";
/// The largest image `attach_image` takes, in bytes.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// The image's file extension if `head`, its first bytes, is a PNG, JPEG,
/// GIF or WebP file.
#[must_use]
pub fn image_extension(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("gif")
    } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// What [`Forge::attach_image`] posts: which image, on which pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullRequestImage<'a> {
    /// The registered remote URL.
    pub remote: &'a str,
    pub number: std::num::NonZeroU64,
    /// The image file, already checked to be an image.
    pub image: &'a Path,
    /// Its file extension, from [`image_extension`].
    pub extension: &'a str,
    /// The approved Git blob ID of its bytes.
    pub blob: &'a str,
    /// The image's alt text.
    pub caption: &'a str,
}

/// Ask reviewers, or a review bot by its trigger comment, to review a pull request again.
pub const REQUEST_REVIEW: &str = "request_review";
/// Create a lightweight tag at a named commit on a registered repository,
/// without publishing a release itself (#748).
pub const CREATE_TAG: &str = "create_tag";

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
    /// Merge the numbered pull request only if its head is still `expected_head`.
    Merge {
        number: std::num::NonZeroU64,
        expected_head: String,
    },
    /// Merge these pull requests in this order, each only if its head is
    /// still the paired commit; stop at the first that fails (#692).
    MergeBatch {
        pulls: Vec<(std::num::NonZeroU64, String)>,
    },
    /// Mark the numbered draft pull request ready for review.
    MarkReady { number: std::num::NonZeroU64 },
    /// Mark these draft pull requests ready in this order; stop at the first
    /// that fails (#692).
    MarkReadyBatch { numbers: Vec<std::num::NonZeroU64> },
    /// Replace the numbered pull request's title and/or body; `None` keeps it.
    EditPullRequest {
        number: std::num::NonZeroU64,
        title: Option<String>,
        body: Option<String>,
    },
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
    /// If the numbered pull request's head is still `expected_head` and contains
    /// `fixed_by`, reply `body` on its review thread `thread_id` and resolve it.
    /// With no `fixed_by`, the finding was declined and `body` says why (#713).
    ResolveReviewThread {
        number: std::num::NonZeroU64,
        expected_head: String,
        thread_id: String,
        #[serde(default)]
        fixed_by: Option<String>,
        body: String,
    },
    /// If the numbered pull request's head is still `expected_head`, re-request
    /// `reviewers` and post `comment`, such as a review bot's trigger.
    RequestReview {
        number: std::num::NonZeroU64,
        expected_head: String,
        reviewers: Vec<String>,
        comment: Option<String>,
    },
    /// Create tag `tag` at the full commit ID `commit`, only if no tag of
    /// that name exists; any existing tag, even at `commit`, is refused.
    CreateTag { tag: String, commit: String },
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

    /// Pushes the image, if its bytes still have the approved blob ID, as a
    /// one-file commit on its own branch, then comments it on the pull
    /// request. Returns the image URL and the comment's URL.
    async fn attach_image(
        &self,
        worktree: &Path,
        attachment: PullRequestImage<'_>,
    ) -> Result<String, ForgeError>;

    /// The branch pull request `number` of the registered `remote` merges into.
    async fn pull_request_base(
        &self,
        remote: &str,
        number: std::num::NonZeroU64,
    ) -> Result<String, ForgeError>;
}

#[cfg(test)]
mod tests {
    use super::{image_extension, valid_label_name};

    #[test]
    fn only_image_signatures_are_images() {
        assert_eq!(image_extension(b"\x89PNG\r\n\x1a\n...."), Some("png"));
        assert_eq!(image_extension(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpg"));
        assert_eq!(image_extension(b"GIF89a.."), Some("gif"));
        assert_eq!(image_extension(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(image_extension(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(image_extension(b"-----BEGIN OPENSSH"), None);
        assert_eq!(image_extension(b""), None);
    }

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
