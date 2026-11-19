//! The Overview capability (Plan.md §11.1, §11.2): tasks, the running list,
//! one task in full, the conversation history, recent items, machines.

use std::time::Duration;

use serde_json::json;
use threadlane_core::client::{
    ApiError, ConversationEntry, HeldResults, LaneTarget, OrgRequest, OverviewReply,
    OverviewRequest,
};
use threadlane_core::ids::{LaneId, ProjectId, TaskId};
use threadlane_core::model::{
    JournalEntry, LaneKind, Mode, NewAgent, NewBinding, NewOutboxMessage, NewProject, NewTask,
    OutboxKind, OutboxMessage, RunOutcome, RunStatus, TaskOrigin, TaskStatus, Urgency,
};
use threadlane_core::store::{MachineStore, OrgStore, OutboxStore, TaskStore};
use threadlane_core::time::Clock;

use crate::common::{RATIONALE, Reply, TestHub, begin, play};

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
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Task);
    let started = hub.clock.now();
    hub.clock.advance(Duration::from_secs(5));
    let step_at = hub.clock.now();
    hub.link.append(&lease, &[
        JournalEntry { run_id: run.run_id, seq: 1, at: started, entry: json!({"type": "text", "text": "Checking the build.\nSecond line."}) },
        JournalEntry { run_id: run.run_id, seq: 2, at: step_at, entry: json!({"type": "tool_use", "name": "Bash", "input": {"command": "do work"}}) },
    ]).await.unwrap();
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
    assert!(tasks[1].latest_step.is_none());
    let OverviewReply::Work { overview: work } = overview(&hub, OverviewRequest::Work).await else {
        panic!("work");
    };
    assert_eq!(work.tasks, tasks);
    assert_eq!(work.tasks[1].task.depends_on, [fix.id]);
    assert_eq!(work.leads.len(), 1);
    assert_eq!(work.leads[0].project, "General");

    let latest = tasks[0].latest_step.as_ref().unwrap();
    assert_eq!(latest.kind, "tool_use");
    assert_eq!(latest.description, "Called Bash");
    assert_eq!(latest.at, step_at);

    let OverviewReply::Tasks { tasks: running } = overview(&hub, OverviewRequest::Running).await
    else {
        panic!("running")
    };
    assert_eq!(
        running.iter().map(|t| t.task.id).collect::<Vec<_>>(),
        [fix.id]
    );
    assert_eq!(running[0].latest_step.as_ref(), Some(latest));

    let OverviewReply::Task { task } = overview(&hub, OverviewRequest::Task { task: fix.id }).await
    else {
        panic!("task")
    };
    assert_eq!(task.summary.task.id, fix.id);
    assert_eq!(task.branch, "threadlane/T-0001");
    assert_eq!(task.runs.len(), 1);
    assert_eq!(task.runs[0].run.status, RunStatus::Active);
    assert_eq!(task.runs[0].machine, machine);
    assert_eq!(task.summary.latest_step.as_ref(), Some(latest));
    assert_eq!(task.runs[0].steps[0].description, "Checking the build.");
    assert!(task.runs[0].usage.is_none());
    assert!(task.decisions.is_empty());
    assert!(!task.stopped);
    let page = i64::from(threadlane_core::client::CONVERSATION_PAGE);
    let events: Vec<_> = (3..=page + 2)
        .map(|seq| JournalEntry {
            run_id: run.run_id,
            seq,
            at: step_at,
            entry: json!({"type": "text", "text": format!("Step {seq}")}),
        })
        .collect();
    hub.link.append(&lease, &events).await.unwrap();

    let OverviewReply::Task { task: light } =
        overview(&hub, OverviewRequest::TaskOverview { task: fix.id }).await
    else {
        panic!("task")
    };
    assert_eq!(light.runs[0].steps.len(), 1);
    assert_eq!(
        light.summary.latest_step.unwrap().description,
        format!("Step {}", page + 2)
    );

    let OverviewReply::TaskActivity { entries, next } = overview(
        &hub,
        OverviewRequest::TaskActivity {
            task: fix.id,
            before: None,
        },
    )
    .await
    else {
        panic!("activity")
    };
    assert_eq!(entries.len(), page as usize);
    assert_eq!(entries.first().unwrap().position.seq, page + 2);
    assert_eq!(entries.last().unwrap().position.seq, 3);
    let OverviewReply::TaskActivity { entries, next } = overview(
        &hub,
        OverviewRequest::TaskActivity {
            task: fix.id,
            before: next,
        },
    )
    .await
    else {
        panic!("activity")
    };
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.position.seq)
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert!(next.is_none());
    hub.api
        .org(OrgRequest::Stop {
            target: LaneTarget::Task { task: fix.id },
            client_request_id: "stop-fix".to_owned(),
        })
        .await
        .unwrap();
    let OverviewReply::Task { task: stopped } =
        overview(&hub, OverviewRequest::Task { task: fix.id }).await
    else {
        panic!("task");
    };
    assert!(stopped.stopped);
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
async fn lead_says_since_when_the_lead_is_working() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let lead = |project: Option<&str>| {
        let api = hub.api.clone();
        let project = project.map(str::to_owned);
        async move { api.overview(OverviewRequest::Lead { project }).await }
    };
    assert_eq!(
        lead(None).await.unwrap(),
        OverviewReply::Lead {
            latest_step: None,
            running_since: None,
            stopped: false,
        },
        "idle"
    );

    hub.send("m-1", "Fix the refunds").await;
    let (_lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Lead);
    assert_eq!(
        lead(None).await.unwrap(),
        OverviewReply::Lead {
            latest_step: None,
            running_since: Some(hub.clock.now()),
            stopped: false,
        },
        "working"
    );
    hub.api
        .org(OrgRequest::Stop {
            target: LaneTarget::Lead { project: None },
            client_request_id: "stop-lead".to_owned(),
        })
        .await
        .unwrap();
    assert!(matches!(
        lead(None).await.unwrap(),
        OverviewReply::Lead { stopped: true, .. }
    ));
    hub.send("m-2", "Resume the lead").await;
    assert!(matches!(
        lead(None).await.unwrap(),
        OverviewReply::Lead { stopped: false, .. }
    ));
    assert!(matches!(
        lead(Some("missing")).await,
        Err(ApiError::NotFound(_))
    ));
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

#[tokio::test]
async fn lead_runs_return_rationale_and_whether_the_run_queued_a_message() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "Check the build").await;
    let quiet = play(&*hub.link, Reply::Silent)
        .await
        .unwrap()
        .only_run()
        .run_id;
    hub.clock.advance(Duration::from_secs(60));
    hub.send("m-2", "Any news?").await;
    let replied = play(&*hub.link, Reply::Send(Some("toolu_1"), "On it"))
        .await
        .unwrap()
        .only_run()
        .run_id;
    hub.clock.advance(Duration::from_secs(60));
    hub.send("m-3", "And the docs?").await;
    let (_lease, active) = begin(&*hub.link).await;

    let request = OverviewRequest::LeadRuns { project: None };
    let OverviewReply::LeadRuns { runs } = overview(&hub, request).await else {
        panic!("lead runs");
    };
    assert_eq!(
        runs.iter().map(|entry| entry.run.id).collect::<Vec<_>>(),
        [quiet, replied, active.run_id]
    );
    assert_eq!(runs[0].run.outcome, Some(RunOutcome::Silent));
    assert_eq!(runs[0].run.rationale.as_deref(), Some(RATIONALE));
    assert!(!runs[0].sent_message);
    assert!(runs[1].sent_message, "the message is still in the outbox");
    assert_eq!(runs[2].run.status, RunStatus::Active);
    assert!(!runs[2].sent_message);
    assert!(matches!(
        hub.api
            .overview(OverviewRequest::LeadRuns {
                project: Some("missing".to_owned())
            })
            .await,
        Err(ApiError::NotFound(_))
    ));
}

#[tokio::test]
async fn done_task_summaries_report_when_the_hub_recorded_the_result() {
    let hub = TestHub::start().await;
    let project = hub.general().await;
    let lead = hub.db.lead_binding(project.id).await.unwrap().unwrap();
    let task = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: project.id,
                agent_id: lead.agent_id,
                title: "Update docs".to_owned(),
                goal: "Explain the new screen".to_owned(),
                repositories: Vec::new(),
                parent_task_id: None,
                depends_on: Vec::new(),
                origin: TaskOrigin::User,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    hub.clock.advance(Duration::from_secs(60));
    hub.db
        .set_task_status(task.id, TaskStatus::Running, None)
        .await
        .unwrap();
    hub.db
        .set_task_status(task.id, TaskStatus::Done, None)
        .await
        .unwrap();
    let OverviewReply::Tasks { tasks } =
        overview(&hub, OverviewRequest::Tasks { project: None }).await
    else {
        panic!("tasks");
    };
    assert_eq!(tasks[0].task.id, task.id);
    assert_ne!(tasks[0].task.created_at, hub.clock.now());
    assert_eq!(tasks[0].completed_at, Some(hub.clock.now()));
}

async fn outbox(
    hub: &TestHub,
    channel: &str,
    origin: Option<(ProjectId, LaneId)>,
    body: serde_json::Value,
) -> OutboxMessage {
    hub.db
        .enqueue(&NewOutboxMessage {
            project_id: origin.map(|(project, _)| project),
            lane_id: origin.map(|(_, lane)| lane),
            session_id: None,
            run_id: None,
            tool_call_id: None,
            kind: OutboxKind::Message,
            decision_id: None,
            channel: channel.to_owned(),
            body,
            urgency: Urgency::Low,
            not_before: None,
            created_at: hub.clock.now(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn stage_b_held_results_count_originals_and_ignore_retries_and_other_channels() {
    let hub = TestHub::start().await;
    let now = hub.clock.now();
    let next = now + Duration::from_secs(60);
    let latest = now + Duration::from_secs(120);
    assert_eq!(
        overview(&hub, OverviewRequest::HeldResults).await,
        OverviewReply::HeldResults { held: None }
    );

    let a = outbox(&hub, "client", None, json!({"text":"First"})).await;
    let b = outbox(&hub, "client", None, json!({"text":"Second"})).await;
    let mut draft = NewOutboxMessage {
        project_id: None,
        lane_id: None,
        session_id: None,
        run_id: None,
        tool_call_id: None,
        kind: OutboxKind::Message,
        decision_id: None,
        channel: "client".to_owned(),
        body: json!({"text":"Digest","digest":[a.id,b.id]}),
        urgency: Urgency::Normal,
        not_before: None,
        created_at: now,
    };
    let digest = hub.db.queue_digest(&draft, &[a.id, b.id]).await.unwrap();
    hub.db.hold_delivery(digest.id, next, "rate").await.unwrap();
    draft.body = json!({"text":"Third"});
    let third = hub.db.enqueue(&draft).await.unwrap();
    hub.db
        .hold_delivery(third.id, latest, "quiet")
        .await
        .unwrap();
    let retry = outbox(&hub, "client", None, json!({"text":"Retry"})).await;
    hub.db
        .delivery_failed(&retry, "Temporary error", now, Some(next))
        .await
        .unwrap();
    let external = outbox(&hub, "other", None, json!({"text":"Outside channel"})).await;
    hub.db
        .hold_delivery(external.id, next, "quiet")
        .await
        .unwrap();
    assert_eq!(
        overview(&hub, OverviewRequest::HeldResults).await,
        OverviewReply::HeldResults {
            held: Some(HeldResults {
                count: 3,
                next,
                latest
            })
        }
    );

    hub.clock.advance(Duration::from_secs(60));
    assert_eq!(
        overview(&hub, OverviewRequest::HeldResults).await,
        OverviewReply::HeldResults {
            held: Some(HeldResults {
                count: 1,
                next: latest,
                latest
            })
        }
    );
}

#[tokio::test]
async fn stage_b_conversations_share_the_digest_and_route_a_dead_delivery_to_its_origin() {
    let hub = TestHub::start().await;
    let general = hub.general().await;
    let api = hub
        .db
        .create_project(&NewProject {
            name: "API".to_owned(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    hub.db
        .create_project(&NewProject {
            name: "Other".to_owned(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let first = outbox(
        &hub,
        "client",
        Some((general.id, general.lead_lane_id)),
        json!({"text":"First"}),
    )
    .await;
    let second = outbox(
        &hub,
        "client",
        Some((api.id, api.lead_lane_id)),
        json!({"text":"Second"}),
    )
    .await;
    let digest = hub
        .db
        .queue_digest(
            &NewOutboxMessage {
                project_id: None,
                lane_id: None,
                session_id: None,
                run_id: None,
                tool_call_id: None,
                kind: OutboxKind::Message,
                decision_id: None,
                channel: "client".to_owned(),
                body: json!({"text":"Digest", "digest":[first.id, second.id]}),
                urgency: Urgency::Normal,
                not_before: None,
                created_at: hub.clock.now(),
            },
            &[first.id, second.id],
        )
        .await
        .unwrap();
    hub.db
        .mark_sent(digest.id, None, hub.clock.now())
        .await
        .unwrap();
    hub.clock.advance(Duration::from_secs(60));
    let dead = outbox(
        &hub,
        "client",
        Some((api.id, api.lead_lane_id)),
        json!({"text":"Undeliverable"}),
    )
    .await;
    hub.db
        .delivery_failed(&dead, "Adapter rejected the result", hub.clock.now(), None)
        .await
        .unwrap();

    let feed = hub.feed().await;
    assert_eq!(feed.len(), 1);
    assert_eq!(
        feed[0]
            .digest
            .iter()
            .map(|result| (result.project.as_deref(), result.text.as_str()))
            .collect::<Vec<_>>(),
        [(Some("General"), "First"), (Some("API"), "Second")]
    );
    assert_eq!(
        overview(&hub, OverviewRequest::Recent { limit: 10 }).await,
        OverviewReply::Recent {
            items: feed.clone()
        }
    );
    let history = |project: Option<&str>, before| {
        let request = OverviewRequest::Conversation {
            project: project.map(str::to_owned),
            before,
            limit: 10,
        };
        overview(&hub, request)
    };
    let delivered = ConversationEntry::Delivered {
        item: feed[0].clone(),
    };
    assert_eq!(
        history(None, None).await,
        OverviewReply::Conversation {
            entries: vec![delivered.clone()]
        }
    );
    assert_eq!(
        history(Some("Other"), None).await,
        OverviewReply::Conversation {
            entries: Vec::new()
        }
    );
    assert_eq!(
        history(Some("API"), Some(hub.clock.now())).await,
        OverviewReply::Conversation {
            entries: vec![delivered.clone()]
        }
    );
    let OverviewReply::Conversation { entries } = history(Some("API"), None).await else {
        panic!("conversation");
    };
    assert!(
        matches!(&entries[..], [item, ConversationEntry::DeliveryFailure { text, .. }]
        if *item == delivered && text.ends_with("Adapter rejected the result"))
    );
}

#[tokio::test]
async fn overview_lead_exposes_the_latest_step_then_backend_usage_after_completion() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("usage-1", "Check the code").await;
    let (lease, run) = begin(&*hub.link).await;
    let at = hub.clock.now();
    hub.link
        .append(
            &lease,
            &[JournalEntry {
                run_id: run.run_id,
                seq: 1,
                at,
                entry: json!({"type": "thinking"}),
            }],
        )
        .await
        .unwrap();
    let OverviewReply::Lead {
        latest_step: Some(step),
        ..
    } = overview(&hub, OverviewRequest::Lead { project: None }).await
    else {
        panic!("active lead")
    };
    assert_eq!(
        (step.kind.as_str(), step.description.as_str(), step.at),
        ("thinking", "Thinking", at)
    );
    hub.link
        .append(
            &lease,
            &[JournalEntry {
                run_id: run.run_id,
                seq: 2,
                at,
                entry: json!({"type": "finished", "usage": {
                    "usage": {"input_tokens": 21, "output_tokens": 13}, "total_cost_usd": 0.0123,
                }}),
            }],
        )
        .await
        .unwrap();
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    let OverviewReply::LeadRuns { runs } =
        overview(&hub, OverviewRequest::LeadRuns { project: None }).await
    else {
        panic!("lead runs")
    };
    assert!(runs[0].latest_step.is_none());
    let usage = runs[0].usage.as_ref().expect("reported usage");
    assert_eq!(usage.input_tokens, Some(21));
    assert_eq!(usage.output_tokens, Some(13));
    assert_eq!(usage.cost_usd, Some(0.0123));
}
