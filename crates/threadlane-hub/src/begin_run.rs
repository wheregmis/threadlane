//! Starting a run inside the hub: Plan.md §4.3 steps 2 to 5.3, §4.4, §8.5
//! (docs/milestone-1.md §9 "begin_run").

use std::collections::HashMap;

use serde_json::Value as Json;
use threadlane_core::backend::{DENY_RULES, NativeSession, free_backends};
use threadlane_core::ids::{EventId, SessionId};
use threadlane_core::link::{Begin, LinkError, RunAssignment};
use threadlane_core::memory::progress_path;
use threadlane_core::model::{
    Agent, BackendKind, Binding, Event, Lane, LaneKind, Lease, Mode, Repository, Session,
    SessionChoice, SessionStatus, StartRun, Task,
};
use threadlane_core::runtime::steps::begin_run as step;
use threadlane_core::store::{
    DecisionStore, InboxStore, MachineStore, MemoryStore, OrgStore, OutboxStore, SessionStore,
    TaskStore,
};
use threadlane_core::time::Timestamp;
use uuid::Uuid;

use crate::accounts::cooling;
use crate::context::{PromptParts, appended_instructions, loaded_memory, render_prompt};
use crate::dreaming;
use crate::error::link_error;
use crate::link::LocalLink;
use crate::tools::offered_in;

/// Why events were skipped by the initiative check (Plan.md §4.3 step 3):
/// they are not requested work, and the lane's agent works On request.
/// Kept word for word, since the recorded events are found by it.
pub const SKIP_REASON: &str = "not requested, and the lead works on request only";

/// Why events were skipped because the lane's agent is Off in its project
/// (invariant 14).
pub const OFF_REASON: &str = "the agent is off in this project";

/// Why events were skipped because the lane's task has ended: done, failed,
/// or cancelled.
pub const ENDED_REASON: &str = "the task has ended";

/// The session a run will work in, as decided before `start_run`.
struct SessionPlan {
    choice: SessionChoice,
    native: NativeSession,
    /// The id chosen for a new session; `None` when resuming.
    new_native_id: Option<String>,
}

impl LocalLink {
    pub(crate) async fn begin(&self, lease: &Lease) -> Result<Begin, LinkError> {
        let inner = &self.core.inner;
        let db = &inner.db;
        let lane = self.check_lease(lease).await?;
        db.check_running(lease).await.map_err(link_error)?;
        let now = inner.clock.now();

        // 1. Pending events.
        let mut pending = db.pending_events(lane.id).await.map_err(link_error)?;
        inner.hook.reach(step::AFTER_READ_PENDING)?;
        if pending.is_empty() || lane.backoff_until.is_some_and(|until| until > now) {
            return Ok(Begin::Idle);
        }

        if pending.iter().any(dreaming::is_dream) {
            let active = db
                .dreaming_timer(lane.project_id)
                .await
                .map_err(link_error)?
                .map(|(id, _)| id);
            let stale: Vec<_> = pending
                .iter()
                .filter(|event| {
                    dreaming::is_dream(event)
                        && active
                            .is_none_or(|id| event.payload["timer_id"].as_i64() != Some(id.get()))
                })
                .map(|event| event.id)
                .collect();
            if !stale.is_empty() {
                db.skip_events(lease, &stale, "dreaming setting changed")
                    .await
                    .map_err(link_error)?;
                pending.retain(|event| !stale.contains(&event.id));
            }
            if pending.is_empty() {
                return Ok(Begin::Idle);
            }
        }
        let dreaming = pending.first().is_some_and(dreaming::is_dream);

        // 2. Initiative. A task that has ended does no more work. An agent
        // that is effectively Off, or not bound in the project at all, never
        // starts (invariant 14). Work you did not request starts only in
        // Assisted and Autonomous (Plan.md §2.3); a task you asked for is
        // requested work whatever arrives in its lane (docs/milestone-3.md
        // §1 item 2).
        let (agent, binding, task) = self.lane_agent(&lane).await?;
        let mode = match &binding {
            Some(binding) => {
                let ceiling = db
                    .effective_ceiling(lane.project_id)
                    .await
                    .map_err(link_error)?;
                binding.mode.stricter(ceiling)
            }
            None => Mode::Off,
        };
        let requested = task.as_ref().is_some_and(|task| task.origin.is_requested())
            || pending.iter().any(|event| event.requested);
        let skip = if task.as_ref().is_some_and(|task| task.status.is_terminal()) {
            Some(ENDED_REASON)
        } else if mode == Mode::Off {
            Some(OFF_REASON)
        } else if !requested && !crate::gate::may_start_unrequested(mode) {
            Some(SKIP_REASON)
        } else {
            None
        };
        if let Some(reason) = skip {
            let ids: Vec<EventId> = pending.iter().map(|event| event.id).collect();
            db.skip_events(lease, &ids, reason)
                .await
                .map_err(link_error)?;
            inner.hook.reach(step::AFTER_INITIATIVE_CHECK)?;
            return Ok(Begin::Idle);
        }
        inner.hook.reach(step::AFTER_INITIATIVE_CHECK)?;

        // 3. Session routing, grouping, isolation.
        let (session, events) = if dreaming {
            pending.truncate(1);
            (None, pending)
        } else {
            pending.retain(|event| !dreaming::is_dream(event));
            self.choose_batch(&lane, pending, now).await?
        };

        let mut retained = Vec::with_capacity(events.len());
        let mut tainted = false;
        for event in events {
            if event.trust == threadlane_core::model::Trust::External {
                let answer = self
                    .core
                    .permission_question(
                        lease,
                        &format!("event:{}", event.id.get()),
                        crate::review::Question::Screen,
                        &crate::review::event_data(&event),
                    )
                    .await
                    .map_err(link_error)?;
                match answer["verdict"].as_str() {
                    Some("drop") => {
                        db.skip_events(
                            lease,
                            &[event.id],
                            answer["reason"].as_str().unwrap_or("Dropped by screening"),
                        )
                        .await
                        .map_err(link_error)?;
                        continue;
                    }
                    Some("clean") => {}
                    _ => tainted = true,
                }
            }
            retained.push(event);
        }
        let events = retained;
        let requested = task.as_ref().is_some_and(|t| t.origin.is_requested())
            || events.iter().any(|e| e.requested);
        let initiative = if tainted { mode.stepped_down() } else { mode };
        if events.is_empty() || (!requested && !crate::gate::may_start_unrequested(initiative)) {
            let ids: Vec<_> = events.iter().map(|e| e.id).collect();
            db.skip_events(lease, &ids, SKIP_REASON)
                .await
                .map_err(link_error)?;
            return Ok(Begin::Idle);
        }

        // 4. Backend.
        let backend = self.choose_backend(&agent).await?;

        // 5. Native session.
        let plan = self.plan_session(session, backend, now).await?;

        // 6. Open the run and claim its events.
        let run = db
            .start_run(
                lease,
                &StartRun {
                    session: plan.choice,
                    agent_id: Some(agent.id),
                    backend: Some(backend),
                    native_session_id: plan.new_native_id.clone(),
                    events: events.iter().map(|event| event.id).collect(),
                    started_at: now,
                },
            )
            .await
            .map_err(link_error)?;
        inner.hook.reach(step::AFTER_START_RUN)?;

        // 7. Context.
        let project = db
            .project(lane.project_id)
            .await
            .map_err(link_error)?
            .ok_or_else(|| LinkError::Rejected(format!("project {} not found", lane.project_id)))?;
        let recent = if !dreaming && plan.new_native_id.is_some() && lane.kind == LaneKind::Lead {
            let mut recent = db
                .recent_messages(project.id, inner.settings.recent_messages)
                .await
                .map_err(link_error)?;
            recent.reverse();
            recent
        } else {
            Vec::new()
        };
        // A fresh task session starts from the task's progress file.
        let progress = match &task {
            Some(task) if plan.new_native_id.is_some() => db
                .read(&progress_path(project.id, task.id))
                .await
                .map_err(link_error)?
                .map(|file| file.content),
            _ => None,
        };
        let decisions = if dreaming {
            Vec::new()
        } else {
            db.pending_decisions(lane.id)
                .await
                .map_err(link_error)?
                .into_iter()
                .filter(|decision| !decision.is_instruction_proposal())
                .collect()
        };
        // The answers among the events: the approved calls, which the
        // runner performs before the session resumes (Plan.md §9.4).
        let mut perform = Vec::new();
        for id in events.iter().filter_map(|event| event.decision_ref) {
            if db
                .decision(id)
                .await
                .map_err(link_error)?
                .is_some_and(|decision| decision.approved())
            {
                perform.push(id);
            }
        }
        let prompt = if dreaming {
            dreaming::PROMPT.to_owned()
        } else {
            render_prompt(&PromptParts {
                project: &project,
                task: task.as_ref(),
                progress: progress.as_deref(),
                recent: &recent,
                decisions: &decisions,
                events: &events,
            })
        };
        let mut memory = Vec::new();
        for (heading, path) in loaded_memory(project.id, lane.kind)
            .into_iter()
            .filter(|_| !dreaming)
        {
            let content = db.read(&path).await.map_err(link_error)?;
            memory.push((
                heading,
                content.map(|file| file.content).unwrap_or_default(),
            ));
        }
        // The space lead also sees the space's other projects.
        let general_lead = !dreaming && project.is_general && lane.kind == LaneKind::Lead;
        if general_lead {
            memory.push((
                "Projects in this space:",
                crate::space_lead::summary(db).await.map_err(link_error)?,
            ));
        }
        let appended_instructions = if dreaming {
            String::new()
        } else {
            appended_instructions(&agent, lane.kind, &memory)
        };
        let repositories = self.task_repositories(task.as_ref()).await?;
        let space_lead_ask = if lane.kind == LaneKind::Lead {
            crate::space_lead::answered_ask(db, &events)
                .await
                .map_err(link_error)?
                .map(|ask| ask.id)
        } else {
            None
        };
        inner.hook.reach(step::AFTER_ASSEMBLE_CONTEXT)?;

        // 8. The assignment.
        Ok(Begin::Run(Box::new(RunAssignment {
            dreaming,
            space_lead_ask,
            run_id: run.id,
            trace_id: run.trace_id,
            session_id: run.session_id,
            project_id: project.id,
            lane_kind: lane.kind,
            task_id: task.as_ref().map(|task| task.id),
            agent_name: agent.name.clone(),
            repositories,
            events: events.iter().map(|event| event.id).collect(),
            perform,
            backend,
            model: model_for(
                binding.as_ref(),
                &agent,
                backend,
                &inner.settings.default_models,
            ),
            native_session: plan.native,
            appended_instructions,
            prompt,
            tools: if dreaming {
                dreaming::definitions()
            } else {
                inner
                    .tools
                    .definitions()
                    .into_iter()
                    .filter(|tool| offered_in(&tool.name, lane.kind))
                    .filter(|tool| general_lead || tool.name != crate::space_lead::ASK_PROJECT_LEAD)
                    .collect()
            },
            // A lead has no file or shell tools to refuse anything with; a
            // task agent has full access less the direct shared effects.
            remove_builtin_tools: lane.kind == LaneKind::Lead,
            deny_rules: match lane.kind {
                LaneKind::Lead => Vec::new(),
                LaneKind::Task => DENY_RULES.iter().map(|rule| (*rule).to_owned()).collect(),
            },
            wall_time_limit: inner.settings.wall_time_limit,
            max_turns: inner.settings.max_turns,
        })))
    }

    /// The agent working the lane and its binding in the project: the
    /// project's lead for a lead lane, the task's agent for a task lane, with
    /// the task.
    async fn lane_agent(
        &self,
        lane: &Lane,
    ) -> Result<(Agent, Option<Binding>, Option<Task>), LinkError> {
        let db = &self.core.inner.db;
        let (agent_id, binding, task) = match lane.kind {
            LaneKind::Lead => {
                let binding = db
                    .lead_binding(lane.project_id)
                    .await
                    .map_err(link_error)?
                    .ok_or_else(|| {
                        LinkError::Rejected(format!(
                            "project {} has no lead binding",
                            lane.project_id
                        ))
                    })?;
                (binding.agent_id, Some(binding), None)
            }
            LaneKind::Task => {
                let task_id = lane.task_id.ok_or_else(|| {
                    LinkError::Rejected(format!("task lane {} has no task", lane.id))
                })?;
                let task = db
                    .task(task_id)
                    .await
                    .map_err(link_error)?
                    .ok_or_else(|| LinkError::Rejected(format!("task {task_id} not found")))?;
                let binding = db
                    .bindings(lane.project_id)
                    .await
                    .map_err(link_error)?
                    .into_iter()
                    .find(|binding| binding.agent_id == task.agent_id);
                (task.agent_id, binding, Some(task))
            }
        };
        let agent = db
            .agent(agent_id)
            .await
            .map_err(link_error)?
            .ok_or_else(|| LinkError::Rejected(format!("agent {agent_id} not found")))?;
        Ok((agent, binding, task))
    }

    /// Routes each pending event to a session and picks the group holding the
    /// oldest event. A group with an event that already failed
    /// `isolate_after_attempts` times is cut to its oldest event.
    async fn choose_batch(
        &self,
        lane: &Lane,
        pending: Vec<Event>,
        now: Timestamp,
    ) -> Result<(Option<Session>, Vec<Event>), LinkError> {
        let mut routed: Vec<(Event, Option<Session>)> = Vec::with_capacity(pending.len());
        for event in pending {
            let session = self.route(lane, &event, now).await?;
            routed.push((event, session));
        }
        let first_key = routed
            .first()
            .map(|(_, session)| session.as_ref().map(|session| session.id));
        let session = routed.first().and_then(|(_, session)| session.clone());
        let mut events: Vec<Event> = routed
            .into_iter()
            .filter(|(_, session)| Some(session.as_ref().map(|session| session.id)) == first_key)
            .map(|(event, _)| event)
            .collect();
        let isolate = self.core.inner.settings.isolate_after_attempts;
        if events.iter().any(|event| event.attempts >= isolate) {
            events.truncate(1);
        }
        if lane.kind == LaneKind::Lead && events.len() > 1 {
            let db = &self.core.inner.db;
            let mut scoped = Vec::with_capacity(events.len());
            let mut first_ask = None;
            for event in events {
                let ask = crate::space_lead::answered_ask(db, std::slice::from_ref(&event))
                    .await
                    .map_err(link_error)?
                    .map(|ask| ask.id);
                if !scoped.is_empty() && ask != first_ask {
                    break;
                }
                first_ask = ask;
                scoped.push(event);
            }
            events = scoped;
        }
        Ok((session, events))
    }

    /// Plan.md §4.4, first match wins. `None` means a new session.
    async fn route(
        &self,
        lane: &Lane,
        event: &Event,
        now: Timestamp,
    ) -> Result<Option<Session>, LinkError> {
        let db = &self.core.inner.db;
        // 1. The answer to a decision resumes the decision's session.
        if let Some(decision_id) = event.decision_ref {
            let session_id = db
                .decision(decision_id)
                .await
                .map_err(link_error)?
                .and_then(|decision| decision.session_id);
            if let Some(session) = self.open_session_in_lane(lane, session_id).await? {
                return Ok(Some(session));
            }
        }
        // 2. An explicit session reference, if that session is open.
        if let Some(session) = self.open_session_in_lane(lane, event.session_ref).await? {
            return Ok(Some(session));
        }
        // 3. A reply to an outbox message resumes the session that sent it.
        if let Some(outbox_id) = event.reply_to_ref {
            let session_id = db
                .message(outbox_id)
                .await
                .map_err(link_error)?
                .and_then(|message| message.session_id);
            if let Some(session) = self.open_session_in_lane(lane, session_id).await? {
                return Ok(Some(session));
            }
        }
        let latest = db.latest_open_session(lane.id).await.map_err(link_error)?;
        Ok(match lane.kind {
            // 4. A task lane resumes its current session.
            LaneKind::Task => latest,
            // 5. A lead lane resumes its latest session if recently active.
            LaneKind::Lead => latest.filter(|session| {
                now.saturating_since(session.last_active_at) <= self.core.inner.settings.idle_window
            }),
        })
        // 6. Otherwise a new session (`None`).
    }

    async fn open_session_in_lane(
        &self,
        lane: &Lane,
        session_id: Option<SessionId>,
    ) -> Result<Option<Session>, LinkError> {
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        Ok(self
            .core
            .inner
            .db
            .session(session_id)
            .await
            .map_err(link_error)?
            .filter(|session| session.status == SessionStatus::Open && session.lane_id == lane.id))
    }

    /// The task's repositories, in the order it named them.
    async fn task_repositories(&self, task: Option<&Task>) -> Result<Vec<Repository>, LinkError> {
        let Some(task) = task.filter(|task| !task.repositories.is_empty()) else {
            return Ok(Vec::new());
        };
        let registered = self
            .core
            .inner
            .db
            .repositories(task.project_id)
            .await
            .map_err(link_error)?;
        task.repositories
            .iter()
            .map(|id| {
                registered
                    .iter()
                    .find(|repository| repository.id == *id)
                    .cloned()
                    .ok_or_else(|| {
                        LinkError::Rejected(format!(
                            "task {}'s repository {id} is not registered in its project",
                            task.id
                        ))
                    })
            })
            .collect()
    }

    /// The first of the agent's backends (all, in [`BackendKind::ALL`] order,
    /// if it names none) that this machine reported usable, on an account
    /// that is not cooling down.
    async fn choose_backend(&self, agent: &Agent) -> Result<BackendKind, LinkError> {
        let db = &self.core.inner.db;
        let machine = db
            .machine(self.machine)
            .await
            .map_err(link_error)?
            .ok_or_else(|| LinkError::Rejected(format!("machine {} not found", self.machine)))?;
        let now = self.core.inner.clock.now();
        let cooling = cooling(&db.accounts().await.map_err(link_error)?, now);
        let usable = free_backends(&machine.backends, self.machine, &cooling);
        let preferred: &[BackendKind] = if agent.backends.is_empty() {
            BackendKind::ALL
        } else {
            &agent.backends
        };
        preferred
            .iter()
            .copied()
            .find(|backend| usable.contains(backend))
            .ok_or_else(|| {
                LinkError::Rejected(format!(
                    "machine {} has none of agent {}'s backends usable with a free account",
                    self.machine, agent.name
                ))
            })
    }

    /// Resume the backend's own session only on the same machine and backend
    /// with a known native id, while it is younger than the age limit;
    /// otherwise close the session and start fresh, from the task's progress
    /// file for a task (Plan.md §8.6). A missing native session is the third
    /// cause: the run fails `SessionMissing` and the hub closes it then.
    async fn plan_session(
        &self,
        session: Option<Session>,
        backend: BackendKind,
        now: Timestamp,
    ) -> Result<SessionPlan, LinkError> {
        if let Some(session) = session {
            let young = now.saturating_since(session.created_at)
                <= self.core.inner.settings.session_max_age;
            match &session.native_session_id {
                // Milestone 6 resumes on another machine from a checkpoint;
                // until then a session last used elsewhere is replaced.
                Some(native)
                    if young
                        && session.machine_id == Some(self.machine)
                        && session.backend == Some(backend) =>
                {
                    return Ok(SessionPlan {
                        choice: SessionChoice::Resume(session.id),
                        native: NativeSession::Resume { id: native.clone() },
                        new_native_id: None,
                    });
                }
                _ => {
                    self.core
                        .inner
                        .db
                        .close_session(session.id)
                        .await
                        .map_err(link_error)?;
                }
            }
        }
        let id = Uuid::new_v4().to_string();
        Ok(SessionPlan {
            choice: SessionChoice::New,
            native: NativeSession::Start { id: id.clone() },
            new_native_id: Some(id),
        })
    }
}

/// The model for `backend`: the binding's choice, else the agent's, else the
/// hub's configured default for the backend, else `None` (the backend's own
/// default).
fn model_for(
    binding: Option<&Binding>,
    agent: &Agent,
    backend: BackendKind,
    defaults: &HashMap<BackendKind, String>,
) -> Option<String> {
    let pick = |models: &Json| {
        models
            .get(backend.as_str())
            .and_then(Json::as_str)
            .map(str::to_owned)
    };
    binding
        .and_then(|binding| pick(&binding.models))
        .or_else(|| pick(&agent.models))
        .or_else(|| defaults.get(&backend).cloned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use threadlane_core::ids::{AgentId, BindingId, ProjectId};

    use super::*;

    fn agent(models: Json) -> Agent {
        Agent {
            id: AgentId(1),
            name: "Lead".into(),
            role: "lead".into(),
            instructions: String::new(),
            backends: Vec::new(),
            models,
            tools: json!({}),
            requirements: json!({}),
        }
    }

    fn binding(models: Json) -> Binding {
        Binding {
            id: BindingId(1),
            project_id: ProjectId(1),
            agent_id: AgentId(1),
            mode: Mode::OnRequest,
            is_lead: true,
            models,
            max_parallel: 1,
            overrides: json!({}),
        }
    }

    #[test]
    fn the_model_is_the_binding_s_then_the_agent_s_then_the_hub_default() {
        let claude = BackendKind::Claude;
        let defaults = HashMap::from([(claude, "haiku".to_owned())]);
        let none = HashMap::new();
        let bound = binding(json!({ "claude": "opus" }));
        let unbound = binding(json!({ "codex": "gpt" }));
        let named = agent(json!({ "claude": "sonnet" }));
        let unnamed = agent(json!({}));

        let model = |binding, agent, defaults| model_for(binding, agent, claude, defaults);
        assert_eq!(
            model(Some(&bound), &named, &defaults).as_deref(),
            Some("opus")
        );
        assert_eq!(
            model(Some(&unbound), &named, &defaults).as_deref(),
            Some("sonnet")
        );
        assert_eq!(model(None, &named, &defaults).as_deref(), Some("sonnet"));
        assert_eq!(
            model(Some(&unbound), &unnamed, &defaults).as_deref(),
            Some("haiku")
        );
        assert_eq!(model(None, &unnamed, &none), None);
        assert_eq!(
            model_for(None, &unnamed, BackendKind::Codex, &defaults),
            None,
            "a default for another backend does not apply"
        );
    }
}
