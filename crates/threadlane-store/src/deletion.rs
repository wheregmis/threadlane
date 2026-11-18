//! Project deletion. The cleanup job is committed with the database deletion.

use std::fmt::Write as _;

use serde_json::{Value as Json, json};
use threadlane_core::client::DeletePreview;
use threadlane_core::github::normalize_remote;
use threadlane_core::ids::{LaneId, ProjectId};
use threadlane_core::model::{NewOutboxMessage, OutboxKind, Urgency};
use threadlane_core::store::{StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use threadlane_core::workspace::WorkspaceRemoval;
use turso::Connection;

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::outbox::enqueue_in;
use crate::row::{Cols, json_text, params};

const OWNED: &str = "WITH
    project_lanes AS (SELECT id FROM lanes WHERE project_id = ?1),
    project_tasks AS (SELECT id FROM tasks WHERE project_id = ?1),
    project_sessions AS (SELECT id FROM sessions WHERE lane_id IN (SELECT id FROM project_lanes)),
    project_runs AS (SELECT id FROM runs WHERE lane_id IN (SELECT id FROM project_lanes)),
    project_events AS (SELECT id FROM events WHERE project_id = ?1 OR lane_id IN (SELECT id FROM project_lanes)),
    project_decisions AS (SELECT id FROM decisions WHERE project_id = ?1 OR lane_id IN (SELECT id FROM project_lanes)),
    project_outbox AS (SELECT id FROM outbox WHERE project_id = ?1 OR lane_id IN (SELECT id FROM project_lanes)
        OR session_id IN (SELECT id FROM project_sessions) OR run_id IN (SELECT id FROM project_runs) OR decision_id IN (SELECT id FROM project_decisions)),
    project_repos AS (SELECT id FROM repositories WHERE project_id = ?1)";

async fn owned<T>(
    conn: &Connection,
    query: &str,
    project: ProjectId,
    decode: impl Fn(&turso::Row) -> StoreResult<T>,
) -> StoreResult<Vec<T>> {
    query_all(
        conn,
        &format!("{OWNED} {query}"),
        params![project.get()],
        decode,
    )
    .await
}

async fn lookup(conn: &Connection, name: &str) -> StoreResult<ProjectId> {
    let (id, general) = query_opt(
        conn,
        "SELECT id, is_general FROM projects WHERE name = ?1",
        params![name],
        |row| Ok((Cols(row).id(0)?, Cols(row).bool(1)?)),
    )
    .await?
    .ok_or_else(|| StoreError::not_found("project", format!("{name:?}")))?;
    if general {
        return Err(StoreError::Invalid(
            "The General project cannot be deleted.".into(),
        ));
    }
    Ok(id)
}

async fn preview(conn: &Connection, id: ProjectId) -> StoreResult<DeletePreview> {
    owned(
        conn,
        "SELECT
            (SELECT COUNT(*) FROM project_tasks),
            (SELECT COUNT(*) FROM tasks WHERE project_id = ?1 AND status = 'queued'),
            (SELECT COUNT(*) FROM project_events),
            (SELECT COUNT(*) FROM project_outbox) + (
                SELECT COUNT(*) FROM outbox digest
                WHERE digest.id NOT IN (SELECT id FROM project_outbox)
                  AND EXISTS (SELECT 1 FROM outbox member WHERE member.digest_id = digest.id
                              AND member.id IN (SELECT id FROM project_outbox))
                  AND NOT EXISTS (SELECT 1 FROM outbox member WHERE member.digest_id = digest.id
                                  AND member.id NOT IN (SELECT id FROM project_outbox))
            ),
            (SELECT COUNT(*) FROM project_decisions),
            (SELECT COUNT(*) FROM decisions WHERE id IN (SELECT id FROM project_decisions) AND status = 'pending'),
            (SELECT COUNT(*) FROM memory_files WHERE path GLOB 'projects/' || ?1 || '/*'),
            (SELECT COUNT(*) FROM memory_versions WHERE path GLOB 'projects/' || ?1 || '/*'),
            (SELECT COUNT(*) FROM timers WHERE project_id = ?1 AND category = 'schedule'),
            (SELECT COUNT(*) FROM timers WHERE project_id = ?1 AND category = 'follow_up'),
            (SELECT COUNT(*) FROM timers WHERE project_id = ?1 AND category = 'dreaming'),
            (SELECT COUNT(*) FROM standing_rules WHERE project_id = ?1),
            (SELECT COUNT(*) FROM bindings WHERE project_id = ?1),
            (SELECT COUNT(*) FROM project_repos)",
        id,
        |row| {
            let c = Cols(row);
            let count = |i| c.i64(i).map(|n| u64::try_from(n).unwrap_or_default());
            Ok(DeletePreview {
                tasks: count(0)?,
                queued_tasks: count(1)?,
                events: count(2)?,
                outbox: count(3)?,
                decisions: count(4)?,
                open_decisions: count(5)?,
                memory_files: count(6)?,
                memory_versions: count(7)?,
                routines: count(8)?,
                follow_ups: count(9)?,
                dreaming_timers: count(10)?,
                standing_rules: count(11)?,
                bindings: count(12)?,
                repositories: count(13)?,
                ..DeletePreview::default()
            })
        },
    )
    .await?
    .pop()
    .ok_or_else(|| StoreError::Corrupt("missing deletion counts".into()))
}

async fn workspace(
    conn: &Connection,
    project: ProjectId,
) -> StoreResult<(WorkspaceRemoval, Vec<String>)> {
    let tasks = owned(
        conn,
        "SELECT id FROM project_tasks ORDER BY id",
        project,
        |row| Cols(row).id(0),
    )
    .await?;
    let repos = query_all(
        conn,
        "SELECT id, name, remote_url, project_id = ?1 FROM repositories ORDER BY id",
        params![project.get()],
        |row| {
            let c = Cols(row);
            let remote = c.text(2)?;
            Ok((
                c.id(0)?,
                c.text(1)?,
                normalize_remote(&remote).unwrap_or(remote),
                c.bool(3)?,
            ))
        },
    )
    .await?;
    let mut removal = WorkspaceRemoval {
        tasks,
        ..WorkspaceRemoval::default()
    };
    let mut cursors = Vec::new();
    for (id, name, remote, own) in &repos {
        if *own {
            removal.repositories.push((*id, name.clone()));
            if !repos
                .iter()
                .any(|(_, _, other, owned)| !owned && other == remote)
            {
                removal.clones.push(*id);
                if let Some(repository) = remote.strip_prefix("github.com/") {
                    cursors.push(format!("github:{repository}@"));
                }
            }
        }
    }
    Ok((removal, cursors))
}

async fn require_idle(conn: &Connection, id: ProjectId, name: &str) -> StoreResult<()> {
    let active = owned(
        conn,
        "SELECT 'run ' || id || ' (lane ' || lane_id || ')' FROM runs
         WHERE id IN (SELECT id FROM project_runs) AND status = 'active'
         UNION ALL
         SELECT 'lane ' || id || ' (leased to machine ' || lease_owner || ')' FROM lanes
         WHERE id IN (SELECT id FROM project_lanes) AND lease_owner IS NOT NULL",
        id,
        |row| Cols(row).text(0),
    )
    .await?;
    if !active.is_empty() {
        return Err(StoreError::Invalid(format!(
            "Stop these before deleting project {name:?}: {}.",
            active.join(", ")
        )));
    }
    Ok(())
}

// Shared digests retain the other projects' rows and their delivery state.
async fn remove_digest_members(conn: &Connection, id: ProjectId) -> StoreResult<()> {
    let digests: Vec<i64> = owned(
        conn,
        "SELECT DISTINCT digest_id FROM outbox WHERE id IN (SELECT id FROM project_outbox) AND digest_id IS NOT NULL",
        id,
        |row| Cols(row).i64(0),
    ).await?;
    for digest in digests {
        let retained = query_all(
            conn,
            &format!(
                "{OWNED} SELECT o.id, p.name, o.body FROM outbox o
                LEFT JOIN projects p ON p.id = o.project_id
                WHERE o.digest_id = ?2 AND o.id NOT IN (SELECT id FROM project_outbox) ORDER BY o.id"
            ),
            params![id.get(), digest],
            |row| {
                let c = Cols(row);
                Ok((c.i64(0)?, c.opt_text(1)?, c.json(2)?))
            },
        )
        .await?;
        if retained.is_empty() {
            exec(
                conn,
                "UPDATE outbox SET digest_id = NULL WHERE digest_id = ?1",
                params![digest],
            )
            .await?;
            exec(conn, "DELETE FROM outbox WHERE id = ?1", params![digest]).await?;
            continue;
        }
        let mut groups = Vec::<(Option<String>, Vec<Json>)>::new();
        let mut ids = Vec::new();
        for (id, project, body) in retained {
            ids.push(id);
            match groups.iter_mut().find(|(name, _)| *name == project) {
                Some((_, messages)) => messages.push(body),
                None => groups.push((project, vec![body])),
            }
        }
        let mut text = String::from("Digest");
        for (project, messages) in groups {
            let _ = write!(text, "\n\n{}", project.as_deref().unwrap_or("Space"));
            for body in messages {
                let _ = write!(
                    text,
                    "\n- {}",
                    body["text"]
                        .as_str()
                        .map_or_else(|| body.to_string(), str::to_owned)
                );
            }
        }
        exec(
            conn,
            "UPDATE outbox SET body = ?1, tool_call_id = 'digest:' || ?2 WHERE id = ?3",
            params![
                json_text(&json!({"text": text, "digest": ids}))?,
                ids.first().copied(),
                digest
            ],
        )
        .await?;
    }
    Ok(())
}

async fn preserve_floors(conn: &Connection) -> StoreResult<()> {
    for (table, column, name) in [
        ("projects", "id", "projects"),
        ("repositories", "id", "repositories"),
        ("bindings", "id", "bindings"),
        ("standing_rules", "id", "standing_rules"),
        ("lanes", "id", "lanes"),
        ("events", "id", "events"),
        ("sessions", "id", "sessions"),
        ("runs", "id", "runs"),
        ("tool_calls", "id", "tool_calls"),
        ("tasks", "id", "tasks"),
        ("timers", "id", "timers"),
        ("memory_versions", "id", "memory_versions"),
        ("decisions", "id", "decisions"),
        ("outbox", "id", "outbox"),
        ("outbox", "delivery_seq", "delivery_seq"),
    ] {
        exec(
            conn,
            &format!(
                "INSERT INTO deletion_floors (name, value)
             VALUES (?1, COALESCE((SELECT MAX({column}) FROM {table}), 0))
             ON CONFLICT(name) DO UPDATE SET value = MAX(deletion_floors.value, excluded.value)"
            ),
            params![name],
        )
        .await?;
    }
    Ok(())
}

impl HubDb {
    /// Reads project counts and the local workspace plan in a single snapshot.
    pub async fn project_deletion_preview(
        &self,
        name: &str,
    ) -> StoreResult<(DeletePreview, WorkspaceRemoval)> {
        let tx = self.db.begin().await?;
        let id = lookup(tx.conn(), name).await?;
        let counts = preview(tx.conn(), id).await?;
        let (removal, _) = workspace(tx.conn(), id).await?;
        tx.commit().await?;
        Ok((counts, removal))
    }

    /// Deletes the project's records, and commits its notice and cleanup job atomically.
    pub async fn delete_project(
        &self,
        name: &str,
        confirm_name: &str,
        now: Timestamp,
    ) -> StoreResult<(ProjectId, DeletePreview, WorkspaceRemoval)> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let id = lookup(conn, name).await?;
        if name != confirm_name {
            return Err(StoreError::Invalid(format!(
                "Type the exact name {name:?} to delete this project."
            )));
        }
        require_idle(conn, id, name).await?;
        let counts = preview(conn, id).await?;
        let (removal, cursors) = workspace(conn, id).await?;
        preserve_floors(conn).await?;
        remove_digest_members(conn, id).await?;

        for statement in [
            "DELETE FROM permission_questions WHERE decision_id IN (SELECT id FROM project_decisions)
                OR question_key IN (SELECT 'event:' || id FROM project_events)",
            "UPDATE outbox SET digest_id = NULL WHERE digest_id IN (SELECT id FROM project_outbox)",
            "UPDATE events SET reply_to_ref = NULL WHERE id NOT IN (SELECT id FROM project_events) AND reply_to_ref IN (SELECT id FROM project_outbox)",
            "DELETE FROM outbox WHERE id IN (SELECT id FROM project_outbox)",
            "UPDATE standing_rules SET created_from_decision = NULL WHERE project_id != ?1
                AND created_from_decision IN (SELECT id FROM project_decisions)",
            "DELETE FROM standing_rules WHERE project_id = ?1",
            "UPDATE events SET decision_ref = NULL WHERE id NOT IN (SELECT id FROM project_events) AND decision_ref IN (SELECT id FROM project_decisions)",
            "UPDATE decisions SET task_id = NULL WHERE id NOT IN (SELECT id FROM project_decisions) AND task_id IN (SELECT id FROM project_tasks)",
            "UPDATE decisions SET session_id = NULL WHERE id NOT IN (SELECT id FROM project_decisions) AND session_id IN (SELECT id FROM project_sessions)",
            "UPDATE decisions SET run_id = NULL WHERE id NOT IN (SELECT id FROM project_decisions) AND run_id IN (SELECT id FROM project_runs)",
            "DELETE FROM decisions WHERE id IN (SELECT id FROM project_decisions)",
            "DELETE FROM timers WHERE project_id = ?1 OR lane_id IN (SELECT id FROM project_lanes) OR task_ref IN (SELECT id FROM project_tasks)",
            "DELETE FROM checkpoints WHERE task_id IN (SELECT id FROM project_tasks) OR run_id IN (SELECT id FROM project_runs) OR repository_id IN (SELECT id FROM project_repos)",
            "DELETE FROM task_dependencies WHERE task_id IN (SELECT id FROM project_tasks) OR depends_on IN (SELECT id FROM project_tasks)",
            "UPDATE tasks SET parent_task_id = NULL WHERE parent_task_id IN (SELECT id FROM project_tasks)",
            "DELETE FROM tasks WHERE id IN (SELECT id FROM project_tasks)",
            "DELETE FROM memory_versions WHERE path GLOB 'projects/' || ?1 || '/*'",
            "DELETE FROM memory_files WHERE path GLOB 'projects/' || ?1 || '/*'",
            "UPDATE memory_versions SET written_by_run = NULL WHERE written_by_run IN (SELECT id FROM project_runs)",
            "UPDATE memory_files SET written_by_run = NULL WHERE written_by_run IN (SELECT id FROM project_runs)",
            "DELETE FROM run_events WHERE run_id IN (SELECT id FROM project_runs) OR event_id IN (SELECT id FROM project_events)",
            "DELETE FROM tool_calls WHERE run_id IN (SELECT id FROM project_runs)",
            "DELETE FROM journal WHERE run_id IN (SELECT id FROM project_runs)",
            "UPDATE events SET session_ref = NULL WHERE id NOT IN (SELECT id FROM project_events) AND session_ref IN (SELECT id FROM project_sessions)",
            "UPDATE events SET claimed_by_run = NULL WHERE id NOT IN (SELECT id FROM project_events) AND claimed_by_run IN (SELECT id FROM project_runs)",
            "DELETE FROM events WHERE id IN (SELECT id FROM project_events)",
            "DELETE FROM runs WHERE id IN (SELECT id FROM project_runs)",
            "DELETE FROM sessions WHERE id IN (SELECT id FROM project_sessions)",
            "DELETE FROM lanes WHERE id IN (SELECT id FROM project_lanes)",
            "DELETE FROM bindings WHERE project_id = ?1",
            "DELETE FROM checkouts WHERE repository_id IN (SELECT id FROM project_repos)",
            "DELETE FROM repositories WHERE id IN (SELECT id FROM project_repos)",
            "DELETE FROM project_machines WHERE project_id = ?1",
            "DELETE FROM projects WHERE id = ?1",
        ] {
            exec(conn, &format!("{OWNED} {statement}"), params![id.get()]).await?;
        }
        for prefix in cursors {
            exec(
                conn,
                "DELETE FROM source_cursors WHERE substr(source, 1, length(?1)) = ?1",
                params![prefix],
            )
            .await?;
        }
        exec(
            conn,
            "INSERT INTO project_cleanup (id, removal) VALUES (?1, ?2)",
            params![id.get(), json_text(&removal)?],
        )
        .await?;
        let (general, lane): (ProjectId, LaneId) = query_one(
            conn,
            "SELECT p.id, l.id FROM projects p JOIN lanes l ON l.project_id = p.id
             WHERE p.is_general = 1 AND l.kind = 'lead'",
            (),
            |row| Ok((Cols(row).id(0)?, Cols(row).id(1)?)),
        )
        .await?;
        enqueue_in(
            conn,
            &NewOutboxMessage {
                project_id: Some(general),
                lane_id: Some(lane),
                session_id: None,
                run_id: None,
                tool_call_id: None,
                kind: OutboxKind::SystemNotice,
                decision_id: None,
                channel: crate::session::NOTICE_CHANNEL.to_owned(),
                body: json!({"text": format!("Project {name} was deleted by you.")}),
                urgency: Urgency::High,
                not_before: None,
                created_at: now,
            },
        )
        .await?;
        tx.commit().await?;
        Ok((id, counts, removal))
    }

    /// Filesystem cleanup jobs that survived a crash or an error.
    pub async fn project_cleanups(&self) -> StoreResult<Vec<(ProjectId, WorkspaceRemoval)>> {
        query_all(
            &self.db.reader()?,
            "SELECT id, removal FROM project_cleanup ORDER BY id",
            (),
            |row| Ok((Cols(row).id(0)?, Cols(row).de(1)?)),
        )
        .await
    }

    /// Acknowledges completed filesystem cleanup.
    pub async fn finish_project_cleanup(&self, id: ProjectId) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "DELETE FROM project_cleanup WHERE id = ?1",
            params![id.get()],
        )
        .await?;
        tx.commit().await
    }
}

#[cfg(test)]
mod tests;
