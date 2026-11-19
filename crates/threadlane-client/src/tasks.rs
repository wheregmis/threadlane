//! What you start and stop yourself through [`ClientCore`]: a task created
//! skipping the lead, cancelling or retrying a task, pausing a project, and the events
//! recorded but acted on by nobody.

use threadlane_core::client::{
    ApiError, DeletePreview, OrgReply, OrgRequest, OverviewReply, OverviewRequest, ProjectSummary,
    RecordedEvent, TaskReply, TaskRequest,
};
use threadlane_core::ids::TaskId;
use threadlane_core::model::Task;

use crate::{ClientCore, ClientError, SpaceListing, api_error, wrong_reply};

/// A task you create directly, as [`ClientCore::create_task`] takes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTaskSpec {
    /// The project; `None` means the General project.
    pub project: Option<String>,
    pub title: String,
    pub goal: String,
    /// An agent bound in the project and not Off.
    pub agent: String,
    /// Names of repositories registered in the project.
    pub repositories: Vec<String>,
    /// Tasks of the project it waits for.
    pub depends_on: Vec<TaskId>,
    /// Whether the task ends by opening a draft pull request.
    pub draft_pr: bool,
}

/// Added to the goal of a task that ends with a draft pull request, so the
/// work does not stop at an unpublished branch.
pub const DRAFT_PR_GOAL: &str = "When the work is done and checked, open a draft pull \
request for it with open_pull_request before you call finish_task.";

impl ClientCore {
    /// Creates a task of yours in one space, skipping the lead, with a
    /// fresh request id. `space: None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, a draft pull request asked
    /// for without a repository, or the hub's error, for example an agent
    /// that is not bound in the project.
    pub async fn create_task(
        &self,
        space: Option<&str>,
        spec: NewTaskSpec,
    ) -> Result<Task, ClientError> {
        if spec.draft_pr && spec.repositories.is_empty() {
            let (name, _api) = self.target(space)?;
            return Err(api_error(
                &name,
                ApiError::Invalid(
                    "A draft pull request needs a repository: choose one for the task.".to_owned(),
                ),
            ));
        }
        let request = TaskRequest::Create {
            project: spec.project,
            title: spec.title,
            goal: if spec.draft_pr {
                format!("{}\n\n{DRAFT_PR_GOAL}", spec.goal)
            } else {
                spec.goal
            },
            agent: spec.agent,
            repositories: spec.repositories,
            depends_on: spec.depends_on,
            client_request_id: uuid::Uuid::new_v4().to_string(),
        };
        self.one_task(space, request).await
    }

    /// Submit an owner action for the saved review workflow.
    pub async fn workflow(
        &self,
        space: &str,
        request: TaskRequest,
    ) -> Result<TaskReply, ClientError> {
        let (space, api) = self.target(Some(space))?;
        api.tasks(request).await.map_err(|e| api_error(&space, e))
    }

    /// Fresh idempotency key for one user-initiated action.
    pub fn request_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Cancels task `task` (`T-0142`) of one space, and what waits on it.
    /// `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, an id that is not a
    /// task's, or the hub's error.
    pub async fn cancel_task(&self, space: Option<&str>, task: &str) -> Result<Task, ClientError> {
        let id = self.task_id(space, task)?;
        self.one_task(space, TaskRequest::Cancel { task: id }).await
    }

    /// Queues failed task `task` (`T-0142`) of one space again, telling its
    /// agent the last error. `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, an id that is not a
    /// task's, or the hub's error, for example a task that has not failed.
    pub async fn retry_task(&self, space: Option<&str>, task: &str) -> Result<Task, ClientError> {
        let id = self.task_id(space, task)?;
        self.one_task(space, TaskRequest::Retry { task: id }).await
    }

    fn task_id(&self, space: Option<&str>, task: &str) -> Result<TaskId, ClientError> {
        let (name, _api) = self.target(space)?;
        TaskId::parse(task).ok_or_else(|| {
            api_error(
                &name,
                ApiError::Invalid(format!("{task:?} is not a task id like T-0142")),
            )
        })
    }

    async fn one_task(
        &self,
        space: Option<&str>,
        request: TaskRequest,
    ) -> Result<Task, ClientError> {
        let (space, api) = self.target(space)?;
        match api.tasks(request).await.map_err(|e| api_error(&space, e))? {
            TaskReply::Task { task } => Ok(task),
            _ => Err(wrong_reply(&space, "task")),
        }
    }

    /// Pauses a project, or resumes it with `paused: false`. `space: None`
    /// means the only paired hub; `project: None` means the General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn set_paused(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        paused: bool,
    ) -> Result<ProjectSummary, ClientError> {
        let request = OrgRequest::SetPaused {
            project: project.map(str::to_owned),
            paused,
        };
        match self.org(space, request).await? {
            (_, OrgReply::Project { project }) => Ok(project),
            (space, _) => Err(wrong_reply(&space, "pause")),
        }
    }

    /// Archives or restores a project in the selected space.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn set_archived(
        &self,
        space: Option<&str>,
        project: &str,
        archived: bool,
    ) -> Result<ProjectSummary, ClientError> {
        let request = OrgRequest::SetArchived {
            project: Some(project.to_owned()),
            archived,
        };
        match self.org(space, request).await? {
            (_, OrgReply::Project { project }) => Ok(project),
            (space, _) => Err(wrong_reply(&space, "archive")),
        }
    }

    /// Reads the permanent deletion inventory for a project.
    ///
    /// # Errors
    ///
    /// No such hub or project, or the hub's refusal.
    pub async fn delete_preview(
        &self,
        space: Option<&str>,
        project: &str,
    ) -> Result<DeletePreview, ClientError> {
        let request = OrgRequest::DeletePreview {
            project: project.to_owned(),
        };
        match self.org(space, request).await? {
            (_, OrgReply::DeletePreview { preview }) => Ok(preview),
            (space, _) => Err(wrong_reply(&space, "deletion preview")),
        }
    }

    /// Permanently deletes the project using its exact confirmed name.
    ///
    /// # Errors
    ///
    /// No such hub or project, or the hub's refusal.
    pub async fn delete_project(
        &self,
        space: Option<&str>,
        project: &str,
        confirm_name: &str,
    ) -> Result<(DeletePreview, Vec<String>), ClientError> {
        let request = OrgRequest::DeleteProject {
            project: project.to_owned(),
            confirm_name: confirm_name.to_owned(),
        };
        match self.org(space, request).await? {
            (
                _,
                OrgReply::ProjectDeleted {
                    deleted, leftovers, ..
                },
            ) => Ok((deleted, leftovers)),
            (space, _) => Err(wrong_reply(&space, "delete project")),
        }
    }

    /// The newest `limit` events of a project recorded and acted on by
    /// nobody, newest first, and how many it has in all. `space: None`
    /// means the only paired hub; `project: None` means the General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn recorded(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        limit: u32,
    ) -> Result<(Vec<RecordedEvent>, u64), ClientError> {
        let request = OverviewRequest::Recorded {
            project: project.map(str::to_owned),
            limit,
        };
        match self.overview(space, request).await? {
            (_, OverviewReply::Recorded { events, total }) => Ok((events, total)),
            (space, _) => Err(wrong_reply(&space, "recorded")),
        }
    }

    /// How many events `space`, or every paired hub when `None`, recorded
    /// and did not act on.
    ///
    /// # Errors
    ///
    /// No such hub, or no hub paired.
    pub async fn recorded_count(
        &self,
        space: Option<&str>,
    ) -> Result<Vec<SpaceListing<u64>>, ClientError> {
        self.overview_each(
            space,
            &OverviewRequest::RecordedCount,
            "recorded count",
            |_, reply| match reply {
                OverviewReply::RecordedCount { total } => Some(total),
                _ => None,
            },
        )
        .await
    }
}
