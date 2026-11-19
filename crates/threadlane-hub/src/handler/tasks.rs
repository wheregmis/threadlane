//! The Tasks capability (Plan.md §5.2): a task you create directly,
//! skipping the lead, cancelling a task, and retrying a failed one. Both go through the same
//! store calls as the lead's `create_task` and `cancel_task` tools.

use threadlane_core::client::{ApiError, TaskReply, TaskRequest};
use threadlane_core::model::{Mode, NewTask, TaskOrigin, TaskStatus};
use threadlane_core::runtime::{WakeTarget, Waker as _};
use threadlane_core::store::{OrgStore, TaskStore};

use super::{ClientApi, active_project, project_error, required};
use crate::error::api_error;

fn review_roles<'a>(
    engineer: &'a str,
    reviewer: &'a str,
) -> Result<threadlane_store::ReviewRoles<'a>, ApiError> {
    let engineer = required("engineer", engineer)?;
    let reviewer = required("reviewer", reviewer)?;
    if engineer == reviewer
        || [engineer, reviewer]
            .iter()
            .any(|s| s.len() > 128 || s.contains(['\n', '\r']))
    {
        return Err(ApiError::Invalid(
            "Choose two distinct, single-line specialist names.".into(),
        ));
    }
    Ok(threadlane_store::ReviewRoles { engineer, reviewer })
}

/// Answers a [`TaskRequest`].
pub(super) async fn handle(api: &ClientApi, request: TaskRequest) -> Result<TaskReply, ApiError> {
    let inner = &api.core.inner;
    let db = &inner.db;
    match request {
        TaskRequest::SaveReviewRecipe { recipe } => {
            for step in [&recipe.implementation, &recipe.review] {
                required("step instructions", step)?;
                if step.len() > 8192 {
                    return Err(ApiError::Invalid("Step instructions exceed 8 KiB.".into()));
                }
            }
            Ok(TaskReply::ReviewRecipe {
                recipe: db
                    .save_review_recipe(&recipe)
                    .await
                    .map_err(project_error)?,
            })
        }
        TaskRequest::StartReviewWorkflow {
            project,
            goal,
            engineer,
            reviewer,
            repository,
            version,
            client_request_id,
        } => {
            let project = api.project_named(Some(&project)).await?;
            active_project(&project)?;
            if db.effective_ceiling(project.id).await.map_err(api_error)? == Mode::Off {
                return Err(ApiError::Invalid("Project mode is off.".into()));
            }
            let roles = review_roles(&engineer, &reviewer)?;
            let goal = required("workflow goal", &goal)?;
            let request = required("request ID", &client_request_id)?;
            if request.len() > 128 || goal.len() > 8192 {
                return Err(ApiError::Invalid("Workflow request is too long.".into()));
            }
            let repo = db
                .repositories(project.id)
                .await
                .map_err(api_error)?
                .into_iter()
                .find(|r| r.name == repository.trim())
                .ok_or_else(|| {
                    ApiError::Invalid("Choose a repository registered in this project.".into())
                })?;
            let recipe = db.review_recipe().await.map_err(api_error)?;
            if recipe.version != version {
                return Err(ApiError::Invalid(
                    "Reload the saved workflow before starting it.".into(),
                ));
            }
            let task = db
                .start_review_workflow(project.id, roles, repo.id, goal, &recipe, request)
                .await
                .map_err(project_error)?;
            inner.wake.wake(WakeTarget::Lane(task.lane_id));
            Ok(TaskReply::Task { task })
        }
        TaskRequest::InstallReviewTeam {
            project,
            engineer,
            reviewer,
            recurrence,
        } => {
            use threadlane_core::model::{NewTimer, TimerCategory, TimerCreator};
            let project = api.project_named(Some(&project)).await?;
            active_project(&project)?;
            let roles = review_roles(&engineer, &reviewer)?;
            crate::recurrence::check(&recurrence).map_err(ApiError::Invalid)?;
            let now = inner.clock.now();
            let templates = crate::templates::all().map_err(ApiError::Internal)?;
            let template = |id| {
                templates
                    .iter()
                    .find(|t| t.id == id)
                    .ok_or_else(|| ApiError::Internal(format!("Missing {id} template")))
            };
            let timer = NewTimer {
                project_id: project.id,
                lane_id: project.lead_lane_id,
                agent_id: None,
                task_ref: None,
                category: TimerCategory::Schedule,
                due_at: crate::recurrence::next_occurrence(&recurrence, now, now)
                    .map_err(ApiError::Invalid)?,
                recurrence: Some(recurrence),
                instruction: threadlane_core::work::review_routine(roles.engineer, roles.reviewer),
                created_by: TimerCreator::User,
                created_by_call: Some(format!("review-team:{}", project.id)),
                requested: true,
            };
            let timer = db
                .install_review_team(
                    &project,
                    [
                        (roles.engineer, template("engineer")?),
                        (roles.reviewer, template("reviewer")?),
                    ],
                    &timer,
                )
                .await
                .map_err(project_error)?;
            Ok(TaskReply::ReviewTeam { timer })
        }
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
            let agent = api.agent_named(&agent).await?;
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
            if binding.mode.stricter(ceiling) == Mode::Off {
                return Err(ApiError::Invalid(format!(
                    "{} is off in project {:?}",
                    agent.name, project.name
                )));
            }
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
        other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
    }
}
