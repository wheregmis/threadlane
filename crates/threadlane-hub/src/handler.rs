//! The client interface: handler (Plan.md §6.3), feed, info, projects, the
//! organization, and the overview (Plan.md §11.2).

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use threadlane_core::client::{
    Accepted, ApiError, BindingSummary, DecisionReply, DecisionRequest, FeedItem, HubApi, HubInfo,
    MemoryReply, MemoryRequest, OrgReply, OrgRequest, OverviewReply, OverviewRequest, PairingReply,
    PairingRequest, ProjectSummary, StandingRuleSummary, UserMessage,
};
use threadlane_core::client::{TaskReply, TaskRequest, TimerReply, TimerRequest};
use threadlane_core::github::normalize_remote;
use threadlane_core::ids::{DecisionId, ProjectId};
use threadlane_core::memory::INSTRUCTIONS_BUDGET;
use threadlane_core::model::{
    Agent, Answered, BackendKind, Binding, Decision, DecisionAnswer, DecisionOption,
    DecisionStatus, EventKind, Inserted, Mode, NewAgent, NewBinding, NewEvent, NewProject,
    NewRepository, NewStandingRule, Project, StandingRule, Trust,
};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{
    DecisionStore, InboxStore, MachineStore, OrgStore, OutboxStore, SessionStore, StoreError,
    TaskStore,
};
use threadlane_core::time::Timestamp;
use threadlane_core::workspace::{valid_branch_name, valid_repository_name};

use crate::core::HubCore;
use crate::defaults::{LEAD_ROLE, ensure_defaults};
use crate::error::api_error;
use crate::gate;
use crate::tools::CLIENT_CHANNEL;

mod memory;
mod overview;
mod tasks;

/// Source name of messages from the client.
pub const CLIENT_SOURCE: &str = "client";

/// The default branch of a repository registered without one.
const DEFAULT_BRANCH: &str = "main";

/// Feed items returned per call.
const FEED_PAGE: u32 = 200;

/// [`HubApi`] over a [`HubCore`].
#[derive(Debug, Clone)]
pub(crate) struct ClientApi {
    core: HubCore,
    pub(crate) updates: Option<std::sync::Arc<crate::updater::HubUpdates>>,
}

impl ClientApi {
    pub(crate) fn new(core: HubCore) -> Self {
        Self {
            core,
            updates: None,
        }
    }

    async fn project_named(&self, name: Option<&str>) -> Result<Project, ApiError> {
        let db = &self.core.inner.db;
        match name {
            None => db.general_project().await.map_err(api_error),
            Some(name) => db
                .projects()
                .await
                .map_err(api_error)?
                .into_iter()
                .find(|project| project.name == name)
                .ok_or_else(|| ApiError::NotFound(format!("project {name:?}"))),
        }
    }

    async fn agent_named(&self, name: &str) -> Result<Agent, ApiError> {
        let name = name.trim();
        self.core
            .inner
            .db
            .agents()
            .await
            .map_err(api_error)?
            .into_iter()
            .find(|agent| agent.name == name)
            .ok_or_else(|| ApiError::NotFound(format!("agent {name:?}")))
    }

    /// Refuses a message to a lead that is Off: it never runs (invariant
    /// 14), so the client would wait for a reply that cannot come.
    async fn lead_is_on(&self, project: &Project) -> Result<(), ApiError> {
        let db = &self.core.inner.db;
        if let Some(lead) = db.lead_binding(project.id).await.map_err(api_error)? {
            let ceiling = db.effective_ceiling(project.id).await.map_err(api_error)?;
            if lead.effective_mode(ceiling) == Mode::Off {
                return Err(ApiError::Invalid(format!(
                    "the lead of project {:?} is off",
                    project.name
                )));
            }
        }
        Ok(())
    }

    /// Answers a decision by option label; with `always`, also creates the
    /// standing rule it offers, in the same transaction.
    async fn answer(
        &self,
        decision: DecisionId,
        option: &str,
        note: Option<String>,
        always: bool,
    ) -> Result<DecisionReply, ApiError> {
        let inner = &self.core.inner;
        let db = &inner.db;
        let current = db
            .decision(decision)
            .await
            .map_err(api_error)?
            .ok_or_else(|| ApiError::NotFound(format!("decision {decision}")))?;
        // Answering is idempotent: the first answer stands.
        if current.status != DecisionStatus::Pending {
            return Ok(DecisionReply::Answered {
                decision: Box::new(current),
                recorded: false,
            });
        }
        let wanted = option.trim();
        let index = current
            .options
            .iter()
            .position(|option| option.label.eq_ignore_ascii_case(wanted))
            .ok_or_else(|| {
                let labels: Vec<&str> = current
                    .options
                    .iter()
                    .map(|option| option.label.as_str())
                    .collect();
                ApiError::Invalid(format!(
                    "{decision} has no option {wanted:?}; answer with {}",
                    labels.join(" or ")
                ))
            })?;
        let now = inner.clock.now();
        let rule = if always {
            Some(self.rule_from(&current, index, now).await?)
        } else {
            None
        };
        let answered = db
            .answer_with_rule(
                decision,
                &DecisionAnswer {
                    option: u32::try_from(index).unwrap_or(u32::MAX),
                    note: note.filter(|note| !note.trim().is_empty()),
                    standing_rule: always,
                    answered_at: now,
                },
                rule.as_ref(),
            )
            .await
            .map_err(api_error)?;
        let recorded = matches!(answered, Answered::Recorded(_));
        let decision = db
            .decision(decision)
            .await
            .map_err(api_error)?
            .ok_or_else(|| ApiError::NotFound(format!("decision {decision}")))?;
        if recorded {
            crate::proposal::record_answer(db, &decision, now)
                .await
                .map_err(api_error)?;
            inner.wake.wake(WakeTarget::Lane(current.lane_id));
        }
        Ok(DecisionReply::Answered {
            decision: Box::new(decision),
            recorded,
        })
    }

    /// The standing rule "always" makes from `decision` answered with
    /// option `index`: the asking run's agent, the project, the tool, and
    /// the target fields of the arguments.
    async fn rule_from(
        &self,
        decision: &Decision,
        index: usize,
        now: Timestamp,
    ) -> Result<NewStandingRule, ApiError> {
        let inner = &self.core.inner;
        if !decision.offers_standing_rule {
            return Err(ApiError::Invalid(format!(
                "{} offers no standing rule; answer it without always",
                decision.id
            )));
        }
        if !decision
            .options
            .get(index)
            .is_some_and(DecisionOption::approves)
        {
            return Err(ApiError::Invalid(
                "always applies only to an option that approves".to_owned(),
            ));
        }
        let agent_id = match decision.run_id {
            Some(run) => inner.db.run(run).await.map_err(api_error)?,
            None => None,
        }
        .and_then(|run| run.agent_id)
        .ok_or_else(|| ApiError::Invalid(format!("{} was asked by no agent", decision.id)))?;
        let tier = inner
            .tools
            .definition(&decision.tool)
            .ok_or_else(|| ApiError::Internal(format!("no tool {}", decision.tool)))?
            .tier;
        Ok(NewStandingRule {
            project_id: decision.project_id,
            agent_id,
            tool: decision.tool.clone(),
            pattern: gate::rule_pattern(&decision.tool, &decision.arguments),
            tier,
            created_at: now,
        })
    }

    async fn rule_summary(&self, rule: StandingRule) -> Result<StandingRuleSummary, ApiError> {
        let db = &self.core.inner.db;
        let project = db
            .project(rule.project_id)
            .await
            .map_err(api_error)?
            .map_or_else(|| rule.project_id.to_string(), |project| project.name);
        let agent = db
            .agent(rule.agent_id)
            .await
            .map_err(api_error)?
            .map_or_else(|| rule.agent_id.to_string(), |agent| agent.name);
        Ok(StandingRuleSummary {
            rule,
            project,
            agent,
        })
    }

    async fn read_feed(&self, after: i64) -> Result<Vec<FeedItem>, ApiError> {
        let db = &self.core.inner.db;
        let messages = db
            .delivered_after(CLIENT_CHANNEL, after, FEED_PAGE)
            .await
            .map_err(api_error)?;
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let names: HashMap<ProjectId, String> = db
            .projects()
            .await
            .map_err(api_error)?
            .into_iter()
            .map(|project| (project.id, project.name))
            .collect();
        let mut items = Vec::with_capacity(messages.len());
        for message in messages {
            items.push(overview::feed_item(db, message, &names).await?);
        }
        Ok(items)
    }
}

#[async_trait]
impl HubApi for ClientApi {
    async fn update_hub(
        &self,
        request: threadlane_core::client::HubUpdate,
    ) -> Result<Option<String>, ApiError> {
        let updates = self.updates.as_ref().ok_or_else(|| {
            ApiError::Invalid("Install the official hub package to enable in-app updates".into())
        })?;
        updates.request(request).await
    }

    async fn info(&self) -> Result<HubInfo, ApiError> {
        let db = &self.core.inner.db;
        let space = db.space().await.map_err(api_error)?;
        Ok(HubInfo {
            space: space.name,
            mode_ceiling: space.mode_ceiling,
            paused: space.paused,
            schema_version: db.schema_version().await.map_err(api_error)?,
            update_version: self
                .updates
                .as_ref()
                .map(|_| env!("CARGO_PKG_VERSION").to_owned()),
            default_models: self
                .core
                .inner
                .settings
                .default_models
                .iter()
                .map(|(backend, model)| (*backend, model.clone()))
                .collect(),
        })
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        self.project_list(false).await
    }

    async fn project_list(&self, include_archived: bool) -> Result<Vec<ProjectSummary>, ApiError> {
        let projects = self.core.inner.db.projects().await.map_err(api_error)?;
        Ok(projects
            .into_iter()
            .filter(|project| include_archived || project.archived_at.is_none())
            .map(summary)
            .collect())
    }

    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError> {
        if message.text.trim().is_empty() && message.images.is_empty() {
            return Err(ApiError::Invalid("the message is empty".into()));
        }
        if message.client_message_id.trim().is_empty() {
            return Err(ApiError::Invalid("the client message id is empty".into()));
        }
        if !message.images.is_empty() {
            if message.task.is_some() {
                return Err(ApiError::Invalid(
                    "Attach images in a conversation with a lead.".into(),
                ));
            }
            let images = message.images.clone();
            tokio::task::spawn_blocking(move || {
                threadlane_core::attachment::validate_images(&images)
            })
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?
            .map_err(ApiError::Invalid)?;
        }
        let inner = &self.core.inner;
        // A message to a task goes to its lane; one there declines the
        // decision open in it (Plan.md §9.4, `DecisionStore`).
        let (project_id, lane_id) = match message.task {
            Some(task) => {
                let task = inner
                    .db
                    .task(task)
                    .await
                    .map_err(api_error)?
                    .ok_or_else(|| ApiError::NotFound(format!("task {task}")))?;
                if let Some(project) = inner.db.project(task.project_id).await.map_err(api_error)? {
                    active_project(&project)?;
                }
                if task.status.is_terminal() {
                    return Err(ApiError::Invalid(format!(
                        "task {} has ended ({}); tell the lead instead",
                        task.id, task.status
                    )));
                }
                (task.project_id, task.lane_id)
            }
            None => {
                let project = self.project_named(message.project.as_deref()).await?;
                active_project(&project)?;
                self.lead_is_on(&project).await?;
                (project.id, project.lead_lane_id)
            }
        };
        let mut payload = json!({ "text": message.text });
        if !message.images.is_empty() {
            payload["images"] = serde_json::to_value(&message.images)
                .map_err(|error| ApiError::Internal(error.to_string()))?;
        }
        let inserted = inner
            .db
            .insert_event(&NewEvent {
                project_id,
                lane_id,
                source: CLIENT_SOURCE.to_owned(),
                source_event_id: message.client_message_id.clone(),
                kind: EventKind::UserMessage,
                payload,
                trust: Trust::User,
                requested: true,
                session_ref: None,
                decision_ref: None,
                reply_to_ref: None,
                received_at: inner.clock.now(),
            })
            .await
            .map_err(project_error)?;
        match inserted {
            Inserted::New(event_id) => {
                inner.wake.wake(WakeTarget::Lane(lane_id));
                Ok(Accepted {
                    event_id,
                    project_id,
                    duplicate: false,
                })
            }
            Inserted::Duplicate(event_id) => {
                // Report where the original went, whatever this retry named.
                let project_id = inner
                    .db
                    .event(event_id)
                    .await
                    .map_err(api_error)?
                    .map_or(project_id, |event| event.project_id);
                Ok(Accepted {
                    event_id,
                    project_id,
                    duplicate: true,
                })
            }
        }
    }

    async fn feed(&self, after: i64, wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        let clock = &self.core.inner.clock;
        let deadline = clock.now() + wait;
        let mut deliveries = self.core.inner.wake.deliveries();
        loop {
            // Mark the current delivery count seen before reading, so a
            // delivery between the read and the wait is not missed.
            deliveries.borrow_and_update();
            let items = self.read_feed(after).await?;
            if !items.is_empty() || clock.now() >= deadline {
                return Ok(items);
            }
            tokio::select! {
                changed = deliveries.changed() => {
                    if changed.is_err() {
                        // The hub is gone; wait out the rest on the clock.
                        clock.sleep_until(deadline).await;
                    }
                }
                () = clock.sleep_until(deadline) => return Ok(Vec::new()),
            }
        }
    }

    async fn org(&self, request: OrgRequest) -> Result<OrgReply, ApiError> {
        let db = &self.core.inner.db;
        match request {
            OrgRequest::CreateProject { name, description } => {
                let name = required("project name", &name)?;
                let project = db
                    .create_project(&NewProject {
                        name: name.to_owned(),
                        description,
                        ..NewProject::default()
                    })
                    .await
                    .map_err(|error| taken(error, "project", name))?;
                // Give the new project its lead binding now, not at next start.
                ensure_defaults(db).await.map_err(api_error)?;
                self.core.ensure_dreaming().await.map_err(api_error)?;
                Ok(OrgReply::Project {
                    project: summary(project),
                })
            }
            OrgRequest::AddRepository {
                project,
                name,
                remote_url,
                default_branch,
            } => {
                let project = self.project_named(project.as_deref()).await?;
                let name = required("repository name", &name)?;
                if !valid_repository_name(name) {
                    return Err(ApiError::Invalid(format!(
                        "{name:?} is not a valid repository name: use letters, digits, '.', \
                         '_' and '-', starting with a letter or digit"
                    )));
                }
                let remote_url = required("remote", &remote_url)?;
                if remote_url.starts_with('-') {
                    return Err(ApiError::Invalid(format!("{remote_url:?} is not a remote")));
                }
                let default_branch = default_branch
                    .as_deref()
                    .map(str::trim)
                    .filter(|branch| !branch.is_empty())
                    .unwrap_or(DEFAULT_BRANCH);
                if !valid_branch_name(default_branch) {
                    return Err(ApiError::Invalid(format!(
                        "{default_branch:?} is not a valid branch name"
                    )));
                }
                // One repository is one registration, whatever the spelling of
                // its remote or which branch a task wants of it.
                let identity = repository_identity(remote_url).await;
                for owner in db.projects().await.map_err(api_error)? {
                    for existing in db.repositories(owner.id).await.map_err(api_error)? {
                        if repository_identity(&existing.remote_url).await == identity {
                            return Err(ApiError::Invalid(format!(
                                "the remote {remote_url} is already registered in this space, \
                                 as repository {:?} of project {:?}",
                                existing.name, owner.name
                            )));
                        }
                    }
                }
                let repository = db
                    .create_repository(&NewRepository {
                        project_id: project.id,
                        name: name.to_owned(),
                        remote_url: remote_url.to_owned(),
                        default_branch: default_branch.to_owned(),
                        protected_branches: Vec::new(),
                    })
                    .await
                    .map_err(|error| match error {
                        StoreError::Conflict(_) => ApiError::Invalid(format!(
                            "project {:?} already has a repository named {name:?}",
                            project.name
                        )),
                        other => api_error(other),
                    })?;
                Ok(OrgReply::Repository { repository })
            }
            OrgRequest::Repositories { project } => {
                let project = self.project_named(project.as_deref()).await?;
                Ok(OrgReply::Repositories {
                    repositories: db.repositories(project.id).await.map_err(api_error)?,
                })
            }
            OrgRequest::InstalledSkills => {
                let (mut skills, mut unavailable) = (Vec::new(), Vec::new());
                for backend in &self.core.inner.backends {
                    match backend.installed_skills(None).await {
                        Ok(names) => skills.extend(names.into_iter().map(|name| {
                            threadlane_core::model::SkillBinding {
                                backend: backend.kind(),
                                name,
                            }
                        })),
                        Err(error) => {
                            tracing::warn!(backend = %backend.kind(), %error, "could not list skills");
                            unavailable.push(backend.kind());
                        }
                    }
                }
                Ok(OrgReply::InstalledSkills {
                    skills,
                    unavailable,
                })
            }
            OrgRequest::Agents => Ok(OrgReply::Agents {
                agents: db.agents().await.map_err(api_error)?,
            }),
            OrgRequest::CreateAgent {
                name,
                role,
                instructions,
                backends,
            } => {
                self.org(OrgRequest::CreateAgentWithModels {
                    name,
                    role,
                    instructions,
                    backends,
                    models: Default::default(),
                })
                .await
            }
            OrgRequest::CreateAgentWithModels {
                name,
                role,
                instructions,
                backends,
                models,
            } => {
                self.org(OrgRequest::CreateAgentWithSkills {
                    name,
                    role,
                    instructions,
                    backends,
                    models,
                    skills: Vec::new(),
                })
                .await
            }
            OrgRequest::CreateAgentWithSkills {
                name,
                role,
                instructions,
                backends,
                models,
                skills,
            } => {
                validate_skills(&self.core.inner.backends, &skills).await?;
                let name = required("agent name", &name)?;
                let role = required("agent role", &role)?;
                let mut preferred = Vec::with_capacity(backends.len());
                for backend in backends {
                    if !preferred.contains(&backend) {
                        preferred.push(backend);
                    }
                }
                let mut overrides = serde_json::Map::new();
                for (backend, model) in models {
                    let backend = known_backend(&backend)?;
                    if !model.trim().is_empty() {
                        overrides.insert(backend.as_str().to_owned(), json!(model.trim()));
                    }
                }
                let agent = db
                    .create_agent(&NewAgent {
                        models: overrides.into(),
                        skills,
                        instructions,
                        backends: preferred,
                        ..NewAgent::named(name, role)
                    })
                    .await
                    .map_err(|error| taken(error, "agent", name))?;
                Ok(OrgReply::Agent { agent })
            }
            OrgRequest::UpdateAgent {
                skills,
                agent,
                name,
                role,
                instructions,
                backends,
                models,
            } => {
                let name = name
                    .as_deref()
                    .map(|value| required("agent name", value))
                    .transpose()?;
                let role = role
                    .as_deref()
                    .map(|value| required("agent role", value))
                    .transpose()?;
                if let Some(instructions) = &instructions
                    && instructions.len() > INSTRUCTIONS_BUDGET
                {
                    return Err(ApiError::Invalid(format!(
                        "the instructions are {} bytes, over their budget of {INSTRUCTIONS_BUDGET}",
                        instructions.len()
                    )));
                }
                let backends = backends
                    .map(|backends| {
                        let mut preferred = Vec::with_capacity(backends.len());
                        for backend in backends {
                            let backend = known_backend(&backend)?;
                            if !preferred.contains(&backend) {
                                preferred.push(backend);
                            }
                        }
                        Ok::<_, ApiError>(preferred)
                    })
                    .transpose()?;
                let models = models
                    .map(|models| {
                        models
                            .into_iter()
                            .map(|(backend, model)| {
                                Ok((known_backend(&backend)?.as_str().to_owned(), model))
                            })
                            .collect::<Result<Vec<_>, ApiError>>()
                    })
                    .transpose()?;
                if let Some(skills) = &skills {
                    validate_skills(&self.core.inner.backends, skills).await?;
                }
                let agent_name = agent.trim();
                let target_name = name.unwrap_or(agent_name);
                let agent = db
                    .patch_agent(agent_name, |current| {
                        if current.role == LEAD_ROLE && role.is_some_and(|role| role != LEAD_ROLE) {
                            return Err(StoreError::Invalid(format!(
                                "the lead's role stays {LEAD_ROLE:?}"
                            )));
                        }
                        if let Some(name) = name {
                            name.clone_into(&mut current.name);
                        }
                        if let Some(role) = role {
                            role.clone_into(&mut current.role);
                        }
                        if let Some(skills) = skills {
                            current.skills = skills;
                        }
                        if let Some(instructions) = instructions {
                            current.instructions = instructions;
                        }
                        if let Some(backends) = backends {
                            current.backends = backends;
                        }
                        if let Some(models) = models {
                            if !current.models.is_object() {
                                current.models = json!({});
                            }
                            if let Some(map) = current.models.as_object_mut() {
                                for (backend, model) in models {
                                    let model = model.trim();
                                    if model.is_empty() {
                                        map.remove(&backend);
                                    } else {
                                        map.insert(backend, json!(model));
                                    }
                                }
                            }
                        }
                        Ok(())
                    })
                    .await
                    .map_err(|error| match error {
                        StoreError::Invalid(message) => ApiError::Invalid(message),
                        StoreError::NotFound { .. } => {
                            ApiError::NotFound(format!("agent {agent_name:?}"))
                        }
                        other => taken(other, "agent", target_name),
                    })?;
                Ok(OrgReply::Agent { agent })
            }
            OrgRequest::DeleteAgent {
                agent,
                confirm_name,
            } => {
                let agent = self.agent_named(&agent).await?;
                if confirm_name.trim() != agent.name {
                    return Err(ApiError::Invalid(format!(
                        "type the agent's name, {:?}, to delete it",
                        agent.name
                    )));
                }
                if agent.role == LEAD_ROLE {
                    return Err(ApiError::Invalid(format!(
                        "{} is the lead and cannot be deleted",
                        agent.name
                    )));
                }
                db.delete_agent(agent.id).await.map_err(project_error)?;
                Ok(OrgReply::AgentDeleted { agent: agent.name })
            }
            OrgRequest::Bindings { project } => {
                let project = self.project_named(project.as_deref()).await?;
                let raw_bindings = db.bindings(project.id).await.map_err(api_error)?;
                let all_agents = db.all_agents().await.map_err(api_error)?;
                let agents: HashMap<_, _> = all_agents
                    .into_iter()
                    .map(|agent| (agent.id, agent))
                    .collect();
                let mut bindings = Vec::with_capacity(raw_bindings.len());
                for binding in raw_bindings {
                    let agent = agents.get(&binding.agent_id).cloned().ok_or_else(|| {
                        ApiError::Internal(format!("agent {} not found", binding.agent_id))
                    })?;
                    bindings.push(binding_summary(agent, &binding));
                }
                Ok(OrgReply::Bindings { bindings })
            }
            OrgRequest::SetMode {
                project,
                agent,
                mode,
            } => {
                let project = self.project_named(project.as_deref()).await?;
                let agent = self.agent_named(&agent).await?;
                let existing = db
                    .bindings(project.id)
                    .await
                    .map_err(api_error)?
                    .into_iter()
                    .find(|binding| binding.agent_id == agent.id);
                let ceiling = db.effective_ceiling(project.id).await.map_err(api_error)?;
                if let Some(mode) = mode.filter(|&mode| mode > ceiling) {
                    return Err(ApiError::Invalid(format!(
                        "{mode} is above the ceiling of project {:?}, {ceiling}",
                        project.name
                    )));
                }
                let binding = match existing {
                    Some(binding) => db.set_binding_mode(binding.id, mode).await,
                    None => {
                        db.bind(&NewBinding {
                            project_id: project.id,
                            agent_id: agent.id,
                            mode,
                            is_lead: false,
                            max_parallel: 1,
                        })
                        .await
                    }
                }
                .map_err(api_error)?;
                Ok(OrgReply::Binding {
                    binding: binding_summary(agent, &binding),
                })
            }
            OrgRequest::SetPaused { project, paused } => {
                let project = self.project_named(project.as_deref()).await?;
                db.set_project_paused(project.id, paused)
                    .await
                    .map_err(api_error)?;
                if !paused {
                    // Its lanes may be granted again.
                    self.core.inner.wake.wake(WakeTarget::Runners);
                }
                Ok(OrgReply::Project {
                    project: summary(Project { paused, ..project }),
                })
            }
            OrgRequest::SetCeiling { project, mode } => {
                let project = self.project_named(project.as_deref()).await?;
                let space = db.space().await.map_err(api_error)?.mode_ceiling;
                if mode > space {
                    return Err(ApiError::Invalid(format!(
                        "{mode} is above the space's ceiling, {space}"
                    )));
                }
                db.set_project_ceiling(project.id, Some(mode))
                    .await
                    .map_err(api_error)?;
                Ok(OrgReply::Ceiling {
                    ceiling: db.effective_ceiling(project.id).await.map_err(api_error)?,
                    project: project.name,
                })
            }
            OrgRequest::SetOverride {
                project,
                agent,
                tier,
                cell,
            } => {
                let project = self.project_named(project.as_deref()).await?;
                let agent = self.agent_named(&agent).await?;
                let binding = db
                    .bindings(project.id)
                    .await
                    .map_err(api_error)?
                    .into_iter()
                    .find(|binding| binding.agent_id == agent.id)
                    .ok_or_else(|| {
                        ApiError::NotFound(format!(
                            "a binding of {:?} in project {:?}",
                            agent.name, project.name
                        ))
                    })?;
                let mut overrides = gate::overrides(&binding.overrides);
                match cell {
                    Some(cell) => {
                        let ceiling = db.effective_ceiling(project.id).await.map_err(api_error)?;
                        let loosest = gate::loosest_override(tier, ceiling);
                        if cell < loosest {
                            return Err(ApiError::Invalid(format!(
                                "{cell} is looser than project {:?}'s ceiling, {ceiling}, \
                                 allows for {tier:?} tools: at most {loosest}",
                                project.name
                            )));
                        }
                        overrides.insert(tier, cell);
                    }
                    None => {
                        overrides.remove(&tier);
                    }
                }
                let stored = serde_json::to_value(&overrides)
                    .map_err(|error| ApiError::Internal(error.to_string()))?;
                db.set_binding_overrides(binding.id, &stored)
                    .await
                    .map_err(api_error)?;
                Ok(OrgReply::Overrides {
                    agent: agent.name,
                    overrides,
                })
            }
            OrgRequest::SetMachineSlots { machine, slots } => {
                db.set_machine_slots(machine, slots)
                    .await
                    .map_err(api_error)?;
                self.core.inner.wake.wake(WakeTarget::Runners);
                Ok(OrgReply::MachineSlotsSet)
            }
            OrgRequest::SetMaxParallel { .. } => Err(ApiError::Invalid(
                "Set runner.max_parallel in the machine configuration and restart its runner"
                    .into(),
            )),
            OrgRequest::SetSpaceCeiling { mode } => {
                let _dispatch = self.core.inner.dispatch.lock().await;
                let mut space = db.space().await.map_err(api_error)?;
                space.mode_ceiling = mode;
                db.update_space(&space).await.map_err(api_error)?;
                Ok(OrgReply::SpaceCeiling { ceiling: mode })
            }
            OrgRequest::SetSpacePaused { paused } => {
                let _dispatch = self.core.inner.dispatch.lock().await;
                let mut space = db.space().await.map_err(api_error)?;
                space.paused = paused;
                db.update_space(&space).await.map_err(api_error)?;
                if !paused {
                    // The queued lanes may be granted again.
                    self.core.inner.wake.wake(WakeTarget::Runners);
                }
                Ok(OrgReply::SpacePaused { paused })
            }
            OrgRequest::Pacing => Ok(OrgReply::Pacing {
                pacing: db.pacing().await.map_err(api_error)?,
            }),
            OrgRequest::SetPacing { pacing } => {
                crate::pacing::check(&pacing).map_err(ApiError::Invalid)?;
                let inner = &self.core.inner;
                let _dispatch = inner.dispatch.lock().await;
                db.set_pacing(&pacing).await.map_err(api_error)?;
                inner.wake.wake(WakeTarget::Dispatcher);
                Ok(OrgReply::Pacing { pacing })
            }
            OrgRequest::Stop {
                target,
                client_request_id,
            } => {
                let request_id = required("client request id", &client_request_id)?;
                let lane_id = self.target_lane(target).await?;
                let inner = &self.core.inner;
                let epoch = db
                    .stop_lane(lane_id, request_id.to_owned(), inner.clock.now())
                    .await
                    .map_err(api_error)?;
                inner
                    .hook
                    .reach(threadlane_core::runtime::steps::stop::AFTER_RECORD)
                    .map_err(|error| ApiError::Internal(error.to_string()))?;
                if let Some(epoch) = epoch {
                    inner
                        .wake
                        .push(threadlane_core::link::HubMessage::Cancel { lane_id, epoch });
                }
                Ok(OrgReply::LaneUpdated)
            }
            OrgRequest::ResumeLane { target } => {
                let lane_id = self.target_lane(target).await?;
                db.resume_lane(lane_id).await.map_err(api_error)?;
                self.core.inner.wake.wake(WakeTarget::Lane(lane_id));
                Ok(OrgReply::LaneUpdated)
            }
            OrgRequest::SetArchived { project, archived } => {
                let project = self.project_named(project.as_deref()).await?;
                let project = db
                    .set_project_archived(project.id, archived)
                    .await
                    .map_err(project_error)?;
                if !archived {
                    self.core.inner.wake.wake(WakeTarget::Runners);
                }
                Ok(OrgReply::Project {
                    project: summary(project),
                })
            }
            OrgRequest::DeletePreview { project } => {
                let (mut preview, removal) = db
                    .project_deletion_preview(&project)
                    .await
                    .map_err(project_error)?;
                if let Some(files) = &self.core.inner.workspace_files {
                    let counts = files.removal_counts(&removal).await;
                    preview.worktrees = counts.worktrees;
                    preview.clones = counts.clones;
                }
                Ok(OrgReply::DeletePreview { preview })
            }
            OrgRequest::DeleteProject {
                project,
                confirm_name,
            } => {
                let inner = &self.core.inner;
                let _removal = inner.project_removal.lock().await;
                let (id, mut deleted, removal) = {
                    let _dispatch = inner.dispatch.lock().await;
                    db.delete_project(&project, &confirm_name, inner.clock.now())
                        .await
                        .map_err(project_error)?
                };
                inner
                    .hook
                    .reach(threadlane_core::runtime::steps::project_delete::AFTER_COMMIT)
                    .map_err(|error| ApiError::Internal(error.to_string()))?;
                inner.wake.wake(WakeTarget::Dispatcher);
                if let Some(files) = &inner.workspace_files {
                    let counts = files.removal_counts(&removal).await;
                    deleted.worktrees = counts.worktrees;
                    deleted.clones = counts.clones;
                }
                let leftovers = self.core.cleanup_project(id, &removal).await;
                if !leftovers.is_empty()
                    && let Some(files) = &inner.workspace_files
                {
                    let remaining = files.removal_counts(&removal).await;
                    deleted.worktrees = deleted.worktrees.saturating_sub(remaining.worktrees);
                    deleted.clones = deleted.clones.saturating_sub(remaining.clones);
                }
                Ok(OrgReply::ProjectDeleted {
                    project,
                    deleted,
                    leftovers,
                })
            }
            OrgRequest::Templates => Ok(OrgReply::Templates {
                templates: crate::templates::all().map_err(ApiError::Internal)?,
            }),
            other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
        }
    }

    async fn decisions(&self, request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        let inner = &self.core.inner;
        let db = &inner.db;
        match request {
            DecisionRequest::Pending => {
                let mut decisions = db.open_decisions().await.map_err(api_error)?;
                for decision in &mut decisions {
                    if decision.kind == threadlane_core::model::DecisionKind::Proposal {
                        decision.proposed_by = crate::proposal::author(db, decision)
                            .await
                            .map_err(api_error)?;
                    }
                }
                Ok(DecisionReply::Pending { decisions })
            }
            DecisionRequest::Answer {
                decision,
                option,
                note,
            } => self.answer(decision, &option, note, false).await,
            DecisionRequest::AnswerAlways {
                decision,
                option,
                note,
            } => self.answer(decision, &option, note, true).await,
            DecisionRequest::StandingRules { project } => {
                let project = match project {
                    Some(name) => Some(self.project_named(Some(&name)).await?.id),
                    None => None,
                };
                let raw_rules = db.standing_rules(project).await.map_err(api_error)?;
                let (all_projects, all_agents) =
                    tokio::try_join!(db.projects(), db.all_agents()).map_err(api_error)?;
                let projects: HashMap<_, _> =
                    all_projects.into_iter().map(|p| (p.id, p.name)).collect();
                let agents: HashMap<_, _> =
                    all_agents.into_iter().map(|a| (a.id, a.name)).collect();
                let rules = raw_rules
                    .into_iter()
                    .map(|rule| {
                        let project = projects
                            .get(&rule.project_id)
                            .cloned()
                            .unwrap_or_else(|| rule.project_id.to_string());
                        let agent = agents
                            .get(&rule.agent_id)
                            .cloned()
                            .unwrap_or_else(|| rule.agent_id.to_string());
                        StandingRuleSummary {
                            rule,
                            project,
                            agent,
                        }
                    })
                    .collect();
                Ok(DecisionReply::StandingRules { rules })
            }
            DecisionRequest::RevokeRule { rule } => {
                let rule = db
                    .revoke_standing_rule(rule, inner.clock.now())
                    .await
                    .map_err(|error| match error {
                        StoreError::NotFound { .. } => {
                            ApiError::NotFound(format!("standing rule {rule}"))
                        }
                        other => api_error(other),
                    })?;
                Ok(DecisionReply::StandingRule {
                    rule: self.rule_summary(rule).await?,
                })
            }
            DecisionRequest::Remind { decision } => Ok(DecisionReply::Reminder {
                first: db
                    .claim_reminder(decision, inner.clock.now())
                    .await
                    .map_err(api_error)?,
            }),
            other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
        }
    }

    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError> {
        crate::pairing::handle(&self.core, request).await
    }

    async fn overview(&self, request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        overview::handle(self, request).await
    }

    async fn memory(&self, request: MemoryRequest) -> Result<MemoryReply, ApiError> {
        memory::handle(self, request).await
    }

    async fn timers(&self, request: TimerRequest) -> Result<TimerReply, ApiError> {
        crate::timers::handle(&self.core, request).await
    }

    async fn tasks(&self, request: TaskRequest) -> Result<TaskReply, ApiError> {
        tasks::handle(self, request).await
    }

    async fn operations(
        &self,
        request: threadlane_core::client::OperationsRequest,
    ) -> Result<threadlane_core::client::OperationsReply, ApiError> {
        operations::handle(self, request).await
    }
}

fn project_error(error: StoreError) -> ApiError {
    match error {
        StoreError::Invalid(message) => ApiError::Invalid(message),
        StoreError::NotFound { kind, id } => ApiError::NotFound(format!("{kind} {id}")),
        other => api_error(other),
    }
}

fn active_project(project: &Project) -> Result<(), ApiError> {
    if project.archived_at.is_some() {
        return Err(ApiError::Invalid(format!(
            "Project {:?} is archived.",
            project.name
        )));
    }
    Ok(())
}

fn summary(project: Project) -> ProjectSummary {
    ProjectSummary {
        id: project.id,
        name: project.name,
        is_general: project.is_general,
        paused: project.paused,
        mode_ceiling: project.mode_ceiling,
        archived: project.archived_at.is_some(),
    }
}

/// A binding of `agent` as a client lists it.
fn binding_summary(agent: Agent, binding: &Binding) -> BindingSummary {
    BindingSummary {
        agent: agent.name,
        role: agent.role,
        mode: binding.mode,
        is_lead: binding.is_lead,
        max_parallel: binding.max_parallel,
        overrides: gate::overrides(&binding.overrides),
    }
}

async fn validate_skills(
    backends: &[std::sync::Arc<dyn threadlane_core::backend::AgentBackend>],
    skills: &[threadlane_core::model::SkillBinding],
) -> Result<(), ApiError> {
    for (i, skill) in skills.iter().enumerate() {
        if !threadlane_core::model::SkillBinding::valid_name(&skill.name)
            || skills[..i].contains(skill)
        {
            return Err(ApiError::Invalid(format!(
                "invalid or duplicate {} skill {:?}",
                skill.backend, skill.name
            )));
        }
        for backend in backends.iter().filter(|b| b.kind() == skill.backend) {
            if let Some(reason) = backend.unusable_skill(&skill.name).await {
                return Err(ApiError::Invalid(format!(
                    "{} skill {:?} {reason}",
                    skill.backend, skill.name
                )));
            }
        }
    }
    Ok(())
}

fn known_backend(name: &str) -> Result<BackendKind, ApiError> {
    name.trim().parse().map_err(|_| {
        let known: Vec<&str> = BackendKind::ALL.iter().map(|b| b.as_str()).collect();
        ApiError::Invalid(format!(
            "{name:?} is not a backend; use {}",
            known.join(" or ")
        ))
    })
}

fn required<'a>(what: &str, value: &'a str) -> Result<&'a str, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApiError::Invalid(format!("the {what} is empty")));
    }
    Ok(value)
}

/// A uniqueness conflict on a name as the client sees it (the schema keeps
/// project and agent names unique).
fn taken(error: StoreError, kind: &str, name: &str) -> ApiError {
    match error {
        StoreError::Conflict(_) => {
            ApiError::Invalid(format!("a {kind} named {name:?} already exists"))
        }
        other => api_error(other),
    }
}

impl ClientApi {
    async fn target_lane(
        &self,
        target: threadlane_core::client::LaneTarget,
    ) -> Result<threadlane_core::ids::LaneId, ApiError> {
        use threadlane_core::client::LaneTarget;
        match target {
            LaneTarget::Lead { project } => {
                Ok(self.project_named(project.as_deref()).await?.lead_lane_id)
            }
            LaneTarget::Task { task } => {
                let task = self
                    .core
                    .inner
                    .db
                    .task(task)
                    .await
                    .map_err(api_error)?
                    .ok_or_else(|| ApiError::NotFound(format!("task {task}")))?;
                if task.status.is_terminal() {
                    return Err(ApiError::Invalid(format!(
                        "task {} has ended ({})",
                        task.id, task.status
                    )));
                }
                Ok(task.lane_id)
            }
        }
    }
}

mod operations;

/// What a remote names, so one repository matches under any spelling: its
/// [`normalize_remote`] address; for a local folder, its `origin`'s address,
/// else its canonical path; else the remote as typed.
async fn repository_identity(remote: &str) -> String {
    let remote = remote.trim();
    if let Some(address) = normalize_remote(remote) {
        return address;
    }
    let folder = remote.strip_prefix("file://").unwrap_or(remote);
    let Ok(folder) = tokio::fs::canonicalize(folder).await else {
        return remote.to_owned();
    };
    let origin = tokio::process::Command::new("git")
        .arg("-C")
        .arg(&folder)
        .args(["config", "--get", "remote.origin.url"])
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    origin
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| normalize_remote(String::from_utf8_lossy(&output.stdout).trim()))
        .unwrap_or_else(|| folder.to_string_lossy().into_owned())
}
