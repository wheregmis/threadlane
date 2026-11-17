//! Pairing over iroh against the real hub (Plan.md §3.5): join codes, the
//! admitted list, revocation, and the hub's key. Every dial is to a loopback
//! address; nothing leaves the computer.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use threadlane_core::admission::{AdmissionStore, KeyRole};
use threadlane_core::client::{ApiError, FeedItem, HubApi, PairingReply, PairingRequest};
use threadlane_core::model::{NewOutboxMessage, OutboxKind, Urgency};
use threadlane_core::runtime::NoHooks;
use threadlane_core::store::OutboxStore;
use threadlane_core::time::Clock;
use threadlane_hub::config::{DataDir, HubConfig};
use threadlane_hub::{CLIENT_CHANNEL, Hub, HubCore, HubSettings, JOIN_CODE_TTL};
use threadlane_store::{Bootstrap, HubDb};
use threadlane_testkit::FakeClock;
use threadlane_transport_iroh::{
    IrohClient, JoinCode, SecretKey, hub_addr, hub_endpoint, join, load_key,
};
use threadlane_transport_local::LocalClient;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// An in-memory hub on a fake clock, serving iroh on loopback.
#[derive(Debug)]
struct Served {
    clock: FakeClock,
    core: HubCore,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Served {
    async fn start() -> Self {
        let clock = FakeClock::at_default_start();
        let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(clock.clone()))
            .await
            .unwrap();
        let core = HubCore::new(
            db,
            Arc::new(clock.clone()),
            Arc::new(NoHooks),
            HubSettings::default(),
        )
        .await
        .unwrap();
        let endpoint = hub_endpoint(
            SecretKey::generate(),
            Some("127.0.0.1:0".parse().unwrap()),
            false,
        )
        .await
        .unwrap();
        let core = core.with_iroh(hub_addr(&endpoint));
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(threadlane_transport_iroh::serve(
            endpoint,
            core.api(),
            core.gate(),
            async {
                let _ = stopped.await;
            },
        ));
        Self {
            clock,
            core,
            stop: Some(stop),
            task,
        }
    }

    async fn code(&self) -> JoinCode {
        issue(self.core.api().as_ref()).await
    }

    /// Delivers a system notice to the client feed.
    async fn deliver(&self, text: &str) {
        self.core
            .db()
            .enqueue(&NewOutboxMessage {
                project_id: None,
                lane_id: None,
                session_id: None,
                run_id: None,
                tool_call_id: None,
                kind: OutboxKind::SystemNotice,
                decision_id: None,
                channel: CLIENT_CHANNEL.to_owned(),
                body: json!({ "text": text }),
                urgency: Urgency::Normal,
                not_before: None,
                created_at: self.clock.now(),
            })
            .await
            .unwrap();
        assert_eq!(self.core.dispatch_once().await.unwrap(), 1);
    }

    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.unwrap();
    }
}

async fn issue(api: &dyn HubApi) -> JoinCode {
    match api.pairing(PairingRequest::IssueJoinCode).await.unwrap() {
        PairingReply::JoinCode { code, .. } => code.parse().unwrap(),
        other => panic!("expected a join code, got {other:?}"),
    }
}

async fn clients(api: &dyn HubApi) -> Vec<String> {
    match api.pairing(PairingRequest::Clients).await.unwrap() {
        PairingReply::Clients { clients } => clients
            .into_iter()
            .map(|client| {
                let state = if client.is_active() { "" } else { " (revoked)" };
                format!("{}{state}", client.name)
            })
            .collect(),
        other => panic!("expected clients, got {other:?}"),
    }
}

fn refused(result: Result<(), ApiError>) -> bool {
    matches!(result, Err(ApiError::Invalid(reason)) if reason.contains("not valid"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_join_code_works_once_and_not_after_it_expires_and_a_wrong_secret_admits_nobody() {
    let hub = Served::start().await;
    let api = hub.core.api();

    let code = hub.code().await;
    let first = SecretKey::generate();
    join(first.clone(), &code, false).await.expect("first use");
    assert!(
        refused(join(SecretKey::generate(), &code, false).await),
        "a second use is refused"
    );

    let mut wrong = hub.code().await;
    wrong.secret[0] ^= 1;
    assert!(refused(join(SecretKey::generate(), &wrong, false).await));

    let late = hub.code().await;
    hub.clock.advance(JOIN_CODE_TTL + Duration::from_secs(1));
    assert!(
        refused(join(SecretKey::generate(), &late, false).await),
        "an expired code is refused"
    );

    assert_eq!(clients(api.as_ref()).await, ["client-1"]);
    let admitted = hub.core.db().admitted_keys().await.unwrap();
    assert_eq!(admitted[0].endpoint_id, first.public().to_string());
    assert_eq!(admitted[0].role, KeyRole::Client);

    // Only the code's hash is stored.
    let stored = hub
        .core
        .db()
        .open_join_codes(hub.clock.now())
        .await
        .unwrap();
    let secret_hex: String = wrong.secret.iter().map(|b| format!("{b:02x}")).collect();
    assert!(stored.iter().all(|hash| *hash != secret_hex), "{stored:?}");

    let client = IrohClient::new(first, code.hub.clone(), false);
    assert_eq!(client.info().await.unwrap().space, "Space");
    client.close().await;
    hub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hub_that_does_not_serve_iroh_issues_no_join_code() {
    let clock = FakeClock::at_default_start();
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(clock.clone()))
        .await
        .unwrap();
    let core = HubCore::new(
        db,
        Arc::new(clock),
        Arc::new(NoHooks),
        HubSettings::default(),
    )
    .await
    .unwrap();
    let issued = core.api().pairing(PairingRequest::IssueJoinCode).await;
    assert!(matches!(issued, Err(ApiError::Invalid(_))), "{issued:?}");
    let revoked = core
        .api()
        .pairing(PairingRequest::Revoke {
            name: "client-9".into(),
        })
        .await;
    assert!(matches!(revoked, Err(ApiError::NotFound(_))), "{revoked:?}");
}

async fn next(feed: &mut mpsc::Receiver<Result<FeedItem, ApiError>>) -> Result<FeedItem, ApiError> {
    timeout(Duration::from_secs(10), feed.recv())
        .await
        .expect("something in time")
        .expect("the stream is open")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_client_is_disconnected_at_once_and_cannot_come_back() {
    let hub = Served::start().await;
    let api = hub.core.api();
    let code = hub.code().await;
    let key = SecretKey::generate();
    join(key.clone(), &code, false).await.unwrap();
    let other = SecretKey::generate();
    join(other.clone(), &hub.code().await, false).await.unwrap();

    let client = Arc::new(IrohClient::new(key.clone(), code.hub.clone(), false));
    let mut feed = client.follow(0);
    hub.deliver("first").await;
    let first = next(&mut feed).await.unwrap();
    assert_eq!(first.message.body, json!({ "text": "first" }));
    let waiting = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.feed(first.seq, Duration::from_secs(30)).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let reply = api
        .pairing(PairingRequest::Revoke {
            name: "client-1".into(),
        })
        .await
        .unwrap();
    assert!(matches!(reply, PairingReply::Revoked { client } if client.revoked_at.is_some()));
    let cut = timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the open request ends at once")
        .unwrap();
    assert!(matches!(cut, Err(ApiError::Unreachable(_))), "{cut:?}");
    // The feed stream is cut too, and every attempt to resume is refused.
    hub.deliver("second").await;
    for _ in 0..2 {
        let resumed = next(&mut feed).await;
        assert!(
            matches!(resumed, Err(ApiError::Unreachable(_))),
            "{resumed:?}"
        );
    }
    assert!(matches!(client.info().await, Err(ApiError::Unreachable(_))));
    // Not even with a fresh join code.
    assert!(refused(join(key, &hub.code().await, false).await));

    // The other client is unaffected.
    let other = IrohClient::new(other, code.hub.clone(), false);
    assert!(other.info().await.is_ok());
    assert_eq!(
        clients(api.as_ref()).await,
        ["client-1 (revoked)", "client-2"]
    );
    other.close().await;
    client.close().await;
    hub.stop().await;
}

/// A configuration on loopback with no discovery and no real `claude`.
fn config(dir: &std::path::Path) -> HubConfig {
    HubConfig::parse(&format!(
        "[iroh]\nbind = \"127.0.0.1:0\"\nlocal_discovery = false\n\
         [backends.claude]\nprogram = {:?}",
        dir.join("no-claude-here").display().to_string()
    ))
    .unwrap()
}

/// Runs the hub in `dir` until `act` returns.
async fn with_running_hub<T>(dir: &std::path::Path, act: impl AsyncFnOnce(&LocalClient) -> T) -> T {
    let hub = Hub::open(DataDir::new(dir), config(dir)).await.unwrap();
    let socket = LocalClient::new(hub.data_dir.socket());
    let (stop, stopped) = oneshot::channel::<()>();
    let running = hub.run(async {
        let _ = stopped.await;
    });
    let acting = async {
        for _ in 0..500 {
            if socket.info().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let result = act(&socket).await;
        stop.send(()).unwrap();
        result
    };
    let (ran, result) = tokio::join!(running, acting);
    ran.unwrap();
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hub_key_and_the_admitted_list_survive_a_restart() {
    let dir = tempfile::Builder::new()
        .prefix("tl")
        .tempdir_in("/tmp")
        .unwrap();
    let client_key = SecretKey::generate();

    let first_id = with_running_hub(dir.path(), async |socket| {
        // Issued over the Unix socket, the way `threadlane pair-code` does it.
        let code = issue(socket).await;
        join(client_key.clone(), &code, false).await.unwrap();
        code.hub.id
    })
    .await;
    let key_file = DataDir::new(dir.path()).key();
    let mode = std::fs::metadata(&key_file).unwrap().permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o600
    );
    assert_eq!(load_key(&key_file).unwrap().public(), first_id);

    let (second_id, info) = with_running_hub(dir.path(), async |socket| {
        assert_eq!(clients(socket).await, ["client-1"]);
        // The address may change across a restart; the id does not.
        let code = issue(socket).await;
        let client = IrohClient::new(client_key.clone(), code.hub.clone(), false);
        let info = client.info().await;
        client.close().await;
        (code.hub.id, info)
    })
    .await;
    assert_eq!(second_id, first_id, "the same key after a restart");
    assert!(info.is_ok(), "the admitted client still gets in: {info:?}");
}
