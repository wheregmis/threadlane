use std::sync::Arc;

use threadlane_core::model::NewProject;
use threadlane_core::store::{OrgStore, OutboxStore};
use threadlane_core::time::SystemClock;
use turso::Value;

use super::*;
use crate::Bootstrap;

const TABLES: &[&str] = &[
    "projects",
    "repositories",
    "bindings",
    "standing_rules",
    "project_machines",
    "lanes",
    "events",
    "sessions",
    "runs",
    "run_events",
    "tool_calls",
    "journal",
    "tasks",
    "task_dependencies",
    "checkpoints",
    "timers",
    "memory_files",
    "memory_versions",
    "decisions",
    "permission_questions",
    "outbox",
    "source_cursors",
    "checkouts",
    "space",
    "agents",
    "machines",
    "accounts",
    "machine_accounts",
    "admitted_keys",
    "join_codes",
];

async fn seed(db: &HubDb, id: i64, name: &str) {
    let tx = db.db.begin().await.unwrap();
    let child = id + 1;
    let sql = format!(
        "INSERT INTO projects (id, name, max_parallel_tasks, created_at) VALUES ({id}, '{name}', 0, 0);
         INSERT INTO repositories (id, project_id, name, remote_url, default_branch)
            VALUES ({id}, {id}, 'app', 'https://github.com/me/{name}.git', 'main');
         INSERT INTO bindings (id, project_id, agent_id, mode, is_lead, max_parallel)
            VALUES ({id}, {id}, 1, 'on_request', 1, 0);
         INSERT INTO project_machines VALUES ({id}, 1);
         INSERT INTO checkouts VALUES (1, {id}, 0);
         INSERT INTO lanes (id, project_id, kind) VALUES ({id}, {id}, 'lead'), ({child}, {id}, 'task'), ({id}+2, {id}, 'task');
         INSERT INTO tasks (id, project_id, lane_id, agent_id, title, goal, origin, status, created_at)
            VALUES ({id}, {id}, {child}, 1, 'first', 'first', 'user', 'queued', 0);
         INSERT INTO tasks (id, project_id, lane_id, agent_id, title, goal, origin, status, created_at, parent_task_id)
            VALUES ({child}, {id}, {id}+2, 1, 'second', 'second', 'user', 'done', 0, {id});
         UPDATE lanes SET task_id = {id} WHERE id = {child};
         UPDATE lanes SET task_id = {child} WHERE id = {id}+2;
         INSERT INTO task_dependencies VALUES ({child}, {id});
         INSERT INTO sessions (id, lane_id, status, created_at, last_active_at) VALUES ({id}, {child}, 'open', 0, 0);
         INSERT INTO runs (id, lane_id, session_id, machine_id, lease_epoch, status, started_at, ended_at)
            VALUES ({id}, {child}, {id}, 1, 1, 'committed', 0, 1);
         INSERT INTO events (id, project_id, lane_id, source, source_event_id, kind, payload, trust,
             requested, received_at, session_ref, decision_ref, reply_to_ref, claimed_by_run)
            VALUES ({id}, {id}, {child}, 'client', '{name}', 'user_message', '{{}}', 'user',
                1, 0, {id}, {id}, {id}, {id});
         INSERT INTO run_events VALUES ({id}, {id});
         INSERT INTO tool_calls (id, run_id, call_key, tool, arguments, safe_to_repeat, status, started_at)
            VALUES ({id}, {id}, '{name}:1', 'ask', '{{}}', 1, 'done', 0);
         INSERT INTO journal VALUES ({id}, 1, 0, '{{}}');
         INSERT INTO checkpoints (id, task_id, run_id, repository_id, base_commit, branch, bundle_ref, bundle_sha256, created_at)
            VALUES ({id}, {id}, {id}, {id}, 'hash', 'work', 'bundle', 'sha', 0);
         INSERT INTO timers (id, project_id, lane_id, task_ref, category, due_at, instruction, status, created_by)
            VALUES ({id}, {id}, {id}, NULL, 'schedule', 0, 'routine', 'active', 'user'),
                   ({child}, {id}, {child}, {id}, 'follow_up', 0, 'follow-up', 'active', 'agent'),
                   ({id}+2, {id}, {id}, NULL, 'dreaming', 0, 'dream', 'active', 'system');
         INSERT INTO memory_files VALUES ('projects/{id}/notes/a.md', 1, '{name}', 0, {id}, 0),
            ('projects/{id}/notes/INDEX.md', 1, 'index', 0, {id}, 0);
         INSERT INTO memory_versions (id, path, version, content, written_by_run, written_at)
            VALUES ({id}, 'projects/{id}/notes/a.md', 1, '{name}', {id}, 0),
                   ({child}, 'projects/{id}/notes/INDEX.md', 1, 'index', {id}, 0);
         INSERT INTO decisions (id, project_id, lane_id, task_id, session_id, run_id, tool, arguments,
             arguments_hash, kind, question, options, status, created_at)
            VALUES ({id}, {id}, {child}, {id}, {id}, {id}, 'ask', '{{}}', 'hash', 'input', 'Ask?', '[]', 'pending', 0);
         INSERT INTO standing_rules (id, project_id, agent_id, tool, pattern, tier, created_from_decision, created_at)
            VALUES ({id}, {id}, 1, 'ask', '{{}}', 0, {id}, 0);
         INSERT INTO permission_questions VALUES ('event:{id}', '{{}}', 0, NULL), ('other:{id}', '{{}}', 0, {id});
         INSERT INTO outbox (id, project_id, lane_id, session_id, run_id, decision_id, channel, kind,
             body, urgency, status, created_at, delivery_seq)
            VALUES ({id}, {id}, {child}, {id}, {id}, {id}, 'client', 'message', '{{}}', 'normal', 'sent', 0, {id});
         INSERT INTO source_cursors VALUES ('github:me/{name}@main', 'cursor', 0);"
    );
    tx.conn().execute_batch(&sql).await.unwrap();
    tx.commit().await.unwrap();
}

async fn snapshot(db: &HubDb, table: &str) -> Vec<Vec<Value>> {
    let where_clause = if table == "outbox" {
        "WHERE kind != 'system_notice'"
    } else {
        ""
    };
    query_all(
        &db.db.reader().unwrap(),
        &format!("SELECT * FROM {table} {where_clause} ORDER BY rowid"),
        (),
        |row| {
            Ok((0..row.column_count())
                .map(|column| row.get_value(column).unwrap())
                .collect())
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn deletion_removes_every_owned_row_and_preserves_the_other_project() {
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(SystemClock))
        .await
        .unwrap();
    let tx = db.db.begin().await.unwrap();
    tx.conn()
        .execute_batch(
            "INSERT INTO agents (id, name, role) VALUES (1, 'Agent', 'role');
         INSERT INTO machines (id, name, endpoint_id, trust, status, max_parallel)
            VALUES (1, 'Machine', 'endpoint', 'trusted', 'online', 1);
         INSERT INTO accounts (id, label, backend) VALUES (1, 'Account', 'codex');
         INSERT INTO machine_accounts VALUES (1, 1);
         INSERT INTO memory_files VALUES ('space/notes/shared.md', 1, 'shared', 0, NULL, 0);
         INSERT INTO memory_versions (id, path, version, content, written_at)
            VALUES (1, 'space/notes/shared.md', 1, 'shared', 0);
         INSERT INTO source_cursors VALUES ('github:notifications', 'feed', 0);",
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    seed(&db, 10, "keep").await;
    let mut untouched = Vec::new();
    for table in TABLES {
        untouched.push(snapshot(&db, table).await);
    }
    seed(&db, 20, "remove").await;

    let (preview, removal) = db.project_deletion_preview("remove").await.unwrap();
    assert_eq!(
        preview,
        DeletePreview {
            tasks: 2,
            queued_tasks: 1,
            events: 1,
            outbox: 1,
            decisions: 1,
            open_decisions: 1,
            memory_files: 2,
            memory_versions: 2,
            routines: 1,
            follow_ups: 1,
            dreaming_timers: 1,
            standing_rules: 1,
            bindings: 1,
            repositories: 1,
            worktrees: 0,
            clones: 0,
        }
    );
    assert_eq!(removal.tasks.len(), 2);
    let now = Timestamp::from_millis(10);
    let (id, deleted, _) = db.delete_project("remove", "remove", now).await.unwrap();
    assert_eq!(id, ProjectId(20));
    assert_eq!(deleted, preview);
    assert_eq!(db.project_cleanups().await.unwrap(), [(id, removal)]);
    db.finish_project_cleanup(id).await.unwrap();
    assert!(db.project_cleanups().await.unwrap().is_empty());
    for (table, before) in TABLES.iter().zip(untouched) {
        assert_eq!(snapshot(&db, table).await, before, "{table}");
    }
    assert!(db.integrity_check().await.unwrap().is_empty());
    let notice = db.deliverable(now, 10).await.unwrap();
    assert_eq!(notice.len(), 1);
    db.mark_sent(notice[0].id, None, now).await.unwrap();
    assert_eq!(db.delivered_after("client", 20, 10).await.unwrap().len(), 1);
    assert!(notice[0].id.get() > 20);
    let replacement = db
        .create_project(&NewProject {
            name: "remove".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    assert!(replacement.id.get() > 20);
    assert!(replacement.lead_lane_id.get() > 22);
}

#[tokio::test]
async fn deletion_scrubs_mixed_digests_and_removes_project_only_digests() {
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(SystemClock))
        .await
        .unwrap();
    let doomed = db
        .create_project(&NewProject {
            name: "remove".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let kept = db
        .create_project(&NewProject {
            name: "keep".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    db.set_project_archived(doomed.id, true).await.unwrap();
    let tx = db.db.begin().await.unwrap();
    tx.conn().execute_batch(&format!(
        "INSERT INTO outbox (id, project_id, kind, channel, body, urgency, status, created_at, digest_id)
         VALUES (102, NULL, 'message', 'client', '{{\"text\":\"private and keep\",\"digest\":[100,101]}}', 'normal', 'sent', 0, NULL),
                (104, NULL, 'message', 'client', '{{\"text\":\"private\",\"digest\":[103]}}', 'normal', 'pending', 0, NULL),
                (100, {}, 'message', 'client', '{{\"text\":\"private\"}}', 'normal', 'suppressed', 0, 102),
                (101, {}, 'message', 'client', '{{\"text\":\"keep\"}}', 'normal', 'suppressed', 0, 102),
                (103, {}, 'message', 'client', '{{\"text\":\"private\"}}', 'normal', 'suppressed', 0, 104)",
        doomed.id.get(), kept.id.get(), doomed.id.get()
    )).await.unwrap();
    tx.commit().await.unwrap();
    let (preview, _) = db.project_deletion_preview("remove").await.unwrap();
    assert_eq!(preview.outbox, 3);
    db.delete_project("remove", "remove", Timestamp::from_millis(0))
        .await
        .unwrap();
    let reader = db.db.reader().unwrap();
    let messages = query_all(
        &reader,
        "SELECT id, body FROM outbox WHERE kind = 'message' ORDER BY id",
        (),
        |row| {
            let c = Cols(row);
            Ok((c.i64(0)?, c.json(1)?))
        },
    )
    .await
    .unwrap();
    assert_eq!(
        messages,
        [
            (101, json!({"text": "keep"})),
            (
                102,
                json!({"text": "Digest\n\nkeep\n- keep", "digest": [101]})
            ),
        ]
    );
}
