//! Ending a run: `complete` (Plan.md §4.3 step 5.6, §4.5) and `fail`
//! (Plan.md §4.9, docs/milestone-1.md §9 "fail").

use serde_json::json;
use threadlane_core::ids::RunId;
use threadlane_core::link::{LinkError, RunFailure};
use threadlane_core::model::{
    EventKind, LaneKind, Lease, NewOutboxMessage, OutboxKind, RetryPolicy, Run, RunOutcome,
    RunStatus, Trust, Urgency,
};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{DecisionStore, LeadReply, OutboxStore, SessionStore};

use crate::error::link_error;
use crate::link::LocalLink;
use crate::tools::{CLIENT_CHANNEL, STAY_SILENT, silent_reason};

/// What Claude Code itself returns when a turn ends with no text: a backend
/// placeholder, not the lead's words. Leads stay silent with stay_silent.
const FILLER: &str = "No response requested.";

/// Whether a lead's final text is worth delivering.
fn deliverable(text: &str) -> bool {
    !text.is_empty() && text != FILLER
}

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
        let mut reply = None;
        let events = db.run_events(run_id).await.map_err(link_error)?;
        if events.iter().any(|event| {
            event.source == threadlane_store::TASK_REVIEW_SOURCE
                && event.payload.get("submission").is_some()
        }) && !db.has_review_verdict(run_id).await.map_err(link_error)?
        {
            self.fail_run(
                lease,
                run_id,
                &RunFailure::Error {
                    message: "The Reviewer ended without submitting a verdict".to_owned(),
                },
            )
            .await?;
            return Ok(RunOutcome::Silent);
        }
        let dreaming = events.iter().any(crate::dreaming::is_dream);
        let silenced = db
            .finished_calls(run_id, STAY_SILENT)
            .await
            .map_err(link_error)?;
        // The last accepted call wins; a refused one (empty reason) does not count.
        let silent_reason = silenced.iter().rev().find_map(silent_reason);
        // Only a reply to the owner (a message or their answer to ask_user) is
        // delivered from final text; routine, GitHub and task wakes speak
        // through send_message alone (#604).
        let replying = events.iter().any(|event| {
            event.kind == EventKind::UserMessage
                || (event.kind == EventKind::DecisionAnswer && event.trust == Trust::User)
        });
        // A run that waited on you ended for your answer: its final text, or
        // the runner's stop notice, is a note for the run detail (#612).
        let replying = replying
            && lane.kind == LaneKind::Lead
            && !db
                .lane_decisions(lane.id)
                .await
                .map_err(link_error)?
                .iter()
                .any(|decision| decision.run_id == Some(run_id));
        let mut rationale = final_text;
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
        } else if let Some(reason) = silent_reason {
            rationale = Some(reason);
            RunOutcome::Silent
        } else if let Some(text) = final_text
            .map(str::trim)
            // A dream reviews memory; it has nothing to tell you.
            .filter(|text| replying && !dreaming && deliverable(text))
        {
            // A lead that just answers is heard, as if it had sent the text.
            let run = self.active_run(lease, run_id).await?;
            let message = NewOutboxMessage {
                project_id: Some(lane.project_id),
                lane_id: Some(lane.id),
                session_id: Some(run.session_id),
                run_id: Some(run_id),
                tool_call_id: Some(format!("final_text:{}", run_id.get())),
                kind: OutboxKind::Message,
                decision_id: None,
                channel: CLIENT_CHANNEL.to_owned(),
                body: json!({ "text": text }),
                urgency: Urgency::Normal,
                not_before: None,
                created_at: inner.clock.now(),
            };
            let route = crate::space_lead::reply_route(
                db,
                message.created_at,
                &events,
                lane.project_id,
                text,
            )
            .await
            .map_err(link_error)?;
            reply = Some(LeadReply { message, route });
            RunOutcome::Sent
        } else {
            RunOutcome::Silent
        };
        use threadlane_core::runtime::steps::dreaming as step;
        if dreaming {
            inner.hook.reach(step::BEFORE_MARKER)?;
        }
        db.commit_run(
            lease,
            run_id,
            outcome,
            rationale,
            reply.as_ref(),
            inner.clock.now(),
        )
        .await
        .map_err(link_error)?;
        if let Some(reply) = reply {
            inner.wake.wake(WakeTarget::Dispatcher);
            if let Some(route) = reply.route {
                inner.wake.wake(WakeTarget::Lane(route.lane_id));
            }
        }
        if dreaming {
            inner.hook.reach(step::AFTER_MARKER)?;
        }
        if lane.kind == LaneKind::Task {
            // A finished task's result waits in the lead lane, and its
            // dependents and its parallel slot may now be free.
            inner.wake.wake(WakeTarget::Runners);
            inner.wake.wake_reviews();
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
        let now = inner.clock.now();
        if *failure == RunFailure::Stopped {
            return db.stop_run(lease, run_id, now).await.map_err(link_error);
        }
        let run = self.active_run(lease, run_id).await?;
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
            // Retrying the same setup fails the same way: die on this attempt.
            RunFailure::Rejected { message } => (
                format!(
                    "the backend refused the run: {message}. Change the agent's model or backend, then retry"
                ),
                RetryPolicy {
                    count_attempt: true,
                    max_attempts: 1,
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
