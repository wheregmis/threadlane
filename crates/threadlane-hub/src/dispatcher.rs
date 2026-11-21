//! The dispatcher (Plan.md §10.2) and the `client` channel.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use threadlane_core::adapters::{ChannelAdapter, Delivered, DeliveryError};
use threadlane_core::ids::ProjectId;
use threadlane_core::model::{NewOutboxMessage, OutboxKind, OutboxMessage, Pacing, Urgency};
use threadlane_core::runtime::steps::dispatcher as step;
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{OrgStore, OutboxStore, StoreError};
use threadlane_core::time::Timestamp;

use crate::core::HubCore;
use crate::error::HubError;
use crate::pacing::Quiet;
use crate::tools::CLIENT_CHANNEL;

/// Messages read per dispatcher pass.
pub(crate) const BATCH: u32 = 100;
const HOUR: Duration = Duration::from_secs(3600);

/// The client channel. Delivering does nothing: being marked sent is what
/// puts a message in the client's feed (docs/milestone-1.md §9).
#[derive(Debug, Clone, Copy, Default)]
pub struct ClientChannel;

#[async_trait]
impl ChannelAdapter for ClientChannel {
    fn name(&self) -> &str {
        CLIENT_CHANNEL
    }

    async fn deliver(&self, _message: &OutboxMessage) -> Result<Delivered, DeliveryError> {
        Ok(Delivered {
            channel_message_id: None,
        })
    }
}

impl HubCore {
    /// Delivers everything deliverable now and returns how many messages
    /// were marked sent. Delivery is at least once: a crash between
    /// delivering and marking sends again.
    ///
    /// # Errors
    ///
    /// A store error, or [`HubError::Killed`] at an injected crash.
    pub async fn dispatch_once(&self) -> Result<u32, HubError> {
        Ok(self.dispatch_pass().await?.0)
    }

    pub(crate) async fn dispatch_pass(&self) -> Result<(u32, bool), HubError> {
        let inner = &self.inner;
        let _dispatch = inner.dispatch.lock().await;
        let pacing = inner.db.pacing().await?;
        let quiet = pacing
            .quiet
            .as_ref()
            .map(Quiet::new)
            .transpose()
            .map_err(StoreError::Corrupt)?;
        let now = inner.clock.now();
        // shortcut: paced passes read the due backlog together, page by channel and window if it outgrows memory.
        let limit = if pacing != Pacing::default() {
            u32::MAX
        } else {
            BATCH
        };
        let messages = inner.db.dispatchable(now, limit).await?;
        if messages.is_empty() {
            return Ok((0, false));
        }
        let full_batch = u32::try_from(messages.len()).unwrap_or(u32::MAX) >= limit;
        inner.hook.reach(step::AFTER_READ)?;
        let quiet_end = |at| {
            quiet
                .as_ref()
                .map(|quiet| quiet.end_at(at))
                .transpose()
                .map(Option::flatten)
                .map_err(StoreError::Corrupt)
        };
        let mut ready = Vec::new();
        let quiet_now = quiet_end(now)?;
        for (message, hold) in messages {
            if !message.is_paced() {
                ready.push(Some((message, None, false)));
                continue;
            }
            if let Some(until) = quiet_now {
                self.hold(&message, until, "quiet").await?;
                continue;
            }
            let was_quiet = if hold.as_deref() == Some("quiet") {
                message.not_before
            } else {
                quiet_end(message.created_at)?
            };
            ready.push(Some((message, was_quiet, hold.as_deref() == Some("rate"))));
        }
        let mut sent = 0;
        let mut cached_projects: Option<HashMap<ProjectId, String>> = None;
        let mut cached_rate_hold: Option<Timestamp> = None;
        for index in 0..ready.len() {
            let Some((mut message, quiet_release, rate_held)) = ready[index].take() else {
                continue;
            };
            if message.is_paced() {
                let now = inner.clock.now();
                if let Some(until) = quiet_end(now)? {
                    self.hold(&message, until, "quiet").await?;
                    continue;
                }
                if message.attempts == 0 && !rate_held && !message.body["digest"].is_array() {
                    let window = pacing
                        .digest_minutes
                        .filter(|_| message.urgency == Urgency::Low)
                        .map(|minutes| {
                            message.created_at + Duration::from_secs(u64::from(minutes) * 60)
                        });
                    let matches = |first: &OutboxMessage,
                                   other: &OutboxMessage,
                                   other_quiet,
                                   other_rate: bool| {
                        other.is_paced()
                            && other.attempts == 0
                            && !other_rate
                            && !other.body["digest"].is_array()
                            && first.channel == other.channel
                            && if quiet_release.is_some() {
                                quiet_release == other_quiet
                            } else {
                                other_quiet.is_none()
                                    && other.urgency == Urgency::Low
                                    && window.is_some_and(|end| other.created_at < end)
                            }
                    };
                    let mut group = vec![message];
                    if quiet_release.is_some() || window.is_some() {
                        for candidate in &mut ready[index + 1..] {
                            if let Some((other, quiet, rate)) = candidate
                                && matches(&group[0], other, *quiet, *rate)
                                && let Some((original, _, _)) = candidate.take()
                            {
                                group.push(original);
                            }
                        }
                    }
                    if quiet_release.is_none()
                        && let Some(until) = window.filter(|until| *until > now)
                    {
                        for original in &group {
                            if original.not_before != Some(until) {
                                self.hold(original, until, "digest").await?;
                            }
                        }
                        continue;
                    }
                    message = if group.len() > 1 {
                        self.digest(&group, now, &mut cached_projects).await?
                    } else {
                        group.remove(0)
                    };
                }
                let until = match cached_rate_hold {
                    Some(until) => Some(until),
                    None => {
                        let hold = self.rate_hold(&pacing, inner.clock.now()).await?;
                        cached_rate_hold = hold;
                        hold
                    }
                };
                if let Some(until) = until {
                    self.hold(&message, until, "rate").await?;
                    continue;
                }
            }
            let delivered = match inner
                .channels
                .iter()
                .find(|channel| channel.name() == message.channel)
            {
                Some(channel) => channel.deliver(&message).await,
                None => Err(DeliveryError::Permanent(
                    "no adapter for this channel".to_owned(),
                )),
            };
            let delivered = match delivered {
                Ok(delivered) => delivered,
                Err(error) => {
                    let retry_at = (matches!(error, DeliveryError::Transient(_))
                        && message.attempts.saturating_add(1) < inner.settings.max_attempts)
                        .then(|| {
                            inner.clock.now() + inner.settings.retry_backoff(message.attempts)
                        });
                    let lane = inner
                        .db
                        .delivery_failed(&message, &error.to_string(), inner.clock.now(), retry_at)
                        .await?;
                    inner.hook.reach(step::AFTER_FAILURE)?;
                    if let Some(lane) = lane {
                        inner.wake.wake(WakeTarget::Lane(lane));
                    }
                    continue;
                }
            };
            inner.hook.reach(step::AFTER_DELIVER)?;
            match inner
                .db
                .mark_sent(
                    message.id,
                    delivered.channel_message_id.as_deref(),
                    inner.clock.now(),
                )
                .await
            {
                Ok(()) => {}
                // Someone else marked it in the meantime.
                Err(StoreError::Conflict(reason)) => {
                    tracing::debug!(outbox_id = %message.id, %reason, "already marked");
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
            if message.channel == CLIENT_CHANNEL {
                inner.wake.delivered();
            }
            inner.hook.reach(step::AFTER_MARK_SENT)?;
            sent += 1;
        }
        Ok((sent, full_batch))
    }

    async fn rate_hold(
        &self,
        pacing: &Pacing,
        now: Timestamp,
    ) -> Result<Option<Timestamp>, HubError> {
        let Some(rate) = pacing.rate else {
            return Ok(None);
        };
        let since = now - HOUR;
        Ok(self
            .inner
            .db
            .rate_boundary(since, rate)
            .await?
            .map(|at| at + HOUR))
    }

    async fn hold(
        &self,
        message: &OutboxMessage,
        until: Timestamp,
        reason: &str,
    ) -> Result<(), HubError> {
        self.inner
            .db
            .hold_delivery(message.id, until, reason)
            .await?;
        Ok(self.inner.hook.reach(step::AFTER_HOLD)?)
    }

    async fn digest(
        &self,
        originals: &[OutboxMessage],
        now: Timestamp,
        cached_projects: &mut Option<HashMap<ProjectId, String>>,
    ) -> Result<OutboxMessage, HubError> {
        let inner = &self.inner;
        let projects = match cached_projects {
            Some(projects) => projects,
            None => {
                let loaded = inner
                    .db
                    .projects()
                    .await?
                    .into_iter()
                    .map(|project| (project.id, project.name))
                    .collect();
                cached_projects.insert(loaded)
            }
        };
        let mut groups = Vec::<(_, Vec<&OutboxMessage>)>::new();
        for message in originals {
            if let Some((_, entries)) = groups
                .iter_mut()
                .find(|(project, _)| *project == message.project_id)
            {
                entries.push(message);
            } else {
                groups.push((message.project_id, vec![message]));
            }
        }
        let mut text = String::from("Digest");
        for (project, messages) in groups {
            let name = project
                .and_then(|id| projects.get(&id))
                .map_or("Space", String::as_str);
            let _ = write!(text, "\n\n{name}");
            for message in messages {
                let body = &message.body;
                let _ = write!(
                    text,
                    "\n- {}",
                    body["text"]
                        .as_str()
                        .map_or_else(|| body.to_string(), str::to_owned)
                );
            }
        }
        let [first, ..] = originals else {
            return Err(StoreError::Invalid("empty digest".to_owned()).into());
        };
        let ids: Vec<_> = originals.iter().map(|original| original.id).collect();
        let digest = inner
            .db
            .queue_digest(
                &NewOutboxMessage {
                    project_id: None,
                    lane_id: None,
                    session_id: None,
                    run_id: None,
                    tool_call_id: Some(format!("digest:{}", first.id)),
                    kind: OutboxKind::Message,
                    decision_id: None,
                    channel: first.channel.clone(),
                    body: json!({ "text": text, "digest": ids }),
                    urgency: Urgency::Normal,
                    not_before: None,
                    created_at: now,
                },
                &ids,
            )
            .await?;
        inner.hook.reach(step::AFTER_DIGEST)?;
        Ok(digest)
    }
}
