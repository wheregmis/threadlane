//! Tasks as the project board and the task screen show them (Plan.md §5.2,
//! §11.1).

use serde_json::Value as Json;
use threadlane_core::client::{ConversationEntry, TaskDetail};
use threadlane_core::ids::{DecisionId, TaskId};
use threadlane_core::model::{DecisionStatus, RunOutcome, RunStatus, TaskOrigin, TaskStatus};
use threadlane_core::time::Timestamp;

use crate::{AppState, When, duration};

/// The start of a `waiting_reason` that says no model account is free
/// (`threadlane-store`'s `BLOCKED_REASON`, Plan.md §5.3).
const BLOCKED: &str = "blocked";

/// One column of the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// `Queued`, `Running`, `Waiting on you`, ...
    pub title: &'static str,
    /// Its tasks, oldest first.
    pub cards: Vec<BoardCard>,
}

/// One task on the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardCard {
    /// Its id.
    pub id: TaskId,
    /// What to do.
    pub title: String,
    /// `Engineer · macbook · 12 min`: its agent, and its machine or why it
    /// is not running.
    pub meta: String,
    /// The decision waiting on you in it, which the card offers to answer.
    pub decision: Option<DecisionId>,
    /// That decision's options, which answer it from the card.
    pub options: Vec<String>,
}

/// The columns in board order, and whether each is shown when empty.
const COLUMNS: [(&str, bool); 9] = [
    ("Proposed", false),
    ("Queued", true),
    ("Blocked", false),
    ("Running", true),
    ("Sleeping", false),
    ("Waiting on you", true),
    ("Done", true),
    ("Failed", false),
    ("Cancelled", false),
];

fn column_of(status: TaskStatus, waiting_reason: Option<&str>) -> &'static str {
    match status {
        TaskStatus::Proposed => "Proposed",
        TaskStatus::Queued if waiting_reason.is_some_and(|why| why.starts_with(BLOCKED)) => {
            "Blocked"
        }
        TaskStatus::Queued => "Queued",
        TaskStatus::Running => "Running",
        TaskStatus::Sleeping => "Sleeping",
        TaskStatus::WaitingOnYou => "Waiting on you",
        TaskStatus::Done => "Done",
        TaskStatus::Failed => "Failed",
        TaskStatus::Cancelled => "Cancelled",
    }
}

/// How the task screen names a status.
#[must_use]
pub fn status_label(status: TaskStatus, waiting_reason: Option<&str>) -> &'static str {
    column_of(status, waiting_reason)
}

impl AppState {
    /// The board of `project` in `space`: its tasks by status, in the
    /// columns Plan.md §5.2 names. Queued, Running, Waiting on you and Done
    /// are always there; the others only when a task is in them.
    #[must_use]
    pub fn board(&self, space: &str, project: &str, when: &When) -> Vec<Column> {
        let tasks = self
            .project(space, project)
            .map(|view| view.tasks.as_slice())
            .unwrap_or_default();
        COLUMNS
            .iter()
            .filter_map(|&(title, always)| {
                let cards: Vec<BoardCard> = tasks
                    .iter()
                    .filter(|summary| {
                        column_of(summary.task.status, summary.task.waiting_reason.as_deref())
                            == title
                    })
                    .map(|summary| {
                        let task = &summary.task;
                        let open = self.open_for(space, task.id).map(|q| &q.decision);
                        let decision = open.map(|decision| decision.id);
                        let options = open
                            .map(|decision| {
                                decision.options.iter().map(|o| o.label.clone()).collect()
                            })
                            .unwrap_or_default();
                        let detail = match (task.status, decision) {
                            (_, Some(id)) => format!("{id} open"),
                            (TaskStatus::Running, None) => match summary.running_since {
                                Some(since) => format!(
                                    "{} · {}",
                                    summary.machine.as_deref().unwrap_or("a machine"),
                                    when.since(since)
                                ),
                                None => summary.machine.clone().unwrap_or_default(),
                            },
                            (TaskStatus::Queued, None) => task
                                .waiting_reason
                                .clone()
                                .unwrap_or_else(|| "waiting for a machine".to_owned()),
                            (TaskStatus::Sleeping, None) => {
                                "waiting for a timer or an event".to_owned()
                            }
                            _ => task
                                .waiting_reason
                                .clone()
                                .or_else(|| summary.machine.clone())
                                .unwrap_or_default(),
                        };
                        BoardCard {
                            id: task.id,
                            title: task.title.clone(),
                            meta: if detail.is_empty() {
                                summary.agent.clone()
                            } else {
                                format!("{} · {detail}", summary.agent)
                            },
                            decision,
                            options,
                        }
                    })
                    .collect();
                (always || !cards.is_empty()).then_some(Column { title, cards })
            })
            .collect()
    }
}

/// The task screen's header chips: agent, backend, machine, repositories,
/// branch, origin, status (Plan.md §11.1 "Task").
#[must_use]
pub fn chips(detail: &TaskDetail) -> Vec<String> {
    let summary = &detail.summary;
    let task = &summary.task;
    let backend = detail
        .runs
        .iter()
        .rev()
        .find_map(|run| run.run.backend)
        .map_or_else(|| "no backend yet".to_owned(), |kind| kind.to_string());
    let mut chips = vec![
        summary.agent.clone(),
        backend,
        summary
            .machine
            .clone()
            .unwrap_or_else(|| "no machine yet".to_owned()),
    ];
    chips.extend(detail.repositories.iter().cloned());
    chips.push(detail.branch.clone());
    chips.push(origin(task.origin).to_owned());
    chips.push(status_label(task.status, task.waiting_reason.as_deref()).to_owned());
    chips
}

fn origin(origin: TaskOrigin) -> &'static str {
    match origin {
        TaskOrigin::User => "Requested by you",
        TaskOrigin::UserRoutine => "From your routine",
        TaskOrigin::Event => "From an outside event",
        TaskOrigin::Agent => "Started by an agent",
    }
}

/// One entry of a task's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// When, as the app shows it.
    pub time: String,
    /// What happened.
    pub text: String,
}

/// A task's timeline, oldest first, from what its hub has: its creation,
/// each run with its machine, backend, duration and outcome, each decision
/// asked and answered, and its result.
#[must_use]
pub fn timeline(detail: &TaskDetail, when: &When) -> Vec<Moment> {
    let summary = &detail.summary;
    let task = &summary.task;
    let mut moments: Vec<(Timestamp, String)> = vec![(
        task.created_at,
        format!("Created: {}. {}.", task.title, origin(task.origin)),
    )];
    for run in &detail.runs {
        let run_summary = &run.run;
        let backend = run_summary
            .backend
            .map_or_else(String::new, |kind| format!(" with {kind}"));
        let on = format!("{} on {}{backend}", summary.agent, run.machine);
        let text = match (run_summary.status, run_summary.ended_at) {
            (RunStatus::Active, _) | (_, None) => {
                format!("{on}: working for {}", when.since(run_summary.started_at))
            }
            (status, Some(ended)) => {
                let took = duration(ended.saturating_since(run_summary.started_at));
                let outcome = match (status, run_summary.outcome) {
                    (RunStatus::Committed, Some(RunOutcome::Sent)) => "reported back".to_owned(),
                    (RunStatus::Committed, Some(RunOutcome::Deferred)) => {
                        "stopped to ask you".to_owned()
                    }
                    (RunStatus::Committed, _) => "ended quietly".to_owned(),
                    (RunStatus::Failed, _) => format!(
                        "failed: {}",
                        run_summary.error.as_deref().unwrap_or("no reason given")
                    ),
                    _ => "was abandoned when its machine was lost".to_owned(),
                };
                format!("{on}: worked for {took}, {outcome}")
            }
        };
        moments.push((run_summary.started_at, text));
    }
    for decision in &detail.decisions {
        moments.push((
            decision.created_at,
            format!("Asked {}: {}", decision.id, decision.question),
        ));
        let answered_at = decision
            .answer
            .as_ref()
            .and_then(|answer| answer.get("answered_at"))
            .and_then(Json::as_i64)
            .map(Timestamp::from_millis);
        match (decision.status, answered_at) {
            (DecisionStatus::Answered, Some(at)) => {
                let label = decision
                    .chosen()
                    .map_or("an option", |option| option.label.as_str());
                let note = decision
                    .answer
                    .as_ref()
                    .and_then(|answer| answer.get("note"))
                    .and_then(Json::as_str)
                    .map_or_else(String::new, |note| format!(" with the note \"{note}\""));
                moments.push((at, format!("You answered {}: {label}{note}", decision.id)));
            }
            (DecisionStatus::Expired, _) => moments.push((
                decision.expires_at.unwrap_or(decision.created_at),
                format!("{} expired unanswered", decision.id),
            )),
            _ => {}
        }
    }
    for entry in &detail.messages {
        if let ConversationEntry::You { text, at, .. } = entry {
            moments.push((*at, format!("You wrote: {text}")));
        }
    }
    if task.status.is_terminal() {
        let ended = detail
            .runs
            .iter()
            .filter_map(|run| run.run.ended_at)
            .max()
            .unwrap_or(task.created_at);
        let result = task
            .result
            .as_ref()
            .and_then(|result| result.get("summary"))
            .and_then(Json::as_str)
            .map_or_else(String::new, |summary| format!(": {summary}"));
        let status = status_label(task.status, None);
        moments.push((ended, format!("{status}{result}")));
    }
    moments.sort_by_key(|(at, _)| *at);
    moments
        .into_iter()
        .map(|(at, text)| Moment {
            time: when.moment(at),
            text,
        })
        .collect()
}
