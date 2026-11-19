//! Review rounds share a task lane, while each verdict belongs to one submission.

use serde_json::{Value as Json, json};
use threadlane_core::client::TaskReviewActivity;
use threadlane_core::ids::{AgentId, MachineId, RunId, TaskId};
use threadlane_core::memory::progress_path;
use threadlane_core::model::{
    EventKind, Lease, NewEvent, Task, TaskReviewVerdict, TaskStatus, Trust,
};
use threadlane_core::store::{StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use threadlane_core::workspace::{ReviewRevision, valid_commit_id};
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::inbox::{check_lease, insert_event_in};
use crate::memory::live_in;
use crate::row::{Cols, json_text, params};
use crate::session::active_run;
use crate::task::{
    TASK_WAKE_SOURCE, complete_result, requeue_after_result, task_of_lane, transition,
    with_published,
};

pub const TASK_REVIEW_SOURCE: &str = "task-review";

#[derive(Debug, Clone)]
pub struct TaskReview {
    pub submission: RunId,
    pub reviewer: AgentId,
    pub machine: MachineId,
    pub revisions: Vec<ReviewRevision>,
    pub implementation_result: Json,
}

fn decode_review(row: &Row) -> StoreResult<TaskReview> {
    let c = Cols(row);
    Ok(TaskReview {
        submission: c.id(0)?,
        reviewer: c.id(1)?,
        machine: c.id(2)?,
        revisions: c.de(3)?,
        implementation_result: c.json(4)?,
    })
}

/// A committed review waiting for delivery to the task's GitHub pull request.
#[derive(Debug)]
pub struct GithubReview {
    pub submission: RunId,
    pub pull_url: String,
    pub delivery_key: String,
    pub revision: String,
    pub body: String,
}

impl HubDb {
    /// Lists outstanding deliveries in submission order.
    pub async fn pending_github_reviews(&self) -> StoreResult<Vec<GithubReview>> {
        query_all(&self.db.reader()?, "SELECT submission, pull_url, revision, body, delivery_key FROM github_reviews ORDER BY submission", (), |row| {
            let c = Cols(row);
            Ok(GithubReview { submission: c.id(0)?, pull_url: c.text(1)?, revision: c.text(2)?, body: c.text(3)?, delivery_key: c.text(4)? })
        }).await
    }

    /// Acknowledges one successful delivery.
    pub async fn delivered_github_review(&self, review: &GithubReview) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "DELETE FROM github_reviews WHERE submission = ?1 AND pull_url = ?2",
            params![review.submission.get(), review.pull_url.as_str()],
        )
        .await?;
        tx.commit().await
    }

    /// Review rounds shown in the task's timeline.
    pub async fn task_reviews(&self, task: TaskId) -> StoreResult<Vec<TaskReviewActivity>> {
        let conn = self.db.reader()?;
        query_all(&conn, "SELECT r.submission_run, r.reviewer, agents.name, r.revisions, submitted.ended_at,
            CASE WHEN r.state IN ('approved', 'changes_requested') THEN r.verdict END,
            CASE WHEN r.state IN ('approved', 'changes_requested') THEN r.feedback END,
            CASE WHEN r.state IN ('approved', 'changes_requested') THEN decided.ended_at END
            FROM task_reviews r JOIN agents ON agents.id = r.reviewer
            JOIN runs submitted ON submitted.id = r.submission_run LEFT JOIN runs decided ON decided.id = r.verdict_run
            WHERE r.task_id = ?1 AND r.state != 'submitted' AND submitted.ended_at IS NOT NULL ORDER BY r.submission_run",
            params![task.get()], |row| {
                let c = Cols(row);
                Ok(TaskReviewActivity {
                    submission: c.id(0)?, reviewer: c.id(1)?, reviewer_name: c.text(2)?, revisions: c.de(3)?,
                    submitted_at: c.ts(4)?, verdict: c.opt_parse(5)?,
                    feedback: c.opt_text(6)?, decided_at: c.opt_ts(7)?,
                })
            }).await
    }
    /// Saves a submission until its implementation run commits.
    pub async fn submit_for_review(
        &self,
        lease: &Lease,
        run_id: RunId,
        reviewer: AgentId,
        revisions: &[ReviewRevision],
        result: &Json,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        let run = active_run(conn, lease, run_id).await?;
        let task = task_of_lane(conn, lease.lane_id)
            .await?
            .ok_or_else(|| StoreError::Invalid("only a task can submit for review".into()))?;
        if task.status != TaskStatus::Running || run.agent_id != Some(task.agent_id) {
            return Err(StoreError::Conflict(
                "only the running task assignee can submit work for review".into(),
            ));
        }
        if reviewer == task.agent_id {
            return Err(StoreError::Invalid(
                "the Reviewer must be a different agent".into(),
            ));
        }
        let progress = progress_path(task.project_id, task.id);
        if live_in(conn, &progress).await?.is_none() {
            return Err(StoreError::Invalid(format!(
                "Write the task's progress file {progress} before submitting for review"
            )));
        }
        if !result.is_object()
            || revisions.len() != task.repositories.len()
            || revisions
                .iter()
                .zip(&task.repositories)
                .any(|(rev, repo)| rev.repository != *repo || !valid_commit_id(&rev.commit))
        {
            return Err(StoreError::Invalid(
                "review requires a submitted commit for each task repository".into(),
            ));
        }
        let revisions = json_text(revisions)?;
        let result = json_text(result)?;
        exec(conn, "INSERT INTO task_reviews (submission_run, task_id, reviewer, revisions, implementation_result)
            VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(submission_run) DO NOTHING",
            params![run_id.get(), task.id.get(), reviewer.get(), revisions.as_str(), result.as_str()]).await?;
        let original = query_one(conn, "SELECT reviewer, revisions, implementation_result FROM task_reviews WHERE submission_run = ?1",
            params![run_id.get()], |row| {
                let c = Cols(row);
                Ok((c.id::<AgentId>(0)?, c.text(1)?, c.text(2)?))
            }).await?;
        if original != (reviewer, revisions, result) {
            return Err(StoreError::Conflict(
                "this run already submitted a different result for review".into(),
            ));
        }
        tx.commit().await
    }

    /// The round currently waiting for its reviewer to finish.
    pub async fn pending_task_review(&self, task: TaskId) -> StoreResult<Option<TaskReview>> {
        let conn = self.db.reader()?;
        pending_in(&conn, task).await
    }

    /// Whether this run recorded a verdict for the current submission.
    pub async fn has_review_verdict(&self, run: RunId) -> StoreResult<bool> {
        let conn = self.db.reader()?;
        Ok(query_opt(
            &conn,
            "SELECT 1 FROM task_reviews WHERE verdict_run = ?1 AND state = 'pending'",
            params![run.get()],
            |row| Cols(row).i64(0),
        )
        .await?
        .is_some())
    }

    /// Attaches a verdict to the current Reviewer run. Only a committed run can resolve it.
    pub async fn store_task_review_verdict(
        &self,
        lease: &Lease,
        run_id: RunId,
        verdict: TaskReviewVerdict,
        feedback: &str,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        let run = active_run(conn, lease, run_id).await?;
        let task = task_of_lane(conn, lease.lane_id)
            .await?
            .ok_or_else(|| StoreError::Invalid("a review must belong to a task lane".into()))?;
        let reviewer = query_opt(
            conn,
            "SELECT reviewer FROM task_reviews WHERE task_id = ?1 AND state = 'pending'",
            params![task.id.get()],
            |row| Cols(row).id::<AgentId>(0),
        )
        .await?;
        if task.status != TaskStatus::InReview || reviewer.is_none() || reviewer != run.agent_id {
            return Err(StoreError::Conflict(
                "only the assigned Reviewer can resolve this submission".into(),
            ));
        }
        let feedback = feedback.trim();
        if feedback.is_empty() {
            return Err(StoreError::Invalid("review feedback is required".into()));
        }
        exec(
            conn,
            "UPDATE task_reviews SET verdict_run = ?2, verdict = ?3, feedback = ?4
            WHERE task_id = ?1 AND state = 'pending'",
            params![task.id.get(), run_id.get(), verdict.as_str(), feedback],
        )
        .await?;
        tx.commit().await
    }
}

pub(crate) async fn pending_in(conn: &Connection, task: TaskId) -> StoreResult<Option<TaskReview>> {
    query_opt(
        conn,
        "SELECT submission_run, reviewer, runs.machine_id, revisions, implementation_result
        FROM task_reviews JOIN runs ON runs.id = submission_run
        WHERE task_id = ?1 AND state = 'pending'",
        params![task.get()],
        decode_review,
    )
    .await
}

pub(crate) async fn submitted(conn: &Connection, task: TaskId, run: RunId) -> StoreResult<bool> {
    Ok(query_opt(conn, "SELECT 1 FROM task_reviews WHERE task_id = ?1 AND submission_run = ?2 AND state = 'submitted'",
        params![task.get(), run.get()], |row| Cols(row).i64(0)).await?.is_some())
}

pub(crate) async fn start_review(
    conn: &Connection,
    task: &Task,
    run: RunId,
    now: Timestamp,
) -> StoreResult<()> {
    exec(
        conn,
        "UPDATE task_reviews SET state = 'pending' WHERE submission_run = ?1",
        params![run.get()],
    )
    .await?;
    transition(conn, task, TaskStatus::InReview).await?;
    review_event(
        conn,
        task,
        format!("request:{}", run.get()),
        json!({"submission": run}),
        now,
    )
    .await
}

pub(crate) async fn finish_review(
    conn: &Connection,
    task: &Task,
    run: RunId,
    now: Timestamp,
) -> StoreResult<()> {
    let recorded = query_opt(
        conn,
        "SELECT submission_run, reviewer, verdict, feedback, implementation_result
        FROM task_reviews WHERE task_id = ?1 AND state = 'pending' AND verdict_run = ?2",
        params![task.id.get(), run.get()],
        |row| {
            let c = Cols(row);
            Ok((
                c.id::<RunId>(0)?,
                c.id::<AgentId>(1)?,
                c.parse::<TaskReviewVerdict>(2)?,
                c.text(3)?,
                c.json(4)?,
            ))
        },
    )
    .await?;
    let Some((submission, reviewer, verdict, feedback, mut result)) = recorded else {
        exec(conn, "UPDATE tasks SET waiting_reason = 'The Reviewer ended without submitting a verdict' WHERE id = ?1", params![task.id.get()]).await?;
        return Ok(());
    };
    exec(
        conn,
        "UPDATE task_reviews SET state = ?2 WHERE submission_run = ?1",
        params![submission.get(), verdict.as_str()],
    )
    .await?;
    let targets = query_all(
        conn,
        "SELECT DISTINCT json_extract(d.outcome, '$.pull_request'), repos.remote_url,
            revision.value ->> '$.commit', agents.name
        FROM task_reviews r JOIN agents ON agents.id = r.reviewer
        JOIN json_each(r.revisions) revision
        JOIN repositories repos ON repos.id = json_extract(revision.value, '$.repository')
        JOIN decisions d ON d.lane_id = ?2
            AND d.tool IN ('open_pull_request', 'publish_branch', 'fast_forward_pull_request')
        WHERE r.submission_run = ?1 AND json_extract(d.outcome, '$.performed') = 1
        AND json_extract(d.outcome, '$.repository') = repos.name
        AND json_type(d.outcome, '$.pull_request') = 'text'",
        params![submission.get(), task.lane_id.get()],
        |row| {
            let c = Cols(row);
            Ok((c.text(0)?, c.text(1)?, c.text(2)?, c.text(3)?))
        },
    )
    .await?;
    for (url, remote, commit, name) in targets {
        let Some(number) = threadlane_core::github::pull_request_number(&url) else {
            continue;
        };
        let Some(repo) = threadlane_core::github::normalize_remote(&remote) else {
            continue;
        };
        if !url.eq_ignore_ascii_case(&format!("https://{repo}/pull/{number}")) {
            continue;
        }
        let decision = match verdict {
            TaskReviewVerdict::Approved => "Approved",
            TaskReviewVerdict::ChangesRequested => "Changes requested",
        };
        let body = format!(
            "**{decision}**\n\nReviewed at `{}`.\n\n{feedback}\n\n🤖 Reviewed by Threadlane ({name}, {})",
            commit.get(..8).unwrap_or(&commit),
            task.id
        );
        exec(conn, "INSERT INTO github_reviews (submission, pull_url, revision, body) VALUES (?1, ?2, ?3, ?4)",
            params![submission.get(), url.as_str(), commit.as_str(), body.as_str()]).await?;
    }
    let more_work = query_opt(
        conn,
        "SELECT 1 FROM events WHERE lane_id = ?1 AND status = 'pending'
        AND ((kind = 'user_message' AND trust = 'user') OR source = ?2) LIMIT 1",
        params![task.lane_id.get(), TASK_WAKE_SOURCE],
        |row| Cols(row).i64(0),
    )
    .await?
    .is_some();
    if verdict == TaskReviewVerdict::Approved && !more_work {
        result["review"] = json!({"reviewer": reviewer, "feedback": feedback, "submission": submission, "run": run});
        let mut reviewed = task.clone();
        reviewed.result = Some(result);
        reviewed.result = with_published(conn, &reviewed).await?;
        exec(
            conn,
            "UPDATE tasks SET result = ?2, waiting_reason = NULL WHERE id = ?1",
            params![task.id.get(), json_text(&reviewed.result)?],
        )
        .await?;
        return complete_result(conn, &reviewed, run, now).await;
    }
    requeue_after_result(conn, task).await?;
    review_event(
        conn,
        task,
        format!("feedback:{}", submission.get()),
        json!({"verdict": verdict, "feedback": feedback}),
        now,
    )
    .await
}

async fn review_event(
    conn: &Connection,
    task: &Task,
    source_event_id: String,
    payload: Json,
    now: Timestamp,
) -> StoreResult<()> {
    // The space's configured review is owner-requested even for autonomous work.
    let requested = payload.get("submission").is_some() || task.origin.is_requested();
    insert_event_in(
        conn,
        &NewEvent {
            project_id: task.project_id,
            lane_id: task.lane_id,
            source: TASK_REVIEW_SOURCE.to_owned(),
            source_event_id,
            kind: EventKind::System,
            payload,
            trust: Trust::System,
            requested,
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: now,
        },
    )
    .await?;
    Ok(())
}
