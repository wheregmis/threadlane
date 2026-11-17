//! The long-running loop: `Runner::run` (docs/milestone-1.md §8).

use std::time::Duration;

use threadlane_core::backend::{AgentBackend, encode_backends};
use threadlane_core::model::{BackendKind, Lease};
use threadlane_core::runtime::Killed;
use threadlane_core::runtime::steps::runner;
use threadlane_runner::RunnerError;
use threadlane_testkit::{CrashPlan, Script, ScriptStep};

use crate::support::{
    Call, HEARTBEAT, Harness, LANE, POLL, assignment, eventually, is_fail, is_release,
    is_request_work, lease, run, settle,
};

const HOUR: Duration = Duration::from_secs(3_600);

/// Grants `lanes`, each with one run that hangs until it is stopped.
fn hanging_lanes(h: &Harness, lanes: &[i64]) {
    for (index, &lane) in lanes.iter().enumerate() {
        h.link.grant(lease(lane));
        let run_id = 11 + i64::try_from(index).expect("few lanes");
        h.link.push_begin(Ok(run(assignment(run_id))));
    }
    h.backend
        .scripted
        .set_default(Script::new().then(ScriptStep::Hang));
}

fn heartbeats(h: &Harness) -> Vec<Call> {
    h.calls()
        .into_iter()
        .filter(|call| matches!(call, Call::Heartbeat { .. }))
        .collect()
}

fn releases(h: &Harness) -> Vec<Lease> {
    h.calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Release { lease } => Some(lease),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn hello_reports_backends_capabilities_and_load() {
    let h = Harness::new();
    h.runner.hello().await.expect("hello");

    let probe = h.backend.scripted.probe().await.expect("probe");
    let reports = h.link.reports();
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_eq!(
        report.backends,
        encode_backends(&[(BackendKind::Claude, probe)])
    );
    assert_eq!(report.load, 0);
    assert!(!report.gh_login);
    assert_eq!(report.capabilities["os"], std::env::consts::OS);
    assert_eq!(report.capabilities["arch"], std::env::consts::ARCH);
    assert_eq!(h.calls(), vec![Call::Hello]);
}

#[tokio::test]
async fn a_nudge_triggers_a_claim() {
    let h = Harness::with_config(|config| {
        config.poll = HOUR;
        config.heartbeat = HOUR;
    });
    let (shutdown, task) = h.spawn_run();
    eventually("the first claim", || h.link.count(is_request_work) == 1).await;
    settle().await;
    assert_eq!(
        h.link.count(is_request_work),
        1,
        "no claim without a reason"
    );

    h.link.nudge();
    eventually("a claim after the nudge", || {
        h.link.count(is_request_work) == 2
    })
    .await;

    shutdown.send(()).expect("runner is running");
    task.await.expect("task").expect("run");
    assert_eq!(
        h.calls(),
        vec![Call::Hello, Call::RequestWork, Call::RequestWork]
    );
}

#[tokio::test]
async fn the_poll_interval_triggers_a_claim_without_a_nudge() {
    let h = Harness::with_config(|config| config.heartbeat = HOUR);
    let (shutdown, task) = h.spawn_run();
    eventually("the first claim and both timers", || {
        h.link.count(is_request_work) == 1 && h.clock.sleepers() == 2
    })
    .await;

    h.clock.advance(POLL - Duration::from_secs(1));
    settle().await;
    assert_eq!(h.link.count(is_request_work), 1);

    h.clock.advance(Duration::from_secs(1));
    eventually("a claim on the poll", || {
        h.link.count(is_request_work) == 2 && h.clock.sleepers() == 2
    })
    .await;

    h.clock.advance(POLL);
    eventually("a claim on the next poll", || {
        h.link.count(is_request_work) == 3
    })
    .await;

    shutdown.send(()).expect("runner is running");
    task.await.expect("task").expect("run");
}

#[tokio::test]
async fn heartbeats_carry_the_held_leases_and_load_and_renew_them() {
    let h = Harness::with_config(|config| config.poll = HOUR);
    hanging_lanes(&h, &[LANE]);
    let held = lease(LANE);
    let renewed = Lease {
        expires_at: held.expires_at + Duration::from_secs(60),
        ..held
    };
    h.link.push_heartbeat(Ok(vec![renewed]));

    let (shutdown, task) = h.spawn_run();
    // Heartbeat, poll, and the run's wall-time limit.
    eventually("the lane is running", || {
        h.link.journal().len() == 1 && h.clock.sleepers() == 3
    })
    .await;
    assert!(heartbeats(&h).is_empty());

    h.clock.advance(HEARTBEAT);
    eventually("the first heartbeat", || {
        heartbeats(&h).len() == 1 && h.clock.sleepers() == 3
    })
    .await;
    h.clock.advance(HEARTBEAT);
    eventually("the second heartbeat", || heartbeats(&h).len() == 2).await;

    assert_eq!(
        heartbeats(&h),
        vec![
            Call::Heartbeat {
                leases: vec![held],
                load: 1,
            },
            Call::Heartbeat {
                leases: vec![renewed],
                load: 1,
            },
        ]
    );
    let probe = h.backend.scripted.probe().await.expect("probe");
    let report = h.link.reports().pop().expect("reports");
    assert_eq!(
        report.backends,
        encode_backends(&[(BackendKind::Claude, probe)])
    );

    shutdown.send(()).expect("runner is running");
    task.await.expect("task").expect("run");
    assert_eq!(
        releases(&h),
        vec![renewed],
        "released with the renewed lease"
    );
}

#[tokio::test]
async fn a_heartbeat_that_drops_a_lease_stops_that_lane() {
    let h = Harness::with_config(|config| config.poll = HOUR);
    hanging_lanes(&h, &[3, 4]);
    h.link.push_heartbeat(Ok(vec![lease(4)]));

    let (shutdown, task) = h.spawn_run();
    eventually("both lanes are running", || {
        h.backend.scripted.received().len() == 2 && h.clock.sleepers() == 4
    })
    .await;

    h.clock.advance(HEARTBEAT);
    eventually("the dropped lane stops", || h.backend.stops() == 1).await;
    settle().await;
    assert_eq!(
        heartbeats(&h),
        vec![Call::Heartbeat {
            leases: vec![lease(3), lease(4)],
            load: 2,
        }]
    );
    assert!(!h.calls().iter().any(is_fail));
    assert!(!h.calls().iter().any(is_release));

    shutdown.send(()).expect("runner is running");
    task.await.expect("task").expect("run");
    assert_eq!(h.backend.stops(), 2);
    assert_eq!(
        releases(&h),
        vec![lease(4)],
        "the lost lease is never released"
    );
    assert!(!h.calls().iter().any(is_fail));
}

#[tokio::test]
async fn at_most_max_parallel_lanes_run_at_once() {
    let h = Harness::with_config(|config| config.poll = HOUR);
    hanging_lanes(&h, &[3, 4, 5]);
    h.link.push_heartbeat(Ok(vec![lease(4)]));

    let (shutdown, task) = h.spawn_run();
    eventually("two lanes are running", || {
        h.backend.scripted.received().len() == 2
    })
    .await;
    settle().await;
    assert_eq!(h.link.count(is_request_work), 2, "no claim while saturated");
    assert_eq!(h.backend.scripted.received().len(), 2);

    // Losing lane 3 frees a slot, which is filled at once.
    h.clock.advance(HEARTBEAT);
    eventually("the third lane runs", || {
        h.backend.scripted.received().len() == 3
    })
    .await;
    assert_eq!(h.backend.stops(), 1);
    assert_eq!(h.link.count(is_request_work), 3);

    shutdown.send(()).expect("runner is running");
    task.await.expect("task").expect("run");
    let mut released = releases(&h);
    released.sort_by_key(|lease| lease.lane_id);
    assert_eq!(released, vec![lease(4), lease(5)]);
}

#[tokio::test]
async fn shutdown_stops_running_backends_and_releases_their_leases() {
    let h = Harness::new();
    hanging_lanes(&h, &[3, 4]);

    let (shutdown, task) = h.spawn_run();
    eventually("both lanes are running", || {
        h.backend.scripted.received().len() == 2 && h.link.journal().len() == 2
    })
    .await;
    assert_eq!(h.backend.stops(), 0);

    shutdown.send(()).expect("runner is running");
    task.await.expect("task").expect("run");

    assert_eq!(h.backend.stops(), 2);
    let mut released = releases(&h);
    released.sort_by_key(|lease| lease.lane_id);
    assert_eq!(released, vec![lease(3), lease(4)]);
    assert!(
        !h.calls().iter().any(is_fail),
        "shutdown does not fail runs"
    );
}

#[tokio::test]
async fn a_kill_inside_a_lane_stops_the_runner() {
    let h = Harness::with_plan(CrashPlan::kill_at(runner::AFTER_CLAIM));
    h.link.grant(lease(LANE));

    let (_shutdown, task) = h.spawn_run();
    let result = task.await.expect("task");

    assert!(
        matches!(
            result,
            Err(RunnerError::Killed(Killed {
                step: runner::AFTER_CLAIM
            }))
        ),
        "{result:?}"
    );
    assert!(!h.calls().iter().any(is_release));
}
