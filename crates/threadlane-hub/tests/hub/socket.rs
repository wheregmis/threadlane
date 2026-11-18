//! The binary's pieces that need no runner: the directory lock, and a hub
//! served on a Unix socket answering a `LocalClient`.

use std::time::Duration;

use serde_json::json;
use threadlane_core::client::{HubApi, TimerReply, TimerRequest, UserMessage};
use threadlane_hub::Hub;
use threadlane_hub::config::{DataDir, HubConfig};
use threadlane_transport_local::{LocalClient, SOCKET_FILE, serve};
use tokio::sync::oneshot;

use crate::common::{Reply, TestHub, play};

#[tokio::test]
async fn a_hub_directory_cannot_be_opened_twice() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(dir.path());
    let first = Hub::open(data_dir.clone(), HubConfig::default())
        .await
        .unwrap();
    let second = Hub::open(data_dir.clone(), HubConfig::default()).await;
    let error = second.expect_err("the lock keeps a second hub out");
    assert!(
        error.to_string().contains("another hub process"),
        "{error:#}"
    );
    drop(first);
    Hub::open(data_dir, HubConfig::default())
        .await
        .expect("the lock is released with the first hub");
}

#[tokio::test]
async fn a_served_hub_answers_a_local_client() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join(SOCKET_FILE);
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn({
        let socket = socket.clone();
        let api = hub.core.api();
        async move {
            serve(&socket, api, async {
                let _ = stopped.await;
            })
            .await
        }
    });

    let client = LocalClient::new(&socket);
    let mut info = None;
    for _ in 0..200 {
        if let Ok(answer) = client.info().await {
            info = Some(answer);
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(info.expect("the hub answers").space, "Space");
    let projects = client.projects().await.unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(
        client
            .timers(TimerRequest::List { project: None })
            .await
            .unwrap(),
        TimerReply::Timers { timers: Vec::new() },
        "the Timers capability crosses the socket"
    );

    let accepted = client
        .send_message(&UserMessage {
            project: None,
            text: "Hello over the socket".into(),
            client_message_id: "sock-1".into(),
            task: None,
        })
        .await
        .unwrap();
    assert!(!accepted.duplicate);
    assert_eq!(hub.event(accepted.event_id).await.source_event_id, "sock-1");

    play(&*hub.link, Reply::Send(Some("toolu_1"), "Hello back"))
        .await
        .unwrap();
    hub.core.dispatch_once().await.unwrap();
    let items = client.feed(0, Duration::ZERO).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].message.body, json!({ "text": "Hello back" }));

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!socket.exists());
}
