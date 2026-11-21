//! Hand-built operational history with deliberately independent timestamp samples.

use std::sync::Arc;

use threadlane_core::ids::LaneId;
use threadlane_core::model::{NewOutboxMessage, OutboxKind, OutboxMessage, Urgency};
use threadlane_core::store::{MemoryStore, OutboxStore};
use threadlane_core::time::{Clock, Sleep, Timestamp};

use super::*;
use crate::Bootstrap;
use crate::db::{Db, DbLocation, db_err};
use crate::migrate::MIGRATIONS;

const NOW: Timestamp = Timestamp::from_millis(172_800_000);
const GENERAL: ProjectId = ProjectId(1);

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        NOW
    }

    fn sleep_until(&self, _deadline: Timestamp) -> Sleep {
        Box::pin(std::future::pending())
    }
}

async fn history() -> HubDb {
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(FixedClock))
        .await
        .unwrap();
    let tx = db.db.begin().await.unwrap();
    tx.conn()
        .execute_batch(
            "INSERT INTO projects (id, name, max_parallel_tasks, created_at)
               VALUES (2, 'Other', 1, 0), (3, 'Empty', 1, 0);
             INSERT INTO lanes (id, project_id, kind) VALUES
               (2, 2, 'lead'), (3, 1, 'task'), (4, 3, 'lead');
             INSERT INTO events (project_id, lane_id, source, source_event_id, kind, payload,
                                 trust, requested, status, received_at) VALUES
               (1, 1, 'test', '1', 'user_message', '{}', 'user', 1, 'pending', 172799700),
               (1, 1, 'test', '2', 'user_message', '{}', 'user', 1, 'pending', 172799900),
               (1, 3, 'test', '3', 'user_message', '{}', 'user', 1, 'pending', 172799990),
               (1, 3, 'test', '4', 'user_message', '{}', 'user', 1, 'claimed', 0),
               (2, 2, 'test', '5', 'user_message', '{}', 'user', 1, 'pending', 172799995),
               (1, 1, 'test', '6', 'user_message', '{}', 'user', 1, 'dead', 0),
               (2, 2, 'test', '7', 'user_message', '{}', 'user', 1, 'dead', 0),
               (1, 1, 'test', '8', 'user_message', '{}', 'user', 1, 'dead', -432000000),
               (1, 1, 'timer', '1:10', 'timer_fired', '{\"occurrence\":10}', 'system', 1, 'done', 20),
               (1, 1, 'timer', '1:30', 'timer_fired', '{\"occurrence\":30}', 'system', 1, 'done', 50);
             INSERT INTO timers (project_id, lane_id, category, due_at, instruction, status,
                                 created_by, created_by_call, last_fired_at)
               VALUES (1, 1, 'follow_up', 100, '', 'done', 'system', 'decision:4', 130);
             INSERT INTO sessions (id, lane_id, status, created_at, last_active_at)
               VALUES (1, 1, 'open', 0, 0), (2, 2, 'open', 0, 0);
             INSERT INTO runs (lane_id, session_id, machine_id, lease_epoch, status, outcome, started_at, ended_at)
               VALUES (1, 1, 1, 1, 'committed', 'silent', 10, 20),
                      (1, 1, 1, 1, 'committed', 'sent', 30, 50),
                      (1, 1, 1, 1, 'failed', NULL, 60, 90),
                      (1, 1, 1, 1, 'active', NULL, 200, NULL),
                      (2, 2, 1, 1, 'committed', 'silent', 0, 1000);
             INSERT INTO accounts (label, backend, cooldown_until)
               VALUES ('cooling', 'claude', 172800050), ('expired', 'claude', 172800000);
             INSERT INTO outbox (project_id, tool_call_id, kind, channel, body, urgency, status, created_at, sent_at)
               VALUES
                 (NULL, 'system:account:1:blocked', 'system_notice', 'client', '{}', 'high', 'sent', 100, 110),
                 (NULL, 'system:account:1:resumed', 'system_notice', 'client', '{}', 'high', 'sent', 150, 160),
                 (NULL, 'system:account:2:blocked', 'system_notice', 'client', '{}', 'high', 'sent', 172799980, 172799981),
                 (1, NULL, 'message', 'client', '{}', 'normal', 'sent', 1, 10),
                 (1, NULL, 'message', 'client', '{}', 'normal', 'sent', 1, 86400000),
                 (1, NULL, 'message', 'client', '{}', 'normal', 'sent', 1, 86400001),
                 (1, NULL, 'decision', 'client', '{}', 'normal', 'sent', 1, 10),
                 (1, NULL, 'message', 'client', '{}', 'normal', 'pending', 172799990, NULL),
                 (1, NULL, 'message', 'client', '{}', 'normal', 'failed', 172799975, NULL),
                 (1, NULL, 'message', 'client', '{}', 'normal', 'dead', 0, NULL),
                 (2, NULL, 'message', 'client', '{}', 'normal', 'pending', 172799950, NULL);
             INSERT INTO decisions (project_id, lane_id, tool, arguments, arguments_hash, kind, question,
                                    options, status, created_at, answered_at, answer)
               VALUES (1, 1, 'ask_user', '{}', '', 'input', '?', '[]', 'answered', 86400000, 86400012, '{\"option\":0}'),
                      (1, 1, 'ask_user', '{}', '', 'input', '?', '[]', 'answered', 172799970, 172799995, NULL),
                      (1, 1, 'ask_user', '{}', '', 'input', '?', '[]', 'pending', 172800000, NULL, NULL),
                      (1, 1, 'ask_user', '{}', '', 'input', '?', '[]', 'expired', 0, 130, NULL),
                      (2, 2, 'ask_user', '{}', '', 'input', '?', '[]', 'pending', 172799999, NULL, NULL),
                      (1, 1, 'ask_user', '{}', '', 'input', '?', '[]', 'answered', 172799980, 172799980,
                       '{\"option\":0,\"classifier\":\"allow\"}');
             INSERT INTO agents (id, name, role) VALUES (99, 'Metrics fixture', 'lead');
             INSERT INTO standing_rules (project_id, agent_id, tool, pattern, tier, created_at, revoked_at)
               VALUES (1, 99, 'a', '{}', 2, 10, NULL), (1, 99, 'a', '{}', 2, 10, 20),
                      (2, 99, 'a', '{}', 2, 10, NULL);",
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    db
}

async fn metrics() -> OperationsMetrics {
    history()
        .await
        .operations_metrics(Some(GENERAL))
        .await
        .unwrap()
}

#[tokio::test]
async fn lane_lag_uses_oldest_pending_event_per_lane() {
    let metrics = metrics().await;
    assert_eq!(metrics.at, NOW);
    assert_eq!(
        metrics.lane_lag_ms,
        [(LaneId(1), 300), (LaneId(3), 10)].into()
    );
}

#[tokio::test]
async fn queue_depth_counts_lanes_with_pending_work_and_includes_empty_projects() {
    let db = history().await;
    assert_eq!(
        db.operations_metrics(None).await.unwrap().queue_depth,
        [
            ("General".to_owned(), 2),
            ("Other".to_owned(), 1),
            ("Empty".to_owned(), 0)
        ]
        .into()
    );
    assert_eq!(
        db.operations_metrics(Some(GENERAL))
            .await
            .unwrap()
            .queue_depth,
        [("General".to_owned(), 2)].into()
    );
}

#[tokio::test]
async fn run_duration_includes_failed_runs_and_omits_active_runs() {
    assert_eq!(
        metrics().await.run_duration,
        Latency {
            count: 3,
            mean_ms: Some(20.0),
            max_ms: Some(30)
        }
    );
}

#[tokio::test]
async fn cooldowns_only_list_accounts_that_have_yet_to_reset() {
    assert_eq!(
        metrics().await.account_cooldowns,
        [("cooling".to_owned(), NOW + Duration::from_millis(50))].into()
    );
}

#[tokio::test]
async fn cooldown_wait_includes_completed_and_current_blockage() {
    assert_eq!(metrics().await.account_wait_ms, 70);
}

#[tokio::test]
async fn dead_events_count_dead_rows_received_in_the_past_week() {
    assert_eq!(metrics().await.dead_events, 1);
}

#[tokio::test]
async fn timer_lateness_includes_each_occurrence_and_decision_expiry() {
    assert_eq!(
        metrics().await.timer_lateness,
        Latency {
            count: 3,
            mean_ms: Some(20.0),
            max_ms: Some(30)
        }
    );
}

#[tokio::test]
async fn outbox_lag_includes_retrying_rows_and_excludes_terminal_rows() {
    assert_eq!(metrics().await.outbox_lag_ms, Some(25));
}

async fn message(
    db: &HubDb,
    project_id: Option<ProjectId>,
    created_at: Timestamp,
    not_before: Option<Timestamp>,
) -> OutboxMessage {
    db.enqueue(&NewOutboxMessage {
        project_id,
        lane_id: None,
        session_id: None,
        run_id: None,
        tool_call_id: None,
        kind: OutboxKind::Message,
        decision_id: None,
        channel: "client".into(),
        body: serde_json::json!({"text": "private content"}),
        urgency: Urgency::Normal,
        not_before,
        created_at,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn delivery_health_separates_pacing_retry_and_failure_by_scope() {
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(FixedClock))
        .await
        .unwrap();
    let minute = Duration::from_secs(60);
    let paced = message(&db, Some(GENERAL), NOW, None).await;
    let retried_then_paced = message(&db, Some(GENERAL), NOW, None).await;
    db.delivery_failed(&retried_then_paced, "transient", NOW - minute, Some(NOW))
        .await
        .unwrap();
    db.hold_delivery(paced.id, NOW + minute, "quiet")
        .await
        .unwrap();
    db.hold_delivery(retried_then_paced.id, NOW + minute + minute, "rate")
        .await
        .unwrap();

    let retry = message(&db, Some(GENERAL), NOW, None).await;
    db.delivery_failed(&retry, "transient", NOW, Some(NOW + minute))
        .await
        .unwrap();
    let terminal = message(&db, Some(GENERAL), NOW - DAY, None).await;
    let old_terminal = message(&db, Some(GENERAL), NOW - DELIVERY_WEEK, None).await;
    let space_notice = message(&db, None, NOW, None).await;
    for failed in [&terminal, &old_terminal, &space_notice] {
        db.delivery_failed(failed, "permanent", NOW, None)
            .await
            .unwrap();
    }
    let sent = message(&db, Some(GENERAL), NOW, None).await;
    db.mark_sent(sent.id, Some("private-channel-message-id"), NOW)
        .await
        .unwrap();
    // A queued message with no failed attempt is not a retry.
    message(&db, Some(GENERAL), NOW, Some(NOW + minute)).await;

    let health = db
        .operations_metrics(Some(GENERAL))
        .await
        .unwrap()
        .delivery_health
        .unwrap();
    assert_eq!(
        health,
        DeliveryHealth {
            held: 2,
            next_hold_check: Some(NOW + minute),
            retrying: 1,
            next_retry: Some(NOW + minute),
            failed_created_in_last_week: 1,
        }
    );
    assert_eq!(
        db.operations_metrics(None)
            .await
            .unwrap()
            .delivery_health
            .unwrap()
            .failed_created_in_last_week,
        2
    );

    // Compare the serialized shape, not just the counts: a health snapshot
    // contains no body, raw error, or destination/adapter metadata.
    assert_eq!(
        serde_json::to_value(health).unwrap(),
        serde_json::json!({
            "held": 2,
            "next_hold_check": (NOW + minute).as_millis(),
            "retrying": 1,
            "next_retry": (NOW + minute).as_millis(),
            "failed_created_in_last_week": 1,
        })
    );
}

#[tokio::test]
async fn message_counts_use_delivery_time_and_utc_days() {
    assert_eq!(
        metrics().await.messages_per_day,
        [("1970-01-01".to_owned(), 1), ("1970-01-02".to_owned(), 2)].into()
    );
}

#[tokio::test]
async fn silent_run_ratio_uses_committed_runs() {
    assert_eq!(metrics().await.silent_run_ratio, Some(0.5));
}

#[tokio::test]
async fn decision_rate_uses_creation_in_the_rolling_day_and_splits_off_the_classifier() {
    let metrics = metrics().await;
    assert_eq!(metrics.decisions_per_day, 2);
    assert_eq!(metrics.classifier_decisions_per_day, 1);
}

#[tokio::test]
async fn answer_latency_omits_unanswered_expired_and_classifier_decisions() {
    assert_eq!(
        metrics().await.answer_latency,
        Latency {
            count: 2,
            mean_ms: Some(18.5),
            max_ms: Some(25)
        }
    );
}

#[tokio::test]
async fn rules_created_includes_revoked_rules() {
    assert_eq!(metrics().await.standing_rules_created, 2);
}

#[tokio::test]
async fn rollback_rechecks_budget_for_a_legacy_version() {
    let db = history().await;
    let path = "projects/1/instructions.md";
    let oversized = "x".repeat(threadlane_core::memory::INSTRUCTIONS_BUDGET + 1);
    let tx = db.db.begin().await.unwrap();
    exec(
        tx.conn(),
        "INSERT INTO memory_versions (path, version, content, written_at) VALUES (?1, 1, ?2, 0)",
        params![path, oversized.as_str()],
    )
    .await
    .unwrap();
    exec(tx.conn(), "INSERT INTO memory_files (path, version, content, written_at) VALUES (?1, 2, 'Short.', 10)",
         params![path]).await.unwrap();
    exec(tx.conn(), "INSERT INTO memory_versions (path, version, content, written_at) VALUES (?1, 2, 'Short.', 10)",
         params![path]).await.unwrap();
    tx.commit().await.unwrap();
    assert!(
        matches!(db.rollback_memory(path, 1, NOW).await, Err(StoreError::Invalid(reason)) if reason.contains("trim it"))
    );
    assert_eq!(db.read(path).await.unwrap().unwrap().version, 2);
    assert_eq!(db.history(path).await.unwrap().len(), 2);
}

#[tokio::test]
async fn migration_backfills_existing_traces_from_the_oldest_event() -> StoreResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prior.db");
    {
        let sqlite = rusqlite::Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..7] {
            sqlite.execute_batch(sql).unwrap();
        }
        sqlite.execute_batch(
            "PRAGMA user_version = 7;
             INSERT INTO projects (id, name, max_parallel_tasks, created_at) VALUES (1, 'Old', 1, 0);
             INSERT INTO lanes (id, project_id, kind) VALUES (1, 1, 'lead');
             INSERT INTO events (id, project_id, lane_id, source, source_event_id, kind, payload,
                                 trust, requested, status, received_at) VALUES
               (1, 1, 1, 'client', 'a', 'user_message', '{}', 'user', 1, 'done', 10),
               (2, 1, 1, 'client', 'b', 'user_message', '{}', 'user', 1, 'done', 5);
             INSERT INTO sessions (id, lane_id, status, created_at, last_active_at) VALUES (1, 1, 'open', 0, 0);
             INSERT INTO runs (id, lane_id, session_id, machine_id, lease_epoch, status, started_at)
               VALUES (1, 1, 1, 1, 1, 'committed', 20);
             INSERT INTO run_events (run_id, event_id) VALUES (1, 1), (1, 2);
             INSERT INTO decisions (project_id, lane_id, run_id, tool, arguments, arguments_hash, kind,
                                    question, options, status, created_at)
               VALUES (1, 1, 1, 'ask_user', '{}', '', 'input', '?', '[]', 'pending', 20);
             INSERT INTO outbox (run_id, kind, channel, body, urgency, status, created_at)
               VALUES (1, 'message', 'client', '{}', 'normal', 'pending', 20);
             INSERT INTO tool_calls (run_id, call_key, tool, arguments, safe_to_repeat, status, started_at)
               VALUES (1, 'once', 'send_message', '{}', 0, 'started', 20);",
        ).unwrap();
    }
    let db = Db::open(DbLocation::File(&path)).await?;
    let conn = db.reader()?;
    for table in ["runs", "decisions", "outbox", "tool_calls"] {
        let trace = query_one(&conn, &format!("SELECT trace_id FROM {table}"), (), |row| {
            Cols(row).text(0)
        })
        .await?;
        assert_eq!(trace, "event-2", "{table}");
    }
    let mut rows = conn
        .query("SELECT id, trace_id, replayed FROM events ORDER BY id", ())
        .await
        .map_err(db_err)?;
    for id in [1, 2] {
        let row = rows.next().await.map_err(db_err)?.expect("event");
        let row = Cols(&row);
        assert_eq!(row.i64(0)?, id);
        assert_eq!(row.text(1)?, format!("event-{id}"));
        assert!(!row.bool(2)?);
    }
    Ok(())
}

#[tokio::test]
async fn agent_skills_migrate_onto_a_version_20_database() -> StoreResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v20.db");
    {
        let sqlite = rusqlite::Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..20] {
            sqlite.execute_batch(sql).unwrap();
        }
        sqlite
            .execute_batch(
                "PRAGMA user_version = 20;
                 INSERT INTO agents (id, name, role) VALUES (1, 'Old', 'helper');",
            )
            .unwrap();
    }
    let db = Db::open(DbLocation::File(&path)).await?;
    let conn = db.reader()?;
    let skills = query_one(&conn, "SELECT skills FROM agents", (), |row| {
        Cols(row).text(0)
    })
    .await?;
    assert_eq!(skills, "[]");
    let version = query_one(&conn, "PRAGMA user_version", (), |row| Cols(row).i64(0)).await?;
    assert_eq!(version, crate::SCHEMA_VERSION);
    Ok(())
}

#[tokio::test]
async fn existing_version_22_routines_retain_their_missed_run_policy() -> StoreResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v22.db");
    {
        let sqlite = rusqlite::Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..22] {
            sqlite.execute_batch(sql).unwrap();
        }
        sqlite
            .execute_batch(
                r#"PRAGMA user_version = 22;
                 INSERT INTO projects(id, name, max_parallel_tasks, created_at)
                 VALUES(1, 'General', 1, 0);
                 INSERT INTO lanes(id, project_id, kind) VALUES(1, 1, 'lead');
                 INSERT INTO timers(id, project_id, lane_id, category, due_at,
                     recurrence, instruction, status, created_by, requested)
                 VALUES(1, 1, 1, 'schedule', 60,
                     '{"rule":"interval","minutes":5,"zone":"UTC"}',
                     'Keep the first routine', 'active', 'user', 1);"#,
            )
            .unwrap();
    }
    let db = Db::open(DbLocation::File(&path)).await?;
    let conn = db.reader()?;
    let (due, grace) = query_one(
        &conn,
        "SELECT due_at, missed_grace_minutes FROM timers WHERE id = 1",
        (),
        |row| Ok((Cols(row).i64(0)?, Cols(row).opt_i64(1)?)),
    )
    .await?;
    assert_eq!((due, grace), (60, None));
    let version = query_one(&conn, "PRAGMA user_version", (), |row| Cols(row).i64(0)).await?;
    assert_eq!(version, crate::SCHEMA_VERSION);
    Ok(())
}

#[tokio::test]
async fn bindings_at_their_project_mode_follow_it_and_no_other_changes() -> StoreResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prior.db");
    let before = MIGRATIONS.len() - 1;
    {
        let sqlite = rusqlite::Connection::open(&path).unwrap();
        for sql in &MIGRATIONS[..before] {
            sqlite.execute_batch(sql).unwrap();
        }
        // The space allows Assisted: a project's effective ceiling is its
        // own, stricter with the space's.
        sqlite
            .execute_batch(&format!(
                "PRAGMA user_version = {before};
                 INSERT INTO space (id, name, mode_ceiling, channels)
                   VALUES (1, 'Work', 'assisted', '[]');
                 INSERT INTO projects (id, name, mode_ceiling, max_parallel_tasks, created_at) VALUES
                   (1, 'Inherits', NULL, 1, 0),
                   (2, 'Above', 'autonomous', 1, 0),
                   (3, 'Below', 'on_request', 1, 0),
                   (4, 'Shut', 'off', 1, 0);
                 INSERT INTO agents (id, name, role) VALUES
                   (1, 'Lead', 'lead'), (2, 'Engineer', 'engineer'), (3, 'Reviewer', 'reviewer');
                 INSERT INTO bindings (id, project_id, agent_id, mode, is_lead, max_parallel) VALUES
                   (1, 1, 1, 'assisted', 1, 1),
                   (2, 1, 2, 'on_request', 0, 1),
                   (3, 1, 3, 'off', 0, 1),
                   (4, 2, 1, 'assisted', 1, 1),
                   (5, 2, 2, 'autonomous', 0, 1),
                   (6, 3, 1, 'on_request', 1, 1),
                   (7, 4, 1, 'off', 1, 1);"
            ))
            .unwrap();
    }
    let db = Db::open(DbLocation::File(&path)).await?;
    let conn = db.reader()?;
    let mut rows = conn
        .query("SELECT id, mode FROM bindings ORDER BY id", ())
        .await
        .map_err(db_err)?;
    let mut modes = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_err)? {
        let row = Cols(&row);
        modes.push((row.i64(0)?, row.opt_text(1)?));
    }
    let own = |mode: &str| Some(mode.to_owned());
    assert_eq!(
        modes,
        [
            (1, None),
            (2, own("on_request")),
            (3, own("off")),
            (4, None),
            (5, own("autonomous")),
            (6, None),
            (7, own("off")),
        ]
    );
    // The one-lead index came back with the table.
    let second_lead = conn
        .execute(
            "INSERT INTO bindings (project_id, agent_id, is_lead, max_parallel) VALUES (3, 2, 1, 1)",
            (),
        )
        .await;
    assert!(second_lead.is_err());
    let version = query_one(&conn, "PRAGMA user_version", (), |row| Cols(row).i64(0)).await?;
    assert_eq!(version, crate::SCHEMA_VERSION);
    Ok(())
}
