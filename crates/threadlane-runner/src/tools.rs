//! The tool endpoint a run's backend calls: forwards to the hub under the lease.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::forge::{
    EXPECTED_HEAD, FAST_FORWARD_PULL_REQUEST, OPEN_PULL_REQUEST, PUBLISH_BRANCH,
};
use threadlane_core::ids::{RunId, TaskId};
use threadlane_core::link::{
    DIFF_PREVIEW, FINISH_TASK, HubLink, MERGE_CHECKS, PROGRESS_WRITTEN, PUBLISH_REVISION,
};
use threadlane_core::model::Repository;
use threadlane_core::tools::{ToolDefinition, ToolEndpoint, ToolError};
use threadlane_core::workspace::Workspaces;
use tracing::warn;

use crate::lane::{LaneState, Stop};

/// Serves `RunAssignment::tools` to one run by forwarding to
/// [`HubLink::call_tool`].
///
/// The backend only sees a failed call as a tool error, so a kill or a lost
/// lease is also raised on the lane, which stops on it, and a call that
/// opened a decision marks the run deferred, which ends it. A result that
/// says the task's progress file was written marks that on the lane.
///
/// A task's `finish_task` carries where its branch stands against each
/// repository's default branch, checked here first (docs/merge-help.md
/// decision 2); the hub decides whether to refuse it.
#[derive(Debug)]
pub(crate) struct LaneTools {
    link: Arc<dyn HubLink>,
    lane: Arc<LaneState>,
    run_id: RunId,
    definitions: Vec<ToolDefinition>,
    workspaces: Arc<dyn Workspaces>,
    /// The task and its repositories; `None` for a lead.
    task: Option<(TaskId, Vec<Repository>)>,
}

impl LaneTools {
    pub(crate) fn new(
        link: Arc<dyn HubLink>,
        lane: Arc<LaneState>,
        run_id: RunId,
        definitions: Vec<ToolDefinition>,
        workspaces: Arc<dyn Workspaces>,
        task: Option<(TaskId, Vec<Repository>)>,
    ) -> Self {
        Self {
            link,
            lane,
            run_id,
            definitions,
            workspaces,
            task,
        }
    }

    /// `finish_task`'s arguments with the merge checks set: the work is
    /// committed first, so the check sees it. A check that fails leaves
    /// them out, and the task finishes unchecked rather than not at all.
    async fn with_merge_checks(&self, mut arguments: Json) -> Json {
        if let Some(object) = arguments.as_object_mut() {
            object.remove(MERGE_CHECKS);
        }
        let Some((task, repositories)) = &self.task else {
            return arguments;
        };
        if repositories.is_empty() {
            return arguments;
        }
        let message = format!("Threadlane: work of {task}, run {}", self.run_id);
        let checked = match self
            .workspaces
            .checkpoint(*task, repositories, &message)
            .await
        {
            Ok(()) => self.workspaces.check_merge(*task, repositories).await,
            Err(error) => Err(error),
        };
        match checked {
            Ok(checks) => {
                if let Some(object) = arguments.as_object_mut() {
                    object.insert(MERGE_CHECKS.to_owned(), serde_json::json!(checks));
                }
            }
            Err(error) => warn!(%task, %error, "could not check whether the task branch merges"),
        }
        arguments
    }

    /// `publish_branch` / `open_pull_request` arguments with a runner-computed
    /// revision and diff so the approval card can show what will be pushed.
    async fn with_publish_preview(
        &self,
        name: &str,
        mut arguments: Json,
    ) -> Result<Json, ToolError> {
        if let Some(object) = arguments.as_object_mut() {
            object.remove(DIFF_PREVIEW);
            object.remove(PUBLISH_REVISION);
        }
        let Some((task, repositories)) = &self.task else {
            return Ok(arguments);
        };
        let Some(repo_name) = arguments.get("repository").and_then(Json::as_str) else {
            return Ok(arguments);
        };
        let Some(repository) = repositories.iter().find(|repo| repo.name == repo_name) else {
            return Ok(arguments);
        };
        let expected_head = if name == FAST_FORWARD_PULL_REQUEST {
            let head = arguments[EXPECTED_HEAD]
                .as_str()
                .filter(|head| threadlane_core::workspace::valid_commit_id(head))
                .ok_or_else(|| {
                    ToolError::InvalidArguments("expected_head must be a full commit SHA".into())
                })?;
            Some(head)
        } else {
            None
        };
        let message = format!("Threadlane: work of {task}, run {}", self.run_id);
        self.workspaces
            .checkpoint(*task, std::slice::from_ref(repository), &message)
            .await
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        let preview = self
            .workspaces
            .preview_publish(*task, repository, expected_head)
            .await
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        arguments[PUBLISH_REVISION] = serde_json::json!(preview.revision);
        if let Some(diff) = preview.diff {
            arguments[DIFF_PREVIEW] = serde_json::json!(diff);
        }
        Ok(arguments)
    }
}

#[async_trait]
impl ToolEndpoint for LaneTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }

    async fn call(
        &self,
        call_id: Option<&str>,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        // A dead process or a lost lease makes no further hub calls.
        match self.lane.current() {
            Some(Stop::Killed(killed)) => return Err(ToolError::Killed(killed)),
            Some(Stop::LeaseLost) => return Err(ToolError::LeaseLost),
            Some(Stop::User) => return Err(ToolError::Blocked("stopped by the user".to_owned())),
            Some(Stop::Shutdown) | None => {}
        }
        // Nothing more happens in a run that waits on you (Plan.md §9.4).
        if let Some(error) = self.lane.deferred() {
            return Err(error);
        }
        let arguments = if name == FINISH_TASK {
            self.with_merge_checks(arguments).await
        } else if matches!(
            name,
            PUBLISH_BRANCH | OPEN_PULL_REQUEST | FAST_FORWARD_PULL_REQUEST
        ) {
            self.with_publish_preview(name, arguments).await?
        } else {
            arguments
        };
        let lease = self.lane.lease();
        let result = self
            .link
            .call_tool(&lease, self.run_id, call_id, name, &arguments)
            .await;
        match &result {
            Ok(output) if output["submitted_for_review"] == true => {
                *self
                    .lane
                    .submitted
                    .lock()
                    .expect("submission lock poisoned") =
                    Some(serde_json::from_value(output[MERGE_CHECKS].clone()).unwrap_or_default());
            }
            Err(ToolError::Killed(killed)) => self.lane.signal(Stop::Killed(*killed)),
            Err(ToolError::LeaseLost) => self.lane.signal(Stop::LeaseLost),
            Err(error @ (ToolError::WaitingOnYou | ToolError::AuthorizedAndQueued)) => {
                self.lane.set_deferred(Some(error.clone()));
            }
            Ok(output) if output.get(PROGRESS_WRITTEN) == Some(&Json::Bool(true)) => {
                self.lane.set_progress_written(true);
            }
            _ => {}
        }
        result
    }
}
