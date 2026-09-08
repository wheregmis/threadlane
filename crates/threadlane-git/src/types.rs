use std::path::PathBuf;

pub const GIT_FIELD_SEPARATOR: char = '\u{1f}';

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubRepository {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubIssueRef {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubLabel {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubIssueSummary {
    pub issue: GitHubIssueRef,
    pub title: String,
    pub state: String,
    pub author: String,
    pub updated_at: String,
    pub labels: Vec<GitHubLabel>,
    pub assignees: Vec<String>,
    pub comments_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubIssueDetail {
    pub summary: GitHubIssueSummary,
    pub body: String,
    pub comments: Vec<GitHubIssueComment>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubIssueComment {
    pub(crate) remote_id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub(crate) url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubPrCommit {
    pub oid: String,
    pub message: String,
    pub author: String,
    pub committed_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrConversationComment {
    pub remote_id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrReview {
    pub remote_id: String,
    pub author: String,
    pub body: String,
    pub state: String,
    pub submitted_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubPrFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub change_type: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PullRequestReviewCommentDraft {
    pub(crate) path: String,
    pub(crate) body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PullRequestReviewVerdict {
    Comment,
    Approve,
    RequestChanges,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrCheckStatus {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub details_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitBranchInfo {
    pub name: String,
    pub is_current: bool,
    pub is_default: bool,
    pub is_remote: bool,
    pub relative_time: String,
    pub committer_date_unix: i64,
    pub upstream: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitStashInfo {
    pub index: usize,
    pub(crate) name: String,
    pub message: String,
    pub relative_time: String,
    pub(crate) timestamp: u64,
    pub(crate) branch: Option<String>,
    pub(crate) files: Vec<GitFile>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitWorktreeInfo {
    pub(crate) path: PathBuf,
    pub(crate) branch: Option<String>,
    pub(crate) head: String,
    pub(crate) is_bare: bool,
    pub(crate) is_detached: bool,
    pub(crate) is_locked: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitFile {
    pub path: String,
    pub(crate) status: String,
    pub(crate) index_status: char,
    pub(crate) worktree_status: char,
    pub(crate) staged: bool,
    pub(crate) unstaged: bool,
    pub additions: u32,
    pub deletions: u32,
}

impl GitFile {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn status_for_section(&self, staged_section: bool) -> char {
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
