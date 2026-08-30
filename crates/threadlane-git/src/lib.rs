//! Project-local Git operations used by threadlane workspace.
//!
//! Git is intentionally invoked through the user's configured `git` executable
//! so existing credential helpers, SSH keys, remotes, hooks, and repository
//! configuration continue to work unchanged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(test)]
thread_local! {
    static COMMAND_SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const REPOSITORY_METADATA_TTL: Duration = Duration::from_secs(60);
const PR_INSPECTION_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct RepositoryMetadata {
    remote: Option<String>,
    default_branch: Option<String>,
}

type TimedCache<T> = HashMap<PathBuf, (Instant, T)>;
type PrCacheKey = (PathBuf, String);

static REPOSITORY_METADATA_CACHE: OnceLock<Mutex<TimedCache<RepositoryMetadata>>> = OnceLock::new();
static PR_CACHE: OnceLock<Mutex<HashMap<PrCacheKey, (Instant, Option<GitHubPrInfo>)>>> =
    OnceLock::new();

fn fresh_cache_value<T: Clone>(entry: &(Instant, T), now: Instant, ttl: Duration) -> Option<T> {
    (now.duration_since(entry.0) <= ttl).then(|| entry.1.clone())
}

fn repository_key(work_dir: &Path) -> PathBuf {
    work_dir
        .canonicalize()
        .unwrap_or_else(|_| work_dir.to_path_buf())
}

fn pr_cache_key(work_dir: &Path, branch: &str) -> PrCacheKey {
    (repository_key(work_dir), branch.to_owned())
}

fn invalidate_pr_cache(work_dir: &Path, branch: &str) {
    if let Some(cache) = PR_CACHE.get() {
        if let Ok(mut cache) = cache.lock() {
            cache.remove(&pr_cache_key(work_dir, branch));
        }
    }
}

const GIT_FIELD_SEPARATOR: char = '\u{1f}';

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
    pub checks: Vec<PrCheckStatus>,
    pub total_checks: usize,
    pub failing_checks: usize,
    pub pending_checks: usize,
    pub passing_checks: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrReviewComment {
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
    pub name: String,
    pub message: String,
    pub relative_time: String,
    pub timestamp: u64,
    pub branch: Option<String>,
    pub files: Vec<GitFile>,
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
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub is_bare: bool,
    pub is_detached: bool,
    pub is_locked: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    pub status: String,
    pub index_status: char,
    pub worktree_status: char,
    pub staged: bool,
    pub unstaged: bool,
    pub additions: u32,
    pub deletions: u32,
}

impl GitFile {
    #[cfg_attr(not(test), allow(dead_code))]
    fn status_for_section(&self, staged_section: bool) -> char {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitError {
    pub work_dir: PathBuf,
    pub message: String,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.work_dir.display(), self.message)
    }
}

impl std::error::Error for GitError {}

fn command(work_dir: &Path, args: &[&str]) -> Result<String, GitError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        #[cfg(test)]
        COMMAND_SPAWNS.set(COMMAND_SPAWNS.get() + 1);
        let output = Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| GitError {
                work_dir: work_dir.to_path_buf(),
                message: format!("could not start git: {error}"),
            })?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let is_lock_error = stderr.contains("index.lock") || stderr.contains("Unable to create");
        if is_lock_error && attempts <= 5 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: if stderr.is_empty() {
                format!("git exited with {}", output.status)
            } else {
                stderr
            },
        });
    }
}

fn gh_command(work_dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("gh");
    command
        .args(args)
        .current_dir(work_dir)
        .env("GH_PROMPT_DISABLED", "1");
    command
}

fn parse_status(_work_dir: &Path, porcelain: &str) -> GitStatus {
    let mut status = GitStatus::default();
    let records = if porcelain.contains('\0') {
        porcelain
            .split('\0')
            .filter(|record| !record.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    } else {
        porcelain.lines().map(str::to_owned).collect::<Vec<_>>()
    };
    let mut records = records.into_iter();
    let header = records
        .next()
        .and_then(|line| line.strip_prefix("## ").map(str::to_owned));
    if let Some(header) = header {
        status.has_upstream = header.contains("...");
        let head = header.split("...").next().unwrap_or(&header);
        if head == "HEAD" || head.starts_with("(no branch)") {
            status.detached = true;
        } else if !head.is_empty() {
            status.branch = Some(head.to_owned());
        }

        if let Some(ahead_behind) = header
            .split(" [")
            .nth(1)
            .and_then(|value| value.strip_suffix(']'))
        {
            for part in ahead_behind.split(", ") {
                if let Some(value) = part.strip_prefix("ahead ") {
                    status.ahead = value.parse().unwrap_or(0);
                } else if let Some(value) = part.strip_prefix("behind ") {
                    status.behind = value.parse().unwrap_or(0);
                }
            }
        }
    }

    while let Some(line) = records.next() {
        let bytes = line.as_bytes();
        if bytes.len() < 2 {
            continue;
        }
        let index = bytes[0] as char;
        let worktree = bytes[1] as char;
        status.staged_changes |= index != ' ' && index != '?';
        status.unstaged_changes |= worktree != ' ';
        status.has_changes = true;
        let raw_path = if porcelain.contains('\0') {
            line.get(3..).unwrap_or_default()
        } else {
            line.get(3..).unwrap_or_default().trim()
        };
        // With -z, rename/copy records are followed by the old path as a
        // separate record; the first path is already the new path we display.
        // The line-based fallback keeps the legacy test format readable.
        if (index == 'R' || index == 'C' || worktree == 'R' || worktree == 'C')
            && porcelain.contains('\0')
        {
            let _old_path = records.next();
        }
        let path = raw_path
            .rsplit_once(" -> ")
            .map(|(_, new_path)| new_path)
            .unwrap_or(raw_path)
            .to_owned();
        if !path.is_empty() {
            let status_code = if index == '?' {
                "?".to_owned()
            } else {
                let code = format!("{index}{worktree}");
                code.trim().to_owned()
            };
            status.files.push(GitFile {
                path,
                status: status_code,
                index_status: index,
                worktree_status: worktree,
                staged: index != ' ' && index != '?',
                unstaged: index == '?' || worktree != ' ',
                additions: 0,
                deletions: 0,
            });
        }
    }
    status
}

pub fn inspect_files(work_dir: &Path) -> Result<Vec<GitFile>, GitError> {
    let porcelain = command(work_dir, &["status", "--porcelain=v1", "-b", "-z"])?;
    let mut status = parse_status(work_dir, &porcelain);
    apply_numstats(work_dir, &mut status);
    Ok(status.files)
}

fn apply_numstats(work_dir: &Path, status: &mut GitStatus) {
    let numstat_output = command(work_dir, &["diff", "HEAD", "--numstat"])
        .or_else(|_| command(work_dir, &["diff", "--numstat"]));
    let mut numstats = std::collections::HashMap::new();
    if let Ok(output) = &numstat_output {
        for line in output.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 3 {
                let add = parts[0].parse::<u32>().unwrap_or(0);
                let del = parts[1].parse::<u32>().unwrap_or(0);
                numstats.insert(parts[2].trim().to_owned(), (add, del));
            }
        }
    }
    for file in &mut status.files {
        if let Some(&(add, del)) = numstats.get(&file.path) {
            file.additions = add;
            file.deletions = del;
        } else if file.index_status == '?' || file.worktree_status == '?' {
            if let Ok(content) = std::fs::read_to_string(work_dir.join(&file.path)) {
                let count = content.lines().count() as u32;
                file.additions = if count == 0 { 1 } else { count };
            }
        }
    }
}

pub fn sync_remote(work_dir: &Path) -> Result<(), GitError> {
    command(work_dir, &["fetch", "--prune", "--quiet"])?;
    if let Ok(Some(branch)) = current_branch(work_dir) {
        invalidate_pr_cache(work_dir, &branch);
    }
    Ok(())
}

fn repository_metadata(work_dir: &Path) -> RepositoryMetadata {
    let key = repository_key(work_dir);
    let now = Instant::now();
    let cache = REPOSITORY_METADATA_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(metadata) = cache.lock().ok().and_then(|cache| {
        cache
            .get(&key)
            .and_then(|entry| fresh_cache_value(entry, now, REPOSITORY_METADATA_TTL))
    }) {
        return metadata;
    }
    let metadata = RepositoryMetadata {
        remote: command(work_dir, &["config", "--get", "remote.origin.url"])
            .ok()
            .map(|remote| remote.trim().to_owned())
            .filter(|remote| !remote.is_empty()),
        default_branch: discover_default_branch(work_dir),
    };
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, (now, metadata.clone()));
    }
    metadata
}

pub fn fetch(work_dir: &Path) -> Result<(), GitError> {
    sync_remote(work_dir)
}

pub fn list_branches_detailed(
    work_dir: &Path,
    provided_default_branch: Option<&str>,
) -> Result<Vec<GitBranchInfo>, GitError> {
    let def_branch = provided_default_branch
        .map(str::to_owned)
        .or_else(|| discover_default_branch(work_dir));
    let output = command(
        work_dir,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)\x1f%(committerdate:relative)\x1f%(committerdate:unix)\x1f%(upstream:short)\x1f%(HEAD)",
            "refs/heads",
            "refs/remotes/origin",
        ],
    )?;

    let mut branches = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(GIT_FIELD_SEPARATOR).collect();
        if parts.is_empty() {
            continue;
        }
        let ref_name = parts[0].trim();
        if ref_name.is_empty()
            || ref_name == "origin"
            || ref_name == "origin/HEAD"
            || ref_name.ends_with("/HEAD")
        {
            continue;
        }

        let is_remote = ref_name.starts_with("origin/");
        let is_current = parts.get(4).map_or(false, |h| h.trim() == "*");
        let relative_time = parts.get(1).map_or("", |t| t.trim()).to_string();
        let committer_date_unix = parts
            .get(2)
            .and_then(|u| u.trim().parse::<i64>().ok())
            .unwrap_or(0);
        let upstream = parts
            .get(3)
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());
        let is_default = def_branch.as_deref().map_or(false, |db| {
            ref_name == db || ref_name == format!("origin/{db}")
        });

        if seen_names.insert(ref_name.to_string()) {
            branches.push(GitBranchInfo {
                name: ref_name.to_string(),
                is_current,
                is_default,
                is_remote,
                relative_time,
                committer_date_unix,
                upstream,
            });
        }
    }

    Ok(branches)
}

pub fn inspect(work_dir: &Path) -> Result<GitStatus, GitError> {
    let porcelain = command(work_dir, &["status", "--porcelain=v1", "-b", "-z"])?;
    let mut status = parse_status(work_dir, &porcelain);
    apply_numstats(work_dir, &mut status);
    status.branches = command(
        work_dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?
    .lines()
    .map(str::trim)
    .filter(|branch| !branch.is_empty())
    .map(str::to_owned)
    .collect();
    if let Some(current_branch) = status.branch.as_ref() {
        if !status
            .branches
            .iter()
            .any(|branch| branch == current_branch)
        {
            status.branches.push(current_branch.clone());
        }
    }
    let metadata = repository_metadata(work_dir);
    status.default_branch = metadata.default_branch.clone();
    status.branch_details = list_branches_detailed(work_dir, status.default_branch.as_deref())
        .unwrap_or_else(|_| {
            status
                .branches
                .iter()
                .map(|name| GitBranchInfo {
                    name: name.clone(),
                    is_current: status.branch.as_deref() == Some(name),
                    is_default: status.default_branch.as_deref() == Some(name),
                    ..GitBranchInfo::default()
                })
                .collect()
        });
    status.remote = metadata.remote;
    if status.remote.is_some() && status.branch.is_some() {
        if !status.has_upstream && status.ahead == 0 {
            status.ahead = command(work_dir, &["rev-list", "--count", "HEAD"])
                .ok()
                .and_then(|count| count.trim().parse().ok())
                .unwrap_or(0);
        }
    }
    if let (Some(branch), Some(base)) = (status.branch.as_deref(), status.default_branch.as_deref())
    {
        if branch != base {
            let local_base = command(work_dir, &["rev-list", "--count", &format!("{base}..HEAD")])
                .or_else(|_| {
                    command(
                        work_dir,
                        &["rev-list", "--count", &format!("origin/{base}..HEAD")],
                    )
                });
            status.pr_ready = local_base
                .ok()
                .and_then(|count| count.trim().parse().ok())
                .is_some_and(|count: usize| count > 0);
        }
    }
    match inspect_pr(work_dir) {
        Ok(pr) => {
            status.pr = pr;
            status.pr_lookup_available = true;
        }
        Err(_) => {
            status.pr = None;
            status.pr_lookup_available = false;
        }
    }
    let stashes = list_stashes(work_dir).unwrap_or_default();
    let current_branch_name = status.branch.as_deref().unwrap_or("");
    let current_stash = stashes
        .iter()
        .find(|s| s.branch.as_deref() == Some(current_branch_name))
        .cloned();
    status.stashes = stashes;
    status.current_stash = current_stash;
    status.recent_commits = list_commits(work_dir, 50).unwrap_or_default();
    Ok(status)
}

pub fn list_stashes(work_dir: &Path) -> Result<Vec<GitStashInfo>, GitError> {
    let output = match command(
        work_dir,
        &["stash", "list", "--format=%gd%x1f%gs%x1f%cr%x1f%ct"],
    ) {
        Ok(out) => out,
        Err(_) => return Ok(Vec::new()),
    };

    let mut stashes = Vec::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.split(GIT_FIELD_SEPARATOR).collect();
        if parts.len() >= 3 {
            let name = parts[0].trim().to_string();
            let index = name
                .strip_prefix("stash@{")
                .and_then(|s| s.strip_suffix('}'))
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let message = parts[1].trim().to_string();
            let relative_time = parts[2].trim().to_string();
            let timestamp = parts
                .get(3)
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);

            let branch = if let Some(rest) = message.strip_prefix("Stash on ") {
                rest.split_whitespace().next().map(|s| s.to_string())
            } else if let Some(rest) = message.strip_prefix("WIP on ") {
                rest.split(':').next().map(|s| s.trim().to_string())
            } else if let Some(rest) = message.strip_prefix("On ") {
                rest.split(':').next().map(|s| s.trim().to_string())
            } else {
                None
            };

            stashes.push(GitStashInfo {
                index,
                name,
                message,
                relative_time,
                timestamp,
                branch,
                files: Vec::new(),
            });
        }
    }
    Ok(stashes)
}

pub fn inspect_stash_files(work_dir: &Path, stash_index: usize) -> Vec<GitFile> {
    let stash_ref = format!("stash@{{{stash_index}}}");
    let numstat_output = command(
        work_dir,
        &[
            "stash",
            "show",
            "--include-untracked",
            "--numstat",
            &stash_ref,
        ],
    )
    .or_else(|_| command(work_dir, &["stash", "show", "--numstat", &stash_ref]))
    .unwrap_or_default();
    let name_status_output = command(
        work_dir,
        &[
            "stash",
            "show",
            "--include-untracked",
            "--name-status",
            &stash_ref,
        ],
    )
    .or_else(|_| command(work_dir, &["stash", "show", "--name-status", &stash_ref]))
    .unwrap_or_default();

    let mut status_map = std::collections::HashMap::new();
    for line in name_status_output.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(code), Some(path)) = (parts.next(), parts.next()) {
            status_map.insert(
                path.trim().to_string(),
                code.trim().chars().next().unwrap_or('M'),
            );
        }
    }

    let mut files = Vec::new();
    for line in numstat_output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            let additions = parts[0].trim().parse::<u32>().unwrap_or(0);
            let deletions = parts[1].trim().parse::<u32>().unwrap_or(0);
            let path = parts[2].trim().to_string();
            let char_status = *status_map.get(&path).unwrap_or(&'M');
            files.push(GitFile {
                path: path.clone(),
                status: char_status.to_string(),
                index_status: char_status,
                worktree_status: ' ',
                staged: false,
                unstaged: true,
                additions,
                deletions,
            });
        }
    }
    files
}

pub fn diff_stash_file(
    work_dir: &Path,
    stash_index: usize,
    file_path: &str,
) -> Result<String, GitError> {
    let stash_ref = format!("stash@{{{stash_index}}}");
    if let Ok(diff) = command(
        work_dir,
        &[
            "diff",
            &format!("{stash_ref}^..{stash_ref}"),
            "--",
            file_path,
        ],
    ) {
        if !diff.trim().is_empty() {
            return Ok(diff);
        }
    }
    if let Ok(diff) = command(
        work_dir,
        &[
            "diff",
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            &format!("{stash_ref}^3"),
            "--",
            file_path,
        ],
    ) {
        if !diff.trim().is_empty() {
            return Ok(diff);
        }
    }
    if let Ok(content) = command(work_dir, &["show", &format!("{stash_ref}^3:{file_path}")]) {
        return Ok(format!(
            "--- /dev/null\n+++ b/{file_path}\n@@ -0,0 +1,{} @@\n{}",
            content.lines().count(),
            content
        ));
    }
    command(
        work_dir,
        &[
            "diff",
            &format!("{stash_ref}^..{stash_ref}"),
            "--",
            file_path,
        ],
    )
}

pub fn pop_stash(work_dir: &Path, stash_index: Option<usize>) -> Result<(), GitError> {
    if let Some(idx) = stash_index {
        let stash_ref = format!("stash@{{{idx}}}");
        command(work_dir, &["stash", "pop", &stash_ref])?;
    } else {
        command(work_dir, &["stash", "pop"])?;
    }
    Ok(())
}

pub fn drop_stash(work_dir: &Path, stash_index: Option<usize>) -> Result<(), GitError> {
    if let Some(idx) = stash_index {
        let stash_ref = format!("stash@{{{idx}}}");
        command(work_dir, &["stash", "drop", &stash_ref])?;
    } else {
        command(work_dir, &["stash", "drop"])?;
    }
    Ok(())
}

pub fn list_commits(work_dir: &Path, max_count: usize) -> Result<Vec<GitCommitInfo>, GitError> {
    let count_arg = format!("-n{max_count}");
    let output = match command(
        work_dir,
        &[
            "log",
            &count_arg,
            "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%cr%x1f%ct%x1f%s",
        ],
    ) {
        Ok(out) => out,
        Err(_) => return Ok(Vec::new()),
    };

    let mut commits = Vec::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.split(GIT_FIELD_SEPARATOR).collect();
        if parts.len() >= 7 {
            let sha = parts[0].trim().to_string();
            let short_sha = parts[1].trim().to_string();
            let author_name = parts[2].trim().to_string();
            let author_email = parts[3].trim().to_string();
            let relative_time = parts[4].trim().to_string();
            let timestamp = parts[5].trim().parse::<i64>().unwrap_or(0);
            let summary = parts[6].trim().to_string();

            commits.push(GitCommitInfo {
                sha,
                short_sha,
                summary,
                body: String::new(),
                author_name,
                author_email,
                relative_time,
                timestamp,
            });
        }
    }
    Ok(commits)
}

pub fn inspect_commit_files(work_dir: &Path, sha: &str) -> Vec<GitFile> {
    let numstat_output =
        command(work_dir, &["show", "--numstat", "--format=", sha]).unwrap_or_default();
    let name_status_output =
        command(work_dir, &["show", "--name-status", "--format=", sha]).unwrap_or_default();

    let mut status_map = std::collections::HashMap::new();
    let mut rename_destinations = std::collections::HashMap::new();
    for line in name_status_output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if let (Some(code), Some(path)) = (parts.first(), parts.get(1)) {
            let status = code.trim().chars().next().unwrap_or('M');
            if matches!(status, 'R' | 'C') {
                if let Some(destination) = parts.get(2) {
                    let source = path.trim().to_string();
                    let destination = destination.trim().to_string();
                    rename_destinations
                        .insert(format!("{source} => {destination}"), destination.clone());
                    status_map.insert(destination, status);
                }
            } else {
                status_map.insert(path.trim().to_string(), status);
            }
        }
    }

    let mut files = Vec::new();
    for line in numstat_output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            let additions = parts[0].trim().parse::<u32>().unwrap_or(0);
            let deletions = parts[1].trim().parse::<u32>().unwrap_or(0);
            let numstat_path = parts[2].trim();
            let path = rename_destinations
                .get(numstat_path)
                .cloned()
                .unwrap_or_else(|| numstat_path.to_string());
            let char_status = *status_map.get(&path).unwrap_or(&'M');
            files.push(GitFile {
                path: path.clone(),
                status: char_status.to_string(),
                index_status: char_status,
                worktree_status: ' ',
                staged: false,
                unstaged: false,
                additions,
                deletions,
            });
        }
    }
    files
}

pub fn diff_commit_file(work_dir: &Path, sha: &str, file_path: &str) -> Result<String, GitError> {
    if let Ok(diff) = command(
        work_dir,
        &["diff", &format!("{sha}^..{sha}"), "--", file_path],
    ) {
        if !diff.trim().is_empty() {
            return Ok(diff);
        }
    }
    command(work_dir, &["show", sha, "--", file_path])
}

pub fn discard_file_changes(work_dir: &Path, relative_path: &str) -> Result<(), GitError> {
    validate_diff_path(work_dir, relative_path)?;
    let full_path = work_dir.join(relative_path);
    let in_index = command(work_dir, &["ls-files", "--error-unmatch", relative_path]).is_ok();
    if in_index {
        if command(
            work_dir,
            &["restore", "--staged", "--worktree", "--", relative_path],
        )
        .is_err()
        {
            let _ = command(work_dir, &["reset", "HEAD", "--", relative_path]);
            command(work_dir, &["checkout", "HEAD", "--", relative_path])?;
        }
    } else {
        if full_path.is_file() || full_path.is_symlink() {
            let _ = std::fs::remove_file(&full_path);
        } else if full_path.is_dir() {
            let _ = std::fs::remove_dir_all(&full_path);
        }
        let _ = command(work_dir, &["clean", "-f", "-d", "--", relative_path]);
    }
    Ok(())
}

pub fn ignore_file(work_dir: &Path, relative_path: &str) -> Result<(), GitError> {
    validate_diff_path(work_dir, relative_path)?;
    let gitignore_path = work_dir.join(".gitignore");
    let mut current_content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();

    let normalized = relative_path.replace('\\', "/");
    let entry = format!("/{normalized}");

    let lines: Vec<&str> = current_content.lines().collect();
    if !lines
        .iter()
        .any(|l| l.trim() == entry || l.trim() == normalized)
    {
        if !current_content.is_empty() && !current_content.ends_with('\n') {
            current_content.push('\n');
        }
        current_content.push_str(&entry);
        current_content.push('\n');
        std::fs::write(&gitignore_path, current_content).map_err(|e| GitError {
            work_dir: work_dir.to_path_buf(),
            message: format!("Failed to update .gitignore: {e}"),
        })?;
    }

    let _ = command(work_dir, &["rm", "--cached", "-r", "--", relative_path]);
    Ok(())
}

pub fn ignore_extension(work_dir: &Path, ext: &str) -> Result<(), GitError> {
    let ext = ext.trim_start_matches('.');
    let gitignore_path = work_dir.join(".gitignore");
    let mut current_content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();

    let entry = format!("*.{ext}");

    let lines: Vec<&str> = current_content.lines().collect();
    if !lines.iter().any(|l| l.trim() == entry) {
        if !current_content.is_empty() && !current_content.ends_with('\n') {
            current_content.push('\n');
        }
        current_content.push_str(&entry);
        current_content.push('\n');
        std::fs::write(&gitignore_path, current_content).map_err(|e| GitError {
            work_dir: work_dir.to_path_buf(),
            message: format!("Failed to update .gitignore: {e}"),
        })?;
    }

    let _ = command(
        work_dir,
        &["rm", "--cached", "-r", "--", &format!("*.{ext}")],
    );
    Ok(())
}

pub fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        let _ = Command::new("xdg-open").arg(target).spawn();
    }
}

pub fn parse_gh_pr_json(json_str: &str) -> Result<GitHubPrInfo, String> {
    let val: serde_json::Value = serde_json::from_str(json_str).map_err(|e| e.to_string())?;

    let number = val["number"].as_u64().unwrap_or(0);
    let title = val["title"].as_str().unwrap_or("").to_string();
    let url = val["url"].as_str().unwrap_or("").to_string();
    let state = val["state"].as_str().unwrap_or("").to_string();
    let is_draft = val["isDraft"].as_bool().unwrap_or(false);
    let head_ref = val["headRefName"].as_str().unwrap_or("").to_string();
    let base_ref = val["baseRefName"].as_str().unwrap_or("").to_string();

    let mut review_comments = Vec::new();
    if let Some(comments_arr) = val["comments"].as_array() {
        for item in comments_arr {
            let author = item["author"]["login"]
                .as_str()
                .or_else(|| item["author"].as_str())
                .unwrap_or("unknown")
                .to_string();
            let body = item["body"].as_str().unwrap_or("").to_string();
            let created_at = item["createdAt"].as_str().unwrap_or("").to_string();
            let path = item["path"].as_str().map(|s| s.to_string());
            let line = item["line"].as_u64();
            review_comments.push(PrReviewComment {
                author,
                body,
                path,
                line,
                created_at,
            });
        }
    }
    let comments_count = review_comments.len();

    let mut checks = Vec::new();
    let mut failing_checks = 0;
    let mut pending_checks = 0;
    let mut passing_checks = 0;

    if let Some(checks_arr) = val["statusCheckRollup"].as_array() {
        for check in checks_arr {
            let name = check["name"]
                .as_str()
                .or_else(|| check["context"].as_str())
                .unwrap_or("check")
                .to_string();
            let status = check["status"]
                .as_str()
                .or_else(|| check["state"].as_str())
                .unwrap_or("COMPLETED")
                .to_string();
            let conclusion = check["conclusion"]
                .as_str()
                .or_else(|| check["state"].as_str())
                .map(|s| s.to_string());
            let details_url = check["detailsUrl"]
                .as_str()
                .or_else(|| check["targetUrl"].as_str())
                .map(|s| s.to_string());

            let conclusion_upper = conclusion.as_deref().unwrap_or("").to_uppercase();
            let status_upper = status.to_uppercase();

            if matches!(
                conclusion_upper.as_str(),
                "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "ERROR"
            ) {
                failing_checks += 1;
            } else if matches!(
                status_upper.as_str(),
                "IN_PROGRESS" | "QUEUED" | "PENDING" | "EXPECTED"
            ) || conclusion.is_none()
            {
                pending_checks += 1;
            } else if matches!(conclusion_upper.as_str(), "SUCCESS" | "NEUTRAL" | "SKIPPED") {
                passing_checks += 1;
            }

            checks.push(PrCheckStatus {
                name,
                status,
                conclusion,
                details_url,
            });
        }
    }

    let total_checks = checks.len();

    Ok(GitHubPrInfo {
        number,
        title,
        url,
        state,
        is_draft,
        head_ref,
        base_ref,
        comments_count,
        review_comments,
        checks,
        total_checks,
        failing_checks,
        pending_checks,
        passing_checks,
    })
}

fn inspect_pr_uncached(work_dir: &Path, branch: &str) -> Result<Option<GitHubPrInfo>, GitError> {
    let output = gh_command(
        work_dir,
        &[
            "pr",
            "view",
            branch,
            "--json",
            "number,title,url,state,isDraft,comments,statusCheckRollup,headRefName,baseRefName",
        ],
    )
    .output()
    .map_err(|error| GitError {
        work_dir: work_dir.to_path_buf(),
        message: format!("could not start gh: {error}"),
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if stderr.to_ascii_lowercase().contains("no pull request") {
            return Ok(None);
        }
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: if stderr.is_empty() {
                format!("gh exited with {}", output.status)
            } else {
                stderr
            },
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut info = parse_gh_pr_json(&stdout).map_err(|error| GitError {
        work_dir: work_dir.to_path_buf(),
        message: format!("could not parse gh pull request response: {error}"),
    })?;

    // `gh pr view --json comments` exposes issue comments only. Inline
    // review comments live on the REST review-comments endpoint.
    if let Some((repo, number)) = info
        .url
        .split_once("/pull/")
        .and_then(|(repo, number)| number.parse::<u64>().ok().map(|n| (repo, n)))
    {
        let api_path = format!(
            "repos/{}/pulls/{}/comments",
            repo.trim_start_matches("https://github.com/"),
            number
        );
        if let Ok(review_output) =
            gh_command(work_dir, &["api", &api_path, "--paginate", "--slurp"]).output()
        {
            if review_output.status.success() {
                let pages = String::from_utf8_lossy(&review_output.stdout);
                if let Ok(pages) = serde_json::from_str::<Vec<Vec<serde_json::Value>>>(&pages) {
                    for item in pages.into_iter().flatten() {
                        info.review_comments.push(PrReviewComment {
                            author: item["user"]["login"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            body: item["body"].as_str().unwrap_or("").to_string(),
                            path: item["path"].as_str().map(str::to_string),
                            line: item["line"]
                                .as_u64()
                                .or_else(|| item["original_line"].as_u64()),
                            created_at: item["created_at"].as_str().unwrap_or("").to_string(),
                        });
                    }
                    info.comments_count = info.review_comments.len();
                }
            }
        }
    }

    if info.number == 0 {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "gh returned a pull request without a number".to_owned(),
        });
    }
    Ok(Some(info))
}

pub fn inspect_pr(work_dir: &Path) -> Result<Option<GitHubPrInfo>, GitError> {
    let Some(branch) = current_branch(work_dir)? else {
        return Ok(None);
    };
    inspect_pr_for_branch(work_dir, &branch)
}

pub fn inspect_pr_for_branch(
    work_dir: &Path,
    branch: &str,
) -> Result<Option<GitHubPrInfo>, GitError> {
    let key = pr_cache_key(work_dir, branch);
    let now = Instant::now();
    let cache = PR_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(info) = cache.lock().ok().and_then(|cache| {
        cache
            .get(&key)
            .and_then(|entry| fresh_cache_value(entry, now, PR_INSPECTION_TTL))
    }) {
        return Ok(info);
    }
    let info = inspect_pr_uncached(work_dir, branch)?;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, (now, info.clone()));
    }
    Ok(info)
}

pub fn current_branch(work_dir: &Path) -> Result<Option<String>, GitError> {
    let branch = command(work_dir, &["branch", "--show-current"])?;
    Ok((!branch.trim().is_empty()).then(|| branch.trim().to_owned()))
}

pub fn create_branch(work_dir: &Path, name: &str) -> Result<(), GitError> {
    create_branch_from(work_dir, name, None)
}

pub fn create_branch_from(
    work_dir: &Path,
    name: &str,
    start_point: Option<&str>,
) -> Result<(), GitError> {
    let name = validate_branch_name(work_dir, name)?;
    if let Some(start) = start_point.map(str::trim).filter(|s| !s.is_empty()) {
        command(work_dir, &["switch", "-c", &name, start])?;
    } else {
        command(work_dir, &["switch", "-c", &name])?;
    }
    Ok(())
}

pub fn normalize_branch_for_checkout(name: &str) -> &str {
    let trimmed = name.trim();
    trimmed
        .strip_prefix("origin/")
        .or_else(|| trimmed.strip_prefix("refs/heads/"))
        .or_else(|| trimmed.strip_prefix("refs/remotes/origin/"))
        .unwrap_or(trimmed)
}

pub fn checkout(work_dir: &Path, name: &str) -> Result<(), GitError> {
    let clean = normalize_branch_for_checkout(name);
    let name = validate_branch_name(work_dir, clean)?;
    if command(work_dir, &["switch", &name]).is_err() {
        command(work_dir, &["checkout", &name])?;
    }
    Ok(())
}

pub fn checkout_with_stash(work_dir: &Path, name: &str) -> Result<(), GitError> {
    let clean = normalize_branch_for_checkout(name);
    let name = validate_branch_name(work_dir, clean)?;
    let current_branch = command(work_dir, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|branch| branch.trim().to_owned())
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    let stash_msg = format!("Stash on {current_branch} before switching to {name}");
    let _ = command(work_dir, &["stash", "push", "-u", "-m", &stash_msg]);
    if command(work_dir, &["switch", &name]).is_err() {
        command(work_dir, &["checkout", &name])?;
    }
    Ok(())
}

pub fn checkout_carrying_changes(work_dir: &Path, name: &str) -> Result<(), GitError> {
    let clean = normalize_branch_for_checkout(name);
    let valid_name = validate_branch_name(work_dir, clean)?;

    // 1. Try git switch -m (native three-way merge to bring local changes)
    if command(work_dir, &["switch", "-m", &valid_name]).is_ok() {
        return Ok(());
    }
    // 2. Try git checkout -m
    if command(work_dir, &["checkout", "-m", &valid_name]).is_ok() {
        return Ok(());
    }
    // 3. Try plain switch / checkout (if working tree has no conflicts)
    if command(work_dir, &["switch", &valid_name]).is_ok()
        || command(work_dir, &["checkout", &valid_name]).is_ok()
    {
        return Ok(());
    }
    // 4. Fallback: stash, switch, and pop
    let stash_msg = format!("Carrying changes to {valid_name}");
    let _ = command(work_dir, &["stash", "push", "-u", "-m", &stash_msg]);
    if command(work_dir, &["switch", &valid_name]).is_err() {
        if let Err(err2) = command(work_dir, &["checkout", &valid_name]) {
            let _ = command(work_dir, &["stash", "pop"]);
            return Err(err2);
        }
    }
    command(work_dir, &["stash", "pop"])?;
    Ok(())
}

/// Describe changed paths in dependency-friendly groups for atomic commit planning.
/// Source files are emitted before generated/lock files, and lock files are excluded.
pub fn atomic_commit_groups(work_dir: &Path) -> Result<Vec<Vec<String>>, GitError> {
    let mut paths = inspect_files(work_dir)?
        .into_iter()
        .map(|file| file.path)
        .filter(|path| !path.ends_with("Cargo.lock") && !path.ends_with("package-lock.json"))
        .collect::<Vec<_>>();
    paths.sort_by_key(|path| {
        let generated = path.contains("/target/") || path.ends_with(".generated.rs");
        (generated, path.clone())
    });
    Ok(paths.into_iter().map(|path| vec![path]).collect())
}

/// Stages and commits each planned atomic group. If any group fails, newly
/// staged paths are reset so a caller can review and retry without an accidental
/// combined commit. Previously created commits are intentionally retained.
pub fn commit_atomic_groups(
    work_dir: &Path,
    message_prefix: &str,
) -> Result<Vec<Vec<String>>, GitError> {
    let groups = atomic_commit_groups(work_dir)?;
    if groups.is_empty() {
        return Ok(groups);
    }
    let prefix = message_prefix.trim();
    if prefix.is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "commit message prefix cannot be empty".into(),
        });
    }
    for (index, group) in groups.iter().enumerate() {
        let paths = group.iter().map(String::as_str).collect::<Vec<_>>();
        let mut add_args = vec!["add", "--"];
        add_args.extend(paths);
        if let Err(error) = command(work_dir, &add_args) {
            let _ = command(work_dir, &["reset"]);
            return Err(error);
        }
        if let Err(error) = command(
            work_dir,
            &[
                "commit",
                "-m",
                &format!("{prefix} ({}/{})", index + 1, groups.len()),
            ],
        ) {
            let _ = command(work_dir, &["reset"]);
            return Err(error);
        }
    }
    Ok(groups)
}

pub fn commit_staged(work_dir: &Path, message: &str) -> Result<(), GitError> {
    let message = message.trim();
    if message.is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "commit message cannot be empty".to_owned(),
        });
    }
    command(work_dir, &["commit", "-m", message])?;
    Ok(())
}

pub fn push(work_dir: &Path) -> Result<(), GitError> {
    let has_upstream = command(
        work_dir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .ok()
    .map(|upstream| !upstream.trim().is_empty())
    .unwrap_or(false);
    if has_upstream {
        command(work_dir, &["push"])?;
        return Ok(());
    }
    let branch = command(work_dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    if branch.is_empty() || branch == "HEAD" {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "cannot push a detached HEAD; check out a named branch first".to_owned(),
        });
    }
    command(work_dir, &["push", "--set-upstream", "origin", branch])?;
    Ok(())
}

/// Publishes the current branch and creates a GitHub pull request from its commits.
///
/// `gh pr create --fill` uses the current branch's commit messages for the PR
/// title and body, so this action remains non-interactive when invoked from the
/// desktop UI. Publishing first also handles branches that do not have an
/// upstream yet.
pub fn create_pull_request(work_dir: &Path) -> Result<String, GitError> {
    let branch = current_branch(work_dir)?.ok_or_else(|| GitError {
        work_dir: work_dir.to_path_buf(),
        message:
            "cannot create a pull request from a detached HEAD; check out a named branch first"
                .to_owned(),
    })?;

    push(work_dir)?;
    invalidate_pr_cache(work_dir, &branch);

    let output = gh_command(work_dir, &["pr", "create", "--fill"])
        .output()
        .map_err(|error| GitError {
            work_dir: work_dir.to_path_buf(),
            message: format!("could not start gh: {error}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: if stderr.is_empty() {
                format!("gh exited with {}", output.status)
            } else {
                stderr
            },
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn pull(work_dir: &Path) -> Result<String, GitError> {
    command(work_dir, &["pull", "--ff-only"])
}

pub fn merge(work_dir: &Path, branch: &str) -> Result<String, GitError> {
    let branch = branch.trim();
    if branch.is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "branch name to merge cannot be empty".to_owned(),
        });
    }
    command(work_dir, &["merge", "--no-edit", branch])
}

pub fn stage_file(work_dir: &Path, path: &str) -> Result<(), GitError> {
    command(work_dir, &["add", "--", path])?;
    Ok(())
}

pub fn unstage_file(work_dir: &Path, path: &str) -> Result<(), GitError> {
    command(work_dir, &["restore", "--staged", "--", path])?;
    Ok(())
}

fn validate_diff_path(work_dir: &Path, path: &str) -> Result<(), GitError> {
    let invalid = || GitError {
        work_dir: work_dir.to_path_buf(),
        message: format!("path is outside the workspace: {path}"),
    };
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(invalid());
    }

    let root = work_dir.canonicalize().map_err(|error| GitError {
        work_dir: work_dir.to_path_buf(),
        message: format!("could not resolve workspace: {error}"),
    })?;
    let mut existing = work_dir.join(relative);
    while !existing.exists() {
        if !existing.pop() {
            return Err(invalid());
        }
    }
    if !existing
        .canonicalize()
        .map_err(|error| GitError {
            work_dir: work_dir.to_path_buf(),
            message: format!("could not resolve path: {error}"),
        })?
        .starts_with(&root)
    {
        return Err(invalid());
    }
    Ok(())
}

pub fn diff_file(work_dir: &Path, path: &str) -> Result<String, GitError> {
    validate_diff_path(work_dir, path)?;
    // 1. Try diff against HEAD (both staged and unstaged combined)
    if let Ok(head_diff) = command(work_dir, &["diff", "--no-ext-diff", "HEAD", "--", path]) {
        if !head_diff.trim().is_empty() {
            return Ok(head_diff);
        }
    }

    // 2. Try unstaged + staged separately (e.g. if HEAD is unborn or detached)
    let mut diff = String::new();
    let staged_result = command(work_dir, &["diff", "--no-ext-diff", "--cached", "--", path]);
    let staged = staged_result.as_deref().unwrap_or_default();
    if !staged.trim().is_empty() {
        diff.push_str("# Staged changes\n");
        diff.push_str(&staged);
    }
    let unstaged_result = command(work_dir, &["diff", "--no-ext-diff", "--", path]);
    let unstaged = unstaged_result.as_deref().unwrap_or_default();
    if !unstaged.trim().is_empty() {
        if !diff.is_empty() {
            diff.push('\n');
        }
        diff.push_str("# Unstaged changes\n");
        diff.push_str(&unstaged);
    }
    if !diff.trim().is_empty() {
        return Ok(diff);
    }
    if let (Err(staged_error), Err(_unstaged_error)) = (&staged_result, &unstaged_result) {
        return Err(staged_error.clone());
    }

    // 3. If untracked or new file, show whole file as additions via git diff --no-index
    let is_untracked = command(work_dir, &["ls-files", "--error-unmatch", "--", path]).is_err();
    if is_untracked {
        let null_source = if cfg!(windows) { "NUL" } else { "/dev/null" };
        if let Ok(output) = Command::new("git")
            .args([
                "diff",
                "--no-ext-diff",
                "--no-index",
                "--",
                null_source,
                path,
            ])
            .current_dir(work_dir)
            .output()
        {
            let new_file_diff = String::from_utf8_lossy(&output.stdout);
            if !new_file_diff.trim().is_empty() {
                return Ok(new_file_diff.into_owned());
            }
        }
    }

    // 4. Fallback: if file exists on disk and is untracked, synthesize additions
    if is_untracked {
        let full_path = work_dir.join(path);
        if full_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let mut synth = format!("diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n", content.lines().count());
                for line in content.lines() {
                    synth.push('+');
                    synth.push_str(line);
                    synth.push('\n');
                }
                return Ok(synth);
            }
        }
    }

    Ok("No textual diff available for this file.\n".to_owned())
}

/// Return the changes most likely to be included in the next commit.
///
/// When anything is staged, only the staged diff is returned. Otherwise the
/// working-tree diff is combined with untracked files so message generation
/// also works before the first staging step.
pub fn commit_message_diff(work_dir: &Path) -> Result<String, GitError> {
    let staged = command(work_dir, &["diff", "--cached", "--"])?;
    if !staged.trim().is_empty() {
        return Ok(staged);
    }

    command(work_dir, &["add", "--intent-to-add", "--", "."])?;
    let diff = command(work_dir, &["diff", "--no-ext-diff", "--"]);
    let reset = command(work_dir, &["reset", "--quiet", "--", "."]);
    reset?;
    let diff = diff?;
    if diff.trim().is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "no changes available for commit message generation".to_owned(),
        });
    }
    Ok(diff)
}

/// Checks whether the given directory is inside a Git repository.
pub fn is_git_repo(work_dir: &Path) -> bool {
    command(work_dir, &["rev-parse", "--is-inside-work-tree"])
        .map(|out| out.trim() == "true")
        .unwrap_or(false)
}

/// Creates a new Git worktree at `worktree_path` checked out on `branch_name`.
pub fn create_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
) -> Result<(), GitError> {
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| GitError {
            work_dir: repo_path.to_path_buf(),
            message: format!("Failed to create parent directory for worktree: {e}"),
        })?;
    }
    let worktree_str = worktree_path.to_str().ok_or_else(|| GitError {
        work_dir: repo_path.to_path_buf(),
        message: "Invalid worktree path".into(),
    })?;

    // If the branch already exists, attach to it; otherwise create a new branch.
    let branch_exists = command(
        repo_path,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch_name}"),
        ],
    )
    .is_ok();
    if branch_exists {
        command(repo_path, &["worktree", "add", worktree_str, branch_name])?;
    } else {
        command(
            repo_path,
            &["worktree", "add", "-b", branch_name, worktree_str],
        )?;
    }
    Ok(())
}

/// Removes an existing Git worktree.
pub fn remove_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    force: bool,
) -> Result<(), GitError> {
    let worktree_str = worktree_path.to_str().ok_or_else(|| GitError {
        work_dir: repo_path.to_path_buf(),
        message: "Invalid worktree path".into(),
    })?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree_str);
    command(repo_path, &args)?;
    Ok(())
}

/// Lists all worktrees in the repository.
pub fn list_worktrees(repo_path: &Path) -> Result<Vec<GitWorktreeInfo>, GitError> {
    let output = command(repo_path, &["worktree", "list", "--porcelain"])?;
    let mut worktrees = Vec::new();
    let mut current = GitWorktreeInfo::default();
    let mut has_entry = false;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            if has_entry {
                worktrees.push(current);
                current = GitWorktreeInfo::default();
                has_entry = false;
            }
            continue;
        }

        if let Some(path_str) = line.strip_prefix("worktree ") {
            if has_entry {
                worktrees.push(current);
                current = GitWorktreeInfo::default();
            }
            current.path = PathBuf::from(path_str.trim());
            has_entry = true;
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current.head = head.trim().to_string();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            let b = branch.trim();
            current.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        } else if line == "bare" {
            current.is_bare = true;
        } else if line == "detached" {
            current.is_detached = true;
        } else if line.starts_with("locked") {
            current.is_locked = true;
        }
    }

    if has_entry {
        worktrees.push(current);
    }

    Ok(worktrees)
}

fn discover_default_branch(work_dir: &Path) -> Option<String> {
    if let Some(branch) = command(
        work_dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|value| value.trim().strip_prefix("origin/").map(str::to_owned))
    {
        return Some(branch);
    }

    // A freshly cloned repository may not have origin/HEAD configured yet.
    // `remote show -n` reads the locally known remote HEAD without contacting
    // the network and covers repositories whose default branch is not main or
    // master.
    if let Some(branch) = command(work_dir, &["remote", "show", "-n", "origin"])
        .ok()
        .and_then(|output| {
            output.lines().find_map(|line| {
                let branch = line.trim().strip_prefix("HEAD branch: ")?.trim();
                (!branch.is_empty() && branch != "(unknown)").then(|| branch.to_owned())
            })
        })
    {
        return Some(branch);
    }
    for candidate in ["main", "master"] {
        if command(
            work_dir,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/remotes/origin/{candidate}"),
            ],
        )
        .is_ok()
        {
            return Some(candidate.to_owned());
        }
    }
    Some("main".to_owned())
}

fn validate_branch_name(work_dir: &Path, name: &str) -> Result<String, GitError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(GitError {
            work_dir: work_dir.to_path_buf(),
            message: "branch name cannot be empty".to_owned(),
        });
    }
    command(work_dir, &["check-ref-format", "--branch", name])?;
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn run_git(work_dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .env("GIT_CONFIG_GLOBAL", work_dir.join("git-test-global-config"))
            .env("GIT_CONFIG_SYSTEM", work_dir.join("git-test-system-config"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn diff_file_uses_builtin_text_diff_when_external_diff_is_configured() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Threadlane"]);
        fs::write(dir.path().join("tracked.txt"), "original\n").unwrap();
        run_git(dir.path(), &["add", "tracked.txt"]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();

        let helper_path = if cfg!(windows) {
            let path = dir.path().join("external-diff.cmd");
            fs::write(&path, "@echo external-diff-sentinel\r\n").unwrap();
            path
        } else {
            let path = dir.path().join("external-diff.sh");
            fs::write(&path, "#!/bin/sh\necho \"external-diff-sentinel\"\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };
        let helper_arg = helper_path.to_str().unwrap();
        run_git(dir.path(), &["config", "diff.external", helper_arg]);

        let diff = diff_file(dir.path(), "tracked.txt").unwrap();

        assert!(!diff.contains("external-diff-sentinel"));
        assert!(diff.contains("-original"));
        assert!(diff.contains("+changed"));
    }

    #[test]
    fn diff_file_does_not_turn_clean_tracked_files_into_new_files() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Threadlane"]);
        fs::write(dir.path().join("tracked.txt"), "unchanged\n").unwrap();
        run_git(dir.path(), &["add", "tracked.txt"]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);

        let diff = diff_file(dir.path(), "tracked.txt").unwrap();

        assert_eq!(diff, "No textual diff available for this file.\n");
    }

    #[test]
    fn diff_file_rejects_paths_outside_workspace() {
        let dir = tempdir().unwrap();

        let error = diff_file(dir.path(), "../outside.txt").unwrap_err();

        assert!(error.message.contains("outside the workspace"));
    }

    #[test]
    fn diff_file_preserves_git_errors() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), "content\n").unwrap();

        let error = diff_file(dir.path(), "file.txt").unwrap_err();

        assert!(!error.message.is_empty());
    }

    #[test]
    fn parses_branch_and_change_state() {
        let status = parse_status(
            Path::new("/tmp/project"),
            "## feature/demo...origin/feature/demo [ahead 2, behind 1]\nM  staged.rs\n M working.rs\nMM mixed.rs\n?? new.rs\n",
        );
        assert_eq!(status.branch.as_deref(), Some("feature/demo"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert!(status.has_upstream);
        assert!(status.staged_changes);
        assert!(status.unstaged_changes);
        assert!(status.has_changes);
        let mixed = status
            .files
            .iter()
            .find(|file| file.path == "mixed.rs")
            .unwrap();
        assert_eq!(mixed.status, "MM");
        assert_eq!(mixed.status_for_section(true), 'M');
        assert_eq!(mixed.status_for_section(false), 'M');
        assert!(mixed.staged);
        assert!(mixed.unstaged);
    }

    #[test]
    fn cache_entry_expires_after_ttl() {
        let started = std::time::Instant::now();
        let fresh = (started, "cached".to_string());

        assert_eq!(
            fresh_cache_value(&fresh, started, std::time::Duration::from_secs(30)),
            Some("cached".to_string())
        );
        assert_eq!(
            fresh_cache_value(
                &fresh,
                started + std::time::Duration::from_secs(31),
                std::time::Duration::from_secs(30),
            ),
            None
        );
    }

    #[test]
    fn pr_cache_separates_branches_in_the_same_repository() {
        let repository = Path::new("/tmp/project");

        assert_ne!(
            pr_cache_key(repository, "feature/one"),
            pr_cache_key(repository, "feature/two")
        );
    }

    #[test]
    fn parses_detached_head() {
        let status = parse_status(Path::new("/tmp/project"), "## HEAD\n");
        assert!(status.detached);
        assert!(status.branch.is_none());
    }

    #[test]
    fn normalizes_renamed_paths() {
        let status = parse_status(
            Path::new("/tmp/project"),
            "## main\nR  old_name.rs -> new_name.rs\n",
        );
        assert_eq!(status.files[0].path, "new_name.rs");
        assert_eq!(status.files[0].status, "R");
    }

    #[test]
    fn parses_nul_delimited_paths_and_renames() {
        let status = parse_status(
            Path::new("/tmp/project"),
            "## feature/demo\0?? line\nbreak.txt\0R  new name.txt\0old name.txt\0",
        );
        assert_eq!(status.files.len(), 2);
        assert_eq!(status.files[0].path, "line\nbreak.txt");
        assert_eq!(status.files[1].path, "new name.txt");
        assert_eq!(status.files[1].status, "R");
    }

    #[test]
    fn preserves_leading_and_trailing_whitespace_in_nul_paths() {
        let status = parse_status(
            Path::new("/tmp/project"),
            "## feature/demo\0??  leading.txt \0",
        );
        assert_eq!(status.files[0].path, " leading.txt ");
    }

    #[test]
    fn atomic_commit_groups_exclude_locks_and_order_sources_first() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        std::fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "lock\n").unwrap();
        let groups = atomic_commit_groups(dir.path()).unwrap();
        assert_eq!(groups, vec![vec!["src.rs".to_string()]]);
    }

    #[test]
    fn atomic_commit_execution_creates_one_commit_per_group() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("initial.txt"), "initial\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        std::fs::write(dir.path().join("first.rs"), "fn first() {}\n").unwrap();
        std::fs::write(dir.path().join("second.rs"), "fn second() {}\n").unwrap();
        let groups = commit_atomic_groups(dir.path(), "atomic changes").unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(
            command(dir.path(), &["rev-list", "--count", "HEAD"])
                .unwrap()
                .trim(),
            "3"
        );
        assert!(!inspect(dir.path()).unwrap().has_changes);
    }

    #[test]
    fn commit_message_diff_prefers_staged_changes_and_includes_untracked_files() {
        let root = std::env::temp_dir().join(format!(
            "threadlane-git-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.email", "threadlane@example.com"]);
        run_git(&root, &["config", "user.name", "Threadlane"]);
        fs::write(root.join("tracked.txt"), "original\n").unwrap();
        run_git(&root, &["add", "tracked.txt"]);
        run_git(&root, &["commit", "-qm", "initial"]);

        fs::write(root.join("tracked.txt"), "staged\n").unwrap();
        run_git(&root, &["add", "tracked.txt"]);
        fs::write(root.join("tracked.txt"), "staged\nunstaged\n").unwrap();
        fs::write(root.join("new.txt"), "new file\n").unwrap();

        let staged = commit_message_diff(&root).unwrap();
        assert!(staged.contains("+staged"));
        assert!(!staged.contains("+unstaged"));
        assert!(!staged.contains("new.txt"));

        run_git(&root, &["restore", "--staged", "tracked.txt"]);
        let working_tree = commit_message_diff(&root).unwrap();
        assert!(working_tree.contains("+unstaged"));
        assert!(working_tree.contains("new.txt"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn commit_message_diff_process_count_is_constant_for_untracked_files() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Threadlane"]);
        fs::write(dir.path().join("tracked.txt"), "initial\n").unwrap();
        run_git(dir.path(), &["add", "tracked.txt"]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        for index in 0..8 {
            fs::write(dir.path().join(format!("new-{index}.txt")), "new\n").unwrap();
        }

        COMMAND_SPAWNS.set(0);
        let diff = commit_message_diff(dir.path()).unwrap();
        let spawn_count = COMMAND_SPAWNS.get();

        assert!(diff.contains("new-0.txt"));
        assert!(diff.contains("new-7.txt"));
        assert_eq!(spawn_count, 4);
    }

    #[test]
    fn commit_message_diff_includes_untracked_files_before_first_commit() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();

        let diff = commit_message_diff(dir.path()).unwrap();

        assert!(diff.contains("new.txt"));
        assert!(diff.contains("+new"));
    }

    #[test]
    fn parses_github_pr_json_with_checks_and_comments() {
        let sample = r#"{
            "number": 42,
            "title": "Center editor panel",
            "url": "https://github.com/threadlane/threadlane/pull/42",
            "state": "OPEN",
            "headRefName": "center_editor_panel",
            "baseRefName": "main",
            "comments": [
                {
                    "author": { "login": "reviewer1" },
                    "body": "Please double check the layout.",
                    "createdAt": "2026-08-19T00:00:00Z",
                    "path": "src/screens/editor/view.rs",
                    "line": 45
                },
                {
                    "author": { "login": "reviewer2" },
                    "body": "Looks great overall!",
                    "createdAt": "2026-08-19T00:05:00Z"
                },
                {
                    "author": { "login": "bot" },
                    "body": "Benchmark passed.",
                    "createdAt": "2026-08-19T00:10:00Z"
                }
            ],
            "statusCheckRollup": [
                {
                    "name": "cargo-test",
                    "status": "COMPLETED",
                    "conclusion": "FAILURE",
                    "detailsUrl": "https://github.com/threadlane/threadlane/actions/runs/1"
                },
                {
                    "name": "cargo-check",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "detailsUrl": "https://github.com/threadlane/threadlane/actions/runs/2"
                },
                {
                    "name": "e2e-tests",
                    "status": "IN_PROGRESS",
                    "conclusion": null,
                    "detailsUrl": "https://github.com/threadlane/threadlane/actions/runs/3"
                }
            ]
        }"#;

        let pr = parse_gh_pr_json(sample).unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Center editor panel");
        assert_eq!(pr.head_ref, "center_editor_panel");
        assert_eq!(pr.base_ref, "main");
        assert!(!pr.is_draft);
        assert_eq!(pr.comments_count, 3);
        assert_eq!(pr.review_comments.len(), 3);
        assert_eq!(pr.review_comments[0].author, "reviewer1");
        assert_eq!(pr.total_checks, 3);
        assert_eq!(pr.failing_checks, 1);
        assert_eq!(pr.passing_checks, 1);
        assert_eq!(pr.pending_checks, 1);

        let draft_sample = r#"{
            "number": 43,
            "title": "WIP Feature",
            "url": "https://github.com/threadlane/threadlane/pull/43",
            "state": "OPEN",
            "isDraft": true,
            "headRefName": "wip-feature",
            "baseRefName": "main",
            "comments": [],
            "statusCheckRollup": []
        }"#;
        let draft_pr = parse_gh_pr_json(draft_sample).unwrap();
        assert!(draft_pr.is_draft);
        assert_eq!(draft_pr.state, "OPEN");

        let merged_sample = r#"{
            "number": 44,
            "title": "Merged Feature",
            "url": "https://github.com/threadlane/threadlane/pull/44",
            "state": "MERGED",
            "isDraft": false,
            "headRefName": "merged-feature",
            "baseRefName": "main",
            "comments": [],
            "statusCheckRollup": []
        }"#;
        let merged_pr = parse_gh_pr_json(merged_sample).unwrap();
        assert!(!merged_pr.is_draft);
        assert_eq!(merged_pr.state, "MERGED");
    }

    #[test]
    fn branch_lifecycle_and_merge() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial commit"]);

        // Create feature branch
        create_branch(dir.path(), "feature-1").unwrap();
        let status = inspect(dir.path()).unwrap();
        assert_eq!(status.branch.as_deref(), Some("feature-1"));

        // Commit on feature branch
        fs::write(dir.path().join("feature.txt"), "feature content\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "add feature"]);

        // Detailed branches
        let branches = list_branches_detailed(dir.path(), None).unwrap();
        assert!(branches
            .iter()
            .any(|b| b.name == "feature-1" && b.is_current));
        assert!(branches.iter().any(|b| b.name == "main"));

        // Switch back to main
        checkout(dir.path(), "main").unwrap();
        let status = inspect(dir.path()).unwrap();
        assert_eq!(status.branch.as_deref(), Some("main"));

        // Merge feature-1 into main
        merge(dir.path(), "feature-1").unwrap();
        assert!(dir.path().join("feature.txt").exists());
    }

    #[test]
    fn switch_branch_with_stash_and_carry() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial commit"]);

        create_branch(dir.path(), "branch-a").unwrap();
        create_branch(dir.path(), "branch-b").unwrap();

        // Switch to branch-a and create uncommitted change
        checkout(dir.path(), "branch-a").unwrap();
        fs::write(dir.path().join("dirty.txt"), "dirty work\n").unwrap();
        assert!(!inspect(dir.path()).unwrap().files.is_empty());

        // Stash and switch to branch-b
        checkout_with_stash(dir.path(), "branch-b").unwrap();
        let status_b = inspect(dir.path()).unwrap();
        assert_eq!(status_b.branch.as_deref(), Some("branch-b"));
        // Dirty file should have been stashed
        assert!(status_b.files.is_empty());

        // Switch carrying changes test
        fs::write(dir.path().join("carry.txt"), "carry me\n").unwrap();
        assert!(!inspect(dir.path()).unwrap().files.is_empty());
        checkout_carrying_changes(dir.path(), "main").unwrap();
        let status_main = inspect(dir.path()).unwrap();
        assert_eq!(status_main.branch.as_deref(), Some("main"));
        assert!(dir.path().join("carry.txt").exists());
    }

    #[test]
    fn pull_request_creation_rejects_detached_head() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial commit"]);
        run_git(dir.path(), &["checkout", "--detach", "HEAD"]);

        let error = create_pull_request(dir.path()).unwrap_err();
        assert!(error.message.contains("detached HEAD"));
    }

    #[test]
    fn stash_pop_and_drop_lifecycle() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial commit"]);

        create_branch(dir.path(), "feature").unwrap();
        checkout(dir.path(), "feature").unwrap();

        fs::write(dir.path().join("work.txt"), "in-progress work\n").unwrap();
        checkout_with_stash(dir.path(), "main").unwrap();

        // Switch back to feature branch
        checkout(dir.path(), "feature").unwrap();
        let status = inspect(dir.path()).unwrap();
        assert!(status.current_stash.is_some());
        let current = status.current_stash.as_ref().unwrap();
        assert!(current.files.is_empty());
        let files = inspect_stash_files(dir.path(), current.index);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "work.txt");

        let diff = diff_stash_file(dir.path(), 0, "work.txt").unwrap();
        assert!(diff.contains("in-progress work"));

        fs::write(dir.path().join("second.txt"), "second stash\n").unwrap();
        run_git(dir.path(), &["stash", "push", "-u", "-m", "second stash"]);
        assert_eq!(list_stashes(dir.path()).unwrap().len(), 2);
        drop_stash(dir.path(), Some(0)).unwrap();
        assert_eq!(list_stashes(dir.path()).unwrap().len(), 1);

        pop_stash(dir.path(), Some(0)).unwrap();
        assert!(dir.path().join("work.txt").exists());
        let status_after_pop = inspect(dir.path()).unwrap();
        assert_eq!(status_after_pop.stashes.len(), 0);

        fs::write(dir.path().join("third.txt"), "third stash\n").unwrap();
        run_git(dir.path(), &["stash", "push", "-u", "-m", "third stash"]);
        pop_stash(dir.path(), None).unwrap();
        assert!(dir.path().join("third.txt").exists());
        assert!(inspect(dir.path()).unwrap().stashes.is_empty());
    }

    #[test]
    fn inspect_does_not_expose_a_stash_from_another_branch() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        create_branch(dir.path(), "feature").unwrap();
        fs::write(dir.path().join("feature.txt"), "work\n").unwrap();
        checkout_with_stash(dir.path(), "main").unwrap();

        let status = inspect(dir.path()).unwrap();

        assert!(status.current_stash.is_none());
        assert_eq!(status.stashes.len(), 1);
    }

    #[test]
    fn git_metadata_parsing_preserves_pipe_characters() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test | Author"]);
        fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "subject | detail"]);
        run_git(dir.path(), &["branch", "topic|branch"]);

        let commits = list_commits(dir.path(), 1).unwrap();
        let branches = list_branches_detailed(dir.path(), None).unwrap();

        assert_eq!(commits[0].author_name, "Test | Author");
        assert_eq!(commits[0].summary, "subject | detail");
        assert!(branches.iter().any(|branch| branch.name == "topic|branch"));
    }

    #[test]
    fn stash_metadata_parsing_preserves_pipe_characters() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        fs::write(dir.path().join("work.txt"), "work\n").unwrap();
        run_git(
            dir.path(),
            &["stash", "push", "-u", "-m", "message | detail"],
        );

        let stashes = list_stashes(dir.path()).unwrap();

        assert_eq!(stashes.len(), 1);
        assert!(stashes[0].message.ends_with("message | detail"));
    }

    #[test]
    fn commit_file_inspection_uses_the_destination_of_a_rename() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("old.txt"), "contents\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial"]);
        run_git(dir.path(), &["mv", "old.txt", "new.txt"]);
        run_git(dir.path(), &["commit", "-qm", "rename file"]);
        let sha = command(dir.path(), &["rev-parse", "HEAD"]).unwrap();

        let files = inspect_commit_files(dir.path(), sha.trim());

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new.txt");
        assert_eq!(files[0].status_char(), 'R');
    }

    #[test]
    fn file_mutations_reject_paths_outside_the_workspace() {
        let dir = tempdir().unwrap();

        assert!(discard_file_changes(dir.path(), "../outside.txt").is_err());
        assert!(ignore_file(dir.path(), "../outside.txt").is_err());
    }

    #[test]
    fn file_discard_and_ignore_lifecycle() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        fs::write(dir.path().join("file1.txt"), "original\n").unwrap();
        fs::write(dir.path().join("file2.md"), "markdown\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial commit"]);

        // 1. Modify tracked file and discard
        fs::write(dir.path().join("file1.txt"), "modified\n").unwrap();
        assert_eq!(inspect(dir.path()).unwrap().files.len(), 1);
        discard_file_changes(dir.path(), "file1.txt").unwrap();
        assert_eq!(inspect(dir.path()).unwrap().files.len(), 0);
        assert_eq!(
            fs::read_to_string(dir.path().join("file1.txt")).unwrap(),
            "original\n"
        );

        // 2. Create untracked file and discard
        fs::write(dir.path().join("untracked.rs"), "fn main() {}\n").unwrap();
        assert_eq!(inspect(dir.path()).unwrap().files.len(), 1);
        discard_file_changes(dir.path(), "untracked.rs").unwrap();
        assert!(!dir.path().join("untracked.rs").exists());
        assert_eq!(inspect(dir.path()).unwrap().files.len(), 0);

        // 3. Ignore file
        fs::write(dir.path().join("secret.env"), "KEY=123\n").unwrap();
        assert_eq!(inspect(dir.path()).unwrap().files.len(), 1);
        ignore_file(dir.path(), "secret.env").unwrap();
        assert!(dir.path().join(".gitignore").exists());
        let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains("/secret.env"));

        // 4. Ignore extension
        ignore_extension(dir.path(), "log").unwrap();
        let gitignore2 = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gitignore2.contains("*.log"));
    }

    #[test]
    fn commit_history_inspection() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "dev@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Dev"]);
        fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "feat: initial commit"]);

        fs::write(dir.path().join("b.txt"), "second file\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "feat: second commit"]);

        let commits = list_commits(dir.path(), 10).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].summary, "feat: second commit");
        assert_eq!(commits[0].author_name, "Dev");
        assert_eq!(commits[1].summary, "feat: initial commit");

        let files = inspect_commit_files(dir.path(), &commits[0].sha);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "b.txt");

        let diff = diff_commit_file(dir.path(), &commits[0].sha, "b.txt").unwrap();
        assert!(diff.contains("second file"));
    }

    #[test]
    fn worktree_lifecycle_and_listing() {
        let dir = tempdir().unwrap();
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "dev@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Dev"]);
        fs::write(dir.path().join("base.txt"), "hello worktree\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-qm", "initial commit"]);

        assert!(is_git_repo(dir.path()));

        let worktree_dir = dir.path().join(".threadlane/worktrees/task_1");
        create_worktree(dir.path(), &worktree_dir, "worktree/task_1").unwrap();
        assert!(worktree_dir.join("base.txt").exists());

        let worktrees = list_worktrees(dir.path()).unwrap();
        assert!(worktrees.len() >= 2);
        assert!(worktrees
            .iter()
            .any(|wt| wt.branch.as_deref() == Some("worktree/task_1")));

        // Write changes in worktree
        fs::write(worktree_dir.join("new_feature.txt"), "isolated feature\n").unwrap();
        assert!(!dir.path().join("new_feature.txt").exists());

        remove_worktree(dir.path(), &worktree_dir, true).unwrap();
        assert!(!worktree_dir.exists());
    }
}
