use std::path::{Path, PathBuf};
use threadlane_runtime::harness::{JsonlStore, SessionStore};

use threadlane_runtime::titles::extract_session_title;
use crate::types::{SessionDiscoveryCache, SessionDiscoveryCacheEntry, SessionHealth, SessionInfo};

pub fn file_mtime(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

pub fn effective_session_work_dir(
    canonical_work_dir: &Path,
    id: &str,
    facts: &std::collections::BTreeMap<String, String>,
) -> PathBuf {
    if !facts
        .get("is_worktree")
        .is_some_and(|value| value == "true")
    {
        return canonical_work_dir.to_path_buf();
    }

    let inferred = canonical_work_dir.join(".threadlane/worktrees").join(id);
    let candidate = facts
        .get("worktree_path")
        .map(PathBuf::from)
        .unwrap_or_else(|| inferred.clone());
    let valid_worktree = std::fs::canonicalize(&candidate).ok().filter(|candidate| {
        std::fs::canonicalize(&inferred).is_ok_and(|inferred| *candidate == inferred)
            && candidate.starts_with(canonical_work_dir)
    });
    valid_worktree.unwrap_or(inferred)
}

/// Canonical on-disk home of a session transcript inside a work dir.
/// Single spelling shared by stub fallback, transcript resolution, and
/// hydration so the layout cannot drift between call sites.
pub fn canonical_session_file(work_dir: &Path, session_id: &str) -> PathBuf {
    work_dir
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"))
}

pub fn resolve_session_transcript_file(
    stub_file: &Path,
    runtime_work_dir: &Path,
    session_id: &str,
    is_worktree: bool,
) -> PathBuf {
    let worktree_file = canonical_session_file(runtime_work_dir, session_id);
    if is_worktree && worktree_file.is_file() {
        worktree_file
    } else {
        stub_file.to_path_buf()
    }
}

fn effective_session_git_branch(
    runtime_work_dir: &Path,
    is_worktree: bool,
    recorded: Option<String>,
) -> Option<String> {
    if is_worktree {
        threadlane_git::current_branch(runtime_work_dir)
            .ok()
            .flatten()
            .or(recorded)
    } else {
        recorded
    }
}

pub fn discover_session_stubs_in_project(work_dir: &Path) -> Vec<SessionInfo> {
    let Ok(entries) = std::fs::read_dir(work_dir.join(".threadlane/sessions")) else {
        return Vec::new();
    };
    let canonical_work_dir =
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    let mut sessions = entries
        .flatten()
        .filter_map(|entry| {
            let path = std::fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path());
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".harness.jsonl"))
            {
                return None;
            }
            let id = path.file_stem()?.to_string_lossy().to_string();
            let (runtime_work_dir, recorded_branch, github_issue, is_worktree) =
                JsonlStore::open_read_only(&path)
                    .ok()
                    .map(|store| {
                        let facts = store.facts();
                        (
                            effective_session_work_dir(&canonical_work_dir, &id, &facts),
                            facts.get("git_branch").cloned(),
                            facts
                                .get("github_issue")
                                .and_then(|issue| serde_json::from_str(issue).ok()),
                            facts
                                .get("is_worktree")
                                .is_some_and(|value| value == "true"),
                        )
                    })
                    .unwrap_or((canonical_work_dir.clone(), None, None, false));
            let session_file =
                resolve_session_transcript_file(&path, &runtime_work_dir, &id, is_worktree);
            let git_branch =
                effective_session_git_branch(&runtime_work_dir, is_worktree, recorded_branch);
            let worktree_available = !is_worktree || runtime_work_dir.is_dir();
            Some(SessionInfo {
                title: id.clone(),
                id,
                work_dir: canonical_work_dir.clone(),
                runtime_work_dir,
                updated_at: file_mtime(&session_file),
                session_file,
                health: SessionHealth::Healthy,
                git_branch,
                github_issue,
                is_worktree,
                worktree_available,
            })
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.title.cmp(&b.title))
    });
    sessions
}

pub fn discover_sessions_in_project(work_dir: &Path) -> Vec<SessionInfo> {
    let mut cache = SessionDiscoveryCache::default();
    discover_sessions_in_project_cached(work_dir, &mut cache)
}

pub fn discover_sessions_in_project_cached(
    work_dir: &Path,
    cache: &mut SessionDiscoveryCache,
) -> Vec<SessionInfo> {
    let sessions_dir = work_dir.join(".threadlane/sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return Vec::new();
    };

    let canonical_work_dir =
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    let mut sessions = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for entry in entries.flatten() {
        let path = std::fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path());
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl")
            || path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".harness.jsonl"))
        {
            continue;
        }
        seen_paths.insert(path.clone());
        let cached_data_path = cache
            .entries
            .get(&path)
            .map(|cached| cached.info.session_file.as_path())
            .unwrap_or(path.as_path());
        let metadata = std::fs::metadata(cached_data_path).ok();
        let len = metadata.as_ref().map_or(0, |metadata| metadata.len());
        let modified = metadata.and_then(|metadata| metadata.modified().ok());
        let info = match cache.entries.get(&path) {
            Some(cached) if cached.len == len && cached.modified == modified => {
                // Session files do not change when a worktree is deleted or
                // recreated, so a metadata cache hit can carry a stale
                // `worktree_available`. Recompute that one cheap probe per pass.
                let worktree_available =
                    !cached.info.is_worktree || cached.info.runtime_work_dir.is_dir();
                if worktree_available == cached.info.worktree_available {
                    cached.info.clone()
                } else {
                    let mut info = cached.info.clone();
                    info.worktree_available = worktree_available;
                    cache.entries.insert(
                        path.clone(),
                        SessionDiscoveryCacheEntry {
                            len,
                            modified,
                            info: info.clone(),
                        },
                    );
                    info
                }
            }
            _ => {
                let id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "session".into());
                let (runtime_work_dir, is_worktree, stub_branch, github_issue) =
                    match JsonlStore::open_read_only(&path) {
                        Ok(store) => {
                            let facts = store.facts();
                            let is_worktree = facts
                                .get("is_worktree")
                                .is_some_and(|value| value == "true");
                            let work_dir =
                                effective_session_work_dir(&canonical_work_dir, &id, &facts);
                            (
                                work_dir,
                                is_worktree,
                                facts.get("git_branch").cloned(),
                                facts
                                    .get("github_issue")
                                    .and_then(|issue| serde_json::from_str(issue).ok()),
                            )
                        }
                        Err(_) => (canonical_work_dir.clone(), false, None, None),
                    };
                let session_file =
                    resolve_session_transcript_file(&path, &runtime_work_dir, &id, is_worktree);
                let (title, health, recorded_branch) =
                    match JsonlStore::open_read_only(&session_file) {
                        Ok(store) => (
                            extract_session_title(&store, &id),
                            SessionHealth::Healthy,
                            store.facts().get("git_branch").cloned().or(stub_branch),
                        ),
                        Err(_) => (
                            "Unreadable session".to_string(),
                            SessionHealth::Warning,
                            stub_branch,
                        ),
                    };
                let git_branch =
                    effective_session_git_branch(&runtime_work_dir, is_worktree, recorded_branch);
                let metadata = std::fs::metadata(&session_file).ok();
                let len = metadata.as_ref().map_or(0, |metadata| metadata.len());
                let modified = metadata.and_then(|metadata| metadata.modified().ok());
                let worktree_available = !is_worktree || runtime_work_dir.is_dir();
                let info = SessionInfo {
                    id,
                    title,
                    work_dir: canonical_work_dir.clone(),
                    runtime_work_dir,
                    updated_at: file_mtime(&session_file),
                    session_file,
                    health,
                    git_branch,
                    github_issue,
                    is_worktree,
                    worktree_available,
                };
                cache.entries.insert(
                    path.clone(),
                    SessionDiscoveryCacheEntry {
                        len,
                        modified,
                        info: info.clone(),
                    },
                );
                info
            }
        };
        sessions.push(info);
    }

    cache.entries.retain(|path, _| seen_paths.contains(path));
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.title.cmp(&b.title))
    });
    sessions
}
