//! The tool endpoint a run's backend calls: forwards to the hub under the lease.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::ids::{RunId, TaskId};
use threadlane_core::link::{FINISH_TASK, HubLink, MERGE_CHECKS, PROGRESS_WRITTEN};
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
        if self.lane.deferred() {
            return Err(ToolError::WaitingOnYou);
        }
        let arguments = if name == FINISH_TASK {
            self.with_merge_checks(arguments).await
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
            Err(ToolError::WaitingOnYou) => self.lane.set_deferred(true),
            Ok(output) if output.get(PROGRESS_WRITTEN) == Some(&Json::Bool(true)) => {
                self.lane.set_progress_written(true);
            }
            _ => {}
        }
        result
    }
}
