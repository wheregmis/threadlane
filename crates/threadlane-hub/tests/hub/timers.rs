//! Timers and routines at the hub (Plan.md §7, docs/milestone-8.md §2): the
//! four tools and their guardrails, the ask before an agent's routine,
//! `requested` from the creator, the scheduler pass on the fake clock with
//! crashes between its steps (invariant 4), and the Timers capability.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::client::{
    ApiError, DecisionRequest, OverviewReply, OverviewRequest, TimerReply, TimerRequest,
};
use threadlane_core::ids::{AgentId, DecisionId};
use threadlane_core::link::{Begin, HubLink, Perform, RunAssignment};
use threadlane_core::model::{
    BackendKind, EventKind, EventStatus, LaneKind, Lease, MAX_LIVE_TIMERS, Mode, NewAgent,
    NewBinding, NewTimer, Recurrence, Rule, RunOutcome, SessionChoice, StartRun, Timer,
    TimerCategory, TimerCreator, TimerStatus,
};
use threadlane_core::runtime::NoHooks;
use threadlane_core::runtime::steps::scheduler;
use threadlane_core::store::{DecisionStore, InboxStore, OrgStore, SessionStore, TimerStore};
use threadlane_core::time::{Clock, Timestamp};
use threadlane_core::tools::ToolError;
use threadlane_hub::{
    CANCEL_TIMER, CREATE_SCHEDULE, CREATE_TASK, HubSettings, LIST_TIMERS, SET_FOLLOW_UP,
    SKIP_REASON,
};
use threadlane_testkit::CrashPlan;

use crate::common::{TestHub, begin};

/// One run in progress, and the link it calls tools through.
struct Running<'a> {
    link: &'a dyn HubLink,
    lease: Lease,
    run: RunAssignment,
}

impl Running<'_> {
    async fn call(&self, call_id: &str, tool: &str, arguments: Json) -> Result<Json, ToolError> {
        self.link
            .call_tool(
                &self.lease,
                self.run.run_id,
                Some(call_id),
                tool,
                &arguments,
            )
            .await
    }

    async fn end(&self) -> RunOutcome {
        let outcome = self
            .link
            .complete(&self.lease, self.run.run_id, None)
            .await
            .unwrap();
        assert_eq!(self.link.begin_run(&self.lease).await.unwrap(), Begin::Idle);
        self.link.release(&self.lease).await.unwrap();
        outcome
    }
}

async fn hub() -> TestHub {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub
}

/// A run of General's lead triggered by a message from you.
async fn lead_run<'a>(hub: &'a TestHub, id: &str) -> Running<'a> {
    hub.send(id, "Keep an eye on the build").await;
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Lead);
    Running {
        link: &*hub.link,
        lease,
        run,
    }
}

fn refused(result: Result<Json, ToolError>, why: &str) {
    match result {
        Err(ToolError::InvalidArguments(message)) => {
            assert!(message.contains(why), "{why:?} in {message:?}");
        }
        other => panic!("expected a refusal saying {why:?}, got {other:?}"),
    }
}

/// Every event in General's lead lane, any status.
async fn timer_events(hub: &TestHub) -> Vec<threadlane_core::model::Event> {
    let lane = hub.general().await.lead_lane_id;
    let mut events = Vec::new();
    for id in 1.. {
        let Some(event) = hub
            .db
            .event(threadlane_core::ids::EventId(id))
            .await
            .unwrap()
        else {
            break;
        };
        if event.lane_id == lane && event.kind == EventKind::TimerFired {
            events.push(event);
        }
    }
    events
}

fn daily(hour: u8, minute: u8, zone: &str) -> Recurrence {
    Recurrence {
        rule: Rule::Daily { hour, minute },
        zone: zone.to_owned(),
    }
}

/// Creates a routine of yours in General through the client interface.
async fn your_routine(hub: &TestHub, recurrence: Recurrence) -> Timer {
    match hub
        .api
        .timers(TimerRequest::CreateRoutine {
            project: None,
            instruction: "Summarize the open pull requests.".into(),
            recurrence,
            client_request_id: "r-1".into(),
        })
        .await
        .unwrap()
    {
        TimerReply::Timer { timer } => timer,
        other => panic!("expected a timer, got {other:?}"),
    }
}

#[tokio::test]
async fn a_follow_up_fires_once_into_the_lane_that_set_it_and_its_run_reads_it() {
    let hub = hub().await;
    let lead = lead_run(&hub, "m-1").await;
    let set = lead
        .call(
            "f1",
            SET_FOLLOW_UP,
            json!({ "instruction": "Check whether the build is green.", "in_minutes": 5 }),
        )
        .await
        .unwrap();
    assert_eq!(set["category"], json!("follow_up"));
    assert_eq!(set["due_at"], json!("2026-01-01T00:05:00Z"));
    assert_eq!(
        lead.call(
            "f1",
            SET_FOLLOW_UP,
            json!({ "instruction": "x", "in_minutes": 9 })
        )
        .await
        .unwrap(),
        set,
        "the same call creates nothing more"
    );
    assert_eq!(lead.end().await, RunOutcome::Silent);
    assert_eq!(
        hub.db
            .project_timers(hub.general().await.id)
            .await
            .unwrap()
            .len(),
        1
    );

    assert_eq!(hub.core.schedule_once().await.unwrap(), 0, "not due yet");
    hub.clock.advance(Duration::from_secs(5 * 60));
    assert_eq!(hub.core.schedule_once().await.unwrap(), 1);
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0, "fired once");
    let events = timer_events(&hub).await;
    assert_eq!(events.len(), 1);
    assert!(events[0].requested, "set in a run you asked for");
    let timer = hub
        .db
        .timer(threadlane_core::ids::TimerId(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((timer.status, timer.fire_count), (TimerStatus::Done, 1));

    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.events, [events[0].id]);
    assert!(
        run.prompt.contains(
            "Follow-up 2 fired. Set by an agent, in an earlier run; due at \
             2026-01-01T00:05:00Z. Its instruction:\nCheck whether the build is green."
        ),
        "{}",
        run.prompt
    );
    Running {
        link: &*hub.link,
        lease,
        run,
    }
    .end()
    .await;
}

async fn bind_engineer(hub: &TestHub) -> AgentId {
    let agent = hub
        .db
        .create_agent(&NewAgent::named("Engineer", "engineer"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: hub.general().await.id,
            agent_id: agent.id,
            mode: Some(Mode::OnRequest),
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    agent.id
}

#[tokio::test]
async fn a_task_sets_its_follow_up_in_its_own_lane() {
    let hub = hub().await;
    let agent = bind_engineer(&hub).await;
    let lead = lead_run(&hub, "m-task").await;
    lead.call(
        "t1",
        CREATE_TASK,
        json!({ "title": "page", "goal": "the page is up", "agent": "Engineer" }),
    )
    .await
    .unwrap();
    lead.end().await;
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Task);
    let task = Running {
        link: &*hub.link,
        lease,
        run,
    };
    let set = task
        .call(
            "f1",
            SET_FOLLOW_UP,
            json!({ "instruction": "Look at the deploy.", "at": "2026-01-01T02:00:00+01:00" }),
        )
        .await
        .unwrap();
    assert_eq!(set["due_at"], json!("2026-01-01T01:00:00Z"));
    let timer = hub
        .db
        .timer(threadlane_core::ids::TimerId(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(timer.lane_id, lease.lane_id);
    assert_eq!(
        timer.task_ref.map(|id| id.to_string()),
        set["task_id"].as_str().map(str::to_owned)
    );
    assert_eq!(timer.agent_id, Some(agent));
    assert!(timer.requested, "the task came from you");
}

#[tokio::test]
async fn each_guardrail_refuses_and_says_its_limit() {
    let hub = hub().await;
    let lead = lead_run(&hub, "m-1").await;
    let follow_up = |arguments: Json| lead.call("x", SET_FOLLOW_UP, arguments);
    refused(
        follow_up(json!({ "instruction": "soon", "in_minutes": 0 })).await,
        "at least 1 minute away",
    );
    // Each refusal is recorded under its call id, so use a new one each time.
    refused(
        lead.call(
            "past",
            SET_FOLLOW_UP,
            json!({ "instruction": "soon", "at": "2025-12-31T23:00:00Z" }),
        )
        .await,
        "at least 1 minute away",
    );
    refused(
        lead.call(
            "both",
            SET_FOLLOW_UP,
            json!({ "instruction": "x", "in_minutes": 5, "at": "2026-01-02T00:00:00Z" }),
        )
        .await,
        "either in_minutes or at",
    );
    refused(
        lead.call(
            "bare",
            SET_FOLLOW_UP,
            json!({ "instruction": " ", "in_minutes": 5 }),
        )
        .await,
        "instruction",
    );
    let schedule =
        |call: &'static str, arguments: Json| lead.call(call, CREATE_SCHEDULE, arguments);
    refused(
        schedule(
            "s1",
            json!({ "instruction": "poll", "rule": "interval", "minutes": 10, "zone": "UTC" }),
        )
        .await,
        "every 15 minutes",
    );
    refused(
        schedule(
            "s2",
            json!({ "instruction": "poll", "rule": "daily", "hour": 9, "minute": 0,
                    "zone": "Mars/Olympus" }),
        )
        .await,
        "not a time zone",
    );
    refused(
        schedule(
            "s3",
            json!({ "instruction": "poll", "rule": "daily", "hour": 25, "minute": 0,
                    "zone": "UTC" }),
        )
        .await,
        "not a time of day",
    );

    // The cap counts every timer that has not ended, yours too.
    let general = hub.general().await;
    for n in 0..MAX_LIVE_TIMERS - 1 {
        hub.db
            .create_timer(&NewTimer {
                project_id: general.id,
                lane_id: general.lead_lane_id,
                agent_id: None,
                task_ref: None,
                category: TimerCategory::FollowUp,
                due_at: hub.clock.now() + Duration::from_secs(3600),
                recurrence: None,
                instruction: format!("timer {n}"),
                created_by: TimerCreator::User,
                created_by_call: None,
                requested: true,
            })
            .await
            .unwrap();
    }
    let last = json!({ "instruction": "the twentieth", "in_minutes": 5 });
    lead.call("ok", SET_FOLLOW_UP, last.clone()).await.unwrap();
    refused(
        lead.call("over", SET_FOLLOW_UP, last).await,
        "the limit is 20",
    );
    refused(
        schedule(
            "s4",
            json!({ "instruction": "poll", "rule": "daily", "hour": 9, "minute": 0,
                    "zone": "UTC" }),
        )
        .await,
        "the limit is 20",
    );
    assert!(
        hub.db.open_decisions().await.unwrap().is_empty(),
        "a refused routine is not asked"
    );
    lead.end().await;
}

#[tokio::test]
async fn an_agents_routine_asks_first_and_is_created_once_when_you_approve() {
    let hub = hub().await;
    let lead = lead_run(&hub, "m-1").await;
    let arguments = json!({
        "instruction": "Post the weekly status.",
        "rule": "weekdays",
        "days": ["mon"],
        "hour": 9,
        "minute": 0,
        "zone": "Europe/Berlin",
    });
    assert_eq!(
        lead.call("s1", CREATE_SCHEDULE, arguments.clone()).await,
        Err(ToolError::WaitingOnYou)
    );
    assert_eq!(lead.end().await, RunOutcome::Deferred);
    assert!(
        hub.db
            .project_timers(hub.general().await.id)
            .await
            .unwrap()
            .is_empty()
    );
    let decision = hub.db.decision(DecisionId(1)).await.unwrap().unwrap();
    assert_eq!(decision.tool, CREATE_SCHEDULE);
    assert_eq!(decision.arguments, arguments);
    assert_eq!(
        decision.question,
        "Create a routine on mon at 09:00 Europe/Berlin?"
    );
    assert!(decision.context.contains("Post the weekly status."));

    hub.api
        .decisions(DecisionRequest::Answer {
            decision: decision.id,
            option: "approve".into(),
            note: None,
        })
        .await
        .unwrap();
    let (lease, after) = begin(&*hub.link).await;
    assert_eq!(after.perform, [decision.id]);
    let performed = hub
        .link
        .begin_perform(&lease, after.run_id, decision.id)
        .await
        .unwrap();
    let Perform::Recorded { outcome } = &performed else {
        panic!("the hub creates a routine itself: {performed:?}");
    };
    assert_eq!(outcome["performed"], json!(true), "{outcome}");
    // 2026-01-01 is a Thursday; the next Monday 09:00 in Berlin is 08:00Z.
    // Timer 1 is General's default dreaming, timer 2 the decision's expiry.
    assert_eq!(
        outcome["summary"],
        json!("Routine 3 is created; it is next due at 2026-01-05T08:00:00Z.")
    );
    assert_eq!(
        hub.link
            .begin_perform(&lease, after.run_id, decision.id)
            .await
            .unwrap(),
        performed,
        "performed once"
    );
    let timers = hub.db.project_timers(hub.general().await.id).await.unwrap();
    assert_eq!(timers.len(), 1, "created at most once");
    let routine = &timers[0];
    assert_eq!(routine.category, TimerCategory::Schedule);
    assert_eq!(routine.created_by, TimerCreator::Agent);
    assert!(
        !routine.requested,
        "an agent's routine is never requested work, even asked for in a run you started"
    );
    assert_eq!(routine.lane_id, hub.general().await.lead_lane_id);
    Running {
        link: &*hub.link,
        lease,
        run: after,
    }
    .end()
    .await;

    // Declined, nothing is created.
    let lead = lead_run(&hub, "m-2").await;
    assert_eq!(
        lead.call("s2", CREATE_SCHEDULE, arguments).await,
        Err(ToolError::WaitingOnYou)
    );
    lead.end().await;
    hub.api
        .decisions(DecisionRequest::Answer {
            decision: DecisionId(2),
            option: "decline".into(),
            note: None,
        })
        .await
        .unwrap();
    let (lease, after) = begin(&*hub.link).await;
    assert!(after.perform.is_empty());
    Running {
        link: &*hub.link,
        lease,
        run: after,
    }
    .end()
    .await;
    assert_eq!(
        hub.db
            .project_timers(hub.general().await.id)
            .await
            .unwrap()
            .len(),
        1
    );

    hub.api
        .timers(TimerRequest::RunOnce {
            timer: routine.id,
            client_request_id: "manual-agent-routine".into(),
        })
        .await
        .unwrap();
    let (lease, run) = begin(&*hub.link).await;
    let event = hub.db.event(run.events[0]).await.unwrap().unwrap();
    assert!(event.requested);
    assert_eq!(event.payload["manual"], true);
    assert!(!hub.db.timer(routine.id).await.unwrap().unwrap().requested);
    Running {
        link: &*hub.link,
        lease,
        run,
    }
    .end()
    .await;
}

#[tokio::test]
async fn agents_list_timers_and_cancel_their_own_but_not_yours() {
    let hub = hub().await;
    let yours = your_routine(&hub, daily(9, 0, "UTC")).await;
    let lead = lead_run(&hub, "m-1").await;
    let set = lead
        .call(
            "f1",
            SET_FOLLOW_UP,
            json!({ "instruction": "again", "in_minutes": 30 }),
        )
        .await
        .unwrap();
    let listed = lead.call("l1", LIST_TIMERS, json!({})).await.unwrap();
    let listed = listed["timers"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0]["recurrence"], json!("daily at 09:00 UTC"));
    assert_eq!(listed[0]["created_by"], json!("user"));
    assert_eq!(listed[1], set);
    refused(
        lead.call(
            "c1",
            CANCEL_TIMER,
            json!({ "timer_id": yours.id.to_string() }),
        )
        .await,
        "only they can cancel it",
    );
    refused(
        lead.call("c2", CANCEL_TIMER, json!({ "timer_id": "99" }))
            .await,
        "no timer 99",
    );
    let cancelled = lead
        .call("c3", CANCEL_TIMER, json!({ "timer_id": set["timer_id"] }))
        .await
        .unwrap();
    assert_eq!(cancelled["status"], json!("cancelled"));
    let listed = lead.call("l2", LIST_TIMERS, json!({})).await.unwrap();
    assert_eq!(listed["timers"].as_array().unwrap().len(), 1);
    lead.end().await;
}

#[tokio::test]
async fn requested_comes_from_the_creator() {
    let hub = hub().await;
    let general = hub.general().await;
    // Your routine is requested work.
    assert!(your_routine(&hub, daily(9, 0, "UTC")).await.requested);

    // A follow-up from a run nobody asked for is not: such a run cannot
    // start under On request, so it is opened directly on the store.
    let event = hub
        .insert(|event| {
            event.requested = false;
        })
        .await;
    let lease = hub.link.request_work().await.unwrap().expect("granted");
    let lead = hub.db.lead_binding(general.id).await.unwrap().unwrap();
    let run = hub
        .db
        .start_run(
            &lease,
            &StartRun {
                session: SessionChoice::New,
                agent_id: Some(lead.agent_id),
                backend: Some(BackendKind::Claude),
                model: None,
                native_session_id: None,
                events: vec![event],
                started_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    hub.link
        .call_tool(
            &lease,
            run.id,
            Some("f1"),
            SET_FOLLOW_UP,
            &json!({ "instruction": "check", "in_minutes": 1 }),
        )
        .await
        .unwrap();
    let timer = hub
        .db
        .project_timers(general.id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(timer.created_by, TimerCreator::Agent);
    assert!(!timer.requested, "set in a run nobody asked for");
    hub.link.complete(&lease, run.id, None).await.unwrap();
    hub.link.release(&lease).await.unwrap();

    // When it fires, its event is not requested, and the lead, On request,
    // does not act.
    hub.clock.advance(Duration::from_secs(60));
    hub.core.schedule_once().await.unwrap();
    let fired = timer_events(&hub).await;
    assert_eq!(fired.len(), 1);
    assert!(!fired[0].requested);
    let lease = hub.link.request_work().await.unwrap().expect("granted");
    assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
    hub.link.release(&lease).await.unwrap();
    let skipped = hub.event(fired[0].id).await;
    assert_eq!(skipped.status, EventStatus::Skipped);
    assert_eq!(skipped.status_reason.as_deref(), Some(SKIP_REASON));
}

#[tokio::test]
async fn downtime_across_several_occurrences_fires_once_then_moves_on() {
    let hub = hub().await;
    // The clock starts at 2026-01-01T00:00:00Z.
    let routine = your_routine(&hub, daily(9, 0, "UTC")).await;
    let nine = Timestamp::from_millis(1_767_258_000_000);
    assert_eq!(routine.due_at, nine);
    assert_eq!(
        your_routine(&hub, daily(10, 0, "UTC")).await.id,
        routine.id,
        "the same request id creates nothing more"
    );

    // The hub is down for three days and an hour.
    hub.clock.advance(Duration::from_secs((3 * 24 + 10) * 3600));
    assert_eq!(hub.core.schedule_once().await.unwrap(), 1);
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    let fired = timer_events(&hub).await;
    assert_eq!(fired.len(), 1, "one event for the missed days");
    assert_eq!(fired[0].payload["occurrence"], json!(nine));
    assert!(fired[0].requested, "your routine is requested work");
    let routine = hub.db.timer(routine.id).await.unwrap().unwrap();
    let day = Duration::from_secs(24 * 3600);
    assert_eq!(
        routine.due_at,
        Timestamp::from_millis(nine.as_millis()) + day * 4
    );
    assert_eq!(routine.fire_count, 1);

    // Paused, it fires nothing; resumed, it is due at its next occurrence.
    let paused = hub
        .api
        .timers(TimerRequest::Pause { timer: routine.id })
        .await
        .unwrap();
    assert!(matches!(paused, TimerReply::Timer { timer } if timer.status == TimerStatus::Paused));
    hub.clock.advance(day * 2);
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    let TimerReply::Timer { timer: resumed } = hub
        .api
        .timers(TimerRequest::Resume { timer: routine.id })
        .await
        .unwrap()
    else {
        panic!("expected a timer");
    };
    assert_eq!(resumed.status, TimerStatus::Active);
    assert_eq!(
        resumed.due_at,
        Timestamp::from_millis(nine.as_millis()) + day * 6
    );
    let TimerReply::Timers { timers } = hub
        .api
        .timers(TimerRequest::List { project: None })
        .await
        .unwrap()
    else {
        panic!("expected timers");
    };
    assert_eq!(timers, std::slice::from_ref(&resumed));
    let TimerReply::Timer { timer: cancelled } = hub
        .api
        .timers(TimerRequest::Cancel { timer: routine.id })
        .await
        .unwrap()
    else {
        panic!("expected a timer");
    };
    assert_eq!(cancelled.status, TimerStatus::Cancelled);
    assert!(
        hub.api
            .timers(TimerRequest::Resume { timer: routine.id })
            .await
            .is_err()
    );
    assert!(
        hub.api
            .timers(TimerRequest::CreateRoutine {
                project: None,
                instruction: "x".into(),
                recurrence: daily(9, 0, "Nowhere/Land"),
                client_request_id: "r-2".into(),
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_kill_between_the_scheduler_steps_fires_nothing_twice() {
    for &step in scheduler::ALL {
        let plan = CrashPlan::kill_at(step);
        let hub = TestHub::with(HubSettings::default(), Arc::new(plan.clone())).await;
        let routine = your_routine(&hub, daily(9, 0, "UTC")).await;
        hub.clock.advance(Duration::from_secs(10 * 3600));
        assert!(hub.core.schedule_once().await.is_err(), "killed at {step}");
        assert_eq!(plan.killed(), Some(step));

        let restarted = hub.restart(Arc::new(NoHooks)).await;
        restarted.core.schedule_once().await.unwrap();
        restarted.core.schedule_once().await.unwrap();
        assert_eq!(timer_events(&restarted).await.len(), 1, "killed at {step}");
        let routine = restarted.db.timer(routine.id).await.unwrap().unwrap();
        assert_eq!(routine.fire_count, 1, "killed at {step}");
    }
}

async fn skip_request(hub: &TestHub, grace_minutes: u16, recurrence: Recurrence) -> Timer {
    let TimerReply::Timer { timer } = hub
        .api
        .timers(TimerRequest::CreateRoutineSkippingLate {
            project: None,
            instruction: "Only useful at the scheduled time.".into(),
            recurrence,
            client_request_id: "skip-late".into(),
            grace_minutes,
        })
        .await
        .unwrap()
    else {
        panic!("expected a timer");
    };
    timer
}

#[tokio::test]
async fn missed_run_grace_is_validated_and_its_exact_boundary_still_runs() {
    let hub = hub().await;
    for grace_minutes in [0, 1441] {
        let result = hub
            .api
            .timers(TimerRequest::CreateRoutineSkippingLate {
                project: None,
                instruction: "Invalid grace.".into(),
                recurrence: daily(9, 0, "UTC"),
                client_request_id: format!("invalid-{grace_minutes}"),
                grace_minutes,
            })
            .await;
        assert!(matches!(result, Err(ApiError::Invalid(why)) if why.contains("grace")));
    }
    assert!(
        hub.db
            .project_timers(hub.general().await.id)
            .await
            .unwrap()
            .is_empty()
    );
    let routine = skip_request(&hub, 5, daily(9, 0, "UTC")).await;
    assert_eq!(routine.missed_grace_minutes, Some(5));
    assert_eq!(
        skip_request(&hub, 30, daily(10, 0, "UTC")).await,
        routine,
        "request replay keeps the first policy and time"
    );
    hub.clock.set(routine.due_at + Duration::from_secs(5 * 60));

    assert_eq!(hub.core.schedule_once().await.unwrap(), 1);
    assert_eq!(timer_events(&hub).await[0].status, EventStatus::Pending);
    assert_eq!(
        hub.db.timer(routine.id).await.unwrap().unwrap().fire_count,
        1
    );
}

#[tokio::test]
async fn a_missed_routine_does_not_wake_or_replay_and_records_one_skip_after_restart() {
    let crash = CrashPlan::kill_at(scheduler::AFTER_FIRE);
    let hub = TestHub::with(HubSettings::default(), Arc::new(crash.clone())).await;
    // The clock later passes 02:00, when default dreaming fires in a UTC zone.
    hub.dreaming_off(None).await;
    let routine = skip_request(
        &hub,
        5,
        Recurrence {
            rule: Rule::Interval { minutes: 15 },
            zone: "UTC".into(),
        },
    )
    .await;
    // 5 minutes and 1ms past the fourth due time. Old occurrences must not replay.
    hub.clock
        .set(routine.due_at + Duration::from_millis(50 * 60 * 1_000 + 1));
    assert!(hub.core.schedule_once().await.is_err());
    assert_eq!(crash.killed(), Some(scheduler::AFTER_FIRE));

    let hub = hub.restart(Arc::new(NoHooks)).await;
    let saved = hub.db.timer(routine.id).await.unwrap().unwrap();
    assert_eq!(saved.due_at, routine.due_at + Duration::from_secs(60 * 60));
    assert_eq!(saved.fire_count, 0);
    assert_eq!(saved.last_fired_at, None);
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    assert!(
        hub.db
            .pending_events(saved.lane_id)
            .await
            .unwrap()
            .is_empty()
    );
    let history = hub
        .db
        .routine_history(Some(routine.id), None)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].due, routine.due_at);
    assert_eq!(history[0].status, "skipped");
    assert_eq!(history[0].started, None);
    assert!(!history[0].manual);
    assert!(
        history[0]
            .result
            .as_deref()
            .unwrap()
            .contains("more than 5 minutes late")
    );

    // Pausing does not record a skip. Running once is still allowed on a paused
    // overdue routine and leaves its schedule unchanged.
    hub.api
        .timers(TimerRequest::Pause { timer: routine.id })
        .await
        .unwrap();
    hub.clock
        .set(saved.due_at + Duration::from_secs(2 * 60 * 60));
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    assert_eq!(
        hub.db
            .routine_history(Some(routine.id), None)
            .await
            .unwrap()
            .len(),
        1
    );
    let TimerReply::Timer { timer: manual } = hub
        .api
        .timers(TimerRequest::RunOnce {
            timer: routine.id,
            client_request_id: "owner-tested".into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected the routine")
    };
    assert_eq!(manual.due_at, saved.due_at);
    assert_eq!(manual.status, TimerStatus::Paused);
    assert_eq!(manual.fire_count, 1);
    let history = hub
        .db
        .routine_history(Some(routine.id), None)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].status, "pending");
    assert!(history[0].manual);
}

#[tokio::test]
async fn paused_routine_records_manual_work_and_resumes_on_its_original_cadence() {
    let hub = hub().await;
    bind_engineer(&hub).await;
    let timer = your_routine(&hub, daily(9, 0, "America/Toronto")).await;
    hub.api
        .timers(TimerRequest::Pause { timer: timer.id })
        .await
        .unwrap();
    let manual = TimerRequest::RunOnce {
        timer: timer.id,
        client_request_id: "preview".into(),
    };
    hub.api.timers(manual.clone()).await.unwrap();
    hub.api.timers(manual).await.unwrap();
    let after = hub.db.timer(timer.id).await.unwrap().unwrap();
    assert_eq!(
        (after.due_at, after.status, after.fire_count),
        (timer.due_at, TimerStatus::Paused, 1)
    );
    let (lease, run) = begin(&*hub.link).await;
    let child = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("delegate"),
            CREATE_TASK,
            &json!({"title":"Review","goal":"Review the change","agent":"Engineer"}),
        )
        .await
        .unwrap();
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    let OverviewReply::RoutineHistory { rows } = hub
        .api
        .overview(OverviewRequest::RoutineHistory {
            timer: timer.id,
            before: None,
        })
        .await
        .unwrap()
    else {
        panic!("history")
    };
    assert_eq!(rows.len(), 1);
    assert!(rows[0].manual);
    assert!(rows[0].started.is_some());
    assert_eq!(
        rows[0].tasks[0].to_string(),
        child["task_id"].as_str().unwrap()
    );
    hub.api
        .timers(TimerRequest::Resume { timer: timer.id })
        .await
        .unwrap();
    let OverviewReply::Agenda { recent, upcoming } = hub
        .api
        .overview(OverviewRequest::Agenda { tomorrow_in: None })
        .await
        .unwrap()
    else {
        panic!("agenda")
    };
    assert_eq!(recent, rows);
    assert_eq!(upcoming[0].due, timer.due_at);
    assert!(upcoming.len() >= 7);
    let OverviewReply::SpaceUsage { groups } = hub
        .api
        .overview(OverviewRequest::SpaceUsage { days: 7 })
        .await
        .unwrap()
    else {
        panic!("usage")
    };
    assert_eq!(groups.iter().map(|g| g.runs).sum::<u64>(), 1);
    assert_eq!(groups[0].cost_reports, 0);
}
