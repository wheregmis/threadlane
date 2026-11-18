//! Milestone 10 delivery through the real dispatcher and client feed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use threadlane_core::adapters::{ChannelAdapter, Delivered, DeliveryError};
use threadlane_core::client::{OrgReply, OrgRequest};
use threadlane_core::ids::ProjectId;
use threadlane_core::model::{
    EventKind, NewOutboxMessage, NewProject, OutboxKind, OutboxMessage, OutboxStatus, Pacing,
    QuietHours, Urgency,
};
use threadlane_core::runtime::steps::dispatcher;
use threadlane_core::store::{OrgStore, OutboxStore};
use threadlane_core::time::{Clock, Timestamp};
use threadlane_hub::{HubError, HubSettings};
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{Sim, World};
use crate::invariants::assert_invariants;

#[derive(Debug, Default)]
struct BrokenChannel {
    keys: Mutex<Vec<String>>,
    permanent: bool,
}

#[async_trait]
impl ChannelAdapter for BrokenChannel {
    fn name(&self) -> &str {
        "client"
    }

    async fn deliver(&self, message: &OutboxMessage) -> Result<Delivered, DeliveryError> {
        self.keys.lock().unwrap().push(message.delivery_key());
        Err(if self.permanent {
            DeliveryError::Permanent("address rejected".to_owned())
        } else {
            DeliveryError::Transient("try later".to_owned())
        })
    }
}

async fn queue(
    sim: &Sim,
    project: Option<ProjectId>,
    text: &str,
    kind: OutboxKind,
    urgency: Urgency,
) -> OutboxMessage {
    let project = match project {
        Some(id) => sim.db().project(id).await.unwrap().unwrap(),
        None => sim.db().general_project().await.unwrap(),
    };
    sim.db()
        .enqueue(&NewOutboxMessage {
            project_id: Some(project.id),
            lane_id: Some(project.lead_lane_id),
            session_id: None,
            run_id: None,
            tool_call_id: Some(format!("test:{text}")),
            kind,
            decision_id: None,
            channel: "client".to_owned(),
            body: json!({ "text": text }),
            urgency,
            not_before: None,
            created_at: sim.clock().now(),
        })
        .await
        .unwrap()
}

async fn message(sim: &Sim, text: &str, urgency: Urgency) -> OutboxMessage {
    queue(sim, None, text, OutboxKind::Message, urgency).await
}

async fn pacing(sim: &Sim, value: Pacing) {
    assert_eq!(
        sim.api
            .org(OrgRequest::SetPacing {
                pacing: value.clone()
            })
            .await
            .unwrap(),
        OrgReply::Pacing { pacing: value }
    );
}

fn at(text: &str) -> Timestamp {
    Timestamp::from_millis(text.parse::<jiff::Timestamp>().unwrap().as_millisecond())
}

fn quiet(end_minute: u16) -> QuietHours {
    QuietHours {
        start_minute: 0,
        end_minute,
        time_zone: "America/New_York".to_owned(),
    }
}

#[tokio::test]
async fn transient_delivery_retries_with_the_same_key_then_dies_once() {
    let world = World::with_settings(HubSettings {
        max_attempts: 3,
        ..HubSettings::default()
    })
    .await;
    let mut sim = world.boot(CrashPlan::new()).await;
    let broken = Arc::new(BrokenChannel::default());
    sim.core = sim.core.with_channels(vec![broken.clone()]);
    let original = message(&sim, "result", Urgency::Normal).await;

    for attempt in 0..3 {
        assert_eq!(sim.dispatch().await.unwrap(), 0);
        let row = sim.db().message(original.id).await.unwrap().unwrap();
        assert_eq!(row.attempts, attempt + 1);
        if attempt < 2 {
            let delay = Duration::from_secs(10 << attempt);
            assert_eq!(row.status, OutboxStatus::Pending);
            assert_eq!(row.not_before, Some(sim.clock().now() + delay));
            assert!(sim.events().await.is_empty());
            sim.clock().advance(delay - Duration::from_millis(1));
            assert_eq!(sim.dispatch().await.unwrap(), 0);
            assert_eq!(broken.keys.lock().unwrap().len(), (attempt + 1) as usize);
            sim.clock().advance(Duration::from_millis(1));
        } else {
            assert_eq!(row.status, OutboxStatus::Dead);
        }
    }
    assert_eq!(
        *broken.keys.lock().unwrap(),
        vec![original.delivery_key(); 3]
    );
    let sim = sim.restart(CrashPlan::new()).await;
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    let events = sim.events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::DeliveryFailure);
    assert_eq!(Some(events[0].lane_id), original.lane_id);
    assert_eq!(events[0].payload["outbox_id"], json!(original.id));
}

#[tokio::test]
async fn permanent_delivery_dies_on_the_first_attempt() {
    let mut sim = Sim::start().await;
    let broken = Arc::new(BrokenChannel {
        permanent: true,
        ..BrokenChannel::default()
    });
    sim.core = sim.core.with_channels(vec![broken]);
    let original = message(&sim, "permanent", Urgency::Normal).await;
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    let row = sim.db().message(original.id).await.unwrap().unwrap();
    assert_eq!((row.status, row.attempts), (OutboxStatus::Dead, 1));
    assert_eq!(sim.events().await[0].kind, EventKind::DeliveryFailure);
}

#[tokio::test]
async fn a_burst_of_low_urgency_results_is_delivered_as_one_linked_digest() {
    let sim = Sim::start().await;
    pacing(
        &sim,
        Pacing {
            digest_minutes: Some(10),
            ..Pacing::default()
        },
    )
    .await;
    let project = sim
        .db()
        .create_project(&NewProject {
            name: "Web".to_owned(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    sim.backend().set_default(
        Script::new()
            .call(
                "send",
                threadlane_hub::SEND_MESSAGE,
                json!({"text": "first", "urgency": "low"}),
            )
            .finish("Reported the result."),
    );
    sim.send("goal", "Report the first result").await;
    assert!(sim.work_once().await.unwrap());
    let start = sim.clock().now();
    let first = sim.outbox().await.remove(0);
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    sim.clock().advance(Duration::from_secs(60));
    let second = queue(
        &sim,
        Some(project.id),
        "second",
        OutboxKind::Message,
        Urgency::Low,
    )
    .await;
    let third = message(&sim, "third", Urgency::Low).await;
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    sim.clock().set(start + Duration::from_secs(600));
    assert_eq!(sim.dispatch().await.unwrap(), 1);
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1);
    let digest = &feed[0].message;
    assert_eq!(
        digest.body["digest"],
        json!([first.id, second.id, third.id])
    );
    assert_eq!(
        digest.body["text"],
        "Digest\n\nGeneral\n- first\n- third\n\nWeb\n- second"
    );
    for original in [first, second, third] {
        let row = sim.db().message(original.id).await.unwrap().unwrap();
        assert_eq!(row.status, OutboxStatus::Suppressed);
        assert_eq!(sim.db().digest_for(row.id).await.unwrap(), Some(digest.id));
    }
    assert_invariants(&sim, "digest across projects").await;
}

#[tokio::test]
async fn quiet_hours_hold_until_the_local_end_across_spring_forward() {
    let sim = Sim::start().await;
    sim.clock().set(at("2026-03-08T05:00:00Z"));
    pacing(
        &sim,
        Pacing {
            quiet: Some(quiet(8 * 60)),
            ..Pacing::default()
        },
    )
    .await;
    let first = message(&sim, "before the jump", Urgency::Normal).await;
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    assert_eq!(
        sim.db()
            .message(first.id)
            .await
            .unwrap()
            .unwrap()
            .not_before,
        Some(at("2026-03-08T12:00:00Z"))
    );
    sim.clock().set(at("2026-03-08T07:00:00Z"));
    let second = message(&sim, "after the jump", Urgency::Normal).await;
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    sim.clock().set(at("2026-03-08T11:59:59Z"));
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    assert!(sim.feed().await.is_empty());
    sim.clock().set(at("2026-03-08T12:00:00Z"));
    assert_eq!(sim.dispatch().await.unwrap(), 1);
    assert_eq!(
        sim.feed().await[0].message.body["digest"],
        json!([first.id, second.id])
    );
    assert_invariants(&sim, "quiet hours through spring forward").await;
}

#[tokio::test]
async fn quiet_end_in_a_fall_back_fold_waits_for_the_second_occurrence() {
    let sim = Sim::start().await;
    sim.clock().set(at("2026-11-01T05:00:00Z"));
    pacing(
        &sim,
        Pacing {
            quiet: Some(quiet(90)),
            ..Pacing::default()
        },
    )
    .await;
    let original = message(&sim, "when the clocks go back", Urgency::Normal).await;
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    assert_eq!(
        sim.db()
            .message(original.id)
            .await
            .unwrap()
            .unwrap()
            .not_before,
        Some(at("2026-11-01T06:30:00Z"))
    );
    sim.clock().set(at("2026-11-01T05:59:00Z"));
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    sim.clock().set(at("2026-11-01T06:30:00Z"));
    assert_eq!(sim.dispatch().await.unwrap(), 1);
}

#[tokio::test]
async fn a_decision_a_high_message_and_a_system_notice_bypass_pacing() {
    let sim = Sim::start().await;
    sim.clock().set(at("2026-03-08T05:00:00Z"));
    pacing(
        &sim,
        Pacing {
            rate: Some(1),
            quiet: Some(quiet(480)),
            digest_minutes: Some(30),
        },
    )
    .await;
    message(&sim, "low", Urgency::Low).await;
    queue(&sim, None, "decision", OutboxKind::Decision, Urgency::Low).await;
    message(&sim, "urgent", Urgency::High).await;
    queue(
        &sim,
        None,
        "notice",
        OutboxKind::SystemNotice,
        Urgency::Normal,
    )
    .await;
    assert_eq!(sim.dispatch().await.unwrap(), 3);
    let delivered: Vec<_> = sim
        .feed()
        .await
        .into_iter()
        .map(|item| item.message.body["text"].clone())
        .collect();
    assert_eq!(delivered, ["decision", "urgent", "notice"]);
}

#[tokio::test]
async fn the_rate_limit_holds_the_excess_and_delivers_it_in_order() {
    let sim = Sim::start().await;
    pacing(
        &sim,
        Pacing {
            rate: Some(1),
            ..Pacing::default()
        },
    )
    .await;
    let first = message(&sim, "first", Urgency::Normal).await;
    let second = message(&sim, "second", Urgency::Normal).await;
    assert_eq!(sim.dispatch().await.unwrap(), 1);
    let hour = Duration::from_secs(3600);
    assert_eq!(
        sim.db()
            .message(second.id)
            .await
            .unwrap()
            .unwrap()
            .not_before,
        Some(sim.clock().now() + hour)
    );
    sim.clock().advance(Duration::from_secs(1));
    let third = message(&sim, "third", Urgency::Normal).await;
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    sim.clock().advance(hour - Duration::from_secs(1));
    assert_eq!(sim.dispatch().await.unwrap(), 1);
    sim.clock().advance(hour);
    assert_eq!(sim.dispatch().await.unwrap(), 1);
    let ids: Vec<_> = sim
        .feed()
        .await
        .into_iter()
        .map(|item| item.message.id)
        .collect();
    assert_eq!(ids, [first.id, second.id, third.id]);
    assert_invariants(&sim, "ordered deliveries after rate hold").await;
}

#[tokio::test]
async fn dispatch_recovers_after_each_new_durable_step() {
    for step in [
        dispatcher::AFTER_HOLD,
        dispatcher::AFTER_DIGEST,
        dispatcher::AFTER_FAILURE,
    ] {
        let mut sim = World::new().await.boot(CrashPlan::kill_at(step)).await;
        let permanent = step == dispatcher::AFTER_FAILURE;
        if permanent {
            sim.core = sim.core.with_channels(vec![Arc::new(BrokenChannel {
                permanent,
                ..BrokenChannel::default()
            })]);
        } else {
            pacing(
                &sim,
                Pacing {
                    digest_minutes: Some(1),
                    ..Pacing::default()
                },
            )
            .await;
        }
        let first = message(&sim, "first", Urgency::Low).await;
        let second = if permanent {
            None
        } else {
            Some(message(&sim, "second", Urgency::Low).await)
        };
        if step == dispatcher::AFTER_DIGEST {
            sim.clock().advance(Duration::from_secs(60));
        }
        assert!(
            matches!(sim.dispatch().await, Err(HubError::Killed(killed)) if killed.step == step)
        );
        let sim = sim.restart(CrashPlan::new()).await;
        sim.clock().advance(Duration::from_secs(60));
        if let Some(second) = second {
            assert_eq!(sim.dispatch().await.unwrap(), 1);
            let feed = sim.feed().await;
            assert_eq!(feed.len(), 1);
            assert_eq!(feed[0].message.body["digest"], json!([first.id, second.id]));
        } else {
            assert_eq!(sim.dispatch().await.unwrap(), 0);
            assert_eq!(sim.events().await.len(), 1);
            assert_eq!(sim.events().await[0].kind, EventKind::DeliveryFailure);
        }
        assert_eq!(sim.dispatch().await.unwrap(), 0);
    }
}

#[tokio::test]
async fn a_dead_digest_reports_one_failure_to_the_oldest_originals_lane() {
    let mut sim = Sim::start().await;
    pacing(
        &sim,
        Pacing {
            digest_minutes: Some(1),
            ..Pacing::default()
        },
    )
    .await;
    let project = sim
        .db()
        .create_project(&NewProject {
            name: "Web".to_owned(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let first = queue(
        &sim,
        Some(project.id),
        "first",
        OutboxKind::Message,
        Urgency::Low,
    )
    .await;
    let second = message(&sim, "second", Urgency::Low).await;
    sim.core = sim.core.with_channels(vec![Arc::new(BrokenChannel {
        permanent: true,
        ..BrokenChannel::default()
    })]);
    sim.clock().advance(Duration::from_secs(60));
    assert_eq!(sim.dispatch().await.unwrap(), 0);
    let digest_id = sim.db().digest_for(first.id).await.unwrap().unwrap();
    assert_eq!(
        sim.db().digest_for(second.id).await.unwrap(),
        Some(digest_id)
    );
    assert_eq!(
        sim.db().message(digest_id).await.unwrap().unwrap().status,
        OutboxStatus::Dead
    );
    let events = sim.events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::DeliveryFailure);
    assert_eq!(events[0].lane_id, project.lead_lane_id);
    assert_eq!(events[0].payload["outbox_id"], json!(digest_id));
}
