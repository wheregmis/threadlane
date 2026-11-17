//! The in-process [`HubLink`] (docs/milestone-1.md §1, §3, §9).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::ids::{DecisionId, MachineId, RunId, SessionId};
use threadlane_core::link::{Begin, HubLink, HubMessage, LinkError, Perform, RunFailure};
use threadlane_core::model::{
    BackendKind, Event, JournalEntry, Lane, Lease, MachineReport, RunOutcome,
};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{InboxStore, MachineStore, SessionStore, StoreError};
use threadlane_core::time::Timestamp;
use threadlane_core::tools::ToolError;

use crate::core::HubCore;
use crate::error::link_error;
use crate::wake::Mailbox;

/// A runner's link to an in-process hub. One per runner.
#[derive(Debug)]
pub(crate) struct LocalLink {
    pub(crate) core: HubCore,
    pub(crate) machine: MachineId,
    mailbox: Arc<Mailbox>,
}

impl LocalLink {
    pub(crate) fn new(core: HubCore, machine: MachineId) -> Self {
        let mailbox = core.inner.wake.subscribe();
        Self {
            core,
            machine,
            mailbox,
        }
    }

    /// The lane, if `lease` is its current lease and was granted to this
    /// link's machine. Writes check again inside their own transaction; this
    /// check keeps reads and unfenced writes from acting on a stale lease.
    pub(crate) async fn check_lease(&self, lease: &Lease) -> Result<Lane, LinkError> {
        if lease.machine_id != self.machine {
            return Err(LinkError::Rejected(format!(
                "lease on lane {} belongs to machine {}, not {}",
                lease.lane_id, lease.machine_id, self.machine
            )));
        }
        let lane = self
            .core
            .inner
            .db
            .lane(lease.lane_id)
            .await
            .map_err(link_error)?
            .ok_or_else(|| LinkError::Rejected(format!("lane {} not found", lease.lane_id)))?;
        if lane.lease_epoch != lease.epoch || lane.lease_owner != Some(lease.machine_id) {
            return Err(LinkError::StaleLease {
                lane_id: lease.lane_id,
            });
        }
        Ok(lane)
    }
}

#[async_trait]
impl HubLink for LocalLink {
    async fn hello(&self, report: &MachineReport) -> Result<(), LinkError> {
        let inner = &self.core.inner;
        inner
            .db
            .report(self.machine, report, inner.clock.now())
            .await
            .map_err(link_error)
    }

    async fn heartbeat(
        &self,
        report: &MachineReport,
        leases: &[Lease],
    ) -> Result<Vec<Lease>, LinkError> {
        let inner = &self.core.inner;
        inner
            .db
            .report(self.machine, report, inner.clock.now())
            .await
            .map_err(link_error)?;
        let mut renewed = Vec::with_capacity(leases.len());
        for lease in leases {
            if lease.machine_id != self.machine {
                continue;
            }
            match inner
                .db
                .renew_lease(lease, inner.clock.now(), inner.settings.lease_ttl)
                .await
            {
                Ok(lease) => renewed.push(lease),
                Err(StoreError::StaleLease { .. }) => {}
                Err(error) => return Err(link_error(error)),
            }
        }
        Ok(renewed)
    }

    async fn request_work(&self) -> Result<Option<Lease>, LinkError> {
        self.place().await
    }

    async fn pending(&self, lease: &Lease) -> Result<Vec<Event>, LinkError> {
        self.check_lease(lease).await?;
        self.core
            .inner
            .db
            .pending_events(lease.lane_id)
            .await
            .map_err(link_error)
    }

    async fn begin_run(&self, lease: &Lease) -> Result<Begin, LinkError> {
        self.begin(lease).await
    }

    async fn native_session(
        &self,
        lease: &Lease,
        session_id: SessionId,
        native_session_id: &str,
    ) -> Result<(), LinkError> {
        self.check_lease(lease).await?;
        self.core
            .inner
            .db
            .set_native_session(lease, session_id, native_session_id)
            .await
            .map_err(link_error)
    }

    async fn append(&self, lease: &Lease, entries: &[JournalEntry]) -> Result<(), LinkError> {
        self.check_lease(lease).await?;
        self.core
            .inner
            .db
            .append_journal(lease, entries)
            .await
            .map_err(link_error)
    }

    async fn call_tool(
        &self,
        lease: &Lease,
        run_id: RunId,
        call_id: Option<&str>,
        tool: &str,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        self.gateway(lease, run_id, call_id, tool, arguments).await
    }

    async fn complete(
        &self,
        lease: &Lease,
        run_id: RunId,
        final_text: Option<&str>,
    ) -> Result<RunOutcome, LinkError> {
        self.commit(lease, run_id, final_text).await
    }

    async fn account(
        &self,
        lease: &Lease,
        backend: BackendKind,
        account_label: Option<&str>,
        resets_at: Option<Timestamp>,
    ) -> Result<(), LinkError> {
        self.usage_limit(lease, backend, account_label, resets_at)
            .await
    }

    async fn begin_perform(
        &self,
        lease: &Lease,
        run_id: RunId,
        decision: DecisionId,
    ) -> Result<Perform, LinkError> {
        LocalLink::begin_perform(self, lease, run_id, decision).await
    }

    async fn finish_perform(
        &self,
        lease: &Lease,
        run_id: RunId,
        decision: DecisionId,
        outcome: &Json,
    ) -> Result<(), LinkError> {
        LocalLink::finish_perform(self, lease, run_id, decision, outcome).await
    }

    async fn fail(
        &self,
        lease: &Lease,
        run_id: RunId,
        failure: &RunFailure,
    ) -> Result<(), LinkError> {
        self.fail_run(lease, run_id, failure).await
    }

    async fn release(&self, lease: &Lease) -> Result<(), LinkError> {
        if lease.machine_id != self.machine {
            return Err(LinkError::Rejected(format!(
                "lease on lane {} belongs to machine {}, not {}",
                lease.lane_id, lease.machine_id, self.machine
            )));
        }
        let inner = &self.core.inner;
        inner.db.release_lease(lease).await.map_err(link_error)?;
        // Events that arrived after the last begin_run would otherwise wait
        // for the runners' next poll.
        if !inner
            .db
            .pending_events(lease.lane_id)
            .await
            .map_err(link_error)?
            .is_empty()
        {
            inner.wake.wake(WakeTarget::Lane(lease.lane_id));
        }
        Ok(())
    }

    async fn next_message(&self) -> Option<HubMessage> {
        Some(self.mailbox.next().await)
    }
}
