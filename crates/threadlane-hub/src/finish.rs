//! Ending a run: `complete` (Plan.md §4.3 step 5.6, §4.5) and `fail`
//! (Plan.md §4.9, docs/milestone-1.md §9 "fail").

use threadlane_core::ids::RunId;
use threadlane_core::link::{LinkError, RunFailure};
use threadlane_core::model::{LaneKind, Lease, RetryPolicy, Run, RunOutcome, RunStatus};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{DecisionStore, OutboxStore, SessionStore};

use crate::error::link_error;
use crate::link::LocalLink;

impl LocalLink {
    /// Derives the outcome from what the run did and commits it.
    pub(crate) async fn commit(
        &self,
        lease: &Lease,
        run_id: RunId,
        final_text: Option<&str>,
    ) -> Result<RunOutcome, LinkError> {
        let inner = &self.core.inner;
        let db = &inner.db;
        let lane = self.check_lease(lease).await?;
        let outcome = if db.count_for_run(run_id).await.map_err(link_error)? > 0 {
            RunOutcome::Sent
        } else if db
            .pending_decisions(lane.id)
            .await
            .map_err(link_error)?
            .iter()
            .any(|decision| decision.run_id == Some(run_id))
        {
            RunOutcome::Deferred
        } else {
            RunOutcome::Silent
        };
        db.commit_run(lease, run_id, outcome, final_text, inner.clock.now())
            .await
            .map_err(link_error)?;
        if lane.kind == LaneKind::Task {
            // A finished task's result waits in the lead lane, and its
            // dependents and its parallel slot may now be free.
            inner.wake.wake(WakeTarget::Runners);
        }
        Ok(outcome)
    }

    pub(crate) async fn fail_run(
        &self,
        lease: &Lease,
        run_id: RunId,
        failure: &RunFailure,
    ) -> Result<(), LinkError> {
        let inner = &self.core.inner;
        let db = &inner.db;
        self.check_lease(lease).await?;
        let run = self.active_run(lease, run_id).await?;
        let now = inner.clock.now();
        let (error, retry) = match failure {
            RunFailure::SessionMissing => {
                db.close_session(run.session_id).await.map_err(link_error)?;
                (
                    "the backend no longer has the native session".to_owned(),
                    RetryPolicy {
                        count_attempt: false,
                        max_attempts: inner.settings.max_attempts,
                        backoff_until: now,
                    },
                )
            }
            // The account call cooled the account down; placement holds
            // the lane until one is free (Plan.md §5.3), so no backoff.
            RunFailure::UsageLimit => (
                "the account reached its usage limit".to_owned(),
                RetryPolicy {
                    count_attempt: false,
                    max_attempts: inner.settings.max_attempts,
                    backoff_until: now,
                },
            ),
            counted => {
                let error = match counted {
                    RunFailure::Error { message } => message.clone(),
                    RunFailure::LimitExceeded { message } => format!("limit exceeded: {message}"),
                    other => format!("{other:?}"),
                };
                let attempts = db
                    .run_events(run_id)
                    .await
                    .map_err(link_error)?
                    .iter()
                    .map(|event| event.attempts)
                    .max()
                    .unwrap_or(0);
                (
                    error,
                    RetryPolicy {
                        count_attempt: true,
                        max_attempts: inner.settings.max_attempts,
                        backoff_until: now + inner.settings.retry_backoff(attempts),
                    },
                )
            }
        };
        let failed = db
            .fail_run(lease, run_id, &error, retry, now)
            .await
            .map_err(link_error)?;
        if !failed.dead.is_empty() {
            // A notice was queued, or a failure event reached a lead lane.
            inner.wake.wake(WakeTarget::Dispatcher);
            inner.wake.wake(WakeTarget::Runners);
        }
        Ok(())
    }

    /// The run, if it is active under `lease`. Checked before any write that
    /// the store does not fence itself.
    pub(crate) async fn active_run(&self, lease: &Lease, run_id: RunId) -> Result<Run, LinkError> {
        self.core
            .inner
            .db
            .run(run_id)
            .await
            .map_err(link_error)?
            .filter(|run| {
                run.lane_id == lease.lane_id
                    && run.lease_epoch == lease.epoch
                    && run.status == RunStatus::Active
            })
            .ok_or_else(|| {
                LinkError::Rejected(format!("run {run_id} is not active under this lease"))
            })
    }
}
