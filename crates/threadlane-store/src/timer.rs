//! [`TimerStore`] on the hub database (Plan.md §7).

use async_trait::async_trait;
use serde_json::json;
use threadlane_core::ids::{ProjectId, TimerId};
use threadlane_core::model::{
    EventKind, Inserted, NewEvent, NewTimer, Timer, TimerCategory, TimerCreator, TimerStatus, Trust,
};
use threadlane_core::store::{StoreError, StoreResult, TimerStore};
use threadlane_core::time::Timestamp;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::decision::{expire_in, expiring_decision};
use crate::inbox::insert_event_in;
use crate::row::{Cols, json_text, opt_raw, params};

/// Source name of `timer_fired` events.
pub const TIMER_SOURCE: &str = "timer";

const TIMER_FIELDS: &str = "id, project_id, lane_id, agent_id, task_ref, category, due_at,
    recurrence, instruction, status, created_by, created_by_call, last_fired_at, fire_count,
    requested";

fn decode_timer(row: &Row) -> StoreResult<Timer> {
    let c = Cols(row);
    Ok(Timer {
        id: c.id(0)?,
        project_id: c.id(1)?,
        lane_id: c.id(2)?,
        agent_id: c.opt_id(3)?,
        task_ref: c.opt_id(4)?,
        category: c.parse(5)?,
        due_at: c.ts(6)?,
        recurrence: c.opt_de(7)?,
        instruction: c.text(8)?,
        status: c.parse(9)?,
        created_by: c.parse(10)?,
        created_by_call: c.opt_text(11)?,
        last_fired_at: c.opt_ts(12)?,
        fire_count: c.u32(13)?,
        requested: c.bool(14)?,
    })
}

async fn timer_in(conn: &Connection, id: TimerId) -> StoreResult<Option<Timer>> {
    query_opt(
        conn,
        &format!("SELECT {TIMER_FIELDS} FROM timers WHERE id = ?1"),
        params![id.get()],
        decode_timer,
    )
    .await
}

/// The event id of one occurrence. Derived from the timer and its due time, so
/// a crash and retry cannot fire an occurrence twice (Plan.md §7.3).
fn occurrence_id(id: TimerId, occurrence: Timestamp) -> String {
    format!("{id}:{}", occurrence.as_millis())
}

/// Moves a timer from `from` to `to`, leaving one already at `to` as it is;
/// one in any other status has ended. `due_at` replaces its due time.
async fn set_status(
    db: &HubDb,
    id: TimerId,
    from: TimerStatus,
    to: TimerStatus,
    due_at: Option<Timestamp>,
) -> StoreResult<Timer> {
    let tx = db.db.begin().await?;
    let timer = timer_in(tx.conn(), id)
        .await?
        .ok_or_else(|| StoreError::not_found("timer", id))?;
    if timer.status == to {
        return Ok(timer);
    }
    if timer.status != from {
        return Err(StoreError::Conflict(format!(
            "timer {id} is {}, so it cannot become {to}",
            timer.status
        )));
    }
    exec(
        tx.conn(),
        "UPDATE timers SET status = ?1, due_at = ?2 WHERE id = ?3",
        params![
            to.as_str(),
            due_at.unwrap_or(timer.due_at).as_millis(),
            id.get()
        ],
    )
    .await?;
    let changed = timer_in(tx.conn(), id)
        .await?
        .ok_or_else(|| StoreError::not_found("timer", id))?;
    tx.commit().await?;
    Ok(changed)
}

/// Creates a timer inside the caller's transaction; a repeated
/// `created_by_call` returns the original.
pub(crate) async fn insert_timer_in(conn: &Connection, timer: &NewTimer) -> StoreResult<Timer> {
    if timer.category == TimerCategory::Schedule && timer.recurrence.is_none() {
        return Err(StoreError::Invalid("a schedule needs a recurrence".into()));
    }
    if let Some(call) = &timer.created_by_call {
        let existing = query_opt(
            conn,
            &format!("SELECT {TIMER_FIELDS} FROM timers WHERE created_by_call = ?1"),
            params![call.as_str()],
            decode_timer,
        )
        .await?;
        if let Some(existing) = existing {
            return Ok(existing);
        }
    }
    query_one(
        conn,
        &format!(
            "INSERT INTO timers (project_id, lane_id, agent_id, task_ref, category, due_at,
                 recurrence, instruction, status, created_by, created_by_call, requested)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', ?9, ?10, ?11)
             RETURNING {TIMER_FIELDS}"
        ),
        params![
            timer.project_id.get(),
            timer.lane_id.get(),
            opt_raw(timer.agent_id, |id| id.get()),
            opt_raw(timer.task_ref, |id| id.get()),
            timer.category.as_str(),
            timer.due_at.as_millis(),
            timer.recurrence.as_ref().map(json_text).transpose()?,
            timer.instruction.as_str(),
            timer.created_by.as_str(),
            timer.created_by_call.as_deref(),
            timer.requested,
        ],
        decode_timer,
    )
    .await
}

#[async_trait]
impl TimerStore for HubDb {
    async fn create_timer(&self, timer: &NewTimer) -> StoreResult<Timer> {
        let tx = self.db.begin().await?;
        let created = insert_timer_in(tx.conn(), timer).await?;
        tx.commit().await?;
        Ok(created)
    }

    async fn timer(&self, id: TimerId) -> StoreResult<Option<Timer>> {
        let conn = self.db.reader()?;
        timer_in(&conn, id).await
    }

    async fn due_timers(&self, now: Timestamp) -> StoreResult<Vec<Timer>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {TIMER_FIELDS} FROM timers WHERE status = 'active' AND due_at <= ?1
                 ORDER BY due_at, id"
            ),
            params![now.as_millis()],
            decode_timer,
        )
        .await
    }

    async fn fire(
        &self,
        id: TimerId,
        occurrence: Timestamp,
        next_due_at: Option<Timestamp>,
        now: Timestamp,
    ) -> StoreResult<Inserted> {
        let source_event_id = occurrence_id(id, occurrence);
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let existing = query_opt(
            conn,
            "SELECT id FROM events WHERE source = ?1 AND source_event_id = ?2",
            params![TIMER_SOURCE, source_event_id.as_str()],
            |row| Cols(row).id(0),
        )
        .await?;
        if let Some(existing) = existing {
            return Ok(Inserted::Duplicate(existing));
        }
        let timer = timer_in(conn, id)
            .await?
            .ok_or_else(|| StoreError::not_found("timer", id))?;
        if timer.status != TimerStatus::Active || timer.due_at != occurrence {
            return Err(StoreError::Conflict(format!(
                "timer {id} is {} and due at {}, not at {occurrence}",
                timer.status, timer.due_at
            )));
        }
        // A decision's expiry fires no event of its own: it records the
        // decision declined, with its answer event (docs/milestone-9.md
        // decision 8).
        if let Some(decision) = timer
            .created_by_call
            .as_deref()
            .and_then(expiring_decision)
            .filter(|_| timer.created_by == TimerCreator::System)
        {
            exec(
                conn,
                "UPDATE timers SET status = 'done', last_fired_at = ?1, fire_count = fire_count + 1
                 WHERE id = ?2",
                params![now.as_millis(), id.get()],
            )
            .await?;
            let inserted = expire_in(conn, decision, now).await?;
            tx.commit().await?;
            return Ok(inserted);
        }
        match timer.category {
            TimerCategory::FollowUp => {
                exec(
                    conn,
                    "UPDATE timers SET status = 'done', last_fired_at = ?1, fire_count = fire_count + 1
                     WHERE id = ?2",
                    params![now.as_millis(), id.get()],
                )
                .await?;
            }
            TimerCategory::Schedule => {
                let next = next_due_at.ok_or_else(|| {
                    StoreError::Invalid(format!("schedule {id} fired without a next occurrence"))
                })?;
                if next <= occurrence {
                    return Err(StoreError::Invalid(format!(
                        "schedule {id}: next occurrence {next} is not after {occurrence}"
                    )));
                }
                exec(
                    conn,
                    "UPDATE timers SET due_at = ?1, last_fired_at = ?2, fire_count = fire_count + 1
                     WHERE id = ?3",
                    params![next.as_millis(), now.as_millis(), id.get()],
                )
                .await?;
            }
        }
        let inserted = insert_event_in(
            conn,
            &NewEvent {
                project_id: timer.project_id,
                lane_id: timer.lane_id,
                source: TIMER_SOURCE.to_owned(),
                source_event_id,
                kind: EventKind::TimerFired,
                payload: json!({
                    "timer_id": id,
                    "category": timer.category,
                    "instruction": timer.instruction,
                    "occurrence": occurrence,
                    "task_ref": timer.task_ref,
                    "created_by": timer.created_by,
                }),
                trust: Trust::System,
                requested: timer.requested,
                session_ref: None,
                decision_ref: None,
                reply_to_ref: None,
                received_at: now,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(inserted)
    }

    async fn cancel_timer(&self, id: TimerId) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let changed = exec(
            tx.conn(),
            "UPDATE timers SET status = 'cancelled' WHERE id = ?1 AND status IN ('active', 'paused')",
            params![id.get()],
        )
        .await?;
        if changed == 0 && timer_in(tx.conn(), id).await?.is_none() {
            return Err(StoreError::not_found("timer", id));
        }
        tx.commit().await
    }

    async fn project_timers(&self, project_id: ProjectId) -> StoreResult<Vec<Timer>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {TIMER_FIELDS} FROM timers
                 WHERE project_id = ?1 AND status IN ('active', 'paused')
                     AND created_by != 'system' ORDER BY id"
            ),
            params![project_id.get()],
            decode_timer,
        )
        .await
    }

    async fn pause_timer(&self, id: TimerId) -> StoreResult<Timer> {
        set_status(self, id, TimerStatus::Active, TimerStatus::Paused, None).await
    }

    async fn resume_timer(&self, id: TimerId, due_at: Option<Timestamp>) -> StoreResult<Timer> {
        set_status(self, id, TimerStatus::Paused, TimerStatus::Active, due_at).await
    }
}
