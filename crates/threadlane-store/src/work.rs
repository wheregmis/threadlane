//! Read models for owner-facing project work. Results are scoped in the query.
use crate::db::exec;
use crate::row::json_text;
use crate::{
    HubDb,
    db::{query_all, query_one, query_opt},
    row::{Cols, opt_raw, params},
};
use serde_json::json;
use threadlane_core::attachment::ImageAttachment;
use threadlane_core::client::AgentTemplate;
use threadlane_core::ids::{AgentId, RepositoryId, RunId};
use threadlane_core::ids::{EventId, ProjectId, TaskId, TimerId};
use threadlane_core::model::{
    DecisionKind, DecisionOption, Mode, NewAgent, NewBinding, NewDecision, NewTask, NewTimer,
    Project, Task, TaskOrigin, TaskStatus, Timer,
};
use threadlane_core::store::{StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use threadlane_core::work::{
    Occurrence, OutputSource, ProjectOutput, ReviewRecipe, UsageGroup, WorkflowGraph,
};
use turso::Connection;

#[cfg(test)]
mod tests;

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
    };
    let (decision, created) = crate::decision::insert_decision_in(conn, None, &new).await?;
    if created {
        crate::decision::enqueue_card(conn, &decision).await?;
    }
    Ok(true)
}

/// Both roles are resolved inside the transaction that creates their tasks.
#[derive(Debug, Clone, Copy)]
pub struct ReviewRoles<'a> {
    pub engineer: &'a str,
    pub reviewer: &'a str,
}

async fn bound_specialist(
    conn: &Connection,
    project: ProjectId,
    name: &str,
) -> StoreResult<AgentId> {
    query_opt(conn,"SELECT b.agent_id FROM bindings b JOIN agents a ON a.id=b.agent_id
        WHERE b.project_id=?1 AND a.name=?2 AND a.deleted_at IS NULL AND b.is_lead=0 AND b.mode!='off'",
        params![project.get(),name],|row|Cols(row).id(0)).await?.ok_or_else(||StoreError::Invalid(format!("Bind an enabled specialist named {name:?} to this project.")))
}

async fn project_graph(
    conn: &Connection,
    project: ProjectId,
) -> StoreResult<Option<WorkflowGraph>> {
    query_one(
        conn,
        "SELECT workflow FROM projects WHERE id=?1",
        params![project.get()],
        |row| Cols(row).opt_de(0),
    )
    .await
}

impl HubDb {
    pub async fn project_workflow(&self, project: ProjectId) -> StoreResult<Option<WorkflowGraph>> {
        project_graph(&self.db.reader()?, project).await
    }

    pub async fn save_project_workflow(
        &self,
        project: ProjectId,
        mut graph: WorkflowGraph,
    ) -> StoreResult<WorkflowGraph> {
        graph
            .validate()
            .map_err(|error| StoreError::Invalid(error.into()))?;
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        crate::org::require_active_project(conn, project).await?;
        let current = project_graph(conn, project)
            .await?
            .map_or(0, |saved| saved.version);
        if graph.version != current {
            return Err(StoreError::Invalid(
                "The workflow changed on another client. Reopen it before saving.".into(),
            ));
        }
        graph.version += 1;
        exec(
            conn,
            "UPDATE projects SET workflow=?1 WHERE id=?2",
            params![json_text(&graph)?, project.get()],
        )
        .await?;
        tx.commit().await?;
        Ok(graph)
    }

    pub async fn start_project_workflow(
        &self,
        project: ProjectId,
        repository: RepositoryId,
        goal: &str,
        version: i64,
        request: &str,
    ) -> StoreResult<Task> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        crate::org::require_active_project(conn, project).await?;
        // The first task is the receipt even if the graph has since been edited.
        for prefix in ["canvas", "canvas-review"] {
            if let Some(task) =
                crate::task::task_by_call(conn, &format!("{prefix}:0:{project}:{request}")).await?
            {
                tx.commit().await?;
                return Ok(task);
            }
        }
        let graph = project_graph(conn, project)
            .await?
            .filter(|graph| graph.version == version)
            .ok_or_else(|| {
                StoreError::Invalid("Reload the saved workflow before starting it.".into())
            })?;
        let layers = graph
            .layers()
            .map_err(|error| StoreError::Invalid(error.into()))?;
        let mut tasks = std::collections::HashMap::new();
        let mut first = None;
        for (index, step) in layers.into_iter().flatten().enumerate() {
            let depends_on: Vec<TaskId> = step.after.iter().map(|id| tasks[id]).collect();
            let mut prompt = format!("{goal}\n\n{}", step.instructions);
            if !depends_on.is_empty() {
                let sources = depends_on
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                prompt.push_str(&format!("\n\nUse the recorded results of {sources}."));
            }
            let prefix = if step.owner_review {
                "canvas-review"
            } else {
                "canvas"
            };
            let task = crate::task::insert_task_in(
                conn,
                None,
                &NewTask {
                    project_id: project,
                    agent_id: bound_specialist(conn, project, &step.agent).await?,
                    title: format!(
                        "{} · {}",
                        step.name,
                        goal.lines()
                            .next()
                            .unwrap_or(goal)
                            .chars()
                            .take(100)
                            .collect::<String>()
                    ),
                    goal: prompt,
                    repositories: vec![repository],
                    parent_task_id: None,
                    depends_on,
                    origin: TaskOrigin::User,
                    status: TaskStatus::Queued,
                    priority: 0,
                    created_by_call: Some(format!("{prefix}:{index}:{project}:{request}")),
                    created_at: self.now(),
                },
                None,
            )
            .await?;
            tasks.insert(step.id, task.id);
            first.get_or_insert(task);
        }
        let first =
            first.ok_or_else(|| StoreError::Invalid("Add a step before starting.".into()))?;
        tx.commit().await?;
        Ok(first)
    }

    pub async fn start_review_workflow(
        &self,
        project: ProjectId,
        roles: ReviewRoles<'_>,
        repository: RepositoryId,
        goal: &str,
        recipe: &ReviewRecipe,
        request: &str,
    ) -> StoreResult<Task> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        crate::org::require_active_project(conn, project).await?;
        let engineer = bound_specialist(conn, project, roles.engineer).await?;
        let reviewer = bound_specialist(conn, project, roles.reviewer).await?;
        let title: String = goal
            .lines()
            .next()
            .unwrap_or(goal)
            .chars()
            .take(100)
            .collect();
        let mut spec = NewTask {
            project_id: project,
            agent_id: engineer,
            title: title.clone(),
            goal: format!("{goal}\n\n{}", recipe.implementation),
            repositories: vec![repository],
            parent_task_id: None,
            depends_on: vec![],
            origin: TaskOrigin::User,
            status: TaskStatus::Queued,
            priority: 0,
            created_by_call: Some(format!("workflow-implement:{project}:{request}")),
            created_at: self.now(),
        };
        let first = crate::task::insert_task_in(conn, None, &spec, None).await?;
        spec.agent_id = reviewer;
        spec.title = format!("Review {title}");
        spec.goal = format!(
            "{}\n\nReview {} and the branch and checks recorded in its result. The owner decides whether to accept your review.",
            recipe.review, first.id
        );
        spec.depends_on = vec![first.id];
        spec.created_by_call = Some(format!("workflow-review:{project}:{request}"));
        crate::task::insert_task_in(conn, None, &spec, None).await?;
        tx.commit().await?;
        Ok(first)
    }

    /// Install both bindings and the initially paused routine atomically.
    pub async fn install_review_team(
        &self,
        project: &Project,
        roles: [(&str, &AgentTemplate); 2],
        timer: &NewTimer,
    ) -> StoreResult<Timer> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        crate::org::require_active_project(conn, project.id).await?;
        for (name, template) in roles {
            let agent = match query_opt(
                conn,
                "SELECT id FROM agents WHERE name=?1 AND deleted_at IS NULL",
                params![name],
                |row| Cols(row).id::<AgentId>(0),
            )
            .await?
            {
                Some(id) => id,
                None => {
                    crate::org::insert_agent_in(
                        conn,
                        &NewAgent {
                            instructions: template.instructions.clone(),
                            backends: template.backends.clone(),
                            ..NewAgent::named(name, template.role.clone())
                        },
                    )
                    .await?
                    .id
                }
            };
            let binding = query_opt(
                conn,
                "SELECT is_lead,mode FROM bindings WHERE project_id=?1 AND agent_id=?2",
                params![project.id.get(), agent.get()],
                |row| Ok((Cols(row).bool(0)?, Cols(row).parse::<Mode>(1)?)),
            )
            .await?;
            match binding {
                Some((true, _)) | Some((_, Mode::Off)) => {
                    return Err(StoreError::Invalid(format!(
                        "Choose an enabled specialist for {name}."
                    )));
                }
                Some(_) => {}
                None => {
                    crate::org::insert_binding_in(
                        conn,
                        &NewBinding {
                            project_id: project.id,
                            agent_id: agent,
                            mode: Mode::OnRequest,
                            is_lead: false,
                            max_parallel: 1,
                        },
                    )
                    .await?;
                }
            }
        }
        let key = timer.created_by_call.as_deref();
        // A cancelled preset stays in history. A new installation gets its own timer.
        exec(conn, "UPDATE timers SET created_by_call=NULL WHERE created_by_call=?1 AND status='cancelled'", params![key]).await?;
        let existed = query_one(
            conn,
            "SELECT EXISTS(SELECT 1 FROM timers WHERE created_by_call=?1)",
            params![key],
            |row| Cols(row).bool(0),
        )
        .await?;
        let mut saved = crate::timer::insert_timer_in(conn, timer, None).await?;
        if saved.instruction != timer.instruction || saved.recurrence != timer.recurrence {
            return Err(StoreError::Invalid(
                "This project already has a different review team routine.".into(),
            ));
        }
        if !existed {
            exec(
                conn,
                "UPDATE timers SET status='paused' WHERE id=?1",
                params![saved.id.get()],
            )
            .await?;
            saved.status = threadlane_core::model::TimerStatus::Paused;
        }
        tx.commit().await?;
        Ok(saved)
    }

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

    /// Aggregate the latest finished journal entry per run, matching Task Details.
    pub async fn space_usage(
        &self,
        since: Timestamp,
        now: Timestamp,
    ) -> StoreResult<Vec<UsageGroup>> {
        let sql = "
        WITH samples AS (
          SELECT p.name project, COALESCE(a.name,'Lead') agent, COALESCE(r.backend,'Unknown') backend,
            l.task_id, MAX(0,COALESCE(r.ended_at,?2)-r.started_at) duration,
            json_extract(j.entry,'$.usage.usage.input_tokens') input,
            json_extract(j.entry,'$.usage.usage.output_tokens') output,
            json_extract(j.entry,'$.usage.total_cost_usd') cost
          FROM runs r JOIN lanes l ON l.id=r.lane_id JOIN projects p ON p.id=l.project_id
          LEFT JOIN agents a ON a.id=r.agent_id
          LEFT JOIN journal j ON j.run_id=r.id AND j.seq=(
            SELECT MAX(seq) FROM journal WHERE run_id=r.id AND json_extract(entry,'$.type')='finished')
          WHERE r.started_at BETWEEN ?1 AND ?2)
        SELECT project,agent,backend,COUNT(*),SUM(duration),SUM(COALESCE(input,0)+COALESCE(output,0)),
          COUNT(input+output),CAST(COALESCE(SUM(cost),0) AS REAL),COUNT(cost),
          json_group_array(DISTINCT task_id)
        FROM (SELECT project,agent,backend,task_id,duration,
          CASE WHEN typeof(input)='integer' AND input>=0 THEN input END input,
          CASE WHEN typeof(output)='integer' AND output>=0 THEN output END output,
          CASE WHEN typeof(cost) IN ('real','integer') AND cost>=0 THEN cost END cost
          FROM samples) GROUP BY project,agent,backend ORDER BY project,agent,backend";
        query_all(
            &self.db.reader()?,
            sql,
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

    pub async fn review_recipe(&self) -> StoreResult<ReviewRecipe> {
        Ok(query_opt(
            &self.db.reader()?,
            "SELECT implementation,review,version FROM review_recipe WHERE id=1",
            (),
            |row| {
                Ok(ReviewRecipe {
                    implementation: Cols(row).text(0)?,
                    review: Cols(row).text(1)?,
                    version: Cols(row).i64(2)?,
                })
            },
        )
        .await?
        .unwrap_or_default())
    }

    pub async fn save_review_recipe(&self, recipe: &ReviewRecipe) -> StoreResult<ReviewRecipe> {
        let tx = self.db.begin().await?;
        let current: i64 = query_one(
            tx.conn(),
            "SELECT COALESCE(MAX(version),0) FROM review_recipe",
            (),
            |row| Cols(row).i64(0),
        )
        .await?;
        if current != recipe.version {
            return Err(StoreError::Invalid(
                "The review workflow changed. Reload it before saving.".into(),
            ));
        }
        let version = current + 1;
        exec(tx.conn(),"INSERT INTO review_recipe(id,version,implementation,review) VALUES(1,?1,?2,?3)
        ON CONFLICT(id) DO UPDATE SET version=excluded.version,implementation=excluded.implementation,review=excluded.review",
        params![version,recipe.implementation.as_str(),recipe.review.as_str()]).await?;
        tx.commit().await?;
        Ok(ReviewRecipe {
            version,
            ..recipe.clone()
        })
    }
}
