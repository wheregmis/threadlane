//! The tool gateway: gate, at-most-once record, execution (Plan.md §4.6,
//! §9.1, docs/milestone-1.md §9 "Tool gateway").

use serde_json::{Value as Json, json};
use threadlane_core::ids::RunId;
use threadlane_core::model::{Lease, Mode, Run, RunStatus, ToolCallBegin, ToolCallKey, Trust};
use threadlane_core::runtime::steps::tool_call as step;
use threadlane_core::store::{OrgStore, SessionStore};
use threadlane_core::tools::{GateRequest, ToolContext, ToolDefinition, ToolError, Verdict};

use crate::error::{link_to_tool, tool_error};
use crate::link::LocalLink;
use crate::tools::derived_call_key;

/// Key of the envelope a failed call's error is recorded in, so a repeated
/// call returns the same error instead of "outcome unknown".
const ERROR_RECORD: &str = "threadlane_tool_error";

impl LocalLink {
    pub(crate) async fn gateway(
        &self,
        lease: &Lease,
        run_id: RunId,
        call_id: Option<&str>,
        tool: &str,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let inner = &self.core.inner;
        let db = &inner.db;
        let lane = self.check_lease(lease).await.map_err(link_to_tool)?;
        let run = db
            .run(run_id)
            .await
            .map_err(tool_error)?
            .filter(|run| {
                run.lane_id == lane.id
                    && run.lease_epoch == lease.epoch
                    && run.status == RunStatus::Active
            })
            .ok_or_else(|| {
                ToolError::Failed(format!("run {run_id} is not active under this lease"))
            })?;
        let definition = inner
            .tools
            .definition(tool)
            .ok_or_else(|| ToolError::UnknownTool(tool.to_owned()))?;
        self.gate(&run, lane.project_id, &definition, arguments)
            .await?;

        let key = ToolCallKey {
            run_id,
            key: call_id.map_or_else(|| derived_call_key(tool, arguments), str::to_owned),
        };
        let begin = db
            .begin_tool_call(
                lease,
                &key,
                tool,
                arguments,
                definition.safe_to_repeat,
                inner.clock.now(),
            )
            .await
            .map_err(tool_error)?;
        match begin {
            ToolCallBegin::Finished(result) => return replay(result),
            ToolCallBegin::OutcomeUnknown => return Err(ToolError::OutcomeUnknown),
            ToolCallBegin::Perform => {}
        }
        inner.hook.reach(step::AFTER_BEGIN)?;

        let context = ToolContext {
            lease: *lease,
            run_id,
            project_id: lane.project_id,
            agent_id: run.agent_id,
            task_id: lane.task_id,
            call_id: call_id.map(str::to_owned),
        };
        let outcome = inner.tools.execute(&context, tool, arguments.clone()).await;
        let record = match &outcome {
            Ok(result) => result.clone(),
            Err(error) => match error_record(error) {
                Some(record) => record,
                // Lease loss, a crash, or an unknown outcome: leave the call
                // started, as a process death would.
                None => return outcome,
            },
        };
        inner.hook.reach(step::AFTER_EFFECT)?;
        db.finish_tool_call(lease, &key, &record, inner.clock.now())
            .await
            .map_err(tool_error)?;
        inner.hook.reach(step::AFTER_FINISH)?;
        outcome
    }

    async fn gate(
        &self,
        run: &Run,
        project_id: threadlane_core::ids::ProjectId,
        definition: &ToolDefinition,
        arguments: &Json,
    ) -> Result<(), ToolError> {
        let inner = &self.core.inner;
        let db = &inner.db;
        let ceiling = db.effective_ceiling(project_id).await.map_err(tool_error)?;
        // No binding for the run's agent means the agent may do nothing.
        let binding_mode = db
            .bindings(project_id)
            .await
            .map_err(tool_error)?
            .into_iter()
            .find(|binding| Some(binding.agent_id) == run.agent_id)
            .map_or(Mode::Off, |binding| binding.mode)
            .stricter(ceiling);
        let tainted = db
            .run_events(run.id)
            .await
            .map_err(tool_error)?
            .iter()
            .any(|event| event.trust == Trust::External);
        let request = GateRequest {
            project_id,
            agent_id: run.agent_id,
            tool: definition.name.clone(),
            tier: definition.tier,
            arguments: arguments.clone(),
            binding_mode,
            tainted,
        };
        match inner.policy.decide(&request).await {
            Verdict::Allow => Ok(()),
            Verdict::Block { reason } => Err(ToolError::Blocked(reason)),
            // Decisions arrive in milestone 3; until then asking is refusing.
            Verdict::Ask => Err(ToolError::Blocked(format!(
                "{} needs the user's approval, which this hub cannot ask for yet",
                definition.name
            ))),
        }
    }
}

/// The record of a call that failed in a way that is safe to answer the same
/// way again. Other errors leave the call unfinished.
fn error_record(error: &ToolError) -> Option<Json> {
    let (kind, message) = match error {
        ToolError::UnknownTool(name) => ("unknown_tool", name),
        ToolError::InvalidArguments(message) => ("invalid_arguments", message),
        ToolError::Blocked(message) => ("blocked", message),
        ToolError::Failed(message) => ("failed", message),
        _ => return None,
    };
    Some(json!({ ERROR_RECORD: { "kind": kind, "message": message } }))
}

/// A recorded result, or the recorded error.
fn replay(result: Json) -> Result<Json, ToolError> {
    let Some(record) = result.get(ERROR_RECORD) else {
        return Ok(result);
    };
    let message = record
        .get("message")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned();
    Err(match record.get("kind").and_then(Json::as_str) {
        Some("unknown_tool") => ToolError::UnknownTool(message),
        Some("invalid_arguments") => ToolError::InvalidArguments(message),
        Some("blocked") => ToolError::Blocked(message),
        _ => ToolError::Failed(message),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_errors_replay_as_the_same_error() {
        for error in [
            ToolError::UnknownTool("x".into()),
            ToolError::InvalidArguments("bad".into()),
            ToolError::Blocked("no".into()),
            ToolError::Failed("boom".into()),
        ] {
            let record = error_record(&error).unwrap();
            assert_eq!(replay(record), Err(error));
        }
        assert!(error_record(&ToolError::LeaseLost).is_none());
        assert_eq!(replay(json!({"outbox_id": 3})), Ok(json!({"outbox_id": 3})));
    }
}
