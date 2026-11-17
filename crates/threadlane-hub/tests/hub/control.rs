//! What you start and stop yourself from a client (Plan.md §5.2, §11.2):
//! a task created skipping the lead, cancelling one, pausing a project, and
//! the events recorded but acted on by nobody.

use threadlane_core::client::{
    ApiError, OrgReply, OrgRequest, OverviewReply, OverviewRequest, TaskReply, TaskRequest,
};
use threadlane_core::ids::TaskId;
use threadlane_core::model::{EventKind, EventStatus, Mode, Task, TaskOrigin, TaskStatus};
use threadlane_core::store::InboxStore;
use threadlane_hub::{OFF_REASON, SKIP_REASON};

use crate::common::{Reply, TestHub, play};

/// A hub with the agent Reviewer bound On request in General, and the
/// repository `api` registered there.
async fn hub_with_reviewer() -> TestHub {
    let hub = TestHub::start().await;
    for request in [
        OrgRequest::CreateAgent {
            name: "Reviewer".into(),
            role: "reviewer".into(),
            instructions: String::new(),
            backends: Vec::new(),
        },
        OrgRequest::SetMode {
            project: None,
            agent: "Reviewer".into(),
            mode: Mode::OnRequest,
        },
        OrgRequest::AddRepository {
            project: None,
            name: "api".into(),
            remote_url: "git@example.com:me/api.git".into(),
            default_branch: None,
        },
    ] {
        hub.api.org(request).await.unwrap();
    }
    hub
}

fn create(title: &str, depends_on: Vec<TaskId>, id: &str) -> TaskRequest {
    TaskRequest::Create {
        project: None,
        title: title.into(),
        goal: "Make it so.".into(),
        agent: "Reviewer".into(),
        repositories: vec!["api".into()],
        depends_on,
        client_request_id: id.into(),
    }
}

async fn task(hub: &TestHub, request: TaskRequest) -> Result<Task, ApiError> {
    match hub.api.tasks(request).await? {
        TaskReply::Task { task } => Ok(task),
        other => panic!("expected a task, got {other:?}"),
    }
}

#[tokio::test]
async fn a_task_you_create_is_queued_requested_work_once_per_request_id() {
    let hub = hub_with_reviewer().await;
    let created = task(&hub, create(" Review the API ", Vec::new(), "c-1"))
        .await
        .unwrap();
    assert_eq!(created.title, "Review the API");
    assert_eq!(created.status, TaskStatus::Queued);
    assert_eq!(created.origin, TaskOrigin::User, "your task is requested");
    assert_eq!(created.repositories.len(), 1);
    assert_eq!(created.parent_task_id, None, "the lead made none of it");
    let start = hub.db.pending_events(created.lane_id).await.unwrap();
    assert_eq!(start.len(), 1);
    assert_eq!(start[0].kind, EventKind::TaskStart);

    let again = task(&hub, create("Review the API", Vec::new(), "c-1"))
        .await
        .unwrap();
    assert_eq!(again.id, created.id, "a retried request creates nothing");
    let second = task(&hub, create("Then the docs", vec![created.id], "c-2"))
        .await
        .unwrap();
    assert_eq!(second.depends_on, [created.id]);
}

#[tokio::test]
async fn a_task_needs_a_bound_agent_that_is_on_and_what_the_project_has() {
    let hub = hub_with_reviewer().await;
    let cancelled = task(&hub, create("Doomed", Vec::new(), "c-0"))
        .await
        .unwrap();
    task(&hub, TaskRequest::Cancel { task: cancelled.id })
        .await
        .unwrap();
    hub.api
        .org(OrgRequest::CreateAgent {
            name: "Loner".into(),
            role: "tester".into(),
            instructions: String::new(),
            backends: Vec::new(),
        })
        .await
        .unwrap();
    let with = |change: fn(&mut TaskRequest)| {
        let mut request = create("Review", Vec::new(), "c-1");
        change(&mut request);
        request
    };
    let refused = [
        with(|request| {
            if let TaskRequest::Create { title, .. } = request {
                *title = "  ".into();
            }
        }),
        with(|request| {
            if let TaskRequest::Create { agent, .. } = request {
                *agent = "Loner".into();
            }
        }),
        with(|request| {
            if let TaskRequest::Create { repositories, .. } = request {
                *repositories = vec!["web".into()];
            }
        }),
        with(|request| {
            if let TaskRequest::Create { depends_on, .. } = request {
                *depends_on = vec![TaskId(99)];
            }
        }),
        create("Review", vec![cancelled.id], "c-1"),
    ];
    for request in refused {
        let outcome = task(&hub, request.clone()).await;
        assert!(
            matches!(outcome, Err(ApiError::Invalid(_))),
            "{request:?}: {outcome:?}"
        );
    }
    hub.api
        .org(OrgRequest::SetMode {
            project: None,
            agent: "Reviewer".into(),
            mode: Mode::Off,
        })
        .await
        .unwrap();
    let off = task(&hub, create("Review", Vec::new(), "c-1")).await;
    assert!(
        matches!(&off, Err(ApiError::Invalid(why)) if why.contains("off")),
        "{off:?}"
    );
    let missing = task(&hub, TaskRequest::Cancel { task: TaskId(99) }).await;
    assert!(matches!(missing, Err(ApiError::NotFound(_))), "{missing:?}");
}

#[tokio::test]
async fn cancelling_a_task_cancels_what_waits_on_it_and_tells_the_lead_once() {
    let hub = hub_with_reviewer().await;
    let first = task(&hub, create("First", Vec::new(), "c-1"))
        .await
        .unwrap();
    let second = task(&hub, create("Second", vec![first.id], "c-2"))
        .await
        .unwrap();
    let cancelled = task(&hub, TaskRequest::Cancel { task: first.id })
        .await
        .unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    let again = task(&hub, TaskRequest::Cancel { task: first.id })
        .await
        .unwrap();
    assert_eq!(again.status, TaskStatus::Cancelled, "a second cancel is a no-op");
    let Ok(OverviewReply::Task { task: detail }) = hub
        .api
        .overview(OverviewRequest::Task { task: second.id })
        .await
    else {
        panic!("listed");
    };
    assert_eq!(detail.summary.task.status, TaskStatus::Cancelled);

    let lead = hub.general().await.lead_lane_id;
    let results: Vec<String> = hub
        .db
        .pending_events(lead)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == EventKind::TaskResult)
        .map(|event| event.source_event_id)
        .collect();
    assert_eq!(
        results,
        [
            format!("result:{}", first.id.get()),
            format!("result:{}", second.id.get())
        ]
    );
}

#[tokio::test]
async fn a_paused_project_is_not_granted_until_resumed() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let pause = |paused| OrgRequest::SetPaused {
        project: None,
        paused,
    };
    let Ok(OrgReply::Project { project }) = hub.api.org(pause(true)).await else {
        panic!("paused");
    };
    assert!(project.paused);
    assert!(hub.api.projects().await.unwrap()[0].paused);
    hub.send("m-1", "while paused").await;
    assert!(hub.link.request_work().await.unwrap().is_none());

    let Ok(OrgReply::Project { project }) = hub.api.org(pause(false)).await else {
        panic!("resumed");
    };
    assert!(!project.paused);
    let played = play(&*hub.link, Reply::Silent).await.unwrap();
    assert_eq!(played.runs.len(), 1, "it runs once resumed");
    let missing = hub
        .api
        .org(OrgRequest::SetPaused {
            project: Some("Nowhere".into()),
            paused: true,
        })
        .await;
    assert!(matches!(missing, Err(ApiError::NotFound(_))), "{missing:?}");
}

#[tokio::test]
async fn events_nobody_asked_for_are_listed_as_recorded_and_counted() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let unasked = hub
        .insert(|event| {
            event.requested = false;
            event.source = "github".into();
            event.payload = serde_json::json!({ "text": "review requested on #4" });
        })
        .await;
    let lease = hub.link.request_work().await.unwrap().expect("granted");
    hub.link.begin_run(&lease).await.unwrap();
    hub.link.release(&lease).await.unwrap();
    let event = hub.event(unasked).await;
    assert_eq!(event.status, EventStatus::Skipped);
    assert_eq!(event.status_reason.as_deref(), Some(SKIP_REASON));
    assert_ne!(SKIP_REASON, OFF_REASON);
    // Asked-for work runs and is not listed.
    hub.send("m-1", "do this").await;
    play(&*hub.link, Reply::Silent).await.unwrap();

    let reply = hub
        .api
        .overview(OverviewRequest::Recorded {
            project: None,
            limit: 10,
        })
        .await
        .unwrap();
    let OverviewReply::Recorded { events, total } = reply else {
        panic!("expected recorded events, got {reply:?}");
    };
    assert_eq!(total, 1);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, unasked);
    assert_eq!(events[0].source, "github");
    assert_eq!(events[0].text, "review requested on #4");
    assert_eq!(
        hub.api.overview(OverviewRequest::RecordedCount).await,
        Ok(OverviewReply::RecordedCount { total: 1 })
    );
    // Only the newest `limit` are listed; the total still counts all.
    let Ok(OverviewReply::Recorded { events, total }) = hub
        .api
        .overview(OverviewRequest::Recorded {
            project: None,
            limit: 0,
        })
        .await
    else {
        panic!("listed");
    };
    assert!(events.is_empty());
    assert_eq!(total, 1);
}
