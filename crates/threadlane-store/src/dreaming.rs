//! Durable dreaming configuration and history over timers, runs and memory versions.

use serde_json::{Value as Json, json};
use threadlane_core::client::{DreamChange, DreamSummary};
use threadlane_core::ids::{EventId, LaneId, ProjectId, RunId, TimerId};
use threadlane_core::model::{
    DecisionAnswer, DecisionOption, Lease, NewTimer, Recurrence, Task, TimerCategory, TimerCreator,
};
use threadlane_core::store::StoreResult;
use threadlane_core::time::Timestamp;
use turso::Connection;

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::row::{Cols, opt_ts, params};
use crate::timer::insert_timer_in;

/// Maximum records of each kind reviewed per occurrence. Unread rows sort first next time.
const REVIEW_LIMIT: usize = 200;

/// The last activity of task row `t`: its creation, the latest event or run in
/// its lane, or its latest result or retry event in the lead's lane. Tasks keep
/// no update time of their own, and every status change leaves one of these.
fn task_updated_at() -> String {
    let source = crate::task::TASK_SOURCE;
    format!(
        "MAX(t.created_at,
        COALESCE((SELECT MAX(e.received_at) FROM events e WHERE e.lane_id=t.lane_id),0),
        COALESCE((SELECT MAX(COALESCE(r.ended_at,r.started_at)) FROM runs r WHERE r.lane_id=t.lane_id),0),
        COALESCE((SELECT MAX(e.received_at) FROM events e WHERE e.source='{source}'
            AND (e.source_event_id='result:'||t.id
                OR (e.source_event_id>='result:'||t.id||':' AND e.source_event_id<'result:'||t.id||';')
                OR (e.source_event_id>='retry:'||t.id||':' AND e.source_event_id<'retry:'||t.id||';'))),0))"
    )
}

const CHOICES: &str =
    "FROM decisions d JOIN events e ON e.source='decision' AND e.source_event_id='answer:' || d.id
    WHERE d.project_id=?1 AND e.project_id=d.project_id AND d.kind='input' AND d.tool='ask_user'
    AND d.status='answered' AND d.run_id IS NOT NULL AND e.trust='user'";

const RECENT_MESSAGES: &str = "SELECT id FROM events WHERE project_id=?1 AND source='client'
    AND kind='user_message' AND trust='user' ORDER BY id DESC LIMIT 20";

fn recent_choices() -> String {
    format!("SELECT d.id {CHOICES} ORDER BY d.answered_at DESC, d.id DESC LIMIT 20")
}

async fn active_timer(
    conn: &Connection,
    project: ProjectId,
) -> StoreResult<Option<(TimerId, Recurrence)>> {
    query_opt(
        conn,
        "SELECT id, recurrence FROM timers WHERE project_id = ?1 AND created_by = 'system'
         AND category = 'dreaming' AND status = 'active'",
        params![project.get()],
        |row| {
            let c = Cols(row);
            Ok((c.id(0)?, c.de(1)?))
        },
    )
    .await
}

fn dreaming_timer(
    project: ProjectId,
    lane: LaneId,
    cadence: Recurrence,
    due_at: Timestamp,
) -> NewTimer {
    NewTimer {
        project_id: project,
        lane_id: lane,
        agent_id: None,
        task_ref: None,
        category: TimerCategory::Dreaming,
        due_at,
        recurrence: Some(cadence),
        instruction: "Curate project memory".into(),
        created_by: TimerCreator::System,
        created_by_call: None,
        requested: true,
    }
}

/// The event membership is immutable for a dream and survives retries and restarts.
pub(crate) async fn dream_run_in(conn: &Connection, run: RunId) -> StoreResult<bool> {
    query_one(
        conn,
        "SELECT EXISTS (
             SELECT 1 FROM run_events re JOIN events e ON e.id = re.event_id
             WHERE re.run_id = ?1 AND e.kind = 'dream' AND e.source = 'timer' AND e.trust = 'system'
         )",
        params![run.get()],
        |row| Cols(row).bool(0),
    )
    .await
}

async fn last_dream(
    conn: &Connection,
    project: ProjectId,
) -> StoreResult<Option<(RunId, Timestamp, Timestamp)>> {
    query_opt(
        conn,
        "SELECT r.id, r.started_at, r.ended_at FROM runs r
         JOIN lanes l ON l.id = r.lane_id
         WHERE l.project_id = ?1 AND l.kind = 'lead' AND r.status = 'committed' AND EXISTS (
             SELECT 1 FROM run_events re JOIN events e ON e.id = re.event_id
             WHERE re.run_id = r.id AND e.kind = 'dream' AND e.source = 'timer' AND e.trust = 'system'
         ) ORDER BY r.id DESC LIMIT 1",
        params![project.get()],
        |row| {
            let c = Cols(row);
            Ok((c.id(0)?, c.ts(1)?, c.ts(2)?))
        },
    )
    .await
}

/// A periodic full check also finds repository changes made outside the hub.
pub(crate) async fn quiet_since_dream(
    conn: &Connection,
    project: ProjectId,
    now: Timestamp,
) -> StoreResult<bool> {
    let Some((last, _, ended)) = last_dream(conn, project).await? else {
        return Ok(false);
    };
    if now.saturating_since(ended) >= std::time::Duration::from_secs(7 * 24 * 3600) {
        return Ok(false);
    }
    query_one(
        conn,
        "SELECT EXISTS (SELECT 1 FROM dream_batches b JOIN run_events re ON re.event_id=b.event_id
        WHERE b.project_id=?1 AND re.run_id=?2
        AND NOT EXISTS(SELECT 1 FROM dream_items i WHERE i.project_id=?1 AND i.last_review=0
            AND NOT EXISTS(SELECT 1 FROM runs done WHERE done.id=i.seen_run AND done.status='committed'))
        AND NOT EXISTS(SELECT 1 FROM events e WHERE e.project_id=?1 AND e.id>b.event_id AND e.kind!='dream')
        AND NOT EXISTS(SELECT 1 FROM tasks t WHERE t.project_id=?1 AND NOT EXISTS(
            SELECT 1 FROM dream_items i WHERE i.project_id=?1 AND i.kind='task' AND i.item_id=t.id))
        AND NOT EXISTS(SELECT 1 FROM runs r JOIN lanes l ON l.id=r.lane_id WHERE l.project_id=?1 AND r.id>b.run_floor
            AND (r.ended_at>b.since_at OR (r.ended_at=b.since_at AND r.id>b.since_run))
            AND NOT EXISTS(SELECT 1 FROM dream_items i WHERE i.project_id=?1 AND i.kind='run' AND i.item_id=r.id)
            AND NOT EXISTS(SELECT 1 FROM run_events re JOIN events e ON e.id=re.event_id WHERE re.run_id=r.id AND e.kind='dream' AND e.trust='system' AND e.source='timer'))
        AND NOT EXISTS(SELECT 1 FROM memory_versions v WHERE v.id>b.memory_id AND v.path GLOB 'projects/'||?1||'/*'
            AND NOT EXISTS(SELECT 1 FROM run_events re WHERE re.run_id=v.written_by_run AND re.event_id=b.event_id)))",
        params![project.get(), last.get()],
        |r| Cols(r).bool(0),
    ).await
}

impl HubDb {
    /// The active system timer and its cadence, if this project dreams.
    pub async fn dreaming_timer(
        &self,
        project: ProjectId,
    ) -> StoreResult<Option<(TimerId, Recurrence)>> {
        active_timer(&self.db.reader()?, project).await
    }

    /// Gives the project the default cadence if it has never had a dreaming
    /// setting, checked and written in one transaction. Turning dreaming off
    /// leaves its cancelled timer behind, so an explicit off is never replaced.
    pub async fn default_dreaming(
        &self,
        project: ProjectId,
        lane: LaneId,
        cadence: &Recurrence,
        due_at: Timestamp,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let unset = query_one(
            conn,
            "SELECT NOT EXISTS (SELECT 1 FROM timers WHERE project_id = ?1
             AND created_by = 'system' AND category = 'dreaming')",
            params![project.get()],
            |row| Cols(row).bool(0),
        )
        .await?;
        if unset {
            insert_timer_in(
                conn,
                &dreaming_timer(project, lane, cadence.clone(), due_at),
                None,
            )
            .await?;
        }
        tx.commit().await
    }

    /// Changes the project setting and retires pending occurrences in the same transaction.
    pub async fn set_dreaming(
        &self,
        project: ProjectId,
        lane: LaneId,
        schedule: Option<(Recurrence, Timestamp)>,
        now: Timestamp,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let active = active_timer(conn, project).await?;
        if active.as_ref().map(|(_, cadence)| cadence)
            == schedule.as_ref().map(|(cadence, _)| cadence)
        {
            return Ok(());
        }
        if let Some((id, _)) = active {
            exec(
                conn,
                "UPDATE timers SET status = 'cancelled' WHERE id = ?1",
                params![id.get()],
            )
            .await?;
        }
        exec(
            conn,
            "UPDATE events SET status = 'skipped', status_reason = 'dreaming setting changed'
             WHERE project_id = ?1 AND status = 'pending' AND kind = 'dream' AND source = 'timer'",
            params![project.get()],
        )
        .await?;
        if let Some((cadence, due_at)) = schedule {
            let due_at = if active.is_none() { now } else { due_at };
            insert_timer_in(conn, &dreaming_timer(project, lane, cadence, due_at), None).await?;
        }
        tx.commit().await
    }

    /// Defer defaults when a reversal could be outside the Lead's recent evidence,
    /// including pages left unread by a committed dream.
    pub async fn preferences_need_review(&self, project: ProjectId) -> StoreResult<bool> {
        let sql = format!("WITH unread AS (
            SELECT i.kind, i.item_id FROM dream_items i
            WHERE i.project_id=?1 AND i.kind IN ('run','choice') AND i.last_review=0
            AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.id=i.seen_run AND r.status='committed')
        )
        SELECT EXISTS(
            SELECT 1 FROM events e WHERE e.project_id=?1 AND e.trust='user' AND e.kind IN ('user_message','decision_answer')
            AND e.id > (SELECT MAX(re.event_id) FROM run_events re JOIN events d ON d.id=re.event_id
                JOIN runs r ON r.id=re.run_id WHERE r.status='committed' AND d.kind='dream' AND d.source='timer'
                AND d.trust='system' AND d.project_id=?1) LIMIT 1 OFFSET 20)
            OR EXISTS (SELECT 1 FROM unread WHERE kind='choice' AND item_id NOT IN ({}))
            OR EXISTS (SELECT 1 FROM unread i JOIN run_events re ON re.run_id=i.item_id
                JOIN events e ON e.id=re.event_id WHERE i.kind='run' AND e.project_id=?1
                AND e.source='client' AND e.kind='user_message' AND e.trust='user'
                AND json_type(e.payload,'$.text')='text' AND e.id NOT IN ({RECENT_MESSAGES})
                AND re.run_id=(SELECT MIN(first.run_id) FROM run_events first WHERE first.event_id=e.id))",
            recent_choices());
        query_one(&self.db.reader()?, &sql, params![project.get()], |r| {
            Cols(r).bool(0)
        })
        .await
    }

    /// Owner-answered input cards for the Lead, including older cited cards.
    pub async fn user_choices(&self, project: ProjectId, cited: &[i64]) -> StoreResult<Vec<Json>> {
        let conn = self.db.reader()?;
        let mut ids = query_all(&conn, &recent_choices(), params![project.get()], |row| {
            Cols(row).i64(0)
        })
        .await?;
        ids.extend_from_slice(cited);
        self.choice_records(project, &ids).await
    }

    /// Resolve just the requested decision IDs against first-party answer events.
    pub async fn choice_records(&self, project: ProjectId, ids: &[i64]) -> StoreResult<Vec<Json>> {
        query_all(
            &self.db.reader()?,
            &format!(
                "SELECT d.id, d.run_id, d.question, d.options, d.answer {CHOICES}
            AND d.id IN (SELECT value FROM json_each(?2)) ORDER BY d.answered_at DESC, d.id DESC"
            ),
            params![project.get(), json!(ids).to_string()],
            |row| {
                let c = Cols(row);
                let options: Vec<DecisionOption> = c.de(3)?;
                let answer: DecisionAnswer = c.de(4)?;
                Ok(
                    json!({"decision_id": c.i64(0)?, "run_id": c.i64(1)?, "question": c.text(2)?,
                    "chosen": options.get(answer.option as usize).map(|o| &o.label),
                    "options": options.iter().map(|o| &o.label).collect::<Vec<_>>(),
                    "note": answer.note, "answered_at": answer.answered_at}),
                )
            },
        )
        .await
    }

    /// The committed run is the marker. Include versions from any attempts at that occurrence.
    pub async fn dream_summary(&self, project: ProjectId) -> StoreResult<Option<DreamSummary>> {
        let conn = self.db.reader()?;
        let Some((run, _, at)) = last_dream(&conn, project).await? else {
            return Ok(None);
        };
        let prefix = format!("projects/{}/", project.get());
        let changes = query_all(
            &conn,
            "SELECT path, version, deleted FROM memory_versions
             WHERE substr(path, 1, ?1) = ?2 AND written_by_run IN (
                 SELECT attempt.run_id FROM run_events attempt JOIN run_events dream
                     ON dream.event_id = attempt.event_id WHERE dream.run_id = ?3
             ) ORDER BY id",
            params![
                i64::try_from(prefix.len()).unwrap_or(i64::MAX),
                prefix.as_str(),
                run.get()
            ],
            |row| {
                let c = Cols(row);
                let path = c.text(0)?;
                Ok(DreamChange {
                    path: format!("project/{}", path.strip_prefix(&prefix).unwrap_or(&path)),
                    version: c.i64(1)?,
                    deleted: c.bool(2)?,
                })
            },
        )
        .await?;
        Ok(Some(DreamSummary { at, changes }))
    }

    /// Fix membership for an occurrence. Unread work and failed attempts remain eligible.
    pub async fn prepare_dream(
        &self,
        project: ProjectId,
        event: EventId,
        lease: &Lease,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        crate::inbox::check_lease(conn, lease).await?;
        let prior = query_opt(
            conn,
            "SELECT event_id FROM dream_batches WHERE project_id=?1",
            params![project.get()],
            |r| Cols(r).id::<EventId>(0),
        )
        .await?;
        if prior == Some(event) {
            return tx.commit().await;
        }
        exec(
            conn,
            "UPDATE dream_items SET last_review=seen_run, seen_run=NULL WHERE project_id=?1
            AND seen_run IN (SELECT id FROM runs WHERE status='committed')",
            params![project.get()],
        )
        .await?;
        exec(
            conn,
            "UPDATE dream_items SET seen_run=NULL WHERE project_id=?1 AND seen_run IS NOT NULL",
            params![project.get()],
        )
        .await?;
        // Existing hubs start at the legacy committed marker. Keep that lower bound on subsequent passes.
        let legacy = if prior.is_none() {
            last_dream(conn, project).await?
        } else {
            None
        };
        exec(
            conn,
            "INSERT INTO dream_batches(project_id,event_id,since_at,since_run) VALUES(?1,?2,?3,?4)
            ON CONFLICT(project_id) DO UPDATE SET event_id=excluded.event_id",
            params![
                project.get(),
                event.get(),
                legacy.map_or(i64::MIN, |(_, start, _)| start.as_millis()),
                legacy.map_or(0, |(id, _, _)| id.get())
            ],
        )
        .await?;
        exec(
            conn,
            "UPDATE dream_batches SET memory_id=COALESCE((SELECT MAX(id) FROM memory_versions),0)
             WHERE project_id=?1",
            params![project.get()],
        )
        .await?;
        exec(conn, "INSERT OR IGNORE INTO dream_items(project_id,kind,item_id)
            SELECT l.project_id,'run',r.id FROM runs r JOIN lanes l ON l.id=r.lane_id
            JOIN dream_batches b ON b.project_id=l.project_id
            WHERE l.project_id=?1 AND r.id>b.run_floor AND (r.ended_at>b.since_at OR (r.ended_at=b.since_at AND r.id>b.since_run))
              AND NOT EXISTS (SELECT 1 FROM run_events re JOIN events e ON e.id=re.event_id
                WHERE re.run_id=r.id AND e.kind='dream' AND e.source='timer' AND e.trust='system')",params![project.get()]).await?;
        // A run that started before the dream can still finish after it.
        exec(
            conn,
            "UPDATE dream_batches SET run_floor=COALESCE(
               (SELECT MIN(r.id)-1 FROM runs r JOIN lanes l ON l.id=r.lane_id
                WHERE l.project_id=?1 AND r.status='active'),
               (SELECT MAX(id) FROM runs),0) WHERE project_id=?1",
            params![project.get()],
        )
        .await?;
        exec(
            conn,
            "INSERT OR IGNORE INTO dream_items(project_id,kind,item_id)
            SELECT project_id,'task',id FROM tasks WHERE project_id=?1
             AND id > COALESCE((SELECT MAX(item_id) FROM dream_items WHERE project_id=?1 AND kind='task'),0)",
            params![project.get()],
        )
        .await?;
        exec(
            conn,
            &format!(
                "INSERT OR IGNORE INTO dream_items(project_id,kind,item_id)
            SELECT d.project_id,'choice',d.id {CHOICES} AND e.id > ?2"
            ),
            params![project.get(), prior.map_or(0, EventId::get)],
        )
        .await?;
        tx.commit().await
    }

    /// Read a stable page of identifiers. Previously unreviewed tasks and choices come first.
    /// `updated_after` keeps only tasks with activity after it.
    pub async fn dream_ids(
        &self,
        project: ProjectId,
        kind: &str,
        updated_after: Option<Timestamp>,
        offset: usize,
        limit: usize,
    ) -> StoreResult<(usize, Vec<i64>)> {
        let conn = self.db.reader()?;
        let after = opt_ts(updated_after);
        let filter = format!(
            "FROM dream_items WHERE project_id=?1 AND kind=?2 AND (kind!='run' OR last_review=0)
            AND (?3 IS NULL OR kind!='task' OR item_id IN
                (SELECT t.id FROM tasks t WHERE t.project_id=?1 AND {} > ?3))",
            task_updated_at()
        );
        let count = query_one(
            &conn,
            &format!("SELECT COUNT(*) FROM (SELECT 1 {filter} LIMIT {REVIEW_LIMIT})"),
            params![project.get(), kind, after.clone()],
            |r| Cols(r).i64(0),
        )
        .await?;
        let limit = limit.min(REVIEW_LIMIT.saturating_sub(offset));
        let order = if kind == "choice" { "DESC" } else { "ASC" };
        let ids = query_all(
            &conn,
            &format!(
                "SELECT item_id {filter} ORDER BY last_review,item_id {order} LIMIT ?4 OFFSET ?5"
            ),
            params![
                project.get(),
                kind,
                after,
                i64::try_from(limit).unwrap_or(i64::MAX),
                i64::try_from(offset).unwrap_or(i64::MAX)
            ],
            |r| Cols(r).i64(0),
        )
        .await?;
        Ok((count as usize, ids))
    }

    /// Stage only the returned identifiers. A committed run makes the review durable.
    pub async fn seen_dream_page(
        &self,
        project: ProjectId,
        event: EventId,
        lease: &Lease,
        run: RunId,
        kind: &str,
        ids: &[i64],
    ) -> StoreResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.db.begin().await?;
        crate::inbox::check_lease(tx.conn(), lease).await?;
        exec(tx.conn(),"UPDATE dream_items SET seen_run=?3 WHERE project_id=?1 AND kind=?2 AND item_id IN (SELECT value FROM json_each(?4))
            AND EXISTS(SELECT 1 FROM dream_batches WHERE project_id=?1 AND event_id=?5)",params![project.get(),kind,run.get(),json!(ids).to_string(),event.get()]).await?;
        tx.commit().await
    }

    /// Recent limitation titles for the agent. The publication gateway checks the full history.
    pub async fn previous_dream_reports(
        &self,
        project: ProjectId,
        lane: LaneId,
    ) -> StoreResult<Vec<Json>> {
        query_all(&self.db.reader()?, "SELECT json_extract(arguments,'$.title'),status,outcome
            FROM decisions WHERE project_id=?1 AND lane_id=?2 AND tool='report_threadlane_limitation'
            ORDER BY id DESC LIMIT 50",params![project.get(),lane.get()],|row| {
                let c=Cols(row);
                Ok(json!({"title":c.opt_text(0)?,"status":c.text(1)?,"outcome":c.opt_json(2)?}))
            }).await
    }

    /// Most recent completed review for each task on this page.
    pub async fn dream_reviews(
        &self,
        project: ProjectId,
        ids: &[i64],
    ) -> StoreResult<std::collections::BTreeMap<i64, Json>> {
        let rows=query_all(&self.db.reader()?, "SELECT t.id,r.submission_run,r.verdict_run,r.verdict,r.feedback FROM tasks t
            JOIN task_reviews r ON r.submission_run=(SELECT MAX(v.submission_run) FROM task_reviews v
                WHERE v.task_id=t.id AND v.state IN ('approved','changes_requested'))
            WHERE t.project_id=?1 AND t.id IN (SELECT value FROM json_each(?2))",params![project.get(),json!(ids).to_string()],|row| {
                let c=Cols(row);
                Ok((c.i64(0)?,json!({"submission_run":c.i64(1)?,"review_run":c.opt_i64(2)?,"verdict":c.opt_text(3)?,"feedback":c.opt_text(4)?})))
            }).await?;
        Ok(rows.into_iter().collect())
    }

    /// When each task on this page last saw activity, see [`task_updated_at`].
    pub async fn dream_task_times(
        &self,
        project: ProjectId,
        ids: &[i64],
    ) -> StoreResult<std::collections::BTreeMap<i64, Timestamp>> {
        let rows = query_all(
            &self.db.reader()?,
            &format!(
                "SELECT t.id,{} FROM tasks t
                WHERE t.project_id=?1 AND t.id IN (SELECT value FROM json_each(?2))",
                task_updated_at()
            ),
            params![project.get(), json!(ids).to_string()],
            |row| Ok((Cols(row).i64(0)?, Cols(row).ts(1)?)),
        )
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Hydrate one task page using the normal task and dependency decoder.
    pub async fn dream_tasks(&self, project: ProjectId, ids: &[i64]) -> StoreResult<Vec<Task>> {
        self.tasks_with_dependencies(
            "project_id=?1 AND id IN (SELECT value FROM json_each(?2))",
            params![project.get(), json!(ids).to_string()],
        )
        .await
    }

    /// Only first-party, human chat text is eligible. The canonical run prevents retries
    /// from turning one event into multiple observations. Images never enter the projection.
    async fn owner_messages(
        &self,
        project: ProjectId,
        ids: &[i64],
        filter: &str,
    ) -> StoreResult<Vec<Json>> {
        query_all(&self.db.reader()?, &format!("SELECT e.id,r.id,e.received_at,json_extract(e.payload,'$.text'),
            (SELECT json_extract(o.body,'$.text') FROM outbox o
             WHERE o.project_id=e.project_id AND o.lane_id=e.lane_id AND o.session_id=r.session_id
             AND o.status='sent' AND o.kind='message' AND o.sent_at<e.received_at
             AND NOT EXISTS (SELECT 1 FROM events between_e WHERE between_e.lane_id=e.lane_id
                AND between_e.source='client' AND between_e.kind='user_message' AND between_e.id<e.id
                AND between_e.received_at>=o.sent_at)
             ORDER BY o.sent_at DESC,o.id DESC LIMIT 1)
            FROM events e JOIN runs r ON r.id=(SELECT MIN(re.run_id) FROM run_events re WHERE re.event_id=e.id)
            WHERE e.project_id=?1 AND e.source='client' AND e.trust='user' AND e.kind='user_message'
            AND json_type(e.payload,'$.text')='text' AND ({filter}) ORDER BY e.id DESC"),
            params![project.get(),json!(ids).to_string()],|row|{
                let c=Cols(row);
                Ok(json!({"event_id":c.i64(0)?,"run_id":c.i64(1)?,"received_at":c.ts(2)?,
                    "text":c.text(3)?,"previous_reply":c.opt_text(4)?}))
            }).await
    }

    /// Project chat belonging to the runs on a session page.
    pub async fn dream_messages(&self, project: ProjectId, runs: &[i64]) -> StoreResult<Vec<Json>> {
        self.owner_messages(project, runs, "r.id IN (SELECT value FROM json_each(?2))")
            .await
    }

    /// Resolve citations alongside the newest project messages that may contradict them.
    pub async fn preference_messages(
        &self,
        project: ProjectId,
        cited: &[i64],
    ) -> StoreResult<Vec<Json>> {
        self.owner_messages(
            project,
            cited,
            &format!("e.id IN (SELECT value FROM json_each(?2)) OR e.id IN ({RECENT_MESSAGES})"),
        )
        .await
    }

    /// Finished run summaries and delivered messages for the selected page.
    pub async fn dream_history(&self, project: ProjectId, ids: &[i64]) -> StoreResult<Vec<Json>> {
        query_all(&self.db.reader()?, "SELECT r.id,r.session_id,r.outcome,r.rationale,r.status,
            (SELECT json_group_array(json_object('body',json(o.body),'sent_at',o.sent_at)) FROM
                (SELECT body,sent_at FROM outbox WHERE run_id=r.id AND project_id=?1 AND status='sent' AND kind='message' ORDER BY id) o)
            FROM runs r JOIN lanes l ON l.id=r.lane_id WHERE l.project_id=?1 AND r.id IN (SELECT value FROM json_each(?2)) ORDER BY r.id",
            params![project.get(),json!(ids).to_string()], |row|{
                let c=Cols(row);
                Ok(json!({"run_id":c.i64(0)?,"session_id":c.i64(1)?,"outcome":c.opt_text(2)?,"rationale":c.opt_text(3)?,"status":c.text(4)?,"sent_messages":c.json(5)?}))
            }).await
    }
}
