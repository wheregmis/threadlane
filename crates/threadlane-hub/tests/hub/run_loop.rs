//! `HubCore::run`: the sweep and dispatcher loops, driven by the fake clock.

use std::time::Duration;

use threadlane_core::store::OutboxStore;
use threadlane_core::time::Clock;
use threadlane_hub::CLIENT_CHANNEL;
use threadlane_testkit::FakeClock;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::common::{Reply, TestHub, eventually, play};

fn start(
    hub: &TestHub,
) -> (
    oneshot::Sender<()>,
    JoinHandle<Result<(), threadlane_hub::HubError>>,
) {
    let (stop, stopped) = oneshot::channel::<()>();
    let core = hub.core.clone();
    let task = tokio::spawn(async move {
        core.run(async {
            let _ = stopped.await;
        })
        .await
    });
    (stop, task)
}

/// Waits until both loops sleep on the clock, so moving it wakes them.
async fn both_loops_parked(clock: &FakeClock) {
    eventually("both loops sleep", || {
        let clock = clock.clone();
        async move { clock.sleepers() >= 2 }
    })
    .await;
}

#[tokio::test]
async fn the_sweep_releases_an_expired_lease_on_its_interval() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "hi").await;
    let (stop, task) = start(&hub);
    both_loops_parked(&hub.clock).await;
    let lease = hub.link.request_work().await.unwrap().expect("granted");

    hub.clock.advance(Duration::from_secs(30));
    both_loops_parked(&hub.clock).await;
    assert_eq!(
        hub.general_lane().await.lease_owner,
        Some(lease.machine_id),
        "a live lease survives the sweep"
    );

    hub.clock.advance(Duration::from_secs(30));
    let probe = hub.clone();
    eventually("the expired lease is released", || {
        let probe = probe.clone();
        async move { probe.general_lane().await.lease_owner.is_none() }
    })
    .await;

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_dispatcher_delivers_on_a_wake_without_waiting_for_the_interval() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let start_time = hub.clock.now();
    hub.send("m-1", "hi").await;
    let (stop, task) = start(&hub);
    both_loops_parked(&hub.clock).await;

    play(&*hub.link, Reply::Send(Some("toolu_1"), "hello"))
        .await
        .unwrap();
    let db = hub.db.clone();
    eventually("the reply is delivered", || {
        let db = db.clone();
        async move {
            !db.delivered_after(CLIENT_CHANNEL, 0, 10)
                .await
                .unwrap()
                .is_empty()
        }
    })
    .await;
    assert_eq!(hub.clock.now(), start_time, "no poll interval passed");

    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
