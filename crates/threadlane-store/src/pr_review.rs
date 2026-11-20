//! Reuse a review of the same pull request revision across scan and implementation tasks.

use threadlane_core::github::normalize_remote;
use threadlane_core::ids::{EventId, ProjectId, RepositoryId, TaskId};
use threadlane_core::model::{Lease, NewTask, Repository, Task};
use threadlane_core::store::{StoreError, StoreResult};
use threadlane_core::workspace::valid_commit_id;
use turso::Connection;

use crate::HubDb;
use crate::db::{exec, query_opt};
use crate::row::{Cols, params};
use crate::task::{check_run_lease, insert_task_in, task_by_call};

/// A registered repository and immutable PR revision.
#[derive(Debug)]
pub struct PrReviewHead {
    pub repository: RepositoryId,
    pub url: String,
    pub head: String,
}

impl PrReviewHead {
    pub fn new(repo: &Repository, number: u64, head: &str) -> StoreResult<Self> {
        if number == 0 || !valid_commit_id(head) {
            return Err(StoreError::Invalid(
                "PR review requires a positive PR number and full head SHA".into(),
            ));
        }
        let remote = normalize_remote(&repo.remote_url)
            .ok_or_else(|| StoreError::Invalid("PR review requires a remote repository".into()))?;
        Ok(Self {
            repository: repo.id,
            url: format!("https://{remote}/pull/{number}"),
            head: head.to_owned(),
        })
    }
}

#[derive(Debug)]
pub struct PrReviewStatus {
    pub task: TaskId,
    pub state: String,
    pub feedback: Option<String>,
}

#[derive(Debug)]
pub enum PrReviewAssignment {
    Existing(PrReviewStatus),
    Created(Box<Task>),
}

async fn status(
    conn: &Connection,
    project: ProjectId,
    target: &PrReviewHead,
) -> StoreResult<Option<PrReviewStatus>> {
    let decode = |row: &turso::Row| {
        let c = Cols(row);
        Ok(PrReviewStatus {
            task: c.id(0)?,
            state: c.text(1)?,
            feedback: c.opt_text(2)?,
        })
    };
    // Read the original round, including history whose GitHub delivery was acknowledged.
    let original = query_opt(conn, "SELECT t.id, CASE WHEN r.state IN ('submitted','pending') THEN 'in_progress' ELSE r.state END,
            CASE WHEN r.state IN ('approved','changes_requested') THEN r.feedback END
        FROM tasks t JOIN task_reviews r ON r.task_id = t.id
        JOIN runs run ON run.id = r.submission_run
        JOIN json_each(r.revisions) revision
        JOIN repositories repo ON repo.id = revision.value ->> '$.repository'
        WHERE t.project_id = ?1 AND repo.id = ?2 AND revision.value ->> '$.commit' = ?3
          AND (r.state IN ('pending','approved','changes_requested') OR (r.state = 'submitted' AND run.status = 'active'))
          AND EXISTS (SELECT 1 FROM decisions d WHERE d.lane_id = t.lane_id
                AND d.tool IN ('open_pull_request','publish_branch','fast_forward_pull_request')
                AND d.outcome ->> '$.performed' = 1 AND d.outcome ->> '$.repository' = repo.name
                AND lower(d.outcome ->> '$.pull_request') = ?4)
        ORDER BY r.submission_run DESC LIMIT 1",
        params![project.get(),target.repository.get(),target.head.as_str(),target.url.as_str()], decode).await?;
    if let Some(original) = original {
        return Ok(Some(original));
    }
    query_opt(conn, "SELECT t.id, CASE WHEN t.status = 'done' THEN t.result ->> '$.pr_verdict' ELSE 'in_progress' END,
            CASE WHEN t.status = 'done' THEN t.result ->> '$.summary' END
        FROM pr_review_tasks p JOIN tasks t ON t.id = p.task_id
        WHERE p.pull_url = ?1 AND p.head = ?2 AND t.project_id = ?3 AND t.status NOT IN ('cancelled','failed')
          AND (t.status != 'done' OR t.result ->> '$.pr_verdict' IN ('approved','changes_requested'))
        ORDER BY t.id DESC LIMIT 1", params![target.url.as_str(),target.head.as_str(),project.get()], decode).await
}

impl HubDb {
    pub async fn pr_review_status(
        &self,
        project: ProjectId,
        target: &PrReviewHead,
    ) -> StoreResult<Option<PrReviewStatus>> {
        status(&self.db.reader()?, project, target).await
    }

    /// Check and assign under the same writer lock, including after approval or retry.
    pub async fn assign_pr_review(
        &self,
        lease: &Lease,
        task: &NewTask,
        ask: Option<EventId>,
        target: &PrReviewHead,
    ) -> StoreResult<PrReviewAssignment> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_run_lease(conn, Some(lease), task.project_id).await?;
        if !task.repositories.contains(&target.repository) {
            return Err(StoreError::Invalid(
                "review repository must be attached to the task".into(),
            ));
        }
        let assignment = if let Some(found) = status(conn, task.project_id, target).await? {
            PrReviewAssignment::Existing(found)
        } else {
            if let Some(key) = &task.created_by_call
                && task_by_call(conn, key).await?.is_some()
            {
                return Err(StoreError::Conflict("this task call already assigned other or ended work; use a new call for this PR head".into()));
            }
            let created = insert_task_in(conn, Some(lease), task, ask).await?;
            exec(
                conn,
                "INSERT INTO pr_review_tasks(task_id,pull_url,head) VALUES (?1,?2,?3)",
                params![created.id.get(), target.url.as_str(), target.head.as_str()],
            )
            .await?;
            PrReviewAssignment::Created(Box::new(created))
        };
        tx.commit().await?;
        Ok(assignment)
    }

    pub async fn assigned_pr_head(&self, task: TaskId) -> StoreResult<Option<String>> {
        query_opt(
            &self.db.reader()?,
            "SELECT head FROM pr_review_tasks WHERE task_id = ?1",
            params![task.get()],
            |row| Cols(row).text(0),
        )
        .await
    }
}
