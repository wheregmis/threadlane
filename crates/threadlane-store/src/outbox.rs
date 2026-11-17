//! [`OutboxStore`] on the hub database (Plan.md §10).

use async_trait::async_trait;
use threadlane_core::ids::{OutboxId, ProjectId, RunId};
use threadlane_core::model::{NewOutboxMessage, OutboxKind, OutboxMessage, OutboxStatus};
use threadlane_core::store::{OutboxStore, StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::row::{Cols, json_text, opt_raw, opt_ts, params};

const OUTBOX_FIELDS: &str = "id, project_id, lane_id, session_id, run_id, tool_call_id, kind,
    decision_id, channel, body, urgency, status, not_before, attempts, channel_message_id,
    created_at, sent_at, delivery_seq";

fn decode_message(row: &Row) -> StoreResult<OutboxMessage> {
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
    let changed = exec(
        conn,
        "UPDATE outbox SET run_id = ?1
         WHERE id = ?2
           AND NOT EXISTS (SELECT 1 FROM runs WHERE id = ?3 AND status = 'committed')",
        params![new_run.get(), existing.id.get(), old_run.get()],
    )
    .await?;
    Ok(if changed == 1 {
        OutboxMessage {
            run_id: Some(new_run),
            ..existing
        }
    } else {
        existing
    })
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
            "INSERT INTO outbox (project_id, lane_id, session_id, run_id, tool_call_id, kind,
                 decision_id, channel, body, urgency, status, not_before, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11, ?12)
             RETURNING {OUTBOX_FIELDS}"
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
            "SELECT COALESCE(MAX(delivery_seq), 0) + 1 FROM outbox",
            (),
            |row| Cols(row).i64(0),
        )
        .await?;
        let changed = exec(
            tx.conn(),
            "UPDATE outbox SET status = ?1, channel_message_id = ?2, sent_at = ?3,
                 attempts = attempts + 1, delivery_seq = ?4
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
                 WHERE project_id = ?1 AND status = 'sent' AND kind = 'message'
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
}
