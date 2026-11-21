//! Tasks at the hub: the task tools, each lane kind's tools, eligibility and
//! limits, and the run cycle of a task lane (docs/milestone-3.md §2).

use serde_json::{Value as Json, json};
use threadlane_core::backend::NativeSession;
use threadlane_core::client::{ApiError, TaskReply, TaskRequest};
use threadlane_core::forge::REPORT_LIMITATION;
use threadlane_core::ids::{AgentId, TaskId};
use threadlane_core::link::{Begin, RunAssignment, RunFailure};
use threadlane_core::model::{
    EventKind, EventStatus, LaneKind, Lease, Mode, NewAgent, NewBinding, NewTask, RunOutcome, Task,
    TaskOrigin, TaskStatus, Trust,
};
use threadlane_core::runtime::NoHooks;
use threadlane_core::store::{InboxStore, OrgStore, OutboxStore, TaskStore};
use threadlane_core::time::Clock;
use threadlane_core::tools::ToolError;
use threadlane_hub::{
    ASK_PROJECT_LEAD, ASK_USER, CANCEL_TIMER, CREATE_SCHEDULE, LIST_TIMERS, SET_FOLLOW_UP,
};
use threadlane_hub::{
    CANCEL_TASK, CREATE_TASK, ENDED_REASON, FINISH_TASK, LIST_TASKS, MEMORY_CREATE, MEMORY_DELETE,
    MEMORY_EDIT, MEMORY_RENAME, MEMORY_VIEW, OFF_REASON, OPEN_PULL_REQUEST, PUBLISH_BRANCH,
    SEND_MESSAGE, SKIP_REASON, STAY_SILENT, TASK_RULES, UPDATE_TASK,
};

use crate::common::{PAST_LEASE, TestHub, begin, usable_report};

#[tokio::test]
async fn independent_review_sends_changes_back_and_completes_the_same_task() {
    let hub = TestHub::with(
        threadlane_hub::HubSettings {
            reviewer: Some("Reviewer".into()),
            ..Default::default()
        },
        std::sync::Arc::new(NoHooks),
    )
    .await;
    hub.hello().await;
    let engineer = engineer(&hub, 1).await;
    let mut settings = hub.db.space().await.unwrap();
    settings.mode_ceiling = Mode::Assisted;
    hub.db.update_space(&settings).await.unwrap();
    let reviewer = hub
        .db
        .create_agent(&NewAgent::named("Reviewer", "reviewer"))
        .await
        .unwrap();
    let project = hub.general().await;
    let engineer_binding = hub
        .db
        .bindings(project.id)
        .await
        .unwrap()
        .into_iter()
        .find(|binding| binding.agent_id == engineer)
        .unwrap();
    hub.db
        .set_binding_mode(engineer_binding.id, Mode::Assisted)
        .await
        .unwrap();
    let widgets = hub
        .db
        .create_repository(&threadlane_core::model::NewRepository {
            project_id: project.id,
            name: "widgets".into(),
            remote_url: "https://github.com/acme/widgets.git".into(),
            default_branch: "main".into(),
            protected_branches: vec![],
        })
        .await
        .unwrap();
    let created = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: project.id,
                agent_id: engineer,
                title: "Draft report".into(),
                goal: "Draft a report with totals".into(),
                repositories: vec![widgets.id],
                parent_task_id: None,
                depends_on: vec![],
                origin: TaskOrigin::Agent,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    let id = created.id;
    let (lease, implementation) = begin(&*hub.link).await;
    assert!(
        call(
            &hub,
            &lease,
            &implementation,
            "self-review",
            "finish_review",
            json!({"verdict":"approved","feedback":"Self-approved","checks":"rerun"})
        )
        .await
        .is_err()
    );
    // The review is posted on the pull request the task pushed onto, too.
    super::github::record_updated_pull_request(&hub, &created, &lease).await;
    write_progress(&hub, &lease, &implementation, &id.to_string()).await;
    call(
        &hub,
        &lease,
        &implementation,
        "submit",
        FINISH_TASK,
        json!({"summary":"First draft","merge":[{"repository":"widgets","branch":"main","base":"main","conflicts":[],"revision":"a".repeat(40)}]}),
    )
    .await
    .unwrap();
    hub.link
        .complete(&lease, implementation.run_id, None)
        .await
        .unwrap();
    assert_eq!(
        task(&hub, &id.to_string()).await.status,
        TaskStatus::InReview
    );
    assert!(task_results(&hub).await.is_empty());
    let target = |number, digit: &str| {
        threadlane_store::PrReviewHead::new(&widgets, number, &digit.repeat(40)).unwrap()
    };
    let pending = hub
        .db
        .pr_review_status(project.id, &target(42, "a"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((pending.task, pending.state.as_str()), (id, "in_progress"));
    assert!(pending.feedback.is_none());

    let Begin::Run(review) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("review never started")
    };
    assert_eq!(review.task_id, Some(id));
    assert_eq!(review.agent_name, "Reviewer");
    assert_eq!(
        review.review.as_ref().unwrap().submission,
        implementation.run_id
    );
    assert!(matches!(review.native_session, NativeSession::Start { .. }));
    assert!(
        call(&hub, &lease, &review, "publish", PUBLISH_BRANCH, json!({}))
            .await
            .is_err()
    );
    // The reviewer reads the task's memory but cannot change it (#610).
    assert!(review.tools.iter().any(|tool| tool.name == MEMORY_VIEW));
    assert!(!review.tools.iter().any(|tool| tool.name == MEMORY_CREATE));
    let progress = format!("project/tasks/{id}.md");
    let viewed = call(
        &hub,
        &lease,
        &review,
        "view",
        MEMORY_VIEW,
        json!({"path": progress}),
    )
    .await
    .unwrap();
    assert_eq!(viewed["content"], format!("# {id}\nDone."));
    let listed = call(
        &hub,
        &lease,
        &review,
        "list",
        MEMORY_VIEW,
        json!({"path": "project/tasks"}),
    )
    .await
    .unwrap();
    assert!(
        listed["files"]
            .as_array()
            .unwrap()
            .contains(&json!(progress))
    );
    for (tool, arguments) in [
        (
            MEMORY_CREATE,
            json!({"path": "project/notes/x.md", "content": "x"}),
        ),
        (
            MEMORY_EDIT,
            json!({"path": progress, "old_text": "Done.", "new_text": "No."}),
        ),
        (MEMORY_DELETE, json!({"path": progress})),
        (
            MEMORY_RENAME,
            json!({"from": progress, "to": "project/tasks/y.md"}),
        ),
    ] {
        assert!(matches!(
            call(&hub, &lease, &review, tool, tool, arguments).await,
            Err(ToolError::Blocked(_))
        ));
    }
    // A verdict must say whether the checks were rerun or taken from the submission.
    for checks in [json!(null), json!("trusted")] {
        assert!(
            call(
                &hub,
                &lease,
                &review,
                &format!("no-checks-{checks}"),
                "finish_review",
                json!({"verdict":"approved","feedback":"Fine","checks":checks}),
            )
            .await
            .is_err()
        );
    }
    call(
        &hub,
        &lease,
        &review,
        "feedback",
        "finish_review",
        json!({"verdict":"changes_requested","feedback":"Include the totals","checks":"from_submission"}),
    )
    .await
    .unwrap();
    hub.link
        .complete(&lease, review.run_id, None)
        .await
        .unwrap();
    assert_eq!(task(&hub, &id.to_string()).await.status, TaskStatus::Queued);
    assert_eq!(tasks(&hub).await.len(), 1);

    let Begin::Run(fix) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("Engineer did not get findings")
    };
    assert_eq!(fix.task_id, Some(id));
    assert_eq!(fix.agent_name, "Engineer");
    assert!(fix.prompt.contains("Include the totals"));
    call(
        &hub,
        &lease,
        &fix,
        "submit",
        FINISH_TASK,
        json!({"summary":"Includes the totals","merge":[{"repository":"widgets","branch":"main","base":"main","conflicts":[],"revision":"b".repeat(40)}]}),
    )
    .await
    .unwrap();
    hub.link.complete(&lease, fix.run_id, None).await.unwrap();
    let Begin::Run(final_review) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("second review never started")
    };
    assert_eq!(final_review.review.as_ref().unwrap().submission, fix.run_id);
    call(
        &hub,
        &lease,
        &final_review,
        "approval",
        "finish_review",
        json!({"verdict":"approved","feedback":"Totals match","checks":"rerun"}),
    )
    .await
    .unwrap();
    hub.link
        .complete(&lease, final_review.run_id, None)
        .await
        .unwrap();
    let completed = tasks(&hub).await;
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].id, id);
    assert_eq!(completed[0].status, TaskStatus::Done);
    assert_eq!(
        completed[0].result.as_ref().unwrap()["review"]["feedback"],
        "Checks: rerun independently in the review checkout.\n\nTotals match"
    );
    assert_eq!(task_results(&hub).await.len(), 1);
    let deliveries = hub.db.pending_github_reviews().await.unwrap();
    assert_eq!(deliveries.len(), 2);
    for (delivery, (sha, verdict, feedback)) in deliveries.iter().zip([
        (
            "a",
            "Changes requested",
            "Checks: not rerun; taken from the submission.\n\nInclude the totals",
        ),
        (
            "b",
            "Approved",
            "Checks: rerun independently in the review checkout.\n\nTotals match",
        ),
    ]) {
        assert_eq!(delivery.pull_url, "https://github.com/acme/widgets/pull/42");
        assert_eq!(delivery.revision, sha.repeat(40));
        assert_eq!(
            delivery.body,
            format!(
                "**{verdict}**\n\nReviewed at `{}`.\n\n{feedback}\n\n🤖 Reviewed by Threadlane (Reviewer, {id})",
                sha.repeat(8)
            )
        );
        hub.db.delivered_github_review(delivery).await.unwrap();
    }
    assert!(hub.db.pending_github_reviews().await.unwrap().is_empty());
    for (digit, verdict) in [("a", "changes_requested"), ("b", "approved")] {
        let recorded = hub
            .db
            .pr_review_status(project.id, &target(42, digit))
            .await
            .unwrap()
            .unwrap();
        assert_eq!((recorded.task, recorded.state.as_str()), (id, verdict));
    }
    for unmatched in [target(43, "b"), target(42, "c")] {
        assert!(
            hub.db
                .pr_review_status(project.id, &unmatched)
                .await
                .unwrap()
                .is_none()
        );
    }
    let history = hub.db.task_reviews(id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].reviewer, reviewer.id);
    assert_eq!(
        history[0].feedback.as_deref(),
        Some("Checks: not rerun; taken from the submission.\n\nInclude the totals")
    );
    assert_eq!(
        history[1].feedback.as_deref(),
        Some("Checks: rerun independently in the review checkout.\n\nTotals match")
    );
}

#[tokio::test]
async fn with_no_reviewer_named_the_projects_reviewer_agent_reviews() {
    let hub = TestHub::with(Default::default(), std::sync::Arc::new(NoHooks)).await;
    hub.hello().await;
    let project = hub.general().await;
    let engineer = engineer(&hub, 1).await;
    let reviewer = hub
        .db
        .create_agent(&NewAgent::named("Checker", "reviewer"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: project.id,
            agent_id: reviewer.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    hub.send("scan", "Check PR readiness").await;
    let (lease, lead) = begin(&*hub.link).await;
    assert!(lead.prompt.contains(r#"set agent to "Checker"."#));
    finish_run(&hub, &lease, &lead).await;
    let report = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: project.id,
                agent_id: engineer,
                title: "Draft report".into(),
                goal: "Draft a report".into(),
                repositories: vec![],
                parent_task_id: None,
                depends_on: vec![],
                origin: TaskOrigin::User,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    let (lease, run) = begin(&*hub.link).await;
    write_progress(&hub, &lease, &run, &report.id.to_string()).await;
    let submitted = call(
        &hub,
        &lease,
        &run,
        "finish",
        FINISH_TASK,
        json!({"summary": "Ready"}),
    )
    .await
    .unwrap();
    assert_eq!(
        submitted["submitted_for_review"],
        json!(true),
        "{submitted}"
    );
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    assert_eq!(
        task(&hub, &report.id.to_string()).await.status,
        TaskStatus::InReview
    );
}

/// A started run of the engineer's task, cancelled by you while it runs.
async fn cancelled_mid_run(hub: &TestHub) -> (Task, Lease, RunAssignment) {
    let engineer = engineer(hub, 1).await;
    let created = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: hub.general().await.id,
                agent_id: engineer,
                title: "Draft report".into(),
                goal: "Draft a report".into(),
                repositories: vec![],
                parent_task_id: None,
                depends_on: vec![],
                origin: TaskOrigin::User,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Task);
    hub.db
        .cancel_task(None, created.id, hub.clock.now())
        .await
        .unwrap();
    (created, lease, run)
}

fn blocked(result: Result<Json, ToolError>, why: &str) {
    match result {
        Err(ToolError::Blocked(message)) => {
            assert!(message.contains(why), "{why:?} in {message:?}")
        }
        other => panic!("expected a refusal saying {why:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn a_run_whose_task_is_cancelled_is_told_on_its_next_call() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let (cancelled, lease, run) = cancelled_mid_run(&hub).await;
    blocked(
        call(&hub, &lease, &run, "timers", LIST_TIMERS, json!({})).await,
        &format!("{} was cancelled at ", cancelled.id),
    );
    // It can still record what it found.
    write_progress(&hub, &lease, &run, &cancelled.id.to_string()).await;
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    assert_eq!(
        task(&hub, &cancelled.id.to_string()).await.status,
        TaskStatus::Cancelled
    );
}

#[tokio::test]
async fn finish_task_for_a_cancelled_task_is_refused_naming_its_status() {
    let hub = TestHub::with(
        threadlane_hub::HubSettings {
            reviewer: Some("Reviewer".into()),
            ..Default::default()
        },
        std::sync::Arc::new(NoHooks),
    )
    .await;
    hub.hello().await;
    let (cancelled, lease, run) = cancelled_mid_run(&hub).await;
    blocked(
        call(
            &hub,
            &lease,
            &run,
            "finish",
            FINISH_TASK,
            json!({"summary": "Ready"}),
        )
        .await,
        "was cancelled",
    );
    let after = task(&hub, &cancelled.id.to_string()).await;
    assert_eq!(after.status, TaskStatus::Cancelled);
    assert_eq!(after.result, None);
}

#[tokio::test]
async fn the_reviewers_own_task_finishes_without_a_review_round() {
    let hub = TestHub::with(
        threadlane_hub::HubSettings {
            reviewer: Some("Reviewer".into()),
            ..Default::default()
        },
        std::sync::Arc::new(NoHooks),
    )
    .await;
    hub.hello().await;
    let project = hub.general().await;
    let reviewer = hub
        .db
        .create_agent(&NewAgent::named("Reviewer", "reviewer"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: project.id,
            agent_id: reviewer.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    let scan = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: project.id,
                agent_id: reviewer.id,
                title: "Scan open pull requests".into(),
                goal: "Review the open pull requests".into(),
                repositories: vec![],
                parent_task_id: None,
                depends_on: vec![],
                origin: TaskOrigin::User,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    let (lease, run) = begin(&*hub.link).await;
    write_progress(&hub, &lease, &run, &scan.id.to_string()).await;
    let finished = call(
        &hub,
        &lease,
        &run,
        "finish",
        FINISH_TASK,
        json!({"summary": "Reviewed"}),
    )
    .await
    .unwrap();
    assert_eq!(finished["finished"], json!(true), "{finished}");
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    assert_eq!(
        task(&hub, &scan.id.to_string()).await.status,
        TaskStatus::Done
    );
}

#[tokio::test]
async fn review_handoffs_share_machine_slots() {
    let hub = TestHub::with(
        threadlane_hub::HubSettings {
            reviewer: Some("Reviewer".into()),
            ..Default::default()
        },
        std::sync::Arc::new(NoHooks),
    )
    .await;
    hub.hello().await;
    let project = hub.general().await;
    let engineer = engineer(&hub, 0).await;
    let reviewer = hub
        .db
        .create_agent(&NewAgent::named("Reviewer", "reviewer"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: project.id,
            agent_id: reviewer.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    for title in ["First report", "Second report"] {
        hub.db
            .create_task(
                None,
                &NewTask {
                    project_id: project.id,
                    agent_id: engineer,
                    title: title.into(),
                    goal: "Draft a report".into(),
                    repositories: vec![],
                    parent_task_id: None,
                    depends_on: vec![],
                    origin: TaskOrigin::User,
                    status: TaskStatus::Queued,
                    priority: 0,
                    created_by_call: None,
                    created_at: hub.clock.now(),
                },
            )
            .await
            .unwrap();
    }
    let (first, first_run) = begin(&*hub.link).await;
    let (second, second_run) = begin(&*hub.link).await;
    for (lease, run) in [(&first, &first_run), (&second, &second_run)] {
        write_progress(&hub, lease, run, &run.task_id.unwrap().to_string()).await;
        call(
            &hub,
            lease,
            run,
            "submit",
            FINISH_TASK,
            json!({"summary": "Ready"}),
        )
        .await
        .unwrap();
    }
    hub.link
        .complete(&first, first_run.run_id, None)
        .await
        .unwrap();
    let Begin::Run(review) = hub.link.begin_run(&first).await.unwrap() else {
        panic!("first review was not granted")
    };
    hub.link
        .complete(&second, second_run.run_id, None)
        .await
        .unwrap();
    let Begin::Run(next) = hub.link.begin_run(&second).await.unwrap() else {
        panic!("second review should use its existing slot")
    };
    call(
        &hub,
        &first,
        &review,
        "approval",
        "finish_review",
        json!({"verdict":"approved","feedback":"Ready","checks":"rerun"}),
    )
    .await
    .unwrap();
    hub.link
        .complete(&first, review.run_id, None)
        .await
        .unwrap();
    assert_eq!(next.agent_name, "Reviewer");
    assert_eq!(next.task_id, second_run.task_id);
}

/// The agent tasks are given to in these tests.
const ENGINEER: &str = "Engineer";

/// Binds an engineer to General that may run `max_parallel` tasks at once.
async fn engineer(hub: &TestHub, max_parallel: u32) -> AgentId {
    let agent = hub
        .db
        .create_agent(&NewAgent::named(ENGINEER, "engineer"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: hub.general().await.id,
            agent_id: agent.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel,
        })
        .await
        .unwrap();
    agent.id
}

/// A hub with an engineer and a lead run in progress on a message.
async fn lead_run(max_parallel: u32) -> (TestHub, Lease, RunAssignment) {
    let hub = TestHub::start().await;
    hub.hello().await;
    engineer(&hub, max_parallel).await;
    hub.send("m-1", "Build the report page").await;
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Lead);
    (hub, lease, run)
}

async fn call(
    hub: &TestHub,
    lease: &Lease,
    run: &RunAssignment,
    call_id: &str,
    tool: &str,
    arguments: Json,
) -> Result<Json, ToolError> {
    hub.link
        .call_tool(lease, run.run_id, Some(call_id), tool, &arguments)
        .await
}

fn task_args(title: &str, depends_on: &[&str]) -> Json {
    json!({
        "title": title,
        "goal": format!("{title} is done"),
        "agent": ENGINEER,
        "depends_on": depends_on,
    })
}

async fn tasks(hub: &TestHub) -> Vec<Task> {
    hub.db.tasks(hub.general().await.id).await.unwrap()
}

async fn task(hub: &TestHub, id: &str) -> Task {
    let id: i64 = id.trim_start_matches("T-").parse().unwrap();
    hub.db.task(TaskId(id)).await.unwrap().unwrap()
}

/// Commits the run and releases the lane.
/// Writes the progress file of `task`, which `finish_task` requires.
async fn write_progress(hub: &TestHub, lease: &Lease, run: &RunAssignment, task: &str) {
    call(
        hub,
        lease,
        run,
        "toolu_p",
        MEMORY_CREATE,
        json!({ "path": format!("project/tasks/{task}.md"), "content": format!("# {task}\nDone.") }),
    )
    .await
    .unwrap();
}

async fn finish_run(hub: &TestHub, lease: &Lease, run: &RunAssignment) -> RunOutcome {
    let outcome = hub.link.complete(lease, run.run_id, None).await.unwrap();
    assert_eq!(hub.link.begin_run(lease).await.unwrap(), Begin::Idle);
    hub.link.release(lease).await.unwrap();
    outcome
}

/// The `task_result` events in General's lead lane, in any status.
async fn task_results(hub: &TestHub) -> Vec<threadlane_core::model::Event> {
    let mut results = Vec::new();
    for id in 1.. {
        match hub
            .db
            .event(threadlane_core::ids::EventId(id))
            .await
            .unwrap()
        {
            Some(event) if event.kind == EventKind::TaskResult => results.push(event),
            Some(_) => {}
            None => return results,
        }
    }
    unreachable!()
}

#[tokio::test]
async fn create_task_makes_one_queued_task_per_call_however_often_it_is_repeated() {
    let (hub, lease, run) = lead_run(2).await;
    let created = call(
        &hub,
        &lease,
        &run,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    assert_eq!(created["task_id"], "T-0001");
    assert_eq!(created["status"], "queued");
    let again = call(
        &hub,
        &lease,
        &run,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    assert_eq!(again, created, "the recorded result");

    // The runner dies; a new run resumes the session and repeats the call.
    hub.clock.advance(PAST_LEASE);
    hub.core.sweep_once().await.unwrap();
    let (lease, rerun) = begin(&*hub.link).await;
    assert_eq!(rerun.session_id, run.session_id);
    assert!(matches!(rerun.native_session, NativeSession::Resume { .. }));
    let repeated = call(
        &hub,
        &lease,
        &rerun,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    assert_eq!(repeated["task_id"], "T-0001", "the same task");
    finish_run(&hub, &lease, &rerun).await;

    let all = tasks(&hub).await;
    assert_eq!(all.len(), 1);
    let task = &all[0];
    assert_eq!(task.status, TaskStatus::Queued);
    assert_eq!(
        task.origin,
        TaskOrigin::User,
        "the lead works on your message"
    );
    assert_eq!(task.parent_task_id, None);
    assert_eq!(task.title, "page");
    assert_eq!(task.goal, "page is done");
    let start = hub.db.pending_events(task.lane_id).await.unwrap();
    assert_eq!(start.len(), 1);
    assert_eq!(start[0].kind, EventKind::TaskStart);
    assert!(start[0].requested);
}

#[tokio::test]
async fn scans_share_one_pr_review_per_head_and_reuse_its_committed_verdict() {
    let hub = TestHub::with(
        threadlane_hub::HubSettings {
            reviewer: Some("Checker".into()),
            ..Default::default()
        },
        std::sync::Arc::new(NoHooks),
    )
    .await;
    hub.hello().await;
    engineer(&hub, 2).await;
    let project = hub.general().await;
    let reviewer = hub
        .db
        .create_agent(&NewAgent::named("Checker", "quality"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: project.id,
            agent_id: reviewer.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    hub.db
        .create_repository(&threadlane_core::model::NewRepository {
            project_id: project.id,
            name: "widgets".into(),
            remote_url: "git@github.com:Acme/Widgets.git".into(),
            default_branch: "main".into(),
            protected_branches: vec![],
        })
        .await
        .unwrap();
    hub.send("scan", "Check PR readiness").await;
    let (lease, lead) = begin(&*hub.link).await;
    assert!(lead.prompt.contains(r#"set agent to "Checker"."#));
    let head = "a".repeat(40);
    let target = json!({"repository":"widgets","number":42,"head":head});
    let scan = json!({"title":"Review PR 42","goal":"Review the diff","agent":ENGINEER,"pr_review":target});
    assert_eq!(
        call(
            &hub,
            &lease,
            &lead,
            "lookup",
            threadlane_hub::PR_REVIEW_STATUS,
            target.clone()
        )
        .await
        .unwrap()["review_status"],
        "unreviewed"
    );
    let (left, right) = tokio::join!(
        call(&hub, &lease, &lead, "scan1", CREATE_TASK, scan.clone()),
        call(&hub, &lease, &lead, "scan2", CREATE_TASK, scan.clone()),
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    let (first, repeated) = if left.get("review_status").is_none() {
        (left, right)
    } else {
        (right, left)
    };
    assert_eq!(first["task_id"], repeated["task_id"]);
    assert_eq!(repeated["review_status"], "in_progress");
    assert_eq!(tasks(&hub).await.len(), 1);
    finish_run(&hub, &lease, &lead).await;

    let (lease, review) = begin(&*hub.link).await;
    assert_eq!(review.agent_name, ENGINEER);
    assert!(review.prompt.contains(&head));
    let target = review.pr_review.as_ref().unwrap();
    assert_eq!(target.number, 42);
    assert_eq!(target.revision.commit, head);
    assert_eq!(target.revision.repository, review.repositories[0].id);
    write_progress(&hub, &lease, &review, first["task_id"].as_str().unwrap()).await;
    let mut result =
        json!({"summary":"All checks pass","pr_verdict":"approved","reviewed_head":"b".repeat(40)});
    assert!(
        call(&hub, &lease, &review, "stale", FINISH_TASK, result.clone())
            .await
            .is_err()
    );
    result["reviewed_head"] = json!(head);
    let finished = call(&hub, &lease, &review, "verdict", FINISH_TASK, result)
        .await
        .unwrap();
    assert_eq!(finished["finished"], true);
    finish_run(&hub, &lease, &review).await;

    let (lease, lead) = begin(&*hub.link).await;
    let reused = call(&hub, &lease, &lead, "scan3", CREATE_TASK, scan.clone())
        .await
        .unwrap();
    assert_eq!(reused["review_status"], "approved");
    assert_eq!(reused["feedback"], "All checks pass");
    let mut updated = scan;
    updated["pr_review"]["head"] = json!("b".repeat(40));
    let new = call(&hub, &lease, &lead, "scan4", CREATE_TASK, updated)
        .await
        .unwrap();
    assert_ne!(new["task_id"], first["task_id"]);
    assert_eq!(tasks(&hub).await.len(), 2);
}

#[tokio::test]
async fn the_task_tools_check_their_arguments() {
    let (hub, lease, run) = lead_run(2).await;
    let other = hub
        .db
        .create_agent(&NewAgent::named("Stranger", "engineer"))
        .await
        .unwrap();
    assert!(other.id.get() > 0);
    for (n, arguments) in [
        json!({ "goal": "no title" }),
        json!({ "title": "x", "goal": " " }),
        json!({ "title": "x", "goal": "y", "assignee": ENGINEER }),
        json!({ "title": "x", "goal": "y", "depends_on": "T-0001" }),
        json!({ "title": "x", "goal": "y", "depends_on": ["first"] }),
        json!({ "title": "x", "goal": "y", "priority": "high" }),
    ]
    .into_iter()
    .enumerate()
    {
        let result = call(&hub, &lease, &run, &format!("c{n}"), CREATE_TASK, arguments).await;
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{result:?}"
        );
    }
    let unbound = call(
        &hub,
        &lease,
        &run,
        "c-agent",
        CREATE_TASK,
        json!({ "title": "x", "goal": "y", "agent": "Stranger" }),
    )
    .await;
    match unbound {
        Err(ToolError::InvalidArguments(message)) => {
            assert!(
                message.contains("Lead, Engineer"),
                "it names the agents: {message}"
            );
        }
        other => panic!("{other:?}"),
    }
    let dangling = call(
        &hub,
        &lease,
        &run,
        "c-dep",
        CREATE_TASK,
        task_args("x", &["T-0009"]),
    )
    .await;
    assert!(
        matches!(dangling, Err(ToolError::Failed(_))),
        "{dangling:?}"
    );
    for (tool, arguments) in [
        (UPDATE_TASK, json!({ "task_id": "T-0009", "title": "x" })),
        (CANCEL_TASK, json!({ "task_id": "T-0009" })),
        (CANCEL_TASK, json!({})),
        (LIST_TASKS, json!({ "status": "queued" })),
    ] {
        let result = call(&hub, &lease, &run, &format!("{tool}-bad"), tool, arguments).await;
        assert!(
            matches!(result, Err(ToolError::InvalidArguments(_))),
            "{tool}: {result:?}"
        );
    }
    assert!(tasks(&hub).await.is_empty());
}

#[tokio::test]
async fn each_lane_kind_is_offered_only_its_tools_and_the_hub_refuses_the_others() {
    let (hub, lease, run) = lead_run(2).await;
    let names = |run: &RunAssignment| -> Vec<String> {
        run.tools.iter().map(|tool| tool.name.clone()).collect()
    };
    assert_eq!(
        names(&run),
        [
            SEND_MESSAGE,
            STAY_SILENT,
            CREATE_TASK,
            UPDATE_TASK,
            CANCEL_TASK,
            LIST_TASKS,
            threadlane_hub::PR_REVIEW_STATUS,
            MEMORY_VIEW,
            MEMORY_CREATE,
            MEMORY_EDIT,
            MEMORY_DELETE,
            MEMORY_RENAME,
            SET_FOLLOW_UP,
            CREATE_SCHEDULE,
            LIST_TIMERS,
            CANCEL_TIMER,
            ASK_USER,
            ASK_PROJECT_LEAD,
            REPORT_LIMITATION,
        ]
    );
    assert!(
        run.remove_builtin_tools,
        "a lead has no file or shell tools"
    );
    let refused = call(
        &hub,
        &lease,
        &run,
        "toolu_f",
        FINISH_TASK,
        json!({ "summary": "x" }),
    )
    .await;
    assert!(matches!(refused, Err(ToolError::Blocked(_))), "{refused:?}");
    call(
        &hub,
        &lease,
        &run,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lease, &run).await;

    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Task);
    assert_eq!(
        names(&run),
        [
            CREATE_TASK,
            threadlane_hub::PR_REVIEW_STATUS,
            FINISH_TASK,
            PUBLISH_BRANCH,
            OPEN_PULL_REQUEST,
            threadlane_hub::FAST_FORWARD_PULL_REQUEST,
            threadlane_hub::COMMENT,
            threadlane_hub::CREATE_ISSUE,
            threadlane_hub::MERGE,
            threadlane_hub::MARK_READY,
            threadlane_hub::LABEL_ISSUE,
            threadlane_hub::CLOSE_PULL_REQUEST,
            threadlane_core::forge::GITHUB_CLI,
            threadlane_hub::ASSIGN_ISSUE,
            threadlane_hub::EDIT_PULL_REQUEST,
            threadlane_hub::RESOLVE_REVIEW_THREAD,
            threadlane_hub::REQUEST_REVIEW,
            MEMORY_VIEW,
            MEMORY_CREATE,
            MEMORY_EDIT,
            MEMORY_DELETE,
            MEMORY_RENAME,
            SET_FOLLOW_UP,
            CREATE_SCHEDULE,
            LIST_TIMERS,
            CANCEL_TIMER,
            ASK_USER,
            REPORT_LIMITATION,
        ]
    );
    assert!(
        !run.remove_builtin_tools,
        "a task agent keeps its file and shell tools"
    );
    for (tool, arguments) in [
        (SEND_MESSAGE, json!({ "text": "hello" })),
        (LIST_TASKS, json!({})),
        (CANCEL_TASK, json!({ "task_id": "T-0001" })),
        (UPDATE_TASK, json!({ "task_id": "T-0001", "title": "x" })),
    ] {
        let refused = call(&hub, &lease, &run, &format!("{tool}-1"), tool, arguments).await;
        assert!(
            matches!(refused, Err(ToolError::Blocked(_))),
            "{tool}: {refused:?}"
        );
    }
    assert_eq!(hub.db.count_for_run(run.run_id).await.unwrap(), 0);
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Running);
}

#[tokio::test]
async fn a_task_run_works_on_its_task_and_finishing_reports_one_result_to_the_lead() {
    let (hub, lease, run) = lead_run(2).await;
    call(
        &hub,
        &lease,
        &run,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lease, &run).await;

    let (lease, run) = begin(&*hub.link).await;
    assert!(
        run.prompt.contains("Your task, T-0001: page"),
        "{}",
        run.prompt
    );
    assert!(
        run.prompt.contains("Done when: page is done"),
        "{}",
        run.prompt
    );
    assert!(run.appended_instructions.contains(TASK_RULES));
    let running = task(&hub, "T-0001").await;
    assert_eq!(running.status, TaskStatus::Running);
    assert_eq!(running.last_machine_id, Some(hub.machine));

    let summary = json!({ "summary": "The page is up." });
    write_progress(&hub, &lease, &run, "T-0001").await;
    let finished = call(&hub, &lease, &run, "toolu_f", FINISH_TASK, summary.clone())
        .await
        .unwrap();
    assert_eq!(finished["finished"], true);
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Running);
    assert!(
        task_results(&hub).await.is_empty(),
        "reported when the run ends"
    );

    // The runner dies after finishing; the resumed session finishes again.
    hub.clock.advance(PAST_LEASE);
    hub.core.sweep_once().await.unwrap();
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Queued);
    let (lease, rerun) = begin(&*hub.link).await;
    assert_eq!(rerun.session_id, run.session_id);
    call(
        &hub,
        &lease,
        &rerun,
        "toolu_f",
        FINISH_TASK,
        summary.clone(),
    )
    .await
    .unwrap();
    assert_eq!(finish_run(&hub, &lease, &rerun).await, RunOutcome::Silent);

    let done = task(&hub, "T-0001").await;
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(done.result, Some(summary.clone()));
    let results = task_results(&hub).await;
    assert_eq!(results.len(), 1, "one result, however often it finished");
    let result = &results[0];
    assert_eq!(result.lane_id, hub.general().await.lead_lane_id);
    assert_eq!(result.trust, Trust::System);
    assert!(
        result.requested,
        "the result of work you asked for is requested"
    );
    assert_eq!(result.payload["status"], "done");
    assert_eq!(result.payload["result"], summary);

    // It wakes the lead, which is shown it.
    let (_, lead) = begin(&*hub.link).await;
    assert_eq!(lead.lane_kind, LaneKind::Lead);
    assert_eq!(lead.events, vec![result.id]);
    assert!(lead.prompt.contains("task_result"), "{}", lead.prompt);
    assert!(lead.prompt.contains("The page is up."), "{}", lead.prompt);
}

#[tokio::test]
async fn a_task_run_that_does_not_finish_sleeps_and_one_that_fails_is_queued_again() {
    let (hub, lease, run) = lead_run(2).await;
    call(
        &hub,
        &lease,
        &run,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lease, &run).await;

    let (lease, run) = begin(&*hub.link).await;
    finish_run(&hub, &lease, &run).await;
    let sleeping = task(&hub, "T-0001").await;
    assert_eq!(sleeping.status, TaskStatus::Sleeping);
    // Nothing is set to wake it: it says so, and the lead is told once.
    assert_eq!(
        sleeping.waiting_reason.as_deref(),
        Some(threadlane_store::UNWOKEN_REASON)
    );
    let notices = task_results(&hub).await;
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].payload["status"], "sleeping");
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(
        run.lane_kind,
        LaneKind::Lead,
        "the lead is shown the notice"
    );
    assert_eq!(run.events, vec![notices[0].id]);
    finish_run(&hub, &lease, &run).await;

    let lane_id = task(&hub, "T-0001").await.lane_id;
    hub.insert(|event| {
        event.lane_id = lane_id;
        event.requested = false;
    })
    .await;
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Queued);
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Task, "requested, as the task is");
    let failure = RunFailure::Error {
        message: "the build broke".into(),
    };
    hub.link.fail(&lease, run.run_id, &failure).await.unwrap();
    hub.link.release(&lease).await.unwrap();
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Queued);

    // The retry is told to continue, not start over.
    hub.clock.advance(std::time::Duration::from_secs(60));
    let (_, retry) = begin(&*hub.link).await;
    assert_eq!(retry.lane_kind, LaneKind::Task);
    assert!(
        retry
            .prompt
            .contains("An earlier run on these events did not finish"),
        "{}",
        retry.prompt
    );
    assert!(!run.prompt.contains("An earlier run"), "{}", run.prompt);
}

#[tokio::test]
async fn a_failed_task_can_be_retried_with_its_last_error_and_runs_again() {
    let (hub, lease, run) = lead_run(2).await;
    call(
        &hub,
        &lease,
        &run,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lease, &run).await;
    let failure = RunFailure::Error {
        message: "the build broke".into(),
    };
    for attempt in 1..=5 {
        let (lease, run) = begin(&*hub.link).await;
        assert_eq!(run.lane_kind, LaneKind::Task);
        hub.link.fail(&lease, run.run_id, &failure).await.unwrap();
        hub.link.release(&lease).await.unwrap();
        if attempt < 5 {
            hub.clock.advance(std::time::Duration::from_secs(600));
        }
    }
    let failed = task(&hub, "T-0001").await;
    assert_eq!(failed.status, TaskStatus::Failed);
    let first_end = hub.db.task_completed_at(failed.id).await.unwrap().unwrap();

    let retried = hub
        .api
        .tasks(TaskRequest::Retry { task: failed.id })
        .await
        .unwrap();
    let TaskReply::Task { task: retried } = retried else {
        panic!("a task");
    };
    assert_eq!(retried.status, TaskStatus::Queued);
    assert_eq!(retried.waiting_reason, None);
    let again = hub.api.tasks(TaskRequest::Retry { task: failed.id }).await;
    assert!(
        matches!(&again, Err(ApiError::Invalid(why)) if why.contains("only a failed task")),
        "{again:?}"
    );

    let (lease, run) = loop {
        let (lease, run) = begin(&*hub.link).await;
        if run.lane_kind == LaneKind::Task {
            break (lease, run);
        }
        finish_run(&hub, &lease, &run).await;
    };
    assert_eq!(run.task_id, Some(failed.id));
    assert_eq!(run.events.len(), 1);
    let ask = hub.event(run.events[0]).await;
    assert_eq!(ask.kind, EventKind::UserMessage);
    assert_eq!(ask.trust, Trust::User);
    assert!(ask.requested);
    let text = ask.payload["text"].as_str().unwrap();
    assert!(text.contains("the build broke"), "{text}");
    assert!(text.contains("task branch"), "{text}");
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Running);

    // A second failure is reported to the lead again, and retried with it.
    for attempt in 1..=5 {
        let (lease, run) = if attempt == 1 {
            (lease, run.clone())
        } else {
            begin(&*hub.link).await
        };
        hub.link.fail(&lease, run.run_id, &failure).await.unwrap();
        hub.link.release(&lease).await.unwrap();
        hub.clock.advance(std::time::Duration::from_secs(600));
    }
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Failed);
    let failures = task_results(&hub)
        .await
        .into_iter()
        .filter(|result| result.payload["status"] == "failed")
        .count();
    assert_eq!(failures, 2);
    // The board dates it by the attempt that ended last.
    let second_end = hub.db.task_completed_at(failed.id).await.unwrap().unwrap();
    assert!(second_end > first_end, "{second_end:?} vs {first_end:?}");
}

#[tokio::test]
async fn only_a_failed_task_can_be_retried() {
    let (hub, lease, run) = lead_run(2).await;
    call(
        &hub,
        &lease,
        &run,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lease, &run).await;
    let queued = task(&hub, "T-0001").await;
    let refused = hub.api.tasks(TaskRequest::Retry { task: queued.id }).await;
    assert!(
        matches!(&refused, Err(ApiError::Invalid(why)) if why.contains("queued")),
        "{refused:?}"
    );
    let missing = hub.api.tasks(TaskRequest::Retry { task: TaskId(99) }).await;
    assert!(matches!(missing, Err(ApiError::NotFound(_))), "{missing:?}");
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Queued);
}

#[tokio::test]
async fn a_subtask_has_its_parent_and_inherits_its_origin() {
    let (hub, lease, run) = lead_run(2).await;
    call(
        &hub,
        &lease,
        &run,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lease, &run).await;
    let (lease, run) = begin(&*hub.link).await;
    let sub = call(
        &hub,
        &lease,
        &run,
        "toolu_s",
        CREATE_TASK,
        json!({ "title": "styles", "goal": "the page has styles" }),
    )
    .await
    .unwrap();
    assert_eq!(sub["parent_task_id"], "T-0001");
    let sub = task(&hub, sub["task_id"].as_str().unwrap()).await;
    let parent = task(&hub, "T-0001").await;
    assert_eq!(sub.parent_task_id, Some(parent.id));
    assert_eq!(sub.origin, parent.origin);
    assert_eq!(
        sub.agent_id, parent.agent_id,
        "the task's own agent by default"
    );
    assert_eq!(
        hub.db.lane(sub.lane_id).await.unwrap().unwrap().kind,
        LaneKind::Task
    );
}

#[tokio::test]
async fn the_lead_updates_lists_and_cancels_tasks_and_a_cancelled_task_runs_no_more() {
    let (hub, lease, run) = lead_run(2).await;
    call(
        &hub,
        &lease,
        &run,
        "toolu_1",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    call(
        &hub,
        &lease,
        &run,
        "toolu_2",
        CREATE_TASK,
        task_args("docs", &["T-0001"]),
    )
    .await
    .unwrap();
    let updated = call(
        &hub,
        &lease,
        &run,
        "toolu_3",
        UPDATE_TASK,
        json!({ "task_id": "T-0002", "goal": "the docs are published", "priority": 5 }),
    )
    .await
    .unwrap();
    assert_eq!(updated["title"], "docs");
    assert_eq!(updated["goal"], "the docs are published");
    assert_eq!(updated["priority"], 5);

    let listed = call(&hub, &lease, &run, "toolu_4", LIST_TASKS, json!({}))
        .await
        .unwrap();
    let listed = listed["tasks"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1]["task_id"], "T-0002");
    assert_eq!(listed[1]["agent"], ENGINEER);
    assert_eq!(listed[1]["depends_on"], json!(["T-0001"]));

    let cancelled = call(
        &hub,
        &lease,
        &run,
        "toolu_5",
        CANCEL_TASK,
        json!({ "task_id": 1 }),
    )
    .await
    .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    let again = call(
        &hub,
        &lease,
        &run,
        "toolu_6",
        CANCEL_TASK,
        json!({ "task_id": "T-0001" }),
    )
    .await
    .unwrap();
    assert_eq!(again["status"], "cancelled");
    let late = call(
        &hub,
        &lease,
        &run,
        "toolu_7",
        UPDATE_TASK,
        json!({ "task_id": 1, "title": "x" }),
    )
    .await;
    assert!(matches!(late, Err(ToolError::Failed(_))), "{late:?}");
    // The docs task waited for the page, which can now never be done: it
    // is cancelled with it, and says why.
    let results = task_results(&hub).await;
    assert_eq!(results.len(), 2, "one result for each ended task");
    assert_eq!(results[0].payload["task"], "T-0001");
    assert_eq!(results[0].payload["status"], "cancelled");
    assert_eq!(results[1].payload["task"], "T-0002");
    assert_eq!(results[1].payload["status"], "cancelled");
    assert_eq!(
        results[1].payload["reason"],
        "T-0001, which it depended on, was cancelled"
    );
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    // The lead's own lane picks the results up under the same claim.
    match hub.link.begin_run(&lease).await.unwrap() {
        Begin::Run(next) => {
            assert_eq!(next.events, vec![results[0].id, results[1].id]);
            hub.link.complete(&lease, next.run_id, None).await.unwrap();
        }
        Begin::Idle => panic!("the result is pending in the lead lane"),
    }
    assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
    hub.link.release(&lease).await.unwrap();

    // Each cancelled task's start event is skipped without a run.
    for id in ["T-0001", "T-0002"] {
        let cancelled = task(&hub, id).await;
        assert_eq!(cancelled.status, TaskStatus::Cancelled, "{id}");
        let lease = hub
            .link
            .request_work()
            .await
            .unwrap()
            .expect("granted to drain it");
        assert_eq!(lease.lane_id, cancelled.lane_id, "{id}");
        assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
        hub.link.release(&lease).await.unwrap();
        assert!(
            hub.db
                .pending_events(cancelled.lane_id)
                .await
                .unwrap()
                .is_empty()
        );
    }
    let skipped = hub.event(threadlane_core::ids::EventId(2)).await;
    assert_eq!(skipped.kind, EventKind::TaskStart);
    assert_eq!(skipped.status, EventStatus::Skipped);
    assert_eq!(skipped.status_reason.as_deref(), Some(ENDED_REASON));
    assert_eq!(hub.link.request_work().await.unwrap(), None);
}

#[tokio::test]
async fn a_dependency_and_the_parallel_limits_hold_tasks_back_with_their_reason() {
    // General runs two tasks at once; the engineer has no limit of its own.
    let (hub, lease, run) = lead_run(0).await;
    assert_eq!(hub.general().await.max_parallel_tasks, 2);
    for (n, (title, depends_on)) in [
        ("a", vec![]),
        ("b", vec![]),
        ("c", vec![]),
        ("d", vec!["T-0001"]),
    ]
    .into_iter()
    .enumerate()
    {
        call(
            &hub,
            &lease,
            &run,
            &format!("toolu_{n}"),
            CREATE_TASK,
            task_args(title, &depends_on),
        )
        .await
        .unwrap();
    }
    finish_run(&hub, &lease, &run).await;

    let (first, first_run) = begin(&*hub.link).await;
    let (_, second_run) = begin(&*hub.link).await;
    assert!(first_run.prompt.contains("T-0001") && second_run.prompt.contains("T-0002"));
    assert_eq!(
        hub.link.request_work().await.unwrap(),
        None,
        "the limit is reached"
    );
    assert_eq!(
        task(&hub, "T-0003").await.waiting_reason.as_deref(),
        Some("the project already runs its limit of 2 tasks at once")
    );
    assert_eq!(
        task(&hub, "T-0004").await.waiting_reason.as_deref(),
        Some("waiting for T-0001 to be done")
    );
    assert_eq!(task(&hub, "T-0003").await.status, TaskStatus::Queued);

    write_progress(&hub, &first, &first_run, "T-0001").await;
    call(
        &hub,
        &first,
        &first_run,
        "toolu_f",
        FINISH_TASK,
        json!({ "summary": "a" }),
    )
    .await
    .unwrap();
    finish_run(&hub, &first, &first_run).await;
    let (_, third_run) = begin(&*hub.link).await;
    assert!(
        third_run.prompt.contains("T-0003"),
        "the older task takes the slot"
    );
    assert_eq!(task(&hub, "T-0003").await.waiting_reason, None);
    assert_eq!(
        hub.link.request_work().await.unwrap(),
        None,
        "the lead also waits for a machine slot"
    );
    assert_eq!(
        task(&hub, "T-0004").await.waiting_reason.as_deref(),
        Some("the project already runs its limit of 2 tasks at once")
    );
}

#[tokio::test]
async fn one_agent_can_fill_machine_slots() {
    let (hub, lease, run) = lead_run(1).await;
    for n in 0..2 {
        call(
            &hub,
            &lease,
            &run,
            &format!("toolu_{n}"),
            CREATE_TASK,
            task_args(&format!("t{n}"), &[]),
        )
        .await
        .unwrap();
    }
    finish_run(&hub, &lease, &run).await;
    begin(&*hub.link).await;
    begin(&*hub.link).await;
    assert_eq!(hub.link.request_work().await.unwrap(), None);
}

#[tokio::test]
async fn a_task_nobody_asked_for_does_not_start_and_an_agent_set_to_off_never_runs() {
    // The engineer is On request; in Assisted or Autonomous such a task
    // starts (`initiative`).
    let hub = TestHub::start().await;
    hub.hello().await;
    let agent = engineer(&hub, 0).await;
    let general = hub.general().await;
    let new_task = |title: &str, origin| NewTask {
        project_id: general.id,
        agent_id: agent,
        title: title.into(),
        goal: "x".into(),
        repositories: Vec::new(),
        parent_task_id: None,
        depends_on: Vec::new(),
        origin,
        status: TaskStatus::Queued,
        priority: 0,
        created_by_call: None,
        created_at: hub.clock.now(),
    };
    let unasked = hub
        .db
        .create_task(None, &new_task("unasked", TaskOrigin::Agent))
        .await
        .unwrap();
    let lease = hub.link.request_work().await.unwrap().unwrap();
    assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
    hub.link.release(&lease).await.unwrap();
    let start = hub.event(threadlane_core::ids::EventId(1)).await;
    assert_eq!(start.lane_id, unasked.lane_id);
    assert_eq!(start.status_reason.as_deref(), Some(SKIP_REASON));

    hub.api
        .org(threadlane_core::client::OrgRequest::SetMode {
            project: None,
            agent: ENGINEER.into(),
            mode: Mode::Off,
        })
        .await
        .unwrap();
    let asked = hub
        .db
        .create_task(None, &new_task("asked", TaskOrigin::User))
        .await
        .unwrap();
    let lease = hub.link.request_work().await.unwrap().unwrap();
    assert_eq!(lease.lane_id, asked.lane_id);
    assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
    let start = hub.event(threadlane_core::ids::EventId(2)).await;
    assert_eq!(start.status, EventStatus::Skipped);
    assert_eq!(start.status_reason.as_deref(), Some(OFF_REASON));
    assert!(
        hub.db
            .deliverable(hub.clock.now(), 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn every_task_tool_refuses_a_stale_lease() {
    let (hub, stale, run) = lead_run(2).await;
    hub.clock.advance(PAST_LEASE);
    hub.core.sweep_once().await.unwrap();
    begin(&*hub.link).await;
    for (tool, arguments) in [
        (CREATE_TASK, task_args("late", &[])),
        (LIST_TASKS, json!({})),
        (CANCEL_TASK, json!({ "task_id": "T-0001" })),
        (UPDATE_TASK, json!({ "task_id": "T-0001" })),
    ] {
        let result = call(&hub, &stale, &run, "toolu_1", tool, arguments).await;
        assert_eq!(result, Err(ToolError::LeaseLost), "{tool}");
    }
    assert!(tasks(&hub).await.is_empty());
}

#[tokio::test]
async fn a_task_names_repositories_registered_in_its_project_and_runs_with_the_deny_rules() {
    let (hub, lease, run) = lead_run(2).await;
    assert!(run.deny_rules.is_empty(), "a lead has no shell to refuse");
    assert!(run.repositories.is_empty());
    assert_eq!(run.task_id, None);
    let general = hub.general().await;
    let app = hub
        .db
        .create_repository(&threadlane_core::model::NewRepository {
            project_id: general.id,
            name: "app".into(),
            remote_url: "/repos/app.git".into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        })
        .await
        .unwrap();
    let elsewhere = hub
        .db
        .create_project(&threadlane_core::model::NewProject {
            name: "Elsewhere".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    hub.db
        .create_repository(&threadlane_core::model::NewRepository {
            project_id: elsewhere.id,
            name: "other".into(),
            remote_url: "/repos/other.git".into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        })
        .await
        .unwrap();

    // Invariant 19: only a repository of the run's own project.
    let mut args = task_args("page", &[]);
    args["repositories"] = json!(["other"]);
    let refused = call(&hub, &lease, &run, "toolu_1", CREATE_TASK, args).await;
    assert_eq!(
        refused,
        Err(ToolError::InvalidArguments(
            "no repository \"other\" in this project; it has app".into()
        ))
    );
    assert!(tasks(&hub).await.is_empty(), "nothing was created");

    let mut args = task_args("page", &[]);
    args["repositories"] = json!(["app"]);
    call(&hub, &lease, &run, "toolu_2", CREATE_TASK, args)
        .await
        .unwrap();
    assert_eq!(task(&hub, "T-0001").await.repositories, vec![app.id]);
    finish_run(&hub, &lease, &run).await;

    let (lease, task_run) = begin(&*hub.link).await;
    assert_eq!(task_run.lane_kind, LaneKind::Task);
    assert_eq!(task_run.task_id, Some(TaskId(1)));
    assert_eq!(task_run.repositories, vec![app]);
    assert!(!task_run.remove_builtin_tools, "full access");
    assert_eq!(
        task_run.deny_rules,
        threadlane_core::backend::DENY_RULES
            .iter()
            .map(|rule| (*rule).to_owned())
            .collect::<Vec<_>>()
    );
    assert!(
        task_run
            .appended_instructions
            .contains("Do not run git push or a gh command that changes GitHub yourself"),
        "layer 3"
    );

    // A subtask works in its parent's repositories unless it names others.
    let created = call(
        &hub,
        &lease,
        &task_run,
        "toolu_3",
        CREATE_TASK,
        json!({ "title": "part", "goal": "part is done" }),
    )
    .await
    .unwrap();
    assert_eq!(created["task_id"], "T-0002");
    assert_eq!(
        task(&hub, "T-0002").await.repositories,
        task(&hub, "T-0001").await.repositories
    );
}

#[tokio::test]
async fn the_lead_can_attach_repositories_and_wake_a_sleeping_task() {
    let (hub, lease, lead_run) = lead_run(2).await;
    let general = hub.general().await;
    let app = hub
        .db
        .create_repository(&threadlane_core::model::NewRepository {
            project_id: general.id,
            name: "app".into(),
            remote_url: "https://github.com/acme/app.git".into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        })
        .await
        .unwrap();
    call(
        &hub,
        &lease,
        &lead_run,
        "create",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lease, &lead_run).await;

    let (lease, run) = begin(&*hub.link).await;
    assert!(run.repositories.is_empty());
    finish_run(&hub, &lease, &run).await;
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Sleeping);

    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Lead);
    let arguments = json!({
        "task_id": "T-0001",
        "repositories": ["app", "app"],
        "wake": "Verify the existing PR head and resume the fix."
    });
    for _ in 0..2 {
        let updated = call(
            &hub,
            &lease,
            &run,
            "recover",
            UPDATE_TASK,
            arguments.clone(),
        )
        .await
        .unwrap();
        assert_eq!(updated["status"], "queued");
    }
    let recovered = task(&hub, "T-0001").await;
    assert_eq!(recovered.repositories, [app.id]);
    assert_eq!(recovered.waiting_reason, None);
    assert_eq!(
        hub.db
            .pending_events(recovered.lane_id)
            .await
            .unwrap()
            .len(),
        1
    );
    finish_run(&hub, &lease, &run).await;

    let (_, resumed) = begin(&*hub.link).await;
    assert_eq!(resumed.repositories, [app]);
    assert!(
        resumed
            .prompt
            .contains("Verify the existing PR head and resume the fix.")
    );
}

#[tokio::test]
async fn the_user_can_attach_a_repository_to_a_task_that_has_not_ended() {
    let (hub, lease, lead_run) = lead_run(2).await;
    let general = hub.general().await;
    let app = hub
        .db
        .create_repository(&threadlane_core::model::NewRepository {
            project_id: general.id,
            name: "app".into(),
            remote_url: "https://github.com/acme/app.git".into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        })
        .await
        .unwrap();
    call(
        &hub,
        &lease,
        &lead_run,
        "create",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lease, &lead_run).await;
    let (lease, run) = begin(&*hub.link).await;
    assert!(run.repositories.is_empty());
    // Publishing with no repository leaves a blocker that outlasts the run (#625).
    let refused = call(
        &hub,
        &lease,
        &run,
        "publish",
        PUBLISH_BRANCH,
        json!({"repository": "app"}),
    )
    .await;
    assert!(
        matches!(&refused, Err(ToolError::Blocked(why)) if why.contains("Attach app")),
        "{refused:?}"
    );
    write_progress(&hub, &lease, &run, "T-0001").await;
    finish_run(&hub, &lease, &run).await;
    let sleeping = task(&hub, "T-0001").await;
    assert_eq!(sleeping.status, TaskStatus::Sleeping);
    let blocker = sleeping.publication_blocker.as_deref().unwrap();
    assert!(blocker.starts_with("Cannot publish: no repository is attached"));
    while let Some(lane) = hub.db.ready_lanes(hub.clock.now()).await.unwrap().first() {
        assert_ne!(*lane, sleeping.lane_id);
        let (lease, run) = begin(&*hub.link).await;
        finish_run(&hub, &lease, &run).await;
    }

    let attach = |names: &[&str]| TaskRequest::AttachRepositories {
        task: sleeping.id,
        repositories: names.iter().map(|name| (*name).to_owned()).collect(),
    };
    let unknown = hub.api.tasks(attach(&["other"])).await;
    assert!(
        matches!(&unknown, Err(ApiError::Invalid(why)) if why.contains("no repository")),
        "{unknown:?}"
    );
    let TaskReply::Task { task: attached } = hub.api.tasks(attach(&["app", "app"])).await.unwrap()
    else {
        panic!("a task");
    };
    assert_eq!(attached.repositories, [app.id]);
    assert_eq!(attached.publication_blocker, None);
    let again = hub.api.tasks(attach(&["app"])).await;
    assert!(
        matches!(&again, Err(ApiError::Invalid(why)) if why.contains("already")),
        "{again:?}"
    );

    // Attaching queues no run: the paused task stays as it was (#625).
    let paused = task(&hub, "T-0001").await;
    assert_eq!(paused.status, TaskStatus::Sleeping);
    assert!(
        hub.db
            .pending_events(paused.lane_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        hub.db
            .ready_lanes(hub.clock.now())
            .await
            .unwrap()
            .is_empty()
    );

    hub.api
        .tasks(TaskRequest::Cancel { task: sleeping.id })
        .await
        .unwrap();
    let ended = hub.api.tasks(attach(&["app"])).await;
    assert!(
        matches!(&ended, Err(ApiError::Invalid(why)) if why.contains("is cancelled")),
        "{ended:?}"
    );
}

#[tokio::test]
async fn a_lead_wake_survives_the_active_implementation_finishing() {
    let (hub, lead, run) = lead_run(2).await;
    call(
        &hub,
        &lead,
        &run,
        "create",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lead, &run).await;

    let (lease, implementation) = begin(&*hub.link).await;
    write_progress(&hub, &lease, &implementation, "T-0001").await;
    hub.send("m-2", "Add a correction to T-0001").await;
    let (lead, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Lead);
    let instruction = "Include the corrected total.";
    call(
        &hub,
        &lead,
        &run,
        "correct",
        UPDATE_TASK,
        json!({"task_id": "T-0001", "wake": instruction}),
    )
    .await
    .unwrap();
    finish_run(&hub, &lead, &run).await;

    call(
        &hub,
        &lease,
        &implementation,
        "first-result",
        FINISH_TASK,
        json!({"summary": "Original total"}),
    )
    .await
    .unwrap();
    hub.link
        .complete(&lease, implementation.run_id, None)
        .await
        .unwrap();

    let queued = task(&hub, "T-0001").await;
    assert_eq!(queued.status, TaskStatus::Queued);
    assert!(queued.result.is_none());
    assert!(task_results(&hub).await.is_empty());
    let Begin::Run(resumed) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("the pending wake should start a new implementation run");
    };
    assert!(resumed.prompt.contains(instruction));
    call(
        &hub,
        &lease,
        &resumed,
        "final-result",
        FINISH_TASK,
        json!({"summary": "Corrected total"}),
    )
    .await
    .unwrap();
    finish_run(&hub, &lease, &resumed).await;
    let done = task(&hub, "T-0001").await;
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(done.result.unwrap()["summary"], "Corrected total");
}

#[tokio::test]
async fn an_attachment_during_review_applies_to_the_next_implementation_run() {
    let (hub, lead, run) = lead_run(2).await;
    let general = hub.general().await;
    let reviewer = hub
        .db
        .create_agent(&NewAgent::named("Reviewer", "reviewer"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: general.id,
            agent_id: reviewer.id,
            is_lead: false,
            mode: Mode::OnRequest,
            max_parallel: 1,
        })
        .await
        .unwrap();
    let app = hub
        .db
        .create_repository(&threadlane_core::model::NewRepository {
            project_id: general.id,
            name: "app".into(),
            remote_url: "https://github.com/acme/app.git".into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        })
        .await
        .unwrap();
    call(
        &hub,
        &lead,
        &run,
        "create",
        CREATE_TASK,
        task_args("page", &[]),
    )
    .await
    .unwrap();
    finish_run(&hub, &lead, &run).await;

    let (lease, implementation) = begin(&*hub.link).await;
    write_progress(&hub, &lease, &implementation, "T-0001").await;
    let submitted = call(
        &hub,
        &lease,
        &implementation,
        "submit",
        FINISH_TASK,
        json!({
            "summary": "Local fix"
        }),
    )
    .await
    .unwrap();
    assert_eq!(submitted["submitted_for_review"], true);
    hub.link
        .complete(&lease, implementation.run_id, None)
        .await
        .unwrap();

    hub.send("m-2", "Attach app and continue T-0001").await;
    let (lead, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Lead);
    call(
        &hub,
        &lead,
        &run,
        "attach",
        UPDATE_TASK,
        json!({
            "task_id": "T-0001", "repositories": ["app"], "wake": "Port the fix into app."
        }),
    )
    .await
    .unwrap();
    finish_run(&hub, &lead, &run).await;

    let Begin::Run(review) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("submitted work should still be reviewable");
    };
    assert!(review.review.is_some());
    assert!(
        review.repositories.is_empty(),
        "the submission had no attached repos"
    );
    call(
        &hub,
        &lease,
        &review,
        "verdict",
        "finish_review",
        json!({
            "verdict": "approved", "feedback": "The local fix is correct.", "checks": "rerun"
        }),
    )
    .await
    .unwrap();
    hub.link
        .complete(&lease, review.run_id, None)
        .await
        .unwrap();
    assert_eq!(task(&hub, "T-0001").await.status, TaskStatus::Queued);

    let Begin::Run(resumed) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("the lead's pending instruction should resume implementation");
    };
    assert!(resumed.review.is_none());
    assert_eq!(resumed.repositories, [app]);
    assert!(resumed.prompt.contains("Port the fix into app."));
}

/// Plan.md §5.4 item 5: a task lane that works in repositories is not
/// granted to a machine that is not logged in to `gh`, and the task says
/// why; a task without repositories is not held back by it.
#[tokio::test]
async fn a_task_with_repositories_waits_for_a_machine_logged_in_to_gh() {
    let (hub, lease, run) = lead_run(2).await;
    let general = hub.general().await;
    hub.db
        .create_repository(&threadlane_core::model::NewRepository {
            project_id: general.id,
            name: "app".into(),
            remote_url: "/repos/app.git".into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        })
        .await
        .unwrap();
    let mut args = task_args("page", &[]);
    args["repositories"] = json!(["app"]);
    call(&hub, &lease, &run, "toolu_1", CREATE_TASK, args)
        .await
        .unwrap();
    finish_run(&hub, &lease, &run).await;

    let logged_out = threadlane_core::model::MachineReport {
        gh_login: false,
        ..usable_report()
    };
    hub.link.hello(&logged_out).await.unwrap();
    assert_eq!(hub.link.request_work().await.unwrap(), None);
    let waiting = task(&hub, "T-0001").await;
    assert_eq!(waiting.status, TaskStatus::Queued);
    assert_eq!(
        waiting.waiting_reason.as_deref(),
        Some(threadlane_store::NO_GH_REASON)
    );

    hub.hello().await;
    let (_, task_run) = begin(&*hub.link).await;
    assert_eq!(task_run.task_id, Some(TaskId(1)));
    assert_eq!(task(&hub, "T-0001").await.waiting_reason, None);
}

#[tokio::test]
async fn list_tasks_lists_unfinished_and_recent_tasks_in_pages_and_one_in_full() {
    let (hub, lease, run) = lead_run(2).await;
    for n in 1..=14 {
        call(
            &hub,
            &lease,
            &run,
            &format!("create-{n}"),
            CREATE_TASK,
            task_args(&format!("t{n}"), &[]),
        )
        .await
        .unwrap();
    }
    // T-0001 finishes last, so it is among the recent ones though created first.
    for n in (2..=13).chain([1]) {
        if n == 1 {
            hub.clock.advance(std::time::Duration::from_secs(60));
        }
        call(
            &hub,
            &lease,
            &run,
            &format!("cancel-{n}"),
            CANCEL_TASK,
            json!({ "task_id": n }),
        )
        .await
        .unwrap();
    }
    let list =
        |id: &'static str, arguments: Json| call(&hub, &lease, &run, id, LIST_TASKS, arguments);
    let ids = |listed: &Json| -> Vec<String> {
        listed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| task["task_id"].as_str().unwrap().to_owned())
            .collect()
    };

    // The default: the 10 most recently finished tasks and the unfinished one.
    let listed = list("l-1", json!({})).await.unwrap();
    let expected: Vec<String> = [1]
        .into_iter()
        .chain(5..=14)
        .map(|n| format!("T-{n:04}"))
        .collect();
    assert_eq!(ids(&listed), expected);
    assert_eq!(listed["total"], 11);
    assert_eq!(listed.get("next_offset"), None);
    assert!(
        listed["hidden_finished"]
            .as_str()
            .unwrap()
            .contains("all: true")
    );
    assert_eq!(listed["tasks"][0].get("goal"), None);
    assert_eq!(listed["tasks"][0]["agent"], ENGINEER);

    let hidden = listed["hidden_finished"].as_str().unwrap();
    assert!(hidden.starts_with("3 older finished tasks"));

    // An offset past the end still returns the count of the filtered snapshot.
    let past_default = list("l-far", json!({ "offset": usize::MAX }))
        .await
        .unwrap();
    assert!(ids(&past_default).is_empty());
    assert_eq!(past_default["total"], 11);
    assert_eq!(past_default["hidden_finished"], listed["hidden_finished"]);

    // Pages of the full list, to the end.
    let first = list("l-2", json!({ "all": true, "limit": 5 }))
        .await
        .unwrap();
    assert_eq!(
        ids(&first),
        ["T-0001", "T-0002", "T-0003", "T-0004", "T-0005"]
    );
    assert_eq!(
        (first["total"].clone(), first["next_offset"].clone()),
        (json!(14), json!(5))
    );
    assert!(first["more"].as_str().unwrap().contains("offset 5"));
    let last = list("l-3", json!({ "all": true, "offset": 10, "limit": 5 }))
        .await
        .unwrap();
    assert_eq!(ids(&last), ["T-0011", "T-0012", "T-0013", "T-0014"]);
    assert_eq!((last.get("next_offset"), last.get("more")), (None, None));
    let past = list("l-4", json!({ "all": true, "offset": 14 }))
        .await
        .unwrap();
    assert!(ids(&past).is_empty());
    assert_eq!(past.get("hidden_finished"), None);

    let full = list("l-5", json!({ "task_id": "T-0001" })).await.unwrap();
    assert_eq!(full["goal"], "t1 is done");
    assert_eq!(full["agent"], ENGINEER);
    for bad in [
        json!({ "all": "yes" }),
        json!({ "limit": 0 }),
        json!({ "limit": 201 }),
        json!({ "offset": -1 }),
        json!({ "task_id": "T-0099" }),
    ] {
        assert!(
            matches!(
                list("l-bad", bad.clone()).await,
                Err(ToolError::InvalidArguments(_))
            ),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn list_tasks_continues_after_the_last_wire_entry_not_the_sql_limit() {
    let (hub, lease, run) = lead_run(2).await;
    let wide = "😀".repeat(196);
    for n in 1..=35 {
        call(
            &hub,
            &lease,
            &run,
            &format!("create-{n}"),
            CREATE_TASK,
            task_args(&format!("{n} {wide}"), &[]),
        )
        .await
        .unwrap();
    }
    let args = json!({ "all": true, "limit": 35 });
    let first = call(&hub, &lease, &run, "page-one", LIST_TASKS, args)
        .await
        .unwrap();
    let len = first["tasks"].as_array().unwrap().len();
    assert!((1..35).contains(&len));
    assert_eq!(first["total"], 35);
    assert_eq!(first["next_offset"], len);
    let second = call(
        &hub,
        &lease,
        &run,
        "page-two",
        LIST_TASKS,
        json!({ "all": true, "limit": 35, "offset": len }),
    )
    .await
    .unwrap();
    let rows = second["tasks"].as_array().unwrap();
    assert_eq!(rows[0]["task_id"], format!("T-{:04}", len + 1));
    assert_eq!(len + rows.len(), 35);
    assert_eq!(second.get("next_offset"), None);
}

#[tokio::test]
async fn a_long_memory_file_is_viewed_in_pages_and_its_writer_is_warned() {
    let (hub, lease, run) = lead_run(1).await;
    let line = format!("{}\n", "word ".repeat(19));
    let long = format!("# Long\nA long note.\n{}", line.repeat(500));
    assert!(long.len() > 45_000);
    let written = call(
        &hub,
        &lease,
        &run,
        "w-1",
        MEMORY_CREATE,
        json!({ "path": "project/notes/long.md", "content": long }),
    )
    .await
    .unwrap();
    assert!(written["warning"].as_str().unwrap().contains("pages"));
    let small = call(
        &hub,
        &lease,
        &run,
        "w-2",
        MEMORY_CREATE,
        json!({ "path": "project/notes/small.md", "content": "# Small\nA small note." }),
    )
    .await
    .unwrap();
    assert_eq!(small.get("warning"), None);

    let mut read = String::new();
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let page = call(
            &hub,
            &lease,
            &run,
            &format!("v-{offset}"),
            MEMORY_VIEW,
            json!({ "path": "project/notes/long.md", "offset": offset }),
        )
        .await
        .unwrap();
        let content = page["content"].as_str().unwrap();
        assert!(content.chars().count() <= 20_000);
        assert!(page.to_string().len() < 25_000);
        assert_eq!(page["total"], long.chars().count());
        read.push_str(content);
        pages += 1;
        match page.get("next_offset") {
            Some(next) => {
                assert!(content.ends_with('\n'), "a page ends at a line break");
                assert!(page["more"].as_str().unwrap().contains("memory_view"));
                offset = usize::try_from(next.as_u64().unwrap()).unwrap();
            }
            None => break,
        }
    }
    assert_eq!(read, long);
    assert_eq!(pages, 3);

    let whole = call(
        &hub,
        &lease,
        &run,
        "v-small",
        MEMORY_VIEW,
        json!({ "path": "project/notes/small.md" }),
    )
    .await
    .unwrap();
    assert_eq!(whole["content"], "# Small\nA small note.");
    assert_eq!((whole.get("total"), whole.get("next_offset")), (None, None));
    let listed = call(
        &hub,
        &lease,
        &run,
        "v-dir",
        MEMORY_VIEW,
        json!({ "path": "project/notes", "limit": 1 }),
    )
    .await
    .unwrap();
    assert_eq!(listed["files"].as_array().unwrap().len(), 1);
    assert_eq!(listed["next_offset"], 1);
}
