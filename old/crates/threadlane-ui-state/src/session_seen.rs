//! Per-project acknowledgment state for the sidebar "New result" marker.
//!
//! `.threadlane/session_seen.json` maps session id -> the newest successful
//! main-lane Run completion the user has been shown (`null` while nothing has
//! been acknowledged yet). A session id being present at all means the store
//! has already made a decision for it — either registered before its first
//! run (value `null`) or baselined to its first successfully parsed
//! completion summary (value = that token, or `null` when none exists).
//! Absent ids have never been seen by a successful discovery pass.
//!
//! Writes are serialized onto a single background writer thread and landed by
//! tmp-file + rename so a crash never leaves a half-written file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use threadlane_daemon::{RunCompletionToken, SessionCompletionSummary, SessionInfo};

const SCHEMA_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct SessionSeenFile {
    version: u32,
    sessions: HashMap<String, Option<RunCompletionToken>>,
}

pub struct SessionSeenStore {
    /// Canonical project directory this store belongs to.
    work_dir: PathBuf,
    path: PathBuf,
    acknowledged: HashMap<String, Option<RunCompletionToken>>,
    dirty: bool,
}

impl SessionSeenStore {
    /// Loads the store for a project. A missing, unreadable, or
    /// version-mismatched file starts empty: the next successful discovery
    /// pass re-baselines every confirmed session, matching upgrade behavior.
    pub fn load(work_dir: &Path) -> Self {
        let path = work_dir.join(".threadlane/session_seen.json");
        let acknowledged = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| {
                serde_json::from_str::<SessionSeenFile>(&content)
                    .map_err(|error| {
                        tracing::warn!(
                            "ignoring unreadable {}: {error}",
                            path.display()
                        );
                        error
                    })
                    .ok()
            })
            .filter(|file| file.version == SCHEMA_VERSION)
            .map(|file| file.sessions)
            .unwrap_or_default();
        Self {
            work_dir: work_dir.to_path_buf(),
            path,
            acknowledged,
            dirty: false,
        }
    }

    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// Whether first discovery has already decided this session's watermark.
    pub fn is_tracked(&self, session_id: &str) -> bool {
        self.acknowledged.contains_key(session_id)
    }

    pub fn acknowledged_token(&self, session_id: &str) -> Option<&RunCompletionToken> {
        self.acknowledged.get(session_id).and_then(Option::as_ref)
    }

    /// Registers a session created in-app before its first run so a later
    /// discovery baseline cannot retroactively acknowledge its first result.
    /// An already-tracked id keeps its watermark — a repeat registration
    /// must never regress an acknowledgment.
    pub fn register(&mut self, session_id: &str) {
        if !self.acknowledged.contains_key(session_id) {
            self.acknowledged.insert(session_id.to_string(), None);
            self.dirty = true;
        }
    }

    /// Baselines a session the first successful discovery just confirmed:
    /// pre-existing history is already "seen" at its current completion.
    /// No-op for already-tracked sessions (a baseline never rewrites a known
    /// watermark) and for `Unknown` summaries (an unreadable stub is never a
    /// baseline).
    pub fn baseline(&mut self, session: &SessionInfo) {
        let token = match &session.completion_summary {
            SessionCompletionSummary::Unknown => return,
            SessionCompletionSummary::None => None,
            SessionCompletionSummary::Latest(token) => Some(token.clone()),
        };
        if self.is_tracked(&session.id) {
            return;
        }
        self.acknowledged.insert(session.id.clone(), token);
        self.dirty = true;
    }

    /// Advances the acknowledged watermark to `token`. Journal seq is a total
    /// order within a session file, so a lower or equal seq is never written
    /// back over a newer acknowledgment.
    pub fn acknowledge(&mut self, session_id: &str, token: &RunCompletionToken) -> bool {
        match self.acknowledged.get(session_id) {
            Some(Some(existing)) if existing.seq >= token.seq => return false,
            _ => {}
        }
        self.acknowledged
            .insert(session_id.to_string(), Some(token.clone()));
        self.dirty = true;
        true
    }

    /// Drops metadata for a session that was explicitly deleted. Absence from
    /// discovery alone must not call this — a temporarily missing transcript
    /// keeps its watermark.
    pub fn prune(&mut self, session_id: &str) {
        if self.acknowledged.remove(session_id).is_some() {
            self.dirty = true;
        }
    }

    /// True while the session's latest confirmed completion is newer than the
    /// acknowledged watermark, judged by the same monotonic journal-seq rule
    /// as `acknowledge`: a restored or truncated journal reporting an older
    /// completion can never produce a marker that acknowledgment then
    /// refuses to clear.
    pub fn has_unseen(&self, session: &SessionInfo) -> bool {
        let SessionCompletionSummary::Latest(token) = &session.completion_summary else {
            return false;
        };
        match self.acknowledged.get(&session.id) {
            Some(Some(existing)) => token.seq > existing.seq,
            _ => true,
        }
    }

    /// Serialized snapshot pending write; clears the dirty flag once handed
    /// to the writer. Re-marked via `mark_dirty` if the write later fails.
    pub fn take_dirty_json(&mut self) -> Option<String> {
        if !self.dirty {
            return None;
        }
        let file = SessionSeenFile {
            version: SCHEMA_VERSION,
            sessions: self.acknowledged.clone(),
        };
        match serde_json::to_string_pretty(&file) {
            Ok(json) => {
                self.dirty = false;
                Some(json)
            }
            Err(error) => {
                tracing::warn!("failed to serialize {}: {error}", self.path.display());
                None
            }
        }
    }

    /// Re-arms a failed write so the next mutation flushes the full state
    /// again (single deferred retry, never a per-frame loop).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

struct SeenWriteJob {
    work_dir: PathBuf,
    path: PathBuf,
    /// Producer-assigned revision echoed back in the result; stores that
    /// gate visibility on write confirmation use it to match acks. Stores
    /// that do not (session_seen) pass `0`.
    generation: u64,
    json: String,
}

pub struct SeenWriteResult {
    pub work_dir: PathBuf,
    pub generation: u64,
    pub error: Option<String>,
}

/// One serialized writer for every project's `session_seen.json`. Callers
/// hand it complete serializations; jobs run in submission order and the tmp
/// + rename pair keeps the on-disk file atomic.
pub struct SessionSeenWriter {
    jobs: mpsc::Sender<SeenWriteJob>,
    results: mpsc::Receiver<SeenWriteResult>,
}

impl SessionSeenWriter {
    pub fn spawn() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel::<SeenWriteJob>();
        let (results_tx, results_rx) = mpsc::channel::<SeenWriteResult>();
        std::thread::spawn(move || {
            while let Ok(job) = jobs_rx.recv() {
                let error = write_session_seen(&job.path, &job.json).err();
                if results_tx
                    .send(SeenWriteResult {
                        work_dir: job.work_dir,
                        generation: job.generation,
                        error,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            jobs: jobs_tx,
            results: results_rx,
        }
    }

    pub fn submit(
        &self,
        work_dir: PathBuf,
        path: PathBuf,
        generation: u64,
        json: String,
    ) -> bool {
        self.jobs
            .send(SeenWriteJob {
                work_dir,
                path,
                generation,
                json,
            })
            .is_ok()
    }

    pub fn try_recv_result(&self) -> Option<SeenWriteResult> {
        self.results.try_recv().ok()
    }
}

fn write_session_seen(path: &Path, json: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("session_seen path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    // Unique name + create_new: a pre-planted file or symlink at the
    // temporary path can never be opened and truncated.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let temporary = dir.join(format!(
        "session_seen.{}.{nonce}.tmp",
        std::process::id()
    ));
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(json.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{write_session_seen, SessionSeenStore};
    use threadlane_daemon::{
        RunCompletionToken, SessionCompletionSummary, SessionHealth, SessionInfo,
    };
    use std::path::{Path, PathBuf};

    fn token(seq: u64) -> RunCompletionToken {
        RunCompletionToken {
            record_id: format!("finish-{seq}"),
            run_id: format!("run-{seq}"),
            seq,
        }
    }

    fn session(id: &str, summary: SessionCompletionSummary) -> SessionInfo {
        let work_dir = PathBuf::from("/project");
        SessionInfo {
            id: id.into(),
            title: id.into(),
            work_dir: work_dir.clone(),
            runtime_work_dir: work_dir,
            session_file: Path::new("/project/.threadlane/sessions").join(format!("{id}.jsonl")),
            updated_at: 0,
            health: SessionHealth::Healthy,
            git_branch: None,
            github_issue: None,
            is_worktree: false,
            worktree_available: true,
            completion_summary: summary,
        }
    }

    #[test]
    fn baseline_marks_preexisting_completions_seen() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SessionSeenStore::load(temp.path());

        let finished = session("finished", SessionCompletionSummary::Latest(token(7)));
        let quiet = session("quiet", SessionCompletionSummary::None);
        let unknown = session("unknown", SessionCompletionSummary::Unknown);
        store.baseline(&finished);
        store.baseline(&quiet);
        store.baseline(&unknown);

        assert!(!store.has_unseen(&finished));
        assert!(!store.has_unseen(&quiet));
        assert!(store.is_tracked("finished"));
        assert!(store.is_tracked("quiet"));
        assert!(!store.is_tracked("unknown"));
    }

    #[test]
    fn registered_session_reports_its_first_completion_unseen() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SessionSeenStore::load(temp.path());
        store.register("new-session");

        let later = session("new-session", SessionCompletionSummary::Latest(token(3)));
        // Baseline must not retroactively acknowledge the registered session.
        store.baseline(&later);

        assert!(store.has_unseen(&later));
    }

    #[test]
    fn acknowledgment_advances_monotonically_and_clears_unseen() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SessionSeenStore::load(temp.path());
        store.register("session");
        let seen = session("session", SessionCompletionSummary::Latest(token(9)));
        assert!(store.has_unseen(&seen));

        assert!(store.acknowledge("session", &token(9)));
        assert!(!store.has_unseen(&seen));
        // Re-acknowledging the same or an older completion never regresses.
        assert!(!store.acknowledge("session", &token(9)));
        assert!(!store.acknowledge("session", &token(4)));
        assert_eq!(store.acknowledged_token("session"), Some(&token(9)));
        // A journal reporting a completion at or below the watermark is not
        // unseen — otherwise acknowledging could never clear the marker.
        let older = session("session", SessionCompletionSummary::Latest(token(4)));
        assert!(!store.has_unseen(&older));
        // A newer completion re-arms the marker.
        let newer = session("session", SessionCompletionSummary::Latest(token(10)));
        assert!(store.has_unseen(&newer));
    }

    #[test]
    fn missing_store_starts_empty_and_unknown_is_never_unseen() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionSeenStore::load(temp.path());

        let unconfirmed = session("s", SessionCompletionSummary::Unknown);
        assert!(!store.has_unseen(&unconfirmed));
        // An untracked session with a confirmed completion is unseen until
        // first discovery baselines it.
        let confirmed = session("s", SessionCompletionSummary::Latest(token(1)));
        assert!(store.has_unseen(&confirmed));
    }

    #[test]
    fn persisted_store_round_trips_and_rejects_wrong_versions() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SessionSeenStore::load(temp.path());
        store.baseline(&session("a", SessionCompletionSummary::Latest(token(5))));
        store.register("b");
        let json = store.take_dirty_json().unwrap();
        assert!(store.take_dirty_json().is_none());

        write_session_seen(store.path(), &json).unwrap();
        let reloaded = SessionSeenStore::load(temp.path());
        assert!(!reloaded.has_unseen(&session("a", SessionCompletionSummary::Latest(token(5)))));
        assert!(reloaded.is_tracked("b"));
        assert_eq!(reloaded.acknowledged_token("b"), None);

        std::fs::write(
            store.path(),
            json.replace("\"version\": 1", "\"version\": 99"),
        )
        .unwrap();
        let stale = SessionSeenStore::load(temp.path());
        assert!(!stale.is_tracked("a"));
    }

    #[test]
    fn prune_drops_metadata_for_deleted_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = SessionSeenStore::load(temp.path());
        store.register("gone");
        store.prune("gone");
        assert!(!store.is_tracked("gone"));
        store.prune("gone");
    }
}
