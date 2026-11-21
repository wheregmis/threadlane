//! [`TaskStore`] on the hub database (Plan.md §5.2), and the task side of the
//! run cycle's transactions.

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::backend::{free_backends, usable_backends};
use threadlane_core::ids::{
    AgentId, DecisionId, EventId, LaneId, MachineId, ProjectId, RunId, TaskId,
};
use threadlane_core::memory::progress_path;
use threadlane_core::model::{
    BackendKind, EventKind, LaneKind, Lease, NewEvent, NewTask, Run, Task, TaskStatus, Trust,
};
use threadlane_core::store::{StoreError, StoreResult, TaskStore, TaskUpdate};
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
pub(crate) const TASK_WAKE_SOURCE: &str = "task-wake";

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

/// One DB-backed page of agent-visible tasks, including counts for empty pages.
#[derive(Debug)]
pub struct TaskListPage {
    /// The selected page, in task ID order.
    pub tasks: Vec<Task>,
    /// All tasks visible for this listing before pagination.
    pub total: usize,
    /// Older terminal tasks excluded from the default listing.
    pub hidden_finished: usize,
}

const TERMINAL: &str = "'done', 'failed', 'cancelled'";

const TASK_FIELDS: &str = "id, project_id, lane_id, agent_id, title, goal, repositories,
    parent_task_id, origin, status, priority, created_by_call, last_machine_id, waiting_reason,
    result, created_at, publication_blocker";

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
        publication_blocker: c.opt_text(16)?,
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

pub(crate) async fn task_by_call(conn: &Connection, call: &str) -> StoreResult<Option<Task>> {
    task_in(conn, "created_by_call = ?1", call.into()).await
}

pub(crate) async fn existing_task(conn: &Connection, id: TaskId) -> StoreResult<Task> {
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
pub(crate) async fn check_run_lease(
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

/// Queues another implementation run and clears the previous result.
pub(crate) async fn requeue_after_result(conn: &Connection, task: &Task) -> StoreResult<()> {
    transition(conn, task, TaskStatus::Queued).await?;
    exec(
        conn,
        "UPDATE tasks SET result = NULL, waiting_reason = NULL WHERE id = ?1",
        params![task.id.get()],
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
    tell_lead(conn, task, result_key(conn, task.id).await?, payload, now).await?;
    // Fixing the agent's setup and retrying can still finish a task that
    // failed on configuration, so what waits for it stays blocked.
    if matches!(status, TaskStatus::Failed | TaskStatus::Cancelled)
        && !(status == TaskStatus::Failed && configuration_result(task))
    {
        cancel_dependents(conn, task, status, now).await?;
    }
    Ok(())
}

/// Honor owner approval on review tasks created by older hubs.
pub(crate) async fn complete_result(
    conn: &Connection,
    task: &Task,
    run: RunId,
    now: Timestamp,
) -> StoreResult<()> {
    if let Some(result) = &task.result
        && task.created_by_call.as_deref().is_some_and(|key| {
            key.starts_with("workflow-review:") || key.starts_with("canvas-review:")
        })
        && crate::work::needs_owner_decision(conn, task, result, run, now).await?
    {
        wait_on_decision(conn, task).await?;
        return Ok(());
    }
    end_task(conn, task, TaskStatus::Done, None, now).await
}

/// How many times the user retried task `id`.
async fn retries(conn: &Connection, id: TaskId) -> StoreResult<i64> {
    query_one(
        conn,
        "SELECT COUNT(*) FROM events WHERE source = ?1 AND source_event_id LIKE ?2",
        params![TASK_SOURCE, format!("retry:{}:%", id.get())],
        |row| Cols(row).i64(0),
    )
    .await
}

/// The `source_event_id` of the lead's `task_result` event for the current
/// attempt of task `id`, a new one after each retry.
async fn result_key(conn: &Connection, id: TaskId) -> StoreResult<String> {
    Ok(match retries(conn, id).await? {
        0 => format!("result:{}", id.get()),
        n => format!("result:{}:retry{n}", id.get()),
    })
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

/// A lead's retry: its run's lease, the agent it names, if another, and
/// the tool call's key, so that repeating the call retries once.
struct LeadRetry<'a> {
    lease: &'a Lease,
    agent: Option<AgentId>,
    call_key: &'a str,
}

/// Queues failed task `id` again: the user's retry when `lead` is `None`,
/// else the lead's, which only a configuration failure allows, on the agent
/// it names or the same one. The lead's retry of a task in review whose
/// Reviewer failed on its configuration queues that review round again.
async fn retry_in(
    conn: &Connection,
    id: TaskId,
    lead: Option<LeadRetry<'_>>,
    now: Timestamp,
) -> StoreResult<Task> {
    let task = existing_task(conn, id).await?;
    if let Some(lead) = &lead {
        check_run_lease(conn, Some(lead.lease), task.project_id).await?;
        let earlier: Vec<Json> = query_all(
            conn,
            "SELECT payload FROM events WHERE source = ?1 AND source_event_id LIKE ?2",
            params![TASK_SOURCE, format!("retry:{}:%", id.get())],
            |row| Cols(row).json(0),
        )
        .await?;
        if earlier
            .iter()
            .any(|payload| payload["call_key"] == lead.call_key)
        {
            return Ok(task);
        }
    }
    if let Some(lead) = &lead
        && task.status == TaskStatus::InReview
    {
        return task_review::retry_review(conn, &task, lead.agent, lead.call_key, now).await;
    }
    if task.status != TaskStatus::Failed {
        return Err(StoreError::Conflict(format!(
            "task {id} is {}; only a failed task, or a review that failed on its Reviewer's \
             configuration, can be retried",
            task.status
        )));
    }
    if lead.is_some() && !is_configuration_failure(&task) {
        return Err(StoreError::Conflict(format!(
            "task {id} did not fail on its agent's configuration; only the user can retry it"
        )));
    }
    // The error is kept in the lead's report of how the attempt ended.
    let error: Option<Json> = query_opt(
        conn,
        "SELECT payload FROM events WHERE source = ?1 AND source_event_id = ?2",
        params![TASK_SOURCE, result_key(conn, id).await?],
        |row| Cols(row).json(0),
    )
    .await?;
    let error = error
        .as_ref()
        .and_then(|payload| payload["error"].as_str())
        .unwrap_or("unknown");
    // The one way out of `failed`, which the lifecycle otherwise ends.
    exec(
        conn,
        "UPDATE tasks SET status = ?1, waiting_reason = NULL, result = NULL WHERE id = ?2",
        params![TaskStatus::Queued.as_str(), id.get()],
    )
    .await?;
    // The user asked for it now: the failed attempts' backoff does not hold it.
    exec(
        conn,
        "UPDATE lanes SET backoff_until = NULL WHERE id = ?1",
        params![task.lane_id.get()],
    )
    .await?;
    if let Some(agent) = lead.as_ref().and_then(|lead| lead.agent) {
        exec(
            conn,
            "UPDATE tasks SET agent_id = ?1 WHERE id = ?2",
            params![agent.get(), id.get()],
        )
        .await?;
    }
    let (kind, trust, text) = if lead.is_some() {
        (
            EventKind::System,
            Trust::System,
            format!(
                "The project lead retried {id}. The last attempt failed with: {error}\n\n\
                 Any earlier work is committed on the task branch. Continue from it."
            ),
        )
    } else {
        (
            EventKind::UserMessage,
            Trust::User,
            format!(
                "The user asked for a retry of {id}. The last attempt failed with: {error}\n\n\
                 Your earlier work is committed on the task branch. Continue from it; \
                 do not start over."
            ),
        )
    };
    let attempt = retries(conn, id).await? + 1;
    insert_event_in(
        conn,
        &NewEvent {
            project_id: task.project_id,
            lane_id: task.lane_id,
            source: TASK_SOURCE.to_owned(),
            source_event_id: format!("retry:{}:{attempt}", id.get()),
            kind,
            payload: json!({ "text": text, "call_key": lead.as_ref().map(|lead| lead.call_key) }),
            trust,
            requested: true,
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: now,
        },
    )
    .await?;
    existing_task(conn, id).await
}

/// Whether `task` failed because its backend refused the agent's setup.
pub(crate) fn is_configuration_failure(task: &Task) -> bool {
    task.status == TaskStatus::Failed && configuration_result(task)
}

/// Whether `task`'s result records a configuration failure.
fn configuration_result(task: &Task) -> bool {
    task.result
        .as_ref()
        .is_some_and(|result| result["configuration_failure"] == true)
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
/// waits on you; one that recorded its result finishes unless a lead wake
/// is pending; otherwise it sleeps until an event
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
        let wake = query_opt(
            conn,
            "SELECT 1 FROM events WHERE lane_id = ?1 AND status = 'pending' AND source = ?2 LIMIT 1",
            params![lane_id.get(), TASK_WAKE_SOURCE],
            |row| Cols(row).i64(0),
        )
        .await?;
        if wake.is_some() {
            return requeue_after_result(conn, &task).await;
        }
        let result = with_published(conn, &task).await?;
        exec(
            conn,
            "UPDATE tasks SET result = ?1 WHERE id = ?2",
            params![json_text(&result)?, task.id.get()],
        )
        .await?;
        let task = Task { result, ..task };
        return complete_result(conn, &task, run_id, now).await;
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
/// `configuration` is the run, when its backend refused the agent's setup:
/// a task that dies of it records a configuration failure as its result.
pub(crate) async fn on_run_failed(
    conn: &Connection,
    lane_id: LaneId,
    died: bool,
    error: &str,
    configuration: Option<&Run>,
    now: Timestamp,
) -> StoreResult<()> {
    let Some(task) = task_of_lane(conn, lane_id).await? else {
        return Ok(());
    };
    if task.status == TaskStatus::InReview && died {
        // The result stays the implementer's; a refused setup is kept on the
        // round, where the lead's retry_task finds it, and named in the reason.
        let reason = match configuration {
            Some(run) => {
                let failure = json!({
                    "configuration_failure": true,
                    "error": error,
                    "agent_id": run.agent_id,
                    "backend": run.backend,
                    "model": run.model,
                });
                exec(
                    conn,
                    "UPDATE task_reviews SET configuration_failure = ?2
                     WHERE task_id = ?1 AND state = 'pending'",
                    params![task.id.get(), json_text(&failure)?],
                )
                .await?;
                format!(
                    "Reviewer failed on its configuration (agent {}, backend {}, model {}): \
                     {error}. Once the setup is fixed, the lead can retry the review with \
                     retry_task, optionally on another Reviewer",
                    run.agent_id.map_or("none".to_owned(), |id| id.to_string()),
                    run.backend.map_or("none", |backend| backend.as_str()),
                    run.model.as_deref().unwrap_or("default"),
                )
            }
            None => format!("Reviewer failed: {error}"),
        };
        exec(
            conn,
            "UPDATE tasks SET waiting_reason = ?2 WHERE id = ?1",
            params![task.id.get(), reason],
        )
        .await?;
        return Ok(());
    }
    if task.status != TaskStatus::Running {
        return Ok(());
    }
    if died {
        let task = match configuration {
            Some(run) => {
                let result = json!({
                    "configuration_failure": true,
                    "error": error,
                    "agent_id": run.agent_id,
                    "backend": run.backend,
                    "model": run.model,
                });
                exec(
                    conn,
                    "UPDATE tasks SET result = ?1 WHERE id = ?2",
                    params![json_text(&result)?, task.id.get()],
                )
                .await?;
                existing_task(conn, task.id).await?
            }
            None => task,
        };
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
        let mut blocked = Vec::new();
        for id in &waiting_for {
            if is_configuration_failure(&existing_task(conn, *id).await?) {
                blocked.push(id.to_string());
            }
        }
        if !blocked.is_empty() {
            return Ok(Some(format!(
                "blocked: {} failed because the backend refused its agent's setup; \
                 retry it after fixing the agent, or reassign it to another agent",
                blocked.join(", ")
            )));
        }
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
) -> StoreResult<(bool, bool)> {
    let task = task_of_lane(conn, lane_id).await?;
    let reason = match &task {
        Some(task) if task.status.is_terminal() => None,
        Some(task) if task.status == TaskStatus::InReview => {
            match task_review::pending_in(conn, task.id).await? {
                Some(review) if !review.revisions.is_empty() && review.machine != machine_id => {
                    return Ok((false, false));
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
    let changed = if let Some(task) = &task
        && task.waiting_reason != reason
    {
        exec(
            conn,
            "UPDATE tasks SET waiting_reason = ?1 WHERE lane_id = ?2",
            params![reason.as_deref(), lane_id.get()],
        )
        .await?
            > 0
    } else {
        false
    };
    Ok((reason.is_none(), changed))
}

impl HubDb {
    /// Rechecks review eligibility if the Engineer handed over under an existing lease.
    pub async fn review_can_start(&self, lease: &Lease, now: Timestamp) -> StoreResult<bool> {
        let tx = self.db.begin().await?;
        check_lease(tx.conn(), lease).await?;
        let (allowed, changed) =
            check_eligible(tx.conn(), lease.lane_id, lease.machine_id, now).await?;
        if changed {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(allowed)
    }

    /// Creates a task and records the space Lead's ask in the same transaction.
    pub async fn create_task_for_ask(
        &self,
        lease: Option<&Lease>,
        task: &NewTask,
        ask: Option<EventId>,
    ) -> StoreResult<Task> {
        let tx = self.db.begin().await?;
        let created = insert_task_in(tx.conn(), lease, task, ask).await?;
        tx.commit().await?;
        Ok(created)
    }
}

pub(crate) async fn insert_task_in(
    conn: &Connection,
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
    check_run_lease(conn, lease, task.project_id).await?;
    if let Some(call) = &task.created_by_call
        && let Some(existing) = task_by_call(conn, call).await?
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
    let ended: Vec<TaskId> = query_all(
        conn,
        "SELECT t.id FROM task_dependencies d JOIN tasks t ON t.id = d.depends_on
             WHERE d.task_id = ?1 AND t.status IN ('failed', 'cancelled') ORDER BY t.id",
        params![id.get()],
        |row| Cols(row).id(0),
    )
    .await?;
    let mut ended_badly = None;
    for dependency in ended {
        // One that failed on configuration can still be retried: it waits, blocked.
        let dependency = existing_task(conn, dependency).await?;
        if !is_configuration_failure(&dependency) {
            ended_badly = Some((dependency.id, dependency.status));
            break;
        }
    }
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
    existing_task(conn, id).await
}

impl HubDb {
    /// The lead retries task `id`, which failed on its agent's
    /// configuration, on `agent` or, with `None`, on the same agent; for a
    /// task in review whose Reviewer failed so, `agent` is the Reviewer.
    /// The run's `lease` must be current in the task's project; repeating
    /// the call `call_key` retries once and returns the task as it is.
    pub async fn lead_retry_task(
        &self,
        lease: &Lease,
        id: TaskId,
        agent: Option<AgentId>,
        call_key: &str,
        now: Timestamp,
    ) -> StoreResult<Task> {
        let tx = self.db.begin().await?;
        let lead = LeadRetry {
            lease,
            agent,
            call_key,
        };
        let retried = retry_in(tx.conn(), id, Some(lead), now).await?;
        tx.commit().await?;
        Ok(retried)
    }

    /// Tasks in `project` whose backend refused their agent's setup and that
    /// nobody has retried yet: failed ones, and ones in review whose
    /// Reviewer's was refused, listed with that failure as their result;
    /// newest first. Scoped to one project because only that project's lead
    /// can retry them.
    pub async fn configuration_failures(&self, project: ProjectId) -> StoreResult<Vec<Task>> {
        let conn = self.db.reader()?;
        let reviews: Vec<(TaskId, Json)> = query_all(
            &conn,
            "SELECT t.id, r.configuration_failure FROM tasks t
             JOIN task_reviews r ON r.task_id = t.id AND r.state = 'pending'
             WHERE t.project_id = ?1 AND t.status = 'in_review'
             AND r.configuration_failure IS NOT NULL",
            params![project.get()],
            |row| Ok((Cols(row).id(0)?, Cols(row).json(1)?)),
        )
        .await?;
        let mut tasks = Vec::new();
        for (id, failure) in reviews {
            let task = existing_task(&conn, id).await?;
            tasks.push(Task {
                result: Some(failure),
                ..task
            });
        }
        // shortcut: matches the compact JSON this module writes; a JSON query if results ever get rewritten.
        let ids: Vec<TaskId> = query_all(
            &conn,
            "SELECT id FROM tasks WHERE project_id = ?1 AND status = 'failed'
             AND result LIKE '%\"configuration_failure\":true%' ORDER BY id DESC",
            params![project.get()],
            |row| Cols(row).id(0),
        )
        .await?;
        for id in ids {
            let task = existing_task(&conn, id).await?;
            if is_configuration_failure(&task) {
                tasks.push(task);
            }
        }
        tasks.sort_by_key(|task| std::cmp::Reverse(task.id));
        Ok(tasks)
    }

    /// Lists tasks for the supplied project membership, even if a project is archived concurrently.
    pub async fn tasks_for_projects(&self, projects: &[ProjectId]) -> StoreResult<Vec<Task>> {
        let ids = json_text(&json!(
            projects
                .iter()
                .map(|project| project.get())
                .collect::<Vec<_>>()
        ))?;
        self.tasks_with_dependencies(
            "project_id IN (SELECT value FROM json_each(?1))",
            params![ids],
        )
        .await
    }

    /// Running and in-review candidates across all projects.
    pub async fn running_tasks(&self) -> StoreResult<Vec<Task>> {
        self.tasks_with_dependencies("status IN ('running', 'in_review')", ())
            .await
    }

    /// Open (non-terminal) tasks across all projects.
    pub async fn open_tasks(&self) -> StoreResult<Vec<Task>> {
        self.tasks_with_dependencies("status NOT IN ('done', 'failed', 'cancelled')", ())
            .await
    }

    pub(crate) async fn tasks_with_dependencies(
        &self,
        filter: &str,
        values: impl turso::IntoParams + Send,
    ) -> StoreResult<Vec<Task>> {
        let conn = self.db.reader()?;
        let mut rows = conn
            .query(
                &format!(
                    "WITH selected AS (SELECT {TASK_FIELDS} FROM tasks WHERE {filter})
                      SELECT selected.*, d.depends_on
                      FROM selected LEFT JOIN task_dependencies d ON d.task_id = selected.id
                      ORDER BY selected.id, d.depends_on"
                ),
                values,
            )
            .await
            .map_err(crate::db::db_err)?;
        let mut tasks: Vec<Task> = Vec::new();
        while let Some(row) = rows.next().await.map_err(crate::db::db_err)? {
            let cols = Cols(&row);
            let id: TaskId = cols.id(0)?;
            let dependency = cols.opt_id(17)?;
            if let Some(task) = tasks.last_mut().filter(|task| task.id == id) {
                task.depends_on.extend(dependency);
            } else {
                tasks.push(decode_task(&row, dependency.into_iter().collect())?);
            }
        }
        Ok(tasks)
    }

    /// Reads one project page and its counts in a single database snapshot.
    /// With `recent_finished`, include only that many recent terminal tasks;
    /// an attempt with no result event sorts by task creation time instead.
    pub async fn task_list_page(
        &self,
        project: ProjectId,
        recent_finished: Option<usize>,
        offset: usize,
        limit: usize,
    ) -> StoreResult<TaskListPage> {
        let as_sql = |n| i64::try_from(n).unwrap_or(i64::MAX);
        let mut values = params![project.get(), as_sql(limit), as_sql(offset)];
        let (ctes, visible) = if let Some(recent) = recent_finished {
            values.extend(params![as_sql(recent), TASK_SOURCE]);
            // Match task_completion_times' current-attempt event lookup. The
            // recent cohort ranks by ending time; the page is still shown by ID.
            (
                format!(
                    "WITH terminal AS (
                    SELECT id, created_at FROM tasks
                    WHERE project_id = ?1 AND status IN ({TERMINAL})
                 ), attempts AS (
                    SELECT id, created_at, (
                        SELECT COUNT(*) FROM events WHERE source = ?5
                        AND source_event_id >= 'retry:' || terminal.id || ':'
                        AND source_event_id < 'retry:' || terminal.id || ';'
                    ) AS retries FROM terminal
                 ), recent AS (
                    SELECT attempts.id FROM attempts LEFT JOIN events
                    ON source = ?5 AND source_event_id = 'result:' || attempts.id ||
                       CASE WHEN retries = 0 THEN '' ELSE ':retry' || retries END
                    ORDER BY COALESCE(received_at, attempts.created_at) DESC, attempts.id DESC
                    LIMIT ?4
                 ), counts AS (
                    SELECT MAX((SELECT COUNT(*) FROM terminal) - ?4, 0) AS hidden,
                           COUNT(*) AS total FROM tasks WHERE project_id = ?1
                 )"
                ),
                format!("AND (status NOT IN ({TERMINAL}) OR id IN (SELECT id FROM recent))"),
            )
        } else {
            (
                "WITH counts AS (
                SELECT 0 AS hidden, COUNT(*) AS total FROM tasks WHERE project_id = ?1
            )"
                .to_owned(),
                String::new(),
            )
        };
        let conn = self.db.reader()?;
        let mut rows = conn
            .query(
                &format!(
                    "{ctes}, page_ids AS (
                    SELECT id FROM tasks WHERE project_id = ?1 {visible}
                    ORDER BY id LIMIT ?2 OFFSET ?3
                ), page AS (
                    SELECT {TASK_FIELDS} FROM tasks WHERE id IN (SELECT id FROM page_ids)
                )
                SELECT page.*, d.depends_on, counts.total - counts.hidden, counts.hidden
                FROM counts LEFT JOIN page ON TRUE
                LEFT JOIN task_dependencies d ON d.task_id = page.id
                ORDER BY page.id, d.depends_on"
                ),
                values,
            )
            .await
            .map_err(crate::db::db_err)?;
        let mut result = TaskListPage {
            tasks: Vec::new(),
            total: 0,
            hidden_finished: 0,
        };
        while let Some(row) = rows.next().await.map_err(crate::db::db_err)? {
            let cols = Cols(&row);
            result.total = usize::try_from(cols.i64(18)?).unwrap_or(usize::MAX);
            result.hidden_finished = usize::try_from(cols.i64(19)?).unwrap_or(usize::MAX);
            let Some(id): Option<TaskId> = cols.opt_id(0)? else {
                break;
            };
            let dependency = cols.opt_id(17)?;
            if let Some(task) = result.tasks.last_mut().filter(|task| task.id == id) {
                task.depends_on.extend(dependency);
            } else {
                result
                    .tasks
                    .push(decode_task(&row, dependency.into_iter().collect())?);
            }
        }
        Ok(result)
    }

    /// Completion time of the current attempt for each supplied task.
    pub async fn task_completion_times(
        &self,
        tasks: &[TaskId],
    ) -> StoreResult<HashMap<TaskId, Timestamp>> {
        if tasks.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.db.reader()?;
        let ids = json_text(&json!(
            tasks.iter().map(|task| task.get()).collect::<Vec<_>>()
        ))?;
        // The retry key's ':' delimiter sorts immediately before ';', bounding the indexed prefix.
        let times = query_all(
            &conn,
            "WITH attempts AS (
                SELECT ids.value AS task_id, (
                    SELECT COUNT(*) FROM events
                    WHERE source = ?1
                    AND source_event_id >= 'retry:' || ids.value || ':'
                    AND source_event_id <  'retry:' || ids.value || ';'
                ) AS retries
                FROM json_each(?2) ids
             )
             SELECT task_id, received_at FROM attempts JOIN events
             ON source = ?1 AND source_event_id = 'result:' || task_id ||
                CASE WHEN retries = 0 THEN '' ELSE ':retry' || retries END",
            params![TASK_SOURCE, ids],
            |row| Ok((Cols(row).id(0)?, Cols(row).ts(1)?)),
        )
        .await?;
        Ok(times.into_iter().collect())
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
        Ok(self.task_completion_times(&[id]).await?.remove(&id))
    }

    async fn tasks(&self, project_id: ProjectId) -> StoreResult<Vec<Task>> {
        self.tasks_with_dependencies("project_id = ?1", params![project_id.get()])
            .await
    }

    async fn update_task(
        &self,
        lease: Option<&Lease>,
        id: TaskId,
        update: TaskUpdate<'_>,
    ) -> StoreResult<Task> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let mut task = existing_task(conn, id).await?;
        check_run_lease(conn, lease, task.project_id).await?;
        if task.status.is_terminal() {
            return Err(StoreError::Conflict(format!(
                "task {id} is {}; it can no longer change",
                task.status
            )));
        }
        for repository in update.repositories {
            if task.repositories.contains(repository) {
                continue;
            }
            let registered = query_opt(
                conn,
                "SELECT 1 FROM repositories WHERE project_id = ?1 AND id = ?2",
                params![task.project_id.get(), repository.get()],
                |row| Cols(row).i64(0),
            )
            .await?;
            if registered.is_none() {
                return Err(StoreError::Invalid(format!(
                    "repository {repository} is not registered in this task's project"
                )));
            }
            task.repositories.push(*repository);
        }
        exec(
            conn,
            "UPDATE tasks SET title = ?1, goal = ?2, priority = ?3, repositories = ?4,
                publication_blocker = CASE WHEN ?4 = '[]' THEN publication_blocker END
             WHERE id = ?5",
            params![
                update.title.unwrap_or(&task.title),
                update.goal.unwrap_or(&task.goal),
                update.priority.unwrap_or(task.priority),
                json_text(&task.repositories)?,
                id.get()
            ],
        )
        .await?;
        if let Some(wake) = update.wake {
            insert_event_in(
                conn,
                &NewEvent {
                    project_id: task.project_id,
                    lane_id: task.lane_id,
                    source: TASK_WAKE_SOURCE.to_owned(),
                    source_event_id: format!("{id}:{}", wake.call_key),
                    kind: EventKind::System,
                    payload: json!({"text": wake.instruction}),
                    trust: Trust::System,
                    requested: task.origin.is_requested() || wake.requested,
                    session_ref: None,
                    decision_ref: None,
                    reply_to_ref: None,
                    received_at: wake.at,
                },
            )
            .await?;
        }
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

    async fn retry_task(&self, id: TaskId, now: Timestamp) -> StoreResult<Task> {
        let tx = self.db.begin().await?;
        let retried = retry_in(tx.conn(), id, None, now).await?;
        tx.commit().await?;
        Ok(retried)
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

    async fn block_publication(&self, id: TaskId, reason: &str) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "UPDATE tasks SET publication_blocker = ?1 WHERE id = ?2 AND repositories = '[]'",
            params![reason, id.get()],
        )
        .await?;
        tx.commit().await
    }
}
