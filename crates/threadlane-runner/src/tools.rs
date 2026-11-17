//! The tool endpoint a run's backend calls: forwards to the hub under the lease.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::ids::RunId;
use threadlane_core::link::HubLink;
use threadlane_core::tools::{ToolDefinition, ToolEndpoint, ToolError};

use crate::lane::{LaneState, Stop};

/// Serves `RunAssignment::tools` to one run by forwarding to
/// [`HubLink::call_tool`].
///
/// The backend only sees a failed call as a tool error, so a kill or a lost
/// lease is also raised on the lane, which stops on it, and a call that
/// opened a decision marks the run deferred, which ends it.
#[derive(Debug)]
pub(crate) struct LaneTools {
    link: Arc<dyn HubLink>,
    lane: Arc<LaneState>,
    run_id: RunId,
    definitions: Vec<ToolDefinition>,
}

impl LaneTools {
    pub(crate) fn new(
        link: Arc<dyn HubLink>,
        lane: Arc<LaneState>,
        run_id: RunId,
        definitions: Vec<ToolDefinition>,
    ) -> Self {
        Self {
            link,
            lane,
            run_id,
            definitions,
        }
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
            Some(Stop::Shutdown) | None => {}
        }
        // Nothing more happens in a run that waits on you (Plan.md §9.4).
        if self.lane.deferred() {
            return Err(ToolError::WaitingOnYou);
        }
        let lease = self.lane.lease();
        let result = self
            .link
            .call_tool(&lease, self.run_id, call_id, name, &arguments)
            .await;
        match &result {
            Err(ToolError::Killed(killed)) => self.lane.signal(Stop::Killed(*killed)),
            Err(ToolError::LeaseLost) => self.lane.signal(Stop::LeaseLost),
            Err(ToolError::WaitingOnYou) => self.lane.set_deferred(true),
            _ => {}
        }
        result
    }
}
