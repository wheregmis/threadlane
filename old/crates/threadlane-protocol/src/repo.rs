//! Project filesystem and Git wire contract (protocol v3).
//!
//! These types cross the daemon boundary: `SessionCommand::GitRequest`,
//! `SessionCommand::ListProjectFiles`/`ReadProjectFile`/`WriteProjectFile`,
//! `ReadProjectFileVersioned`/`WriteProjectFileGuarded`,
//! and `SessionEvent::WorkspaceChanged` carry them so a thin client can
//! browse the daemon host's file tree and run Git operations on its
//! checkouts. The canonical definitions live here; `threadlane-git`
//! re-exports them for its engine-side callers.
//!
//! Everything is plain data — `Serialize + Deserialize`, path-referenced —
//! mirroring the `daemon` module's rules.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Shared task branch preview and creation use the same deterministic title slug.
pub fn issue_branch_name(number: u64, title: &str, suffix: &str) -> String {
    let slug = title
        .chars()
        .flat_map(char::to_lowercase)
        .fold(String::new(), |mut slug, character| {
            if character.is_ascii_alphanumeric() {
                slug.push(character);
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
            slug
        })
        .trim_matches('-')
        .to_string();
    format!(
        "issue/{number}-{}-{suffix}",
        if slug.is_empty() { "task" } else { &slug }
    )
}

/// One node of the project file tree produced by `ListProjectFiles`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFileNode {
    /// Path relative to the project root (`/` separated).
    pub relative_path: String,
    /// Leaf name (last component of `relative_path`).
    pub name: String,
    pub is_dir: bool,
    #[serde(default)]
    pub children: Vec<ProjectFileNode>,
    /// A directory whose contents were not (fully) listed because the scan hit its
    /// entry limit or depth cap, so an empty `children` does not mean an empty folder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Options that alter how a diff is produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffOptions {
    /// Corresponds to `git diff --ignore-all-space`.
    pub ignore_whitespace: bool,
}

/// `FileInventory` error marker when `work_dir` is outside Git. The UI maps
/// it back to its "requires a Git workspace" affordance instead of treating
/// it as a generic failure.
pub const FILE_INVENTORY_NOT_A_REPOSITORY: &str = "not inside a Git work tree";

/// The files `git ls-files` reports for a repository, de-duplicated.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileInventory {
    /// Repository-relative file paths (`/` separated), sorted.
    pub paths: Vec<String>,
    /// Names that were not valid UTF-8 and were skipped rather than mangled.
    pub non_utf8_skipped: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubPrInfo {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub head_ref: String,
    pub base_ref: String,
    pub comments_count: usize,
    pub review_comments: Vec<PrReviewComment>,
    #[serde(default)]
    pub review_comments_complete: bool,
    pub checks: Vec<PrCheckStatus>,
    pub total_checks: usize,
    pub failing_checks: usize,
    pub pending_checks: usize,
    pub passing_checks: usize,
    pub body: String,
    pub author: String,
    pub updated_at: String,
    pub review_decision: Option<String>,
    pub head_oid: String,
    pub issue_comments: Vec<PrConversationComment>,
    pub reviews: Vec<PrReview>,
    #[serde(default)]
    pub commits: Vec<GitHubPrCommit>,
    pub files: Vec<GitHubPrFile>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubPrCommit {
    pub oid: String,
    pub message: String,
    pub author: String,
    pub committed_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrConversationComment {
    pub remote_id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
    #[serde(default)]
    pub viewer_did_author: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReview {
    pub remote_id: String,
    pub author: String,
    pub body: String,
    pub state: String,
    pub submitted_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubPrFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub change_type: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReviewComment {
    pub remote_id: String,
    #[serde(default)]
    pub in_reply_to_id: Option<String>,
    pub author: String,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub created_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrCheckStatus {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub details_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchInfo {
    pub name: String,
    pub is_current: bool,
    pub is_default: bool,
    pub is_remote: bool,
    pub relative_time: String,
    pub committer_date_unix: i64,
    pub upstream: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStashInfo {
    pub index: usize,
    pub name: String,
    pub message: String,
    pub relative_time: String,
    pub timestamp: u64,
    pub branch: Option<String>,
    pub files: Vec<GitFile>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitInfo {
    pub sha: String,
    pub short_sha: String,
    pub summary: String,
    pub body: String,
    pub author_name: String,
    pub author_email: String,
    pub relative_time: String,
    pub timestamp: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub default_branch: Option<String>,
    pub detached: bool,
    pub has_upstream: bool,
    pub has_changes: bool,
    pub staged_changes: bool,
    pub unstaged_changes: bool,
    pub ahead: usize,
    pub behind: usize,
    pub pr_ready: bool,
    pub pr_lookup_available: bool,
    pub remote: Option<String>,
    pub branches: Vec<String>,
    pub branch_details: Vec<GitBranchInfo>,
    pub files: Vec<GitFile>,
    pub pr: Option<GitHubPrInfo>,
    pub last_fetched_at: Option<String>,
    pub stashes: Vec<GitStashInfo>,
    pub current_stash: Option<GitStashInfo>,
    pub recent_commits: Vec<GitCommitInfo>,
}

// ---------------------------------------------------------------------------
// GitHub (forge) payloads — canonical wire types. `threadlane-git` re-exports
// them so `threadlane_git::*` paths keep working; they ride
// `SessionCommand::GitHubRequest`/`CommandResponse::GitHub` on the wire.

/// A forge repository coordinate (`host/owner/repo` parsed from remote URLs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubRepository {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubLabel {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubIssueSummary {
    pub issue: crate::daemon::GitHubIssueRef,
    pub title: String,
    pub state: String,
    pub author: String,
    pub updated_at: String,
    pub labels: Vec<GitHubLabel>,
    pub assignees: Vec<String>,
    pub comments_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubIssueComment {
    pub remote_id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubIssueDetail {
    pub summary: GitHubIssueSummary,
    pub body: String,
    pub comments: Vec<GitHubIssueComment>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubPullRequestSummary {
    pub repository: GitHubRepository,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
    pub is_draft: bool,
    pub head_ref: String,
    pub base_ref: String,
    pub author: String,
    pub updated_at: String,
    pub review_decision: Option<String>,
    pub checks: Vec<PrCheckStatus>,
}

/// `ListIssues` state filter. The forge API accepts open/closed only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubIssueListState {
    #[default]
    Open,
    Closed,
}

impl GitHubIssueListState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// `ListPullRequests` state filter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubPrListState {
    #[default]
    Open,
    Closed,
    Merged,
}

impl GitHubPrListState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
        }
    }
}

/// One GitHub (forge) operation against the checkout at
/// `SessionCommand::GitHubRequest::work_dir`. The daemon runs these through
/// the repository's configured forge integration (`gh`), so they work for
/// remote clients that have no forge access of their own.
///
/// Read-only operations answer with their own [`GitHubResponse`] payload;
/// mutations answer [`GitHubResponse::Action`] with the command's
/// user-facing result text, if any.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GitHubOperation {
    /// `gh issue list` — summaries for the issue panel.
    ListIssues {
        #[serde(default)]
        state: GitHubIssueListState,
        #[serde(default)]
        query: Option<String>,
        #[serde(default = "default_github_list_limit")]
        limit: usize,
    },
    /// `gh issue view` — one issue with its body and comments.
    InspectIssue { number: u64 },
    /// `gh issue create` — answered by [`GitHubResponse::Number`].
    CreateIssue { title: String, body: String },
    /// `gh issue comment`.
    CommentIssue { number: u64, body: String },
    /// `gh issue close`/`gh issue reopen`.
    SetIssueState { number: u64, close: bool },
    /// `gh issue delete` — permanent; callers confirm first.
    DeleteIssue { number: u64 },
    /// `gh pr list` — summaries for the pull-request panel.
    ListPullRequests {
        #[serde(default)]
        state: GitHubPrListState,
        #[serde(default)]
        query: Option<String>,
        #[serde(default = "default_github_list_limit")]
        limit: usize,
    },
    /// `gh pr view` — one pull request with checks, reviews, and files.
    InspectPullRequest { number: u64 },
    /// `gh pr diff` — the PR's unified diff text.
    PullRequestDiff { number: u64 },
    /// `gh pr comment`.
    CommentPullRequest { number: u64, body: String },
}

fn default_github_list_limit() -> usize {
    30
}

/// Payload of `CommandResponse::GitHub`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitHubResponse {
    /// `ListIssues` result.
    Issues { issues: Vec<GitHubIssueSummary> },
    /// `InspectIssue` result.
    Issue { detail: GitHubIssueDetail },
    /// `ListPullRequests` result.
    PullRequests { prs: Vec<GitHubPullRequestSummary> },
    /// `InspectPullRequest` result.
    PullRequest { pr: GitHubPrInfo },
    /// `PullRequestDiff` and other textual results.
    Text { text: String },
    /// `CreateIssue` result: the new issue's number.
    Number { number: u64 },
    /// A mutation settled; `message` is the forge command's user-facing
    /// result when it produced one.
    Action { message: Option<String> },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFile {
    pub path: String,
    #[serde(default)]
    pub orig_path: Option<String>,
    /// The porcelain status pair as text (e.g. `"MM"`, `"R "`).
    #[serde(default)]
    pub status: String,
    /// Porcelain index (staged) status character.
    #[serde(default)]
    pub index_status: char,
    /// Porcelain worktree (unstaged) status character.
    #[serde(default)]
    pub worktree_status: char,
    pub staged: bool,
    pub unstaged: bool,
    pub additions: u32,
    pub deletions: u32,
}

impl GitFile {
    /// Untracked (`??`) files are not user edits to versioned content.
    /// Callers that guard destructive actions (e.g. archiving a worktree)
    /// use this to exempt Threadlane's own untracked bookkeeping under
    /// `.threadlane/` without exempting real untracked work.
    pub fn is_untracked(&self) -> bool {
        self.index_status == '?' || self.worktree_status == '?'
    }

    /// The status character that represents this file in one section
    /// (`staged_section` selects index vs worktree columns).
    pub fn status_for_section(&self, staged_section: bool) -> char {
        if staged_section {
            self.index_status
        } else {
            self.worktree_status
        }
    }

    pub fn status_char(&self) -> char {
        if self.index_status != ' ' && self.index_status != '?' {
            self.index_status
        } else if self.worktree_status != ' ' {
            self.worktree_status
        } else {
            'M'
        }
    }
}

/// How a checkout resolves local worktree changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutMode {
    /// Plain `git checkout` — fails when local changes would be overwritten.
    Clean,
    /// Stash, checkout, then pop the stash on the target branch.
    Stash,
    /// Attempt a merge checkout that carries local changes over.
    Carry,
}

/// One Git repository operation executed against the checkout at
/// `SessionCommand::GitRequest::work_dir`.
///
/// Read-only operations answer with their own [`GitResponse`] payload.
/// Mutations run the action, then re-inspect the repository and answer
/// [`GitResponse::Action`] so the requester gets a refreshed status and the
/// user-facing result text in one round trip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GitOperation {
    /// `git status` (+ `fetch --prune` first when `sync_remote`).
    Inspect {
        #[serde(default)]
        sync_remote: bool,
    },
    /// The pull request associated with `branch`, if any.
    InspectPrForBranch { branch: String },
    /// Unified diff of `path` (HEAD vs worktree; untracked included).
    DiffFile {
        path: String,
        #[serde(default)]
        options: DiffOptions,
    },
    /// Diff of `paths`, concatenated in order (empty hunks skipped).
    DiffFiles {
        paths: Vec<String>,
        #[serde(default)]
        options: DiffOptions,
    },
    /// Whole-worktree diff (staged + unstaged + untracked).
    DiffWorktree {
        #[serde(default)]
        options: DiffOptions,
    },
    /// Diff fed to commit-message generation.
    CommitMessageDiff,
    /// `base...HEAD` diff for draft-PR generation.
    DraftPrDiff { base: String },
    /// Diff of `branch` against its base, for lane/worktree review.
    DiffBranch { branch: String },
    /// Files recorded in `stash@{index}`.
    StashFiles { index: usize },
    /// Diff of one file within `stash@{index}`.
    DiffStashFile { index: usize, path: String },
    /// Files recorded in commit `sha`.
    CommitFiles { sha: String },
    /// Diff of one file within commit `sha`.
    DiffCommitFile { sha: String, path: String },
    /// `git ls-files` inventory for `@` completion.
    FileInventory,
    /// Whether `work_dir` is inside a Git work tree.
    IsRepo,

    // --- Mutations: every variant below answers `GitResponse::Action`. ---
    Stage { paths: Vec<String> },
    Unstage { paths: Vec<String> },
    StageAll,
    UnstageAll,
    /// Stage `selected_paths`, unstage the rest, commit staged changes,
    /// then push when `push` is set.
    Commit {
        message: String,
        selected_paths: Vec<String>,
        push: bool,
    },
    Push,
    Pull,
    Fetch,
    /// `gh pr create` via the configured forge integration.
    CreatePullRequest,
    /// `gh pr create --draft` against `base`.
    CreateDraftPullRequest {
        base: String,
        title: String,
        body: String,
    },
    Checkout { branch: String, mode: CheckoutMode },
    CreateBranch { name: String },
    DeleteBranch { branch: String, force: bool },
    Merge { branch: String },
    StashPush {
        message: Option<String>,
        include_untracked: bool,
    },
    PopStash { index: Option<usize> },
    DropStash { index: Option<usize> },
    Discard { paths: Vec<String> },
    DiscardAll,
    IgnoreFile { path: String },
    IgnoreExtension { extension: String },
    /// `git worktree add` at `worktree` on `branch`.
    CreateWorktree { worktree: PathBuf, branch: String },
    /// `git worktree remove` of `worktree`.
    RemoveWorktree { worktree: PathBuf, force: bool },
    PruneWorktrees,
}

/// Result of a mutating [`GitOperation`]: what the action itself did plus a
/// fresh `git status` snapshot taken afterwards (matching the panel's
/// action-then-inspect flow — a failed action still reports current status).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitActionOutcome {
    /// The action's own failure, if it did not complete.
    #[serde(default)]
    pub action_error: Option<String>,
    /// User-facing success note (e.g. `"Pull request created: …"`).
    #[serde(default)]
    pub message: Option<String>,
    /// Post-action `Inspect` result; `Err` when even status is unavailable.
    pub status: Result<GitStatus, String>,
}

/// Payload of `CommandResponse::Git`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitResponse {
    /// `Inspect`: a full status snapshot.
    Status { status: Box<GitStatus> },
    /// `InspectPrForBranch`: the PR or `None`.
    Pr { pr: Option<GitHubPrInfo> },
    /// `StashFiles`/`CommitFiles`: files in the queried object.
    Files { files: Vec<GitFile> },
    /// `FileInventory`: the `ls-files` name list.
    Inventory { inventory: GitFileInventory },
    /// Diff bodies and other textual results.
    Text { text: String },
    /// `IsRepo` and other yes/no answers.
    Bool { value: bool },
    /// A mutation settled; see [`GitActionOutcome`].
    Action { outcome: GitActionOutcome },
}

/// One saved-file matching line. Snippets are plain text, not Markdown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSearchMatch {
    pub path: String,
    pub line: usize,
    pub snippet: String,
    /// UTF-8 byte range of the first match within the bounded snippet.
    pub match_start: usize,
    pub match_end: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSearchResult {
    pub matches: Vec<FileSearchMatch>,
    /// Exact coverage limits and skip counts; empty only for a complete scan.
    pub partial: Vec<String>,
}
