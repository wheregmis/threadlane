//! The client over iroh against real hubs, each in a temporary data
//! directory, listening on loopback with local discovery off. Nothing leaves
//! the computer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use threadlane_client::{ClientCore, ClientDirs, Reach};
use threadlane_core::admission::AdmissionStore;
use threadlane_core::client::{HubApi, PairingReply, PairingRequest};
use threadlane_core::model::{NewOutboxMessage, OutboxKind, Urgency};
use threadlane_core::store::OutboxStore;
use threadlane_core::time::Timestamp;
use threadlane_hub::CLIENT_CHANNEL;
use threadlane_hub::Hub;
use threadlane_hub::config::{DataDir, HubConfig};
use threadlane_store::HubDb;
use threadlane_transport_local::LocalClient;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// A real hub running in this process until stopped.
#[derive(Debug)]
struct RunningHub {
    dir: TempDir,
    db: HubDb,
    socket: LocalClient,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl RunningHub {
    async fn start(space: &str) -> Self {
        // Short enough for the socket path limit.
        let dir = tempfile::Builder::new()
            .prefix("tl")
            .tempdir_in("/tmp")
            .unwrap();
        let config = HubConfig::parse(&format!(
            "[space]\nname = {space:?}\n\
             [iroh]\nbind = \"127.0.0.1:0\"\nlocal_discovery = false\n\
             [timing]\npoll_secs = 1\nheartbeat_secs = 1\n\
             [backends.claude]\nprogram = {:?}",
            dir.path().join("no-claude-here").display().to_string()
        ))
        .unwrap();
        let hub = Hub::open(DataDir::new(dir.path()), config).await.unwrap();
        let db = hub.db.clone();
        let socket = LocalClient::new(hub.data_dir.socket());
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            hub.run(async {
                let _ = stopped.await;
            })
            .await
        });
        for _ in 0..500 {
            if socket.info().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            dir,
            db,
            socket,
            stop: Some(stop),
            task,
        }
    }

    /// A join code, asked for over the Unix socket.
    async fn code(&self) -> String {
        match self
            .socket
            .pairing(PairingRequest::IssueJoinCode)
            .await
            .unwrap()
        {
            PairingReply::JoinCode { code, .. } => code,
            other => panic!("expected a join code, got {other:?}"),
        }
    }

    /// Queues a notice for the client feed; the dispatcher delivers it.
    async fn notice(&self, text: &str) {
        self.db
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
                created_at: Timestamp::from_millis(0),
            })
            .await
            .unwrap();
    }

    async fn stop(mut self) -> TempDir {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.unwrap().unwrap();
        self.dir
    }
}

fn command(client_dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_threadlane"));
    command
        .args(args)
        .env("THREADLANE_CLIENT_DIR", client_dir)
        .env("THREADLANE_LOCAL_DISCOVERY", "off")
        .kill_on_drop(true);
    command
}

async fn threadlane(client_dir: &Path, args: &[&str]) -> Output {
    timeout(Duration::from_secs(30), command(client_dir, args).output())
        .await
        .expect("threadlane finishes")
        .expect("threadlane runs")
}

/// Runs `threadlane` and expects success; returns its standard output.
async fn success(client_dir: &Path, args: &[&str]) -> String {
    let output = threadlane(client_dir, args).await;
    assert!(
        output.status.success(),
        "threadlane {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_pairs_with_a_real_hub_over_iroh_and_works_end_to_end() {
    let hub = RunningHub::start("Work").await;
    let client = tempfile::tempdir().unwrap();
    let dir = client.path();

    let code = hub.code().await;
    let paired = success(dir, &["hub", "join", "work", &code]).await;
    assert!(paired.starts_with("paired work: iroh "), "{paired}");
    assert!(
        !threadlane(dir, &["hub", "join", "again", &code])
            .await
            .status
            .success(),
        "a join code works once"
    );
    let key = dir.join("keys").join("work.key");
    let mode = std::fs::metadata(&key).unwrap().permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o600
    );

    assert_eq!(
        success(dir, &["status"]).await,
        "work: reachable, space Work, ceiling on_request\n"
    );
    assert_eq!(
        success(dir, &["projects"]).await,
        "[work] General (general)\n"
    );
    assert_eq!(
        success(dir, &["send", "--no-wait", "hello"]).await,
        "[work] accepted as event 1\n"
    );
    assert_eq!(
        success(dir, &["agent", "add", "Reviewer", "--role", "reviewer"]).await,
        "created agent Reviewer (reviewer)\n"
    );
    // A second client, paired with a code the first one asked for.
    let issued = success(dir, &["pair-code", "-s", "work"]).await;
    let (heading, second_code) = issued.trim_end().split_once('\n').expect("two lines");
    assert_eq!(heading, "[work] join code, for one use within 10 minutes:");
    let second_client = tempfile::tempdir().unwrap();
    let second = second_client.path();
    success(second, &["hub", "join", "work", second_code]).await;
    assert_eq!(
        success(dir, &["clients"]).await,
        "client-1 (client)\nclient-2 (client)\n"
    );

    // The feed is pushed: nothing asks again after the first request.
    let mut follower = command(dir, &["feed", "--follow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(follower.stdout.take().unwrap()).lines();
    hub.notice("backup finished").await;
    let line = timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("a line in time")
        .unwrap()
        .unwrap();
    assert_eq!(line, "[work] notice: backup finished");

    // The second client revokes the first: its follower is cut off at once.
    assert_eq!(
        success(second, &["revoke", "client-1"]).await,
        "revoked client-1\n"
    );
    let exited = timeout(Duration::from_secs(10), follower.wait())
        .await
        .expect("the follower stops at once")
        .unwrap();
    assert!(!exited.success());
    let status = threadlane(dir, &["status"]).await;
    assert!(!status.status.success());
    assert!(
        String::from_utf8_lossy(&status.stdout).starts_with("work: unreachable"),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
    assert_eq!(
        success(second, &["clients"]).await,
        "client-1 (client) (revoked)\nclient-2 (client)\n"
    );

    assert_eq!(
        success(dir, &["hub", "remove", "work"]).await,
        "removed work\n"
    );
    assert!(!key.exists(), "the key goes with the pairing");
    hub.stop().await;
}

/// Every file under `root`, read whole.
fn contents_under(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                found.push((path, bytes));
            }
        }
    }
    found
}

fn holds(files: &[(PathBuf, Vec<u8>)], needle: &str) -> Option<PathBuf> {
    files
        .iter()
        .find(|(_, bytes)| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
        .map(|(path, _)| path.clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn one_client_paired_with_two_hubs_uses_two_keys_and_the_hubs_share_nothing() {
    let work = RunningHub::start("Work").await;
    let home = RunningHub::start("Home").await;
    let client = tempfile::tempdir().unwrap();
    let dirs = ClientDirs::new(client.path());
    let mut core = ClientCore::load(dirs.clone())
        .unwrap()
        .with_local_discovery(false);
    core.join("work", &work.code().await).await.unwrap();
    core.join("home", &home.code().await).await.unwrap();

    let ids: Vec<String> = core
        .hubs()
        .iter()
        .map(|hub| match &hub.reach {
            Reach::Iroh { hub, .. } => hub.to_string(),
            other => panic!("expected iroh, got {other:?}"),
        })
        .collect();
    let keys: Vec<String> = ["work", "home"]
        .iter()
        .map(|name| {
            threadlane_transport_iroh::load_key(&dirs.key_file(name))
                .unwrap()
                .public()
                .to_string()
        })
        .collect();
    assert_ne!(keys[0], keys[1], "one key per joining");

    // Both answer, and one merged feed labels each item with its space.
    let statuses = core.status().await;
    assert!(
        statuses.iter().all(|status| status.is_reachable()),
        "{statuses:?}"
    );
    let mut merged = core.follow_all().unwrap();
    work.notice("from work").await;
    home.notice("from home").await;
    let mut seen = BTreeSet::new();
    while seen.len() < 2 {
        let item = timeout(Duration::from_secs(10), merged.recv())
            .await
            .expect("an item in time")
            .unwrap()
            .unwrap();
        seen.insert(format!("{}: {}", item.space, item.text()));
        core.acknowledge(&item).unwrap();
    }
    assert_eq!(
        seen,
        BTreeSet::from(["home: from home".to_owned(), "work: from work".to_owned()])
    );
    drop(merged);

    let work_keys = work.db.admitted_keys().await.unwrap();
    let home_keys = home.db.admitted_keys().await.unwrap();
    assert_eq!(work_keys.len(), 1);
    assert_eq!(work_keys[0].endpoint_id, keys[0]);
    assert_eq!(home_keys.len(), 1);
    assert_eq!(home_keys[0].endpoint_id, keys[1]);
    drop(core);

    // Invariant 17, at the byte level: no file of one hub holds the other
    // hub's id or the key this client uses there.
    let work_dir = work.stop().await;
    let home_dir = home.stop().await;
    let work_files = contents_under(work_dir.path());
    let home_files = contents_under(home_dir.path());
    for (files, other_hub, other_key) in [
        (&work_files, &ids[1], &keys[1]),
        (&home_files, &ids[0], &keys[0]),
    ] {
        assert_eq!(holds(files, other_hub), None, "the other hub's id");
        assert_eq!(holds(files, other_key), None, "the other hub's client key");
    }
    assert!(
        holds(&work_files, &keys[0]).is_some(),
        "the check can see a key"
    );
}
