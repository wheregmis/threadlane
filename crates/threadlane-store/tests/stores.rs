//! Behavior of every store through its interface, on an in-memory database.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use threadlane_core::ids::{DecisionId, EventId, LaneId, MachineId, RunId};
use threadlane_core::model::*;
use threadlane_core::store::*;
use threadlane_core::time::{Clock, SystemClock, Timestamp};
use threadlane_store::{Bootstrap, HubDb, SYSTEM_SOURCE};

const T0: Timestamp = Timestamp::from_millis(1_767_225_600_000);
const LEASE_TTL: Duration = Duration::from_secs(60);

async fn hub() -> HubDb {
    HubDb::open_in_memory(&Bootstrap::default(), Arc::new(SystemClock))
        .await
        .expect("in-memory hub opens")
}

fn message(project: &Project, id: &str) -> NewEvent {
    NewEvent {
        project_id: project.id,
        lane_id: project.lead_lane_id,
        source: "client".into(),
        source_event_id: id.into(),
        kind: EventKind::UserMessage,
        payload: json!({ "text": id }),
        trust: Trust::User,
        requested: true,
        session_ref: None,
        decision_ref: None,
        reply_to_ref: None,
        received_at: T0,
    }
}

fn new_run(events: Vec<EventId>, started_at: Timestamp) -> StartRun {
    StartRun {
        session: SessionChoice::New,
        agent_id: None,
        backend: None,
        native_session_id: None,
        events,
        started_at,
    }
}

/// Leases the lane and starts one run on all of its pending events.
async fn run_pending(
    hub: &HubDb,
    lane: LaneId,
    machine: MachineId,
    now: Timestamp,
) -> (Lease, Run) {
    let lease = hub
        .grant_lease(lane, machine, now, LEASE_TTL)
        .await
        .unwrap()
        .expect("lane is free");
    let events = hub
        .pending_events(lane)
        .await
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();
    let run = hub.start_run(&lease, &new_run(events, now)).await.unwrap();
    (lease, run)
}

fn retry(count_attempt: bool, max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        count_attempt,
        max_attempts,
        backoff_until: T0 + Duration::from_secs(30),
    }
}

/// System notices waiting in the outbox.
async fn notices(hub: &HubDb) -> Vec<OutboxMessage> {
    hub.deliverable(T0 + Duration::from_secs(3600), 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m.kind == OutboxKind::SystemNotice)
        .collect()
}

async fn machine(hub: &HubDb, name: &str) -> MachineId {
    hub.register_machine(&NewMachine {
        name: name.into(),
        endpoint_id: format!("key-{name}"),
        is_hub: false,
        trust: MachineTrust::Trusted,
        max_parallel: 2,
    })
    .await
    .expect("machine registers")
    .id
}

#[tokio::test]
async fn bootstrap_creates_space_and_general_project_once() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    assert!(general.is_general);
    assert_eq!(hub.space().await.unwrap(), SpaceSettings::default());
    assert_eq!(hub.projects().await.unwrap(), vec![general.clone()]);
    assert_eq!(
        hub.schema_version().await.unwrap(),
        threadlane_store::SCHEMA_VERSION
    );
    assert!(hub.integrity_check().await.unwrap().is_empty());
}

#[tokio::test]
async fn reopening_a_file_keeps_data_and_does_not_rebootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.db");
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let bootstrap = Bootstrap {
        space: SpaceSettings {
            name: "Work".into(),
            ..SpaceSettings::default()
        },
        ..Bootstrap::default()
    };
    {
        let hub = HubDb::open(&path, &bootstrap, clock.clone()).await.unwrap();
        hub.create_project(&NewProject {
            name: "Payments API".into(),
            max_parallel_tasks: 3,
            ..NewProject::default()
        })
        .await
        .unwrap();
    }
    let hub = HubDb::open(&path, &Bootstrap::default(), clock)
        .await
        .unwrap();
    assert_eq!(hub.space().await.unwrap().name, "Work");
    let names: Vec<_> = hub
        .projects()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, vec!["General", "Payments API"]);
}

#[tokio::test]
async fn organization_rules_are_enforced() {
    let hub = hub().await;
    let mut space = hub.space().await.unwrap();
    space.mode_ceiling = Mode::Assisted;
    space.quiet_hours = Some(QuietHours {
        start_minute: 22 * 60,
        end_minute: 7 * 60,
        time_zone: "Europe/Berlin".into(),
    });
    hub.update_space(&space).await.unwrap();
    assert_eq!(hub.space().await.unwrap(), space);

    let project = hub
        .create_project(&NewProject {
            name: "Payments API".into(),
            description: "payments".into(),
            mode_ceiling: Some(Mode::OnRequest),
            max_parallel_tasks: 3,
        })
        .await
        .unwrap();
    assert_eq!(
        hub.effective_ceiling(project.id).await.unwrap(),
        Mode::OnRequest
    );
    let duplicate = hub
        .create_project(&NewProject {
            name: "Payments API".into(),
            ..NewProject::default()
        })
        .await;
    assert!(
        matches!(duplicate, Err(StoreError::Conflict(_))),
        "{duplicate:?}"
    );

    let repo = NewRepository {
        project_id: project.id,
        name: "payments-service".into(),
        remote_url: "git@github.com:me/payments-service.git".into(),
        default_branch: "main".into(),
        protected_branches: vec!["main".into()],
    };
    let created = hub.create_repository(&repo).await.unwrap();
    assert_eq!(created.protected_branches, vec!["main".to_string()]);
    assert_eq!(
        hub.repository_by_remote(&repo.remote_url).await.unwrap(),
        Some(created)
    );
    let general = hub.general_project().await.unwrap();
    let same_remote_elsewhere = hub
        .create_repository(&NewRepository {
            project_id: general.id,
            name: "copy".into(),
            ..repo.clone()
        })
        .await;
    assert!(matches!(
        same_remote_elsewhere,
        Err(StoreError::Conflict(_))
    ));

    let lead = hub
        .create_agent(&NewAgent::named("Lead", "lead"))
        .await
        .unwrap();
    let engineer = hub
        .create_agent(&NewAgent {
            backends: vec![BackendKind::Claude, BackendKind::Codex],
            ..NewAgent::named("Engineer", "engineer")
        })
        .await
        .unwrap();
    assert_eq!(
        hub.agent(engineer.id)
            .await
            .unwrap()
            .unwrap()
            .backends
            .len(),
        2
    );

    let bind = |agent: &Agent, is_lead: bool| NewBinding {
        project_id: project.id,
        agent_id: agent.id,
        mode: Mode::OnRequest,
        is_lead,
        max_parallel: 2,
    };
    hub.bind(&bind(&lead, true)).await.unwrap();
    let second_lead = hub.bind(&bind(&engineer, true)).await;
    assert!(
        matches!(second_lead, Err(StoreError::Conflict(_))),
        "{second_lead:?}"
    );
    hub.bind(&bind(&engineer, false)).await.unwrap();
    let twice = hub.bind(&bind(&engineer, false)).await;
    assert!(matches!(twice, Err(StoreError::Conflict(_))));
    assert_eq!(hub.bindings(project.id).await.unwrap().len(), 2);
    assert_eq!(
        hub.lead_binding(project.id)
            .await
            .unwrap()
            .map(|b| b.agent_id),
        Some(lead.id)
    );
}

#[tokio::test]
async fn a_binding_s_mode_can_be_changed() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = hub
        .create_agent(&NewAgent::named("Lead", "lead"))
        .await
        .unwrap();
    let bound = hub
        .bind(&NewBinding {
            project_id: general.id,
            agent_id: agent.id,
            mode: Mode::OnRequest,
            is_lead: true,
            max_parallel: 1,
        })
        .await
        .unwrap();

    let off = hub.set_binding_mode(bound.id, Mode::Off).await.unwrap();
    assert_eq!(
        off,
        Binding {
            mode: Mode::Off,
            ..bound.clone()
        }
    );
    assert_eq!(hub.lead_binding(general.id).await.unwrap(), Some(off));

    let missing = hub
        .set_binding_mode(
            threadlane_core::ids::BindingId(bound.id.get() + 1),
            Mode::Off,
        )
        .await;
    assert!(
        matches!(
            missing,
            Err(StoreError::NotFound {
                kind: "binding",
                ..
            })
        ),
        "{missing:?}"
    );
}

#[tokio::test]
async fn events_deduplicate_and_must_match_their_lane() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let first = hub.insert_event(&message(&general, "m1")).await.unwrap();
    let again = hub.insert_event(&message(&general, "m1")).await.unwrap();
    assert!(matches!(first, Inserted::New(_)));
    assert_eq!(again, Inserted::Duplicate(first.id()));
    assert_eq!(
        hub.pending_events(general.lead_lane_id)
            .await
            .unwrap()
            .len(),
        1
    );

    let other = hub
        .create_project(&NewProject {
            name: "Other".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let wrong_lane = NewEvent {
        lane_id: other.lead_lane_id,
        ..message(&general, "m2")
    };
    assert!(matches!(
        hub.insert_event(&wrong_lane).await,
        Err(StoreError::Invalid(_))
    ));
}

#[tokio::test]
async fn one_lease_per_lane_with_fencing() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    let b = machine(&hub, "b").await;
    hub.insert_event(&message(&general, "m1")).await.unwrap();
    assert_eq!(hub.ready_lanes(T0).await.unwrap(), vec![lane]);

    let lease_a = hub
        .grant_lease(lane, a, T0, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease_a.epoch, 1);
    assert!(hub.ready_lanes(T0).await.unwrap().is_empty());
    assert_eq!(hub.grant_lease(lane, b, T0, LEASE_TTL).await.unwrap(), None);

    // After expiry another machine takes over and the first one is fenced off.
    let later = T0 + LEASE_TTL;
    let lease_b = hub
        .grant_lease(lane, b, later, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lease_b.epoch, 2);
    let stale = hub.renew_lease(&lease_a, later, LEASE_TTL).await;
    assert!(
        matches!(
            stale,
            Err(StoreError::StaleLease {
                held: 1,
                current: 2,
                ..
            })
        ),
        "{stale:?}"
    );
    let events: Vec<_> = hub
        .pending_events(lane)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    let start = StartRun {
        session: SessionChoice::New,
        agent_id: None,
        backend: None,
        native_session_id: None,
        events: events.clone(),
        started_at: later,
    };
    assert!(matches!(
        hub.start_run(&lease_a, &start).await,
        Err(StoreError::StaleLease { .. })
    ));
    // Releasing a stale lease is a harmless no-op.
    hub.release_lease(&lease_a).await.unwrap();
    assert_eq!(hub.lane(lane).await.unwrap().unwrap().lease_owner, Some(b));

    let renewed = hub.renew_lease(&lease_b, later, LEASE_TTL).await.unwrap();
    assert_eq!(renewed.expires_at, later + LEASE_TTL);
    hub.release_lease(&renewed).await.unwrap();
    assert_eq!(hub.lane(lane).await.unwrap().unwrap().lease_owner, None);
}

#[tokio::test]
async fn run_lifecycle_commits_events_exactly_once() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    for id in ["m1", "m2", "m3"] {
        hub.insert_event(&message(&general, id)).await.unwrap();
    }
    let lease = hub
        .grant_lease(lane, a, T0, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    let events: Vec<_> = hub
        .pending_events(lane)
        .await
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();
    let start = StartRun {
        session: SessionChoice::New,
        agent_id: None,
        backend: Some(BackendKind::Claude),
        native_session_id: None,
        events: events.clone(),
        started_at: T0,
    };
    let run = hub.start_run(&lease, &start).await.unwrap();
    assert_eq!(run.status, RunStatus::Active);
    assert!(hub.pending_events(lane).await.unwrap().is_empty());

    // Invariant 2: a second active run in the lane is refused.
    hub.insert_event(&message(&general, "m4")).await.unwrap();
    let m4 = hub.pending_events(lane).await.unwrap()[0].id;
    let second = hub
        .start_run(
            &lease,
            &StartRun {
                events: vec![m4],
                ..start.clone()
            },
        )
        .await;
    assert!(matches!(second, Err(StoreError::Conflict(_))), "{second:?}");

    hub.set_native_session(&lease, run.session_id, "claude-abc")
        .await
        .unwrap();
    hub.commit_run(&lease, run.id, RunOutcome::Sent, Some("told them"), T0)
        .await
        .unwrap();
    for id in &events {
        let event = hub.event(*id).await.unwrap().unwrap();
        assert_eq!(event.status, EventStatus::Done);
        assert_eq!(event.claimed_by_run, Some(run.id));
    }
    // Invariant 3: a committed run cannot be committed again.
    assert!(matches!(
        hub.commit_run(&lease, run.id, RunOutcome::Silent, None, T0)
            .await,
        Err(StoreError::Conflict(_))
    ));

    // The next run resumes the same session.
    let session = hub.latest_open_session(lane).await.unwrap().unwrap();
    assert_eq!(session.id, run.session_id);
    assert_eq!(session.native_session_id.as_deref(), Some("claude-abc"));
    let resumed = hub
        .start_run(
            &lease,
            &StartRun {
                session: SessionChoice::Resume(session.id),
                events: vec![m4],
                ..start
            },
        )
        .await
        .unwrap();
    assert_eq!(resumed.session_id, session.id);
    hub.commit_run(&lease, resumed.id, RunOutcome::Silent, None, T0)
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_runs_retry_with_backoff_then_die() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    hub.insert_event(&message(&general, "m1")).await.unwrap();
    let mut now = T0;
    for attempt in 1..=3 {
        let lease = hub
            .grant_lease(lane, a, now, LEASE_TTL)
            .await
            .unwrap()
            .unwrap();
        let events: Vec<_> = hub
            .pending_events(lane)
            .await
            .unwrap()
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(events.len(), 1, "attempt {attempt}");
        let run = hub
            .start_run(
                &lease,
                &StartRun {
                    session: SessionChoice::New,
                    agent_id: None,
                    backend: None,
                    native_session_id: None,
                    events,
                    started_at: now,
                },
            )
            .await
            .unwrap();
        let backoff_until = now + Duration::from_secs(30);
        let failed = hub
            .fail_run(
                &lease,
                run.id,
                "boom",
                RetryPolicy {
                    count_attempt: true,
                    max_attempts: 3,
                    backoff_until,
                },
                now,
            )
            .await
            .unwrap();
        hub.release_lease(&lease).await.unwrap();
        if attempt < 3 {
            assert_eq!(failed.retried.len(), 1);
            assert!(
                hub.ready_lanes(now).await.unwrap().is_empty(),
                "backoff holds"
            );
            assert_eq!(hub.ready_lanes(backoff_until).await.unwrap(), vec![lane]);
        } else {
            assert_eq!(failed.dead.len(), 1);
            let event = hub.event(failed.dead[0]).await.unwrap().unwrap();
            assert_eq!(event.status, EventStatus::Dead);
            assert_eq!(event.attempts, 3);
            assert_eq!(event.status_reason.as_deref(), Some("boom"));
        }
        now = backoff_until;
    }
    assert!(hub.ready_lanes(now).await.unwrap().is_empty());
}

#[tokio::test]
async fn expired_leases_return_claimed_events_and_abandon_runs() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    hub.insert_event(&message(&general, "m1")).await.unwrap();
    let lease = hub
        .grant_lease(lane, a, T0, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    let events: Vec<_> = hub
        .pending_events(lane)
        .await
        .unwrap()
        .iter()
        .map(|e| e.id)
        .collect();
    let run = hub
        .start_run(
            &lease,
            &StartRun {
                session: SessionChoice::New,
                agent_id: None,
                backend: None,
                native_session_id: None,
                events: events.clone(),
                started_at: T0,
            },
        )
        .await
        .unwrap();
    assert!(hub.expire_leases(T0).await.unwrap().is_empty());
    assert_eq!(hub.expire_leases(T0 + LEASE_TTL).await.unwrap(), vec![lane]);
    assert_eq!(
        hub.run(run.id).await.unwrap().unwrap().status,
        RunStatus::Abandoned
    );
    assert_eq!(hub.pending_events(lane).await.unwrap()[0].id, events[0]);
    assert!(matches!(
        hub.commit_run(&lease, run.id, RunOutcome::Sent, None, T0)
            .await,
        Err(StoreError::StaleLease { .. })
    ));
}

#[tokio::test]
async fn system_tool_calls_take_effect_at_most_once() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    hub.insert_event(&message(&general, "m1")).await.unwrap();
    let lease = hub
        .grant_lease(lane, a, T0, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    let events = vec![hub.pending_events(lane).await.unwrap()[0].id];
    let run = hub
        .start_run(
            &lease,
            &StartRun {
                session: SessionChoice::New,
                agent_id: None,
                backend: None,
                native_session_id: None,
                events,
                started_at: T0,
            },
        )
        .await
        .unwrap();
    let args = json!({ "body": "hi" });
    let send = ToolCallKey {
        run_id: run.id,
        key: "call-1".into(),
    };
    let first = hub
        .begin_tool_call(&lease, &send, "send_message", &args, false, T0)
        .await
        .unwrap();
    assert_eq!(first, ToolCallBegin::Perform);
    let unknown = hub
        .begin_tool_call(&lease, &send, "send_message", &args, false, T0)
        .await
        .unwrap();
    assert_eq!(unknown, ToolCallBegin::OutcomeUnknown);
    hub.finish_tool_call(&lease, &send, &json!({ "outbox_id": 7 }), T0)
        .await
        .unwrap();
    let repeated = hub
        .begin_tool_call(&lease, &send, "send_message", &args, false, T0)
        .await
        .unwrap();
    assert_eq!(repeated, ToolCallBegin::Finished(json!({ "outbox_id": 7 })));

    let read = ToolCallKey {
        run_id: run.id,
        key: "call-2".into(),
    };
    for _ in 0..2 {
        let begin = hub
            .begin_tool_call(&lease, &read, "memory_view", &json!({}), true, T0)
            .await
            .unwrap();
        assert_eq!(
            begin,
            ToolCallBegin::Perform,
            "safe-to-repeat calls run again"
        );
    }
}

#[tokio::test]
async fn timers_fire_each_occurrence_once() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let follow_up = hub
        .create_timer(&NewTimer {
            project_id: general.id,
            lane_id: general.lead_lane_id,
            agent_id: None,
            task_ref: None,
            category: TimerCategory::FollowUp,
            due_at: T0,
            recurrence: None,
            instruction: "check the build".into(),
            created_by: TimerCreator::Agent,
            created_by_call: Some("call-9".into()),
        })
        .await
        .unwrap();
    let same = hub
        .create_timer(&NewTimer {
            instruction: "different".into(),
            created_by_call: Some("call-9".into()),
            category: TimerCategory::FollowUp,
            project_id: general.id,
            lane_id: general.lead_lane_id,
            agent_id: None,
            task_ref: None,
            due_at: T0,
            recurrence: None,
            created_by: TimerCreator::Agent,
        })
        .await
        .unwrap();
    assert_eq!(same.id, follow_up.id, "creation is idempotent per call");

    let schedule = hub
        .create_timer(&NewTimer {
            project_id: general.id,
            lane_id: general.lead_lane_id,
            agent_id: None,
            task_ref: None,
            category: TimerCategory::Schedule,
            due_at: T0,
            recurrence: Some(json!({ "rule": "FREQ=DAILY", "time_zone": "Europe/Berlin" })),
            instruction: "daily summary".into(),
            created_by: TimerCreator::User,
            created_by_call: None,
        })
        .await
        .unwrap();

    assert_eq!(hub.due_timers(T0).await.unwrap().len(), 2);
    let fired = hub.fire(follow_up.id, T0, None, T0).await.unwrap();
    assert!(matches!(fired, Inserted::New(_)));
    assert_eq!(
        hub.fire(follow_up.id, T0, None, T0).await.unwrap(),
        Inserted::Duplicate(fired.id()),
        "a retried fire inserts nothing"
    );
    assert_eq!(
        hub.timer(follow_up.id).await.unwrap().unwrap().status,
        TimerStatus::Done
    );

    let tomorrow = T0 + Duration::from_secs(24 * 3600);
    hub.fire(schedule.id, T0, Some(tomorrow), T0).await.unwrap();
    let schedule = hub.timer(schedule.id).await.unwrap().unwrap();
    assert_eq!(schedule.due_at, tomorrow);
    assert_eq!(schedule.fire_count, 1);
    assert!(hub.due_timers(T0).await.unwrap().is_empty());

    let events = hub.pending_events(general.lead_lane_id).await.unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.kind == EventKind::TimerFired));
    assert!(
        events.iter().all(|e| e.requested),
        "user routine and agent follow-up"
    );

    hub.cancel_timer(schedule.id).await.unwrap();
    assert!(hub.due_timers(tomorrow).await.unwrap().is_empty());
}

#[tokio::test]
async fn tasks_get_a_lane_and_a_start_event() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let engineer = hub
        .create_agent(&NewAgent::named("Engineer", "engineer"))
        .await
        .unwrap();
    let new_task = |title: &str, call: &str, depends_on| NewTask {
        project_id: general.id,
        agent_id: engineer.id,
        title: title.into(),
        goal: "done when tests pass".into(),
        repositories: Vec::new(),
        parent_task_id: None,
        depends_on,
        origin: TaskOrigin::User,
        status: TaskStatus::Queued,
        priority: 0,
        created_by_call: Some(call.into()),
        created_at: T0,
    };
    let first = hub
        .create_task(&new_task("first", "c1", vec![]))
        .await
        .unwrap();
    assert_eq!(first.id.to_string(), "T-0001");
    let again = hub
        .create_task(&new_task("ignored", "c1", vec![]))
        .await
        .unwrap();
    assert_eq!(again, first, "creation is idempotent per call");
    let second = hub
        .create_task(&new_task("second", "c2", vec![first.id]))
        .await
        .unwrap();
    assert_eq!(second.depends_on, vec![first.id]);

    let lane = hub.lane(first.lane_id).await.unwrap().unwrap();
    assert_eq!(lane.kind, LaneKind::Task);
    assert_eq!(lane.task_id, Some(first.id));
    let start = hub.pending_events(first.lane_id).await.unwrap();
    assert_eq!(start.len(), 1);
    assert_eq!(start[0].kind, EventKind::TaskStart);
    assert!(start[0].requested);

    hub.set_task_status(first.id, TaskStatus::Running, None)
        .await
        .unwrap();
    let tasks = hub.tasks(general.id).await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].status, TaskStatus::Running);
}

#[tokio::test]
async fn machines_report_and_accounts_cool_down() {
    let hub = hub().await;
    let id = machine(&hub, "mac").await;
    let report = MachineReport {
        capabilities: json!({ "os": "macos" }),
        backends: json!({ "claude": { "version": "2.1", "logged_in": true } }),
        gh_login: true,
        load: 1,
    };
    hub.report(id, &report, T0).await.unwrap();
    let machine = hub.machine(id).await.unwrap().unwrap();
    assert_eq!(machine.status, MachineStatus::Online);
    assert_eq!(machine.last_seen, Some(T0));
    assert!(machine.gh_login);
    assert_eq!(
        hub.machine_by_endpoint("key-mac")
            .await
            .unwrap()
            .map(|m| m.id),
        Some(id)
    );

    hub.set_machine_status(id, MachineStatus::Revoked)
        .await
        .unwrap();
    assert!(matches!(
        hub.report(id, &report, T0).await,
        Err(StoreError::Conflict(_))
    ));

    let account = hub
        .ensure_account("claude-work", BackendKind::Claude)
        .await
        .unwrap();
    assert_eq!(
        hub.ensure_account("claude-work", BackendKind::Claude)
            .await
            .unwrap(),
        account
    );
    assert!(
        hub.ensure_account("claude-work", BackendKind::Codex)
            .await
            .is_err()
    );
    hub.set_cooldown("claude-work", Some(T0)).await.unwrap();
    assert_eq!(hub.accounts().await.unwrap()[0].cooldown_until, Some(T0));
}

#[tokio::test]
async fn memory_writes_are_versioned() {
    let hub = hub().await;
    let path = "projects/1/notes/build.md";
    let write = |content: &str| MemoryWrite {
        path: path.into(),
        content: content.into(),
        written_by_run: None,
        written_at: T0,
    };
    assert_eq!(hub.write(&write("v1")).await.unwrap().version, 1);
    assert_eq!(hub.write(&write("v2")).await.unwrap().version, 2);
    assert_eq!(hub.read(path).await.unwrap().unwrap().content, "v2");
    hub.write(&MemoryWrite {
        path: "projects/2/notes/other.md".into(),
        ..write("x")
    })
    .await
    .unwrap();
    assert_eq!(hub.list("projects/1/").await.unwrap().len(), 1);

    hub.delete(path, None, T0).await.unwrap();
    assert_eq!(hub.read(path).await.unwrap(), None);
    let history: Vec<_> = hub
        .history(path)
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.version, f.deleted))
        .collect();
    assert_eq!(history, vec![(1, false), (2, false), (3, true)]);
    assert!(hub.read("../escape").await.is_err());
}

#[tokio::test]
async fn decisions_are_answered_once_and_wake_their_lane() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let decision = hub
        .open_decision(&NewDecision {
            project_id: general.id,
            lane_id: general.lead_lane_id,
            task_id: None,
            session_id: None,
            run_id: None,
            tool_call_id: Some("call-3".into()),
            tool: "open_pull_request".into(),
            arguments: json!({ "repository": "payments-service" }),
            arguments_hash: "sha256:abc".into(),
            kind: DecisionKind::Approval,
            question: "Open a draft pull request?".into(),
            context: String::new(),
            options: vec![
                DecisionOption {
                    label: "Open draft".into(),
                    effect: json!({ "approve": true }),
                },
                DecisionOption {
                    label: "Not now".into(),
                    effect: json!({ "approve": false }),
                },
            ],
            recommended: Some(0),
            offers_standing_rule: true,
            expires_at: None,
            created_at: T0,
        })
        .await
        .unwrap();
    assert_eq!(decision.id, DecisionId(1));
    assert_eq!(decision.id.to_string(), "D-0001");
    assert_eq!(
        hub.pending_decisions(general.lead_lane_id)
            .await
            .unwrap()
            .len(),
        1
    );

    let answer = DecisionAnswer {
        option: 0,
        note: None,
        standing_rule: false,
        answered_at: T0,
    };
    let Answered::Recorded(event) = hub.answer(decision.id, &answer).await.unwrap() else {
        panic!("first answer is recorded");
    };
    assert_eq!(
        hub.answer(decision.id, &answer).await.unwrap(),
        Answered::Ignored
    );
    let event = hub.event(event).await.unwrap().unwrap();
    assert_eq!(event.kind, EventKind::DecisionAnswer);
    assert_eq!(event.decision_ref, Some(decision.id));
    assert!(
        hub.pending_decisions(general.lead_lane_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn outbox_sends_are_idempotent_per_tool_call() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let new = |call: &str, not_before| NewOutboxMessage {
        project_id: Some(general.id),
        lane_id: Some(general.lead_lane_id),
        session_id: None,
        run_id: None,
        tool_call_id: Some(call.into()),
        kind: OutboxKind::Message,
        decision_id: None,
        channel: "client".into(),
        body: json!({ "text": "done" }),
        urgency: Urgency::Normal,
        not_before,
        created_at: T0,
    };
    let first = hub.enqueue(&new("c1", None)).await.unwrap();
    assert_eq!(
        hub.enqueue(&new("c1", None)).await.unwrap(),
        first,
        "invariant 6"
    );
    let later = hub
        .enqueue(&new("c2", Some(T0 + Duration::from_secs(60))))
        .await
        .unwrap();

    assert_eq!(hub.deliverable(T0, 10).await.unwrap(), vec![first.clone()]);
    assert_eq!(first.delivery_seq, None);
    hub.mark_sent(first.id, Some("client-1"), T0).await.unwrap();
    assert_eq!(
        hub.message(first.id).await.unwrap().unwrap().delivery_seq,
        Some(1)
    );
    assert!(hub.mark_sent(first.id, None, T0).await.is_err());
    assert!(hub.deliverable(T0, 10).await.unwrap().is_empty());
    assert_eq!(
        hub.deliverable(T0 + Duration::from_secs(60), 10)
            .await
            .unwrap()[0]
            .id,
        later.id
    );
    let recent = hub.recent_messages(general.id, 5).await.unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].channel_message_id.as_deref(), Some("client-1"));
}

#[tokio::test]
async fn a_new_session_records_its_chosen_native_session() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    hub.insert_event(&message(&general, "m1")).await.unwrap();
    let lease = hub
        .grant_lease(lane, a, T0, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    let m1 = hub.pending_events(lane).await.unwrap()[0].id;
    let run = hub
        .start_run(
            &lease,
            &StartRun {
                native_session_id: Some("uuid-1".into()),
                ..new_run(vec![m1], T0)
            },
        )
        .await
        .unwrap();
    let session = hub.session(run.session_id).await.unwrap().unwrap();
    assert_eq!(session.native_session_id.as_deref(), Some("uuid-1"));
    hub.commit_run(&lease, run.id, RunOutcome::Silent, None, T0)
        .await
        .unwrap();

    // Resuming ignores the field and keeps the recorded id.
    hub.insert_event(&message(&general, "m2")).await.unwrap();
    let m2 = hub.pending_events(lane).await.unwrap()[0].id;
    let resumed = hub
        .start_run(
            &lease,
            &StartRun {
                session: SessionChoice::Resume(session.id),
                native_session_id: Some("uuid-2".into()),
                ..new_run(vec![m2], T0)
            },
        )
        .await
        .unwrap();
    assert_eq!(resumed.session_id, session.id);
    assert_eq!(
        hub.session(session.id)
            .await
            .unwrap()
            .unwrap()
            .native_session_id
            .as_deref(),
        Some("uuid-1")
    );
}

#[tokio::test]
async fn run_events_lists_what_a_run_consumed_whatever_its_status() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    for id in ["m1", "m2"] {
        hub.insert_event(&message(&general, id)).await.unwrap();
    }
    let (lease, run) = run_pending(&hub, lane, a, T0).await;
    // Arrives during the run; not part of it.
    hub.insert_event(&message(&general, "m3")).await.unwrap();

    let claimed = hub.run_events(run.id).await.unwrap();
    let sources: Vec<_> = claimed.iter().map(|e| e.source_event_id.as_str()).collect();
    assert_eq!(sources, vec!["m1", "m2"]);
    assert!(claimed.iter().all(|e| e.status == EventStatus::Claimed));

    hub.fail_run(&lease, run.id, "boom", retry(true, 5), T0)
        .await
        .unwrap();
    let retried = hub.run_events(run.id).await.unwrap();
    assert_eq!(retried.len(), 2);
    assert!(retried.iter().all(|e| e.status == EventStatus::Pending));
    assert!(hub.run_events(RunId(999)).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_failure_that_does_not_count_keeps_attempts_and_kills_nothing() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    hub.insert_event(&message(&general, "m1")).await.unwrap();

    // With one attempt allowed, a counted failure would kill the event.
    let mut now = T0;
    for _ in 0..3 {
        let (lease, run) = run_pending(&hub, lane, a, now).await;
        let failed = hub
            .fail_run(&lease, run.id, "usage limit", retry(false, 1), now)
            .await
            .unwrap();
        hub.release_lease(&lease).await.unwrap();
        assert_eq!(failed.dead, Vec::<EventId>::new());
        assert_eq!(failed.retried.len(), 1);
        let event = hub.event(failed.retried[0]).await.unwrap().unwrap();
        assert_eq!(event.status, EventStatus::Pending);
        assert_eq!(event.attempts, 0);
        assert_eq!(event.claimed_by_run, None);
        assert_eq!(
            hub.lane(lane).await.unwrap().unwrap().backoff_until,
            Some(retry(false, 1).backoff_until),
            "the lane still backs off"
        );
        now = retry(false, 1).backoff_until;
    }
    assert!(notices(&hub).await.is_empty());

    let (lease, run) = run_pending(&hub, lane, a, now).await;
    let failed = hub
        .fail_run(&lease, run.id, "boom", retry(true, 1), now)
        .await
        .unwrap();
    assert_eq!(failed.dead.len(), 1);
    assert_eq!(
        hub.event(failed.dead[0]).await.unwrap().unwrap().attempts,
        1
    );
}

#[tokio::test]
async fn dead_events_in_a_lead_lane_queue_one_system_notice() {
    let hub = hub().await;
    let project = hub
        .create_project(&NewProject {
            name: "Payments API".into(),
            max_parallel_tasks: 2,
            ..NewProject::default()
        })
        .await
        .unwrap();
    let lane = project.lead_lane_id;
    let a = machine(&hub, "a").await;
    for id in ["m1", "m2"] {
        hub.insert_event(&message(&project, id)).await.unwrap();
    }
    let (lease, run) = run_pending(&hub, lane, a, T0).await;
    let failed = hub
        .fail_run(&lease, run.id, "backend crashed.", retry(true, 1), T0)
        .await
        .unwrap();
    assert_eq!(failed.dead.len(), 2);

    let queued = notices(&hub).await;
    assert_eq!(queued.len(), 1, "one notice for the whole run: {queued:?}");
    let notice = &queued[0];
    assert_eq!(notice.channel, "client");
    assert_eq!(notice.urgency, Urgency::High);
    assert_eq!(notice.status, OutboxStatus::Pending);
    assert_eq!(notice.tool_call_id, Some(format!("dead:{}", run.id)));
    assert_eq!(notice.project_id, Some(project.id));
    assert_eq!(notice.lane_id, Some(lane));
    assert_eq!(notice.run_id, Some(run.id));
    assert_eq!(
        notice.body,
        json!({ "text": "I could not finish something you asked for in Payments API after 1 attempt: backend crashed." })
    );
    assert!(
        hub.pending_events(lane).await.unwrap().is_empty(),
        "the lead lane is not told about itself"
    );
    // A notice is not an ordinary message of the run.
    assert_eq!(hub.count_for_run(run.id).await.unwrap(), 0);

    // Failing the same run again is refused and queues nothing more.
    let again = hub
        .fail_run(&lease, run.id, "backend crashed.", retry(true, 1), T0)
        .await;
    assert!(matches!(again, Err(StoreError::Conflict(_))), "{again:?}");
    assert_eq!(notices(&hub).await.len(), 1);
}

#[tokio::test]
async fn dead_events_in_a_task_lane_tell_the_lead_lane_once() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let a = machine(&hub, "a").await;
    let engineer = hub
        .create_agent(&NewAgent::named("Engineer", "engineer"))
        .await
        .unwrap();
    let task = hub
        .create_task(&NewTask {
            project_id: general.id,
            agent_id: engineer.id,
            title: "fix the build".into(),
            goal: "tests pass".into(),
            repositories: Vec::new(),
            parent_task_id: None,
            depends_on: Vec::new(),
            origin: TaskOrigin::User,
            status: TaskStatus::Queued,
            priority: 0,
            created_by_call: None,
            created_at: T0,
        })
        .await
        .unwrap();
    let (lease, run) = run_pending(&hub, task.lane_id, a, T0).await;
    let failed = hub
        .fail_run(&lease, run.id, "boom", retry(true, 1), T0)
        .await
        .unwrap();
    assert_eq!(failed.dead.len(), 1);

    let told = hub.pending_events(general.lead_lane_id).await.unwrap();
    assert_eq!(told.len(), 1, "{told:?}");
    let event = &told[0];
    assert_eq!(event.source, SYSTEM_SOURCE);
    assert_eq!(event.source_event_id, format!("dead:{}", run.id));
    assert_eq!(event.kind, EventKind::System);
    assert_eq!(event.trust, Trust::System);
    assert!(event.requested);
    assert_eq!(event.project_id, general.id);
    assert_eq!(
        event.payload,
        json!({
            "kind": "events_dead",
            "lane_id": task.lane_id,
            "run_id": run.id,
            "event_ids": failed.dead,
            "error": "boom",
        })
    );
    assert!(
        notices(&hub).await.is_empty(),
        "the lead can still tell you"
    );

    let again = hub
        .fail_run(&lease, run.id, "boom", retry(true, 1), T0)
        .await;
    assert!(matches!(again, Err(StoreError::Conflict(_))), "{again:?}");
    assert_eq!(
        hub.pending_events(general.lead_lane_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn the_journal_appends_in_order_once_and_only_under_the_lease() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    let b = machine(&hub, "b").await;
    hub.insert_event(&message(&general, "m1")).await.unwrap();
    let (lease, run) = run_pending(&hub, lane, a, T0).await;
    let entry = |seq: i64| JournalEntry {
        run_id: run.id,
        seq,
        at: T0 + Duration::from_millis(seq.unsigned_abs()),
        entry: json!({ "type": "assistant", "n": seq }),
    };
    let seqs = |entries: Vec<JournalEntry>| entries.iter().map(|e| e.seq).collect::<Vec<_>>();

    hub.append_journal(&lease, &[]).await.unwrap();
    hub.append_journal(&lease, &[entry(1), entry(2)])
        .await
        .unwrap();
    // A retry that overlaps what is stored only adds the new tail.
    hub.append_journal(&lease, &[entry(2), entry(3)])
        .await
        .unwrap();
    hub.append_journal(&lease, &[entry(1), entry(2), entry(3)])
        .await
        .unwrap();
    let stored = hub.journal(run.id).await.unwrap();
    assert_eq!(stored, vec![entry(1), entry(2), entry(3)]);

    // A gap is refused, and nothing of that batch is kept.
    let gap = hub.append_journal(&lease, &[entry(4), entry(6)]).await;
    assert!(matches!(gap, Err(StoreError::Conflict(_))), "{gap:?}");
    let skip = hub.append_journal(&lease, &[entry(5)]).await;
    assert!(matches!(skip, Err(StoreError::Conflict(_))), "{skip:?}");
    assert_eq!(seqs(hub.journal(run.id).await.unwrap()), vec![1, 2, 3]);
    let zero = hub.append_journal(&lease, &[entry(0)]).await;
    assert!(matches!(zero, Err(StoreError::Invalid(_))), "{zero:?}");

    // Invariant 10: once the lane is someone else's, the old lease cannot append.
    let later = T0 + LEASE_TTL;
    let lease_b = hub
        .grant_lease(lane, b, later, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    let stale = hub.append_journal(&lease, &[entry(4)]).await;
    assert!(
        matches!(stale, Err(StoreError::StaleLease { .. })),
        "{stale:?}"
    );
    // Nor can the new holder write into a run that is no longer active.
    let inactive = hub.append_journal(&lease_b, &[entry(4)]).await;
    assert!(
        matches!(inactive, Err(StoreError::Conflict(_))),
        "{inactive:?}"
    );
    assert_eq!(seqs(hub.journal(run.id).await.unwrap()), vec![1, 2, 3]);
}

#[tokio::test]
async fn delivery_seq_follows_the_order_messages_are_sent() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let new = |call: &str, channel: &str| NewOutboxMessage {
        project_id: Some(general.id),
        lane_id: Some(general.lead_lane_id),
        session_id: None,
        run_id: None,
        tool_call_id: Some(call.into()),
        kind: OutboxKind::Message,
        decision_id: None,
        channel: channel.into(),
        body: json!({ "text": call }),
        urgency: Urgency::Normal,
        not_before: None,
        created_at: T0,
    };
    let m1 = hub.enqueue(&new("m1", "client")).await.unwrap();
    let m2 = hub.enqueue(&new("m2", "client")).await.unwrap();
    let m3 = hub.enqueue(&new("m3", "client")).await.unwrap();
    let other = hub.enqueue(&new("o1", "slack")).await.unwrap();
    let unsent = hub.enqueue(&new("m4", "client")).await.unwrap();

    // Sent out of id order, interleaved with another channel.
    for id in [m3.id, m1.id, other.id, m2.id] {
        hub.mark_sent(id, None, T0).await.unwrap();
    }
    let seq = |id| {
        let hub = hub.clone();
        async move { hub.message(id).await.unwrap().unwrap().delivery_seq }
    };
    assert_eq!(seq(m3.id).await, Some(1));
    assert_eq!(seq(m1.id).await, Some(2));
    assert_eq!(seq(other.id).await, Some(3));
    assert_eq!(seq(m2.id).await, Some(4));
    assert_eq!(seq(unsent.id).await, None);
    // A failed second mark changes nothing.
    assert!(hub.mark_sent(m1.id, None, T0).await.is_err());
    assert_eq!(seq(m1.id).await, Some(2));

    let ids = |messages: Vec<OutboxMessage>| messages.iter().map(|m| m.id).collect::<Vec<_>>();
    assert_eq!(
        ids(hub.delivered_after("client", 0, 10).await.unwrap()),
        vec![m3.id, m1.id, m2.id]
    );

    // Paging by the last seen delivery_seq.
    let page = hub.delivered_after("client", 0, 2).await.unwrap();
    assert_eq!(ids(page.clone()), vec![m3.id, m1.id]);
    let cursor = page.last().and_then(|m| m.delivery_seq).unwrap();
    let page = hub.delivered_after("client", cursor, 2).await.unwrap();
    assert_eq!(ids(page.clone()), vec![m2.id]);
    let cursor = page.last().and_then(|m| m.delivery_seq).unwrap();
    assert!(
        hub.delivered_after("client", cursor, 2)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ids(hub.delivered_after("slack", 0, 10).await.unwrap()),
        vec![other.id]
    );

    // The next one sent appears after the cursor.
    hub.mark_sent(unsent.id, None, T0).await.unwrap();
    assert_eq!(
        ids(hub.delivered_after("client", cursor, 2).await.unwrap()),
        vec![unsent.id]
    );
    assert_eq!(seq(unsent.id).await, Some(5));
}

#[tokio::test]
async fn count_for_run_counts_ordinary_messages_in_any_status() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let new = |call: &str, run: i64, kind| NewOutboxMessage {
        project_id: Some(general.id),
        lane_id: Some(general.lead_lane_id),
        session_id: None,
        run_id: Some(RunId(run)),
        tool_call_id: Some(call.into()),
        kind,
        decision_id: None,
        channel: "client".into(),
        body: json!({ "text": call }),
        urgency: Urgency::Normal,
        not_before: None,
        created_at: T0,
    };
    assert_eq!(hub.count_for_run(RunId(7)).await.unwrap(), 0);
    let sent = hub
        .enqueue(&new("a", 7, OutboxKind::Message))
        .await
        .unwrap();
    hub.mark_sent(sent.id, None, T0).await.unwrap();
    hub.enqueue(&new("b", 7, OutboxKind::Message))
        .await
        .unwrap();
    hub.enqueue(&new("c", 7, OutboxKind::SystemNotice))
        .await
        .unwrap();
    hub.enqueue(&new("d", 7, OutboxKind::Decision))
        .await
        .unwrap();
    hub.enqueue(&new("e", 8, OutboxKind::Message))
        .await
        .unwrap();
    assert_eq!(hub.count_for_run(RunId(7)).await.unwrap(), 2);
    assert_eq!(hub.count_for_run(RunId(8)).await.unwrap(), 1);
}

#[tokio::test]
async fn a_repeated_send_is_handed_to_the_run_that_commits() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lane = general.lead_lane_id;
    let a = machine(&hub, "a").await;
    hub.insert_event(&message(&general, "m1")).await.unwrap();
    let send = |run: &Run| NewOutboxMessage {
        project_id: Some(general.id),
        lane_id: Some(lane),
        session_id: Some(run.session_id),
        run_id: Some(run.id),
        tool_call_id: Some("s1:call-1".into()),
        kind: OutboxKind::Message,
        decision_id: None,
        channel: "client".into(),
        body: json!({ "text": "done" }),
        urgency: Urgency::Normal,
        not_before: None,
        created_at: T0,
    };

    // The first run queues the message, then its machine dies.
    let (_, crashed) = run_pending(&hub, lane, a, T0).await;
    let first = hub.enqueue(&send(&crashed)).await.unwrap();
    assert_eq!(first.run_id, Some(crashed.id));
    let later = T0 + LEASE_TTL;
    hub.expire_leases(later).await.unwrap();

    // The run that retries repeats the send: no new row, and the row is now its own.
    let (lease, retried) = run_pending(&hub, lane, a, later).await;
    let again = hub.enqueue(&send(&retried)).await.unwrap();
    assert_eq!(again.id, first.id, "invariant 6: still one row");
    assert_eq!(again.run_id, Some(retried.id));
    assert_eq!(hub.count_for_run(retried.id).await.unwrap(), 1);
    assert_eq!(hub.count_for_run(crashed.id).await.unwrap(), 0);
    hub.commit_run(&lease, retried.id, RunOutcome::Sent, None, later)
        .await
        .unwrap();
    hub.release_lease(&lease).await.unwrap();

    // Once the sending run has committed, a later repeat does not take the row.
    hub.insert_event(&message(&general, "m2")).await.unwrap();
    let (_, third) = run_pending(&hub, lane, a, later).await;
    let kept = hub.enqueue(&send(&third)).await.unwrap();
    assert_eq!(kept.id, first.id);
    assert_eq!(kept.run_id, Some(retried.id));
    assert_eq!(hub.count_for_run(third.id).await.unwrap(), 0);
}
