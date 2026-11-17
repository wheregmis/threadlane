//! Exit scenarios of docs/milestone-8.md §2: an agent sets a follow-up and
//! wakes on time, and a routine you created fires at the right local time
//! across a daylight-saving change.

use std::time::Duration;

use serde_json::json;
use threadlane_core::client::{TimerReply, TimerRequest};
use threadlane_core::model::{Event, EventKind, Recurrence, Rule, TimerStatus};
use threadlane_core::runtime::steps::scheduler::AFTER_WAKE;
use threadlane_core::store::TimerStore;
use threadlane_core::time::{Clock, Timestamp};
use threadlane_hub::{SET_FOLLOW_UP, format_time};
use threadlane_testkit::Script;

use crate::harness::{REPLY, Sim, count, join, reply_script, until};
use crate::invariants::{assert_invariants, assert_quiet};
use crate::loops::{sleepers_at_least, spawn_hub};

/// What the lead asks to be woken with.
const FOLLOW: &str = "Check whether the build is green now.";

async fn fired(sim: &Sim) -> Vec<Event> {
    sim.events()
        .await
        .into_iter()
        .filter(|event| event.kind == EventKind::TimerFired)
        .collect()
}

/// Lets the hub's own scheduler loop make one more pass.
async fn one_pass(sim: &Sim, by: Duration) {
    let passes = count(&sim.plan, AFTER_WAKE);
    sim.clock().advance(by);
    let plan = sim.plan.clone();
    until("the scheduler makes a pass", || {
        std::future::ready(count(&plan, AFTER_WAKE) > passes)
    })
    .await;
}

#[tokio::test]
async fn an_agent_sets_a_follow_up_and_wakes_on_time() {
    let sim = Sim::start().await;
    sim.backend().push(
        Script::new()
            .text("The build is still running; I will look again.")
            .call(
                "toolu_f",
                SET_FOLLOW_UP,
                json!({ "instruction": FOLLOW, "in_minutes": 30 }),
            )
            .finish("Set a follow-up instead of waiting."),
    );
    sim.backend().route(FOLLOW, reply_script());
    sim.send("m-1", "Tell me when the build is green").await;
    sim.work_until_idle().await.unwrap();
    let set_at = sim.clock().now();
    assert!(sim.feed().await.is_empty(), "nothing to say yet");

    // The hub's loops run on the fake clock; nothing fires before it is due.
    let (stop_hub, hub) = spawn_hub(&sim);
    sleepers_at_least(&sim, "the sweep, dispatcher and scheduler wait", 3).await;
    one_pass(&sim, Duration::from_secs(29 * 60)).await;
    assert!(fired(&sim).await.is_empty(), "not due yet");
    one_pass(&sim, Duration::from_secs(60)).await;
    let events = fired(&sim).await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].received_at,
        set_at + Duration::from_secs(30 * 60),
        "fired on time"
    );
    assert!(events[0].requested, "set while working on your message");
    let _ = stop_hub.send(());
    join("the hub stops", hub).await.unwrap();

    // The lead wakes with its instruction and replies.
    sim.work_until_idle().await.unwrap();
    let received = sim.backend().received();
    assert_eq!(received.len(), 2);
    assert!(
        received[1].prompt.contains(FOLLOW),
        "{}",
        received[1].prompt
    );
    sim.dispatch().await.unwrap();
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].message.body, json!({ "text": REPLY }));
    assert_eq!(sim.event(events[0].id).await.status.as_str(), "done");
    assert_quiet(&sim, "follow-up").await;
    assert_invariants(&sim, "follow-up").await;
}

#[tokio::test]
async fn a_routine_you_created_fires_at_the_right_local_time_across_a_daylight_saving_change() {
    let sim = Sim::start().await;
    sim.backend().set_default(reply_script());
    // New York moves its clocks forward on 8 March 2026: 09:00 is 14:00Z
    // the day before and 13:00Z that day.
    sim.clock().set(Timestamp::from_millis(1_772_809_200_000)); // 2026-03-06T15:00:00Z
    let TimerReply::Timer { timer: routine } = sim
        .api
        .timers(TimerRequest::CreateRoutine {
            project: None,
            instruction: "Post the morning summary.".into(),
            recurrence: Recurrence {
                rule: Rule::Daily { hour: 9, minute: 0 },
                zone: "America/New_York".into(),
            },
            client_request_id: "r-1".into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected the routine");
    };
    assert_eq!(format_time(routine.due_at), "2026-03-07T14:00:00Z");

    for (due, next) in [
        ("2026-03-07T14:00:00Z", "2026-03-08T13:00:00Z"),
        ("2026-03-08T13:00:00Z", "2026-03-09T13:00:00Z"),
    ] {
        let timer = sim.db().timer(routine.id).await.unwrap().unwrap();
        assert_eq!(format_time(timer.due_at), due);
        sim.clock().set(timer.due_at - Duration::from_secs(60));
        assert_eq!(
            sim.core.schedule_once().await.unwrap(),
            0,
            "not before {due}"
        );
        sim.clock().set(timer.due_at);
        assert_eq!(sim.core.schedule_once().await.unwrap(), 1, "at {due}");
        sim.work_until_idle().await.unwrap();
        let timer = sim.db().timer(routine.id).await.unwrap().unwrap();
        assert_eq!(format_time(timer.due_at), next);
        assert_eq!(timer.status, TimerStatus::Active);
    }
    let events = fired(&sim).await;
    assert_eq!(events.len(), 2, "once a day");
    assert!(events.iter().all(|event| event.requested), "your routine");
    let received = sim.backend().received();
    assert_eq!(received.len(), 2);
    assert!(
        received[1].prompt.contains(
            "Routine 1 fired. Set by the user; due at 2026-03-08T13:00:00Z. Its instruction:\n\
             Post the morning summary."
        ),
        "{}",
        received[1].prompt
    );
    sim.dispatch().await.unwrap();
    assert_eq!(sim.feed().await.len(), 2);
    assert_quiet(&sim, "routine").await;
    assert_invariants(&sim, "routine").await;
}
