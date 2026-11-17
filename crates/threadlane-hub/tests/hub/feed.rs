//! The client feed (Plan.md §11.2): long poll on the clock, paging.

use std::time::Duration;

use serde_json::json;
use threadlane_core::model::{NewOutboxMessage, OutboxKind, Urgency};
use threadlane_core::store::OutboxStore;
use threadlane_core::time::Clock;
use threadlane_hub::CLIENT_CHANNEL;

use crate::common::{Reply, TestHub, eventually, play};

#[tokio::test]
async fn a_long_poll_returns_as_soon_as_a_message_is_delivered() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let start = hub.clock.now();
    let api = hub.api.clone();
    let waiting = tokio::spawn(async move { api.feed(0, Duration::from_secs(60)).await });
    let clock = hub.clock.clone();
    eventually("the feed waits", || {
        let clock = clock.clone();
        async move { clock.sleepers() >= 1 }
    })
    .await;
    assert!(!waiting.is_finished());

    hub.send("m-1", "ping").await;
    play(&*hub.link, Reply::Send(Some("toolu_1"), "pong"))
        .await
        .unwrap();
    hub.core.dispatch_once().await.unwrap();
    let items = waiting.await.unwrap().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].message.body, json!({ "text": "pong" }));
    assert_eq!(hub.clock.now(), start, "no time passed");
}

#[tokio::test]
async fn a_long_poll_with_nothing_arriving_returns_empty_after_the_wait() {
    let hub = TestHub::start().await;
    let api = hub.api.clone();
    let waiting = tokio::spawn(async move { api.feed(0, Duration::from_secs(5)).await });
    let clock = hub.clock.clone();
    eventually("the feed waits", || {
        let clock = clock.clone();
        async move { clock.sleepers() >= 1 }
    })
    .await;

    hub.clock.advance(Duration::from_millis(4_999));
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(!waiting.is_finished(), "still waiting before the deadline");

    hub.clock.advance(Duration::from_millis(1));
    let items = waiting.await.unwrap().unwrap();
    assert!(items.is_empty());
}

#[tokio::test]
async fn after_pages_through_the_feed() {
    let hub = TestHub::start().await;
    let general = hub.general().await;
    for n in 1..=3 {
        hub.db
            .enqueue(&NewOutboxMessage {
                project_id: Some(general.id),
                lane_id: Some(general.lead_lane_id),
                session_id: None,
                run_id: None,
                tool_call_id: Some(format!("test-{n}")),
                kind: OutboxKind::Message,
                decision_id: None,
                channel: CLIENT_CHANNEL.into(),
                body: json!({ "text": format!("message {n}") }),
                urgency: Urgency::Normal,
                not_before: None,
                created_at: hub.clock.now(),
            })
            .await
            .unwrap();
    }
    assert_eq!(hub.core.dispatch_once().await.unwrap(), 3);

    let seqs = |items: Vec<threadlane_core::client::FeedItem>| {
        items.into_iter().map(|item| item.seq).collect::<Vec<_>>()
    };
    assert_eq!(
        seqs(hub.api.feed(0, Duration::ZERO).await.unwrap()),
        [1, 2, 3]
    );
    assert_eq!(seqs(hub.api.feed(1, Duration::ZERO).await.unwrap()), [2, 3]);
    assert_eq!(
        seqs(hub.api.feed(3, Duration::ZERO).await.unwrap()),
        Vec::<i64>::new()
    );
    let items = hub.api.feed(2, Duration::ZERO).await.unwrap();
    assert_eq!(items[0].message.body, json!({ "text": "message 3" }));
    assert_eq!(items[0].project_name.as_deref(), Some("General"));
}
