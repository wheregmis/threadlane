//! [`TaskStore`] on the hub database (Plan.md §5.2).

use async_trait::async_trait;
use serde_json::json;
use threadlane_core::ids::{LaneId, ProjectId, TaskId};
use threadlane_core::model::{
    EventKind, LaneKind, NewEvent, NewTask, Task, TaskOrigin, TaskStatus, Trust,
};
use threadlane_core::store::{StoreError, StoreResult, TaskStore};
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::inbox::insert_event_in;
use crate::row::{Cols, json_text, opt_raw, params};

/// Source name of `task_start` events.
pub const TASK_SOURCE: &str = "task";

const TASK_FIELDS: &str = "id, project_id, lane_id, agent_id, title, goal, repositories,
    parent_task_id, origin, status, priority, created_by_call, last_machine_id, waiting_reason,
    result, created_at";

fn decode_task(row: &Row, depends_on: Vec<TaskId>) -> StoreResult<Task> {
    let c = Cols(row);
    Ok(Task {
        id: c.id(0)?,
        project_id: c.id(1)?,
        lane_id: c.id(2)?,
        agent_id: c.id(3)?,
        title: c.text(4)?,
        goal: c.text(5)?,
        repositories: c.de(6)?,
        parent_task_id: c.opt_id(7)?,
        depends_on,
        origin: c.parse(8)?,
        status: c.parse(9)?,
        priority: c.i64(10)?,
        created_by_call: c.opt_text(11)?,
        last_machine_id: c.opt_id(12)?,
        waiting_reason: c.opt_text(13)?,
        result: c.opt_json(14)?,
        created_at: c.ts(15)?,
    })
}

async fn task_in(
    conn: &Connection,
    filter: &str,
    value: turso::Value,
) -> StoreResult<Option<Task>> {
    let found = query_opt(
        conn,
        &format!("SELECT {TASK_FIELDS} FROM tasks WHERE {filter}"),
        vec![value],
        |row| decode_task(row, Vec::new()),
    )
    .await?;
    match found {
        Some(mut task) => {
            task.depends_on = dependencies(conn, task.id).await?;
            Ok(Some(task))
        }
        None => Ok(None),
    }
}

async fn dependencies(conn: &Connection, id: TaskId) -> StoreResult<Vec<TaskId>> {
    query_all(
        conn,
        "SELECT depends_on FROM task_dependencies WHERE task_id = ?1 ORDER BY depends_on",
        params![id.get()],
        |row| Cols(row).id(0),
    )
    .await
}

#[async_trait]
impl TaskStore for HubDb {
    async fn create_task(&self, task: &NewTask) -> StoreResult<Task> {
        if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Queued) {
            return Err(StoreError::Invalid(format!(
                "a task starts proposed or queued, not {}",
                task.status
            )));
        }
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        if let Some(call) = &task.created_by_call
            && let Some(existing) =
                task_in(conn, "created_by_call = ?1", call.as_str().into()).await?
        {
            return Ok(existing);
        }
        let lane_id: LaneId = query_one(
            conn,
            "INSERT INTO lanes (project_id, kind) VALUES (?1, ?2) RETURNING id",
            params![task.project_id.get(), LaneKind::Task.as_str()],
            |row| Cols(row).id(0),
        )
        .await?;
        let id: TaskId = query_one(
            conn,
            "INSERT INTO tasks (project_id, lane_id, agent_id, title, goal, repositories,
                 parent_task_id, origin, status, priority, created_by_call, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) RETURNING id",
            params![
                task.project_id.get(),
                lane_id.get(),
                task.agent_id.get(),
                task.title.as_str(),
                task.goal.as_str(),
                json_text(&task.repositories)?,
                opt_raw(task.parent_task_id, |id| id.get()),
                task.origin.as_str(),
                task.status.as_str(),
                task.priority,
                task.created_by_call.as_deref(),
                task.created_at.as_millis(),
            ],
            |row| Cols(row).id(0),
        )
        .await?;
        exec(
            conn,
            "UPDATE lanes SET task_id = ?1 WHERE id = ?2",
            params![id.get(), lane_id.get()],
        )
        .await?;
        for dependency in &task.depends_on {
            exec(
                conn,
                "INSERT INTO task_dependencies (task_id, depends_on) VALUES (?1, ?2)",
                params![id.get(), dependency.get()],
            )
            .await?;
        }
        if task.status == TaskStatus::Queued {
            insert_event_in(
                conn,
                &NewEvent {
                    project_id: task.project_id,
                    lane_id,
                    source: TASK_SOURCE.to_owned(),
                    source_event_id: format!("start:{}", id.get()),
                    kind: EventKind::TaskStart,
                    payload: json!({ "task_id": id, "title": task.title, "goal": task.goal }),
                    trust: Trust::System,
                    requested: matches!(task.origin, TaskOrigin::User | TaskOrigin::UserRoutine),
                    session_ref: None,
                    decision_ref: None,
                    reply_to_ref: None,
                    received_at: task.created_at,
                },
            )
            .await?;
        }
        let created = task_in(conn, "id = ?1", id.get().into())
            .await?
            .ok_or_else(|| StoreError::not_found("task", id))?;
        tx.commit().await?;
        Ok(created)
    }

    async fn task(&self, id: TaskId) -> StoreResult<Option<Task>> {
        let conn = self.db.reader()?;
        task_in(&conn, "id = ?1", id.get().into()).await
    }

    async fn tasks(&self, project_id: ProjectId) -> StoreResult<Vec<Task>> {
        let conn = self.db.reader()?;
        let mut tasks = query_all(
            &conn,
            &format!("SELECT {TASK_FIELDS} FROM tasks WHERE project_id = ?1 ORDER BY id"),
            params![project_id.get()],
            |row| decode_task(row, Vec::new()),
        )
        .await?;
        for task in &mut tasks {
            task.depends_on = dependencies(&conn, task.id).await?;
        }
        Ok(tasks)
    }

    async fn set_task_status(
        &self,
        id: TaskId,
        status: TaskStatus,
        waiting_reason: Option<&str>,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let changed = exec(
            tx.conn(),
            "UPDATE tasks SET status = ?1, waiting_reason = ?2 WHERE id = ?3",
            params![status.as_str(), waiting_reason, id.get()],
        )
        .await?;
        if changed != 1 {
            return Err(StoreError::not_found("task", id));
        }
        tx.commit().await
    }
}
