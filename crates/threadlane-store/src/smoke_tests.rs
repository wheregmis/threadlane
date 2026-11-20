//! Schema smoke test on Turso (Plan.md §18.4 item 2): every table exists, and
//! every database feature the schema relies on is enforced by Turso itself.

use std::sync::Arc;

use threadlane_core::store::StoreError;
use threadlane_core::time::SystemClock;

use crate::db::{exec, query_all, query_one};
use crate::row::{Cols, params};
use crate::{Bootstrap, HubDb};

/// Every table named in Plan.md §2.4 to §10.1, plus the system's own.
const TABLES: &[&str] = &[
    "accounts",
    "admitted_keys",
    "agents",
    "bindings",
    "checkouts",
    "checkpoints",
    "decisions",
    "events",
    "github_reviews",
    "join_codes",
    "journal",
    "lanes",
    "machine_accounts",
    "machines",
    "memory_files",
    "memory_versions",
    "outbox",
    "permission_questions",
    "pr_review_tasks",
    "project_machines",
    "projects",
    "repositories",
    "review_recipe",
    "run_events",
    "runs",
    "sessions",
    "source_cursors",
    "space",
    "standing_rules",
    "task_dependencies",
    "task_reviews",
    "tasks",
    "timers",
    "tool_calls",
    "deletion_floors",
    "project_cleanup",
    "dream_batches",
    "dream_items",
];

async fn hub() -> HubDb {
    HubDb::open_in_memory(&Bootstrap::default(), Arc::new(SystemClock))
        .await
        .expect("hub opens")
}

async fn expect_conflict(hub: &HubDb, sql: &str) {
    let tx = hub.db.begin().await.expect("begin");
    let result = exec(tx.conn(), sql, ()).await;
    assert!(
        matches!(result, Err(StoreError::Conflict(_))),
        "expected a constraint violation from {sql:?}, got {result:?}"
    );
}

#[tokio::test]
async fn every_table_exists() {
    let hub = hub().await;
    let conn = hub.db.reader().expect("reader");
    let mut found = query_all(
        &conn,
        "SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name",
        (),
        |row| Cols(row).text(0),
    )
    .await
    .expect("schema query");
    found.retain(|name| !name.starts_with("sqlite_"));
    let mut expected = TABLES.to_vec();
    expected.sort_unstable();
    assert_eq!(found, expected);
}

#[tokio::test]
async fn partial_unique_indexes_are_enforced() {
    let hub = hub().await;
    // A second General project.
    expect_conflict(
        &hub,
        "INSERT INTO projects (name, is_general, max_parallel_tasks, created_at) VALUES ('G2', 1, 1, 0)",
    )
    .await;
    // A second lead lane for the General project (id 1).
    expect_conflict(
        &hub,
        "INSERT INTO lanes (project_id, kind) VALUES (1, 'lead')",
    )
    .await;
    // Task lanes are not limited by the lead-lane index.
    {
        let tx = hub.db.begin().await.expect("begin");
        exec(
            tx.conn(),
            "INSERT INTO lanes (project_id, kind) VALUES (1, 'task')",
            (),
        )
        .await
        .expect("first task lane");
        exec(
            tx.conn(),
            "INSERT INTO lanes (project_id, kind) VALUES (1, 'task')",
            (),
        )
        .await
        .expect("second task lane");
        tx.commit().await.expect("commit");
    }
    // Unsent messages share a NULL delivery_seq; sent ones never share one.
    {
        let tx = hub.db.begin().await.expect("begin");
        for _ in 0..2 {
            exec(
                tx.conn(),
                "INSERT INTO outbox (kind, channel, body, urgency, status, created_at)
                 VALUES ('message', 'client', '{}', 'normal', 'pending', 0)",
                (),
            )
            .await
            .expect("unsent message");
        }
        exec(
            tx.conn(),
            "INSERT INTO outbox (kind, channel, body, urgency, status, created_at, delivery_seq)
             VALUES ('message', 'client', '{}', 'normal', 'sent', 0, 1)",
            (),
        )
        .await
        .expect("sent message");
        tx.commit().await.expect("commit");
    }
    expect_conflict(
        &hub,
        "INSERT INTO outbox (kind, channel, body, urgency, status, created_at, delivery_seq)
         VALUES ('message', 'client', '{}', 'normal', 'sent', 0, 1)",
    )
    .await;
}

#[tokio::test]
async fn foreign_keys_are_enforced() {
    let hub = hub().await;
    expect_conflict(
        &hub,
        "INSERT INTO lanes (project_id, kind) VALUES (999, 'task')",
    )
    .await;
}

#[tokio::test]
async fn upsert_returning_and_json_functions_work() {
    let hub = hub().await;
    let tx = hub.db.begin().await.expect("begin");
    let conn = tx.conn();
    let first = query_one(
        conn,
        "INSERT INTO source_cursors (source, cursor, updated_at) VALUES ('gh', 'a', 1)
         ON CONFLICT (source) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at
         RETURNING cursor",
        (),
        |row| Cols(row).text(0),
    )
    .await
    .expect("insert returning");
    let second = query_one(
        conn,
        "INSERT INTO source_cursors (source, cursor, updated_at) VALUES ('gh', 'b', 2)
         ON CONFLICT (source) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at
         RETURNING cursor",
        (),
        |row| Cols(row).text(0),
    )
    .await
    .expect("upsert returning");
    assert_eq!((first.as_str(), second.as_str()), ("a", "b"));

    let extracted = query_one(
        conn,
        "SELECT json_extract(?1, '$.repository')",
        params![r#"{"repository":"payments-service"}"#],
        |row| Cols(row).text(0),
    )
    .await
    .expect("json_extract");
    assert_eq!(extracted, "payments-service");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn an_abandoned_write_is_rolled_back_before_the_next() {
    let hub = hub().await;
    {
        let tx = hub.db.begin().await.expect("begin");
        exec(
            tx.conn(),
            "INSERT INTO source_cursors (source, cursor, updated_at) VALUES ('lost', 'x', 1)",
            (),
        )
        .await
        .expect("insert");
        // Dropped without commit.
    }
    let tx = hub.db.begin().await.expect("next writer begins");
    let count = query_one(
        tx.conn(),
        "SELECT COUNT(*) FROM source_cursors WHERE source = 'lost'",
        (),
        |row| Cols(row).i64(0),
    )
    .await
    .expect("count");
    assert_eq!(count, 0);
}
