//! The Overview capability (Plan.md §11.1, §11.2): the tasks, conversation
//! history, and machines the app's screens show. Read-only.

use std::collections::HashMap;

use threadlane_core::client::{
    ActivityCursor, ApiError, ConversationEntry, DigestResult, FeedItem, HubApi as _, LeadRun,
    OverviewReply, OverviewRequest, RecordedEvent, RunStep, RunSummary, RunUsage,
    TaskActivityEntry, TaskDetail, TaskSummary, WorkOverview,
};
use threadlane_core::ids::{AgentId, MachineId, ProjectId, TaskId};
use threadlane_core::model::{
    Event, EventKind, JournalEntry, OutboxMessage, Run, RunOutcome, RunStatus, Task, TaskStatus,
};
use threadlane_core::store::{
    DecisionStore, InboxStore, MachineStore, OrgStore, OutboxStore, SessionStore, TaskStore,
};
use threadlane_core::time::Timestamp;
use threadlane_core::workspace::task_branch;
use threadlane_store::HubDb;

use super::{ClientApi, FEED_PAGE};
use crate::begin_run::SKIP_REASON;
use crate::context::event_text;
use crate::error::api_error;
use crate::tools::CLIENT_CHANNEL;

/// The names a summary shows instead of ids.
struct Names {
    projects: HashMap<ProjectId, String>,
    agents: HashMap<AgentId, String>,
    machines: HashMap<MachineId, String>,
}

impl Names {
    async fn of(db: &HubDb) -> Result<Self, ApiError> {
        Ok(Self {
            projects: db
                .projects()
                .await
                .map_err(api_error)?
                .into_iter()
                .map(|project| (project.id, project.name))
                .collect(),
            // Deleted agents too: finished tasks still name them.
            agents: db
                .all_agents()
                .await
                .map_err(api_error)?
                .into_iter()
                .map(|agent| (agent.id, agent.name))
                .collect(),
            machines: Self::machines(db).await?,
        })
    }

    async fn machines(db: &HubDb) -> Result<HashMap<MachineId, String>, ApiError> {
        Ok(db
            .machines()
            .await
            .map_err(api_error)?
            .into_iter()
            .map(|machine| (machine.id, machine.name))
            .collect())
    }

    fn machine(&self, id: MachineId) -> String {
        self.machines
            .get(&id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }
}

fn one_line(text: &str) -> String {
    const LIMIT: usize = 160;
    let mut chars = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .chars();
    let mut line: String = chars.by_ref().take(LIMIT).collect();
    if chars.next().is_some() {
        line.push('…');
    }
    line
}

fn step(entry: &JournalEntry) -> Option<RunStep> {
    let kind = entry.entry["type"].as_str()?;
    let description = match kind {
        "tool_use" => entry.entry["name"].as_str().map_or_else(
            || "Called a tool".to_owned(),
            |name| format!("Called {}", one_line(name)),
        ),
        "text" => {
            let text = one_line(entry.entry["text"].as_str().unwrap_or_default());
            if text.is_empty() {
                "Message".to_owned()
            } else {
                text
            }
        }
        "thinking" | "reasoning" => "Thinking".to_owned(),
        "session_started" => "Started the session".to_owned(),
        "session_missing" => "Session unavailable".to_owned(),
        "tool_result" if entry.entry["is_error"].as_bool() == Some(true) => {
            "Tool failed".to_owned()
        }
        "tool_result" => "Tool returned".to_owned(),
        "usage_limit" => "Reached the account limit".to_owned(),
        "turn_boundary" => "Finished a turn".to_owned(),
        "finished" => "Finished the run".to_owned(),
        "failed" => "Run failed".to_owned(),
        "warning" => one_line(entry.entry["message"].as_str().unwrap_or("Warning")),
        _ => "Run activity".to_owned(),
    };
    Some(RunStep {
        kind: kind.to_owned(),
        description,
        at: entry.at,
    })
}

fn usage(result: &JournalEntry) -> Option<RunUsage> {
    let reported = &result.entry["usage"];
    let input_tokens = reported["usage"]["input_tokens"].as_u64();
    let output_tokens = reported["usage"]["output_tokens"].as_u64();
    let cost_usd = reported["total_cost_usd"].as_f64();
    (input_tokens.is_some() || output_tokens.is_some() || cost_usd.is_some()).then_some(RunUsage {
        input_tokens,
        output_tokens,
        cost_usd,
    })
}

async fn latest(db: &HubDb, run: Option<&Run>) -> Result<Option<RunStep>, ApiError> {
    let Some(run) = run else { return Ok(None) };
    Ok(db
        .latest_journal_entry(run.id, None)
        .await
        .map_err(api_error)?
        .as_ref()
        .and_then(step))
}

/// `task` with its names, and its active run's machine and start.
async fn summary(
    db: &HubDb,
    names: &Names,
    task: Task,
    runs: Option<&[RunSummary]>,
    completed_at: Option<Timestamp>,
) -> Result<TaskSummary, ApiError> {
    let active = if let Some(runs) = runs {
        runs.iter()
            .rfind(|row| row.run.status == RunStatus::Active)
            .map(|row| row.run.clone())
    } else if matches!(task.status, TaskStatus::Running | TaskStatus::InReview) {
        db.active_lane_run(task.lane_id).await.map_err(api_error)?
    } else {
        None
    };
    let machine = active
        .as_ref()
        .map(|run| run.machine_id)
        .or(task.last_machine_id)
        .map(|id| names.machine(id));
    let latest_step = if let Some(runs) = runs {
        runs.iter()
            .rfind(|row| row.run.status == RunStatus::Active)
            .and_then(|row| row.steps.last().cloned())
    } else {
        latest(db, active.as_ref()).await?
    };
    Ok(TaskSummary {
        project: names
            .projects
            .get(&task.project_id)
            .cloned()
            .unwrap_or_default(),
        agent: names
            .agents
            .get(&task.agent_id)
            .cloned()
            .unwrap_or_else(|| task.agent_id.to_string()),
        machine,
        active_machine_id: active.as_ref().map(|run| run.machine_id),
        running_since: active.map(|run| run.started_at),
        latest_step,
        completed_at,
        task,
    })
}

async fn summaries(
    db: &HubDb,
    names: &Names,
    tasks: Vec<Task>,
) -> Result<Vec<TaskSummary>, ApiError> {
    let done: Vec<_> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Done)
        .map(|task| task.id)
        .collect();
    let completed = db.task_completion_times(&done).await.map_err(api_error)?;
    let mut summaries = Vec::with_capacity(tasks.len());
    for task in tasks {
        let completed_at = completed.get(&task.id).copied();
        summaries.push(summary(db, names, task, None, completed_at).await?);
    }
    Ok(summaries)
}

/// A sent message as the feed has it.
pub(super) async fn feed_item(
    db: &HubDb,
    message: OutboxMessage,
    projects: &HashMap<ProjectId, String>,
) -> Result<FeedItem, ApiError> {
    let (Some(seq), Some(delivered_at)) = (message.delivery_seq, message.sent_at) else {
        return Err(ApiError::Internal(format!(
            "sent message {} has no delivery position",
            message.id
        )));
    };
    let digest = if message.body["digest"].is_array() {
        db.digest_originals(message.id)
            .await
            .map_err(api_error)?
            .into_iter()
            .map(|original| DigestResult {
                id: original.id,
                project: original
                    .project_id
                    .and_then(|id| projects.get(&id).cloned()),
                text: original.body["text"]
                    .as_str()
                    .map_or_else(|| original.body.to_string(), str::to_owned),
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(FeedItem {
        digest,
        seq,
        project_name: message.project_id.and_then(|id| projects.get(&id).cloned()),
        delivered_at,
        message,
    })
}

/// Your message as the conversation and the task screen show it; a
/// space lead's ask, as its bare message, in a project's conversation.
fn you(event: Event, include_images: bool) -> ConversationEntry {
    let field = |name: &str| {
        event
            .payload
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    if event.source == crate::space_lead::SPACE_LEAD_SOURCE {
        return ConversationEntry::SpaceLead {
            event_id: event.id,
            text: field("message"),
            at: event.received_at,
        };
    }
    ConversationEntry::You {
        event_id: event.id,
        text: field("text"),
        images: if include_images {
            threadlane_core::attachment::images_in(&event.payload)
        } else {
            Vec::new()
        },
        at: event.received_at,
    }
}

/// Answers an [`OverviewRequest`].
pub(super) async fn handle(
    api: &ClientApi,
    request: OverviewRequest,
) -> Result<OverviewReply, ApiError> {
    let db = &api.core.inner.db;
    match request {
        OverviewRequest::RoutineHistory { timer, before } => Ok(OverviewReply::RoutineHistory {
            rows: db
                .routine_history(Some(timer), before)
                .await
                .map_err(api_error)?,
        }),
        OverviewRequest::Agenda { tomorrow_in } => {
            use threadlane_core::model::{TimerCategory, TimerStatus};
            use threadlane_core::store::TimerStore as _;
            use threadlane_core::work::Scheduled;
            let now = api.core.inner.clock.now();
            let (start, until) = if let Some(zone) = tomorrow_in {
                let (start, end) =
                    crate::recurrence::tomorrow(now, &zone).map_err(ApiError::Invalid)?;
                (Some(start), end)
            } else {
                (None, now + std::time::Duration::from_secs(7 * 86400))
            };
            let mut upcoming = Vec::new();
            for project in api.project_list(false).await? {
                for timer in db.project_timers(project.id).await.map_err(api_error)? {
                    if timer.status != TimerStatus::Active
                        || timer.category == TimerCategory::Dreaming
                    {
                        continue;
                    }
                    let mut due = timer.due_at;
                    if let Some(start) = start
                        && due < start
                    {
                        let Some(rule) = &timer.recurrence else {
                            continue;
                        };
                        due = crate::recurrence::next_occurrence(
                            rule,
                            due,
                            start - std::time::Duration::from_millis(1),
                        )
                        .map_err(ApiError::Invalid)?;
                    }
                    // Only the first 100 results can appear in the space agenda.
                    for _ in 0..100 {
                        if due >= until {
                            break;
                        }
                        upcoming.push(Scheduled {
                            timer: timer.id,
                            project: project.name.clone(),
                            instruction: timer.instruction.clone(),
                            due,
                        });
                        let Some(rule) = &timer.recurrence else {
                            break;
                        };
                        due = crate::recurrence::next_occurrence(rule, due, due.max(now))
                            .map_err(ApiError::Invalid)?;
                    }
                    if upcoming.len() > 200 {
                        upcoming.sort_by_key(|r| (r.due, r.timer));
                        upcoming.truncate(100);
                    }
                }
            }
            upcoming.sort_by_key(|r| (r.due, r.timer));
            upcoming.truncate(100);
            Ok(OverviewReply::Agenda {
                upcoming,
                recent: db.routine_history(None, None).await.map_err(api_error)?,
            })
        }
        OverviewRequest::SpaceUsage { days } => {
            if !(1..=366).contains(&days) {
                return Err(ApiError::Invalid("Choose 1 to 366 days.".into()));
            }
            let now = api.core.inner.clock.now();
            Ok(OverviewReply::SpaceUsage {
                groups: db
                    .space_usage(
                        now - std::time::Duration::from_secs(u64::from(days) * 86400),
                        now,
                    )
                    .await
                    .map_err(api_error)?,
            })
        }
        OverviewRequest::Outputs { project, search } => {
            let project = api.project_named(Some(&project)).await?;
            Ok(OverviewReply::Outputs {
                rows: db
                    .project_outputs(project.id, &search)
                    .await
                    .map_err(api_error)?,
            })
        }
        OverviewRequest::OutputImage {
            project,
            event,
            index,
        } => {
            let project = api.project_named(Some(&project)).await?;
            Ok(OverviewReply::OutputImage {
                image: db
                    .output_image(project.id, event, index)
                    .await
                    .map_err(api_error)?
                    .ok_or_else(|| ApiError::NotFound("Attachment".into()))?,
            })
        }
        OverviewRequest::ReviewRecipe => Ok(OverviewReply::ReviewRecipe {
            recipe: db.review_recipe().await.map_err(api_error)?,
        }),
        OverviewRequest::Tasks { project } => {
            let project = api.project_named(project.as_deref()).await?;
            let names = Names::of(db).await?;
            let tasks = db.tasks(project.id).await.map_err(api_error)?;
            Ok(OverviewReply::Tasks {
                tasks: summaries(db, &names, tasks).await?,
            })
        }
        OverviewRequest::Work => {
            let names = Names::of(db).await?;
            let projects = api.project_list(true).await?;
            let active: Vec<_> = projects
                .iter()
                .filter(|project| !project.archived)
                .map(|project| project.id)
                .collect();
            let tasks = db.tasks_for_projects(&active).await.map_err(api_error)?;
            let leads = db.active_project_leads().await.map_err(api_error)?;
            Ok(OverviewReply::Work {
                overview: WorkOverview {
                    projects,
                    tasks: summaries(db, &names, tasks).await?,
                    leads,
                },
            })
        }
        OverviewRequest::Running => {
            let names = Names::of(db).await?;
            let running = db.running_tasks().await.map_err(api_error)?;
            let mut tasks = summaries(db, &names, running).await?;
            tasks.retain(|summary| {
                summary.task.status == TaskStatus::Running || summary.running_since.is_some()
            });
            Ok(OverviewReply::Tasks { tasks })
        }
        OverviewRequest::TaskActivity { task, before } => {
            let task = db
                .task(task)
                .await
                .map_err(api_error)?
                .ok_or_else(|| ApiError::NotFound(format!("task {task}")))?;
            let size = threadlane_core::client::CONVERSATION_PAGE as usize;
            let mut journal = db
                .lane_journal_page(
                    task.lane_id,
                    before.map(|cursor| (cursor.run, cursor.seq)),
                    size as u32 + 1,
                )
                .await
                .map_err(api_error)?;
            let next = if journal.len() > size {
                journal.pop();
                journal.last().map(|entry| ActivityCursor {
                    run: entry.run_id,
                    seq: entry.seq,
                })
            } else {
                None
            };
            let entries = journal
                .iter()
                .filter_map(|entry| {
                    step(entry).map(|step| TaskActivityEntry {
                        position: ActivityCursor {
                            run: entry.run_id,
                            seq: entry.seq,
                        },
                        step,
                    })
                })
                .collect();
            Ok(OverviewReply::TaskActivity { entries, next })
        }
        OverviewRequest::Task { task } => task_detail(db, task, true).await,
        OverviewRequest::TaskOverview { task } => task_detail(db, task, false).await,
        OverviewRequest::Conversation {
            project,
            before,
            limit,
        } => {
            let project = api.project_named(project.as_deref()).await?;
            let include_images = limit <= threadlane_core::client::CONVERSATION_PAGE;
            let limit = limit.min(FEED_PAGE);
            let names = Names::of(db).await?;
            let mut entries: Vec<(Timestamp, ConversationEntry)> = db
                .user_messages(project.lead_lane_id, before, limit)
                .await
                .map_err(api_error)?
                .into_iter()
                .map(|event| (event.received_at, you(event, include_images)))
                .collect();
            if project.is_general {
                for event in db
                    .lane_events_from(
                        project.lead_lane_id,
                        EventKind::System,
                        crate::space_lead::PROJECT_LEAD_SOURCE,
                        before,
                        limit,
                    )
                    .await
                    .map_err(api_error)?
                {
                    let payload = &event.payload;
                    entries.push((
                        event.received_at,
                        ConversationEntry::ProjectReply {
                            event_id: event.id,
                            project: payload["project"].as_str().unwrap_or_default().to_owned(),
                            ask_event_id: serde_json::from_value(payload["ask_event_id"].clone())
                                .ok(),
                            text: event_text(&event),
                            at: event.received_at,
                        },
                    ));
                }
            }
            for message in db
                .sent_before(CLIENT_CHANNEL, Some(project.id), before, limit)
                .await
                .map_err(api_error)?
            {
                let item = feed_item(db, message, &names.projects).await?;
                entries.push((item.delivered_at, ConversationEntry::Delivered { item }));
            }
            for event in db
                .delivery_failures(project.id, before, limit)
                .await
                .map_err(api_error)?
            {
                entries.push((
                    event.received_at,
                    ConversationEntry::DeliveryFailure {
                        event_id: event.id,
                        text: event_text(&event),
                        at: event.received_at,
                    },
                ));
            }
            // The newest entries from all sources, then oldest first.
            entries.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
            entries.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            entries.reverse();
            Ok(OverviewReply::Conversation {
                entries: entries.into_iter().map(|(_, entry)| entry).collect(),
            })
        }
        OverviewRequest::Recent { limit } => {
            let names = Names::of(db).await?;
            let messages = db
                .sent_before(CLIENT_CHANNEL, None, None, limit.min(FEED_PAGE))
                .await
                .map_err(api_error)?;
            let mut items = Vec::with_capacity(messages.len());
            for message in messages.into_iter().rev() {
                items.push(feed_item(db, message, &names.projects).await?);
            }
            Ok(OverviewReply::Recent { items })
        }
        OverviewRequest::Machines => Ok(OverviewReply::Machines {
            machines: db.machines().await.map_err(api_error)?,
            accounts: db.accounts().await.map_err(api_error)?,
        }),
        OverviewRequest::Lead { project } => {
            let project = api.project_named(project.as_deref()).await?;
            let active = db
                .active_lane_run(project.lead_lane_id)
                .await
                .map_err(api_error)?;
            let stopped = db
                .lane_stopped(project.lead_lane_id)
                .await
                .map_err(api_error)?;
            Ok(OverviewReply::Lead {
                latest_step: latest(db, active.as_ref()).await?,
                running_since: active.map(|run| run.started_at),
                stopped,
            })
        }
        OverviewRequest::Recorded { project, limit } => {
            let project = api.project_named(project.as_deref()).await?;
            let (events, total) = db
                .skipped_events(
                    Some(project.id),
                    [SKIP_REASON, threadlane_store::ARCHIVED_EVENT_REASON],
                    limit.min(FEED_PAGE),
                )
                .await
                .map_err(api_error)?;
            Ok(OverviewReply::Recorded {
                events: events
                    .into_iter()
                    .map(|event| RecordedEvent {
                        text: event_text(&event),
                        id: event.id,
                        source: event.source,
                        kind: event.kind,
                        received_at: event.received_at,
                    })
                    .collect(),
                total,
            })
        }
        OverviewRequest::RecordedCount => {
            let (_, total) = db
                .skipped_events(
                    None,
                    [SKIP_REASON, threadlane_store::ARCHIVED_EVENT_REASON],
                    0,
                )
                .await
                .map_err(api_error)?;
            Ok(OverviewReply::RecordedCount { total })
        }
        OverviewRequest::LeadRuns { project } => {
            let project = api.project_named(project.as_deref()).await?;
            let runs = db
                .lane_runs(project.lead_lane_id)
                .await
                .map_err(api_error)?;
            let mut listed = Vec::with_capacity(runs.len());
            for run in runs {
                let sent_message = match run.outcome {
                    Some(outcome) => outcome == RunOutcome::Sent,
                    None => db.count_for_run(run.id).await.map_err(api_error)? > 0,
                };
                let (latest_step, usage) = if run.status == RunStatus::Active {
                    (latest(db, Some(&run)).await?, None)
                } else {
                    let result = db
                        .latest_journal_entry(run.id, Some("finished"))
                        .await
                        .map_err(api_error)?;
                    (None, result.as_ref().and_then(usage))
                };
                listed.push(LeadRun {
                    usage,
                    latest_step,
                    run,
                    sent_message,
                });
            }
            Ok(OverviewReply::LeadRuns { runs: listed })
        }
        OverviewRequest::HeldResults => Ok(OverviewReply::HeldResults {
            held: db
                .held_results(CLIENT_CHANNEL, api.core.inner.clock.now())
                .await
                .map_err(api_error)?,
        }),
        other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
    }
}

async fn task_detail(db: &HubDb, task: TaskId, full: bool) -> Result<OverviewReply, ApiError> {
    let task = db
        .task(task)
        .await
        .map_err(api_error)?
        .ok_or_else(|| ApiError::NotFound(format!("task {task}")))?;
    let names = Names::of(db).await?;
    let repositories = db
        .repositories(task.project_id)
        .await
        .map_err(api_error)?
        .into_iter()
        .filter(|repository| task.repositories.contains(&repository.id))
        .map(|repository| repository.name)
        .collect();
    let mut runs = Vec::new();
    for run in db.lane_runs(task.lane_id).await.map_err(api_error)? {
        let active = run.status == RunStatus::Active;
        let (steps, usage) = if full {
            let journal = db.journal(run.id).await.map_err(api_error)?;
            let usage = if active {
                None
            } else {
                journal
                    .iter()
                    .rev()
                    .find(|entry| entry.entry["type"] == "finished")
                    .and_then(usage)
            };
            (journal.iter().filter_map(step).collect(), usage)
        } else if active {
            (latest(db, Some(&run)).await?.into_iter().collect(), None)
        } else {
            let result = db
                .latest_journal_entry(run.id, Some("finished"))
                .await
                .map_err(api_error)?;
            (Vec::new(), result.as_ref().and_then(usage))
        };
        runs.push(RunSummary {
            machine: names.machine(run.machine_id),
            usage,
            steps,
            run,
        });
    }
    let completed_at = if task.status == TaskStatus::Done {
        db.task_completed_at(task.id).await.map_err(api_error)?
    } else {
        None
    };
    let summary = summary(db, &names, task.clone(), Some(&runs), completed_at).await?;
    let decisions = db.lane_decisions(task.lane_id).await.map_err(api_error)?;
    let related = db
        .tasks(task.project_id)
        .await
        .map_err(api_error)?
        .into_iter()
        .filter(|other| task.depends_on.contains(&other.id) || other.depends_on.contains(&task.id))
        .collect();
    let related = summaries(db, &names, related).await?;
    let mut messages: Vec<ConversationEntry> = db
        .user_messages(task.lane_id, None, FEED_PAGE)
        .await
        .map_err(api_error)?
        .into_iter()
        .map(|event| you(event, false))
        .collect();
    messages.reverse();
    let proposed_by = crate::proposal::accepted_author(db, &task)
        .await
        .map_err(api_error)?;
    let stopped = db.lane_stopped(task.lane_id).await.map_err(api_error)?;
    Ok(OverviewReply::Task {
        task: Box::new(TaskDetail {
            branch: task_branch(task.id),
            repositories,
            runs,
            reviews: db.task_reviews(task.id).await.map_err(api_error)?,
            decisions,
            related,
            messages,
            summary,
            stopped,
            proposed_by,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_warning_entry_shows_its_message_as_the_step() {
        let entry = JournalEntry {
            run_id: 1.into(),
            seq: 1,
            at: Timestamp::from_millis(0),
            entry: serde_json::json!({"type": "warning", "message": "codex skill \"gpui\" was skipped for this run: it is not installed on this runner."}),
        };
        let step = step(&entry).expect("a step");
        assert_eq!(step.kind, "warning");
        assert_eq!(
            step.description,
            "codex skill \"gpui\" was skipped for this run: it is not installed on this runner."
        );
    }
}
