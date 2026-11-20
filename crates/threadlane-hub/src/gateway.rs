//! The tool gateway: gate, at-most-once record, execution (Plan.md §4.6,
//! §9.1, docs/milestone-1.md §9 "Tool gateway").

use std::sync::Arc;

use serde_json::{Value as Json, json};
use threadlane_core::forge::{REPORT_LIMITATION, RepositoryAction};
use threadlane_core::ids::{DecisionId, RunId, StandingRuleId};
use threadlane_core::link::{LinkError, Perform};
use threadlane_core::model::{
    Decision, DecisionKind, DecisionOption, Lane, Lease, MemoryAuthor, Mode, NewDecision, Run,
    RunStatus, StandingRule, ToolCallBegin, ToolCallKey, Trust,
};
use threadlane_core::runtime::steps::{perform, tool_call as step};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{DecisionStore, OrgStore, SessionStore};
use threadlane_core::tools::{ToolContext, ToolDefinition, ToolError, Verdict};

use crate::dreaming;
use crate::error::{link_error, link_to_tool, tool_error};
use crate::gate::{self, GateInput};
use crate::link::LocalLink;
use crate::memory;
use crate::publish::approval_options;
use crate::timers;
use crate::tools::{
    ASK_USER, CREATE_TASK, HubTools, derived_call_key, offered_for_review, offered_in, question,
};

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
        let events = db.run_events(run.id).await.map_err(tool_error)?;
        let review = events.iter().any(|event| {
            event.source == threadlane_store::TASK_REVIEW_SOURCE
                && event.payload.get("submission").is_some()
        });
        let dream = events.iter().find(|event| dreaming::is_dream(event));
        let definition = if dream.is_some() {
            let definition = dreaming::definitions()
                .into_iter()
                .find(|definition| definition.name == tool)
                .ok_or_else(|| ToolError::Blocked(format!("{tool} is not available in a dream")))?;
            if memory::is_memory(tool) {
                dreaming::check_paths(tool, arguments)?;
            }
            definition
        } else {
            let definition = inner
                .tools
                .definition(tool)
                .ok_or_else(|| ToolError::UnknownTool(tool.to_owned()))?;
            // A backend can name a tool it was never offered.
            if (review && !offered_for_review(tool)) || (!review && !offered_in(tool, lane.kind)) {
                return Err(ToolError::Blocked(format!(
                    "{tool} is not available in a {} lane",
                    lane.kind
                )));
            }
            definition
        };
        let (verdict, new_work) = self
            .gate(&run, &lane, &definition, arguments, &events)
            .await?;
        let asked = match verdict {
            Verdict::Block { reason } => return Err(ToolError::Blocked(reason)),
            _ if tool == REPORT_LIMITATION
                && (dream.is_some() || arguments["require_approval"] == true) =>
            {
                Some(Asked::You {
                    offers_rule: false,
                    proposal: false,
                })
            }
            _ if dream.is_some() && memory::protected(tool, arguments) => Some(Asked::Dream),
            Verdict::Allow { rule: None } if dream.is_some() => None,
            _ if dream.is_some() && tool != REPORT_LIMITATION => {
                return Err(ToolError::Blocked(format!(
                    "{tool} needs authorization outside a dream"
                )));
            }
            Verdict::Allow { rule } => rule.map(Asked::Rule),
            Verdict::Review { offers_rule } => Some(Asked::Review {
                offers_rule,
                proposal: new_work,
            }),
            Verdict::Ask { offers_rule } => Some(Asked::You {
                offers_rule,
                proposal: new_work,
            }),
        };

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
                definition.safe_to_repeat || matches!(asked, Some(Asked::Review { .. })),
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
        let outcome = match (asked, dream) {
            (None, Some(event)) => self.core.dream_call(event, &context, tool, arguments).await,
            (None, None) => inner.tools.execute(&context, tool, arguments.clone()).await,
            (Some(asked), _) => {
                self.ask(lease, &run, &lane, &key, tool, arguments, asked)
                    .await
            }
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

    /// Gathers what governs the call and asks the gate (Plan.md §9.1).
    /// Also says whether the call starts work you did not request, which
    /// makes asking about it a proposal.
    async fn gate(
        &self,
        run: &Run,
        lane: &Lane,
        definition: &ToolDefinition,
        arguments: &Json,
        events: &[threadlane_core::model::Event],
    ) -> Result<(Verdict, bool), ToolError> {
        let db = &self.core.inner.db;
        let project_id = lane.project_id;
        let space = db.space().await.map_err(tool_error)?;
        let project = db
            .project(project_id)
            .await
            .map_err(tool_error)?
            .ok_or_else(|| ToolError::Failed(format!("project {project_id} not found")))?;
        // No binding for the run's agent means the agent may do nothing.
        let bindings = db.bindings(project_id).await.map_err(tool_error)?;
        let binding = bindings
            .iter()
            .find(|binding| Some(binding.agent_id) == run.agent_id);
        let overrides = binding
            .as_ref()
            .map(|binding| gate::overrides(&binding.overrides))
            .unwrap_or_default();
        let rules: Vec<StandingRule> = match run.agent_id {
            Some(agent) => db
                .standing_rules(Some(project_id))
                .await
                .map_err(tool_error)?
                .into_iter()
                .filter(|rule| rule.agent_id == agent)
                .collect(),
            None => Vec::new(),
        };
        let tainted = events.iter().any(|e| e.trust == Trust::External)
            && db.run_tainted(run.id).await.map_err(tool_error)?;
        // A run you triggered: every event in its batch is from you
        // (docs/milestone-5.md §1 decision 6).
        let requested = !events.is_empty() && events.iter().all(|event| event.trust == Trust::User);
        let review = events.iter().any(|event| {
            event.source == threadlane_store::TASK_REVIEW_SOURCE
                && event.payload.get("submission").is_some()
        });
        let mode = binding.map_or(
            if review { Mode::OnRequest } else { Mode::Off },
            |binding| binding.mode,
        );
        let new_work = if definition.name == CREATE_TASK
            && !timers::run_requested(db, run.id, lane.task_id)
                .await
                .map_err(tool_error)?
        {
            // The assignee's mode; a name not bound here is refused before
            // any ask or effect, so the lead's own stands in for it.
            let assignee = match arguments.get("agent").and_then(Json::as_str) {
                Some(name) => db
                    .agents()
                    .await
                    .map_err(tool_error)?
                    .into_iter()
                    .find(|agent| agent.name == name)
                    .map(|agent| agent.id),
                None => run.agent_id,
            };
            Some(
                bindings
                    .iter()
                    .find(|binding| Some(binding.agent_id) == assignee)
                    .map_or(mode, |binding| binding.mode),
            )
        } else {
            None
        };
        let verdict = gate::gate(&GateInput {
            tool: &definition.name,
            tier: definition.tier,
            arguments,
            mode,
            space_ceiling: space.mode_ceiling,
            project_ceiling: project.mode_ceiling,
            overrides: &overrides,
            rules: &rules,
            tainted,
            requested,
            new_work,
        });
        Ok((verdict, new_work.is_some()))
    }

    /// Pauses the call as a decision, with its card, and tells the agent to
    /// end its turn (Plan.md §9.4). The run then ends `deferred`. A call a
    /// standing rule allowed is recorded the same way, already answered by
    /// the rule and with no card, so the run that follows performs it as an
    /// approved one; its run ends at once too.
    #[allow(clippy::too_many_arguments, reason = "the call and its context")]
    async fn ask(
        &self,
        lease: &Lease,
        run: &Run,
        lane: &Lane,
        key: &ToolCallKey,
        tool: &str,
        arguments: &Json,
        asked: Asked,
    ) -> Result<Json, ToolError> {
        let inner = &self.core.inner;
        // Checks the call, so a refused one is refused before any ask
        // (invariant 19).
        let mut proposal = if tool == REPORT_LIMITATION {
            let (title, body) = dreaming::report_issue(arguments)?;
            let existing = inner.db.lane_decisions(lane.id).await.map_err(tool_error)?;
            if existing.iter().any(|d| {
                d.tool == tool
                    && d.arguments["title"]
                        .as_str()
                        .is_some_and(|prior| prior.trim().eq_ignore_ascii_case(&title))
                    && !d
                        .outcome
                        .as_ref()
                        .is_some_and(|outcome| outcome["performed"] == false)
            }) {
                return Err(ToolError::Blocked(
                    "This limitation has already been reported or considered.".into(),
                ));
            }
            crate::publish::Proposal {
                question: format!("Open issue “{title}” in wheregmis/threadlane?"),
                context: body,
            }
        } else {
            let mut args = arguments.clone();
            if matches!(asked, Asked::Dream)
                && let Some(obj) = args.as_object_mut()
            {
                obj.remove("expected_version");
            }
            crate::publish::proposal(&inner.db, lane, tool, &args).await?
        };
        if matches!(asked, Asked::Dream) {
            let details = proposal
                .context
                .split_once("\n\n")
                .map_or("", |(_, details)| details);
            proposal.context = format!("You can apply a suggested change in Project instructions on the Memory tab.\n\n{details}").trim_end().to_owned();
        }
        let (kind, options, recommended) = if tool == ASK_USER {
            (DecisionKind::Input, question(arguments)?.1, None)
        } else if matches!(asked, Asked::Dream) {
            (
                DecisionKind::Proposal,
                vec![DecisionOption {
                    label: "Dismiss".to_owned(),
                    effect: json!({"dream_proposal": true}),
                }],
                None,
            )
        } else if matches!(
            asked,
            Asked::You { proposal: true, .. } | Asked::Review { proposal: true, .. }
        ) {
            (DecisionKind::Proposal, crate::proposal::options(), Some(0))
        } else {
            (DecisionKind::Approval, approval_options(), Some(0))
        };
        let now = inner.clock.now();
        let mut decision = NewDecision {
            project_id: lane.project_id,
            lane_id: lane.id,
            task_id: lane.task_id,
            session_id: Some(run.session_id),
            run_id: Some(run.id),
            tool_call_id: Some(key.key.clone()),
            tool: tool.to_owned(),
            arguments: arguments.clone(),
            arguments_hash: derived_call_key(tool, arguments),
            kind,
            question: proposal.question,
            context: proposal.context,
            options,
            recommended,
            offers_standing_rule: matches!(
                asked,
                Asked::You {
                    offers_rule: true,
                    ..
                } | Asked::Review {
                    offers_rule: true,
                    ..
                }
            ),
            expires_at: (!matches!(asked, Asked::Rule(_)))
                .then(|| now + inner.settings.decision_expiry),
            created_at: now,
        };
        let authorized = match asked {
            Asked::Review { .. } => {
                let (id, reason) = self.classify(lease, run, &decision).await?;
                if reason.is_some() {
                    decision.offers_standing_rule = false;
                    decision.expires_at = None;
                }
                inner
                    .db
                    .reviewed_decision(lease, &id, &decision, reason.as_deref())
                    .await
                    .map(|()| false)
            }
            Asked::You { .. } | Asked::Dream => inner
                .db
                .open_decision(Some(lease), &decision)
                .await
                .map(|_| false),
            Asked::Rule(rule) => inner
                .db
                .allow_by_rule(lease, &decision, rule)
                .await
                .map(|decision| decision.approved()),
        }
        .map_err(tool_error)?;
        inner.wake.wake(WakeTarget::Dispatcher);
        if authorized {
            Err(ToolError::AuthorizedAndQueued)
        } else {
            Err(ToolError::WaitingOnYou)
        }
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
        let lane = self.check_lease(lease).await?;
        self.active_run(lease, run_id).await?;
        if inner
            .db
            .run_events(run_id)
            .await
            .map_err(link_error)?
            .iter()
            .any(dreaming::is_dream)
        {
            return Err(LinkError::Rejected(
                "a dream cannot perform a decision".into(),
            ));
        }

        let decision = db
            .decision(id)
            .await
            .map_err(link_error)?
            .filter(|decision| {
                decision.lane_id == lane.id
                    && decision.project_id == lane.project_id
                    && decision.approved()
                    && !decision.is_instruction_proposal()
            })
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
        if decision.tool == timers::CREATE_SCHEDULE {
            return self.perform_schedule(lease, run_id, &decision).await;
        }
        if decision.tool == CREATE_TASK {
            return self.perform_task(lease, run_id, &decision).await;
        }
        if matches!(
            decision.tool.as_str(),
            crate::COMMENT
                | crate::CREATE_ISSUE
                | crate::MERGE
                | crate::MARK_READY
                | crate::CLOSE_PULL_REQUEST
                | REPORT_LIMITATION
        ) {
            let (remote, action) = if decision.tool == REPORT_LIMITATION {
                let (title, body) = dreaming::report_issue(&decision.arguments)
                    .map_err(|e| LinkError::Rejected(e.to_string()))?;
                (
                    dreaming::UPSTREAM.to_owned(),
                    RepositoryAction::CreateIssue { title, body },
                )
            } else {
                let action = crate::publish::repository_action(&decision.tool, &decision.arguments)
                    .map_err(|e| LinkError::Rejected(e.to_string()))?;
                if lane.kind != threadlane_core::model::LaneKind::Task {
                    return Err(LinkError::Rejected(
                        "repository actions require a task lane".into(),
                    ));
                }
                let repo = db
                    .repositories(lane.project_id)
                    .await
                    .map_err(link_error)?
                    .into_iter()
                    .find(|r| decision.arguments["repository"] == r.name)
                    .ok_or_else(|| {
                        LinkError::Rejected("repository is no longer registered".into())
                    })?;
                (repo.remote_url, action)
            };
            // shortcut: a lost forge response stays unknown until these writes have a remote idempotency key.
            if !db
                .claim_forge_dispatch(lease, id)
                .await
                .map_err(link_error)?
            {
                let outcome =
                    json!({"unknown":true,"error":"A previous dispatch was interrupted."});
                self.finish_perform(lease, run_id, id, &outcome).await?;
                return Ok(Perform::Recorded { outcome });
            }
            return Ok(Perform::Repository { remote, action });
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
            expected_version: None,
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

    /// Creates the task of an accepted proposal in the hub, which is where
    /// tasks are, with the same record and step boundaries as
    /// [`Self::perform_memory`]. The task is keyed on the decision, so
    /// performing it again after an interruption creates nothing more
    /// (invariant 5), and it is work you requested from then on.
    async fn perform_task(
        &self,
        lease: &Lease,
        run_id: RunId,
        decision: &Decision,
    ) -> Result<Perform, LinkError> {
        let inner = &self.core.inner;
        let lane = self.check_lease(lease).await?;
        let run = self.active_run(lease, run_id).await?;
        let tools = HubTools::new(
            inner.db.clone(),
            Arc::clone(&inner.clock),
            Arc::clone(&inner.wake) as Arc<dyn Waker>,
            None,
        );
        let context = ToolContext {
            lease: *lease,
            run_id,
            project_id: lane.project_id,
            agent_id: run.agent_id,
            task_id: lane.task_id,
            call_id: None,
        };
        let outcome = match tools.create_accepted_task(&context, decision).await {
            Ok(task) if task.get("review_status").is_some() => json!({
                "performed": true,
                "summary": format!("Reused the review on {}.", task["task_id"].as_str().unwrap_or_default()),
                "result": task,
            }),
            Ok(task) => json!({
                "performed": true,
                "summary": format!(
                    "Task {} is created: {}.",
                    task["task_id"].as_str().unwrap_or_default(),
                    task["title"].as_str().unwrap_or_default()
                ),
                "result": task,
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
        if inner
            .db
            .run_events(run_id)
            .await
            .map_err(link_error)?
            .iter()
            .any(dreaming::is_dream)
        {
            return Err(LinkError::Rejected(
                "a dream cannot perform a decision".into(),
            ));
        }

        inner
            .db
            .finish_perform(lease, id, outcome)
            .await
            .map_err(link_error)?;
        inner.hook.reach(perform::AFTER_FINISH)?;
        Ok(())
    }
}

/// Who answers a paused call; `proposal` when it starts work you did not
/// request.
#[derive(Clone, Copy)]
enum Asked {
    Dream,
    Review { offers_rule: bool, proposal: bool },
    You { offers_rule: bool, proposal: bool },
    Rule(StandingRuleId),
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
        ToolError::AuthorizedAndQueued => ("authorized_and_queued", &String::new()),
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
        Some("authorized_and_queued") => ToolError::AuthorizedAndQueued,
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
            ToolError::AuthorizedAndQueued,
        ] {
            let record = error_record(&error).unwrap();
            assert_eq!(replay(record), Err(error));
        }
        assert!(error_record(&ToolError::LeaseLost).is_none());
        assert_eq!(replay(json!({"outbox_id": 3})), Ok(json!({"outbox_id": 3})));
    }
}
