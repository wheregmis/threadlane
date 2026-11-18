//! Failed runs (Plan.md §4.9, docs/milestone-1.md §9 "fail").

use std::sync::Arc;
use std::time::Duration;

use threadlane_core::backend::NativeSession;
use threadlane_core::link::{Begin, RunFailure};
use threadlane_core::model::{BackendKind, EventStatus, OutboxKind, RunStatus, SessionStatus};
use threadlane_core::runtime::NoHooks;
use threadlane_core::store::{MachineStore, SessionStore};
use threadlane_core::time::{Clock, Timestamp};
use threadlane_hub::HubSettings;

use crate::common::{Reply, TestHub, begin, play};

fn error() -> RunFailure {
    RunFailure::Error {
        message: "the backend crashed".into(),
    }
}

/// Grants the lane, begins a run, fails it, and releases the lane. Returns
/// when the failure happened.
async fn fail_once(
    hub: &TestHub,
    failure: &RunFailure,
) -> (Timestamp, Vec<threadlane_core::ids::EventId>) {
    let (lease, run) = begin(&*hub.link).await;
    let at = hub.clock.now();
    hub.link.fail(&lease, run.run_id, failure).await.unwrap();
    assert_eq!(
        hub.db.run(run.run_id).await.unwrap().unwrap().status,
        RunStatus::Failed
    );
    hub.link.release(&lease).await.unwrap();
    (at, run.events)
}

async fn assert_backoffs(settings: HubSettings, expected: &[u64]) {
    let hub = TestHub::with(settings, Arc::new(NoHooks)).await;
    hub.hello().await;
    let event = hub.send("m-1", "do the thing").await.event_id;
    for (attempt, seconds) in (1..).zip(expected) {
        let backoff = Duration::from_secs(*seconds);
        let (at, _) = fail_once(&hub, &error()).await;
        assert_eq!(
            hub.general_lane().await.backoff_until,
            Some(at + backoff),
            "attempt {attempt}"
        );
        let failed = hub.event(event).await;
        assert_eq!(failed.attempts, attempt);
        assert_eq!(failed.status, EventStatus::Pending);

        assert_eq!(hub.link.request_work().await.unwrap(), None, "backing off");
        hub.clock.advance(backoff - Duration::from_millis(1));
        assert_eq!(
            hub.link.request_work().await.unwrap(),
            None,
            "still backing off"
        );
        hub.clock.advance(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn an_error_counts_an_attempt_and_backs_off_by_doubling() {
    assert_backoffs(HubSettings::default(), &[10, 20, 40, 80]).await;
}

#[tokio::test]
async fn the_backoff_is_capped() {
    let settings = HubSettings {
        retry_backoff_max: Duration::from_secs(25),
        ..HubSettings::default()
    };
    assert_backoffs(settings, &[10, 20, 25, 25]).await;
}

#[tokio::test]
async fn a_backing_off_lane_does_not_start_a_run_under_the_same_lease() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "do the thing").await;
    let (lease, run) = begin(&*hub.link).await;
    hub.link.fail(&lease, run.run_id, &error()).await.unwrap();
    assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
}

#[tokio::test]
async fn after_max_attempts_the_event_dies_with_one_notice() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let event = hub.send("m-1", "do the thing").await.event_id;
    for attempt in 1..=5 {
        fail_once(
            &hub,
            &RunFailure::LimitExceeded {
                message: "wall time".into(),
            },
        )
        .await;
        if attempt < 5 {
            hub.clock.advance(Duration::from_secs(600));
        }
    }
    let dead = hub.event(event).await;
    assert_eq!(dead.status, EventStatus::Dead);
    assert_eq!(dead.attempts, 5);
    assert!(dead.status_reason.unwrap().contains("wall time"));
    hub.clock.advance(Duration::from_secs(600));
    assert_eq!(hub.link.request_work().await.unwrap(), None);

    assert_eq!(hub.core.dispatch_once().await.unwrap(), 1);
    let feed = hub.feed().await;
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].message.kind, OutboxKind::SystemNotice);
    let text = feed[0].message.body["text"].as_str().unwrap();
    assert!(text.contains("General"), "{text}");
    assert_eq!(hub.core.dispatch_once().await.unwrap(), 0);
}

#[tokio::test]
async fn a_batch_that_keeps_failing_is_retried_one_event_at_a_time() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let mut ids = Vec::new();
    for n in 0..3 {
        ids.push(
            hub.send(&format!("m-{n}"), &format!("part {n}"))
                .await
                .event_id,
        );
    }
    for _ in 0..2 {
        let (_, events) = fail_once(&hub, &error()).await;
        assert_eq!(events, ids, "the whole batch");
        hub.clock.advance(Duration::from_secs(600));
    }
    let played = play(&*hub.link, Reply::Silent).await.unwrap();
    let batches: Vec<_> = played
        .runs
        .iter()
        .map(|(assignment, _)| assignment.events.clone())
        .collect();
    assert_eq!(
        batches,
        vec![vec![ids[0]], vec![ids[1]], vec![ids[2]]],
        "one event per run"
    );
}

#[tokio::test]
async fn a_missing_native_session_starts_a_new_one_at_once_without_an_attempt() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "first").await;
    let first = play(&*hub.link, Reply::Silent).await.unwrap();
    let first = first.only_run().clone();

    let event = hub.send("m-2", "second").await.event_id;
    let (lease, resumed) = begin(&*hub.link).await;
    assert_eq!(resumed.session_id, first.session_id);
    assert!(matches!(
        resumed.native_session,
        NativeSession::Resume { .. }
    ));
    hub.link
        .fail(&lease, resumed.run_id, &RunFailure::SessionMissing)
        .await
        .unwrap();

    let old = hub.db.session(first.session_id).await.unwrap().unwrap();
    assert_eq!(old.status, SessionStatus::Closed);
    let pending = hub.event(event).await;
    assert_eq!(pending.status, EventStatus::Pending);
    assert_eq!(pending.attempts, 0);
    assert!(
        hub.general_lane()
            .await
            .backoff_until
            .is_none_or(|until| until <= hub.clock.now())
    );

    let Begin::Run(fresh) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("runs again at once");
    };
    assert_ne!(fresh.session_id, first.session_id);
    assert!(matches!(fresh.native_session, NativeSession::Start { .. }));
    assert_eq!(fresh.events, vec![event]);
}

/// Begins a run that reaches its account's usage limit: reports it, fails
/// the run, and releases the lane. Returns when it happened.
async fn limited(hub: &TestHub, resets_at: Option<Timestamp>) -> Timestamp {
    let (lease, run) = begin(&*hub.link).await;
    let at = hub.clock.now();
    hub.link
        .account(
            &lease,
            BackendKind::Claude,
            Some("me@example.com"),
            resets_at,
        )
        .await
        .unwrap();
    hub.link
        .fail(&lease, run.run_id, &RunFailure::UsageLimit)
        .await
        .unwrap();
    hub.link.release(&lease).await.unwrap();
    at
}

#[tokio::test]
async fn a_usage_limit_cools_the_account_down_and_waits_for_the_reset() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let event = hub.send("m-1", "do the thing").await.event_id;
    let resets_at = hub.clock.now() + Duration::from_secs(3600);
    let at = limited(&hub, Some(resets_at)).await;
    let accounts = hub.db.accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].label, "me@example.com");
    assert_eq!(accounts[0].cooldown_until, Some(resets_at));
    assert_eq!(
        hub.general_lane().await.backoff_until,
        Some(at),
        "the account holds the lane, not a backoff"
    );
    assert_eq!(hub.event(event).await.attempts, 0);
    assert_eq!(hub.event(event).await.status, EventStatus::Pending);
    assert_eq!(hub.link.request_work().await.unwrap(), None, "cooling down");
    hub.clock.set(resets_at - Duration::from_millis(1));
    assert_eq!(
        hub.link.request_work().await.unwrap(),
        None,
        "still cooling"
    );

    hub.clock.set(resets_at);
    let at = limited(&hub, None).await;
    assert_eq!(
        hub.db.accounts().await.unwrap()[0].cooldown_until,
        Some(at + Duration::from_secs(300)),
        "the fallback, when the backend gave no reset time"
    );
    assert_eq!(hub.event(event).await.attempts, 0);
    hub.clock.advance(Duration::from_secs(300));
    assert!(hub.link.request_work().await.unwrap().is_some(), "reset");
}

#[tokio::test]
async fn an_account_report_under_a_stale_lease_changes_nothing() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "do the thing").await;
    let (lease, _) = begin(&*hub.link).await;
    let stale = threadlane_core::model::Lease {
        epoch: lease.epoch - 1,
        ..lease
    };
    let refused = hub
        .link
        .account(&stale, BackendKind::Claude, Some("me@example.com"), None)
        .await;
    assert!(
        matches!(
            refused,
            Err(threadlane_core::link::LinkError::StaleLease { .. })
        ),
        "{refused:?}"
    );
    assert!(hub.db.accounts().await.unwrap().is_empty());
}
