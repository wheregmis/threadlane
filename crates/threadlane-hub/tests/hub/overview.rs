//! The Overview capability (Plan.md §11.1, §11.2): tasks, the running list,
//! one task in full, the conversation history, recent items, machines.

use std::time::Duration;

use threadlane_core::client::{ApiError, ConversationEntry, OverviewReply, OverviewRequest};
use threadlane_core::ids::TaskId;
use threadlane_core::model::{
    LaneKind, Mode, NewAgent, NewBinding, NewTask, RunStatus, TaskOrigin, TaskStatus,
};
use threadlane_core::store::{MachineStore, OrgStore, TaskStore};
use threadlane_core::time::Clock;

use crate::common::{Reply, TestHub, begin, play};

async fn overview(hub: &TestHub, request: OverviewRequest) -> OverviewReply {
    hub.api.overview(request).await.unwrap()
}

#[tokio::test]
async fn tasks_the_running_list_and_one_task_show_their_agent_machine_and_runs() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let general = hub.general().await;
    let agent = hub
        .db
        .create_agent(&NewAgent::named("Engineer", "engineer"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: general.id,
            agent_id: agent.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel: 2,
        })
        .await
        .unwrap();
    let new = |title: &str, depends_on| NewTask {
        project_id: general.id,
        agent_id: agent.id,
        title: title.to_owned(),
        goal: String::new(),
        repositories: Vec::new(),
        parent_task_id: None,
        depends_on,
        origin: TaskOrigin::User,
        status: TaskStatus::Queued,
        priority: 0,
        created_by_call: None,
        created_at: hub.clock.now(),
    };
    let fix = hub.db.create_task(None, &new("Fix", vec![])).await.unwrap();
    let docs = hub
        .db
        .create_task(None, &new("Docs", vec![fix.id]))
        .await
        .unwrap();
    let (_lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Task);
    let started = hub.clock.now();
    let machine = hub.db.machines().await.unwrap()[0].name.clone();

    let OverviewReply::Tasks { tasks } =
        overview(&hub, OverviewRequest::Tasks { project: None }).await
    else {
        panic!("tasks")
    };
    let shown: Vec<_> = tasks
        .iter()
        .map(|t| {
            (
                t.task.id,
                t.task.status,
                t.agent.as_str(),
                t.machine.clone(),
                t.running_since,
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (
                fix.id,
                TaskStatus::Running,
                "Engineer",
                Some(machine.clone()),
                Some(started)
            ),
            (docs.id, TaskStatus::Queued, "Engineer", None, None),
        ]
    );
    assert_eq!(tasks[0].project, "General");

    let OverviewReply::Tasks { tasks: running } = overview(&hub, OverviewRequest::Running).await
    else {
        panic!("running")
    };
    assert_eq!(
        running.iter().map(|t| t.task.id).collect::<Vec<_>>(),
        [fix.id]
    );

    let OverviewReply::Task { task } = overview(&hub, OverviewRequest::Task { task: fix.id }).await
    else {
        panic!("task")
    };
    assert_eq!(task.summary.task.id, fix.id);
    assert_eq!(task.branch, "threadlane/T-0001");
    assert_eq!(task.runs.len(), 1);
    assert_eq!(task.runs[0].run.status, RunStatus::Active);
    assert_eq!(task.runs[0].machine, machine);
    assert!(task.decisions.is_empty());
    assert_eq!(
        task.related.iter().map(|t| t.task.id).collect::<Vec<_>>(),
        [docs.id],
        "what depends on it"
    );
    let OverviewReply::Task { task } =
        overview(&hub, OverviewRequest::Task { task: docs.id }).await
    else {
        panic!("task")
    };
    assert_eq!(
        task.related.iter().map(|t| t.task.id).collect::<Vec<_>>(),
        [fix.id],
        "what it depends on"
    );

    assert!(matches!(
        hub.api
            .overview(OverviewRequest::Task { task: TaskId(99) })
            .await,
        Err(ApiError::NotFound(_))
    ));
}

#[tokio::test]
async fn the_conversation_merges_your_messages_and_the_lead_s_in_time_order() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let first = hub.send("m-1", "Fix the refunds").await;
    hub.clock.advance(Duration::from_secs(60));
    play(&*hub.link, Reply::Send(Some("toolu_1"), "On it"))
        .await
        .unwrap();
    hub.core.dispatch_once().await.unwrap();
    hub.clock.advance(Duration::from_secs(60));
    let second = hub.send("m-2", "And the changelog").await;
    let last_sent = hub.clock.now();

    let conversation = |before, limit| {
        let api = hub.api.clone();
        async move {
            let request = OverviewRequest::Conversation {
                project: None,
                before,
                limit,
            };
            match api.overview(request).await.unwrap() {
                OverviewReply::Conversation { entries } => entries
                    .into_iter()
                    .map(|entry| match entry {
                        ConversationEntry::You { event_id, text, .. } => (Some(event_id), text),
                        ConversationEntry::Delivered { item } => {
                            (None, item.message.body["text"].as_str().unwrap().to_owned())
                        }
                        _ => panic!("unknown entry"),
                    })
                    .collect::<Vec<_>>(),
                other => panic!("{other:?}"),
            }
        }
    };
    assert_eq!(
        conversation(None, 10).await,
        [
            (Some(first.event_id), "Fix the refunds".to_owned()),
            (None, "On it".to_owned()),
            (Some(second.event_id), "And the changelog".to_owned()),
        ]
    );
    assert_eq!(
        conversation(None, 1).await,
        [(Some(second.event_id), "And the changelog".to_owned())],
        "the newest page"
    );
    assert_eq!(
        conversation(Some(last_sent), 10).await,
        [
            (Some(first.event_id), "Fix the refunds".to_owned()),
            (None, "On it".to_owned()),
        ],
        "the page before"
    );
    assert!(matches!(
        hub.api
            .overview(OverviewRequest::Conversation {
                project: Some("missing".to_owned()),
                before: None,
                limit: 10,
            })
            .await,
        Err(ApiError::NotFound(_))
    ));

    let OverviewReply::Recent { items } =
        overview(&hub, OverviewRequest::Recent { limit: 10 }).await
    else {
        panic!("recent")
    };
    assert_eq!(items, hub.feed().await, "the same items as the feed");
}

#[tokio::test]
async fn machines_lists_every_machine_and_account() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let (_, _) = hub.other_machine("linux-box").await;
    let OverviewReply::Machines { machines, accounts } =
        overview(&hub, OverviewRequest::Machines).await
    else {
        panic!("machines")
    };
    assert_eq!(machines, hub.db.machines().await.unwrap());
    assert_eq!(machines.len(), 2);
    assert_eq!(accounts, hub.db.accounts().await.unwrap());
}
