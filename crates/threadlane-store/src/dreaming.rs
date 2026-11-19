//! Durable dreaming configuration and history over timers, runs and memory versions.

use serde_json::{Value as Json, json};
use threadlane_core::client::{DreamChange, DreamSummary};
use threadlane_core::ids::{LaneId, ProjectId, RunId, TimerId};
use threadlane_core::model::{
    DecisionAnswer, DecisionOption, NewTimer, Recurrence, TimerCategory, TimerCreator,
};
use threadlane_core::store::StoreResult;
use threadlane_core::time::Timestamp;
use turso::Connection;

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::row::{Cols, params};
use crate::timer::insert_timer_in;

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

    /// Reads recent choices explicitly answered by the user in this project.
    pub async fn recent_user_choices(&self, project: ProjectId) -> StoreResult<Vec<Json>> {
        let conn = self.db.reader()?;
        // shortcut: review the newest 20 choices, add topic-based retrieval if older choices are needed.
        query_all(
            &conn,
            "SELECT d.id, d.run_id, d.question, d.options, d.answer FROM decisions d
             JOIN events e ON e.source = 'decision' AND e.source_event_id = 'answer:' || d.id
             WHERE d.project_id = ?1 AND d.kind = 'input' AND d.tool = 'ask_user'
               AND d.status = 'answered' AND d.run_id IS NOT NULL AND e.trust = 'user'
             ORDER BY d.answered_at DESC, d.id DESC LIMIT 20",
            params![project.get()],
            |row| {
                let c = Cols(row);
                let options: Vec<DecisionOption> = c.de(3)?;
                let answer: DecisionAnswer = c.de(4)?;
                Ok(json!({
                    "decision_id": c.i64(0)?,
                    "run_id": c.i64(1)?,
                    "question": c.text(2)?,
                    "chosen": options.get(answer.option as usize).map(|option| &option.label),
                    "options": options.iter().map(|option| &option.label).collect::<Vec<_>>(),
                    "note": answer.note,
                    "answered_at": answer.answered_at,
                }))
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

    /// Finished project runs since the preceding dream began, with only their delivered messages.
    pub async fn dream_history(&self, project: ProjectId) -> StoreResult<Vec<Json>> {
        let conn = self.db.reader()?;
        let since = last_dream(&conn, project).await?;
        let runs = query_all(
            &conn,
            "SELECT r.id, r.session_id, r.outcome, r.rationale, r.status
             FROM runs r JOIN lanes l ON l.id = r.lane_id WHERE l.project_id = ?1
             AND r.ended_at IS NOT NULL AND (r.ended_at > ?2 OR (r.ended_at = ?2 AND r.id > ?3))
             AND NOT EXISTS (
                 SELECT 1 FROM run_events re JOIN events e ON e.id = re.event_id WHERE re.run_id = r.id
                 AND e.kind = 'dream' AND e.source = 'timer' AND e.trust = 'system'
             ) ORDER BY r.ended_at, r.id",
            params![
                project.get(),
                since.map_or(i64::MIN, |(_, start, _)| start.as_millis()),
                since.map_or(0, |(run, _, _)| run.get()),
            ],
            |row| {
                let c = Cols(row);
                Ok((c.id::<RunId>(0)?, json!({
                    "run_id": c.i64(0)?,
                    "session_id": c.i64(1)?,
                    "outcome": c.opt_text(2)?,
                    "rationale": c.opt_text(3)?,
                    "status": c.text(4)?,
                })))
            },
        ).await?;
        let mut history = Vec::with_capacity(runs.len());
        for (run, mut record) in runs {
            let messages = query_all(
                &conn,
                "SELECT body, sent_at FROM outbox WHERE run_id = ?1 AND project_id = ?2
                 AND status = 'sent' AND kind = 'message' ORDER BY id",
                params![run.get(), project.get()],
                |row| {
                    let c = Cols(row);
                    Ok(json!({ "body": c.json(0)?, "sent_at": c.opt_ts(1)? }))
                },
            )
            .await?;
            record["sent_messages"] = json!(messages);
            history.push(record);
        }
        Ok(history)
    }
}
