use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;
use threadlane_client::ClientDirs;
use threadlane_core::client::{
    Accepted, ApiError, DecisionReply, DecisionRequest, FeedItem, HubApi, HubInfo, MemoryReply,
    MemoryRequest, OrgReply, OrgRequest, OverviewReply, OverviewRequest, PairingReply,
    PairingRequest, RecordedEvent, RunStep, RunSummary, RunUsage, StandingRuleSummary, TaskReply,
    TaskRequest, TimerReply, TimerRequest, UserMessage,
};
use threadlane_core::ids::{
    AgentId, LaneId, MachineId, OutboxId, RepositoryId, RunId, SessionId, TimerId,
};
use threadlane_core::model::{
    BackendKind, DecisionKind, DecisionOption, DecisionStatus, EventKind, MachineTrust, MemoryFile,
    Mode, OutboxMessage, OutboxStatus, Recurrence, Repository, Rule, Run, RunOutcome, RunStatus,
    Task, TaskOrigin, TaskStatus, Timer, TimerCategory, TimerCreator, TimerStatus, Urgency,
    Weekday,
};
use threadlane_core::time::Timestamp;
use tokio::sync::oneshot;

use super::*;

type BlockedInfo = (oneshot::Sender<()>, oneshot::Receiver<()>);

/// A hub with the projects `api` and General, and what it was given.
/// While `down`, it answers nothing; while `old`, it answers like a hub
/// from before the Overview capability.
#[derive(Debug, Default)]
struct FakeHub {
    space: String,
    down: AtomicBool,
    blocked_info: Mutex<Option<BlockedInfo>>,
    old: AtomicBool,
    batched_work: AtomicBool,
    project_task_requests: AtomicI64,
    /// Answers like a hub from before it could list recorded events.
    no_recorded: AtomicBool,
    projects_fail: AtomicBool,
    lead_fail: AtomicBool,
    created: Mutex<Vec<ProjectSummary>>,
    archived: Mutex<BTreeSet<ProjectId>>,
    removed_projects: Mutex<BTreeSet<ProjectId>>,
    machines: Mutex<Vec<Machine>>,
    accounts: Mutex<Vec<Account>>,
    lead: Mutex<Option<Timestamp>>,
    stopped: Mutex<Vec<LaneTarget>>,
    pacing: Mutex<Pacing>,
    held: Mutex<Option<HeldResults>>,
    lead_runs: Mutex<Vec<LeadRun>>,
    task_runs: Mutex<Vec<RunSummary>>,
    now: AtomicI64,
    sent_count: AtomicI64,
    decisions: Mutex<Vec<Decision>>,
    traced_decisions: Mutex<Vec<Decision>>,
    tasks: Mutex<Vec<TaskSummary>>,
    history: Mutex<Vec<ConversationEntry>>,
    /// A project's own conversation, by name, in place of `history`.
    histories: Mutex<BTreeMap<String, Vec<ConversationEntry>>>,
    recent: Mutex<Vec<FeedItem>>,
    bindings: Mutex<Vec<BindingSummary>>,
    memory: Mutex<Vec<MemoryFile>>,
    timers: Mutex<Vec<Timer>>,
    dreaming: Mutex<BTreeMap<String, threadlane_core::client::DreamingStatus>>,
    findings: Mutex<Vec<threadlane_core::client::MemoryFinding>>,
    metrics: Mutex<Option<threadlane_core::client::OperationsMetrics>>,
    agents: Mutex<Vec<Agent>>,
    repositories: Mutex<Vec<Repository>>,
    recorded: Mutex<Vec<RecordedEvent>>,
    rules: Mutex<Vec<StandingRuleSummary>>,
    /// The space's ceiling; Assisted until set.
    ceiling: Mutex<Option<Mode>>,
    /// The project ceilings set, by project.
    project_ceilings: Mutex<Vec<(Option<String>, Mode)>>,
    /// The decisions answered with "always".
    always: Mutex<Vec<DecisionId>>,
}

impl FakeHub {
    fn new(space: &str, decisions: Vec<Decision>) -> Arc<Self> {
        Arc::new(Self {
            space: space.to_owned(),
            decisions: Mutex::new(decisions),
            ..Self::default()
        })
    }

    fn when(&self) -> When {
        When {
            now: Timestamp::from_millis(self.now.load(Ordering::SeqCst)),
            offset_minutes: 0,
        }
    }

    fn up(&self) -> Result<(), ApiError> {
        if self.down.load(Ordering::SeqCst) {
            Err(ApiError::Unreachable("connection refused".to_owned()))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl HubApi for FakeHub {
    async fn info(&self) -> Result<HubInfo, ApiError> {
        let blocked = self.blocked_info.lock().unwrap().take();
        if let Some((started, release)) = blocked {
            let _ = started.send(());
            let _ = release.await;
        }
        self.up()?;
        Ok(HubInfo {
            space: self.space.clone(),
            mode_ceiling: self.ceiling.lock().expect("lock").unwrap_or(Mode::Assisted),
            paused: false,
            schema_version: 1,
            update_version: None,
        })
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        self.project_list(false).await
    }

    async fn project_list(&self, include_archived: bool) -> Result<Vec<ProjectSummary>, ApiError> {
        self.up()?;
        if self.projects_fail.load(Ordering::SeqCst) {
            return Err(ApiError::Internal(
                "project list temporarily unavailable".to_owned(),
            ));
        }
        let created = self.created.lock().expect("lock").clone();
        let archived = self.archived.lock().expect("lock");
        let removed = self.removed_projects.lock().expect("lock");
        Ok([
            ProjectSummary {
                id: ProjectId(1),
                name: "api".to_owned(),
                is_general: false,
                paused: false,
                mode_ceiling: None,
                archived: false,
            },
            ProjectSummary {
                id: ProjectId(2),
                name: "General".to_owned(),
                is_general: true,
                paused: false,
                mode_ceiling: None,
                archived: false,
            },
        ]
        .into_iter()
        .chain(created)
        .map(|project| ProjectSummary {
            archived: archived.contains(&project.id),
            ..project
        })
        .filter(|project| !removed.contains(&project.id) && (include_archived || !project.archived))
        .collect())
    }

    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError> {
        self.up()?;
        let target = message.task.map_or_else(
            || LaneTarget::Lead {
                project: message.project.clone(),
            },
            |task| LaneTarget::Task { task },
        );
        self.stopped
            .lock()
            .unwrap()
            .retain(|stopped| *stopped != target);
        Ok(Accepted {
            event_id: EventId(99 + self.sent_count.fetch_add(1, Ordering::SeqCst)),
            project_id: project_id(message.project.as_deref()),
            duplicate: false,
        })
    }

    async fn feed(&self, _after: i64, _wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        Err(ApiError::Invalid("unsupported".to_owned()))
    }

    async fn org(&self, request: OrgRequest) -> Result<OrgReply, ApiError> {
        self.up()?;
        if let OrgRequest::SetArchived { project, archived } = request {
            let mut project = self
                .project_list(true)
                .await?
                .into_iter()
                .find(|known| project.as_deref() == Some(known.name.as_str()))
                .ok_or_else(|| ApiError::NotFound(format!("project {project:?}")))?;
            let mut archived_ids = self.archived.lock().expect("lock");
            if archived {
                archived_ids.insert(project.id);
            } else {
                archived_ids.remove(&project.id);
            }
            project.archived = archived;
            return Ok(OrgReply::Project { project });
        }
        let request = match request {
            OrgRequest::CreateAgent {
                name,
                role,
                instructions,
                backends,
            } => OrgRequest::CreateAgentWithModels {
                name,
                role,
                instructions,
                backends,
                models: Default::default(),
            },
            other => other,
        };
        let mut bindings = self.bindings.lock().expect("lock");
        match request {
            OrgRequest::Pacing => Ok(OrgReply::Pacing {
                pacing: self.pacing.lock().unwrap().clone(),
            }),
            OrgRequest::SetPacing { pacing } => {
                *self.pacing.lock().unwrap() = pacing.clone();
                Ok(OrgReply::Pacing { pacing })
            }
            OrgRequest::Stop { target, .. } => {
                self.stopped.lock().unwrap().push(target);
                Ok(OrgReply::LaneUpdated)
            }
            OrgRequest::Bindings { .. } => Ok(OrgReply::Bindings {
                bindings: bindings.clone(),
            }),
            OrgRequest::SetMode { agent, mode, .. }
                if !bindings.iter().any(|b| b.agent == agent) =>
            {
                let binding = BindingSummary {
                    agent,
                    role: "reviewer".to_owned(),
                    mode,
                    is_lead: false,
                    max_parallel: 1,
                    overrides: Default::default(),
                };
                bindings.push(binding.clone());
                Ok(OrgReply::Binding { binding })
            }
            OrgRequest::Repositories { project } => Ok(OrgReply::Repositories {
                repositories: self
                    .repositories
                    .lock()
                    .expect("lock")
                    .iter()
                    .filter(|repository| repository.project_id == project_id(project.as_deref()))
                    .cloned()
                    .collect(),
            }),
            OrgRequest::Templates => Ok(OrgReply::Templates {
                templates: vec![threadlane_core::client::AgentTemplate {
                    id: "chief-of-staff".to_owned(),
                    name: "Chief of Staff".to_owned(),
                    role: "offers the next actions".to_owned(),
                    backends: vec![BackendKind::Codex, BackendKind::Claude],
                    instructions: "Offer a menu.".to_owned(),
                }],
            }),
            OrgRequest::Agents => Ok(OrgReply::Agents {
                agents: self.agents.lock().expect("lock").clone(),
            }),
            OrgRequest::CreateAgentWithModels {
                name,
                role,
                instructions,
                backends,
                models,
            } => {
                let mut agents = self.agents.lock().expect("lock");
                if agents.iter().any(|agent| agent.name == name) {
                    return Err(ApiError::Invalid(format!(
                        "an agent named {name:?} already exists"
                    )));
                }
                let agent = Agent {
                    id: AgentId(i64::try_from(agents.len()).expect("few") + 1),
                    name,
                    role,
                    instructions,
                    backends,
                    models: json!(models),
                    tools: json!({}),
                    skills: Vec::new(),
                    requirements: json!({}),
                };
                agents.push(agent.clone());
                Ok(OrgReply::Agent { agent })
            }
            OrgRequest::UpdateAgent {
                skills,
                agent: from,
                name,
                role,
                instructions,
                backends,
                models,
            } => {
                let mut agents = self.agents.lock().expect("lock");
                let agent = agents
                    .iter_mut()
                    .find(|agent| agent.name == from)
                    .ok_or_else(|| ApiError::NotFound(format!("agent {from:?}")))?;
                if let Some(skills) = skills {
                    agent.skills = skills;
                }
                if let Some(name) = name {
                    agent.name = name;
                }
                if let Some(role) = role {
                    agent.role = role;
                }
                if let Some(instructions) = instructions {
                    agent.instructions = instructions;
                }
                if let Some(backends) = backends {
                    agent.backends = backends
                        .iter()
                        .map(|backend| backend.parse().expect("a backend"))
                        .collect();
                }
                if let Some(models) = models {
                    if !agent.models.is_object() {
                        agent.models = json!({});
                    }
                    if let Some(map) = agent.models.as_object_mut() {
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
                for binding in bindings.iter_mut().filter(|b| b.agent == from) {
                    binding.agent.clone_from(&agent.name);
                }
                Ok(OrgReply::Agent {
                    agent: agent.clone(),
                })
            }
            OrgRequest::DeleteAgent {
                agent,
                confirm_name,
            } => {
                if agent != confirm_name {
                    return Err(ApiError::Invalid("type the agent's name".to_owned()));
                }
                self.agents
                    .lock()
                    .expect("lock")
                    .retain(|known| known.name != agent);
                bindings.retain(|binding| binding.agent != agent);
                Ok(OrgReply::AgentDeleted { agent })
            }
            OrgRequest::AddRepository {
                project,
                name,
                remote_url,
                default_branch,
                ..
            } => {
                let mut repositories = self.repositories.lock().expect("lock");
                let repository = Repository {
                    id: RepositoryId(i64::try_from(repositories.len()).expect("few") + 1),
                    project_id: project_id(project.as_deref()),
                    name,
                    remote_url,
                    default_branch: default_branch.unwrap_or_else(|| "main".to_owned()),
                    protected_branches: Vec::new(),
                };
                repositories.push(repository.clone());
                Ok(OrgReply::Repository { repository })
            }
            OrgRequest::SetPaused { project, paused } => Ok(OrgReply::Project {
                project: ProjectSummary {
                    id: project_id(project.as_deref()),
                    name: project.unwrap_or_else(|| "General".to_owned()),
                    is_general: false,
                    paused,
                    mode_ceiling: None,
                    archived: false,
                },
            }),
            OrgRequest::CreateProject { name, .. } => {
                let mut created = self.created.lock().expect("lock");
                let project = ProjectSummary {
                    id: ProjectId(3 + i64::try_from(created.len()).expect("few")),
                    name,
                    is_general: false,
                    paused: false,
                    mode_ceiling: None,
                    archived: false,
                };
                created.push(project.clone());
                Ok(OrgReply::Project { project })
            }
            OrgRequest::DeletePreview { project } if project == "api" => {
                Ok(OrgReply::DeletePreview {
                    preview: deletion_test_preview(),
                })
            }
            OrgRequest::DeleteProject {
                project,
                confirm_name,
            } if project == "api" && project == confirm_name => {
                self.removed_projects.lock().unwrap().insert(ProjectId(1));
                Ok(OrgReply::ProjectDeleted {
                    project,
                    deleted: deletion_test_preview(),
                    leftovers: vec!["workspaces/clones/1".into()],
                })
            }
            OrgRequest::SetSpacePaused { paused } => Ok(OrgReply::SpacePaused { paused }),
            OrgRequest::SetSpaceCeiling { mode } => {
                *self.ceiling.lock().expect("lock") = Some(mode);
                Ok(OrgReply::SpaceCeiling { ceiling: mode })
            }
            OrgRequest::SetCeiling { project, mode } => {
                self.project_ceilings
                    .lock()
                    .expect("lock")
                    .push((project.clone(), mode));
                Ok(OrgReply::Ceiling {
                    project: project.unwrap_or_else(|| "General".to_owned()),
                    ceiling: mode,
                })
            }
            OrgRequest::SetOverride {
                agent, tier, cell, ..
            } => {
                let binding = bindings
                    .iter_mut()
                    .find(|binding| binding.agent == agent)
                    .ok_or(ApiError::NotFound(agent.clone()))?;
                match cell {
                    Some(cell) => binding.overrides.insert(tier, cell),
                    None => binding.overrides.remove(&tier),
                };
                Ok(OrgReply::Overrides {
                    agent,
                    overrides: binding.overrides.clone(),
                })
            }
            OrgRequest::SetMaxParallel {
                agent,
                max_parallel,
                ..
            } => {
                let binding = bindings
                    .iter_mut()
                    .find(|binding| binding.agent == agent)
                    .ok_or(ApiError::NotFound(agent))?;
                binding.max_parallel = max_parallel;
                Ok(OrgReply::Binding {
                    binding: binding.clone(),
                })
            }
            OrgRequest::SetMode { agent, mode, .. } => {
                let binding = bindings
                    .iter_mut()
                    .find(|binding| binding.agent == agent)
                    .ok_or(ApiError::NotFound(agent))?;
                binding.mode = mode;
                Ok(OrgReply::Binding {
                    binding: binding.clone(),
                })
            }
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    async fn decisions(&self, request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        self.up()?;
        let mut decisions = self.decisions.lock().expect("lock");
        match request {
            DecisionRequest::Pending => Ok(DecisionReply::Pending {
                decisions: decisions.clone(),
            }),
            DecisionRequest::StandingRules { .. } => Ok(DecisionReply::StandingRules {
                rules: self.rules.lock().expect("lock").clone(),
            }),
            DecisionRequest::RevokeRule { rule } => {
                let mut rules = self.rules.lock().expect("lock");
                let listed = rules
                    .iter_mut()
                    .find(|listed| listed.rule.id == rule)
                    .ok_or_else(|| ApiError::NotFound(format!("standing rule {rule}")))?;
                listed.rule.revoked_at = Some(Timestamp::from_millis(5_000));
                Ok(DecisionReply::StandingRule {
                    rule: listed.clone(),
                })
            }
            DecisionRequest::AnswerAlways { decision: id, .. } => {
                self.always.lock().expect("lock").push(id);
                let index = decisions
                    .iter()
                    .position(|decision| decision.id == id)
                    .ok_or_else(|| ApiError::NotFound(id.to_string()))?;
                let mut decision = decisions.remove(index);
                decision.status = DecisionStatus::Answered;
                Ok(DecisionReply::Answered {
                    decision: Box::new(decision),
                    recorded: true,
                })
            }
            DecisionRequest::Answer {
                decision: id,
                option,
                note,
            } => {
                let index = decisions
                    .iter()
                    .position(|decision| decision.id == id)
                    .ok_or_else(|| ApiError::NotFound(id.to_string()))?;
                let mut decision = decisions.remove(index);
                let option = decision
                    .options
                    .iter()
                    .position(|choice| choice.label == option)
                    .ok_or_else(|| ApiError::Invalid(format!("unknown option {option}")))?;
                decision.answer = Some(json!({
                    "option": option, "note": note, "answered_at": self.now.load(Ordering::SeqCst),
                }));
                decision.status = DecisionStatus::Answered;
                Ok(DecisionReply::Answered {
                    decision: Box::new(decision),
                    recorded: true,
                })
            }
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError> {
        self.up()?;
        match request {
            PairingRequest::Clients => Ok(PairingReply::Clients {
                clients: Vec::new(),
            }),
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    async fn overview(&self, request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        self.up()?;
        if self.old.load(Ordering::SeqCst) {
            return Err(ApiError::Invalid("unknown variant `overview`".to_owned()));
        }
        let tasks = self.tasks.lock().expect("lock").clone();
        if self.no_recorded.load(Ordering::SeqCst)
            && matches!(
                request,
                OverviewRequest::Recorded { .. } | OverviewRequest::RecordedCount
            )
        {
            return Err(ApiError::Invalid("unknown variant `recorded`".to_owned()));
        }
        match request {
            OverviewRequest::Tasks { .. } => {
                self.project_task_requests.fetch_add(1, Ordering::SeqCst);
                Ok(OverviewReply::Tasks { tasks })
            }
            OverviewRequest::Running => Ok(OverviewReply::Tasks {
                tasks: tasks
                    .into_iter()
                    .filter(|summary| summary.task.status == TaskStatus::Running)
                    .collect(),
            }),
            OverviewRequest::Conversation {
                project,
                before,
                limit,
            } => {
                use crate::decision_record::conversation_time;
                let named = project.unwrap_or_else(|| "General".to_owned());
                let mut entries = self
                    .histories
                    .lock()
                    .expect("lock")
                    .get(&named)
                    .cloned()
                    .unwrap_or_else(|| self.history.lock().expect("lock").clone());
                entries.retain(|entry| {
                    conversation_time(entry)
                        .is_some_and(|at| before.is_none_or(|before| at < before))
                });
                entries.sort_by_key(conversation_time);
                let start = entries.len().saturating_sub(limit as usize);
                Ok(OverviewReply::Conversation {
                    entries: entries.split_off(start),
                })
            }
            OverviewRequest::Recent { .. } => Ok(OverviewReply::Recent {
                items: self.recent.lock().expect("lock").clone(),
            }),
            OverviewRequest::Machines => Ok(OverviewReply::Machines {
                machines: self.machines.lock().expect("lock").clone(),
                accounts: self.accounts.lock().expect("lock").clone(),
            }),
            OverviewRequest::Lead { .. } if self.lead_fail.load(Ordering::SeqCst) => {
                Err(ApiError::Internal("lead status unavailable".to_owned()))
            }
            OverviewRequest::Lead { project } => Ok(OverviewReply::Lead {
                latest_step: None,
                running_since: *self.lead.lock().expect("lock"),
                stopped: self
                    .stopped
                    .lock()
                    .unwrap()
                    .contains(&LaneTarget::Lead { project }),
            }),
            OverviewRequest::HeldResults => Ok(OverviewReply::HeldResults {
                held: *self.held.lock().unwrap(),
            }),
            OverviewRequest::LeadRuns { .. } => Ok(OverviewReply::LeadRuns {
                runs: self.lead_runs.lock().expect("lock").clone(),
            }),
            OverviewRequest::Recorded { limit, .. } => {
                let recorded = self.recorded.lock().expect("lock");
                Ok(OverviewReply::Recorded {
                    events: recorded
                        .iter()
                        .take(usize::try_from(limit).expect("small"))
                        .cloned()
                        .collect(),
                    total: u64::try_from(recorded.len()).expect("few"),
                })
            }
            OverviewRequest::RecordedCount => Ok(OverviewReply::RecordedCount {
                total: u64::try_from(self.recorded.lock().expect("lock").len()).expect("few"),
            }),
            OverviewRequest::Task { task } => {
                let summary = tasks
                    .into_iter()
                    .find(|summary| summary.task.id == task)
                    .ok_or_else(|| ApiError::NotFound(format!("task {task}")))?;
                let runs = self
                    .task_runs
                    .lock()
                    .expect("lock")
                    .iter()
                    .filter(|row| row.run.lane_id == summary.task.lane_id)
                    .cloned()
                    .collect();
                Ok(OverviewReply::Task {
                    task: Box::new(TaskDetail {
                        summary,
                        branch: format!("threadlane/{task}"),
                        repositories: Vec::new(),
                        runs,
                        reviews: Vec::new(),
                        decisions: Vec::new(),
                        related: Vec::new(),
                        messages: Vec::new(),
                        proposed_by: None,
                        stopped: self
                            .stopped
                            .lock()
                            .unwrap()
                            .contains(&LaneTarget::Task { task }),
                    }),
                })
            }
            OverviewRequest::Work if self.batched_work.load(Ordering::SeqCst) => {
                use threadlane_core::client::{ProjectLeadStatus, WorkOverview};
                let projects = self.project_list(true).await?;
                let leads = projects
                    .iter()
                    .filter(|project| !project.archived)
                    .map(|project| ProjectLeadStatus {
                        project: project.name.clone(),
                        running_since: *self.lead.lock().unwrap(),
                        stopped: false,
                    })
                    .collect();
                Ok(OverviewReply::Work {
                    overview: WorkOverview {
                        projects,
                        tasks,
                        leads,
                    },
                })
            }
            OverviewRequest::Work => Err(ApiError::Invalid("unknown variant `work`".to_owned())),
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    /// Memory like a hub's: every write a new version by you, refused
    /// when over budget, at an index, or outside the project.
    async fn memory(&self, request: MemoryRequest) -> Result<MemoryReply, ApiError> {
        self.up()?;
        let mut files = self.memory.lock().expect("lock");
        let current = |files: &[MemoryFile], path: &str| {
            files
                .iter()
                .rfind(|file| file.path == path)
                .filter(|file| !file.deleted)
                .cloned()
        };
        let mut change = |path: String, content: String, deleted: bool| {
            let version = files.iter().filter(|file| file.path == path).count() + 1;
            let file = memory_file(&path, &content, i64::try_from(version).expect("few"), None);
            let file = MemoryFile { deleted, ..file };
            files.push(file.clone());
            MemoryReply::File { file }
        };
        match request {
            MemoryRequest::Dreaming { project } => {
                let status = self
                    .dreaming
                    .lock()
                    .unwrap()
                    .get(&project.unwrap_or_default())
                    .cloned()
                    .unwrap_or_default();
                Ok(MemoryReply::Dreaming { status })
            }
            MemoryRequest::SetDreaming { project, cadence } => {
                let mut state = self.dreaming.lock().unwrap();
                let status = state.entry(project.unwrap_or_default()).or_default();
                status.cadence = cadence;
                Ok(MemoryReply::Dreaming {
                    status: status.clone(),
                })
            }
            MemoryRequest::List { .. } => {
                let mut paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
                paths.sort();
                paths.dedup();
                Ok(MemoryReply::Files {
                    files: paths
                        .iter()
                        .filter_map(|path| current(&files, path))
                        .collect(),
                })
            }
            MemoryRequest::History { path, .. } => Ok(MemoryReply::History {
                versions: files
                    .iter()
                    .filter(|file| file.path == path)
                    .cloned()
                    .collect(),
            }),
            MemoryRequest::Write { path, content, .. } if content.len() > 20 => {
                Err(ApiError::Invalid(format!(
                    "{path} would be {} bytes, over its budget of 20",
                    content.len()
                )))
            }
            MemoryRequest::Write { path, .. } if path.ends_with("INDEX.md") => Err(
                ApiError::Invalid("INDEX.md files are written by the system".to_owned()),
            ),
            MemoryRequest::Write { path, content, .. } => Ok(change(path, content, false)),
            MemoryRequest::Delete { path, .. } => Ok(change(path, String::new(), true)),
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    async fn operations(
        &self,
        request: threadlane_core::client::OperationsRequest,
    ) -> Result<threadlane_core::client::OperationsReply, ApiError> {
        use threadlane_core::client::{OperationsReply, OperationsRequest, TraceInspection};

        self.up()?;
        match request {
            OperationsRequest::Trace { id } => Ok(OperationsReply::Trace {
                trace: TraceInspection {
                    decisions: self
                        .traced_decisions
                        .lock()
                        .expect("lock")
                        .iter()
                        .filter(|decision| decision.trace_id.as_deref() == Some(&id))
                        .cloned()
                        .collect(),
                    id,
                    events: Vec::new(),
                    runs: Vec::new(),
                    tool_calls: Vec::new(),
                    outbox: Vec::new(),
                },
            }),
            OperationsRequest::Metrics { project: None } => self
                .metrics
                .lock()
                .unwrap()
                .clone()
                .map(|metrics| OperationsReply::Metrics {
                    metrics: Box::new(metrics),
                })
                .ok_or_else(|| ApiError::Invalid("metrics unavailable".into())),
            OperationsRequest::CheckMemory {
                project: Some(project),
            } if project == "api" => Ok(OperationsReply::MemoryCheck {
                findings: self.findings.lock().unwrap().clone(),
            }),
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    /// Timers like a hub's: a routine you create is yours and requested
    /// work; an ended timer is no longer listed.
    async fn timers(&self, request: TimerRequest) -> Result<TimerReply, ApiError> {
        self.up()?;
        let mut timers = self.timers.lock().expect("lock");
        let mut set = |id: TimerId, status: TimerStatus| {
            let timer = timers
                .iter_mut()
                .find(|timer| timer.id == id)
                .ok_or_else(|| ApiError::NotFound(format!("timer {id}")))?;
            timer.status = status;
            Ok(TimerReply::Timer {
                timer: timer.clone(),
            })
        };
        match request {
            TimerRequest::Pause { timer } => set(timer, TimerStatus::Paused),
            TimerRequest::Resume { timer } => set(timer, TimerStatus::Active),
            TimerRequest::Cancel { timer } => set(timer, TimerStatus::Cancelled),
            TimerRequest::List { project } => Ok(TimerReply::Timers {
                timers: timers
                    .iter()
                    .filter(|timer| timer.project_id == project_id(project.as_deref()))
                    .filter(|timer| {
                        matches!(timer.status, TimerStatus::Active | TimerStatus::Paused)
                    })
                    .cloned()
                    .collect(),
            }),
            TimerRequest::CreateRoutine {
                project,
                instruction,
                recurrence,
                ..
            } => {
                let id = i64::try_from(timers.len()).expect("few") + 1;
                let mut created = timer(
                    id,
                    project_id(project.as_deref()),
                    Some(recurrence),
                    TimerCreator::User,
                );
                created.instruction = instruction;
                timers.push(created.clone());
                Ok(TimerReply::Timer { timer: created })
            }
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }
    /// Tasks like a hub's: yours and queued; a cancel ends one.
    async fn tasks(&self, request: TaskRequest) -> Result<TaskReply, ApiError> {
        self.up()?;
        let mut tasks = self.tasks.lock().expect("lock");
        match request {
            TaskRequest::Create {
                title,
                goal,
                agent,
                depends_on,
                ..
            } => {
                let mut created = task(
                    i64::try_from(tasks.len()).expect("few") + 1,
                    TaskStatus::Queued,
                    None,
                );
                created.task.title = title;
                created.task.goal = goal;
                created.task.depends_on = depends_on;
                created.agent = agent;
                tasks.push(created.clone());
                Ok(TaskReply::Task { task: created.task })
            }
            TaskRequest::Cancel { task: id } => {
                let summary = tasks
                    .iter_mut()
                    .find(|summary| summary.task.id == id)
                    .ok_or_else(|| ApiError::NotFound(format!("task {id}")))?;
                summary.task.status = TaskStatus::Cancelled;
                Ok(TaskReply::Task {
                    task: summary.task.clone(),
                })
            }
            TaskRequest::Retry { task: id } => {
                let summary = tasks
                    .iter_mut()
                    .find(|summary| summary.task.id == id)
                    .ok_or_else(|| ApiError::NotFound(format!("task {id}")))?;
                if summary.task.status != TaskStatus::Failed {
                    return Err(ApiError::Invalid(format!("task {id} has not failed")));
                }
                summary.task.status = TaskStatus::Queued;
                Ok(TaskReply::Task {
                    task: summary.task.clone(),
                })
            }
            // The fake numbers repositories by name: `app` is 1.
            TaskRequest::AttachRepositories {
                task: id,
                repositories,
            } => {
                let summary = tasks
                    .iter_mut()
                    .find(|summary| summary.task.id == id)
                    .ok_or_else(|| ApiError::NotFound(format!("task {id}")))?;
                if repositories != ["app"] {
                    return Err(ApiError::Invalid(format!("no {repositories:?}")));
                }
                summary.task.repositories.push(RepositoryId(1));
                Ok(TaskReply::Task {
                    task: summary.task.clone(),
                })
            }
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }
}

/// The project id a fake hub gives `project`: api is 1, General 2.
fn project_id(project: Option<&str>) -> ProjectId {
    ProjectId(if project == Some("api") { 1 } else { 2 })
}

/// A timer of project `project` like a hub's, due at 09:00 on day one.
fn timer(id: i64, project: ProjectId, recurrence: Option<Recurrence>, by: TimerCreator) -> Timer {
    Timer {
        id: TimerId(id),
        project_id: project,
        lane_id: LaneId(1),
        agent_id: None,
        task_ref: None,
        category: if recurrence.is_some() {
            TimerCategory::Schedule
        } else {
            TimerCategory::FollowUp
        },
        due_at: Timestamp::from_millis(9 * 3_600_000),
        recurrence,
        instruction: format!("Check {id}"),
        status: TimerStatus::Active,
        created_by: by,
        created_by_call: None,
        last_fired_at: None,
        fire_count: 0,
        missed_grace_minutes: None,
        requested: by == TimerCreator::User,
    }
}

fn memory_file(path: &str, content: &str, version: i64, run: Option<i64>) -> MemoryFile {
    MemoryFile {
        id: threadlane_core::ids::MemoryFileId(version),
        path: path.to_owned(),
        content: content.to_owned(),
        version,
        written_by_run: run.map(RunId),
        deleted: false,
        written_at: Timestamp::from_millis(version * 60_000),
    }
}

fn decision(id: i64, created_at: i64) -> Decision {
    let option = |label: &str, approve: bool| DecisionOption {
        label: label.to_owned(),
        effect: json!({ "approve": approve }),
    };
    Decision {
        trace_id: None,
        id: DecisionId(id),
        project_id: ProjectId(1),
        lane_id: LaneId(1),
        task_id: None,
        session_id: None,
        run_id: None,
        tool_call_id: None,
        tool: "open_pull_request".to_owned(),
        arguments: json!({}),
        arguments_hash: String::new(),
        kind: DecisionKind::Approval,
        question: format!("Question {id}?"),
        context: String::new(),
        options: vec![option("approve", true), option("decline", false)],
        recommended: Some(0),
        offers_standing_rule: false,
        status: DecisionStatus::Pending,
        answer: None,
        expires_at: None,
        created_at: Timestamp::from_millis(created_at),
        outcome: None,
        proposed_by: None,
    }
}

fn feed_item(seq: i64, kind: OutboxKind, body: Json, at: i64) -> FeedItem {
    FeedItem {
        digest: Vec::new(),
        seq,
        message: OutboxMessage {
            trace_id: None,
            id: OutboxId(seq),
            project_id: Some(ProjectId(1)),
            lane_id: None,
            session_id: None,
            run_id: None,
            tool_call_id: None,
            kind,
            decision_id: None,
            channel: "client".to_owned(),
            body,
            urgency: Urgency::Normal,
            status: OutboxStatus::Sent,
            not_before: None,
            attempts: 1,
            channel_message_id: None,
            created_at: Timestamp::from_millis(at),
            sent_at: Some(Timestamp::from_millis(at)),
            delivery_seq: Some(seq),
        },
        project_name: Some("api".to_owned()),
        delivered_at: Timestamp::from_millis(at),
    }
}

fn item(space: &str, seq: i64, kind: OutboxKind, body: Json) -> SpaceFeedItem {
    SpaceFeedItem {
        space: space.to_owned(),
        item: feed_item(seq, kind, body, 2_000),
    }
}

/// A space-wide account notice, as the hub keys it.
fn account_notice(seq: i64, key: &str, text: &str) -> FeedItem {
    let mut notice = feed_item(seq, OutboxKind::SystemNotice, json!({ "text": text }), seq);
    notice.message.project_id = None;
    notice.project_name = None;
    notice.message.tool_call_id = Some(key.to_owned());
    notice
}

fn task(id: i64, status: TaskStatus, waiting_reason: Option<&str>) -> TaskSummary {
    TaskSummary {
        latest_step: None,
        task: Task {
            id: TaskId(id),
            project_id: ProjectId(1),
            lane_id: LaneId(10 + id),
            agent_id: AgentId(2),
            title: format!("Task {id}"),
            goal: String::new(),
            repositories: Vec::new(),
            parent_task_id: None,
            depends_on: Vec::new(),
            origin: TaskOrigin::User,
            status,
            priority: 0,
            created_by_call: None,
            last_machine_id: None,
            waiting_reason: waiting_reason.map(str::to_owned),
            result: None,
            created_at: Timestamp::from_millis(1_000),
            publication_blocker: None,
        },
        project: "api".to_owned(),
        agent: "Engineer".to_owned(),
        machine: (status == TaskStatus::Running).then(|| "macbook".to_owned()),
        active_machine_id: None,
        running_since: (status == TaskStatus::Running).then(|| Timestamp::from_millis(0)),
        completed_at: None,
    }
}

/// Twelve minutes after the epoch, in UTC.
fn when() -> When {
    When {
        now: Timestamp::from_millis(12 * 60_000),
        offset_minutes: 0,
    }
}

/// A core with each `(name, hub)` paired for this process only.
fn core(hubs: &[(&str, &Arc<FakeHub>)]) -> (TempDir, ClientCore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut core = ClientCore::load(ClientDirs::new(dir.path())).expect("load");
    for (name, hub) in hubs {
        core = core.with_hub(name, Arc::clone(hub) as Arc<dyn HubApi>);
    }
    (dir, core)
}

fn queue(state: &AppState) -> Vec<(String, String)> {
    state
        .queue()
        .iter()
        .map(|queued| (queued.space.clone(), queued.decision.id.to_string()))
        .collect()
}

fn pair(space: &str, id: &str) -> (String, String) {
    (space.to_owned(), id.to_owned())
}

#[tokio::test]
async fn decisions_from_two_hubs_merge_oldest_first() {
    let work = FakeHub::new("Work", vec![decision(1, 3_000), decision(2, 1_000)]);
    let home = FakeHub::new("Home", vec![decision(1, 2_000)]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();

    state.apply(refresh(&core).await);

    assert_eq!(
        queue(&state),
        [
            pair("work", "D-0002"),
            pair("home", "D-0001"),
            pair("work", "D-0001")
        ]
    );
    let names: Vec<&str> = state.spaces().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["work", "home"]);
    assert!(state.spaces().iter().all(|s| s.reach == Reach::Reachable));
    assert_eq!(state.project_name("home", ProjectId(1)), Some("api"));
}

#[tokio::test]
async fn the_to_picker_lists_each_space_lead_first_and_names_spaces_only_when_several() {
    let work = FakeHub::new("Work", vec![]);
    let (_one, single) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&single).await);
    assert_eq!(state.lead_targets(), [("work", "General"), ("work", "api")]);
    assert_eq!(state.lead_name("work", "General"), "Space lead");
    assert_eq!(state.to_label("work", "General"), "To: Space lead");
    assert_eq!(state.to_label("work", "api"), "To: api lead");

    let home = FakeHub::new("Home", vec![]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    state.apply(refresh(&core).await);
    assert_eq!(
        state.lead_targets(),
        [
            ("work", "General"),
            ("work", "api"),
            ("home", "General"),
            ("home", "api")
        ]
    );
    assert_eq!(state.to_label("home", "api"), "To: home · api lead");
}

#[tokio::test]
async fn a_chat_tab_sends_to_its_own_lead_until_another_is_picked_there() {
    let work = FakeHub::new("Work", vec![]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert_eq!(state.chat_target("work", "api"), ("work", "api"));

    state.set_chat_target(("work", "api"), ("work", "General"));
    assert_eq!(state.chat_target("work", "api"), ("work", "General"));
    assert_eq!(
        state.chat_target("work", "General"),
        ("work", "General"),
        "another page keeps its own lead"
    );
    state.set_chat_target(("work", "api"), ("work", "gone"));
    assert_eq!(
        state.chat_target("work", "api"),
        ("work", "api"),
        "unknown lead"
    );

    state.set_chat_target(("work", "api"), ("work", "General"));
    state.reset_chat_target();
    assert_eq!(
        state.chat_target("work", "api"),
        ("work", "api"),
        "after navigating"
    );
}

#[test]
fn choosing_another_recipient_carries_the_draft_being_typed() {
    let mut state = AppState::default();
    let api = Destination::Lead("work".into(), "api".into());
    let general = Destination::Lead("work".into(), "General".into());
    state.edit_draft(&api, "Ship it");
    state.move_draft(&api, &general);
    assert_eq!(state.draft(&general), "Ship it");
    assert!(
        state.draft(&api).is_empty(),
        "no copy left on the page's own lead"
    );

    state.edit_draft(&api, "Second");
    state.move_draft(&api, &general);
    assert_eq!(state.draft(&general), "Second\n\nShip it");
}

#[test]
fn the_recipient_is_locked_while_a_send_is_held_or_images_load() {
    let mut state = AppState::default();
    let general = Destination::Lead("work".into(), "General".into());
    assert!(state.can_retarget(&general));

    state.loading_images(&general);
    assert!(!state.can_retarget(&general), "images loading");
    state.attach_images(&general, Ok(vec![]));
    assert!(state.can_retarget(&general));

    state.edit_draft(&general, "Ship it");
    assert!(
        state
            .request_send(general.clone(), Timestamp::from_millis(1_000))
            .held
            .is_some()
    );
    assert!(!state.can_retarget(&general), "send held");
    assert!(state.undo_send(&general));
    assert!(state.can_retarget(&general));
}

#[tokio::test]
async fn each_space_uses_its_own_general_project_for_main_chat() {
    let work = FakeHub::new("Work", vec![]);
    let home = FakeHub::new("Home", vec![]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.set_target("work", "api");

    for space in ["work", "home"] {
        assert_eq!(
            state.space(space).unwrap().main_chat_project(),
            Some("General")
        );
    }
    assert_eq!(state.target(), Some(("work", "api")));
}

#[tokio::test]
async fn an_unreachable_hub_does_not_hide_the_others() {
    let work = FakeHub::new("Work", vec![decision(1, 1_000)]);
    work.tasks
        .lock()
        .expect("lock")
        .push(task(1, TaskStatus::Running, None));
    let home = FakeHub::new("Home", vec![decision(1, 2_000)]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    work.down.store(true, Ordering::SeqCst);
    home.decisions.lock().expect("lock").push(decision(2, 500));
    state.apply(refresh(&core).await);

    let work_space = &state.spaces()[0];
    assert!(
        matches!(work_space.reach, Reach::Unreachable(_)),
        "work is shown as away"
    );
    assert_eq!(work_space.projects.len(), 2, "its last projects are kept");
    assert_eq!(work_space.running.len(), 1, "and what it last ran");
    assert_eq!(state.spaces()[1].reach, Reach::Reachable);
    assert_eq!(work_space.gap(Part::Running), None, "away is not an error");
    assert!(state.retry_soon(), "and is asked again soon");
    assert_eq!(
        queue(&state),
        [
            pair("home", "D-0002"),
            pair("work", "D-0001"),
            pair("home", "D-0001")
        ],
        "home's new decision shows; work's last known one stays"
    );
}

#[tokio::test]
async fn answering_removes_the_decision_from_the_queue_counts_and_board() {
    let mut in_task = decision(1, 1_000);
    in_task.task_id = Some(TaskId(7));
    let work = FakeHub::new("Work", vec![in_task, decision(2, 2_000)]);
    work.tasks
        .lock()
        .expect("lock")
        .push(task(7, TaskStatus::WaitingOnYou, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    assert!(state.is_pending("work", "D-0001"));
    assert_eq!(state.open_in("work", ProjectId(1)), 2, "per-project count");
    assert_eq!(state.open_in("work", ProjectId(2)), 0);
    let waiting = |state: &AppState| {
        state
            .board("work", "api", &when())
            .into_iter()
            .find(|column| column.title == "Waiting on you")
            .expect("column")
            .cards
    };
    assert_eq!(waiting(&state)[0].decision, Some(DecisionId(1)));
    assert_eq!(waiting(&state)[0].meta, "Engineer · D-0001 open");

    let answer = core
        .answer(Some("work"), "D-0001", "approve", Some("go ahead"))
        .await
        .expect("answer");
    state.answered(&answer);

    assert!(!state.is_pending("work", "D-0001"));
    assert_eq!(queue(&state), [pair("work", "D-0002")]);
    assert_eq!(state.open_in("work", ProjectId(1)), 1);
    assert_eq!(waiting(&state)[0].decision, None, "no Answer button");
    state.apply(refresh(&core).await);
    assert_eq!(queue(&state), [pair("work", "D-0002")], "the hub agrees");
}

#[test]
fn decisions_and_notices_raise_one_notification_and_messages_none() {
    let mut state = AppState::default();
    let card = item(
        "work",
        2,
        OutboxKind::Decision,
        json!({
            "text": "D-0003: Open a pull request? Answer with ...",
            "decision": "D-0003",
            "question": "Open a pull request?",
            "options": ["approve", "decline"],
        }),
    );

    assert_eq!(
        state.receive(item(
            "work",
            1,
            OutboxKind::Message,
            json!({ "text": "hi" })
        )),
        None
    );
    let alert = state.receive(card.clone()).expect("a decision notifies");
    assert_eq!(alert.tag(), "work/D-0003");
    assert_eq!(decision_of_tag(&alert.tag()), Some(("work", "D-0003")));
    assert_eq!(alert.body(), "Open a pull request?");
    assert_eq!(alert.options(), ["approve", "decline"]);
    let notice = state.receive(item(
        "work",
        3,
        OutboxKind::SystemNotice,
        json!({ "text": "the lead stopped" }),
    ));
    assert!(matches!(notice, Some(Alert::Notice { ref text, .. }) if text == "the lead stopped"));
    let notice = notice.expect("a notice notifies");
    assert_eq!(decision_of_tag(&notice.tag()), None, "nothing to answer");

    assert_eq!(
        state.receive(card),
        None,
        "a replayed card does not notify again"
    );
}

fn said(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|line| match line {
            Line::Day(day) => format!("-- {day}"),
            Line::New => "new".to_owned(),
            Line::Sending { text, .. } => format!("sending: {text}"),
            Line::You { text, .. } => format!("you: {text}"),
            Line::SpaceLead { text, .. } => format!("General lead: {text}"),
            Line::ProjectReply { project, text, .. } => format!("{project} lead: {text}"),
            Line::Lead { text, .. } => format!("lead: {text}"),
            Line::Notice { text, .. } => format!("notice: {text}"),
            Line::Decision { card, pending, .. } => format!("{}: pending {pending}", card.id),
            Line::Quiet { text, .. } => text.clone(),
            Line::Stopped { .. } => "lead stopped".to_owned(),
            Line::DeliveryFailure { text, .. } => format!("failed: {text}"),
            Line::Digest { digest, .. } => digest.toggle.clone(),
        })
        .collect()
}

#[tokio::test]
async fn the_conversation_merges_history_your_messages_and_the_feed_once_each() {
    let day = 86_400_000;
    let work = FakeHub::new("Work", Vec::new());
    *work.history.lock().expect("lock") = vec![
        ConversationEntry::You {
            event_id: EventId(1),
            text: "Fix the refunds".to_owned(),
            images: Vec::new(),
            at: Timestamp::from_millis(1_000),
        },
        ConversationEntry::Delivered {
            item: feed_item(1, OutboxKind::Message, json!({ "text": "On it" }), 2_000),
        },
        ConversationEntry::Delivered {
            item: feed_item(
                2,
                OutboxKind::Decision,
                json!({ "decision": "D-0001", "question": "Open it?", "options": ["yes"] }),
                day + 1_000,
            ),
        },
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    // The feed delivers the same lead message again, and one more; you send
    // a message the history does not have yet.
    state.receive(SpaceFeedItem {
        space: "work".to_owned(),
        item: feed_item(1, OutboxKind::Message, json!({ "text": "On it" }), 2_000),
    });
    state.receive(SpaceFeedItem {
        space: "work".to_owned(),
        item: feed_item(
            3,
            OutboxKind::Message,
            json!({ "text": "Done" }),
            day + 3_000,
        ),
    });
    let sent_at = Timestamp::from_millis(day + 2_000);
    state.sent("work", "api", "Also the changelog", EventId(2), sent_at);
    // The next history load has your message too, under the same event id.
    work.history
        .lock()
        .expect("lock")
        .push(ConversationEntry::You {
            event_id: EventId(2),
            text: "Also the changelog".to_owned(),
            images: Vec::new(),
            at: sent_at,
        });
    state.apply_project(load_project(&core, "work", "api").await);

    let when = When {
        now: Timestamp::from_millis(day + 5_000),
        offset_minutes: 0,
    };
    assert_eq!(
        said(&state.conversation("work", "api", &when)),
        [
            "-- Yesterday",
            "you: Fix the refunds",
            "lead: On it",
            "-- Today",
            "D-0001: pending false",
            "you: Also the changelog",
            "lead: Done",
        ]
    );
    assert!(state.conversation("work", "General", &when).is_empty());
    assert!(state.conversation("home", "api", &when).is_empty());
    assert_eq!(state.earlier("work", "api"), None, "one page holds it all");
}

#[test]
fn an_item_seen_twice_is_shown_once() {
    let mut state = AppState::default();
    let first = item("work", 1, OutboxKind::Message, json!({ "text": "hi" }));
    state.receive(first.clone());
    assert!(state.home_has_new_replies());
    state.saw_home_replies();
    state.receive(first);
    assert!(!state.home_has_new_replies());
    // The same seq from another space is another item.
    state.receive(item(
        "home",
        1,
        OutboxKind::Message,
        json!({ "text": "hi" }),
    ));

    state.set_filter(Some("work"));
    state.saw_home_replies();
    assert!(!state.home_has_new_replies());
    state.set_filter(None);
    assert!(state.home_has_new_replies());

    let lines = |space| said(&state.conversation(space, "api", &when()));
    assert_eq!(lines("work"), ["-- Today", "lead: hi"]);
    assert_eq!(lines("home"), ["-- Today", "lead: hi"]);
    assert!(state.conversation("work", "other", &when()).is_empty());
}

#[tokio::test]
async fn the_board_groups_tasks_by_status_and_says_why_one_waits() {
    let work = FakeHub::new("Work", Vec::new());
    *work.tasks.lock().expect("lock") = vec![
        task(1, TaskStatus::Queued, Some("waiting for T-0002 to be done")),
        task(2, TaskStatus::Running, None),
        task(3, TaskStatus::Queued, Some(BLOCKED_FOR_TEST)),
        task(4, TaskStatus::Done, None),
        task(5, TaskStatus::Queued, None),
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);

    state.apply_task(load_task(&core, "work", TaskId(1)).await);
    let detail = state.task("work", TaskId(1)).unwrap();
    assert_eq!(
        state.waiting_task("work", &detail.summary).as_deref(),
        Some("waiting for T-0002 to be done")
    );
    let queued = state
        .board("work", "api", &when())
        .into_iter()
        .find(|column| column.title == "Queued")
        .unwrap();
    assert_eq!(queued.cards[0].elapsed.as_deref(), Some("11 min"));
    let board: Vec<(&str, Vec<(TaskId, String)>)> = state
        .board("work", "api", &when())
        .into_iter()
        .map(|column| {
            (
                column.title,
                column
                    .cards
                    .into_iter()
                    .map(|card| (card.id, card.meta))
                    .collect(),
            )
        })
        .collect();
    let card = |id, meta: &str| (TaskId(id), meta.to_owned());
    assert_eq!(
        board,
        [
            (
                "Queued",
                vec![
                    card(1, "Engineer · waiting for T-0002 to be done"),
                    card(5, "Engineer · waiting for a machine"),
                ]
            ),
            (
                "Blocked",
                vec![card(3, &format!("Engineer · {BLOCKED_FOR_TEST}"))]
            ),
            ("Running", vec![card(2, "Engineer · macbook")]),
            ("Waiting on you", vec![]),
            ("Done", vec![card(4, "Engineer")]),
        ]
    );
    assert!(
        state
            .board("work", "other", &when())
            .iter()
            .all(|c| c.cards.is_empty())
    );
}

#[tokio::test]
async fn a_finished_only_board_keeps_the_four_default_columns_and_all_three_cards() {
    let work = FakeHub::new("Work", Vec::new());
    *work.tasks.lock().expect("lock") =
        (1..=3).map(|id| task(id, TaskStatus::Done, None)).collect();
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);

    let board = state.board("work", "api", &when());
    let headers: Vec<_> = board
        .iter()
        .map(|column| (column.title, column.count))
        .collect();
    assert_eq!(
        headers,
        [
            ("Queued", 0),
            ("Running", 0),
            ("Waiting on you", 0),
            ("Done", 3)
        ]
    );
    let cards: Vec<_> = board[3].cards.iter().map(|card| card.id).collect();
    assert_eq!(cards, [TaskId(3), TaskId(2), TaskId(1)]);
}

#[test]
fn count_forms_use_singular_only_for_one() {
    for (one, many) in [
        ("machine", "machines"),
        ("task", "tasks"),
        ("result", "results"),
        ("run", "runs"),
        ("note", "notes"),
        ("file", "files"),
        ("rule", "rules"),
    ] {
        assert_eq!(count_form(0_usize, one, many), many);
        assert_eq!(count_form(1_usize, one, many), one);
        assert_eq!(count_form(2_usize, one, many), many);
    }
}

#[tokio::test]
async fn inbox_lists_the_newest_recent_failures_of_the_spaces_it_shows() {
    let day = 24 * 60 * 60 * 1_000;
    let failed = |id: i64, at: i64| {
        let mut summary = task(id, TaskStatus::Failed, None);
        summary.task.created_at = Timestamp::from_millis(at);
        summary
    };
    let work = FakeHub::new("Work", Vec::new());
    let mut tasks: Vec<_> = (1..=6).map(|id| failed(id, 10 * day + id)).collect();
    tasks.push(failed(7, 2 * day));
    tasks.push(task(8, TaskStatus::Done, None));
    *work.tasks.lock().expect("lock") = tasks;
    let home = FakeHub::new("Home", Vec::new());
    *home.tasks.lock().expect("lock") = vec![failed(9, 10 * day + 10)];
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    let when = When {
        now: Timestamp::from_millis(12 * day),
        offset_minutes: 0,
    };
    let listed = |state: &AppState| -> Vec<_> {
        state
            .recent_failures(&when)
            .into_iter()
            .map(|(space, summary)| (space.to_owned(), summary.task.id.0))
            .collect()
    };

    assert_eq!(
        listed(&state),
        [
            ("home".to_owned(), 9),
            ("work".to_owned(), 6),
            ("work".to_owned(), 5),
            ("work".to_owned(), 4),
            ("work".to_owned(), 3),
        ],
        "newest first, at most five, none older than a week, no done tasks"
    );
    state.set_filter(Some("home"));
    assert_eq!(listed(&state), [("home".to_owned(), 9)]);
}

#[tokio::test]
async fn running_inbox_entries_open_the_right_task_when_spaces_share_an_id() {
    let personal = FakeHub::new("Personal", Vec::new());
    let work = FakeHub::new("Work", Vec::new());
    let mut reported = task(26, TaskStatus::Running, None);
    reported.task.title = "Bind Codex and Claude skills to agent profiles".into();
    reported.project = "threadlane".into();
    *personal.tasks.lock().unwrap() = vec![reported.clone(), task(27, TaskStatus::Queued, None)];
    *work.tasks.lock().unwrap() = vec![task(26, TaskStatus::Running, None)];
    let (_dir, core) = core(&[("personal", &personal), ("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    assert_eq!(state.running().len(), 2);
    state.set_filter(Some("personal"));
    let entries = state.running();
    assert_eq!(entries, [("personal", &reported)]);
    let (space, entry) = entries[0];
    let detail = load_task(&core, space, entry.task.id).await;
    state.apply_task(detail);
    assert_eq!(
        state.task("personal", TaskId(26)).unwrap().summary,
        reported
    );
}

#[tokio::test]
async fn inbox_is_caught_up_only_when_nothing_needs_you_and_nothing_runs_in_the_spaces_shown() {
    let work = FakeHub::new("Work", Vec::new());
    *work.tasks.lock().expect("lock") = vec![task(1, TaskStatus::Running, None)];
    let home = FakeHub::new("Home", Vec::new());
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    assert_eq!(state.needs_you().count(), 0);
    assert!(
        !state.caught_up(),
        "a running task keeps the Inbox from saying caught up"
    );
    state.set_filter(Some("home"));
    assert!(state.caught_up(), "nothing runs in the space shown");
    state.set_filter(Some("work"));
    assert!(!state.caught_up());

    *work.tasks.lock().expect("lock") = vec![task(1, TaskStatus::Done, None)];
    state.apply(refresh(&core).await);
    assert!(state.caught_up(), "no decisions and nothing running");
}

const BLOCKED_FOR_TEST: &str = "blocked: every model account it can use is at its usage limit";

#[tokio::test]
async fn home_lists_what_runs_and_the_latest_result_per_project_filtered_by_space() {
    let work = FakeHub::new("Work", vec![decision(1, 1_000)]);
    *work.tasks.lock().expect("lock") = vec![
        task(1, TaskStatus::Running, None),
        task(2, TaskStatus::Queued, None),
    ];
    *work.recent.lock().expect("lock") = vec![
        feed_item(1, OutboxKind::Message, json!({ "text": "older" }), 1_000),
        feed_item(2, OutboxKind::Message, json!({ "text": "newer" }), 2_000),
    ];
    let home = FakeHub::new("Home", Vec::new());
    *home.recent.lock().expect("lock") = vec![feed_item(
        1,
        OutboxKind::Message,
        json!({ "text": "home result" }),
        3_000,
    )];
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    let running: Vec<_> = state
        .running()
        .into_iter()
        .map(|(space, summary)| (space, summary.task.id))
        .collect();
    assert_eq!(running, [("work", TaskId(1))]);
    let recent: Vec<_> = state
        .recent()
        .into_iter()
        .map(|item| (item.space.as_str(), item.text()))
        .collect();
    assert_eq!(
        recent,
        [
            ("home", "home result".to_owned()),
            ("work", "newer".to_owned())
        ]
    );
    assert_eq!(
        state.status_line(),
        "2 spaces reachable · 1 running · 1 needs you · 0 of 0 machines online"
    );
    assert_eq!(
        state.target(),
        Some(("work", "General")),
        "General by default"
    );

    state.set_filter(Some("home"));
    assert!(state.running().is_empty());
    assert_eq!(state.needs_you().count(), 0);
    assert_eq!(state.recent().len(), 1);
    assert_eq!(state.target(), Some(("home", "General")));
    state.set_target("work", "api");
    assert_eq!(state.target(), Some(("work", "api")), "the last one chosen");
}

#[tokio::test]
async fn a_space_is_blocked_from_its_blocked_notice_until_one_says_it_resumed() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert_eq!(state.blocked("work"), None);

    let notice = |seq, key: &str, text: &str| SpaceFeedItem {
        space: "work".to_owned(),
        item: account_notice(seq, key, text),
    };
    state.receive(notice(1, "system:account:1:blocked", "No account left."));
    assert_eq!(state.blocked("work").as_deref(), Some("No account left."));
    assert_eq!(state.blocked("home"), None, "per space");
    // Another notice does not end it.
    state.receive(item(
        "work",
        2,
        OutboxKind::SystemNotice,
        json!({ "text": "the lead stopped" }),
    ));
    assert!(state.blocked("work").is_some());
    state.receive(notice(3, "system:account:1:resumed", "Resumed."));
    assert_eq!(state.blocked("work"), None);

    // From history after a restart: the latest one counts.
    *work.recent.lock().expect("lock") = vec![account_notice(
        4,
        "system:account:2:blocked",
        "Blocked again.",
    )];
    state.apply(refresh(&core).await);
    assert_eq!(state.blocked("work").as_deref(), Some("Blocked again."));
}

#[tokio::test]
async fn a_mode_change_shows_in_the_project_at_once() {
    let work = FakeHub::new("Work", Vec::new());
    *work.bindings.lock().expect("lock") = vec![BindingSummary {
        agent: "Engineer".to_owned(),
        role: "engineer".to_owned(),
        mode: Mode::OnRequest,
        is_lead: false,
        max_parallel: 1,
        overrides: Default::default(),
    }];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);

    let binding = core
        .set_mode(Some("work"), Some("api"), "Engineer", Mode::Assisted)
        .await
        .expect("set mode");
    state.mode_set("work", "api", binding);

    let modes: Vec<_> = state
        .project("work", "api")
        .expect("loaded")
        .bindings
        .iter()
        .map(|binding| (binding.agent.as_str(), binding.mode))
        .collect();
    assert_eq!(modes, [("Engineer", Mode::Assisted)]);
}

#[test]
fn a_task_s_timeline_has_its_runs_decisions_and_result() {
    let mut summary = task(7, TaskStatus::Done, None);
    summary.machine = Some("linux-box".to_owned());
    summary.task.result = Some(json!({ "summary": "Refunds are idempotent." }));
    let run = |id, started, ended, status, outcome| RunSummary {
        steps: Vec::new(),
        usage: None,
        run: Run {
            trace_id: String::new(),
            id: RunId(id),
            lane_id: LaneId(17),
            session_id: SessionId(1),
            machine_id: MachineId(2),
            lease_epoch: 1,
            agent_id: Some(AgentId(2)),
            backend: Some(BackendKind::Claude),
            status,
            outcome,
            rationale: None,
            error: (status == RunStatus::Failed).then(|| "usage limit".to_owned()),
            started_at: Timestamp::from_millis(started),
            ended_at: Some(Timestamp::from_millis(ended)),
        },
        machine: "linux-box".to_owned(),
    };
    let mut asked = decision(3, 200_000);
    asked.task_id = Some(TaskId(7));
    asked.status = DecisionStatus::Answered;
    asked.answer = Some(json!({ "option": 0, "note": "draft", "answered_at": 300_000 }));
    let detail = TaskDetail {
        summary,
        branch: "threadlane/T-0007".to_owned(),
        repositories: vec!["payments-service".to_owned()],
        reviews: Vec::new(),
        runs: vec![
            run(1, 60_000, 120_000, RunStatus::Failed, None),
            run(
                2,
                120_000,
                180_000 + 60_000 * 28,
                RunStatus::Committed,
                Some(RunOutcome::Deferred),
            ),
            run(
                3,
                400_000,
                460_000,
                RunStatus::Committed,
                Some(RunOutcome::Sent),
            ),
        ],
        decisions: vec![asked],
        proposed_by: None,
        stopped: false,
        related: Vec::new(),
        messages: vec![ConversationEntry::You {
            event_id: EventId(9),
            text: "Use the ledger.".to_owned(),
            images: Vec::new(),
            at: Timestamp::from_millis(350_000),
        }],
    };
    let when = When {
        now: Timestamp::from_millis(3_600_000),
        offset_minutes: 0,
    };

    assert_eq!(
        chips(&detail, &when),
        [
            "Engineer",
            "claude",
            "linux-box",
            "payments-service",
            "threadlane/T-0007",
            "Requested by you",
            "Done"
        ]
    );
    let mut accepted = detail.clone();
    accepted.summary.task.origin = TaskOrigin::Accepted;
    accepted.proposed_by = Some("Coordinator".to_owned());
    assert!(
        chips(&accepted, &when).contains(&"Proposed by Coordinator, accepted by you".to_owned())
    );
    let moments: Vec<_> = timeline(&detail, &when)
        .into_iter()
        .map(|moment| format!("{} {}", moment.time, moment.text))
        .collect();
    assert_eq!(
        moments,
        [
            "00:00 Created: Task 7. Requested by you.",
            "00:01 Engineer on linux-box with claude: worked for 1 min, failed: usage limit",
            "00:02 Engineer on linux-box with claude: worked for 29 min, stopped to ask you",
            "00:03 Asked D-0003: Question 3?",
            "00:05 You answered D-0003: approve with the note \"draft\"",
            "00:05 You wrote:\n\nUse the ledger.",
            "00:06 Engineer on linux-box with claude: worked for 1 min, reported back",
            "00:31 Done:\n\nRefunds are idempotent.",
        ]
    );
}

fn machine(id: i64, status: MachineStatus) -> Machine {
    Machine {
        id: MachineId(id),
        name: format!("machine-{id}"),
        endpoint_id: String::new(),
        is_hub: id == 1,
        trust: MachineTrust::Trusted,
        status,
        last_seen: None,
        capabilities: json!({}),
        backends: json!({}),
        gh_login: false,
        max_parallel: 1,
        load: 0,
    }
}

#[test]
fn paired_hubs_show_before_the_first_refresh_as_not_yet_known() {
    let state = AppState::paired(["work", "home"]);

    let names: Vec<&str> = state.spaces().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["work", "home"], "no \"No hub is paired\"");
    assert!(state.spaces().iter().all(|s| s.reach == Reach::Unknown));
    assert_eq!(state.spaces()[0].gap(Part::Projects), Some(Gap::Loading));
    assert_eq!(
        state.gaps(Part::Running),
        [("work", Gap::Loading), ("home", Gap::Loading)],
        "not \"Nothing is running\""
    );
    assert_eq!(state.machine_count(), None, "not 0 / 0");
    assert_eq!(
        state.status_line(),
        "0 spaces reachable · 0 running · 0 need you"
    );
    assert!(state.retry_soon());
    assert!(AppState::paired([]).spaces().is_empty());
}

#[tokio::test]
async fn a_hub_that_refuses_a_part_shows_its_error_there_and_the_rest_loads() {
    let work = FakeHub::new("Work", vec![decision(1, 1_000)]);
    work.machines
        .lock()
        .expect("lock")
        .push(machine(1, MachineStatus::Online));
    work.tasks
        .lock()
        .expect("lock")
        .push(task(1, TaskStatus::Running, None));
    work.old.store(true, Ordering::SeqCst);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);

    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    let space = &state.spaces()[0];
    assert_eq!(space.reach, Reach::Reachable);
    let refused = "hub work refused the request: invalid request: unknown variant `overview`";
    for part in [Part::Running, Part::Machines, Part::Recent] {
        assert_eq!(
            space.gap(part),
            Some(Gap::Failed(refused.to_owned())),
            "{part:?}"
        );
    }
    assert_eq!(state.machine_count(), None, "no count rather than 0 / 0");
    assert_eq!(space.gap(Part::Projects), None, "what did load shows");
    assert_eq!(space.projects.len(), 2);
    assert_eq!(queue(&state), [pair("work", "D-0001")]);
    assert_eq!(
        state.project_gap("work", "api", Part::Tasks),
        Some(Gap::Failed(refused.to_owned()))
    );
    assert_eq!(state.project_gap("work", "api", Part::Bindings), None);
    assert!(state.retry_soon(), "asked again at the next tick");

    // The hub learns the request; the next refresh fills the gaps.
    work.old.store(false, Ordering::SeqCst);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    let space = &state.spaces()[0];
    assert_eq!(space.gap(Part::Machines), None);
    assert_eq!(state.machine_count(), Some((1, 1)));
    assert_eq!(state.running().len(), 1);
    assert_eq!(state.project_gap("work", "api", Part::Tasks), None);
    assert!(!state.retry_soon());
    assert_eq!(
        state.status_line(),
        "1 space reachable · 1 running · 1 needs you · 1 of 1 machine online"
    );

    // Refused again: the old count is not shown as if it were current.
    work.old.store(true, Ordering::SeqCst);
    state.apply(refresh(&core).await);
    assert_eq!(state.machine_count(), None);
    assert!(state.spaces()[0].machines.is_empty());
    assert!(state.running().is_empty());
}

#[tokio::test]
async fn a_task_that_cannot_be_loaded_says_why_instead_of_loading_forever() {
    let work = FakeHub::new("Work", Vec::new());
    work.tasks
        .lock()
        .expect("lock")
        .push(task(4, TaskStatus::Done, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    assert_eq!(state.task_gap("work", TaskId(4)), Some(Gap::Loading));

    state.apply_task(load_task(&core, "work", TaskId(4)).await);
    assert_eq!(state.task_gap("work", TaskId(4)), None);
    assert!(state.task("work", TaskId(4)).is_some());

    state.apply_task(load_task(&core, "work", TaskId(9)).await);
    assert_eq!(
        state.task_gap("work", TaskId(9)),
        Some(Gap::Failed(
            "hub work refused the request: not found: task T-0009".to_owned()
        ))
    );
}

#[test]
fn a_home_result_uses_the_rendered_three_line_limit_and_remembers_its_toggle() {
    let mut state = AppState::default();
    let limit = ProseLimit::Result;
    assert_eq!(limit.disclosure(false, false), (Some(3), None));
    assert_eq!(limit.disclosure(false, true), (Some(3), Some("Show more")));
    state.toggle_result("work", 1);
    assert_eq!(
        limit.disclosure(state.result_expanded("work", 1), true),
        (None, Some("Show less"))
    );
    assert!(!state.result_expanded("home", 1));
    assert!(!state.result_expanded("work", 2));
    state.toggle_result("work", 1);
    assert!(!state.result_expanded("work", 1));
}

#[tokio::test]
async fn a_card_waiting_on_you_offers_the_decision_s_options() {
    let mut asked = decision(1, 1_000);
    asked.task_id = Some(TaskId(7));
    let work = FakeHub::new("Work", vec![asked]);
    work.tasks
        .lock()
        .expect("lock")
        .push(task(7, TaskStatus::WaitingOnYou, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    let card = state
        .board("work", "api", &when())
        .into_iter()
        .flat_map(|column| column.cards)
        .find(|card| card.id == TaskId(7))
        .expect("card");
    assert_eq!(card.decision, Some(DecisionId(1)));
    assert_eq!(card.options, ["approve", "decline"]);

    let answer = core
        .answer(Some("work"), "D-0001", &card.options[0], None)
        .await
        .expect("answer");
    state.answered(&answer);
    let card = state
        .board("work", "api", &when())
        .into_iter()
        .flat_map(|column| column.cards)
        .find(|card| card.id == TaskId(7))
        .expect("card");
    assert_eq!((card.decision, card.options.len()), (None, 0));
}

#[tokio::test]
async fn the_modes_grid_has_a_row_per_project_and_a_cell_per_agent() {
    let work = FakeHub::new("Work", Vec::new());
    *work.bindings.lock().expect("lock") = vec![BindingSummary {
        agent: "Engineer".to_owned(),
        role: "engineer".to_owned(),
        mode: Mode::Assisted,
        is_lead: false,
        max_parallel: 1,
        overrides: Default::default(),
    }];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    // The fake lists no agents; the grid's columns come from the space.
    state.spaces[0].agents = ["Engineer", "Reviewer"]
        .iter()
        .map(|name| Agent {
            id: AgentId(1),
            name: (*name).to_owned(),
            role: String::new(),
            instructions: String::new(),
            backends: Vec::new(),
            models: json!({}),
            tools: json!({}),
            skills: Vec::new(),
            requirements: json!({}),
        })
        .collect();
    let grid = state.modes("work");
    assert_eq!(grid.agents, ["Engineer", "Reviewer"]);
    assert!(
        grid.rows.iter().all(|row| row.modes == Err(Gap::Loading)),
        "until the bindings answer"
    );

    let projects = vec![
        ("work".to_owned(), "api".to_owned()),
        ("work".to_owned(), "General".to_owned()),
    ];
    state.apply_modes(load_modes(&core, projects).await);
    let binding = core
        .set_mode(Some("work"), Some("api"), "Reviewer", Mode::OnRequest)
        .await
        .expect("set mode");
    state.mode_set("work", "api", binding);

    let rows: Vec<_> = state
        .modes("work")
        .rows
        .into_iter()
        .map(|row| (row.project, row.modes))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "api".to_owned(),
                Ok(vec![Some(Mode::Assisted), Some(Mode::OnRequest)])
            ),
            // The fake shares one binding list across projects.
            ("General".to_owned(), Ok(vec![Some(Mode::Assisted), None])),
        ]
    );
    assert_eq!(state.modes("home"), ModesGrid::default());
}

#[tokio::test]
async fn the_conversation_header_says_since_when_the_lead_is_working() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(state.lead_line("work", "api", &when()), None, "idle");

    *work.lead.lock().expect("lock") = Some(Timestamp::from_millis(9 * 60_000));
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(
        state.lead_line("work", "api", &when()).as_deref(),
        Some("Lead is working · started 00:09")
    );

    work.old.store(true, Ordering::SeqCst);
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(
        state.lead_line("work", "api", &when()).as_deref(),
        Some(
            "Whether the lead is working: hub work refused the request: invalid request: \
             unknown variant `overview`"
        )
    );
}

#[tokio::test]
async fn a_new_project_shows_in_its_space_at_once() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    let created = core
        .create_project(Some("work"), "Payments", "")
        .await
        .expect("create");
    state.project_created("work", created.clone());
    assert_eq!(
        state.activity(),
        Some(&Activity::Done("Created Payments in work.".to_owned()))
    );
    state.project_created("work", created);
    state.project_created(
        "home",
        ProjectSummary {
            id: ProjectId(9),
            name: "Elsewhere".to_owned(),
            is_general: false,
            paused: false,
            mode_ceiling: None,
            archived: false,
        },
    );

    let names: Vec<_> = state.spaces()[0]
        .projects
        .iter()
        .map(|project| project.name.as_str())
        .collect();
    assert_eq!(names, ["api", "General", "Payments"], "once");
    assert!(state.targets().contains(&("work", "Payments")));
    state.apply(refresh(&core).await);
    assert_eq!(state.spaces()[0].projects.len(), 3, "the hub agrees");
}

#[tokio::test]
async fn the_memory_tab_groups_the_files_the_lead_sees_and_you_change_them_as_yourself() {
    let work = FakeHub::new("Work", Vec::new());
    *work.memory.lock().expect("lock") = vec![
        memory_file(
            "project/tasks/T-0001.md",
            "# T-0001\nHalf done.",
            1,
            Some(4),
        ),
        memory_file("project/notes/INDEX.md", "- [Build](build.md)", 1, Some(3)),
        memory_file("project/notes/build.md", "# Build\nRun make.", 1, Some(3)),
        memory_file("space/notes/style.md", "# Style\nShort.", 1, Some(2)),
        memory_file("space/instructions.md", "Be kind.", 1, None),
        memory_file("project/repos/app.md", "no title", 1, Some(3)),
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    assert_eq!(state.memory_gap("work", "api"), Some(Gap::Loading));
    assert_eq!(state.lead_knows("work", "api"), None);

    state.apply_memory(load_memory(&core, "work", "api", None).await);
    let groups: Vec<(&str, Vec<(String, String)>)> = state
        .memory_groups("work", "api")
        .into_iter()
        .map(|group| {
            let files = group
                .files
                .into_iter()
                .map(|row| (row.path, row.title))
                .collect();
            (group.title, files)
        })
        .collect();
    let row = |path: &str, title: &str| (path.to_owned(), title.to_owned());
    assert_eq!(
        groups,
        [
            (
                "Space instructions",
                vec![row("space/instructions.md", "Be kind.")]
            ),
            ("Space notes", vec![row("space/notes/style.md", "Style")]),
            (
                "Project instructions",
                vec![row("project/instructions.md", "Not written yet")]
            ),
            (
                "Project notes",
                vec![
                    row("project/notes/INDEX.md", "INDEX.md"),
                    row("project/notes/build.md", "Build")
                ]
            ),
            (
                "Repository notes",
                vec![row("project/repos/app.md", "no title")]
            ),
            (
                "Task progress files",
                vec![row("project/tasks/T-0001.md", "T-0001")]
            ),
        ]
    );
    let knows = state.lead_knows("work", "api").expect("listed");
    assert_eq!(
        knows.lines(),
        [
            "No instructions for this project".to_owned(),
            "2 notes · 1 task progress file".to_owned()
        ]
    );

    // An index is shown, never edited.
    state.select_memory("work", "api", "project/notes/INDEX.md");
    assert_eq!(state.edit_memory("work", "api", true), None);
    assert!(!state.memory_editing("work", "api"));
    assert!(!editable("project/notes/INDEX.md"));

    // Instructions are yours to write, even before they exist; a write over
    // budget shows the refusal.
    let path = "project/instructions.md";
    state.select_memory("work", "api", path);
    assert!(state.memory_can_edit("work", "api"));
    assert_eq!(state.edit_memory("work", "api", true).as_deref(), Some(""));
    let saved = state
        .apply_change(change_memory(&core, "work", "api", path, Some("Ship it.".to_owned())).await);
    assert_eq!(
        saved.as_deref(),
        Ok("Saved project/instructions.md as version 1.")
    );
    assert!(state.memory_groups("work", "api")[2].files[0].selected);
    assert_eq!(
        state.edit_memory("work", "api", true).as_deref(),
        Some("Ship it.")
    );
    assert!(state.memory_editing("work", "api"));
    let refused =
        state.apply_change(change_memory(&core, "work", "api", path, Some("x".repeat(30))).await);
    assert!(refused.is_err());
    assert!(state.memory_editing("work", "api"), "still open to fix");
    assert!(
        state
            .memory_refused("work", "api")
            .is_some_and(|why| why.contains("over its budget")),
        "{:?}",
        state.memory_refused("work", "api")
    );
    state
        .apply_change(
            change_memory(&core, "work", "api", path, Some("Ship today.".to_owned())).await,
        )
        .expect("saved");
    assert!(!state.memory_editing("work", "api"));
    assert_eq!(state.memory_refused("work", "api"), None);
    assert_eq!(
        state
            .memory_file("work", "api")
            .map(|file| file.content.as_str()),
        Some("Ship today.")
    );
    assert_eq!(
        state.lead_knows("work", "api").expect("listed").lines()[0],
        "Instructions for this project"
    );

    // Versions, newest first, by you or by a run.
    state.select_memory("work", "api", "project/notes/build.md");
    state.apply_memory(
        load_memory(
            &core,
            "work",
            "api",
            Some("project/notes/build.md".to_owned()),
        )
        .await,
    );
    state
        .apply_change(change_memory(&core, "work", "api", "project/notes/build.md", None).await)
        .expect("deleted");
    assert_eq!(
        state.memory_versions("work", "api", &when()),
        ["v2 · 00:02 · deleted by you", "v1 · 00:01 · by run 3"]
    );
    assert_eq!(state.memory_file("work", "api"), None);

    work.down.store(true, Ordering::SeqCst);
    state.apply_memory(load_memory(&core, "work", "api", None).await);
    assert!(matches!(
        state.memory_gap("work", "api"),
        Some(Gap::Failed(_))
    ));
    assert!(state.retry_soon());
}

#[test]
fn a_decision_card_shows_why_it_is_asked_and_the_calls_arguments() {
    let mut pull = decision(1, 0);
    pull.context = "\nThe branch is pushed.\nMore detail.".to_owned();
    pull.arguments = json!({
        "body": "Adds   the\npage.",
        "title": "Add the page",
        "repository": "app",
        "draft": true,
    });
    assert_eq!(context_line(&pull), Some("The branch is pushed."));
    assert_eq!(
        arguments(&pull),
        [
            "Repository: app",
            "Title: Add the page",
            "Body: Adds the page.",
            "Draft: true"
        ]
    );

    let mut memory = decision(2, 0);
    memory.tool = "memory_create".to_owned();
    memory.arguments = json!({
        "path": "project/instructions.md",
        "content": "word ".repeat(100),
    });
    let shown = arguments(&memory);
    assert_eq!(shown[0], "Path: project/instructions.md");
    assert!(shown[1].starts_with("Content: word word"));
    assert!(
        shown[1].ends_with('…') && shown[1].chars().count() < 180,
        "{}",
        shown[1]
    );
    assert_eq!(context_line(&decision(3, 0)), None);
    memory.arguments = json!("not an object");
    assert!(arguments(&memory).is_empty());
}

#[tokio::test]
async fn a_space_added_or_unpaired_shows_at_once_and_the_last_action_stays() {
    let work = FakeHub::new("Work", vec![decision(1, 0)]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.report(Activity::Working("Pairing home…".to_owned()));

    state.space_added("home");
    state.space_added("home");
    let names: Vec<&str> = state.spaces().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["work", "home"]);
    assert_eq!(state.space("home").map(|s| &s.reach), Some(&Reach::Unknown));
    state.report(Activity::Done("Paired home.".to_owned()));
    assert_eq!(state.activity().map(Activity::text), Some("Paired home."));

    state.ask_unpair("work");
    assert_eq!(state.unpairing(), Some("work"));
    state.cancel_unpair();
    assert_eq!(state.unpairing(), None);
    state.ask_unpair("work");
    state.set_filter(Some("work"));
    state.space_removed("work");
    assert_eq!(state.unpairing(), None);
    assert!(state.space("work").is_none());
    assert!(state.queue().is_empty());
    assert_eq!(state.filter(), None);
    assert_eq!(
        state.receive(item(
            "work",
            1,
            OutboxKind::SystemNotice,
            json!({ "text": "hi" })
        )),
        None,
        "an unpaired space's feed is not shown"
    );
    assert!(state.recent().is_empty());
    assert!(
        state
            .receive(item(
                "home",
                1,
                OutboxKind::SystemNotice,
                json!({ "text": "hi" })
            ))
            .is_some()
    );
    assert_eq!(state.activity().map(Activity::text), Some("Paired home."));
}

#[tokio::test]
async fn your_message_to_a_task_shows_in_its_timeline_once() {
    let work = FakeHub::new("Work", Vec::new());
    work.tasks
        .lock()
        .expect("lock")
        .push(task(1, TaskStatus::WaitingOnYou, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_task(load_task(&core, "work", TaskId(1)).await);
    let at = Timestamp::from_millis(5 * 60_000);
    state.sent_to_task("work", TaskId(1), "Add tests first.", EventId(4), at);
    state.sent_to_task("work", TaskId(1), "Add tests first.", EventId(4), at);
    state.sent_to_task("work", TaskId(2), "Elsewhere.", EventId(5), at);
    let detail = state.task("work", TaskId(1)).expect("loaded");
    assert_eq!(
        detail.messages,
        [ConversationEntry::You {
            images: Vec::new(),
            event_id: EventId(4),
            text: "Add tests first.".to_owned(),
            at
        }]
    );
    let moments: Vec<String> = timeline(detail, &when())
        .into_iter()
        .map(|moment| moment.text)
        .collect();
    assert!(
        moments.contains(&"You wrote:\n\nAdd tests first.".to_owned()),
        "{moments:?}"
    );
}

fn daily(hour: u8) -> Recurrence {
    Recurrence {
        rule: Rule::Daily { hour, minute: 0 },
        zone: "Europe/Berlin".to_owned(),
    }
}

#[tokio::test]
async fn routines_list_and_pause_resume_and_cancel_only_once_confirmed() {
    let work = FakeHub::new("Work", Vec::new());
    let mut follow_up = timer(1, ProjectId(1), None, TimerCreator::Agent);
    follow_up.fire_count = 2;
    follow_up.last_fired_at = Some(Timestamp::from_millis(8 * 3_600_000));
    work.timers.lock().expect("lock").push(follow_up);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    let api = || vec![("work".to_owned(), "api".to_owned())];
    assert_eq!(state.routines_gap("work", "api"), Some(Gap::Loading));

    let created = core
        .add_routine(Some("work"), Some("api"), "Triage new issues", daily(9))
        .await
        .expect("created");
    state.apply_routines(load_routines(&core, api()).await);

    assert_eq!(state.routines_gap("work", "api"), None);
    let rows = state.routine_rows("work", "api", &when());
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].cadence.as_str(), rows[0].zone.as_deref()),
        ("follow-up", None)
    );
    assert_eq!(rows[0].by, "Set by an agent");
    assert_eq!(rows[0].fired, "Fired 2 times · last 08:00");
    assert_eq!(rows[1].instruction, "Triage new issues");
    assert_eq!(
        (rows[1].cadence.as_str(), rows[1].zone.as_deref()),
        ("daily at 09:00", Some("Europe/Berlin"))
    );
    assert_eq!(rows[1].next, "Next 09:00", "local time");
    assert_eq!(rows[1].by, "Set by you · work you requested");
    assert_eq!(rows[1].fired, "Not fired yet");
    assert_eq!(rows[1].status, "Active");

    let number = created.id.to_string();
    let paused = core
        .pause_timer(Some("work"), &number)
        .await
        .expect("paused");
    state.timer_changed("work", "api", paused);
    let row = &state.routine_rows("work", "api", &when())[1];
    assert!(row.paused);
    assert_eq!((row.status, row.next.as_str()), ("Paused", "Paused"));
    let resumed = core
        .resume_timer(Some("work"), &number)
        .await
        .expect("resumed");
    state.timer_changed("work", "api", resumed);
    assert!(!state.routine_rows("work", "api", &when())[1].paused);

    state.ask_cancel_timer("work", created.id);
    let confirming: Vec<bool> = state
        .routine_rows("work", "api", &when())
        .iter()
        .map(|row| row.confirming)
        .collect();
    assert_eq!(confirming, [false, true], "only the one asked about");
    state.keep_timer();
    assert!(
        state
            .routine_rows("work", "api", &when())
            .iter()
            .all(|row| !row.confirming)
    );
    state.ask_cancel_timer("work", created.id);
    let cancelled = core
        .cancel_timer(Some("work"), &number)
        .await
        .expect("cancelled");
    state.timer_changed("work", "api", cancelled);
    let rows = state.routine_rows("work", "api", &when());
    assert_eq!(rows.len(), 1, "a cancelled routine leaves the list");
    assert!(!rows[0].confirming);
    state.apply_routines(load_routines(&core, api()).await);
    assert_eq!(
        state.routine_rows("work", "api", &when()).len(),
        1,
        "the hub agrees"
    );
}

#[test]
fn routine_cadences_display_whole_hours_and_keep_other_intervals_in_minutes() {
    let mut state = AppState::default();
    for (id, minutes) in [(1, 45), (2, 60), (3, 120)] {
        state.timer_changed(
            "work",
            "api",
            timer(
                id,
                ProjectId(1),
                Some(Recurrence {
                    rule: Rule::Interval { minutes },
                    zone: "Europe/Berlin".to_owned(),
                }),
                TimerCreator::User,
            ),
        );
    }
    let rows = state.routine_rows("work", "api", &when());
    let cadences: Vec<_> = rows.iter().map(|row| row.cadence.as_str()).collect();
    assert_eq!(
        cadences,
        ["every 45 minutes", "every 1 hour", "every 2 hours"]
    );
}

#[tokio::test]
async fn the_routines_screen_groups_by_space_and_project_and_shows_a_refusal() {
    let work = FakeHub::new("Work", Vec::new());
    work.timers.lock().expect("lock").extend([timer(
        1,
        ProjectId(2),
        Some(daily(8)),
        TimerCreator::User,
    )]);
    let home = FakeHub::new("Home", Vec::new());
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    let all = |state: &AppState| -> Vec<(String, String)> {
        state
            .targets()
            .into_iter()
            .map(|(space, project)| (space.to_owned(), project.to_owned()))
            .collect()
    };
    state.apply_routines(load_routines(&core, all(&state)).await);

    let groups = state.routine_groups(&when());
    let shown: Vec<(&str, &str, usize)> = groups
        .iter()
        .map(|group| {
            let rows = group.rows.as_ref().map_or(0, Vec::len);
            (group.space.as_str(), group.project.as_str(), rows)
        })
        .collect();
    assert_eq!(
        shown,
        [("work", "General", 1)],
        "projects with none are left out"
    );

    home.old.store(true, Ordering::SeqCst);
    home.down.store(true, Ordering::SeqCst);
    state.apply_routines(load_routines(&core, all(&state)).await);
    let groups = state.routine_groups(&when());
    assert_eq!(
        groups.len(),
        3,
        "home's projects say why they are not listed"
    );
    assert!(matches!(groups[1].rows, Err(Gap::Failed(_))), "{groups:?}");
    assert!(state.retry_soon());
}

#[test]
fn the_new_routine_form_makes_each_cadence_and_says_what_is_wrong() {
    let text = |every, at, day| RoutineText {
        every,
        at,
        day,
        zone: "Europe/Berlin",
    };
    let mut form = RoutineForm {
        hours: true,
        ..RoutineForm::default()
    };
    assert!(!form.is_open_for("work", "api"));
    form.open_for = Some(("work".to_owned(), "api".to_owned()));
    assert!(form.is_open_for("work", "api"));
    assert!(!form.is_open_for("work", "web"));
    assert!(!form.is_open_for("personal", "api"));
    assert_eq!(
        form.recurrence(text("2", "", "")).map(|r| r.rule),
        Ok(Rule::Interval { minutes: 120 })
    );
    form.hours = false;
    assert_eq!(
        form.recurrence(text("90", "", "")).map(|r| r.rule),
        Ok(Rule::Interval { minutes: 90 })
    );
    form.cadence = Cadence::Daily;
    assert_eq!(
        form.recurrence(text("", "08:30", "")),
        Ok(Recurrence {
            rule: Rule::Daily {
                hour: 8,
                minute: 30
            },
            zone: "Europe/Berlin".to_owned()
        })
    );
    assert!(form.recurrence(text("", "25:00", "")).is_err());
    assert!(form.recurrence(text("", "soon", "")).is_err());
    form.cadence = Cadence::Weekdays;
    form.toggle_day(Weekday::Fri);
    form.toggle_day(Weekday::Wed);
    assert_eq!(
        form.recurrence(text("", "07:00", "")).map(|r| r.rule),
        Ok(Rule::Weekdays {
            days: vec![Weekday::Mon, Weekday::Wed, Weekday::Fri],
            hour: 7,
            minute: 0
        }),
        "days in week order"
    );
    for day in [Weekday::Mon, Weekday::Wed, Weekday::Fri] {
        form.toggle_day(day);
    }
    assert!(form.recurrence(text("", "07:00", "")).is_err(), "no day");
    form.cadence = Cadence::Monthly;
    assert_eq!(
        form.recurrence(text("", "06:15", "31")).map(|r| r.rule),
        Ok(Rule::Monthly {
            day: 31,
            hour: 6,
            minute: 15
        })
    );
    assert!(form.recurrence(text("", "06:15", "32")).is_err());
    let no_zone = RoutineText {
        zone: " ",
        ..text("", "06:15", "1")
    };
    assert!(form.recurrence(no_zone).is_err(), "a zone is needed");
}

#[test]
fn every_tab_is_selected_where_it_is_shown() {
    let labels = Tab::ALL.map(Tab::label);
    for tab in Tab::ALL {
        assert_eq!(Tab::at(tab.index()), Some(tab), "{tab:?}");
        assert_eq!(labels[tab.index()], tab.label(), "{tab:?}");
        assert_eq!(Tab::named(&tab.label().to_lowercase()), Some(tab));
        // The bar shows only icons, so the tooltip must name the tab.
        assert!(tab.tooltip().starts_with(tab.label()), "{tab:?}");
    }
    assert_eq!(
        labels,
        ["Chat", "Tasks", "Team", "Routines", "Memory", "Setup"]
    );
}

#[test]
fn the_legacy_agents_tab_name_opens_team() {
    assert_eq!(Tab::named("agents"), Some(Tab::Team));
    assert_eq!(Tab::named("board"), None);
}

#[test]
fn the_task_screen_shows_usage_and_whether_its_branch_is_published() {
    let run = |id, started: i64, ended: Option<i64>| RunSummary {
        steps: Vec::new(),
        usage: None,
        run: Run {
            trace_id: String::new(),
            id: RunId(id),
            lane_id: LaneId(17),
            session_id: SessionId(1),
            machine_id: MachineId(2),
            lease_epoch: 1,
            agent_id: Some(AgentId(2)),
            backend: Some(BackendKind::Claude),
            status: if ended.is_some() {
                RunStatus::Committed
            } else {
                RunStatus::Active
            },
            outcome: None,
            rationale: None,
            error: None,
            started_at: Timestamp::from_millis(started),
            ended_at: ended.map(Timestamp::from_millis),
        },
        machine: "linux-box".to_owned(),
    };
    let mut detail = TaskDetail {
        summary: task(7, TaskStatus::Running, None),
        branch: "threadlane/T-0007".to_owned(),
        repositories: Vec::new(),
        runs: vec![run(1, 0, Some(5 * 60_000)), run(2, 10 * 60_000, None)],
        reviews: Vec::new(),
        decisions: Vec::new(),
        related: Vec::new(),
        messages: Vec::new(),
        proposed_by: None,
        stopped: false,
    };
    let usage_lines = usage(&detail, &when());
    assert_eq!(
        usage_lines[0], "2 runs · 7 min",
        "the running one counts until now"
    );
    assert!(usage_lines[1].contains("not reported"), "{usage_lines:?}");
    let unstarted = TaskDetail {
        summary: task(7, TaskStatus::Queued, None),
        runs: Vec::new(),
        ..detail.clone()
    };
    assert_eq!(usage(&unstarted, &when()), ["No runs"]);

    let published = |detail: &TaskDetail| {
        let changes = changes(detail);
        assert_eq!(changes.branch, "threadlane/T-0007");
        (changes.state, changes.pull_request)
    };
    assert_eq!(published(&detail), ("Not pushed".to_owned(), None));
    let mut push = decision(4, 1_000);
    push.tool = "publish_branch".to_owned();
    detail.decisions.push(push.clone());
    assert_eq!(published(&detail).0, "Waiting on you to push (D-0004)");
    push.status = DecisionStatus::Answered;
    push.answer = Some(json!({ "option": 1 }));
    detail.decisions = vec![push.clone()];
    assert_eq!(published(&detail).0, "Not pushed: you declined D-0004");
    push.answer = Some(json!({ "option": 0 }));
    push.outcome = Some(json!({ "performed": false, "error": "no gh login" }));
    detail.decisions = vec![push.clone()];
    assert_eq!(published(&detail).0, "Not pushed: no gh login");
    let mut pull = decision(5, 2_000);
    pull.status = DecisionStatus::Answered;
    pull.answer = Some(json!({ "option": 0 }));
    pull.outcome = Some(json!({
        "performed": true,
        "branch": "threadlane/T-0007",
        "pull_request": "https://github.com/me/api/pull/3",
    }));
    detail.decisions.push(pull);
    assert_eq!(
        published(&detail),
        (
            "Pushed · pull request opened".to_owned(),
            Some("https://github.com/me/api/pull/3".to_owned())
        )
    );
    // The hub also records what was published in the task's result.
    detail.decisions.clear();
    detail.summary.task.result = Some(json!({ "summary": "done", "branch": "threadlane/T-0007" }));
    assert_eq!(published(&detail), ("Pushed".to_owned(), None));
}

#[test]
fn copying_says_so_and_with_no_hub_the_app_explains_pairing() {
    let mut state = AppState::default();
    assert!(state.first_run(), "no hub paired");
    state.copied("D-0012");
    assert_eq!(state.activity().map(Activity::text), Some("Copied D-0012"));
    state.space_added("work");
    assert!(!state.first_run());
    state.space_removed("work");
    assert!(state.first_run(), "unpairing the last one shows it again");
}

fn binding(agent: &str, mode: Mode) -> BindingSummary {
    BindingSummary {
        agent: agent.to_owned(),
        role: "engineer".to_owned(),
        mode,
        is_lead: agent == "Lead",
        max_parallel: 1,
        overrides: Default::default(),
    }
}

#[tokio::test]
async fn a_new_agent_is_listed_with_unbound_cells_that_bind_it() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    state.apply_modes(load_modes(&core, vec![("work".into(), "api".into())]).await);

    state.agent_form_mut().space = Some("work".to_owned());
    state.agent_form_mut().backends = 2;
    assert_eq!(
        state.agent_form().backends(),
        [BackendKind::Codex, BackendKind::Claude]
    );
    let backends = state.agent_form().backends();
    let agent = core
        .create_agent(Some("work"), "Reviewer", "reviewer", "Review.", &backends)
        .await
        .expect("created");
    state.agent_created("work", agent);
    assert_eq!(state.agent_form().space, None, "the form closes");
    assert_eq!(work.agents.lock().expect("lock")[0].backends, backends);

    let grid = state.modes("work");
    assert_eq!(grid.agents, ["Reviewer"]);
    assert_eq!(grid.rows[0].modes, Ok(vec![None]), "a — cell");
    let bound = core
        .set_mode(Some("work"), Some("api"), "Reviewer", Mode::OnRequest)
        .await
        .expect("bound");
    state.mode_set("work", "api", bound);
    assert_eq!(
        state.modes("work").rows[0].modes,
        Ok(vec![Some(Mode::OnRequest)])
    );
}

#[tokio::test]
async fn a_template_fills_the_new_agent_form_to_edit_before_saving() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);

    state.open_agent_form("work");
    assert_eq!(state.agent_form().templates, None, "not listed yet");
    assert_eq!(state.agent_form_mut().pick_template("chief-of-staff"), None);
    state.templates_loaded("home", core.agent_templates(Some("work")).await);
    assert_eq!(state.agent_form().templates, None, "another space's form");
    state.templates_loaded("work", core.agent_templates(Some("work")).await);

    let draft = state
        .agent_form_mut()
        .pick_template("chief-of-staff")
        .expect("offered");
    assert_eq!(
        (
            draft.name.as_str(),
            draft.role.as_str(),
            draft.instructions.as_str()
        ),
        ("Chief of Staff", "chief-of-staff", "Offer a menu.")
    );
    assert_eq!(
        state.agent_form().backends(),
        [BackendKind::Codex, BackendKind::Claude]
    );
    assert_eq!(state.agent_form_mut().pick_template("nobody"), None);

    let agent = core
        .create_agent(
            Some("work"),
            "Tudi",
            &draft.role,
            &draft.instructions,
            &state.agent_form().backends(),
        )
        .await
        .expect("created");
    state.agent_created("work", agent);
    assert_eq!(state.agent_form().space, None, "the form closes");
    assert_eq!(work.agents.lock().expect("lock")[0].name, "Tudi");

    work.down.store(true, Ordering::SeqCst);
    state.open_agent_form("work");
    state.templates_loaded("work", core.agent_templates(Some("work")).await);
    assert!(
        matches!(state.agent_form().templates, Some(Err(_))),
        "a hub that cannot answer says why"
    );
}

#[tokio::test]
async fn copying_an_agent_offers_what_the_other_space_lacks_and_copies_it_whole() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    for (name, hub) in [("Reviewer", &work), ("Writer", &work), ("Writer", &home)] {
        hub.org(OrgRequest::CreateAgentWithModels {
            name: name.to_owned(),
            role: "role".to_owned(),
            instructions: format!("Be {name}."),
            backends: vec![BackendKind::Codex],
            models: [("codex".into(), "custom".into())].into(),
        })
        .await
        .expect("created");
    }
    let mut state = AppState::paired(["work", "home"]);
    state.apply(refresh(&core).await);

    let targets = state.copy_targets("work");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].0, "home");
    let offered: Vec<&str> = targets[0]
        .1
        .iter()
        .map(|agent| agent.name.as_str())
        .collect();
    assert_eq!(offered, ["Reviewer"], "Writer is there already");

    let copied = copy_agent(&core, &targets[0].1[0], "home")
        .await
        .expect("copied");
    assert_eq!(
        (
            copied.role.as_str(),
            copied.instructions.as_str(),
            copied.backends.as_slice()
        ),
        ("role", "Be Reviewer.", [BackendKind::Codex].as_slice())
    );
    assert_eq!(copied.models, json!({"codex": "custom"}));
    state.agent_created("home", copied);
    assert!(state.copy_targets("work")[0].1.is_empty());
    assert!(copy_agent(&core, &targets[0].1[0], "home").await.is_err());
}

#[tokio::test]
async fn a_remote_another_space_has_is_refused_naming_that_space() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    home.org(OrgRequest::AddRepository {
        project: Some("api".to_owned()),
        name: "widgets".to_owned(),
        remote_url: "https://github.com/Acme/Widgets.git".to_owned(),
        default_branch: None,
    })
    .await
    .expect("added");
    home.org(OrgRequest::AddRepository {
        project: None,
        name: "notes".to_owned(),
        remote_url: "/srv/git/notes".to_owned(),
        default_branch: None,
    })
    .await
    .expect("added");

    let refused = add_repository(&core, "work", "api", "w", "git@github.com:acme/widgets", "")
        .await
        .expect_err("refused");
    assert!(
        refused.contains("already registered in home")
            && refused.contains("repository widgets of project api"),
        "{refused}"
    );
    let refused = add_repository(&core, "work", "api", "n", "/srv/git/notes", "")
        .await
        .expect_err("same text");
    assert!(refused.contains("in home"), "{refused}");
    assert!(
        add_repository(&core, "work", "api", "", "x", "")
            .await
            .is_err()
    );

    // The same space is the hub's to check; another path is new.
    let mut state = AppState::paired(["work", "home"]);
    state.open_repository_form("work", "api", true);
    assert!(state.repository_form_open("work", "api"));
    let added = add_repository(&core, "work", "api", "site", "/srv/git/site", "trunk")
        .await
        .expect("added");
    assert_eq!(added.default_branch, "trunk");
    state.repository_added("work", "api", added);
    assert!(!state.repository_form_open("work", "api"));
    assert_eq!(state.task_repositories("work", "api"), ["site"]);

    home.down.store(true, Ordering::SeqCst);
    let unchecked = add_repository(&core, "work", "api", "web", "/srv/git/web", "")
        .await
        .expect_err("not checked");
    assert!(unchecked.contains("Could not check home"), "{unchecked}");
}

#[tokio::test]
async fn a_new_task_goes_to_an_agent_that_is_on_and_waits_for_open_tasks() {
    let work = FakeHub::new("Work", Vec::new());
    {
        let mut tasks = work.tasks.lock().expect("lock");
        tasks.push(task(1, TaskStatus::Running, None));
        tasks.push(task(2, TaskStatus::Done, None));
    }
    work.bindings.lock().expect("lock").extend([
        binding("Lead", Mode::OnRequest),
        binding("Engineer", Mode::Assisted),
        binding("Idle", Mode::Off),
    ]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    assert_eq!(state.task_agents("work", "api"), ["Lead", "Engineer"]);
    let open: Vec<TaskId> = state
        .task_dependencies("work", "api")
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(open, [TaskId(1)], "a done task is not offered");
    assert!(state.task_spec("work", "api", "T", "G").is_err(), "closed");

    state.open_task_form("work", "api", true);
    assert_eq!(state.task_form("work", "api").expect("open").agent, None);
    assert!(state.task_form("work", "General").is_none());
    assert_eq!(
        state.task_spec("work", "api", "Fix it", "Make it pass"),
        Err("Choose the agent that does it.".to_owned())
    );
    state.task_form_mut().agent = Some("Engineer".to_owned());
    state.task_form_mut().toggle_dependency(TaskId(1));
    state.task_form_mut().draft_pr = true;
    assert_eq!(
        state.task_spec("work", "api", "  ", "Make it pass"),
        Err("Give the task a title and a goal.".to_owned())
    );
    let spec = state
        .task_spec("work", "api", " Fix it ", "Make it pass")
        .expect("complete");
    assert_eq!(spec.title, "Fix it");
    assert_eq!(spec.depends_on, [TaskId(1)]);
    assert_eq!(spec.project.as_deref(), Some("api"));
    assert!(spec.draft_pr);
    let refused = describe(
        &core
            .create_task(Some("work"), spec.clone())
            .await
            .expect_err("a draft pull request needs a repository"),
    );
    assert!(refused.contains("needs a repository"), "{refused}");
    let spec = threadlane_client::NewTaskSpec {
        repositories: vec!["app".to_owned()],
        ..spec
    };
    let created = core.create_task(Some("work"), spec).await.expect("created");
    assert!(
        created.goal.starts_with("Make it pass")
            && created.goal.ends_with(threadlane_client::DRAFT_PR_GOAL),
        "{}",
        created.goal
    );
    state.task_created("work", "api");
    assert!(state.task_form("work", "api").is_none());
    state.apply_project(load_project(&core, "work", "api").await);
    let titles: Vec<String> = state
        .project("work", "api")
        .expect("loaded")
        .tasks
        .iter()
        .map(|summary| summary.task.title.clone())
        .collect();
    assert!(titles.contains(&created.title), "{titles:?}");
}

#[tokio::test]
async fn cancelling_a_task_asks_first_then_shows_it_ended() {
    let work = FakeHub::new("Work", Vec::new());
    work.tasks
        .lock()
        .expect("lock")
        .push(task(1, TaskStatus::Running, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    state.apply_task(load_task(&core, "work", TaskId(1)).await);

    assert_eq!(state.task_control("work", TaskId(1)), TaskControl::Cancel);
    state.ask_cancel_task("work", TaskId(1));
    let TaskControl::Confirm(text) = state.task_control("work", TaskId(1)) else {
        panic!("asks first");
    };
    assert!(text.starts_with("Cancel T-0001 for good?"), "{text}");
    state.keep_task();
    assert_eq!(state.task_control("work", TaskId(1)), TaskControl::Cancel);

    state.ask_cancel_task("work", TaskId(1));
    let cancelled = core
        .cancel_task(Some("work"), "T-0001")
        .await
        .expect("cancelled");
    state.task_cancelled("work", cancelled);
    assert_eq!(state.task_control("work", TaskId(1)), TaskControl::Ended);
}

#[tokio::test]
async fn a_failed_task_offers_retry_and_is_queued_again() {
    let work = FakeHub::new("Work", Vec::new());
    work.tasks
        .lock()
        .expect("lock")
        .push(task(1, TaskStatus::Failed, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    state.apply_task(load_task(&core, "work", TaskId(1)).await);
    assert_eq!(state.task_control("work", TaskId(1)), TaskControl::Retry);

    let retried = core
        .retry_task(Some("work"), "T-0001")
        .await
        .expect("retried");
    assert_eq!(retried.status, TaskStatus::Queued);
    state.apply_task(load_task(&core, "work", TaskId(1)).await);
    assert_eq!(state.task_control("work", TaskId(1)), TaskControl::Cancel);
}

#[tokio::test]
async fn a_task_without_a_repository_says_it_cannot_publish_until_one_is_attached() {
    let work = FakeHub::new("Work", Vec::new());
    work.tasks.lock().expect("lock").extend([
        task(1, TaskStatus::Sleeping, None),
        task(2, TaskStatus::Done, None),
    ]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    state.apply_task(load_task(&core, "work", TaskId(1)).await);
    state.apply_task(load_task(&core, "work", TaskId(2)).await);
    let line = |state: &AppState, id| state.task_publication("work", TaskId(id)).unwrap();
    assert_eq!(
        line(&state, 1).line,
        "Cannot publish: no repository is attached",
        "the project's repositories are not loaded yet"
    );
    state.apply_repositories("work", "api", Ok(Vec::new()));
    assert!(line(&state, 1).line.contains("has none registered"));

    let repository = |id, name: &str| Repository {
        id: RepositoryId(id),
        project_id: ProjectId(1),
        name: name.into(),
        remote_url: format!("/srv/git/{name}"),
        default_branch: "main".into(),
        protected_branches: Vec::new(),
    };
    state.apply_repositories(
        "work",
        "api",
        Ok(vec![repository(1, "app"), repository(2, "docs")]),
    );
    assert_eq!(
        line(&state, 1),
        Publication {
            line: "Cannot publish: no repository is attached".into(),
            attachable: vec!["app".into(), "docs".into()],
            blocked: false,
        }
    );
    assert_eq!(
        line(&state, 2),
        Publication {
            line: "Cannot publish: no repository was attached before it ended".into(),
            attachable: Vec::new(),
            blocked: false,
        },
        "an ended task cannot take one"
    );

    let blocker = "Cannot publish: no repository is attached. Attach app or docs from the task screen, or the lead can attach it with update_task.";
    work.tasks.lock().expect("lock")[0].task.publication_blocker = Some(blocker.into());
    state.apply_task(load_task(&core, "work", TaskId(1)).await);
    assert_eq!(
        line(&state, 1),
        Publication {
            line: blocker.into(),
            blocked: true,
            attachable: vec!["app".into(), "docs".into()],
        },
        "the hub's blocker stays until a repository is attached"
    );

    let attached = core
        .attach_repositories(Some("work"), "T-0001", vec!["app".into()])
        .await
        .expect("attached");
    assert_eq!(attached.repositories, [RepositoryId(1)]);
    state.apply_task(load_task(&core, "work", TaskId(1)).await);
    assert_eq!(
        line(&state, 1),
        Publication {
            line: "Can publish to app".into(),
            attachable: vec!["docs".into()],
            blocked: false,
        }
    );
}

#[tokio::test]
async fn pausing_a_project_shows_it_paused_until_resumed() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    assert!(!state.is_paused("work", "api"));
    let paused = core
        .set_paused(Some("work"), Some("api"), true)
        .await
        .expect("paused");
    state.project_updated("work", paused);
    assert!(state.is_paused("work", "api"));
    let resumed = core
        .set_paused(Some("work"), Some("api"), false)
        .await
        .expect("resumed");
    state.project_updated("work", resumed);
    assert!(!state.is_paused("work", "api"));
}

#[tokio::test]
async fn pausing_a_space_shows_it_paused_and_its_queued_tasks_say_why() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    let summary = task(7, TaskStatus::Queued, None);
    assert!(!state.is_space_paused("work"));
    let paused = core
        .set_space_paused(Some("work"), true)
        .await
        .expect("paused");
    state.space_paused_set("work", paused);
    assert!(state.is_space_paused("work"));
    assert_eq!(
        state.queued_reason("work", "api", &summary).as_deref(),
        Some("the space is paused")
    );
    let resumed = core
        .set_space_paused(Some("work"), false)
        .await
        .expect("resumed");
    state.space_paused_set("work", resumed);
    assert!(!state.is_space_paused("work"));
    assert_ne!(
        state.queued_reason("work", "api", &summary).as_deref(),
        Some("the space is paused")
    );
}

fn recorded_event(id: i64, text: &str) -> RecordedEvent {
    RecordedEvent {
        id: EventId(id),
        source: "github".to_owned(),
        kind: EventKind::OutsideEvent,
        text: text.to_owned(),
        received_at: Timestamp::from_millis(5 * 60_000),
    }
}

#[tokio::test]
async fn events_nobody_asked_for_are_a_collapsed_section_and_a_count_on_home() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(state.recorded("work", "api", &when()), None, "nothing yet");
    assert!(!state.status_line().contains("recorded"));

    work.recorded.lock().expect("lock").extend([
        recorded_event(
            2,
            "\nGitHub: review requested on acme/api, pull request #4\n\nBody",
        ),
        recorded_event(1, "Routine 3 fired."),
    ]);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    assert!(
        state.status_line().ends_with(" · 2 recorded, not acted on"),
        "{}",
        state.status_line()
    );
    let section = state.recorded("work", "api", &when()).expect("shown");
    assert_eq!(section.title, "Recorded, not acted on (2)");
    assert!(!section.open, "collapsed");
    assert_eq!(
        section.rows[0],
        RecordedRow {
            source: "github".to_owned(),
            text: "GitHub: review requested on acme/api, pull request #4".to_owned(),
            time: "00:05".to_owned(),
        }
    );
    assert_eq!(section.more, None);
    state.toggle_recorded("work", "api");
    assert!(state.recorded("work", "api", &when()).expect("shown").open);

    // A hub from before it could say hides the section and the count, and
    // is not asked again every few seconds for them.
    work.no_recorded.store(true, Ordering::SeqCst);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(state.recorded("work", "api", &when()), None);
    assert!(!state.status_line().contains("recorded"));
    assert!(!state.retry_soon());
}

#[tokio::test]
async fn a_decision_waiting_ten_minutes_is_reminded_about_once() {
    let minute = 60_000;
    let work = FakeHub::new("Work", vec![decision(1, 0), decision(2, 9 * minute)]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    let at = |millis| Timestamp::from_millis(millis);

    assert!(
        state.reminders(at(9 * minute)).is_empty(),
        "nothing is old yet"
    );
    let first = state.reminders(at(10 * minute));
    assert_eq!(first.len(), 1, "only D-0001 has waited ten minutes");
    assert_eq!(
        first[0].tag(),
        "work/D-0001",
        "its buttons answer the decision"
    );
    assert_eq!(decision_of_tag(&first[0].tag()), Some(("work", "D-0001")));
    assert!(first[0].title().starts_with("Still waiting: "));
    assert_eq!(first[0].options(), ["approve", "decline"]);
    state.apply(refresh(&core).await);
    assert!(
        state.reminders(at(11 * minute)).is_empty(),
        "one reminder only"
    );

    let answer = core
        .answer(Some("work"), "D-0002", "approve", None)
        .await
        .expect("answer");
    state.answered(&answer);
    assert!(
        state.reminders(at(30 * minute)).is_empty(),
        "an answered decision gets none"
    );
}

#[test]
fn notification_settings_are_kept_in_the_client_directory_and_applied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = AppState::paired(["work"]);
    state.set_client_dir(dir.path().to_owned());
    assert_eq!(state.notifications(), Notifications::default());
    let decision = Alert::Decision {
        space: "work".to_owned(),
        label: "work/api".to_owned(),
        card: Card {
            id: "D-0001".to_owned(),
            question: "Merge?".to_owned(),
            options: Vec::new(),
            kind: None,
            context: None,
            recommended: None,
        },
        reminder: false,
    };
    let notice = Alert::Notice {
        label: "work".to_owned(),
        text: "Blocked".to_owned(),
        seq: 1,
    };
    assert!(state.notifies(&decision) && state.notifies(&notice));

    state.set_notifications(false, false).expect("saved");
    assert!(state.notifies(&decision));
    assert!(!state.notifies(&notice));
    let mut again = AppState::paired(["work"]);
    again.set_client_dir(dir.path().to_owned());
    assert_eq!(
        again.notifications(),
        Notifications {
            decisions: true,
            notices: false
        }
    );
    assert_eq!(again.client_dir(), Some(dir.path()));
}

#[tokio::test]
async fn settings_show_what_each_hub_reports_and_how_it_is_reached() {
    let work = FakeHub::new("Work", Vec::new());
    work.machines.lock().expect("lock").push(Machine {
        is_hub: true,
        name: "studio".to_owned(),
        ..machine(9, MachineStatus::Online)
    });
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    assert_eq!(state.settings_rows()[0].space, None, "not asked yet");
    state.apply(refresh(&core).await);
    let row = &state.settings_rows()[0];
    assert_eq!(row.paired_as, "work");
    assert_eq!(row.space.as_deref(), Some("Work"));
    assert_eq!(row.ceiling, Some(Mode::Assisted));
    assert_eq!(row.schema_version, Some(1));
    assert_eq!(row.host.as_deref(), Some("studio"));
    assert!(
        row.reached_by.starts_with("Unix socket"),
        "{}",
        row.reached_by
    );

    // Away, it keeps what it last reported.
    work.down.store(true, Ordering::SeqCst);
    state.apply(refresh(&core).await);
    assert_eq!(state.settings_rows()[0].space.as_deref(), Some("Work"));
}

#[tokio::test]
async fn a_sleeping_card_claims_a_follow_up_only_when_one_is_listed() {
    let work = FakeHub::new("Work", Vec::new());
    *work.tasks.lock().expect("lock") = vec![
        task(
            6,
            TaskStatus::Sleeping,
            Some("ended without finishing and nothing is set to wake it"),
        ),
        task(7, TaskStatus::Sleeping, None),
        task(8, TaskStatus::Sleeping, None),
        task(9, TaskStatus::Sleeping, None),
    ];
    // Task 7's lane has an active follow-up at 09:00; task 9's is paused.
    let mut due = timer(1, ProjectId(1), None, TimerCreator::Agent);
    due.lane_id = LaneId(17);
    let mut paused = timer(2, ProjectId(1), None, TimerCreator::Agent);
    paused.lane_id = LaneId(19);
    paused.status = TimerStatus::Paused;
    work.timers.lock().expect("lock").extend([due, paused]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);
    state.apply_routines(load_routines(&core, vec![("work".to_owned(), "api".to_owned())]).await);

    let sleeping: Vec<String> = state
        .board("work", "api", &when())
        .into_iter()
        .find(|column| column.title == "Sleeping")
        .expect("a Sleeping column")
        .cards
        .into_iter()
        .map(|card| card.meta)
        .collect();
    assert_eq!(
        sleeping,
        [
            "Engineer · ended without finishing and nothing is set to wake it",
            "Engineer · sleeping until its follow-up at 09:00",
            "Engineer · sleeping",
            "Engineer · sleeping",
        ]
    );
}

/// A standing rule of Engineer in api, granted from decision 9.
fn standing_rule(id: i64, tool: &str, pattern: Json, uses: u32) -> StandingRuleSummary {
    StandingRuleSummary {
        rule: threadlane_core::model::StandingRule {
            id: threadlane_core::ids::StandingRuleId(id),
            project_id: ProjectId(1),
            agent_id: AgentId(2),
            tool: tool.to_owned(),
            pattern,
            tier: threadlane_core::tools::Tier::SharedReversible,
            created_from_decision: Some(DecisionId(9)),
            created_at: Timestamp::from_millis(0),
            revoked_at: None,
            uses,
        },
        project: "api".to_owned(),
        agent: "Engineer".to_owned(),
    }
}

#[test]
fn the_matrix_is_the_plans_table() {
    use threadlane_core::model::Cell::{Allow, Ask, Block, Review};
    use threadlane_core::tools::Tier;
    // Plan.md §2.3 "The full matrix", columns On request, Assisted,
    // Autonomous; a tier 3 standing rule is applied by the hub, not here.
    let table = [
        (Tier::Read, [Allow, Allow, Allow]),
        (Tier::Local, [Allow, Allow, Allow]),
        (Tier::SharedReversible, [Ask, Review, Review]),
        (Tier::SharedHardToUndo, [Ask, Ask, Ask]),
        (Tier::Forbidden, [Block, Block, Block]),
    ];
    for (tier, cells) in table {
        for (mode, cell) in [Mode::OnRequest, Mode::Assisted, Mode::Autonomous]
            .into_iter()
            .zip(cells)
        {
            assert_eq!(matrix(tier, mode), cell, "{tier:?} in {mode}");
        }
        assert_eq!(matrix(tier, Mode::Off), Block, "{tier:?} when off");
    }
    // Overrides: stricter always, looser only up to the ceiling's column.
    let set = |tier, cell| BTreeMap::from([(tier, cell)]);
    let tier2 = Tier::SharedReversible;
    assert_eq!(
        applied(tier2, Mode::Autonomous, Mode::Autonomous, &set(tier2, Ask)),
        Ask
    );
    assert_eq!(
        applied(tier2, Mode::OnRequest, Mode::Autonomous, &set(tier2, Allow)),
        Review
    );
    assert_eq!(
        applied(
            tier2,
            Mode::Autonomous,
            Mode::OnRequest,
            &set(tier2, Review)
        ),
        Ask
    );
    assert_eq!(
        applied(tier2, Mode::Autonomous, Mode::Assisted, &Default::default()),
        Review
    );
    assert_eq!(
        applied(Tier::Read, Mode::Assisted, Mode::Off, &Default::default()),
        Block
    );
}

#[tokio::test]
async fn the_agents_tab_locks_what_is_above_a_ceiling_and_states_the_matrix() {
    let work = FakeHub::new("Work", Vec::new());
    *work.bindings.lock().expect("lock") = vec![
        binding("Lead", Mode::OnRequest),
        BindingSummary {
            max_parallel: 3,
            ..binding("Engineer", Mode::Assisted)
        },
    ];
    *work.rules.lock().expect("lock") = vec![
        standing_rule(1, "open_pull_request", json!({ "repository": "app" }), 6),
        StandingRuleSummary {
            agent: "Lead".to_owned(),
            ..standing_rule(2, "publish_branch", json!({ "repository": "app" }), 1)
        },
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    // The space allows Assisted: Autonomous is locked on the ceiling card.
    let card = state.ceiling_card("work", "api");
    assert_eq!(
        card.note,
        "No agent in this project can go above this. Inherited from work."
    );
    let locked: Vec<_> = card
        .choices
        .iter()
        .map(|choice| (choice.label, choice.selected, choice.locked.is_some()))
        .collect();
    assert_eq!(
        locked,
        [
            ("Off", false, false),
            ("On request", false, false),
            ("Assisted", true, false),
            ("Autonomous", false, true),
        ]
    );
    assert_eq!(
        card.choices[3].locked.as_deref(),
        Some("Above work's ceiling, Assisted; raise it in Settings")
    );

    // A project ceiling of On request locks Assisted for every agent.
    core.set_ceiling(Some("work"), Some("api"), Mode::OnRequest)
        .await
        .expect("set");
    state.ceiling_set("work", "api", Mode::OnRequest);
    let rows = state.agent_rows("work", "api");
    let engineer = &rows[1];

    let modes: Vec<_> = engineer
        .choices
        .iter()
        .map(|choice| (choice.selected, choice.locked.clone()))
        .collect();
    let above = Some("Above this project's ceiling, On request".to_owned());
    assert_eq!(
        modes,
        [
            (false, None),
            (false, None),
            (true, above.clone()),
            (false, above)
        ],
        "its stored mode stays selected, and shows locked"
    );

    // Expanded: the matrix as the gate applies it, at the ceiling.
    assert!(!engineer.open);
    state.toggle_agent("work", "api", "Engineer");
    assert!(state.agent_rows("work", "api")[1].open);
    let may = state.may_do("work", "api", "Engineer").expect("bound");
    assert_eq!(may.title, "What Engineer may do here without asking");
    let cells: Vec<_> = may
        .statements
        .iter()
        .map(|statement| {
            (
                statement.what,
                statement.cell.as_str(),
                statement.choices.len(),
            )
        })
        .collect();
    assert_eq!(
        cells,
        [
            (
                "Start work you did not request",
                "Recorded, not acted on",
                0
            ),
            ("Read", "Yes", 4),
            ("Work locally", "Yes", 4),
            ("Shared, reversible", "Asks you", 2),
            ("Its own branch and draft pull request", "Yes", 0),
            ("Hard to undo", "Asks you", 2),
            ("Forbidden", "Blocked", 0),
        ]
    );
    assert!(may.tainted.contains("Engineer works one mode lower"));

    // An override makes a row stricter and can be cleared again.
    let overrides = core
        .set_override(
            Some("work"),
            Some("api"),
            "Engineer",
            threadlane_core::tools::Tier::Local,
            Some(threadlane_core::model::Cell::Ask),
        )
        .await
        .expect("set");
    state.overrides_set("work", "api", "Engineer", overrides);
    let may = state.may_do("work", "api", "Engineer").expect("bound");
    let local = &may.statements[2];
    assert_eq!(local.cell, "Asks you");
    assert!(
        local
            .choices
            .iter()
            .any(|c| c.selected && c.label == "Asks you")
    );
    assert_eq!(
        local.choices.last().map(|c| (c.cell, c.label.as_str())),
        Some((None, "As its mode sets it"))
    );

    // Its standing rules only, in words, and Revoke asks first.
    assert_eq!(may.rules_title, "Standing rule (1)");
    let rules = may.rules.expect("listed");
    assert_eq!(rules[0].allows, "Open draft pull requests in app");
    assert_eq!(rules[0].from, "Granted from D-0009 · used 6 times");
    assert_eq!(rules[0].confirm, None);
    state.ask_revoke("work", rules[0].id);
    let may = state.may_do("work", "api", "Engineer").expect("bound");
    assert_eq!(
        may.rules.expect("listed")[0].confirm.as_deref(),
        Some(
            "Revoke \"Open draft pull requests in app\" for Engineer? Its next matching call asks you again."
        )
    );
    let revoked = core.revoke_rule(Some("work"), "1").await.expect("revoked");
    state.rule_revoked("work", &revoked);
    let may = state.may_do("work", "api", "Engineer").expect("bound");
    assert_eq!(may.rules, Ok(Vec::new()), "a revoked rule leaves the list");
    assert_eq!(
        state
            .may_do("work", "api", "Lead")
            .expect("bound")
            .rules_title,
        "Standing rule (1)"
    );
}

#[tokio::test]
async fn always_allow_names_the_place_once_when_repository_and_project_match() {
    let mut offered = decision(1, 1_000);
    offered.offers_standing_rule = true;
    offered.arguments = json!({ "repository": "api" });
    let mut distinct = decision(2, 2_000);
    distinct.offers_standing_rule = true;
    distinct.arguments = json!({ "repository": "service in api" });
    let work = FakeHub::new("Work", vec![offered, distinct]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert_eq!(
        state.always_offer(&state.queue()[0]).map(|(line, _)| line),
        Some("Always allow the lead to open draft pull requests in api".to_owned())
    );
    assert_eq!(
        state.always_offer(&state.queue()[1]).map(|(line, _)| line),
        Some(
            "Always allow the lead to open draft pull requests in service in api, in api"
                .to_owned()
        )
    );
}

#[tokio::test]
async fn always_allow_is_worded_from_the_decision_and_used_only_when_ticked_to_approve() {
    let mut offered = decision(1, 1_000);
    offered.offers_standing_rule = true;
    offered.task_id = Some(TaskId(7));
    offered.arguments = json!({ "repository": "app", "title": "Fix it" });
    let work = FakeHub::new("Work", vec![offered, decision(2, 2_000)]);
    *work.tasks.lock().expect("lock") = vec![task(7, TaskStatus::WaitingOnYou, None)];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    let first = state.queue()[0].clone();
    assert_eq!(
        state.always_offer(&first),
        Some((
            "Always allow Engineer to open draft pull requests in app, in api".to_owned(),
            false
        ))
    );
    assert_eq!(
        state.always_offer(&state.queue()[1]),
        None,
        "offers no rule"
    );
    assert!(
        !state.answers_always("work", "D-0001", "approve"),
        "not ticked"
    );
    state.toggle_always("work", DecisionId(1));
    assert_eq!(
        state.always_offer(&first).map(|(_, ticked)| ticked),
        Some(true)
    );
    assert!(state.answers_always("work", "D-0001", "Approve"));
    assert!(
        !state.answers_always("work", "D-0001", "decline"),
        "only to approve"
    );
    assert!(!state.answers_always("work", "D-0002", "approve"));

    let answer = core
        .answer_always(Some("work"), "D-0001", "approve", None)
        .await
        .expect("answered");
    state.answered(&answer);
    assert_eq!(*work.always.lock().expect("lock"), [DecisionId(1)]);
    assert!(!state.answers_always("work", "D-0001", "approve"));
}

#[tokio::test]
async fn a_queued_card_says_why_it_waits_from_the_hub_or_from_what_the_app_knows() {
    let work = FakeHub::new("Work", Vec::new());
    let mut after = task(4, TaskStatus::Queued, None);
    after.task.depends_on = vec![TaskId(3)];
    let mut other = task(5, TaskStatus::Running, None);
    other.agent = "Reviewer".to_owned();
    *work.tasks.lock().expect("lock") = vec![
        task(1, TaskStatus::Running, None),
        task(
            2,
            TaskStatus::Queued,
            Some("its agent already runs its limit of 1 tasks at once in this project"),
        ),
        task(3, TaskStatus::Queued, None),
        after,
        other,
    ];
    *work.bindings.lock().expect("lock") = vec![binding("Engineer", Mode::OnRequest)];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    let queued: Vec<(TaskId, String)> = state
        .board("work", "api", &when())
        .into_iter()
        .find(|column| column.title == "Queued")
        .expect("queued")
        .cards
        .into_iter()
        .map(|card| (card.id, card.meta))
        .collect();
    assert_eq!(
        queued,
        [
            (
                TaskId(2),
                "Engineer · its agent already runs its limit of 1 tasks at once in this \
                 project: T-0001 holds it"
                    .to_owned()
            ),
            (TaskId(3), "Engineer · waiting for a machine".to_owned()),
            (TaskId(4), "Engineer · waits for T-0003".to_owned()),
        ]
    );

    // With no limit and every machine full, the machines are why.
    *work.bindings.lock().expect("lock") = vec![BindingSummary {
        max_parallel: 0,
        ..binding("Engineer", Mode::OnRequest)
    }];
    *work.machines.lock().expect("lock") = vec![Machine {
        load: 1,
        ..machine(1, MachineStatus::Online)
    }];
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    let summary = task(3, TaskStatus::Queued, None);
    assert_eq!(
        state.queued_reason("work", "api", &summary).as_deref(),
        Some("every machine is busy: machine-1 runs 1 of 1")
    );
    *work.machines.lock().expect("lock") = vec![machine(1, MachineStatus::Online)];
    state.apply(refresh(&core).await);
    assert_eq!(state.queued_reason("work", "api", &summary), None);

    // A paused project says so.
    state.project_updated(
        "work",
        ProjectSummary {
            id: ProjectId(1),
            name: "api".to_owned(),
            is_general: false,
            paused: true,
            mode_ceiling: None,
            archived: false,
        },
    );
    assert_eq!(
        state.queued_reason("work", "api", &summary).as_deref(),
        Some("the project is paused")
    );
}

#[tokio::test]
async fn recovered_task_failures_stay_on_the_board_and_only_open_decisions_need_you() {
    let work = FakeHub::new("Work", Vec::new());
    let mut cancelled = task(18, TaskStatus::Cancelled, None);
    cancelled.task.depends_on = vec![TaskId(16)];
    *work.tasks.lock().expect("lock") = vec![
        task(16, TaskStatus::Failed, Some("the tests do not build")),
        cancelled,
        task(
            19,
            TaskStatus::Sleeping,
            Some("ended a run without finishing; nothing is set to wake it"),
        ),
        task(17, TaskStatus::Running, None),
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    assert_eq!(state.needs_you().count(), 0);
    assert_eq!(state.open_in("work", ProjectId(1)), 0);
    assert_eq!(state.running()[0].1.task.id, TaskId(17));
    let board = state.board("work", "api", &when());
    for (status, id) in [("Failed", 16), ("Cancelled", 18), ("Sleeping", 19)] {
        let column = board.iter().find(|column| column.title == status).unwrap();
        assert_eq!(column.cards[0].id, TaskId(id));
    }

    work.decisions.lock().expect("lock").push(Decision {
        tool: "ask_user".to_owned(),
        kind: DecisionKind::Input,
        question: "How should I recover T-0016?".to_owned(),
        ..decision(1, 1_000)
    });
    state.apply(refresh(&core).await);
    let queued = state
        .needs_you()
        .next()
        .expect("Lead's question is on Home");
    assert_eq!(queued.decision.question, "How should I recover T-0016?");
    assert_eq!(state.open_in("work", ProjectId(1)), 1);

    let answer = core
        .answer(Some("work"), "D-0001", "approve", None)
        .await
        .expect("answer");
    state.answered(&answer);
    assert_eq!(state.needs_you().count(), 0);
    assert_eq!(state.open_in("work", ProjectId(1)), 0);
}

#[tokio::test]
async fn the_space_ceiling_is_set_from_settings_after_saying_what_it_permits() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    let selected = |state: &AppState| {
        state
            .space_ceiling_choices("work")
            .into_iter()
            .find(|choice| choice.selected)
            .map(|choice| choice.mode)
    };
    assert_eq!(selected(&state), Some(Mode::Assisted));

    state.ask_space_ceiling("work", Mode::Autonomous);
    let (mode, text) = state.space_ceiling_confirm("work").expect("asked");
    assert_eq!(mode, Mode::Autonomous);
    assert!(
        text.starts_with("Set the ceiling of work to Autonomous?"),
        "{text}"
    );
    assert!(text.contains("start work on their own"), "{text}");
    assert_eq!(state.space_ceiling_confirm("home"), None);
    state.keep_space_ceiling();
    assert_eq!(state.space_ceiling_confirm("work"), None);

    state.ask_space_ceiling("work", Mode::Autonomous);
    let set = core
        .set_space_ceiling(Some("work"), Mode::Autonomous)
        .await
        .expect("set");
    state.space_ceiling_set("work", set);
    assert_eq!(state.space_ceiling_confirm("work"), None);
    assert_eq!(selected(&state), Some(Mode::Autonomous));
    assert!(
        state.ceiling_card("work", "api").choices[3]
            .locked
            .is_none(),
        "the project may now go to Autonomous"
    );
}

#[tokio::test]
async fn proposals_and_questions_have_distinct_cards_and_notifications() {
    let mut proposal = decision(1, 1_000);
    proposal.kind = DecisionKind::Proposal;
    proposal.proposed_by = Some("Coordinator".to_owned());
    proposal.arguments = json!({ "agent": "Engineer", "repositories": ["api", "web"] });
    let mut input = decision(2, 2_000);
    input.kind = DecisionKind::Input;
    input.recommended = None;
    input.arguments = json!({ "question": "Where shall this go?" });
    input.options[0].label = "Staging".to_owned();
    let work = FakeHub::new("Work", vec![proposal, input]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    let mut cards = state.needs_you().map(AppState::decision_card);
    let proposal = cards.next().unwrap();
    assert_eq!(proposal.chip, Some("Proposal"));
    assert!(proposal.dashed);
    assert_eq!(proposal.recommended, Some(0));
    assert_eq!(
        proposal.context.as_deref(),
        Some("You did not ask for this. Coordinator is Assisted here, so it proposes first.")
    );
    assert_eq!(
        proposal.would_run_as.as_deref(),
        Some("Would run as: Engineer · api, web")
    );
    let question = cards.next().unwrap();
    assert_eq!(question.chip, Some("Question"));
    assert!(!question.dashed);
    assert_eq!(question.note_label, "Your answer (optional)");
    assert!(question.details.is_empty());
    assert!(question.recommended.is_none());
    drop(cards);

    for (seq, kind, option) in [(1, "proposal", "start"), (2, "question", "Staging")] {
        let alert = state.receive(item(
            "work",
            seq,
            OutboxKind::Decision,
            json!({ "decision": format!("D-{seq:04}"), "kind": if seq == 1 { "proposal" } else { "input" },
                "question": "Decide?", "options": [option] }),
        )).unwrap();
        assert_eq!(alert.title(), format!("work/api: {kind} D-{seq:04}"));
        assert_eq!(alert.options(), [option]);
    }
}

#[tokio::test]
async fn the_proposed_column_answers_open_proposals_in_the_same_project_and_space() {
    let mut proposal = decision(1, 1_000);
    proposal.kind = DecisionKind::Proposal;
    proposal.arguments = json!({ "title": "Fix the build", "agent": "Engineer" });
    proposal.options[0].label = "start".to_owned();
    proposal.options[1].label = "not now".to_owned();
    proposal.options.insert(
        0,
        DecisionOption {
            label: "never again".to_owned(),
            effect: json!({ "approve": false, "never_again": true }),
        },
    );
    let mut second = proposal.clone();
    second.id = DecisionId(2);
    let mut other_project = proposal.clone();
    other_project.id = DecisionId(3);
    other_project.project_id = ProjectId(2);
    let work = FakeHub::new(
        "Work",
        vec![proposal.clone(), second, other_project, decision(4, 0)],
    );
    let home = FakeHub::new("Home", vec![proposal]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    let board = state.board("work", "api", &when());
    assert_eq!(board[0].title, "Proposed");
    let [first, second] = &board[0].proposals[..] else {
        panic!("two open proposals");
    };
    assert_eq!(first.title, "Fix the build");
    assert_eq!(first.meta, "Lead proposes · Engineer");
    for (card, option, approved) in [
        (first, &first.start, true),
        (second, &second.decline, false),
    ] {
        let answer = core
            .answer(
                Some("work"),
                &card.id.to_string(),
                option.as_deref().unwrap(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(answer.decision.approved(), approved);
        assert_eq!(
            answer.decision.chosen().unwrap().label,
            if approved { "start" } else { "not now" }
        );
        state.answered(&answer);
    }
    assert_ne!(state.board("work", "api", &when())[0].title, "Proposed");
    assert_eq!(
        state.board("work", "General", &when())[0].proposals.len(),
        1
    );
    assert_eq!(state.board("home", "api", &when())[0].proposals.len(), 1);
}

#[tokio::test]
async fn quiet_lead_runs_group_in_chronological_order_and_reveal_why() {
    let work = FakeHub::new("Work", Vec::new());
    let at = |minutes: i64| Timestamp::from_millis(minutes * 60_000);
    *work.lead_runs.lock().unwrap() = [
        (
            1,
            8 * 60 + 55,
            Some("Already handled"),
            Some(RunOutcome::Silent),
        ),
        (2, 9 * 60 + 8, None, Some(RunOutcome::Silent)),
        (3, 9 * 60 + 10, Some("I replied"), Some(RunOutcome::Sent)),
        (4, 14 * 60 + 52, None, Some(RunOutcome::Deferred)),
        (5, 15 * 60, None, None),
    ]
    .into_iter()
    .map(|(id, minutes, rationale, outcome)| LeadRun {
        latest_step: None,
        usage: None,
        sent_message: outcome == Some(RunOutcome::Sent),
        run: Run {
            trace_id: String::new(),
            id: RunId(id),
            lane_id: LaneId(1),
            session_id: SessionId(1),
            machine_id: MachineId(1),
            lease_epoch: 1,
            agent_id: None,
            backend: None,
            status: if outcome.is_some() {
                RunStatus::Committed
            } else {
                RunStatus::Active
            },
            outcome,
            rationale: rationale.map(str::to_owned),
            error: None,
            started_at: at(minutes - 1),
            ended_at: outcome.map(|_| at(minutes)),
        },
    })
    .collect();
    *work.history.lock().unwrap() = vec![
        ConversationEntry::You {
            event_id: EventId(1),
            text: "Look at the build".to_owned(),
            images: Vec::new(),
            at: at(8 * 60 + 41),
        },
        ConversationEntry::Delivered {
            item: feed_item(
                1,
                OutboxKind::Message,
                json!({ "text": "On it." }),
                at(9 * 60 + 14).as_millis(),
            ),
        },
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);
    let lines = state.conversation("work", "api", &when());
    assert!(matches!(
        &lines[..],
        [
            Line::Day(_),
            Line::You { .. },
            Line::Quiet { .. },
            Line::Lead { .. },
            Line::Quiet { .. }
        ]
    ));
    assert!(
        matches!(&lines[2], Line::Quiet { text, toggle: "Why", reasons, .. } if
        text == "Lead ran twice and stayed quiet · 08:55, 09:08" && reasons.is_empty())
    );
    assert!(
        matches!(&lines[4], Line::Quiet { text, .. } if text == "Lead ran and stayed quiet · 14:52")
    );

    state.toggle_quiet("work", RunId(1));
    state.toggle_quiet("work", RunId(4));
    let lines = state.conversation("work", "api", &when());
    let Line::Quiet {
        reasons,
        toggle: "Hide",
        ..
    } = &lines[2]
    else {
        panic!("expanded quiet group");
    };
    assert_eq!(
        reasons
            .iter()
            .map(|reason| (reason.time.as_str(), reason.text.as_str()))
            .collect::<Vec<_>>(),
        [
            ("08:55", "Already handled"),
            ("09:08", "The run ended silently.")
        ]
    );
    assert!(
        matches!(&lines[4], Line::Quiet { reasons, .. } if reasons[0].text == "Stopped to ask you.")
    );
}

#[tokio::test]
async fn running_elapsed_advances_on_home_and_the_task_without_reloading() {
    let work = FakeHub::new("Work", Vec::new());
    let mut running = task(7, TaskStatus::Running, None);
    running.machine = Some("hub".to_owned());
    let started = Timestamp::from_millis(2 * 60_000);
    running.running_since = Some(started);
    *work.tasks.lock().unwrap() = vec![running.clone()];
    *work.task_runs.lock().unwrap() = vec![RunSummary {
        steps: Vec::new(),
        usage: None,
        machine: "hub".to_owned(),
        run: Run {
            trace_id: String::new(),
            id: RunId(1),
            lane_id: running.task.lane_id,
            session_id: SessionId(1),
            machine_id: MachineId(1),
            lease_epoch: 1,
            agent_id: Some(running.task.agent_id),
            backend: Some(BackendKind::Claude),
            status: RunStatus::Active,
            outcome: None,
            rationale: None,
            error: None,
            started_at: started,
            ended_at: None,
        },
    }];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_task(load_task(&core, "work", TaskId(7)).await);

    let home = state.running()[0].1;
    let detail = state.task("work", TaskId(7)).unwrap();
    for (now, want) in [(2 * 60_000, "less than a minute"), (8 * 60_000, "6 min")] {
        work.now.store(now, Ordering::SeqCst);
        let when = work.when();
        assert_eq!(running_elapsed(home, &when).as_deref(), Some(want));
        assert!(chips(detail, &when).contains(&format!("Running · {want}")));
        assert!(timeline(detail, &when).iter().any(|moment| {
            moment.text == format!("Engineer on hub with claude: working for {want}")
        }));
    }
    work.tasks.lock().unwrap()[0].task.status = TaskStatus::Done;
    {
        let mut runs = work.task_runs.lock().unwrap();
        runs[0].run.status = RunStatus::Committed;
        runs[0].run.ended_at = Some(work.when().now);
    }
    state.apply(refresh(&core).await);
    state.apply_task(load_task(&core, "work", TaskId(7)).await);
    work.now.store(15 * 60_000, Ordering::SeqCst);
    let when = work.when();
    let detail = state.task("work", TaskId(7)).unwrap();
    assert!(state.running().is_empty());
    assert!(chips(detail, &when).contains(&"Done".to_owned()));
    assert!(timeline(detail, &when).iter().any(|moment| {
        moment.text == "Engineer on hub with claude: worked for 6 min, ended quietly"
    }));
}

#[tokio::test]
async fn the_done_column_shows_the_five_most_recent_completions() {
    let work = FakeHub::new("Work", Vec::new());
    *work.tasks.lock().unwrap() = (1..=7)
        .map(|id| {
            let mut task = task(id, TaskStatus::Done, None);
            task.completed_at = Some(Timestamp::from_millis((8 - id) * 60_000));
            task
        })
        .collect();
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);
    let board = state.board("work", "api", &when());
    let done = board.iter().find(|column| column.title == "Done").unwrap();
    assert_eq!(done.count, 7);
    assert_eq!(done.more.as_deref(), Some("2 more…"));
    assert_eq!(
        done.cards
            .iter()
            .map(|card| card.id.get())
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5],
    );
    assert_eq!(done.cards[0].meta, "Engineer · 00:07");
}

#[tokio::test]
async fn stage_b_stops_running_lanes_and_sending_a_message_resumes_them() {
    let work = FakeHub::new("Work", Vec::new());
    let at = Timestamp::from_millis(60_000);
    *work.lead.lock().unwrap() = Some(at);
    work.tasks
        .lock()
        .unwrap()
        .push(task(7, TaskStatus::Running, None));
    let stopped_run = Run {
        trace_id: String::new(),
        id: RunId(1),
        lane_id: LaneId(1),
        session_id: SessionId(1),
        machine_id: MachineId(1),
        lease_epoch: 1,
        agent_id: None,
        backend: None,
        status: RunStatus::Stopped,
        outcome: None,
        rationale: None,
        error: None,
        started_at: at,
        ended_at: Some(at),
    };
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    let id = TaskId(7);
    state.apply_project(load_project(&core, "work", "api").await);
    state.apply_task(load_task(&core, "work", id).await);
    let lead = state.lead_stop("work", "api").unwrap();
    let task_target = state.task_stop("work", id).unwrap();
    assert_eq!(task_target, LaneTarget::Task { task: id });
    for target in [lead, task_target] {
        assert!(state.begin_stop("work", &target));
        assert!(!state.begin_stop("work", &target));
        let result = core.stop(Some("work"), target.clone()).await;
        assert!(result.is_ok());
        state.finish_stop("work", &target, result);
    }
    assert!(state.lead_stop("work", "api").is_none());
    assert!(state.task_stop("work", id).is_none());
    assert!(
        state
            .lead_line("work", "api", &when())
            .unwrap()
            .starts_with("Stopping")
    );

    *work.lead.lock().unwrap() = None;
    work.tasks.lock().unwrap()[0].task.status = TaskStatus::Queued;
    work.lead_runs.lock().unwrap().push(LeadRun {
        latest_step: None,
        usage: None,
        run: stopped_run.clone(),
        sent_message: true,
    });
    work.task_runs.lock().unwrap().push(RunSummary {
        steps: Vec::new(),
        usage: None,
        run: Run {
            lane_id: LaneId(17),
            ..stopped_run
        },
        machine: "hub".to_owned(),
    });
    state.apply_project(load_project(&core, "work", "api").await);
    state.apply_task(load_task(&core, "work", id).await);
    assert_eq!(
        state.lead_line("work", "api", &when()).as_deref(),
        Some("Lead stopped. Send a message to resume.")
    );
    assert!(
        state
            .conversation("work", "api", &when())
            .iter()
            .any(|line| matches!(line, Line::Stopped { .. }))
    );
    let detail = state.task("work", id).unwrap();
    assert!(chips(detail, &when()).contains(&"Stopped".to_owned()));
    assert!(
        timeline(detail, &when())
            .iter()
            .any(|moment| moment.text.contains("stopped by you"))
    );
    assert_eq!(
        state.task_hint("work", id),
        "Send a message to resume this task."
    );

    let message = core
        .send(Some("work"), Some("api"), "Continue")
        .await
        .unwrap();
    state.sent("work", "api", "Continue", message.accepted.event_id, at);
    let message = core
        .send_to_task(Some("work"), "T-0007", "Continue")
        .await
        .unwrap();
    state.sent_to_task("work", id, "Continue", message.accepted.event_id, at);
    state.apply_project(load_project(&core, "work", "api").await);
    state.apply_task(load_task(&core, "work", id).await);
    assert_eq!(state.lead_line("work", "api", &when()), None);
    assert!(!state.task("work", id).unwrap().stopped);
    assert!(work.stopped.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stage_b_pacing_edits_and_held_results_are_scoped_to_each_space() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    let next = Timestamp::from_millis(8 * 60 * 60_000);
    *work.held.lock().unwrap() = Some(HeldResults {
        count: 3,
        next,
        latest: next,
    });
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert_eq!(
        state.held_lines(&when()),
        ["work · 3 results held until 08:00"]
    );
    state.set_filter(Some("home"));
    assert!(state.held_lines(&when()).is_empty());

    let pacing = core.pacing(Some("work")).await;
    state.apply_pacing("work".to_owned(), pacing);
    assert_eq!(
        state.edit_pacing("work").unwrap(),
        ["Off", "Off", "", "Off"]
    );
    assert!(
        state
            .submit_pacing("work", ["0", "Off", "", "Off"])
            .is_none()
    );
    assert!(
        state
            .submit_pacing("work", ["5", "22:00–08:00", "", "10"])
            .is_none()
    );
    let draft = state
        .submit_pacing("work", ["5", "22:00–08:00", "America/Toronto", "10"])
        .unwrap();
    let saved = core.set_pacing(Some("work"), draft.clone()).await;
    state.finish_pacing("work", saved);
    assert_eq!(state.pacing_editor("work"), None);
    assert_eq!(
        state.pacing_values("work").unwrap(),
        ["5", "22:00–08:00", "America/Toronto", "10"]
    );
    assert_eq!(core.pacing(Some("work")).await.unwrap(), draft);
    assert_eq!(core.pacing(Some("home")).await.unwrap(), Pacing::default());
    assert_eq!(
        parse_pacing(["Off", "Off", "America/Toronto", ""]).unwrap(),
        Pacing::default()
    );
}

#[tokio::test]
async fn stage_b_digest_is_one_entry_on_home_and_in_each_project_conversation() {
    use threadlane_core::client::DigestResult;
    let work = FakeHub::new("Work", Vec::new());
    let mut item = feed_item(1, OutboxKind::Message, json!({"text": "Digest"}), 2_000);
    item.project_name = None;
    item.digest = vec![
        DigestResult {
            id: OutboxId(1),
            project: Some("api".to_owned()),
            text: "Fixed the API".to_owned(),
        },
        DigestResult {
            id: OutboxId(2),
            project: Some("General".to_owned()),
            text: "Reported back".to_owned(),
        },
    ];
    *work.recent.lock().unwrap() = vec![item.clone()];
    *work.history.lock().unwrap() = vec![ConversationEntry::Delivered { item }];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(
        state.recent().len(),
        1,
        "history and Home share one feed position"
    );
    assert!(state.digest(state.recent()[0]).unwrap().results.is_empty());
    assert!(state.conversation("work", "other", &when()).is_empty());
    state.toggle_result("work", 1);
    let home = state.digest(state.recent()[0]).unwrap();
    assert_eq!(
        home.results
            .iter()
            .map(|result| result.project.as_deref())
            .collect::<Vec<_>>(),
        [Some("api"), Some("General")]
    );
    for project in ["api", "General"] {
        let lines = state.conversation("work", project, &when());
        assert!(
            matches!(&lines[..], [Line::Day(_), Line::Digest { digest, .. }] if *digest == home)
        );
    }
}

#[tokio::test]
async fn stage_b_a_dead_delivery_is_shown_once_in_its_own_conversation() {
    let work = FakeHub::new("Work", Vec::new());
    let text = "Delivery of message 3 to client failed: adapter unavailable";
    *work.history.lock().unwrap() = vec![ConversationEntry::DeliveryFailure {
        event_id: EventId(4),
        text: text.to_owned(),
        at: Timestamp::from_millis(3_000),
    }];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    for _ in 0..2 {
        state.apply_project(load_project(&core, "work", "api").await);
    }
    assert!(matches!(&state.conversation("work", "api", &when())[..],
        [Line::Day(_), Line::DeliveryFailure { text: shown, .. }] if shown == text));
    assert!(state.conversation("work", "General", &when()).is_empty());
    assert!(state.conversation("home", "api", &when()).is_empty());
    assert!(state.recent().is_empty());
}

#[tokio::test]
async fn stage_b_composer_lists_every_project_and_restores_each_space_after_restart() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    home.created.lock().unwrap().push(ProjectSummary {
        id: ProjectId(3),
        name: "garden".to_owned(),
        is_general: false,
        paused: false,
        mode_ceiling: None,
        archived: false,
    });
    let (dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.set_client_dir(dir.path().to_owned());
    state.apply(refresh(&core).await);
    state.set_filter(Some("home"));
    assert_eq!(state.target(), Some(("home", "General")));
    assert_eq!(
        state.targets(),
        [
            ("work", "api"),
            ("work", "General"),
            ("home", "api"),
            ("home", "General"),
            ("home", "garden")
        ]
    );
    state.set_target("home", "garden");
    state.set_target("work", "api");
    state.set_notifications(true, false).unwrap();
    state.set_filter(Some("home"));
    assert_eq!(state.target(), Some(("home", "garden")));
    home.projects_fail.store(true, Ordering::SeqCst);
    state.apply(refresh(&core).await);
    assert!(state.targets().contains(&("home", "garden")));
    assert!(matches!(
        state.space("home").unwrap().gap(Part::Projects),
        Some(Gap::Failed(_))
    ));
    home.projects_fail.store(false, Ordering::SeqCst);

    let mut state = AppState::default();
    state.set_client_dir(dir.path().to_owned());
    state.apply(refresh(&core).await);
    assert!(
        !state.notifications().decisions,
        "writing one setting preserves the other"
    );
    assert_eq!(state.target(), Some(("work", "api")));
    state.set_filter(Some("home"));
    assert_eq!(state.target(), Some(("home", "garden")));
}

#[tokio::test]
async fn stage_b_lead_mode_obeys_the_ceiling_and_agent_placement_uses_backend_probes() {
    use threadlane_core::backend::{Probe, encode_backends};

    let work = FakeHub::new("Work", Vec::new());
    work.bindings.lock().unwrap().push(BindingSummary {
        agent: "Lead".to_owned(),
        role: "lead".to_owned(),
        mode: Mode::Autonomous,
        is_lead: true,
        max_parallel: 1,
        overrides: Default::default(),
    });
    let mut local = machine(1, MachineStatus::Online);
    local.backends = encode_backends(&[(
        BackendKind::Claude,
        Probe {
            installed: true,
            version: None,
            logged_in: true,
            account_label: None,
        },
    )]);
    work.machines.lock().unwrap().push(local);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(state.lead_mode("work", "api"), Some(Mode::Assisted));

    let mut agent = Agent {
        id: AgentId(1),
        name: "Lead".to_owned(),
        role: "lead".to_owned(),
        instructions: String::new(),
        backends: vec![BackendKind::Claude],
        models: json!({}),
        tools: json!({}),
        skills: Vec::new(),
        requirements: json!({}),
    };
    assert_eq!(
        state.agent_runs_on("work", &agent).as_deref(),
        Some("Runs on machine-1")
    );
    agent.backends = vec![BackendKind::Codex];
    assert_eq!(
        state.agent_runs_on("work", &agent).as_deref(),
        Some("No online machine has a usable backend.")
    );
}

#[tokio::test]
async fn dreaming_card_applies_project_settings_versions_and_findings() {
    use threadlane_core::client::{DreamChange, DreamSummary, MemoryFinding};

    let work = FakeHub::new("Work", vec![]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    let when = work.when();
    assert!(state.dreaming_card("work", "api", &when).disabled);

    work.dreaming
        .lock()
        .unwrap()
        .entry("api".into())
        .or_default()
        .last = Some(DreamSummary {
        at: Timestamp::from_millis(120_000),
        changes: vec![DreamChange {
            path: "project/notes/obsolete.md".into(),
            version: 2,
            deleted: true,
        }],
    });
    work.findings
        .lock()
        .unwrap()
        .push(MemoryFinding::MissingTask {
            note: "project/notes/guide.md".into(),
            task: TaskId(999),
        });
    let stale = load_dreaming(&core, "work", "api", 0).await;
    state.apply_dreaming(load_dreaming(&core, "work", "api", 0).await);
    let card = state.dreaming_card("work", "api", &when);
    assert!(!card.enabled && !card.disabled);
    assert_eq!(card.last, ["Last dream · 00:02"]);
    assert_eq!(card.changes[0].path, "project/notes/obsolete.md");
    assert_eq!(card.changes[0].version, 2);
    assert!(card.changes[0].deleted);
    assert!(card.findings[0].contains("T-0999"));

    let on = state
        .dreaming_request(
            "work",
            "api",
            DreamingAction::Toggle(true),
            "America/Toronto",
        )
        .unwrap()
        .unwrap();
    assert!(state.dreaming_card("work", "api", &when).disabled);
    assert!(
        state
            .dreaming_request("work", "api", DreamingAction::Toggle(false), "UTC")
            .unwrap()
            .is_none()
    );
    let saved = change_dreaming(&core, "work", on).await;
    state.dreaming_saved("work", "api", saved);
    state.apply_dreaming(stale);
    let card = state.dreaming_card("work", "api", &when);
    assert!(card.enabled && card.cadence.contains("02:00 America/Toronto"));
    assert!(!state.dreaming_card("work", "General", &when).enabled);
    assert!(
        state
            .dreaming_request("work", "api", DreamingAction::Cadence("every 0h"), "UTC")
            .is_err()
    );
    let cadence = state
        .dreaming_request("work", "api", DreamingAction::Cadence("every 6h"), "UTC")
        .unwrap()
        .unwrap();
    state.dreaming_saved("work", "api", change_dreaming(&core, "work", cadence).await);
    assert_eq!(
        state.dreaming_card("work", "api", &when).cadence,
        "Every 6 h"
    );

    let off = state
        .dreaming_request("work", "api", DreamingAction::Toggle(false), "UTC")
        .unwrap()
        .unwrap();
    state.dreaming_saved("work", "api", change_dreaming(&core, "work", off).await);
    assert!(!state.dreaming_card("work", "api", &when).enabled);
    let again = state
        .dreaming_request("work", "api", DreamingAction::Toggle(true), "UTC")
        .unwrap()
        .unwrap();
    state.dreaming_saved("work", "api", change_dreaming(&core, "work", again).await);
    assert_eq!(
        state.dreaming_card("work", "api", &when).cadence,
        "Every 6 h"
    );
}

#[tokio::test]
async fn metrics_cards_show_each_hubs_snapshot_and_its_own_error() {
    let work = FakeHub::new("Work", vec![]);
    let home = FakeHub::new("Home", vec![]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    let when = work.when();
    assert_eq!(state.metrics_rows("work", &when), Err("Loading metrics…"));
    *work.metrics.lock().unwrap() = Some(serde_json::from_value(json!({
        "at": 0, "lane_lag_ms": {"1": 53, "2": 22}, "queue_depth": {"api": 3},
        "run_duration": {"count": 1, "mean_ms": 12.0, "max_ms": 12},
        "account_cooldowns": {}, "account_wait_ms": 83, "dead_events": 2,
        "timer_lateness": {"count": 2, "mean_ms": 24_520.0, "max_ms": 7_200_000},
        "outbox_lag_ms": 125_000, "messages_per_day": {"2026-01-01": 4},
        "silent_run_ratio": 0.25, "decisions_per_day": 5,
        "answer_latency": {"count": 0, "mean_ms": null, "max_ms": null}, "standing_rules_created": 1
    })).unwrap());
    for space in ["work", "home"] {
        state.apply_metrics(space.to_owned(), load_metrics(&core, space).await);
    }
    let rows: BTreeMap<_, _> = state
        .metrics_rows("work", &when)
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(rows["Project queues"], "api 3");
    assert_eq!(rows["Oldest pending event"], "53 ms");
    assert_eq!(rows["Outbox lag"], "2 min");
    assert_eq!(rows["Delivery health"], "Unavailable on this hub");
    assert_eq!(
        rows["Run duration"],
        "12 ms avg · 12 ms max · 1 observation"
    );
    assert_eq!(
        rows["Timer lateness"],
        "24.5 s avg · 2 h 0 min max · 2 observations"
    );
    assert_eq!(rows["Decisions in 24 hours"], "5 by you · 0 automatic");
    assert_eq!(rows["Silent runs"], "25%");
    assert!(
        state
            .metrics_rows("home", &when)
            .unwrap_err()
            .contains("metrics unavailable")
    );
    let mut current = work.metrics.lock().unwrap().clone().unwrap();
    current.delivery_health = Some(threadlane_core::client::DeliveryHealth {
        held: 0,
        next_hold_check: None,
        retrying: 1,
        next_retry: Some(current.at),
        failed_created_in_last_week: 2,
    });
    *work.metrics.lock().unwrap() = Some(current);
    state.apply_metrics("work".to_owned(), load_metrics(&core, "work").await);
    let rows: BTreeMap<_, _> = state
        .metrics_rows("work", &when)
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(
        rows["Delivery health"],
        "0 held; 1 retrying · retry due; 2 permanently failed (created in the past 7 days)"
    );
}

#[test]
fn the_selected_space_round_trips_and_falls_back_to_the_first() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(SETTINGS_FILE), r#"{"appearance":"dark"}"#).unwrap();
    let mut state = AppState::paired(["dev", "ops"]);
    state.set_client_dir(dir.path().to_owned());
    assert_eq!(
        state.selected_space().map(|space| space.name.as_str()),
        Some("dev")
    );
    state.select_space("ops");

    let mut reloaded = AppState::paired(["dev", "ops"]);
    reloaded.set_client_dir(dir.path().to_owned());
    assert_eq!(
        reloaded.selected_space().map(|space| space.name.as_str()),
        Some("ops")
    );
    assert_eq!(reloaded.appearance(), Appearance::Dark);

    // A saved space that is no longer paired shows the first one.
    let mut unpaired = AppState::paired(["dev"]);
    unpaired.set_client_dir(dir.path().to_owned());
    assert_eq!(
        unpaired.selected_space().map(|space| space.name.as_str()),
        Some("dev")
    );
}

#[test]
fn desktop_appearance_round_trips_with_existing_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(SETTINGS_FILE);
    std::fs::write(
        &path,
        r#"{"notifications":{"decisions":false},"last_projects":{"work":"general"}}"#,
    )
    .unwrap();
    let mut state = AppState::default();
    state.set_client_dir(dir.path().to_owned());
    assert_eq!(state.appearance(), Appearance::System);

    for chosen in [Appearance::Dark, Appearance::Light, Appearance::System] {
        state.set_appearance(chosen).unwrap();
        let mut reloaded = AppState::default();
        reloaded.set_client_dir(dir.path().to_owned());
        assert_eq!(reloaded.appearance(), chosen);
        assert!(!reloaded.notifications().decisions);
        assert_eq!(
            reloaded.last_targets.get("work").map(String::as_str),
            Some("general")
        );
    }

    std::fs::write(&path, r#"{"appearance":"unknown"}"#).unwrap();
    state.set_client_dir(dir.path().to_owned());
    assert_eq!(state.appearance(), Appearance::System);
}

#[tokio::test]
async fn decision_keys_step_through_home_and_only_the_open_project() {
    let mut general = decision(2, 2_000);
    general.project_id = ProjectId(2);
    let hub = FakeHub::new(
        "Work",
        vec![decision(3, 3_000), general, decision(1, 1_000)],
    );
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    let current = |state: &AppState| state.current_decision().map(|row| row.decision.id);
    state.set_decision_scope(DecisionScope::Home);
    assert_eq!(current(&state), Some(DecisionId(1)));
    assert_eq!(
        state.decision_command(DecisionCommand::Next, false),
        Some(DecisionEffect::Scroll)
    );
    assert_eq!(current(&state), Some(DecisionId(2)));
    state.decision_command(DecisionCommand::Previous, false);
    assert_eq!(current(&state), Some(DecisionId(1)));

    state.set_decision_scope(DecisionScope::Project("work".into(), "api".into()));
    assert_eq!(current(&state), Some(DecisionId(1)));
    state.decision_command(DecisionCommand::Next, false);
    assert_eq!(current(&state), Some(DecisionId(3)));
    state.decision_command(DecisionCommand::Previous, false);
    assert_eq!(current(&state), Some(DecisionId(1)));
    state.clear_current_decision();
    assert_eq!(current(&state), None);
    assert_eq!(
        state.decision_command(DecisionCommand::Pick(0), false),
        None
    );
    state.decision_command(DecisionCommand::Next, false);
    assert_eq!(current(&state), Some(DecisionId(1)));

    let key = |id| DecisionKey {
        space: "work".into(),
        id: DecisionId(id),
    };
    state.select_decision(key(3));
    assert_eq!(current(&state), Some(DecisionId(3)));
    state.select_decision(key(2));
    assert_eq!(current(&state), Some(DecisionId(3)));

    state.set_decision_scope(DecisionScope::None);
    state.select_decision(key(3));
    assert_eq!(
        state.decision_command(DecisionCommand::Pick(0), false),
        None
    );
}

#[tokio::test]
async fn decision_number_answers_current_and_keeps_its_record_in_conversation() {
    let hub = FakeHub::new("Work", vec![decision(1, 1_000), decision(2, 2_000)]);
    hub.now.store(5 * 60_000, Ordering::SeqCst);
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.set_decision_scope(DecisionScope::Project("work".into(), "api".into()));
    let Some(DecisionEffect::Answer(choice)) =
        state.decision_command(DecisionCommand::Pick(1), false)
    else {
        panic!("the tier 2 tool answers immediately")
    };
    assert_eq!(choice.key.id, DecisionId(1));
    assert_eq!(choice.option, "decline");
    assert!(state.begin_decision_answer(choice.key.clone()));
    assert_eq!(
        state.decision_command(DecisionCommand::Pick(0), false),
        None
    );
    let answer = core
        .answer(
            Some(&choice.key.space),
            &choice.key.id.to_string(),
            &choice.option,
            Some("Try again tomorrow"),
        )
        .await
        .expect("answer");
    state.finish_decision_answer(&choice.key);
    state.answered(&answer);

    assert_eq!(state.current_decision().unwrap().decision.id, DecisionId(2));
    assert_eq!(state.needs_you().count(), 1);
    let lines: Vec<_> = state
        .conversation("work", "api", &when())
        .into_iter()
        .filter_map(|line| match line {
            Line::Decision {
                card,
                pending,
                time,
            } => Some((card, pending, time)),
            _ => None,
        })
        .collect();
    assert_eq!(lines.len(), 2);
    let (card, pending, time) = &lines[0];
    assert!(!pending);
    let shown = state
        .present_feed_decision("work", "api", card, time, &when())
        .unwrap();
    assert_eq!(shown.options, ["approve", "decline"]);
    assert_eq!(shown.contents.recommended, Some(0));
    assert!(!shown.current && !shown.show_note && shown.always.is_none());
    assert_eq!(
        shown.record,
        Some(DecisionRecord {
            status: "Answered · 00:05".into(),
            chosen: Some(1),
            note: Some("Try again tomorrow".into()),
        })
    );
}

#[tokio::test]
async fn a_long_question_splits_into_a_title_and_a_six_line_body_and_a_short_one_stays_whole() {
    let mut long = decision(1, 1_000);
    long.kind = DecisionKind::Input;
    long.tool = "ask_user".into();
    long.question = "Which of these should the team take on next? I read the progress files \
        and the dogfood log, and these are the open threads that are worth a task.\n\n\
        1. Deliver a lead's final text: findings 24 and 8.\n2. Fix the sidebar rows."
        .into();
    let hub = FakeHub::new("Work", vec![long, decision(2, 2_000)]);
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    let shown: Vec<_> = state
        .queue()
        .iter()
        .map(|queued| state.present_decision(queued, "api", &when()))
        .collect();
    assert_eq!(
        shown[0].question,
        "Which of these should the team take on next?"
    );
    assert_eq!(
        shown[0].body.as_deref(),
        Some(
            "I read the progress files and the dogfood log, and these are the open threads \
             that are worth a task.\n\n1. Deliver a lead's final text: findings 24 and 8.\n\
             2. Fix the sidebar rows."
        )
    );
    assert_eq!(shown[1].question, "Question 2?");
    assert_eq!(shown[1].body, None);
    assert_eq!(
        ProseLimit::Question.disclosure(false, true),
        (Some(6), Some("Show more"))
    );
    assert_eq!(
        question_parts("Pick one:\n- a\n- b"),
        ("Pick one:".to_owned(), Some("- a\n- b".to_owned()))
    );
}

#[tokio::test]
async fn decision_shortcuts_yield_to_fields_and_enter_only_submits_a_question_note() {
    let mut question = decision(2, 2_000);
    question.kind = DecisionKind::Input;
    question.tool = "ask_user".into();
    let mut no_recommendation = question.clone();
    no_recommendation.id = DecisionId(3);
    no_recommendation.recommended = None;
    let hub = FakeHub::new(
        "Work",
        vec![decision(1, 1_000), question, no_recommendation],
    );
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.set_decision_scope(DecisionScope::Home);
    let approval = DecisionKey::from(state.current_decision().unwrap());
    for command in [
        DecisionCommand::Next,
        DecisionCommand::Previous,
        DecisionCommand::Pick(0),
        DecisionCommand::Reply,
    ] {
        assert_eq!(state.decision_command(command, true), None);
    }
    assert!(!state.shows_decision_note(state.current_decision().unwrap()));
    assert_eq!(
        state.decision_command(DecisionCommand::Reply, false),
        Some(DecisionEffect::FocusNote(approval.clone()))
    );
    assert!(state.shows_decision_note(state.current_decision().unwrap()));
    assert_eq!(state.decision_note_enter(&approval, "do this"), None);
    state.decision_command(DecisionCommand::Next, false);
    let question = state.current_decision().unwrap();
    assert!(state.shows_decision_note(question));
    let key = DecisionKey::from(question);
    assert_eq!(state.decision_note_enter(&key, "   "), None);
    let choice = state.decision_note_enter(&key, "do this").unwrap();
    assert_eq!(choice.option, "approve");
    let answer = core
        .answer(
            Some("work"),
            &key.id.to_string(),
            &choice.option,
            Some("do this"),
        )
        .await
        .unwrap();
    state.answered(&answer);
    let key = DecisionKey::from(state.current_decision().unwrap());
    assert_eq!(state.decision_note_enter(&key, "do this"), None);
}

#[tokio::test]
async fn decision_merge_number_requires_confirmation_of_the_exact_option() {
    let mut merge = decision(1, 1_000);
    merge.tool = threadlane_core::forge::MERGE.to_owned();
    let hub = FakeHub::new("Work", vec![merge]);
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.set_decision_scope(DecisionScope::Home);

    let Some(DecisionEffect::Confirm(shown)) =
        state.decision_command(DecisionCommand::Pick(0), false)
    else {
        panic!("merge needs a separate confirmation")
    };
    assert_eq!(
        state.decision_command(DecisionCommand::Pick(1), false),
        None
    );
    assert_eq!(hub.decisions.lock().unwrap().len(), 1);
    state.cancel_decision_confirmation();
    assert_eq!(state.confirm_decision(&shown), None);

    assert_eq!(
        state.decision_command(DecisionCommand::Pick(0), false),
        Some(DecisionEffect::Confirm(shown.clone()))
    );
    let choice = state.confirm_decision(&shown).unwrap();
    assert_eq!(choice, shown);
    let answer = core
        .answer(Some("work"), "D-0001", &choice.option, None)
        .await
        .unwrap();
    state.answered(&answer);
    assert!(state.current_decision().is_none());
}

#[tokio::test]
async fn decision_history_recovers_an_answer_from_its_feed_trace() {
    let mut proposal = decision(1, 1_000);
    proposal.kind = DecisionKind::Proposal;
    proposal.status = DecisionStatus::Answered;
    proposal.trace_id = Some("trace-1".into());
    proposal.answer = Some(json!({"option": 0, "note": "Ship Tuesday", "answered_at": 120_000}));
    proposal.proposed_by = Some("Engineer".into());
    let mut item = feed_item(
        1,
        OutboxKind::Decision,
        json!({"decision":"D-0001", "question":"Start?", "kind":"proposal"}),
        1_000,
    );
    item.message.trace_id = proposal.trace_id.clone();
    item.message.decision_id = Some(proposal.id);
    let hub = FakeHub::new("Work", Vec::new());
    hub.history
        .lock()
        .unwrap()
        .push(ConversationEntry::Delivered { item });
    hub.traced_decisions.lock().unwrap().push(proposal);
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    let shown = state
        .conversation("work", "api", &when())
        .into_iter()
        .find_map(|line| {
            if let Line::Decision { card, time, .. } = line {
                state.present_feed_decision("work", "api", &card, &time, &when())
            } else {
                None
            }
        })
        .unwrap();
    assert!(shown.contents.dashed);
    assert!(
        shown
            .contents
            .context
            .unwrap()
            .starts_with("You did not ask for this.")
    );
    assert!(shown.origin.contains("Engineer"));
    assert_eq!(
        shown.record,
        Some(DecisionRecord {
            status: "Answered · 00:02".into(),
            chosen: Some(0),
            note: Some("Ship Tuesday".into()),
        })
    );
}

#[test]
fn stage_three_references_preserve_code_explicit_links_and_render_html_literally() {
    let text = "Open **T-0001**, D-0012 and https://example.test/pull/4. `T-0002`";
    let references = segments(text)
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Link(label, target) => Some((label, target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        references,
        [
            ("T-0001", Reference::Task(TaskId(1))),
            ("D-0012", Reference::Decision(DecisionId(12))),
            (
                "https://example.test/pull/4",
                Reference::Web("https://example.test/pull/4")
            ),
        ]
    );
    assert_eq!(
        markdown_source(text),
        "Open **[T-0001](threadlane:T-0001)**, [D-0012](threadlane:D-0012) and https://example.test/pull/4. `T-0002`"
    );
    let code = "# Heading
```rust
let v = \"D-0001 <div>\";
```

~~~sh
T-0002
~~~

``the ` T-0003 id``";
    assert_eq!(markdown_source(code), code);
    assert_eq!(
        markdown_source("[T-0001](https://example.test) <strong>x</strong>"),
        "[T-0001](https://example.test) &lt;strong>x&lt;/strong>"
    );
    assert_eq!(
        markdown_source("préT-0001 T-0002_suffix T-0000"),
        "préT-0001 T-0002_suffix T-0000"
    );
    assert_eq!(
        Reference::from_link("threadlane:D-0012"),
        Some(Reference::Decision(DecisionId(12)))
    );
    assert_eq!(
        markdown_source("`unclosed\n\n<div>\n\nthen `"),
        "`unclosed\n\n&lt;div>\n\nthen `",
    );
    assert_eq!(Reference::from_link("file:///tmp/file"), None);
    assert_eq!(Reference::from_link("javascript:alert(1)"), None);
}

#[test]
fn bare_github_references_link_only_outside_words_code_and_urls() {
    let text = "Fix #565 (#572) **#1**, skip abc#5, foo_#6, foo-#7, #, #0, #05, #5abc, \
                ##565, &#33;, foo/#5, \\#5, `#565`, # Heading, https://example.test/page#565, \
                <https://example.test/page#566>, and [see #567](https://example.test/page#568).";
    let references = segments(text)
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Link(label, target) => Some((label, target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        references,
        [
            ("#565", Reference::Github(None, 565)),
            ("#572", Reference::Github(None, 572)),
            ("#1", Reference::Github(None, 1)),
            (
                "https://example.test/page#565",
                Reference::Web("https://example.test/page#565")
            ),
            (
                "<https://example.test/page#566>",
                Reference::Web("https://example.test/page#566")
            ),
        ]
    );

    let https_repo = github_repository("https://github.com/Wheregmis/Threadlane.git").unwrap();
    let ssh_repo = github_repository("git@github.com:wheregmis/threadlane.git").unwrap();
    let ssh_url_repo = github_repository("ssh://git@github.com/wheregmis/threadlane").unwrap();
    assert_eq!(https_repo, "wheregmis/threadlane");
    assert_eq!(ssh_repo, "wheregmis/threadlane");
    assert_eq!(ssh_url_repo, "wheregmis/threadlane");
    assert_eq!(
        github_repository("https://gitlab.com/acme/widgets.git"),
        None
    );
    assert_eq!(github_repository("/srv/git/notes"), None);

    assert_eq!(
        markdown_source_for(
            "See #565, \\#5, abc#5, #, and https://example.test/page#565.",
            None,
            Some(&https_repo),
        ),
        "See [#565](https://github.com/wheregmis/threadlane/pull/565), \\#5, abc#5, #, and https://example.test/page#565."
    );
    assert_eq!(
        markdown_source_for("See #565.", None, Some(&ssh_repo)),
        "See [#565](https://github.com/wheregmis/threadlane/pull/565)."
    );
    // Without a GitHub remote, `#N` stays plain text.
    assert_eq!(markdown_source_for("See #565.", None, None), "See #565.");
    assert_eq!(markdown_source("See #565."), "See #565.");

    assert_eq!(
        Reference::from_link("threadlane:#565"),
        Some(Reference::Github(None, 565))
    );
    assert_eq!(Reference::from_link("threadlane:#"), None);
    assert_eq!(Reference::from_link("threadlane:#0"), None);
}

#[tokio::test]
async fn project_github_repository_links_bare_numbers_only_when_unambiguous() {
    let hub = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    // No repositories yet -> None.
    assert_eq!(state.github_repository("work", "api"), None);

    // Non-GitHub remotes only -> None.
    state.apply_repositories(
        "work",
        "api",
        Ok(vec![
            Repository {
                id: RepositoryId(1),
                project_id: ProjectId(1),
                name: "local".into(),
                remote_url: "/srv/git/local".into(),
                default_branch: "main".into(),
                protected_branches: Vec::new(),
            },
            Repository {
                id: RepositoryId(2),
                project_id: ProjectId(1),
                name: "gitlab".into(),
                remote_url: "git@gitlab.com:acme/docs.git".into(),
                default_branch: "main".into(),
                protected_branches: Vec::new(),
            },
        ]),
    );
    assert_eq!(state.github_repository("work", "api"), None);

    // Two GitHub repositories: a bare `#N` is ambiguous without context.
    state.apply_repositories(
        "work",
        "api",
        Ok(vec![
            Repository {
                id: RepositoryId(1),
                project_id: ProjectId(1),
                name: "local".into(),
                remote_url: "/srv/git/local".into(),
                default_branch: "main".into(),
                protected_branches: Vec::new(),
            },
            Repository {
                id: RepositoryId(2),
                project_id: ProjectId(1),
                name: "ssh".into(),
                remote_url: "git@github.com:Acme/Widgets.git".into(),
                default_branch: "main".into(),
                protected_branches: Vec::new(),
            },
            Repository {
                id: RepositoryId(3),
                project_id: ProjectId(1),
                name: "https".into(),
                remote_url: "https://github.com/acme/other.git".into(),
                default_branch: "main".into(),
                protected_branches: Vec::new(),
            },
        ]),
    );
    assert_eq!(state.github_repository("work", "api"), None);
    assert_eq!(
        markdown_source_for("See #1.", None, None),
        "See #1.",
        "multi-repo without context stays plain text"
    );

    // A task's attached repository is the context, skipping non-GitHub ones.
    let task_repo =
        state.github_repository_among("work", "api", Some(&[RepositoryId(1), RepositoryId(3)]));
    assert_eq!(task_repo.as_deref(), Some("acme/other"));
    assert_eq!(
        markdown_source_for("See #1.", None, task_repo.as_deref()),
        "See [#1](https://github.com/acme/other/pull/1)."
    );
    assert_eq!(
        state.github_repository_among("work", "api", Some(&[RepositoryId(2), RepositoryId(3)])),
        None
    );

    // A single GitHub repository, among non-GitHub ones, is unambiguous.
    state.apply_repositories(
        "work",
        "api",
        Ok(vec![
            Repository {
                id: RepositoryId(1),
                project_id: ProjectId(1),
                name: "local".into(),
                remote_url: "/srv/git/local".into(),
                default_branch: "main".into(),
                protected_branches: Vec::new(),
            },
            Repository {
                id: RepositoryId(2),
                project_id: ProjectId(1),
                name: "ssh".into(),
                remote_url: "git@github.com:Acme/Widgets.git".into(),
                default_branch: "main".into(),
                protected_branches: Vec::new(),
            },
        ]),
    );
    assert_eq!(
        state.github_repository("work", "api").as_deref(),
        Some("acme/widgets")
    );
    // A task with no repositories has no context, not the project's only one.
    assert_eq!(
        state.github_repository_among("work", "api", Some(&[])),
        None
    );
}

#[test]
fn qualified_github_references_link_to_their_own_repository() {
    let text = "See acme/other#7, wheregmis/threadlane#565. Skip a/b/c#1, x.acme/b#2, \
                acme/b#05, acme/b#7x, `acme/b#9`, and https://github.com/acme/web/issues/3.";
    let references = segments(text)
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Link(label, target) => Some((label, target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        references,
        [
            ("acme/other#7", Reference::Github(Some("acme/other"), 7)),
            (
                "wheregmis/threadlane#565",
                Reference::Github(Some("wheregmis/threadlane"), 565)
            ),
            (
                "https://github.com/acme/web/issues/3",
                Reference::Web("https://github.com/acme/web/issues/3")
            ),
        ]
    );
    // Qualified and URL references ignore the context's repository; bare ones use it.
    for context in [None, Some("acme/widgets")] {
        assert_eq!(
            markdown_source_for(
                "acme/other#7 and https://github.com/acme/web/pull/3",
                None,
                context
            ),
            "[acme/other#7](https://github.com/acme/other/pull/7) and https://github.com/acme/web/pull/3"
        );
    }
    assert_eq!(
        markdown_source_for("#7 and acme/other#7", None, Some("acme/widgets")),
        "[#7](https://github.com/acme/widgets/pull/7) and [acme/other#7](https://github.com/acme/other/pull/7)"
    );
}

#[test]
fn switching_spaces_keeps_drafts_and_failed_sends_with_their_original_chat() {
    let mut state = AppState::default();
    let work = Destination::Lead("work".into(), "General".into());
    let home = Destination::Lead("home".into(), "General".into());
    let at = Timestamp::from_millis(2_000);
    state.edit_draft(&work, "Work plan");
    let first = state.request_send(work.clone(), at).held.unwrap();

    assert!(state.draft(&home).is_empty());
    assert!(state.held_seconds(&home).is_none());
    assert!(!state.undo_send(&home));
    state.edit_draft(&home, "Home plan");
    let send = state.request_send(home.clone(), at).flushed.unwrap();
    assert_eq!(send.id, first);
    state.finish_send(
        send,
        Err(ClientError::Unreachable {
            space: "work".into(),
            reason: "offline".into(),
        }),
    );
    assert_eq!(state.draft(&work), "Work plan");
    assert!(state.send_error(&work).is_some());
    assert_eq!(state.draft(&home), "Home plan");
    assert!(state.send_error(&home).is_none());
    assert_eq!(state.held_seconds(&home), Some(3));
}

#[tokio::test]
async fn stage_three_hold_undo_flush_and_confirmation_use_the_original_recipient() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    let to = Destination::Lead("work".into(), "api".into());
    let at = Timestamp::from_millis(2_000);

    assert!(sends_on_enter(false, false));
    assert!(!sends_on_enter(false, true));
    assert!(sends_on_enter(true, false));
    assert!(sends_on_enter(true, true));
    state.edit_draft(&to, "  \n ");
    assert!(!state.is_send_enabled(&to));
    assert!(state.request_send(to.clone(), at).held.is_none());

    state.edit_draft(&to, "Write the docs");
    let hold = state.request_send(to.clone(), at).held.unwrap();
    assert_eq!(state.held_seconds(&to), Some(3));
    assert!(
        matches!(state.conversation("work", "api", &when()).as_slice(),
        [Line::Day(_), Line::Sending { text, .. }] if text == "Write the docs")
    );
    assert!(state.tick_send(hold).is_none());
    assert_eq!(state.held_seconds(&to), Some(2));
    assert!(state.undo_send(&to));
    assert_eq!(state.draft(&to), "Write the docs");
    assert!(state.tick_send(hold).is_none());
    assert!(state.conversation("work", "api", &when()).is_empty());

    let hold = state.request_send(to.clone(), at).held.unwrap();
    let second = state.request_send(to.clone(), at);
    assert!(
        second.held.is_none(),
        "repeating Send expedites the existing draft"
    );
    let flushed = second.flushed.unwrap();
    assert_eq!(flushed.id, hold);
    assert_eq!(flushed.destination, to);
    assert!(state.tick_send(hold).is_none());
    assert!(state.is_sending(&to));
    assert_eq!(state.draft(&to), "Write the docs");

    let result = flushed.deliver(&core).await;
    let event_id = result.as_ref().unwrap().accepted.event_id;
    work.history.lock().unwrap().push(ConversationEntry::You {
        event_id,
        text: flushed.text.clone(),
        images: Vec::new(),
        at,
    });
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(
        said(&state.conversation("work", "api", &when())),
        ["-- Today", "sending: Write the docs"]
    );
    state.finish_send(flushed, result);
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(
        said(&state.conversation("work", "api", &when())),
        ["-- Today", "you: Write the docs"]
    );
    assert_eq!(state.draft(&to), "");
    assert!(!state.is_sending(&to));
}

#[tokio::test]
async fn stage_three_countdown_flushes_an_edited_second_send_and_keeps_failed_task_text() {
    let work = FakeHub::new("Work", Vec::new());
    let (dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.set_client_dir(dir.path().to_owned());
    work.tasks
        .lock()
        .unwrap()
        .push(task(7, TaskStatus::Running, None));
    state.apply_task(load_task(&core, "work", TaskId(7)).await);
    let to = Destination::Task("work".into(), TaskId(7));
    let at = Timestamp::from_millis(2_000);
    state.edit_draft(&to, "First");
    let first = state.request_send(to.clone(), at).held.unwrap();
    state.edit_draft(&to, "Second");
    let next = state.request_send(to.clone(), at);
    let flushed = next.flushed.unwrap();
    assert_eq!(flushed.id, first);
    assert_eq!(flushed.text, "First");
    state.finish_send(flushed.clone(), flushed.deliver(&core).await);
    assert_eq!(state.draft(&to), "Second");
    let second = next.held.unwrap();
    assert!(state.tick_send(second).is_none());
    assert!(state.tick_send(second).is_none());
    assert_eq!(state.held_seconds(&to), Some(1));
    assert!(
        state
            .task_timeline("work", state.task("work", TaskId(7)).unwrap(), &when())
            .iter()
            .any(|moment| moment.text == "You · Sending…\n\nSecond")
    );
    let outgoing = state.tick_send(second).unwrap();
    assert!(state.tick_send(second).is_none());
    work.down.store(true, Ordering::SeqCst);
    state.finish_send(outgoing.clone(), outgoing.deliver(&core).await);
    assert_eq!(state.draft(&to), "Second");
    assert!(
        state
            .send_error(&to)
            .unwrap()
            .contains("connection refused")
    );
    assert!(state.is_send_enabled(&to));

    state.set_hold_messages(false).unwrap();
    let mut restarted = AppState::default();
    restarted.set_client_dir(dir.path().to_owned());
    assert!(!restarted.hold_messages());
    work.down.store(false, Ordering::SeqCst);
    let send = state.request_send(to.clone(), at).immediate.unwrap();
    state.finish_send(send.clone(), send.deliver(&core).await);
    assert_eq!(state.draft(&to), "");
    assert!(state.send_error(&to).is_none());
    let moments = state.task_timeline("work", state.task("work", TaskId(7)).unwrap(), &when());
    assert_eq!(
        moments
            .iter()
            .filter(|moment| moment.text == "You wrote:\n\nSecond")
            .count(),
        1
    );
}

#[tokio::test]
async fn stage_three_new_divider_tracks_each_visit_and_the_newest_body_stays_open() {
    let work = FakeHub::new("Work", vec![decision(12, 2_000)]);
    let (dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.set_client_dir(dir.path().to_owned());
    state.apply(refresh(&core).await);
    assert_eq!(state.reference_project("work", DecisionId(12)), Some("api"));
    work.history
        .lock()
        .unwrap()
        .push(ConversationEntry::Delivered {
            item: feed_item(
                10,
                OutboxKind::Decision,
                json!({"decision": "D-0050", "question": "Old decision?", "options": ["yes"]}),
                500,
            ),
        });
    work.history
        .lock()
        .unwrap()
        .extend((0..i64::from(PAGE)).map(|index| ConversationEntry::You {
            event_id: EventId(1 + index),
            text: "Newer page".to_owned(),
            images: Vec::new(),
            at: Timestamp::from_millis(3_000 + index),
        }));
    assert_eq!(state.reference_project("work", DecisionId(50)), None);
    let link = load_decision_link(&core, "work", vec!["api".to_owned()], DecisionId(50))
        .await
        .unwrap()
        .unwrap();
    let mut linked = AppState::default();
    assert_eq!(linked.apply_decision_link(link), "api");
    assert_eq!(
        linked.reference_project("work", DecisionId(50)),
        Some("api")
    );
    let first = item("work", 1, OutboxKind::Message, json!({"text": "Earlier"}));
    let second = item(
        "work",
        2,
        OutboxKind::Message,
        json!({"text": "New result"}),
    );
    *work.history.lock().unwrap() = vec![ConversationEntry::Delivered { item: first.item }];
    state.apply_project(load_project(&core, "work", "api").await);
    state.visit_conversation("work", "api");
    state.saw_conversation_latest("work", "api");
    assert!(!state.conversation_has_newer("work", "api", false));
    // Leaving and reopening after restart reads the same saved watermark.
    let mut reopened = AppState::default();
    reopened.set_client_dir(dir.path().to_owned());
    reopened.receive(second.clone());
    reopened.visit_conversation("work", "api");
    assert!(
        reopened
            .conversation("work", "api", &when())
            .contains(&Line::New)
    );

    state.receive(second.clone());
    state.visit_conversation("work", "api");
    let lines = state.conversation("work", "api", &when());
    let mark = lines
        .iter()
        .position(|line| matches!(line, Line::New))
        .unwrap();
    assert!(matches!(&lines[mark+1], Line::Lead { text, .. } if text == "New result"));
    assert!(state.conversation_has_newer("work", "api", false));
    assert!(!state.conversation_has_newer("work", "api", true));

    assert!(!follow_latest(false, false, true));
    assert!(follow_latest(false, true, true));
    assert!(follow_latest(true, false, false));
    let newest = newest_message(&lines);
    assert_eq!(
        message_limit(newest, mark - 1).disclosure(false, true),
        (Some(40), Some("Show more"))
    );
    assert_eq!(
        message_limit(newest, mark + 1).disclosure(false, true),
        (None, None)
    );
    state.toggle_message("earlier");
    assert_eq!(
        message_limit(newest, mark - 1).disclosure(state.message_expanded("earlier"), true),
        (None, Some("Show less"))
    );

    state.saw_conversation_latest("work", "api");
    state.receive(second);
    state.visit_conversation("work", "api");
    assert!(
        !state
            .conversation("work", "api", &when())
            .contains(&Line::New)
    );
    let to = Destination::Lead("work".into(), "api".into());
    state.edit_draft(&to, "I agree.");
    state.reply_to_message(&to, "First line\nSecond line");
    assert_eq!(state.draft(&to), "> First line\n> Second line\n\nI agree.");
}

#[tokio::test]
async fn in_progress_caps_running_leads_and_queued_work_and_uses_the_display_clock() {
    let work = FakeHub::new("Work", Vec::new());
    *work.tasks.lock().unwrap() = (1..=7)
        .map(|id| {
            task(
                id,
                if id == 1 {
                    TaskStatus::Running
                } else {
                    TaskStatus::Queued
                },
                None,
            )
        })
        .collect();
    *work.lead.lock().unwrap() = Some(Timestamp::from_millis(600_000));
    let (_dir, core) = core(&[("dev", &work)]);
    let mut state = AppState::paired(["dev"]);
    state.apply_at(refresh(&core).await, when().now);
    let work = state.in_progress("dev", &when());
    assert_eq!(work.rows.len(), 5);
    assert_eq!(work.more, 4);
    assert_eq!(
        work.rows[0].destination,
        Destination::Task("dev".to_owned(), TaskId(1))
    );
    assert_eq!(work.rows[0].elapsed, "12 min");
    assert_eq!(work.rows[1].title, "api · lead");
    assert_eq!(work.rows[1].label, "api · lead");
    assert_eq!(work.rows[0].label, "Task 1");
    assert_eq!(work.rows[1].elapsed, "2 min");
    assert_eq!(state.running_leads().len(), 2);
    assert_eq!(work.rows[3].status, "Queued");
    assert_eq!(work.rows[3].elapsed, "11 min");
    let later = When {
        now: Timestamp::from_millis(780_000),
        ..when()
    };
    assert_eq!(state.in_progress("dev", &later).rows[3].elapsed, "12 min");
    assert!(state.in_progress("other", &when()).rows.is_empty());
}

#[tokio::test]
async fn in_progress_rows_clip_long_titles_to_one_line() {
    let work = FakeHub::new("Work", Vec::new());
    let mut running = task(1, TaskStatus::Running, None);
    running.task.title = "Deliver a lead's final text to the feed".into();
    *work.tasks.lock().unwrap() = vec![running];
    *work.lead.lock().unwrap() = Some(Timestamp::from_millis(600_000));
    let (_dir, core) = core(&[("dev", &work)]);
    let mut state = AppState::paired(["dev"]);
    state.apply_at(refresh(&core).await, when().now);
    let rows = state.in_progress("dev", &when()).rows;
    assert_eq!(rows[0].title, "Deliver a lead's final text to the feed");
    assert_eq!(rows[0].label, "Deliver a lead's f…");
    assert_eq!(rows[0].elapsed, "12 min");
}

#[tokio::test]
async fn running_work_marks_its_project_even_past_the_cap() {
    let work = FakeHub::new("Work", Vec::new());
    *work.tasks.lock().unwrap() = (1..=7)
        .map(|id| task(id, TaskStatus::Queued, None))
        .collect();
    let (_dir, core) = core(&[("dev", &work)]);
    let mut state = AppState::paired(["dev"]);
    state.apply_at(refresh(&core).await, when().now);
    assert!(state.is_working("dev", "api"));
    assert!(!state.is_working("dev", "General"));
    assert!(!state.is_working("other", "api"));
    *work.tasks.lock().unwrap() = Vec::new();
    *work.lead.lock().unwrap() = Some(Timestamp::from_millis(600_000));
    state.apply_at(refresh(&core).await, when().now);
    assert!(state.is_working("dev", "General"));
}

#[tokio::test]
async fn in_progress_titles_leave_room_for_a_long_time() {
    let work = FakeHub::new("Work", Vec::new());
    let mut running = task(1, TaskStatus::Running, None);
    running.task.title = "One place for every finding".into();
    *work.tasks.lock().unwrap() = vec![running];
    let (_dir, core) = core(&[("dev", &work)]);
    let mut state = AppState::paired(["dev"]);
    state.apply_at(refresh(&core).await, when().now);
    let at = |millis| When {
        now: Timestamp::from_millis(millis),
        ..when()
    };
    for (now, label, elapsed) in [
        (30_000, "One place for eve…", "< 1 min"),
        (12 * 60_000, "One place for ever…", "12 min"),
        (125 * 60_000, "One place for e…", "2 h 5 min"),
        (725 * 60_000, "One place for…", "12 h 5 min"),
    ] {
        let row = &state.in_progress("dev", &at(now)).rows[0];
        assert_eq!((row.label.as_str(), row.elapsed.as_str()), (label, elapsed));
        // The title and the time share one row's width; the time keeps its own.
        assert!(row.label.chars().count() + row.elapsed.chars().count() <= 25);
    }
}

#[tokio::test]
async fn task_header_and_activity_use_journal_clock_and_finished_usage() {
    let work = FakeHub::new("Work", Vec::new());
    let mut running = task(1, TaskStatus::Running, None);
    let started = Timestamp::from_millis(60_000);
    let step = RunStep {
        kind: "tool_use".to_owned(),
        description: "Called Bash".to_owned(),
        at: Timestamp::from_millis(120_000),
    };
    running.running_since = Some(started);
    running.latest_step = Some(step.clone());
    *work.tasks.lock().unwrap() = vec![running.clone()];
    let run = |id, status, started_at, ended_at, usage| RunSummary {
        steps: vec![step.clone()],
        usage,
        machine: "macbook".to_owned(),
        run: Run {
            trace_id: String::new(),
            id: RunId(id),
            lane_id: running.task.lane_id,
            session_id: SessionId(1),
            machine_id: MachineId(1),
            lease_epoch: 1,
            agent_id: Some(running.task.agent_id),
            backend: Some(BackendKind::Claude),
            status,
            outcome: None,
            rationale: None,
            error: None,
            started_at,
            ended_at,
        },
    };
    let reported = RunUsage {
        input_tokens: Some(12),
        output_tokens: Some(4),
        cost_usd: Some(0.0123),
    };
    *work.task_runs.lock().unwrap() = vec![
        run(
            1,
            RunStatus::Committed,
            Timestamp::from_millis(0),
            Some(started),
            Some(reported),
        ),
        run(2, RunStatus::Active, started, None, None),
    ];
    let (_dir, core) = core(&[("dev", &work)]);
    let mut state = AppState::default();
    state.apply_task(load_task(&core, "dev", TaskId(1)).await);
    let detail = state.task("dev", TaskId(1)).unwrap();
    work.now.store(240_000, Ordering::SeqCst);
    let header = task_header(detail, &work.when());
    assert_eq!(
        header.meta,
        "Engineer · macbook · claude · started 00:01 · running for 3 min"
    );
    assert_eq!(
        header.latest.unwrap(),
        LatestStep {
            text: "Latest: Called Bash · 2 min ago".to_owned(),
            quiet: false
        }
    );
    work.now.store(719_999, Ordering::SeqCst);
    assert!(!task_header(detail, &work.when()).latest.unwrap().quiet);
    work.now.store(720_000, Ordering::SeqCst);
    assert_eq!(
        task_header(detail, &work.when()).latest.unwrap(),
        LatestStep {
            text: "Quiet for 10 min".to_owned(),
            quiet: true
        }
    );
    assert_eq!(activity_runs(detail, &work.when())[0].0, "Run 2 · 00:01");
    assert_eq!(
        usage(detail, &work.when())[1],
        "Reported usage · 12 input tokens · 4 output tokens · $0.0123"
    );
    assert!(!state.activity_open("dev", TaskId(1)));
    state.toggle_activity("dev", TaskId(1));
    assert!(state.activity_open("dev", TaskId(1)));
    work.tasks.lock().unwrap()[0].task.status = TaskStatus::Done;
    {
        let mut runs = work.task_runs.lock().unwrap();
        runs[1].run.status = RunStatus::Committed;
        runs[1].run.ended_at = Some(work.when().now);
    }
    state.apply_task(load_task(&core, "dev", TaskId(1)).await);
    let header = task_header(state.task("dev", TaskId(1)).unwrap(), &when());
    assert_eq!(
        header.meta,
        "Engineer · macbook · claude · started 00:01 · took 11 min"
    );
    assert_eq!(header.latest, None);
}

#[tokio::test]
async fn connection_keeps_first_failure_time_and_clears_it_after_recovery() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("dev", &work)]);
    let mut state = AppState::paired(["dev"]);
    assert_eq!(
        state.connection("dev", &when()).unwrap().status,
        ConnectionStatus::Reconnecting
    );
    assert_eq!(
        state.connection_warning("dev", &when()).unwrap().status,
        ConnectionStatus::Reconnecting
    );
    state.apply_at(refresh(&core).await, when().now);
    assert_eq!(state.connection("dev", &when()).unwrap().label, "Live");
    assert!(state.connection_warning("dev", &when()).is_none());
    work.down.store(true, Ordering::SeqCst);
    state.apply_at(refresh(&core).await, when().now);
    assert_eq!(
        state.connection("dev", &when()).unwrap().label,
        "Unreachable since 00:12"
    );
    assert_eq!(
        state.connection_warning("dev", &when()).unwrap().status,
        ConnectionStatus::Unreachable
    );
    assert!(state.connection_banner().unwrap().contains("dev"));
    state.checking_connections();
    assert_eq!(
        state.connection("dev", &when()).unwrap().label,
        "Reconnecting…"
    );
    let later = When {
        now: Timestamp::from_millis(1_800_000),
        ..when()
    };
    state.apply_at(refresh(&core).await, later.now);
    assert_eq!(
        state.connection("dev", &later).unwrap().label,
        "Unreachable since 00:12"
    );
    work.down.store(false, Ordering::SeqCst);
    state.apply_at(refresh(&core).await, later.now);
    assert!(state.connection_banner().is_none());
    assert!(state.connection_warning("dev", &later).is_none());
    work.down.store(true, Ordering::SeqCst);
    state.apply_at(refresh(&core).await, later.now);
    assert_eq!(
        state.connection("dev", &later).unwrap().label,
        "Unreachable since 00:30"
    );
}

#[tokio::test]
async fn screen_states_pair_plain_sentences_with_hub_errors_and_retry() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("dev", &work)]);
    let mut state = AppState::paired(["dev"]);
    assert_eq!(
        state
            .space("dev")
            .unwrap()
            .gap(Part::Machines)
            .unwrap()
            .sentence("machines", "dev"),
        StateSentence {
            text: "Loading machines from dev.".to_owned(),
            detail: None
        }
    );
    work.old.store(true, Ordering::SeqCst);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "dev", "api").await);
    state.apply_task(load_task(&core, "dev", TaskId(1)).await);
    for (what, gap) in [
        ("tasks", state.project_gap("dev", "api", Part::Tasks)),
        (
            "conversation",
            state.project_gap("dev", "api", Part::Conversation),
        ),
        ("the task", state.task_gap("dev", TaskId(1))),
        (
            "running tasks",
            state.space("dev").unwrap().gap(Part::Running),
        ),
        ("machines", state.space("dev").unwrap().gap(Part::Machines)),
    ] {
        let sentence = gap.unwrap().sentence(what, "dev");
        assert_eq!(sentence.text, format!("Could not load {what} from dev."));
        assert!(sentence.detail.unwrap().contains("unknown variant"));
    }
    let projects = vec![("dev".to_owned(), "api".to_owned())];
    let loading = AppState::paired(["dev"]);
    for (what, gap) in [
        ("delivery pacing", Some(loading.pacing_gap("dev"))),
        ("metrics", Some(loading.metrics_gap("dev"))),
        ("routines", loading.routines_gap("dev", "api")),
        ("memory", loading.memory_gap("dev", "api")),
        ("agents", loading.space("dev").unwrap().gap(Part::Agents)),
    ] {
        let sentence = gap.unwrap().sentence(what, "dev");
        assert_eq!(sentence.text, format!("Loading {what} from dev."));
        assert!(sentence.detail.is_none());
    }
    work.down.store(true, Ordering::SeqCst);
    let mut down = AppState::paired(["dev"]);
    down.apply(refresh(&core).await);
    down.apply_memory(load_memory(&core, "dev", "api", None).await);
    down.apply_routines(load_routines(&core, projects).await);
    down.apply_pacing("dev".to_owned(), core.pacing(Some("dev")).await);
    down.apply_metrics("dev".to_owned(), load_metrics(&core, "dev").await);
    for (what, gap) in [
        ("delivery pacing", Some(down.pacing_gap("dev"))),
        ("metrics", Some(down.metrics_gap("dev"))),
        ("routines", down.routines_gap("dev", "api")),
        ("memory", down.memory_gap("dev", "api")),
        ("agents", down.space("dev").unwrap().gap(Part::Agents)),
        ("projects", down.space("dev").unwrap().gap(Part::Projects)),
    ] {
        let sentence = gap.unwrap().sentence(what, "dev");
        assert_eq!(sentence.text, format!("Could not load {what} from dev."));
        assert!(sentence.detail.is_some());
    }
    for region in [
        EmptyRegion::Pairing,
        EmptyRegion::Projects,
        EmptyRegion::Decisions,
        EmptyRegion::Running,
        EmptyRegion::Results,
        EmptyRegion::Board,
        EmptyRegion::Repositories,
        EmptyRegion::Agents,
        EmptyRegion::ProjectAgents,
        EmptyRegion::Conversation,
        EmptyRegion::Memory,
        EmptyRegion::MemoryVersions,
        EmptyRegion::Rules,
        EmptyRegion::Routines,
        EmptyRegion::AllRoutines,
        EmptyRegion::Related,
        EmptyRegion::Activity,
        EmptyRegion::Timeline,
        EmptyRegion::Machines,
        EmptyRegion::Clients,
        EmptyRegion::MachineActive,
        EmptyRegion::MachineRecent,
        EmptyRegion::Tools,
    ] {
        assert!(region.sentence().ends_with('.'), "{region:?}");
    }
    assert_eq!(
        EmptyRegion::Decisions.sentence(),
        "Nothing waits on you. The team is on it."
    );
    assert_eq!(
        EmptyRegion::Routines.sentence(),
        "No routines yet. Add one to have the lead check something on a cadence."
    );
}

#[tokio::test]
async fn irreversible_commands_confirm_and_only_a_ceiling_that_caps_work_asks() {
    let work = FakeHub::new("Work", Vec::new());
    *work.ceiling.lock().unwrap() = Some(Mode::Autonomous);
    *work.bindings.lock().unwrap() = vec![BindingSummary {
        agent: "Engineer".to_owned(),
        role: "engineer".to_owned(),
        mode: Mode::Assisted,
        is_lead: false,
        max_parallel: 1,
        overrides: BTreeMap::new(),
    }];
    let (_dir, core) = core(&[("dev", &work)]);
    let mut state = AppState::paired(["dev"]);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "dev", "api").await);
    let named = || "dev".to_owned();
    for (action, button) in [
        (Consequence::CancelTask(named(), TaskId(1)), "Cancel task"),
        (
            Consequence::CancelRoutine(named(), "api".to_owned(), TimerId(1)),
            "Cancel routine",
        ),
        (
            Consequence::RevokeRule(named(), threadlane_core::ids::StandingRuleId(1)),
            "Revoke rule",
        ),
        (Consequence::Unpair(named()), "Unpair"),
        (
            Consequence::DeleteMemory(
                named(),
                "api".to_owned(),
                "project/notes/build.md".to_owned(),
            ),
            "Delete file",
        ),
        (
            Consequence::SetCeiling(named(), Some("api".to_owned()), Mode::OnRequest),
            "Lower ceiling",
        ),
        (
            Consequence::SetCeiling(named(), None, Mode::OnRequest),
            "Lower ceiling",
        ),
    ] {
        let confirm = state.confirmation(&action).unwrap();
        assert_eq!(confirm.button, button);
        assert!(confirm.title.ends_with('?'));
        assert!(confirm.sentence.ends_with('.'));
    }
    assert_eq!(
        state.confirmation(&Consequence::SetCeiling(
            named(),
            Some("api".to_owned()),
            Mode::Assisted
        )),
        None
    );
    assert_eq!(
        state.confirmation(&Consequence::SetCeiling(named(), None, Mode::Autonomous)),
        None
    );
}

#[tokio::test]
async fn machine_totals_count_active_tasks_across_local_spaces() {
    let work = FakeHub::new("Work", Vec::new());
    let other = FakeHub::new("Other", Vec::new());
    let mut local = machine(1, MachineStatus::Online);
    local.load = 3;
    local.max_parallel = 4;
    local.capabilities = json!({"max_parallel": 4});
    let mut remote = machine(2, MachineStatus::Online);
    remote.name.clone_from(&local.name);
    *work.machines.lock().unwrap() = vec![local.clone(), remote.clone()];
    let mut on_local = task(1, TaskStatus::Running, None);
    on_local.machine = Some(local.name.clone());
    on_local.active_machine_id = Some(local.id);
    let mut on_remote = task(2, TaskStatus::Running, None);
    on_remote.machine = Some(remote.name.clone());
    on_remote.active_machine_id = Some(remote.id);
    let mut between_runs = task(3, TaskStatus::Running, None);
    between_runs.running_since = None;
    between_runs.machine = Some(local.name.clone());
    *work.tasks.lock().unwrap() = vec![on_local.clone(), on_remote, between_runs];

    // An older hub only supplies the active machine's name.
    *other.machines.lock().unwrap() = vec![local.clone()];
    on_local.active_machine_id = None;
    *other.tasks.lock().unwrap() = vec![on_local, task(4, TaskStatus::Queued, None)];
    let (_dir, core) = core(&[("work", &work), ("other", &other)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.set_filter(Some("other"));
    let space = state.space("work").unwrap();
    assert_eq!(space.machine_running_tasks(&local), Some(1));
    assert_eq!(space.machine_running_tasks(&remote), Some(1));
    assert_eq!(state.local_running_tasks(), Some(2));

    let mut aliases = refresh(&core).await;
    for status in &mut aliases.statuses {
        status.reach = threadlane_client::Reach::Socket {
            socket: PathBuf::from("/tmp/shared-hub.sock"),
        };
    }
    state.apply(aliases);
    assert_eq!(state.local_running_tasks(), Some(1));

    state.machine_slots_changed("work", local.id, 2);
    let detail = state.machine_detail("work", local.id, &when()).unwrap();
    assert_eq!((detail.slots, detail.runner_slots), (2, 4));
    assert_eq!(detail.running_tasks, Some(1));

    work.down.store(true, Ordering::SeqCst);
    state.apply(refresh(&core).await);
    assert_eq!(state.local_running_tasks(), None);
    let detail = state.machine_detail("work", local.id, &when()).unwrap();
    assert_eq!(detail.running_tasks, None);
    assert!(!detail.limit_writable);
}

#[tokio::test]
async fn machine_detail_reads_capabilities_and_filters_active_and_recent_runs_by_machine() {
    let work = FakeHub::new("Work", Vec::new());
    let mut online = machine(1, MachineStatus::Online);
    online.capabilities = json!({"os": "macos", "arch": "aarch64"});
    online.backends = json!({
        "claude": { "installed": true, "logged_in": true, "version": "2", "account_label": "owner" },
    });
    *work.accounts.lock().unwrap() = vec![
        Account {
            id: threadlane_core::ids::AccountId(1),
            label: "owner".to_owned(),
            backend: BackendKind::Claude,
            max_parallel: None,
            cooldown_until: Some(Timestamp::from_millis(900_000)),
        },
        Account {
            id: threadlane_core::ids::AccountId(2),
            label: "other".to_owned(),
            backend: BackendKind::Claude,
            max_parallel: None,
            cooldown_until: None,
        },
    ];
    online.last_seen = Some(Timestamp::from_millis(600_000));
    online.max_parallel = 2;
    online.load = 1;
    let mut unseen = machine(2, MachineStatus::Offline);
    unseen.last_seen = None;
    *work.machines.lock().unwrap() = vec![online, unseen];
    let summary = task(1, TaskStatus::Running, None);
    let sample = |id, machine, status, ended_at| Run {
        trace_id: String::new(),
        id: RunId(id),
        lane_id: summary.task.lane_id,
        session_id: SessionId(1),
        machine_id: MachineId(machine),
        lease_epoch: 1,
        agent_id: Some(summary.task.agent_id),
        backend: Some(BackendKind::Claude),
        status,
        outcome: None,
        rationale: None,
        error: None,
        started_at: Timestamp::from_millis(300_000),
        ended_at,
    };
    let ended = Some(Timestamp::from_millis(480_000));
    *work.task_runs.lock().unwrap() = [
        sample(1, 1, RunStatus::Committed, ended),
        sample(2, 2, RunStatus::Committed, ended),
        sample(3, 1, RunStatus::Active, None),
    ]
    .into_iter()
    .map(|run| RunSummary {
        run,
        machine: "test".to_owned(),
        steps: vec![],
        usage: None,
    })
    .collect();
    *work.tasks.lock().unwrap() = vec![summary];
    let (_dir, core) = core(&[("dev", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert_eq!(
        state.machine_runs("dev", MachineId(1)).unwrap_err(),
        Gap::Loading
    );
    state.apply_machine_history(load_machine_history(&core, "dev", &when()).await);
    let (active, recent) = state.machine_runs("dev", MachineId(1)).unwrap();
    assert_eq!(
        active.iter().map(|run| run.id()).collect::<Vec<_>>(),
        [RunId(3)]
    );
    assert_eq!(
        recent.iter().map(|run| run.id()).collect::<Vec<_>>(),
        [RunId(1)]
    );
    let detail = state.machine_detail("dev", MachineId(1), &when()).unwrap();
    assert_eq!(detail.platform.as_deref(), Some("macos · aarch64"));
    assert_eq!(detail.last_seen.as_deref(), Some("00:10"));
    assert_eq!(detail.occupancy, Some(50.));
    assert!(
        detail
            .report
            .backends
            .iter()
            .any(|probe| probe == "claude 2 logged in")
    );
    assert_eq!(detail.accounts, ["owner · claude · cooldown until 00:15"]);
    let detail = state.machine_detail("dev", MachineId(2), &when()).unwrap();
    assert_eq!(detail.occupancy, None);
    assert!(!detail.tools_reported);
    work.old.store(true, Ordering::SeqCst);
    state.apply_machine_history(load_machine_history(&core, "dev", &when()).await);
    let status = state
        .machine_runs("dev", MachineId(1))
        .unwrap_err()
        .sentence("machine runs", "dev");
    assert_eq!(status.text, "Could not load machine runs from dev.");
    assert!(status.detail.is_some());
}

#[tokio::test]
async fn archived_projects_leave_work_surfaces_and_restore_the_same_history() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    work.tasks
        .lock()
        .unwrap()
        .push(task(1, TaskStatus::Done, None));
    work.history.lock().unwrap().push(ConversationEntry::You {
        event_id: EventId(1),
        text: "A message before archive".into(),
        images: Vec::new(),
        at: Timestamp::from_millis(1_000),
    });
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    state.set_target("work", "api");
    state.apply_project(load_project(&core, "work", "api").await);
    let to = Destination::Lead("work".into(), "api".into());
    state.edit_draft(&to, "Keep this draft");
    let conversation = state.conversation("work", "api", &when());
    assert!(!conversation.is_empty());
    assert!(state.offers_archive("work", "api"));
    assert!(!state.offers_archive("work", "General"));
    assert_eq!(state.target(), Some(("work", "api")));

    let archive = Consequence::ArchiveProject("work".into(), "api".into());
    let confirmation = state.confirmation(&archive).unwrap();
    assert!(
        confirmation
            .sentence
            .contains("history stays readable under Archived")
    );
    assert!(
        confirmation
            .sentence
            .contains("unavailable to other spaces")
    );
    let project = core.set_archived(Some("work"), "api", true).await.unwrap();
    state.project_updated("work", project);
    assert!(state.is_archived("work", "api"));
    assert!(!state.offers_archive("work", "api"));
    assert_eq!(state.target(), Some(("work", "General")));
    assert!(!state.targets().contains(&("work", "api")));
    assert!(
        state
            .modes("work")
            .rows
            .iter()
            .all(|row| row.project != "api")
    );
    let space = state.space("work").unwrap();
    assert!(!space.archived_open());
    assert_eq!(
        space
            .active_projects()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["General"]
    );
    assert_eq!(
        space
            .archived_projects()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["api"]
    );
    state.toggle_archived("work");
    state.apply(refresh(&core).await);
    assert!(state.space("work").unwrap().archived_open());
    state.apply_project(load_project(&core, "work", "api").await);
    state.apply_task(load_task(&core, "work", TaskId(1)).await);
    assert!(state.task_is_archived("work", TaskId(1)));
    assert_eq!(state.conversation("work", "api", &when()), conversation);
    assert_eq!(state.project("work", "api").unwrap().tasks.len(), 1);
    let send = state.request_send(Destination::Lead("work".into(), "api".into()), when().now);
    assert!(send.held.is_none() && send.immediate.is_none() && send.flushed.is_none());
    assert_eq!(state.draft(&to), "Keep this draft");

    let project = core.set_archived(Some("work"), "api", false).await.unwrap();
    state.project_updated("work", project);
    assert!(!state.is_archived("work", "api"));
    assert!(state.offers_archive("work", "api"));
    assert_eq!(state.target(), Some(("work", "api")));
    assert!(
        state
            .modes("work")
            .rows
            .iter()
            .any(|row| row.project == "api")
    );
    assert!(
        state
            .space("work")
            .unwrap()
            .archived_projects()
            .next()
            .is_none()
    );
    assert_eq!(state.conversation("work", "api", &when()), conversation);
}

#[tokio::test]
async fn an_archived_projects_remote_cannot_move_to_another_paired_space() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    core.add_repository(
        Some("home"),
        Some("api"),
        "widgets",
        "https://github.com/Acme/Widgets.git",
        None,
    )
    .await
    .unwrap();
    core.set_archived(Some("home"), "api", true).await.unwrap();
    assert!(
        core.projects(Some("home"))
            .await
            .unwrap()
            .projects
            .iter()
            .all(|p| p.name != "api")
    );
    let refused = add_repository(
        &core,
        "work",
        "api",
        "widgets",
        "git@github.com:acme/widgets",
        "",
    )
    .await
    .unwrap_err();
    assert!(refused.contains("already registered in home, as repository widgets of project api"));
}

#[test]
fn a_newly_paired_space_appears_and_home_opens_it() {
    let mut state = AppState::paired(["home"]);
    state.open_paired_space("work");
    assert_eq!(state.filter(), Some("work"));
    assert_eq!(
        state
            .spaces()
            .iter()
            .map(|space| space.name.as_str())
            .collect::<Vec<_>>(),
        ["home", "work"]
    );
}

fn deletion_test_preview() -> threadlane_core::client::DeletePreview {
    threadlane_core::client::DeletePreview {
        tasks: 3,
        queued_tasks: 2,
        decisions: 4,
        open_decisions: 1,
        memory_files: 2,
        clones: 1,
        ..Default::default()
    }
}

#[tokio::test]
async fn deletion_dialog_requires_an_exact_name_and_shows_the_result_and_leftovers() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    state.set_target("work", "api");
    assert!(state.offers_delete("work", "api"));
    assert!(!state.offers_delete("work", "General"));
    let preview = core.delete_preview(Some("work"), "api").await.unwrap();
    assert!(!can_confirm_deletion("api", "api", None));
    for different in ["", "API", "api ", " api"] {
        assert!(!can_confirm_deletion("api", different, Some(&preview)));
    }
    assert!(can_confirm_deletion("api", "api", Some(&preview)));
    let (deleted, leftovers) = core
        .delete_project(Some("work"), "api", "api")
        .await
        .unwrap();
    state.project_deleted("work", "api", &deleted, &leftovers);
    assert!(state.project_absent("work", "api"));
    assert!(state.project("work", "api").is_none());
    assert_eq!(state.target(), Some(("work", "General")));
    assert!(
        matches!(state.activity(), Some(Activity::Done(text)) if text.contains("3 tasks (2 queued)")
        && text.contains("4 decisions (1 open)") && text.contains("workspaces/clones/1"))
    );
}

#[tokio::test]
async fn deletion_in_another_client_drops_cached_project_and_history_on_refresh() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    work.history.lock().unwrap().push(ConversationEntry::You {
        event_id: EventId(123),
        text: "Project history".to_owned(),
        images: Vec::new(),
        at: when().now,
    });
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    let stale = load_project(&core, "work", "api").await;
    assert!(!state.conversation("work", "api", &when()).is_empty());
    core.delete_project(Some("work"), "api", "api")
        .await
        .unwrap();
    state.apply(refresh(&core).await);
    state.apply_project(stale);
    assert!(state.project_absent("work", "api"));
    assert!(!state.targets().contains(&("work", "api")));
    assert!(state.project("work", "api").is_none());
    assert!(state.conversation("work", "api", &when()).is_empty());
}

#[tokio::test]
async fn the_board_and_task_page_say_whether_a_finished_branch_merges() {
    let work = FakeHub::new("Work", Vec::new());
    let mut clean = task(1, TaskStatus::Done, None);
    clean.task.result = Some(json!({
        "summary": "done",
        "merge": [{ "repository": "app", "branch": "main",
                    "base": "1a2b3c4d5e6f", "revision": "e4d3c2b1a6f5", "conflicts": [] }],
    }));
    let mut conflicted = task(2, TaskStatus::Done, None);
    conflicted.task.result = Some(json!({
        "summary": "done",
        "merge": [{ "repository": "app", "branch": "main", "base": "9f8e7d6",
                    "conflicts": ["src/a.rs", "src/b.rs"] }],
    }));
    let unchecked = task(3, TaskStatus::Done, None);
    *work.tasks.lock().expect("lock") = vec![clean, conflicted, unchecked];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);

    let done = state
        .board("work", "api", &when())
        .into_iter()
        .find(|column| column.title == "Done")
        .unwrap();
    let merge = |id| {
        done.cards
            .iter()
            .find(|card| card.id == TaskId(id))
            .unwrap()
            .merge
            .clone()
    };
    assert_eq!(
        merge(1).as_deref(),
        Some("Merges cleanly into main at 1a2b3c4 (checked e4d3c2b)")
    );
    assert_eq!(
        merge(2).as_deref(),
        Some("Conflicts with main: src/a.rs, src/b.rs")
    );
    assert_eq!(merge(3), None, "no check recorded, nothing claimed");

    state.apply_task(load_task(&core, "work", TaskId(2)).await);
    let detail = state.task("work", TaskId(2)).unwrap();
    assert_eq!(
        changes(detail).merge.as_deref(),
        Some("Conflicts with main: src/a.rs, src/b.rs")
    );
}

#[test]
fn several_repositories_are_named_in_the_merge_state() {
    let result = json!({ "merge": [
        { "repository": "app", "branch": "main", "base": "abcdef0123", "conflicts": [] },
        { "repository": "web", "branch": "trunk", "base": "0123", "conflicts": ["x"] },
    ]});
    assert_eq!(
        merge_state(Some(&result)).as_deref(),
        Some("app: Merges cleanly into main at abcdef0 · web: Conflicts with trunk: x")
    );
}

#[tokio::test]
async fn a_saved_mixed_backend_order_can_be_kept_or_cleared() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let order = vec![BackendKind::Antigravity, BackendKind::Claude];
    work.org(OrgRequest::CreateAgent {
        name: "Reviewer".into(),
        role: "reviewer".into(),
        instructions: "Review.".into(),
        backends: order.clone(),
    })
    .await
    .unwrap();
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    let fields = state.open_agent_edit("work", "Reviewer").unwrap();
    let edit = state.agent_edit().unwrap();
    let saved = edit.backends;
    assert_eq!(
        edit.saved_backend_label().as_deref(),
        Some("antigravity, then claude")
    );
    assert_eq!(edit.model_backends(), order);
    assert_eq!(state.agent_update(&fields), Ok(None));

    state.agent_edit_mut().unwrap().backends = 0;
    assert_eq!(
        state.agent_edit().unwrap().model_backends(),
        BackendKind::ALL
    );
    assert!(
        matches!(state.agent_update(&fields), Ok(Some(OrgRequest::UpdateAgent {
        backends: Some(kinds), ..
    })) if kinds.is_empty())
    );

    state.agent_edit_mut().unwrap().backends = saved;
    assert_eq!(state.agent_update(&fields), Ok(None));
}

#[tokio::test]
async fn agent_models_follow_the_provider_after_reordering_and_reopening() {
    use BackendKind::{Claude, Codex};

    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    work.org(OrgRequest::CreateAgent {
        name: "Picker".into(),
        role: "reviewer".into(),
        instructions: String::new(),
        backends: vec![Claude, Codex],
    })
    .await
    .unwrap();
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    let mut fields = state.open_agent_edit("work", "Picker").unwrap();
    let codex_first = BACKENDS
        .iter()
        .position(|(_, kinds)| *kinds == [Codex, Claude])
        .unwrap();
    state.agent_edit_mut().unwrap().backends = codex_first;
    for (kind, value) in &mut fields.models {
        match kind {
            Claude => *value = "opus".into(),
            Codex => *value = "private-model-id".into(),
            _ => {}
        }
    }
    for clear_fallback in [false, true] {
        if clear_fallback {
            fields
                .models
                .iter_mut()
                .find(|(kind, _)| *kind == Claude)
                .unwrap()
                .1
                .clear();
        }
        let request = state.agent_update(&fields).unwrap().unwrap();
        core.update_agent(Some("work"), request).await.unwrap();
        state.close_agent_edit();
        state.apply(refresh(&core).await);
        let reopened = state.open_agent_edit("work", "Picker").unwrap();
        assert_eq!(
            state.agent_edit().unwrap().model_backends(),
            [Codex, Claude]
        );
        assert_eq!(reopened.models, fields.models);
        assert_eq!(state.agent_update(&reopened), Ok(None));
        if clear_fallback {
            assert!(
                state.space("work").unwrap().agents[0]
                    .models
                    .get("claude")
                    .is_none()
            );
        }
        fields = reopened;
    }
}

#[tokio::test]
async fn agent_save_is_single_flight_and_keeps_edits_after_submit() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    work.org(OrgRequest::CreateAgent {
        name: "Reviewer".into(),
        role: "reviewer".into(),
        instructions: "Review.".into(),
        backends: vec![BackendKind::Claude],
    })
    .await
    .unwrap();
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    let mut submitted = state.open_agent_edit("work", "Reviewer").unwrap();
    submitted.name = "Critic".into();
    submitted.instructions = "First draft.".into();
    let request = state.agent_update(&submitted).unwrap().unwrap();
    let flight = state.begin_agent_save().expect("first click");
    assert!(state.agent_saving(&flight));
    assert!(
        state.begin_agent_save().is_none(),
        "second click is a no-op"
    );

    let mut current = submitted.clone();
    current.instructions = "Edited while save was pending.".into();
    state.fail_agent_save(&flight, "Hub disconnected.".into());
    assert_eq!(
        state.agent_edit().unwrap().save_error(),
        Some("Hub disconnected.")
    );
    assert!(!state.agent_saving(&flight));

    let retry = state.begin_agent_save().expect("retry is allowed");
    assert_eq!(state.agent_edit().unwrap().save_error(), None);
    let agent = core.update_agent(Some("work"), request).await.unwrap();
    assert!(state.finish_agent_save(&retry, submitted, &current, agent));
    assert_eq!(state.agent_edit().unwrap().agent, "Critic");
    assert_eq!(
        state.agent_update(&current).unwrap(),
        Some(OrgRequest::UpdateAgent {
            skills: None,
            agent: "Critic".into(),
            name: None,
            role: None,
            instructions: Some(current.instructions),
            backends: None,
            models: None,
        }),
        "only the newer edit needs another save"
    );
}

#[tokio::test]
async fn an_agent_is_edited_field_by_field_and_deleted_after_its_name_is_typed() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    for (name, role) in [("Lead", "lead"), ("Reviewer", "reviewer")] {
        work.org(OrgRequest::CreateAgent {
            name: name.to_owned(),
            role: role.to_owned(),
            instructions: "Review.".to_owned(),
            backends: vec![BackendKind::Claude],
        })
        .await
        .expect("created");
    }
    *work.bindings.lock().expect("lock") = vec![BindingSummary {
        agent: "Lead".to_owned(),
        role: "lead".to_owned(),
        mode: Mode::OnRequest,
        is_lead: true,
        max_parallel: 1,
        overrides: Default::default(),
    }];
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);
    let projects = vec![
        ("work".to_owned(), "api".to_owned()),
        ("work".to_owned(), "General".to_owned()),
    ];
    state.apply_modes(load_modes(&core, projects).await);
    let binding = core
        .set_mode(Some("work"), Some("api"), "Reviewer", Mode::Assisted)
        .await
        .expect("bound");
    state.mode_set("work", "api", binding);

    assert!(state.open_agent_edit("work", "Nobody").is_none());
    let mut fields = state.open_agent_edit("work", "Reviewer").expect("known");
    assert_eq!(
        (fields.name.as_str(), fields.instructions.as_str()),
        ("Reviewer", "Review.")
    );
    let edit = state.agent_edit().expect("open").clone();
    assert_eq!(BACKENDS[edit.backends].1, [BackendKind::Claude]);
    assert_eq!(edit.model_backends(), [BackendKind::Claude]);
    let bound: Vec<_> = state
        .agent_bindings("work", "Reviewer")
        .into_iter()
        .map(|bound| (bound.project, bound.binding.mode))
        .collect();
    assert_eq!(bound[0], ("api".to_owned(), Mode::Assisted));
    assert!(state.offers_agent_delete("work", "Reviewer"));
    assert!(!state.offers_agent_delete("work", "Lead"), "never the lead");

    work.org(OrgRequest::UpdateAgent {
        skills: None,
        agent: "Reviewer".to_owned(),
        name: None,
        role: None,
        instructions: Some("Updated by another client.".to_owned()),
        backends: None,
        models: None,
    })
    .await
    .expect("edited from another client");
    state.apply(refresh(&core).await);
    assert_eq!(state.agent_update(&fields), Ok(None), "nothing changed");
    fields.role = " ".to_owned();
    assert!(state.agent_update(&fields).is_err(), "a role is needed");
    fields.role = "reviewer".to_owned();
    fields.name = " Critic ".to_owned();
    fields.models[0].1 = "opus".to_owned();
    let skill = threadlane_core::model::SkillBinding {
        backend: BackendKind::Codex,
        name: "review".into(),
    };
    state.agent_edit_mut().unwrap().skills.push(skill.clone());
    state.agent_edit_mut().expect("open").backends = 2;
    assert_eq!(
        state.agent_edit().expect("open").model_backends(),
        [BackendKind::Codex, BackendKind::Claude]
    );
    let request = state
        .agent_update(&fields)
        .expect("valid")
        .expect("changed");
    assert_eq!(
        request,
        OrgRequest::UpdateAgent {
            skills: Some(vec![skill]),
            agent: "Reviewer".to_owned(),
            name: Some("Critic".to_owned()),
            role: None,
            instructions: None,
            backends: Some(vec!["codex".to_owned(), "claude".to_owned()]),
            models: Some([("claude".to_owned(), "opus".to_owned())].into()),
        },
        "only what changed"
    );
    let saving = state.agent_edit().expect("open").clone();
    let agent = core
        .update_agent(Some("work"), request)
        .await
        .expect("updated");
    assert_eq!(agent.instructions, "Updated by another client.");
    state.close_agent_edit();
    let reopened = state.open_agent_edit("work", "Reviewer").expect("reopened");
    state.agent_updated(&saving, agent);
    assert_eq!(
        state.agent_edit().expect("reopened view stays").agent,
        "Critic"
    );
    assert_eq!(state.agent_update(&reopened), Ok(None));
    assert_eq!(state.modes("work").agents, ["Lead", "Critic"]);
    assert_eq!(state.agent_bindings("work", "Critic").len(), 1, "renamed");
    assert_eq!(
        state.space("work").expect("space").agents[1].models["claude"],
        "opus"
    );

    state.open_agent_edit("work", "Critic").expect("known");
    let deleted = core
        .delete_agent(Some("work"), "Critic", "Critic")
        .await
        .expect("deleted");
    state.agent_deleted("work", &deleted);
    assert!(state.agent_edit().is_none());
    assert_eq!(state.modes("work").agents, ["Lead"]);
    assert!(state.agent_bindings("work", "Critic").is_empty());
    assert!(
        work.agents
            .lock()
            .expect("lock")
            .iter()
            .all(|a| a.name != "Critic")
    );
}

/// A delivered lead message of `project` at `at`.
fn said_by_lead(seq: i64, project: &str, text: &str, at: i64) -> ConversationEntry {
    let mut item = feed_item(seq, OutboxKind::Message, json!({ "text": text }), at);
    item.project_name = Some(project.to_owned());
    item.message.project_id = Some(project_id(Some(project)));
    ConversationEntry::Delivered { item }
}

fn said_by_you(id: i64, text: &str, at: i64) -> ConversationEntry {
    ConversationEntry::You {
        event_id: EventId(id),
        text: text.to_owned(),
        images: Vec::new(),
        at: Timestamp::from_millis(at),
    }
}

fn timeline_of(state: &AppState) -> Vec<String> {
    state
        .timeline()
        .iter()
        .map(|entry| {
            let who = match entry.spoken {
                Spoken::Said(_, _, Author::You) => "you".to_owned(),
                Spoken::Said(_, _, by) => by.label(),
                Spoken::Delivered(_) => "lead".to_owned(),
            };
            format!(
                "{} / {} {who}: {}",
                entry.space,
                entry.project.unwrap_or("-"),
                entry.text()
            )
        })
        .collect()
}

#[tokio::test]
async fn home_merges_every_conversation_of_every_space_newest_first() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    work.histories.lock().unwrap().extend([
        (
            "api".to_owned(),
            vec![
                said_by_you(1, "Fix the refunds", 1_000),
                said_by_lead(1, "api", "On it", 4_000),
                ConversationEntry::Delivered {
                    item: feed_item(
                        2,
                        OutboxKind::Decision,
                        json!({ "decision": "D-0001", "question": "Open it?" }),
                        5_000,
                    ),
                },
            ],
        ),
        (
            "General".to_owned(),
            vec![said_by_you(2, "What is everyone doing?", 2_000)],
        ),
    ]);
    home.histories.lock().unwrap().insert(
        "General".to_owned(),
        vec![said_by_lead(1, "General", "Garden watered", 3_000)],
    );
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert_eq!(
        state.timeline_projects(),
        [
            pair("work", "api"),
            pair("work", "General"),
            pair("home", "api"),
            pair("home", "General"),
        ]
    );
    state.apply_timeline(load_timeline(&core, state.timeline_projects()).await);
    // The feed delivers one of them again: still shown once.
    state.receive(SpaceFeedItem {
        space: "work".to_owned(),
        item: feed_item(1, OutboxKind::Message, json!({ "text": "On it" }), 4_000),
    });

    let all = [
        "work / api lead: On it",
        "home / General lead: Garden watered",
        "work / General you: What is everyone doing?",
        "work / api you: Fix the refunds",
    ];
    assert_eq!(timeline_of(&state), all, "decisions stay under Needs you");
    assert!(!state.timeline_has_earlier());
    // The project screen shows the same history.
    assert_eq!(
        said(&state.conversation("work", "api", &when()))
            .into_iter()
            .filter(|line| !line.starts_with("--"))
            .collect::<Vec<_>>(),
        [
            "you: Fix the refunds",
            "lead: On it",
            "D-0001: pending false"
        ]
    );

    state.set_filter(Some("home"));
    assert_eq!(timeline_of(&state), ["home / General lead: Garden watered"]);
    assert_eq!(
        state.timeline_projects(),
        [pair("home", "api"), pair("home", "General")]
    );
    state.set_filter(None);
    assert_eq!(timeline_of(&state), all);
}

#[tokio::test]
async fn home_s_timeline_pages_and_stops_where_a_conversation_is_not_loaded() {
    let work = FakeHub::new("Work", Vec::new());
    // api has 30 lead messages, one a second from 1 s; General one at 0.5 s.
    work.histories.lock().unwrap().extend([
        (
            "api".to_owned(),
            (1..=30)
                .map(|n| said_by_lead(n, "api", &format!("api {n}"), n * 1_000))
                .collect(),
        ),
        (
            "General".to_owned(),
            vec![said_by_you(1, "older in General", 500)],
        ),
    ]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_timeline(load_timeline(&core, state.timeline_projects()).await);

    // Only api's newest 20 (11..=30) are loaded, so nothing older than 11 s
    // is shown yet: General's message waits until api's are loaded past it.
    let shown = timeline_of(&state);
    assert_eq!(shown.len(), TIMELINE_PAGE);
    assert_eq!(shown[0], "work / api lead: api 30");
    assert_eq!(shown[19], "work / api lead: api 11");
    assert!(state.timeline_has_earlier());

    let earlier = state.show_earlier_timeline();
    assert_eq!(
        earlier,
        [(
            "work".to_owned(),
            "api".to_owned(),
            Timestamp::from_millis(11_000)
        )]
    );
    assert_eq!(
        timeline_of(&state).len(),
        TIMELINE_PAGE,
        "nothing more loaded yet"
    );
    for (space, project, before) in earlier {
        assert!(state.begin_earlier(&space, &project, before));
        assert!(state.apply_earlier(load_earlier(&core, &space, &project, before).await));
    }
    let shown = timeline_of(&state);
    assert_eq!(shown.len(), 31);
    assert_eq!(shown[29], "work / api lead: api 1");
    assert_eq!(shown[30], "work / General you: older in General");
    assert!(!state.timeline_has_earlier());
    assert!(state.show_earlier_timeline().is_empty());

    // A refresh of the newest page keeps how far back is loaded.
    state.apply_timeline(load_timeline(&core, state.timeline_projects()).await);
    assert_eq!(timeline_of(&state).len(), 31);
}

#[test]
fn earlier_chat_pages_retry_the_same_cursor_without_dropping_visible_history() {
    let mut state = AppState::paired(["work"]);
    let page = (1..=i64::from(PAGE))
        .map(|offset| said_by_lead(offset + 10, "General", "visible", (offset + 10) * 1_000))
        .collect();
    state.take_history("work", "General", page, true, PAGE);
    let before = state.earlier("work", "General").expect("full initial page");
    let shown = state.conversation("work", "General", &when());

    assert!(state.begin_earlier("work", "General", before));
    assert!(!state.begin_earlier("work", "General", before));
    assert_eq!(
        state.earlier_status("work", "General"),
        Some(&EarlierStatus::Loading)
    );
    assert_eq!(state.earlier_status("work", "Other project"), None);
    assert!(!state.apply_earlier(EarlierLoad {
        space: "work".into(),
        project: "General".into(),
        before,
        history: Err(ClientError::Unreachable {
            space: "work".into(),
            reason: "disconnected".into(),
        }),
        decision_records: Vec::new(),
    }));
    assert!(matches!(
        state.earlier_status("work", "General"),
        Some(EarlierStatus::Failed(why)) if why.contains("disconnected")
    ));
    assert_eq!(state.conversation("work", "General", &when()), shown);
    assert_eq!(state.earlier("work", "General"), Some(before));

    assert!(state.begin_earlier("work", "General", before));
    assert!(state.apply_earlier(EarlierLoad {
        space: "work".into(),
        project: "General".into(),
        before,
        history: Ok(vec![said_by_lead(1, "General", "earlier", 1_000)]),
        decision_records: Vec::new(),
    }));
    assert!(state.conversation("work", "General", &when()).len() > shown.len());
    assert_eq!(state.earlier("work", "General"), None);
    assert_eq!(state.earlier_status("work", "General"), None);
}

#[tokio::test]
async fn home_s_composer_stays_on_general_until_you_pick_another_project() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert_eq!(state.target(), Some(("work", "General")));
    // Writing in a project's own conversation does not move Home's composer.
    state.sent("work", "api", "From the api tab", EventId(9), when().now);
    assert_eq!(state.target(), Some(("work", "General")));
    assert_eq!(
        timeline_of(&state),
        ["work / api you: From the api tab"],
        "and the timeline has it at once"
    );
    state.set_target("work", "api");
    assert_eq!(state.target(), Some(("work", "api")));
}

#[tokio::test]
async fn the_space_lead_s_asks_and_the_replies_are_labeled_and_linked() {
    let work = FakeHub::new("Work", Vec::new());
    work.histories.lock().unwrap().extend([
        (
            "api".to_owned(),
            vec![
                ConversationEntry::SpaceLead {
                    event_id: EventId(5),
                    text: "Ship the refunds fix".to_owned(),
                    at: Timestamp::from_millis(2_000),
                },
                said_by_lead(1, "api", "Shipped", 3_000),
            ],
        ),
        (
            "General".to_owned(),
            vec![
                said_by_you(1, "Get refunds shipped", 1_000),
                ConversationEntry::ProjectReply {
                    event_id: EventId(6),
                    project: "api".to_owned(),
                    ask_event_id: Some(EventId(5)),
                    text: "The lead of api replied: Shipped".to_owned(),
                    at: Timestamp::from_millis(3_500),
                },
            ],
        ),
    ]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_timeline(load_timeline(&core, state.timeline_projects()).await);

    let lines = |project| {
        said(&state.conversation("work", project, &when()))
            .into_iter()
            .filter(|line| !line.starts_with("--"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lines("api"),
        ["General lead: Ship the refunds fix", "lead: Shipped"],
        "an ask is not yours"
    );
    assert_eq!(
        lines("General"),
        [
            "you: Get refunds shipped",
            "api lead: The lead of api replied: Shipped"
        ]
    );
    assert!(
        state
            .conversation("work", "General", &when())
            .iter()
            .any(|line| matches!(line, Line::ProjectReply { project, .. } if project == "api"))
    );
    assert_eq!(
        timeline_of(&state),
        [
            "work / General api lead: The lead of api replied: Shipped",
            "work / api lead: Shipped",
            "work / api General lead: Ship the refunds fix",
            "work / General you: Get refunds shipped",
        ]
    );
    let reply = state.timeline()[0];
    assert!(matches!(reply.spoken, Spoken::Said(_, _, Author::ProjectLead(from)) if from == "api"));
}

#[tokio::test]
async fn home_timeline_entries_have_stable_prose_ids_and_disclosure_for_said_messages() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    work.histories.lock().unwrap().insert(
        "General".to_owned(),
        vec![
            said_by_you(1, "See **T-0042** and `code`", 1_000),
            said_by_lead(1, "General", "Done", 2_000),
        ],
    );
    home.histories.lock().unwrap().insert(
        "General".to_owned(),
        vec![said_by_you(1, "Water plants", 1_500)],
    );
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_timeline(load_timeline(&core, state.timeline_projects()).await);

    let ids: Vec<String> = state
        .timeline()
        .iter()
        .map(TimelineEntry::prose_id)
        .collect();
    assert_eq!(
        ids,
        [
            "result-work-1",
            "timeline-said-home-1",
            "timeline-said-work-1",
        ]
    );
    let work_said_id = ids[2].clone();
    assert_eq!(
        markdown_source(&state.timeline()[2].text()),
        "See **[T-0042](threadlane:T-0042)** and `code`"
    );

    // Expanding a Spoken::Said entry uses its stable prose_id with ProseLimit::Result.
    assert_eq!(
        ProseLimit::Result.disclosure(state.message_expanded(&work_said_id), true),
        (Some(3), Some("Show more"))
    );
    state.toggle_message(&work_said_id);
    assert_eq!(
        ProseLimit::Result.disclosure(state.message_expanded(&work_said_id), true),
        (None, Some("Show less"))
    );
    assert!(!state.message_expanded("timeline-said-home-1"));

    // Adding a newer message shifts list indices without changing the existing prose_id.
    work.histories
        .lock()
        .unwrap()
        .get_mut("General")
        .unwrap()
        .push(said_by_you(2, "Follow-up", 3_000));
    state.apply_timeline(load_timeline(&core, state.timeline_projects()).await);
    assert_eq!(state.timeline()[3].prose_id(), work_said_id);
    assert!(state.message_expanded(&work_said_id));
}

#[tokio::test]
async fn inbox_timeline_loads_repositories_and_links_github_references_per_project() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    *work.repositories.lock().unwrap() = vec![Repository {
        id: RepositoryId(1),
        project_id: ProjectId(1),
        name: "threadlane".into(),
        remote_url: "https://github.com/wheregmis/threadlane.git".into(),
        default_branch: "main".into(),
        protected_branches: Vec::new(),
    }];
    *home.repositories.lock().unwrap() = vec![
        Repository {
            id: RepositoryId(1),
            project_id: ProjectId(1),
            name: "site".into(),
            remote_url: "git@github.com:acme/site.git".into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        },
        Repository {
            id: RepositoryId(2),
            project_id: ProjectId(2),
            name: "notes".into(),
            remote_url: "/srv/git/notes".into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        },
    ];
    work.histories.lock().unwrap().extend([
        (
            "api".to_owned(),
            vec![
                said_by_you(1, "Check #572", 1_000),
                said_by_lead(1, "api", "Merged #572", 2_000),
            ],
        ),
        (
            "General".to_owned(),
            vec![
                said_by_you(2, "Any news on #572?", 3_000),
                ConversationEntry::ProjectReply {
                    event_id: EventId(3),
                    project: "api".to_owned(),
                    ask_event_id: None,
                    text: "The lead of api replied: #572 is ready".to_owned(),
                    at: Timestamp::from_millis(4_000),
                },
            ],
        ),
    ]);
    home.histories.lock().unwrap().extend([
        (
            "api".to_owned(),
            vec![said_by_lead(1, "api", "Opened #572", 5_000)],
        ),
        (
            "General".to_owned(),
            vec![said_by_lead(2, "General", "Note #572", 6_000)],
        ),
    ]);
    *work.recent.lock().unwrap() = vec![account_notice(9, "quota", "Notice #572")];

    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    // Cold launch Inbox: only load_timeline runs (no load_project).
    state.apply_timeline(load_timeline(&core, state.timeline_projects()).await);

    assert_eq!(
        state.github_repository("work", "api").as_deref(),
        Some("wheregmis/threadlane")
    );
    assert_eq!(state.github_repository("work", "General"), None);
    assert_eq!(
        state.github_repository("home", "api").as_deref(),
        Some("acme/site")
    );
    assert_eq!(state.github_repository("home", "General"), None);

    let rendered: Vec<(String, String)> = state
        .timeline()
        .iter()
        .map(|entry| {
            let repo = entry
                .repository_project()
                .and_then(|project| state.github_repository(entry.space, project));
            (
                entry.prose_id(),
                markdown_source_for(&entry.text(), None, repo.as_deref()),
            )
        })
        .collect();
    assert_eq!(
        rendered,
        [
            ("result-home-2".to_owned(), "Note #572".to_owned()),
            (
                "result-home-1".to_owned(),
                "Opened [#572](https://github.com/acme/site/pull/572)".to_owned(),
            ),
            (
                "timeline-said-work-3".to_owned(),
                "The lead of api replied: [#572](https://github.com/wheregmis/threadlane/pull/572) is ready".to_owned(),
            ),
            ("timeline-said-work-2".to_owned(), "Any news on #572?".to_owned()),
            (
                "result-work-1".to_owned(),
                "Merged [#572](https://github.com/wheregmis/threadlane/pull/572)".to_owned(),
            ),
            (
                "timeline-said-work-1".to_owned(),
                "Check [#572](https://github.com/wheregmis/threadlane/pull/572)".to_owned(),
            ),
            ("result-work-9".to_owned(), "Notice #572".to_owned()),
        ]
    );
}

#[tokio::test]
async fn image_only_drafts_are_scoped_and_survive_undo_failure_and_history_reload() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    let general = Destination::Lead("work".into(), "General".into());
    let other = Destination::Lead("work".into(), "api".into());
    let at = when().now;
    let image = ImageAttachment::new("reference.png".into(), vec![1, 2, 3]);
    state.loading_images(&general);
    assert!(!state.is_send_enabled(&general));
    state.attach_images(&general, Ok(vec![image.clone()]));
    assert!(state.is_send_enabled(&general));
    assert!(state.draft_images(&other).is_empty());

    let held = state.request_send(general.clone(), at).held.unwrap();
    assert!(
        matches!(state.conversation("work", "General", &when()).last(),
        Some(Line::Sending { images, .. }) if images == std::slice::from_ref(&image))
    );
    state.remove_image(&general, &image);
    assert!(state.undo_send(&general));
    assert_eq!(state.draft_images(&general), std::slice::from_ref(&image));
    assert!(state.tick_send(held).is_none());

    let submit = |state: &mut AppState| {
        state.request_send(general.clone(), at).held.unwrap();
        state.request_send(general.clone(), at).flushed.unwrap()
    };
    let send = submit(&mut state);
    state.finish_send(
        send,
        Err(ClientError::Unreachable {
            space: "work".into(),
            reason: "offline".into(),
        }),
    );
    assert_eq!(state.draft_images(&general), std::slice::from_ref(&image));

    let send = submit(&mut state);
    let result = send.deliver(&core).await;
    let event_id = result.as_ref().unwrap().accepted.event_id;
    let new_image = ImageAttachment::new("later.png".into(), vec![4, 5, 6]);
    state.attach_images(&general, Ok(vec![new_image.clone()]));
    state.finish_send(send, result);
    assert_eq!(state.draft_images(&general), &[new_image]);

    let history = ConversationEntry::You {
        event_id,
        text: String::new(),
        images: vec![image.clone()],
        at,
    };
    state.take_history("work", "General", vec![history], true, PAGE);
    assert!(
        matches!(state.conversation("work", "General", &when()).last(),
        Some(Line::You { images, .. }) if images == std::slice::from_ref(&image))
    );
    assert_eq!(
        state
            .timeline()
            .iter()
            .filter(|row| row.images == std::slice::from_ref(&image))
            .count(),
        1
    );
    assert!(state.conversation("work", "api", &when()).is_empty());
}

#[tokio::test]
async fn space_tasks_view_groups_sorts_newest_first_collapses_done_and_filters() {
    let make = |id: i64, title: &str, project: &str, agent: &str, status: TaskStatus, at: i64| {
        let mut summary = task(id, status, None);
        summary.task.title = title.into();
        summary.task.created_at = Timestamp::from_millis(at);
        summary.project = project.into();
        summary.agent = agent.into();
        summary.machine = Some("hub".into());
        summary.running_since = (status == TaskStatus::Running).then(|| Timestamp::from_millis(at));
        summary.completed_at =
            (status == TaskStatus::Done).then(|| Timestamp::from_millis(at + 60_000));
        summary
    };
    let tasks = vec![
        make(1, "Old done", "api", "Engineer", TaskStatus::Done, 100_000),
        make(2, "New done", "web", "Reviewer", TaskStatus::Done, 300_000),
        make(
            3,
            "Broken build",
            "api",
            "Engineer",
            TaskStatus::Failed,
            200_000,
        ),
        make(
            4,
            "Waiting on approval",
            "web",
            "Lead",
            TaskStatus::WaitingOnYou,
            150_000,
        ),
        make(
            5,
            "Active work",
            "api",
            "Engineer",
            TaskStatus::Running,
            250_000,
        ),
        make(
            6,
            "Queued work",
            "web",
            "Engineer",
            TaskStatus::Queued,
            180_000,
        ),
    ];
    let hub = FakeHub::new("Work", Vec::new());
    *hub.tasks.lock().expect("lock") = tasks;
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);

    let view = state.space_tasks_view("work", &when());
    assert_eq!(view.total, 6);
    assert_eq!(view.projects, vec!["api", "web"]);
    assert_eq!(view.agents, vec!["Engineer", "Lead", "Reviewer"]);
    let groups: Vec<TaskGroup> = view.sections.iter().map(|s| s.group).collect();
    assert_eq!(
        groups,
        vec![
            TaskGroup::NeedsYou,
            TaskGroup::Failed,
            TaskGroup::Running,
            TaskGroup::Queued,
            TaskGroup::Done,
        ]
    );
    // Done starts collapsed when active/failed work exists.
    let done = view.sections.last().unwrap();
    assert!(done.collapsed);
    assert_eq!(done.total, 2);
    assert!(done.rows.is_empty());

    // Expanding Done shows newest first and marks Done rows as quiet.
    state.toggle_done_tasks_expanded("work");
    let expanded = state.space_tasks_view("work", &when());
    let done_rows = &expanded.sections.last().unwrap().rows;
    assert_eq!(done_rows.len(), 2);
    assert_eq!(done_rows[0].id, TaskId(2));
    assert_eq!(done_rows[1].id, TaskId(1));
    assert!(done_rows[0].quiet);
    assert!(!expanded.sections[1].rows[0].quiet);

    // Filtering by project + status narrows sections cleanly.
    state.set_space_task_project_filter("work", Some("api".into()));
    state.set_space_task_status_filter("work", Some(TaskGroup::Failed));
    let filtered = state.space_tasks_view("work", &when());
    assert_eq!(filtered.matched, 1);
    assert_eq!(filtered.sections.len(), 1);
    assert_eq!(filtered.sections[0].rows[0].id, TaskId(3));

    state.clear_space_task_filter("work");
    assert!(!state.space_task_filter("work").is_active());
}

#[test]
fn decision_card_and_changes_extract_diff_preview_without_leaking_into_details() {
    let preview = threadlane_core::workspace::DiffPreview {
        repository: "threadlane".into(),
        base_branch: "main".into(),
        files: vec![threadlane_core::workspace::DiffFileStat {
            path: "crates/threadlane-desktop/src/tasks.rs".into(),
            insertions: 42,
            deletions: 7,
            binary: false,
        }],
        insertions: 42,
        deletions: 7,
        patch: "@@ -1,2 +1,3 @@\n+hello".into(),
        truncated: false,
    };
    let mut decision = decision(1, 100_000);
    decision.question = "Push branch threadlane/T-0001 of threadlane?".into();
    decision.tool = "publish_branch".into();
    decision.arguments = serde_json::json!({
        "repository": "threadlane",
        threadlane_core::link::DIFF_PREVIEW: preview,
    });
    let card = AppState::decision_card(&Queued {
        space: "work".into(),
        decision,
    });
    assert_eq!(card.diff.as_ref(), Some(&preview));
    assert!(
        card.details
            .iter()
            .all(|line| !line.contains("diff_preview"))
    );

    let mut summary = task(1, TaskStatus::Done, None);
    summary.task.title = "Ship diff preview".into();
    summary.task.result = Some(serde_json::json!({
        threadlane_core::link::MERGE_CHECKS: [threadlane_core::workspace::MergeCheck {
            repository: "threadlane".into(),
            branch: "main".into(),
            base: "abcdef1234567890".into(),
            revision: Some("1234567890abcdef".into()),
            conflicts: Vec::new(),
            diff: Some(preview.clone()),
        }]
    }));
    let detail = TaskDetail {
        summary,
        branch: "threadlane/T-0001".into(),
        proposed_by: None,
        repositories: Vec::new(),
        stopped: false,
        runs: Vec::new(),
        reviews: Vec::new(),
        decisions: Vec::new(),
        messages: Vec::new(),
        related: Vec::new(),
    };
    assert_eq!(changes(&detail).diffs, vec![preview]);
}

#[test]
fn prose_humanizes_iso_timestamps_and_long_shas_outside_code_and_urls() {
    // 2026-11-18 18:00 UTC, offset -5h -> 13:00 local.
    let now = parse_iso_utc("2026-11-18T18:00:00Z").unwrap();
    let local = When {
        now,
        offset_minutes: -300,
    };
    let input = "Merged 9667947bf370cfe005fb9f0553cbbdd6a1ce2c4b at 2026-11-18T17:19:47Z \
                 (keep `9667947bf370cfe005fb9f0553cbbdd6a1ce2c4b`, \
                 [commit](https://example.com/commit/bcee6b3ac19fc5a6d3039fd55d4813bdfd883568), \
                 2026-11-18T17:19:€Z, and \
                 https://github.com/org/repo/commit/9667947bf370cfe005fb9f0553cbbdd6a1ce2c4b).";
    let rendered = markdown_source_with_time(input, Some(&local));
    assert!(rendered.contains("Merged `9667947` at 12:19"), "{rendered}");
    assert!(
        rendered.contains("`9667947bf370cfe005fb9f0553cbbdd6a1ce2c4b`"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "[commit](https://example.com/commit/bcee6b3ac19fc5a6d3039fd55d4813bdfd883568)"
        ),
        "{rendered}"
    );
    assert!(rendered.contains("2026-11-18T17:19:€Z"), "{rendered}");
    assert!(
        rendered.contains(
            "https://github.com/org/repo/commit/9667947bf370cfe005fb9f0553cbbdd6a1ce2c4b"
        ),
        "{rendered}"
    );
}

#[tokio::test]
async fn batched_refresh_supplies_the_project_board_and_the_tasks_tab_skips_chat() {
    let work = FakeHub::new("Work", Vec::new());
    work.batched_work.store(true, Ordering::SeqCst);
    work.tasks
        .lock()
        .unwrap()
        .push(task(7, TaskStatus::Queued, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    let refreshed = refresh(&core).await;
    let (tasks, lead) = refreshed.project_parts("work", "api");
    assert_eq!((tasks, lead), (true, true));
    assert_eq!(refreshed.project_parts("work", "General"), (true, true));
    state.apply(refreshed);
    assert_eq!(state.project_gap("work", "General", Part::Tasks), None);
    let loaded = load_project_tab(&core, "work", "api", Some(Tab::Tasks), tasks, lead).await;
    assert!(loaded.tasks.is_none());
    assert!(loaded.lead.is_none());
    assert!(loaded.history.is_none());
    assert!(loaded.recorded.is_none());
    assert!(loaded.rules.is_none());
    assert!(loaded.lead_runs.is_none());
    state.apply_project(loaded);
    assert_eq!(
        state.project("work", "api").unwrap().tasks[0].task.id,
        TaskId(7)
    );
    assert_eq!(work.project_task_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_refresh_yields_a_healthy_hub_without_waiting_for_slow_peers() {
    let names = ["one", "two", "three", "four", "five"];
    let hubs: Vec<_> = names
        .iter()
        .map(|name| FakeHub::new(name, Vec::new()))
        .collect();
    let pairs: Vec<_> = names.into_iter().zip(&hubs).collect();
    let (_dir, core) = core(&pairs);
    let mut state = AppState::paired(names.into_iter().chain(["old-pairing"]));
    let mut gates: Vec<_> = hubs
        .iter()
        .map(|hub| {
            let (started, watch) = oneshot::channel();
            let (release, blocked) = oneshot::channel();
            *hub.blocked_info.lock().unwrap() = Some((started, blocked));
            (watch, Some(release))
        })
        .collect();
    hubs[1].decisions.lock().unwrap().push(decision(2, 42));

    let (send, mut receive) = mpsc::unbounded_channel();
    let refresh = tokio::spawn(async move {
        refresh_each(&core, |_, part| send.send(part).is_ok()).await;
    });

    // The first four peers are all in flight, and the fifth waits for a slot.
    for (started, _) in &mut gates[..4] {
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .expect("a slot should start")
            .unwrap();
    }
    assert!(matches!(
        gates[4].0.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));

    // Free only the second slot. Its result appears while slot one is blocked.
    gates[1].1.take().unwrap().send(()).unwrap();
    let part = tokio::time::timeout(Duration::from_secs(2), receive.recv())
        .await
        .expect("the healthy peer should not be blocked")
        .unwrap();
    assert_eq!(part.refreshed_space(), Some("two"));
    state.checking_connections();
    state.apply_partial(part);
    assert!(state.checking_connections);
    assert_eq!(
        state.connection("two", &when()).unwrap().status,
        ConnectionStatus::Live
    );
    assert_eq!(
        state.connection("one", &when()).unwrap().status,
        ConnectionStatus::Reconnecting
    );
    assert_eq!(state.queue.len(), 1);

    tokio::time::timeout(Duration::from_secs(2), &mut gates[4].0)
        .await
        .expect("the first free slot should start another peer")
        .unwrap();
    for (_, release) in gates {
        if let Some(release) = release {
            release.send(()).unwrap();
        }
    }
    while let Some(part) = receive.recv().await {
        state.apply_partial(part);
    }
    refresh.await.unwrap();

    // Keep old pairings while only partial data is available; remove
    // them from the display when the complete roster has been confirmed.
    assert!(state.space("old-pairing").is_some());
    state.retain_paired_spaces(&names.map(str::to_owned));
    assert!(state.space("old-pairing").is_none());
    assert_eq!(
        state
            .spaces
            .iter()
            .map(|space| space.name.as_str())
            .collect::<Vec<_>>(),
        names
    );
    assert_eq!(state.queue.len(), 1);
    assert_eq!(state.queue[0].space, "two");
}

#[tokio::test]
async fn a_legacy_refresh_reuses_only_the_project_parts_it_supplied() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert_eq!(state.project_gap("work", "api", Part::Lead), None);
    assert_eq!(state.project_gap("work", "api", Part::Tasks), None);

    work.lead_fail.store(true, Ordering::SeqCst);
    let refreshed = refresh(&core).await;
    let (tasks, lead) = refreshed.project_parts("work", "api");
    assert_eq!((tasks, lead), (true, false));
    state.apply(refreshed);
    let loaded = load_project_tab(&core, "work", "api", Some(Tab::Tasks), tasks, lead).await;
    assert!(loaded.tasks.is_none());
    assert!(matches!(loaded.lead, Some(Err(_))));

    let mut older = load_project_tab(&core, "work", "api", Some(Tab::Tasks), false, false).await;
    older.keep_refreshed_parts((true, false));
    assert!(older.tasks.is_none());
    assert!(matches!(older.lead, Some(Err(_))));

    work.projects_fail.store(true, Ordering::SeqCst);
    assert_eq!(
        refresh(&core).await.project_parts("work", "api"),
        (false, false)
    );
}

#[test]
fn task_dag_layers_dependencies_highlights_bottlenecks_and_classifies_edges() {
    let mut t1 = task(1, TaskStatus::Done, None);
    t1.task.title = "Foundation".into();

    let mut t2 = task(2, TaskStatus::Running, None);
    t2.task.title = "Build feature".into();
    t2.task.depends_on = vec![TaskId(1)];

    let mut t3 = task(3, TaskStatus::Queued, None);
    t3.task.title = "Ship feature".into();
    t3.task.depends_on = vec![TaskId(2)];

    let mut t4 = task(4, TaskStatus::Failed, None);
    t4.task.title = "Broken step".into();
    t4.task.depends_on = vec![TaskId(1)];

    let mut t5 = task(5, TaskStatus::Queued, None);
    t5.task.title = "Blocked downstream".into();
    t5.task.depends_on = vec![TaskId(4)];

    let dag = build_task_dag(&[t1, t2, t3, t4, t5], &when());
    assert!(dag.has_dependencies);
    assert_eq!(dag.nodes.len(), 5);
    assert_eq!(dag.edges.len(), 4);

    let node = |id: i64| dag.nodes.iter().find(|n| n.id == TaskId(id)).unwrap();
    assert_eq!(node(1).layer, 0);
    assert_eq!(node(2).layer, 1);
    assert_eq!(node(3).layer, 2);
    assert_eq!(node(4).layer, 1);
    assert_eq!(node(5).layer, 2);

    // T-0002 (Running -> Queued T-0003) and T-0004 (Failed -> Queued T-0005) are bottlenecks.
    assert!(!node(1).is_bottleneck);
    assert!(node(2).is_bottleneck);
    assert_eq!(node(2).downstream_blocked, 1);
    assert!(node(4).is_bottleneck);
    assert_eq!(node(4).downstream_blocked, 1);
    assert_eq!(dag.bottlenecks, 2);

    let edge = |from: i64, to: i64| {
        let id = format!("edge-{}-{}", TaskId(from), TaskId(to));
        dag.edges.iter().find(|e| e.id == id).unwrap().state
    };
    assert_eq!(edge(1, 2), EdgeState::Active);
    assert_eq!(edge(2, 3), EdgeState::Active);
    assert_eq!(edge(1, 4), EdgeState::Satisfied);
    assert_eq!(edge(4, 5), EdgeState::Blocked);
    assert_eq!(dag.active_edges, 2);
}

#[test]
fn task_dag_handles_cycles_and_flat_lists_without_panicking() {
    let mut a = task(10, TaskStatus::Queued, None);
    a.task.depends_on = vec![TaskId(11)];
    let mut b = task(11, TaskStatus::Queued, None);
    b.task.depends_on = vec![TaskId(10)];
    let cyclic = build_task_dag(&[a, b], &when());
    assert_eq!(cyclic.nodes.len(), 2);
    // Root must never be counted in its own downstream_blocked count.
    for node in &cyclic.nodes {
        assert_eq!(node.downstream_blocked, 1);
    }

    let flat = build_task_dag(
        &[
            task(1, TaskStatus::Done, None),
            task(2, TaskStatus::Running, None),
        ],
        &when(),
    );
    assert!(!flat.has_dependencies);
    assert_eq!(flat.nodes.len(), 2);
    assert!(flat.edges.is_empty());
}

#[tokio::test]
async fn agent_topology_builds_three_tier_org_and_lights_up_active_delegations() {
    let mut running = task(21, TaskStatus::Running, None);
    running.project = "api".into();
    running.agent = "Engineer".into();
    let hub = FakeHub::new("Work", Vec::new());
    hub.agents.lock().expect("lock").push(Agent {
        id: AgentId(1),
        name: "Engineer".into(),
        role: "engineer".into(),
        instructions: String::new(),
        backends: vec![BackendKind::Claude],
        models: Default::default(),
        tools: serde_json::json!([]),
        requirements: Default::default(),
        skills: Vec::new(),
    });
    *hub.tasks.lock().expect("lock") = vec![running];
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);

    let topo = state.agent_topology("work", None, &when());
    assert!(topo.nodes.iter().any(|n| matches!(
        n.kind,
        TopologyNodeKind::SpaceLead { ref space } if space == "work"
    )));
    assert!(topo.nodes.iter().any(|n| matches!(
        n.kind,
        TopologyNodeKind::Project { ref project, .. } if project == "api"
    )));
    assert!(topo.nodes.iter().any(|n| matches!(
        n.kind,
        TopologyNodeKind::Agent { ref agent, .. } if agent == "Engineer"
    )));
    assert!(
        topo.edges
            .iter()
            .any(|e| e.state == EdgeState::Active && e.label.as_deref() == Some("T-0021"))
    );
    assert!(topo.active_delegations >= 1);

    // Toggling an agent selects and deselects it for the inline autonomy dial.
    state.toggle_topology_agent("work", "Engineer");
    assert_eq!(state.topology_selected_agent(), Some(("work", "Engineer")));
    state.set_topology_project("work", Some("api".into()));
    state.space_removed("work");
    assert_eq!(state.topology_project("work"), None);
    assert_eq!(state.topology_selected_agent(), None);
}

#[tokio::test]
async fn team_cards_show_each_backend_model_and_cut_long_names() {
    let hub = FakeHub::new("Work", Vec::new());
    hub.agents.lock().expect("lock").push(Agent {
        id: AgentId(1),
        name: "Engineer".into(),
        role: "engineer".into(),
        instructions: String::new(),
        backends: vec![BackendKind::Claude, BackendKind::Codex],
        models: json!({"claude": "claude-opus-5-5-extended-thinking", "codex": "gpt-5"}),
        tools: json!([]),
        requirements: Default::default(),
        skills: Vec::new(),
    });
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);

    let agent = &state.space("work").expect("space").agents[0];
    assert_eq!(
        backend_summary(agent),
        (
            "claude · claude-opus-5-5-exte… → codex · gpt-5".to_owned(),
            "claude · claude-opus-5-5-extended-thinking → codex · gpt-5".to_owned(),
        )
    );
    let topo = state.agent_topology("work", None, &when());
    let card = topo
        .nodes
        .iter()
        .find(|n| matches!(n.kind, TopologyNodeKind::Agent { .. }))
        .expect("agent card");
    assert_eq!(
        card.subtitle,
        "engineer · claude · claude-opus-5-5-exte… → codex · gpt-5"
    );
    assert_eq!(
        card.tooltip.as_deref(),
        Some("engineer · claude · claude-opus-5-5-extended-thinking → codex · gpt-5")
    );

    // No order and no models is either backend at its default.
    let mut any = agent.clone();
    any.backends.clear();
    any.models = json!({});
    assert_eq!(backend_summary(&any).0, "Any backend");
    any.models = json!({"codex": "gpt-5"});
    assert_eq!(backend_summary(&any).0, "Any backend, codex · gpt-5");
}

#[tokio::test]
async fn agent_topology_shows_general_handoffs_and_keeps_queued_lead_edge_pending() {
    let mut general_run = task(30, TaskStatus::Running, None);
    general_run.project = "General".into();
    general_run.agent = "Engineer".into();
    let mut queued_api = task(31, TaskStatus::Queued, None);
    queued_api.project = "api".into();
    queued_api.agent = "Engineer".into();
    let hub = FakeHub::new("Work", Vec::new());
    hub.agents.lock().expect("lock").push(Agent {
        id: AgentId(1),
        name: "Engineer".into(),
        role: "engineer".into(),
        instructions: String::new(),
        backends: vec![BackendKind::Claude],
        models: Default::default(),
        tools: serde_json::json!([]),
        requirements: Default::default(),
        skills: Vec::new(),
    });
    *hub.tasks.lock().expect("lock") = vec![general_run, queued_api];
    let (_dir, core) = core(&[("work", &hub)]);
    let mut state = AppState::paired(["work"]);
    state.apply(refresh(&core).await);

    let topo = state.agent_topology("work", None, &when());
    // General task lights up a direct Space Lead -> Engineer edge in All projects.
    assert!(
        topo.edges
            .iter()
            .any(|e| e.id == "topo-edge-general-Engineer"
                && e.state == EdgeState::Active
                && e.label.as_deref() == Some("T-0030"))
    );
    // Queued work in api alone must not animate the Space Lead -> api edge.
    let lead_api = topo
        .edges
        .iter()
        .find(|e| e.id == "topo-edge-lead-api")
        .unwrap();
    assert_eq!(lead_api.state, EdgeState::Pending);
}

#[test]
fn quiet_update_detector_flags_internal_no_op_messages_only() {
    assert!(is_quiet_update(
        "I didn't send a message. T-0066 found no issues."
    ));
    assert!(is_quiet_update(
        "I didn’t message the user. Nothing new to tell them."
    ));
    assert!(is_quiet_update("No message sent. Waiting on CI."));
    assert!(!is_quiet_update(
        "I've queued T-0069 for the Backend Developer."
    ));
}
