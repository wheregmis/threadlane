//! The Overview capability (Plan.md §11.1, §11.2): the tasks, conversation
//! history, and machines the app's screens show. Read-only.

use std::collections::HashMap;

use threadlane_core::client::{
    ApiError, ConversationEntry, FeedItem, OverviewReply, OverviewRequest, RunSummary, TaskDetail,
    TaskSummary,
};
use threadlane_core::ids::{AgentId, MachineId, ProjectId};
use threadlane_core::model::{OutboxMessage, RunStatus, Task, TaskStatus};
use threadlane_core::store::{
    DecisionStore, InboxStore, MachineStore, OrgStore, OutboxStore, SessionStore, TaskStore,
};
use threadlane_core::time::Timestamp;
use threadlane_core::workspace::task_branch;
use threadlane_store::HubDb;

use super::{ClientApi, FEED_PAGE};
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
            agents: db
                .agents()
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

/// `task` with its names, and its active run's machine and start.
async fn summary(db: &HubDb, names: &Names, task: Task) -> Result<TaskSummary, ApiError> {
    let active = if task.status == TaskStatus::Running {
        db.lane_runs(task.lane_id)
            .await
            .map_err(api_error)?
            .into_iter()
            .rfind(|run| run.status == RunStatus::Active)
    } else {
        None
    };
    let machine = active
        .as_ref()
        .map(|run| run.machine_id)
        .or(task.last_machine_id)
        .map(|id| names.machine(id));
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
        running_since: active.map(|run| run.started_at),
        task,
    })
}

async fn summaries(
    db: &HubDb,
    names: &Names,
    tasks: Vec<Task>,
) -> Result<Vec<TaskSummary>, ApiError> {
    let mut summaries = Vec::with_capacity(tasks.len());
    for task in tasks {
        summaries.push(summary(db, names, task).await?);
    }
    Ok(summaries)
}

/// A sent message as the feed has it.
fn feed_item(
    message: OutboxMessage,
    projects: &HashMap<ProjectId, String>,
) -> Result<FeedItem, ApiError> {
    let (Some(seq), Some(delivered_at)) = (message.delivery_seq, message.sent_at) else {
        return Err(ApiError::Internal(format!(
            "sent message {} has no delivery position",
            message.id
        )));
    };
    Ok(FeedItem {
        seq,
        project_name: message.project_id.and_then(|id| projects.get(&id).cloned()),
        delivered_at,
        message,
    })
}

/// Answers an [`OverviewRequest`].
pub(super) async fn handle(
    api: &ClientApi,
    request: OverviewRequest,
) -> Result<OverviewReply, ApiError> {
    let db = &api.core.inner.db;
    match request {
        OverviewRequest::Tasks { project } => {
            let project = api.project_named(project.as_deref()).await?;
            let names = Names::of(db).await?;
            let tasks = db.tasks(project.id).await.map_err(api_error)?;
            Ok(OverviewReply::Tasks {
                tasks: summaries(db, &names, tasks).await?,
            })
        }
        OverviewRequest::Running => {
            let names = Names::of(db).await?;
            let mut running = Vec::new();
            for project in names.projects.keys() {
                running.extend(
                    db.tasks(*project)
                        .await
                        .map_err(api_error)?
                        .into_iter()
                        .filter(|task| task.status == TaskStatus::Running),
                );
            }
            running.sort_by_key(|task| task.id);
            Ok(OverviewReply::Tasks {
                tasks: summaries(db, &names, running).await?,
            })
        }
        OverviewRequest::Task { task } => {
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
            let runs = db
                .lane_runs(task.lane_id)
                .await
                .map_err(api_error)?
                .into_iter()
                .map(|run| RunSummary {
                    machine: names.machine(run.machine_id),
                    run,
                })
                .collect();
            let decisions = db.lane_decisions(task.lane_id).await.map_err(api_error)?;
            let related = db
                .tasks(task.project_id)
                .await
                .map_err(api_error)?
                .into_iter()
                .filter(|other| {
                    task.depends_on.contains(&other.id) || other.depends_on.contains(&task.id)
                })
                .collect();
            let related = summaries(db, &names, related).await?;
            Ok(OverviewReply::Task {
                task: Box::new(TaskDetail {
                    branch: task_branch(task.id),
                    repositories,
                    runs,
                    decisions,
                    related,
                    summary: summary(db, &names, task).await?,
                }),
            })
        }
        OverviewRequest::Conversation {
            project,
            before,
            limit,
        } => {
            let project = api.project_named(project.as_deref()).await?;
            let limit = limit.min(FEED_PAGE);
            let names = Names::of(db).await?;
            let mut entries: Vec<(Timestamp, ConversationEntry)> = db
                .user_messages(project.lead_lane_id, before, limit)
                .await
                .map_err(api_error)?
                .into_iter()
                .map(|event| {
                    let text = event
                        .payload
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let entry = ConversationEntry::You {
                        event_id: event.id,
                        text,
                        at: event.received_at,
                    };
                    (event.received_at, entry)
                })
                .collect();
            for message in db
                .sent_before(CLIENT_CHANNEL, Some(project.id), before, limit)
                .await
                .map_err(api_error)?
            {
                let item = feed_item(message, &names.projects)?;
                entries.push((item.delivered_at, ConversationEntry::Delivered { item }));
            }
            // The newest `limit` of both, then oldest first.
            entries.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
            entries.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            entries.reverse();
            Ok(OverviewReply::Conversation {
                entries: entries.into_iter().map(|(_, entry)| entry).collect(),
            })
        }
        OverviewRequest::Recent { limit } => {
            let names = Names::of(db).await?;
            let mut items = db
                .sent_before(CLIENT_CHANNEL, None, None, limit.min(FEED_PAGE))
                .await
                .map_err(api_error)?
                .into_iter()
                .map(|message| feed_item(message, &names.projects))
                .collect::<Result<Vec<_>, _>>()?;
            items.reverse();
            Ok(OverviewReply::Recent { items })
        }
        OverviewRequest::Machines => Ok(OverviewReply::Machines {
            machines: db.machines().await.map_err(api_error)?,
            accounts: db.accounts().await.map_err(api_error)?,
        }),
        other => Err(ApiError::Invalid(format!("unsupported request {other:?}"))),
    }
}
