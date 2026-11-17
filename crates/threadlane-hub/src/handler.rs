//! The client interface: handler (Plan.md §6.3), feed, info, projects, and
//! the organization (Plan.md §11.2).

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use threadlane_core::client::{
    Accepted, ApiError, BindingSummary, DecisionReply, DecisionRequest, FeedItem, HubApi, HubInfo,
    OrgReply, OrgRequest, PairingReply, PairingRequest, ProjectSummary, UserMessage,
};
use threadlane_core::ids::ProjectId;
use threadlane_core::model::{
    Agent, Answered, Binding, DecisionAnswer, DecisionStatus, EventKind, Inserted, Mode, NewAgent,
    NewBinding, NewEvent, NewProject, NewRepository, Project, Trust,
};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{DecisionStore, InboxStore, OrgStore, OutboxStore, StoreError};
use threadlane_core::workspace::{valid_branch_name, valid_repository_name};

use crate::core::HubCore;
use crate::defaults::ensure_defaults;
use crate::error::api_error;
use crate::tools::CLIENT_CHANNEL;

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
}

impl ClientApi {
    pub(crate) fn new(core: HubCore) -> Self {
        Self { core }
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

    async fn binding_summary(&self, binding: Binding) -> Result<BindingSummary, ApiError> {
        let agent = self
            .core
            .inner
            .db
            .agent(binding.agent_id)
            .await
            .map_err(api_error)?
            .ok_or_else(|| ApiError::Internal(format!("agent {} not found", binding.agent_id)))?;
        Ok(BindingSummary {
            agent: agent.name,
            role: agent.role,
            mode: binding.mode,
            is_lead: binding.is_lead,
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
        messages
            .into_iter()
            .map(|message| {
                let (Some(seq), Some(delivered_at)) = (message.delivery_seq, message.sent_at)
                else {
                    return Err(ApiError::Internal(format!(
                        "sent message {} has no delivery position",
                        message.id
                    )));
                };
                Ok(FeedItem {
                    seq,
                    project_name: message.project_id.and_then(|id| names.get(&id).cloned()),
                    delivered_at,
                    message,
                })
            })
            .collect()
    }
}

#[async_trait]
impl HubApi for ClientApi {
    async fn info(&self) -> Result<HubInfo, ApiError> {
        let db = &self.core.inner.db;
        let space = db.space().await.map_err(api_error)?;
        Ok(HubInfo {
            space: space.name,
            mode_ceiling: space.mode_ceiling,
            schema_version: db.schema_version().await.map_err(api_error)?,
        })
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        let projects = self.core.inner.db.projects().await.map_err(api_error)?;
        Ok(projects.into_iter().map(summary).collect())
    }

    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError> {
        if message.text.trim().is_empty() {
            return Err(ApiError::Invalid("the message is empty".into()));
        }
        if message.client_message_id.trim().is_empty() {
            return Err(ApiError::Invalid("the client message id is empty".into()));
        }
        let project = self.project_named(message.project.as_deref()).await?;
        let inner = &self.core.inner;
        // An Off lead never runs (invariant 14): refuse rather than leave the
        // client waiting for a reply that cannot come.
        if let Some(lead) = inner.db.lead_binding(project.id).await.map_err(api_error)? {
            let ceiling = inner
                .db
                .effective_ceiling(project.id)
                .await
                .map_err(api_error)?;
            if lead.mode.stricter(ceiling) == Mode::Off {
                return Err(ApiError::Invalid(format!(
                    "the lead of project {:?} is off",
                    project.name
                )));
            }
        }
        let inserted = inner
            .db
            .insert_event(&NewEvent {
                project_id: project.id,
                lane_id: project.lead_lane_id,
                source: CLIENT_SOURCE.to_owned(),
                source_event_id: message.client_message_id.clone(),
                kind: EventKind::UserMessage,
                payload: json!({ "text": message.text }),
                trust: Trust::User,
                requested: true,
                session_ref: None,
                decision_ref: None,
                reply_to_ref: None,
                received_at: inner.clock.now(),
            })
            .await
            .map_err(api_error)?;
        match inserted {
            Inserted::New(event_id) => {
                inner.wake.wake(WakeTarget::Lane(project.lead_lane_id));
                Ok(Accepted {
                    event_id,
                    project_id: project.id,
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
                    .map_or(project.id, |event| event.project_id);
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
                () = clock.sleep_until(deadline) => {}
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
                if let Some(existing) = db
                    .repository_by_remote(remote_url)
                    .await
                    .map_err(api_error)?
                {
                    let owner = db
                        .project(existing.project_id)
                        .await
                        .map_err(api_error)?
                        .map_or_else(|| existing.project_id.to_string(), |project| project.name);
                    return Err(ApiError::Invalid(format!(
                        "the remote {remote_url} is already registered in this space, as \
                         repository {:?} of project {owner:?}",
                        existing.name
                    )));
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
            OrgRequest::Agents => Ok(OrgReply::Agents {
                agents: db.agents().await.map_err(api_error)?,
            }),
            OrgRequest::CreateAgent {
                name,
                role,
                instructions,
            } => {
                let name = required("agent name", &name)?;
                let role = required("agent role", &role)?;
                let agent = db
                    .create_agent(&NewAgent {
                        instructions,
                        ..NewAgent::named(name, role)
                    })
                    .await
                    .map_err(|error| taken(error, "agent", name))?;
                Ok(OrgReply::Agent { agent })
            }
            OrgRequest::Bindings { project } => {
                let project = self.project_named(project.as_deref()).await?;
                let mut bindings = Vec::new();
                for binding in db.bindings(project.id).await.map_err(api_error)? {
                    bindings.push(self.binding_summary(binding).await?);
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
                // Until milestone 9 a mode above On request is stored as given
                // and behaves as On request.
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
                    binding: BindingSummary {
                        agent: agent.name,
                        role: agent.role,
                        mode: binding.mode,
                        is_lead: binding.is_lead,
                    },
                })
            }
            other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
        }
    }

    async fn decisions(&self, request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        let inner = &self.core.inner;
        let db = &inner.db;
        match request {
            DecisionRequest::Pending => Ok(DecisionReply::Pending {
                decisions: db.open_decisions().await.map_err(api_error)?,
            }),
            DecisionRequest::Answer {
                decision,
                option,
                note,
            } => {
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
                let answered = db
                    .answer(
                        decision,
                        &DecisionAnswer {
                            option: u32::try_from(index).unwrap_or(u32::MAX),
                            note: note.filter(|note| !note.trim().is_empty()),
                            standing_rule: false,
                            answered_at: inner.clock.now(),
                        },
                    )
                    .await
                    .map_err(api_error)?;
                let recorded = matches!(answered, Answered::Recorded(_));
                if recorded {
                    inner.wake.wake(WakeTarget::Lane(current.lane_id));
                }
                let decision = db
                    .decision(decision)
                    .await
                    .map_err(api_error)?
                    .ok_or_else(|| ApiError::NotFound(format!("decision {decision}")))?;
                Ok(DecisionReply::Answered {
                    decision: Box::new(decision),
                    recorded,
                })
            }
            other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
        }
    }

    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError> {
        crate::pairing::handle(&self.core, request).await
    }
}

fn summary(project: Project) -> ProjectSummary {
    ProjectSummary {
        id: project.id,
        name: project.name,
        is_general: project.is_general,
        paused: project.paused,
    }
}

/// `value` trimmed, or an error naming `what` if that leaves nothing.
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
