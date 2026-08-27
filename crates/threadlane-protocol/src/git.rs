use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitFileStatusCode {
    Modified,
    Added,
    Deleted,
    Untracked,
    Renamed,
    Typechange,
    Conflicted,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFile {
    pub path: String,
    pub status: String,
    pub index_status: char,
    pub worktree_status: char,
    pub staged: bool,
    pub unstaged: bool,
    pub additions: u32,
    pub deletions: u32,
}

impl GitFile {
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
pub struct PrReviewComment {
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
    pub checks: Vec<PrCheckStatus>,
    pub total_checks: usize,
    pub failing_checks: usize,
    pub pending_checks: usize,
    pub passing_checks: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitWorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub is_bare: bool,
    pub is_detached: bool,
    pub is_locked: bool,
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

// ── Client-to-Daemon Requests ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileStatus {
    pub path: String,
    pub status: GitFileStatusCode,
    pub staged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusRequest {
    pub project_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusResponse {
    pub branch: Option<String>,
    pub files: Vec<GitFileStatus>,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiffRequest {
    pub project_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    pub staged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiffResponse {
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchesRequest {
    pub project_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchesResponse {
    pub branches: Vec<GitBranchInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCheckoutRequest {
    pub project_path: String,
    pub branch: String,
    pub create_if_missing: bool,
}
