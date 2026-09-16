use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub fn file_mtime(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
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

pub fn resolve_session_transcript_file(
    stub_file: &Path,
    runtime_work_dir: &Path,
    session_id: &str,
    is_worktree: bool,
) -> PathBuf {
    let worktree_file = runtime_work_dir
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    if is_worktree && worktree_file.is_file() {
        worktree_file
    } else {
        stub_file.to_path_buf()
    }
}
