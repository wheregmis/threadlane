//! The tool gateway: gate, at-most-once record, execution (Plan.md §4.6,
//! §9.1, docs/milestone-1.md §9 "Tool gateway").

use serde_json::{Value as Json, json};
use threadlane_core::ids::{DecisionId, RunId};
use threadlane_core::link::{LinkError, Perform};
use threadlane_core::model::{
    Decision, DecisionKind, Lane, Lease, MemoryAuthor, Mode, NewDecision, Run, RunStatus,
    ToolCallBegin, ToolCallKey, Trust,
};
use threadlane_core::runtime::steps::{perform, tool_call as step};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{DecisionStore, OrgStore, SessionStore};
use threadlane_core::tools::{GateRequest, ToolContext, ToolDefinition, ToolError, Verdict};

use crate::error::{link_error, link_to_tool, tool_error};
use crate::link::LocalLink;
use crate::memory;
use crate::publish::{approval_options, proposal};
use crate::timers;
use crate::tools::{derived_call_key, offered_in};

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
        // Each lane kind has its own tools (Plan.md §5.1); not offering the
        // others is not enough, a backend can name any tool.
        if !offered_in(tool, lane.kind) {
            return Err(ToolError::Blocked(format!(
                "{tool} is not available in a {} lane",
                lane.kind
            )));
        }
        let ask = self
            .gate(&run, lane.project_id, &definition, arguments)
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
        let outcome = if ask {
            self.ask(lease, &run, &lane, &key, tool, arguments).await
        } else {
            inner.tools.execute(&context, tool, arguments.clone()).await
        };
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

    /// Whether the call may proceed (`false`) or must ask you (`true`).
    async fn gate(
        &self,
        run: &Run,
        project_id: threadlane_core::ids::ProjectId,
        definition: &ToolDefinition,
        arguments: &Json,
    ) -> Result<bool, ToolError> {
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
        let events = db.run_events(run.id).await.map_err(tool_error)?;
        let tainted = events.iter().any(|event| event.trust == Trust::External);
        // A run you triggered: every event in its batch is from you
        // (docs/milestone-5.md §1 decision 6).
        let yours = !events.is_empty() && events.iter().all(|event| event.trust == Trust::User);
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
            // Instructions change only when you asked, or you approve.
            // An agent's routine asks first (docs/milestone-8.md §1 decision 4).
            Verdict::Allow => Ok(timers::asks(&definition.name)
                || (!yours && memory::protected(&definition.name, arguments))),
            Verdict::Block { reason } => Err(ToolError::Blocked(reason)),
            Verdict::Ask => Ok(true),
        }
    }

    /// Pauses the call as a decision, with its card, and tells the agent to
    /// end its turn (Plan.md §9.4). The run then ends `deferred`.
    async fn ask(
        &self,
        lease: &Lease,
        run: &Run,
        lane: &Lane,
        key: &ToolCallKey,
        tool: &str,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let inner = &self.core.inner;
        let asked = proposal(&inner.db, lane, tool, arguments).await?;
        inner
            .db
            .open_decision(
                Some(lease),
                &NewDecision {
                    project_id: lane.project_id,
                    lane_id: lane.id,
                    task_id: lane.task_id,
                    session_id: Some(run.session_id),
                    run_id: Some(run.id),
                    tool_call_id: Some(key.key.clone()),
                    tool: tool.to_owned(),
                    arguments: arguments.clone(),
                    arguments_hash: derived_call_key(tool, arguments),
                    kind: DecisionKind::Approval,
                    question: asked.question,
                    context: asked.context,
                    options: approval_options(),
                    recommended: Some(0),
                    offers_standing_rule: false,
                    expires_at: None,
                    created_at: inner.clock.now(),
                },
            )
            .await
            .map_err(tool_error)?;
        inner.wake.wake(WakeTarget::Dispatcher);
        Err(ToolError::WaitingOnYou)
    }

    /// [`threadlane_core::link::HubLink::begin_perform`]: the stored call of
    /// an approved decision in the leased lane, if its arguments still hash
    /// to what you approved (invariant 7), with its intent recorded.
    pub(crate) async fn begin_perform(
        &self,
        lease: &Lease,
        run_id: RunId,
        id: DecisionId,
    ) -> Result<Perform, LinkError> {
        let inner = &self.core.inner;
        let db = &inner.db;
        self.check_lease(lease).await?;
        self.active_run(lease, run_id).await?;
        let decision = db
            .decision(id)
            .await
            .map_err(link_error)?
            .filter(|decision| decision.lane_id == lease.lane_id && decision.approved())
            .ok_or_else(|| {
                LinkError::Rejected(format!("{id} is not an approved decision in this lane"))
            })?;
        if let Some(outcome) = decision.outcome {
            return Ok(Perform::Recorded { outcome });
        }
        if derived_call_key(&decision.tool, &decision.arguments) != decision.arguments_hash {
            let outcome = json!({
                "performed": false,
                "error": format!(
                    "the stored call of {id} is not the one the user approved: its arguments \
                     no longer match their hash, so nothing was done"
                ),
            });
            db.finish_perform(lease, id, &outcome)
                .await
                .map_err(link_error)?;
            return Ok(Perform::Recorded { outcome });
        }
        let begin = db
            .begin_perform(lease, id, inner.clock.now())
            .await
            .map_err(link_error)?;
        let retry = match begin {
            ToolCallBegin::Finished(outcome) => return Ok(Perform::Recorded { outcome }),
            ToolCallBegin::Perform => false,
            ToolCallBegin::OutcomeUnknown => true,
        };
        inner.hook.reach(perform::AFTER_BEGIN)?;
        if memory::is_change(&decision.tool) {
            return self.perform_memory(lease, run_id, &decision).await;
        }
        if timers::asks(&decision.tool) {
            return self.perform_schedule(lease, run_id, &decision).await;
        }
        Ok(Perform::Call {
            tool: decision.tool,
            arguments: decision.arguments,
            retry,
        })
    }

    /// Performs an approved memory change in the hub, which is where memory
    /// is: the same record and step boundaries as a call the runner
    /// performs, with the effect between them. The change is keyed on the
    /// decision, so performing it again after an interruption writes
    /// nothing more (invariant 5). The runner is given the outcome.
    async fn perform_memory(
        &self,
        lease: &Lease,
        run_id: RunId,
        decision: &Decision,
    ) -> Result<Perform, LinkError> {
        let inner = &self.core.inner;
        let lane = self.check_lease(lease).await?;
        let by = MemoryAuthor {
            lease: *lease,
            run_id,
            call_key: format!("perform:{}", decision.id),
            at: inner.clock.now(),
        };
        let outcome = match memory::execute(
            &inner.db,
            &by,
            lane.project_id,
            lane.task_id,
            &decision.tool,
            &decision.arguments,
        )
        .await
        {
            Ok(result) => json!({
                "performed": true,
                "summary": memory::performed(&decision.tool, &decision.arguments),
                "result": result,
            }),
            Err(ToolError::LeaseLost) => {
                return Err(LinkError::StaleLease {
                    lane_id: lease.lane_id,
                });
            }
            Err(error) => json!({ "performed": false, "error": error.to_string() }),
        };
        inner.hook.reach(perform::AFTER_EFFECT)?;
        self.finish_perform(lease, run_id, decision.id, &outcome)
            .await?;
        Ok(Perform::Recorded { outcome })
    }

    /// Creates an approved routine in the hub, which is where timers are,
    /// with the same record and step boundaries as [`Self::perform_memory`].
    /// The routine is keyed on the decision, so performing it again after
    /// an interruption creates nothing more (invariant 5).
    async fn perform_schedule(
        &self,
        lease: &Lease,
        run_id: RunId,
        decision: &Decision,
    ) -> Result<Perform, LinkError> {
        let inner = &self.core.inner;
        let lane = self.check_lease(lease).await?;
        let outcome =
            match timers::perform_schedule(&inner.db, inner.clock.now(), &lane, decision).await {
                Ok(result) => json!({
                    "performed": true,
                    "summary": format!(
                        "Routine {} is created; it is next due at {}.",
                        result["timer_id"].as_str().unwrap_or_default(),
                        result["due_at"].as_str().unwrap_or_default()
                    ),
                    "result": result,
                }),
                Err(ToolError::LeaseLost) => {
                    return Err(LinkError::StaleLease {
                        lane_id: lease.lane_id,
                    });
                }
                Err(error) => json!({ "performed": false, "error": error.to_string() }),
            };
        inner.hook.reach(perform::AFTER_EFFECT)?;
        self.finish_perform(lease, run_id, decision.id, &outcome)
            .await?;
        Ok(Perform::Recorded { outcome })
    }

    /// [`threadlane_core::link::HubLink::finish_perform`].
    pub(crate) async fn finish_perform(
        &self,
        lease: &Lease,
        run_id: RunId,
        id: DecisionId,
        outcome: &Json,
    ) -> Result<(), LinkError> {
        let inner = &self.core.inner;
        self.check_lease(lease).await?;
        self.active_run(lease, run_id).await?;
        inner
            .db
            .finish_perform(lease, id, outcome)
            .await
            .map_err(link_error)?;
        inner.hook.reach(perform::AFTER_FINISH)?;
        Ok(())
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
        ToolError::WaitingOnYou => ("waiting_on_you", &String::new()),
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
        Some("waiting_on_you") => ToolError::WaitingOnYou,
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
            ToolError::WaitingOnYou,
        ] {
            let record = error_record(&error).unwrap();
            assert_eq!(replay(record), Err(error));
        }
        assert!(error_record(&ToolError::LeaseLost).is_none());
        assert_eq!(replay(json!({"outbox_id": 3})), Ok(json!({"outbox_id": 3})));
    }
}
