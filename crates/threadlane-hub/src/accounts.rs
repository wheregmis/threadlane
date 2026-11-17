//! Usage limits (Plan.md §5.3): the `account` link call, and the system
//! notices when the whole space is blocked for lack of a model account and
//! when it resumes (Plan.md §10.2).

use serde_json::json;
use threadlane_core::backend::{account_label, free_backends, usable_backends};
use threadlane_core::link::LinkError;
use threadlane_core::model::{Account, BackendKind, Lease, MachineStatus};
use threadlane_core::runtime::steps::account as step;
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{MachineStore, OutboxStore};
use threadlane_core::time::Timestamp;

use crate::context::format_time;
use crate::core::HubCore;
use crate::error::{HubError, link_error};
use crate::link::LocalLink;

impl LocalLink {
    /// Cools the account down until it resets, or for the configured
    /// fallback when the backend did not say, then tells you if that left
    /// nothing in the space able to run.
    pub(crate) async fn usage_limit(
        &self,
        lease: &Lease,
        backend: BackendKind,
        reported: Option<&str>,
        resets_at: Option<Timestamp>,
    ) -> Result<(), LinkError> {
        let inner = &self.core.inner;
        self.check_lease(lease).await?;
        let now = inner.clock.now();
        let until = resets_at
            .filter(|at| *at > now)
            .unwrap_or(now + inner.settings.usage_limit_fallback);
        let label = account_label(backend, reported, self.machine);
        inner
            .db
            .cool_down(lease, &label, backend, until)
            .await
            .map_err(link_error)?;
        inner.hook.reach(step::AFTER_COOLDOWN)?;
        match self.core.notice_accounts().await {
            Ok(()) => Ok(()),
            Err(HubError::Killed(killed)) => Err(killed.into()),
            // The sweep tries again on its next pass.
            Err(error) => {
                tracing::warn!(%error, "could not check whether the space is blocked");
                Ok(())
            }
        }
    }
}

impl HubCore {
    /// Queues one system notice when no machine has a usable backend on a
    /// free account, saying when work should resume, and one more when a
    /// machine has one again. Called after every usage limit and on every
    /// sweep; the store keeps it to one notice per change.
    pub(crate) async fn notice_accounts(&self) -> Result<(), HubError> {
        let inner = &self.inner;
        let now = inner.clock.now();
        let accounts = inner.db.accounts().await?;
        let cooling = cooling(&accounts, now);
        let machines: Vec<_> = inner
            .db
            .machines()
            .await?
            .into_iter()
            .filter(|machine| {
                !matches!(
                    machine.status,
                    MachineStatus::Revoked | MachineStatus::Offline
                ) && !usable_backends(&machine.backends).is_empty()
            })
            .collect();
        if machines.is_empty() {
            // Nothing logged in anywhere is not a usage limit.
            return Ok(());
        }
        let blocked = machines
            .iter()
            .all(|machine| free_backends(&machine.backends, machine.id, &cooling).is_empty());
        let text = if blocked {
            let resumes = next_reset(&accounts, now).map_or_else(
                || "when a limit resets".to_owned(),
                |at| format!("at about {}", format_time(at)),
            );
            format!(
                "Every model account in this space has reached its usage limit, so no agent \
                 can run. Work resumes by itself {resumes}. Messages you send meanwhile are \
                 kept and answered then."
            )
        } else {
            "A model account is available again; work has resumed.".to_owned()
        };
        if inner
            .db
            .account_notice(blocked, &json!({ "text": text }), now)
            .await?
            .is_some()
        {
            inner.hook.reach(step::AFTER_NOTICE)?;
            inner.wake.wake(WakeTarget::Dispatcher);
        }
        Ok(())
    }

    /// When the next cooldown ends, if one is in progress.
    pub(crate) async fn next_reset(&self) -> Result<Option<Timestamp>, HubError> {
        let accounts = self.inner.db.accounts().await?;
        Ok(next_reset(&accounts, self.inner.clock.now()))
    }
}

/// The labels of the accounts cooling down at `now`.
pub(crate) fn cooling(accounts: &[Account], now: Timestamp) -> Vec<String> {
    accounts
        .iter()
        .filter(|account| account.cooldown_until.is_some_and(|until| until > now))
        .map(|account| account.label.clone())
        .collect()
}

fn next_reset(accounts: &[Account], now: Timestamp) -> Option<Timestamp> {
    accounts
        .iter()
        .filter_map(|account| account.cooldown_until)
        .filter(|until| *until > now)
        .min()
}
