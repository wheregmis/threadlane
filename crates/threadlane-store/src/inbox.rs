//! [`InboxStore`] on the hub database: lanes, leases, and events (Plan.md §6).

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::ids::{EventId, LaneId, MachineId, ProjectId};
use threadlane_core::model::{
    Event, EventKind, EventStatus, Inserted, Lane, LaneKind, Lease, NewEvent, Trust,
};
use threadlane_core::store::{InboxStore, StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::decision;
use crate::row::{Cols, json_text, opt_raw, params};
use crate::task;

pub(crate) const EVENT_FIELDS: &str = "id, project_id, lane_id, source, source_event_id, kind,
    payload, trust, requested, session_ref, decision_ref, reply_to_ref, status, claimed_by_run,
    attempts, received_at, status_reason, trace_id";

pub(crate) fn decode_event(row: &Row) -> StoreResult<Event> {
    let c = Cols(row);
    Ok(Event {
        id: c.id(0)?,
        project_id: c.id(1)?,
        lane_id: c.id(2)?,
        source: c.text(3)?,
        source_event_id: c.text(4)?,
        kind: c.parse(5)?,
        payload: c.json(6)?,
        trust: c.parse(7)?,
        requested: c.bool(8)?,
        session_ref: c.opt_id(9)?,
        decision_ref: c.opt_id(10)?,
        reply_to_ref: c.opt_id(11)?,
        status: c.parse(12)?,
        claimed_by_run: c.opt_id(13)?,
        attempts: c.u32(14)?,
        received_at: c.ts(15)?,
        status_reason: c.opt_text(16)?,
        trace_id: c.text(17)?,
    })
}

pub(crate) const LANE_FIELDS: &str = "id, project_id, kind, task_id, lease_owner, lease_expires_at,
    lease_epoch, backoff_until, last_machine_id";

pub(crate) fn decode_lane(row: &Row) -> StoreResult<Lane> {
    let c = Cols(row);
    Ok(Lane {
        id: c.id(0)?,
        project_id: c.id(1)?,
        kind: c.parse(2)?,
        task_id: c.opt_id(3)?,
        lease_owner: c.opt_id(4)?,
        lease_expires_at: c.opt_ts(5)?,
        lease_epoch: c.i64(6)?,
        backoff_until: c.opt_ts(7)?,
        last_machine_id: c.opt_id(8)?,
    })
}

/// Fails with [`StoreError::StaleLease`] unless `lease` is the lane's current
/// lease. Called first in every write a run makes (Plan.md invariant 10).
pub(crate) async fn check_lease(conn: &Connection, lease: &Lease) -> StoreResult<()> {
    let current = query_opt(
        conn,
        "SELECT lease_epoch, lease_owner FROM lanes WHERE id = ?1",
        params![lease.lane_id.get()],
        |row| {
            let c = Cols(row);
            Ok((c.i64(0)?, c.opt_id::<MachineId>(1)?))
        },
    )
    .await?
    .ok_or_else(|| StoreError::not_found("lane", lease.lane_id))?;
    match current {
        (epoch, Some(owner)) if epoch == lease.epoch && owner == lease.machine_id => Ok(()),
        (epoch, _) => Err(StoreError::StaleLease {
            lane_id: lease.lane_id,
            held: lease.epoch,
            current: epoch,
        }),
    }
}

/// Inserts an event inside the caller's transaction, deduplicating on
/// `(source, source_event_id)`.
pub(crate) async fn insert_event_in(conn: &Connection, event: &NewEvent) -> StoreResult<Inserted> {
    insert_event_with_skip_in(conn, event, None).await
}

/// Records a terminal receipt without routing an event to a task or lane.
pub(crate) async fn insert_event_with_skip_in(
    conn: &Connection,
    event: &NewEvent,
    skip_reason: Option<&str>,
) -> StoreResult<Inserted> {
    let existing: Option<EventId> = query_opt(
        conn,
        "SELECT id FROM events WHERE source = ?1 AND source_event_id = ?2",
        params![event.source.as_str(), event.source_event_id.as_str()],
        |row| Cols(row).id(0),
    )
    .await?;
    if let Some(id) = existing {
        return Ok(Inserted::Duplicate(id));
    }
    let lane: Option<(i64, LaneKind, Option<String>)> = query_opt(
        conn,
        "SELECT l.project_id, l.kind, CASE WHEN p.archived_at IS NOT NULL THEN p.name END
         FROM lanes l JOIN projects p ON p.id = l.project_id WHERE l.id = ?1",
        params![event.lane_id.get()],
        |row| {
            let c = Cols(row);
            Ok((c.i64(0)?, c.parse(1)?, c.opt_text(2)?))
        },
    )
    .await?;
    let (kind, archived) = match lane {
        None => return Err(StoreError::not_found("lane", event.lane_id)),
        Some((project, _, _)) if project != event.project_id.get() => {
            return Err(StoreError::Invalid(format!(
                "lane {} belongs to project {project}, not {}",
                event.lane_id, event.project_id
            )));
        }
        Some((_, kind, archived)) => (kind, archived),
    };
    if let Some(name) = &archived
        && event.kind == EventKind::UserMessage
    {
        return Err(StoreError::Invalid(format!(
            "Project {name:?} is archived."
        )));
    }
    let skip_reason = skip_reason.or_else(|| {
        (archived.is_some() && event.kind == EventKind::OutsideEvent)
            .then_some(crate::ARCHIVED_EVENT_REASON)
    });
    let id: EventId = query_one(
        conn,
        concat!("INSERT INTO events (id, project_id, lane_id, source, source_event_id, kind, payload, trust,
             requested, session_ref, decision_ref, reply_to_ref, status, received_at, status_reason)
         VALUES (", crate::db::next_id_sql!("events"), ", ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) RETURNING id"),
        params![
            event.project_id.get(),
            event.lane_id.get(),
            event.source.as_str(),
            event.source_event_id.as_str(),
            event.kind.as_str(),
            json_text(&event.payload)?,
            event.trust.as_str(),
            event.requested,
            opt_raw(event.session_ref, |id| id.get()),
            opt_raw(event.decision_ref, |id| id.get()),
            opt_raw(event.reply_to_ref, |id| id.get()),
            if skip_reason.is_some() { "skipped" } else { "pending" },
            event.received_at.as_millis(),
            skip_reason,
        ],
        |row| Cols(row).id(0),
    )
    .await?;
    exec(
        conn,
        "UPDATE events SET trace_id = 'event-' || id WHERE id = ?1",
        params![id.get()],
    )
    .await?;
    if skip_reason.is_some() {
        return Ok(Inserted::New(id));
    }
    if event.kind == EventKind::Stop {
        exec(
            conn,
            "UPDATE events SET status = 'done' WHERE id = ?1",
            params![id.get()],
        )
        .await?;
        return Ok(Inserted::New(id));
    }
    if event.kind == EventKind::UserMessage && event.trust == Trust::User {
        resume_lane_in(conn, event.lane_id).await?;
    }
    if kind == LaneKind::Task && event.kind == EventKind::UserMessage {
        let note = event
            .payload
            .get("text")
            .and_then(Json::as_str)
            .unwrap_or_default();
        decision::decline_pending_in(conn, event.lane_id, note, event.received_at).await?;
    }
    task::on_event_inserted(conn, event.lane_id).await?;
    Ok(Inserted::New(id))
}

/// Ends whatever was in flight on a lane whose lease is going away: claimed
/// events return to pending, active runs are abandoned, and a running task
/// is queued again.
pub(crate) async fn reclaim_lane(
    conn: &Connection,
    lane_id: LaneId,
    now: Timestamp,
) -> StoreResult<()> {
    task::on_lane_reclaimed(conn, lane_id).await?;
    exec(
        conn,
        "UPDATE events SET status = 'pending', claimed_by_run = NULL
         WHERE lane_id = ?1 AND status = 'claimed'",
        params![lane_id.get()],
    )
    .await?;
    exec(
        conn,
        "UPDATE runs SET ended_at = ?1,
             status = CASE WHEN lease_epoch = (SELECT cancelled_epoch FROM lanes WHERE id = ?2)
                           THEN 'stopped' ELSE 'abandoned' END,
             error = CASE WHEN lease_epoch = (SELECT cancelled_epoch FROM lanes WHERE id = ?2)
                          THEN 'stopped by the user' ELSE 'lease ended' END
         WHERE lane_id = ?2 AND status = 'active'",
        params![now.as_millis(), lane_id.get()],
    )
    .await?;
    Ok(())
}

pub(crate) async fn check_running(conn: &Connection, lease: &Lease) -> StoreResult<()> {
    let cancelled = query_one(
        conn,
        "SELECT cancelled_epoch FROM lanes WHERE id = ?1",
        params![lease.lane_id.get()],
        |row| Cols(row).opt_i64(0),
    )
    .await?;
    if cancelled == Some(lease.epoch) {
        return Err(StoreError::Stopped);
    }
    Ok(())
}

async fn resume_lane_in(conn: &Connection, lane: LaneId) -> StoreResult<()> {
    exec(
        conn,
        "UPDATE lanes SET stopped = 0, backoff_until = NULL WHERE id = ?1 AND stopped = 1",
        params![lane.get()],
    )
    .await?;
    Ok(())
}

impl HubDb {
    /// Records a user stop and pauses the lane atomically. Returns the epoch to interrupt.
    pub async fn stop_lane(
        &self,
        lane: LaneId,
        source_event_id: String,
        now: Timestamp,
    ) -> StoreResult<Option<i64>> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let (project_id, epoch) = query_opt(
            conn,
            "SELECT project_id, CASE WHEN lease_owner IS NOT NULL THEN lease_epoch END
             FROM lanes WHERE id = ?1",
            params![lane.get()],
            |row| Ok((Cols(row).id(0)?, Cols(row).opt_i64(1)?)),
        )
        .await?
        .ok_or_else(|| StoreError::not_found("lane", lane))?;
        let inserted = insert_event_in(
            conn,
            &NewEvent {
                project_id,
                lane_id: lane,
                source: "client_stop".to_owned(),
                source_event_id,
                kind: EventKind::Stop,
                payload: json!({ "text": "Stopped by the user." }),
                trust: Trust::User,
                requested: true,
                session_ref: None,
                decision_ref: None,
                reply_to_ref: None,
                received_at: now,
            },
        )
        .await?;
        if matches!(inserted, Inserted::Duplicate(_)) {
            return Ok(None);
        }
        exec(
            conn,
            "UPDATE lanes SET stopped = 1, cancelled_epoch = ?1 WHERE id = ?2",
            params![epoch, lane.get()],
        )
        .await?;
        tx.commit().await?;
        Ok(epoch)
    }

    /// Resumes a lane without clearing cancellation of its previous run.
    pub async fn resume_lane(&self, lane: LaneId) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        resume_lane_in(tx.conn(), lane).await?;
        tx.commit().await
    }

    /// Whether the lane was stopped and has yet to resume.
    pub async fn lane_stopped(&self, lane: LaneId) -> StoreResult<bool> {
        let conn = self.db.reader()?;
        query_one(
            &conn,
            "SELECT stopped FROM lanes WHERE id = ?1",
            params![lane.get()],
            |row| Cols(row).bool(0),
        )
        .await
    }

    /// Permanent delivery failures recorded for a project, newest first.
    pub async fn delivery_failures(
        &self,
        project: ProjectId,
        before: Option<Timestamp>,
        limit: u32,
    ) -> StoreResult<Vec<Event>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {EVENT_FIELDS} FROM events
                      WHERE project_id = ?1 AND kind = 'delivery_failure' AND received_at < ?2
                      ORDER BY received_at DESC, id DESC LIMIT ?3"
            ),
            params![
                project.get(),
                before.map_or(i64::MAX, Timestamp::as_millis),
                i64::from(limit)
            ],
            decode_event,
        )
        .await
    }

    /// Checks that the lease is still current and has not been interrupted.
    pub async fn check_running(&self, lease: &Lease) -> StoreResult<()> {
        let conn = self.db.reader()?;
        let current = query_opt(
            &conn,
            "SELECT lease_epoch, lease_owner, cancelled_epoch FROM lanes WHERE id = ?1",
            params![lease.lane_id.get()],
            |row| {
                let c = Cols(row);
                Ok((c.i64(0)?, c.opt_id::<MachineId>(1)?, c.opt_i64(2)?))
            },
        )
        .await?
        .ok_or_else(|| StoreError::not_found("lane", lease.lane_id))?;
        match current {
            (epoch, Some(owner), cancelled)
                if epoch == lease.epoch && owner == lease.machine_id =>
            {
                if cancelled == Some(lease.epoch) {
                    return Err(StoreError::Stopped);
                }
                Ok(())
            }
            (epoch, _, _) => Err(StoreError::StaleLease {
                lane_id: lease.lane_id,
                held: lease.epoch,
                current: epoch,
            }),
        }
    }

    /// Whether `lane_id` has at least one pending event without loading payloads.
    pub async fn has_pending_events(&self, lane_id: LaneId) -> StoreResult<bool> {
        let conn = self.db.reader()?;
        query_one(
            &conn,
            "SELECT EXISTS(SELECT 1 FROM events WHERE lane_id = ?1 AND status = 'pending')",
            params![lane_id.get()],
            |row| Cols(row).bool(0),
        )
        .await
    }
}

fn ttl_millis(ttl: Duration) -> StoreResult<i64> {
    i64::try_from(ttl.as_millis()).map_err(|_| StoreError::Invalid(format!("lease ttl {ttl:?}")))
}

#[async_trait]
impl InboxStore for HubDb {
    async fn lane(&self, id: LaneId) -> StoreResult<Option<Lane>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {LANE_FIELDS} FROM lanes WHERE id = ?1"),
            params![id.get()],
            decode_lane,
        )
        .await
    }

    async fn insert_event(&self, event: &NewEvent) -> StoreResult<Inserted> {
        let tx = self.db.begin().await?;
        let inserted = insert_event_in(tx.conn(), event).await?;
        tx.commit().await?;
        Ok(inserted)
    }

    async fn event(&self, id: EventId) -> StoreResult<Option<Event>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {EVENT_FIELDS} FROM events WHERE id = ?1"),
            params![id.get()],
            decode_event,
        )
        .await
    }

    async fn source_event_payload(
        &self,
        source: &str,
        source_event_id: &str,
    ) -> StoreResult<Option<Json>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            "SELECT payload FROM events WHERE source = ?1 AND source_event_id = ?2",
            params![source, source_event_id],
            |row| Cols(row).json(0),
        )
        .await
    }

    async fn pending_events(&self, lane_id: LaneId) -> StoreResult<Vec<Event>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {EVENT_FIELDS} FROM events WHERE lane_id = ?1 AND status = 'pending' ORDER BY id"
            ),
            params![lane_id.get()],
            decode_event,
        )
        .await
    }

    async fn skip_events(
        &self,
        lease: &Lease,
        events: &[EventId],
        reason: &str,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        check_lease(tx.conn(), lease).await?;
        check_running(tx.conn(), lease).await?;
        for event in events {
            let changed = exec(
                tx.conn(),
                "UPDATE events SET status = ?1, status_reason = ?2
                 WHERE id = ?3 AND lane_id = ?4 AND status = 'pending'",
                params![
                    EventStatus::Skipped.as_str(),
                    reason,
                    event.get(),
                    lease.lane_id.get()
                ],
            )
            .await?;
            if changed != 1 {
                return Err(StoreError::Conflict(format!(
                    "event {event} is not pending in lane {}",
                    lease.lane_id
                )));
            }
        }
        tx.commit().await
    }

    async fn ready_lanes(&self, now: Timestamp) -> StoreResult<Vec<LaneId>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            "SELECT l.id FROM lanes l
             JOIN projects p ON p.id = l.project_id
             JOIN events e ON e.lane_id = l.id AND e.status = 'pending'
             WHERE p.archived_at IS NULL AND l.stopped = 0 AND (p.paused = 0 OR l.kind = 'task')
               AND (l.lease_owner IS NULL OR l.lease_expires_at <= ?1)
               AND (l.backoff_until IS NULL OR l.backoff_until <= ?1)
               AND NOT (l.kind = 'task' AND EXISTS (
                   SELECT 1 FROM decisions d WHERE d.lane_id = l.id AND d.status = 'pending'))
             GROUP BY l.id
             ORDER BY MIN(CASE WHEN l.kind = 'lead' AND e.trust = 'user' THEN 0 ELSE 1 END),
                      MIN(e.id)",
            params![now.as_millis()],
            |row| Cols(row).id(0),
        )
        .await
    }

    async fn grant_lease(
        &self,
        lane_id: LaneId,
        machine_id: MachineId,
        now: Timestamp,
        ttl: Duration,
    ) -> StoreResult<Option<Lease>> {
        let expires_at = Timestamp::from_millis(now.as_millis() + ttl_millis(ttl)?);
        let tx = self.db.begin().await?;
        let lane = query_opt(
            tx.conn(),
            &format!("SELECT {LANE_FIELDS} FROM lanes WHERE id = ?1"),
            params![lane_id.get()],
            decode_lane,
        )
        .await?
        .ok_or_else(|| StoreError::not_found("lane", lane_id))?;
        let live = lane.lease_owner.is_some()
            && lane.lease_expires_at.is_some_and(|expires| expires > now);
        let unavailable = query_one(
            tx.conn(),
            "SELECT l.stopped != 0 OR p.archived_at IS NOT NULL
             FROM lanes l JOIN projects p ON p.id = l.project_id WHERE l.id = ?1",
            params![lane_id.get()],
            |row| Cols(row).bool(0),
        )
        .await?;
        if live || unavailable {
            tx.rollback().await?;
            return Ok(None);
        }
        let (eligible, reason_changed) =
            task::check_eligible(tx.conn(), lane_id, machine_id, now).await?;
        if !eligible {
            if reason_changed {
                tx.commit().await?;
            } else {
                tx.rollback().await?;
            }
            return Ok(None);
        }
        let full = query_one(
            tx.conn(),
            "SELECT (SELECT COUNT(*) FROM lanes WHERE lease_owner = ?1 AND lease_expires_at > ?2)
                >= COALESCE(slot_limit, max_parallel) FROM machines WHERE id = ?1",
            params![machine_id.get(), now.as_millis()],
            |row| Cols(row).bool(0),
        )
        .await?;
        if full {
            if reason_changed {
                tx.commit().await?;
            } else {
                tx.rollback().await?;
            }
            return Ok(None);
        }
        if lane.lease_owner.is_some() {
            // Taking over an expired lease ends the previous holder's run.
            reclaim_lane(tx.conn(), lane_id, now).await?;
        }
        let epoch = query_one(
            tx.conn(),
            "UPDATE lanes SET lease_owner = ?1, lease_expires_at = ?2,
                 lease_epoch = lease_epoch + 1, last_machine_id = ?1
             WHERE id = ?3 RETURNING lease_epoch",
            params![machine_id.get(), expires_at.as_millis(), lane_id.get()],
            |row| Cols(row).i64(0),
        )
        .await?;
        tx.commit().await?;
        Ok(Some(Lease {
            lane_id,
            machine_id,
            epoch,
            expires_at,
        }))
    }

    async fn renew_lease(
        &self,
        lease: &Lease,
        now: Timestamp,
        ttl: Duration,
    ) -> StoreResult<Lease> {
        let expires_at = Timestamp::from_millis(now.as_millis() + ttl_millis(ttl)?);
        let tx = self.db.begin().await?;
        check_lease(tx.conn(), lease).await?;
        exec(
            tx.conn(),
            "UPDATE lanes SET lease_expires_at = ?1 WHERE id = ?2",
            params![expires_at.as_millis(), lease.lane_id.get()],
        )
        .await?;
        tx.commit().await?;
        Ok(Lease {
            expires_at,
            ..*lease
        })
    }

    async fn release_lease(&self, lease: &Lease) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        match check_lease(tx.conn(), lease).await {
            Ok(()) => {}
            Err(StoreError::StaleLease { .. }) => return Ok(()),
            Err(other) => return Err(other),
        }
        reclaim_lane(tx.conn(), lease.lane_id, self.now()).await?;
        exec(
            tx.conn(),
            "UPDATE lanes SET lease_owner = NULL, lease_expires_at = NULL WHERE id = ?1",
            params![lease.lane_id.get()],
        )
        .await?;
        tx.commit().await
    }

    async fn expire_leases(&self, now: Timestamp) -> StoreResult<Vec<LaneId>> {
        let has_expired = query_one(
            &self.db.reader()?,
            "SELECT EXISTS(SELECT 1 FROM lanes WHERE lease_owner IS NOT NULL AND lease_expires_at <= ?1)",
            params![now.as_millis()],
            |row| Cols(row).bool(0),
        )
        .await?;
        if !has_expired {
            return Ok(Vec::new());
        }
        let tx = self.db.begin().await?;
        let expired: Vec<LaneId> = query_all(
            tx.conn(),
            "SELECT id FROM lanes WHERE lease_owner IS NOT NULL AND lease_expires_at <= ?1 ORDER BY id",
            params![now.as_millis()],
            |row| Cols(row).id(0),
        )
        .await?;
        if expired.is_empty() {
            tx.rollback().await?;
            return Ok(expired);
        }
        for lane_id in &expired {
            reclaim_lane(tx.conn(), *lane_id, now).await?;
            exec(
                tx.conn(),
                "UPDATE lanes SET lease_owner = NULL, lease_expires_at = NULL WHERE id = ?1",
                params![lane_id.get()],
            )
            .await?;
        }
        tx.commit().await?;
        tracing::info!(count = expired.len(), "expired lane leases");
        Ok(expired)
    }

    async fn source_cursor(&self, source: &str) -> StoreResult<Option<String>> {
        let conn = self.db.reader()?;
        let cursor = query_opt(
            &conn,
            "SELECT cursor FROM source_cursors WHERE source = ?1",
            params![source],
            |row| Cols(row).opt_text(0),
        )
        .await?;
        Ok(cursor.flatten())
    }

    async fn set_source_cursor(
        &self,
        source: &str,
        cursor: &str,
        now: Timestamp,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "INSERT INTO source_cursors (source, cursor, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (source) DO UPDATE SET cursor = excluded.cursor,
                 updated_at = excluded.updated_at",
            params![source, cursor, now.as_millis()],
        )
        .await?;
        tx.commit().await
    }

    async fn user_messages(
        &self,
        lane_id: LaneId,
        before: Option<Timestamp>,
        limit: u32,
    ) -> StoreResult<Vec<Event>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {EVENT_FIELDS} FROM events
                 WHERE lane_id = ?1 AND kind = ?2 AND received_at < ?3
                 ORDER BY received_at DESC, id DESC LIMIT ?4"
            ),
            params![
                lane_id.get(),
                EventKind::UserMessage.as_str(),
                before.map_or(i64::MAX, Timestamp::as_millis),
                i64::from(limit)
            ],
            decode_event,
        )
        .await
    }

    async fn lane_events_from(
        &self,
        lane_id: LaneId,
        kind: EventKind,
        source: &str,
        before: Option<Timestamp>,
        limit: u32,
    ) -> StoreResult<Vec<Event>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {EVENT_FIELDS} FROM events
                 WHERE lane_id = ?1 AND kind = ?2 AND source = ?3 AND received_at < ?4
                 ORDER BY received_at DESC, id DESC LIMIT ?5"
            ),
            params![
                lane_id.get(),
                kind.as_str(),
                source,
                before.map_or(i64::MAX, Timestamp::as_millis),
                i64::from(limit)
            ],
            decode_event,
        )
        .await
    }

    async fn skipped_events(
        &self,
        project_id: Option<ProjectId>,
        [reason, additional_reason]: [&str; 2],
        limit: u32,
    ) -> StoreResult<(Vec<Event>, u64)> {
        let conn = self.db.reader()?;
        let project = opt_raw(project_id, ProjectId::get);
        let filter =
            "status = ?1 AND status_reason IN (?2, ?3) AND (?4 IS NULL OR project_id = ?4)";
        let events = query_all(
            &conn,
            &format!(
                "SELECT {EVENT_FIELDS} FROM events WHERE {filter}
                 ORDER BY received_at DESC, id DESC LIMIT ?5"
            ),
            params![
                EventStatus::Skipped.as_str(),
                reason,
                additional_reason,
                project.clone(),
                i64::from(limit)
            ],
            decode_event,
        )
        .await?;
        let total = query_one(
            &conn,
            &format!("SELECT COUNT(*) FROM events WHERE {filter}"),
            params![
                EventStatus::Skipped.as_str(),
                reason,
                additional_reason,
                project
            ],
            |row| Cols(row).i64(0),
        )
        .await?;
        Ok((events, u64::try_from(total).unwrap_or(0)))
    }
}
