//! A stale lease cannot append, call a tool, or complete (Plan.md §6.4,
//! invariant 10; docs/milestone-1.md §10 item 6).
//!
//! The real runner talks to the real hub through [`StaleBefore`], which lets
//! the runner's lease expire and another machine take the lane over just
//! before one chosen call, as if the runner had stalled there.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::ids::{DecisionId, RunId, SessionId};
use threadlane_core::link::{Begin, HubLink, HubMessage, LinkError, Perform, RunFailure};
use threadlane_core::model::{
    BackendKind, Event, EventStatus, JournalEntry, Lease, MachineReport, OutboxKind, RunOutcome,
    RunStatus,
};
use threadlane_core::time::Timestamp;
use threadlane_core::tools::ToolError;
use threadlane_hub::HubCore;
use threadlane_testkit::FakeClock;

use crate::harness::{PAST_LEASE, Sim, reply_script};
use crate::invariants::{assert_invariants, assert_quiet};

/// The call before which the lease goes stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Before {
    /// The append of the first journal entry of this type.
    Append(&'static str),
    CallTool,
    Complete,
}

#[derive(Debug)]
struct StaleBefore {
    inner: Arc<dyn HubLink>,
    before: Before,
    core: HubCore,
    clock: FakeClock,
    other: Arc<dyn HubLink>,
    /// The lease the other machine took, once it took it.
    taken: Mutex<Option<Lease>>,
}

impl StaleBefore {
    /// Expires the lease and lets the other machine take the lane, once.
    async fn at(&self, point: Before) {
        if point != self.before || self.taken.lock().unwrap().is_some() {
            return;
        }
        self.clock.advance(PAST_LEASE);
        self.core.sweep_once().await.unwrap();
        let lease = self
            .other
            .request_work()
            .await
            .unwrap()
            .expect("the other machine takes the lane over");
        *self.taken.lock().unwrap() = Some(lease);
    }

    fn taken(&self) -> Option<Lease> {
        *self.taken.lock().unwrap()
    }
}

#[async_trait]
impl HubLink for StaleBefore {
    async fn hello(&self, report: &MachineReport) -> Result<(), LinkError> {
        self.inner.hello(report).await
    }

    async fn heartbeat(
        &self,
        report: &MachineReport,
        leases: &[Lease],
    ) -> Result<Vec<Lease>, LinkError> {
        self.inner.heartbeat(report, leases).await
    }

    async fn request_work(&self) -> Result<Option<Lease>, LinkError> {
        self.inner.request_work().await
    }

    async fn pending(&self, lease: &Lease) -> Result<Vec<Event>, LinkError> {
        self.inner.pending(lease).await
    }

    async fn left_unwoken(&self, lease: &Lease) -> Result<bool, LinkError> {
        self.inner.left_unwoken(lease).await
    }

    async fn begin_run(&self, lease: &Lease) -> Result<Begin, LinkError> {
        self.inner.begin_run(lease).await
    }

    async fn native_session(
        &self,
        lease: &Lease,
        session_id: SessionId,
        native_session_id: &str,
    ) -> Result<(), LinkError> {
        self.inner
            .native_session(lease, session_id, native_session_id)
            .await
    }

    async fn append(&self, lease: &Lease, entries: &[JournalEntry]) -> Result<(), LinkError> {
        if let Before::Append(wanted) = self.before
            && entries
                .iter()
                .any(|entry| entry.entry.get("type").and_then(Json::as_str) == Some(wanted))
        {
            self.at(self.before).await;
        }
        self.inner.append(lease, entries).await
    }

    async fn call_tool(
        &self,
        lease: &Lease,
        run_id: RunId,
        call_id: Option<&str>,
        tool: &str,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        self.at(Before::CallTool).await;
        self.inner
            .call_tool(lease, run_id, call_id, tool, arguments)
            .await
    }

    async fn complete(
        &self,
        lease: &Lease,
        run_id: RunId,
        final_text: Option<&str>,
    ) -> Result<RunOutcome, LinkError> {
        self.at(Before::Complete).await;
        self.inner.complete(lease, run_id, final_text).await
    }

    async fn begin_perform(
        &self,
        lease: &Lease,
        run_id: RunId,
        decision: DecisionId,
    ) -> Result<Perform, LinkError> {
        self.inner.begin_perform(lease, run_id, decision).await
    }

    async fn finish_perform(
        &self,
        lease: &Lease,
        run_id: RunId,
        decision: DecisionId,
        outcome: &Json,
    ) -> Result<(), LinkError> {
        self.inner
            .finish_perform(lease, run_id, decision, outcome)
            .await
    }

    async fn fail(
        &self,
        lease: &Lease,
        run_id: RunId,
        failure: &RunFailure,
    ) -> Result<(), LinkError> {
        self.inner.fail(lease, run_id, failure).await
    }

    async fn account(
        &self,
        lease: &Lease,
        backend: BackendKind,
        account_label: Option<&str>,
        resets_at: Option<Timestamp>,
    ) -> Result<(), LinkError> {
        self.inner
            .account(lease, backend, account_label, resets_at)
            .await
    }

    async fn release(&self, lease: &Lease) -> Result<(), LinkError> {
        self.inner.release(lease).await
    }

    async fn next_message(&self) -> Option<HubMessage> {
        self.inner.next_message().await
    }
}

/// What the stale runner left behind.
struct Expect {
    /// The journal of the run that lost its lease.
    journal: &'static [&'static str],
    /// Outbox rows the run wrote while it still held the lease.
    rows: usize,
}

async fn stale_before(before: Before, expect: Expect) {
    let scenario = format!("stale before {before:?}");
    let sim = Sim::start().await;
    sim.backend().set_default(reply_script());
    let other = sim.other_machine("laptop").await;
    let stale = Arc::new(StaleBefore {
        inner: sim.core.link(sim.machine),
        before,
        core: sim.core.clone(),
        clock: sim.clock().clone(),
        other: Arc::clone(&other),
        taken: Mutex::new(None),
    });
    let stalling = sim
        .clone()
        .with_runner_link(Arc::clone(&stale) as Arc<dyn HubLink>);
    let event = sim.send("m-1", "Are you there?").await;

    assert!(
        stalling.work_once().await.unwrap(),
        "{scenario}: the runner abandons the lane and carries on"
    );
    let taken = stale.taken().expect("the lease went stale");
    assert_eq!(taken.epoch, 2, "{scenario}");

    // Nothing the stale runner tried after losing the lease took effect.
    let runs = sim.runs().await;
    assert_eq!(runs.len(), 1, "{scenario}");
    let run = &runs[0];
    assert_eq!(run.lease_epoch, 1);
    assert_eq!(
        run.status,
        RunStatus::Abandoned,
        "{scenario}: ended by the sweep, never committed or failed"
    );
    assert_eq!(
        sim.journal_types(run.id).await,
        expect.journal,
        "{scenario}"
    );
    let rows: Vec<_> = sim
        .outbox()
        .await
        .into_iter()
        .filter(|row| row.kind == OutboxKind::Message)
        .collect();
    assert_eq!(rows.len(), expect.rows, "{scenario}");
    let untouched = sim.event(event).await;
    assert_eq!(untouched.status, EventStatus::Pending, "{scenario}");
    assert_eq!(untouched.attempts, 0, "{scenario}");
    let lane = sim.general_lane().await;
    assert_eq!(
        lane.lease_epoch, taken.epoch,
        "{scenario}: the new holder's"
    );
    assert_eq!(lane.lease_owner, Some(taken.machine_id), "{scenario}");
    assert_eq!(lane.backoff_until, None, "{scenario}: nothing was failed");

    // The other machine gives the lane back; the hub's runner finishes.
    other.release(&taken).await.unwrap();
    assert_eq!(sim.work_until_idle().await.unwrap(), 1, "{scenario}");
    sim.dispatch().await.unwrap();
    assert_eq!(
        sim.event(event).await.status,
        EventStatus::Done,
        "{scenario}"
    );
    assert_eq!(sim.feed().await.len(), 1, "{scenario}: one reply");
    assert_quiet(&sim, &scenario).await;
    assert_invariants(&sim, &scenario).await;
}

#[tokio::test]
async fn a_stale_lease_cannot_append() {
    stale_before(
        Before::Append("text"),
        Expect {
            journal: &["session_started"],
            rows: 0,
        },
    )
    .await;
}

#[tokio::test]
async fn a_stale_lease_cannot_call_a_tool() {
    stale_before(
        Before::CallTool,
        Expect {
            journal: &["session_started", "text", "tool_use"],
            rows: 0,
        },
    )
    .await;
}

#[tokio::test]
async fn a_stale_lease_cannot_complete() {
    stale_before(
        Before::Complete,
        Expect {
            journal: &[
                "session_started",
                "text",
                "tool_use",
                "tool_result",
                "finished",
            ],
            // Sent while the lease was live; the run that commits adopts it.
            rows: 1,
        },
    )
    .await;
}
