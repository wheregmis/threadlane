//! Milestone 9 stage B in the hub: what may start under each mode, proposals
//! (a paused `create_task` from work you did not request), the stricter of
//! two agents, and an agent's routine (docs/milestone-9.md §3).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::client::{DecisionReply, DecisionRequest, OverviewReply, OverviewRequest};
use threadlane_core::ids::{DecisionId, EventId};
use threadlane_core::link::{Begin, Perform, RunAssignment};
use threadlane_core::model::{
    DecisionKind, DecisionOption, EventKind, EventStatus, LaneKind, Lease, Mode, NewAgent,
    NewBinding, NewDecision, RunOutcome, TaskOrigin, Trust,
};
use threadlane_core::runtime::steps::perform;
use threadlane_core::runtime::{NoHooks, StepHook};
use threadlane_core::store::{DecisionStore, MemoryStore, OrgStore, TaskStore, TimerStore};
use threadlane_core::time::Clock;
use threadlane_core::tools::ToolError;
use threadlane_hub::{CREATE_SCHEDULE, CREATE_TASK, HubSettings, SKIP_REASON, derived_call_key};
use threadlane_testkit::CrashPlan;

use crate::common::{PAST_LEASE, Stop, TestHub, begin};

const ENGINEER: &str = "Engineer";

/// A hub whose space allows Autonomous, with General's lead at `lead` and
/// an engineer bound at `engineer`.
async fn hub_with(hook: Arc<dyn StepHook>, lead: Mode, engineer: Mode) -> TestHub {
    let hub = TestHub::with(HubSettings::default(), hook).await;
    hub.hello().await;
    let db = &hub.db;
    let mut space = db.space().await.unwrap();
    space.mode_ceiling = Mode::Autonomous;
    db.update_space(&space).await.unwrap();
    let general = hub.general().await;
    let binding = db.lead_binding(general.id).await.unwrap().unwrap();
    db.set_binding_mode(binding.id, lead).await.unwrap();
    let agent = db
        .create_agent(&NewAgent::named(ENGINEER, "engineer"))
        .await
        .unwrap();
    let bound = db
        .bind(&NewBinding {
            project_id: general.id,
            agent_id: agent.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    db.set_binding_mode(bound.id, engineer).await.unwrap();
    hub
}

async fn hub(lead: Mode, engineer: Mode) -> TestHub {
    hub_with(Arc::new(NoHooks), lead, engineer).await
}

/// A failing build reported from outside, for no task: not requested work.
async fn outside(hub: &TestHub) -> EventId {
    hub.insert(|event| {
        event.source = "github".into();
        event.trust = Trust::External;
        event.requested = false;
        event.payload = json!({ "text": "the build of main failed" });
    })
    .await
}

fn fix_the_build() -> Json {
    json!({ "title": "Fix the build", "goal": "main builds again", "agent": ENGINEER })
}

async fn call(
    hub: &TestHub,
    lease: &Lease,
    run: &RunAssignment,
    id: &str,
    tool: &str,
    arguments: &Json,
) -> Result<Json, ToolError> {
    hub.link
        .call_tool(lease, run.run_id, Some(id), tool, arguments)
        .await
}

/// Ends the run and gives the lane back, expecting nothing more to run.
async fn end(hub: &TestHub, lease: &Lease, run: &RunAssignment) -> RunOutcome {
    let outcome = hub.link.complete(lease, run.run_id, None).await.unwrap();
    assert_eq!(hub.link.begin_run(lease).await.unwrap(), Begin::Idle);
    hub.link.release(lease).await.unwrap();
    outcome
}

/// What the space lists as recorded and not acted on: the event ids and
/// the total.
async fn recorded(hub: &TestHub) -> (Vec<EventId>, u64) {
    let Ok(OverviewReply::Recorded { events, total }) = hub
        .api
        .overview(OverviewRequest::Recorded {
            project: None,
            limit: 50,
        })
        .await
    else {
        panic!("not recorded events");
    };
    assert_eq!(
        hub.api.overview(OverviewRequest::RecordedCount).await,
        Ok(OverviewReply::RecordedCount { total })
    );
    (events.iter().map(|event| event.id).collect(), total)
}

async fn answer(hub: &TestHub, id: i64, option: &str, note: Option<&str>) -> bool {
    match hub
        .api
        .decisions(DecisionRequest::Answer {
            decision: DecisionId(id),
            option: option.into(),
            note: note.map(str::to_owned),
        })
        .await
        .unwrap()
    {
        DecisionReply::Answered { recorded, .. } => recorded,
        other => panic!("not answered: {other:?}"),
    }
}

/// The lead's run on the outside event asks to fix the build, and ends.
async fn propose(hub: &TestHub) {
    outside(hub).await;
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Lead);
    assert_eq!(
        call(hub, &lease, &run, "c-1", CREATE_TASK, &fix_the_build()).await,
        Err(ToolError::WaitingOnYou)
    );
    assert_eq!(end(hub, &lease, &run).await, RunOutcome::Deferred);
}

#[tokio::test]
async fn each_mode_against_an_unrequested_external_event() {
    for mode in [Mode::OnRequest, Mode::Assisted, Mode::Autonomous] {
        let hub = hub(mode, Mode::OnRequest).await;
        let event = outside(&hub).await;
        let lease = hub.link.request_work().await.unwrap().expect("granted");
        let begun = hub.link.begin_run(&lease).await.unwrap();
        if mode == Mode::OnRequest {
            // Recorded, not acted on.
            assert_eq!(begun, Begin::Idle, "{mode}");
            hub.link.release(&lease).await.unwrap();
            let skipped = hub.event(event).await;
            assert_eq!(skipped.status, EventStatus::Skipped, "{mode}");
            assert_eq!(skipped.status_reason.as_deref(), Some(SKIP_REASON));
            assert_eq!(recorded(&hub).await, (vec![event], 1), "{mode}");
        } else {
            // The lead runs on it, and it is not listed as recorded.
            let Begin::Run(run) = begun else {
                panic!("{mode}: the lead runs");
            };
            assert_eq!(run.lane_kind, LaneKind::Lead, "{mode}");
            assert_eq!(run.events, [event], "{mode}");
            end(&hub, &lease, &run).await;
            assert_eq!(hub.event(event).await.status, EventStatus::Done, "{mode}");
            assert_eq!(recorded(&hub).await, (Vec::new(), 0), "{mode}");
        }
    }
}

#[tokio::test]
async fn a_task_from_unrequested_work_follows_the_stricter_of_the_two_agents() {
    let cases = [
        // (lead, assignee, created without asking)
        (Mode::Assisted, Mode::Assisted, false),
        (Mode::Autonomous, Mode::Autonomous, true),
        (Mode::Autonomous, Mode::OnRequest, false),
        (Mode::Assisted, Mode::Autonomous, false),
    ];
    for (lead, assignee, created) in cases {
        let what = format!("lead {lead}, assignee {assignee}");
        let hub = hub(lead, assignee).await;
        outside(&hub).await;
        let (lease, run) = begin(&*hub.link).await;
        let result = call(&hub, &lease, &run, "c-1", CREATE_TASK, &fix_the_build()).await;
        let tasks = hub.db.tasks(hub.general().await.id).await.unwrap();
        if created {
            let result = result.unwrap();
            assert_eq!(result["title"], json!("Fix the build"), "{what}");
            assert_eq!(tasks.len(), 1, "{what}");
            assert_eq!(tasks[0].origin, TaskOrigin::Agent, "{what}");
            assert!(hub.db.decision(DecisionId(1)).await.unwrap().is_none());
            assert_eq!(end(&hub, &lease, &run).await, RunOutcome::Silent);
            // Autonomous, the engineer starts on it though nobody asked.
            let (lease, task_run) = begin(&*hub.link).await;
            assert_eq!(task_run.lane_kind, LaneKind::Task, "{what}");
            assert_eq!(task_run.task_id, Some(tasks[0].id), "{what}");
            end(&hub, &lease, &task_run).await;
            continue;
        }
        assert_eq!(result, Err(ToolError::WaitingOnYou), "{what}");
        assert!(
            tasks.is_empty(),
            "{what}: nothing is created before you answer"
        );
        let decision = hub.db.decision(DecisionId(1)).await.unwrap().unwrap();
        assert_eq!(decision.kind, DecisionKind::Proposal, "{what}");
        assert_eq!(decision.tool, CREATE_TASK);
        assert_eq!(decision.arguments, fix_the_build());
        assert_eq!(
            decision.question,
            "Start a task for Engineer: Fix the build?"
        );
        let labels: Vec<&str> = decision.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            ["start", "not now", "do not propose this kind again"]
        );
        assert_eq!(decision.recommended, Some(0), "start is recommended");
        assert!(!decision.offers_standing_rule, "{what}");
        assert!(decision.expires_at.is_some(), "{what}");
        assert_eq!(end(&hub, &lease, &run).await, RunOutcome::Deferred);
    }
}

#[tokio::test]
async fn a_proposal_names_no_agent_or_repository_it_cannot_have() {
    let hub = hub(Mode::Assisted, Mode::Assisted).await;
    outside(&hub).await;
    let (lease, run) = begin(&*hub.link).await;
    for (id, arguments, why) in [
        (
            "c-1",
            json!({ "title": "x", "goal": "y", "agent": "Nobody" }),
            "no agent",
        ),
        (
            "c-2",
            json!({ "title": "x", "goal": "y", "repositories": ["elsewhere"] }),
            "no repository",
        ),
        ("c-3", json!({ "title": "x" }), "goal"),
    ] {
        match call(&hub, &lease, &run, id, CREATE_TASK, &arguments).await {
            Err(ToolError::InvalidArguments(message)) => {
                assert!(message.contains(why), "{why:?} in {message:?}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    assert!(
        hub.db.decision(DecisionId(1)).await.unwrap().is_none(),
        "nothing was asked"
    );
    end(&hub, &lease, &run).await;
}

#[tokio::test]
async fn starting_a_proposal_creates_the_task_once_even_killed_at_each_perform_step() {
    for step in [
        None,
        Some(perform::AFTER_BEGIN),
        Some(perform::AFTER_EFFECT),
        Some(perform::AFTER_FINISH),
    ] {
        let what = format!("{step:?}");
        let plan = step.map_or_else(CrashPlan::new, CrashPlan::kill_at);
        // The engineer is On request, so its task runs only as requested work.
        let mut hub = hub_with(Arc::new(plan.clone()), Mode::Assisted, Mode::OnRequest).await;
        propose(&hub).await;
        assert!(answer(&hub, 1, "start", None).await);
        let (mut lease, mut run) = begin(&*hub.link).await;
        assert_eq!(run.perform, [DecisionId(1)], "{what}");
        let mut performed = hub
            .link
            .begin_perform(&lease, run.run_id, DecisionId(1))
            .await;
        if step.is_some() {
            assert!(Stop::from(performed.unwrap_err()).is_killed(), "{what}");
            assert_eq!(plan.killed(), step);
            hub.clock.advance(PAST_LEASE);
            hub = hub.restart(Arc::new(NoHooks)).await;
            hub.core.sweep_once().await.unwrap();
            hub.hello().await;
            (lease, run) = begin(&*hub.link).await;
            assert_eq!(run.perform, [DecisionId(1)], "{what}: still to be acted on");
            performed = hub
                .link
                .begin_perform(&lease, run.run_id, DecisionId(1))
                .await;
        }
        let performed = performed.unwrap();
        let Perform::Recorded { outcome } = &performed else {
            panic!("{what}: the hub creates the task itself: {performed:?}");
        };
        assert_eq!(outcome["performed"], json!(true), "{what}: {outcome}");
        assert_eq!(
            outcome["summary"],
            json!("Task T-0001 is created: Fix the build."),
            "{what}"
        );
        assert_eq!(
            hub.link
                .begin_perform(&lease, run.run_id, DecisionId(1))
                .await
                .unwrap(),
            performed,
            "{what}: performed once"
        );
        end(&hub, &lease, &run).await;

        let tasks = hub.db.tasks(hub.general().await.id).await.unwrap();
        assert_eq!(tasks.len(), 1, "{what}: created exactly once");
        let task = &tasks[0];
        assert!(task.origin.is_requested(), "{what}: your work from now on");
        assert!(
            task.created_by_call
                .as_deref()
                .unwrap()
                .ends_with(":perform:D-0001"),
            "{what}: {:?}",
            task.created_by_call
        );
        // The On request engineer runs it: it is requested work.
        let (lease, task_run) = begin(&*hub.link).await;
        assert_eq!(task_run.task_id, Some(task.id), "{what}");
        end(&hub, &lease, &task_run).await;
    }
}

#[tokio::test]
async fn not_now_resumes_the_lead_with_your_note_and_creates_nothing() {
    let hub = hub(Mode::Assisted, Mode::Assisted).await;
    propose(&hub).await;
    assert!(answer(&hub, 1, "not now", Some("After the release.")).await);
    let (lease, run) = begin(&*hub.link).await;
    assert!(run.perform.is_empty(), "nothing to perform");
    assert!(
        run.prompt
            .contains("You declined D-0001 (create_task): \"not now\".")
            && run.prompt.contains("After the release."),
        "{}",
        run.prompt
    );
    end(&hub, &lease, &run).await;
    assert!(
        hub.db
            .tasks(hub.general().await.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        hub.db
            .read("projects/1/notes/declined-proposals.md")
            .await
            .unwrap()
            .is_none(),
        "a plain decline writes no note"
    );
}

#[tokio::test]
async fn do_not_propose_this_kind_again_declines_and_writes_the_lead_s_note_once() {
    let hub = hub(Mode::Assisted, Mode::Assisted).await;
    propose(&hub).await;
    assert!(answer(&hub, 1, "do not propose this kind again", None).await);
    assert!(
        !answer(&hub, 1, "do not propose this kind again", None).await,
        "ignored"
    );
    assert!(
        !answer(&hub, 1, "start", None).await,
        "the first answer stands"
    );

    let path = "projects/1/notes/declined-proposals.md";
    let note = hub
        .db
        .read(path)
        .await
        .unwrap()
        .expect("the note is written");
    assert_eq!(
        note.content,
        "# Declined proposals\nKinds of work the user asked you not to propose again.\n\n\
         - Do not propose work like \"Fix the build\" again (D-0001).\n"
    );
    assert_eq!(hub.db.history(path).await.unwrap().len(), 1, "written once");

    // The lead resumes with the decline and has the note in its index.
    let (lease, run) = begin(&*hub.link).await;
    assert!(run.perform.is_empty());
    assert!(run.prompt.contains("You declined D-0001"), "{}", run.prompt);
    assert!(
        run.appended_instructions.contains("Declined proposals"),
        "{}",
        run.appended_instructions
    );
    end(&hub, &lease, &run).await;
    assert!(
        hub.db
            .tasks(hub.general().await.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_proposal_whose_arguments_no_longer_hash_creates_nothing() {
    let hub = hub(Mode::Assisted, Mode::Assisted).await;
    outside(&hub).await;
    let (lease, run) = begin(&*hub.link).await;
    let tampered = hub
        .db
        .open_decision(
            Some(&lease),
            &NewDecision {
                project_id: hub.general().await.id,
                lane_id: hub.general().await.lead_lane_id,
                task_id: None,
                session_id: Some(run.session_id),
                run_id: Some(run.run_id),
                tool_call_id: Some("c-1".to_owned()),
                tool: CREATE_TASK.to_owned(),
                arguments: json!({ "title": "Something else", "goal": "y" }),
                arguments_hash: derived_call_key(CREATE_TASK, &fix_the_build()),
                kind: DecisionKind::Proposal,
                question: "Start a task?".to_owned(),
                context: String::new(),
                options: vec![
                    DecisionOption {
                        label: "start".to_owned(),
                        effect: json!({ "approve": true }),
                    },
                    DecisionOption {
                        label: "not now".to_owned(),
                        effect: json!({ "approve": false }),
                    },
                ],
                recommended: Some(0),
                offers_standing_rule: false,
                expires_at: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    end(&hub, &lease, &run).await;
    assert!(answer(&hub, tampered.id.get(), "start", None).await);
    let (lease, run) = begin(&*hub.link).await;
    let Perform::Recorded { outcome } = hub
        .link
        .begin_perform(&lease, run.run_id, tampered.id)
        .await
        .unwrap()
    else {
        panic!("a changed call must not be performed");
    };
    assert_eq!(outcome["performed"], json!(false));
    assert!(
        outcome["error"]
            .as_str()
            .unwrap()
            .contains("no longer match"),
        "{outcome}"
    );
    end(&hub, &lease, &run).await;
    assert!(
        hub.db
            .tasks(hub.general().await.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_agents_routine_asks_below_autonomous_and_its_events_follow_the_initiative_row() {
    let every_quarter_hour = json!({
        "instruction": "Check the build.",
        "rule": "interval",
        "minutes": 15,
        "zone": "UTC",
    });
    for mode in [Mode::OnRequest, Mode::Assisted, Mode::Autonomous] {
        let hub = hub(mode, Mode::OnRequest).await;
        hub.send("m-1", "Keep an eye on the build").await;
        let (lease, run) = begin(&*hub.link).await;
        let created = call(
            &hub,
            &lease,
            &run,
            "s-1",
            CREATE_SCHEDULE,
            &every_quarter_hour,
        )
        .await;
        if mode == Mode::Autonomous {
            assert_eq!(created.unwrap()["category"], json!("schedule"), "{mode}");
            end(&hub, &lease, &run).await;
        } else {
            assert_eq!(created, Err(ToolError::WaitingOnYou), "{mode}: asked");
            end(&hub, &lease, &run).await;
            assert!(answer(&hub, 1, "approve", None).await);
            let (lease, run) = begin(&*hub.link).await;
            let performed = hub
                .link
                .begin_perform(&lease, run.run_id, DecisionId(1))
                .await
                .unwrap();
            assert!(
                matches!(&performed, Perform::Recorded { outcome } if outcome["performed"] == json!(true)),
                "{mode}: {performed:?}"
            );
            end(&hub, &lease, &run).await;
        }
        let general = hub.general().await;
        let routines = hub.db.project_timers(general.id).await.unwrap();
        let routine = routines
            .iter()
            .find(|timer| timer.instruction == "Check the build.")
            .expect("the routine");
        assert!(
            !routine.requested,
            "{mode}: an agent's routine is not requested work"
        );

        hub.clock.advance(Duration::from_secs(15 * 60));
        hub.core.schedule_once().await.unwrap();
        let lease = hub.link.request_work().await.unwrap().expect("granted");
        let begun = hub.link.begin_run(&lease).await.unwrap();
        let (listed, _) = recorded(&hub).await;
        match begun {
            Begin::Idle => {
                assert_eq!(mode, Mode::OnRequest, "only On request records it");
                hub.link.release(&lease).await.unwrap();
                assert_eq!(listed.len(), 1, "{mode}");
                let event = hub.event(listed[0]).await;
                assert_eq!(event.kind, EventKind::TimerFired);
                assert_eq!(event.status_reason.as_deref(), Some(SKIP_REASON));
            }
            Begin::Run(run) => {
                assert_ne!(mode, Mode::OnRequest, "On request does not run it");
                assert!(run.prompt.contains("Check the build."), "{mode}");
                assert!(listed.is_empty(), "{mode}");
                end(&hub, &lease, &run).await;
            }
        }
    }
}
