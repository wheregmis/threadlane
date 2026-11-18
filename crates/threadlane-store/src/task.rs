//! [`TaskStore`] on the hub database (Plan.md §5.2), and the task side of the
//! run cycle's transactions.

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::backend::{free_backends, usable_backends};
use threadlane_core::ids::{DecisionId, EventId, LaneId, MachineId, ProjectId, RunId, TaskId};
use threadlane_core::memory::progress_path;
use threadlane_core::model::{
    BackendKind, EventKind, LaneKind, Lease, NewEvent, NewTask, Task, TaskStatus, Trust,
};
use threadlane_core::store::{StoreError, StoreResult, TaskStore};
use threadlane_core::time::Timestamp;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::inbox::{check_lease, insert_event_in};
use crate::memory::live_in;
use crate::row::{Cols, json_text, opt_raw, params};
use crate::task_review;

/// Source name of `task_start` and `task_result` events.
pub const TASK_SOURCE: &str = "task";

/// The `waiting_reason` of a task that may not start for lack of a model
/// account (Plan.md §5.3 "blocked, not failed").
pub const BLOCKED_REASON: &str = "blocked: every model account it can use is at its usage limit";

/// The `waiting_reason` of a task with repositories offered to a machine
/// that is not logged in to `gh` (Plan.md §5.4 item 5).
pub const NO_GH_REASON: &str =
    "this machine is not logged in to gh, so it cannot reach the task's repositories";

/// The `waiting_reason` of a task whose run ended without `finish_task` and
/// left nothing to wake it: no decision, timer or event.
pub const UNWOKEN_REASON: &str = "ended without finishing and nothing is set to wake it";

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

async fn existing_task(conn: &Connection, id: TaskId) -> StoreResult<Task> {
    task_in(conn, "id = ?1", id.get().into())
        .await?
        .ok_or_else(|| StoreError::not_found("task", id))
}

/// The task a lane works, if it is a task lane.
pub(crate) async fn task_of_lane(conn: &Connection, lane_id: LaneId) -> StoreResult<Option<Task>> {
    task_in(conn, "lane_id = ?1", lane_id.get().into()).await
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

/// Checks that a run's lease is current and works in `project_id`.
async fn check_run_lease(
    conn: &Connection,
    lease: Option<&Lease>,
    project_id: ProjectId,
) -> StoreResult<()> {
    let Some(lease) = lease else {
        return Ok(());
    };
    check_lease(conn, lease).await?;
    let lane_project: ProjectId = query_one(
        conn,
        "SELECT project_id FROM lanes WHERE id = ?1",
        params![lease.lane_id.get()],
        |row| Cols(row).id(0),
    )
    .await?;
    if lane_project != project_id {
        return Err(StoreError::Invalid(format!(
            "lane {} works in project {lane_project}, not {project_id}",
            lease.lane_id
        )));
    }
    Ok(())
}

/// The oldest decision pending in the lane, if any.
async fn pending_decision(conn: &Connection, lane_id: LaneId) -> StoreResult<Option<DecisionId>> {
    query_opt(
        conn,
        "SELECT id FROM decisions WHERE lane_id = ?1 AND status = 'pending' ORDER BY id LIMIT 1",
        params![lane_id.get()],
        |row| Cols(row).id(0),
    )
    .await
}

/// Whether the lane has an active timer of its own, which will wake it.
async fn active_timer(conn: &Connection, lane_id: LaneId) -> StoreResult<bool> {
    let count = query_one(
        conn,
        "SELECT COUNT(*) FROM timers WHERE lane_id = ?1 AND status = 'active'",
        params![lane_id.get()],
        |row| Cols(row).i64(0),
    )
    .await?;
    Ok(count > 0)
}

/// Whether a branch of the lane's task was pushed for it, so that GitHub's
/// events on it reach the lane. shortcut: counted even on a hub without a
/// GitHub source; ask the hub's settings when one can run without it.
async fn published(conn: &Connection, lane_id: LaneId) -> StoreResult<bool> {
    let outcomes: Vec<Json> = query_all(
        conn,
        "SELECT outcome FROM decisions WHERE lane_id = ?1 AND outcome IS NOT NULL",
        params![lane_id.get()],
        |row| Cols(row).json(0),
    )
    .await?;
    Ok(outcomes
        .iter()
        .any(|outcome| outcome["performed"] == true && outcome["branch"].is_string()))
}

/// Whether anything will wake the lane's sleeping task: an active timer
/// of its own, or a published branch GitHub reports on.
async fn will_wake(conn: &Connection, lane_id: LaneId) -> StoreResult<bool> {
    Ok(active_timer(conn, lane_id).await? || published(conn, lane_id).await?)
}

/// The number of events pending in the lane.
async fn pending_events(conn: &Connection, lane_id: LaneId) -> StoreResult<i64> {
    query_one(
        conn,
        "SELECT COUNT(*) FROM events WHERE lane_id = ?1 AND status = 'pending'",
        params![lane_id.get()],
        |row| Cols(row).i64(0),
    )
    .await
}

/// Why a task waits on a pending decision.
fn waiting_on(decision: DecisionId) -> String {
    format!("waiting on your answer to {decision}")
}

/// Puts a running task whose lane has a pending decision in
/// `waiting_on_you`, with the reason. Returns whether it did.
async fn wait_on_decision(conn: &Connection, task: &Task) -> StoreResult<bool> {
    let Some(decision) = pending_decision(conn, task.lane_id).await? else {
        return Ok(false);
    };
    transition(conn, task, TaskStatus::WaitingOnYou).await?;
    exec(
        conn,
        "UPDATE tasks SET waiting_reason = ?1 WHERE id = ?2",
        params![waiting_on(decision), task.id.get()],
    )
    .await?;
    Ok(true)
}

/// The task's result with the branch and pull request of the calls it had
/// performed for it (docs/milestone-3.md §4 "Result"). shortcut: with
/// several repositories the last one performed wins; key them by repository
/// when a task publishes more than one.
pub(crate) async fn with_published(conn: &Connection, task: &Task) -> StoreResult<Option<Json>> {
    let outcomes: Vec<Json> = query_all(
        conn,
        "SELECT outcome FROM decisions WHERE lane_id = ?1 AND outcome IS NOT NULL ORDER BY id",
        params![task.lane_id.get()],
        |row| Cols(row).json(0),
    )
    .await?;
    let mut result = task.result.clone();
    if let Some(Json::Object(fields)) = result.as_mut() {
        for outcome in outcomes
            .iter()
            .filter(|outcome| outcome.get("performed") == Some(&Json::Bool(true)))
        {
            for key in ["branch", "pull_request"] {
                if let Some(value) = outcome.get(key) {
                    fields.insert(key.to_owned(), value.clone());
                }
            }
        }
    }
    Ok(result)
}

/// Moves a task to `status`, refusing what the lifecycle does not allow.
pub(crate) async fn transition(
    conn: &Connection,
    task: &Task,
    status: TaskStatus,
) -> StoreResult<()> {
    if !task.status.can_become(status) {
        return Err(StoreError::Conflict(format!(
            "task {} cannot go from {} to {status}",
            task.id, task.status
        )));
    }
    exec(
        conn,
        "UPDATE tasks SET status = ?1 WHERE id = ?2",
        params![status.as_str(), task.id.get()],
    )
    .await?;
    Ok(())
}

async fn insert_start_event(conn: &Connection, task: &Task) -> StoreResult<()> {
    insert_event_in(
        conn,
        &NewEvent {
            project_id: task.project_id,
            lane_id: task.lane_id,
            source: TASK_SOURCE.to_owned(),
            source_event_id: format!("start:{}", task.id.get()),
            kind: EventKind::TaskStart,
            payload: json!({ "task_id": task.id, "title": task.title, "goal": task.goal }),
            trust: Trust::System,
            requested: task.origin.is_requested(),
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: task.created_at,
        },
    )
    .await?;
    Ok(())
}

/// Ends a task as `status` and tells the project's lead lane, once, with
/// `why` as the error of a failed task or the reason for another ending. A
/// task that failed or was cancelled cancels the tasks waiting for it, and
/// theirs in turn.
pub(crate) async fn end_task(
    conn: &Connection,
    task: &Task,
    status: TaskStatus,
    why: Option<&str>,
    now: Timestamp,
) -> StoreResult<()> {
    transition(conn, task, status).await?;
    if status == TaskStatus::Cancelled {
        exec(conn, "UPDATE task_reviews SET state = 'cancelled' WHERE task_id = ?1 AND state IN ('submitted', 'pending')",
            params![task.id.get()]).await?;
    }
    // A terminal task's lane can drain its events without starting a backend.
    exec(
        conn,
        "UPDATE lanes SET stopped = 0 WHERE id = ?1 AND stopped = 1",
        params![task.lane_id.get()],
    )
    .await?;
    // Nothing is left to approve for a task that has ended.
    exec(
        conn,
        "UPDATE decisions SET status = 'cancelled' WHERE lane_id = ?1 AND status = 'pending'",
        params![task.lane_id.get()],
    )
    .await?;
    let mut payload = json!({
        "task_id": task.id,
        "task": task.id.to_string(),
        "title": task.title,
        "status": status,
        "result": task.result,
    });
    if let Some(why) = why {
        let key = if status == TaskStatus::Failed {
            "error"
        } else {
            "reason"
        };
        payload[key] = json!(why);
    }
    tell_lead(
        conn,
        task,
        format!("result:{}", task.id.get()),
        payload,
        now,
    )
    .await?;
    if matches!(status, TaskStatus::Failed | TaskStatus::Cancelled) {
        cancel_dependents(conn, task, status, now).await?;
    }
    Ok(())
}

/// Inserts a `task_result` event about `task` in its project's lead lane,
/// once per `source_event_id`.
async fn tell_lead(
    conn: &Connection,
    task: &Task,
    source_event_id: String,
    payload: Json,
    now: Timestamp,
) -> StoreResult<()> {
    let lead_lane_id: LaneId = query_one(
        conn,
        "SELECT id FROM lanes WHERE project_id = ?1 AND kind = ?2",
        params![task.project_id.get(), LaneKind::Lead.as_str()],
        |row| Cols(row).id(0),
    )
    .await?;
    insert_event_in(
        conn,
        &NewEvent {
            project_id: task.project_id,
            lane_id: lead_lane_id,
            source: TASK_SOURCE.to_owned(),
            source_event_id,
            kind: EventKind::TaskResult,
            payload,
            trust: Trust::System,
            requested: task.origin.is_requested(),
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: now,
        },
    )
    .await?;
    Ok(())
}

/// Cancels every task that waits for `task`, which ended as `status`, since
/// it can never become `done`.
async fn cancel_dependents(
    conn: &Connection,
    task: &Task,
    status: TaskStatus,
    now: Timestamp,
) -> StoreResult<()> {
    let dependents: Vec<TaskId> = query_all(
        conn,
        "SELECT t.id FROM task_dependencies d JOIN tasks t ON t.id = d.task_id
         WHERE d.depends_on = ?1 AND t.status NOT IN ('done', 'failed', 'cancelled')
         ORDER BY t.id",
        params![task.id.get()],
        |row| Cols(row).id(0),
    )
    .await?;
    let reason = dependency_ended(task.id, status);
    for id in dependents {
        let dependent = existing_task(conn, id).await?;
        Box::pin(end_task(
            conn,
            &dependent,
            TaskStatus::Cancelled,
            Some(&reason),
            now,
        ))
        .await?;
    }
    Ok(())
}

/// Why a task was cancelled when `dependency` ended as `status`.
fn dependency_ended(dependency: TaskId, status: TaskStatus) -> String {
    let ended = if status == TaskStatus::Failed {
        "failed"
    } else {
        "was cancelled"
    };
    format!("{dependency}, which it depended on, {ended}")
}

/// [`SessionStore::start_run`]: a queued task starts running here.
///
/// [`SessionStore::start_run`]: threadlane_core::store::SessionStore::start_run
pub(crate) async fn on_run_started(
    conn: &Connection,
    lane_id: LaneId,
    machine_id: MachineId,
) -> StoreResult<()> {
    exec(
        conn,
        "UPDATE tasks SET status = ?1, last_machine_id = ?2, waiting_reason = NULL
         WHERE lane_id = ?3 AND status = ?4",
        params![
            TaskStatus::Running.as_str(),
            machine_id.get(),
            lane_id.get(),
            TaskStatus::Queued.as_str()
        ],
    )
    .await?;
    Ok(())
}

/// [`SessionStore::commit_run`]: a running task with a pending decision
/// waits on you; one that recorded its result is done, with the branch and
/// pull request performed for it; otherwise it sleeps until an event
/// arrives, or is queued again if one already has. A task left sleeping
/// with nothing to wake it, neither a timer of its own nor a published
/// branch, carries [`UNWOKEN_REASON`], and its lead lane is told once per
/// run `run_id`.
///
/// [`SessionStore::commit_run`]: threadlane_core::store::SessionStore::commit_run
pub(crate) async fn on_run_committed(
    conn: &Connection,
    lane_id: LaneId,
    run_id: RunId,
    now: Timestamp,
) -> StoreResult<()> {
    let Some(task) = task_of_lane(conn, lane_id).await? else {
        return Ok(());
    };
    if task.status == TaskStatus::InReview {
        return task_review::finish_review(conn, &task, run_id, now).await;
    }
    if task.status != TaskStatus::Running || wait_on_decision(conn, &task).await? {
        return Ok(());
    }
    if task_review::submitted(conn, task.id, run_id).await? {
        return task_review::start_review(conn, &task, run_id, now).await;
    }
    if task.result.is_some() {
        let result = with_published(conn, &task).await?;
        exec(
            conn,
            "UPDATE tasks SET result = ?1 WHERE id = ?2",
            params![json_text(&result)?, task.id.get()],
        )
        .await?;
        let task = Task { result, ..task };
        return end_task(conn, &task, TaskStatus::Done, None, now).await;
    }
    if pending_events(conn, lane_id).await? > 0 {
        return transition(conn, &task, TaskStatus::Queued).await;
    }
    transition(conn, &task, TaskStatus::Sleeping).await?;
    if will_wake(conn, lane_id).await? {
        return Ok(());
    }
    exec(
        conn,
        "UPDATE tasks SET waiting_reason = ?1 WHERE id = ?2",
        params![UNWOKEN_REASON, task.id.get()],
    )
    .await?;
    let payload = json!({
        "task_id": task.id,
        "task": task.id.to_string(),
        "title": task.title,
        "status": TaskStatus::Sleeping,
        "reason": UNWOKEN_REASON,
    });
    tell_lead(
        conn,
        &task,
        format!("unwoken:{}", run_id.get()),
        payload,
        now,
    )
    .await
}

/// [`TaskStore::left_unwoken`] on one connection.
async fn left_unwoken(conn: &Connection, lane_id: LaneId) -> StoreResult<bool> {
    let Some(task) = task_of_lane(conn, lane_id).await? else {
        return Ok(false);
    };
    Ok(task.status == TaskStatus::Running
        && task.result.is_none()
        && query_opt(
            conn,
            "SELECT 1 FROM task_reviews JOIN runs ON runs.id = submission_run
            WHERE task_id = ?1 AND state = 'submitted' AND runs.status = 'active'",
            params![task.id.get()],
            |row| Cols(row).i64(0),
        )
        .await?
        .is_none()
        && pending_decision(conn, lane_id).await?.is_none()
        && pending_events(conn, lane_id).await? == 0
        && !will_wake(conn, lane_id).await?)
}

/// [`SessionStore::fail_run`]: a running task is queued again, or fails when
/// its events died.
///
/// [`SessionStore::fail_run`]: threadlane_core::store::SessionStore::fail_run
pub(crate) async fn on_run_failed(
    conn: &Connection,
    lane_id: LaneId,
    died: bool,
    error: &str,
    now: Timestamp,
) -> StoreResult<()> {
    let Some(task) = task_of_lane(conn, lane_id).await? else {
        return Ok(());
    };
    if task.status == TaskStatus::InReview && died {
        exec(
            conn,
            "UPDATE tasks SET waiting_reason = ?2 WHERE id = ?1",
            params![task.id.get(), format!("Reviewer failed: {error}")],
        )
        .await?;
        return Ok(());
    }
    if task.status != TaskStatus::Running {
        return Ok(());
    }
    if died {
        end_task(conn, &task, TaskStatus::Failed, Some(error), now).await
    } else if wait_on_decision(conn, &task).await? {
        Ok(())
    } else {
        transition(conn, &task, TaskStatus::Queued).await
    }
}

/// A lane's lease went away with a run in flight: its task is queued again,
/// or waits on you if the run asked something.
pub(crate) async fn on_lane_reclaimed(conn: &Connection, lane_id: LaneId) -> StoreResult<()> {
    if let Some(task) = task_of_lane(conn, lane_id).await?
        && task.status == TaskStatus::Running
        && wait_on_decision(conn, &task).await?
    {
        return Ok(());
    }
    exec(
        conn,
        "UPDATE tasks SET status = ?1 WHERE lane_id = ?2 AND status = ?3",
        params![
            TaskStatus::Queued.as_str(),
            lane_id.get(),
            TaskStatus::Running.as_str()
        ],
    )
    .await?;
    Ok(())
}

/// An event arrived in a lane: a sleeping task is queued again (Plan.md
/// §5.2), and so is one waiting on you once nothing is pending to answer.
pub(crate) async fn on_event_inserted(conn: &Connection, lane_id: LaneId) -> StoreResult<()> {
    exec(
        conn,
        "UPDATE tasks SET status = ?1, waiting_reason = NULL WHERE lane_id = ?2 AND status = ?3",
        params![
            TaskStatus::Queued.as_str(),
            lane_id.get(),
            TaskStatus::Sleeping.as_str()
        ],
    )
    .await?;
    if pending_decision(conn, lane_id).await?.is_none() {
        exec(
            conn,
            "UPDATE tasks SET status = ?1, waiting_reason = NULL WHERE lane_id = ?2 AND status = ?3",
            params![
                TaskStatus::Queued.as_str(),
                lane_id.get(),
                TaskStatus::WaitingOnYou.as_str()
            ],
        )
        .await?;
    }
    Ok(())
}

/// Why the lane's task may not start now (Plan.md §5.2, §5.4 items 1 and 2),
/// or `None` if it may, or if the lane has no task. A limit of 0 is no
/// limit. Only tasks whose lane holds a live lease count as running.
async fn waiting_reason(
    conn: &Connection,
    task: &Task,
    now: Timestamp,
) -> StoreResult<Option<String>> {
    let (paused, project_limit) = query_one(
        conn,
        "SELECT paused, max_parallel_tasks FROM projects WHERE id = ?1",
        params![task.project_id.get()],
        |row| {
            let c = Cols(row);
            Ok((c.bool(0)?, c.i64(1)?))
        },
    )
    .await?;
    if paused {
        return Ok(Some("the project is paused".to_owned()));
    }
    if let Some(decision) = pending_decision(conn, task.lane_id).await? {
        return Ok(Some(waiting_on(decision)));
    }
    let waiting_for: Vec<TaskId> = query_all(
        conn,
        "SELECT t.id FROM task_dependencies d JOIN tasks t ON t.id = d.depends_on
         WHERE d.task_id = ?1 AND t.status != ?2 ORDER BY t.id",
        params![task.id.get(), TaskStatus::Done.as_str()],
        |row| Cols(row).id(0),
    )
    .await?;
    if !waiting_for.is_empty() {
        let names: Vec<String> = waiting_for.iter().map(ToString::to_string).collect();
        return Ok(Some(format!("waiting for {} to be done", names.join(", "))));
    }
    if project_limit > 0 {
        let running = query_one(
            conn,
            "SELECT COUNT(*) FROM tasks t JOIN lanes l ON l.id = t.lane_id
             WHERE t.project_id = ?1 AND t.id != ?2 AND l.lease_owner IS NOT NULL
               AND l.lease_expires_at > ?3 AND t.status NOT IN ('done', 'failed', 'cancelled')",
            params![task.project_id.get(), task.id.get(), now.as_millis()],
            |row| Cols(row).i64(0),
        )
        .await?;
        if running >= project_limit {
            return Ok(Some(format!(
                "the project already runs its limit of {project_limit} tasks at once"
            )));
        }
    }
    Ok(None)
}

/// [`BLOCKED_REASON`] if the agent working the lane has backends usable on
/// the machine but every one of them runs on an account cooling down
/// (Plan.md §5.4 item 4). A machine with none of them usable at all is not
/// blocked for lack of an account; placement leaves it out.
async fn blocked(
    conn: &Connection,
    lane_id: LaneId,
    machine_id: MachineId,
    now: Timestamp,
) -> StoreResult<Option<String>> {
    let preferred = query_opt(
        conn,
        "SELECT a.backends FROM lanes l JOIN agents a ON a.id = COALESCE(
             (SELECT reviewer FROM task_reviews WHERE task_id = l.task_id AND state = 'pending'),
             (SELECT agent_id FROM tasks WHERE lane_id = l.id),
             (SELECT agent_id FROM bindings WHERE project_id = l.project_id AND is_lead = 1))
         WHERE l.id = ?1",
        params![lane_id.get()],
        |row| Cols(row).de::<Vec<BackendKind>>(0),
    )
    .await?;
    let backends = query_opt(
        conn,
        "SELECT backends FROM machines WHERE id = ?1",
        params![machine_id.get()],
        |row| Cols(row).json(0),
    )
    .await?;
    let (Some(preferred), Some(backends)) = (preferred, backends) else {
        return Ok(None);
    };
    let cooling: Vec<String> = query_all(
        conn,
        "SELECT label FROM accounts WHERE cooldown_until > ?1",
        params![now.as_millis()],
        |row| Cols(row).text(0),
    )
    .await?;
    let wanted = |kind: &BackendKind| preferred.is_empty() || preferred.contains(kind);
    let usable = usable_backends(&backends).iter().any(wanted);
    let free = free_backends(&backends, machine_id, &cooling)
        .iter()
        .any(wanted);
    Ok((usable && !free).then(|| BLOCKED_REASON.to_owned()))
}

/// Whether the machine reported `gh` logged in.
async fn gh_login(conn: &Connection, machine_id: MachineId) -> StoreResult<bool> {
    Ok(query_opt(
        conn,
        "SELECT gh_login FROM machines WHERE id = ?1",
        params![machine_id.get()],
        |row| Cols(row).bool(0),
    )
    .await?
    .unwrap_or(false))
}

/// [`InboxStore::grant_lease`]: records why the lane's task may not start, or
/// clears the reason. Returns whether the lane may be granted.
///
/// [`InboxStore::grant_lease`]: threadlane_core::store::InboxStore::grant_lease
pub(crate) async fn check_eligible(
    conn: &Connection,
    lane_id: LaneId,
    machine_id: MachineId,
    now: Timestamp,
) -> StoreResult<bool> {
    let task = task_of_lane(conn, lane_id).await?;
    let reason = match &task {
        Some(task) if task.status.is_terminal() => None,
        Some(task) if task.status == TaskStatus::InReview => {
            match task_review::pending_in(conn, task.id).await? {
                Some(review) if !review.revisions.is_empty() && review.machine != machine_id => {
                    return Ok(false);
                }
                Some(_) => match waiting_reason(conn, task, now).await? {
                    Some(reason) => Some(reason),
                    None => blocked(conn, lane_id, machine_id, now).await?,
                },
                None => Some("Waiting for a Reviewer".to_owned()),
            }
        }
        Some(task) => match waiting_reason(conn, task, now).await? {
            None if !task.repositories.is_empty() && !gh_login(conn, machine_id).await? => {
                Some(NO_GH_REASON.to_owned())
            }
            None => blocked(conn, lane_id, machine_id, now).await?,
            reason => reason,
        },
        None => blocked(conn, lane_id, machine_id, now).await?,
    };
    exec(
        conn,
        "UPDATE tasks SET waiting_reason = ?1 WHERE lane_id = ?2",
        params![reason.as_deref(), lane_id.get()],
    )
    .await?;
    Ok(reason.is_none())
}

impl HubDb {
    /// Rechecks review eligibility if the Engineer handed over under an existing lease.
    pub async fn review_can_start(&self, lease: &Lease, now: Timestamp) -> StoreResult<bool> {
        let tx = self.db.begin().await?;
        check_lease(tx.conn(), lease).await?;
        let allowed = check_eligible(tx.conn(), lease.lane_id, lease.machine_id, now).await?;
        tx.commit().await?;
        Ok(allowed)
    }

    /// Creates a task and records the space Lead's ask in the same transaction.
    pub async fn create_task_for_ask(
        &self,
        lease: Option<&Lease>,
        task: &NewTask,
        ask: Option<EventId>,
    ) -> StoreResult<Task> {
        if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Queued) {
            return Err(StoreError::Invalid(format!(
                "a task starts proposed or queued, not {}",
                task.status
            )));
        }
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_run_lease(conn, lease, task.project_id).await?;
        if let Some(call) = &task.created_by_call
            && let Some(existing) =
                task_in(conn, "created_by_call = ?1", call.as_str().into()).await?
        {
            return Ok(existing);
        }
        crate::org::require_active_project(conn, task.project_id).await?;
        for other in task.depends_on.iter().chain(&task.parent_task_id) {
            let project = query_opt(
                conn,
                "SELECT project_id FROM tasks WHERE id = ?1",
                params![other.get()],
                |row| Cols(row).id::<ProjectId>(0),
            )
            .await?;
            if project != Some(task.project_id) {
                return Err(StoreError::Invalid(format!(
                    "{other} is not a task of project {}",
                    task.project_id
                )));
            }
        }
        let lane_id: LaneId = query_one(
            conn,
            concat!(
                "INSERT INTO lanes (id, project_id, kind) VALUES (",
                crate::db::next_id_sql!("lanes"),
                ", ?1, ?2) RETURNING id"
            ),
            params![task.project_id.get(), LaneKind::Task.as_str()],
            |row| Cols(row).id(0),
        )
        .await?;
        let id: TaskId = query_one(
            conn,
            concat!(
                "INSERT INTO tasks (id, project_id, lane_id, agent_id, title, goal, repositories,
                 parent_task_id, origin, status, priority, created_by_call, created_at, space_lead_event)
             VALUES (",
                crate::db::next_id_sql!("tasks"),
                ", ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) RETURNING id"
            ),
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
                opt_raw(ask, |id| id.get()),
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
        let created = existing_task(conn, id).await?;
        let ended_badly = query_opt(
            conn,
            "SELECT t.id, t.status FROM task_dependencies d JOIN tasks t ON t.id = d.depends_on
             WHERE d.task_id = ?1 AND t.status IN ('failed', 'cancelled') ORDER BY t.id LIMIT 1",
            params![id.get()],
            |row| {
                let c = Cols(row);
                Ok((c.id::<TaskId>(0)?, c.parse::<TaskStatus>(1)?))
            },
        )
        .await?;
        if let Some((dependency, status)) = ended_badly {
            // It could never start: cancel it as if it had been waiting.
            let reason = dependency_ended(dependency, status);
            end_task(
                conn,
                &created,
                TaskStatus::Cancelled,
                Some(&reason),
                task.created_at,
            )
            .await?;
        } else if created.status == TaskStatus::Queued {
            insert_start_event(conn, &created).await?;
        }
        let created = existing_task(conn, id).await?;
        tx.commit().await?;
        Ok(created)
    }
}

#[async_trait]
impl TaskStore for HubDb {
    async fn create_task(&self, lease: Option<&Lease>, task: &NewTask) -> StoreResult<Task> {
        self.create_task_for_ask(lease, task, None).await
    }

    async fn left_unwoken(&self, lane_id: LaneId) -> StoreResult<bool> {
        let conn = self.db.reader()?;
        left_unwoken(&conn, lane_id).await
    }

    async fn task(&self, id: TaskId) -> StoreResult<Option<Task>> {
        let conn = self.db.reader()?;
        task_in(&conn, "id = ?1", id.get().into()).await
    }

    async fn task_completed_at(&self, id: TaskId) -> StoreResult<Option<Timestamp>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            "SELECT received_at FROM events WHERE source = ?1 AND source_event_id = ?2",
            params![TASK_SOURCE, format!("result:{}", id.get())],
            |row| Cols(row).ts(0),
        )
        .await
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

    async fn update_task(
        &self,
        lease: &Lease,
        id: TaskId,
        title: Option<&str>,
        goal: Option<&str>,
        priority: Option<i64>,
    ) -> StoreResult<Task> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let task = existing_task(conn, id).await?;
        check_run_lease(conn, Some(lease), task.project_id).await?;
        if task.status.is_terminal() {
            return Err(StoreError::Conflict(format!(
                "task {id} is {}; it can no longer change",
                task.status
            )));
        }
        exec(
            conn,
            "UPDATE tasks SET title = ?1, goal = ?2, priority = ?3 WHERE id = ?4",
            params![
                title.unwrap_or(&task.title),
                goal.unwrap_or(&task.goal),
                priority.unwrap_or(task.priority),
                id.get()
            ],
        )
        .await?;
        let updated = existing_task(conn, id).await?;
        tx.commit().await?;
        Ok(updated)
    }

    async fn cancel_task(
        &self,
        lease: Option<&Lease>,
        id: TaskId,
        now: Timestamp,
    ) -> StoreResult<Task> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let task = existing_task(conn, id).await?;
        check_run_lease(conn, lease, task.project_id).await?;
        if task.status == TaskStatus::Cancelled {
            return Ok(task);
        }
        end_task(conn, &task, TaskStatus::Cancelled, None, now).await?;
        let cancelled = existing_task(conn, id).await?;
        tx.commit().await?;
        Ok(cancelled)
    }

    async fn finish_task(&self, lease: &Lease, result: &Json) -> StoreResult<Task> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        let task = task_of_lane(conn, lease.lane_id)
            .await?
            .ok_or_else(|| StoreError::Invalid(format!("lane {} has no task", lease.lane_id)))?;
        if task.status != TaskStatus::Running {
            return Err(StoreError::Conflict(format!(
                "task {} is {}, not running",
                task.id, task.status
            )));
        }
        let progress = progress_path(task.project_id, task.id);
        if live_in(conn, &progress).await?.is_none() {
            return Err(StoreError::Invalid(format!(
                "{progress} does not exist: write the task's progress file, what was done and \
                 what is left, before finishing it"
            )));
        }
        exec(
            conn,
            "UPDATE tasks SET result = ?1 WHERE id = ?2",
            params![json_text(result)?, task.id.get()],
        )
        .await?;
        let finished = existing_task(conn, task.id).await?;
        tx.commit().await?;
        Ok(finished)
    }

    async fn count_merge_refusal(&self, lease: &Lease, limit: i64) -> StoreResult<bool> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        let task = task_of_lane(conn, lease.lane_id)
            .await?
            .ok_or_else(|| StoreError::Invalid(format!("lane {} has no task", lease.lane_id)))?;
        let counted = query_one(
            conn,
            "SELECT merge_refusals FROM tasks WHERE id = ?1",
            params![task.id.get()],
            |row| Cols(row).i64(0),
        )
        .await?;
        if counted >= limit {
            return Ok(false);
        }
        exec(
            conn,
            "UPDATE tasks SET merge_refusals = merge_refusals + 1 WHERE id = ?1",
            params![task.id.get()],
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn space_lead_ask(&self, id: TaskId) -> StoreResult<Option<EventId>> {
        let conn = self.db.reader()?;
        let ask: Option<Option<EventId>> = query_opt(
            &conn,
            "SELECT space_lead_event FROM tasks WHERE id = ?1",
            params![id.get()],
            |row| Cols(row).opt_id(0),
        )
        .await?;
        Ok(ask.flatten())
    }

    async fn set_task_status(
        &self,
        id: TaskId,
        status: TaskStatus,
        waiting_reason: Option<&str>,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let task = existing_task(conn, id).await?;
        if status.is_terminal() {
            end_task(conn, &task, status, None, self.now()).await?;
        } else {
            crate::org::require_active_project(conn, task.project_id).await?;
            transition(conn, &task, status).await?;
        }
        exec(
            conn,
            "UPDATE tasks SET waiting_reason = ?1 WHERE id = ?2",
            params![waiting_reason, id.get()],
        )
        .await?;
        if task.status == TaskStatus::Proposed && status == TaskStatus::Queued {
            insert_start_event(conn, &task).await?;
        }
        tx.commit().await
    }
}
