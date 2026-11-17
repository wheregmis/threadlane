use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;
use threadlane_client::ClientDirs;
use threadlane_core::client::{
    Accepted, ApiError, DecisionReply, DecisionRequest, FeedItem, HubApi, HubInfo, MemoryReply,
    MemoryRequest, OrgReply, OrgRequest, OverviewReply, OverviewRequest, PairingReply,
    PairingRequest, RunSummary, UserMessage,
};
use threadlane_core::ids::{AgentId, LaneId, MachineId, OutboxId, RunId, SessionId};
use threadlane_core::model::{
    BackendKind, DecisionKind, DecisionOption, DecisionStatus, MachineTrust, MemoryFile, Mode,
    OutboxMessage, OutboxStatus, Run, RunOutcome, RunStatus, Task, TaskOrigin, TaskStatus, Urgency,
};
use threadlane_core::time::Timestamp;

use super::*;

/// A hub with the projects `api` and General, and what it was given.
/// While `down`, it answers nothing; while `old`, it answers like a hub
/// from before the Overview capability.
#[derive(Debug, Default)]
struct FakeHub {
    space: String,
    down: AtomicBool,
    old: AtomicBool,
    created: Mutex<Vec<ProjectSummary>>,
    machines: Mutex<Vec<Machine>>,
    lead: Mutex<Option<Timestamp>>,
    decisions: Mutex<Vec<Decision>>,
    tasks: Mutex<Vec<TaskSummary>>,
    history: Mutex<Vec<ConversationEntry>>,
    recent: Mutex<Vec<FeedItem>>,
    bindings: Mutex<Vec<BindingSummary>>,
    memory: Mutex<Vec<MemoryFile>>,
}

impl FakeHub {
    fn new(space: &str, decisions: Vec<Decision>) -> Arc<Self> {
        Arc::new(Self {
            space: space.to_owned(),
            decisions: Mutex::new(decisions),
            ..Self::default()
        })
    }

    fn up(&self) -> Result<(), ApiError> {
        if self.down.load(Ordering::SeqCst) {
            Err(ApiError::Unreachable("connection refused".to_owned()))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl HubApi for FakeHub {
    async fn info(&self) -> Result<HubInfo, ApiError> {
        self.up()?;
        Ok(HubInfo {
            space: self.space.clone(),
            mode_ceiling: Mode::Assisted,
            schema_version: 1,
        })
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        self.up()?;
        let created = self.created.lock().expect("lock").clone();
        Ok([
            ProjectSummary {
                id: ProjectId(1),
                name: "api".to_owned(),
                is_general: false,
                paused: false,
            },
            ProjectSummary {
                id: ProjectId(2),
                name: "General".to_owned(),
                is_general: true,
                paused: false,
            },
        ]
        .into_iter()
        .chain(created)
        .collect())
    }

    async fn send_message(&self, _message: &UserMessage) -> Result<Accepted, ApiError> {
        Err(ApiError::Invalid("unsupported".to_owned()))
    }

    async fn feed(&self, _after: i64, _wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        Err(ApiError::Invalid("unsupported".to_owned()))
    }

    async fn org(&self, request: OrgRequest) -> Result<OrgReply, ApiError> {
        self.up()?;
        let mut bindings = self.bindings.lock().expect("lock");
        match request {
            OrgRequest::Bindings { .. } => Ok(OrgReply::Bindings {
                bindings: bindings.clone(),
            }),
            OrgRequest::SetMode { agent, mode, .. }
                if !bindings.iter().any(|b| b.agent == agent) =>
            {
                let binding = BindingSummary {
                    agent,
                    role: "reviewer".to_owned(),
                    mode,
                    is_lead: false,
                };
                bindings.push(binding.clone());
                Ok(OrgReply::Binding { binding })
            }
            OrgRequest::Repositories { .. } => Ok(OrgReply::Repositories {
                repositories: Vec::new(),
            }),
            OrgRequest::Agents => Ok(OrgReply::Agents { agents: Vec::new() }),
            OrgRequest::CreateProject { name, .. } => {
                let mut created = self.created.lock().expect("lock");
                let project = ProjectSummary {
                    id: ProjectId(3 + i64::try_from(created.len()).expect("few")),
                    name,
                    is_general: false,
                    paused: false,
                };
                created.push(project.clone());
                Ok(OrgReply::Project { project })
            }
            OrgRequest::SetMode { agent, mode, .. } => {
                let binding = bindings
                    .iter_mut()
                    .find(|binding| binding.agent == agent)
                    .ok_or(ApiError::NotFound(agent))?;
                binding.mode = mode;
                Ok(OrgReply::Binding {
                    binding: binding.clone(),
                })
            }
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    async fn decisions(&self, request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        self.up()?;
        let mut decisions = self.decisions.lock().expect("lock");
        match request {
            DecisionRequest::Pending => Ok(DecisionReply::Pending {
                decisions: decisions.clone(),
            }),
            DecisionRequest::Answer { decision: id, .. } => {
                let index = decisions
                    .iter()
                    .position(|decision| decision.id == id)
                    .ok_or_else(|| ApiError::NotFound(id.to_string()))?;
                let mut decision = decisions.remove(index);
                decision.status = DecisionStatus::Answered;
                Ok(DecisionReply::Answered {
                    decision: Box::new(decision),
                    recorded: true,
                })
            }
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError> {
        self.up()?;
        match request {
            PairingRequest::Clients => Ok(PairingReply::Clients {
                clients: Vec::new(),
            }),
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    async fn overview(&self, request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        self.up()?;
        if self.old.load(Ordering::SeqCst) {
            return Err(ApiError::Invalid("unknown variant `overview`".to_owned()));
        }
        let tasks = self.tasks.lock().expect("lock").clone();
        match request {
            OverviewRequest::Tasks { .. } => Ok(OverviewReply::Tasks { tasks }),
            OverviewRequest::Running => Ok(OverviewReply::Tasks {
                tasks: tasks
                    .into_iter()
                    .filter(|summary| summary.task.status == TaskStatus::Running)
                    .collect(),
            }),
            OverviewRequest::Conversation { .. } => Ok(OverviewReply::Conversation {
                entries: self.history.lock().expect("lock").clone(),
            }),
            OverviewRequest::Recent { .. } => Ok(OverviewReply::Recent {
                items: self.recent.lock().expect("lock").clone(),
            }),
            OverviewRequest::Machines => Ok(OverviewReply::Machines {
                machines: self.machines.lock().expect("lock").clone(),
                accounts: Vec::new(),
            }),
            OverviewRequest::Lead { .. } => Ok(OverviewReply::Lead {
                running_since: *self.lead.lock().expect("lock"),
            }),
            OverviewRequest::Task { task } => {
                let summary = tasks
                    .into_iter()
                    .find(|summary| summary.task.id == task)
                    .ok_or_else(|| ApiError::NotFound(format!("task {task}")))?;
                Ok(OverviewReply::Task {
                    task: Box::new(TaskDetail {
                        summary,
                        branch: format!("threadlane/{task}"),
                        repositories: Vec::new(),
                        runs: Vec::new(),
                        decisions: Vec::new(),
                        related: Vec::new(),
                        messages: Vec::new(),
                    }),
                })
            }
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    /// Memory like a hub's: every write a new version by you, refused
    /// when over budget, at an index, or outside the project.
    async fn memory(&self, request: MemoryRequest) -> Result<MemoryReply, ApiError> {
        self.up()?;
        let mut files = self.memory.lock().expect("lock");
        let current = |files: &[MemoryFile], path: &str| {
            files
                .iter()
                .rfind(|file| file.path == path)
                .filter(|file| !file.deleted)
                .cloned()
        };
        let mut change = |path: String, content: String, deleted: bool| {
            let version = files.iter().filter(|file| file.path == path).count() + 1;
            let file = memory_file(&path, &content, i64::try_from(version).expect("few"), None);
            let file = MemoryFile { deleted, ..file };
            files.push(file.clone());
            MemoryReply::File { file }
        };
        match request {
            MemoryRequest::List { .. } => {
                let mut paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
                paths.sort();
                paths.dedup();
                Ok(MemoryReply::Files {
                    files: paths
                        .iter()
                        .filter_map(|path| current(&files, path))
                        .collect(),
                })
            }
            MemoryRequest::History { path, .. } => Ok(MemoryReply::History {
                versions: files
                    .iter()
                    .filter(|file| file.path == path)
                    .cloned()
                    .collect(),
            }),
            MemoryRequest::Write { path, content, .. } if content.len() > 20 => {
                Err(ApiError::Invalid(format!(
                    "{path} would be {} bytes, over its budget of 20",
                    content.len()
                )))
            }
            MemoryRequest::Write { path, .. } if path.ends_with("INDEX.md") => Err(
                ApiError::Invalid("INDEX.md files are written by the system".to_owned()),
            ),
            MemoryRequest::Write { path, content, .. } => Ok(change(path, content, false)),
            MemoryRequest::Delete { path, .. } => Ok(change(path, String::new(), true)),
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }
}

fn memory_file(path: &str, content: &str, version: i64, run: Option<i64>) -> MemoryFile {
    MemoryFile {
        id: threadlane_core::ids::MemoryFileId(version),
        path: path.to_owned(),
        content: content.to_owned(),
        version,
        written_by_run: run.map(RunId),
        deleted: false,
        written_at: Timestamp::from_millis(version * 60_000),
    }
}

fn decision(id: i64, created_at: i64) -> Decision {
    let option = |label: &str, approve: bool| DecisionOption {
        label: label.to_owned(),
        effect: json!({ "approve": approve }),
    };
    Decision {
        id: DecisionId(id),
        project_id: ProjectId(1),
        lane_id: LaneId(1),
        task_id: None,
        session_id: None,
        run_id: None,
        tool_call_id: None,
        tool: "open_pull_request".to_owned(),
        arguments: json!({}),
        arguments_hash: String::new(),
        kind: DecisionKind::Approval,
        question: format!("Question {id}?"),
        context: String::new(),
        options: vec![option("approve", true), option("decline", false)],
        recommended: Some(0),
        offers_standing_rule: false,
        status: DecisionStatus::Pending,
        answer: None,
        expires_at: None,
        created_at: Timestamp::from_millis(created_at),
        outcome: None,
    }
}

fn feed_item(seq: i64, kind: OutboxKind, body: Json, at: i64) -> FeedItem {
    FeedItem {
        seq,
        message: OutboxMessage {
            id: OutboxId(seq),
            project_id: Some(ProjectId(1)),
            lane_id: None,
            session_id: None,
            run_id: None,
            tool_call_id: None,
            kind,
            decision_id: None,
            channel: "client".to_owned(),
            body,
            urgency: Urgency::Normal,
            status: OutboxStatus::Sent,
            not_before: None,
            attempts: 1,
            channel_message_id: None,
            created_at: Timestamp::from_millis(at),
            sent_at: Some(Timestamp::from_millis(at)),
            delivery_seq: Some(seq),
        },
        project_name: Some("api".to_owned()),
        delivered_at: Timestamp::from_millis(at),
    }
}

fn item(space: &str, seq: i64, kind: OutboxKind, body: Json) -> SpaceFeedItem {
    SpaceFeedItem {
        space: space.to_owned(),
        item: feed_item(seq, kind, body, 2_000),
    }
}

/// A space-wide account notice, as the hub keys it.
fn account_notice(seq: i64, key: &str, text: &str) -> FeedItem {
    let mut notice = feed_item(seq, OutboxKind::SystemNotice, json!({ "text": text }), seq);
    notice.message.project_id = None;
    notice.project_name = None;
    notice.message.tool_call_id = Some(key.to_owned());
    notice
}

fn task(id: i64, status: TaskStatus, waiting_reason: Option<&str>) -> TaskSummary {
    TaskSummary {
        task: Task {
            id: TaskId(id),
            project_id: ProjectId(1),
            lane_id: LaneId(10 + id),
            agent_id: AgentId(2),
            title: format!("Task {id}"),
            goal: String::new(),
            repositories: Vec::new(),
            parent_task_id: None,
            depends_on: Vec::new(),
            origin: TaskOrigin::User,
            status,
            priority: 0,
            created_by_call: None,
            last_machine_id: None,
            waiting_reason: waiting_reason.map(str::to_owned),
            result: None,
            created_at: Timestamp::from_millis(1_000),
        },
        project: "api".to_owned(),
        agent: "Engineer".to_owned(),
        machine: (status == TaskStatus::Running).then(|| "macbook".to_owned()),
        running_since: (status == TaskStatus::Running).then(|| Timestamp::from_millis(0)),
    }
}

/// Twelve minutes after the epoch, in UTC.
fn when() -> When {
    When {
        now: Timestamp::from_millis(12 * 60_000),
        offset_minutes: 0,
    }
}

/// A core with each `(name, hub)` paired for this process only.
fn core(hubs: &[(&str, &Arc<FakeHub>)]) -> (TempDir, ClientCore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut core = ClientCore::load(ClientDirs::new(dir.path())).expect("load");
    for (name, hub) in hubs {
        core = core.with_hub(name, Arc::clone(hub) as Arc<dyn HubApi>);
    }
    (dir, core)
}

fn queue(state: &AppState) -> Vec<(String, String)> {
    state
        .queue()
        .iter()
        .map(|queued| (queued.space.clone(), queued.decision.id.to_string()))
        .collect()
}

fn pair(space: &str, id: &str) -> (String, String) {
    (space.to_owned(), id.to_owned())
}

#[tokio::test]
async fn decisions_from_two_hubs_merge_oldest_first() {
    let work = FakeHub::new("Work", vec![decision(1, 3_000), decision(2, 1_000)]);
    let home = FakeHub::new("Home", vec![decision(1, 2_000)]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();

    state.apply(refresh(&core).await);

    assert_eq!(
        queue(&state),
        [
            pair("work", "D-0002"),
            pair("home", "D-0001"),
            pair("work", "D-0001")
        ]
    );
    let names: Vec<&str> = state.spaces().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["work", "home"]);
    assert!(state.spaces().iter().all(|s| s.reach == Reach::Reachable));
    assert_eq!(state.project_name("home", ProjectId(1)), Some("api"));
}

#[tokio::test]
async fn an_unreachable_hub_does_not_hide_the_others() {
    let work = FakeHub::new("Work", vec![decision(1, 1_000)]);
    work.tasks
        .lock()
        .expect("lock")
        .push(task(1, TaskStatus::Running, None));
    let home = FakeHub::new("Home", vec![decision(1, 2_000)]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    work.down.store(true, Ordering::SeqCst);
    home.decisions.lock().expect("lock").push(decision(2, 500));
    state.apply(refresh(&core).await);

    let work_space = &state.spaces()[0];
    assert!(
        matches!(work_space.reach, Reach::Unreachable(_)),
        "work is shown as away"
    );
    assert_eq!(work_space.projects.len(), 2, "its last projects are kept");
    assert_eq!(work_space.running.len(), 1, "and what it last ran");
    assert_eq!(state.spaces()[1].reach, Reach::Reachable);
    assert_eq!(work_space.gap(Part::Running), None, "away is not an error");
    assert!(state.retry_soon(), "and is asked again soon");
    assert_eq!(
        queue(&state),
        [
            pair("home", "D-0002"),
            pair("work", "D-0001"),
            pair("home", "D-0001")
        ],
        "home's new decision shows; work's last known one stays"
    );
}

#[tokio::test]
async fn answering_removes_the_decision_from_the_queue_counts_and_board() {
    let mut in_task = decision(1, 1_000);
    in_task.task_id = Some(TaskId(7));
    let work = FakeHub::new("Work", vec![in_task, decision(2, 2_000)]);
    work.tasks
        .lock()
        .expect("lock")
        .push(task(7, TaskStatus::WaitingOnYou, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    assert!(state.is_pending("work", "D-0001"));
    assert_eq!(state.open_in("work", ProjectId(1)), 2, "per-project count");
    assert_eq!(state.open_in("work", ProjectId(2)), 0);
    let waiting = |state: &AppState| {
        state
            .board("work", "api", &when())
            .into_iter()
            .find(|column| column.title == "Waiting on you")
            .expect("column")
            .cards
    };
    assert_eq!(waiting(&state)[0].decision, Some(DecisionId(1)));
    assert_eq!(waiting(&state)[0].meta, "Engineer · D-0001 open");

    let answer = core
        .answer(Some("work"), "D-0001", "approve", Some("go ahead"))
        .await
        .expect("answer");
    state.answered(&answer);

    assert!(!state.is_pending("work", "D-0001"));
    assert_eq!(queue(&state), [pair("work", "D-0002")]);
    assert_eq!(state.open_in("work", ProjectId(1)), 1);
    assert_eq!(waiting(&state)[0].decision, None, "no Answer button");
    state.apply(refresh(&core).await);
    assert_eq!(queue(&state), [pair("work", "D-0002")], "the hub agrees");
}

#[test]
fn decisions_and_notices_raise_one_notification_and_messages_none() {
    let mut state = AppState::default();
    let card = item(
        "work",
        2,
        OutboxKind::Decision,
        json!({
            "text": "D-0003: Open a pull request? Answer with ...",
            "decision": "D-0003",
            "question": "Open a pull request?",
            "options": ["approve", "decline"],
        }),
    );

    assert_eq!(
        state.receive(item(
            "work",
            1,
            OutboxKind::Message,
            json!({ "text": "hi" })
        )),
        None
    );
    let alert = state.receive(card.clone()).expect("a decision notifies");
    assert_eq!(alert.tag(), "work/D-0003");
    assert_eq!(decision_of_tag(&alert.tag()), Some(("work", "D-0003")));
    assert_eq!(alert.body(), "Open a pull request?");
    assert_eq!(alert.options(), ["approve", "decline"]);
    let notice = state.receive(item(
        "work",
        3,
        OutboxKind::SystemNotice,
        json!({ "text": "the lead stopped" }),
    ));
    assert!(matches!(notice, Some(Alert::Notice { ref text, .. }) if text == "the lead stopped"));
    let notice = notice.expect("a notice notifies");
    assert_eq!(decision_of_tag(&notice.tag()), None, "nothing to answer");

    assert_eq!(
        state.receive(card),
        None,
        "a replayed card does not notify again"
    );
}

fn said(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|line| match line {
            Line::Day(day) => format!("-- {day}"),
            Line::You { text, .. } => format!("you: {text}"),
            Line::Lead { text, .. } => format!("lead: {text}"),
            Line::Notice { text, .. } => format!("notice: {text}"),
            Line::Decision { card, pending, .. } => format!("{}: pending {pending}", card.id),
        })
        .collect()
}

#[tokio::test]
async fn the_conversation_merges_history_your_messages_and_the_feed_once_each() {
    let day = 86_400_000;
    let work = FakeHub::new("Work", Vec::new());
    *work.history.lock().expect("lock") = vec![
        ConversationEntry::You {
            event_id: EventId(1),
            text: "Fix the refunds".to_owned(),
            at: Timestamp::from_millis(1_000),
        },
        ConversationEntry::Delivered {
            item: feed_item(1, OutboxKind::Message, json!({ "text": "On it" }), 2_000),
        },
        ConversationEntry::Delivered {
            item: feed_item(
                2,
                OutboxKind::Decision,
                json!({ "decision": "D-0001", "question": "Open it?", "options": ["yes"] }),
                day + 1_000,
            ),
        },
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    // The feed delivers the same lead message again, and one more; you send
    // a message the history does not have yet.
    state.receive(SpaceFeedItem {
        space: "work".to_owned(),
        item: feed_item(1, OutboxKind::Message, json!({ "text": "On it" }), 2_000),
    });
    state.receive(SpaceFeedItem {
        space: "work".to_owned(),
        item: feed_item(
            3,
            OutboxKind::Message,
            json!({ "text": "Done" }),
            day + 3_000,
        ),
    });
    let sent_at = Timestamp::from_millis(day + 2_000);
    state.sent("work", "api", "Also the changelog", EventId(2), sent_at);
    // The next history load has your message too, under the same event id.
    work.history
        .lock()
        .expect("lock")
        .push(ConversationEntry::You {
            event_id: EventId(2),
            text: "Also the changelog".to_owned(),
            at: sent_at,
        });
    state.apply_project(load_project(&core, "work", "api").await);

    let when = When {
        now: Timestamp::from_millis(day + 5_000),
        offset_minutes: 0,
    };
    assert_eq!(
        said(&state.conversation("work", "api", &when)),
        [
            "-- Yesterday",
            "you: Fix the refunds",
            "lead: On it",
            "-- Today",
            "D-0001: pending false",
            "you: Also the changelog",
            "lead: Done",
        ]
    );
    assert!(state.conversation("work", "General", &when).is_empty());
    assert!(state.conversation("home", "api", &when).is_empty());
    assert_eq!(state.earlier("work", "api"), None, "one page holds it all");
}

#[test]
fn an_item_seen_twice_is_shown_once() {
    let mut state = AppState::default();
    let first = item("work", 1, OutboxKind::Message, json!({ "text": "hi" }));
    state.receive(first.clone());
    state.receive(first);
    // The same seq from another space is another item.
    state.receive(item(
        "home",
        1,
        OutboxKind::Message,
        json!({ "text": "hi" }),
    ));

    let lines = |space| said(&state.conversation(space, "api", &when()));
    assert_eq!(lines("work"), ["-- Today", "lead: hi"]);
    assert_eq!(lines("home"), ["-- Today", "lead: hi"]);
    assert!(state.conversation("work", "other", &when()).is_empty());
}

#[tokio::test]
async fn the_board_groups_tasks_by_status_and_says_why_one_waits() {
    let work = FakeHub::new("Work", Vec::new());
    *work.tasks.lock().expect("lock") = vec![
        task(1, TaskStatus::Queued, Some("waiting for T-0002 to be done")),
        task(2, TaskStatus::Running, None),
        task(3, TaskStatus::Queued, Some(BLOCKED_FOR_TEST)),
        task(4, TaskStatus::Done, None),
        task(5, TaskStatus::Queued, None),
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);

    let board: Vec<(&str, Vec<(TaskId, String)>)> = state
        .board("work", "api", &when())
        .into_iter()
        .map(|column| {
            (
                column.title,
                column
                    .cards
                    .into_iter()
                    .map(|card| (card.id, card.meta))
                    .collect(),
            )
        })
        .collect();
    let card = |id, meta: &str| (TaskId(id), meta.to_owned());
    assert_eq!(
        board,
        [
            (
                "Queued",
                vec![
                    card(1, "Engineer · waiting for T-0002 to be done"),
                    card(5, "Engineer · waiting for a machine"),
                ]
            ),
            (
                "Blocked",
                vec![card(3, &format!("Engineer · {BLOCKED_FOR_TEST}"))]
            ),
            ("Running", vec![card(2, "Engineer · macbook · 12 min")]),
            ("Waiting on you", vec![]),
            ("Done", vec![card(4, "Engineer")]),
        ]
    );
    assert!(
        state
            .board("work", "other", &when())
            .iter()
            .all(|c| c.cards.is_empty())
    );
}

const BLOCKED_FOR_TEST: &str = "blocked: every model account it can use is at its usage limit";

#[tokio::test]
async fn home_lists_what_runs_and_the_latest_result_per_project_filtered_by_space() {
    let work = FakeHub::new("Work", vec![decision(1, 1_000)]);
    *work.tasks.lock().expect("lock") = vec![
        task(1, TaskStatus::Running, None),
        task(2, TaskStatus::Queued, None),
    ];
    *work.recent.lock().expect("lock") = vec![
        feed_item(1, OutboxKind::Message, json!({ "text": "older" }), 1_000),
        feed_item(2, OutboxKind::Message, json!({ "text": "newer" }), 2_000),
    ];
    let home = FakeHub::new("Home", Vec::new());
    *home.recent.lock().expect("lock") = vec![feed_item(
        1,
        OutboxKind::Message,
        json!({ "text": "home result" }),
        3_000,
    )];
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    let running: Vec<_> = state
        .running()
        .into_iter()
        .map(|(space, summary)| (space, summary.task.id))
        .collect();
    assert_eq!(running, [("work", TaskId(1))]);
    let recent: Vec<_> = state
        .recent()
        .into_iter()
        .map(|item| (item.space.as_str(), item.text()))
        .collect();
    assert_eq!(
        recent,
        [
            ("home", "home result".to_owned()),
            ("work", "newer".to_owned())
        ]
    );
    assert_eq!(
        state.status_line(),
        "2 spaces reachable · 1 running · 1 need you · 0 of 0 machines online"
    );
    assert_eq!(
        state.target(),
        Some(("work", "General")),
        "General by default"
    );

    state.set_filter(Some("home"));
    assert!(state.running().is_empty());
    assert_eq!(state.needs_you().count(), 0);
    assert_eq!(state.recent().len(), 1);
    assert_eq!(state.target(), Some(("home", "General")));
    state.set_target("work", "api");
    assert_eq!(state.target(), Some(("work", "api")), "the last one chosen");
}

#[tokio::test]
async fn a_space_is_blocked_from_its_blocked_notice_until_one_says_it_resumed() {
    let work = FakeHub::new("Work", Vec::new());
    let home = FakeHub::new("Home", Vec::new());
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert_eq!(state.blocked("work"), None);

    let notice = |seq, key: &str, text: &str| SpaceFeedItem {
        space: "work".to_owned(),
        item: account_notice(seq, key, text),
    };
    state.receive(notice(1, "system:account:1:blocked", "No account left."));
    assert_eq!(state.blocked("work").as_deref(), Some("No account left."));
    assert_eq!(state.blocked("home"), None, "per space");
    // Another notice does not end it.
    state.receive(item(
        "work",
        2,
        OutboxKind::SystemNotice,
        json!({ "text": "the lead stopped" }),
    ));
    assert!(state.blocked("work").is_some());
    state.receive(notice(3, "system:account:1:resumed", "Resumed."));
    assert_eq!(state.blocked("work"), None);

    // From history after a restart: the latest one counts.
    *work.recent.lock().expect("lock") = vec![account_notice(
        4,
        "system:account:2:blocked",
        "Blocked again.",
    )];
    state.apply(refresh(&core).await);
    assert_eq!(state.blocked("work").as_deref(), Some("Blocked again."));
}

#[tokio::test]
async fn a_mode_change_shows_in_the_project_at_once() {
    let work = FakeHub::new("Work", Vec::new());
    *work.bindings.lock().expect("lock") = vec![BindingSummary {
        agent: "Engineer".to_owned(),
        role: "engineer".to_owned(),
        mode: Mode::OnRequest,
        is_lead: false,
    }];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);

    let binding = core
        .set_mode(Some("work"), Some("api"), "Engineer", Mode::Assisted)
        .await
        .expect("set mode");
    state.mode_set("work", "api", binding);

    let modes: Vec<_> = state
        .project("work", "api")
        .expect("loaded")
        .bindings
        .iter()
        .map(|binding| (binding.agent.as_str(), binding.mode))
        .collect();
    assert_eq!(modes, [("Engineer", Mode::Assisted)]);
}

#[test]
fn a_task_s_timeline_has_its_runs_decisions_and_result() {
    let mut summary = task(7, TaskStatus::Done, None);
    summary.machine = Some("linux-box".to_owned());
    summary.task.result = Some(json!({ "summary": "Refunds are idempotent." }));
    let run = |id, started, ended, status, outcome| RunSummary {
        run: Run {
            id: RunId(id),
            lane_id: LaneId(17),
            session_id: SessionId(1),
            machine_id: MachineId(2),
            lease_epoch: 1,
            agent_id: Some(AgentId(2)),
            backend: Some(BackendKind::Claude),
            status,
            outcome,
            rationale: None,
            error: (status == RunStatus::Failed).then(|| "usage limit".to_owned()),
            started_at: Timestamp::from_millis(started),
            ended_at: Some(Timestamp::from_millis(ended)),
        },
        machine: "linux-box".to_owned(),
    };
    let mut asked = decision(3, 200_000);
    asked.task_id = Some(TaskId(7));
    asked.status = DecisionStatus::Answered;
    asked.answer = Some(json!({ "option": 0, "note": "draft", "answered_at": 300_000 }));
    let detail = TaskDetail {
        summary,
        branch: "threadlane/T-0007".to_owned(),
        repositories: vec!["payments-service".to_owned()],
        runs: vec![
            run(1, 60_000, 120_000, RunStatus::Failed, None),
            run(
                2,
                120_000,
                180_000 + 60_000 * 28,
                RunStatus::Committed,
                Some(RunOutcome::Deferred),
            ),
            run(
                3,
                400_000,
                460_000,
                RunStatus::Committed,
                Some(RunOutcome::Sent),
            ),
        ],
        decisions: vec![asked],
        related: Vec::new(),
        messages: vec![ConversationEntry::You {
            event_id: EventId(9),
            text: "Use the ledger.".to_owned(),
            at: Timestamp::from_millis(350_000),
        }],
    };
    let when = When {
        now: Timestamp::from_millis(3_600_000),
        offset_minutes: 0,
    };

    assert_eq!(
        chips(&detail),
        [
            "Engineer",
            "claude",
            "linux-box",
            "payments-service",
            "threadlane/T-0007",
            "Requested by you",
            "Done"
        ]
    );
    let moments: Vec<_> = timeline(&detail, &when)
        .into_iter()
        .map(|moment| format!("{} {}", moment.time, moment.text))
        .collect();
    assert_eq!(
        moments,
        [
            "00:00 Created: Task 7. Requested by you.",
            "00:01 Engineer on linux-box with claude: worked for 1 min, failed: usage limit",
            "00:02 Engineer on linux-box with claude: worked for 29 min, stopped to ask you",
            "00:03 Asked D-0003: Question 3?",
            "00:05 You answered D-0003: approve with the note \"draft\"",
            "00:05 You wrote: Use the ledger.",
            "00:06 Engineer on linux-box with claude: worked for 1 min, reported back",
            "00:31 Done: Refunds are idempotent.",
        ]
    );
}

fn machine(id: i64, status: MachineStatus) -> Machine {
    Machine {
        id: MachineId(id),
        name: format!("machine-{id}"),
        endpoint_id: String::new(),
        is_hub: id == 1,
        trust: MachineTrust::Trusted,
        status,
        last_seen: None,
        capabilities: json!({}),
        backends: json!({}),
        gh_login: false,
        max_parallel: 1,
        load: 0,
    }
}

#[test]
fn paired_hubs_show_before_the_first_refresh_as_not_yet_known() {
    let state = AppState::paired(["work", "home"]);

    let names: Vec<&str> = state.spaces().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["work", "home"], "no \"No hub is paired\"");
    assert!(state.spaces().iter().all(|s| s.reach == Reach::Unknown));
    assert_eq!(state.spaces()[0].gap(Part::Projects), Some(Gap::Loading));
    assert_eq!(
        state.gaps(Part::Running),
        [("work", Gap::Loading), ("home", Gap::Loading)],
        "not \"Nothing is running\""
    );
    assert_eq!(state.machine_count(), None, "not 0 / 0");
    assert_eq!(
        state.status_line(),
        "0 spaces reachable · 0 running · 0 need you"
    );
    assert!(state.retry_soon());
    assert!(AppState::paired([]).spaces().is_empty());
}

#[tokio::test]
async fn a_hub_that_refuses_a_part_shows_its_error_there_and_the_rest_loads() {
    let work = FakeHub::new("Work", vec![decision(1, 1_000)]);
    work.machines
        .lock()
        .expect("lock")
        .push(machine(1, MachineStatus::Online));
    work.tasks
        .lock()
        .expect("lock")
        .push(task(1, TaskStatus::Running, None));
    work.old.store(true, Ordering::SeqCst);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::paired(["work"]);

    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    let space = &state.spaces()[0];
    assert_eq!(space.reach, Reach::Reachable);
    let refused = "hub work refused the request: invalid request: unknown variant `overview`";
    for part in [Part::Running, Part::Machines, Part::Recent] {
        assert_eq!(
            space.gap(part),
            Some(Gap::Failed(refused.to_owned())),
            "{part:?}"
        );
    }
    assert_eq!(state.machine_count(), None, "no count rather than 0 / 0");
    assert_eq!(space.gap(Part::Projects), None, "what did load shows");
    assert_eq!(space.projects.len(), 2);
    assert_eq!(queue(&state), [pair("work", "D-0001")]);
    assert_eq!(
        state.project_gap("work", "api", Part::Tasks),
        Some(Gap::Failed(refused.to_owned()))
    );
    assert_eq!(state.project_gap("work", "api", Part::Bindings), None);
    assert!(state.retry_soon(), "asked again at the next tick");

    // The hub learns the request; the next refresh fills the gaps.
    work.old.store(false, Ordering::SeqCst);
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);
    let space = &state.spaces()[0];
    assert_eq!(space.gap(Part::Machines), None);
    assert_eq!(state.machine_count(), Some((1, 1)));
    assert_eq!(state.running().len(), 1);
    assert_eq!(state.project_gap("work", "api", Part::Tasks), None);
    assert!(!state.retry_soon());
    assert_eq!(
        state.status_line(),
        "1 space reachable · 1 running · 1 need you · 1 of 1 machines online"
    );

    // Refused again: the old count is not shown as if it were current.
    work.old.store(true, Ordering::SeqCst);
    state.apply(refresh(&core).await);
    assert_eq!(state.machine_count(), None);
    assert!(state.spaces()[0].machines.is_empty());
    assert!(state.running().is_empty());
}

#[tokio::test]
async fn a_task_that_cannot_be_loaded_says_why_instead_of_loading_forever() {
    let work = FakeHub::new("Work", Vec::new());
    work.tasks
        .lock()
        .expect("lock")
        .push(task(4, TaskStatus::Done, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    assert_eq!(state.task_gap("work", TaskId(4)), Some(Gap::Loading));

    state.apply_task(load_task(&core, "work", TaskId(4)).await);
    assert_eq!(state.task_gap("work", TaskId(4)), None);
    assert!(state.task("work", TaskId(4)).is_some());

    state.apply_task(load_task(&core, "work", TaskId(9)).await);
    assert_eq!(
        state.task_gap("work", TaskId(9)),
        Some(Gap::Failed(
            "hub work refused the request: not found: task T-0009".to_owned()
        ))
    );
}

#[test]
fn a_long_result_shows_its_first_lines_until_you_ask_for_all_of_it() {
    let mut state = AppState::default();
    let long = "Line one\nLine two\nLine three\nLine four";
    state.receive(item(
        "work",
        1,
        OutboxKind::Message,
        json!({ "text": long }),
    ));
    state.receive(item(
        "work",
        2,
        OutboxKind::Message,
        json!({ "text": "Short." }),
    ));
    let wide = "word ".repeat(100);
    state.receive(item(
        "home",
        1,
        OutboxKind::Message,
        json!({ "text": wide }),
    ));
    let excerpt = |state: &AppState, space: &str, seq| {
        let item = state
            .delivered
            .get(&(space.to_owned(), seq))
            .expect("delivered");
        state.excerpt(item)
    };

    assert_eq!(
        excerpt(&state, "work", 1),
        Excerpt {
            text: "Line one\nLine two\nLine three…".to_owned(),
            toggle: Some("Show"),
        }
    );
    assert_eq!(excerpt(&state, "work", 2).toggle, None, "fits");
    let shown = excerpt(&state, "home", 1);
    assert_eq!(shown.text.chars().count(), 240, "one long line is cut too");
    assert_eq!(shown.toggle, Some("Show"));

    state.toggle_result("work", 1);
    assert_eq!(
        excerpt(&state, "work", 1),
        Excerpt {
            text: long.to_owned(),
            toggle: Some("Hide"),
        }
    );
    assert_eq!(
        excerpt(&state, "home", 1).toggle,
        Some("Show"),
        "per result"
    );
    state.toggle_result("work", 1);
    assert_eq!(excerpt(&state, "work", 1).toggle, Some("Show"));
}

#[tokio::test]
async fn a_card_waiting_on_you_offers_the_decision_s_options() {
    let mut asked = decision(1, 1_000);
    asked.task_id = Some(TaskId(7));
    let work = FakeHub::new("Work", vec![asked]);
    work.tasks
        .lock()
        .expect("lock")
        .push(task(7, TaskStatus::WaitingOnYou, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.apply_project(load_project(&core, "work", "api").await);

    let card = state
        .board("work", "api", &when())
        .into_iter()
        .flat_map(|column| column.cards)
        .find(|card| card.id == TaskId(7))
        .expect("card");
    assert_eq!(card.decision, Some(DecisionId(1)));
    assert_eq!(card.options, ["approve", "decline"]);

    let answer = core
        .answer(Some("work"), "D-0001", &card.options[0], None)
        .await
        .expect("answer");
    state.answered(&answer);
    let card = state
        .board("work", "api", &when())
        .into_iter()
        .flat_map(|column| column.cards)
        .find(|card| card.id == TaskId(7))
        .expect("card");
    assert_eq!((card.decision, card.options.len()), (None, 0));
}

#[tokio::test]
async fn the_modes_grid_has_a_row_per_project_and_a_cell_per_agent() {
    let work = FakeHub::new("Work", Vec::new());
    *work.bindings.lock().expect("lock") = vec![BindingSummary {
        agent: "Engineer".to_owned(),
        role: "engineer".to_owned(),
        mode: Mode::Assisted,
        is_lead: false,
    }];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    // The fake lists no agents; the grid's columns come from the space.
    state.spaces[0].agents = ["Engineer", "Reviewer"]
        .iter()
        .map(|name| Agent {
            id: AgentId(1),
            name: (*name).to_owned(),
            role: String::new(),
            instructions: String::new(),
            backends: Vec::new(),
            models: json!({}),
            tools: json!({}),
            requirements: json!({}),
        })
        .collect();
    let grid = state.modes("work");
    assert_eq!(grid.agents, ["Engineer", "Reviewer"]);
    assert!(
        grid.rows.iter().all(|row| row.modes == Err(Gap::Loading)),
        "until the bindings answer"
    );

    let projects = vec![
        ("work".to_owned(), "api".to_owned()),
        ("work".to_owned(), "General".to_owned()),
    ];
    state.apply_modes(load_modes(&core, projects).await);
    let binding = core
        .set_mode(Some("work"), Some("api"), "Reviewer", Mode::OnRequest)
        .await
        .expect("set mode");
    state.mode_set("work", "api", binding);

    let rows: Vec<_> = state
        .modes("work")
        .rows
        .into_iter()
        .map(|row| (row.project, row.modes))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "api".to_owned(),
                Ok(vec![Some(Mode::Assisted), Some(Mode::OnRequest)])
            ),
            // The fake shares one binding list across projects.
            ("General".to_owned(), Ok(vec![Some(Mode::Assisted), None])),
        ]
    );
    assert_eq!(state.modes("home"), ModesGrid::default());
}

#[tokio::test]
async fn the_conversation_header_says_since_when_the_lead_is_working() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(state.lead_line("work", "api", &when()), None, "idle");

    *work.lead.lock().expect("lock") = Some(Timestamp::from_millis(9 * 60_000));
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(
        state.lead_line("work", "api", &when()).as_deref(),
        Some("Lead is working · started 00:09")
    );

    work.old.store(true, Ordering::SeqCst);
    state.apply_project(load_project(&core, "work", "api").await);
    assert_eq!(
        state.lead_line("work", "api", &when()).as_deref(),
        Some(
            "Whether the lead is working: hub work refused the request: invalid request: \
             unknown variant `overview`"
        )
    );
}

#[tokio::test]
async fn a_new_project_shows_in_its_space_at_once() {
    let work = FakeHub::new("Work", Vec::new());
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    let created = core
        .create_project(Some("work"), "Payments", "")
        .await
        .expect("create");
    state.project_created("work", created.clone());
    state.project_created("work", created);
    state.project_created(
        "home",
        ProjectSummary {
            id: ProjectId(9),
            name: "Elsewhere".to_owned(),
            is_general: false,
            paused: false,
        },
    );

    let names: Vec<_> = state.spaces()[0]
        .projects
        .iter()
        .map(|project| project.name.as_str())
        .collect();
    assert_eq!(names, ["api", "General", "Payments"], "once");
    assert!(state.targets().contains(&("work", "Payments")));
    state.apply(refresh(&core).await);
    assert_eq!(state.spaces()[0].projects.len(), 3, "the hub agrees");
}

#[tokio::test]
async fn the_memory_tab_groups_the_files_the_lead_sees_and_you_change_them_as_yourself() {
    let work = FakeHub::new("Work", Vec::new());
    *work.memory.lock().expect("lock") = vec![
        memory_file(
            "project/tasks/T-0001.md",
            "# T-0001\nHalf done.",
            1,
            Some(4),
        ),
        memory_file("project/notes/INDEX.md", "- [Build](build.md)", 1, Some(3)),
        memory_file("project/notes/build.md", "# Build\nRun make.", 1, Some(3)),
        memory_file("space/notes/style.md", "# Style\nShort.", 1, Some(2)),
        memory_file("space/instructions.md", "Be kind.", 1, None),
        memory_file("project/repos/app.md", "no title", 1, Some(3)),
    ];
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    assert_eq!(state.memory_gap("work", "api"), Some(Gap::Loading));
    assert_eq!(state.lead_knows("work", "api"), None);

    state.apply_memory(load_memory(&core, "work", "api", None).await);
    let groups: Vec<(&str, Vec<(String, String)>)> = state
        .memory_groups("work", "api")
        .into_iter()
        .map(|group| {
            let files = group
                .files
                .into_iter()
                .map(|row| (row.path, row.title))
                .collect();
            (group.title, files)
        })
        .collect();
    let row = |path: &str, title: &str| (path.to_owned(), title.to_owned());
    assert_eq!(
        groups,
        [
            (
                "Space instructions",
                vec![row("space/instructions.md", "Be kind.")]
            ),
            ("Space notes", vec![row("space/notes/style.md", "Style")]),
            (
                "Project instructions",
                vec![row("project/instructions.md", "Not written yet")]
            ),
            (
                "Project notes",
                vec![
                    row("project/notes/INDEX.md", "INDEX.md"),
                    row("project/notes/build.md", "Build")
                ]
            ),
            (
                "Repository notes",
                vec![row("project/repos/app.md", "no title")]
            ),
            (
                "Task progress files",
                vec![row("project/tasks/T-0001.md", "T-0001")]
            ),
        ]
    );
    let knows = state.lead_knows("work", "api").expect("listed");
    assert_eq!(
        knows.lines(),
        [
            "No instructions for this project".to_owned(),
            "2 notes · 1 task progress file".to_owned()
        ]
    );

    // An index is shown, never edited.
    state.select_memory("work", "api", "project/notes/INDEX.md");
    assert_eq!(state.edit_memory("work", "api", true), None);
    assert!(!state.memory_editing("work", "api"));
    assert!(!editable("project/notes/INDEX.md"));

    // Instructions are yours to write, even before they exist; a write over
    // budget shows the refusal.
    let path = "project/instructions.md";
    state.select_memory("work", "api", path);
    assert!(state.memory_can_edit("work", "api"));
    assert_eq!(state.edit_memory("work", "api", true).as_deref(), Some(""));
    let saved = state
        .apply_change(change_memory(&core, "work", "api", path, Some("Ship it.".to_owned())).await);
    assert_eq!(
        saved.as_deref(),
        Ok("Saved project/instructions.md as version 1.")
    );
    assert!(state.memory_groups("work", "api")[2].files[0].selected);
    assert_eq!(
        state.edit_memory("work", "api", true).as_deref(),
        Some("Ship it.")
    );
    assert!(state.memory_editing("work", "api"));
    let refused =
        state.apply_change(change_memory(&core, "work", "api", path, Some("x".repeat(30))).await);
    assert!(refused.is_err());
    assert!(state.memory_editing("work", "api"), "still open to fix");
    assert!(
        state
            .memory_refused("work", "api")
            .is_some_and(|why| why.contains("over its budget")),
        "{:?}",
        state.memory_refused("work", "api")
    );
    state
        .apply_change(
            change_memory(&core, "work", "api", path, Some("Ship today.".to_owned())).await,
        )
        .expect("saved");
    assert!(!state.memory_editing("work", "api"));
    assert_eq!(state.memory_refused("work", "api"), None);
    assert_eq!(
        state
            .memory_file("work", "api")
            .map(|file| file.content.as_str()),
        Some("Ship today.")
    );
    assert_eq!(
        state.lead_knows("work", "api").expect("listed").lines()[0],
        "Instructions for this project"
    );

    // Versions, newest first, by you or by a run.
    state.select_memory("work", "api", "project/notes/build.md");
    state.apply_memory(
        load_memory(
            &core,
            "work",
            "api",
            Some("project/notes/build.md".to_owned()),
        )
        .await,
    );
    state
        .apply_change(change_memory(&core, "work", "api", "project/notes/build.md", None).await)
        .expect("deleted");
    assert_eq!(
        state.memory_versions("work", "api", &when()),
        ["v2 · 00:02 · deleted by you", "v1 · 00:01 · by run 3"]
    );
    assert_eq!(state.memory_file("work", "api"), None);

    work.down.store(true, Ordering::SeqCst);
    state.apply_memory(load_memory(&core, "work", "api", None).await);
    assert!(matches!(
        state.memory_gap("work", "api"),
        Some(Gap::Failed(_))
    ));
    assert!(state.retry_soon());
}

#[test]
fn a_decision_card_shows_why_it_is_asked_and_the_calls_arguments() {
    let mut pull = decision(1, 0);
    pull.context = "\nThe branch is pushed.\nMore detail.".to_owned();
    pull.arguments = json!({
        "body": "Adds   the\npage.",
        "title": "Add the page",
        "repository": "app",
        "draft": true,
    });
    assert_eq!(context_line(&pull), Some("The branch is pushed."));
    assert_eq!(
        arguments(&pull),
        [
            "Repository: app",
            "Title: Add the page",
            "Body: Adds the page.",
            "Draft: true"
        ]
    );

    let mut memory = decision(2, 0);
    memory.tool = "memory_create".to_owned();
    memory.arguments = json!({
        "path": "project/instructions.md",
        "content": "word ".repeat(100),
    });
    let shown = arguments(&memory);
    assert_eq!(shown[0], "Path: project/instructions.md");
    assert!(shown[1].starts_with("Content: word word"));
    assert!(
        shown[1].ends_with('…') && shown[1].chars().count() < 180,
        "{}",
        shown[1]
    );
    assert_eq!(context_line(&decision(3, 0)), None);
    memory.arguments = json!("not an object");
    assert!(arguments(&memory).is_empty());
}

#[tokio::test]
async fn a_space_added_or_unpaired_shows_at_once_and_the_last_action_stays() {
    let work = FakeHub::new("Work", vec![decision(1, 0)]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    state.report(Activity::Working("Pairing home…".to_owned()));

    state.space_added("home");
    state.space_added("home");
    let names: Vec<&str> = state.spaces().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["work", "home"]);
    assert_eq!(state.space("home").map(|s| &s.reach), Some(&Reach::Unknown));
    state.report(Activity::Done("Paired home.".to_owned()));
    assert_eq!(state.activity().map(Activity::text), Some("Paired home."));

    state.ask_unpair("work");
    assert_eq!(state.unpairing(), Some("work"));
    state.cancel_unpair();
    assert_eq!(state.unpairing(), None);
    state.ask_unpair("work");
    state.set_filter(Some("work"));
    state.space_removed("work");
    assert_eq!(state.unpairing(), None);
    assert!(state.space("work").is_none());
    assert!(state.queue().is_empty());
    assert_eq!(state.filter(), None);
    assert_eq!(
        state.receive(item(
            "work",
            1,
            OutboxKind::SystemNotice,
            json!({ "text": "hi" })
        )),
        None,
        "an unpaired space's feed is not shown"
    );
    assert!(state.recent().is_empty());
    assert!(
        state
            .receive(item(
                "home",
                1,
                OutboxKind::SystemNotice,
                json!({ "text": "hi" })
            ))
            .is_some()
    );
    assert_eq!(state.activity().map(Activity::text), Some("Paired home."));
}

#[tokio::test]
async fn your_message_to_a_task_shows_in_its_timeline_once() {
    let work = FakeHub::new("Work", Vec::new());
    work.tasks
        .lock()
        .expect("lock")
        .push(task(1, TaskStatus::WaitingOnYou, None));
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply_task(load_task(&core, "work", TaskId(1)).await);
    let at = Timestamp::from_millis(5 * 60_000);
    state.sent_to_task("work", TaskId(1), "Add tests first.", EventId(4), at);
    state.sent_to_task("work", TaskId(1), "Add tests first.", EventId(4), at);
    state.sent_to_task("work", TaskId(2), "Elsewhere.", EventId(5), at);
    let detail = state.task("work", TaskId(1)).expect("loaded");
    assert_eq!(
        detail.messages,
        [ConversationEntry::You {
            event_id: EventId(4),
            text: "Add tests first.".to_owned(),
            at
        }]
    );
    let moments: Vec<String> = timeline(detail, &when())
        .into_iter()
        .map(|moment| moment.text)
        .collect();
    assert!(
        moments.contains(&"You wrote: Add tests first.".to_owned()),
        "{moments:?}"
    );
}
