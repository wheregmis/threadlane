//! The database gate of Plan.md §18.1.
//!
//! Repeatedly kills a process while it commits transactions, then checks that
//! the database opens, passes Turso's and SQLite's integrity checks, contains
//! every transaction reported committed, and contains no part of any other.
//!
//! The default run is short so it fits in `cargo test`. Run the full gate
//! (thousands of kills) with:
//!
//! ```sh
//! THREADLANE_CRASH_GATE_ITERATIONS=3000 cargo test --release -p threadlane-store \
//!     --test crash_gate -- --nocapture
//! ```

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use threadlane_core::model::TaskStatus;
use threadlane_core::store::{InboxStore, OrgStore, TaskStore};
use threadlane_core::time::SystemClock;
use threadlane_store::{Bootstrap, HubDb};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

const DEFAULT_ITERATIONS: u32 = 20;
/// Kill delays are drawn from `0..MAX_KILL_DELAY_MS` after spawn, so some kills
/// land while the database opens and most land mid-commit.
const MAX_KILL_DELAY_MS: u64 = 120;

/// Small deterministic generator so a failing iteration can be reproduced
/// from its printed seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

fn iterations() -> u32 {
    std::env::var("THREADLANE_CRASH_GATE_ITERATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_ITERATIONS)
}

fn seed() -> u64 {
    std::env::var("THREADLANE_CRASH_GATE_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0x7468_7265_6164)
}

/// Spawns the writer, kills it after `delay`, and returns the sequence
/// numbers it reported committed.
async fn run_and_kill(db: &Path, first: i64, delay: Duration) -> BTreeSet<i64> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_threadlane-crash-writer"))
        .arg(db)
        .arg(first.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("writer spawns");
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut committed = BTreeSet::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(n) = line.strip_prefix("committed ") {
                committed.insert(n.parse::<i64>().expect("sequence number"));
            }
        }
        committed
    });
    tokio::time::sleep(delay).await;
    if let Ok(Some(status)) = child.try_wait() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            use tokio::io::AsyncReadExt;
            let _ = pipe.read_to_string(&mut stderr).await;
        }
        panic!("writer exited on its own with {status}: {stderr}");
    }
    child.kill().await.expect("writer is killed");
    // Everything printed before the kill is still in the pipe.
    reader.await.expect("reader finishes")
}

/// Copies the database and its WAL so SQLite can check them without touching
/// the files Turso will open next.
fn copy_for_sqlite(db: &Path, into: &Path) -> PathBuf {
    let target = into.join("copy.db");
    for suffix in ["", "-wal"] {
        let from = PathBuf::from(format!("{}{suffix}", db.display()));
        let to = PathBuf::from(format!("{}{suffix}", target.display()));
        let _ = std::fs::remove_file(&to);
        if from.exists() {
            std::fs::copy(&from, &to).expect("copy database file");
        }
    }
    let _ = std::fs::remove_file(format!("{}-shm", target.display()));
    target
}

fn sqlite_check(db: &Path) -> (String, i64) {
    let conn = rusqlite::Connection::open(db).expect("SQLite opens the file");
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("SQLite integrity check runs");
    let tasks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .expect("SQLite reads tasks");
    (integrity, tasks)
}

/// Checks the database after a kill and returns the next sequence to use.
async fn verify(db: &Path, reported: &BTreeSet<i64>, iteration: u32) -> i64 {
    let hub = HubDb::open(db, &Bootstrap::default(), Arc::new(SystemClock))
        .await
        .unwrap_or_else(|error| panic!("iteration {iteration}: database does not open: {error}"));
    let problems = hub.integrity_check().await.expect("integrity check runs");
    assert!(problems.is_empty(), "iteration {iteration}: {problems:?}");

    let general = hub.general_project().await.expect("general project");
    let tasks = hub.tasks(general.id).await.expect("tasks");
    let mut present = BTreeSet::new();
    for task in &tasks {
        let n: i64 = task
            .created_by_call
            .as_deref()
            .and_then(|call| call.strip_prefix("gate-"))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("iteration {iteration}: unexpected task {task:?}"));
        present.insert(n);
        // All-or-nothing: a committed task has its lane and its start event.
        assert_eq!(task.status, TaskStatus::Queued);
        let lane = hub
            .lane(task.lane_id)
            .await
            .expect("lane query")
            .unwrap_or_else(|| panic!("iteration {iteration}: task {} has no lane", task.id));
        assert_eq!(lane.task_id, Some(task.id), "iteration {iteration}");
        let events = hub.pending_events(task.lane_id).await.expect("events");
        assert_eq!(
            events.len(),
            1,
            "iteration {iteration}: task {} has {} start events",
            task.id,
            events.len()
        );
    }
    let missing: Vec<_> = reported.difference(&present).collect();
    assert!(
        missing.is_empty(),
        "iteration {iteration}: reported committed but missing: {missing:?}"
    );
    present.last().map_or(0, |last| last + 1)
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_writers_never_lose_or_tear_transactions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let scratch = tempfile::tempdir().expect("scratch dir");
    let db = dir.path().join("hub.db");
    let iterations = iterations();
    let seed = seed();
    let mut rng = Lcg(seed);
    let mut next = 0;
    let mut total_reported = 0;
    for iteration in 0..iterations {
        let delay = Duration::from_millis(rng.next() % MAX_KILL_DELAY_MS);
        let reported = run_and_kill(&db, next, delay).await;
        total_reported += reported.len();

        if db.exists() {
            let copy = copy_for_sqlite(&db, scratch.path());
            let (integrity, sqlite_tasks) = sqlite_check(&copy);
            assert_eq!(
                integrity, "ok",
                "iteration {iteration}: SQLite integrity check"
            );
            assert!(
                sqlite_tasks >= reported.len() as i64,
                "iteration {iteration}: SQLite sees fewer tasks than were reported"
            );
        }
        next = verify(&db, &reported, iteration).await.max(next);
        if (iteration + 1) % 100 == 0 {
            eprintln!(
                "crash gate: {}/{iterations} kills, {total_reported} commits reported",
                iteration + 1
            );
        }
    }
    eprintln!(
        "crash gate passed: {iterations} kills (seed {seed}), {total_reported} commits reported, \
         {next} sequence numbers used"
    );
    assert!(
        total_reported > 0,
        "no commit was reported; kill delays are too short for this machine"
    );
}
