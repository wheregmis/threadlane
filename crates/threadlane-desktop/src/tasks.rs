//! Tasks as the project board and the task screen show them (Plan.md §5.2,
//! §11.1).

use serde_json::Value as Json;
use threadlane_core::client::{ConversationEntry, TaskDetail, TaskSummary};
use threadlane_core::ids::{DecisionId, TaskId};
use threadlane_core::link::MERGE_CHECKS;
use threadlane_core::model::{
    DecisionKind, DecisionStatus, RunOutcome, RunStatus, TaskOrigin, TaskStatus,
};
use threadlane_core::time::Timestamp;
use threadlane_core::workspace::MergeCheck;

use crate::{AppState, Destination, When, count_form, duration, proposed_assignee};

/// The start of a `waiting_reason` that says no model account is free
/// (`threadlane-store`'s `BLOCKED_REASON`, Plan.md §5.3).
const BLOCKED: &str = "blocked";

/// One column of the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// `Queued`, `Running`, `Waiting on you`, ...
    pub title: &'static str,
    /// Its visible tasks, oldest first except for Done.
    pub cards: Vec<BoardCard>,
    /// The total shown in the column heading, including any collapsed tasks.
    pub count: usize,
    /// The count label for hidden Done cards, if any.
    pub more: Option<String>,
    /// Open proposals that have yet to create a task.
    pub proposals: Vec<ProposalCard>,
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
    /// How long the task has been running or queued.
    pub elapsed: Option<String>,
    /// Whether this task still accepts cancellation.
    pub can_cancel: bool,
    /// The decision waiting on you in it, which the card offers to answer.
    pub decision: Option<DecisionId>,
    /// That decision's options, which answer it from the card.
    pub options: Vec<String>,
    /// Whether its branch merges into its base, from [`merge_state`].
    pub merge: Option<String>,
}

/// An open proposal on the board with the two in-place answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalCard {
    /// The decision to answer.
    pub id: DecisionId,
    /// The proposed task's title.
    pub title: String,
    /// The proposing agent and the assignee.
    pub meta: String,
    /// The option that starts the task.
    pub start: Option<String>,
    /// The option that declines this proposal once.
    pub decline: Option<String>,
}

/// How many of the most recently finished tasks the board shows.
const DONE_SHOWN: usize = 5;

/// The columns in board order, and whether each is shown when empty.
const COLUMNS: [(&str, bool); 10] = [
    ("Proposed", false),
    ("Queued", true),
    ("Blocked", false),
    ("Running", true),
    ("In review", false),
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
        TaskStatus::InReview => "In review",
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
                let mut matching: Vec<_> = tasks
                    .iter()
                    .filter(|summary| {
                        column_of(summary.task.status, summary.task.waiting_reason.as_deref())
                            == title
                    })
                    .collect();
                let total = matching.len();
                if title == "Done" {
                    matching.sort_by_key(|summary| {
                        std::cmp::Reverse((
                            summary.completed_at.unwrap_or(summary.task.created_at),
                            summary.task.id,
                        ))
                    });
                    matching.truncate(DONE_SHOWN);
                }
                let hidden = total - matching.len();
                let cards: Vec<BoardCard> = matching
                    .into_iter()
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
                            (TaskStatus::Queued, _) => {
                                self.waiting_task(space, summary).unwrap_or_default()
                            }
                            (_, Some(id)) => format!("{id} open"),
                            (TaskStatus::Running | TaskStatus::InReview, None) => {
                                summary.machine.clone().unwrap_or_default()
                            }
                            // Claim a follow-up only when one is listed.
                            (TaskStatus::Sleeping, None) => task
                                .waiting_reason
                                .clone()
                                .or_else(|| {
                                    self.follow_up_due(space, project, task.lane_id).map(|due| {
                                        format!(
                                            "sleeping until its follow-up at {}",
                                            when.moment(due)
                                        )
                                    })
                                })
                                .unwrap_or_else(|| "sleeping".to_owned()),
                            (TaskStatus::Done, None) => summary
                                .completed_at
                                .map(|at| when.moment(at))
                                .or_else(|| summary.machine.clone())
                                .unwrap_or_default(),
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
                            elapsed: work_elapsed(summary, when),
                            can_cancel: !task.status.is_terminal(),
                            decision,
                            options,
                            merge: merge_state(task.result.as_ref()),
                        }
                    })
                    .collect();
                let proposals = if title == "Proposed" {
                    self.proposals(space, project)
                } else {
                    Vec::new()
                };
                let count = total + proposals.len();
                (always || count != 0).then_some(Column {
                    title,
                    cards,
                    count,
                    more: (hidden > 0).then(|| format!("{hidden} more…")),
                    proposals,
                })
            })
            .collect()
    }

    /// The same waiting reason on the board and task page.
    #[must_use]
    pub fn waiting_task(&self, space: &str, summary: &TaskSummary) -> Option<String> {
        if summary.task.status == TaskStatus::Queued {
            return Some(
                self.queued_reason(space, &summary.project, summary)
                    .unwrap_or_else(|| "waiting for a machine".to_owned()),
            );
        }
        summary.task.waiting_reason.clone()
    }

    /// Running and queued work in this space, with active work first.
    #[must_use]
    pub fn in_progress(&self, space: &str, when: &When) -> InProgress {
        let Some(space) = self.space(space) else {
            return InProgress::default();
        };
        let mut rows: Vec<_> = space
            .running
            .iter()
            .map(|task| WorkRow::task_in(&space.name, task, when))
            .chain(space.leads.iter().map(|(project, since)| {
                let elapsed = sidebar_time(when.since(*since));
                WorkRow {
                    destination: Destination::Lead(space.name.clone(), project.clone()),
                    label: format!("{} · lead", clip(project, title_chars(&elapsed) - 7)),
                    title: format!("{project} · lead"),
                    elapsed,
                    status: "Running",
                }
            }))
            .collect();
        rows.extend(
            space
                .tasks
                .iter()
                .filter(|task| task.task.status == TaskStatus::Queued)
                .map(|task| WorkRow::task_in(&space.name, task, when)),
        );
        let more = rows.len().saturating_sub(5);
        rows.truncate(5);
        InProgress { rows, more }
    }

    /// Whether ticking durations are shown by any work view.
    #[must_use]
    pub fn has_ticking_work(&self) -> bool {
        self.spaces.iter().any(|space| {
            !space.running.is_empty()
                || !space.leads.is_empty()
                || space
                    .tasks
                    .iter()
                    .any(|task| task.task.status == TaskStatus::Queued)
        }) || self.projects.values().any(|view| {
            view.tasks
                .iter()
                .any(|row| matches!(row.task.status, TaskStatus::Running | TaskStatus::Queued))
        }) || self
            .tasks
            .values()
            .any(|detail| detail.summary.task.status == TaskStatus::Running)
    }

    /// Whether this task's journal is expanded.
    #[must_use]
    pub fn activity_open(&self, space: &str, task: TaskId) -> bool {
        self.task_activity.contains(&(space.to_owned(), task))
    }

    /// Toggles the task's journal.
    pub fn toggle_activity(&mut self, space: &str, task: TaskId) {
        let key = (space.to_owned(), task);
        if !self.task_activity.remove(&key) {
            self.task_activity.insert(key);
        }
    }

    fn proposals(&self, space: &str, project: &str) -> Vec<ProposalCard> {
        self.queue()
            .iter()
            .filter(|q| {
                q.space == space
                    && q.decision.kind == DecisionKind::Proposal
                    && !q.decision.is_instruction_proposal()
                    && self.project_name(space, q.decision.project_id) == Some(project)
            })
            .map(|q| {
                let decision = &q.decision;
                let proposer =
                    decision
                        .proposed_by
                        .as_deref()
                        .unwrap_or(if decision.task_id.is_some() {
                            "The task’s agent"
                        } else {
                            "Lead"
                        });
                ProposalCard {
                    id: decision.id,
                    title: decision.arguments["title"]
                        .as_str()
                        .unwrap_or(&decision.question)
                        .to_owned(),
                    meta: format!("{proposer} proposes · {}", proposed_assignee(decision)),
                    start: decision
                        .options
                        .iter()
                        .find(|option| option.approves())
                        .map(|option| option.label.clone()),
                    decline: decision
                        .options
                        .iter()
                        .find(|option| {
                            option.effect["approve"] == Json::Bool(false)
                                && option.effect["never_again"] != Json::Bool(true)
                        })
                        .map(|option| option.label.clone()),
                }
            })
            .collect()
    }
}

/// Characters of a title that fit on one sidebar row beside a short time.
const SIDEBAR_CHARS: usize = 18;

/// Characters of title and time together that fit on one sidebar row. The
/// time keeps its full width, so a longer time leaves less for the title.
const ROW_CHARS: usize = 24;

/// Characters of a title, its "…" included, that fit beside `elapsed`.
fn title_chars(elapsed: &str) -> usize {
    SIDEBAR_CHARS
        .min(ROW_CHARS.saturating_sub(elapsed.chars().count()))
        .max(8)
}

/// `elapsed` in the sidebar's narrow form: "< 1 min" for "less than a minute".
fn sidebar_time(elapsed: String) -> String {
    if elapsed == crate::clock::UNDER_A_MINUTE {
        "< 1 min".to_owned()
    } else {
        elapsed
    }
}

/// `text` cut to `chars` characters, ending in "…" when cut.
fn clip(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", text[..at].trim_end()),
        None => text.to_owned(),
    }
}

/// A compact row that opens a task or a lead conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRow {
    /// The page to open.
    pub destination: Destination,
    /// The task or lead's display title.
    pub title: String,
    /// The title cut to fit the sidebar's one line.
    pub label: String,
    /// Time elapsed from the start or enqueue.
    pub elapsed: String,
    /// The display status.
    pub status: &'static str,
}

impl WorkRow {
    fn task_in(space: &str, summary: &TaskSummary, when: &When) -> Self {
        let elapsed = sidebar_time(work_elapsed(summary, when).unwrap_or_default());
        Self {
            destination: Destination::Task(space.to_owned(), summary.task.id),
            label: clip(&summary.task.title, title_chars(&elapsed)),
            title: summary.task.title.clone(),
            elapsed,
            status: status_label(summary.task.status, summary.task.waiting_reason.as_deref()),
        }
    }
}

/// The sidebar's visible work and count beyond its cap.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InProgress {
    /// Up to five rows.
    pub rows: Vec<WorkRow>,
    /// Number of remaining rows.
    pub more: usize,
}

/// The elapsed time shown for running and queued work.
#[must_use]
pub fn work_elapsed(summary: &TaskSummary, when: &When) -> Option<String> {
    match summary.task.status {
        TaskStatus::Queued => Some(when.since(summary.task.created_at)),
        TaskStatus::Running | TaskStatus::InReview => running_elapsed(summary, when),
        _ => None,
    }
}

/// Display data for the task header.
#[derive(Debug, PartialEq, Eq)]
pub struct TaskHeader {
    /// Badge text.
    pub status: &'static str,
    /// Single line with the most recent run's timing and assignment.
    pub meta: String,
    /// The task's origin and repositories.
    pub context: Vec<String>,
    /// The current progress or quiet warning.
    pub latest: Option<LatestStep>,
}

/// A live line with its warning state.
#[derive(Debug, PartialEq, Eq)]
pub struct LatestStep {
    /// Copy for the live line.
    pub text: String,
    /// Whether the run has had no step for ten minutes or more.
    pub quiet: bool,
}

/// Builds the task header against the display clock.
#[must_use]
pub fn task_header(detail: &TaskDetail, when: &When) -> TaskHeader {
    let summary = &detail.summary;
    let running = summary.task.status == TaskStatus::Running
        || (summary.task.status == TaskStatus::InReview && summary.running_since.is_some());
    let run = if running {
        detail
            .runs
            .iter()
            .rfind(|row| row.run.status == RunStatus::Active)
    } else {
        detail.runs.last()
    };
    let status = match (detail.stopped, running) {
        (true, true) => "Stopping",
        (true, false) => "Stopped",
        _ => status_label(summary.task.status, summary.task.waiting_reason.as_deref()),
    };
    let machine = run
        .map(|row| row.machine.as_str())
        .or(summary.machine.as_deref());
    let backend = run.and_then(|row| row.run.backend);
    let mut meta = format!(
        "{} · {} · {}",
        summary.agent,
        machine.unwrap_or("no machine yet"),
        backend.map_or_else(|| "no backend yet".to_owned(), |kind| kind.to_string())
    );
    let start = if running {
        summary
            .running_since
            .or_else(|| run.map(|row| row.run.started_at))
    } else {
        run.map(|row| row.run.started_at)
    };
    if let Some(start) = start {
        meta.push_str(&format!(" · started {}", when.time(start)));
        if running {
            meta.push_str(&format!(" · running for {}", when.since(start)));
        } else if let Some(end) = run.and_then(|row| row.run.ended_at) {
            meta.push_str(&format!(
                " · took {}",
                duration(end.saturating_since(start))
            ));
        }
    }
    let latest = running.then(|| {
        let step = summary.latest_step.as_ref();
        let since = step.map(|step| step.at).or(start);
        let quiet = since.is_some_and(|at| when.now.saturating_since(at).as_secs() >= 600);
        let text = match (quiet, step, since) {
            (true, _, Some(at)) => format!("Quiet for {}", when.since(at)),
            (_, Some(step), _) => {
                format!("Latest: {} · {} ago", step.description, when.since(step.at))
            }
            _ => "Waiting for the first step.".to_owned(),
        };
        LatestStep { text, quiet }
    });
    let mut context = detail.repositories.clone();
    context.push(origin(detail));
    TaskHeader {
        status,
        meta,
        context,
        latest,
    }
}

/// Rows of the task's journal, grouped newest run first.
#[must_use]
pub fn activity_runs(detail: &TaskDetail, when: &When) -> Vec<(String, Vec<String>)> {
    detail
        .runs
        .iter()
        .rev()
        .filter(|row| !row.steps.is_empty())
        .map(|row| {
            let title = format!("Run {} · {}", row.run.id, when.moment(row.run.started_at));
            let steps = row
                .steps
                .iter()
                .map(|step| format!("{} · {}", when.moment(step.at), step.description))
                .collect();
            (title, steps)
        })
        .collect()
}

/// How long the task's active run has been working, if the hub reported a start.
#[must_use]
pub fn running_elapsed(summary: &TaskSummary, when: &When) -> Option<String> {
    if !matches!(
        summary.task.status,
        TaskStatus::Running | TaskStatus::InReview
    ) {
        return None;
    }
    summary.running_since.map(|since| when.since(since))
}

/// The task screen's header chips: agent, backend, machine, repositories,
/// branch, origin, status (Plan.md §11.1 "Task").
#[must_use]
pub fn chips(detail: &TaskDetail, when: &When) -> Vec<String> {
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
    chips.push(origin(detail));
    let status = if detail.stopped {
        if task.status == TaskStatus::Running {
            "Stopping"
        } else {
            "Stopped"
        }
    } else {
        status_label(task.status, task.waiting_reason.as_deref())
    };
    chips.push(running_elapsed(summary, when).map_or_else(
        || status.to_owned(),
        |elapsed| format!("{status} · {elapsed}"),
    ));
    chips
}

fn origin(detail: &TaskDetail) -> String {
    match detail.summary.task.origin {
        TaskOrigin::User => "Requested by you",
        TaskOrigin::UserRoutine => "From your routine",
        TaskOrigin::Event => "From an outside event",
        TaskOrigin::Agent => "Started by an agent",
        TaskOrigin::Accepted => {
            return format!(
                "Proposed by {}, accepted by you",
                detail.proposed_by.as_deref().unwrap_or("an agent")
            );
        }
    }
    .to_owned()
}

/// The tools whose approved call pushes the task's branch.
const PUBLISHING: [&str; 2] = ["publish_branch", "open_pull_request"];

/// The task screen's "Usage" card: how many runs, how long they took, and
/// what the hub says of tokens and cost.
#[must_use]
pub fn usage(detail: &TaskDetail, when: &When) -> Vec<String> {
    if detail.runs.is_empty() {
        return vec!["No runs".to_owned()];
    }
    let took = detail
        .runs
        .iter()
        .map(|run| {
            let ended = run.run.ended_at.unwrap_or(when.now);
            ended.saturating_since(run.run.started_at)
        })
        .sum();
    let run_count = detail.runs.len();
    let runs = count_form(run_count, "run", "runs");
    let reported: Vec<_> = detail
        .runs
        .iter()
        .filter_map(|row| row.usage.as_ref())
        .collect();
    let mut fields = Vec::new();
    for (direction, counts) in [
        (
            "input",
            reported
                .iter()
                .filter_map(|u| u.input_tokens)
                .collect::<Vec<_>>(),
        ),
        (
            "output",
            reported
                .iter()
                .filter_map(|u| u.output_tokens)
                .collect::<Vec<_>>(),
        ),
    ] {
        if !counts.is_empty() {
            let total: u128 = counts.into_iter().map(u128::from).sum();
            let tokens = count_form(total, "token", "tokens");
            fields.push(format!("{total} {direction} {tokens}"));
        }
    }
    let mut costs = reported.iter().filter_map(|u| u.cost_usd).peekable();
    if costs.peek().is_some() {
        fields.push(format!("${:.4}", costs.sum::<f64>()));
    }
    let totals = if fields.is_empty() {
        "The backend has not reported tokens or cost.".to_owned()
    } else {
        format!("Reported usage · {}", fields.join(" · "))
    };
    vec![format!("{run_count} {runs} · {}", duration(took)), totals]
}

/// The task screen's "Changes" card: its branch and whether it is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    /// The branch it works on.
    pub branch: String,
    /// `Pushed`, `Not pushed`, `Waiting on you to push (D-0012)`, ...
    pub state: String,
    /// The pull request opened for it, if any.
    pub pull_request: Option<String>,
    /// Whether its branch merges into its base, from [`merge_state`].
    pub merge: Option<String>,
}

/// Where a task's branches stood against their bases when it last finished
/// (docs/merge-help.md decision 4): `Merges cleanly into main at 1a2b3c4`
/// or `Conflicts with main: src/a.rs, src/b.rs`, one per repository, named
/// when there are several. `None` before a check was recorded.
#[must_use]
pub fn merge_state(result: Option<&Json>) -> Option<String> {
    let checks: Vec<MergeCheck> =
        serde_json::from_value(result?.get(MERGE_CHECKS)?.clone()).ok()?;
    let several = checks.len() > 1;
    let lines: Vec<String> = checks
        .iter()
        .map(|check| {
            let mut line = if check.clean() {
                let short = check.base.get(..7).unwrap_or(&check.base);
                format!("Merges cleanly into {} at {short}", check.branch)
            } else {
                format!(
                    "Conflicts with {}: {}",
                    check.branch,
                    check.conflicts.join(", ")
                )
            };
            if let Some(revision) = check.revision.as_deref() {
                let short = revision.get(..7).unwrap_or(revision);
                line.push_str(&format!(" (checked {short})"));
            }
            if several {
                format!("{}: {line}", check.repository)
            } else {
                line
            }
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join(" · "))
}

/// Its branch and publish state, from its result and the outcomes of the
/// decisions to push it.
#[must_use]
pub fn changes(detail: &TaskDetail) -> Changes {
    let result = detail.summary.task.result.as_ref();
    let text = |json: Option<&Json>, field: &str| {
        json.and_then(|json| json.get(field))
            .and_then(Json::as_str)
            .map(str::to_owned)
    };
    let mut pushed = text(result, "branch").is_some();
    let mut pull_request = text(result, "pull_request");
    let mut last = None;
    for decision in detail
        .decisions
        .iter()
        .filter(|decision| PUBLISHING.contains(&decision.tool.as_str()))
    {
        let outcome = decision.outcome.as_ref();
        let performed = outcome.and_then(|outcome| outcome.get("performed"));
        last = Some(match (decision.status, performed) {
            (_, Some(Json::Bool(true))) => {
                pushed = true;
                pull_request = text(outcome, "pull_request").or(pull_request);
                "Pushed".to_owned()
            }
            (_, Some(_)) => format!(
                "Not pushed: {}",
                text(outcome, "error").unwrap_or_else(|| "the push failed".to_owned())
            ),
            (DecisionStatus::Pending, _) => format!("Waiting on you to push ({})", decision.id),
            (DecisionStatus::Answered, _) if !decision.approved() => {
                format!("Not pushed: you declined {}", decision.id)
            }
            (DecisionStatus::Answered, _) => "Pushing…".to_owned(),
            (DecisionStatus::Expired | DecisionStatus::Cancelled, _) => {
                format!("Not pushed: {} was not answered", decision.id)
            }
        });
    }
    let state = match (&pull_request, pushed, last) {
        (Some(_), _, _) => "Pushed · pull request opened".to_owned(),
        (None, true, _) => "Pushed".to_owned(),
        (None, false, Some(last)) => last,
        (None, false, None) => "Not pushed".to_owned(),
    };
    Changes {
        branch: detail.branch.clone(),
        state,
        pull_request,
        merge: merge_state(result),
    }
}

/// One entry of a task's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// Stable identity within the timeline.
    pub id: String,
    /// When, as the app shows it.
    pub time: String,
    /// What happened.
    pub text: String,
    /// The underlying message, when it can be copied or quoted.
    pub message: Option<String>,
}

/// A task's timeline, oldest first, from what its hub has: its creation,
/// each run with its machine, backend, duration and outcome, each decision
/// asked and answered, and its result.
#[must_use]
pub fn timeline(detail: &TaskDetail, when: &When) -> Vec<Moment> {
    task_moments(detail, when, |_, _| true, std::iter::empty())
}

impl AppState {
    /// The visible task timeline including local sends that are still pending.
    #[must_use]
    pub fn task_timeline(&self, space: &str, detail: &TaskDetail, when: &When) -> Vec<Moment> {
        let destination = Destination::Task(space.to_owned(), detail.summary.task.id);
        task_moments(
            detail,
            when,
            |text, at| !self.has_optimistic_copy(&destination, text, &[], at),
            self.outgoing()
                .filter(|send| send.destination == destination)
                .map(|send| {
                    (
                        send.at,
                        format!("sending-{}", send.id),
                        format!("You · Sending…\n\n{}", send.text),
                        None,
                    )
                }),
        )
    }
}

fn task_moments(
    detail: &TaskDetail,
    when: &When,
    show_owner: impl Fn(&str, Timestamp) -> bool,
    sending: impl Iterator<Item = (Timestamp, String, String, Option<String>)>,
) -> Vec<Moment> {
    let summary = &detail.summary;
    let task = &summary.task;
    let mut moments = vec![(
        task.created_at,
        "created".to_owned(),
        format!("Created: {}. {}.", task.title, origin(detail)),
        None,
    )];
    for run in &detail.runs {
        let run_summary = &run.run;
        let backend = run_summary
            .backend
            .map_or_else(String::new, |kind| format!(" with {kind}"));
        let agent = detail
            .reviews
            .iter()
            .find(|review| Some(review.reviewer) == run_summary.agent_id)
            .map_or(summary.agent.as_str(), |review| {
                review.reviewer_name.as_str()
            });
        let on = format!("{agent} on {}{backend}", run.machine);
        let text = match run_summary.status {
            RunStatus::Active => {
                format!("{on}: working for {}", when.since(run_summary.started_at))
            }
            status => {
                let ended = run_summary.ended_at.unwrap_or(run_summary.started_at);
                let took = duration(ended.saturating_since(run_summary.started_at));
                let outcome = match (status, run_summary.outcome) {
                    (RunStatus::Committed, Some(RunOutcome::Sent)) => "reported back".to_owned(),
                    (RunStatus::Committed, Some(RunOutcome::Deferred)) => {
                        "stopped to ask you".to_owned()
                    }
                    (RunStatus::Committed, _) => "ended quietly".to_owned(),
                    (RunStatus::Stopped, _) => "stopped by you".to_owned(),
                    (RunStatus::Failed, _) => format!(
                        "failed: {}",
                        run_summary.error.as_deref().unwrap_or("no reason given")
                    ),
                    _ => "was abandoned when its machine was lost".to_owned(),
                };
                format!("{on}: worked for {took}, {outcome}")
            }
        };
        moments.push((
            run_summary.started_at,
            format!("run-{}", run_summary.id),
            text,
            None,
        ));
    }
    for review in &detail.reviews {
        let ids: Vec<_> = review
            .revisions
            .iter()
            .map(|revision| revision.commit.get(..7).unwrap_or(&revision.commit))
            .collect();
        let revision = if ids.is_empty() {
            String::new()
        } else {
            format!(" · {}", ids.join(", "))
        };
        moments.push((
            review.submitted_at,
            format!("review-request-{}", review.submission),
            format!("Sent to {} for review{revision}", review.reviewer_name),
            None,
        ));
        if let (Some(verdict), Some(at)) = (review.verdict, review.decided_at) {
            let label = match verdict {
                threadlane_core::model::TaskReviewVerdict::Approved => "approved",
                threadlane_core::model::TaskReviewVerdict::ChangesRequested => "requested changes",
            };
            moments.push((
                at,
                format!("review-result-{}", review.submission),
                format!(
                    "{} {label}{revision}\n\n{}",
                    review.reviewer_name,
                    review.feedback.as_deref().unwrap_or_default()
                ),
                None,
            ));
        }
    }
    for decision in &detail.decisions {
        moments.push((
            decision.created_at,
            format!("asked-{}", decision.id),
            format!("Asked {}: {}", decision.id, decision.question),
            None,
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
                moments.push((
                    at,
                    format!("ended-{}", decision.id),
                    format!("You answered {}: {label}{note}", decision.id),
                    None,
                ));
            }
            (DecisionStatus::Expired, _) => moments.push((
                decision.expires_at.unwrap_or(decision.created_at),
                format!("ended-{}", decision.id),
                format!("{} expired unanswered", decision.id),
                None,
            )),
            _ => {}
        }
    }
    for entry in &detail.messages {
        if let ConversationEntry::You {
            event_id, text, at, ..
        } = entry
            && show_owner(text, *at)
        {
            moments.push((
                *at,
                format!("you-{event_id}"),
                format!("You wrote:\n\n{text}"),
                Some(text.clone()),
            ));
        }
    }
    moments.extend(sending);
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
            .map_or_else(String::new, |summary| format!(":\n\n{summary}"));
        let status = status_label(task.status, None);
        moments.push((
            ended,
            "result".to_owned(),
            format!("{status}{result}"),
            None,
        ));
    }
    moments.sort_by_key(|(at, ..)| *at);
    moments
        .into_iter()
        .map(|(at, id, text, message)| Moment {
            id,
            time: when.moment(at),
            text,
            message,
        })
        .collect()
}
