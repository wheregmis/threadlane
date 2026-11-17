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
    PairingRequest, RecordedEvent, RunSummary, StandingRuleSummary, TaskReply, TaskRequest,
    TimerReply, TimerRequest, UserMessage,
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

use super::*;

/// A hub with the projects `api` and General, and what it was given.
/// While `down`, it answers nothing; while `old`, it answers like a hub
/// from before the Overview capability.
#[derive(Debug, Default)]
struct FakeHub {
    space: String,
    down: AtomicBool,
    old: AtomicBool,
    /// Answers like a hub from before it could list recorded events.
    no_recorded: AtomicBool,
    projects_fail: AtomicBool,
    created: Mutex<Vec<ProjectSummary>>,
    machines: Mutex<Vec<Machine>>,
    lead: Mutex<Option<Timestamp>>,
    stopped: Mutex<Vec<LaneTarget>>,
    pacing: Mutex<Pacing>,
    held: Mutex<Option<HeldResults>>,
    lead_runs: Mutex<Vec<LeadRun>>,
    task_runs: Mutex<Vec<RunSummary>>,
    now: AtomicI64,
    decisions: Mutex<Vec<Decision>>,
    tasks: Mutex<Vec<TaskSummary>>,
    history: Mutex<Vec<ConversationEntry>>,
    recent: Mutex<Vec<FeedItem>>,
    bindings: Mutex<Vec<BindingSummary>>,
    memory: Mutex<Vec<MemoryFile>>,
    timers: Mutex<Vec<Timer>>,
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
        self.up()?;
        Ok(HubInfo {
            space: self.space.clone(),
            mode_ceiling: self.ceiling.lock().expect("lock").unwrap_or(Mode::Assisted),
            schema_version: 1,
        })
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        self.up()?;
        if self.projects_fail.load(Ordering::SeqCst) {
            return Err(ApiError::Internal(
                "project list temporarily unavailable".to_owned(),
            ));
        }
        let created = self.created.lock().expect("lock").clone();
        Ok([
            ProjectSummary {
                id: ProjectId(1),
                name: "api".to_owned(),
                is_general: false,
                paused: false,
                mode_ceiling: None,
            },
            ProjectSummary {
                id: ProjectId(2),
                name: "General".to_owned(),
                is_general: true,
                paused: false,
                mode_ceiling: None,
            },
        ]
        .into_iter()
        .chain(created)
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
            event_id: EventId(99),
            project_id: project_id(message.project.as_deref()),
            duplicate: false,
        })
    }

    async fn feed(&self, _after: i64, _wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        Err(ApiError::Invalid("unsupported".to_owned()))
    }

    async fn org(&self, request: OrgRequest) -> Result<OrgReply, ApiError> {
        self.up()?;
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
            OrgRequest::Agents => Ok(OrgReply::Agents {
                agents: self.agents.lock().expect("lock").clone(),
            }),
            OrgRequest::CreateAgent {
                name,
                role,
                instructions,
                backends,
                ..
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
                    models: json!({}),
                    tools: json!({}),
                    requirements: json!({}),
                };
                agents.push(agent.clone());
                Ok(OrgReply::Agent { agent })
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
                };
                created.push(project.clone());
                Ok(OrgReply::Project { project })
            }
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
                decision.answer = Some(json!({ "option": option, "note": note }));
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
            OverviewRequest::Tasks { .. } => Ok(OverviewReply::Tasks { tasks }),
            OverviewRequest::Running => Ok(OverviewReply::Tasks {
                tasks: tasks
                    .into_iter()
                    .filter(|summary| summary.task.status == TaskStatus::Running)
                    .collect(),
            }),
            OverviewRequest::Conversation { .. } => Ok(OverviewReply::Conversation {
                entries: self.history.lock().expect("lock").clone(),
            }),
            OverviewRequest::Recent { .. } => Ok(OverviewReply::Recent {
                items: self.recent.lock().expect("lock").clone(),
            }),
            OverviewRequest::Machines => Ok(OverviewReply::Machines {
                machines: self.machines.lock().expect("lock").clone(),
                accounts: Vec::new(),
            }),
            OverviewRequest::Lead { project } => Ok(OverviewReply::Lead {
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
        },
        project: "api".to_owned(),
        agent: "Engineer".to_owned(),
        machine: (status == TaskStatus::Running).then(|| "macbook".to_owned()),
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
            Line::You { text, .. } => format!("you: {text}"),
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
    state.receive(first);
    // The same seq from another space is another item.
    state.receive(item(
        "home",
        1,
        OutboxKind::Message,
        json!({ "text": "hi" }),
    ));

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
            ("Running", vec![card(2, "Engineer · macbook · 12 min")]),
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
        "2 spaces reachable · 1 running · 1 need you · 0 of 0 machines online"
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
        run: Run {
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
            "00:05 You wrote: Use the ledger.",
            "00:06 Engineer on linux-box with claude: worked for 1 min, reported back",
            "00:31 Done: Refunds are idempotent.",
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
        "1 space reachable · 1 running · 1 need you · 1 of 1 machines online"
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
fn a_long_result_shows_its_first_lines_until_you_ask_for_all_of_it() {
    let mut state = AppState::default();
    let long = "Line one\nLine two\nLine three\nLine four";
    state.receive(item(
        "work",
        1,
        OutboxKind::Message,
        json!({ "text": long }),
    ));
    state.receive(item(
        "work",
        2,
        OutboxKind::Message,
        json!({ "text": "Short." }),
    ));
    let wide = "word ".repeat(100);
    state.receive(item(
        "home",
        1,
        OutboxKind::Message,
        json!({ "text": wide }),
    ));
    let excerpt = |state: &AppState, space: &str, seq| {
        let item = state
            .delivered
            .get(&(space.to_owned(), seq))
            .expect("delivered");
        state.excerpt(item)
    };

    assert_eq!(
        excerpt(&state, "work", 1),
        Excerpt {
            text: "Line one\nLine two\nLine three…".to_owned(),
            toggle: Some("Show"),
        }
    );
    assert_eq!(excerpt(&state, "work", 2).toggle, None, "fits");
    let shown = excerpt(&state, "home", 1);
    assert_eq!(shown.text.chars().count(), 240, "one long line is cut too");
    assert_eq!(shown.toggle, Some("Show"));

    state.toggle_result("work", 1);
    assert_eq!(
        excerpt(&state, "work", 1),
        Excerpt {
            text: long.to_owned(),
            toggle: Some("Hide"),
        }
    );
    assert_eq!(
        excerpt(&state, "home", 1).toggle,
        Some("Show"),
        "per result"
    );
    state.toggle_result("work", 1);
    assert_eq!(excerpt(&state, "work", 1).toggle, Some("Show"));
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
    state.project_created("work", created);
    state.project_created(
        "home",
        ProjectSummary {
            id: ProjectId(9),
            name: "Elsewhere".to_owned(),
            is_general: false,
            paused: false,
            mode_ceiling: None,
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
        moments.contains(&"You wrote: Add tests first.".to_owned()),
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
    }
    assert_eq!(
        labels,
        ["Tasks", "Conversation", "Agents", "Routines", "Memory"],
        "the wireframe's order"
    );
}

#[test]
fn the_task_screen_shows_usage_and_whether_its_branch_is_published() {
    let run = |id, started: i64, ended: Option<i64>| RunSummary {
        run: Run {
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
        decisions: Vec::new(),
        related: Vec::new(),
        messages: Vec::new(),
        proposed_by: None,
        stopped: false,
    };
    let [runs, tokens] = usage(&detail, &when());
    assert_eq!(runs, "2 runs · 7 min", "the running one counts until now");
    assert!(tokens.contains("not reported"), "{tokens}");

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
async fn copying_an_agent_offers_what_the_other_space_lacks_and_copies_it_whole() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    for (name, hub) in [("Reviewer", &work), ("Writer", &work), ("Writer", &home)] {
        hub.org(OrgRequest::CreateAgent {
            name: name.to_owned(),
            role: "role".to_owned(),
            instructions: format!("Be {name}."),
            backends: vec![BackendKind::Codex],
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
    let created = core.create_task(Some("work"), spec).await.expect("created");
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
    state.project_paused("work", paused);
    assert!(state.is_paused("work", "api"));
    let resumed = core
        .set_paused(Some("work"), Some("api"), false)
        .await
        .expect("resumed");
    state.project_paused("work", resumed);
    assert!(!state.is_paused("work", "api"));
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
        },
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
    assert_eq!(engineer.at_once, at_once(3));
    assert_eq!(at_once(3).fewer, Some(2));
    assert_eq!(at_once(1).fewer, None);
    assert_eq!(at_once(0).label, "No limit");
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
    assert_eq!(may.rules_title, "Standing rules (1)");
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
        "Standing rules (1)"
    );

    // "At once" goes through the hub and shows at once.
    let set = core
        .set_max_parallel(Some("work"), Some("api"), "Engineer", 2)
        .await
        .expect("set");
    state.mode_set("work", "api", set);
    assert_eq!(state.agent_rows("work", "api")[1].at_once.label, "2");
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
            (
                TaskId(3),
                "Engineer · Engineer runs at most 1 at once here: T-0001 holds it".to_owned()
            ),
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
    state.project_paused(
        "work",
        ProjectSummary {
            id: ProjectId(1),
            name: "api".to_owned(),
            is_general: false,
            paused: true,
            mode_ceiling: None,
        },
    );
    assert_eq!(
        state.queued_reason("work", "api", &summary).as_deref(),
        Some("the project is paused")
    );
}

#[tokio::test]
async fn home_lists_tasks_that_failed_or_are_stuck_and_the_sidebar_counts_them() {
    let work = FakeHub::new("Work", Vec::new());
    let mut cancelled = task(2, TaskStatus::Cancelled, None);
    cancelled.task.depends_on = vec![TaskId(1)];
    let mut chained = task(3, TaskStatus::Cancelled, None);
    chained.task.depends_on = vec![TaskId(2)];
    let mut by_you = task(5, TaskStatus::Cancelled, None);
    by_you.task.depends_on = vec![TaskId(6)];
    *work.tasks.lock().expect("lock") = vec![
        task(1, TaskStatus::Failed, Some("the tests do not build")),
        cancelled,
        chained,
        task(
            4,
            TaskStatus::Sleeping,
            Some("ended a run without finishing; nothing is set to wake it"),
        ),
        by_you,
        task(6, TaskStatus::Cancelled, None),
        task(
            7,
            TaskStatus::Sleeping,
            Some("waiting for a follow-up at 09:00"),
        ),
        task(8, TaskStatus::Queued, None),
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    let rows: Vec<(TaskId, String)> = state
        .needs_attention()
        .into_iter()
        .map(|row| (row.task, row.why))
        .collect();
    assert_eq!(
        rows,
        [
            (
                TaskId(1),
                "api · Engineer · failed: the tests do not build".to_owned()
            ),
            (
                TaskId(2),
                "api · Engineer · cancelled because T-0001, which it depended on, failed"
                    .to_owned()
            ),
            (
                TaskId(3),
                "api · Engineer · cancelled because T-0001, which it depended on, failed"
                    .to_owned()
            ),
            (
                TaskId(4),
                "api · Engineer · ended a run without finishing; nothing is set to wake it"
                    .to_owned()
            ),
        ]
    );
    assert_eq!(state.attention_in("work", "api"), 4);
    assert_eq!(state.attention_in("work", "General"), 0);
    assert!(needs_you("Stuck: no run can continue it"));
    assert!(!needs_you("waiting for T-0002 to be done"));
    state.set_filter(Some("home"));
    assert!(state.needs_attention().is_empty());
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
        sent_message: outcome == Some(RunOutcome::Sent),
        run: Run {
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
        machine: "hub".to_owned(),
        run: Run {
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
        run: stopped_run.clone(),
        sent_message: true,
    });
    work.task_runs.lock().unwrap().push(RunSummary {
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
