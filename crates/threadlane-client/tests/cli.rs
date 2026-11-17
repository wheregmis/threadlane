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
use threadlane_core::ids::{
    AgentId, DecisionId, EventId, LaneId, OutboxId, ProjectId, RepositoryId,
};
use threadlane_core::model::{
    Agent, Decision, DecisionKind, DecisionOption, DecisionStatus, Repository,
};
use threadlane_core::model::{Mode, OutboxKind, OutboxMessage, OutboxStatus, Urgency};
use threadlane_core::time::Timestamp;
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
    /// D-0007, pending until it is answered.
    decision: Mutex<Decision>,
}

impl Shared {
    /// Appends an item with the next `seq`.
    fn push(&self, kind: OutboxKind, project: Option<&str>, text: &str) {
        let mut items = self.items.lock().expect("lock");
        let seq = i64::try_from(items.len()).expect("small") + 1;
        items.push(FeedItem {
            seq,
            message: OutboxMessage {
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
            decision: Mutex::new(pending_decision()),
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
            },
            ProjectSummary {
                id: ProjectId(2),
                name: "api".to_owned(),
                is_general: false,
                paused: true,
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
                },
            },
            OrgRequest::Agents => OrgReply::Agents {
                agents: vec![agent("Lead", "lead", ""), agent("Reviewer", "reviewer", "")],
            },
            OrgRequest::CreateAgent {
                name,
                role,
                instructions,
            } => OrgReply::Agent {
                agent: agent(&name, &role, &instructions),
            },
            OrgRequest::Bindings { .. } => OrgReply::Bindings {
                bindings: vec![BindingSummary {
                    agent: "Lead".to_owned(),
                    role: "lead".to_owned(),
                    mode: Mode::OnRequest,
                    is_lead: true,
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
                },
            },
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
            other => Err(ApiError::NotFound(format!("{other:?}"))),
        }
    }

    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError> {
        Err(ApiError::Invalid(format!("unsupported {request:?}")))
    }

    async fn overview(&self, request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        Err(ApiError::Invalid(format!("unsupported {request:?}")))
    }

    async fn memory(&self, request: MemoryRequest) -> Result<MemoryReply, ApiError> {
        Err(ApiError::Invalid(format!("unsupported {request:?}")))
    }
}

fn pending_decision() -> Decision {
    let option = |label: &str, approve: bool| DecisionOption {
        label: label.to_owned(),
        effect: json!({ "approve": approve }),
    };
    Decision {
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
        &["agent", "add", "Reviewer", "--role", "reviewer"],
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
