//! Read-only operational snapshots and the one-shot inbox repair.

use std::time::Duration;

use threadlane_core::client::{
    DeliveryHealth, LaneInspection, Latency, OperationsMetrics, TraceInspection, TracedToolCall,
};
use threadlane_core::ids::{EventId, LaneId, ProjectId};
use threadlane_core::model::Event;
use threadlane_core::store::{StoreError, StoreResult};
use turso::Connection;

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::decision::{DECISION_FIELDS, decode_decision};
use crate::inbox::{EVENT_FIELDS, LANE_FIELDS, decode_event, decode_lane};
use crate::outbox::{ACCOUNT_NOTICE, OUTBOX_FIELDS, decode_message};
use crate::row::{Cols, opt_raw, params};
use crate::session::{RUN_FIELDS, SESSION_FIELDS, decode_run, decode_session};
use crate::timer::{TIMER_FIELDS, decode_timer};

const DAY: Duration = Duration::from_secs(86_400);
const DELIVERY_WEEK: Duration = Duration::from_secs(7 * 86_400);

async fn latency(
    conn: &Connection,
    project: Option<ProjectId>,
    samples: &str,
) -> StoreResult<Latency> {
    query_one(
        conn,
        &format!("SELECT COUNT(*), COALESCE(SUM(ms), 0), MAX(ms) FROM ({samples})"),
        params![opt_raw(project, ProjectId::get)],
        |row| {
            let c = Cols(row);
            let count = c.i64(0)?;
            let sum = c.i64(1)?;
            Ok(Latency {
                count,
                mean_ms: (count != 0).then(|| sum as f64 / count as f64),
                max_ms: c.opt_i64(2)?,
            })
        },
    )
    .await
}

impl HubDb {
    /// Computes a read-only metrics snapshot from the hub's current and historical records.
    pub async fn operations_metrics(
        &self,
        project: Option<ProjectId>,
    ) -> StoreResult<OperationsMetrics> {
        let at = self.now();
        let now = at.as_millis();
        let project_id = opt_raw(project, ProjectId::get);
        let conn = self.db.reader()?;
        exec(&conn, "BEGIN", ()).await?;
        let lane_lag_ms = query_all(
            &conn,
            "SELECT lane_id, MAX(0, ?2 - MIN(received_at))
             FROM events WHERE status = 'pending' AND (?1 IS NULL OR project_id = ?1)
             GROUP BY lane_id",
            params![project_id.clone(), now],
            |row| Ok((Cols(row).id(0)?, Cols(row).i64(1)?)),
        )
        .await?;
        let queue_depth = query_all(
            &conn,
            "SELECT p.name, COUNT(DISTINCT e.lane_id) FROM projects p
             LEFT JOIN events e ON e.project_id = p.id AND e.status = 'pending'
             WHERE ?1 IS NULL OR p.id = ?1 GROUP BY p.id",
            params![project_id.clone()],
            |row| Ok((Cols(row).text(0)?, Cols(row).i64(1)?)),
        )
        .await?;
        let run_duration = latency(
            &conn,
            project,
            "SELECT MAX(0, r.ended_at - r.started_at) AS ms
             FROM runs r JOIN lanes l ON l.id = r.lane_id
             WHERE r.ended_at IS NOT NULL AND (?1 IS NULL OR l.project_id = ?1)",
        )
        .await?;
        let account_cooldowns = query_all(
            &conn,
            "SELECT label, cooldown_until FROM accounts WHERE cooldown_until > ?1",
            params![now],
            |row| Ok((Cols(row).text(0)?, Cols(row).ts(1)?)),
        )
        .await?;
        let account_wait_ms = query_one(
            &conn,
            "SELECT COALESCE(SUM(MAX(0, MIN(?1, COALESCE(r.created_at, ?1)) - b.created_at)), 0)
             FROM outbox b LEFT JOIN outbox r ON r.tool_call_id = REPLACE(b.tool_call_id, ':blocked', ':resumed')
             WHERE b.tool_call_id LIKE ?2",
            params![now, format!("{ACCOUNT_NOTICE}%:blocked")],
            |row| Cols(row).i64(0),
        )
        .await?;
        let dead_events = query_one(
            &conn,
            "SELECT COUNT(*) FROM events
             WHERE status = 'dead' AND received_at > ?2 AND (?1 IS NULL OR project_id = ?1)",
            params![project_id.clone(), (at - DELIVERY_WEEK).as_millis()],
            |row| Cols(row).i64(0),
        )
        .await?;
        let timer_lateness = latency(
            &conn, project,
            "SELECT MAX(0, received_at - json_extract(payload, '$.occurrence')) AS ms FROM events
             WHERE source = 'timer' AND kind IN ('timer_fired', 'dream')
                AND json_extract(payload, '$.occurrence') IS NOT NULL AND (?1 IS NULL OR project_id = ?1)
             UNION ALL
             SELECT MAX(0, last_fired_at - due_at) FROM timers
             WHERE created_by = 'system' AND created_by_call LIKE 'decision:%'
                AND last_fired_at IS NOT NULL AND (?1 IS NULL OR project_id = ?1)",
        )
        .await?;
        let (outbox_lag_ms, delivery_health) = query_one(
            &conn,
            "WITH scoped AS (
                SELECT status, created_at, not_before,
                       status = 'pending' AND pacing_hold IS NOT NULL AS held,
                       status = 'pending' AND pacing_hold IS NULL AND attempts > 0 AS retrying
                FROM outbox
                WHERE (?1 IS NULL OR project_id = ?1)
                  AND (status IN ('pending', 'failed')
                    OR (status = 'dead' AND created_at > ?3 AND created_at <= ?2))
             )
             SELECT MAX(CASE WHEN status IN ('pending', 'failed')
                             THEN MAX(0, ?2 - created_at) END),
                    COUNT(CASE WHEN held THEN 1 END),
                    MIN(CASE WHEN held THEN not_before END),
                    COUNT(CASE WHEN retrying THEN 1 END),
                    MIN(CASE WHEN retrying THEN COALESCE(not_before, ?2) END),
                    COUNT(CASE WHEN status = 'dead' THEN 1 END)
             FROM scoped",
            params![project_id.clone(), now, (at - DELIVERY_WEEK).as_millis()],
            |row| {
                let c = Cols(row);
                Ok((
                    c.opt_i64(0)?,
                    DeliveryHealth {
                        held: c.i64(1)?,
                        next_hold_check: c.opt_ts(2)?,
                        retrying: c.i64(3)?,
                        next_retry: c.opt_ts(4)?,
                        failed_created_in_last_week: c.i64(5)?,
                    },
                ))
            },
        )
        .await?;
        let messages_per_day = query_all(
            &conn,
            "SELECT date(sent_at / 1000.0, 'unixepoch') AS day, COUNT(*)
             FROM outbox WHERE status = 'sent' AND kind = 'message' AND sent_at IS NOT NULL
                 AND (?1 IS NULL OR project_id = ?1)
             GROUP BY day",
            params![project_id.clone()],
            |row| Ok((Cols(row).text(0)?, Cols(row).i64(1)?)),
        )
        .await?;
        let silent_run_ratio = query_one(
            &conn,
            "SELECT COUNT(CASE WHEN r.outcome = 'silent' THEN 1 END), COUNT(*)
             FROM runs r JOIN lanes l ON l.id = r.lane_id
             WHERE r.status = 'committed' AND (?1 IS NULL OR l.project_id = ?1)",
            params![project_id.clone()],
            |row| {
                let c = Cols(row);
                let silent = c.i64(0)?;
                let total = c.i64(1)?;
                Ok((total != 0).then(|| silent as f64 / total as f64))
            },
        )
        .await?;
        let (decisions_per_day, classifier_decisions_per_day) = query_one(
            &conn,
            "SELECT COUNT(CASE WHEN json_extract(answer, '$.classifier') IS NULL THEN 1 END),
                    COUNT(CASE WHEN json_extract(answer, '$.classifier') IS NOT NULL THEN 1 END)
             FROM decisions WHERE (?1 IS NULL OR project_id = ?1)
             AND created_at > ?2 AND created_at <= ?3",
            params![project_id.clone(), (at - DAY).as_millis(), now],
            |row| Ok((Cols(row).i64(0)?, Cols(row).i64(1)?)),
        )
        .await?;
        let answer_latency = latency(
            &conn, project,
            "SELECT MAX(0, answered_at - created_at) AS ms FROM decisions
             WHERE status = 'answered' AND answered_at IS NOT NULL
                AND json_extract(answer, '$.classifier') IS NULL AND (?1 IS NULL OR project_id = ?1)",
        )
        .await?;
        let standing_rules_created = query_one(
            &conn,
            "SELECT COUNT(*) FROM standing_rules WHERE ?1 IS NULL OR project_id = ?1",
            params![project_id],
            |row| Cols(row).i64(0),
        )
        .await?;
        exec(&conn, "COMMIT", ()).await?;
        Ok(OperationsMetrics {
            at,
            lane_lag_ms: lane_lag_ms.into_iter().collect(),
            queue_depth: queue_depth.into_iter().collect(),
            run_duration,
            account_cooldowns: account_cooldowns.into_iter().collect(),
            account_wait_ms,
            dead_events,
            timer_lateness,
            outbox_lag_ms,
            delivery_health: Some(delivery_health),
            messages_per_day: messages_per_day.into_iter().collect(),
            silent_run_ratio,
            decisions_per_day,
            classifier_decisions_per_day,
            answer_latency,
            standing_rules_created,
        })
    }

    // shortcut: lane and trace replies include full histories, page them when they can exceed the transport limit.
    /// Reads a lane, its lease, and its operational records in one snapshot.
    pub async fn inspect_lane(&self, id: LaneId) -> StoreResult<LaneInspection> {
        let conn = self.db.reader()?;
        exec(&conn, "BEGIN", ()).await?;
        let lane = query_opt(
            &conn,
            &format!("SELECT {LANE_FIELDS} FROM lanes WHERE id = ?1"),
            params![id.get()],
            decode_lane,
        )
        .await?
        .ok_or_else(|| StoreError::not_found("lane", id))?;
        let events = query_all(
            &conn,
            &format!("SELECT {EVENT_FIELDS} FROM events WHERE lane_id = ?1 ORDER BY id"),
            params![id.get()],
            decode_event,
        )
        .await?;
        let sessions = query_all(
            &conn,
            &format!("SELECT {SESSION_FIELDS} FROM sessions WHERE lane_id = ?1 ORDER BY id"),
            params![id.get()],
            decode_session,
        )
        .await?;
        let runs = query_all(
            &conn,
            &format!("SELECT {RUN_FIELDS} FROM runs WHERE lane_id = ?1 ORDER BY id"),
            params![id.get()],
            decode_run,
        )
        .await?;
        let open_decisions = query_all(
            &conn, &format!("SELECT {DECISION_FIELDS} FROM decisions WHERE lane_id = ?1 AND status = 'pending' ORDER BY id"),
            params![id.get()], decode_decision,
        ).await?;
        let timers = query_all(
            &conn,
            &format!("SELECT {TIMER_FIELDS} FROM timers WHERE lane_id = ?1 ORDER BY id"),
            params![id.get()],
            decode_timer,
        )
        .await?;
        exec(&conn, "COMMIT", ()).await?;
        Ok(LaneInspection {
            lane,
            events,
            sessions,
            runs,
            open_decisions,
            timers,
        })
    }

    /// Moves a dead event to pending once in its lifetime, keeping its identity and trace.
    pub async fn replay_event(&self, id: EventId) -> StoreResult<Event> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let event = query_opt(
            conn,
            &format!(
                "UPDATE events SET status = 'pending', attempts = 0, status_reason = NULL,
                       claimed_by_run = NULL, replayed = 1
                      WHERE id = ?1 AND status = 'dead' AND replayed = 0 RETURNING {EVENT_FIELDS}"
            ),
            params![id.get()],
            decode_event,
        )
        .await?;
        let Some(event) = event else {
            return match query_opt(
                conn,
                "SELECT replayed FROM events WHERE id = ?1",
                params![id.get()],
                |row| Cols(row).bool(0),
            )
            .await?
            {
                None => Err(StoreError::not_found("event", id)),
                Some(true) => Err(StoreError::Conflict(format!(
                    "event {id} was already replayed"
                ))),
                Some(false) => Err(StoreError::Conflict(format!("event {id} is not dead"))),
            };
        };
        crate::task::on_event_inserted(conn, event.lane_id).await?;
        tx.commit().await?;
        Ok(event)
    }

    /// Looks up records by their stored trace ID.
    pub async fn inspect_trace(&self, id: &str) -> StoreResult<TraceInspection> {
        let conn = self.db.reader()?;
        exec(&conn, "BEGIN", ()).await?;
        let events = query_all(
            &conn,
            &format!("SELECT {EVENT_FIELDS} FROM events WHERE trace_id = ?1 ORDER BY id"),
            params![id],
            decode_event,
        )
        .await?;
        let runs = query_all(
            &conn,
            &format!("SELECT {RUN_FIELDS} FROM runs WHERE trace_id = ?1 ORDER BY id"),
            params![id],
            decode_run,
        )
        .await?;
        if events.is_empty() && runs.is_empty() {
            return Err(StoreError::not_found("trace", id));
        }
        let tool_calls = query_all(
            &conn,
            "SELECT run_id, call_key, tool, arguments, status, result, started_at, finished_at, trace_id
             FROM tool_calls WHERE trace_id = ?1 ORDER BY id",
            params![id],
            |row| {
                let c = Cols(row);
                Ok(TracedToolCall {
                    run_id: c.id(0)?,
                    key: c.text(1)?,
                    tool: c.text(2)?,
                    arguments: c.json(3)?,
                    status: c.parse(4)?,
                    result: c.opt_json(5)?,
                    started_at: c.ts(6)?,
                    finished_at: c.opt_ts(7)?,
                    trace_id: c.text(8)?,
                })
            },
        ).await?;
        let decisions = query_all(
            &conn,
            &format!("SELECT {DECISION_FIELDS} FROM decisions WHERE trace_id = ?1 ORDER BY id"),
            params![id],
            decode_decision,
        )
        .await?;
        let outbox = query_all(
            &conn,
            &format!("SELECT {OUTBOX_FIELDS} FROM outbox WHERE trace_id = ?1 ORDER BY id"),
            params![id],
            decode_message,
        )
        .await?;
        exec(&conn, "COMMIT", ()).await?;
        Ok(TraceInspection {
            id: id.to_owned(),
            events,
            runs,
            tool_calls,
            decisions,
            outbox,
        })
    }
}

#[cfg(test)]
mod tests;
