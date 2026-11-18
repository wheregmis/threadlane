//! A stale lease writes nothing (Plan.md §6.4, invariant 10); heartbeats.

use std::time::Duration;

use serde_json::json;
use threadlane_core::link::{Begin, LinkError, RunFailure};
use threadlane_core::model::{EventStatus, JournalEntry, NewProject, RunStatus};
use threadlane_core::store::{OrgStore, OutboxStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_core::tools::ToolError;
use threadlane_hub::SEND_MESSAGE;

use crate::common::{PAST_LEASE, TestHub, begin, usable_report};

fn is_stale(result: Result<impl std::fmt::Debug, LinkError>) -> bool {
    matches!(result, Err(LinkError::StaleLease { .. }))
}

#[tokio::test]
async fn a_stale_lease_is_rejected_everywhere_and_changes_nothing() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let event = hub.send("m-1", "hi").await.event_id;
    let (stale, run) = begin(&*hub.link).await;
    let session_before = hub.db.session(run.session_id).await.unwrap().unwrap();

    hub.clock.advance(PAST_LEASE);
    assert_eq!(hub.core.sweep_once().await.unwrap(), vec![stale.lane_id]);
    let (laptop, laptop_link) = hub.other_machine("laptop").await;
    let current = laptop_link
        .request_work()
        .await
        .unwrap()
        .expect("taken over");
    assert_eq!(current.epoch, stale.epoch + 1);

    let link = &hub.link;
    let entry = JournalEntry {
        run_id: run.run_id,
        seq: 1,
        at: hub.clock.now(),
        entry: json!({ "type": "text", "text": "late" }),
    };
    assert!(is_stale(link.append(&stale, &[entry]).await));
    assert_eq!(
        link.call_tool(
            &stale,
            run.run_id,
            Some("toolu_1"),
            SEND_MESSAGE,
            &json!({ "text": "late" })
        )
        .await,
        Err(ToolError::LeaseLost)
    );
    assert!(is_stale(
        link.complete(&stale, run.run_id, Some("late")).await
    ));
    let failure = RunFailure::Error {
        message: "late".into(),
    };
    assert!(is_stale(link.fail(&stale, run.run_id, &failure).await));
    assert!(is_stale(
        link.fail(&stale, run.run_id, &RunFailure::SessionMissing)
            .await
    ));
    assert!(is_stale(
        link.native_session(&stale, run.session_id, "someone-else")
            .await
    ));
    assert!(is_stale(link.pending(&stale).await));
    assert!(is_stale(link.begin_run(&stale).await));

    // Nothing changed.
    assert!(hub.db.journal(run.run_id).await.unwrap().is_empty());
    assert_eq!(
        hub.db.run(run.run_id).await.unwrap().unwrap().status,
        RunStatus::Abandoned,
        "the sweep ended the old run"
    );
    let untouched = hub.event(event).await;
    assert_eq!(untouched.status, EventStatus::Pending);
    assert_eq!(untouched.attempts, 0);
    assert_eq!(
        hub.db.session(run.session_id).await.unwrap().unwrap(),
        session_before
    );
    assert!(
        hub.db
            .deliverable(hub.clock.now(), 10)
            .await
            .unwrap()
            .is_empty()
    );
    let lane = hub.general_lane().await;
    assert_eq!(lane.lease_epoch, current.epoch);
    assert_eq!(lane.lease_owner, Some(laptop));
    assert_eq!(lane.backoff_until, None);

    // A stale release is a no-op, and the new holder carries on.
    link.release(&stale).await.unwrap();
    assert_eq!(hub.general_lane().await.lease_owner, Some(laptop));
    assert!(matches!(
        laptop_link.begin_run(&current).await.unwrap(),
        Begin::Run(_)
    ));
}

#[tokio::test]
async fn a_lease_of_another_machine_is_refused() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "hi").await;
    let lease = hub.link.request_work().await.unwrap().unwrap();
    let (_, laptop_link) = hub.other_machine("laptop").await;
    assert!(matches!(
        laptop_link.begin_run(&lease).await,
        Err(LinkError::Rejected(_))
    ));
}

#[tokio::test]
async fn heartbeats_renew_leases_and_omit_lost_ones() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let alpha = hub
        .db
        .create_project(&NewProject {
            name: "Alpha".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    hub.send("m-1", "general work").await;
    hub.api
        .send_message(&threadlane_core::client::UserMessage {
            project: Some("Alpha".into()),
            text: "alpha work".into(),
            images: Vec::new(),
            client_message_id: "a-1".into(),
            task: None,
        })
        .await
        .unwrap();
    let general = hub.link.request_work().await.unwrap().unwrap();
    let other = hub.link.request_work().await.unwrap().unwrap();
    assert_eq!(other.lane_id, alpha.lead_lane_id);

    hub.clock.advance(Duration::from_secs(30));
    let renewed = hub
        .link
        .heartbeat(&usable_report(), &[general, other])
        .await
        .unwrap();
    assert_eq!(renewed.len(), 2);
    for lease in &renewed {
        assert_eq!(lease.expires_at, hub.clock.now() + Duration::from_secs(60));
    }

    hub.clock.advance(PAST_LEASE);
    let (_, laptop_link) = hub.other_machine("laptop").await;
    let taken = laptop_link.request_work().await.unwrap().unwrap();
    assert_eq!(taken.lane_id, general.lane_id, "the oldest work goes first");

    let kept = hub
        .link
        .heartbeat(&usable_report(), &renewed)
        .await
        .unwrap();
    assert_eq!(kept.len(), 1, "the lost lease is omitted");
    assert_eq!(kept[0].lane_id, alpha.lead_lane_id);
    assert_eq!(
        kept[0].expires_at,
        hub.clock.now() + Duration::from_secs(60)
    );
}
