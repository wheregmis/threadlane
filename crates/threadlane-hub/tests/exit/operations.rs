//! Operational requests through the real hub and scripted runner.

use std::time::Duration;

use serde_json::json;
use threadlane_core::client::{
    ApiError, ClientRequest, ClientResponse, LaneTarget, MemoryReply, MemoryRequest,
    OperationsReply, OperationsRequest, OrgRequest, dispatch,
};
use threadlane_core::ids::EventId;
use threadlane_core::model::{
    EventStatus, NewTimer, OutboxKind, Timer, TimerCategory, TimerCreator, ToolCallStatus,
};
use threadlane_core::store::{InboxStore, OrgStore, TimerStore};
use threadlane_core::time::Clock;
use threadlane_hub::{ASK_USER, HubSettings, SEND_MESSAGE};
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{Sim, World, reply_script};

pub(super) fn asking(script: Script, key: &str) -> Script {
    script
        .call(
            key,
            ASK_USER,
            json!({ "question": "Which colour?", "options": ["blue", "green"] }),
        )
        .finish("Asked.")
}

pub(super) async fn follow_up(sim: &Sim, project: Option<&str>, delay: Duration) -> Timer {
    let project = match project {
        Some(name) => sim
            .db()
            .projects()
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap(),
        None => sim.db().general_project().await.unwrap(),
    };
    sim.db()
        .create_timer(&NewTimer {
            project_id: project.id,
            lane_id: project.lead_lane_id,
            agent_id: None,
            task_ref: None,
            category: TimerCategory::FollowUp,
            due_at: sim.clock().now() + delay,
            recurrence: None,
            instruction: "A scheduled wake.".into(),
            created_by: TimerCreator::User,
            created_by_call: None,
            requested: true,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn trace_connects_oldest_event_run_tool_calls_decision_and_messages() {
    let sim = Sim::start().await;
    sim.backend().set_default(asking(
        Script::new().call("send", SEND_MESSAGE, json!({ "text": "I can help." })),
        "ask",
    ));
    let oldest = sim.send("first", "First message.").await;
    sim.clock().advance(Duration::from_millis(1));
    let later = sim.send("later", "A related message.").await;
    assert!(sim.work_once().await.unwrap());

    let root = sim.event(oldest).await;
    let other = sim.event(later).await;
    assert_ne!(root.trace_id, other.trace_id);
    let run = sim.committed_runs_with(oldest).await.remove(0);
    assert_eq!(
        sim.committed_runs_with(later).await.as_slice(),
        std::slice::from_ref(&run)
    );
    assert_eq!(run.trace_id, root.trace_id);

    let OperationsReply::Trace { trace } = sim
        .api
        .operations(OperationsRequest::Trace {
            id: root.trace_id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("expected trace");
    };
    assert_eq!(trace.events.as_slice(), std::slice::from_ref(&root));
    assert_eq!(trace.runs.as_slice(), std::slice::from_ref(&run));
    assert_eq!(trace.tool_calls.len(), 2);
    assert!(
        trace
            .tool_calls
            .iter()
            .all(|call| call.trace_id == root.trace_id
                && call.run_id == run.id
                && call.status == ToolCallStatus::Finished)
    );
    assert!(trace.tool_calls.iter().any(|call| call.tool == ASK_USER));
    assert!(
        trace
            .tool_calls
            .iter()
            .any(|call| call.tool == SEND_MESSAGE)
    );
    assert_eq!(trace.decisions.len(), 1);
    assert_eq!(
        trace.decisions[0].trace_id.as_deref(),
        Some(root.trace_id.as_str())
    );
    assert_eq!(trace.decisions[0].run_id, Some(run.id));
    assert_eq!(
        trace.outbox.iter().map(|row| row.kind).collect::<Vec<_>>(),
        [OutboxKind::Message, OutboxKind::Decision]
    );
    assert!(trace.outbox.iter().all(
        |row| row.trace_id.as_deref() == Some(root.trace_id.as_str()) && row.run_id == Some(run.id)
    ));
    assert!(matches!(
        sim.api
            .operations(OperationsRequest::Trace {
                id: "missing".into()
            })
            .await,
        Err(ApiError::NotFound(_))
    ));
}

async fn dead_event(sim: &Sim, key: &str) -> EventId {
    sim.backend()
        .set_default(Script::new().fail("scripted failure"));
    let id = sim.send(key, "Try the work.").await;
    assert!(sim.work_once().await.unwrap());
    let event = sim.event(id).await;
    assert_eq!((event.status, event.attempts), (EventStatus::Dead, 1));
    sim.clock().advance(sim.world.settings.retry_backoff_max);
    id
}

#[tokio::test]
async fn replay_revives_once_and_the_runner_consumes_the_same_event() {
    let sim = World::with_settings(HubSettings {
        max_attempts: 1,
        ..HubSettings::default()
    })
    .await
    .boot(CrashPlan::new())
    .await;
    let id = dead_event(&sim, "dead").await;
    let request = OperationsRequest::Replay { event: id };
    let (left, right) = tokio::join!(
        sim.api.operations(request.clone()),
        sim.api.operations(request.clone())
    );
    let (Ok(OperationsReply::Replayed { event }), Err(ApiError::Invalid(_))) = (if left.is_ok() {
        (left, right)
    } else {
        (right, left)
    }) else {
        panic!("exactly one concurrent replay should succeed");
    };
    assert_eq!(
        (event.id, event.status, event.attempts),
        (id, EventStatus::Pending, 0)
    );
    assert_eq!(event.status_reason, None);
    sim.backend().set_default(reply_script());
    assert!(sim.work_once().await.unwrap());
    assert_eq!(sim.event(id).await.status, EventStatus::Done);
    let committed = sim.committed_runs_with(id).await;
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].trace_id, event.trace_id);
    assert!(matches!(
        sim.api.operations(request).await,
        Err(ApiError::Invalid(_))
    ));
}

#[tokio::test]
async fn replay_refuses_an_event_that_died_again_after_replay() {
    let sim = World::with_settings(HubSettings {
        max_attempts: 1,
        ..HubSettings::default()
    })
    .await
    .boot(CrashPlan::new())
    .await;
    let id = dead_event(&sim, "dies twice").await;
    let request = OperationsRequest::Replay { event: id };
    assert!(sim.api.operations(request.clone()).await.is_ok());
    assert!(sim.work_once().await.unwrap());
    assert_eq!(sim.event(id).await.status, EventStatus::Dead);
    assert!(matches!(
        sim.api.operations(request).await,
        Err(ApiError::Invalid(_))
    ));
}

#[tokio::test]
async fn rollback_restores_content_and_appends_an_owner_version() {
    let sim = Sim::start().await;
    sim.backend().set_default(
        Script::new()
            .call(
                "note",
                threadlane_hub::MEMORY_CREATE,
                json!({
                    "path": "project/notes/plan.md",
                    "content": "# Original\nFirst plan.",
                }),
            )
            .finish("Saved."),
    );
    sim.send("memory", "Save the first plan.").await;
    assert!(sim.work_once().await.unwrap());
    let path = "project/notes/plan.md".to_owned();
    let history = MemoryRequest::History {
        project: None,
        path: path.clone(),
    };
    let MemoryReply::History { versions: initial } = sim.api.memory(history.clone()).await.unwrap()
    else {
        panic!("expected history");
    };
    assert_eq!(initial.len(), 1);
    assert!(initial[0].written_by_run.is_some());

    sim.api
        .memory(MemoryRequest::Write {
            project: None,
            path: path.clone(),
            content: "# Revised\nSecond plan.".into(),
        })
        .await
        .unwrap();
    sim.api
        .memory(MemoryRequest::Delete {
            project: None,
            path: path.clone(),
        })
        .await
        .unwrap();
    let request = OperationsRequest::Rollback {
        project: None,
        path: path.clone(),
        version: 1,
    };
    let OperationsReply::RolledBack { file } = sim.api.operations(request).await.unwrap() else {
        panic!("expected restored version");
    };
    assert_eq!(file.path, path);
    assert_eq!(file.version, 4);
    assert_eq!(file.written_by_run, None);
    assert_eq!(file.content, initial[0].content);
    let MemoryReply::History { versions } = sim.api.memory(history).await.unwrap() else {
        panic!("expected history");
    };
    assert_eq!(versions[0], initial[0]);
    assert!(versions[2].deleted);
    assert_eq!(versions[3], file);
    let MemoryReply::File { file: index } = sim
        .api
        .memory(MemoryRequest::Read {
            project: None,
            path: "project/notes/INDEX.md".into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected reindexed note");
    };
    assert!(index.content.contains("Original"));
    for (path, version) in [
        ("project/notes/plan.md", 3),
        ("projects/2/notes/plan.md", 1),
    ] {
        assert!(matches!(
            sim.api
                .operations(OperationsRequest::Rollback {
                    project: None,
                    path: path.into(),
                    version,
                })
                .await,
            Err(ApiError::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn lane_and_metrics_dispatch_use_scope_and_show_live_lease_and_history() {
    let sim = Sim::start().await;
    sim.backend().set_default(asking(Script::new(), "display"));
    sim.send("one", "Ask me about the colour.").await;
    assert!(sim.work_once().await.unwrap());
    let timer = follow_up(&sim, None, Duration::from_secs(60)).await;
    sim.api
        .org(OrgRequest::CreateProject {
            name: "Other".into(),
            description: String::new(),
        })
        .await
        .unwrap();
    sim.send_to(Some("Other"), "two", "Other work.").await;
    let lane = sim.general_lane().await;
    let lease = sim
        .db()
        .grant_lease(
            lane.id,
            sim.machine,
            sim.clock().now(),
            Duration::from_secs(60),
        )
        .await
        .unwrap()
        .unwrap();

    let ClientResponse::Operations {
        reply: OperationsReply::Lane { lane: inspected },
    } = dispatch(
        sim.api.as_ref(),
        ClientRequest::Operations {
            request: OperationsRequest::Lane {
                target: LaneTarget::Lead {
                    project: Some("General".into()),
                },
            },
        },
    )
    .await
    else {
        panic!("expected lane inspection");
    };
    assert_eq!(inspected.lane.lease_owner, Some(sim.machine));
    assert_eq!(inspected.lane.lease_epoch, lease.epoch);
    assert_eq!(inspected.lane.lease_expires_at, Some(lease.expires_at));
    assert_eq!(inspected.events.len(), 1);
    assert_eq!(
        (inspected.events[0].status, inspected.events[0].attempts),
        (EventStatus::Done, 0)
    );
    assert_eq!(inspected.sessions.len(), 1);
    assert_eq!(inspected.runs.len(), 1);
    assert_eq!(inspected.open_decisions.len(), 1);
    // General's default dreaming, the decision's expiry, then the follow-up.
    assert_eq!(inspected.timers.len(), 3);
    assert_eq!(inspected.timers[2], timer);
    assert_eq!(inspected.timers[0].created_by, TimerCreator::System);
    assert_eq!(inspected.timers[1].created_by, TimerCreator::System);
    assert_eq!(
        Some(inspected.timers[1].due_at),
        inspected.open_decisions[0].expires_at
    );

    let OperationsReply::Metrics { metrics } = sim
        .api
        .operations(OperationsRequest::Metrics {
            project: Some("General".into()),
        })
        .await
        .unwrap()
    else {
        panic!("expected metrics");
    };
    assert_eq!(metrics.queue_depth, [("General".to_owned(), 0)].into());
    let OperationsReply::Metrics { metrics } = sim
        .api
        .operations(OperationsRequest::Metrics { project: None })
        .await
        .unwrap()
    else {
        panic!("expected metrics");
    };
    assert_eq!(metrics.queue_depth.len(), 2);
    assert_eq!(metrics.queue_depth.values().sum::<i64>(), 1);
    assert_eq!(metrics.lane_lag_ms.len(), 1);
    let response = ClientResponse::Operations {
        reply: OperationsReply::Metrics { metrics },
    };
    let wire = serde_json::to_vec(&response).unwrap();
    assert_eq!(
        serde_json::from_slice::<ClientResponse>(&wire).unwrap(),
        response
    );
    assert!(matches!(
        sim.api
            .operations(OperationsRequest::Replay { event: EventId(99) })
            .await,
        Err(ApiError::NotFound(_))
    ));
}

#[tokio::test]
async fn lane_inspects_a_task_after_it_has_ended() {
    use threadlane_core::client::{TaskReply, TaskRequest};
    use threadlane_core::model::TaskStatus;

    let sim = Sim::start().await;
    crate::harness::add_engineer(sim.db(), 1).await;
    let TaskReply::Task { task } = sim
        .api
        .tasks(TaskRequest::Create {
            project: None,
            title: "An ended task".into(),
            goal: "Finish something.".into(),
            agent: crate::harness::ENGINEER.into(),
            repositories: Vec::new(),
            depends_on: Vec::new(),
            playbooks: Vec::new(),
            client_request_id: "inspect-ended-task".into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected the task");
    };
    let TaskReply::Task { task } = sim
        .api
        .tasks(TaskRequest::Cancel { task: task.id })
        .await
        .unwrap()
    else {
        panic!("expected the cancelled task");
    };
    assert_eq!(task.status, TaskStatus::Cancelled);
    let OperationsReply::Lane { lane } = sim
        .api
        .operations(OperationsRequest::Lane {
            target: LaneTarget::Task { task: task.id },
        })
        .await
        .unwrap()
    else {
        panic!("expected task lane");
    };
    assert_eq!(lane.lane.id, task.lane_id);
    assert_eq!(lane.lane.task_id, Some(task.id));
    assert_eq!(lane.events.len(), 1);
}
