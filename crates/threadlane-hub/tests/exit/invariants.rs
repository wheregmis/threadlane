//! Plan.md §13 invariants as assertions on the stores, called at the end of
//! every scenario. Invariant 10 has its own scenarios (`fencing`); 9 is
//! invariant 1 plus the feed checks of each scenario.

use std::collections::{HashMap, HashSet};

use serde_json::Value as Json;
use threadlane_core::ids::{EventId, LaneId, SessionId};
use threadlane_core::model::{EventStatus, OutboxKind, RunStatus};
use threadlane_core::store::SessionStore;
use threadlane_hub::SEND_MESSAGE;

use crate::harness::Sim;

/// Every invariant this milestone covers that can be read from the stores.
pub async fn assert_invariants(sim: &Sim, scenario: &str) {
    assert_every_event_finished(sim, scenario).await;
    assert_one_active_run_per_lane(sim, scenario).await;
    assert_no_event_in_two_committed_runs(sim, scenario).await;
    assert_one_outbox_row_per_send_message_call(sim, scenario).await;
}

/// The end of a scenario: nothing holds a lease and no run is active.
pub async fn assert_quiet(sim: &Sim, scenario: &str) {
    for lane in sim.lanes().await {
        assert_eq!(
            lane.lease_owner, None,
            "{scenario}: lane {} still holds a lease",
            lane.id
        );
    }
    let active: Vec<_> = sim
        .runs()
        .await
        .into_iter()
        .filter(|run| run.status == RunStatus::Active)
        .map(|run| run.id)
        .collect();
    assert!(
        active.is_empty(),
        "{scenario}: runs left active: {active:?}"
    );
}

/// Invariant 1: every accepted event became `done`, `skipped`, or `dead`.
pub async fn assert_every_event_finished(sim: &Sim, scenario: &str) {
    for event in sim.events().await {
        assert!(
            matches!(
                event.status,
                EventStatus::Done | EventStatus::Skipped | EventStatus::Dead
            ),
            "{scenario}: invariant 1: event {} is {}",
            event.id,
            event.status
        );
    }
}

/// Invariant 2: at most one run is active per lane.
pub async fn assert_one_active_run_per_lane(sim: &Sim, scenario: &str) {
    let mut active: HashMap<LaneId, u32> = HashMap::new();
    for run in sim.runs().await {
        if run.status == RunStatus::Active {
            *active.entry(run.lane_id).or_default() += 1;
        }
    }
    for (lane, runs) in active {
        assert!(
            runs <= 1,
            "{scenario}: invariant 2: lane {lane} has {runs} active runs"
        );
    }
}

/// Invariant 3: no event is consumed by more than one committed run.
pub async fn assert_no_event_in_two_committed_runs(sim: &Sim, scenario: &str) {
    let mut consumed: HashMap<EventId, u32> = HashMap::new();
    for run in sim.runs().await {
        if run.status != RunStatus::Committed {
            continue;
        }
        for event in sim.db().run_events(run.id).await.unwrap() {
            *consumed.entry(event.id).or_default() += 1;
        }
    }
    for (event, runs) in consumed {
        assert_eq!(
            runs, 1,
            "{scenario}: invariant 3: event {event} was consumed by {runs} committed runs"
        );
    }
}

/// Invariants 5 and 6: one `send_message` call produced at most one outbox
/// row, however often a crash made the backend repeat it.
///
/// A call is what the mirrored journals show: a call id within a backend
/// session (a resumed session repeats its ids), or, without an id, each
/// `tool_use` entry on its own. So a session's message rows must not
/// outnumber its calls, and no two rows may share an idempotency key.
pub async fn assert_one_outbox_row_per_send_message_call(sim: &Sim, scenario: &str) {
    let mut ids: HashSet<(SessionId, String)> = HashSet::new();
    let mut calls: HashMap<SessionId, u32> = HashMap::new();
    for run in sim.runs().await {
        for entry in sim.journal(run.id).await {
            let entry = &entry.entry;
            if entry.get("type").and_then(Json::as_str) != Some("tool_use")
                || entry.get("name").and_then(Json::as_str) != Some(SEND_MESSAGE)
            {
                continue;
            }
            let new_call = match entry.get("call_id").and_then(Json::as_str) {
                Some(id) => ids.insert((run.session_id, id.to_owned())),
                None => true,
            };
            if new_call {
                *calls.entry(run.session_id).or_default() += 1;
            }
        }
    }
    let mut rows: HashMap<SessionId, u32> = HashMap::new();
    let mut keys: HashSet<String> = HashSet::new();
    for row in sim.outbox().await {
        if row.kind != OutboxKind::Message {
            continue;
        }
        let key = row
            .tool_call_id
            .clone()
            .unwrap_or_else(|| panic!("{scenario}: message {} names no call", row.id));
        assert!(
            keys.insert(key.clone()),
            "{scenario}: invariant 6: two rows under key {key}"
        );
        let session = row.session_id.expect("a message from a run has a session");
        *rows.entry(session).or_default() += 1;
    }
    for (session, rows) in rows {
        let calls = calls.get(&session).copied().unwrap_or(0);
        assert!(
            rows <= calls,
            "{scenario}: invariant 6: session {session} has {rows} messages from {calls} send_message calls"
        );
    }
}
