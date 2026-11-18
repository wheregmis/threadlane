use std::path::{Path, PathBuf};
use threadlane_runtime::harness::{JsonlStore, SessionStore};

use threadlane_runtime::titles::extract_session_title;
use crate::projection::project_latest_run_completion;
use crate::types::{
    SessionCompletionSummary, SessionDiscoveryCache, SessionDiscoveryCacheEntry, SessionHealth,
    SessionInfo,
};

pub fn file_mtime(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

fn discovery_fingerprint(path: &Path) -> Option<(u64, std::time::SystemTime)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()?))
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

    // A recovery fork shares the original checkout, not its execution state.
    // Accept only one normal path component; metadata must not escape the project.
    let owner = facts
        .get("worktree_owner")
        .map(String::as_str)
        .filter(|owner| {
            let mut components = Path::new(owner).components();
            matches!(components.next(), Some(std::path::Component::Normal(_)))
                && components.next().is_none()
        })
        .unwrap_or(id);
    let inferred = canonical_work_dir.join(".threadlane/worktrees").join(owner);
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
                // The stub pass never parses the transcript: completions stay
                // Unknown until the background discovery refresh parses it.
                completion_summary: SessionCompletionSummary::Unknown,
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
    discover_sessions_in_project_cached_with_loader(work_dir, cache, |path| {
        JsonlStore::open_read_only(path)
    })
}

fn discover_sessions_in_project_cached_with_loader(
    work_dir: &Path,
    cache: &mut SessionDiscoveryCache,
    mut load_store: impl FnMut(&Path) -> std::io::Result<JsonlStore>,
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
        let cached_fingerprint = discovery_fingerprint(cached_data_path);
        let info = match (cache.entries.get(&path), cached_fingerprint) {
            (Some(cached), Some((len, modified)))
                if cached.len == len && cached.modified == Some(modified) =>
            {
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
                            modified: Some(modified),
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
                let stub_fingerprint = discovery_fingerprint(&path);
                let stub = load_store(&path);
                let (runtime_work_dir, is_worktree, stub_branch, github_issue) =
                    match &stub {
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
                let (transcript, pre_open_fingerprint) = if session_file == path {
                    (stub, stub_fingerprint)
                } else {
                    let fingerprint = discovery_fingerprint(&session_file);
                    (load_store(&session_file), fingerprint)
                };
                let (title, health, recorded_branch, completion_summary) =
                    match transcript {
                        Ok(store) => (
                            extract_session_title(&store, &id),
                            SessionHealth::Healthy,
                            store.facts().get("git_branch").cloned().or(stub_branch),
                            match project_latest_run_completion(&store) {
                                Some(token) => SessionCompletionSummary::Latest(token),
                                None => SessionCompletionSummary::None,
                            },
                        ),
                        Err(_) => (
                            "Unreadable session".to_string(),
                            SessionHealth::Warning,
                            stub_branch,
                            SessionCompletionSummary::Unknown,
                        ),
                    };
                let git_branch =
                    effective_session_git_branch(&runtime_work_dir, is_worktree, recorded_branch);
                let post_projection_fingerprint = discovery_fingerprint(&session_file);
                let cache_fingerprint = match (pre_open_fingerprint, post_projection_fingerprint) {
                    (Some(before), Some(after)) if before == after => Some(after),
                    _ => None,
                };
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
                    completion_summary,
                };
                if let Some((len, modified)) = cache_fingerprint {
                    cache.entries.insert(
                        path.clone(),
                        SessionDiscoveryCacheEntry {
                            len,
                            modified: Some(modified),
                            info: info.clone(),
                        },
                    );
                } else {
                    cache.entries.remove(&path);
                }
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

#[cfg(test)]
mod tests {
    use super::{
        canonical_session_file, discover_sessions_in_project_cached,
        discover_sessions_in_project_cached_with_loader,
    };
    use crate::types::{SessionCompletionSummary, SessionDiscoveryCache, SessionHealth};
    use std::cell::Cell;
    use std::path::Path;
    use threadlane_runtime::harness::{JsonlStore, SessionStore};

    #[test]
    fn discovery_reuses_local_transcript_and_refreshes_changed_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().canonicalize().unwrap();
        let path = canonical_session_file(&project, "local");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut store = JsonlStore::open(&path).unwrap();
        store.set_name("Original title").unwrap();
        store.append_fact("main", "git_branch", "feature", None).unwrap();
        let mut cache = SessionDiscoveryCache::default();
        for _ in 0..2 {
            let sessions = discover_sessions_in_project_cached(&project, &mut cache);
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].title, "Original title");
            assert_eq!(sessions[0].session_file, path);
            assert_eq!(sessions[0].git_branch.as_deref(), Some("feature"));
            assert_eq!(sessions[0].health, SessionHealth::Healthy);
            assert_eq!(
                sessions[0].completion_summary,
                SessionCompletionSummary::None
            );
        }
        store.set_name("Changed title").unwrap();
        assert_eq!(
            discover_sessions_in_project_cached(&project, &mut cache)[0].title,
            "Changed title"
        );
        drop(store);
        std::fs::write(&path, "{invalid journal}\n").unwrap();
        let sessions = discover_sessions_in_project_cached(&project, &mut cache);
        assert_eq!(sessions[0].health, SessionHealth::Warning);
        assert_eq!(sessions[0].title, "Unreadable session");
        assert_eq!(
            sessions[0].completion_summary,
            SessionCompletionSummary::Unknown
        );
    }

    #[test]
    fn discovery_does_not_cache_snapshot_if_journal_changes_during_open() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().canonicalize().unwrap();
        let path = canonical_session_file(&project, "local");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut store = JsonlStore::open(&path).unwrap();
        store.set_name("Original title").unwrap();
        drop(store);

        let mut cache = SessionDiscoveryCache::default();
        let open_count = Cell::new(0);
        {
            let mut loader = |path: &Path| {
                open_count.set(open_count.get() + 1);
                JsonlStore::open_read_only(path)
            };
            let sessions =
                discover_sessions_in_project_cached_with_loader(&project, &mut cache, &mut loader);
            assert_eq!(sessions[0].title, "Original title");
            assert_eq!(open_count.get(), 1);
            open_count.set(0);
            let sessions =
                discover_sessions_in_project_cached_with_loader(&project, &mut cache, &mut loader);
            assert_eq!(sessions[0].title, "Original title");
            assert_eq!(open_count.get(), 0);
        }

        let mut writer = JsonlStore::open(&path).unwrap();
        writer
            .set_name("Snapshot title captured before concurrent append")
            .unwrap();
        drop(writer);

        let race_open_count = Cell::new(0);
        let injected_write = Cell::new(false);
        let mut loader = |path: &Path| {
            race_open_count.set(race_open_count.get() + 1);
            let snapshot = JsonlStore::open_read_only(path)?;
            if !injected_write.replace(true) {
                let mut writer = JsonlStore::open(path)?;
                writer
                    .set_name("Changed title after reader opened its snapshot")
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                drop(writer);
            }
            Ok(snapshot)
        };
        let raced =
            discover_sessions_in_project_cached_with_loader(&project, &mut cache, &mut loader);

        assert_eq!(race_open_count.get(), 1);
        assert_eq!(
            raced[0].title,
            "Snapshot title captured before concurrent append"
        );
        assert!(!cache.entries.contains_key(&path));

        let refreshed = discover_sessions_in_project_cached(&project, &mut cache);
        assert_eq!(
            refreshed[0].title,
            "Changed title after reader opened its snapshot"
        );
        assert!(cache.entries.contains_key(&path));
        assert_eq!(
            cache.entries[&path].info.title,
            "Changed title after reader opened its snapshot"
        );
    }
}
