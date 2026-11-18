//! Placement (Plan.md §5.4, reduced to one machine).

use std::time::Duration;

use threadlane_core::backend::Probe;
use threadlane_core::model::{MachineStatus, MachineTrust, NewMachine};
use threadlane_core::store::MachineStore;
use threadlane_core::time::Clock;

use crate::common::{TestHub, report_with};

#[tokio::test]
async fn nothing_is_granted_to_a_machine_without_a_usable_backend() {
    let hub = TestHub::start().await;
    hub.send("m-1", "hello").await;

    // Never reported anything.
    assert_eq!(hub.link.request_work().await.unwrap(), None);

    // Reported a backend that is installed but logged out.
    hub.link
        .hello(&report_with(Probe {
            installed: true,
            version: None,
            logged_in: false,
            account_label: None,
        }))
        .await
        .unwrap();
    assert_eq!(hub.link.request_work().await.unwrap(), None);

    // A machine that never said hello.
    let silent = hub
        .db
        .register_machine(&NewMachine {
            name: "laptop".into(),
            endpoint_id: "laptop-key".into(),
            is_hub: false,
            trust: MachineTrust::Trusted,
            max_parallel: 1,
        })
        .await
        .unwrap();
    assert_eq!(hub.core.link(silent.id).request_work().await.unwrap(), None);

    hub.hello().await;
    assert!(hub.link.request_work().await.unwrap().is_some());
}

#[tokio::test]
async fn draining_and_revoked_machines_get_nothing() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "hello").await;
    for status in [MachineStatus::Draining, MachineStatus::Revoked] {
        hub.db
            .set_machine_status(hub.machine, status)
            .await
            .unwrap();
        assert_eq!(hub.link.request_work().await.unwrap(), None, "{status}");
    }
    hub.db
        .set_machine_status(hub.machine, MachineStatus::Online)
        .await
        .unwrap();
    assert!(hub.link.request_work().await.unwrap().is_some());
}

#[tokio::test]
async fn nothing_is_granted_when_nothing_is_pending() {
    let hub = TestHub::start().await;
    hub.hello().await;
    assert_eq!(hub.link.request_work().await.unwrap(), None);
}

#[tokio::test]
async fn a_lane_is_granted_once_while_its_lease_lives() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "hello").await;
    let lease = hub.link.request_work().await.unwrap().expect("granted");
    assert_eq!(lease.lane_id, hub.general().await.lead_lane_id);
    assert_eq!(lease.machine_id, hub.machine);
    assert_eq!(lease.epoch, 1);
    assert_eq!(lease.expires_at, hub.clock.now() + Duration::from_secs(60));

    assert_eq!(hub.link.request_work().await.unwrap(), None);
    let (_, other) = hub.other_machine("laptop").await;
    assert_eq!(other.request_work().await.unwrap(), None);

    hub.clock.advance(Duration::from_secs(59));
    assert_eq!(other.request_work().await.unwrap(), None);
}
