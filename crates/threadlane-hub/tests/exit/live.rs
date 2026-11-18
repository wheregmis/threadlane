//! The real `threadlane-hub` binary with the real `claude`, end to end.
//! Ignored by default: it uses the logged-in Claude account. Run it with
//! `cargo test -p threadlane-hub -- --ignored`.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use threadlane_core::client::{FeedItem, HubApi, UserMessage};
use threadlane_core::model::OutboxKind;
use threadlane_transport_local::LocalClient;
use tokio::process::{Child, Command};

const PROMPT: &str = "Reply to me with the single word pong, using your send_message tool.";
const REPLY_TIMEOUT: Duration = Duration::from_secs(120);

/// Panics with `message` and the hub's log.
fn fail(log: &Path, message: &str) -> ! {
    let log = std::fs::read_to_string(log).unwrap_or_else(|error| format!("(no log: {error})"));
    panic!("{message}\n--- hub log ---\n{log}");
}

async fn wait_for_hub(hub: &mut Child, client: &LocalClient, log: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if client.info().await.is_ok() {
            return;
        }
        if let Ok(Some(status)) = hub.try_wait() {
            fail(log, &format!("the hub exited early with {status}"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    fail(log, "the hub never answered on its socket");
}

async fn poll_for_reply(client: &LocalClient, log: &Path) -> Vec<FeedItem> {
    let deadline = Instant::now() + REPLY_TIMEOUT;
    while Instant::now() < deadline {
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(30));
        match client.feed(0, wait).await {
            Ok(items) if !items.is_empty() => return items,
            Ok(_) => {}
            Err(error) => fail(log, &format!("the feed failed: {error}")),
        }
    }
    fail(log, "no reply within 120 s");
}

#[tokio::test]
#[ignore = "starts the threadlane-hub binary, which runs the real claude on the logged-in account"]
async fn the_hub_binary_answers_a_message_through_claude() {
    // Unix socket paths are limited to about 100 bytes; keep this one short.
    let dir = tempfile::Builder::new()
        .prefix("tl")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::write(
        dir.path().join("hub.toml"),
        "[backends.claude]\nmodel = \"haiku\"\n",
    )
    .unwrap();
    let log = dir.path().join("hub.log");
    let log_file = std::fs::File::create(&log).unwrap();
    let mut hub = Command::new(env!("CARGO_BIN_EXE_threadlane-hub"))
        .arg("--data-dir")
        .arg(dir.path())
        .arg("run")
        .env(
            "RUST_LOG",
            "info,threadlane_runner=debug,threadlane_backend_claude=debug",
        )
        .stdin(Stdio::null())
        .stdout(log_file.try_clone().unwrap())
        .stderr(log_file)
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let socket = dir.path().join("hub.sock");
    let client = LocalClient::new(&socket);
    wait_for_hub(&mut hub, &client, &log).await;
    let accepted = client
        .send_message(&UserMessage {
            project: None,
            text: PROMPT.to_owned(),
            client_message_id: uuid::Uuid::new_v4().to_string(),
            task: None,
        })
        .await
        .unwrap_or_else(|error| fail(&log, &format!("send failed: {error}")));
    assert!(!accepted.duplicate);

    let items = poll_for_reply(&client, &log).await;
    if items.len() != 1 || items[0].message.kind != OutboxKind::Message {
        fail(&log, &format!("expected one message, got {items:#?}"));
    }
    println!("the lead replied: {}", items[0].message.body);
    let again = client
        .feed(items[0].seq, Duration::from_secs(10))
        .await
        .unwrap();
    if !again.is_empty() {
        fail(&log, &format!("a second poll found more: {again:#?}"));
    }
    let all = client.feed(0, Duration::ZERO).await.unwrap();
    if all.len() != 1 {
        fail(
            &log,
            &format!("the feed holds {} items: {all:#?}", all.len()),
        );
    }

    let pid = hub.id().expect("the hub is running").to_string();
    let signalled = std::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .unwrap();
    assert!(signalled.success());
    let status = match tokio::time::timeout(Duration::from_secs(30), hub.wait()).await {
        Ok(status) => status.unwrap(),
        Err(_) => fail(&log, "the hub did not stop within 30 s of Ctrl-C"),
    };
    if !status.success() {
        fail(&log, &format!("the hub exited with {status}"));
    }
    assert!(!socket.exists(), "the socket is removed on shutdown");
    println!(
        "--- hub log ---\n{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
}
