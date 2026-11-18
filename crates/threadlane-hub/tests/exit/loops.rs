//! The long-running loops, `HubCore::run` and `Runner::run`, as spawned tasks
//! on the fake clock.

use std::collections::BTreeSet;
use std::time::Duration;

use threadlane_core::model::{EventStatus, NewProject, RunStatus};
use threadlane_core::runtime::steps::runner::AFTER_CLAIM;
use threadlane_core::store::OrgStore;
use threadlane_core::time::Clock;
use threadlane_hub::HubError;
use threadlane_runner::RunnerError;
use threadlane_testkit::CrashPlan;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::harness::{REPLY, Sim, World, count, join, reply_after, reply_script, until};
use crate::invariants::{assert_invariants, assert_one_active_run_per_lane, assert_quiet};

pub fn spawn_runner(sim: &Sim) -> (oneshot::Sender<()>, JoinHandle<Result<(), RunnerError>>) {
    let (stop, stopped) = oneshot::channel::<()>();
    let runner = sim.runner.clone();
    let task = tokio::spawn(async move {
        runner
            .run(async {
                let _ = stopped.await;
            })
            .await
    });
    (stop, task)
}

pub fn spawn_hub(sim: &Sim) -> (oneshot::Sender<()>, JoinHandle<Result<(), HubError>>) {
    let (stop, stopped) = oneshot::channel::<()>();
    let core = sim.core.clone();
    let task = tokio::spawn(async move {
        core.run(async {
            let _ = stopped.await;
        })
        .await
    });
    (stop, task)
}

pub async fn sleepers_at_least(sim: &Sim, what: &str, sleepers: usize) {
    let clock = sim.clock().clone();
    until(what, || std::future::ready(clock.sleepers() >= sleepers)).await;
}

#[tokio::test]
async fn two_projects_run_in_two_lanes_at_the_same_time() {
    let world = World::new().await;
    world
        .db
        .create_project(&NewProject {
            name: "Alpha".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let sim = world.boot(CrashPlan::new()).await;
    assert_eq!(sim.world.runner_config.max_parallel, 2);
    // Both runs wait on the clock before replying, so both are in progress
    // together.
    sim.backend()
        .set_default(reply_after(Duration::from_secs(10)));
    let general = sim.send("g-1", "General question").await;
    let alpha = sim.send_to(Some("Alpha"), "a-1", "Alpha question").await;

    let (stop, task) = spawn_runner(&sim);
    // Both lanes claimed and debouncing, besides the heartbeat and the poll.
    let plan = sim.plan.clone();
    let clock = sim.clock().clone();
    until("both lanes are claimed and debouncing", || {
        std::future::ready(count(&plan, AFTER_CLAIM) == 2 && clock.sleepers() >= 4)
    })
    .await;
    sim.clock().advance(sim.world.runner_config.debounce);

    let backend = sim.backend().clone();
    until("both backends run", || {
        std::future::ready(backend.received().len() == 2)
    })
    .await;
    let db_sim = sim.clone();
    until("both runs are active", || {
        let sim = db_sim.clone();
        async move {
            sim.runs()
                .await
                .iter()
                .filter(|run| run.status == RunStatus::Active)
                .count()
                == 2
        }
    })
    .await;
    let active: BTreeSet<_> = sim
        .runs()
        .await
        .iter()
        .filter(|run| run.status == RunStatus::Active)
        .map(|run| run.lane_id)
        .collect();
    assert_eq!(active.len(), 2, "two lanes, one active run each");
    assert_one_active_run_per_lane(&sim, "two projects, in progress").await;

    sim.clock().advance(Duration::from_secs(10));
    let probe = sim.clone();
    until("both events are done and both lanes released", || {
        let sim = probe.clone();
        async move {
            sim.event(general).await.status == EventStatus::Done
                && sim.event(alpha).await.status == EventStatus::Done
                && sim
                    .lanes()
                    .await
                    .iter()
                    .all(|lane| lane.lease_owner.is_none())
        }
    })
    .await;
    stop.send(()).unwrap();
    join("the runner stops", task).await.unwrap();

    assert_eq!(sim.dispatch().await.unwrap(), 2);
    let projects: BTreeSet<_> = sim
        .feed()
        .await
        .into_iter()
        .map(|item| item.project_name.unwrap_or_default())
        .collect();
    assert_eq!(
        projects,
        BTreeSet::from(["Alpha".to_owned(), "General".to_owned()])
    );
    assert_quiet(&sim, "two projects").await;
    assert_invariants(&sim, "two projects").await;
}

#[tokio::test]
async fn the_long_running_loops_deliver_a_reply_to_a_waiting_feed() {
    let sim = Sim::start().await;
    sim.backend().set_default(reply_script());
    let (stop_hub, hub) = spawn_hub(&sim);
    let (stop_runner, runner) = spawn_runner(&sim);
    // The sweep, dispatcher and scheduler loops, the runner's heartbeat and
    // poll.
    sleepers_at_least(&sim, "every loop waits on the clock", 5).await;
    let start = sim.clock().now();

    let api = sim.api.clone();
    let feed = tokio::spawn(async move { api.feed(0, Duration::from_secs(60)).await });
    sleepers_at_least(&sim, "the feed waits", 6).await;

    let event = sim.send("m-1", "Are you there?").await;
    // The wake hint reaches the runner, which claims and debounces.
    let plan = sim.plan.clone();
    let clock = sim.clock().clone();
    until("the runner claims and debounces", || {
        std::future::ready(count(&plan, AFTER_CLAIM) == 1 && clock.sleepers() >= 7)
    })
    .await;
    sim.clock().advance(sim.world.runner_config.debounce);

    let items = join("the feed answers", feed).await.unwrap();
    assert_eq!(items.len(), 1, "the reply");
    assert_eq!(items[0].message.body["text"], REPLY);
    assert_eq!(
        sim.clock().now(),
        start + sim.world.runner_config.debounce,
        "delivered on the hints, not on a poll interval"
    );
    let probe = sim.clone();
    until("the lane is released", || {
        let sim = probe.clone();
        async move { sim.general_lane().await.lease_owner.is_none() }
    })
    .await;
    assert_eq!(sim.event(event).await.status, EventStatus::Done);

    stop_runner.send(()).unwrap();
    stop_hub.send(()).unwrap();
    join("the runner stops", runner).await.unwrap();
    join("the hub stops", hub).await.unwrap();
    assert_quiet(&sim, "long-running loops").await;
    assert_invariants(&sim, "long-running loops").await;
}
