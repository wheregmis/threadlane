//! Crash injection at every hub step boundary (Plan.md invariants 1, 3, 9;
//! docs/milestone-1.md §10 item 3, hub side).

use std::sync::Arc;

use serde_json::json;
use threadlane_core::model::EventStatus;
use threadlane_core::runtime::steps::{begin_run, dispatcher, placement, tool_call};
use threadlane_core::runtime::{NoHooks, Step};
use threadlane_hub::HubSettings;
use threadlane_testkit::CrashPlan;

use crate::common::{PAST_LEASE, Reply, Stop, TestHub, play};

const REPLY: Reply<'static> = Reply::Send(Some("toolu_1"), "Here is your answer.");

fn every_step() -> Vec<Step> {
    [
        placement::ALL,
        begin_run::ALL,
        tool_call::ALL,
        dispatcher::ALL,
    ]
    .concat()
}

/// One message, played to the end with a reply, then dispatched.
async fn cycle(hub: &TestHub) -> Result<(), Stop> {
    play(&*hub.link, REPLY).await?;
    hub.core.dispatch_once().await?;
    Ok(())
}

async fn crash_and_recover(step: Step) {
    let plan = CrashPlan::kill_at(step);
    let hub = TestHub::with(HubSettings::default(), Arc::new(plan.clone())).await;
    hub.hello().await;
    let event = hub.send("m-1", "Are you there?").await.event_id;

    let died = cycle(&hub).await;
    assert!(
        died.as_ref().is_err_and(Stop::is_killed),
        "{step}: the hub was killed, got {died:?}"
    );
    assert_eq!(
        plan.killed(),
        Some(step),
        "{step}: killed at the right step"
    );

    // The process is gone; its lease runs out; a new process starts.
    hub.clock.advance(PAST_LEASE);
    let fresh = hub.restart(Arc::new(NoHooks)).await;
    fresh.core.sweep_once().await.unwrap();
    fresh.hello().await;
    cycle(&fresh)
        .await
        .unwrap_or_else(|stop| panic!("{step}: recovery failed: {stop:?}"));

    let done = fresh.event(event).await;
    assert_eq!(done.status, EventStatus::Done, "{step}: the event is done");
    assert_eq!(
        fresh.committed_runs_with(event).await,
        1,
        "{step}: one committed run consumed it"
    );
    let feed = fresh.feed().await;
    assert_eq!(feed.len(), 1, "{step}: exactly one reply in the feed");
    assert_eq!(
        feed[0].message.body,
        json!({ "text": "Here is your answer." }),
        "{step}"
    );
    assert_eq!(
        fresh.general_lane().await.lease_owner,
        None,
        "{step}: no lease is left"
    );
}

#[tokio::test]
async fn every_hub_step_boundary_survives_a_crash() {
    let steps = every_step();
    assert_eq!(steps.len(), 12);
    for step in steps {
        crash_and_recover(step).await;
    }
}

#[tokio::test]
async fn an_uninterrupted_cycle_reaches_every_boundary_in_order() {
    let plan = CrashPlan::new();
    let hub = TestHub::with(HubSettings::default(), Arc::new(plan.clone())).await;
    hub.hello().await;
    hub.send("m-1", "Are you there?").await;
    cycle(&hub).await.unwrap();
    assert_eq!(
        plan.reached(),
        vec![
            placement::AFTER_SELECT,
            placement::AFTER_GRANT,
            begin_run::AFTER_READ_PENDING,
            begin_run::AFTER_INITIATIVE_CHECK,
            begin_run::AFTER_START_RUN,
            begin_run::AFTER_ASSEMBLE_CONTEXT,
            tool_call::AFTER_BEGIN,
            tool_call::AFTER_EFFECT,
            tool_call::AFTER_FINISH,
            // The re-check finds nothing pending.
            begin_run::AFTER_READ_PENDING,
            dispatcher::AFTER_READ,
            dispatcher::AFTER_DELIVER,
            dispatcher::AFTER_MARK_SENT,
        ]
    );
}
