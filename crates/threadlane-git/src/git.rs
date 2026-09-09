use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::GitError;
use crate::github::{fresh_cache_value, inspect_pr, invalidate_pr_cache, repository_key};
use crate::types::{
    GitBranchInfo, GitCommitInfo, GitFile, GitStashInfo, GitStatus, GIT_FIELD_SEPARATOR,
};
#[cfg(test)]
use crate::types::GitWorktreeInfo;

#[cfg(test)]
thread_local! {
    pub(crate) static COMMAND_SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const REPOSITORY_METADATA_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct RepositoryMetadata {
    remote: Option<String>,
    default_branch: Option<String>,
}

type TimedCache<T> = HashMap<PathBuf, (Instant, T)>;

static REPOSITORY_METADATA_CACHE: OnceLock<Mutex<TimedCache<RepositoryMetadata>>> = OnceLock::new();

pub(crate) fn command(work_dir: &Path, args: &[&str]) -> Result<String, GitError> {
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
            .map_err(|error| GitError::new(
                work_dir,
                format!("could not start git: {error}"),
            ))?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let is_lock_error = stderr.contains("index.lock") || stderr.contains("Unable to create");
        if is_lock_error && attempts <= 5 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        return Err(GitError::new(
            work_dir,
            if stderr.is_empty() {
                format!("git exited with {}", output.status)
            } else {
                stderr
            },
        ));
    }
}

pub(crate) fn parse_status(_work_dir: &Path, porcelain: &str) -> GitStatus {
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

#[cfg(test)]
pub(crate) fn inspect_files(work_dir: &Path) -> Result<Vec<GitFile>, GitError> {
    let porcelain = command(work_dir, &["status", "--porcelain=v1", "-b", "-z"])?;
    let mut status = parse_status(work_dir, &porcelain);
    apply_numstats(work_dir, &mut status);
    Ok(status.files)
}

pub(crate) fn apply_numstats(work_dir: &Path, status: &mut GitStatus) {
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

pub(crate) fn list_branches_detailed(
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

pub(crate) fn list_stashes(work_dir: &Path) -> Result<Vec<GitStashInfo>, GitError> {
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
        std::fs::write(&gitignore_path, current_content).map_err(|e| GitError::new(
            work_dir,
            format!("Failed to update .gitignore: {e}"),
        ))?;
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
        std::fs::write(&gitignore_path, current_content).map_err(|e| GitError::new(
            work_dir,
            format!("Failed to update .gitignore: {e}"),
        ))?;
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

pub fn current_branch(work_dir: &Path) -> Result<Option<String>, GitError> {
    let branch = command(work_dir, &["branch", "--show-current"])?;
    Ok((!branch.trim().is_empty()).then(|| branch.trim().to_owned()))
}

pub fn create_branch(work_dir: &Path, name: &str) -> Result<(), GitError> {
    create_branch_from(work_dir, name, None)
}

fn create_branch_from(
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

fn normalize_branch_for_checkout(name: &str) -> &str {
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
#[cfg(test)]
pub(crate) fn atomic_commit_groups(work_dir: &Path) -> Result<Vec<Vec<String>>, GitError> {
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
#[cfg(test)]
pub(crate) fn commit_atomic_groups(
    work_dir: &Path,
    message_prefix: &str,
) -> Result<Vec<Vec<String>>, GitError> {
    let groups = atomic_commit_groups(work_dir)?;
    if groups.is_empty() {
        return Ok(groups);
    }
    let prefix = message_prefix.trim();
    if prefix.is_empty() {
        return Err(GitError::new(
            work_dir,
            "commit message prefix cannot be empty",
        ));
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
        return Err(GitError::new(
            work_dir,
            "commit message cannot be empty".to_owned(),
        ));
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
        return Err(GitError::new(
            work_dir,
            "cannot push a detached HEAD; check out a named branch first".to_owned(),
        ));
    }
    command(work_dir, &["push", "--set-upstream", "origin", branch])?;
    Ok(())
}

pub fn pull(work_dir: &Path) -> Result<String, GitError> {
    command(work_dir, &["pull", "--ff-only"])
}

pub fn merge(work_dir: &Path, branch: &str) -> Result<String, GitError> {
    let branch = branch.trim();
    if branch.is_empty() {
        return Err(GitError::new(
            work_dir,
            "branch name to merge cannot be empty".to_owned(),
        ));
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

pub fn stage_all(work_dir: &Path) -> Result<(), GitError> {
    command(work_dir, &["add", "-A"])?;
    Ok(())
}

pub fn unstage_all(work_dir: &Path) -> Result<(), GitError> {
    command(work_dir, &["restore", "--staged", "."])?;
    Ok(())
}

pub(crate) fn validate_diff_path(work_dir: &Path, path: &str) -> Result<(), GitError> {
    let invalid = || GitError::new(
        work_dir,
        format!("path is outside the workspace: {path}"),
    );
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(invalid());
    }

    let root = work_dir.canonicalize().map_err(|error| GitError::new(
        work_dir,
        format!("could not resolve workspace: {error}"),
    ))?;
    let mut existing = work_dir.join(relative);
    while !existing.exists() {
        if !existing.pop() {
            return Err(invalid());
        }
    }
    if !existing
        .canonicalize()
        .map_err(|error| GitError::new(
            work_dir,
            format!("could not resolve path: {error}"),
        ))?
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
        return Err(GitError::new(
            work_dir,
            "no changes available for commit message generation".to_owned(),
        ));
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
        std::fs::create_dir_all(parent).map_err(|e| GitError::new(
            repo_path,
            format!("Failed to create parent directory for worktree: {e}"),
        ))?;
    }
    let worktree_str = worktree_path.to_str().ok_or_else(|| GitError::new(
        repo_path,
        "Invalid worktree path",
    ))?;

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
    let worktree_str = worktree_path.to_str().ok_or_else(|| GitError::new(
        repo_path,
        "Invalid worktree path",
    ))?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree_str);
    command(repo_path, &args)?;
    Ok(())
}

/// Lists all worktrees in the repository.
#[cfg(test)]
pub(crate) fn list_worktrees(repo_path: &Path) -> Result<Vec<GitWorktreeInfo>, GitError> {
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
        return Err(GitError::new(
            work_dir,
            "branch name cannot be empty".to_owned(),
        ));
    }
    command(work_dir, &["check-ref-format", "--branch", name])?;
    Ok(name.to_owned())
}
