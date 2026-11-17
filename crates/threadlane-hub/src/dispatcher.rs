//! The dispatcher (Plan.md §10.2) and the `client` channel.

use async_trait::async_trait;
use threadlane_core::adapters::{ChannelAdapter, Delivered, DeliveryError};
use threadlane_core::model::OutboxMessage;
use threadlane_core::runtime::steps::dispatcher as step;
use threadlane_core::store::{OutboxStore, StoreError};

use crate::core::HubCore;
use crate::error::HubError;
use crate::tools::CLIENT_CHANNEL;

/// Messages read per dispatcher pass.
const BATCH: u32 = 100;

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
        let inner = &self.inner;
        let messages = inner.db.deliverable(inner.clock.now(), BATCH).await?;
        if messages.is_empty() {
            return Ok(0);
        }
        inner.hook.reach(step::AFTER_READ)?;
        let mut sent = 0;
        for message in messages {
            let Some(channel) = inner
                .channels
                .iter()
                .find(|channel| channel.name() == message.channel)
            else {
                tracing::warn!(
                    outbox_id = %message.id,
                    channel = %message.channel,
                    "no adapter for this channel; leaving the message pending"
                );
                continue;
            };
            let delivered = match channel.deliver(&message).await {
                Ok(delivered) => delivered,
                Err(error) => {
                    // Milestone 1 has no failed/dead marking in the store;
                    // the message stays pending and is retried next pass.
                    tracing::warn!(outbox_id = %message.id, %error, "delivery failed");
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
            inner.wake.delivered();
            inner.hook.reach(step::AFTER_MARK_SENT)?;
            sent += 1;
        }
        Ok(sent)
    }
}
