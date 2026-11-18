//! [`SessionStore`] on the hub database: sessions, runs, tool calls (Plan.md §4).

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::ids::{EventId, LaneId, ProjectId, RunId, SessionId};
use threadlane_core::model::{
    Event, EventStatus, FailedRun, JournalEntry, LaneKind, Lease, NewOutboxMessage, OutboxKind,
    RetryPolicy, Run, RunOutcome, RunStatus, Session, SessionChoice, SessionStatus, StartRun,
    ToolCallBegin, ToolCallKey, ToolCallStatus, Urgency,
};
use threadlane_core::store::{SessionStore, StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::inbox::{EVENT_FIELDS, check_lease, check_running, decode_event, reclaim_lane};
use crate::outbox::{enqueue_in, enqueue_lead_reply_in};
use crate::row::{Cols, json_text, opt_raw, params};
use crate::task;

/// Channel that system notices about dead work in a lead lane go to.
pub(crate) const NOTICE_CHANNEL: &str = "client";

pub(crate) const SESSION_FIELDS: &str =
    "id, lane_id, agent_id, backend, native_session_id, machine_id,
    status, created_at, last_active_at";

pub(crate) fn decode_session(row: &Row) -> StoreResult<Session> {
    let c = Cols(row);
    Ok(Session {
        id: c.id(0)?,
        lane_id: c.id(1)?,
        agent_id: c.opt_id(2)?,
        backend: c.opt_parse(3)?,
        native_session_id: c.opt_text(4)?,
        machine_id: c.opt_id(5)?,
        status: c.parse(6)?,
        created_at: c.ts(7)?,
        last_active_at: c.ts(8)?,
    })
}

pub(crate) const RUN_FIELDS: &str =
    "id, lane_id, session_id, machine_id, lease_epoch, agent_id, backend,
    status, outcome, rationale, error, started_at, ended_at, trace_id";

pub(crate) fn decode_run(row: &Row) -> StoreResult<Run> {
    let c = Cols(row);
    Ok(Run {
        id: c.id(0)?,
        lane_id: c.id(1)?,
        session_id: c.id(2)?,
        machine_id: c.id(3)?,
        lease_epoch: c.i64(4)?,
        agent_id: c.opt_id(5)?,
        backend: c.opt_parse(6)?,
        status: c.parse(7)?,
        outcome: c.opt_parse(8)?,
        rationale: c.opt_text(9)?,
        error: c.opt_text(10)?,
        started_at: c.ts(11)?,
        ended_at: c.opt_ts(12)?,
        trace_id: c.text(13)?,
    })
}

/// The run, which must be active and belong to the lease's lane and epoch.
async fn active_run(conn: &Connection, lease: &Lease, run_id: RunId) -> StoreResult<Run> {
    let run = query_opt(
        conn,
        &format!("SELECT {RUN_FIELDS} FROM runs WHERE id = ?1"),
        params![run_id.get()],
        decode_run,
    )
    .await?
    .ok_or_else(|| StoreError::not_found("run", run_id))?;
    if run.lane_id != lease.lane_id || run.lease_epoch != lease.epoch {
        return Err(StoreError::Conflict(format!(
            "run {run_id} does not belong to lease epoch {} of lane {}",
            lease.epoch, lease.lane_id
        )));
    }
    if run.status != RunStatus::Active {
        return Err(StoreError::Conflict(format!(
            "run {run_id} is {}, not active",
            run.status
        )));
    }
    Ok(run)
}

/// The idempotency key of the notice about a run's dead events, its
/// `tool_call_id`. One run can be failed only once, and the key makes a
/// second notice a no-op.
fn dead_key(run_id: RunId) -> String {
    format!("dead:{run_id}")
}

/// The one plain sentence a system notice about dead lead-lane work says.
fn dead_notice_text(project: &str, attempts: u32, error: &str) -> String {
    let attempts = if attempts == 1 {
        "1 attempt".to_owned()
    } else {
        format!("{attempts} attempts")
    };
    let error = error.trim().trim_end_matches('.');
    if error.is_empty() {
        format!("I could not finish something you asked for in {project} after {attempts}.")
    } else {
        format!(
            "I could not finish something you asked for in {project} after {attempts}: {error}."
        )
    }
}

/// Makes sure you hear about events that died in a failed run of a lead
/// lane, where no agent is left to tell you (Plan.md §4.9): a system notice
/// is queued. A task lane reports through its task's result instead.
async fn report_dead(
    conn: &Connection,
    run: &Run,
    attempts: u32,
    error: &str,
    now: Timestamp,
) -> StoreResult<()> {
    let (kind, project_id, project_name) = query_one(
        conn,
        "SELECT l.kind, p.id, p.name FROM lanes l JOIN projects p ON p.id = l.project_id
         WHERE l.id = ?1",
        params![run.lane_id.get()],
        |row| {
            let c = Cols(row);
            Ok((c.parse::<LaneKind>(0)?, c.id::<ProjectId>(1)?, c.text(2)?))
        },
    )
    .await?;
    if kind != LaneKind::Lead {
        return Ok(());
    }
    enqueue_in(
        conn,
        &NewOutboxMessage {
            project_id: Some(project_id),
            lane_id: Some(run.lane_id),
            session_id: None,
            run_id: Some(run.id),
            tool_call_id: Some(dead_key(run.id)),
            kind: OutboxKind::SystemNotice,
            decision_id: None,
            channel: NOTICE_CHANNEL.to_owned(),
            body: json!({ "text": dead_notice_text(&project_name, attempts, error) }),
            urgency: Urgency::High,
            not_before: None,
            created_at: now,
        },
    )
    .await?;
    Ok(())
}

async fn claimed_events(conn: &Connection, run_id: RunId) -> StoreResult<Vec<(EventId, u32)>> {
    query_all(
        conn,
        "SELECT id, attempts FROM events WHERE claimed_by_run = ?1 AND status = 'claimed' ORDER BY id",
        params![run_id.get()],
        |row| {
            let c = Cols(row);
            Ok((c.id(0)?, c.u32(1)?))
        },
    )
    .await
}

fn decode_journal_entry(row: &Row) -> StoreResult<JournalEntry> {
    let c = Cols(row);
    Ok(JournalEntry {
        run_id: c.id(0)?,
        seq: c.i64(1)?,
        at: c.ts(2)?,
        entry: c.json(3)?,
    })
}

#[async_trait]
impl SessionStore for HubDb {
    async fn session(&self, id: SessionId) -> StoreResult<Option<Session>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {SESSION_FIELDS} FROM sessions WHERE id = ?1"),
            params![id.get()],
            decode_session,
        )
        .await
    }

    async fn latest_open_session(&self, lane_id: LaneId) -> StoreResult<Option<Session>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!(
                "SELECT {SESSION_FIELDS} FROM sessions WHERE lane_id = ?1 AND status = 'open'
                 ORDER BY last_active_at DESC, id DESC LIMIT 1"
            ),
            params![lane_id.get()],
            decode_session,
        )
        .await
    }

    async fn set_native_session(
        &self,
        lease: &Lease,
        session_id: SessionId,
        native_session_id: &str,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        check_lease(tx.conn(), lease).await?;
        let changed = exec(
            tx.conn(),
            "UPDATE sessions SET native_session_id = ?1 WHERE id = ?2 AND lane_id = ?3",
            params![native_session_id, session_id.get(), lease.lane_id.get()],
        )
        .await?;
        if changed != 1 {
            return Err(StoreError::not_found("session", session_id));
        }
        tx.commit().await
    }

    async fn close_session(&self, session_id: SessionId) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        exec(
            tx.conn(),
            "UPDATE sessions SET status = ?1 WHERE id = ?2",
            params![SessionStatus::Closed.as_str(), session_id.get()],
        )
        .await?;
        tx.commit().await
    }

    async fn start_run(&self, lease: &Lease, start: &StartRun) -> StoreResult<Run> {
        if start.events.is_empty() {
            return Err(StoreError::Invalid("a run needs at least one event".into()));
        }
        let now = start.started_at.as_millis();
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        check_running(conn, lease).await?;

        let session_id: SessionId = match start.session {
            SessionChoice::New => query_one(
                conn,
                concat!("INSERT INTO sessions (id, lane_id, agent_id, backend, native_session_id, machine_id,
                         status, created_at, last_active_at)
                     VALUES (", crate::db::next_id_sql!("sessions"), ", ?1, ?2, ?3, ?4, ?5, 'open', ?6, ?6) RETURNING id"),
                params![
                    lease.lane_id.get(),
                    opt_raw(start.agent_id, |id| id.get()),
                    start.backend.map(|backend| backend.as_str()),
                    start.native_session_id.as_deref(),
                    lease.machine_id.get(),
                    now,
                ],
                |row| Cols(row).id(0),
            )
            .await?,
            SessionChoice::Resume(id) => {
                let changed = exec(
                    conn,
                    "UPDATE sessions SET machine_id = ?1, last_active_at = ?2
                     WHERE id = ?3 AND lane_id = ?4 AND status = 'open'",
                    params![lease.machine_id.get(), now, id.get(), lease.lane_id.get()],
                )
                .await?;
                if changed != 1 {
                    return Err(StoreError::Conflict(format!(
                        "session {id} is not open in lane {}",
                        lease.lane_id
                    )));
                }
                id
            }
        };

        let run_id: RunId = query_one(
            conn,
            concat!("INSERT INTO runs (id, lane_id, session_id, machine_id, lease_epoch, agent_id, backend, status, started_at)
             VALUES (", crate::db::next_id_sql!("runs"), ", ?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7) RETURNING id"),
            params![
                lease.lane_id.get(),
                session_id.get(),
                lease.machine_id.get(),
                lease.epoch,
                opt_raw(start.agent_id, |id| id.get()),
                start.backend.map(|backend| backend.as_str()),
                now,
            ],
            |row| Cols(row).id(0),
        )
        .await?;

        claim_events(conn, lease.lane_id, run_id, &start.events).await?;
        if crate::dreaming::dream_run_in(conn, run_id).await? {
            if start.events.len() != 1 || start.session != SessionChoice::New {
                return Err(StoreError::Invalid(
                    "a dream needs its own session and event".to_owned(),
                ));
            }
            // A dream's native session is never a candidate for ordinary lead work.
            exec(
                conn,
                "UPDATE sessions SET status = 'closed' WHERE id = ?1",
                params![session_id.get()],
            )
            .await?;
        }
        let run = query_one(
            conn,
            &format!(
                "UPDATE runs SET trace_id = (
                    SELECT e.trace_id FROM run_events re JOIN events e ON e.id = re.event_id
                    WHERE re.run_id = ?1 ORDER BY e.received_at, e.id LIMIT 1
                 ) WHERE id = ?1 RETURNING {RUN_FIELDS}"
            ),
            params![run_id.get()],
            decode_run,
        )
        .await?;
        task::on_run_started(conn, lease.lane_id, lease.machine_id).await?;
        tx.commit().await?;
        Ok(run)
    }

    async fn run(&self, id: RunId) -> StoreResult<Option<Run>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            &format!("SELECT {RUN_FIELDS} FROM runs WHERE id = ?1"),
            params![id.get()],
            decode_run,
        )
        .await
    }

    async fn run_events(&self, run_id: RunId) -> StoreResult<Vec<Event>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {EVENT_FIELDS} FROM events
                 WHERE id IN (SELECT event_id FROM run_events WHERE run_id = ?1)
                 ORDER BY id"
            ),
            params![run_id.get()],
            decode_event,
        )
        .await
    }

    async fn commit_run(
        &self,
        lease: &Lease,
        run_id: RunId,
        outcome: RunOutcome,
        rationale: Option<&str>,
        reply: Option<&threadlane_core::store::LeadReply>,
        ended_at: Timestamp,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        check_running(conn, lease).await?;
        let run = active_run(conn, lease, run_id).await?;
        if let Some(reply) = reply {
            enqueue_lead_reply_in(conn, reply).await?;
        }
        exec(
            conn,
            "UPDATE runs SET status = ?1, outcome = ?2, rationale = ?3, ended_at = ?4 WHERE id = ?5",
            params![
                RunStatus::Committed.as_str(),
                outcome.as_str(),
                rationale,
                ended_at.as_millis(),
                run_id.get(),
            ],
        )
        .await?;
        exec(
            conn,
            "UPDATE events SET status = ?1 WHERE claimed_by_run = ?2 AND status = 'claimed'",
            params![EventStatus::Done.as_str(), run_id.get()],
        )
        .await?;
        exec(
            conn,
            "UPDATE sessions SET last_active_at = ?1 WHERE id = ?2",
            params![ended_at.as_millis(), run.session_id.get()],
        )
        .await?;
        task::on_run_committed(conn, lease.lane_id, run_id, ended_at).await?;
        tx.commit().await
    }

    async fn fail_run(
        &self,
        lease: &Lease,
        run_id: RunId,
        error: &str,
        retry: RetryPolicy,
        ended_at: Timestamp,
    ) -> StoreResult<FailedRun> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        check_running(conn, lease).await?;
        let run = active_run(conn, lease, run_id).await?;
        exec(
            conn,
            "UPDATE runs SET status = ?1, error = ?2, ended_at = ?3 WHERE id = ?4",
            params![
                RunStatus::Failed.as_str(),
                error,
                ended_at.as_millis(),
                run_id.get()
            ],
        )
        .await?;
        let mut failed = FailedRun::default();
        let mut dead_attempts = 0;
        for (event_id, attempts) in claimed_events(conn, run_id).await? {
            let attempts = if retry.count_attempt {
                attempts.saturating_add(1)
            } else {
                attempts
            };
            if retry.count_attempt && attempts >= retry.max_attempts {
                exec(
                    conn,
                    "UPDATE events SET status = ?1, attempts = ?2, claimed_by_run = NULL, status_reason = ?3
                     WHERE id = ?4",
                    params![
                        EventStatus::Dead.as_str(),
                        i64::from(attempts),
                        error,
                        event_id.get()
                    ],
                )
                .await?;
                failed.dead.push(event_id);
                dead_attempts = dead_attempts.max(attempts);
            } else {
                exec(
                    conn,
                    "UPDATE events SET status = ?1, attempts = ?2, claimed_by_run = NULL WHERE id = ?3",
                    params![
                        EventStatus::Pending.as_str(),
                        i64::from(attempts),
                        event_id.get()
                    ],
                )
                .await?;
                failed.retried.push(event_id);
            }
        }
        exec(
            conn,
            "UPDATE lanes SET backoff_until = ?1 WHERE id = ?2",
            params![retry.backoff_until.as_millis(), lease.lane_id.get()],
        )
        .await?;
        task::on_run_failed(
            conn,
            lease.lane_id,
            !failed.dead.is_empty(),
            error,
            ended_at,
        )
        .await?;
        if !failed.dead.is_empty() && !crate::dreaming::dream_run_in(conn, run_id).await? {
            report_dead(conn, &run, dead_attempts, error, ended_at).await?;
        }
        tx.commit().await?;
        Ok(failed)
    }

    async fn begin_tool_call(
        &self,
        lease: &Lease,
        key: &ToolCallKey,
        tool: &str,
        arguments: &Json,
        safe_to_repeat: bool,
        now: Timestamp,
    ) -> StoreResult<ToolCallBegin> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        check_running(conn, lease).await?;
        let run = active_run(conn, lease, key.run_id).await?;
        let existing = query_opt(
            conn,
            "SELECT status, result, safe_to_repeat, tool, arguments FROM tool_calls WHERE run_id = ?1 AND call_key = ?2",
            params![key.run_id.get(), key.key.as_str()],
            |row| {
                let c = Cols(row);
                let status = c.parse::<ToolCallStatus>(0)?;
                if status == ToolCallStatus::Started && (c.text(3)? != tool || c.json(4)? != *arguments) {
                    return Err(StoreError::Conflict("tool call id reused with different arguments".into()));
                }
                Ok((
                    status,
                    c.opt_json(1)?,
                    c.bool(2)?,
                ))
            },
        )
        .await?;
        let begin = match existing {
            None => {
                exec(
                    conn,
                    concat!("INSERT INTO tool_calls (id, run_id, call_key, tool, arguments, safe_to_repeat, status, started_at, trace_id)
                     VALUES (", crate::db::next_id_sql!("tool_calls"), ", ?1, ?2, ?3, ?4, ?5, 'started', ?6, ?7)"),
                    params![
                        key.run_id.get(),
                        key.key.as_str(),
                        tool,
                        json_text(arguments)?,
                        safe_to_repeat,
                        now.as_millis(),
                        run.trace_id.as_str(),
                    ],
                )
                .await?;
                ToolCallBegin::Perform
            }
            Some((ToolCallStatus::Finished, result, _)) => {
                ToolCallBegin::Finished(result.unwrap_or(Json::Null))
            }
            Some((ToolCallStatus::Started, _, true)) if safe_to_repeat => ToolCallBegin::Perform,
            Some((ToolCallStatus::Started, _, _)) => ToolCallBegin::OutcomeUnknown,
        };
        tx.commit().await?;
        Ok(begin)
    }

    async fn finish_tool_call(
        &self,
        lease: &Lease,
        key: &ToolCallKey,
        result: &Json,
        now: Timestamp,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        active_run(conn, lease, key.run_id).await?;
        let changed = exec(
            conn,
            "UPDATE tool_calls SET status = 'finished', result = ?1, finished_at = ?2
             WHERE run_id = ?3 AND call_key = ?4 AND status = 'started'",
            params![
                json_text(result)?,
                now.as_millis(),
                key.run_id.get(),
                key.key.as_str()
            ],
        )
        .await?;
        if changed != 1 {
            return Err(StoreError::Conflict(format!(
                "tool call {} in run {} was not started or already finished",
                key.key, key.run_id
            )));
        }
        tx.commit().await
    }

    async fn append_journal(&self, lease: &Lease, entries: &[JournalEntry]) -> StoreResult<()> {
        if let Some(entry) = entries.iter().find(|entry| entry.seq < 1) {
            return Err(StoreError::Invalid(format!(
                "journal entry {} of run {} is numbered below 1",
                entry.seq, entry.run_id
            )));
        }
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        // The last stored sequence number of each run seen in this batch.
        let mut last: BTreeMap<RunId, i64> = BTreeMap::new();
        for entry in entries {
            let stored = match last.entry(entry.run_id) {
                Entry::Occupied(stored) => stored.into_mut(),
                Entry::Vacant(vacant) => {
                    active_run(conn, lease, entry.run_id).await?;
                    let seq = query_one(
                        conn,
                        "SELECT COALESCE(MAX(seq), 0) FROM journal WHERE run_id = ?1",
                        params![entry.run_id.get()],
                        |row| Cols(row).i64(0),
                    )
                    .await?;
                    vacant.insert(seq)
                }
            };
            if entry.seq <= *stored {
                continue;
            }
            if entry.seq != *stored + 1 {
                return Err(StoreError::Conflict(format!(
                    "journal entry {} of run {} would leave a gap after {}",
                    entry.seq, entry.run_id, stored
                )));
            }
            exec(
                conn,
                "INSERT INTO journal (run_id, seq, at, entry) VALUES (?1, ?2, ?3, ?4)",
                params![
                    entry.run_id.get(),
                    entry.seq,
                    entry.at.as_millis(),
                    json_text(&entry.entry)?,
                ],
            )
            .await?;
            *stored = entry.seq;
        }
        tx.commit().await
    }

    async fn journal(&self, run_id: RunId) -> StoreResult<Vec<JournalEntry>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            "SELECT run_id, seq, at, entry FROM journal WHERE run_id = ?1 ORDER BY seq",
            params![run_id.get()],
            decode_journal_entry,
        )
        .await
    }

    async fn lane_runs(&self, lane_id: LaneId) -> StoreResult<Vec<Run>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!("SELECT {RUN_FIELDS} FROM runs WHERE lane_id = ?1 ORDER BY id"),
            params![lane_id.get()],
            decode_run,
        )
        .await
    }
}

impl HubDb {
    /// Reads one journal entry, optionally selecting the newest one of a given backend kind.
    pub async fn latest_journal_entry(
        &self,
        run_id: RunId,
        kind: Option<&str>,
    ) -> StoreResult<Option<JournalEntry>> {
        let conn = self.db.reader()?;
        query_opt(
            &conn,
            "SELECT run_id, seq, at, entry FROM journal
             WHERE run_id = ?1 AND (?2 IS NULL OR json_extract(entry, '$.type') = ?2)
             ORDER BY seq DESC LIMIT 1",
            params![run_id.get(), kind],
            decode_journal_entry,
        )
        .await
    }

    /// Records an interrupted run and restores its claimed events without charging attempts.
    pub async fn stop_run(&self, lease: &Lease, run_id: RunId, now: Timestamp) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        active_run(conn, lease, run_id).await?;
        match check_running(conn, lease).await {
            Err(StoreError::Stopped) => {}
            Err(error) => return Err(error),
            Ok(()) => {
                return Err(StoreError::Invalid(
                    "run was not stopped by the user".to_owned(),
                ));
            }
        }
        reclaim_lane(conn, lease.lane_id, now).await?;
        tx.commit().await
    }
}

async fn claim_events(
    conn: &Connection,
    lane_id: LaneId,
    run_id: RunId,
    events: &[EventId],
) -> StoreResult<()> {
    for event in events {
        let changed = exec(
            conn,
            "UPDATE events SET status = 'claimed', claimed_by_run = ?1
                 WHERE id = ?2 AND lane_id = ?3 AND status = 'pending'",
            params![run_id.get(), event.get(), lane_id.get()],
        )
        .await?;
        if changed != 1 {
            return Err(StoreError::Conflict(format!(
                "event {event} is not pending in lane {}",
                lane_id
            )));
        }
        exec(
            conn,
            "INSERT INTO run_events (run_id, event_id) VALUES (?1, ?2)",
            params![run_id.get(), event.get()],
        )
        .await?;
    }
    Ok(())
}

impl HubDb {
    /// Claims user events accepted during a run under its current, uninterrupted lease.
    pub async fn fold_in(
        &self,
        lease: &Lease,
        run_id: RunId,
        events: &[EventId],
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        check_running(conn, lease).await?;
        let run = active_run(conn, lease, run_id).await?;
        if crate::dreaming::dream_run_in(conn, run_id).await? {
            return Err(StoreError::Invalid(
                "a dream cannot accept user input".to_owned(),
            ));
        }
        for id in events {
            let eligible = query_one(
                conn,
                "SELECT EXISTS (SELECT 1 FROM events WHERE id = ?1 AND kind = 'user_message'
                 AND trust = 'user' AND (session_ref IS NULL OR session_ref = ?2))",
                params![id.get(), run.session_id.get()],
                |row| Cols(row).bool(0),
            )
            .await?;
            if !eligible {
                return Err(StoreError::Invalid(format!(
                    "event {id} cannot be folded into run {run_id}"
                )));
            }
        }
        claim_events(conn, lease.lane_id, run_id, events).await?;
        tx.commit().await
    }
}
