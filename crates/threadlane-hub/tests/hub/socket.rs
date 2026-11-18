//! The binary's pieces that need no runner: the directory lock, and a hub
//! served on a Unix socket answering a `LocalClient`.

use std::{sync::Arc, time::Duration};

use serde_json::json;
use threadlane_core::attachment::ImageAttachment;
use threadlane_core::client::{
    ApiError, CONVERSATION_PAGE, ConversationEntry, HubApi, OrgReply, OrgRequest, OverviewReply,
    OverviewRequest, TimerReply, TimerRequest, UserMessage,
};
use threadlane_core::runtime::NoHooks;
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
            images: Vec::new(),
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

    let image = test_image();
    let message = UserMessage {
        images: vec![image.clone()],
        project: None,
        text: String::new(),
        client_message_id: "sock-image".into(),
        task: None,
    };
    let bad = UserMessage {
        images: vec![ImageAttachment::new(
            "broken.png".into(),
            b"bad data".to_vec(),
        )],
        ..message.clone()
    };
    assert!(matches!(
        client.send_message(&bad).await,
        Err(ApiError::Invalid(_))
    ));
    let attached = client.send_message(&message).await.unwrap();
    assert!(client.send_message(&message).await.unwrap().duplicate);
    let played = play(&*hub.link, Reply::Silent).await.unwrap();
    assert_eq!(played.only_run().images, std::slice::from_ref(&image));
    assert!(
        played
            .only_run()
            .prompt
            .contains("Attached image: \"test.png\"")
    );
    let restarted = hub.restart(Arc::new(NoHooks)).await;
    let history = restarted
        .api
        .overview(OverviewRequest::Conversation {
            project: None,
            before: None,
            limit: 20,
        })
        .await
        .unwrap();
    assert!(matches!(history, OverviewReply::Conversation { entries }
        if entries.iter().any(|entry| matches!(entry, ConversationEntry::You { event_id, text, images, .. }
            if *event_id == attached.event_id && text.is_empty() && images == std::slice::from_ref(&image)))));

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!socket.exists());
}

fn test_image() -> ImageAttachment {
    serde_json::from_value(json!({
        "name": "test.png",
        "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
    })).unwrap()
}

#[tokio::test]
async fn project_images_stay_in_order_and_are_available_on_earlier_pages() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let OrgReply::Project { project } = hub
        .api
        .org(OrgRequest::CreateProject {
            name: "Design".into(),
            description: String::new(),
        })
        .await
        .unwrap()
    else {
        panic!("expected project");
    };

    let image = test_image();
    let mut sent = Vec::new();
    for index in 0..=CONVERSATION_PAGE {
        hub.clock.advance(Duration::from_millis(1));
        let accepted = hub
            .api
            .send_message(&UserMessage {
                project: Some(project.name.clone()),
                text: format!("Message {index}"),
                images: if index == 1 {
                    vec![image.clone()]
                } else {
                    Vec::new()
                },
                client_message_id: format!("project-image-{index}"),
                task: None,
            })
            .await
            .unwrap();
        sent.push(accepted.event_id);
    }

    let played = play(&*hub.link, Reply::Silent).await.unwrap();
    let [(first, _), (second, _), (last, _)] = &played.runs[..] else {
        panic!("expected text, image, and later text runs");
    };
    assert_eq!(second.project_id, project.id);
    assert!(first.images.is_empty() && last.images.is_empty());
    assert!(first.prompt.contains("Message 0") && last.prompt.contains("Message 2"));
    assert!(!first.prompt.contains("Message 1\n"));
    assert_eq!(second.images, std::slice::from_ref(&image));
    assert!(second.prompt.contains("Message 1\n") && !second.prompt.contains("Message 2\n"));

    let history = |project, before| {
        hub.api.overview(OverviewRequest::Conversation {
            project,
            before,
            limit: u32::MAX,
        })
    };
    let OverviewReply::Conversation { entries } =
        history(Some(project.name.clone()), None).await.unwrap()
    else {
        panic!("expected history");
    };
    assert_eq!(entries.len(), CONVERSATION_PAGE as usize);
    let ConversationEntry::You {
        event_id,
        images,
        at,
        ..
    } = &entries[0]
    else {
        panic!("expected oldest row with an image");
    };
    assert_eq!(*event_id, sent[1]);
    assert_eq!(images, std::slice::from_ref(&image));
    let earlier = history(Some(project.name), Some(*at)).await.unwrap();
    assert!(matches!(earlier, OverviewReply::Conversation { entries }
        if matches!(entries.as_slice(), [ConversationEntry::You { event_id, images, .. }]
            if *event_id == sent[0] && images.is_empty())));
    assert_eq!(
        history(None, None).await.unwrap(),
        OverviewReply::Conversation {
            entries: Vec::new()
        }
    );
}
