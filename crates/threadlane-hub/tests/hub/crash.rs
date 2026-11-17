//! Crash injection at every hub step boundary (Plan.md invariants 1, 3, 9;
//! docs/milestone-1.md §10 item 3, hub side).

use std::sync::Arc;

use serde_json::json;
use threadlane_core::ids::DecisionId;
use threadlane_core::ids::EventId;
use threadlane_core::link::Begin;
use threadlane_core::link::Perform;
use threadlane_core::model::{EventKind, EventStatus, LaneKind, TaskStatus};
use threadlane_core::runtime::steps::{begin_run, dispatcher, perform, placement, tool_call};
use threadlane_core::runtime::{NoHooks, Step};
use threadlane_core::store::{InboxStore, TaskStore};
use threadlane_hub::{CREATE_TASK, FINISH_TASK, HubSettings, SEND_MESSAGE};
use threadlane_testkit::CrashPlan;

use crate::common::{PAST_LEASE, RATIONALE, Reply, Stop, TestHub, begin, play};
use crate::decisions::approved;

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

/// What the lead tells you once the task is done.
const RESULT: &str = "The page is up.";

/// One message, played to the end through a task: the lead creates one task,
/// the task finishes it, and the lead reports its result.
async fn task_cycle(hub: &TestHub) -> Result<(), Stop> {
    while let Some(lease) = hub.link.request_work().await? {
        while let Begin::Run(run) = hub.link.begin_run(&lease).await? {
            let (call_id, tool, arguments) = match run.lane_kind {
                LaneKind::Task => ("toolu_f", FINISH_TASK, json!({ "summary": "Done." })),
                LaneKind::Lead if run.prompt.contains("task_result") => {
                    ("toolu_s", SEND_MESSAGE, json!({ "text": RESULT }))
                }
                LaneKind::Lead => (
                    "toolu_c",
                    CREATE_TASK,
                    json!({ "title": "page", "goal": "the page is up" }),
                ),
            };
            hub.link
                .call_tool(&lease, run.run_id, Some(call_id), tool, &arguments)
                .await?;
            hub.link
                .complete(&lease, run.run_id, Some(RATIONALE))
                .await?;
        }
        hub.link.release(&lease).await?;
    }
    hub.core.dispatch_once().await?;
    Ok(())
}

async fn crash_task_cycle_and_recover(step: Step, nth: u32) {
    let scenario = format!("{step} #{nth}");
    let plan = CrashPlan::kill_at_nth(step, nth);
    let hub = TestHub::with(HubSettings::default(), Arc::new(plan.clone())).await;
    hub.hello().await;
    hub.send("m-1", "Put the page up").await;

    let died = task_cycle(&hub).await;
    assert!(
        died.as_ref().is_err_and(Stop::is_killed),
        "{scenario}: the hub was killed, got {died:?}"
    );
    assert_eq!(plan.killed(), Some(step), "{scenario}");

    hub.clock.advance(PAST_LEASE);
    let fresh = hub.restart(Arc::new(NoHooks)).await;
    fresh.core.sweep_once().await.unwrap();
    fresh.hello().await;
    task_cycle(&fresh)
        .await
        .unwrap_or_else(|stop| panic!("{scenario}: recovery failed: {stop:?}"));

    let tasks = fresh.db.tasks(fresh.general().await.id).await.unwrap();
    assert_eq!(tasks.len(), 1, "{scenario}: one task");
    assert_eq!(tasks[0].status, TaskStatus::Done, "{scenario}");
    let mut results = 0;
    for id in 1.. {
        let Some(event) = fresh.db.event(EventId(id)).await.unwrap() else {
            break;
        };
        assert_eq!(event.status, EventStatus::Done, "{scenario}: event {id}");
        results += usize::from(event.kind == EventKind::TaskResult);
    }
    assert_eq!(results, 1, "{scenario}: one task result");
    let feed = fresh.feed().await;
    assert_eq!(feed.len(), 1, "{scenario}: exactly one message in the feed");
    assert_eq!(
        feed[0].message.body,
        json!({ "text": RESULT }),
        "{scenario}"
    );
    for lane in [fresh.general().await.lead_lane_id, tasks[0].lane_id] {
        let lane = fresh.db.lane(lane).await.unwrap().unwrap();
        assert_eq!(
            lane.lease_owner, None,
            "{scenario}: lane {} is released",
            lane.id
        );
    }
}

#[tokio::test]
async fn every_hub_step_of_a_task_cycle_survives_a_crash() {
    let plan = CrashPlan::new();
    let hub = TestHub::with(HubSettings::default(), Arc::new(plan.clone())).await;
    hub.hello().await;
    hub.send("m-1", "Put the page up").await;
    task_cycle(&hub).await.unwrap();
    let reached = plan.reached();
    let runs = reached
        .iter()
        .filter(|step| **step == begin_run::AFTER_START_RUN)
        .count();
    assert_eq!(runs, 3, "the lead's, the task's, and the lead's again");
    for step in every_step() {
        assert!(reached.contains(&step), "the cycle reaches {step}");
    }
    // Every time a boundary is passed: in the lead's runs, the task's run,
    // and each of their tool calls.
    for (index, &step) in reached.iter().enumerate() {
        let nth = reached[..=index]
            .iter()
            .filter(|seen| **seen == step)
            .count();
        crash_task_cycle_and_recover(step, u32::try_from(nth).unwrap()).await;
    }
}

/// The hub's own boundaries in performing a stored call you approved: the
/// intent and the outcome are recorded durably, so a hub killed right after
/// either still knows it on restart, and the next run neither loses the
/// outcome nor repeats the call blindly (Plan.md §9.4, invariant 5).
#[tokio::test]
async fn killing_the_hub_after_it_records_a_performed_call_keeps_the_record() {
    let hub_steps = [perform::AFTER_BEGIN, perform::AFTER_FINISH];
    assert!(hub_steps.iter().all(|step| perform::ALL.contains(step)));
    let outcome = json!({ "performed": true, "branch": "threadlane/T-0001" });
    for step in hub_steps {
        let plan = CrashPlan::kill_at(step);
        let hub = approved(Arc::new(plan.clone())).await;
        let (lease, run) = begin(&*hub.link).await;
        assert_eq!(run.perform, [DecisionId(1)], "{step}");
        let begun = hub
            .link
            .begin_perform(&lease, run.run_id, DecisionId(1))
            .await;
        if step == perform::AFTER_FINISH {
            assert!(
                matches!(begun, Ok(Perform::Call { retry: false, .. })),
                "{begun:?}"
            );
            let finished = hub
                .link
                .finish_perform(&lease, run.run_id, DecisionId(1), &outcome)
                .await;
            assert!(Stop::from(finished.unwrap_err()).is_killed(), "{step}");
        } else {
            assert!(Stop::from(begun.unwrap_err()).is_killed(), "{step}");
        }
        assert_eq!(plan.killed(), Some(step));

        hub.clock.advance(PAST_LEASE);
        let fresh = hub.restart(Arc::new(NoHooks)).await;
        fresh.core.sweep_once().await.unwrap();
        fresh.hello().await;
        let (lease, run) = begin(&*fresh.link).await;
        assert_eq!(
            run.perform,
            [DecisionId(1)],
            "{step}: the answer is still to be acted on"
        );
        let again = fresh
            .link
            .begin_perform(&lease, run.run_id, DecisionId(1))
            .await
            .unwrap();
        if step == perform::AFTER_FINISH {
            assert_eq!(
                again,
                Perform::Recorded {
                    outcome: outcome.clone()
                },
                "{step}"
            );
        } else {
            assert!(
                matches!(again, Perform::Call { retry: true, .. }),
                "{step}: the intent survived, so the runner checks before repeating: {again:?}"
            );
        }
    }
}
