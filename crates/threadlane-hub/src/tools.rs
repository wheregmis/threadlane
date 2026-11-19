//! The system's tools (Plan.md §4.6, §5.1, docs/milestone-1.md §9).

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use threadlane_core::forge::{
    COMMENT, CREATE_ISSUE, MARK_READY, MERGE, OPEN_PULL_REQUEST, PUBLISH_BRANCH,
};
use threadlane_core::ids::{AgentId, ProjectId, RepositoryId, TaskId};
use threadlane_core::link::MERGE_CHECKS;
use threadlane_core::model::{
    Decision, DecisionOption, LaneKind, MemoryAuthor, Mode, NewOutboxMessage, NewTask, OutboxKind,
    Run, Task, TaskOrigin, TaskReviewVerdict, TaskStatus, Urgency,
};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{LeadReply, OrgStore, SessionStore, StoreError, TaskStore};
use threadlane_core::time::Clock;
use threadlane_core::tools::{Tier, ToolContext, ToolDefinition, ToolError, ToolRegistry};
use threadlane_core::workspace::{MergeCheck, ReviewRevision};
use threadlane_store::HubDb;

use crate::error::tool_error;

/// The channel ordinary messages are queued on in milestone 1.
pub const CLIENT_CHANNEL: &str = "client";

/// Name of the lead's messaging tool.
pub const SEND_MESSAGE: &str = "send_message";

/// Creates a task; in a task lane, a subtask (Plan.md §5.1).
pub const CREATE_TASK: &str = "create_task";
/// The lead changes a task's title, goal, or priority.
pub const UPDATE_TASK: &str = "update_task";
/// The lead cancels a task.
pub const CANCEL_TASK: &str = "cancel_task";
/// The lead lists the project's tasks.
pub const LIST_TASKS: &str = "list_tasks";
/// A task agent reports its task done, with a result.
pub use threadlane_core::link::FINISH_TASK;
pub(crate) const FINISH_REVIEW: &str = "finish_review";

pub(crate) fn offered_for_review(tool: &str) -> bool {
    tool == FINISH_REVIEW
}
/// Asks you a question with options; your answer resumes the session
/// (Plan.md §9.4, kind `input`).
pub const ASK_USER: &str = "ask_user";

/// The most options a question may offer.
const MAX_OPTIONS: usize = 6;
/// The longest option label accepted.
const MAX_LABEL: usize = 80;

/// Whether runs in a lane of `kind` are offered `tool`, and may call it
/// (Plan.md §5.1). A tool this hub does not know is offered in both kinds.
#[must_use]
pub fn offered_in(tool: &str, kind: LaneKind) -> bool {
    match tool {
        threadlane_core::forge::REPORT_LIMITATION
        | crate::dreaming::PROJECT_SESSIONS
        | crate::dreaming::PROJECT_DECISIONS
        | FINISH_REVIEW => false,
        // ask_project_lead is offered to the General lead only (begin_run).
        SEND_MESSAGE
        | UPDATE_TASK
        | CANCEL_TASK
        | LIST_TASKS
        | crate::space_lead::ASK_PROJECT_LEAD => kind == LaneKind::Lead,
        FINISH_TASK | PUBLISH_BRANCH | OPEN_PULL_REQUEST | COMMENT | CREATE_ISSUE | MERGE
        | MARK_READY => kind == LaneKind::Task,
        _ => true,
    }
}

/// The key a call is recorded under when the backend supplied no call id:
/// `"<tool>:<sha256 of the canonical arguments>"` (Plan.md §4.6).
#[must_use]
pub fn derived_call_key(tool: &str, arguments: &Json) -> String {
    let mut canonical = String::new();
    write_canonical(arguments, &mut canonical);
    let digest = Sha256::digest(canonical.as_bytes());
    let mut key = format!("{tool}:");
    for byte in digest.iter() {
        // Writing to a String cannot fail.
        let _ = write!(key, "{byte:02x}");
    }
    key
}

/// JSON with object keys sorted and no insignificant whitespace, so equal
/// arguments hash equally whatever order the backend wrote them in.
fn write_canonical(value: &Json, out: &mut String) {
    match value {
        Json::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Json::String(key.clone()).to_string());
                out.push(':');
                if let Some(item) = map.get(key) {
                    write_canonical(item, out);
                }
            }
            out.push('}');
        }
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// The hub's built-in tools: `send_message`, and the task tools of
/// Plan.md §5.1. Every task tool is idempotent, so safe to repeat: creating
/// is keyed on the creating call ([`NewTask::created_by_call`]), and the
/// others set a state that setting again leaves as it is.
#[derive(Debug, Clone)]
pub struct HubTools {
    db: HubDb,
    clock: Arc<dyn Clock>,
    waker: Arc<dyn Waker>,
    reviewer: Option<String>,
}

impl HubTools {
    /// Tools that write to `db` and wake the dispatcher through `waker`.
    #[must_use]
    pub fn new(
        db: HubDb,
        clock: Arc<dyn Clock>,
        waker: Arc<dyn Waker>,
        reviewer: Option<String>,
    ) -> Self {
        Self {
            db,
            clock,
            waker,
            reviewer,
        }
    }

    fn send_message_definition() -> ToolDefinition {
        ToolDefinition {
            name: SEND_MESSAGE.to_owned(),
            description: "Send one message to the user. This is the only way anything you \
                          write reaches them. Send at most one per reply, and nothing when \
                          nothing needs saying."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The message, in plain language."
                    },
                    "urgency": {
                        "type": "string",
                        "enum": ["low", "normal", "high"],
                        "description": "How soon the user should see it. Defaults to normal."
                    }
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            tier: Tier::Local,
            safe_to_repeat: false,
        }
    }

    async fn send_message(
        &self,
        context: &ToolContext,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let (text, urgency) = parse_send_message(arguments)?;
        let run = self.run(context).await?;
        let key = effect_key(context, &run, SEND_MESSAGE, arguments);
        let message = NewOutboxMessage {
            project_id: Some(context.project_id),
            lane_id: Some(run.lane_id),
            session_id: Some(run.session_id),
            run_id: Some(run.id),
            tool_call_id: Some(key),
            kind: OutboxKind::Message,
            decision_id: None,
            channel: CLIENT_CHANNEL.to_owned(),
            body: json!({ "text": text }),
            urgency,
            not_before: None,
            created_at: self.clock.now(),
        };
        let events = self.db.run_events(run.id).await.map_err(tool_error)?;
        let route = crate::space_lead::reply_route(
            &self.db,
            message.created_at,
            &events,
            context.project_id,
            &text,
        )
        .await
        .map_err(tool_error)?;
        let reply = LeadReply { message, route };
        let message = self
            .db
            .enqueue_lead_reply(&reply)
            .await
            .map_err(tool_error)?;
        self.waker.wake(WakeTarget::Dispatcher);
        if let Some(route) = reply.route {
            self.waker.wake(WakeTarget::Lane(route.lane_id));
        }
        Ok(json!({ "outbox_id": message.id }))
    }
}

impl HubTools {
    async fn run(&self, context: &ToolContext) -> Result<Run, ToolError> {
        self.db
            .run(context.run_id)
            .await
            .map_err(tool_error)?
            .ok_or_else(|| ToolError::Failed(format!("run {} not found", context.run_id)))
    }

    /// A task of the run's project, as the agent named it.
    async fn project_task(
        &self,
        context: &ToolContext,
        reference: &Json,
    ) -> Result<Task, ToolError> {
        let id = task_ref(reference)?;
        self.db
            .task(id)
            .await
            .map_err(tool_error)?
            .filter(|task| task.project_id == context.project_id)
            .ok_or_else(|| ToolError::InvalidArguments(format!("no task {id} in this project")))
    }

    async fn create_task(
        &self,
        context: &ToolContext,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        self.create_task_as(context, arguments, None).await
    }

    /// Creates the task of `decision`, an accepted proposal, in the lane of
    /// the run that performs it. It is work you requested, and keyed on the
    /// decision in the session that proposed it.
    pub(crate) async fn create_accepted_task(
        &self,
        context: &ToolContext,
        decision: &Decision,
    ) -> Result<Json, ToolError> {
        self.create_task_as(context, &decision.arguments, Some(decision))
            .await
    }

    async fn create_task_as(
        &self,
        context: &ToolContext,
        arguments: &Json,
        accepted: Option<&Decision>,
    ) -> Result<Json, ToolError> {
        let call = TaskCall::parse(arguments)?;
        let (object, title, goal, priority) = (call.object, call.title, call.goal, call.priority);
        let depends_on = match object.get("depends_on") {
            None | Some(Json::Null) => Vec::new(),
            Some(Json::Array(items)) => items.iter().map(task_ref).collect::<Result<_, _>>()?,
            Some(_) => return Err(invalid("depends_on must be a list of task ids")),
        };
        let run = self.run(context).await?;
        let agent_id = match call.agent {
            Some(name) => bound_agent(&self.db, context.project_id, name).await?,
            None => run
                .agent_id
                .ok_or_else(|| ToolError::Failed(format!("run {} has no agent", run.id)))?,
        };
        let named = named_repositories(&self.db, context.project_id, object).await?;
        // A subtask inherits its parent's origin, and its repositories if it
        // names none; a lead's task comes from you if the run works on
        // something you asked for.
        let mut space_lead_ask = None;
        let (parent_task_id, origin, repositories) = match context.task_id {
            Some(parent) => {
                let parent = self
                    .db
                    .task(parent)
                    .await
                    .map_err(tool_error)?
                    .ok_or_else(|| ToolError::Failed(format!("task {parent} not found")))?;
                let repositories = named.unwrap_or_else(|| parent.repositories.clone());
                (Some(parent.id), parent.origin, repositories)
            }
            None => {
                let events = self.db.run_events(run.id).await.map_err(tool_error)?;
                let requested = events.iter().any(|event| event.requested);
                space_lead_ask = crate::space_lead::answered_ask(&self.db, &events)
                    .await
                    .map_err(tool_error)?
                    .map(|event| event.id);
                (
                    None,
                    if requested {
                        TaskOrigin::User
                    } else {
                        TaskOrigin::Agent
                    },
                    named.unwrap_or_default(),
                )
            }
        };
        let (origin, created_by_call) = match accepted {
            Some(decision) => (
                TaskOrigin::Accepted,
                format!(
                    "s{}:perform:{}",
                    decision.session_id.unwrap_or(run.session_id),
                    decision.id
                ),
            ),
            None => (origin, effect_key(context, &run, CREATE_TASK, arguments)),
        };
        let task = self
            .db
            .create_task_for_ask(
                Some(&context.lease),
                &NewTask {
                    project_id: context.project_id,
                    agent_id,
                    title: title.to_owned(),
                    goal: goal.to_owned(),
                    repositories,
                    parent_task_id,
                    depends_on,
                    origin,
                    status: TaskStatus::Queued,
                    priority,
                    created_by_call: Some(created_by_call),
                    created_at: self.clock.now(),
                },
                space_lead_ask,
            )
            .await
            .map_err(tool_error)?;
        self.waker.wake(WakeTarget::Lane(task.lane_id));
        Ok(task_json(&task))
    }

    async fn update_task(
        &self,
        context: &ToolContext,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let object = fields(arguments, &["task_id", "title", "goal", "priority"])?;
        let task = self
            .project_task(
                context,
                object.get("task_id").ok_or_else(|| missing("task_id"))?,
            )
            .await?;
        let updated = self
            .db
            .update_task(
                &context.lease,
                task.id,
                text_field(object, "title")?,
                text_field(object, "goal")?,
                priority_field(object)?,
            )
            .await
            .map_err(tool_error)?;
        Ok(task_json(&updated))
    }

    async fn cancel_task(
        &self,
        context: &ToolContext,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let object = fields(arguments, &["task_id"])?;
        let task = self
            .project_task(
                context,
                object.get("task_id").ok_or_else(|| missing("task_id"))?,
            )
            .await?;
        // Cancelling a task that has ended leaves it as it is.
        if task.status.is_terminal() {
            return Ok(task_json(&task));
        }
        let cancelled = self
            .db
            .cancel_task(Some(&context.lease), task.id, self.clock.now())
            .await
            .map_err(tool_error)?;
        // Its pending events are skipped by the next cycle of its lane.
        self.waker.wake(WakeTarget::Lane(cancelled.lane_id));
        Ok(task_json(&cancelled))
    }

    async fn list_tasks(&self, context: &ToolContext, arguments: &Json) -> Result<Json, ToolError> {
        let object = fields(arguments, &["task_id", "all", "offset", "limit"])?;
        let agents = self.db.all_agents().await.map_err(tool_error)?;
        let agent = |task: &Task| {
            agents
                .iter()
                .find(|agent| agent.id == task.agent_id)
                .map_or(Json::Null, |agent| json!(agent.name))
        };
        if let Some(reference) = object.get("task_id") {
            let task = self.project_task(context, reference).await?;
            let mut entry = task_json(&task);
            entry["agent"] = agent(&task);
            return Ok(entry);
        }
        let all = match object.get("all") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| invalid("all must be true or false"))?,
        };
        let (offset, limit) = crate::page::offset_limit(object, TASK_PAGE, MAX_TASK_PAGE)?;
        let tasks = self
            .db
            .tasks(context.project_id)
            .await
            .map_err(tool_error)?;
        let finished: Vec<&Task> = tasks
            .iter()
            .filter(|task| task.status.is_terminal())
            .collect();
        let hidden = if all {
            0
        } else {
            finished.len().saturating_sub(RECENT_FINISHED)
        };
        // The finished tasks that ended least recently are the ones hidden by
        // default; one with no recorded end counts as ending when created.
        let ids: Vec<TaskId> = finished.iter().map(|task| task.id).collect();
        let ended = self
            .db
            .task_completion_times(&ids)
            .await
            .map_err(tool_error)?;
        let mut by_end = finished;
        by_end.sort_by_key(|task| {
            (
                ended.get(&task.id).copied().unwrap_or(task.created_at),
                task.id,
            )
        });
        let hide: std::collections::HashSet<TaskId> =
            by_end.iter().take(hidden).map(|task| task.id).collect();
        let listed: Vec<Json> = tasks
            .iter()
            .filter(|task| !hide.contains(&task.id))
            .map(|task| {
                let mut entry = task_line(task);
                entry["agent"] = agent(task);
                entry
            })
            .collect();
        let (page, next) = crate::page::take(&listed, offset, limit);
        let mut result = json!({
            "tasks": page,
            "detail": "Results are cut to one line. Call list_tasks with task_id for a task's \
                       full goal and result.",
        });
        crate::page::mark(&mut result, LIST_TASKS, listed.len(), next);
        if hidden > 0 {
            result["hidden_finished"] = json!(format!(
                "{hidden} older finished tasks are not listed. Call list_tasks with all: true \
                 to list them."
            ));
        }
        Ok(result)
    }

    /// A memory call, written as the run and keyed on the call.
    async fn memory(
        &self,
        context: &ToolContext,
        tool: &str,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let run = self.run(context).await?;
        let by = MemoryAuthor {
            lease: context.lease,
            run_id: run.id,
            call_key: effect_key(context, &run, tool, arguments),
            at: self.clock.now(),
        };
        crate::memory::execute(
            &self.db,
            &by,
            context.project_id,
            context.task_id,
            tool,
            arguments,
        )
        .await
    }

    async fn finish_task(
        &self,
        context: &ToolContext,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let object = fields(arguments, &["summary", MERGE_CHECKS])?;
        let summary = text_field(object, "summary")?.ok_or_else(|| missing("summary"))?;
        let mut result = json!({ "summary": summary });
        if let Some(merge) = object.get(MERGE_CHECKS) {
            let checks: Vec<MergeCheck> = serde_json::from_value(merge.clone())
                .map_err(|error| ToolError::InvalidArguments(format!("merge: {error}")))?;
            if checks.iter().any(|check| !check.clean())
                && self
                    .db
                    .count_merge_refusal(&context.lease, MAX_MERGE_REFUSALS)
                    .await
                    .map_err(tool_error)?
            {
                return Err(ToolError::Blocked(merge_refusal(&checks)));
            }
            result[MERGE_CHECKS] = merge.clone();
        }
        // A named Reviewer must exist. With none named, the project's own
        // agent of role "reviewer" reviews, if it has one that is not off.
        let agents = self.db.agents().await.map_err(tool_error)?;
        let reviewer = match &self.reviewer {
            Some(name) => Some(
                agents
                    .into_iter()
                    .find(|agent| agent.name == *name)
                    .ok_or_else(|| {
                        ToolError::Blocked(format!("Space Reviewer {name:?} is unavailable"))
                    })?,
            ),
            None => {
                let bindings = self
                    .db
                    .bindings(context.project_id)
                    .await
                    .map_err(tool_error)?;
                agents.into_iter().find(|agent| {
                    agent.role.eq_ignore_ascii_case("reviewer")
                        && bindings.iter().any(|binding| {
                            binding.agent_id == agent.id && binding.mode != Mode::Off
                        })
                })
            }
        };
        if let (Some(reviewer), Some(task_id)) = (reviewer, context.task_id) {
            let task = self
                .project_task(context, &json!(task_id.to_string()))
                .await?;
            // The Reviewer cannot review its own task: that finishes unreviewed.
            if reviewer.id != task.agent_id {
                let revisions = self.review_revisions(&task, &result).await?;
                self.db
                    .submit_for_review(
                        &context.lease,
                        context.run_id,
                        reviewer.id,
                        &revisions,
                        &result,
                    )
                    .await
                    .map_err(tool_error)?;
                return Ok(
                    json!({"task_id": task.id, "submitted_for_review": true, "merge": result[MERGE_CHECKS], "next": "End your turn so the Reviewer can start."}),
                );
            }
        }
        let task =
            self.db
                .finish_task(&context.lease, &result)
                .await
                .map_err(|error| match error {
                    // No progress file yet: the agent can write it and retry.
                    StoreError::Invalid(_) => crate::memory::refused(context.project_id, error),
                    other => tool_error(other),
                })?;
        Ok(json!({
            "task_id": task.id.to_string(),
            "finished": true,
            "next": "End your turn now. The task is done, and its result reaches the lead, when this run ends.",
        }))
    }

    async fn review_revisions(
        &self,
        task: &Task,
        result: &Json,
    ) -> Result<Vec<ReviewRevision>, ToolError> {
        if task.repositories.is_empty() {
            return Ok(Vec::new());
        }
        let checks: Vec<MergeCheck> = serde_json::from_value(result[MERGE_CHECKS].clone())
            .map_err(|_| {
                ToolError::Blocked(
                    "A successful merge check is required to identify the submitted commits".into(),
                )
            })?;
        let repositories = self
            .db
            .repositories(task.project_id)
            .await
            .map_err(tool_error)?;
        task.repositories
            .iter()
            .map(|id| {
                let commit = repositories
                    .iter()
                    .find(|repo| repo.id == *id)
                    .and_then(|repo| checks.iter().find(|check| check.repository == repo.name))
                    .and_then(|check| check.revision.clone())
                    .ok_or_else(|| {
                        ToolError::Blocked(format!("No submitted commit for repository {id}"))
                    })?;
                Ok(ReviewRevision {
                    repository: *id,
                    commit,
                })
            })
            .collect()
    }

    async fn finish_review(
        &self,
        context: &ToolContext,
        arguments: &Json,
    ) -> Result<Json, ToolError> {
        let object = fields(arguments, &["verdict", "feedback"])?;
        let verdict = text_field(object, "verdict")?
            .ok_or_else(|| missing("verdict"))?
            .parse::<TaskReviewVerdict>()
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        let feedback = text_field(object, "feedback")?.ok_or_else(|| missing("feedback"))?;
        self.db
            .store_task_review_verdict(&context.lease, context.run_id, verdict, feedback)
            .await
            .map_err(tool_error)?;
        Ok(json!({"verdict": verdict, "next": "End your turn to record the review."}))
    }
}

/// How many times `finish_task` is refused for merge conflicts before it
/// finishes with the conflict recorded (docs/merge-help.md decision 5).
const MAX_MERGE_REFUSALS: i64 = 2;

/// Why `finish_task` was refused: what conflicts where, and what to do.
fn merge_refusal(checks: &[MergeCheck]) -> String {
    let mut message =
        "finish_task refused: the task branch does not merge cleanly into its base.".to_owned();
    for check in checks.iter().filter(|check| !check.clean()) {
        let _ = write!(
            message,
            " In {}, it conflicts with {} at {} in: {}. Rebase onto that commit (git rebase {}, \
             already fetched as origin/{}), resolve the conflicts, and continue the rebase.",
            check.repository,
            check.branch,
            check.base,
            check.conflicts.join(", "),
            check.base,
            check.branch,
        );
    }
    message.push_str(
        " Then run the task's checks again, and call finish_task again. After two refusals, \
         finish_task is accepted with the conflict recorded in the result.",
    );
    message
}

/// What the effect of a call is keyed on, so that repeating the call repeats
/// nothing. A backend-supplied id is stable across a crash and a resume of
/// the same session, so the session scopes it; a derived key only
/// identifies the call within its run.
fn effect_key(context: &ToolContext, run: &Run, tool: &str, arguments: &Json) -> String {
    match &context.call_id {
        Some(call_id) => format!("s{}:{call_id}", run.session_id),
        None => format!("r{}:{}", run.id, derived_call_key(tool, arguments)),
    }
}

/// The fields of a `create_task` call, checked.
pub(crate) struct TaskCall<'a> {
    pub object: &'a Map<String, Json>,
    pub title: &'a str,
    pub goal: &'a str,
    pub agent: Option<&'a str>,
    pub priority: i64,
}

impl<'a> TaskCall<'a> {
    /// Checks the fields every `create_task` call must have right.
    pub(crate) fn parse(arguments: &'a Json) -> Result<Self, ToolError> {
        let object = fields(
            arguments,
            &[
                "title",
                "goal",
                "agent",
                "depends_on",
                "priority",
                "repositories",
            ],
        )?;
        Ok(Self {
            title: text_field(object, "title")?.ok_or_else(|| missing("title"))?,
            goal: text_field(object, "goal")?.ok_or_else(|| missing("goal"))?,
            agent: text_field(object, "agent")?,
            priority: priority_field(object)?.unwrap_or(0),
            object,
        })
    }
}

/// The agent bound in `project` with this name.
pub(crate) async fn bound_agent(
    db: &HubDb,
    project: ProjectId,
    name: &str,
) -> Result<AgentId, ToolError> {
    let bound: Vec<AgentId> = db
        .bindings(project)
        .await
        .map_err(tool_error)?
        .iter()
        .map(|binding| binding.agent_id)
        .collect();
    let agents = db.agents().await.map_err(tool_error)?;
    let mut names = Vec::new();
    for agent in agents.iter().filter(|agent| bound.contains(&agent.id)) {
        if agent.name == name {
            return Ok(agent.id);
        }
        names.push(agent.name.as_str());
    }
    Err(ToolError::InvalidArguments(format!(
        "no agent {name:?} in this project; it has {}",
        names.join(", ")
    )))
}

/// The repositories a `create_task` call names, by their registered
/// names; `None` if it names none. A name not registered in `project` is
/// refused (invariant 19).
pub(crate) async fn named_repositories(
    db: &HubDb,
    project: ProjectId,
    object: &Map<String, Json>,
) -> Result<Option<Vec<RepositoryId>>, ToolError> {
    let names: Vec<&str> = match object.get("repositories") {
        None | Some(Json::Null) => return Ok(None),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| invalid("repositories must be a list of repository names"))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(invalid("repositories must be a list of repository names")),
    };
    let registered = db.repositories(project).await.map_err(tool_error)?;
    let mut ids = Vec::with_capacity(names.len());
    for name in names {
        let repository = registered
            .iter()
            .find(|repository| repository.name == name.trim())
            .ok_or_else(|| {
                let known: Vec<&str> = registered
                    .iter()
                    .map(|repository| repository.name.as_str())
                    .collect();
                ToolError::InvalidArguments(format!(
                    "no repository {name:?} in this project; it has {}",
                    if known.is_empty() {
                        "none".to_owned()
                    } else {
                        known.join(", ")
                    }
                ))
            })?;
        if !ids.contains(&repository.id) {
            ids.push(repository.id);
        }
    }
    Ok(Some(ids))
}

/// A task as the agent sees it, with ids in their short form.
pub(crate) fn task_json(task: &Task) -> Json {
    json!({
        "task_id": task.id.to_string(),
        "title": task.title,
        "goal": task.goal,
        "status": task.status,
        "priority": task.priority,
        "depends_on": task.depends_on.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "parent_task_id": task.parent_task_id.map(|id| id.to_string()),
        "waiting_reason": task.waiting_reason,
        "result": task.result,
    })
}

/// The tasks `list_tasks` lists by default besides the unfinished ones.
const RECENT_FINISHED: usize = 10;
/// The tasks one page of `list_tasks` holds by default, and at most.
const TASK_PAGE: usize = 50;
const MAX_TASK_PAGE: usize = 200;
/// The longest one-line text in a listed task.
const LINE_CHARS: usize = 200;

/// A task in one line: its goal left out and its result cut to a line.
fn task_line(task: &Task) -> Json {
    let line = |text: &str| crate::page::one_line(text, LINE_CHARS);
    let result = task
        .result
        .as_ref()
        .map(|result| match result["summary"].as_str() {
            Some(summary) => line(summary),
            None => line(&result.to_string()),
        });
    json!({
        "task_id": task.id.to_string(),
        "title": line(&task.title),
        "status": task.status,
        "priority": task.priority,
        "depends_on": task.depends_on.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "parent_task_id": task.parent_task_id.map(|id| id.to_string()),
        "waiting_reason": task.waiting_reason.as_deref().map(line),
        "result": result,
    })
}

pub(crate) fn invalid(message: &str) -> ToolError {
    ToolError::InvalidArguments(message.to_owned())
}

fn missing(field: &str) -> ToolError {
    ToolError::InvalidArguments(format!("{field} is required"))
}

/// The arguments as an object with no fields but `allowed`.
pub(crate) fn fields<'a>(
    arguments: &'a Json,
    allowed: &[&str],
) -> Result<&'a Map<String, Json>, ToolError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| invalid("expected an object"))?;
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(unknown) => Err(ToolError::InvalidArguments(format!(
            "unknown field {unknown:?}"
        ))),
        None => Ok(object),
    }
}

/// A non-empty text field, or `None` if it is absent.
fn text_field<'a>(object: &'a Map<String, Json>, name: &str) -> Result<Option<&'a str>, ToolError> {
    match object.get(name) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(text)) if !text.trim().is_empty() => Ok(Some(text.as_str())),
        Some(_) => Err(ToolError::InvalidArguments(format!(
            "{name} must be non-empty text"
        ))),
    }
}

fn priority_field(object: &Map<String, Json>) -> Result<Option<i64>, ToolError> {
    match object.get("priority") {
        None | Some(Json::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| invalid("priority must be an integer")),
    }
}

/// A task id as `T-0142`, `142`, or the number 142.
pub(crate) fn task_ref(value: &Json) -> Result<TaskId, ToolError> {
    threadlane_core::workspace::task_ref(value)
        .ok_or_else(|| ToolError::InvalidArguments(format!("{value} is not a task id")))
}

/// The question of an `ask_user` call and its options, each answering with
/// its label. Labels are one line, distinct in any case, since you answer by
/// label.
pub(crate) fn question(arguments: &Json) -> Result<(String, Vec<DecisionOption>), ToolError> {
    let object = fields(arguments, &["question", "options"])?;
    let question = text_field(object, "question")?.ok_or_else(|| missing("question"))?;
    let labels: Vec<&str> = match object.get("options") {
        Some(Json::Array(items)) => items.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    };
    let well_formed = labels.iter().all(|label| {
        let label = label.trim();
        !label.is_empty()
            && label.chars().count() <= MAX_LABEL
            && !label.chars().any(char::is_control)
    });
    let distinct = labels.iter().enumerate().all(|(index, label)| {
        labels[..index]
            .iter()
            .all(|earlier| !earlier.trim().eq_ignore_ascii_case(label.trim()))
    });
    let count = object
        .get("options")
        .and_then(Json::as_array)
        .map_or(0, Vec::len);
    if count != labels.len()
        || !(2..=MAX_OPTIONS).contains(&labels.len())
        || !well_formed
        || !distinct
    {
        return Err(ToolError::InvalidArguments(format!(
            "options must be 2 to {MAX_OPTIONS} distinct one-line labels, at most {MAX_LABEL} \
             characters each"
        )));
    }
    let options = labels
        .into_iter()
        .map(|label| DecisionOption {
            label: label.trim().to_owned(),
            effect: json!({ "answer": label.trim() }),
        })
        .collect();
    Ok((question.trim().to_owned(), options))
}

fn ask_user_definition() -> ToolDefinition {
    ToolDefinition {
        name: ASK_USER.to_owned(),
        description: "Ask the user a question with a few options, when you cannot go on \
                      without their choice. The call returns at once with \"waiting on you\", \
                      and you must then end your turn; you are resumed with their answer."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "One plain question." },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 2,
                    "maxItems": MAX_OPTIONS,
                    "description": "The answers to choose from, each a short label."
                }
            },
            "required": ["question", "options"],
            "additionalProperties": false
        }),
        tier: Tier::Read,
        safe_to_repeat: false,
    }
}

/// The definitions of the task tools, in the order they are offered.
fn task_definitions() -> Vec<ToolDefinition> {
    let task_id = json!({
        "type": "string",
        "description": "The task's id, for example T-0042."
    });
    let define = |name: &str, description: &str, tier: Tier, schema: Json| ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        tier,
        safe_to_repeat: true,
    };
    vec![
        define(
            CREATE_TASK,
            "Create a task for an agent of this project to work on in its own lane. \
             Independent tasks run in parallel; give a task the ids of the tasks it must \
             wait for in depends_on. From inside a task, this creates a subtask. For work the \
             user did not ask for, they may be asked first: if the call returns \"waiting on \
             you\", end your turn.",
            Tier::Local,
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "What to do, in a few words." },
                    "goal": { "type": "string", "description": "What done looks like." },
                    "agent": {
                        "type": "string",
                        "description": "The name of the agent to do it. Defaults to you."
                    },
                    "depends_on": {
                        "type": "array",
                        "items": task_id.clone(),
                        "description": "Tasks that must be done before this one starts."
                    },
                    "priority": { "type": "integer", "description": "Higher runs first." },
                    "repositories": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "The names of this project's repositories the task \
                                        works in; it gets its own worktree of each, on its own \
                                        branch. A subtask defaults to its parent's."
                    }
                },
                "required": ["title", "goal"],
                "additionalProperties": false
            }),
        ),
        define(
            UPDATE_TASK,
            "Change the title, goal, or priority of a task that has not ended.",
            Tier::Local,
            json!({
                "type": "object",
                "properties": {
                    "task_id": task_id.clone(),
                    "title": { "type": "string" },
                    "goal": { "type": "string" },
                    "priority": { "type": "integer" }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
        ),
        define(
            CANCEL_TASK,
            "Cancel a task that has not ended.",
            Tier::Local,
            json!({
                "type": "object",
                "properties": { "task_id": task_id },
                "required": ["task_id"],
                "additionalProperties": false
            }),
        ),
        define(
            LIST_TASKS,
            "List this project's tasks with their status, why a queued one waits, and the \
             result of a finished one in one line. By default it lists the unfinished tasks \
             and the 10 most recent finished ones, in pages. Pass task_id for one task's full \
             goal and result.",
            Tier::Read,
            json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Show this one task in full instead of listing."
                    },
                    "all": {
                        "type": "boolean",
                        "description": "Also list the older finished tasks."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Where the page starts; a cut page gives the next one."
                    },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_TASK_PAGE }
                },
                "additionalProperties": false
            }),
        ),
        define(
            FINISH_TASK,
            "Report your task done with a summary of the result, then end your turn. \
             The summary is what the lead sees. Write your progress file, \
             project/tasks/<task id>.md, first: finishing is refused without it.",
            Tier::Local,
            json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "What was done, and anything the lead needs to know."
                    }
                },
                "required": ["summary"],
                "additionalProperties": false
            }),
        ),
        define(
            FINISH_REVIEW,
            "Record your independent verdict for the submitted revision, then end the turn. Give file and line for each requested correction.",
            Tier::Local,
            json!({ "type": "object", "properties": {
                "verdict": {"type": "string", "enum": ["approved", "changes_requested"]},
                "feedback": {"type": "string"}
            }, "required": ["verdict", "feedback"], "additionalProperties": false }),
        ),
    ]
}

fn parse_send_message(arguments: &Json) -> Result<(String, Urgency), ToolError> {
    let invalid = |message: &str| ToolError::InvalidArguments(message.to_owned());
    let object: &Map<String, Json> = arguments
        .as_object()
        .ok_or_else(|| invalid("expected an object with a text field"))?;
    if let Some(unknown) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "text" | "urgency"))
    {
        return Err(ToolError::InvalidArguments(format!(
            "unknown field {unknown:?}"
        )));
    }
    let text = object
        .get("text")
        .and_then(Json::as_str)
        .ok_or_else(|| invalid("text must be a string"))?;
    if text.trim().is_empty() {
        return Err(invalid("text must not be empty"));
    }
    let urgency = match object.get("urgency") {
        None | Some(Json::Null) => Urgency::Normal,
        Some(Json::String(value)) => value
            .parse()
            .map_err(|_| invalid("urgency must be low, normal, or high"))?,
        Some(_) => return Err(invalid("urgency must be low, normal, or high")),
    };
    Ok((text.to_owned(), urgency))
}

#[async_trait]
impl ToolRegistry for HubTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = vec![Self::send_message_definition()];
        definitions.extend(task_definitions());
        definitions.extend(crate::publish::definitions());
        definitions.extend(crate::memory::definitions());
        definitions.extend(crate::timers::definitions());
        definitions.push(ask_user_definition());
        definitions.push(crate::space_lead::definition());
        definitions
    }

    fn definition(&self, name: &str) -> Option<ToolDefinition> {
        self.definitions()
            .into_iter()
            .find(|definition| definition.name == name)
    }

    async fn execute(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        match name {
            SEND_MESSAGE => self.send_message(context, &arguments).await,
            CREATE_TASK => self.create_task(context, &arguments).await,
            UPDATE_TASK => self.update_task(context, &arguments).await,
            CANCEL_TASK => self.cancel_task(context, &arguments).await,
            LIST_TASKS => self.list_tasks(context, &arguments).await,
            FINISH_TASK => self.finish_task(context, &arguments).await,
            FINISH_REVIEW => self.finish_review(context, &arguments).await,
            crate::space_lead::ASK_PROJECT_LEAD => {
                let run = self.run(context).await?;
                let key = effect_key(context, &run, name, &arguments);
                crate::space_lead::ask(
                    &self.db,
                    self.waker.as_ref(),
                    self.clock.now(),
                    context.project_id,
                    &run,
                    key,
                    &arguments,
                )
                .await
            }
            name if crate::memory::is_memory(name) => self.memory(context, name, &arguments).await,
            name if crate::timers::is_timer(name) => {
                let run = self.run(context).await?;
                let key = effect_key(context, &run, name, &arguments);
                crate::timers::execute(
                    &self.db,
                    self.clock.now(),
                    context,
                    &run,
                    key,
                    name,
                    &arguments,
                )
                .await
            }
            // The gate asks before these, and the runner performs them.
            PUBLISH_BRANCH | OPEN_PULL_REQUEST | COMMENT | CREATE_ISSUE | MERGE | MARK_READY => {
                Err(ToolError::Blocked(format!(
                    "{name} is performed only once the user approves it"
                )))
            }
            // The gate always asks: the answer is the outcome.
            ASK_USER => Err(ToolError::Blocked(format!(
                "{name} is answered by the user, through a decision"
            ))),
            other => Err(ToolError::UnknownTool(other.to_owned())),
        }
    }
}

/// Several registries as one. A name resolves to the first registry that
/// defines it, so a registry earlier in the list shadows later ones.
#[derive(Debug, Clone)]
pub struct CombinedTools {
    registries: Vec<Arc<dyn ToolRegistry>>,
}

impl CombinedTools {
    /// Combines `registries`, earlier ones first.
    #[must_use]
    pub fn new(registries: Vec<Arc<dyn ToolRegistry>>) -> Self {
        Self { registries }
    }

    fn owner(&self, name: &str) -> Option<&Arc<dyn ToolRegistry>> {
        self.registries
            .iter()
            .find(|registry| registry.definition(name).is_some())
    }
}

#[async_trait]
impl ToolRegistry for CombinedTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions: Vec<ToolDefinition> = Vec::new();
        for registry in &self.registries {
            for definition in registry.definitions() {
                if definitions.iter().all(|seen| seen.name != definition.name) {
                    definitions.push(definition);
                }
            }
        }
        definitions
    }

    fn definition(&self, name: &str) -> Option<ToolDefinition> {
        self.owner(name)
            .and_then(|registry| registry.definition(name))
    }

    async fn execute(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        match self.owner(name) {
            Some(registry) => registry.execute(context, name, arguments).await,
            None => Err(ToolError::UnknownTool(name.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_keys_ignore_key_order_and_whitespace() {
        let a: Json = serde_json::from_str(r#"{"text": "hi", "urgency": "low"}"#).unwrap();
        let b: Json = serde_json::from_str(r#"{"urgency":"low","text":"hi"}"#).unwrap();
        assert_eq!(
            derived_call_key("send_message", &a),
            derived_call_key("send_message", &b)
        );
        assert_ne!(
            derived_call_key("send_message", &a),
            derived_call_key("send_message", &json!({"text": "hello"}))
        );
        let key = derived_call_key("send_message", &a);
        assert!(key.starts_with("send_message:"));
        assert_eq!(key.len(), "send_message:".len() + 64);
    }

    #[test]
    fn a_listed_task_has_its_result_in_one_short_line_and_no_goal() {
        let summary = format!("Merged.\n\n{}", "detail ".repeat(500));
        let task = Task {
            id: TaskId(7),
            project_id: ProjectId(1),
            lane_id: threadlane_core::ids::LaneId(1),
            agent_id: AgentId(1),
            title: "page".into(),
            goal: "g".repeat(5_000),
            repositories: vec![],
            parent_task_id: None,
            depends_on: vec![TaskId(3)],
            origin: TaskOrigin::User,
            status: TaskStatus::Done,
            priority: 0,
            created_by_call: None,
            last_machine_id: None,
            waiting_reason: None,
            result: Some(json!({ "summary": summary, "review": { "feedback": "x" } })),
            created_at: threadlane_core::time::Timestamp::from_millis(0),
        };
        let line = task_line(&task);
        let result = line["result"].as_str().unwrap();
        assert!(result.starts_with("Merged. detail detail"), "{result}");
        assert!(!result.contains('\n'));
        assert!(result.chars().count() < LINE_CHARS + 30, "{result}");
        assert_eq!(line.get("goal"), None);
        assert_eq!(line["task_id"], "T-0007");
        assert_eq!(line["depends_on"], json!(["T-0003"]));
        assert!(line.to_string().len() < 600);
    }

    #[test]
    fn task_ids_are_read_in_their_short_and_raw_forms() {
        for good in [json!("T-0042"), json!("t-42"), json!(" 42 "), json!(42)] {
            assert_eq!(task_ref(&good), Ok(TaskId(42)), "{good}");
        }
        for bad in [
            json!("T-"),
            json!("X-1"),
            json!(0),
            json!(-3),
            json!(1.5),
            json!(null),
        ] {
            assert!(task_ref(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn each_lane_kind_is_offered_its_own_tools() {
        let offered = |kind| -> Vec<String> {
            let mut names = vec![SEND_MESSAGE.to_owned()];
            names.extend(task_definitions().into_iter().map(|tool| tool.name));
            names.extend(
                crate::publish::definitions()
                    .into_iter()
                    .map(|tool| tool.name),
            );
            names.extend(
                crate::memory::definitions()
                    .into_iter()
                    .map(|tool| tool.name),
            );
            names.extend(
                crate::timers::definitions()
                    .into_iter()
                    .map(|tool| tool.name),
            );
            names.push(ASK_USER.to_owned());
            names.push(crate::space_lead::ASK_PROJECT_LEAD.to_owned());
            names
                .into_iter()
                .filter(|name| offered_in(name, kind))
                .collect()
        };
        let memory = [
            crate::memory::MEMORY_VIEW,
            crate::memory::MEMORY_CREATE,
            crate::memory::MEMORY_EDIT,
            crate::memory::MEMORY_DELETE,
            crate::memory::MEMORY_RENAME,
            crate::timers::SET_FOLLOW_UP,
            crate::timers::CREATE_SCHEDULE,
            crate::timers::LIST_TIMERS,
            crate::timers::CANCEL_TIMER,
            ASK_USER,
        ];
        assert_eq!(
            offered(LaneKind::Lead),
            [
                &[
                    SEND_MESSAGE,
                    CREATE_TASK,
                    UPDATE_TASK,
                    CANCEL_TASK,
                    LIST_TASKS
                ][..],
                &memory,
                &[crate::space_lead::ASK_PROJECT_LEAD]
            ]
            .concat()
        );
        assert_eq!(
            offered(LaneKind::Task),
            [
                &[
                    CREATE_TASK,
                    FINISH_TASK,
                    PUBLISH_BRANCH,
                    OPEN_PULL_REQUEST,
                    COMMENT,
                    CREATE_ISSUE,
                    MERGE,
                    MARK_READY
                ][..],
                &memory
            ]
            .concat()
        );
        assert!(
            offered_in("read_notes", LaneKind::Task) && offered_in("read_notes", LaneKind::Lead)
        );
    }

    #[test]
    fn a_question_needs_two_to_six_distinct_one_line_options() {
        let (asked, options) =
            question(&json!({ "question": " Which? ", "options": ["Blue", " green "] })).unwrap();
        assert_eq!(asked, "Which?");
        let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
        assert_eq!(labels, ["Blue", "green"]);
        assert!(options.iter().all(|option| !option.approves()));
        for bad in [
            json!({ "question": "Which?", "options": ["only"] }),
            json!({ "question": "Which?", "options": ["a", "A"] }),
            json!({ "question": "Which?", "options": ["a", "two\nlines"] }),
            json!({ "question": "Which?", "options": ["a", 2] }),
            json!({ "question": "Which?", "options": ["1", "2", "3", "4", "5", "6", "7"] }),
            json!({ "question": " ", "options": ["a", "b"] }),
            json!({ "options": ["a", "b"] }),
        ] {
            assert!(question(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn send_message_arguments_are_checked() {
        assert_eq!(
            parse_send_message(&json!({"text": "hi"})).unwrap(),
            ("hi".to_owned(), Urgency::Normal)
        );
        assert_eq!(
            parse_send_message(&json!({"text": "hi", "urgency": "high"})).unwrap(),
            ("hi".to_owned(), Urgency::High)
        );
        for bad in [
            json!("hi"),
            json!({}),
            json!({"text": "  "}),
            json!({"text": "hi", "urgency": "now"}),
            json!({"text": "hi", "to": "everyone"}),
        ] {
            assert!(
                matches!(
                    parse_send_message(&bad),
                    Err(ToolError::InvalidArguments(_))
                ),
                "{bad} is rejected"
            );
        }
    }
}
