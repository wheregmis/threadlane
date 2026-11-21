//! `serve` and `LocalClient` against an in-memory hub.

use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;
use threadlane_core::client::{
    Accepted, ApiError, ClientResponse, DecisionReply, DecisionRequest, FeedItem, HubApi, HubInfo,
    MemoryReply, MemoryRequest, OrgReply, OrgRequest, OverviewReply, OverviewRequest, PairingReply,
    PairingRequest, ProjectSummary, TaskReply, TaskRequest, UserMessage,
};
use threadlane_core::ids::{EventId, OutboxId, ProjectId, TaskId};
use threadlane_core::model::{Mode, OutboxKind, OutboxMessage, OutboxStatus, Urgency};
use threadlane_core::time::Timestamp;
use threadlane_transport_local::{LocalClient, SOCKET_FILE, serve};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

#[derive(Debug)]
struct FakeHub {
    items: Mutex<Vec<FeedItem>>,
    changed: watch::Sender<u64>,
    sent: Mutex<Vec<UserMessage>>,
}

impl FakeHub {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            items: Mutex::new(Vec::new()),
            changed: watch::channel(0).0,
            sent: Mutex::new(Vec::new()),
        })
    }

    fn push(&self, item: FeedItem) {
        self.items.lock().expect("lock").push(item);
        self.changed.send_modify(|version| *version += 1);
    }

    fn after(&self, after: i64) -> Vec<FeedItem> {
        self.items
            .lock()
            .expect("lock")
            .iter()
            .filter(|item| item.seq > after)
            .cloned()
            .collect()
    }
}

fn info() -> HubInfo {
    HubInfo {
        space: "Work".to_owned(),
        mode_ceiling: Mode::Assisted,
        paused: false,
        schema_version: 1,
        update_version: None,
        default_models: Default::default(),
    }
}

fn projects() -> Vec<ProjectSummary> {
    vec![
        ProjectSummary {
            id: ProjectId(1),
            name: "General".to_owned(),
            is_general: true,
            paused: false,
            mode_ceiling: None,
            archived: false,
        },
        ProjectSummary {
            id: ProjectId(2),
            name: "api".to_owned(),
            is_general: false,
            paused: true,
            mode_ceiling: None,
            archived: false,
        },
    ]
}

#[async_trait]
impl HubApi for FakeHub {
    async fn update_hub(
        &self,
        request: threadlane_core::client::HubUpdate,
    ) -> Result<Option<String>, ApiError> {
        use threadlane_core::client::HubUpdate;

        Ok(Some(match request {
            HubUpdate::Check => "1.2.3".to_owned(),
            HubUpdate::Install { version } => version,
        }))
    }

    async fn info(&self) -> Result<HubInfo, ApiError> {
        Ok(info())
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        Ok(projects())
    }

    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError> {
        if message.project.as_deref() == Some("missing") {
            return Err(ApiError::NotFound("project missing".to_owned()));
        }
        self.sent.lock().expect("lock").push(message.clone());
        Ok(Accepted {
            event_id: EventId(7),
            project_id: ProjectId(1),
            duplicate: false,
        })
    }

    async fn feed(&self, after: i64, wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        let deadline = Instant::now() + wait;
        let mut changed = self.changed.subscribe();
        loop {
            let items = self.after(after);
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
        match request {
            OrgRequest::CreateProject { name, .. } => Ok(OrgReply::Project {
                project: ProjectSummary {
                    id: ProjectId(3),
                    name,
                    is_general: false,
                    paused: false,
                    mode_ceiling: None,
                    archived: false,
                },
            }),
            _ => Err(ApiError::Invalid("unsupported".to_owned())),
        }
    }

    async fn decisions(&self, request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        match request {
            DecisionRequest::Pending => Ok(DecisionReply::Pending {
                decisions: Vec::new(),
            }),
            _ => Err(ApiError::Invalid("unsupported".to_owned())),
        }
    }

    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError> {
        match request {
            PairingRequest::Clients => Ok(PairingReply::Clients {
                clients: Vec::new(),
            }),
            _ => Err(ApiError::Invalid("unsupported".to_owned())),
        }
    }

    async fn overview(&self, request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        match request {
            OverviewRequest::HeldResults => Ok(OverviewReply::HeldResults { held: None }),
            OverviewRequest::Machines => Ok(OverviewReply::Machines {
                machines: Vec::new(),
                accounts: Vec::new(),
            }),
            _ => Err(ApiError::Invalid("unsupported".to_owned())),
        }
    }

    async fn memory(&self, request: MemoryRequest) -> Result<MemoryReply, ApiError> {
        match request {
            MemoryRequest::List { .. } => Ok(MemoryReply::Files { files: Vec::new() }),
            _ => Err(ApiError::NotFound(
                "there is no file at project/a.md".to_owned(),
            )),
        }
    }
    async fn tasks(&self, request: TaskRequest) -> Result<TaskReply, ApiError> {
        match request {
            TaskRequest::Cancel { task } => Err(ApiError::NotFound(format!("task {task}"))),
            _ => Err(ApiError::Invalid("unsupported".to_owned())),
        }
    }
}

fn item(seq: i64, text: &str) -> FeedItem {
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
            kind: OutboxKind::Message,
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
        project_name: Some("General".to_owned()),
        delivered_at: Timestamp::from_millis(2_000),
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
struct Running {
    _dir: TempDir,
    socket: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<io::Result<()>>,
}

impl Running {
    async fn stop(mut self) -> io::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.expect("serve task")
    }
}

async fn start_at(dir: TempDir, api: Arc<dyn HubApi>) -> Running {
    let socket = dir.path().join(SOCKET_FILE);
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn({
        let socket = socket.clone();
        async move {
            serve(&socket, api, async {
                let _ = stopped.await;
            })
            .await
        }
    });
    let client = LocalClient::new(&socket);
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.info().await.is_err() {
        assert!(!task.is_finished(), "serve ended early: {:?}", task.await);
        assert!(Instant::now() < deadline, "the server did not come up");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Running {
        _dir: dir,
        socket,
        stop: Some(stop),
        task,
    }
}

async fn start(api: Arc<dyn HubApi>) -> Running {
    start_at(short_tempdir(), api).await
}

/// Writes all `lines` on one connection at once, then reads one answer per line.
async fn exchange_raw(socket: &Path, lines: &[&str]) -> Vec<ClientResponse> {
    let stream = UnixStream::connect(socket).await.expect("connect");
    let (read, mut write) = stream.into_split();
    let mut payload = String::new();
    for line in lines {
        payload.push_str(line);
        payload.push('\n');
    }
    write.write_all(payload.as_bytes()).await.expect("write");
    let mut reader = BufReader::new(read).lines();
    let mut answers = Vec::new();
    for _ in lines {
        let line = tokio::time::timeout(Duration::from_secs(5), reader.next_line())
            .await
            .expect("answer in time")
            .expect("read")
            .expect("a line");
        answers.push(serde_json::from_str(&line).expect("a response"));
    }
    answers
}

#[tokio::test(flavor = "multi_thread")]
async fn every_request_kind_round_trips() {
    let hub = FakeHub::new();
    hub.push(item(1, "first"));
    let server = start(hub.clone()).await;
    let client = LocalClient::new(&server.socket);

    assert_eq!(client.info().await, Ok(info()));
    use threadlane_core::client::HubUpdate;
    assert_eq!(
        client.update_hub(HubUpdate::Check).await,
        Ok(Some("1.2.3".to_owned()))
    );
    assert_eq!(
        client
            .update_hub(HubUpdate::Install {
                version: "1.2.3".to_owned()
            })
            .await,
        Ok(Some("1.2.3".to_owned()))
    );
    assert_eq!(client.projects().await, Ok(projects()));

    let message = UserMessage {
        project: Some("api".to_owned()),
        text: "hello".to_owned(),
        images: Vec::new(),
        client_message_id: "m-1".to_owned(),
        task: Some(TaskId(4)),
    };
    assert_eq!(
        client.send_message(&message).await,
        Ok(Accepted {
            event_id: EventId(7),
            project_id: ProjectId(1),
            duplicate: false,
        })
    );
    assert_eq!(*hub.sent.lock().expect("lock"), vec![message]);

    let missing = UserMessage {
        project: Some("missing".to_owned()),
        text: "hello".to_owned(),
        images: Vec::new(),
        client_message_id: "m-2".to_owned(),
        task: None,
    };
    assert_eq!(
        client.send_message(&missing).await,
        Err(ApiError::NotFound("project missing".to_owned()))
    );

    let web = ProjectSummary {
        id: ProjectId(3),
        name: "web".to_owned(),
        is_general: false,
        paused: false,
        mode_ceiling: None,
        archived: false,
    };
    assert_eq!(
        client
            .org(OrgRequest::CreateProject {
                name: "web".to_owned(),
                description: String::new(),
            })
            .await,
        Ok(OrgReply::Project {
            project: web.clone()
        })
    );
    assert_eq!(
        client.org(OrgRequest::Agents).await,
        Err(ApiError::Invalid("unsupported".to_owned()))
    );
    assert_eq!(
        client.decisions(DecisionRequest::Pending).await,
        Ok(DecisionReply::Pending {
            decisions: Vec::new()
        })
    );
    // The wire form of an organization request and its answer.
    assert_eq!(
        exchange_raw(
            &server.socket,
            &[r#"{"op":"org","request":{"op":"create_project","name":"web","description":""}}"#]
        )
        .await,
        vec![ClientResponse::Org {
            reply: OrgReply::Project { project: web }
        }]
    );

    assert_eq!(
        client.pairing(PairingRequest::Clients).await,
        Ok(PairingReply::Clients {
            clients: Vec::new()
        })
    );
    assert_eq!(
        client.pairing(PairingRequest::IssueJoinCode).await,
        Err(ApiError::Invalid("unsupported".to_owned()))
    );
    assert_eq!(
        client.overview(OverviewRequest::Machines).await,
        Ok(OverviewReply::Machines {
            machines: Vec::new(),
            accounts: Vec::new()
        })
    );
    assert_eq!(
        client.memory(MemoryRequest::List { project: None }).await,
        Ok(MemoryReply::Files { files: Vec::new() })
    );
    assert_eq!(
        client
            .tasks(TaskRequest::Cancel { task: TaskId(9) })
            .await
            .map(|_| ()),
        Err(ApiError::NotFound("task T-0009".to_owned())),
        "a task request crosses with its error"
    );
    assert_eq!(
        client
            .memory(MemoryRequest::Read {
                project: None,
                path: "project/a.md".to_owned()
            })
            .await,
        Err(ApiError::NotFound(
            "there is no file at project/a.md".to_owned()
        ))
    );

    assert_eq!(
        client.feed(0, Duration::ZERO).await,
        Ok(vec![item(1, "first")])
    );
    assert_eq!(client.feed(1, Duration::ZERO).await, Ok(Vec::new()));

    let waiting = tokio::spawn({
        let client = client.clone();
        async move { client.feed(1, Duration::from_secs(10)).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    hub.push(item(2, "second"));
    let delivered = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the waiting feed returns once an item arrives")
        .expect("task");
    assert_eq!(delivered, Ok(vec![item(2, "second")]));

    server.stop().await.expect("serve");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_feed_wait_does_not_delay_another_connection() {
    let hub = FakeHub::new();
    let server = start(hub.clone()).await;
    let client = LocalClient::new(&server.socket);

    let waiting = tokio::spawn({
        let client = client.clone();
        async move { client.feed(0, Duration::from_secs(30)).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = Instant::now();
    assert_eq!(client.info().await, Ok(info()));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "info took {:?}",
        started.elapsed()
    );
    assert!(!waiting.is_finished(), "the feed wait ended early");

    hub.push(item(1, "done"));
    let delivered = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("feed returns")
        .expect("task");
    assert_eq!(delivered, Ok(vec![item(1, "done")]));
    server.stop().await.expect("serve");
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_on_one_connection_are_answered_in_order() {
    let hub = FakeHub::new();
    let server = start(hub.clone()).await;

    // The slow feed comes first; its answer must still come first.
    let answers = exchange_raw(
        &server.socket,
        &[
            r#"{"op":"feed","after":0,"wait_ms":300}"#,
            r#"{"op":"info"}"#,
            r#"{"op":"projects"}"#,
        ],
    )
    .await;
    assert_eq!(
        answers,
        vec![
            ClientResponse::Feed { items: Vec::new() },
            ClientResponse::Info { info: info() },
            ClientResponse::Projects {
                projects: projects()
            },
        ]
    );
    server.stop().await.expect("serve");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unparseable_line_is_answered_with_invalid_and_the_connection_stays_usable() {
    let hub = FakeHub::new();
    let server = start(hub.clone()).await;

    let answers = exchange_raw(
        &server.socket,
        &["this is not json", r#"{"op":"launch"}"#, r#"{"op":"info"}"#],
    )
    .await;
    assert_eq!(answers.len(), 3);
    for answer in &answers[..2] {
        assert!(
            matches!(
                answer,
                ClientResponse::Error {
                    error: ApiError::Invalid(_)
                }
            ),
            "{answer:?}"
        );
    }
    assert_eq!(answers[2], ClientResponse::Info { info: info() });
    server.stop().await.expect("serve");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_client_reports_unreachable_when_nothing_listens() {
    let dir = short_tempdir();
    let client = LocalClient::new(dir.path().join(SOCKET_FILE));
    assert!(matches!(client.info().await, Err(ApiError::Unreachable(_))));
    assert!(matches!(
        client.feed(0, Duration::from_secs(1)).await,
        Err(ApiError::Unreachable(_))
    ));

    let too_long = dir.path().join("x".repeat(200));
    let client = LocalClient::new(too_long);
    assert!(matches!(
        client.projects().await,
        Err(ApiError::Unreachable(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_socket_file_has_mode_0600() {
    let server = start(FakeHub::new()).await;
    let metadata = std::fs::metadata(&server.socket).expect("metadata");
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    server.stop().await.expect("serve");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_socket_file_is_replaced() {
    let dir = short_tempdir();
    let socket = dir.path().join(SOCKET_FILE);
    drop(std::os::unix::net::UnixListener::bind(&socket).expect("bind a stale socket"));
    assert!(socket.exists(), "the stale socket file stays behind");

    let server = start_at(dir, FakeHub::new()).await;
    let client = LocalClient::new(&server.socket);
    assert_eq!(client.info().await, Ok(info()));
    server.stop().await.expect("serve");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_socket_or_a_regular_file_is_not_replaced() {
    let server = start(FakeHub::new()).await;
    let error = serve(&server.socket, FakeHub::new(), std::future::pending())
        .await
        .expect_err("a second server on a live socket");
    assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
    let client = LocalClient::new(&server.socket);
    assert_eq!(
        client.info().await,
        Ok(info()),
        "the first server still runs"
    );
    server.stop().await.expect("serve");

    let dir = short_tempdir();
    let file = dir.path().join(SOCKET_FILE);
    std::fs::write(&file, "keep me").expect("write");
    let error = serve(&file, FakeHub::new(), std::future::pending())
        .await
        .expect_err("a regular file in the way");
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "keep me");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_removes_the_socket_file_and_clients_become_unreachable() {
    let server = start(FakeHub::new()).await;
    let socket = server.socket.clone();
    let client = LocalClient::new(&socket);
    assert_eq!(client.info().await, Ok(info()));

    server.stop().await.expect("serve");
    assert!(!socket.exists(), "the socket file is gone");
    assert!(matches!(client.info().await, Err(ApiError::Unreachable(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_socket_path_too_long_for_the_platform_is_a_clear_error() {
    let dir = short_tempdir();
    let socket = dir.path().join("x".repeat(200));
    let error = serve(&socket, FakeHub::new(), std::future::pending())
        .await
        .expect_err("the path is too long");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("too long"), "{error}");
}
