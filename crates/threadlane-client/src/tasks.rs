//! What you start and stop yourself through [`ClientCore`]: a task created
//! skipping the lead, cancelling a task, pausing a project, and the events
//! recorded but acted on by nobody.

use threadlane_core::client::{
    ApiError, OrgReply, OrgRequest, OverviewReply, OverviewRequest, ProjectSummary, RecordedEvent,
    TaskReply, TaskRequest,
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
}

impl ClientCore {
    /// Creates a task of yours in one space, skipping the lead, with a
    /// fresh request id. `space: None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error, for
    /// example an agent that is not bound in the project.
    pub async fn create_task(
        &self,
        space: Option<&str>,
        spec: NewTaskSpec,
    ) -> Result<Task, ClientError> {
        let request = TaskRequest::Create {
            project: spec.project,
            title: spec.title,
            goal: spec.goal,
            agent: spec.agent,
            repositories: spec.repositories,
            depends_on: spec.depends_on,
            client_request_id: uuid::Uuid::new_v4().to_string(),
        };
        self.one_task(space, request).await
    }

    /// Cancels task `task` (`T-0142`) of one space, and what waits on it.
    /// `None` means the only paired hub.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, an id that is not a
    /// task's, or the hub's error.
    pub async fn cancel_task(&self, space: Option<&str>, task: &str) -> Result<Task, ClientError> {
        let (name, _api) = self.target(space)?;
        let id = TaskId::parse(task).ok_or_else(|| {
            api_error(
                &name,
                ApiError::Invalid(format!("{task:?} is not a task id like T-0142")),
            )
        })?;
        self.one_task(space, TaskRequest::Cancel { task: id }).await
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
