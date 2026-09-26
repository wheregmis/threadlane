//! Calendar intent is committed before dispatch. The session harness owns execution.
use chrono::{DateTime, Datelike, Days, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub fn now() -> i64 {
    Utc::now().timestamp()
}
pub fn display_time(seconds: i64, timezone: &str) -> String {
    DateTime::from_timestamp(seconds, 0)
        .map(|t| {
            t.with_timezone(&timezone.parse::<Tz>().unwrap_or(chrono_tz::UTC))
                .format("%a %b %d, %H:%M %Z")
                .to_string()
        })
        .unwrap_or_else(|| "Invalid time".into())
}
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{nanos:x}-{:x}-{:x}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Schedule {
    Manual,
    Interval {
        minutes: u32,
    },
    Calendar {
        hour: u32,
        minute: u32,
        days: Vec<u32>,
        timezone: String,
    },
}
impl Schedule {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Manual => Ok(()),
            Self::Interval { minutes } if (1..=525_600).contains(minutes) => Ok(()),
            Self::Calendar {
                hour,
                minute,
                days,
                timezone,
            } if *hour < 24
                && *minute < 60
                && !days.is_empty()
                && days.iter().all(|day| *day < 7) =>
            {
                timezone
                    .parse::<Tz>()
                    .map(|_| ())
                    .map_err(|_| "Enter an IANA timezone, such as America/Toronto".into())
            }
            _ => Err("Choose a valid time, weekdays, or interval of at least one minute".into()),
        }
    }
    pub fn timezone(&self) -> &str {
        match self {
            Self::Calendar { timezone, .. } => timezone,
            _ => "UTC",
        }
    }
    /// Strictly after `after`. Calendar gaps are skipped; folds use the first instant only.
    pub fn next(&self, after: i64, anchor: i64) -> Result<Option<i64>, String> {
        self.validate()?;
        match self {
            Self::Manual => Ok(None),
            Self::Interval { minutes } => {
                let step = i64::from(*minutes) * 60;
                let count = after.saturating_sub(anchor).div_euclid(step).max(0) + 1;
                anchor
                    .checked_add(count.checked_mul(step).ok_or("Schedule overflow")?)
                    .map(Some)
                    .ok_or_else(|| "Schedule overflow".into())
            }
            Self::Calendar {
                hour,
                minute,
                days,
                timezone,
            } => {
                let tz: Tz = timezone.parse().map_err(|_| "Invalid timezone")?;
                let date = DateTime::from_timestamp(after, 0)
                    .ok_or("Invalid timestamp")?
                    .with_timezone(&tz)
                    .date_naive();
                for offset in 0..15 {
                    let date = date
                        .checked_add_days(Days::new(offset))
                        .ok_or("Schedule overflow")?;
                    if !days.contains(&date.weekday().num_days_from_monday()) {
                        continue;
                    }
                    let local = date.and_hms_opt(*hour, *minute, 0).ok_or("Invalid time")?;
                    let instant = match tz.from_local_datetime(&local) {
                        LocalResult::Single(t) => t.timestamp(),
                        LocalResult::Ambiguous(a, b) => a.timestamp().min(b.timestamp()),
                        LocalResult::None => continue,
                    };
                    if instant > after {
                        return Ok(Some(instant));
                    }
                }
                Err("Could not find the next occurrence".into())
            }
        }
    }
    pub fn label(&self) -> String {
        match self {
            Self::Manual => "Manual".into(),
            Self::Interval { minutes } => format!("Every {minutes} min"),
            Self::Calendar {
                hour,
                minute,
                days,
                timezone,
            } => {
                let names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
                let days = days
                    .iter()
                    .filter_map(|d| names.get(*d as usize).copied())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{days} {hour:02}:{minute:02} · {timezone}")
            }
        }
    }

    fn latest_due(&self, due: i64, at: i64, anchor: i64) -> Result<i64, String> {
        if let Self::Interval { minutes } = self {
            let step = i64::from(*minutes) * 60;
            return Ok(due + (at - due) / step * step);
        }
        let mut latest = due;
        let mut cursor = at.saturating_sub(15 * 86_400).max(due.saturating_sub(1));
        while let Some(next) = self.next(cursor, anchor)? {
            if next > at {
                break;
            }
            latest = next;
            cursor = next;
        }
        Ok(latest)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub prompt: String,
    pub project: PathBuf,
    pub model: String,
    pub effort: String,
    pub worktree: bool,
    pub schedule: Schedule,
    pub enabled: bool,
    pub notify_all: bool,
    pub anchor: i64,
    pub next_at: Option<i64>,
    pub failures: u32,
    pub paused_reason: Option<String>,
}
impl Definition {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("Invalid automation identity".into());
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 160 {
            return Err("Enter a name of at most 160 characters".into());
        }
        if self.prompt.trim().is_empty() || self.prompt.len() > 64_000 {
            return Err("Enter a prompt of at most 64,000 bytes".into());
        }
        if !self.project.is_absolute() {
            return Err("Choose an attached project".into());
        }
        if self.model.trim().is_empty() || self.model.starts_with("acp/") {
            return Err(
                "Choose a native provider model; external agents are not supported yet".into(),
            );
        }
        self.schedule.validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Queued,
    Starting,
    Running,
    WaitingPermission,
    WaitingAnswer,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}
impl RunStatus {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Starting
                | Self::Running
                | Self::WaitingPermission
                | Self::WaitingAnswer
        )
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::WaitingPermission => "Needs permission",
            Self::WaitingAnswer => "Needs an answer",
            Self::Succeeded => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::Interrupted => "Interrupted",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub definition: Definition,
    pub scheduled_for: Option<i64>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub status: RunStatus,
    pub session_id: String,
    pub session_file: Option<PathBuf>,
    pub error: Option<String>,
    pub reviewed: bool,
}
impl Run {
    pub fn needs_attention(&self) -> bool {
        matches!(
            self.status,
            RunStatus::WaitingPermission | RunStatus::WaitingAnswer
        ) || (!self.reviewed
            && matches!(
                self.status,
                RunStatus::Succeeded | RunStatus::Failed | RunStatus::Interrupted
            ))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub revision: u64,
    pub definitions: Vec<Definition>,
    pub runs: Vec<Run>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            definitions: vec![],
            runs: vec![],
        }
    }
}

/// One writer per process and one OS lease per storage directory. Failed writes never update the projection.
pub struct Store {
    root: PathBuf,
    _lease: File,
    snapshot: Snapshot,
}
impl Store {
    pub fn open(root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("owner.lock"))
            .map_err(|e| e.to_string())?;
        lease.try_lock().map_err(|e| format!("Automations are owned by another Threadlane process, or storage is unavailable: {e}"))?;
        let snapshot: Snapshot = match std::fs::read(root.join("state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                format!("Could not read automations; the original file was preserved: {e}")
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Snapshot::default(),
            Err(e) => return Err(e.to_string()),
        };
        if snapshot.version != 1 {
            return Err("Unsupported automation storage version".into());
        }
        for definition in &snapshot.definitions {
            definition.validate()?;
        }
        let mut ids = std::collections::HashSet::new();
        for definition in &snapshot.definitions {
            if !ids.insert(&definition.id) {
                return Err("Duplicate automation identity".into());
            }
        }
        let mut ids = std::collections::HashSet::new();
        for run in &snapshot.runs {
            run.definition.validate()?;
            if run.id.is_empty()
                || !run
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || run.session_id != format!("automation_{}", run.id)
                || !ids.insert(&run.id)
            {
                return Err("Invalid or duplicate automation run identity".into());
            }
        }
        Ok(Self {
            root: root.into(),
            _lease: lease,
            snapshot,
        })
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    fn commit<T>(
        &mut self,
        change: impl FnOnce(&mut Snapshot) -> Result<T, String>,
    ) -> Result<T, String> {
        // ponytail: unreviewed history remains unbounded; use an indexed store if it makes writes costly.
        let mut next = self.snapshot.clone();
        let result = change(&mut next)?;
        let mut kept = std::collections::HashMap::new();
        next.runs.reverse();
        next.runs.retain(|run| {
            if run.status.active() || !run.reviewed {
                return true;
            }
            let count = kept.entry(run.definition.id.clone()).or_insert(0);
            *count += 1;
            *count <= 200
        });
        next.runs.reverse();
        next.revision += 1;
        let mut file = tempfile::NamedTempFile::new_in(&self.root).map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut file, &next).map_err(|e| e.to_string())?;
        file.flush()
            .and_then(|_| file.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        file.persist(self.root.join("state.json"))
            .map_err(|e| e.to_string())?;
        // Once rename succeeds, the in-memory projection must follow it even if directory sync fails.
        self.snapshot = next;
        #[cfg(unix)]
        File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(result)
    }
    pub fn save(&mut self, mut definition: Definition, at: i64) -> Result<(), String> {
        definition.validate()?;
        self.commit(|state| {
            if let Some(old) = state.definitions.iter().find(|d| d.id == definition.id) {
                if old.revision != definition.revision {
                    return Err("This automation changed. Reopen it before saving".into());
                }
                definition.revision += 1;
                definition.failures = old.failures;
                definition.paused_reason = old.paused_reason.clone();
                if old.enabled != definition.enabled {
                    definition.failures = 0;
                    definition.paused_reason = None;
                }
                if old.schedule == definition.schedule && old.enabled == definition.enabled {
                    definition.anchor = old.anchor;
                    definition.next_at = old.next_at;
                } else {
                    definition.anchor = at;
                    definition.next_at = if definition.enabled {
                        definition.schedule.next(at, at)?
                    } else {
                        None
                    };
                }
            } else {
                if definition.revision != 0 {
                    return Err("This automation was deleted".into());
                }
                definition.revision = 1;
                definition.anchor = at;
                definition.next_at = if definition.enabled {
                    definition.schedule.next(at, at)?
                } else {
                    None
                };
            }
            state.definitions.retain(|d| d.id != definition.id);
            state.definitions.push(definition);
            Ok(())
        })
    }
    /// A retried chat request reuses its saved definition without resetting its schedule.
    pub fn create_once(
        &mut self,
        mut definition: Definition,
        at: i64,
    ) -> Result<Definition, String> {
        definition.validate()?;
        if definition.revision != 0 {
            return Err("Creation requires a new definition".into());
        }
        if let Some(existing) = self
            .snapshot
            .definitions
            .iter()
            .find(|d| d.id == definition.id)
        {
            definition.revision = existing.revision;
            definition.anchor = existing.anchor;
            definition.next_at = existing.next_at;
            definition.failures = existing.failures;
            definition.paused_reason = existing.paused_reason.clone();
            return if &definition == existing {
                Ok(existing.clone())
            } else {
                Err("This request_key already names a different or edited automation. Use a new key for a new automation; edit existing ones in the sidebar".into())
            };
        }
        let id = definition.id.clone();
        self.save(definition, at)?;
        Ok(self
            .snapshot
            .definitions
            .iter()
            .find(|d| d.id == id)
            .unwrap()
            .clone())
    }
    pub fn set_enabled(&mut self, id: &str, enabled: bool, at: i64) -> Result<(), String> {
        self.commit(|state| {
            let d = state
                .definitions
                .iter_mut()
                .find(|d| d.id == id)
                .ok_or("Automation no longer exists")?;
            d.enabled = enabled;
            d.revision += 1;
            d.paused_reason = None;
            d.failures = 0;
            d.anchor = at;
            d.next_at = if enabled {
                d.schedule.next(at, at)?
            } else {
                None
            };
            if !enabled {
                for run in state.runs.iter_mut().filter(|r| {
                    r.definition.id == id
                        && r.status == RunStatus::Queued
                        && r.scheduled_for.is_some()
                }) {
                    run.status = RunStatus::Cancelled;
                    run.finished_at = Some(at);
                    run.error = Some("Schedule paused before dispatch".into());
                }
            }
            Ok(())
        })
    }
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.commit(|state| {
            if state
                .runs
                .iter()
                .any(|r| r.definition.id == id && r.status.active())
            {
                return Err("Cancel the active run before deleting this automation".into());
            }
            state.definitions.retain(|d| d.id != id);
            Ok(())
        })
    }
    pub fn enqueue(&mut self, id: &str, scheduled: bool, at: i64) -> Result<String, String> {
        self.commit(|state| {
            if state
                .runs
                .iter()
                .any(|r| r.definition.id == id && r.status.active())
            {
                return Err("This automation already has a queued or active run".into());
            }
            let definition = state
                .definitions
                .iter_mut()
                .find(|d| d.id == id)
                .ok_or("Automation no longer exists")?;
            let scheduled_for = if scheduled {
                let due = definition
                    .next_at
                    .filter(|t| *t <= at && definition.enabled)
                    .ok_or("Automation is not due")?;
                definition.next_at = definition.schedule.next(at, definition.anchor)?;
                Some(definition.schedule.latest_due(due, at, definition.anchor)?)
            } else {
                None
            };
            let id = new_id();
            state.runs.push(Run {
                session_id: format!("automation_{id}"),
                id: id.clone(),
                definition: definition.clone(),
                scheduled_for,
                created_at: at,
                finished_at: None,
                status: RunStatus::Queued,
                session_file: None,
                error: None,
                reviewed: false,
            });
            Ok(id)
        })
    }
    pub fn update_run(
        &mut self,
        id: &str,
        status: RunStatus,
        error: Option<String>,
        session_file: Option<PathBuf>,
        at: i64,
    ) -> Result<(), String> {
        self.commit(|state| {
            let run = state
                .runs
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or("Run no longer exists")?;
            if !run.status.active() {
                return Ok(());
            }
            run.status = status;
            if session_file.is_some() {
                run.session_file = session_file;
            }
            run.error = error;
            if !status.active() {
                run.finished_at = Some(at);
                if let Some(d) = state
                    .definitions
                    .iter_mut()
                    .find(|d| d.id == run.definition.id)
                {
                    if matches!(status, RunStatus::Failed | RunStatus::Interrupted) {
                        d.failures += 1;
                    }
                    if status == RunStatus::Succeeded {
                        d.failures = 0;
                    }
                    if d.failures >= 3 {
                        d.enabled = false;
                        d.revision += 1;
                        d.next_at = None;
                        d.paused_reason =
                            Some("Paused after three failed or interrupted runs".into());
                    }
                }
            }
            Ok(())
        })
    }
    pub fn mark_reviewed(&mut self, id: &str) -> Result<(), String> {
        self.commit(|state| {
            let run = state
                .runs
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or("Run no longer exists")?;
            if !run.status.active() {
                run.reviewed = true;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn definition() -> Definition {
        Definition {
            id: "test".into(),
            revision: 0,
            name: "Review".into(),
            prompt: "Review changes".into(),
            project: std::env::temp_dir(),
            model: "model".into(),
            effort: "medium".into(),
            worktree: true,
            schedule: Schedule::Interval { minutes: 1 },
            enabled: true,
            notify_all: false,
            anchor: 0,
            next_at: None,
            failures: 0,
            paused_reason: None,
        }
    }
    fn ts(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp()
    }
    #[test]
    fn calendar_skips_dst_gap_and_executes_fold_once() {
        let mut s = Schedule::Calendar {
            hour: 2,
            minute: 30,
            days: (0..7).collect(),
            timezone: "America/Toronto".into(),
        };
        assert_eq!(
            s.next(ts("2026-03-08T00:00:00Z"), 0).unwrap(),
            Some(ts("2026-03-09T06:30:00Z"))
        );
        if let Schedule::Calendar { hour, .. } = &mut s {
            *hour = 1;
        }
        let first = s.next(ts("2026-11-01T00:00:00Z"), 0).unwrap().unwrap();
        assert_eq!(first, ts("2026-11-01T05:30:00Z"));
        assert_eq!(s.next(first, 0).unwrap(), Some(ts("2026-11-02T06:30:00Z")));
    }
    #[test]
    fn claims_survive_restart_coalesce_and_preserve_revision_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        assert!(Store::open(dir.path()).is_err());
        store.save(definition(), 0).unwrap();
        let run = store.enqueue("test", true, 3600).unwrap();
        assert_eq!(store.snapshot().runs[0].scheduled_for, Some(3600));
        assert_eq!(store.snapshot().definitions[0].next_at, Some(3660));
        assert!(store.enqueue("test", false, 3601).is_err());
        let mut edit = store.snapshot().definitions[0].clone();
        edit.prompt = "New prompt".into();
        store.save(edit.clone(), 3601).unwrap();
        assert!(store.save(edit, 3601).is_err());
        assert_eq!(store.snapshot().runs[0].definition.prompt, "Review changes");
        drop(store);
        let mut store = Store::open(dir.path()).unwrap();
        assert_eq!(store.snapshot().runs[0].id, run);
        store
            .update_run(&run, RunStatus::Interrupted, None, None, 3700)
            .unwrap();
        store.set_enabled("test", false, 3800).unwrap();
        let manual = store.enqueue("test", false, 3900).unwrap();
        assert!(!store.snapshot().definitions[0].enabled);
        store
            .update_run(&manual, RunStatus::Succeeded, None, None, 3901)
            .unwrap();
        store.delete("test").unwrap();
        assert_eq!(store.snapshot().runs.len(), 2);
    }
    #[test]
    fn corrupt_storage_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.json"), b"broken").unwrap();
        assert!(Store::open(dir.path()).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            b"broken"
        );
    }

    #[test]
    fn chat_creation_retry_does_not_reset_or_overwrite_the_saved_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let saved = store.create_once(definition(), 1000).unwrap();
        assert_eq!(saved.next_at, Some(1060));
        drop(store);
        let mut store = Store::open(dir.path()).unwrap();
        assert_eq!(store.create_once(definition(), 2000).unwrap(), saved);
        let mut changed = definition();
        changed.prompt = "Different work".into();
        assert!(store.create_once(changed, 2000).is_err());
        assert_eq!(store.snapshot().definitions, vec![saved]);
    }

    #[test]
    fn pause_cancels_queued_occurrences_and_failures_pause_without_double_counting() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.save(definition(), 0).unwrap();
        let queued = store.enqueue("test", true, 60).unwrap();
        store.set_enabled("test", false, 61).unwrap();
        assert_eq!(store.snapshot().runs[0].status, RunStatus::Cancelled);
        store.set_enabled("test", true, 1000).unwrap();
        assert_eq!(store.snapshot().definitions[0].next_at, Some(1060));
        for at in 1..=3 {
            let run = store.enqueue("test", false, 1100 + at).unwrap();
            store
                .update_run(
                    &run,
                    RunStatus::Failed,
                    Some("Failed".into()),
                    None,
                    1200 + at,
                )
                .unwrap();
            store
                .update_run(&run, RunStatus::Failed, None, None, 1200 + at)
                .unwrap();
        }
        assert_eq!(store.snapshot().definitions[0].failures, 3);
        assert!(!store.snapshot().definitions[0].enabled);
        assert!(store.snapshot().definitions[0].paused_reason.is_some());
        assert_eq!(store.snapshot().runs[0].id, queued);
        let mut edit = store.snapshot().definitions[0].clone();
        edit.name = "Renamed".into();
        edit.paused_reason = None;
        store.save(edit, 1300).unwrap();
        assert_eq!(store.snapshot().definitions[0].failures, 3);
        assert!(store.snapshot().definitions[0].paused_reason.is_some());
        let mut edit = store.snapshot().definitions[0].clone();
        edit.enabled = true;
        store.save(edit, 1400).unwrap();
        assert_eq!(store.snapshot().definitions[0].failures, 0);
        assert!(store.snapshot().definitions[0].paused_reason.is_none());
        assert_eq!(store.snapshot().definitions[0].next_at, Some(1460));
        let run = store.enqueue("test", true, 1460).unwrap();
        store.update_run(&run, RunStatus::Failed, None, None, 1461).unwrap();
        assert!(store.snapshot().definitions[0].enabled);
    }

    #[test]
    fn retention_preserves_active_unreviewed_and_newest_reviewed_runs_per_definition() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.save(definition(), 0).unwrap();
        store.enqueue("test", false, 1).unwrap();
        let template = store.snapshot().runs[0].clone();
        store.commit(|state| {
            state.runs.clear();
            for project in ["test", "deleted"] {
                for n in 0..205 {
                    let mut run = template.clone();
                    run.id = format!("{project}-{n}");
                    run.session_id = format!("automation_{}", run.id);
                    run.definition.id = project.into();
                    run.status = if n == 0 { RunStatus::Running } else { RunStatus::Succeeded };
                    run.reviewed = n != 1;
                    state.runs.push(run);
                }
            }
            Ok(())
        }).unwrap();
        drop(store);
        let store = Store::open(dir.path()).unwrap();
        let expected: Vec<_> = ["test", "deleted"].into_iter()
            .flat_map(|project| [0, 1].into_iter().chain(5..205)
                .map(move |n| format!("{project}-{n}"))).collect();
        assert_eq!(store.snapshot().runs.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn save_rejects_external_agent_models() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut definition = definition();
        definition.model = "acp/test".into();
        assert!(store.save(definition, 0).unwrap_err().contains("external agents"));
        assert!(store.snapshot().definitions.is_empty());
    }

    #[test]
    fn failed_save_does_not_publish_uncommitted_definition() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        std::fs::create_dir(dir.path().join("state.json")).unwrap();
        assert!(store.save(definition(), 0).is_err());
        assert!(store.snapshot().definitions.is_empty());
        assert_eq!(store.snapshot().revision, 0);
    }
}
