//! [`OutboxStore`] on the hub database (Plan.md §10).

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::ids::{LaneId, OutboxId, ProjectId, RunId};
use threadlane_core::model::{
    EventKind, NewEvent, NewOutboxMessage, OutboxKind, OutboxMessage, OutboxStatus, Pacing, Trust,
    Urgency,
};
use threadlane_core::store::{OutboxStore, StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::inbox::insert_event_in;
use crate::row::{Cols, json_text, opt_raw, opt_ts, params};
use crate::session::NOTICE_CHANNEL;

/// Key prefix of the account notices: `system:account:<n>:blocked`, then
/// `system:account:<n>:resumed`, for the n-th time the space was blocked.
pub(crate) const ACCOUNT_NOTICE: &str = "system:account:";

pub(crate) const OUTBOX_FIELDS: &str =
    "id, project_id, lane_id, session_id, run_id, tool_call_id, kind,
    decision_id, channel, body, urgency, status, not_before, attempts, channel_message_id,
    created_at, sent_at, delivery_seq, trace_id";

pub(crate) fn decode_message(row: &Row) -> StoreResult<OutboxMessage> {
    let c = Cols(row);
    Ok(OutboxMessage {
        id: c.id(0)?,
        project_id: c.opt_id(1)?,
        lane_id: c.opt_id(2)?,
        session_id: c.opt_id(3)?,
        run_id: c.opt_id(4)?,
        tool_call_id: c.opt_text(5)?,
        kind: c.parse(6)?,
        decision_id: c.opt_id(7)?,
        channel: c.text(8)?,
        body: c.json(9)?,
        urgency: c.parse(10)?,
        status: c.parse(11)?,
        not_before: c.opt_ts(12)?,
        attempts: c.u32(13)?,
        channel_message_id: c.opt_text(14)?,
        created_at: c.ts(15)?,
        sent_at: c.opt_ts(16)?,
        delivery_seq: c.opt_i64(17)?,
        trace_id: c.opt_text(18)?,
    })
}

/// Hands an already queued message to the run that repeated the send, when
/// the run that first queued it never committed. The run that finally
/// commits is then the one recorded as having sent it, so its outcome is
/// `sent` (Plan.md §4.5) even though the row was written before a crash.
async fn adopt(
    conn: &Connection,
    existing: OutboxMessage,
    message: &NewOutboxMessage,
) -> StoreResult<OutboxMessage> {
    let (Some(new_run), Some(old_run)) = (message.run_id, existing.run_id) else {
        return Ok(existing);
    };
    if new_run == old_run {
        return Ok(existing);
    }
    let changed = query_opt(
        conn,
        &format!(
            "UPDATE outbox SET run_id = ?1, trace_id = (SELECT trace_id FROM runs WHERE id = ?1)
         WHERE id = ?2
           AND NOT EXISTS (SELECT 1 FROM runs WHERE id = ?3 AND status = 'committed')
         RETURNING {OUTBOX_FIELDS}"
        ),
        params![new_run.get(), existing.id.get(), old_run.get()],
        decode_message,
    )
    .await?;
    Ok(changed.unwrap_or(existing))
}

/// Queues a message inside the caller's transaction. A repeated
/// `tool_call_id` returns the original row and inserts nothing.
pub(crate) async fn enqueue_in(
    conn: &Connection,
    message: &NewOutboxMessage,
) -> StoreResult<OutboxMessage> {
    if let Some(call) = &message.tool_call_id {
        let existing = query_opt(
            conn,
            &format!("SELECT {OUTBOX_FIELDS} FROM outbox WHERE tool_call_id = ?1"),
            params![call.as_str()],
            decode_message,
        )
        .await?;
        if let Some(existing) = existing {
            return adopt(conn, existing, message).await;
        }
    }
    query_one(
        conn,
        &format!(
            "INSERT INTO outbox (id, project_id, lane_id, session_id, run_id, tool_call_id, kind,
                 decision_id, channel, body, urgency, status, not_before, created_at, trace_id)
             VALUES ({}, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11, ?12, (SELECT trace_id FROM runs WHERE id = ?4))
             RETURNING {OUTBOX_FIELDS}", crate::db::next_id_sql!("outbox")
        ),
        params![
            opt_raw(message.project_id, |id| id.get()),
            opt_raw(message.lane_id, |id| id.get()),
            opt_raw(message.session_id, |id| id.get()),
            opt_raw(message.run_id, |id| id.get()),
            message.tool_call_id.as_deref(),
            message.kind.as_str(),
            opt_raw(message.decision_id, |id| id.get()),
            message.channel.as_str(),
            json_text(&message.body)?,
            message.urgency.as_str(),
            opt_ts(message.not_before),
            message.created_at.as_millis(),
        ],
        decode_message,
    )
    .await
}

#[async_trait]
impl OutboxStore for HubDb {
    async fn enqueue(&self, message: &NewOutboxMessage) -> StoreResult<OutboxMessage> {
        let tx = self.db.begin().await?;
        let created = enqueue_in(tx.conn(), message).await?;
        tx.commit().await?;
        Ok(created)
    }

    async fn message(&self, id: OutboxId) -> StoreResult<Option<OutboxMessage>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {OUTBOX_FIELDS} FROM outbox WHERE id = ?1"),
            params![id.get()],
            decode_message,
        )
        .await
    }

    async fn deliverable(&self, now: Timestamp, limit: u32) -> StoreResult<Vec<OutboxMessage>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {OUTBOX_FIELDS} FROM outbox
                 WHERE status = 'pending' AND (not_before IS NULL OR not_before <= ?1)
                 ORDER BY id LIMIT ?2"
            ),
            params![now.as_millis(), i64::from(limit)],
            decode_message,
        )
        .await
    }

    async fn mark_sent(
        &self,
        id: OutboxId,
        channel_message_id: Option<&str>,
        now: Timestamp,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let delivery_seq = query_one(
            tx.conn(),
            "SELECT MAX(COALESCE((SELECT MAX(delivery_seq) FROM outbox), 0),
                 COALESCE((SELECT value FROM deletion_floors WHERE name = 'delivery_seq'), 0)) + 1",
            (),
            |row| Cols(row).i64(0),
        )
        .await?;
        let changed = exec(
            tx.conn(),
            "UPDATE outbox SET status = ?1, channel_message_id = ?2, sent_at = ?3,
                 attempts = attempts + 1, delivery_seq = ?4, pacing_hold = NULL
             WHERE id = ?5 AND status = 'pending'",
            params![
                OutboxStatus::Sent.as_str(),
                channel_message_id,
                now.as_millis(),
                delivery_seq,
                id.get()
            ],
        )
        .await?;
        if changed != 1 {
            return Err(StoreError::Conflict(format!(
                "outbox message {id} is not pending"
            )));
        }
        tx.commit().await
    }

    async fn recent_messages(
        &self,
        project_id: ProjectId,
        limit: u32,
    ) -> StoreResult<Vec<OutboxMessage>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {OUTBOX_FIELDS} FROM outbox
                 WHERE project_id = ?1 AND kind = 'message' AND (status = 'sent' OR (
                     status = 'suppressed' AND EXISTS (
                         SELECT 1 FROM outbox digest WHERE digest.id = outbox.digest_id AND digest.status = 'sent'
                     )
                 ))
                 ORDER BY id DESC LIMIT ?2"
            ),
            params![project_id.get(), i64::from(limit)],
            decode_message,
        )
        .await
    }

    async fn count_for_run(&self, run_id: RunId) -> StoreResult<u32> {
        let conn = self.db.reader()?;
        query_one(
            &conn,
            "SELECT COUNT(*) FROM outbox WHERE run_id = ?1 AND kind = ?2",
            params![run_id.get(), OutboxKind::Message.as_str()],
            |row| Cols(row).u32(0),
        )
        .await
    }

    async fn account_notice(
        &self,
        blocked: bool,
        body: &Json,
        now: Timestamp,
    ) -> StoreResult<Option<OutboxMessage>> {
        let tx = self.db.begin().await?;
        let last: Option<String> = query_opt(
            tx.conn(),
            "SELECT tool_call_id FROM outbox WHERE tool_call_id LIKE ?1 ORDER BY id DESC LIMIT 1",
            params![format!("{ACCOUNT_NOTICE}%")],
            |row| Cols(row).text(0),
        )
        .await?;
        let (episode, was_blocked) = match &last {
            None => (0, false),
            Some(key) => {
                let rest = key.strip_prefix(ACCOUNT_NOTICE).unwrap_or_default();
                let (episode, state) = rest.split_once(':').unwrap_or_default();
                let episode: u64 = episode
                    .parse()
                    .map_err(|_| StoreError::Corrupt(format!("account notice key {key:?}")))?;
                (episode, state == "blocked")
            }
        };
        let key = match (blocked, was_blocked) {
            (true, false) => format!("{ACCOUNT_NOTICE}{}:blocked", episode + 1),
            (false, true) => format!("{ACCOUNT_NOTICE}{episode}:resumed"),
            _ => return Ok(None),
        };
        let queued = enqueue_in(
            tx.conn(),
            &NewOutboxMessage {
                project_id: None,
                lane_id: None,
                session_id: None,
                run_id: None,
                tool_call_id: Some(key),
                kind: OutboxKind::SystemNotice,
                decision_id: None,
                channel: NOTICE_CHANNEL.to_owned(),
                body: body.clone(),
                urgency: Urgency::High,
                not_before: None,
                created_at: now,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(Some(queued))
    }

    async fn delivered_after(
        &self,
        channel: &str,
        after: i64,
        limit: u32,
    ) -> StoreResult<Vec<OutboxMessage>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {OUTBOX_FIELDS} FROM outbox
                 WHERE status = 'sent' AND channel = ?1 AND delivery_seq > ?2
                 ORDER BY delivery_seq LIMIT ?3"
            ),
            params![channel, after, i64::from(limit)],
            decode_message,
        )
        .await
    }

    async fn sent_before(
        &self,
        channel: &str,
        project_id: Option<ProjectId>,
        before: Option<Timestamp>,
        limit: u32,
    ) -> StoreResult<Vec<OutboxMessage>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {OUTBOX_FIELDS} FROM outbox
                 WHERE status = 'sent' AND channel = ?1 AND (
                     ?2 IS NULL OR project_id = ?2 OR EXISTS (
                         SELECT 1 FROM outbox original
                         WHERE original.digest_id = outbox.id AND original.project_id = ?2
                     )
                 )
                   AND sent_at < ?3
                 ORDER BY sent_at DESC, delivery_seq DESC LIMIT ?4"
            ),
            params![
                channel,
                project_id.map_or(turso::Value::Null, |id| turso::Value::Integer(id.get())),
                before.map_or(i64::MAX, Timestamp::as_millis),
                i64::from(limit)
            ],
            decode_message,
        )
        .await
    }
}

impl HubDb {
    /// Lists the suppressed originals that make up a digest, oldest first.
    pub async fn digest_originals(&self, digest: OutboxId) -> StoreResult<Vec<OutboxMessage>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {OUTBOX_FIELDS} FROM outbox
                      WHERE digest_id = ?1 AND status = 'suppressed' ORDER BY id"
            ),
            params![digest.get()],
            decode_message,
        )
        .await
    }

    /// Counts the original results held by pacing on a channel and reports their release times.
    pub async fn held_results(
        &self,
        channel: &str,
        now: Timestamp,
    ) -> StoreResult<Option<threadlane_core::client::HeldResults>> {
        let conn = self.db.reader()?;
        query_one(
            &conn,
            "SELECT COALESCE(SUM(MAX(1, (SELECT COUNT(*) FROM outbox original
                     WHERE original.digest_id = outbox.id))), 0),
                    MIN(not_before), MAX(not_before)
             FROM outbox
             WHERE status = 'pending' AND pacing_hold IS NOT NULL
               AND channel = ?1 AND not_before > ?2",
            params![channel, now.as_millis()],
            |row| {
                let c = Cols(row);
                let count = c.u32(0)?;
                Ok(c.opt_ts(1)?.zip(c.opt_ts(2)?).map(|(next, latest)| {
                    threadlane_core::client::HeldResults {
                        count: u64::from(count),
                        next,
                        latest,
                    }
                }))
            },
        )
        .await
    }

    /// Reads the space's delivery pacing.
    pub async fn pacing(&self) -> StoreResult<Pacing> {
        let conn = self.db.reader()?;
        query_one(
            &conn,
            "SELECT delivery_rate, quiet_hours, digest_minutes FROM space WHERE id = 1",
            (),
            |row| {
                let c = Cols(row);
                let positive = |index| {
                    c.opt_i64(index)?
                        .map(|value| {
                            u32::try_from(value)
                                .ok()
                                .filter(|value| *value > 0)
                                .ok_or_else(|| {
                                    StoreError::Corrupt(format!("invalid pacing setting {value}"))
                                })
                        })
                        .transpose()
                };
                Ok(Pacing {
                    rate: positive(0)?,
                    quiet: c.opt_de(1)?,
                    digest_minutes: positive(2)?,
                })
            },
        )
        .await
    }

    /// Saves hub-validated pacing settings and releases holds made under the old settings.
    pub async fn set_pacing(&self, pacing: &Pacing) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "UPDATE space SET delivery_rate = ?1, quiet_hours = ?2, digest_minutes = ?3 WHERE id = 1",
            params![
                pacing.rate.map(i64::from),
                pacing.quiet.as_ref().map(json_text).transpose()?,
                pacing.digest_minutes.map(i64::from),
            ],
        )
        .await?;
        exec(
            tx.conn(),
            "UPDATE outbox SET not_before = NULL, pacing_hold = NULL
             WHERE status = 'pending' AND pacing_hold IS NOT NULL",
            (),
        )
        .await?;
        tx.commit().await
    }

    /// Reads due messages with their current pacing hold, oldest first.
    pub async fn dispatchable(
        &self,
        now: Timestamp,
        limit: u32,
    ) -> StoreResult<Vec<(OutboxMessage, Option<String>)>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {OUTBOX_FIELDS}, pacing_hold FROM outbox
                 WHERE status = 'pending' AND (not_before IS NULL OR not_before <= ?1 OR pacing_hold = 'digest')
                 ORDER BY COALESCE((SELECT MIN(original.id) FROM outbox original
                                    WHERE original.digest_id = outbox.id), outbox.id)
                 LIMIT ?2"
            ),
            params![now.as_millis(), i64::from(limit)],
            |row| Ok((decode_message(row)?, Cols(row).opt_text(18)?)),
        )
        .await
    }

    /// Earliest future time when a retry or a pacing hold is due.
    pub async fn next_delivery_after(&self, now: Timestamp) -> StoreResult<Option<Timestamp>> {
        let conn = self.db.reader()?;
        query_one(
            &conn,
            "SELECT MIN(not_before) FROM outbox WHERE status = 'pending' AND not_before > ?1",
            params![now.as_millis()],
            |row| Cols(row).opt_ts(0),
        )
        .await
    }

    /// Holds a due message until the next pacing boundary.
    pub async fn hold_delivery(
        &self,
        id: OutboxId,
        until: Timestamp,
        reason: &str,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "UPDATE outbox SET not_before = ?1, pacing_hold = ?2 WHERE id = ?3 AND status = 'pending'",
            params![until.as_millis(), reason, id.get()],
        )
        .await?;
        tx.commit().await
    }

    /// Returns the delivery time of the Nth most recent paced message in the window.
    pub async fn rate_boundary(
        &self,
        since: Timestamp,
        rate: u32,
    ) -> StoreResult<Option<Timestamp>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            "SELECT sent_at FROM outbox WHERE status = 'sent' AND kind = 'message'
             AND urgency != 'high' AND sent_at > ?1
             ORDER BY sent_at DESC, delivery_seq DESC LIMIT 1 OFFSET ?2",
            params![since.as_millis(), i64::from(rate.saturating_sub(1))],
            |row| Cols(row).ts(0),
        )
        .await
    }

    /// Creates a digest and suppresses every original in one transaction.
    pub async fn queue_digest(
        &self,
        digest: &NewOutboxMessage,
        originals: &[OutboxId],
    ) -> StoreResult<OutboxMessage> {
        let tx = self.db.begin().await?;
        let digest = enqueue_in(tx.conn(), digest).await?;
        for id in originals {
            let changed = exec(
                tx.conn(),
                "UPDATE outbox SET status = 'suppressed', digest_id = ?1, pacing_hold = NULL
                 WHERE id = ?2 AND status = 'pending' AND attempts = 0",
                params![digest.id.get(), id.get()],
            )
            .await?;
            if changed != 1 {
                return Err(StoreError::Conflict(format!(
                    "outbox message {id} cannot join a digest"
                )));
            }
        }
        tx.commit().await?;
        Ok(digest)
    }

    /// Finds the digest that replaced a suppressed original.
    pub async fn digest_for(&self, id: OutboxId) -> StoreResult<Option<OutboxId>> {
        let conn = self.db.reader()?;
        Ok(query_opt(
            &conn,
            "SELECT digest_id FROM outbox WHERE id = ?1",
            params![id.get()],
            |row| Cols(row).opt_id(0),
        )
        .await?
        .flatten())
    }

    /// Records a failed attempt and returns the lane notified when delivery dies.
    pub async fn delivery_failed(
        &self,
        message: &OutboxMessage,
        error: &str,
        now: Timestamp,
        retry_at: Option<Timestamp>,
    ) -> StoreResult<Option<LaneId>> {
        let tx = self.db.begin().await?;
        let changed = exec(
            tx.conn(),
            "UPDATE outbox SET status = ?1, attempts = attempts + 1, not_before = ?2, pacing_hold = NULL
             WHERE id = ?3 AND status = 'pending' AND attempts = ?4",
            params![
                if retry_at.is_some() { "pending" } else { "dead" },
                opt_ts(retry_at),
                message.id.get(),
                i64::from(message.attempts),
            ],
        )
        .await?;
        if changed == 0 || retry_at.is_some() {
            tx.commit().await?;
            return Ok(None);
        }
        let origin = match (message.project_id, message.lane_id) {
            (Some(project), Some(lane)) => Some((project, lane, message.session_id)),
            // A space digest reports failure to its oldest original's lane.
            _ => {
                query_opt(
                    tx.conn(),
                    "SELECT project_id, lane_id, session_id FROM outbox
                 WHERE digest_id = ?1 AND project_id IS NOT NULL AND lane_id IS NOT NULL
                 ORDER BY id LIMIT 1",
                    params![message.id.get()],
                    |row| {
                        let c = Cols(row);
                        Ok((c.id(0)?, c.id(1)?, c.opt_id(2)?))
                    },
                )
                .await?
            }
        };
        if let Some((project_id, lane_id, session_ref)) = origin {
            insert_event_in(tx.conn(), &NewEvent {
                project_id,
                lane_id,
                source: "dispatcher".to_owned(),
                source_event_id: message.id.to_string(),
                kind: EventKind::DeliveryFailure,
                payload: json!({
                    "outbox_id": message.id,
                    "text": format!("Delivery of message {} to {} failed: {error}", message.id, message.channel),
                }),
                trust: Trust::System,
                requested: false,
                session_ref,
                decision_ref: None,
                reply_to_ref: None,
                received_at: now,
            }).await?;
        }
        tx.commit().await?;
        Ok(origin.map(|(_, lane, _)| lane))
    }
}
