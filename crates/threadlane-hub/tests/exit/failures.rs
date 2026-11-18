//! Failed runs with the real runner (docs/milestone-1.md §10 items 5 and 7,
//! Plan.md §4.9, §8.6).

use std::time::Duration;

use serde_json::json;
use threadlane_core::backend::NativeSession;
use threadlane_core::model::{EventStatus, OutboxKind, RunOutcome, RunStatus, SessionStatus};
use threadlane_core::store::SessionStore;
use threadlane_core::time::Clock;
use threadlane_testkit::{Script, SessionOutcome};

use crate::harness::{Sim, reply_script, reply_with};
use crate::invariants::{assert_invariants, assert_quiet};

#[tokio::test]
async fn a_failing_run_backs_off_then_dies_with_one_notice() {
    let sim = Sim::start().await;
    sim.backend()
        .set_default(Script::new().text("Trying.").fail("the backend crashed"));
    let event = sim.send("m-1", "Do the thing").await;
    let settings = sim.world.settings.clone();
    assert_eq!(settings.max_attempts, 5);

    for attempt in 1..=settings.max_attempts {
        assert!(sim.work_once().await.unwrap(), "attempt {attempt} runs");
        let failed_at = sim.clock().now();
        let current = sim.event(event).await;
        assert_eq!(current.attempts, attempt);
        if attempt == settings.max_attempts {
            break;
        }
        assert_eq!(current.status, EventStatus::Pending);
        let backoff = settings.retry_backoff(attempt - 1);
        assert_eq!(
            sim.general_lane().await.backoff_until,
            Some(failed_at + backoff),
            "attempt {attempt} backs off by {backoff:?}"
        );
        assert!(!sim.work_once().await.unwrap(), "backing off");
        sim.clock().advance(backoff - Duration::from_millis(1));
        assert!(!sim.work_once().await.unwrap(), "still backing off");
        sim.clock().advance(Duration::from_millis(1));
    }

    let dead = sim.event(event).await;
    assert_eq!(dead.status, EventStatus::Dead);
    assert!(
        dead.status_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("the backend crashed")),
        "{:?}",
        dead.status_reason
    );
    assert_eq!(
        sim.backend().received().len(),
        5,
        "one backend start per attempt"
    );
    assert!(
        sim.runs()
            .await
            .iter()
            .all(|run| run.status == RunStatus::Failed)
    );
    sim.clock().advance(settings.retry_backoff_max);
    assert!(
        !sim.work_once().await.unwrap(),
        "a dead event is not retried"
    );

    assert_eq!(sim.dispatch().await.unwrap(), 1);
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1, "one notice");
    assert_eq!(feed[0].message.kind, OutboxKind::SystemNotice);
    assert_eq!(sim.dispatch().await.unwrap(), 0);

    assert_quiet(&sim, "dies after max attempts").await;
    assert_invariants(&sim, "dies after max attempts").await;
}

#[tokio::test]
async fn a_lost_native_session_starts_fresh_without_using_an_attempt() {
    let sim = Sim::start().await;
    sim.backend().push(reply_script());
    sim.send("m-1", "First question").await;
    sim.work_until_idle().await.unwrap();
    let first = sim.runs().await[0].clone();
    let native = sim.backend().received()[0].session_id().to_owned();

    // The session's files are deleted on the machine.
    assert!(sim.backend().forget_session(&native));
    sim.backend().push(reply_with("toolu_2", "Second answer."));
    let event = sim.send("m-2", "Second question").await;
    assert!(sim.work_once().await.unwrap());
    assert!(!sim.work_once().await.unwrap(), "nothing is left");

    let received = sim.backend().received();
    let outcomes: Vec<_> = received.iter().map(|run| run.session_outcome).collect();
    assert_eq!(
        outcomes,
        [
            SessionOutcome::Created,
            SessionOutcome::Missing,
            SessionOutcome::Created
        ]
    );
    assert_eq!(
        received[1].session,
        NativeSession::Resume { id: native },
        "the hub tried to resume first"
    );

    let done = sim.event(event).await;
    assert_eq!(done.status, EventStatus::Done);
    assert_eq!(done.attempts, 0, "no attempt was used");
    let lane = sim.general_lane().await;
    assert!(
        lane.backoff_until
            .is_none_or(|until| until <= sim.clock().now())
    );
    assert_eq!(lane.lease_epoch, 2, "retried under the same claim");

    let runs = sim.runs().await;
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[1].status, RunStatus::Failed);
    assert_eq!(runs[1].session_id, first.session_id);
    assert_eq!(runs[2].status, RunStatus::Committed);
    assert_eq!(runs[2].outcome, Some(RunOutcome::Sent));
    assert_ne!(runs[2].session_id, first.session_id, "a fresh session");
    let old = sim.db().session(first.session_id).await.unwrap().unwrap();
    assert_eq!(old.status, SessionStatus::Closed);

    sim.dispatch().await.unwrap();
    let texts: Vec<_> = sim
        .feed()
        .await
        .iter()
        .map(|item| item.message.body["text"].clone())
        .collect();
    assert_eq!(
        texts,
        [json!(crate::harness::REPLY), json!("Second answer.")]
    );

    assert_quiet(&sim, "lost native session").await;
    assert_invariants(&sim, "lost native session").await;
}
