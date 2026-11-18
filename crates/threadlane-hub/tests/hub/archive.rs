//! Archiving preserves history and stops a project's work until restored.

use std::time::Duration;

use serde_json::json;
use threadlane_core::client::{
    ApiError, DecisionRequest, MemoryRequest, OrgReply, OrgRequest, OverviewRequest,
    ProjectSummary, TaskRequest, TimerReply, TimerRequest, UserMessage,
};
use threadlane_core::model::{
    Decision, DecisionKind, DecisionOption, EventKind, NewDecision, Project, Recurrence, Rule,
    Task, TaskStatus,
};
use threadlane_core::store::{
    DecisionStore, InboxStore, OrgStore, StoreError, TaskStore, TimerStore,
};
use threadlane_core::time::Clock;

use crate::common::TestHub;

const PROJECT: &str = "API";
const REMOTE: &str = "https://github.com/acme/api.git";

fn named() -> Option<String> {
    Some(PROJECT.into())
}

async fn project(hub: &TestHub) -> Project {
    let reply = hub
        .api
        .org(OrgRequest::CreateProject {
            name: PROJECT.into(),
            description: "A project with history".into(),
        })
        .await
        .unwrap();
    let OrgReply::Project { project } = reply else {
        panic!("expected project summary");
    };
    hub.db.project(project.id).await.unwrap().unwrap()
}

async fn archive(hub: &TestHub, archived: bool) -> Result<ProjectSummary, ApiError> {
    match hub
        .api
        .org(OrgRequest::SetArchived {
            project: named(),
            archived,
        })
        .await?
    {
        OrgReply::Project { project } => Ok(project),
        other => panic!("expected project summary, got {other:?}"),
    }
}

fn new_task() -> TaskRequest {
    TaskRequest::Create {
        project: named(),
        title: "Check the API".into(),
        goal: "Read its history.".into(),
        agent: "Lead".into(),
        repositories: Vec::new(),
        depends_on: Vec::new(),
        client_request_id: "archive-task".into(),
    }
}

async fn task(hub: &TestHub) -> Task {
    let threadlane_core::client::TaskReply::Task { task } =
        hub.api.tasks(new_task()).await.unwrap()
    else {
        panic!("expected task");
    };
    task
}

async fn open_decision(hub: &TestHub, project: &Project) -> Decision {
    hub.db
        .open_decision(
            None,
            &NewDecision {
                project_id: project.id,
                lane_id: project.lead_lane_id,
                task_id: None,
                session_id: None,
                run_id: None,
                tool_call_id: None,
                tool: "ask".into(),
                arguments: json!({}),
                arguments_hash: "archive-decision".into(),
                kind: DecisionKind::Input,
                question: "Keep the record?".into(),
                context: String::new(),
                options: vec![DecisionOption {
                    label: "Yes".into(),
                    effect: json!({}),
                }],
                recommended: None,
                offers_standing_rule: false,
                expires_at: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap()
}

fn answer(decision: &Decision) -> DecisionRequest {
    DecisionRequest::Answer {
        decision: decision.id,
        option: "Yes".into(),
        note: Some("Keep this record.".into()),
    }
}

fn message(id: &str) -> UserMessage {
    UserMessage {
        project: named(),
        text: "The conversation remains readable.".into(),
        images: Vec::new(),
        client_message_id: id.into(),
        task: None,
    }
}

#[tokio::test]
async fn archive_refuses_general_queued_tasks_running_tasks_and_open_decisions() {
    let hub = TestHub::start().await;
    let general = hub
        .api
        .org(OrgRequest::SetArchived {
            project: None,
            archived: true,
        })
        .await;
    assert!(matches!(general, Err(ApiError::Invalid(why)) if why.contains("General")));
    let project = project(&hub).await;
    let task = task(&hub).await;
    let queued = archive(&hub, true).await;
    assert!(matches!(queued, Err(ApiError::Invalid(why)) if why.contains("queued tasks")));
    hub.db
        .set_task_status(task.id, TaskStatus::Running, None)
        .await
        .unwrap();
    let running = archive(&hub, true).await;
    assert!(matches!(running, Err(ApiError::Invalid(why)) if why.contains("running tasks")));
    hub.api
        .tasks(TaskRequest::Cancel { task: task.id })
        .await
        .unwrap();
    let decision = open_decision(&hub, &project).await;
    let pending = archive(&hub, true).await;
    assert!(matches!(pending, Err(ApiError::Invalid(why)) if why.contains("open decisions")));
    assert!(
        !hub.api
            .projects()
            .await
            .unwrap()
            .iter()
            .find(|p| p.id == project.id)
            .unwrap()
            .archived
    );
    hub.api.decisions(answer(&decision)).await.unwrap();
    hub.hello().await;
    let lease = hub.link.request_work().await.unwrap().unwrap();
    let leased = archive(&hub, true).await;
    assert!(matches!(leased, Err(ApiError::Invalid(why)) if why.contains("an active lane")));
    hub.link.release(&lease).await.unwrap();
    assert!(archive(&hub, true).await.unwrap().archived);
}

#[tokio::test]
async fn archive_keeps_history_and_repository_ownership_and_restores_the_project() {
    let hub = TestHub::start().await;
    let project = project(&hub).await;
    let repository = |project| OrgRequest::AddRepository {
        project,
        name: "api".into(),
        remote_url: REMOTE.into(),
        default_branch: None,
    };
    hub.api.org(repository(named())).await.unwrap();
    let task = task(&hub).await;
    hub.api
        .tasks(TaskRequest::Cancel { task: task.id })
        .await
        .unwrap();
    hub.api.send_message(&message("before")).await.unwrap();
    let decision = open_decision(&hub, &project).await;
    let answer = answer(&decision);
    hub.api.decisions(answer.clone()).await.unwrap();
    let path = "project/instructions.md";
    let memory = hub
        .api
        .memory(MemoryRequest::Write {
            project: named(),
            path: path.into(),
            content: "Use the API.".into(),
        })
        .await
        .unwrap();
    hub.api
        .org(OrgRequest::SetPaused {
            project: named(),
            paused: true,
        })
        .await
        .unwrap();
    let with_pause = hub.db.project(project.id).await.unwrap().unwrap();
    let conversation = OverviewRequest::Conversation {
        project: named(),
        before: None,
        limit: 20,
    };
    let tasks = OverviewRequest::Tasks { project: named() };
    let prior_conversation = hub.api.overview(conversation.clone()).await.unwrap();
    let prior_tasks = hub.api.overview(tasks.clone()).await.unwrap();
    let prior_decision = hub.db.decision(decision.id).await.unwrap().unwrap();
    let prior_repo = hub
        .api
        .org(OrgRequest::Repositories { project: named() })
        .await
        .unwrap();

    let archived = archive(&hub, true).await.unwrap();
    assert!(archived.archived && archived.paused);
    let archived_at = hub
        .db
        .project(project.id)
        .await
        .unwrap()
        .unwrap()
        .archived_at;
    assert_eq!(archived_at, Some(hub.clock.now()));
    hub.clock.advance(Duration::from_secs(1));
    assert_eq!(archive(&hub, true).await.unwrap(), archived);
    assert_eq!(
        hub.db
            .project(project.id)
            .await
            .unwrap()
            .unwrap()
            .archived_at,
        archived_at
    );
    assert!(
        hub.api
            .projects()
            .await
            .unwrap()
            .iter()
            .all(|p| p.id != project.id)
    );
    assert!(
        hub.api
            .project_list(true)
            .await
            .unwrap()
            .contains(&archived)
    );
    assert_eq!(
        hub.api.overview(conversation).await.unwrap(),
        prior_conversation
    );
    assert_eq!(hub.api.overview(tasks).await.unwrap(), prior_tasks);
    let reply = hub.api.decisions(answer).await.unwrap();
    assert!(
        matches!(reply, threadlane_core::client::DecisionReply::Answered {
        decision, recorded: false
    } if *decision == prior_decision)
    );
    assert_eq!(
        hub.api
            .memory(MemoryRequest::Read {
                project: named(),
                path: path.into()
            })
            .await
            .unwrap(),
        memory
    );
    assert_eq!(
        hub.api
            .org(OrgRequest::Repositories { project: named() })
            .await
            .unwrap(),
        prior_repo
    );
    assert!(
        matches!(hub.api.org(repository(None)).await, Err(ApiError::Invalid(why)) if why.contains("already registered") && why.contains(PROJECT))
    );
    for task in [None, Some(task.id)] {
        let result = hub
            .api
            .send_message(&UserMessage {
                task,
                ..message("during")
            })
            .await;
        assert_eq!(
            result,
            Err(ApiError::Invalid("Project \"API\" is archived.".into()))
        );
    }
    assert!(
        matches!(hub.api.tasks(new_task()).await, Err(ApiError::Invalid(why)) if why.contains("archived"))
    );

    let restored = archive(&hub, false).await.unwrap();
    assert!(!restored.archived && restored.paused);
    assert_eq!(
        hub.db.project(project.id).await.unwrap().unwrap(),
        with_pause
    );
    assert!(hub.api.projects().await.unwrap().contains(&restored));
    assert!(hub.api.send_message(&message("after")).await.is_ok());
}

#[tokio::test]
async fn archived_placement_routines_and_dreaming_are_inert_on_the_fake_clock() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let project = project(&hub).await;
    let task = task(&hub).await;
    hub.api
        .tasks(TaskRequest::Cancel { task: task.id })
        .await
        .unwrap();
    hub.api
        .send_message(&message("pending-before-archive"))
        .await
        .unwrap();
    let cadence = Recurrence {
        rule: Rule::Interval { minutes: 60 },
        zone: "UTC".into(),
    };
    let TimerReply::Timer { timer: routine } = hub
        .api
        .timers(TimerRequest::CreateRoutine {
            project: named(),
            instruction: "Check the project.".into(),
            recurrence: cadence.clone(),
            client_request_id: "archive-routine".into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected a routine");
    };
    hub.api
        .memory(MemoryRequest::SetDreaming {
            project: named(),
            cadence: Some(cadence),
        })
        .await
        .unwrap();
    let dream = hub.db.dreaming_timer(project.id).await.unwrap().unwrap().0;
    let dream = hub.db.timer(dream).await.unwrap().unwrap();
    let pending = hub.db.pending_events(project.lead_lane_id).await.unwrap();
    assert!(!pending.is_empty());
    assert!(
        hub.db
            .ready_lanes(hub.clock.now())
            .await
            .unwrap()
            .contains(&project.lead_lane_id)
    );

    archive(&hub, true).await.unwrap();
    hub.clock.advance(Duration::from_secs(7200));
    let now = hub.clock.now();
    assert!(now > routine.due_at && now > dream.due_at);
    assert!(hub.db.due_timers(now).await.unwrap().is_empty());
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    for timer in [&routine, &dream] {
        assert!(
            matches!(hub.db.fire(timer.id, timer.due_at, Some(now), now).await, Err(StoreError::Conflict(why)) if why.contains("archived"))
        );
        assert_eq!(&hub.db.timer(timer.id).await.unwrap().unwrap(), timer);
    }
    assert_eq!(
        hub.db.pending_events(project.lead_lane_id).await.unwrap(),
        pending
    );
    assert!(hub.link.request_work().await.unwrap().is_none());
    for lane in [project.lead_lane_id, task.lane_id] {
        assert!(
            hub.db
                .grant_lease(lane, hub.machine, now, Duration::from_secs(60))
                .await
                .unwrap()
                .is_none()
        );
    }

    archive(&hub, false).await.unwrap();
    assert_eq!(
        hub.core.schedule_once().await.unwrap(),
        1,
        "both timers fire into the same lane"
    );
    let events = hub.db.pending_events(project.lead_lane_id).await.unwrap();
    assert!(events.iter().any(|e| e.kind == EventKind::Dream));
    assert!(events.iter().any(|e| e.kind == EventKind::TimerFired));
    assert!(hub.link.request_work().await.unwrap().is_some());
}

fn delete_request(name: &str, typed: &str) -> OrgRequest {
    OrgRequest::DeleteProject {
        project: name.into(),
        confirm_name: typed.into(),
    }
}

#[tokio::test]
async fn delete_refuses_wrong_confirmation_general_an_active_run_and_a_leased_lane() {
    let hub = TestHub::start().await;
    let project = project(&hub).await;
    let wrong = hub.api.org(delete_request(PROJECT, "api")).await;
    assert!(matches!(wrong, Err(ApiError::Invalid(why)) if why.contains("exact name")));
    let general = hub.general().await.name;
    let protected = hub.api.org(delete_request(&general, &general)).await;
    assert!(matches!(protected, Err(ApiError::Invalid(why)) if why.contains("General")));
    assert!(hub.db.project(project.id).await.unwrap().is_some());

    hub.hello().await;
    hub.api.send_message(&message("active")).await.unwrap();
    let lease = hub.link.request_work().await.unwrap().unwrap();
    let leased = hub.api.org(delete_request(PROJECT, PROJECT)).await;
    assert!(
        matches!(leased, Err(ApiError::Invalid(why)) if why.contains("Stop these") && why.contains("leased to machine"))
    );
    let threadlane_core::link::Begin::Run(run) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("expected an active run");
    };
    let active = hub.api.org(delete_request(PROJECT, PROJECT)).await;
    assert!(
        matches!(active, Err(ApiError::Invalid(why)) if why.contains(&format!("run {}", run.run_id.get()))
        && why.contains(&format!("lane {}", lease.lane_id.get())))
    );
    assert!(hub.db.project(project.id).await.unwrap().is_some());
}

#[tokio::test]
async fn deletion_previews_queued_work_and_open_decisions_notifies_general_and_frees_remotes() {
    let hub = TestHub::start().await;
    let project = project(&hub).await;
    let repository = |project| OrgRequest::AddRepository {
        project,
        name: "api".into(),
        remote_url: REMOTE.into(),
        default_branch: None,
    };
    hub.api.org(repository(named())).await.unwrap();
    let task = task(&hub).await;
    let decision = open_decision(&hub, &project).await;
    hub.api
        .send_message(&message("delete-history"))
        .await
        .unwrap();
    hub.api
        .memory(MemoryRequest::Write {
            project: named(),
            path: "project/instructions.md".into(),
            content: "project-only history".into(),
        })
        .await
        .unwrap();
    let OrgReply::DeletePreview { preview } = hub
        .api
        .org(OrgRequest::DeletePreview {
            project: PROJECT.into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected preview");
    };
    assert_eq!(preview.tasks, 1);
    assert_eq!(preview.queued_tasks, 1);
    assert_eq!(preview.open_decisions, 1);
    assert_eq!(preview.decisions, 1);
    assert_eq!(preview.events, 2);
    assert_eq!(preview.outbox, 1);
    assert_eq!(preview.repositories, 1);
    assert_eq!(preview.bindings, 1);
    assert!(preview.memory_files > 0 && preview.memory_versions > 0);
    let reply = hub.api.org(delete_request(PROJECT, PROJECT)).await.unwrap();
    assert!(
        matches!(reply, OrgReply::ProjectDeleted { project, deleted, leftovers } if project == PROJECT && deleted == preview && leftovers.is_empty())
    );
    assert!(hub.db.task(task.id).await.unwrap().is_none());
    assert!(hub.db.decision(decision.id).await.unwrap().is_none());
    assert!(
        matches!(hub.api.send_message(&message("stale")).await, Err(ApiError::NotFound(why)) if why.contains(PROJECT))
    );
    assert!(
        matches!(hub.api.org(delete_request(PROJECT, PROJECT)).await, Err(ApiError::NotFound(why)) if why.contains(PROJECT))
    );
    hub.api.org(repository(None)).await.unwrap();

    assert_eq!(hub.core.dispatch_once().await.unwrap(), 1);
    let feed = hub.feed().await;
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].message.project_id, Some(hub.general().await.id));
    assert_eq!(
        feed[0].message.body["text"],
        "Project API was deleted by you."
    );
}

#[tokio::test]
async fn a_crash_after_delete_commit_leaves_workspace_cleanup_for_the_next_sweep() {
    use std::sync::Arc;
    use threadlane_core::runtime::{NoHooks, steps::project_delete::AFTER_COMMIT};
    use threadlane_hub::HubSettings;
    use threadlane_testkit::CrashPlan;

    let dir = tempfile::tempdir().unwrap();
    let hub = TestHub::with(
        HubSettings::default(),
        Arc::new(CrashPlan::kill_at(AFTER_COMMIT)),
    )
    .await;
    let hub = TestHub::around(
        hub.core.clone().with_workspaces(dir.path().into()),
        hub.clock.clone(),
    )
    .await;
    let project = project(&hub).await;
    let task = task(&hub).await;
    let work = dir.path().join("worktrees").join(task.id.to_string());
    std::fs::create_dir_all(&work).unwrap();
    let reply = hub.api.org(delete_request(PROJECT, PROJECT)).await;
    assert!(matches!(reply, Err(ApiError::Internal(_))));
    assert!(work.is_dir());
    assert!(hub.db.project(project.id).await.unwrap().is_none());
    assert_eq!(hub.db.project_cleanups().await.unwrap().len(), 1);

    let restarted = hub.restart(Arc::new(NoHooks)).await;
    let restarted = restarted.core.with_workspaces(dir.path().into());
    restarted.sweep_once().await.unwrap();
    assert!(!work.exists());
    assert!(hub.db.project_cleanups().await.unwrap().is_empty());
    assert_eq!(restarted.dispatch_once().await.unwrap(), 1);
    assert_eq!(restarted.dispatch_once().await.unwrap(), 0);
}

#[tokio::test]
async fn deletion_reports_leftovers_without_undoing_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let hub = TestHub::start().await;
    let hub = TestHub::around(
        hub.core.clone().with_workspaces(dir.path().into()),
        hub.clock.clone(),
    )
    .await;
    let project = project(&hub).await;
    let task = task(&hub).await;
    let worktrees = dir.path().join("worktrees");
    std::fs::create_dir(&worktrees).unwrap();
    let bad = worktrees.canonicalize().unwrap().join(task.id.to_string());
    std::fs::write(&bad, "not a directory").unwrap();
    let reply = hub.api.org(delete_request(PROJECT, PROJECT)).await.unwrap();
    assert!(
        matches!(reply, OrgReply::ProjectDeleted { leftovers, .. } if leftovers == [bad.display().to_string()])
    );
    assert!(hub.db.project(project.id).await.unwrap().is_none());
    assert_eq!(hub.db.project_cleanups().await.unwrap().len(), 1);
    std::fs::remove_file(bad).unwrap();
    hub.core.sweep_once().await.unwrap();
    assert!(hub.db.project_cleanups().await.unwrap().is_empty());
}
