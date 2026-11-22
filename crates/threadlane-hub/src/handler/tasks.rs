//! The Tasks capability (Plan.md §5.2): a task you create directly,
//! skipping the lead, cancelling a task, and retrying a failed one. Both go through the same
//! store calls as the lead's `create_task` and `cancel_task` tools.

use threadlane_core::client::{ApiError, TaskReply, TaskRequest};
use threadlane_core::model::{Agent, Mode, NewTask, Project, TaskOrigin, TaskStatus};
use threadlane_core::runtime::{WakeTarget, Waker as _};
use threadlane_core::store::{OrgStore, TaskStore, TaskUpdate};

use super::{ClientApi, active_project, project_error, required};
use crate::error::api_error;

/// Answers a [`TaskRequest`].
pub(super) async fn handle(api: &ClientApi, request: TaskRequest) -> Result<TaskReply, ApiError> {
    let inner = &api.core.inner;
    let db = &inner.db;
    match request {
        TaskRequest::Create {
            project,
            title,
            goal,
            agent,
            repositories,
            depends_on,
            client_request_id,
        } => {
            let title = required("title", &title)?;
            let goal = required("goal", &goal)?;
            let client_request_id = required("client request id", &client_request_id)?;
            let project = api.project_named(project.as_deref()).await?;
            active_project(&project)?;
            let agent = working_agent(api, &project, &agent).await?;
            let registered = db.repositories(project.id).await.map_err(api_error)?;
            let mut repository_ids = Vec::with_capacity(repositories.len());
            for name in &repositories {
                let repository = registered
                    .iter()
                    .find(|repository| repository.name == name.trim())
                    .ok_or_else(|| {
                        ApiError::Invalid(format!(
                            "project {:?} has no repository {name:?}",
                            project.name
                        ))
                    })?;
                if !repository_ids.contains(&repository.id) {
                    repository_ids.push(repository.id);
                }
            }
            for id in &depends_on {
                let other = db
                    .task(*id)
                    .await
                    .map_err(api_error)?
                    .filter(|other| other.project_id == project.id)
                    .ok_or_else(|| {
                        ApiError::Invalid(format!("project {:?} has no task {id}", project.name))
                    })?;
                // It could never be done, so this task could never start.
                if matches!(other.status, TaskStatus::Failed | TaskStatus::Cancelled) {
                    return Err(ApiError::Invalid(format!(
                        "task {id} has ended ({}); it cannot be waited for",
                        other.status
                    )));
                }
            }
            let task = db
                .create_task(
                    None,
                    &NewTask {
                        project_id: project.id,
                        agent_id: agent.id,
                        title: title.to_owned(),
                        goal: goal.to_owned(),
                        repositories: repository_ids,
                        parent_task_id: None,
                        depends_on,
                        origin: TaskOrigin::User,
                        status: TaskStatus::Queued,
                        priority: 0,
                        created_by_call: Some(format!("client:{client_request_id}")),
                        created_at: inner.clock.now(),
                    },
                )
                .await
                .map_err(project_error)?;
            inner.wake.wake(WakeTarget::Lane(task.lane_id));
            Ok(TaskReply::Task { task })
        }
        TaskRequest::Cancel { task: id } => {
            let task = db
                .task(id)
                .await
                .map_err(api_error)?
                .ok_or_else(|| ApiError::NotFound(format!("task {id}")))?;
            let cancelled = db
                .cancel_task(None, task.id, inner.clock.now())
                .await
                .map_err(api_error)?;
            // Its pending events are skipped by the next cycle of its lane,
            // and the lead hears of it in its own lane.
            inner.wake.wake(WakeTarget::Lane(cancelled.lane_id));
            if let Some(project) = db.project(task.project_id).await.map_err(api_error)? {
                inner.wake.wake(WakeTarget::Lane(project.lead_lane_id));
            }
            Ok(TaskReply::Task { task: cancelled })
        }
        TaskRequest::Retry { task: id } => {
            let _removal = inner.project_removal.lock().await;
            let task = db
                .task(id)
                .await
                .map_err(api_error)?
                .ok_or_else(|| ApiError::NotFound(format!("task {id}")))?;
            if task.status != TaskStatus::Failed {
                return Err(ApiError::Invalid(format!(
                    "task {id} is {}; only a failed task can be retried",
                    task.status
                )));
            }
            let retried = db
                .retry_task(task.id, inner.clock.now())
                .await
                .map_err(api_error)?;
            inner.wake.wake(WakeTarget::Lane(retried.lane_id));
            Ok(TaskReply::Task { task: retried })
        }
        TaskRequest::Reassign { task: id, agent } => {
            let task = db
                .task(id)
                .await
                .map_err(api_error)?
                .ok_or_else(|| ApiError::NotFound(format!("task {id}")))?;
            let project = db
                .project(task.project_id)
                .await
                .map_err(api_error)?
                .ok_or_else(|| ApiError::NotFound(format!("task {id}'s project")))?;
            active_project(&project)?;
            let agent = working_agent(api, &project, &agent).await?;
            let reassigned = db.reassign_task(id, agent.id).await.map_err(api_error)?;
            inner.wake.wake(WakeTarget::Lane(reassigned.lane_id));
            Ok(TaskReply::Task { task: reassigned })
        }
        TaskRequest::AttachRepositories {
            task: id,
            repositories,
        } => {
            let task = db
                .task(id)
                .await
                .map_err(api_error)?
                .ok_or_else(|| ApiError::NotFound(format!("task {id}")))?;
            if task.status.is_terminal() {
                return Err(ApiError::Invalid(format!(
                    "task {id} is {}; only a task that has not ended can take a repository",
                    task.status
                )));
            }
            let registered = db.repositories(task.project_id).await.map_err(api_error)?;
            let mut added = Vec::new();
            for name in &repositories {
                let repository = registered
                    .iter()
                    .find(|repository| repository.name == name.trim())
                    .ok_or_else(|| {
                        ApiError::Invalid(format!("task {id}'s project has no repository {name:?}"))
                    })?;
                if !task.repositories.contains(&repository.id) && !added.contains(repository) {
                    added.push(repository.clone());
                }
            }
            if added.is_empty() {
                return Err(ApiError::Invalid(format!(
                    "task {id} already has every repository named"
                )));
            }
            // Attaching queues no run: a sleeping or waiting task stays put
            // until something else wakes it, and its next run is given the
            // worktree and told where its earlier work is.
            let ids: Vec<_> = added.iter().map(|repository| repository.id).collect();
            let updated = db
                .update_task(
                    None,
                    id,
                    TaskUpdate {
                        repositories: &ids,
                        ..TaskUpdate::default()
                    },
                )
                .await
                .map_err(api_error)?;
            Ok(TaskReply::Task { task: updated })
        }
        other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
    }
}

/// The agent named `name`, if a task in `project` can go to it: bound there
/// and not Off.
async fn working_agent(api: &ClientApi, project: &Project, name: &str) -> Result<Agent, ApiError> {
    let db = &api.core.inner.db;
    let agent = api.agent_named(name).await?;
    let binding = db
        .bindings(project.id)
        .await
        .map_err(api_error)?
        .into_iter()
        .find(|binding| binding.agent_id == agent.id)
        .ok_or_else(|| {
            ApiError::Invalid(format!(
                "{} is not bound in project {:?}",
                agent.name, project.name
            ))
        })?;
    let ceiling = db.effective_ceiling(project.id).await.map_err(api_error)?;
    if binding.effective_mode(ceiling) == Mode::Off {
        return Err(ApiError::Invalid(format!(
            "{} is off in project {:?}",
            agent.name, project.name
        )));
    }
    Ok(agent)
}
