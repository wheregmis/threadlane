//! Behavior of every store through its interface, on an in-memory database.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use threadlane_core::ids::{
    AgentId, DecisionId, EventId, LaneId, MachineId, OutboxId, RunId, TaskId,
};
use threadlane_core::model::*;
use threadlane_core::store::*;
use threadlane_core::time::{Clock, SystemClock, Timestamp};
use threadlane_store::{Bootstrap, HubDb, TASK_SOURCE, UNWOKEN_REASON};

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
    hub.commit_run(
        &lease,
        run.id,
        RunOutcome::Sent,
        Some("told them"),
        None,
        T0,
    )
    .await
    .unwrap();
    for id in &events {
        let event = hub.event(*id).await.unwrap().unwrap();
        assert_eq!(event.status, EventStatus::Done);
        assert_eq!(event.claimed_by_run, Some(run.id));
    }
    // Invariant 3: a committed run cannot be committed again.
    assert!(matches!(
        hub.commit_run(&lease, run.id, RunOutcome::Silent, None, None, T0)
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
    hub.commit_run(&lease, resumed.id, RunOutcome::Silent, None, None, T0)
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
        hub.commit_run(&lease, run.id, RunOutcome::Sent, None, None, T0)
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
            requested: true,
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
            requested: true,
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
            recurrence: Some(Recurrence {
                rule: Rule::Daily { hour: 9, minute: 0 },
                zone: "Europe/Berlin".into(),
            }),
            instruction: "daily summary".into(),
            created_by: TimerCreator::User,
            created_by_call: None,
            requested: true,
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
async fn timers_list_by_project_pause_resume_and_fire_requested_as_created() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let timer = |requested: bool, call: &str| NewTimer {
        project_id: general.id,
        lane_id: general.lead_lane_id,
        agent_id: None,
        task_ref: None,
        category: TimerCategory::FollowUp,
        due_at: T0,
        recurrence: None,
        instruction: "look again".into(),
        created_by: TimerCreator::Agent,
        created_by_call: Some(call.into()),
        requested,
    };
    let unrequested = hub.create_timer(&timer(false, "a")).await.unwrap();
    let requested = hub.create_timer(&timer(true, "b")).await.unwrap();
    let ended = hub.create_timer(&timer(true, "c")).await.unwrap();
    hub.cancel_timer(ended.id).await.unwrap();
    let listed: Vec<_> = hub
        .project_timers(general.id)
        .await
        .unwrap()
        .into_iter()
        .map(|timer| timer.id)
        .collect();
    assert_eq!(
        listed,
        [unrequested.id, requested.id],
        "ended timers are left out"
    );

    let paused = hub.pause_timer(requested.id).await.unwrap();
    assert_eq!(paused.status, TimerStatus::Paused);
    assert_eq!(hub.pause_timer(requested.id).await.unwrap(), paused);
    assert_eq!(
        hub.due_timers(T0).await.unwrap().len(),
        1,
        "paused is not due"
    );
    assert_eq!(hub.project_timers(general.id).await.unwrap().len(), 2);
    let later = T0 + Duration::from_secs(60);
    let resumed = hub.resume_timer(requested.id, Some(later)).await.unwrap();
    assert_eq!(
        (resumed.status, resumed.due_at),
        (TimerStatus::Active, later)
    );
    assert!(matches!(
        hub.pause_timer(ended.id).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        hub.resume_timer(ended.id, None).await,
        Err(StoreError::Conflict(_))
    ));

    hub.fire(unrequested.id, T0, None, T0).await.unwrap();
    hub.fire(requested.id, later, None, later).await.unwrap();
    let events = hub.pending_events(general.lead_lane_id).await.unwrap();
    assert_eq!(
        events.iter().map(|e| e.requested).collect::<Vec<_>>(),
        [false, true]
    );
    assert_eq!(events[0].payload["created_by"], "agent");
    assert_eq!(events[1].payload["occurrence"], json!(later));
}

/// An agent bound to `project` that may run `max_parallel` tasks at once there.
async fn engineer(hub: &HubDb, project: &Project, max_parallel: u32) -> AgentId {
    let agent = hub
        .create_agent(&NewAgent::named(
            format!("Engineer {} {max_parallel}", project.id),
            "engineer",
        ))
        .await
        .unwrap();
    hub.bind(&NewBinding {
        project_id: project.id,
        agent_id: agent.id,
        mode: Mode::OnRequest,
        is_lead: false,
        max_parallel,
    })
    .await
    .unwrap();
    agent.id
}

fn new_task(project: &Project, agent: AgentId, title: &str, depends_on: Vec<TaskId>) -> NewTask {
    NewTask {
        project_id: project.id,
        agent_id: agent,
        title: title.into(),
        goal: "done when tests pass".into(),
        repositories: Vec::new(),
        parent_task_id: None,
        depends_on,
        origin: TaskOrigin::User,
        status: TaskStatus::Queued,
        priority: 0,
        created_by_call: Some(format!("call-{}-{title}", project.id)),
        created_at: T0,
    }
}

async fn task(hub: &HubDb, id: TaskId) -> Task {
    hub.task(id).await.unwrap().unwrap()
}

/// The `task_result` events waiting in the project's lead lane.
async fn task_results(hub: &HubDb, project: &Project) -> Vec<Event> {
    hub.pending_events(project.lead_lane_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == EventKind::TaskResult)
        .collect()
}

#[tokio::test]
async fn tasks_get_a_lane_and_a_start_event_once_per_creating_call() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let first = hub
        .create_task(None, &new_task(&general, agent, "first", vec![]))
        .await
        .unwrap();
    assert_eq!(first.id.to_string(), "T-0001");
    assert_eq!(first.status, TaskStatus::Queued);
    let again = hub
        .create_task(
            None,
            &NewTask {
                title: "ignored".into(),
                ..new_task(&general, agent, "first", vec![])
            },
        )
        .await
        .unwrap();
    assert_eq!(again, first, "creation is idempotent per call");
    let second = hub
        .create_task(None, &new_task(&general, agent, "second", vec![first.id]))
        .await
        .unwrap();
    assert_eq!(second.depends_on, vec![first.id]);
    assert_eq!(
        hub.tasks(general.id).await.unwrap(),
        vec![first.clone(), second]
    );

    let lane = hub.lane(first.lane_id).await.unwrap().unwrap();
    assert_eq!(lane.kind, LaneKind::Task);
    assert_eq!(lane.task_id, Some(first.id));
    let start = hub.pending_events(first.lane_id).await.unwrap();
    assert_eq!(
        start.len(),
        1,
        "one start event, however often it was created"
    );
    assert_eq!(start[0].kind, EventKind::TaskStart);
    assert_eq!(start[0].source, TASK_SOURCE);
    assert_eq!(
        start[0].source_event_id,
        format!("start:{}", first.id.get())
    );
    assert!(start[0].requested, "a task you asked for is requested work");

    let unasked = hub
        .create_task(
            None,
            &NewTask {
                origin: TaskOrigin::Agent,
                ..new_task(&general, agent, "unasked", vec![])
            },
        )
        .await
        .unwrap();
    assert!(!hub.pending_events(unasked.lane_id).await.unwrap()[0].requested);

    let proposed = hub
        .create_task(
            None,
            &NewTask {
                status: TaskStatus::Proposed,
                ..new_task(&general, agent, "proposed", vec![])
            },
        )
        .await
        .unwrap();
    assert!(
        hub.pending_events(proposed.lane_id)
            .await
            .unwrap()
            .is_empty()
    );
    hub.set_task_status(proposed.id, TaskStatus::Queued, None)
        .await
        .unwrap();
    assert_eq!(hub.pending_events(proposed.lane_id).await.unwrap().len(), 1);

    let other = hub
        .create_project(&NewProject {
            name: "Other".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    for bad in [
        new_task(&other, agent, "across", vec![first.id]),
        NewTask {
            parent_task_id: Some(first.id),
            ..new_task(&other, agent, "child", vec![])
        },
        new_task(&general, agent, "dangling", vec![TaskId(99)]),
    ] {
        let refused = hub.create_task(None, &bad).await;
        assert!(
            matches!(refused, Err(StoreError::Invalid(_))),
            "{refused:?}"
        );
    }
    assert!(hub.tasks(other.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_task_status_changes_only_along_its_lifecycle() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 0).await;
    // How to bring a fresh queued task to each status.
    let path = |status: TaskStatus| -> Vec<TaskStatus> {
        use TaskStatus::*;
        match status {
            Proposed | Queued => vec![],
            Running => vec![Running],
            InReview | WaitingOnYou | Sleeping | Done | Failed => vec![Running, status],
            Cancelled => vec![Cancelled],
        }
    };
    let mut ended = Vec::new();
    for (n, &from) in TaskStatus::ALL.iter().enumerate() {
        for (m, &to) in TaskStatus::ALL.iter().enumerate() {
            let created = hub
                .create_task(
                    None,
                    &NewTask {
                        status: if from == TaskStatus::Proposed {
                            from
                        } else {
                            TaskStatus::Queued
                        },
                        ..new_task(&general, agent, &format!("{n}-{m}"), vec![])
                    },
                )
                .await
                .unwrap();
            for step in path(from) {
                hub.set_task_status(created.id, step, None).await.unwrap();
            }
            assert_eq!(task(&hub, created.id).await.status, from);
            let moved = hub.set_task_status(created.id, to, Some("because")).await;
            if from.can_become(to) {
                assert_eq!(moved, Ok(()), "{from} to {to}");
                assert_eq!(task(&hub, created.id).await.status, to);
            } else {
                assert!(
                    matches!(moved, Err(StoreError::Conflict(_))),
                    "{from} to {to}: {moved:?}"
                );
                assert_eq!(task(&hub, created.id).await.status, from, "{from} to {to}");
            }
            if task(&hub, created.id).await.status.is_terminal() {
                ended.push(created.id);
            }
        }
    }
    let mut reported: Vec<TaskId> = task_results(&hub, &general)
        .await
        .iter()
        .map(|event| serde_json::from_value(event.payload["task_id"].clone()).unwrap())
        .collect();
    reported.sort();
    ended.sort();
    assert_eq!(reported, ended, "every task that ended was reported once");
}

#[tokio::test]
async fn the_run_cycle_takes_a_task_from_queued_to_done_and_reports_it_once() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let a = machine(&hub, "a").await;
    let created = hub
        .create_task(None, &new_task(&general, agent, "fix", vec![]))
        .await
        .unwrap();

    let (lease, run) = run_pending(&hub, created.lane_id, a, T0).await;
    let running = task(&hub, created.id).await;
    assert_eq!(running.status, TaskStatus::Running);
    assert_eq!(running.last_machine_id, Some(a));

    let result = json!({ "summary": "fixed" });
    let unwritten = hub.finish_task(&lease, &result).await;
    assert!(
        matches!(&unwritten, Err(StoreError::Invalid(message))
            if message.starts_with("projects/1/tasks/T-0001.md does not exist")),
        "a task finishes only once its progress file exists: {unwritten:?}"
    );
    write_progress(&hub, &lease, &run, &created).await;
    let finished = hub.finish_task(&lease, &result).await.unwrap();
    assert_eq!(finished.result, Some(result.clone()));
    assert_eq!(
        finished.status,
        TaskStatus::Running,
        "done only when the run commits"
    );
    hub.finish_task(&lease, &result).await.unwrap();
    assert!(task_results(&hub, &general).await.is_empty());

    hub.commit_run(&lease, run.id, RunOutcome::Silent, None, None, T0)
        .await
        .unwrap();
    assert_eq!(task(&hub, created.id).await.status, TaskStatus::Done);
    let results = task_results(&hub, &general).await;
    assert_eq!(results.len(), 1);
    let event = &results[0];
    assert_eq!(event.source, TASK_SOURCE);
    assert_eq!(
        event.source_event_id,
        format!("result:{}", created.id.get())
    );
    assert_eq!(event.trust, Trust::System);
    assert!(event.requested);
    assert_eq!(event.payload["task"], "T-0001");
    assert_eq!(event.payload["status"], "done");
    assert_eq!(event.payload["result"], result);

    let late = hub.finish_task(&lease, &result).await;
    assert!(matches!(late, Err(StoreError::Conflict(_))), "{late:?}");
    hub.release_lease(&lease).await.unwrap();
    assert_eq!(task(&hub, created.id).await.status, TaskStatus::Done);
}

#[tokio::test]
async fn a_run_that_does_not_finish_leaves_the_task_sleeping_until_an_event_arrives() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let a = machine(&hub, "a").await;
    let created = hub
        .create_task(None, &new_task(&general, agent, "wait", vec![]))
        .await
        .unwrap();
    let event_for = |id: &str| NewEvent {
        lane_id: created.lane_id,
        ..message(&general, id)
    };

    let (lease, run) = run_pending(&hub, created.lane_id, a, T0).await;
    assert!(hub.left_unwoken(created.lane_id).await.unwrap());
    hub.commit_run(&lease, run.id, RunOutcome::Silent, None, None, T0)
        .await
        .unwrap();
    // Nothing will wake it: it says so, and the lead is told once.
    let sleeping = task(&hub, created.id).await;
    assert_eq!(sleeping.status, TaskStatus::Sleeping);
    assert_eq!(sleeping.waiting_reason.as_deref(), Some(UNWOKEN_REASON));
    let notices = task_results(&hub, &general).await;
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].source_event_id,
        format!("unwoken:{}", run.id.get())
    );
    assert_eq!(notices[0].payload["status"], "sleeping");
    assert_eq!(notices[0].payload["reason"], UNWOKEN_REASON);
    hub.release_lease(&lease).await.unwrap();
    hub.insert_event(&event_for("wake")).await.unwrap();
    let woken = task(&hub, created.id).await;
    assert_eq!(woken.status, TaskStatus::Queued);
    assert_eq!(woken.waiting_reason, None, "woken, it no longer says so");

    // An event that arrives during the run queues the task at commit.
    let (lease, run) = run_pending(&hub, created.lane_id, a, T0).await;
    hub.insert_event(&event_for("meanwhile")).await.unwrap();
    assert_eq!(task(&hub, created.id).await.status, TaskStatus::Running);
    hub.commit_run(&lease, run.id, RunOutcome::Silent, None, None, T0)
        .await
        .unwrap();
    assert_eq!(task(&hub, created.id).await.status, TaskStatus::Queued);
    assert_eq!(task_results(&hub, &general).await.len(), 1, "no new notice");
}

#[tokio::test]
async fn a_task_has_something_to_wake_it_with_a_timer_event_decision_or_result() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let a = machine(&hub, "a").await;
    let created = hub
        .create_task(None, &new_task(&general, agent, "wait", vec![]))
        .await
        .unwrap();
    let lane = created.lane_id;
    let (lease, run) = run_pending(&hub, lane, a, T0).await;
    assert!(hub.left_unwoken(lane).await.unwrap(), "nothing yet");
    assert!(
        !hub.left_unwoken(general.lead_lane_id).await.unwrap(),
        "a lead lane is never left unwoken"
    );

    // An event that arrives during the run.
    hub.insert_event(&NewEvent {
        lane_id: lane,
        ..message(&general, "meanwhile")
    })
    .await
    .unwrap();
    assert!(!hub.left_unwoken(lane).await.unwrap(), "a pending event");
    hub.commit_run(&lease, run.id, RunOutcome::Silent, None, None, T0)
        .await
        .unwrap();
    hub.release_lease(&lease).await.unwrap();

    // A follow-up of its own; one paused does not count.
    let (lease, _run) = run_pending(&hub, lane, a, T0).await;
    assert!(hub.left_unwoken(lane).await.unwrap());
    let timer = hub
        .create_timer(&NewTimer {
            project_id: general.id,
            lane_id: lane,
            agent_id: None,
            task_ref: Some(created.id),
            category: TimerCategory::FollowUp,
            due_at: T0,
            recurrence: None,
            instruction: "look at the checks".into(),
            created_by: TimerCreator::Agent,
            created_by_call: Some("call-f".into()),
            requested: true,
        })
        .await
        .unwrap();
    assert!(
        !hub.left_unwoken(lane).await.unwrap(),
        "an active follow-up"
    );
    hub.pause_timer(timer.id).await.unwrap();
    assert!(
        hub.left_unwoken(lane).await.unwrap(),
        "a paused one wakes nothing"
    );

    // A decision waiting on you.
    hub.open_decision(
        Some(&lease),
        &NewDecision {
            project_id: general.id,
            lane_id: lane,
            task_id: Some(created.id),
            session_id: None,
            run_id: None,
            tool_call_id: Some("call-d".into()),
            tool: "publish_branch".into(),
            arguments: json!({ "repository": "app" }),
            arguments_hash: "sha256:abc".into(),
            kind: DecisionKind::Approval,
            question: "Push the branch?".into(),
            context: String::new(),
            options: vec![DecisionOption {
                label: "Push".into(),
                effect: json!({ "approve": true }),
            }],
            recommended: None,
            offers_standing_rule: false,
            expires_at: None,
            created_at: T0,
        },
    )
    .await
    .unwrap();
    assert!(!hub.left_unwoken(lane).await.unwrap(), "an open decision");
}

#[tokio::test]
async fn a_failed_run_queues_the_task_again_and_dead_events_fail_it_once() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let a = machine(&hub, "a").await;
    let created = hub
        .create_task(None, &new_task(&general, agent, "flaky", vec![]))
        .await
        .unwrap();

    let (lease, run) = run_pending(&hub, created.lane_id, a, T0).await;
    let failed = hub
        .fail_run(&lease, run.id, "boom", retry(true, 2), T0)
        .await
        .unwrap();
    assert_eq!(failed.retried.len(), 1);
    assert_eq!(task(&hub, created.id).await.status, TaskStatus::Queued);
    hub.release_lease(&lease).await.unwrap();

    let later = T0 + Duration::from_secs(60);
    let (lease, run) = run_pending(&hub, created.lane_id, a, later).await;
    let failed = hub
        .fail_run(&lease, run.id, "boom again", retry(true, 2), later)
        .await
        .unwrap();
    assert_eq!(failed.dead.len(), 1);
    assert_eq!(task(&hub, created.id).await.status, TaskStatus::Failed);
    let told = hub.pending_events(general.lead_lane_id).await.unwrap();
    assert_eq!(told.len(), 1, "only the task's result: {told:?}");
    assert_eq!(told[0].kind, EventKind::TaskResult);
    assert_eq!(told[0].payload["status"], "failed");
    assert_eq!(told[0].payload["error"], "boom again");
    assert!(told[0].requested);
    assert!(
        notices(&hub).await.is_empty(),
        "the lead can still tell you"
    );

    let again = hub
        .fail_run(&lease, run.id, "boom again", retry(true, 2), later)
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
async fn a_lost_lease_queues_a_running_task_again() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let a = machine(&hub, "a").await;
    let created = hub
        .create_task(None, &new_task(&general, agent, "lost", vec![]))
        .await
        .unwrap();
    run_pending(&hub, created.lane_id, a, T0).await;
    hub.expire_leases(T0 + LEASE_TTL).await.unwrap();
    assert_eq!(task(&hub, created.id).await.status, TaskStatus::Queued);
    assert_eq!(hub.pending_events(created.lane_id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_cancelled_task_is_reported_once_and_changes_no_more() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let a = machine(&hub, "a").await;
    let created = hub
        .create_task(None, &new_task(&general, agent, "drop", vec![]))
        .await
        .unwrap();
    let lead = hub
        .grant_lease(general.lead_lane_id, a, T0, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();

    let updated = hub
        .update_task(&lead, created.id, Some("drop it"), None, Some(3))
        .await
        .unwrap();
    assert_eq!(updated.title, "drop it");
    assert_eq!(updated.goal, created.goal, "an absent field is kept");
    assert_eq!(updated.priority, 3);

    let cancelled = hub.cancel_task(Some(&lead), created.id, T0).await.unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    assert_eq!(
        hub.cancel_task(Some(&lead), created.id, T0).await.unwrap(),
        cancelled,
        "cancelling again changes nothing"
    );
    let results = task_results(&hub, &general).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].payload["status"], "cancelled");

    let changed = hub
        .update_task(&lead, created.id, Some("again"), None, None)
        .await;
    assert!(
        matches!(changed, Err(StoreError::Conflict(_))),
        "{changed:?}"
    );

    let other = hub
        .create_project(&NewProject {
            name: "Other".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let foreign = hub
        .grant_lease(other.lead_lane_id, a, T0, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    let fresh = hub
        .create_task(None, &new_task(&general, agent, "keep", vec![]))
        .await
        .unwrap();
    let across = hub.cancel_task(Some(&foreign), fresh.id, T0).await;
    assert!(matches!(across, Err(StoreError::Invalid(_))), "{across:?}");

    hub.release_lease(&lead).await.unwrap();
    let stale = hub.cancel_task(Some(&lead), fresh.id, T0).await;
    assert!(
        matches!(stale, Err(StoreError::StaleLease { .. })),
        "{stale:?}"
    );
    let stale = hub
        .update_task(&lead, fresh.id, Some("x"), None, None)
        .await;
    assert!(
        matches!(stale, Err(StoreError::StaleLease { .. })),
        "{stale:?}"
    );
    let stale = hub
        .create_task(Some(&lead), &new_task(&general, agent, "late", vec![]))
        .await;
    assert!(
        matches!(stale, Err(StoreError::StaleLease { .. })),
        "{stale:?}"
    );
    assert_eq!(task(&hub, fresh.id).await.status, TaskStatus::Queued);
    assert_eq!(hub.tasks(general.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn only_a_running_task_s_own_lane_finishes_it_under_its_current_lease() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let a = machine(&hub, "a").await;
    let created = hub
        .create_task(None, &new_task(&general, agent, "own", vec![]))
        .await
        .unwrap();
    let lead = hub
        .grant_lease(general.lead_lane_id, a, T0, LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    let result = json!({ "summary": "x" });
    let not_a_task = hub.finish_task(&lead, &result).await;
    assert!(
        matches!(not_a_task, Err(StoreError::Invalid(_))),
        "{not_a_task:?}"
    );

    let (lease, _) = run_pending(&hub, created.lane_id, a, T0).await;
    hub.expire_leases(T0 + LEASE_TTL).await.unwrap();
    let stale = hub.finish_task(&lease, &result).await;
    assert!(
        matches!(stale, Err(StoreError::StaleLease { .. })),
        "{stale:?}"
    );
    assert_eq!(task(&hub, created.id).await.result, None);
}

#[tokio::test]
async fn a_task_lane_is_granted_only_when_its_task_is_eligible() {
    let hub = hub().await;
    let a = machine(&hub, "a").await;
    let grant = |lane: LaneId| hub.grant_lease(lane, a, T0, LEASE_TTL);

    // A project limit of two, and an agent without a limit of its own.
    let limited = hub
        .create_project(&NewProject {
            name: "Limited".into(),
            max_parallel_tasks: 2,
            ..NewProject::default()
        })
        .await
        .unwrap();
    let agent = engineer(&hub, &limited, 0).await;
    let mut ids = Vec::new();
    for title in ["a", "b", "c"] {
        ids.push(
            hub.create_task(None, &new_task(&limited, agent, title, vec![]))
                .await
                .unwrap(),
        );
    }
    let dependent = hub
        .create_task(None, &new_task(&limited, agent, "d", vec![ids[0].id]))
        .await
        .unwrap();
    let [first, second, third] = [&ids[0], &ids[1], &ids[2]];

    let lease = grant(first.lane_id).await.unwrap().expect("granted");
    assert!(grant(second.lane_id).await.unwrap().is_some());
    assert_eq!(grant(third.lane_id).await.unwrap(), None);
    assert_eq!(
        task(&hub, third.id).await.waiting_reason.as_deref(),
        Some("the project already runs its limit of 2 tasks at once")
    );
    assert_eq!(grant(dependent.lane_id).await.unwrap(), None);
    assert_eq!(
        task(&hub, dependent.id).await.waiting_reason.as_deref(),
        Some("waiting for T-0001 to be done")
    );
    let ready = hub.ready_lanes(T0).await.unwrap();
    assert!(ready.contains(&third.lane_id) && ready.contains(&dependent.lane_id));

    // The first task finishes: its slot goes to the third.
    let events = hub
        .pending_events(first.lane_id)
        .await
        .unwrap()
        .iter()
        .map(|event| event.id)
        .collect();
    let run = hub.start_run(&lease, &new_run(events, T0)).await.unwrap();
    write_progress(&hub, &lease, &run, first).await;
    hub.finish_task(&lease, &json!({ "summary": "a" }))
        .await
        .unwrap();
    hub.commit_run(&lease, run.id, RunOutcome::Silent, None, None, T0)
        .await
        .unwrap();
    hub.release_lease(&lease).await.unwrap();
    assert!(grant(third.lane_id).await.unwrap().is_some());
    assert_eq!(
        task(&hub, third.id).await.waiting_reason,
        None,
        "cleared on grant"
    );
    assert_eq!(grant(dependent.lane_id).await.unwrap(), None);
    assert_eq!(
        task(&hub, dependent.id).await.waiting_reason.as_deref(),
        Some("the project already runs its limit of 2 tasks at once"),
        "its dependency is done; now it waits for a slot"
    );

    // No project limit (0), and an agent limit of one.
    let unlimited = hub
        .create_project(&NewProject {
            name: "Unlimited".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    assert_eq!(unlimited.max_parallel_tasks, 0);
    let single = engineer(&hub, &unlimited, 1).await;
    let one = hub
        .create_task(None, &new_task(&unlimited, single, "one", vec![]))
        .await
        .unwrap();
    let two = hub
        .create_task(None, &new_task(&unlimited, single, "two", vec![]))
        .await
        .unwrap();
    assert!(
        grant(one.lane_id).await.unwrap().is_none(),
        "other projects occupy both machine slots"
    );
    let b = machine(&hub, "b").await;
    let other = |lane| hub.grant_lease(lane, b, T0, LEASE_TTL);
    assert!(other(one.lane_id).await.unwrap().is_some());
    assert!(
        other(two.lane_id).await.unwrap().is_some(),
        "the same agent can use both slots"
    );

    hub.set_machine_slots(b, 1).await.unwrap();
    let three = hub
        .create_task(None, &new_task(&unlimited, single, "three", vec![]))
        .await
        .unwrap();
    assert!(other(three.lane_id).await.unwrap().is_none());
    let later = T0 + LEASE_TTL;
    assert!(
        hub.grant_lease(three.lane_id, b, later, LEASE_TTL)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(hub.machine(b).await.unwrap().unwrap().max_parallel, 1);

    // A paused project's tasks wait and say so.
    let paused = hub
        .create_project(&NewProject {
            name: "Paused".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let idle = engineer(&hub, &paused, 0).await;
    let held = hub
        .create_task(None, &new_task(&paused, idle, "held", vec![]))
        .await
        .unwrap();
    hub.set_project_paused(paused.id, true).await.unwrap();
    assert!(hub.ready_lanes(T0).await.unwrap().contains(&held.lane_id));
    assert_eq!(grant(held.lane_id).await.unwrap(), None);
    assert_eq!(
        task(&hub, held.id).await.waiting_reason.as_deref(),
        Some("the project is paused")
    );
    // A task that ended is granted anyway, so its events can be skipped.
    hub.cancel_task(None, held.id, T0).await.unwrap();
    assert!(
        hub.grant_lease(held.lane_id, a, later, LEASE_TTL)
            .await
            .unwrap()
            .is_some()
    );
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

/// Writes `task`'s progress file in `run`, which `finish_task` requires.
async fn write_progress(hub: &HubDb, lease: &Lease, run: &Run, task: &Task) {
    let by = MemoryAuthor {
        lease: *lease,
        run_id: run.id,
        call_key: "progress".into(),
        at: T0,
    };
    let path = threadlane_core::memory::progress_path(task.project_id, task.id);
    hub.write(&by, &path, &format!("# {}\nWhat is done.", task.id))
        .await
        .unwrap();
}

/// A run in General's lead lane, and an author for its memory writes.
async fn memory_author(hub: &HubDb) -> (MemoryAuthor, Run) {
    let general = hub.general_project().await.unwrap();
    let a = machine(hub, "a").await;
    hub.insert_event(&message(&general, "m-1")).await.unwrap();
    let (lease, run) = run_pending(hub, general.lead_lane_id, a, T0).await;
    let by = MemoryAuthor {
        lease,
        run_id: run.id,
        call_key: "call-1".into(),
        at: T0,
    };
    (by, run)
}

/// `by` keyed on another call.
fn keyed(by: &MemoryAuthor, call: &str) -> MemoryAuthor {
    MemoryAuthor {
        call_key: call.into(),
        ..by.clone()
    }
}

#[tokio::test]
async fn memory_writes_are_versioned_with_their_run_and_written_once_per_call() {
    let hub = hub().await;
    let (by, run) = memory_author(&hub).await;
    let path = "projects/1/repos/app.md";
    assert_eq!(hub.write(&by, path, "v1").await.unwrap().version, 1);
    // The same call again writes nothing more.
    assert_eq!(hub.write(&by, path, "v1").await.unwrap().version, 1);
    assert_eq!(
        hub.write(&keyed(&by, "c2"), path, "v2")
            .await
            .unwrap()
            .version,
        2
    );
    let edited = hub.edit(&keyed(&by, "c3"), path, "v2", "v3").await.unwrap();
    assert_eq!((edited.version, edited.content.as_str()), (3, "v3"));
    assert_eq!(
        hub.edit(&keyed(&by, "c3"), path, "v2", "v3").await.unwrap(),
        edited,
        "a repeated edit returns what it wrote, although the text is gone"
    );
    for (old, why) in [("v9", "does not occur"), ("", "does not occur")] {
        let refused = hub.edit(&keyed(&by, "c4"), path, old, "x").await;
        assert!(
            matches!(&refused, Err(StoreError::Invalid(message)) if message.contains(why)),
            "{refused:?}"
        );
    }
    hub.write(&keyed(&by, "c5"), path, "a a").await.unwrap();
    assert!(matches!(
        hub.edit(&keyed(&by, "c6"), path, "a", "b").await,
        Err(StoreError::Invalid(message)) if message.contains("2 times")
    ));
    hub.write(&keyed(&by, "c7"), "projects/2/repos/other.md", "x")
        .await
        .unwrap();
    assert_eq!(hub.list("projects/1/").await.unwrap().len(), 1);

    let deleted = hub.delete(&keyed(&by, "c8"), path).await.unwrap();
    assert!(deleted.deleted);
    assert_eq!(hub.read(path).await.unwrap(), None);
    assert_eq!(hub.delete(&keyed(&by, "c8"), path).await.unwrap(), deleted);
    assert!(matches!(
        hub.delete(&keyed(&by, "c9"), path).await,
        Err(StoreError::NotFound { .. })
    ));
    let history: Vec<_> = hub
        .history(path)
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.version, f.deleted, f.written_by_run))
        .collect();
    let run = Some(run.id);
    assert_eq!(
        history,
        vec![
            (1, false, run),
            (2, false, run),
            (3, false, run),
            (4, false, run),
            (5, true, run)
        ]
    );
    assert!(hub.read("../escape").await.is_err());

    // A write under a lease since granted to someone else changes nothing.
    hub.release_lease(&by.lease).await.unwrap();
    assert!(matches!(
        hub.write(&keyed(&by, "c10"), path, "late").await,
        Err(StoreError::StaleLease { .. })
    ));
    assert_eq!(hub.read(path).await.unwrap(), None);
}

#[tokio::test]
async fn your_own_changes_need_no_run_keep_the_rules_and_rewrite_the_index() {
    let hub = hub().await;
    let (by, run) = memory_author(&hub).await;
    let note = "projects/1/notes/build.md";
    hub.write(&by, note, "# Build\nBy the lead.").await.unwrap();

    // You may write instructions directly, as a new version with no run.
    let mine = hub
        .write_by_owner("projects/1/instructions.md", "Be brief.", T0)
        .await
        .unwrap();
    assert_eq!((mine.version, mine.written_by_run), (1, None));
    let edited = hub
        .write_by_owner(note, "# Build\nBy you.", T0)
        .await
        .unwrap();
    assert_eq!((edited.version, edited.written_by_run), (2, None));
    let index = hub
        .read("projects/1/notes/INDEX.md")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(index.content, "- [Build](build.md): By you.\n");
    assert_eq!(index.written_by_run, None);
    let authors: Vec<_> = hub
        .history(note)
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.written_by_run)
        .collect();
    assert_eq!(authors, [Some(run.id), None]);

    // The same budgets, title rules and index protection as a run's.
    let over = "x".repeat(threadlane_core::memory::INSTRUCTIONS_BUDGET + 1);
    for (path, content, why) in [
        ("space/instructions.md", over.as_str(), "trim it"),
        ("projects/1/notes/a.md", "no description", "title line"),
        (
            "projects/1/notes/INDEX.md",
            "# A\nForged.",
            "written by the system",
        ),
        ("projects/1/../2/a.md", "x", "memory path"),
    ] {
        assert!(
            matches!(
                hub.write_by_owner(path, content, T0).await,
                Err(StoreError::Invalid(message)) if message.contains(why)
            ),
            "{path}"
        );
    }

    let deleted = hub.delete_by_owner(note, T0).await.unwrap();
    assert!(deleted.deleted && deleted.written_by_run.is_none());
    assert_eq!(hub.read("projects/1/notes/INDEX.md").await.unwrap(), None);
    assert!(matches!(
        hub.delete_by_owner(note, T0).await,
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        hub.delete_by_owner("projects/1/notes/INDEX.md", T0).await,
        Err(StoreError::Invalid(_))
    ));
}

#[tokio::test]
async fn a_rename_is_a_deletion_and_a_write_and_keeps_both_histories() {
    let hub = hub().await;
    let (by, _) = memory_author(&hub).await;
    let (from, to) = ("projects/1/repos/old.md", "projects/1/repos/new.md");
    hub.write(&keyed(&by, "c1"), from, "one").await.unwrap();
    hub.write(&keyed(&by, "c2"), from, "two").await.unwrap();
    hub.write(&keyed(&by, "c3"), to, "earlier").await.unwrap();
    assert!(matches!(
        hub.rename(&keyed(&by, "c4"), from, to).await,
        Err(StoreError::Invalid(message)) if message.contains("already exists")
    ));
    hub.delete(&keyed(&by, "c5"), to).await.unwrap();
    let moved = hub.rename(&keyed(&by, "c6"), from, to).await.unwrap();
    assert_eq!((moved.version, moved.content.as_str()), (3, "two"));
    assert_eq!(
        hub.rename(&keyed(&by, "c6"), from, to).await.unwrap(),
        moved
    );
    assert_eq!(hub.read(from).await.unwrap(), None);
    assert_eq!(hub.read(to).await.unwrap().unwrap().content, "two");
    let versions = |history: Vec<MemoryFile>| -> Vec<(String, bool)> {
        history
            .into_iter()
            .map(|f| (f.content, f.deleted))
            .collect()
    };
    assert_eq!(
        versions(hub.history(from).await.unwrap()),
        [
            ("one".into(), false),
            ("two".into(), false),
            (String::new(), true)
        ]
    );
    assert_eq!(
        versions(hub.history(to).await.unwrap()),
        [
            ("earlier".into(), false),
            (String::new(), true),
            ("two".into(), false)
        ]
    );
    assert!(matches!(
        hub.rename(&keyed(&by, "c7"), from, "projects/1/repos/x.md")
            .await,
        Err(StoreError::NotFound { .. })
    ));
}

#[tokio::test]
async fn preloaded_files_keep_their_budgets_and_indexes_are_not_written_directly() {
    let hub = hub().await;
    let (by, _) = memory_author(&hub).await;
    let refused = |result: StoreResult<MemoryFile>, why: &str| match result {
        Err(StoreError::Invalid(message)) => assert!(message.contains(why), "{message}"),
        other => panic!("expected a refusal saying {why:?}, got {other:?}"),
    };
    let over = "x".repeat(threadlane_core::memory::INSTRUCTIONS_BUDGET + 1);
    for path in ["space/instructions.md", "projects/1/instructions.md"] {
        refused(hub.write(&by, path, &over).await, "trim it");
    }
    refused(
        hub.write(
            &by,
            "projects/1/tasks/T-0001.md",
            &format!("# T-0001\nLeft.\n{over}"),
        )
        .await,
        "Trim it",
    );
    refused(
        hub.write(&by, "projects/1/notes/a.md", "no description")
            .await,
        "title line",
    );
    let long_title = format!("# {}\nAbout it.", "t".repeat(300));
    refused(
        hub.write(&by, "space/notes/a.md", &long_title).await,
        "shorten",
    );
    refused(
        hub.write(&by, "space/notes/INDEX.md", "# Mine\nForged.")
            .await,
        "written by the system",
    );

    // An edit that would take a file over its budget is refused too.
    let path = "space/instructions.md";
    hub.write(&by, path, "Be brief.").await.unwrap();
    refused(
        hub.edit(&keyed(&by, "c2"), path, "brief", &over).await,
        "trim it",
    );
    // Moving a note to where no index lists it needs no title; moving a
    // file into notes does.
    hub.write(&keyed(&by, "c3"), "space/notes/b.md", "# B\nAbout b.")
        .await
        .unwrap();
    refused(
        hub.rename(
            &keyed(&by, "c4"),
            "space/instructions.md",
            "space/notes/c.md",
        )
        .await,
        "title line",
    );
    refused(
        hub.rename(
            &keyed(&by, "c5"),
            "space/notes/INDEX.md",
            "space/notes/i.md",
        )
        .await,
        "written by the system",
    );
    assert_eq!(hub.read(path).await.unwrap().unwrap().content, "Be brief.");
}

#[tokio::test]
async fn indexes_are_rewritten_with_every_change_and_their_bytes_depend_only_on_the_files() {
    let notes = [
        (
            "space/notes/style.md",
            "# Style\nShort sentences.\n\nMore detail.",
        ),
        ("space/notes/about.md", "# About you\nYou work on payments."),
    ];
    let index = "space/notes/INDEX.md";
    let expected = "- [About you](about.md): You work on payments.\n\
                    - [Style](style.md): Short sentences.\n";
    let mut bytes = Vec::new();
    for order in [[0, 1], [1, 0]] {
        let hub = hub().await;
        let (by, run) = memory_author(&hub).await;
        for (n, &i) in order.iter().enumerate() {
            let (path, content) = notes[i];
            hub.write(&keyed(&by, &format!("c{n}")), path, content)
                .await
                .unwrap();
        }
        let written = hub.read(index).await.unwrap().unwrap();
        assert_eq!(written.written_by_run, Some(run.id));
        bytes.push(written.content);
    }
    assert_eq!(bytes[0], expected);
    assert_eq!(bytes[1], expected, "the same files give the same bytes");

    let hub = hub().await;
    let (by, _) = memory_author(&hub).await;
    for (n, (path, content)) in notes.iter().enumerate() {
        hub.write(&keyed(&by, &format!("c{n}")), path, content)
            .await
            .unwrap();
    }
    let versions = || async { hub.history(index).await.unwrap().len() };
    assert_eq!(versions().await, 2);
    // A change that leaves the title and description alone leaves the index.
    hub.edit(&keyed(&by, "e1"), notes[0].0, "More detail.", "Less.")
        .await
        .unwrap();
    assert_eq!(versions().await, 2);
    hub.rename(&keyed(&by, "r1"), notes[0].0, "space/notes/writing.md")
        .await
        .unwrap();
    assert_eq!(
        hub.read(index).await.unwrap().unwrap().content,
        "- [About you](about.md): You work on payments.\n\
         - [Style](writing.md): Short sentences.\n"
    );
    // A progress file has its own index, in its project's tasks directory.
    hub.write(
        &keyed(&by, "p1"),
        "projects/1/tasks/T-0001.md",
        "# T-0001: the page\nThe form is done; tests are left.",
    )
    .await
    .unwrap();
    assert_eq!(
        hub.read("projects/1/tasks/INDEX.md")
            .await
            .unwrap()
            .unwrap()
            .content,
        "- [T-0001: the page](T-0001.md): The form is done; tests are left.\n"
    );
    // The last note gone, its index goes too.
    hub.delete(&keyed(&by, "d1"), "space/notes/about.md")
        .await
        .unwrap();
    hub.delete(&keyed(&by, "d2"), "space/notes/writing.md")
        .await
        .unwrap();
    assert_eq!(hub.read(index).await.unwrap(), None);
    assert!(hub.history(index).await.unwrap().last().unwrap().deleted);
}

#[tokio::test]
async fn decisions_are_answered_once_and_wake_their_lane() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let decision = hub
        .open_decision(
            None,
            &NewDecision {
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
            },
        )
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
async fn opening_a_decision_queues_one_card_and_a_repeat_opens_nothing() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let new = NewDecision {
        project_id: general.id,
        lane_id: general.lead_lane_id,
        task_id: None,
        session_id: None,
        run_id: Some(RunId(9)),
        tool_call_id: Some("call-1".into()),
        tool: "publish_branch".into(),
        arguments: json!({ "repository": "app" }),
        arguments_hash: "publish_branch:abc".into(),
        kind: DecisionKind::Approval,
        question: "Push branch threadlane/T-0001 of app?".into(),
        context: String::new(),
        options: vec![
            DecisionOption {
                label: "approve".into(),
                effect: json!({ "approve": true }),
            },
            DecisionOption {
                label: "decline".into(),
                effect: json!({ "approve": false }),
            },
        ],
        recommended: Some(0),
        offers_standing_rule: false,
        expires_at: None,
        created_at: T0,
    };
    let first = hub.open_decision(None, &new).await.unwrap();
    let again = hub.open_decision(None, &new).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(hub.open_decisions().await.unwrap(), vec![first.clone()]);
    let card = hub.message(OutboxId(1)).await.unwrap().unwrap();
    assert_eq!(card.kind, OutboxKind::Decision);
    assert_eq!(card.decision_id, Some(first.id));
    assert_eq!(card.urgency, Urgency::High);
    assert_eq!(card.body["recommended"], json!("approve"));
    assert!(
        hub.message(OutboxId(2)).await.unwrap().is_none(),
        "one card"
    );
    assert_eq!(first.outcome, None);
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
    hub.commit_run(&lease, run.id, RunOutcome::Silent, None, None, T0)
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
    hub.commit_run(&lease, retried.id, RunOutcome::Sent, None, None, later)
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

#[tokio::test]
async fn a_failed_task_cancels_what_waits_for_it_transitively_with_one_result_each() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let a = machine(&hub, "a").await;
    let schema = hub
        .create_task(None, &new_task(&general, agent, "schema", vec![]))
        .await
        .unwrap();
    let migration = hub
        .create_task(
            None,
            &new_task(&general, agent, "migration", vec![schema.id]),
        )
        .await
        .unwrap();
    let rollout = hub
        .create_task(
            None,
            &new_task(&general, agent, "rollout", vec![migration.id]),
        )
        .await
        .unwrap();
    let unrelated = hub
        .create_task(None, &new_task(&general, agent, "docs", vec![]))
        .await
        .unwrap();

    let (lease, run) = run_pending(&hub, schema.lane_id, a, T0).await;
    hub.fail_run(&lease, run.id, "boom", retry(true, 1), T0)
        .await
        .unwrap();
    assert_eq!(task(&hub, schema.id).await.status, TaskStatus::Failed);
    assert_eq!(task(&hub, migration.id).await.status, TaskStatus::Cancelled);
    assert_eq!(task(&hub, rollout.id).await.status, TaskStatus::Cancelled);
    assert_eq!(task(&hub, unrelated.id).await.status, TaskStatus::Queued);
    let results = task_results(&hub, &general).await;
    let told: Vec<_> = results
        .iter()
        .map(|event| {
            (
                event.payload["task"].as_str().unwrap().to_owned(),
                event.payload["status"].as_str().unwrap().to_owned(),
                event.payload["reason"].as_str().map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(
        told,
        [
            ("T-0001".to_owned(), "failed".to_owned(), None),
            (
                "T-0002".to_owned(),
                "cancelled".to_owned(),
                Some("T-0001, which it depended on, failed".to_owned())
            ),
            (
                "T-0003".to_owned(),
                "cancelled".to_owned(),
                Some("T-0002, which it depended on, was cancelled".to_owned())
            ),
        ]
    );

    // A task created after its dependency failed never waits.
    let late = hub
        .create_task(None, &new_task(&general, agent, "late", vec![schema.id]))
        .await
        .unwrap();
    assert_eq!(late.status, TaskStatus::Cancelled);
    assert!(hub.pending_events(late.lane_id).await.unwrap().is_empty());
    assert_eq!(task_results(&hub, &general).await.len(), 4);
}

/// Reports a machine with Claude logged in on account `label`.
async fn report_claude(hub: &HubDb, id: MachineId, label: &str) {
    let report = MachineReport {
        capabilities: json!({}),
        backends: json!({ "claude": {
            "installed": true, "version": "2.1", "logged_in": true, "account_label": label
        } }),
        gh_login: false,
        load: 0,
    };
    hub.report(id, &report, T0).await.unwrap();
}

#[tokio::test]
async fn no_lane_is_granted_while_every_account_its_agent_can_use_cools_down() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let agent = engineer(&hub, &general, 2).await;
    let lead = hub
        .create_agent(&NewAgent::named("Lead", "lead"))
        .await
        .unwrap();
    hub.bind(&NewBinding {
        project_id: general.id,
        agent_id: lead.id,
        mode: Mode::OnRequest,
        is_lead: true,
        max_parallel: 1,
    })
    .await
    .unwrap();
    let a = machine(&hub, "a").await;
    report_claude(&hub, a, "claude-work").await;
    let created = hub
        .create_task(None, &new_task(&general, agent, "page", vec![]))
        .await
        .unwrap();
    hub.insert_event(&message(&general, "m-1")).await.unwrap();

    // The lead lane runs into the limit and reports it under its lease.
    let (lease, run) = run_pending(&hub, general.lead_lane_id, a, T0).await;
    let until = T0 + Duration::from_secs(3600);
    let cooled = hub
        .cool_down(&lease, "claude-work", BackendKind::Claude, until)
        .await
        .unwrap();
    assert_eq!(cooled.cooldown_until, Some(until));
    let earlier = hub
        .cool_down(&lease, "claude-work", BackendKind::Claude, T0)
        .await
        .unwrap();
    assert_eq!(earlier.cooldown_until, Some(until), "never shortened");
    hub.fail_run(&lease, run.id, "usage limit", retry(false, 5), T0)
        .await
        .unwrap();
    hub.release_lease(&lease).await.unwrap();
    let stale = hub
        .cool_down(&lease, "claude-work", BackendKind::Claude, until)
        .await;
    assert!(
        matches!(stale, Err(StoreError::StaleLease { .. })),
        "{stale:?}"
    );

    let now = T0 + Duration::from_secs(60);
    assert_eq!(
        hub.grant_lease(created.lane_id, a, now, LEASE_TTL)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        task(&hub, created.id).await.waiting_reason.as_deref(),
        Some(threadlane_store::BLOCKED_REASON)
    );
    assert_eq!(task(&hub, created.id).await.status, TaskStatus::Queued);
    assert_eq!(
        hub.grant_lease(general.lead_lane_id, a, now, LEASE_TTL)
            .await
            .unwrap(),
        None,
        "the lead too"
    );

    // Another machine on another account may take it.
    let b = machine(&hub, "b").await;
    report_claude(&hub, b, "claude-home").await;
    let lease = hub
        .grant_lease(created.lane_id, b, now, LEASE_TTL)
        .await
        .unwrap()
        .expect("a free account");
    assert_eq!(task(&hub, created.id).await.waiting_reason, None);
    hub.release_lease(&lease).await.unwrap();

    // And the first one may once the limit resets.
    assert!(
        hub.grant_lease(general.lead_lane_id, a, until, LEASE_TTL)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn the_account_notices_are_queued_once_per_change() {
    let hub = hub().await;
    let blocked = json!({ "text": "blocked" });
    let resumed = json!({ "text": "resumed" });
    assert!(
        hub.account_notice(false, &resumed, T0)
            .await
            .unwrap()
            .is_none(),
        "nothing to resume from"
    );
    let first = hub.account_notice(true, &blocked, T0).await.unwrap();
    assert_eq!(
        first.as_ref().map(|m| m.kind),
        Some(OutboxKind::SystemNotice)
    );
    assert!(
        hub.account_notice(true, &blocked, T0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        hub.account_notice(false, &resumed, T0)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        hub.account_notice(false, &resumed, T0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        hub.account_notice(true, &blocked, T0)
            .await
            .unwrap()
            .is_some()
    );
    let texts: Vec<_> = notices(&hub)
        .await
        .iter()
        .map(|m| m.body["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(texts, ["blocked", "resumed", "blocked"]);
}

#[tokio::test]
async fn the_client_history_queries_page_from_the_newest() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let other = hub
        .create_project(&NewProject {
            name: "Other".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let at = |seconds: u64| T0 + Duration::from_secs(seconds);

    // Your messages: only user messages of the lane, newest first, by time.
    for (seconds, id) in [(0, "m1"), (10, "m2"), (20, "m3")] {
        hub.insert_event(&NewEvent {
            received_at: at(seconds),
            ..message(&general, id)
        })
        .await
        .unwrap();
    }
    hub.insert_event(&NewEvent {
        kind: EventKind::OutsideEvent,
        source: "test".into(),
        received_at: at(5),
        ..message(&general, "outside")
    })
    .await
    .unwrap();
    hub.insert_event(&message(&other, "elsewhere"))
        .await
        .unwrap();
    let ids = |events: Vec<Event>| {
        events
            .iter()
            .map(|e| e.source_event_id.clone())
            .collect::<Vec<_>>()
    };
    let lane = general.lead_lane_id;
    assert_eq!(
        ids(hub.user_messages(lane, None, 10).await.unwrap()),
        ["m3", "m2", "m1"]
    );
    assert_eq!(
        ids(hub.user_messages(lane, Some(at(20)), 1).await.unwrap()),
        ["m2"]
    );

    // Sent messages: one project or any, newest first, before a time.
    let new = |project: &Project, call: &str| NewOutboxMessage {
        project_id: Some(project.id),
        lane_id: Some(project.lead_lane_id),
        session_id: None,
        run_id: None,
        tool_call_id: Some(call.into()),
        kind: OutboxKind::Message,
        decision_id: None,
        channel: "client".into(),
        body: json!({ "text": call }),
        urgency: Urgency::Normal,
        not_before: None,
        created_at: T0,
    };
    for (seconds, project, call) in [(0, &general, "a"), (1, &other, "b"), (2, &general, "c")] {
        let queued = hub.enqueue(&new(project, call)).await.unwrap();
        hub.mark_sent(queued.id, None, at(seconds)).await.unwrap();
    }
    hub.enqueue(&new(&general, "unsent")).await.unwrap();
    let calls = |messages: Vec<OutboxMessage>| {
        messages
            .iter()
            .filter_map(|m| m.tool_call_id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        calls(
            hub.sent_before("client", Some(general.id), None, 10)
                .await
                .unwrap()
        ),
        ["c", "a"]
    );
    assert_eq!(
        calls(hub.sent_before("client", None, None, 10).await.unwrap()),
        ["c", "b", "a"]
    );
    assert_eq!(
        calls(
            hub.sent_before("client", None, Some(at(2)), 1)
                .await
                .unwrap()
        ),
        ["b"]
    );
    assert!(
        hub.sent_before("slack", None, None, 10)
            .await
            .unwrap()
            .is_empty()
    );

    // Runs and decisions of a lane, in any status, oldest first.
    let machine = machine(&hub, "box").await;
    let (lease, first) = run_pending(&hub, lane, machine, at(100)).await;
    hub.commit_run(&lease, first.id, RunOutcome::Silent, None, None, at(101))
        .await
        .unwrap();
    hub.release_lease(&lease).await.unwrap();
    hub.insert_event(&NewEvent {
        received_at: at(102),
        ..message(&general, "m4")
    })
    .await
    .unwrap();
    let (_lease, second) = run_pending(&hub, lane, machine, at(103)).await;
    let runs = hub.lane_runs(lane).await.unwrap();
    assert_eq!(
        runs.iter()
            .map(|run| (run.id, run.status))
            .collect::<Vec<_>>(),
        [
            (first.id, RunStatus::Committed),
            (second.id, RunStatus::Active)
        ]
    );
    assert!(hub.lane_runs(other.lead_lane_id).await.unwrap().is_empty());

    let decision = |call: &str| NewDecision {
        project_id: general.id,
        lane_id: lane,
        task_id: None,
        session_id: None,
        run_id: Some(second.id),
        tool_call_id: Some(call.into()),
        tool: "ask_user".into(),
        arguments: json!({}),
        arguments_hash: String::new(),
        kind: DecisionKind::Input,
        question: format!("{call}?"),
        context: String::new(),
        options: vec![DecisionOption {
            label: "yes".into(),
            effect: json!({}),
        }],
        recommended: None,
        offers_standing_rule: false,
        expires_at: None,
        created_at: at(104),
    };
    let asked = hub.open_decision(None, &decision("one")).await.unwrap();
    hub.answer(
        asked.id,
        &DecisionAnswer {
            option: 0,
            note: None,
            standing_rule: false,
            answered_at: at(105),
        },
    )
    .await
    .unwrap();
    let open = hub.open_decision(None, &decision("two")).await.unwrap();
    let decisions = hub.lane_decisions(lane).await.unwrap();
    assert_eq!(
        decisions
            .iter()
            .map(|d| (d.id, d.status))
            .collect::<Vec<_>>(),
        [
            (asked.id, DecisionStatus::Answered),
            (open.id, DecisionStatus::Pending)
        ]
    );
}

#[tokio::test]
async fn an_agent_can_be_updated_and_its_name_stays_unique() {
    let hub = hub().await;
    let first = hub
        .create_agent(&NewAgent::named("Engineer", "engineer"))
        .await
        .unwrap();
    hub.create_agent(&NewAgent::named("Reviewer", "reviewer"))
        .await
        .unwrap();
    let updated = hub
        .update_agent(&Agent {
            name: "Builder".into(),
            instructions: "Build it.".into(),
            backends: vec![BackendKind::Codex],
            models: json!({"codex": "gpt-5"}),
            ..first.clone()
        })
        .await
        .unwrap();
    assert_eq!(updated.name, "Builder");
    assert_eq!(updated.models, json!({"codex": "gpt-5"}));
    assert_eq!(hub.agent(first.id).await.unwrap(), Some(updated.clone()));
    let clash = hub
        .update_agent(&Agent {
            name: "Reviewer".into(),
            ..updated
        })
        .await;
    assert!(matches!(clash, Err(StoreError::Conflict(_))), "{clash:?}");
}

#[tokio::test]
async fn deleting_an_agent_waits_for_its_tasks_and_keeps_its_history() {
    let hub = hub().await;
    let general = hub.general_project().await.unwrap();
    let lead = hub
        .create_agent(&NewAgent::named("Lead", "lead"))
        .await
        .unwrap();
    hub.bind(&NewBinding {
        project_id: general.id,
        agent_id: lead.id,
        mode: Mode::OnRequest,
        is_lead: true,
        max_parallel: 1,
    })
    .await
    .unwrap();
    let refused = hub.delete_agent(lead.id).await;
    assert!(
        matches!(&refused, Err(StoreError::Invalid(m)) if m.contains("lead")),
        "{refused:?}"
    );

    let agent = engineer(&hub, &general, 1).await;
    let queued = hub
        .create_task(None, &new_task(&general, agent, "open", vec![]))
        .await
        .unwrap();
    let refused = hub.delete_agent(agent).await;
    assert!(
        matches!(&refused, Err(StoreError::Invalid(m)) if m.contains(&queued.id.to_string())),
        "{refused:?}"
    );

    hub.cancel_task(None, queued.id, T0).await.unwrap();
    let name = hub.agent(agent).await.unwrap().unwrap().name;
    hub.delete_agent(agent).await.unwrap();
    assert!(hub.agents().await.unwrap().iter().all(|a| a.id != agent));
    assert!(
        hub.bindings(general.id)
            .await
            .unwrap()
            .iter()
            .all(|b| b.agent_id != agent)
    );
    let kept = hub.agent(agent).await.unwrap().unwrap();
    assert_eq!(kept.name, format!("{name} (deleted)"));
    assert!(hub.all_agents().await.unwrap().contains(&kept));
    assert_eq!(task(&hub, queued.id).await.agent_id, agent);

    // The name is free again, and a second deletion of it still succeeds.
    let again = hub
        .create_agent(&NewAgent::named(name.clone(), "engineer"))
        .await
        .unwrap();
    hub.delete_agent(again.id).await.unwrap();
    assert_eq!(
        hub.agent(again.id).await.unwrap().unwrap().name,
        format!("{name} (deleted {})", again.id)
    );
    let missing = hub.delete_agent(agent).await;
    assert!(
        matches!(missing, Err(StoreError::NotFound { .. })),
        "{missing:?}"
    );
}
