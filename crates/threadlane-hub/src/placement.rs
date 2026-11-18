//! Placement, reduced to one machine for milestone 1 (Plan.md §5.4).

use threadlane_core::backend::usable_backends;
use threadlane_core::link::LinkError;
use threadlane_core::model::{Lease, MachineStatus};
use threadlane_core::runtime::steps::placement as step;
use threadlane_core::store::{InboxStore, MachineStore, StoreError};

use crate::error::link_error;
use crate::link::LocalLink;

impl LocalLink {
    /// Grants the first ready lane this machine may take, if any.
    pub(crate) async fn place(&self) -> Result<Option<Lease>, LinkError> {
        let inner = &self.core.inner;
        let machine = inner
            .db
            .machine(self.machine)
            .await
            .map_err(link_error)?
            .ok_or_else(|| LinkError::Rejected(format!("machine {} not found", self.machine)))?;
        if matches!(
            machine.status,
            MachineStatus::Revoked | MachineStatus::Draining
        ) || usable_backends(&machine.backends).is_empty()
        {
            return Ok(None);
        }
        let now = inner.clock.now();
        let lanes = inner.db.ready_lanes(now).await.map_err(link_error)?;
        if lanes.is_empty() {
            return Ok(None);
        }
        inner.hook.reach(step::AFTER_SELECT)?;
        for lane_id in lanes {
            let granted = match inner
                .db
                .grant_lease(lane_id, self.machine, now, inner.settings.lease_ttl)
                .await
            {
                Ok(granted) => granted,
                Err(StoreError::NotFound { kind: "lane", .. }) => continue,
                Err(error) => return Err(link_error(error)),
            };
            if let Some(lease) = granted {
                inner.hook.reach(step::AFTER_GRANT)?;
                return Ok(Some(lease));
            }
        }
        Ok(None)
    }
}
