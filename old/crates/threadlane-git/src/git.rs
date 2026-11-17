use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::GitError;
use crate::github::{fresh_cache_value, inspect_pr, repository_key};
use crate::types::{
    DiffOptions, GitBranchInfo, GitCommitInfo, GitFile, GitFileInventory, GitStashInfo,
    GitStatus, GitWorktreeInfo, GIT_FIELD_SEPARATOR, GIT_RECORD_SEPARATOR,
};

#[cfg(test)]
thread_local! {
    pub(crate) static COMMAND_SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static BRANCH_LIST_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static REMOTE_FETCH_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const REPOSITORY_METADATA_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct RepositoryMetadata {
    remote: Option<String>,
    default_branch: Option<String>,
}

type TimedCache<T> = HashMap<PathBuf, (Instant, T)>;

static REPOSITORY_METADATA_CACHE: OnceLock<Mutex<TimedCache<RepositoryMetadata>>> = OnceLock::new();

pub(crate) fn command_bytes(work_dir: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        #[cfg(test)]
        COMMAND_SPAWNS.set(COMMAND_SPAWNS.get() + 1);
        #[cfg(test)]
        if args.first() == Some(&"for-each-ref") {
            BRANCH_LIST_RUNS.set(BRANCH_LIST_RUNS.get() + 1);
        }
        let output = Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| GitError::new(work_dir, format!("could not start git: {error}")))?;

        if output.status.success() {
            return Ok(output.stdout);
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

pub(crate) fn command(work_dir: &Path, args: &[&str]) -> Result<String, GitError> {
    command_bytes(work_dir, args).map(|stdout| String::from_utf8_lossy(&stdout).into_owned())
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
        let orig_path = if index == 'R' || index == 'C' || worktree == 'R' || worktree == 'C' {
            if porcelain.contains('\0') {
                records.next()
            } else {
                raw_path
                    .split_once(" -> ")
                    .map(|(old_p, _)| old_p.trim().to_string())
            }
        } else {
            None
        };
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
                orig_path,
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

/// Inspect changed files and line counts in the current worktree.
pub fn inspect_files(work_dir: &Path) -> Result<Vec<GitFile>, GitError> {
    let porcelain = command(
        work_dir,
        &[
            "status",
            "--porcelain=v1",
            "-b",
            "-z",
            "--untracked-files=all",
        ],
    )?;
    let mut status = parse_status(work_dir, &porcelain);
    apply_numstats(work_dir, &mut status);
    Ok(status.files)
}

fn apply_numstats(work_dir: &Path, status: &mut GitStatus) {
    if status.files.is_empty() {
        return;
    }
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

/// Background remote syncs share one fetch per repository per window.
/// Every refresh path (project switches, panel opens, watcher-driven
/// reloads) funnels here, so a burst of triggers costs one remote
/// round-trip instead of one per caller.
const REMOTE_SYNC_TTL: Duration = Duration::from_secs(60);

/// Fetch bookkeeping for one repository. Linked worktrees share
/// `refs/remotes/*`, so they share a slot too.
#[derive(Default)]
struct RemoteFetchSlot {
    state: Mutex<RemoteFetchState>,
    changed: Condvar,
}

#[derive(Default)]
struct RemoteFetchState {
    in_flight: bool,
    /// Callers blocked on `changed`, observable to tests.
    waiters: usize,
    /// The completed run's outcome; errors reach queued callers but are
    /// never freshness-stamped, so a failed fetch does not suppress retries.
    last_completed: Option<(Instant, Result<(), String>)>,
}

static REMOTE_FETCH_SLOTS: OnceLock<Mutex<HashMap<(PathBuf, String), Arc<RemoteFetchSlot>>>> =
    OnceLock::new();

/// A bare `git fetch` reads `branch.<name>.remote`, falling back to
/// `origin`. Sibling worktrees on branches tracking different remotes must
/// not share a slot, or one fetch satisfies callers expecting the other.
fn fetch_remote_name(work_dir: &Path) -> String {
    if let Ok(branch) = command(work_dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        let key = format!("branch.{}.remote", branch.trim());
        if let Ok(remote) = command(work_dir, &["config", "--get", &key]) {
            let remote = remote.trim();
            if !remote.is_empty() {
                return remote.to_owned();
            }
        }
    }
    "origin".to_owned()
}

fn remote_fetch_slot(work_dir: &Path) -> Arc<RemoteFetchSlot> {
    // `--git-common-dir` resolves to the shared refs dir for linked
    // worktrees, so callers in sibling worktrees land on one slot.
    let dir = command(work_dir, &["rev-parse", "--git-common-dir"])
        .ok()
        .map(|output| {
            let dir = output.trim();
            let path = if Path::new(dir).is_absolute() {
                PathBuf::from(dir)
            } else {
                work_dir.join(dir)
            };
            path.canonicalize().unwrap_or(path)
        })
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| repository_key(work_dir));
    let key = (dir, fetch_remote_name(work_dir));
    REMOTE_FETCH_SLOTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .entry(key)
        .or_default()
        .clone()
}

fn run_remote_fetch(
    work_dir: &Path,
    slot: &RemoteFetchSlot,
    ttl: Duration,
    fetch_op: impl FnOnce(&Path) -> Result<(), GitError>,
) -> Result<(), GitError> {
    {
        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if state.in_flight {
                state.waiters += 1;
                state = slot.changed.wait(state).unwrap_or_else(|e| e.into_inner());
                state.waiters -= 1;
                if !state.in_flight {
                    // The run just completed is fresh by definition: its
                    // result satisfies a queued caller on any window.
                    if let Some((_, result)) = &state.last_completed {
                        return result
                            .clone()
                            .map_err(|message| GitError::new(work_dir, message));
                    }
                }
                continue;
            }
            if let Some((fetched_at, result)) = &state.last_completed {
                if result.is_ok() && fetched_at.elapsed() <= ttl {
                    return result
                        .clone()
                        .map_err(|message| GitError::new(work_dir, message));
                }
            }
            state.in_flight = true;
            break;
        }
    }
    let outcome = fetch_op(work_dir);
    let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
    state.in_flight = false;
    state.last_completed = Some((
        Instant::now(),
        outcome.clone().map_err(|error| error.message.clone()),
    ));
    slot.changed.notify_all();
    outcome
}

fn git_fetch(work_dir: &Path) -> Result<(), GitError> {
    #[cfg(test)]
    REMOTE_FETCH_RUNS.set(REMOTE_FETCH_RUNS.get() + 1);
    command(work_dir, &["fetch", "--prune", "--quiet"])?;
    Ok(())
}

/// `fetch --prune` when the repository's last successful fetch is older
/// than [`REMOTE_SYNC_TTL`]; a fresher run or one already in flight
/// answers the caller without another remote round-trip.
pub fn sync_remote(work_dir: &Path) -> Result<(), GitError> {
    let slot = remote_fetch_slot(work_dir);
    run_remote_fetch(work_dir, &slot, REMOTE_SYNC_TTL, git_fetch)
}

/// Explicit fetch (the panel's Fetch action): always contacts the remote
/// but still coalesces with a run already in flight instead of racing it
/// on `.git` locks.
pub fn fetch(work_dir: &Path) -> Result<(), GitError> {
    let slot = remote_fetch_slot(work_dir);
    run_remote_fetch(work_dir, &slot, Duration::ZERO, git_fetch)
}

/// A completed pull already refreshed remote-tracking refs; let the next
/// background sync skip its fetch.
fn note_remote_fetched(work_dir: &Path) {
    let slot = remote_fetch_slot(work_dir);
    slot.state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .last_completed = Some((Instant::now(), Ok(())));
}

#[cfg(test)]
mod fetch_tests {
    use super::{run_remote_fetch, GitError, RemoteFetchSlot};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    #[test]
    fn queued_callers_share_the_in_flight_run() {
        let slot = RemoteFetchSlot::default();
        let runs = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            let leader = {
                let runs = runs.clone();
                let entered = entered.clone();
                let release = release.clone();
                let slot = &slot;
                scope.spawn(move || {
                    run_remote_fetch(Path::new("/repo"), slot, Duration::ZERO, move |_| {
                        runs.fetch_add(1, Ordering::SeqCst);
                        entered.wait();
                        release.wait();
                        Ok(())
                    })
                })
            };
            entered.wait();
            let waiters: Vec<_> = (0..4)
                .map(|_| {
                    let slot = &slot;
                    scope.spawn(move || {
                        run_remote_fetch(Path::new("/repo"), slot, Duration::ZERO, |_| {
                            Err(GitError::new("/repo", "must not run"))
                        })
                    })
                })
                .collect();
            // Release the leader only once every waiter is blocked on the
            // condvar, so a descheduled waiter can't restart a fresh run.
            while slot.state.lock().unwrap().waiters < waiters.len() {
                std::thread::yield_now();
            }
            release.wait();
            for waiter in waiters {
                waiter.join().unwrap().unwrap();
            }
            leader.join().unwrap().unwrap();
        });
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }
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
            "--format=%(refname:short)\x1f%(committerdate:relative)\x1f%(committerdate:unix)\x1f%(upstream:short)\x1f%(HEAD)\x1f%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
    )?;

    let mut branches = Vec::new();
    let mut seen_refs = HashSet::new();

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
        let full_name = parts.get(5).copied().unwrap_or_default();
        if full_name.is_empty() || full_name == "refs/remotes/origin/HEAD" {
            continue;
        }

        let is_remote = full_name.starts_with("refs/remotes/");
        let name = if is_remote {
            ref_name
        } else {
            full_name.strip_prefix("refs/heads/").unwrap_or(ref_name)
        };
        if name.is_empty() {
            continue;
        }
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
            full_name == format!("refs/heads/{db}")
                || full_name == format!("refs/remotes/origin/{db}")
        });

        if seen_refs.insert(full_name) {
            branches.push(GitBranchInfo {
                name: name.to_string(),
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
    // Expand new directories so file-level review and line totals include their contents.
    let porcelain = command(
        work_dir,
        &[
            "status",
            "--porcelain=v1",
            "-b",
            "-z",
            "--untracked-files=all",
        ],
    )?;
    let mut status = parse_status(work_dir, &porcelain);
    apply_numstats(work_dir, &mut status);
    let metadata = repository_metadata(work_dir);
    status.default_branch = metadata.default_branch.clone();
    let branch_details = list_branches_detailed(work_dir, status.default_branch.as_deref());
    status.branches = match &branch_details {
        Ok(branches) => {
            let mut names: Vec<_> = branches
                .iter()
                .filter(|branch| !branch.is_remote)
                .map(|branch| branch.name.clone())
                .collect();
            names.sort();
            names
        }
        Err(_) => command(
            work_dir,
            &[
                "for-each-ref",
                "--format=%(refname:lstrip=2)",
                "refs/heads",
            ],
        )?
        .lines()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
        .collect(),
    };
    if let Some(current_branch) = status.branch.as_ref() {
        if !status
            .branches
            .iter()
            .any(|branch| branch == current_branch)
        {
            status.branches.push(current_branch.clone());
        }
    }
    status.branch_details = branch_details.unwrap_or_else(|_| {
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
                orig_path: None,
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
pub fn stash_push(
    work_dir: &Path,
    message: Option<&str>,
    include_untracked: bool,
) -> Result<(), GitError> {
    let mut args = vec!["stash", "push"];
    if include_untracked {
        args.push("-u");
    }
    let msg_str;
    if let Some(msg) = message.map(str::trim).filter(|s| !s.is_empty()) {
        msg_str = msg.to_string();
        args.push("-m");
        args.push(&msg_str);
    }
    command(work_dir, &args)?;
    Ok(())
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
            "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%cr%x1f%ct%x1f%s%x1f%b%x1e",
        ],
    ) {
        Ok(out) => out,
        Err(_) => return Ok(Vec::new()),
    };

    let mut commits = Vec::new();
    for record in output.split(GIT_RECORD_SEPARATOR) {
        let record = record.trim();
        if record.is_empty() {
            continue;
        }
        let parts: Vec<&str> = record.split(GIT_FIELD_SEPARATOR).collect();
        if parts.len() >= 7 {
            let sha = parts[0].trim().to_string();
            let short_sha = parts[1].trim().to_string();
            let author_name = parts[2].trim().to_string();
            let author_email = parts[3].trim().to_string();
            let relative_time = parts[4].trim().to_string();
            let timestamp = parts[5].trim().parse::<i64>().unwrap_or(0);
            let summary = parts[6].trim().to_string();
            let body = if parts.len() > 7 {
                parts[7].trim().to_string()
            } else {
                String::new()
            };

            commits.push(GitCommitInfo {
                sha,
                short_sha,
                summary,
                body,
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
    let mut rename_sources = std::collections::HashMap::new();
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
                    rename_sources.insert(destination.clone(), source);
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
                orig_path: rename_sources.get(&path).cloned(),
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

fn find_rename_source(work_dir: &Path, destination_path: &str) -> Option<String> {
    let porcelain = command(work_dir, &["status", "--porcelain=v1", "-z"]).ok()?;
    let mut records = porcelain.split('\0').filter(|r| !r.is_empty());
    while let Some(line) = records.next() {
        let bytes = line.as_bytes();
        if bytes.len() < 3 {
            continue;
        }
        let index = bytes[0] as char;
        let worktree = bytes[1] as char;
        let is_rename = index == 'R' || index == 'C' || worktree == 'R' || worktree == 'C';
        let path = line.get(3..).unwrap_or_default();
        if is_rename {
            let old_path = records.next();
            if path == destination_path {
                return old_path.map(|s| s.to_string());
            }
        }
    }
    None
}

pub fn discard_file_changes(work_dir: &Path, relative_path: &str) -> Result<(), GitError> {
    validate_diff_path(work_dir, relative_path)?;
    let rename_source = find_rename_source(work_dir, relative_path);
    if let Some(ref old_path) = rename_source {
        validate_diff_path(work_dir, old_path)?;
    }

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

    if let Some(old_path) = rename_source {
        if command(
            work_dir,
            &["restore", "--staged", "--worktree", "--", &old_path],
        )
        .is_err()
        {
            let _ = command(work_dir, &["reset", "HEAD", "--", &old_path]);
            let _ = command(work_dir, &["checkout", "HEAD", "--", &old_path]);
        }
    }

    Ok(())
}

pub fn discard_files<S: AsRef<str>>(work_dir: &Path, relative_paths: &[S]) -> Result<(), GitError> {
    for path in relative_paths {
        validate_diff_path(work_dir, path.as_ref())?;
    }
    for path in relative_paths {
        discard_file_changes(work_dir, path.as_ref())?;
    }
    Ok(())
}

pub fn discard_all_changes(work_dir: &Path) -> Result<(), GitError> {
    if command(work_dir, &["restore", "--staged", "--worktree", "."]).is_err() {
        let _ = command(work_dir, &["reset", "HEAD", "."]);
        let _ = command(work_dir, &["checkout", "HEAD", "."]);
    }
    let _ = command(work_dir, &["clean", "-f", "-d"]);
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
        std::fs::write(&gitignore_path, current_content)
            .map_err(|e| GitError::new(work_dir, format!("Failed to update .gitignore: {e}")))?;
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
        std::fs::write(&gitignore_path, current_content)
            .map_err(|e| GitError::new(work_dir, format!("Failed to update .gitignore: {e}")))?;
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
pub fn commit_amend(work_dir: &Path, message: &str) -> Result<(), GitError> {
    let message = message.trim();
    if message.is_empty() {
        return Err(GitError::new(
            work_dir,
            "commit message cannot be empty".to_owned(),
        ));
    }
    command(work_dir, &["commit", "--amend", "-m", message])?;
    Ok(())
}

pub fn last_commit_message(work_dir: &Path) -> Result<String, GitError> {
    let msg = command(work_dir, &["log", "-1", "--format=%B"])?;
    Ok(msg.trim_end().to_string())
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
    let output = command(work_dir, &["pull", "--ff-only"])?;
    note_remote_fetched(work_dir);
    Ok(output)
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

/// Changes the PR contains, from the selected base's merge base through HEAD.
/// Prefer the fetched remote base; neither the index nor working tree is included.
pub fn draft_pr_diff(work_dir: &Path, base: &str) -> Result<String, GitError> {
    let base = validate_branch_name(work_dir, base)?;
    let remote = format!("refs/remotes/origin/{base}");
    let base_ref = if command(work_dir, &["rev-parse", "--verify", &remote]).is_ok() {
        remote
    } else {
        format!("refs/heads/{base}")
    };
    command(work_dir, &["diff", "--stat", "--patch", &format!("{base_ref}...HEAD"), "--"])
}

pub fn diff_branch(work_dir: &Path, branch: &str) -> Result<String, GitError> {
    let branch = validate_branch_name(work_dir, branch)?;
    command(
        work_dir,
        &["diff", "--stat", "--patch", &format!("HEAD...{branch}")],
    )
}

pub fn delete_branch(work_dir: &Path, branch: &str, force: bool) -> Result<(), GitError> {
    let branch = validate_branch_name(work_dir, branch)?;
    command(
        work_dir,
        &["branch", if force { "-D" } else { "-d" }, &branch],
    )?;
    Ok(())
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
pub fn stage_files<S: AsRef<str>>(work_dir: &Path, paths: &[S]) -> Result<(), GitError> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["add", "--"];
    for p in paths {
        args.push(p.as_ref());
    }
    command(work_dir, &args)?;
    Ok(())
}

pub fn unstage_files<S: AsRef<str>>(work_dir: &Path, paths: &[S]) -> Result<(), GitError> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["restore", "--staged", "--"];
    for p in paths {
        args.push(p.as_ref());
    }
    command(work_dir, &args)?;
    Ok(())
}

fn validate_diff_path(work_dir: &Path, path: &str) -> Result<(), GitError> {
    let invalid = || GitError::new(work_dir, format!("path is outside the workspace: {path}"));
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(invalid());
    }

    let root = work_dir.canonicalize().map_err(|error| {
        GitError::new(work_dir, format!("could not resolve workspace: {error}"))
    })?;
    let mut existing = work_dir.join(relative);
    while !existing.exists() {
        if !existing.pop() {
            return Err(invalid());
        }
    }
    if !existing
        .canonicalize()
        .map_err(|error| GitError::new(work_dir, format!("could not resolve path: {error}")))?
        .starts_with(&root)
    {
        return Err(invalid());
    }
    Ok(())
}

pub fn diff_file(work_dir: &Path, path: &str) -> Result<String, GitError> {
    let diff = diff_file_with_options(work_dir, path, DiffOptions::default())?;
    Ok(if diff.is_empty() {
        "No textual diff available for this file.\n".to_owned()
    } else {
        diff
    })
}

fn tracked_diff(
    work_dir: &Path,
    path: Option<&str>,
    options: DiffOptions,
) -> Result<String, GitError> {
    let mut args = vec!["diff", "--no-ext-diff", "--no-textconv"];
    if options.ignore_whitespace {
        args.push("--ignore-all-space");
    }
    let compare = |base: Option<&str>| {
        let mut args = args.clone();
        args.extend(base);
        args.push("--");
        args.extend(path);
        command(work_dir, &args)
    };
    match compare(Some("HEAD")) {
        Ok(diff) => Ok(diff),
        Err(error) => {
            if command(work_dir, &["rev-parse", "--verify", "HEAD"]).is_ok() {
                return Err(error);
            }
            let staged = compare(Some("--cached"))?;
            let unstaged = compare(None)?;
            let mut diff = String::new();
            if !staged.is_empty() {
                diff.push_str("# Staged changes\n");
                diff.push_str(&staged);
            }
            if !unstaged.is_empty() {
                diff.push_str("# Unstaged changes\n");
                diff.push_str(&unstaged);
            }
            Ok(diff)
        }
    }
}

fn untracked_diff(work_dir: &Path, paths: &str, options: DiffOptions) -> Result<String, GitError> {
    if paths.is_empty() {
        return Ok(String::new());
    }
    for path in paths.split('\0').filter(|path| !path.is_empty()) {
        validate_diff_path(work_dir, path)?;
    }
    let io_error =
        |error| GitError::new(work_dir, format!("could not prepare untracked diff: {error}"));
    let scratch = tempfile::tempdir().map_err(io_error)?;
    let objects = scratch.path().join("objects");
    let pathspec = scratch.path().join("paths");
    std::fs::create_dir(&objects).map_err(io_error)?;
    std::fs::write(&pathspec, paths).map_err(io_error)?;
    let git = || {
        let mut command = Command::new("git");
        command
            .current_dir(work_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_INDEX_FILE", scratch.path().join("index"))
            .env("GIT_OBJECT_DIRECTORY", &objects)
            .args(["-c", "core.splitIndex=false", "-c", "core.fsmonitor=false"]);
        command
    };
    let run = |command: &mut Command| {
        #[cfg(test)]
        COMMAND_SPAWNS.set(COMMAND_SPAWNS.get() + 1);
        let output = command
            .output()
            .map_err(|error| GitError::new(work_dir, format!("could not start git: {error}")))?;
        if !output.status.success() {
            return Err(GitError::new(
                work_dir,
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    run(git()
        .args([
            "--literal-pathspecs",
            "add",
            "--intent-to-add",
            "--pathspec-from-file",
        ])
        .arg(&pathspec)
        .arg("--pathspec-file-nul"))?;
    let mut diff = git();
    diff.args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--ita-invisible-in-index",
        "--relative",
    ]);
    if options.ignore_whitespace {
        diff.arg("--ignore-all-space");
    }
    run(diff.arg("--"))
}

fn untracked_paths(work_dir: &Path, path: Option<&str>) -> Result<String, GitError> {
    let mut args = vec!["ls-files", "-z", "--others", "--exclude-standard", "--"];
    args.extend(path);
    command(work_dir, &args)
}

/// Local patch with presentation options; a successful empty comparison is empty.
pub fn diff_file_with_options(
    work_dir: &Path,
    path: &str,
    options: DiffOptions,
) -> Result<String, GitError> {
    validate_diff_path(work_dir, path)?;
    let mut diff = tracked_diff(work_dir, Some(path), options)?;
    diff.push_str(&untracked_diff(
        work_dir,
        &untracked_paths(work_dir, Some(path))?,
        options,
    )?);
    Ok(diff)
}

/// Combined tracked and untracked changes with presentation options.
pub fn worktree_diff_with_options(
    work_dir: &Path,
    options: DiffOptions,
) -> Result<String, GitError> {
    let mut diff = tracked_diff(work_dir, None, options)?;
    diff.push_str(&untracked_diff(
        work_dir,
        &untracked_paths(work_dir, None)?,
        options,
    )?);
    Ok(diff)
}

/// Combined working-tree diff, including untracked additions.
pub fn worktree_diff(work_dir: &Path) -> Result<String, GitError> {
    let diff = worktree_diff_with_options(work_dir, DiffOptions::default())?;
    Ok(if diff.is_empty() {
        "No changes in the working tree.\n".to_owned()
    } else {
        diff
    })
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

/// Why a file inventory could not be produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileInventoryError {
    /// `work_dir` is not inside a Git work tree.
    NotARepository,
    /// Git ran but failed, so the result is unknown (not "empty").
    Failed(GitError),
}

/// Lists files selectable for `@` completion under `work_dir`.
///
/// Runs `git ls-files -z --cached --others --exclude-standard`, so results are
/// tracked files plus non-ignored untracked files; names ignored by
/// `.gitignore`/`.git/info/exclude`/global excludes are never enumerated.
/// Index entries whose worktree file is gone (`ls-files --deleted`: unstaged
/// deletions, sparse-checkout omissions) are subtracted so every listed path
/// exists on disk. `.git` and `.threadlane` entries are excluded on top of
/// that, duplicate names (tracked + untracked) are removed, and non-UTF-8
/// names are counted and skipped instead of being lossy-decoded into
/// replacement characters.
///
/// Only names are handled: no file content is read and no symlink is
/// traversed. The inventory is purely Git's index/worktree name list.
pub fn list_project_files(work_dir: &Path) -> Result<GitFileInventory, FileInventoryError> {
    if !is_git_repo(work_dir) {
        return Err(FileInventoryError::NotARepository);
    }
    let output = command_bytes(
        work_dir,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )
    .map_err(FileInventoryError::Failed)?;
    // Index entries survive unstaged deletions; without this they would be
    // selectable but could never pass the on-disk existence check at insert.
    let deleted_output = command_bytes(work_dir, &["ls-files", "-z", "--deleted"])
        .map_err(FileInventoryError::Failed)?;
    let deleted: HashSet<&[u8]> = deleted_output
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .collect();

    let mut inventory = GitFileInventory::default();
    let mut seen = HashSet::new();
    for raw in output.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let Ok(path) = std::str::from_utf8(raw) else {
            inventory.non_utf8_skipped += 1;
            continue;
        };
        if deleted.contains(raw) {
            continue;
        }
        if path == ".git"
            || path == ".threadlane"
            || path.starts_with(".git/")
            || path.starts_with(".threadlane/")
            || path.split('/').any(|component| component == "..")
        {
            continue;
        }
        if seen.insert(path) {
            inventory.paths.push(path.to_owned());
        }
    }
    inventory.paths.sort();
    Ok(inventory)
}

/// Returns the primary checkout root even when `work_dir` is itself a worktree.
pub fn primary_worktree_root(work_dir: &Path) -> Result<PathBuf, GitError> {
    let common_dir = command(
        work_dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    PathBuf::from(common_dir.trim())
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| GitError::new(work_dir, "Git common directory has no parent"))
}

/// Creates a new Git worktree at `worktree_path` checked out on `branch_name`.
pub fn create_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
) -> Result<(), GitError> {
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            GitError::new(
                repo_path,
                format!("Failed to create parent directory for worktree: {e}"),
            )
        })?;
    }
    let worktree_str = worktree_path
        .to_str()
        .ok_or_else(|| GitError::new(repo_path, "Invalid worktree path"))?;

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

/// Creates a fresh branch from an explicit commit; never attaches an existing branch.
pub fn create_worktree_from(
    repo: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<(), GitError> {
    validate_branch_name(repo, branch)?;
    // Resolve before creating anything. `--end-of-options` also rejects option injection.
    let commit = command(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )?;
    let path = path
        .to_str()
        .ok_or_else(|| GitError::new(repo, "Invalid worktree path"))?;
    command(
        repo,
        &["worktree", "add", "-b", branch, path, commit.trim()],
    )?;
    Ok(())
}

/// Local and fetched remote branches suitable as worktree starting points.
pub fn worktree_bases(repo: &Path) -> Result<(String, Vec<String>), GitError> {
    // This runs on the setup worker; retain cached bases when offline.
    let _ = fetch(repo);
    let branches: Vec<String> = command(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ],
    )?
    .lines()
    .filter(|line| !line.ends_with("/HEAD"))
    .map(str::to_owned)
    .collect();
    let default = command(
        repo,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok()
    .map(|s| s.trim().to_owned())
    .filter(|name| branches.contains(name))
    .or_else(|| {
        discover_default_branch(repo).and_then(|name| {
            [format!("origin/{name}"), name]
                .into_iter()
                .find(|candidate| branches.contains(candidate))
        })
    })
    .or_else(|| {
        current_branch(repo)
            .ok()
            .flatten()
            .filter(|name| branches.contains(name))
    })
    .ok_or_else(|| GitError::new(repo, "Commit a change before creating a worktree"))?;
    Ok((default, branches))
}

/// Removes an existing Git worktree.
pub fn remove_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    force: bool,
) -> Result<(), GitError> {
    let worktree_str = worktree_path
        .to_str()
        .ok_or_else(|| GitError::new(repo_path, "Invalid worktree path"))?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree_str);
    command(repo_path, &args)?;
    Ok(())
}

/// Reclaims a deterministic subagent worktree slot before provisioning.
///
/// A crashed or killed run leaves its path registered and its branch behind;
/// retrying the same lane would fail `worktree add <existing-path>
/// <existing-branch>` forever. Returns `true` when the slot is reusable
/// as-is (same branch, clean), so the caller skips creation; otherwise the
/// slot is cleared and the caller recreates it.
///
/// Safety: only paths under `.threadlane/worktrees/subagents` are ever
/// cleared, and a branch checked out in any *other* live worktree is
/// refused rather than attached twice.
pub fn reclaim_subagent_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
) -> Result<bool, GitError> {
    let managed = worktree_path
        .components()
        .any(|component| component.as_os_str() == ".threadlane");
    if !managed {
        return Err(GitError::new(
            repo_path,
            "refusing to reclaim a worktree outside .threadlane",
        ));
    }
    let same_file = |registered: &Path| {
        std::fs::canonicalize(registered).unwrap_or_else(|_| registered.to_path_buf())
            == std::fs::canonicalize(worktree_path)
                .unwrap_or_else(|_| worktree_path.to_path_buf())
    };
    let registered = list_worktrees(repo_path)
        .unwrap_or_default()
        .into_iter()
        .find(|worktree| same_file(&worktree.path));
    match registered {
        Some(info) if info.branch.as_deref() == Some(branch_name) => {
            // Same lane's leftover: reuse when no *tracked* file is
            // modified, clear and recreate otherwise. Untracked-only dirt
            // does NOT block reuse: environment sidecars (`.tokensave/`,
            // hook outputs) and harness bookkeeping land untracked in fresh
            // checkouts (porcelain `??`), and leftovers from the dead
            // attempt belong to the same retried task anyway.
            let reusable = worktree_path.exists()
                && inspect(worktree_path)
                    .map(|status| {
                        status.files.iter().all(|file| {
                            file.index_status == '?' && file.worktree_status == '?'
                        })
                    })
                    .unwrap_or(false);
            if reusable {
                return Ok(true);
            }
            if remove_worktree(repo_path, worktree_path, true).is_err() {
                prune_worktrees(repo_path)?;
                if worktree_path.exists() {
                    std::fs::remove_dir_all(worktree_path).map_err(|error| {
                        GitError::new(
                            repo_path,
                            format!("Failed to clear orphaned worktree: {error}"),
                        )
                    })?;
                }
            }
        }
        Some(_) => {
            // Our deterministic path claimed by a foreign branch: clear it.
            // The branch check below still guards liveness.
            if remove_worktree(repo_path, worktree_path, true).is_err() {
                prune_worktrees(repo_path)?;
                if worktree_path.exists() {
                    std::fs::remove_dir_all(worktree_path).map_err(|error| {
                        GitError::new(
                            repo_path,
                            format!("Failed to clear orphaned worktree: {error}"),
                        )
                    })?;
                }
            }
        }
        None if worktree_path.exists() => {
            // Stale directory without metadata (pruned record): clear it.
            std::fs::remove_dir_all(worktree_path).map_err(|error| {
                GitError::new(
                    repo_path,
                    format!("Failed to clear orphaned worktree: {error}"),
                )
            })?;
        }
        None => {}
    }
    // The branch must not be checked out anywhere else: that holder is a
    // live sibling lane, and attaching twice corrupts both.
    let elsewhere = list_worktrees(repo_path)
        .unwrap_or_default()
        .into_iter()
        .find(|worktree| {
            worktree.branch.as_deref() == Some(branch_name) && !same_file(&worktree.path)
        });
    if let Some(holder) = elsewhere {
        return Err(GitError::new(
            repo_path,
            format!(
                "branch {branch_name} is already checked out at {}; refusing to attach a second worktree",
                holder.path.display()
            ),
        ));
    }
    Ok(false)
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

/// Removes stale administrative worktree records.
pub fn prune_worktrees(repo_path: &Path) -> Result<(), GitError> {
    command(repo_path, &["worktree", "prune"])?;
    Ok(())
}

/// Resolves the remote's default branch name (e.g. `main`) without fetching:
/// `origin/HEAD`, then `remote show -n`, then `main`/`master` probing.
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
                // Older git prints "(not queried)" here, newer git
                // "(unknown)" — neither is a real branch name.
                (!branch.is_empty() && !branch.starts_with('(')).then(|| branch.to_owned())
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

/// Bounded variant of the Git inventory for interactive content search.
/// Uses the same tracked/nonignored-untracked policy, excluding deleted entries.
/// The reader stops at the byte ceiling; the parent kills/reaps Git on timeout.
pub fn list_search_files(work_dir: &Path, deadline: Instant) -> Result<(GitFileInventory, Option<&'static str>), String> {
    use std::io::Read;
    use std::process::Stdio;
    const LIMIT: u64 = 4 * 1024 * 1024;
    let mut child = Command::new("git")
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .current_dir(work_dir).env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("Git stdout unavailable")?;
    let full = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_full = full.clone();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(LIMIT + 1).read_to_end(&mut bytes)?;
        reader_full.store(bytes.len() > LIMIT as usize, std::sync::atomic::Ordering::Release);
        Ok::<_, std::io::Error>(bytes)
    });
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? { break status; }
        if Instant::now() >= deadline || full.load(std::sync::atomic::Ordering::Acquire) {
            timed_out = Instant::now() >= deadline;
            let _ = child.kill();
            break child.wait().map_err(|e| e.to_string())?;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut bytes = reader.join().map_err(|_| "Git inventory reader failed")?.map_err(|e| e.to_string())?;
    let partial = timed_out || bytes.len() > LIMIT as usize;
    if !status.success() && !partial { return Err("Find in files requires an available Git checkout".into()); }
    if partial {
        bytes.truncate(LIMIT as usize);
        if let Some(end) = bytes.iter().rposition(|b| *b == 0) { bytes.truncate(end + 1); } else { bytes.clear(); }
    }
    let mut inventory = GitFileInventory::default();
    let mut seen = HashSet::new();
    for raw in bytes.split(|b| *b == 0).filter(|raw| !raw.is_empty()) {
        if Instant::now() >= deadline { timed_out = true; break; }
        let Ok(path) = std::str::from_utf8(raw) else { inventory.non_utf8_skipped += 1; continue; };
        if path.split('/').any(|part| matches!(part, ".git" | ".threadlane" | "..")) { continue; }
        if std::fs::symlink_metadata(work_dir.join(path)).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) { continue; }
        if seen.insert(path) { inventory.paths.push(path.to_owned()); }
    }
    inventory.paths.sort();
    Ok((inventory, if timed_out { Some("3-second inventory budget reached") }
        else if partial { Some("4 MiB inventory limit reached") } else { None }))
}
