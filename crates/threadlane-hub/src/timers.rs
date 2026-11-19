//! Follow-ups and routines (Plan.md §7, docs/milestone-8.md §2): the timer
//! tools, the scheduler pass, and the client's Timers capability.
//!
//! A timer fires into the lane that set it. Its events count as work you
//! requested as its creator's did (decision 3): your routine always; an
//! agent's follow-up when the run that set it was requested; an agent's
//! routine never (Plan.md §7.3), so its events follow the initiative rule.
//! An agent's routine asks you first below Autonomous (decision 4,
//! docs/milestone-9.md stage B); the hub creates it once you approve, keyed
//! on the decision ([`crate::gateway`]). The guardrails of decision 5 bind
//! agents; your own routines are only checked for sense.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{Map, Value as Json, json};
use threadlane_core::client::{ApiError, TimerReply, TimerRequest};
use threadlane_core::ids::{AgentId, LaneId, ProjectId, RunId, TaskId, TimerId};
use threadlane_core::model::{
    Decision, Inserted, Lane, MAX_LIVE_TIMERS, MIN_FOLLOW_UP_DELAY, MIN_SCHEDULE_INTERVAL,
    NewTimer, Recurrence, Run, Timer, TimerCategory, TimerCreator, TimerStatus,
};
use threadlane_core::runtime::steps::scheduler;
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{OrgStore, SessionStore, StoreError, TaskStore, TimerStore};
use threadlane_core::time::Timestamp;
use threadlane_core::tools::{Tier, ToolContext, ToolDefinition, ToolError};
use threadlane_store::HubDb;

use crate::context::format_time;
use crate::core::HubCore;
use crate::error::{HubError, api_error, tool_error};
use crate::publish::Proposal;
use crate::recurrence::{check, next_occurrence};
use crate::tools::{fields, invalid};

/// Wakes the calling lane once, later, with an instruction.
pub const SET_FOLLOW_UP: &str = "set_follow_up";
/// Asks to create a routine in the calling lane.
pub const CREATE_SCHEDULE: &str = "create_schedule";
/// Lists the project's timers.
pub const LIST_TIMERS: &str = "list_timers";
/// Cancels a timer an agent set.
pub const CANCEL_TIMER: &str = "cancel_timer";

/// Whether `tool` is a timer tool.
pub(crate) fn is_timer(tool: &str) -> bool {
    matches!(
        tool,
        SET_FOLLOW_UP | CREATE_SCHEDULE | LIST_TIMERS | CANCEL_TIMER
    )
}

/// Whether the run counts as work you requested, as begin-run decides it:
/// its task came from you, or an event it consumes was requested.
pub(crate) async fn run_requested(
    db: &HubDb,
    run: RunId,
    task: Option<TaskId>,
) -> Result<bool, StoreError> {
    if let Some(task) = task
        && db
            .task(task)
            .await?
            .is_some_and(|task| task.origin.is_requested())
    {
        return Ok(true);
    }
    Ok(db
        .run_events(run)
        .await?
        .iter()
        .any(|event| event.requested))
}

/// Refuses a new timer in a project that has [`MAX_LIVE_TIMERS`] already,
/// unless one of them is the one this call made before.
async fn within_cap(db: &HubDb, project: ProjectId, call: &str) -> Result<(), ToolError> {
    let live = db.project_timers(project).await.map_err(tool_error)?;
    if live.len() >= MAX_LIVE_TIMERS
        && !live
            .iter()
            .any(|timer| timer.created_by_call.as_deref() == Some(call))
    {
        return Err(ToolError::InvalidArguments(format!(
            "this project has {} timers that have not ended, and the limit is {MAX_LIVE_TIMERS}; \
             cancel one first",
            live.len()
        )));
    }
    Ok(())
}

/// An instruction: non-empty text.
fn instruction(object: &Map<String, Json>) -> Result<String, ToolError> {
    match object.get("instruction") {
        Some(Json::String(text)) if !text.trim().is_empty() => Ok(text.trim().to_owned()),
        _ => Err(invalid("instruction must be non-empty text")),
    }
}

/// A `create_schedule` call's instruction and recurrence, checked against
/// the agent guardrails.
fn schedule(arguments: &Json) -> Result<(String, Recurrence), ToolError> {
    let object = fields(
        arguments,
        &[
            "instruction",
            "rule",
            "minutes",
            "days",
            "day",
            "hour",
            "minute",
            "zone",
        ],
    )?;
    let instruction = instruction(object)?;
    let mut rest = object.clone();
    rest.remove("instruction");
    let recurrence: Recurrence = serde_json::from_value(Json::Object(rest))
        .map_err(|error| ToolError::InvalidArguments(format!("not a recurrence: {error}")))?;
    check(&recurrence).map_err(ToolError::InvalidArguments)?;
    if recurrence.min_interval() < MIN_SCHEDULE_INTERVAL {
        return Err(ToolError::InvalidArguments(format!(
            "a routine may repeat at most every {} minutes, the minimum interval",
            MIN_SCHEDULE_INTERVAL.as_secs() / 60
        )));
    }
    Ok((instruction, recurrence))
}

/// A timer as an agent sees it.
fn timer_json(timer: &Timer) -> Json {
    json!({
        "timer_id": timer.id.to_string(),
        "category": timer.category,
        "instruction": timer.instruction,
        "status": timer.status,
        "due_at": format_time(timer.due_at),
        "recurrence": timer.recurrence.as_ref().map(ToString::to_string),
        "created_by": timer.created_by,
        "task_id": timer.task_ref.map(|id| id.to_string()),
    })
}

/// Performs a timer call of `run`, keyed on `key`.
pub(crate) async fn execute(
    db: &HubDb,
    now: Timestamp,
    context: &ToolContext,
    run: &Run,
    key: String,
    tool: &str,
    arguments: &Json,
) -> Result<Json, ToolError> {
    match tool {
        SET_FOLLOW_UP => {
            let object = fields(arguments, &["instruction", "in_minutes", "at"])?;
            let instruction = instruction(object)?;
            let due_at = match (object.get("in_minutes"), object.get("at")) {
                (Some(minutes), None) => {
                    let minutes = minutes
                        .as_u64()
                        .ok_or_else(|| invalid("in_minutes must be a whole number of minutes"))?;
                    now + Duration::from_secs(minutes.saturating_mul(60))
                }
                (None, Some(Json::String(at))) => {
                    let at = at.trim().parse::<jiff::Timestamp>().map_err(|_| {
                        invalid("at must be a time like 2026-05-01T09:00:00Z, with its offset")
                    })?;
                    Timestamp::from_millis(at.as_millisecond())
                }
                _ => return Err(invalid("give either in_minutes or at")),
            };
            if due_at < now + MIN_FOLLOW_UP_DELAY {
                return Err(ToolError::InvalidArguments(format!(
                    "a follow-up must be at least {} minute away, the minimum delay",
                    MIN_FOLLOW_UP_DELAY.as_secs() / 60
                )));
            }
            within_cap(db, context.project_id, &key).await?;
            let requested = run_requested(db, run.id, context.task_id)
                .await
                .map_err(tool_error)?;
            let ask = if context.task_id.is_none() {
                let events = db.run_events(run.id).await.map_err(tool_error)?;
                crate::space_lead::answered_ask(db, &events)
                    .await
                    .map_err(tool_error)?
                    .map(|event| event.id)
            } else {
                None
            };
            let timer = db
                .create_timer_for_ask(
                    &NewTimer {
                        project_id: context.project_id,
                        lane_id: context.lease.lane_id,
                        agent_id: context.agent_id,
                        task_ref: context.task_id,
                        category: TimerCategory::FollowUp,
                        due_at,
                        recurrence: None,
                        instruction,
                        created_by: TimerCreator::Agent,
                        created_by_call: Some(key),
                        requested,
                    },
                    ask,
                )
                .await
                .map_err(tool_error)?;
            Ok(timer_json(&timer))
        }
        LIST_TIMERS => {
            fields(arguments, &[])?;
            let timers = db
                .project_timers(context.project_id)
                .await
                .map_err(tool_error)?;
            Ok(json!({ "timers": timers.iter().map(timer_json).collect::<Vec<_>>() }))
        }
        CANCEL_TIMER => {
            let object = fields(arguments, &["timer_id"])?;
            let id = object
                .get("timer_id")
                .and_then(|id| match id {
                    Json::Number(number) => number.as_i64(),
                    Json::String(text) => text.trim().parse().ok(),
                    _ => None,
                })
                .map(TimerId)
                .ok_or_else(|| invalid("timer_id must be a timer's id, for example 3"))?;
            let timer = db
                .timer(id)
                .await
                .map_err(tool_error)?
                .filter(|timer| {
                    timer.project_id == context.project_id
                        && timer.created_by != TimerCreator::System
                })
                .ok_or_else(|| ToolError::InvalidArguments(format!("no timer {id} here")))?;
            if timer.created_by == TimerCreator::User {
                return Err(ToolError::InvalidArguments(format!(
                    "timer {id} is a routine the user made; only they can cancel it"
                )));
            }
            db.cancel_timer(id).await.map_err(tool_error)?;
            let timer = db
                .timer(id)
                .await
                .map_err(tool_error)?
                .ok_or_else(|| ToolError::Failed(format!("timer {id} not found")))?;
            Ok(timer_json(&timer))
        }
        // The gate let it through without asking: Autonomous.
        CREATE_SCHEDULE => {
            routine(
                db,
                now,
                context.project_id,
                context.lease.lane_id,
                context.task_id,
                context.agent_id,
                arguments,
                key,
            )
            .await
        }
        other => Err(ToolError::UnknownTool(other.to_owned())),
    }
}

/// What a decision on `create_schedule` asks you. The call is checked
/// first, so a routine the guardrails refuse is refused, not asked.
pub(crate) async fn proposal(
    db: &HubDb,
    lane: &Lane,
    arguments: &Json,
) -> Result<Proposal, ToolError> {
    let (instruction, recurrence) = schedule(arguments)?;
    within_cap(db, lane.project_id, "").await?;
    let lane = match lane.task_id {
        Some(task) => format!("task {task}'s lane"),
        None => "the lead's lane".to_owned(),
    };
    Ok(Proposal {
        question: format!("Create a routine {recurrence}?"),
        context: format!(
            "An agent asks to be woken in {lane} {recurrence}, each time with this \
             instruction:\n\n{instruction}"
        ),
    })
}

/// Creates the routine of an approved `create_schedule` in the decision's
/// lane, keyed on the decision so performing it again creates nothing more
/// (invariant 5).
pub(crate) async fn perform_schedule(
    db: &HubDb,
    now: Timestamp,
    lane: &Lane,
    decision: &Decision,
) -> Result<Json, ToolError> {
    let asked = match decision.run_id {
        Some(id) => db.run(id).await.map_err(tool_error)?,
        None => None,
    };
    routine(
        db,
        now,
        lane.project_id,
        lane.id,
        lane.task_id,
        asked.and_then(|run| run.agent_id),
        &decision.arguments,
        format!("perform:{}", decision.id),
    )
    .await
}

/// Creates an agent's routine in `lane_id`, keyed on `key`. Its scheduled events are
/// never requested work (Plan.md §7.3).
#[allow(clippy::too_many_arguments, reason = "the routine and where it goes")]
async fn routine(
    db: &HubDb,
    now: Timestamp,
    project_id: ProjectId,
    lane_id: LaneId,
    task_ref: Option<TaskId>,
    agent_id: Option<AgentId>,
    arguments: &Json,
    key: String,
) -> Result<Json, ToolError> {
    let (instruction, recurrence) = schedule(arguments)?;
    within_cap(db, project_id, &key).await?;
    let due_at = next_occurrence(&recurrence, now, now).map_err(ToolError::InvalidArguments)?;
    let timer = db
        .create_timer(&NewTimer {
            project_id,
            lane_id,
            agent_id,
            task_ref,
            category: TimerCategory::Schedule,
            due_at,
            recurrence: Some(recurrence),
            instruction,
            created_by: TimerCreator::Agent,
            created_by_call: Some(key),
            requested: false,
        })
        .await
        .map_err(tool_error)?;
    Ok(timer_json(&timer))
}

/// The definitions of the timer tools, in the order they are offered.
pub(crate) fn definitions() -> Vec<ToolDefinition> {
    let instruction = json!({
        "type": "string",
        "description": "What to do when it fires; it is shown to you then."
    });
    let define = |name: &str, description: &str, tier: Tier, schema: Json| ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        tier,
        // Creating is keyed on the call, and the rest set a state.
        safe_to_repeat: true,
    };
    vec![
        define(
            SET_FOLLOW_UP,
            "Wake yourself later in this lane: at the time, an event with your instruction \
             arrives here and you are run with it. Give in_minutes or at; it must be at least \
             a minute away. Finished CI checks, reviews and comments on a pull request this \
             task opened, commented on or merged wake you by themselves, so do not set a \
             follow-up to wait for CI.",
            Tier::Local,
            json!({
                "type": "object",
                "properties": {
                    "instruction": instruction.clone(),
                    "in_minutes": { "type": "integer", "minimum": 1 },
                    "at": {
                        "type": "string",
                        "description": "A time with its offset, for example 2026-05-01T09:00:00Z."
                    }
                },
                "required": ["instruction"],
                "additionalProperties": false
            }),
        ),
        define(
            CREATE_SCHEDULE,
            "Create a routine that wakes you in this lane on a cadence, in a time zone: \
             rule interval (every minutes, at least 15), daily (hour, minute), weekdays \
             (days such as [\"mon\", \"fri\"], hour, minute) or monthly (day, hour, minute). \
             The user may be asked first: if the call returns \"waiting on you\", you must \
             then end your turn.",
            Tier::Local,
            json!({
                "type": "object",
                "properties": {
                    "instruction": instruction,
                    "rule": { "type": "string", "enum": ["interval", "daily", "weekdays", "monthly"] },
                    "minutes": { "type": "integer", "minimum": 15 },
                    "days": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] }
                    },
                    "day": { "type": "integer", "minimum": 1, "maximum": 31 },
                    "hour": { "type": "integer", "minimum": 0, "maximum": 23 },
                    "minute": { "type": "integer", "minimum": 0, "maximum": 59 },
                    "zone": {
                        "type": "string",
                        "description": "An IANA time zone, for example Europe/Berlin."
                    }
                },
                "required": ["instruction", "rule", "zone"],
                "additionalProperties": false
            }),
        ),
        define(
            LIST_TIMERS,
            "List this project's follow-ups and routines that have not ended.",
            Tier::Read,
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        define(
            CANCEL_TIMER,
            "Cancel a follow-up or a routine an agent set. Routines the user made are theirs.",
            Tier::Local,
            json!({
                "type": "object",
                "properties": { "timer_id": { "type": "string" } },
                "required": ["timer_id"],
                "additionalProperties": false
            }),
        ),
    ]
}

impl HubCore {
    /// One scheduler pass (Plan.md §7.3): fires every active timer that is
    /// due, each in one transaction, then wakes the lanes that got events.
    /// A routine that missed several occurrences fires once, for the one it
    /// was due at, and moves to its next occurrence after now. Returns how
    /// many events were inserted.
    ///
    /// # Errors
    ///
    /// A store error, or [`HubError::Killed`] at an injected crash.
    pub async fn schedule_once(&self) -> Result<usize, HubError> {
        let inner = &self.inner;
        let now = inner.clock.now();
        let due = inner.db.due_timers(now).await?;
        inner.hook.reach(scheduler::AFTER_SCAN)?;
        let mut lanes = BTreeSet::new();
        for timer in due {
            let next = match &timer.recurrence {
                Some(recurrence) if timer.category != TimerCategory::FollowUp => {
                    match next_occurrence(recurrence, timer.due_at, now) {
                        Ok(next) => Some(next),
                        Err(error) => {
                            tracing::error!(timer = %timer.id, %error, "routine cannot fire");
                            continue;
                        }
                    }
                }
                _ => None,
            };
            match inner.db.fire(timer.id, timer.due_at, next, now).await {
                Ok(Inserted::New(_)) => {
                    lanes.insert(timer.lane_id);
                }
                Ok(Inserted::Duplicate(_)) => {}
                // Paused, cancelled, moved or deleted since the scan.
                Err(StoreError::Conflict(_) | StoreError::NotFound { kind: "timer", .. }) => {}
                Err(error) => return Err(error.into()),
            }
            inner.hook.reach(scheduler::AFTER_FIRE)?;
        }
        for lane in &lanes {
            inner.wake.wake(WakeTarget::Lane(*lane));
        }
        inner.hook.reach(scheduler::AFTER_WAKE)?;
        Ok(lanes.len())
    }
}

/// The client's routine: its fields checked and its zone known.
fn checked(recurrence: &Recurrence) -> Result<(), ApiError> {
    check(recurrence).map_err(ApiError::Invalid)
}

/// A timer of this hub, or not found.
async fn timer(db: &HubDb, id: TimerId) -> Result<Timer, ApiError> {
    db.timer(id)
        .await
        .map_err(api_error)?
        .filter(|timer| timer.category != TimerCategory::Dreaming)
        .ok_or_else(|| ApiError::NotFound(format!("timer {id}")))
}

/// A change refused because the timer has ended, as the client sees it.
fn ended(error: StoreError) -> ApiError {
    match error {
        StoreError::Conflict(message) => ApiError::Invalid(message),
        StoreError::NotFound { .. } => ApiError::NotFound(error.to_string()),
        other => api_error(other),
    }
}

/// Answers a [`TimerRequest`].
pub(crate) async fn handle(core: &HubCore, request: TimerRequest) -> Result<TimerReply, ApiError> {
    let inner = &core.inner;
    let db = &inner.db;
    let project = |name: Option<String>| async move {
        match name {
            None => db.general_project().await.map_err(api_error),
            Some(name) => db
                .projects()
                .await
                .map_err(api_error)?
                .into_iter()
                .find(|project| project.name == name)
                .ok_or_else(|| ApiError::NotFound(format!("project {name:?}"))),
        }
    };
    match request {
        TimerRequest::List { project: name } => Ok(TimerReply::Timers {
            timers: db
                .project_timers(project(name).await?.id)
                .await
                .map_err(api_error)?,
        }),
        TimerRequest::CreateRoutine {
            project: name,
            instruction,
            recurrence,
            client_request_id,
        } => {
            let instruction = instruction.trim();
            if instruction.is_empty() {
                return Err(ApiError::Invalid("the instruction is empty".into()));
            }
            if client_request_id.trim().is_empty() {
                return Err(ApiError::Invalid("the client request id is empty".into()));
            }
            checked(&recurrence)?;
            let project = project(name).await?;
            let now = inner.clock.now();
            let due_at = next_occurrence(&recurrence, now, now).map_err(ApiError::Invalid)?;
            let timer = db
                .create_timer(&NewTimer {
                    project_id: project.id,
                    lane_id: project.lead_lane_id,
                    agent_id: None,
                    task_ref: None,
                    category: TimerCategory::Schedule,
                    due_at,
                    recurrence: Some(recurrence),
                    instruction: instruction.to_owned(),
                    created_by: TimerCreator::User,
                    created_by_call: Some(format!("client:{client_request_id}")),
                    requested: true,
                })
                .await
                .map_err(api_error)?;
            Ok(TimerReply::Timer { timer })
        }
        TimerRequest::RunOnce {
            timer: id,
            client_request_id,
        } => {
            if client_request_id.trim().is_empty() || client_request_id.len() > 128 {
                return Err(ApiError::Invalid("Invalid run request ID".into()));
            }
            let timer = db
                .run_routine_once(id, &client_request_id, inner.clock.now())
                .await
                .map_err(ended)?;
            inner.wake.wake(WakeTarget::Lane(timer.lane_id));
            Ok(TimerReply::Timer { timer })
        }
        TimerRequest::Pause { timer: id } => {
            timer(db, id).await?;
            Ok(TimerReply::Timer {
                timer: db.pause_timer(id).await.map_err(ended)?,
            })
        }
        TimerRequest::Resume { timer: id } => {
            let current = timer(db, id).await?;
            let now = inner.clock.now();
            let due_at = match (&current.recurrence, current.status) {
                (Some(recurrence), TimerStatus::Paused) if current.due_at <= now => Some(
                    next_occurrence(recurrence, current.due_at, now).map_err(ApiError::Invalid)?,
                ),
                _ => None,
            };
            Ok(TimerReply::Timer {
                timer: db.resume_timer(id, due_at).await.map_err(ended)?,
            })
        }
        TimerRequest::Cancel { timer: id } => {
            timer(db, id).await?;
            db.cancel_timer(id).await.map_err(ended)?;
            Ok(TimerReply::Timer {
                timer: timer(db, id).await?,
            })
        }
        other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
    }
}
