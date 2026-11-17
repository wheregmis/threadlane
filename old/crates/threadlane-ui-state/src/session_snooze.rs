//! Per-project snooze state for the sidebar's `Snoozed` organization group.
//!
//! `.threadlane/session_snooze.json` maps session id -> `{ wake_at, baseline }`:
//! `wake_at` is an absolute unix-second deadline (durations mean elapsed hours,
//! so sleep and restart do not shift the return time) and `baseline` is the
//! completion token captured when the snooze was recorded, so a newer confirmed
//! completion can end the snooze early.
//!
//! Snoozing is a presentation-only sidebar action: it never schedules, stops,
//! or delays an agent, and it is local-only (remote-daemon sessions are
//! ineligible). Records are write-confirmation gated: a record saved by the
//! serialized writer is *pending* until that exact revision is acknowledged,
//! and only confirmed records hide a row. A stale acknowledgment therefore
//! cannot reinstate a snooze over an unsnooze or a newer edit, and a failed
//! write never hides the row.
//!
//! Writes reuse `SessionSeenWriter`'s serialized job/result machinery and the
//! same tmp-file + rename atomic replacement.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use threadlane_daemon::RunCompletionToken;

use crate::session_seen::{SeenWriteResult, SessionSeenWriter};

const SCHEMA_VERSION: u32 = 1;

/// Wall-clock unix seconds — deadlines are absolute so sleep/restart never
/// shifts a return time.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// Fixed durations offered by the sidebar Snooze menu: `(label, seconds)`.
/// The deadline is computed at activation time, never when a row mounted.
pub const SNOOZE_OPTIONS: &[(&str, u64)] = &[
    ("For 1 hour", 60 * 60),
    ("For 4 hours", 4 * 60 * 60),
    ("For 24 hours", 24 * 60 * 60),
];

/// Resolved local-time label for a return deadline — a textual deadline,
/// not a color-only cue: "3:45 PM" today, "Tomorrow, 3:45 PM" tomorrow,
/// "Tue, Jan 6, 3:45 PM" beyond. `None` only if `wake_at` overflows the
/// local clock representation.
pub fn snooze_return_label(wake_at: u64) -> String {
    let Some(wake) = chrono::DateTime::from_timestamp(wake_at as i64, 0) else {
        return "unknown time".to_string();
    };
    let wake = wake.with_timezone(&chrono::Local);
    return_label(wake.naive_local(), chrono::Local::now().date_naive())
}

fn return_label(wake: chrono::NaiveDateTime, today: chrono::NaiveDate) -> String {
    let time = wake.format("%-I:%M %p").to_string();
    let day = wake.date();
    if day == today {
        time
    } else if Some(day) == today.succ_opt() {
        format!("Tomorrow, {time}")
    } else {
        wake.format("%a, %b %-d").to_string() + &format!(", {time}")
    }
}

/// One snooze record: an absolute deadline plus the completion baseline the
/// snooze was captured against.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnoozeRecord {
    /// Unix seconds at which the session returns to normal grouping.
    pub wake_at: u64,
    /// Newest confirmed Run completion at snooze time. A confirmed
    /// `Latest` token with a higher seq ends the snooze; `None` means any
    /// confirmed completion is newer work.
    pub baseline: Option<RunCompletionToken>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SessionSnoozeFile {
    version: u32,
    sessions: HashMap<String, SnoozeRecord>,
}

/// What a drained writer result did to the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnoozeWriteOutcome {
    /// The newest submitted revision was confirmed; pending records at or
    /// below it may now hide their rows.
    Confirmed,
    /// A superseded write finished — a newer revision is still in flight,
    /// so neither the ack nor the failure changes record state.
    Stale,
    /// The newest in-flight write failed; records stay pending and the row
    /// stays unhidden. The store is re-marked dirty so the next flush
    /// retries the full serialization.
    Failed,
}

/// Per-project snooze records. `pending` maps session id -> the revision at
/// which its current record last changed; a record hides its row only once
/// `confirmed` reaches that revision.
pub struct SessionSnoozeStore {
    /// Canonical project directory this store belongs to.
    work_dir: PathBuf,
    path: PathBuf,
    records: HashMap<String, SnoozeRecord>,
    pending: HashMap<String, u64>,
    /// Bumped every time a record changes; identifies the serialization a
    /// write job carries.
    revision: u64,
    /// Highest revision the writer has confirmed.
    confirmed: u64,
    /// Submitted revisions awaiting a writer result, in submission order.
    in_flight: VecDeque<u64>,
    dirty: bool,
    /// Set when the newest in-flight write failed; cleared by the next
    /// confirmed revision. Per-store, so it flags any pending record.
    save_failed: bool,
}

impl SessionSnoozeStore {
    /// Loads the store for a project. A missing file starts empty. An
    /// unreadable or version-mismatched file is renamed aside for diagnosis
    /// (`session_snooze.unreadable.json`) and the store starts empty —
    /// expired or invalid records are dropped rather than allowed to hide
    /// anything.
    pub fn load(work_dir: &Path, now: u64) -> Self {
        let path = work_dir.join(".threadlane/session_snooze.json");
        let mut dirty = false;
        let records = match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<SessionSnoozeFile>(&content) {
                Ok(file) if file.version == SCHEMA_VERSION => file
                    .sessions
                    .into_iter()
                    .filter(|(session_id, record)| {
                        // An out-of-range deadline cannot render a return
                        // time or be reached sanely — drop it like an
                        // expired record instead of hiding the row forever.
                        let keep = record.wake_at > now
                            && i64::try_from(record.wake_at).is_ok()
                            && !session_id.is_empty();
                        dirty |= !keep;
                        keep
                    })
                    .collect(),
                _ => {
                    // Preserve the unreadable file for diagnosis instead of
                    // silently overwriting whatever it held. A previous
                    // diagnostic copy must survive, so an existing target
                    // moves the new copy to a `.{now}.{n}` name.
                    let mut unreadable =
                        path.with_file_name("session_snooze.unreadable.json");
                    let mut suffix = 0_u32;
                    while unreadable.exists() {
                        suffix += 1;
                        unreadable = path.with_file_name(format!(
                            "session_snooze.unreadable.{now}.{suffix}.json"
                        ));
                    }
                    if let Err(error) = std::fs::rename(&path, &unreadable) {
                        tracing::warn!(
                            "could not preserve unreadable {}: {error}",
                            path.display()
                        );
                    }
                    HashMap::new()
                }
            },
            Err(_) => HashMap::new(),
        };
        Self {
            work_dir: work_dir.to_path_buf(),
            path,
            records,
            pending: HashMap::new(),
            revision: 0,
            confirmed: 0,
            in_flight: VecDeque::new(),
            dirty,
            save_failed: false,
        }
    }

    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn records(&self) -> &HashMap<String, SnoozeRecord> {
        &self.records
    }

    /// Whether the newest write for this store failed.
    pub fn save_failed(&self) -> bool {
        self.save_failed
    }

    /// Whether the failed write carried a deletion. The removed record has
    /// no row left to offer the menu retry, so the drain resubmits it —
    /// otherwise the stale on-disk record would hide the session again on
    /// the next load.
    pub fn failed_delete_dirty(&self) -> bool {
        self.save_failed && self.dirty && self.records.is_empty()
    }

    /// The record a row should respect: confirmed records hide, pending
    /// ones only show "Saving snooze…" without moving the row.
    pub fn record(&self, session_id: &str) -> Option<(&SnoozeRecord, bool)> {
        self.records
            .get(session_id)
            .map(|record| (record, self.pending.contains_key(session_id)))
    }

    /// Inserts or replaces a snooze. The record is pending until the write
    /// carrying `revision` is acknowledged.
    pub fn snooze(&mut self, session_id: &str, record: SnoozeRecord) {
        self.revision += 1;
        self.records.insert(session_id.to_string(), record);
        self.pending.insert(session_id.to_string(), self.revision);
        self.dirty = true;
    }

    /// Drops a record — unsnooze, expiry, ended-by-new-work, or confirmed
    /// session removal. In-memory removal is fail-open: the row rejoins its
    /// normal grouping immediately and the delete is persisted on the next
    /// flush. Absence from discovery alone never calls this.
    pub fn remove(&mut self, session_id: &str) -> bool {
        let removed = self.records.remove(session_id).is_some();
        self.pending.remove(session_id);
        if removed {
            // A delete is a state transition like a write: bump the
            // revision so the next flush can never be mistaken for the
            // pre-delete serialization by a stale acknowledgment.
            self.revision += 1;
            self.dirty = true;
        }
        removed
    }

    /// Earliest deadline still tracked, pending or confirmed — drives the
    /// sidebar's single wake-up task.
    pub fn next_deadline(&self) -> Option<u64> {
        self.records.values().map(|record| record.wake_at).min()
    }

    /// Serialized snapshot pending write, with the revision it captures.
    /// Records the revision as in-flight; `write_submit_failed` rolls that
    /// back if the writer is gone. Re-marked dirty on a failed write via
    /// [`Self::apply_write_result`].
    pub fn take_dirty_json(&mut self) -> Option<(String, u64)> {
        if !self.dirty {
            return None;
        }
        let file = SessionSnoozeFile {
            version: SCHEMA_VERSION,
            sessions: self.records.clone(),
        };
        match serde_json::to_string_pretty(&file) {
            Ok(json) => {
                self.dirty = false;
                self.in_flight.push_back(self.revision);
                Some((json, self.revision))
            }
            Err(error) => {
                tracing::warn!("failed to serialize {}: {error}", self.path.display());
                None
            }
        }
    }

    /// The writer channel died mid-submit: the revision never ran, so it is
    /// no longer in flight and the store stays dirty for the next flush.
    pub fn write_submit_failed(&mut self, revision: u64) {
        self.in_flight.retain(|rev| *rev != revision);
        self.dirty = true;
    }

    /// Applies a drained writer result. Acks are cumulative — the single
    /// writer thread runs jobs in order, so confirming `revision` confirms
    /// every earlier one. A result is stale (neither ack nor failure counts)
    /// whenever a newer revision is still in flight.
    pub fn apply_write_result(
        &mut self,
        revision: u64,
        error: Option<String>,
    ) -> SnoozeWriteOutcome {
        self.in_flight.retain(|rev| *rev != revision);
        if self.in_flight.iter().any(|rev| *rev > revision) {
            return SnoozeWriteOutcome::Stale;
        }
        match error {
            None => {
                self.confirmed = self.confirmed.max(revision);
                let confirmed = self.confirmed;
                self.pending.retain(|_, rev| *rev > confirmed);
                self.save_failed = false;
                SnoozeWriteOutcome::Confirmed
            }
            Some(error) => {
                tracing::warn!(
                    "session_snooze write failed for {}: {error}",
                    self.path.display()
                );
                // Nothing was persisted at this revision: every record still
                // pending stays pending and unhidden. Re-arm the dirty flag
                // so the next flush retries the full state.
                self.dirty = true;
                self.save_failed = true;
                SnoozeWriteOutcome::Failed
            }
        }
    }
}

/// What a row should show for a snoozed session, as seen by the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionSnooze {
    /// Absolute unix-second return time.
    pub wake_at: u64,
    /// `true` while the record's write is unacknowledged: the row stays in
    /// its normal group and shows "Saving snooze…" instead of moving.
    pub pending: bool,
    /// `true` when the newest write failed — the row reports the failure
    /// and the menu offers retry, but the snooze is never claimed saved.
    pub save_failed: bool,
}

/// Re-export of the shared serialized-writer result so AppState can keep a
/// second dedicated writer instance without a new channel type.
pub type SnoozeWriteResult = SeenWriteResult;
pub type SessionSnoozeWriter = SessionSeenWriter;

#[cfg(test)]
mod tests {
    use crate::session_snooze::{
        return_label, unix_now, SessionSnoozeFile, SessionSnoozeStore, SnoozeRecord,
        SnoozeWriteOutcome, SNOOZE_OPTIONS,
    };
    use chrono::{NaiveDate, NaiveDateTime};

    fn record(wake_at: u64, seq: Option<u64>) -> SnoozeRecord {
        SnoozeRecord {
            wake_at,
            baseline: seq.map(|seq| crate::RunCompletionToken {
                record_id: format!("finish-{seq}"),
                run_id: format!("run-{seq}"),
                seq,
            }),
        }
    }

    fn store(dir: &std::path::Path, now: u64) -> SessionSnoozeStore {
        SessionSnoozeStore::load(dir, now)
    }

    #[test]
    fn load_drops_expired_and_empty_id_records() {
        let now = unix_now();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".threadlane")).unwrap();
        std::fs::write(
            dir.path().join(".threadlane/session_snooze.json"),
            serde_json::to_string(&SessionSnoozeFile {
                version: 1,
                sessions: std::collections::HashMap::from([
                    ("live".into(), record(now + 600, None)),
                    ("expired".into(), record(now.saturating_sub(1), None)),
                    ("far-future".into(), record(u64::MAX, None)),
                    ("".into(), record(now + 600, None)),
                ]),
            })
            .unwrap(),
        )
        .unwrap();

        let mut store = store(dir.path(), now);
        assert!(store.record("live").is_some());
        assert!(store.record("expired").is_none());
        assert!(
            store.record("far-future").is_none(),
            "out-of-range deadlines fail open"
        );
        assert!(store.record("").is_none());
        // The dropped metadata rewrites itself on the next flush.
        assert!(store.take_dirty_json().is_some());
    }

    #[test]
    fn unreadable_metadata_is_preserved_for_diagnosis() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".threadlane")).unwrap();
        std::fs::write(
            dir.path().join(".threadlane/session_snooze.json"),
            "{\"version\": 99, \"sessions\": {}}",
        )
        .unwrap();

        let store = store(dir.path(), unix_now());
        assert!(store.records().is_empty());
        assert!(
            dir.path()
                .join(".threadlane/session_snooze.unreadable.json")
                .exists(),
            "unreadable file must be renamed aside, not overwritten"
        );
        assert!(!dir.path().join(".threadlane/session_snooze.json").exists());
    }

    #[test]
    fn a_new_snooze_is_pending_until_the_write_is_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path(), unix_now());
        store.snooze("session", record(unix_now() + 3600, Some(3)));

        let (seen, pending) = store.record("session").unwrap();
        assert!(pending);
        assert_eq!(seen.baseline.as_ref().map(|token| token.seq), Some(3));

        let (json, revision) = store.take_dirty_json().expect("dirty after snooze");
        assert!(json.contains("\"session\""));
        assert!(store.take_dirty_json().is_none(), "nothing new to write");

        let outcome = store.apply_write_result(revision, None);
        assert_eq!(outcome, SnoozeWriteOutcome::Confirmed);
        let (_, pending) = store.record("session").unwrap();
        assert!(!pending);
        assert!(!store.save_failed());
    }

    #[test]
    fn a_stale_ack_cannot_reinstate_a_superseded_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path(), unix_now());
        store.snooze("session", record(unix_now() + 3600, None));
        let (_, first) = store.take_dirty_json().unwrap();
        store.snooze("session", record(unix_now() + 7200, Some(9)));
        let (_, second) = store.take_dirty_json().unwrap();
        assert!(second > first);

        // The older result must not confirm the newer record.
        assert_eq!(store.apply_write_result(first, None), SnoozeWriteOutcome::Stale);
        let (seen, pending) = store.record("session").unwrap();
        assert!(pending, "newer revision is still unacknowledged");
        assert_eq!(seen.wake_at, unix_now() + 7200);

        assert_eq!(
            store.apply_write_result(second, None),
            SnoozeWriteOutcome::Confirmed
        );
        let (_, pending) = store.record("session").unwrap();
        assert!(!pending);
    }

    #[test]
    fn a_failed_write_keeps_records_pending_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path(), unix_now());
        store.snooze("session", record(unix_now() + 3600, None));
        let (json, revision) = store.take_dirty_json().unwrap();
        drop(json);

        let outcome = store.apply_write_result(revision, Some("read-only fs".into()));
        assert_eq!(outcome, SnoozeWriteOutcome::Failed);
        assert!(store.save_failed());
        let (_, pending) = store.record("session").unwrap();
        assert!(pending, "failure never claims a saved snooze");
        assert!(
            store.take_dirty_json().is_some(),
            "failed state re-arms dirty for retry"
        );
    }

    #[test]
    fn remove_is_fail_open_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path(), unix_now());
        store.snooze("session", record(unix_now() + 3600, None));
        let (_, revision) = store.take_dirty_json().unwrap();
        assert_eq!(
            store.apply_write_result(revision, None),
            SnoozeWriteOutcome::Confirmed
        );

        assert!(store.remove("session"));
        assert!(!store.remove("session"), "already gone");
        assert!(store.record("session").is_none());
        assert!(store.take_dirty_json().is_some(), "delete persists");
    }

    #[test]
    fn a_failed_delete_re_arms_for_the_drain_retry() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path(), unix_now());
        store.snooze("session", record(unix_now() + 3600, None));
        let (_, revision) = store.take_dirty_json().unwrap();
        assert_eq!(
            store.apply_write_result(revision, None),
            SnoozeWriteOutcome::Confirmed
        );

        store.remove("session");
        let (_, delete_revision) = store.take_dirty_json().unwrap();
        assert_eq!(
            store.apply_write_result(delete_revision, Some("read-only fs".into())),
            SnoozeWriteOutcome::Failed
        );
        assert!(store.failed_delete_dirty());
        assert!(
            store.take_dirty_json().is_some(),
            "a failed deletion resubmits — the stale file must not come back on reload"
        );
    }

    #[test]
    fn remove_while_pending_keeps_the_row_visible_and_drops_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path(), unix_now());
        store.snooze("session", record(unix_now() + 3600, None));
        assert!(store.remove("session"));
        assert!(store.record("session").is_none());
    }

    #[test]
    fn next_deadline_tracks_the_earliest_return() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path(), unix_now());
        assert!(store.next_deadline().is_none());
        store.snooze("far", record(unix_now() + 7200, None));
        store.snooze("near", record(unix_now() + 100, None));
        assert_eq!(store.next_deadline(), Some(unix_now() + 100));
        store.remove("near");
        assert_eq!(store.next_deadline(), Some(unix_now() + 7200));
    }

    #[test]
    fn write_submit_failed_requeues_the_revision() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(dir.path(), unix_now());
        store.snooze("session", record(unix_now() + 3600, None));
        let (_, revision) = store.take_dirty_json().unwrap();
        store.write_submit_failed(revision);
        let (_, pending) = store.record("session").unwrap();
        assert!(pending);
        assert!(store.take_dirty_json().is_some(), "submit failure re-dirties");
    }

    #[test]
    fn fixed_options_are_one_four_and_twentyfour_hours() {
        let labels: Vec<&str> = SNOOZE_OPTIONS.iter().map(|(label, _)| *label).collect();
        let secs: Vec<u64> = SNOOZE_OPTIONS.iter().map(|(_, secs)| *secs).collect();
        assert_eq!(labels, vec!["For 1 hour", "For 4 hours", "For 24 hours"]);
        assert_eq!(secs, vec![3_600, 14_400, 86_400]);
    }

    #[test]
    fn return_label_uses_local_clock_phrasing() {
        let wake_same_day = NaiveDateTime::new(
            NaiveDate::from_ymd_opt(2027, 1, 2).unwrap(),
            chrono::NaiveTime::from_hms_opt(15, 30, 0).unwrap(),
        );
        let same = return_label(wake_same_day, wake_same_day.date());
        assert!(!same.contains("Tomorrow"));
        assert!(same.contains("3:30"), "{same}");

        let tomorrow = return_label(
            NaiveDateTime::new(
                NaiveDate::from_ymd_opt(2027, 1, 2).unwrap(),
                chrono::NaiveTime::from_hms_opt(9, 5, 0).unwrap(),
            ),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
        );
        assert!(tomorrow.starts_with("Tomorrow, "), "{tomorrow}");

        let later = return_label(
            NaiveDateTime::new(
                NaiveDate::from_ymd_opt(2027, 1, 4).unwrap(),
                chrono::NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
            ),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
        );
        assert!(later.contains("Jan 4"), "{later}");
    }
}
