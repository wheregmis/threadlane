//! Read models for owner-facing project work. Results are scoped in the query.
use crate::row::json_text;
use crate::{
    HubDb,
    db::{query_all, query_one, query_opt},
    row::{Cols, opt_raw, params},
};
use serde_json::json;
use std::collections::HashMap;
use threadlane_core::attachment::ImageAttachment;
use threadlane_core::client::RunUsage;
use threadlane_core::ids::RunId;
use threadlane_core::ids::{EventId, LaneId, ProjectId, SessionId, TaskId, TimerId};
use threadlane_core::model::{DecisionKind, DecisionOption, NewDecision, Task};
use threadlane_core::store::{StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use threadlane_core::work::{Occurrence, OutputSource, ProjectOutput, UsageGroup};
use turso::Connection;

#[cfg(test)]
mod tests;

/// CTEs ending in `run_usage(run_id,input,output,cost)`: each run's usage from its latest
/// `finished` entry, for the runs of the sessions a preceding `scope(session_id)` CTE names.
/// Claude reports its cost, and Codex its tokens, as running totals for the session, so those
/// count the rise since the session's latest earlier report, or the whole value when the total
/// restarted lower. Input includes cached input for both, as Codex already reports it.
const RUN_USAGE: &str = "
  reports AS (
    SELECT r.id run_id, r.session_id, r.backend,
      json_extract(j.entry,'$.usage.usage.input_tokens') input,
      json_extract(j.entry,'$.usage.usage.cache_read_input_tokens') cache_read,
      json_extract(j.entry,'$.usage.usage.cache_creation_input_tokens') cache_write,
      json_extract(j.entry,'$.usage.usage.output_tokens') output,
      json_extract(j.entry,'$.usage.total_cost_usd') cost
    FROM runs r JOIN journal j ON j.run_id=r.id AND j.seq=(
      SELECT MAX(seq) FROM journal WHERE run_id=r.id AND json_extract(entry,'$.type')='finished')
    WHERE r.session_id IN (SELECT session_id FROM scope)),
  counts AS (
    SELECT run_id, session_id, backend,
      CASE WHEN typeof(input)='integer' AND input>=0 THEN input
        + CASE WHEN typeof(cache_read)='integer' AND cache_read>=0 THEN cache_read ELSE 0 END
        + CASE WHEN typeof(cache_write)='integer' AND cache_write>=0 THEN cache_write ELSE 0 END
      END input,
      CASE WHEN typeof(output)='integer' AND output>=0 THEN output END output,
      CASE WHEN typeof(cost) IN ('real','integer') AND cost>=0 THEN CAST(cost AS REAL) END cost
    FROM reports),
  previous AS (
    SELECT *,
      LAG(input) OVER (PARTITION BY session_id, input IS NULL ORDER BY run_id) input_before,
      LAG(output) OVER (PARTITION BY session_id, output IS NULL ORDER BY run_id) output_before,
      LAG(cost) OVER (PARTITION BY session_id, cost IS NULL ORDER BY run_id) cost_before
    FROM counts),
  run_usage AS (
    SELECT run_id,
      CASE WHEN backend='codex' AND input_before<=input THEN input-input_before ELSE input END input,
      CASE WHEN backend='codex' AND output_before<=output THEN output-output_before ELSE output END output,
      CASE WHEN backend='claude' AND cost_before<=cost THEN cost-cost_before ELSE cost END cost
    FROM previous)";

pub(crate) async fn needs_owner_decision(
    conn: &Connection,
    task: &Task,
    result: &serde_json::Value,
    run: RunId,
    now: Timestamp,
) -> StoreResult<bool> {
    let accepted = query_one(
        conn,
        "SELECT EXISTS(SELECT 1 FROM decisions WHERE task_id=?1 AND tool='workflow_review'
        AND status='answered' AND json_extract(answer,'$.option')=0 AND arguments=?2)",
        params![task.id.get(), json_text(result)?],
        |row| Cols(row).bool(0),
    )
    .await?;
    if accepted {
        return Ok(false);
    }
    let new = NewDecision {
        project_id: task.project_id,
        lane_id: task.lane_id,
        task_id: Some(task.id),
        session_id: None,
        run_id: Some(run),
        tool_call_id: Some("workflow-owner-review".into()),
        tool: "workflow_review".into(),
        arguments: result.clone(),
        arguments_hash: format!("workflow-review:{run}"),
        kind: DecisionKind::Input,
        question: format!("Accept the review for {}?", task.title),
        context: result
            .get("summary")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| result.to_string()),
        options: ["Accept review", "Request changes"]
            .into_iter()
            .map(|label| DecisionOption {
                label: label.into(),
                effect: json!({"answer":label}),
            })
            .collect(),
        recommended: None,
        offers_standing_rule: false,
        expires_at: None,
        created_at: now,
        urgent: true,
    };
    let (decision, created) = crate::decision::insert_decision_in(conn, None, &new).await?;
    if created {
        crate::decision::enqueue_card(conn, &decision).await?;
    }
    Ok(true)
}

impl HubDb {
    /// Occurrences retain their own run even when the same routine fires again.
    pub async fn routine_history(
        &self,
        timer: Option<TimerId>,
        before: Option<EventId>,
    ) -> StoreResult<Vec<Occurrence>> {
        let sql = "
        SELECT e.id, json_extract(e.payload,'$.timer_id'), p.name,
          json_extract(e.payload,'$.instruction'), COALESCE(json_extract(e.payload,'$.occurrence'),e.received_at),
          r.started_at, COALESCE(json_extract(e.payload,'$.manual'),0),
          CASE WHEN e.status IN ('pending','dead','skipped') THEN e.status ELSE COALESCE(r.status,e.status) END,
          COALESCE(r.error,e.status_reason,
            (SELECT substr(json_extract(o.body,'$.text'),1,4000) FROM outbox o
             WHERE o.run_id=r.id AND o.kind='message' ORDER BY o.id DESC LIMIT 1),r.rationale,r.outcome),
          (SELECT json_group_array(DISTINCT json_extract(c.result,'$.task_id')) FROM tool_calls c
           JOIN run_events re ON re.run_id=c.run_id WHERE re.event_id=e.id AND c.tool='create_task'
             AND json_type(c.result,'$.task_id')='text'),
          (SELECT COUNT(*) FROM run_events inputs WHERE inputs.run_id=r.id)
        FROM events e JOIN projects p ON p.id=e.project_id
        LEFT JOIN runs r ON r.id=(SELECT MAX(re.run_id) FROM run_events re WHERE re.event_id=e.id)
        WHERE e.source='timer' AND e.kind='timer_fired' AND (?1 IS NULL OR json_extract(e.payload,'$.timer_id')=?1)
          AND (?2 IS NULL OR e.id < ?2) ORDER BY e.id DESC LIMIT 50";
        query_all(
            &self.db.reader()?,
            sql,
            params![opt_raw(timer, TimerId::get), opt_raw(before, EventId::get)],
            |row| {
                let c = Cols(row);
                Ok(Occurrence {
                    event: c.id(0)?,
                    timer: c.id(1)?,
                    project: c.text(2)?,
                    instruction: c.text(3)?,
                    due: c.ts(4)?,
                    started: c.opt_ts(5)?,
                    manual: c.bool(6)?,
                    input_count: c.u32(10)?,
                    status: c.text(7)?,
                    result: c.opt_text(8)?,
                    tasks: c
                        .de::<Vec<String>>(9)?
                        .iter()
                        .filter_map(|id| TaskId::parse(id))
                        .collect(),
                })
            },
        )
        .await
    }

    /// Input tokens of the last model request in `session`'s latest finished run: how much
    /// its next run rereads. Zero when the backend reports no per-request usage, as Codex.
    pub async fn session_context_tokens(&self, session: SessionId) -> StoreResult<u64> {
        // The last `message` iteration is the context as it now stands; earlier ones were
        // smaller and a `compaction` one is not a request the next run rereads.
        let sql = "SELECT (SELECT COALESCE(json_extract(it.value,'$.input_tokens'),0)
                + COALESCE(json_extract(it.value,'$.cache_read_input_tokens'),0)
                + COALESCE(json_extract(it.value,'$.cache_creation_input_tokens'),0)
              FROM json_each(j.entry,'$.usage.usage.iterations') it
              WHERE COALESCE(json_extract(it.value,'$.type'),'message')='message'
              ORDER BY it.key DESC LIMIT 1)
            FROM journal j JOIN runs r ON r.id=j.run_id
            WHERE r.session_id=?1 AND json_extract(j.entry,'$.type')='finished'
            ORDER BY j.run_id DESC, j.seq DESC LIMIT 1";
        let tokens = query_opt(&self.db.reader()?, sql, params![session.get()], |row| {
            Cols(row).opt_i64(0)
        })
        .await?;
        Ok(tokens.flatten().map_or(0, |n| n.max(0) as u64))
    }

    /// Each finished run's usage in `lane`, counted as `RUN_USAGE` describes for both Task Details
    /// and [`Self::space_usage`].
    pub async fn lane_run_usage(&self, lane: LaneId) -> StoreResult<HashMap<RunId, RunUsage>> {
        let sql = format!(
            "WITH scope AS (SELECT id session_id FROM sessions WHERE lane_id=?1), {RUN_USAGE}
             SELECT run_id,input,output,cost FROM run_usage
             WHERE input IS NOT NULL OR output IS NOT NULL OR cost IS NOT NULL"
        );
        let rows = query_all(&self.db.reader()?, &sql, params![lane.get()], |row| {
            let c = Cols(row);
            let count = |idx| Ok::<_, StoreError>(c.opt_i64(idx)?.map(|n| n as u64));
            Ok((
                c.id(0)?,
                RunUsage {
                    input_tokens: count(1)?,
                    output_tokens: count(2)?,
                    cost_usd: c.opt_real(3)?,
                },
            ))
        })
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Aggregate each run's usage as `RUN_USAGE` counts it, matching Task Details.
    pub async fn space_usage(
        &self,
        since: Timestamp,
        now: Timestamp,
    ) -> StoreResult<Vec<UsageGroup>> {
        // Earlier runs of a session outside the window still give the baseline for its totals.
        let sql = format!(
            "WITH scope AS (SELECT session_id FROM runs WHERE started_at BETWEEN ?1 AND ?2), {RUN_USAGE},
        samples AS (
          SELECT p.name project, COALESCE(a.name,'Lead') agent, COALESCE(r.backend,'Unknown') backend,
            l.task_id, MAX(0,COALESCE(r.ended_at,?2)-r.started_at) duration, u.input, u.output, u.cost
          FROM runs r JOIN lanes l ON l.id=r.lane_id JOIN projects p ON p.id=l.project_id
          LEFT JOIN agents a ON a.id=r.agent_id LEFT JOIN run_usage u ON u.run_id=r.id
          WHERE r.started_at BETWEEN ?1 AND ?2)
        SELECT project,agent,backend,COUNT(*),SUM(duration),SUM(COALESCE(input,0)+COALESCE(output,0)),
          COUNT(input+output),CAST(COALESCE(SUM(cost),0) AS REAL),COUNT(cost),
          json_group_array(DISTINCT task_id)
        FROM samples GROUP BY project,agent,backend ORDER BY project,agent,backend"
        );
        query_all(
            &self.db.reader()?,
            &sql,
            params![since.as_millis(), now.as_millis()],
            |row| {
                let c = Cols(row);
                Ok(UsageGroup {
                    project: c.text(0)?,
                    agent: c.text(1)?,
                    backend: c.text(2)?,
                    runs: c.i64(3)? as u64,
                    duration_ms: c.i64(4)? as u64,
                    tokens: c.i64(5)? as u64,
                    token_reports: c.i64(6)? as u64,
                    cost: c.real(7)?,
                    cost_reports: c.i64(8)? as u64,
                    tasks: c
                        .de::<Vec<Option<TaskId>>>(9)?
                        .into_iter()
                        .flatten()
                        .collect(),
                })
            },
        )
        .await
    }

    /// Search published records without reading worktree paths or returning image bytes.
    pub async fn project_outputs(
        &self,
        project: ProjectId,
        search: &str,
    ) -> StoreResult<Vec<ProjectOutput>> {
        let prefix = format!("projects/{}/", project.get());
        let sql = "
        SELECT kind,name,at,task_id,run_id,target,event_id,image_index FROM (
          SELECT 'pr' kind, json_extract(d.outcome,'$.pull_request') target,
            COALESCE(t.title,'Pull request') name,d.created_at at,d.task_id,d.run_id,NULL event_id,NULL image_index
          FROM decisions d LEFT JOIN tasks t ON t.id=d.task_id WHERE d.project_id=?1
            AND d.tool IN ('publish_branch','open_pull_request') AND json_extract(d.outcome,'$.performed')=1
            AND json_type(d.outcome,'$.pull_request')='text'
          UNION ALL
          SELECT 'report','project/'||substr(m.path,length(?2)+1),substr(m.path,length(?2)+15),
            m.written_at,l.task_id,m.written_by_run,NULL,NULL
          FROM memory_files m LEFT JOIN runs r ON r.id=m.written_by_run LEFT JOIN lanes l ON l.id=r.lane_id
          WHERE m.deleted=0 AND m.path LIKE ?2||'notes/reports/%' AND m.path NOT LIKE '%/INDEX.md'
          UNION ALL
          SELECT 'image','',json_extract(i.value,'$.name'),e.received_at,l.task_id,NULL,e.id,CAST(i.key AS INTEGER)
          FROM events e JOIN lanes l ON l.id=e.lane_id JOIN json_each(e.payload,'$.images') i
          WHERE e.project_id=?1 AND e.kind='user_message'
        ) WHERE instr(lower(name||' '||target),lower(?3))>0 ORDER BY at DESC,kind,name LIMIT 100";
        query_all(
            &self.db.reader()?,
            sql,
            params![project.get(), prefix, search],
            |row| {
                let c = Cols(row);
                let source = match c.text(0)?.as_str() {
                    "pr" => OutputSource::PullRequest(c.text(5)?),
                    "report" => OutputSource::Report(c.text(5)?),
                    _ => OutputSource::Attachment {
                        event: c.id(6)?,
                        index: c.u32(7)?,
                    },
                };
                Ok(ProjectOutput {
                    name: c.text(1)?,
                    at: c.ts(2)?,
                    source,
                    task: c.opt_id(3)?,
                    run: c.opt_id(4)?,
                })
            },
        )
        .await
    }

    /// Fetch one selected attachment in its originating project.
    pub async fn output_image(
        &self,
        project: ProjectId,
        event: EventId,
        index: u32,
    ) -> StoreResult<Option<ImageAttachment>> {
        query_opt(
            &self.db.reader()?,
            "SELECT i.value FROM events e JOIN json_each(e.payload,'$.images') i
        WHERE e.project_id=?1 AND e.id=?2 AND e.kind='user_message' AND i.key=?3",
            params![project.get(), event.get(), i64::from(index)],
            |row| Cols(row).de(0),
        )
        .await
    }
}
