//! `serve` and `IrohClient` on loopback, with an in-memory hub and gate. No
//! relay, no address lookup, no multicast: every dial uses the hub's direct
//! loopback address.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use threadlane_core::admission::{AdmittedKey, KeyRole};
use threadlane_core::client::{
    Accepted, ApiError, DecisionReply, DecisionRequest, FeedItem, HubApi, HubInfo, MemoryReply,
    MemoryRequest, OrgReply, OrgRequest, OverviewReply, OverviewRequest, PairingReply,
    PairingRequest, ProjectSummary, TaskReply, TaskRequest, UserMessage,
};
use threadlane_core::ids::{EventId, OutboxId, ProjectId, TaskId};
use threadlane_core::model::{Mode, OutboxKind, OutboxMessage, OutboxStatus, Urgency};
use threadlane_core::time::Timestamp;
use threadlane_transport_iroh::{
    CLIENT_ALPN, Endpoint, EndpointAddr, EndpointId, Gate, IrohClient, JOIN_ALPN, JoinCode,
    SecretKey, hub_addr, hub_endpoint, join, serve,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};

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

    fn push(&self, seq: i64) {
        self.items.lock().expect("lock").push(item(seq));
        self.changed.send_modify(|version| *version += 1);
    }
}

fn info() -> HubInfo {
    HubInfo {
        space: "Work".to_owned(),
        mode_ceiling: Mode::Assisted,
        schema_version: 1,
    }
}

fn general() -> ProjectSummary {
    ProjectSummary {
        id: ProjectId(1),
        name: "General".to_owned(),
        is_general: true,
        paused: false,
        mode_ceiling: None,
    }
}

fn client_key() -> AdmittedKey {
    AdmittedKey {
        endpoint_id: "abc".to_owned(),
        name: "client-1".to_owned(),
        role: KeyRole::Client,
        admitted_at: Timestamp::from_millis(5),
        revoked_at: None,
    }
}

#[async_trait]
impl HubApi for FakeHub {
    async fn info(&self) -> Result<HubInfo, ApiError> {
        Ok(info())
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        Ok(vec![general()])
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
            let items: Vec<_> = self
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
        match request {
            OrgRequest::Agents => Ok(OrgReply::Agents { agents: Vec::new() }),
            _ => Err(ApiError::Invalid("unsupported".to_owned())),
        }
    }

    async fn decisions(&self, _request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        Ok(DecisionReply::Pending {
            decisions: Vec::new(),
        })
    }

    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError> {
        match request {
            PairingRequest::Clients => Ok(PairingReply::Clients {
                clients: vec![client_key()],
            }),
            PairingRequest::Revoke { name } => Err(ApiError::NotFound(name)),
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

fn item(seq: i64) -> FeedItem {
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
            body: json!({ "text": format!("item {seq}") }),
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

/// Admits whatever presents `secret`, until revoked.
#[derive(Debug)]
struct FakeGate {
    secret: [u8; 32],
    admitted: Mutex<HashSet<EndpointId>>,
    revocations: watch::Sender<u64>,
}

impl FakeGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            secret: [9; 32],
            admitted: Mutex::new(HashSet::new()),
            revocations: watch::channel(0).0,
        })
    }

    fn admit(&self, key: EndpointId) {
        self.admitted.lock().expect("lock").insert(key);
    }

    fn revoke(&self, key: EndpointId) {
        self.admitted.lock().expect("lock").remove(&key);
        self.revocations.send_modify(|count| *count += 1);
    }

    fn is(&self, key: EndpointId) -> bool {
        self.admitted.lock().expect("lock").contains(&key)
    }
}

#[async_trait]
impl Gate for FakeGate {
    async fn is_admitted(&self, key: EndpointId) -> bool {
        self.is(key)
    }

    async fn join(&self, key: EndpointId, secret: &[u8; 32]) -> Result<(), ApiError> {
        if *secret != self.secret {
            return Err(ApiError::Invalid("the join code is not valid".to_owned()));
        }
        self.admit(key);
        Ok(())
    }

    fn revocations(&self) -> watch::Receiver<u64> {
        self.revocations.subscribe()
    }
}

#[derive(Debug)]
struct Running {
    addr: EndpointAddr,
    bound: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Running {
    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.expect("serve task");
    }
}

async fn start_at(
    secret: SecretKey,
    bind: SocketAddr,
    api: Arc<dyn HubApi>,
    gate: Arc<dyn Gate>,
) -> Running {
    // A port just given up may take a moment to be free again: iroh releases
    // its socket once the last internal handle of the old endpoint is gone.
    let mut attempts = 0;
    let endpoint: Endpoint = loop {
        match hub_endpoint(secret.clone(), Some(bind), false).await {
            Ok(endpoint) => break endpoint,
            Err(_busy) if attempts < 50 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => panic!("bind: {error}"),
        }
    };
    let addr = hub_addr(&endpoint);
    let bound = endpoint.bound_sockets()[0];
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(serve(endpoint, api, gate, async {
        let _ = stopped.await;
    }));
    Running {
        addr,
        bound,
        stop: Some(stop),
        task,
    }
}

async fn start(api: Arc<dyn HubApi>, gate: Arc<dyn Gate>) -> Running {
    start_at(
        SecretKey::generate(),
        "127.0.0.1:0".parse().expect("addr"),
        api,
        gate,
    )
    .await
}

/// A client whose key `gate` admits.
fn admitted(gate: &FakeGate, server: &Running) -> Arc<IrohClient> {
    let client = Arc::new(IrohClient::new(
        SecretKey::generate(),
        server.addr.clone(),
        false,
    ));
    gate.admit(client.id());
    client
}

#[tokio::test(flavor = "multi_thread")]
async fn every_request_kind_round_trips() {
    let hub = FakeHub::new();
    hub.push(1);
    let gate = FakeGate::new();
    let server = start(hub.clone(), gate.clone()).await;
    assert!(
        server.addr.ip_addrs().any(|addr| addr.ip().is_loopback()),
        "the join address names loopback: {:?}",
        server.addr
    );
    let client = admitted(&gate, &server);

    assert_eq!(client.info().await, Ok(info()));
    assert_eq!(client.projects().await, Ok(vec![general()]));
    let message = UserMessage {
        project: None,
        text: "hello".to_owned(),
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
        client_message_id: "m-2".to_owned(),
        task: None,
    };
    assert_eq!(
        client.send_message(&missing).await,
        Err(ApiError::NotFound("project missing".to_owned()))
    );
    assert_eq!(
        client.org(OrgRequest::Agents).await,
        Ok(OrgReply::Agents { agents: Vec::new() })
    );
    assert_eq!(
        client.pairing(PairingRequest::Clients).await,
        Ok(PairingReply::Clients {
            clients: vec![client_key()]
        })
    );
    assert_eq!(
        client
            .pairing(PairingRequest::Revoke {
                name: "nobody".to_owned()
            })
            .await,
        Err(ApiError::NotFound("nobody".to_owned()))
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
    assert_eq!(client.feed(0, Duration::ZERO).await, Ok(vec![item(1)]));

    let waiting = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.feed(1, Duration::from_secs(10)).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    hub.push(2);
    let delivered = timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the waiting feed returns once an item arrives")
        .expect("task");
    assert_eq!(delivered, Ok(vec![item(2)]));

    client.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_that_is_not_admitted_gets_no_application_data() {
    let gate = FakeGate::new();
    let server = start(FakeHub::new(), gate.clone()).await;
    let stranger = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .expect("bind");

    let connection = stranger
        .connect(server.addr.clone(), CLIENT_ALPN)
        .await
        .expect("the handshake itself completes");
    let answer = async {
        let (mut send, mut recv) = connection.open_bi().await.ok()?;
        send.write_all(b"{\"op\":\"info\"}\n").await.ok()?;
        send.finish().ok()?;
        recv.read_to_end(1 << 20).await.ok()
    };
    let answer = timeout(Duration::from_secs(10), answer)
        .await
        .expect("the hub closes the connection rather than leave it hanging");
    assert_eq!(answer, None, "not a single response byte");
    assert!(
        connection
            .close_reason()
            .is_some_and(|reason| reason.to_string().contains("not admitted")),
        "{:?}",
        connection.close_reason()
    );

    // The pairing protocol answers once and then closes, whatever is asked.
    let connection = stranger
        .connect(server.addr.clone(), JOIN_ALPN)
        .await
        .expect("connect");
    let (mut send, mut recv) = connection.open_bi().await.expect("open");
    send.write_all(b"{\"op\":\"info\"}\n").await.expect("write");
    send.finish().expect("finish");
    let answer = recv.read_to_end(512).await.expect("an answer");
    let answer: Result<(), ApiError> = serde_json::from_slice(&answer).expect("json");
    assert!(matches!(answer, Err(ApiError::Invalid(_))), "{answer:?}");
    let _closed = timeout(Duration::from_secs(10), connection.closed())
        .await
        .expect("closed");
    assert!(!gate.is(stranger.id()));

    stranger.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_join_code_admits_its_key_and_a_wrong_secret_admits_nobody() {
    let gate = FakeGate::new();
    let server = start(FakeHub::new(), gate.clone()).await;

    let wrong = JoinCode {
        hub: server.addr.clone(),
        secret: [1; 32],
    };
    let key = SecretKey::generate();
    let refused = join(key.clone(), &wrong, false).await;
    assert!(matches!(refused, Err(ApiError::Invalid(_))), "{refused:?}");
    assert!(!gate.is(key.public()));

    let code: JoinCode = JoinCode {
        hub: server.addr.clone(),
        secret: gate.secret,
    }
    .to_string()
    .parse()
    .expect("parse");
    join(key.clone(), &code, false).await.expect("join");
    assert!(gate.is(key.public()));
    let client = IrohClient::new(key, server.addr.clone(), false);
    assert_eq!(client.info().await, Ok(info()));
    client.close().await;
    server.stop().await;
}

/// The next item or error from a feed stream, within ten seconds.
async fn next(feed: &mut mpsc::Receiver<Result<FeedItem, ApiError>>) -> Result<FeedItem, ApiError> {
    timeout(Duration::from_secs(10), feed.recv())
        .await
        .expect("something in time")
        .expect("the stream is open")
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_key_closes_its_connection_and_feed_at_once() {
    let hub = FakeHub::new();
    let gate = FakeGate::new();
    let server = start(hub.clone(), gate.clone()).await;
    let client = admitted(&gate, &server);
    assert_eq!(client.info().await, Ok(info()));

    let mut feed = client.follow(0);
    hub.push(1);
    assert_eq!(next(&mut feed).await, Ok(item(1)));
    let waiting = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.feed(1, Duration::from_secs(30)).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let revoked = Instant::now();
    gate.revoke(client.id());
    let cut = timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the waiting request ends at once")
        .expect("task");
    assert!(matches!(cut, Err(ApiError::Unreachable(_))), "{cut:?}");
    assert!(revoked.elapsed() < Duration::from_secs(5));
    // The feed cannot resume: every attempt is refused.
    let refused = next(&mut feed).await;
    assert!(
        matches!(refused, Err(ApiError::Unreachable(_))),
        "{refused:?}"
    );
    hub.push(2);
    let refused = next(&mut feed).await;
    assert!(
        matches!(refused, Err(ApiError::Unreachable(_))),
        "{refused:?}"
    );
    assert!(matches!(client.info().await, Err(ApiError::Unreachable(_))));

    client.close().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pushed_feed_delivers_without_asking_and_resumes_without_gap_or_duplicate() {
    let hub = FakeHub::new();
    hub.push(1);
    let gate = FakeGate::new();
    let secret = SecretKey::generate();
    let server = start_at(
        secret.clone(),
        "127.0.0.1:0".parse().expect("addr"),
        hub.clone(),
        gate.clone(),
    )
    .await;
    let client = admitted(&gate, &server);

    let mut feed = client.follow(0);
    assert_eq!(next(&mut feed).await, Ok(item(1)), "what was already there");
    hub.push(2);
    assert_eq!(next(&mut feed).await, Ok(item(2)), "pushed, not asked for");
    hub.push(3);
    assert_eq!(next(&mut feed).await, Ok(item(3)));

    // The hub goes away; items are delivered meanwhile; it comes back on the
    // same key and port.
    let bound = server.bound;
    server.stop().await;
    hub.push(4);
    hub.push(5);
    let server = start_at(secret, bound, hub.clone(), gate.clone()).await;
    let mut seen = Vec::new();
    while seen.len() < 2 {
        // Failed attempts while the hub was away come out as errors.
        if let Ok(item) = next(&mut feed).await {
            seen.push(item.seq);
        }
    }
    hub.push(6);
    while seen.len() < 3 {
        if let Ok(item) = next(&mut feed).await {
            seen.push(item.seq);
        }
    }
    assert_eq!(seen, [4, 5, 6], "no gap, no duplicate");

    drop(feed);
    client.close().await;
    server.stop().await;
}
