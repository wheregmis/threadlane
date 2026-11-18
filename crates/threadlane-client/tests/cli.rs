//! The `threadlane` binary against `serve` on a temporary socket, with an
//! in-memory hub behind it.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;
use threadlane_core::client::{
    Accepted, ApiError, BindingSummary, DecisionReply, DecisionRequest, FeedItem, HubApi, HubInfo,
    MemoryReply, MemoryRequest, OrgReply, OrgRequest, OverviewReply, OverviewRequest, PairingReply,
    PairingRequest, ProjectSummary, UserMessage,
};
use threadlane_core::client::{LaneTarget, OperationsReply, OperationsRequest, TraceInspection};
use threadlane_core::client::{
    StandingRuleSummary, TaskReply, TaskRequest, TimerReply, TimerRequest,
};
use threadlane_core::ids::StandingRuleId;
use threadlane_core::ids::TimerId;
use threadlane_core::ids::{
    AgentId, DecisionId, EventId, LaneId, OutboxId, ProjectId, RepositoryId,
};
use threadlane_core::model::StandingRule;
use threadlane_core::model::{
    Agent, BackendKind, Decision, DecisionKind, DecisionOption, DecisionStatus, Repository, Task,
    TaskOrigin, TaskStatus,
};
use threadlane_core::model::{Mode, OutboxKind, OutboxMessage, OutboxStatus, Urgency};
use threadlane_core::model::{
    Recurrence, Rule, Timer, TimerCategory, TimerCreator, TimerStatus, Weekday,
};
use threadlane_core::time::Timestamp;
use threadlane_core::tools::Tier;
use threadlane_transport_local::{SOCKET_FILE, serve};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

#[derive(Debug)]
struct Shared {
    items: Mutex<Vec<FeedItem>>,
    changed: watch::Sender<u64>,
    sent: Mutex<Vec<UserMessage>>,
    org: Mutex<Vec<OrgRequest>>,
    pacing: Mutex<threadlane_core::model::Pacing>,
    /// D-0007, pending until it is answered.
    decision: Mutex<Decision>,
    timers: Mutex<Vec<TimerRequest>>,
    operations: Mutex<Vec<OperationsRequest>>,
}

impl Shared {
    /// Appends an item with the next `seq`.
    fn push(&self, kind: OutboxKind, project: Option<&str>, text: &str) {
        let mut items = self.items.lock().expect("lock");
        let seq = i64::try_from(items.len()).expect("small") + 1;
        items.push(FeedItem {
            digest: Vec::new(),
            seq,
            message: OutboxMessage {
                trace_id: None,
                id: OutboxId(seq),
                project_id: project.map(|_| ProjectId(1)),
                lane_id: None,
                session_id: None,
                run_id: None,
                tool_call_id: None,
                kind,
                decision_id: None,
                channel: "client".to_owned(),
                body: json!({ "text": text }),
                urgency: Urgency::Normal,
                status: OutboxStatus::Sent,
                not_before: None,
                attempts: 1,
                channel_message_id: None,
                created_at: Timestamp::from_millis(1_000),
                sent_at: Some(Timestamp::from_millis(2_000)),
                delivery_seq: Some(seq),
            },
            project_name: project.map(str::to_owned),
            delivered_at: Timestamp::from_millis(2_000),
        });
        drop(items);
        self.changed.send_modify(|version| *version += 1);
    }
}

/// Answers every message, except to the project `silent` or to a task, with a notice and
/// then a reply, a little later, through its feed.
#[derive(Debug, Clone)]
struct FakeHub(Arc<Shared>);

impl FakeHub {
    fn new() -> Self {
        Self(Arc::new(Shared {
            items: Mutex::new(Vec::new()),
            changed: watch::channel(0).0,
            sent: Mutex::new(Vec::new()),
            org: Mutex::new(Vec::new()),
            pacing: Mutex::default(),
            decision: Mutex::new(pending_decision()),
            timers: Mutex::new(Vec::new()),
            operations: Mutex::new(Vec::new()),
        }))
    }
}

#[async_trait]
impl HubApi for FakeHub {
    async fn info(&self) -> Result<HubInfo, ApiError> {
        Ok(HubInfo {
            space: "Work".to_owned(),
            mode_ceiling: Mode::Assisted,
            schema_version: 1,
        })
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        Ok(vec![
            ProjectSummary {
                id: ProjectId(1),
                name: "General".to_owned(),
                is_general: true,
                paused: false,
                mode_ceiling: None,
            },
            ProjectSummary {
                id: ProjectId(2),
                name: "api".to_owned(),
                is_general: false,
                paused: true,
                mode_ceiling: None,
            },
        ])
    }

    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError> {
        let count = {
            let mut sent = self.0.sent.lock().expect("lock");
            sent.push(message.clone());
            sent.len()
        };
        if message.project.as_deref() != Some("silent") && message.task.is_none() {
            let shared = Arc::clone(&self.0);
            let project = message
                .project
                .clone()
                .unwrap_or_else(|| "General".to_owned());
            let reply = format!("reply to: {}", message.text);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                shared.push(OutboxKind::SystemNotice, None, "thinking");
                tokio::time::sleep(Duration::from_millis(100)).await;
                shared.push(OutboxKind::Message, Some(&project), &reply);
            });
        }
        Ok(Accepted {
            event_id: EventId(i64::try_from(count).expect("small")),
            project_id: ProjectId(1),
            duplicate: false,
        })
    }

    async fn feed(&self, after: i64, wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        let deadline = Instant::now() + wait;
        let mut changed = self.0.changed.subscribe();
        loop {
            let items: Vec<_> = self
                .0
                .items
                .lock()
                .expect("lock")
                .iter()
                .filter(|item| item.seq > after)
                .cloned()
                .collect();
            if !items.is_empty() {
                return Ok(items);
            }
            if tokio::time::timeout_at(deadline, changed.changed())
                .await
                .is_err()
            {
                return Ok(Vec::new());
            }
        }
    }

    async fn org(&self, request: OrgRequest) -> Result<OrgReply, ApiError> {
        self.0.org.lock().expect("lock").push(request.clone());
        Ok(match request {
            OrgRequest::CreateProject { name, .. } => OrgReply::Project {
                project: ProjectSummary {
                    id: ProjectId(3),
                    name,
                    is_general: false,
                    paused: false,
                    mode_ceiling: None,
                },
            },
            OrgRequest::Agents => OrgReply::Agents {
                agents: vec![agent("Lead", "lead", ""), agent("Reviewer", "reviewer", "")],
            },
            OrgRequest::CreateAgent {
                name,
                role,
                instructions,
                ..
            } => OrgReply::Agent {
                agent: agent(&name, &role, &instructions),
            },
            OrgRequest::Bindings { .. } => OrgReply::Bindings {
                bindings: vec![BindingSummary {
                    agent: "Lead".to_owned(),
                    role: "lead".to_owned(),
                    mode: Mode::OnRequest,
                    is_lead: true,
                    max_parallel: 1,
                    overrides: Default::default(),
                }],
            },
            OrgRequest::AddRepository {
                name,
                remote_url,
                default_branch,
                ..
            } => OrgReply::Repository {
                repository: repository(&name, &remote_url, default_branch.as_deref()),
            },
            OrgRequest::Repositories { .. } => OrgReply::Repositories {
                repositories: vec![repository("app", "git@example.com:me/app.git", None)],
            },
            OrgRequest::SetMode { agent, mode, .. } => OrgReply::Binding {
                binding: BindingSummary {
                    agent,
                    role: "reviewer".to_owned(),
                    mode,
                    is_lead: false,
                    max_parallel: 1,
                    overrides: Default::default(),
                },
            },
            OrgRequest::SetPaused { project, paused } => OrgReply::Project {
                project: ProjectSummary {
                    id: ProjectId(2),
                    name: project.unwrap_or_else(|| "General".to_owned()),
                    is_general: false,
                    paused,
                    mode_ceiling: None,
                },
            },
            OrgRequest::SetCeiling { mode, .. } if mode > Mode::Assisted => {
                return Err(ApiError::Invalid(format!(
                    "{mode} is above the space's ceiling, assisted"
                )));
            }
            OrgRequest::SetCeiling { project, mode } => OrgReply::Ceiling {
                project: project.unwrap_or_else(|| "General".to_owned()),
                ceiling: mode,
            },
            OrgRequest::SetSpaceCeiling { mode } => OrgReply::SpaceCeiling { ceiling: mode },
            OrgRequest::SetMaxParallel {
                agent,
                max_parallel,
                ..
            } => OrgReply::Binding {
                binding: BindingSummary {
                    agent,
                    role: "reviewer".to_owned(),
                    mode: Mode::OnRequest,
                    is_lead: false,
                    max_parallel,
                    overrides: Default::default(),
                },
            },
            OrgRequest::Pacing => OrgReply::Pacing {
                pacing: self.0.pacing.lock().unwrap().clone(),
            },
            OrgRequest::SetPacing { pacing } => {
                *self.0.pacing.lock().unwrap() = pacing.clone();
                OrgReply::Pacing { pacing }
            }
            OrgRequest::Stop { .. } | OrgRequest::ResumeLane { .. } => OrgReply::LaneUpdated,
            other => return Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        })
    }

    async fn decisions(&self, request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        let mut decision = self.0.decision.lock().expect("lock");
        match request {
            DecisionRequest::Pending => Ok(DecisionReply::Pending {
                decisions: if decision.status == DecisionStatus::Pending {
                    vec![decision.clone()]
                } else {
                    Vec::new()
                },
            }),
            DecisionRequest::Answer {
                decision: id,
                option,
                note,
            } if id == decision.id => {
                let recorded = decision.status == DecisionStatus::Pending;
                if recorded {
                    let index = decision
                        .options
                        .iter()
                        .position(|known| known.label == option)
                        .ok_or_else(|| ApiError::Invalid(format!("no option {option}")))?;
                    decision.status = DecisionStatus::Answered;
                    decision.answer = Some(json!({ "option": index, "note": note }));
                }
                Ok(DecisionReply::Answered {
                    decision: Box::new(decision.clone()),
                    recorded,
                })
            }
            DecisionRequest::AnswerAlways {
                decision: id,
                option,
                note,
            } if id == decision.id && decision.status == DecisionStatus::Pending => {
                let index = decision
                    .options
                    .iter()
                    .position(|known| known.label == option)
                    .ok_or_else(|| ApiError::Invalid(format!("no option {option}")))?;
                decision.status = DecisionStatus::Answered;
                decision.answer = Some(json!({ "option": index, "note": note, "always": true }));
                Ok(DecisionReply::Answered {
                    decision: Box::new(decision.clone()),
                    recorded: true,
                })
            }
            DecisionRequest::StandingRules { project } => Ok(DecisionReply::StandingRules {
                rules: if project.as_deref() == Some("api") {
                    Vec::new()
                } else {
                    vec![standing_rule(false)]
                },
            }),
            DecisionRequest::RevokeRule {
                rule: StandingRuleId(1),
            } => Ok(DecisionReply::StandingRule {
                rule: standing_rule(true),
            }),
            other => Err(ApiError::NotFound(format!("{other:?}"))),
        }
    }

    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError> {
        Err(ApiError::Invalid(format!("unsupported {request:?}")))
    }

    async fn overview(&self, request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        match request {
            OverviewRequest::HeldResults => Ok(OverviewReply::HeldResults { held: None }),
            _ => Err(ApiError::Invalid(format!("unsupported {request:?}"))),
        }
    }

    async fn memory(&self, request: MemoryRequest) -> Result<MemoryReply, ApiError> {
        Err(ApiError::Invalid(format!("unsupported {request:?}")))
    }

    async fn timers(&self, request: TimerRequest) -> Result<TimerReply, ApiError> {
        self.0.timers.lock().expect("lock").push(request.clone());
        Ok(match request {
            TimerRequest::List { .. } => TimerReply::Timers {
                timers: vec![
                    timer(
                        3,
                        Some(daily_berlin()),
                        TimerStatus::Active,
                        TimerCreator::User,
                    ),
                    timer(4, None, TimerStatus::Paused, TimerCreator::Agent),
                ],
            },
            TimerRequest::CreateRoutine { recurrence, .. } => TimerReply::Timer {
                timer: timer(5, Some(recurrence), TimerStatus::Active, TimerCreator::User),
            },
            TimerRequest::Pause { timer: id } => TimerReply::Timer {
                timer: timer(
                    id.0,
                    Some(daily_berlin()),
                    TimerStatus::Paused,
                    TimerCreator::User,
                ),
            },
            TimerRequest::Resume { timer: id } | TimerRequest::Cancel { timer: id } => {
                TimerReply::Timer {
                    timer: timer(
                        id.0,
                        Some(daily_berlin()),
                        TimerStatus::Active,
                        TimerCreator::User,
                    ),
                }
            }
            other => return Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        })
    }

    async fn tasks(&self, request: TaskRequest) -> Result<TaskReply, ApiError> {
        match request {
            TaskRequest::Cancel { task } => Ok(TaskReply::Task {
                task: Task {
                    id: task,
                    project_id: ProjectId(1),
                    lane_id: LaneId(5),
                    agent_id: AgentId(1),
                    title: "Review".to_owned(),
                    goal: String::new(),
                    repositories: Vec::new(),
                    parent_task_id: None,
                    depends_on: Vec::new(),
                    origin: TaskOrigin::User,
                    status: TaskStatus::Cancelled,
                    priority: 0,
                    created_by_call: None,
                    last_machine_id: None,
                    waiting_reason: None,
                    result: None,
                    created_at: Timestamp::from_millis(1_000),
                },
            }),
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    async fn operations(&self, request: OperationsRequest) -> Result<OperationsReply, ApiError> {
        let id = format!("{request:?}");
        self.0.operations.lock().expect("operations").push(request);
        Ok(OperationsReply::Trace {
            trace: TraceInspection {
                id,
                events: Vec::new(),
                runs: Vec::new(),
                tool_calls: Vec::new(),
                decisions: Vec::new(),
                outbox: Vec::new(),
            },
        })
    }
}

/// Rule 1: the reviewer may open draft pull requests in `app`.
fn standing_rule(revoked: bool) -> StandingRuleSummary {
    StandingRuleSummary {
        rule: StandingRule {
            id: StandingRuleId(1),
            project_id: ProjectId(1),
            agent_id: AgentId(2),
            tool: "open_pull_request".to_owned(),
            pattern: json!({ "repository": "app" }),
            tier: Tier::SharedReversible,
            created_from_decision: Some(DecisionId(7)),
            created_at: Timestamp::from_millis(1_000),
            revoked_at: revoked.then(|| Timestamp::from_millis(2_000)),
            uses: 3,
        },
        project: "General".to_owned(),
        agent: "Reviewer".to_owned(),
    }
}

fn daily_berlin() -> Recurrence {
    Recurrence {
        rule: Rule::Daily { hour: 9, minute: 0 },
        zone: "Europe/Berlin".to_owned(),
    }
}

/// A timer due long ago, so the CLI shows it as due now.
fn timer(
    id: i64,
    recurrence: Option<Recurrence>,
    status: TimerStatus,
    created_by: TimerCreator,
) -> Timer {
    Timer {
        id: TimerId(id),
        project_id: ProjectId(1),
        lane_id: LaneId(1),
        agent_id: None,
        task_ref: None,
        category: if recurrence.is_some() {
            TimerCategory::Schedule
        } else {
            TimerCategory::FollowUp
        },
        due_at: Timestamp::from_millis(1_000),
        recurrence,
        instruction: "Summarize the night.".to_owned(),
        status,
        created_by,
        created_by_call: None,
        last_fired_at: None,
        fire_count: 0,
        requested: true,
    }
}

fn pending_decision() -> Decision {
    let option = |label: &str, approve: bool| DecisionOption {
        label: label.to_owned(),
        effect: json!({ "approve": approve }),
    };
    Decision {
        trace_id: None,
        id: DecisionId(7),
        project_id: ProjectId(1),
        lane_id: LaneId(4),
        task_id: None,
        session_id: None,
        run_id: None,
        tool_call_id: None,
        tool: "open_pull_request".to_owned(),
        arguments: json!({ "repository": "app", "title": "Add the page" }),
        arguments_hash: String::new(),
        kind: DecisionKind::Approval,
        question: "Open a draft pull request?".to_owned(),
        context: String::new(),
        options: vec![option("approve", true), option("decline", false)],
        recommended: Some(0),
        offers_standing_rule: false,
        status: DecisionStatus::Pending,
        answer: None,
        expires_at: None,
        created_at: Timestamp::from_millis(1_000),
        outcome: None,
        proposed_by: None,
    }
}

fn repository(name: &str, remote_url: &str, default_branch: Option<&str>) -> Repository {
    Repository {
        id: RepositoryId(1),
        project_id: ProjectId(3),
        name: name.to_owned(),
        remote_url: remote_url.to_owned(),
        default_branch: default_branch.unwrap_or("main").to_owned(),
        protected_branches: Vec::new(),
    }
}

fn agent(name: &str, role: &str, instructions: &str) -> Agent {
    Agent {
        id: AgentId(1),
        name: name.to_owned(),
        role: role.to_owned(),
        instructions: instructions.to_owned(),
        backends: Vec::new(),
        models: json!({}),
        tools: json!({}),
        requirements: json!({}),
    }
}

/// A temporary directory whose `hub.sock` fits the platform's socket path limit.
fn short_tempdir() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    if dir.path().join(SOCKET_FILE).as_os_str().len() < 100 {
        dir
    } else {
        tempfile::Builder::new()
            .prefix("tl")
            .tempdir_in("/tmp")
            .expect("tempdir in /tmp")
    }
}

#[derive(Debug)]
struct Hub {
    dir: TempDir,
    fake: FakeHub,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<io::Result<()>>,
}

impl Hub {
    async fn start() -> Self {
        let dir = short_tempdir();
        let socket = dir.path().join(SOCKET_FILE);
        let fake = FakeHub::new();
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn({
            let api = Arc::new(fake.clone());
            let socket = socket.clone();
            async move {
                serve(&socket, api, async {
                    let _ = stopped.await;
                })
                .await
            }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while tokio::net::UnixStream::connect(&socket).await.is_err() {
            assert!(Instant::now() < deadline, "the hub did not come up");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            dir,
            fake,
            stop: Some(stop),
            task,
        }
    }

    fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join(SOCKET_FILE)
    }

    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.expect("serve task").expect("serve");
    }
}

fn command(client_dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_threadlane"));
    command
        .args(args)
        .env("THREADLANE_CLIENT_DIR", client_dir)
        .kill_on_drop(true);
    command
}

async fn threadlane(client_dir: &Path, args: &[&str]) -> Output {
    tokio::time::timeout(Duration::from_secs(30), command(client_dir, args).output())
        .await
        .expect("threadlane finishes")
        .expect("threadlane runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Runs `threadlane` and expects success with exactly `expected` on stdout.
async fn expect_success(client_dir: &Path, args: &[&str], expected: &str) {
    let output = threadlane(client_dir, args).await;
    assert!(
        output.status.success(),
        "threadlane {args:?} failed: {}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), expected, "threadlane {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_against_a_running_hub() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let client_dir = client.path();
    let data_dir = hub.data_dir().to_str().expect("utf-8 path").to_owned();
    let socket = hub.socket().display().to_string();

    expect_success(
        client_dir,
        &["hub", "add", "work", &data_dir],
        &format!("paired work: {socket}\n"),
    )
    .await;
    expect_success(client_dir, &["hub", "list"], &format!("work  {socket}\n")).await;
    expect_success(
        client_dir,
        &["status"],
        "work: reachable, space Work, ceiling assisted\n",
    )
    .await;
    expect_success(
        client_dir,
        &["projects"],
        "[work] General (general)\n[work] api (paused)\n",
    )
    .await;

    expect_success(
        client_dir,
        &["send", "--no-wait", "-p", "silent", "hello", "there"],
        "[work/silent] accepted as event 1\n",
    )
    .await;
    {
        let sent = hub.fake.0.sent.lock().expect("lock");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].text, "hello there");
        assert_eq!(sent[0].project.as_deref(), Some("silent"));
        assert_eq!(sent[0].task, None);
    }
    expect_success(
        client_dir,
        &["send", "--no-wait", "--task", "T-0003", "add", "tests"],
        "[work/T-0003] accepted as event 2\n",
    )
    .await;
    {
        let mut sent = hub.fake.0.sent.lock().expect("lock");
        assert_eq!(sent[1].task, Some(threadlane_core::ids::TaskId(3)));
        assert_eq!(sent[1].text, "add tests");
        sent.pop();
    }

    hub.fake
        .0
        .push(OutboxKind::SystemNotice, None, "backup finished");
    hub.fake
        .0
        .push(OutboxKind::Message, Some("api"), "status update");
    expect_success(
        client_dir,
        &["feed"],
        "[work] notice: backup finished\n[work/api] status update\n",
    )
    .await;
    expect_success(client_dir, &["feed", "-s", "work"], "").await;

    // An unread older message is shown first and not taken for the reply.
    hub.fake
        .0
        .push(OutboxKind::Message, Some("General"), "old news");
    expect_success(
        client_dir,
        &["send", "-p", "api", "how", "are", "you"],
        "[work/General] old news\n\
         [work/api] accepted as event 2\n\
         [work] notice: thinking\n\
         [work/api] reply to: how are you\n",
    )
    .await;
    expect_success(client_dir, &["feed"], "").await;

    expect_success(
        client_dir,
        &["feed", "--from-start"],
        "[work] notice: backup finished\n\
         [work/api] status update\n\
         [work/General] old news\n\
         [work] notice: thinking\n\
         [work/api] reply to: how are you\n",
    )
    .await;

    expect_success(client_dir, &["hub", "remove", "work"], "removed work\n").await;
    expect_success(client_dir, &["hub", "list"], "no hubs paired\n").await;
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_manages_projects_agents_and_modes() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let client_dir = client.path();
    let socket = hub.socket().display().to_string();
    expect_success(
        client_dir,
        &["hub", "add", "work", &socket],
        &format!("paired work: {socket}\n"),
    )
    .await;

    expect_success(
        client_dir,
        &["project", "add", "web", "--description", "the site"],
        "created project web\n",
    )
    .await;
    expect_success(
        client_dir,
        &[
            "agent",
            "add",
            "Reviewer",
            "--role",
            "reviewer",
            "--backend",
            "codex,claude",
        ],
        "created agent Reviewer (reviewer)\n",
    )
    .await;
    expect_success(
        client_dir,
        &["agents", "-s", "work"],
        "Lead (lead)\nReviewer (reviewer)\n",
    )
    .await;
    expect_success(
        client_dir,
        &["bindings", "-p", "web"],
        "Lead (lead, lead): on_request\n",
    )
    .await;
    expect_success(
        client_dir,
        &["mode", "Reviewer", "off", "-p", "web"],
        "Reviewer is now off\n",
    )
    .await;
    let output = threadlane(client_dir, &["mode", "Reviewer", "sleepy"]).await;
    assert!(!output.status.success(), "an unknown mode is refused");

    assert_eq!(
        *hub.fake.0.org.lock().expect("lock"),
        vec![
            OrgRequest::CreateProject {
                name: "web".to_owned(),
                description: "the site".to_owned(),
            },
            OrgRequest::CreateAgent {
                name: "Reviewer".to_owned(),
                role: "reviewer".to_owned(),
                instructions: String::new(),
                backends: vec![BackendKind::Codex, BackendKind::Claude],
            },
            OrgRequest::Agents,
            OrgRequest::Bindings {
                project: Some("web".to_owned()),
            },
            OrgRequest::SetMode {
                project: Some("web".to_owned()),
                agent: "Reviewer".to_owned(),
                mode: Mode::Off,
            },
        ]
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_registers_and_lists_repositories() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let client_dir = client.path();
    let socket = hub.socket().display().to_string();
    expect_success(
        client_dir,
        &["hub", "add", "work", &socket],
        &format!("paired work: {socket}\n"),
    )
    .await;
    expect_success(
        client_dir,
        &[
            "repo",
            "add",
            "app",
            "git@example.com:me/app.git",
            "-p",
            "web",
            "--branch",
            "trunk",
        ],
        "registered repository app (git@example.com:me/app.git, branch trunk)\n",
    )
    .await;
    expect_success(
        client_dir,
        &["repos", "-p", "web"],
        "app git@example.com:me/app.git (main)\n",
    )
    .await;
    assert_eq!(
        *hub.fake.0.org.lock().expect("lock"),
        vec![
            OrgRequest::AddRepository {
                project: Some("web".to_owned()),
                name: "app".to_owned(),
                remote_url: "git@example.com:me/app.git".to_owned(),
                default_branch: Some("trunk".to_owned()),
            },
            OrgRequest::Repositories {
                project: Some("web".to_owned()),
            },
        ]
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_lists_and_answers_decisions() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let client_dir = client.path();
    let socket = hub.socket().display().to_string();
    expect_success(
        client_dir,
        &["hub", "add", "work", &socket],
        &format!("paired work: {socket}\n"),
    )
    .await;
    expect_success(
        client_dir,
        &["decisions"],
        "[work] D-0007 Open a draft pull request?\n    options: approve (recommended), decline\n",
    )
    .await;
    expect_success(
        client_dir,
        &["answer", "D-0007", "approve", "--note", "go ahead"],
        "[work] D-0007 answered: approve\n",
    )
    .await;
    assert_eq!(
        hub.fake.0.decision.lock().expect("lock").answer,
        Some(json!({ "option": 0, "note": "go ahead" }))
    );
    expect_success(
        client_dir,
        &["answer", "d-7", "decline", "-s", "work"],
        "[work] D-0007 was already answered (approve); this answer was ignored\n",
    )
    .await;
    expect_success(
        client_dir,
        &["decisions", "-s", "work"],
        "nothing waits on you\n",
    )
    .await;
    let output = threadlane(client_dir, &["answer", "seven", "approve"]).await;
    assert!(
        !output.status.success(),
        "an id that is not a decision's is refused"
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_sets_a_ceiling_answers_always_and_lists_and_revokes_rules() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let client_dir = client.path();
    let socket = hub.socket().display().to_string();
    expect_success(
        client_dir,
        &["hub", "add", "work", &socket],
        &format!("paired work: {socket}\n"),
    )
    .await;
    expect_success(
        client_dir,
        &["ceiling", "on_request", "-p", "api"],
        "the ceiling is now on_request\n",
    )
    .await;
    assert_eq!(
        hub.fake.0.org.lock().expect("lock").last(),
        Some(&OrgRequest::SetCeiling {
            project: Some("api".to_owned()),
            mode: Mode::OnRequest,
        })
    );
    let output = threadlane(client_dir, &["ceiling", "autonomous"]).await;
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("above the space's ceiling"),
        "{}",
        stderr(&output)
    );
    expect_success(
        client_dir,
        &["answer", "D-0007", "approve", "--always"],
        "[work] D-0007 answered: approve\n",
    )
    .await;
    assert_eq!(
        hub.fake.0.decision.lock().expect("lock").answer,
        Some(json!({ "option": 0, "note": null, "always": true }))
    );
    expect_success(
        client_dir,
        &["rules"],
        "1 General/Reviewer: open_pull_request {\"repository\":\"app\"}, used 3 times\n",
    )
    .await;
    expect_success(client_dir, &["rules", "-p", "api"], "no standing rules\n").await;
    expect_success(client_dir, &["rule", "revoke", "1"], "revoked rule 1\n").await;
    let output = threadlane(client_dir, &["rule", "revoke", "one"]).await;
    assert!(!output.status.success(), "not a rule number");
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_sets_the_space_ceiling_and_how_many_tasks_run_at_once() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let client_dir = client.path();
    let socket = hub.socket().display().to_string();
    expect_success(
        client_dir,
        &["hub", "add", "work", &socket],
        &format!("paired work: {socket}\n"),
    )
    .await;
    expect_success(
        client_dir,
        &["ceiling", "autonomous", "--space-wide"],
        "the ceiling is now autonomous\n",
    )
    .await;
    assert_eq!(
        hub.fake.0.org.lock().expect("lock").last(),
        Some(&OrgRequest::SetSpaceCeiling {
            mode: Mode::Autonomous
        })
    );
    let output = threadlane(
        client_dir,
        &["ceiling", "assisted", "--space-wide", "-p", "api"],
    )
    .await;
    assert!(!output.status.success(), "a project is not space-wide");
    expect_success(
        client_dir,
        &["at-once", "Reviewer", "2", "-p", "api"],
        "Reviewer may run 2 at once\n",
    )
    .await;
    assert_eq!(
        hub.fake.0.org.lock().expect("lock").last(),
        Some(&OrgRequest::SetMaxParallel {
            project: Some("api".to_owned()),
            agent: "Reviewer".to_owned(),
            max_parallel: 2,
        })
    );
    expect_success(
        client_dir,
        &["at-once", "Reviewer", "0"],
        "Reviewer may run any number of tasks at once\n",
    )
    .await;
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn feed_follow_keeps_waiting_for_new_items() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let socket = hub.socket().display().to_string();
    expect_success(
        client.path(),
        &["hub", "add", "work", &socket],
        &format!("paired work: {socket}\n"),
    )
    .await;

    let mut child = command(client.path(), &["feed", "--follow"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    let mut next_line = async || {
        tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("a line in time")
            .expect("read")
            .expect("not closed")
    };

    tokio::time::sleep(Duration::from_millis(200)).await;
    hub.fake.0.push(OutboxKind::Message, Some("api"), "first");
    assert_eq!(next_line().await, "[work/api] first");
    hub.fake.0.push(OutboxKind::SystemNotice, None, "second");
    assert_eq!(next_line().await, "[work] notice: second");
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "still following"
    );

    child.kill().await.expect("kill");
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn send_fails_clearly_when_no_reply_comes_in_time() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let socket = hub.socket().display().to_string();
    expect_success(
        client.path(),
        &["hub", "add", "work", &socket],
        &format!("paired work: {socket}\n"),
    )
    .await;

    let output = threadlane(
        client.path(),
        &["send", "-p", "silent", "--timeout", "1", "anyone?"],
    )
    .await;
    assert!(!output.status.success());
    assert_eq!(stdout(&output), "[work/silent] accepted as event 1\n");
    assert_eq!(
        stderr(&output),
        "threadlane: no message from work within 1 s\n"
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_hub_gives_one_line_and_a_failure_exit_code() {
    let client = tempfile::tempdir().expect("tempdir");
    let nothing = short_tempdir();
    let data_dir = nothing.path().to_str().expect("utf-8 path").to_owned();
    let socket = nothing.path().join(SOCKET_FILE).display().to_string();
    expect_success(
        client.path(),
        &["hub", "add", "down", &data_dir],
        &format!("paired down: {socket}\n"),
    )
    .await;

    for args in [
        &["projects"][..],
        &["send", "--no-wait", "hello"],
        &["send", "hello"],
        &["feed"],
    ] {
        let output = threadlane(client.path(), args).await;
        assert!(!output.status.success(), "threadlane {args:?} succeeded");
        assert_eq!(stdout(&output), "", "threadlane {args:?}");
        let message = stderr(&output);
        assert_eq!(message.lines().count(), 1, "threadlane {args:?}: {message}");
        assert!(
            message.starts_with(&format!(
                "threadlane: hub down is unreachable: cannot connect to {socket}: "
            )),
            "threadlane {args:?}: {message}"
        );
    }

    let output = threadlane(client.path(), &["status"]).await;
    assert!(!output.status.success());
    assert!(
        stdout(&output).starts_with(&format!("down: unreachable (cannot connect to {socket}: ")),
        "{}",
        stdout(&output)
    );

    // With a second hub paired, an unnamed request lists both.
    expect_success(
        client.path(),
        &["hub", "add", "home", "/nonexistent/home.sock"],
        "paired home: /nonexistent/home.sock\n",
    )
    .await;
    let output = threadlane(client.path(), &["projects"]).await;
    assert!(!output.status.success());
    assert_eq!(
        stderr(&output),
        "threadlane: several hubs are paired (down, home); choose one with --space\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_lists_adds_and_changes_routines() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let client_dir = client.path();
    let socket = hub.socket().display().to_string();
    expect_success(
        client_dir,
        &["hub", "add", "work", &socket],
        &format!("paired work: {socket}\n"),
    )
    .await;
    expect_success(
        client_dir,
        &["routines", "-p", "web"],
        "3 daily at 09:00 Europe/Berlin, due now: Summarize the night.\n\
         4 follow-up, due now (paused), set by agent: Summarize the night.\n",
    )
    .await;
    expect_success(
        client_dir,
        &[
            "routine",
            "add",
            "--daily",
            "09:00",
            "--zone",
            "Europe/Berlin",
            "-p",
            "web",
            "Summarize",
            "the",
            "night.",
        ],
        "created routine 5 daily at 09:00 Europe/Berlin, due now: Summarize the night.\n",
    )
    .await;
    expect_success(
        client_dir,
        &[
            "routine",
            "add",
            "--weekdays",
            "mon,fri",
            "--at",
            "8:30",
            "--zone",
            "UTC",
            "standup",
        ],
        "created routine 5 on mon,fri at 08:30 UTC, due now: Summarize the night.\n",
    )
    .await;
    // Without --zone, this computer's zone.
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        command(client_dir, &["routine", "add", "--every", "2h", "poll"])
            .env("TZ", "Asia/Tokyo")
            .output(),
    )
    .await
    .expect("threadlane finishes")
    .expect("threadlane runs");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "created routine 5 every 120 minutes, due now: Summarize the night.\n"
    );
    expect_success(
        client_dir,
        &["routine", "pause", "3"],
        "paused 3 daily at 09:00 Europe/Berlin, due now (paused): Summarize the night.\n",
    )
    .await;
    expect_success(
        client_dir,
        &["routine", "resume", "3"],
        "resumed 3 daily at 09:00 Europe/Berlin, due now: Summarize the night.\n",
    )
    .await;
    expect_success(client_dir, &["routine", "cancel", "3"], "cancelled 3\n").await;

    for bad in [
        &["routine", "add", "--daily", "25:00", "--zone", "UTC", "x"][..],
        &["routine", "add", "--every", "0", "--zone", "UTC", "x"],
        &["routine", "add", "--weekdays", "mon", "--zone", "UTC", "x"],
        &[
            "routine", "add", "--daily", "09:00", "--every", "2h", "--zone", "UTC", "x",
        ],
        &["routine", "pause", "T-1"],
    ] {
        let output = threadlane(client_dir, bad).await;
        assert!(!output.status.success(), "threadlane {bad:?} is refused");
    }

    let sent = hub.fake.0.timers.lock().expect("lock").clone();
    assert_eq!(
        sent,
        vec![
            TimerRequest::List {
                project: Some("web".to_owned())
            },
            created(
                Some("web"),
                "Summarize the night.",
                Rule::Daily { hour: 9, minute: 0 },
                "Europe/Berlin",
                &sent,
            ),
            created(
                None,
                "standup",
                Rule::Weekdays {
                    days: vec![Weekday::Mon, Weekday::Fri],
                    hour: 8,
                    minute: 30
                },
                "UTC",
                &sent,
            ),
            created(
                None,
                "poll",
                Rule::Interval { minutes: 120 },
                "Asia/Tokyo",
                &sent,
            ),
            TimerRequest::Pause { timer: TimerId(3) },
            TimerRequest::Resume { timer: TimerId(3) },
            TimerRequest::Cancel { timer: TimerId(3) },
        ]
    );
    hub.stop().await;
}

/// The create request with this instruction among `sent`, checked but for its
/// fresh request id.
fn created(
    project: Option<&str>,
    instruction: &str,
    rule: Rule,
    zone: &str,
    sent: &[TimerRequest],
) -> TimerRequest {
    let client_request_id = sent
        .iter()
        .find_map(|request| match request {
            TimerRequest::CreateRoutine {
                instruction: seen,
                client_request_id,
                ..
            } if seen == instruction => Some(client_request_id.clone()),
            _ => None,
        })
        .expect("sent");
    assert!(!client_request_id.is_empty());
    TimerRequest::CreateRoutine {
        project: project.map(str::to_owned),
        instruction: instruction.to_owned(),
        recurrence: Recurrence {
            rule,
            zone: zone.to_owned(),
        },
        client_request_id,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_pauses_and_resumes_projects_and_cancels_tasks() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().expect("tempdir");
    let client_dir = client.path();
    let socket = hub.socket().display().to_string();
    expect_success(
        client_dir,
        &["hub", "add", "work", &socket],
        &format!("paired work: {socket}\n"),
    )
    .await;
    expect_success(
        client_dir,
        &["project", "pause", "api"],
        "paused project api\n",
    )
    .await;
    expect_success(
        client_dir,
        &["project", "resume", "api", "-s", "work"],
        "resumed project api\n",
    )
    .await;
    expect_success(
        client_dir,
        &["task", "cancel", "T-0003"],
        "T-0003 is cancelled\n",
    )
    .await;
    let output = threadlane(client_dir, &["task", "cancel", "3x"]).await;
    assert!(!output.status.success(), "a task id is checked");
    assert!(
        stderr(&output).contains("not a task id"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        *hub.fake.0.org.lock().expect("lock"),
        vec![
            OrgRequest::SetPaused {
                project: Some("api".to_owned()),
                paused: true,
            },
            OrgRequest::SetPaused {
                project: Some("api".to_owned()),
                paused: false,
            },
        ]
    );
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_pacing_reads_updates_and_turns_off_the_space_settings() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().unwrap();
    let dir = client.path();
    expect_success(
        dir,
        &["hub", "add", "work", hub.data_dir().to_str().unwrap()],
        &format!("paired work: {}\n", hub.socket().display()),
    )
    .await;
    let off = "Rate: off\nQuiet hours: off\nDigest: off\n";
    expect_success(dir, &["pacing"], off).await;
    expect_success(
        dir,
        &[
            "pacing",
            "--rate",
            "2",
            "--quiet",
            "21:30-07:00",
            "--zone",
            "America/Toronto",
            "--digest",
            "15",
        ],
        "Rate: 2/hour\nQuiet hours: 21:30-07:00 (America/Toronto)\nDigest: 15 minutes\n",
    )
    .await;
    expect_success(
        dir,
        &["pacing", "--rate", "3"],
        "Rate: 3/hour\nQuiet hours: 21:30-07:00 (America/Toronto)\nDigest: 15 minutes\n",
    )
    .await;
    expect_success(dir, &["pacing", "--off"], off).await;
    expect_success(dir, &["pacing"], off).await;
    assert!(hub.fake.0.org.lock().unwrap().iter().any(|request| matches!(request, OrgRequest::SetPacing { pacing }
        if pacing.rate == Some(2) && pacing.quiet.as_ref().unwrap().start_minute == 1290 && pacing.digest_minutes == Some(15))));
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_stop_targets_the_default_lead_a_named_project_or_a_task() {
    use threadlane_core::client::LaneTarget;
    use threadlane_core::ids::TaskId;

    let hub = Hub::start().await;
    let client = tempfile::tempdir().unwrap();
    let dir = client.path();
    expect_success(
        dir,
        &["hub", "add", "work", hub.data_dir().to_str().unwrap()],
        &format!("paired work: {}\n", hub.socket().display()),
    )
    .await;
    expect_success(dir, &["stop"], "Stop requested.\n").await;
    expect_success(dir, &["stop", "-p", "Web"], "Stop requested.\n").await;
    expect_success(dir, &["stop", "--task", "T-0001"], "Stop requested.\n").await;
    expect_success(dir, &["resume", "--task", "T-0001"], "Lane resumed.\n").await;
    {
        let org = hub.fake.0.org.lock().unwrap();
        let targets: Vec<_> = org
            .iter()
            .filter_map(|request| match request {
                OrgRequest::Stop {
                    target,
                    client_request_id,
                } => {
                    assert!(uuid::Uuid::parse_str(client_request_id).is_ok());
                    Some(target.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            targets,
            [
                LaneTarget::Lead { project: None },
                LaneTarget::Lead {
                    project: Some("Web".to_owned())
                },
                LaneTarget::Task { task: TaskId(1) }
            ]
        );
        assert_eq!(
            org.last(),
            Some(&OrgRequest::ResumeLane {
                target: LaneTarget::Task { task: TaskId(1) }
            })
        );
    }
    hub.stop().await;
}

#[tokio::test]
async fn cli_rejects_conflicting_lane_targets_and_incomplete_quiet_hours() {
    let client = tempfile::tempdir().unwrap();
    for (args, expected) in [
        (
            &["stop", "-p", "Web", "--task", "T-0001"][..],
            "cannot be used with",
        ),
        (&["pacing", "--quiet", "21:30-07:00"][..], "--zone"),
        (
            &["pacing", "--off", "--rate", "2"][..],
            "cannot be used with",
        ),
    ] {
        let output = threadlane(client.path(), args).await;
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains(expected),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[tokio::test]
async fn cli_sends_operations_to_the_selected_hub_and_prints_its_reply() {
    let hub = Hub::start().await;
    let client = tempfile::tempdir().unwrap();
    let data_dir = hub.data_dir().to_str().unwrap();
    expect_success(
        client.path(),
        &["hub", "add", "work", data_dir],
        &format!("paired work: {}\n", hub.socket().display()),
    )
    .await;

    let requests = [
        (
            &["metrics", "-s", "work", "-p", "General"][..],
            OperationsRequest::Metrics {
                project: Some("General".into()),
            },
        ),
        (
            &["lane", "T-0003"][..],
            OperationsRequest::Lane {
                target: LaneTarget::Task {
                    task: threadlane_core::ids::TaskId(3),
                },
            },
        ),
        (
            &["replay", "7"][..],
            OperationsRequest::Replay { event: EventId(7) },
        ),
        (
            &["memory", "rollback", "space/notes/style.md", "2"][..],
            OperationsRequest::Rollback {
                path: "space/notes/style.md".into(),
                version: 2,
                project: None,
            },
        ),
        (
            &["trace", "event-7"][..],
            OperationsRequest::Trace {
                id: "event-7".into(),
            },
        ),
    ];
    for (args, request) in &requests {
        let output = threadlane(client.path(), args).await;
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        let reply = serde_json::from_slice::<OperationsReply>(&output.stdout).unwrap();
        let OperationsReply::Trace { trace } = reply else {
            panic!("expected fake reply");
        };
        assert_eq!(trace.id, format!("{request:?}"));
    }
    let seen = hub.fake.0.operations.lock().expect("operations").clone();
    assert_eq!(
        seen,
        requests
            .into_iter()
            .map(|(_, request)| request)
            .collect::<Vec<_>>()
    );
    hub.stop().await;
}
