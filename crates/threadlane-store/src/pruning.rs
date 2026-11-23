//! Tiered compaction and pruning of terminal tasks.
//!
//! - **Tier 2 (compaction):** removes heavy per-run artifacts (`journal`,
//!   `tool_calls`, `checkpoints`) and the task's progress file (`memory_files`
//!   + `memory_versions`) while keeping the compact `tasks` row and result.
//! - **Tier 3 (full pruning):** removes old terminal tasks and their lanes,
//!   sessions, runs, events, and task-scoped decisions in FK-safe order while
//!   preserving `deletion_floors`, `standing_rules`, and Lead chat `outbox`.

use std::collections::BTreeSet;

use threadlane_core::ids::{LaneId, ProjectId, RepositoryId, TaskId};
use threadlane_core::memory::progress_path;
use threadlane_core::store::StoreResult;
use threadlane_core::time::Timestamp;
use threadlane_core::workspace::WorkspaceRemoval;
use turso::Connection;

use crate::HubDb;
use crate::db::{exec, query_all, query_one};
use crate::deletion::preserve_floors;
use crate::row::{Cols, params};

/// Retention policy for one compaction and pruning pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskPruningPolicy {
    /// Compact terminal tasks whose last activity is older than this cutoff.
    pub compact_before: Option<Timestamp>,
    /// Fully prune terminal tasks whose last activity is older than this cutoff.
    pub prune_before: Option<Timestamp>,
    /// Keep at most this many terminal tasks per project (oldest eligible pruned first).
    pub max_terminal_per_project: Option<usize>,
    /// Maximum number of tasks to compact or prune in one transaction.
    pub batch_size: usize,
    /// When `true`, bypass the dreaming-review barrier (for explicit manual pruning).
    pub ignore_dreaming_barrier: bool,
}

/// Counts and workspace cleanups produced by one compaction/pruning pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskPruningReport {
    pub compacted_tasks: usize,
    pub pruned_tasks: usize,
    pub removed_journal_rows: usize,
    pub removed_tool_calls: usize,
    pub removed_memory_files: usize,
    pub workspace_removals: Vec<WorkspaceRemoval>,
}

#[derive(Debug, Clone)]
struct CandidateTask {
    id: TaskId,
    project_id: ProjectId,
    lane_id: LaneId,
    repositories: Vec<RepositoryId>,
}

const LAST_ACTIVITY_SQL: &str = "COALESCE(
    (SELECT MAX(COALESCE(r.ended_at, r.started_at)) FROM runs r WHERE r.lane_id = t.lane_id),
    t.created_at
)";

const IDLE_TERMINAL_SQL: &str = "t.status IN ('done', 'failed', 'cancelled')
    AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.lane_id = t.lane_id AND r.status = 'active')
    AND NOT EXISTS (SELECT 1 FROM lanes l WHERE l.id = t.lane_id AND l.lease_owner IS NOT NULL)";

const DREAMING_BARRIER_SQL: &str = "(
    ?3 = 1
    OR NOT EXISTS (
        SELECT 1 FROM timers dm
        WHERE dm.project_id = t.project_id AND dm.category = 'dreaming' AND dm.status = 'active'
    )
    OR (
        EXISTS (
            SELECT 1 FROM dream_items di
            WHERE di.project_id = t.project_id
              AND di.kind = 'task'
              AND di.item_id = t.id
              AND di.last_review > 0
        )
        AND NOT EXISTS (
            SELECT 1 FROM dream_items di
            JOIN runs r ON r.id = di.item_id
            WHERE di.project_id = t.project_id
              AND di.kind = 'run'
              AND r.lane_id = t.lane_id
              AND di.last_review = 0
        )
    )
)";

const NO_ACTIVE_DEPENDENTS_SQL: &str = "NOT EXISTS (
        SELECT 1 FROM tasks child
        WHERE child.parent_task_id = t.id
          AND child.status NOT IN ('done', 'failed', 'cancelled')
    )
    AND NOT EXISTS (
        SELECT 1 FROM task_dependencies td
        JOIN tasks dep ON dep.id = td.task_id
        WHERE td.depends_on = t.id
          AND dep.status NOT IN ('done', 'failed', 'cancelled')
    )";

fn decode_candidate(row: &turso::Row) -> StoreResult<CandidateTask> {
    let c = Cols(row);
    let repos_raw = c.text(3)?;
    let repositories: Vec<RepositoryId> = serde_json::from_str(&repos_raw).unwrap_or_default();
    Ok(CandidateTask {
        id: c.id(0)?,
        project_id: c.id(1)?,
        lane_id: c.id(2)?,
        repositories,
    })
}

async fn count_i64(conn: &Connection, sql: &str, p: Vec<turso::Value>) -> StoreResult<usize> {
    let n = query_one(conn, sql, p, |row| Cols(row).i64(0)).await?;
    Ok(usize::try_from(n).unwrap_or_default())
}

async fn compact_one(
    conn: &Connection,
    task: &CandidateTask,
    now: Timestamp,
    report: &mut TaskPruningReport,
) -> StoreResult<()> {
    let lane = task.lane_id.get();
    let progress = progress_path(task.project_id, task.id);

    report.removed_journal_rows += count_i64(
        conn,
        "SELECT COUNT(*) FROM journal WHERE run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
        params![lane],
    )
    .await?;
    report.removed_tool_calls += count_i64(
        conn,
        "SELECT COUNT(*) FROM tool_calls WHERE run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
        params![lane],
    )
    .await?;
    report.removed_memory_files += count_i64(
        conn,
        "SELECT COUNT(*) FROM memory_files WHERE path = ?1",
        params![progress.as_str()],
    )
    .await?;

    exec(
        conn,
        "DELETE FROM journal WHERE run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
        params![lane],
    )
    .await?;
    exec(
        conn,
        "DELETE FROM tool_calls WHERE run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
        params![lane],
    )
    .await?;
    exec(
        conn,
        "DELETE FROM checkpoints WHERE task_id = ?1 OR run_id IN (SELECT id FROM runs WHERE lane_id = ?2)",
        params![task.id.get(), lane],
    )
    .await?;
    exec(
        conn,
        "DELETE FROM memory_versions WHERE path = ?1",
        params![progress.as_str()],
    )
    .await?;
    exec(
        conn,
        "DELETE FROM memory_files WHERE path = ?1",
        params![progress.as_str()],
    )
    .await?;
    exec(
        conn,
        "UPDATE tasks SET compacted_at = ?1 WHERE id = ?2",
        params![now.as_millis(), task.id.get()],
    )
    .await?;
    report.compacted_tasks += 1;
    Ok(())
}

async fn workspace_removal_for(
    conn: &Connection,
    task: &CandidateTask,
) -> StoreResult<Option<WorkspaceRemoval>> {
    if task.repositories.is_empty() {
        return Ok(None);
    }
    let repos: Vec<(RepositoryId, String)> = query_all(
        conn,
        "SELECT id, name FROM repositories WHERE project_id = ?1 ORDER BY id",
        params![task.project_id.get()],
        |row| {
            let c = Cols(row);
            Ok((c.id(0)?, c.text(1)?))
        },
    )
    .await?
    .into_iter()
    .filter(|(id, _)| task.repositories.contains(id))
    .collect();
    if repos.is_empty() {
        return Ok(None);
    }
    Ok(Some(WorkspaceRemoval {
        tasks: vec![task.id],
        repositories: repos,
        clones: Vec::new(),
    }))
}

async fn prune_one(
    conn: &Connection,
    task: &CandidateTask,
    report: &mut TaskPruningReport,
) -> StoreResult<()> {
    let tid = task.id.get();
    let lane = task.lane_id.get();
    let progress = progress_path(task.project_id, task.id);

    if let Some(removal) = workspace_removal_for(conn, task).await? {
        report.workspace_removals.push(removal);
    }
    report.removed_journal_rows += count_i64(
        conn,
        "SELECT COUNT(*) FROM journal WHERE run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
        params![lane],
    )
    .await?;
    report.removed_tool_calls += count_i64(
        conn,
        "SELECT COUNT(*) FROM tool_calls WHERE run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
        params![lane],
    )
    .await?;
    report.removed_memory_files += count_i64(
        conn,
        "SELECT COUNT(*) FROM memory_files WHERE path = ?1",
        params![progress.as_str()],
    )
    .await?;

    // Clear references from surviving tables (Lead outbox, standing rules,
    // cross-lane events/decisions, memory provenance, parent links).
    for (sql, p) in [
        (
            "DELETE FROM permission_questions
             WHERE decision_id IN (SELECT id FROM decisions WHERE lane_id = ?1)
                OR question_key IN (SELECT 'event:' || id FROM events WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE outbox SET digest_id = NULL WHERE digest_id IN (SELECT id FROM outbox WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE events SET reply_to_ref = NULL WHERE lane_id != ?1 AND reply_to_ref IN (SELECT id FROM outbox WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE outbox SET session_id = NULL WHERE lane_id != ?1 AND session_id IN (SELECT id FROM sessions WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE outbox SET run_id = NULL WHERE lane_id != ?1 AND run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE outbox SET decision_id = NULL WHERE lane_id != ?1 AND decision_id IN (SELECT id FROM decisions WHERE lane_id = ?1)",
            params![lane],
        ),
        ("DELETE FROM outbox WHERE lane_id = ?1", params![lane]),
        (
            "UPDATE standing_rules SET created_from_decision = NULL
             WHERE created_from_decision IN (SELECT id FROM decisions WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE events SET decision_ref = NULL
             WHERE lane_id != ?1 AND decision_ref IN (SELECT id FROM decisions WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE decisions SET task_id = NULL WHERE lane_id != ?1 AND task_id = ?2",
            params![lane, tid],
        ),
        (
            "UPDATE decisions SET session_id = NULL
             WHERE lane_id != ?1 AND session_id IN (SELECT id FROM sessions WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE decisions SET run_id = NULL
             WHERE lane_id != ?1 AND run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
            params![lane],
        ),
        ("DELETE FROM decisions WHERE lane_id = ?1", params![lane]),
        (
            "DELETE FROM timers WHERE lane_id = ?1 OR task_ref = ?2",
            params![lane, tid],
        ),
        (
            "DELETE FROM checkpoints WHERE task_id = ?1 OR run_id IN (SELECT id FROM runs WHERE lane_id = ?2)",
            params![tid, lane],
        ),
        (
            "UPDATE task_reviews SET verdict_run = NULL
             WHERE verdict_run IN (SELECT id FROM runs WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "DELETE FROM task_reviews
             WHERE task_id = ?1 OR submission_run IN (SELECT id FROM runs WHERE lane_id = ?2)",
            params![tid, lane],
        ),
        (
            "DELETE FROM pr_review_tasks WHERE task_id = ?1",
            params![tid],
        ),
        (
            "DELETE FROM task_dependencies WHERE task_id = ?1 OR depends_on = ?1",
            params![tid],
        ),
        (
            "DELETE FROM dream_items WHERE project_id = ?1 AND kind = 'task' AND item_id = ?2",
            params![task.project_id.get(), tid],
        ),
        (
            "DELETE FROM dream_items
             WHERE project_id = ?1 AND kind = 'run'
               AND item_id IN (SELECT id FROM runs WHERE lane_id = ?2)",
            params![task.project_id.get(), lane],
        ),
        (
            "DELETE FROM memory_versions WHERE path = ?1",
            params![progress.as_str()],
        ),
        (
            "DELETE FROM memory_files WHERE path = ?1",
            params![progress.as_str()],
        ),
        (
            "UPDATE memory_versions SET written_by_run = NULL
             WHERE written_by_run IN (SELECT id FROM runs WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE memory_files SET written_by_run = NULL
             WHERE written_by_run IN (SELECT id FROM runs WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "DELETE FROM run_events
             WHERE run_id IN (SELECT id FROM runs WHERE lane_id = ?1)
                OR event_id IN (SELECT id FROM events WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "DELETE FROM tool_calls WHERE run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "DELETE FROM journal WHERE run_id IN (SELECT id FROM runs WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE events SET session_ref = NULL
             WHERE lane_id != ?1 AND session_ref IN (SELECT id FROM sessions WHERE lane_id = ?1)",
            params![lane],
        ),
        (
            "UPDATE events SET claimed_by_run = NULL
             WHERE lane_id != ?1 AND claimed_by_run IN (SELECT id FROM runs WHERE lane_id = ?1)",
            params![lane],
        ),
        ("DELETE FROM events WHERE lane_id = ?1", params![lane]),
        (
            "UPDATE tasks SET parent_task_id = NULL WHERE parent_task_id = ?1",
            params![tid],
        ),
        ("DELETE FROM tasks WHERE id = ?1", params![tid]),
        ("DELETE FROM runs WHERE lane_id = ?1", params![lane]),
        ("DELETE FROM sessions WHERE lane_id = ?1", params![lane]),
        ("DELETE FROM lanes WHERE id = ?1", params![lane]),
    ] {
        exec(conn, sql, p).await?;
    }
    report.pruned_tasks += 1;
    Ok(())
}

impl HubDb {
    /// Runs one bounded Tier-2 compaction and Tier-3 pruning pass according to
    /// `policy`. Returns counts and any workspace removals for pruned tasks.
    pub async fn prune_tasks_batch(
        &self,
        policy: TaskPruningPolicy,
        now: Timestamp,
    ) -> StoreResult<TaskPruningReport> {
        let mut report = TaskPruningReport::default();
        if policy.batch_size == 0 {
            return Ok(report);
        }
        let batch = i64::try_from(policy.batch_size).unwrap_or(i64::MAX);
        let bypass_dream = i64::from(policy.ignore_dreaming_barrier);

        // Tier 2: compact eligible uncompacted terminal tasks older than compact_before.
        if let Some(cutoff) = policy.compact_before {
            let tx = self.db.begin().await?;
            let conn = tx.conn();
            let candidates = query_all(
                conn,
                &format!(
                    "SELECT t.id, t.project_id, t.lane_id, t.repositories
                     FROM tasks t
                     WHERE {IDLE_TERMINAL_SQL}
                       AND t.compacted_at IS NULL
                       AND {LAST_ACTIVITY_SQL} < ?1
                       AND {DREAMING_BARRIER_SQL}
                     ORDER BY {LAST_ACTIVITY_SQL} ASC, t.id ASC
                     LIMIT ?2"
                ),
                params![cutoff.as_millis(), batch, bypass_dream],
                decode_candidate,
            )
            .await?;
            for task in &candidates {
                compact_one(conn, task, now, &mut report).await?;
            }
            tx.commit().await?;
        }

        // Tier 3: prune eligible terminal tasks older than prune_before, or
        // exceeding max_terminal_per_project (oldest first).
        if policy.prune_before.is_some() || policy.max_terminal_per_project.is_some() {
            let tx = self.db.begin().await?;
            let conn = tx.conn();
            let mut to_prune: Vec<CandidateTask> = Vec::new();
            let mut seen: BTreeSet<TaskId> = BTreeSet::new();

            if let Some(cutoff) = policy.prune_before {
                let aged = query_all(
                    conn,
                    &format!(
                        "SELECT t.id, t.project_id, t.lane_id, t.repositories
                         FROM tasks t
                         WHERE {IDLE_TERMINAL_SQL}
                           AND {LAST_ACTIVITY_SQL} < ?1
                           AND {DREAMING_BARRIER_SQL}
                           AND {NO_ACTIVE_DEPENDENTS_SQL}
                         ORDER BY {LAST_ACTIVITY_SQL} ASC, t.id ASC
                         LIMIT ?2"
                    ),
                    params![cutoff.as_millis(), batch, bypass_dream],
                    decode_candidate,
                )
                .await?;
                for task in aged {
                    if seen.insert(task.id) {
                        to_prune.push(task);
                    }
                }
            }

            if let Some(cap) = policy.max_terminal_per_project
                && to_prune.len() < policy.batch_size
            {
                let projects: Vec<ProjectId> = query_all(
                    conn,
                    "SELECT project_id FROM tasks
                     WHERE status IN ('done', 'failed', 'cancelled')
                     GROUP BY project_id
                     HAVING COUNT(*) > ?1
                     ORDER BY project_id",
                    params![i64::try_from(cap).unwrap_or(i64::MAX)],
                    |row| Cols(row).id(0),
                )
                .await?;
                for project in projects {
                    let remaining = policy.batch_size.saturating_sub(to_prune.len());
                    if remaining == 0 {
                        break;
                    }
                    let total = count_i64(
                        conn,
                        "SELECT COUNT(*) FROM tasks
                         WHERE project_id = ?1 AND status IN ('done', 'failed', 'cancelled')",
                        params![project.get()],
                    )
                    .await?;
                    let excess = total.saturating_sub(cap).min(remaining);
                    if excess == 0 {
                        continue;
                    }
                    let capped = query_all(
                        conn,
                        &format!(
                            "SELECT t.id, t.project_id, t.lane_id, t.repositories
                             FROM tasks t
                             WHERE t.project_id = ?1
                               AND {IDLE_TERMINAL_SQL}
                               AND {DREAMING_BARRIER_SQL}
                               AND {NO_ACTIVE_DEPENDENTS_SQL}
                             ORDER BY {LAST_ACTIVITY_SQL} ASC, t.id ASC
                             LIMIT ?2"
                        ),
                        params![
                            project.get(),
                            i64::try_from(excess).unwrap_or(i64::MAX),
                            bypass_dream
                        ],
                        decode_candidate,
                    )
                    .await?;
                    for task in capped {
                        if seen.insert(task.id) {
                            to_prune.push(task);
                        }
                    }
                }
            }

            if !to_prune.is_empty() {
                preserve_floors(conn).await?;
                for task in &to_prune {
                    prune_one(conn, task, &mut report).await?;
                }
            }
            tx.commit().await?;
        }

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use threadlane_core::model::{NewAgent, NewTask, Recurrence, Rule, TaskOrigin, TaskStatus};
    use threadlane_core::store::{MemoryStore, OrgStore, TaskStore};
    use threadlane_core::time::SystemClock;

    use super::*;
    use crate::Bootstrap;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    const T0: Timestamp = Timestamp::from_millis(1_800_000_000_000);

    async fn setup() -> (HubDb, ProjectId, LaneId, threadlane_core::ids::AgentId) {
        let hub = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(SystemClock))
            .await
            .unwrap();
        let project = hub.general_project().await.unwrap();
        let agent = hub
            .create_agent(&NewAgent {
                name: "Engineer".to_owned(),
                role: "developer".to_owned(),
                instructions: String::new(),
                backends: Vec::new(),
                models: serde_json::json!({}),
                tools: serde_json::json!({}),
                requirements: serde_json::json!({}),
                skills: Vec::new(),
                playbooks: Vec::new(),
            })
            .await
            .unwrap();
        (hub, project.id, project.lead_lane_id, agent.id)
    }

    async fn add_task(
        hub: &HubDb,
        project_id: ProjectId,
        agent_id: threadlane_core::ids::AgentId,
        title: &str,
        created_at: Timestamp,
        parent: Option<TaskId>,
        depends_on: Vec<TaskId>,
    ) -> threadlane_core::model::Task {
        hub.create_task(
            None,
            &NewTask {
                project_id,
                agent_id,
                title: title.to_owned(),
                goal: "Do the work.".to_owned(),
                repositories: Vec::new(),
                parent_task_id: parent,
                depends_on,
                playbooks: Vec::new(),
                origin: TaskOrigin::User,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at,
            },
        )
        .await
        .unwrap()
    }

    async fn assert_fk_clean(hub: &HubDb) {
        let conn = hub.db.reader().unwrap();
        let violations = query_all(&conn, "PRAGMA foreign_key_check", (), |row| {
            Cols(row).text(0)
        })
        .await
        .unwrap();
        assert!(
            violations.is_empty(),
            "foreign key violations: {violations:?}"
        );
    }

    #[tokio::test]
    async fn tier2_compaction_clears_heavy_artifacts_and_progress_file_while_keeping_task_row() {
        let (hub, project_id, _lead_lane, agent_id) = setup().await;
        let old = add_task(&hub, project_id, agent_id, "Old", T0, None, vec![]).await;
        let recent = add_task(
            &hub,
            project_id,
            agent_id,
            "Recent",
            T0 + DAY * 20,
            None,
            vec![],
        )
        .await;
        let active = add_task(&hub, project_id, agent_id, "Active", T0, None, vec![]).await;

        hub.cancel_task(None, old.id, T0 + DAY).await.unwrap();
        hub.cancel_task(None, recent.id, T0 + DAY * 20)
            .await
            .unwrap();

        let old_progress = progress_path(project_id, old.id);
        let recent_progress = progress_path(project_id, recent.id);
        hub.write_by_owner(&old_progress, "# Old\nDone.", T0)
            .await
            .unwrap();
        hub.write_by_owner(&recent_progress, "# Recent\nDone.", T0 + DAY * 20)
            .await
            .unwrap();

        // Seed a committed run, journal row, and tool_call on the old task.
        {
            let tx = hub.db.begin().await.unwrap();
            let c = tx.conn();
            exec(
                c,
                "INSERT INTO sessions (id, lane_id, status, created_at, last_active_at)
                 VALUES (100, ?1, 'closed', ?2, ?2)",
                params![old.lane_id.get(), T0.as_millis()],
            )
            .await
            .unwrap();
            exec(
                c,
                "INSERT INTO runs (id, lane_id, session_id, machine_id, lease_epoch, status, started_at, ended_at)
                 VALUES (200, ?1, 100, 1, 1, 'committed', ?2, ?2)",
                params![old.lane_id.get(), T0.as_millis()],
            )
            .await
            .unwrap();
            exec(
                c,
                "INSERT INTO journal (run_id, seq, at, entry) VALUES (200, 1, ?1, '{}')",
                params![T0.as_millis()],
            )
            .await
            .unwrap();
            exec(
                c,
                "INSERT INTO tool_calls (id, run_id, call_key, tool, arguments, safe_to_repeat, status, started_at)
                 VALUES (300, 200, 'k1', 'read', '{}', 1, 'done', ?1)",
                params![T0.as_millis()],
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }

        let now = T0 + DAY * 25;
        let report = hub
            .prune_tasks_batch(
                TaskPruningPolicy {
                    compact_before: Some(now - DAY * 14),
                    prune_before: None,
                    max_terminal_per_project: None,
                    batch_size: 50,
                    ignore_dreaming_barrier: false,
                },
                now,
            )
            .await
            .unwrap();

        assert_eq!(report.compacted_tasks, 1);
        assert_eq!(report.pruned_tasks, 0);
        assert_eq!(report.removed_journal_rows, 1);
        assert_eq!(report.removed_tool_calls, 1);
        assert_eq!(report.removed_memory_files, 1);

        // Task row remains, progress file is gone, recent & active untouched.
        assert!(hub.task(old.id).await.unwrap().is_some());
        assert!(hub.read(&old_progress).await.unwrap().is_none());
        assert!(hub.read(&recent_progress).await.unwrap().is_some());
        assert!(hub.task(active.id).await.unwrap().is_some());

        // Second pass is a no-op (idempotent).
        let second = hub
            .prune_tasks_batch(
                TaskPruningPolicy {
                    compact_before: Some(now - DAY * 14),
                    prune_before: None,
                    max_terminal_per_project: None,
                    batch_size: 50,
                    ignore_dreaming_barrier: false,
                },
                now,
            )
            .await
            .unwrap();
        assert_eq!(second.compacted_tasks, 0);
        assert_fk_clean(&hub).await;
    }

    #[tokio::test]
    async fn tier3_pruning_respects_dependents_dreaming_barrier_and_deletion_floors() {
        let (hub, project_id, lead_lane, agent_id) = setup().await;
        let parent = add_task(&hub, project_id, agent_id, "Parent", T0, None, vec![]).await;
        let active_child = add_task(
            &hub,
            project_id,
            agent_id,
            "ActiveChild",
            T0,
            Some(parent.id),
            vec![],
        )
        .await;
        let standalone = add_task(&hub, project_id, agent_id, "Standalone", T0, None, vec![]).await;

        hub.cancel_task(None, parent.id, T0).await.unwrap();
        hub.cancel_task(None, standalone.id, T0).await.unwrap();

        // Enable dreaming on the project: neither task has been reviewed yet.
        hub.set_dreaming(
            project_id,
            lead_lane,
            Some((
                Recurrence {
                    rule: Rule::Daily { hour: 3, minute: 0 },
                    zone: "UTC".to_owned(),
                },
                T0 + DAY,
            )),
            T0,
        )
        .await
        .unwrap();

        let now = T0 + DAY * 100;
        let blocked_by_dream = hub
            .prune_tasks_batch(
                TaskPruningPolicy {
                    compact_before: None,
                    prune_before: Some(now - DAY * 90),
                    max_terminal_per_project: None,
                    batch_size: 50,
                    ignore_dreaming_barrier: false,
                },
                now,
            )
            .await
            .unwrap();
        assert_eq!(blocked_by_dream.pruned_tasks, 0);

        // Mark both tasks as reviewed by a dream (last_review > 0).
        {
            let tx = hub.db.begin().await.unwrap();
            for tid in [parent.id.get(), standalone.id.get()] {
                exec(
                    tx.conn(),
                    "INSERT INTO dream_items (project_id, kind, item_id, last_review)
                     VALUES (?1, 'task', ?2, 42)",
                    params![project_id.get(), tid],
                )
                .await
                .unwrap();
            }
            tx.commit().await.unwrap();
        }

        // Now standalone is pruned, while parent is still protected by active_child!
        let report = hub
            .prune_tasks_batch(
                TaskPruningPolicy {
                    compact_before: None,
                    prune_before: Some(now - DAY * 90),
                    max_terminal_per_project: None,
                    batch_size: 50,
                    ignore_dreaming_barrier: false,
                },
                now,
            )
            .await
            .unwrap();
        assert_eq!(report.pruned_tasks, 1);
        assert!(hub.task(standalone.id).await.unwrap().is_none());
        assert!(hub.task(parent.id).await.unwrap().is_some());

        // Deletion floors guarantee the next created task ID never reuses standalone's ID.
        let after = add_task(&hub, project_id, agent_id, "After", now, None, vec![]).await;
        assert!(after.id.get() > standalone.id.get());

        // Once active_child finishes too, both parent and active_child are eligible
        // and get pruned cleanly (with ignore_dreaming_barrier).
        hub.cancel_task(None, active_child.id, now).await.unwrap();
        let report2 = hub
            .prune_tasks_batch(
                TaskPruningPolicy {
                    compact_before: None,
                    prune_before: Some(now - DAY * 90),
                    max_terminal_per_project: None,
                    batch_size: 50,
                    ignore_dreaming_barrier: true,
                },
                now,
            )
            .await
            .unwrap();
        assert_eq!(report2.pruned_tasks, 2);
        assert!(hub.task(parent.id).await.unwrap().is_none());
        assert!(hub.task(active_child.id).await.unwrap().is_none());
        assert_fk_clean(&hub).await;
    }

    #[tokio::test]
    async fn tier3_count_cap_prunes_oldest_excess_terminal_tasks() {
        let (hub, project_id, _lead_lane, agent_id) = setup().await;
        let mut ids = Vec::new();
        for i in 0..5_u32 {
            let t = add_task(
                &hub,
                project_id,
                agent_id,
                &format!("Task {i}"),
                T0 + DAY * i,
                None,
                vec![],
            )
            .await;
            hub.cancel_task(None, t.id, T0 + DAY * i).await.unwrap();
            ids.push(t.id);
        }

        let report = hub
            .prune_tasks_batch(
                TaskPruningPolicy {
                    compact_before: None,
                    prune_before: None,
                    max_terminal_per_project: Some(2),
                    batch_size: 50,
                    ignore_dreaming_barrier: false,
                },
                T0 + DAY * 10,
            )
            .await
            .unwrap();
        assert_eq!(report.pruned_tasks, 3);
        for old_id in &ids[0..3] {
            assert!(hub.task(*old_id).await.unwrap().is_none());
        }
        for kept_id in &ids[3..5] {
            assert!(hub.task(*kept_id).await.unwrap().is_some());
        }
        assert_fk_clean(&hub).await;
    }
}
