//! Milestone 10 run interruption, fold-in, and mode changes through the client API.

use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::client::{LaneTarget, OrgReply, OrgRequest, UserMessage};
use threadlane_core::model::{EventKind, EventStatus, Mode, NewEvent, RunStatus, Trust};
use threadlane_core::runtime::steps::{runner, stop};
use threadlane_core::store::{InboxStore, OrgStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_hub::{OFF_REASON, SEND_MESSAGE};
use threadlane_runner::RunnerError;
use threadlane_testkit::git::git;
use threadlane_testkit::{CrashPlan, Script, ScriptStep};

use crate::harness::{
    PAST_LEASE, Sim, World, add_engineer, finish_after, plan_tasks_in, reply_script, until,
};
use crate::invariants::{assert_invariants, assert_quiet};

const TURN: Duration = Duration::from_secs(10);
const MID_RUN: &str = "Please include the new appendix.";

async fn request_stop(sim: &Sim, target: LaneTarget) -> OrgReply {
    sim.api
        .org(OrgRequest::Stop {
            target,
            client_request_id: "stop-1".to_owned(),
        })
        .await
        .unwrap()
}

async fn waiting(sim: &Sim, starts: usize, sleepers: usize) {
    until("backend has started and is waiting", || {
        std::future::ready(
            sim.backend().received().len() == starts && sim.clock().sleepers() >= sleepers,
        )
    })
    .await;
}

#[tokio::test]
async fn stop_interrupts_the_task_preserves_its_work_and_waits_for_user_input() {
    let world = World::new().await;
    add_engineer(&world.db, 1).await;
    world.add_repository("app").await;
    let sim = world.boot(CrashPlan::new()).await;
    sim.backend()
        .push(plan_tasks_in(&["app"], &[("page", &[])]));
    sim.backend().push(Script::new().then(ScriptStep::Hang));
    sim.backend().route("task_result", reply_script());
    sim.send("goal", "Build the page").await;
    assert!(sim.work_once().await.unwrap());

    let task = sim.task("T-0001").await;
    let run = sim.spawn_work_once();
    waiting(&sim, 2, 1).await;
    let worktree = sim.backend().received()[1].working_dir.clone();
    std::fs::write(worktree.join("page.txt"), "work in progress\n").unwrap();
    let now = sim.clock().now();
    assert_eq!(
        request_stop(&sim, LaneTarget::Task { task: task.id }).await,
        OrgReply::LaneUpdated
    );
    assert!(run.finish(&sim).await.unwrap());
    assert_eq!(
        sim.clock().now(),
        now,
        "stop does not wait for the backend deadline"
    );
    let stopped = sim.runs().await.remove(1);
    assert_eq!(stopped.status, RunStatus::Stopped);
    let events = sim.db().run_events(stopped.id).await.unwrap();
    assert_eq!(
        (events[0].status, events[0].attempts),
        (EventStatus::Pending, 0)
    );
    assert!(
        sim.events()
            .await
            .iter()
            .any(|event| event.kind == EventKind::Stop && event.status == EventStatus::Done)
    );
    assert_eq!(
        git(&worktree, &["show", "HEAD:page.txt"]),
        "work in progress\n"
    );
    assert!(
        !sim.work_once().await.unwrap(),
        "pending work cannot grant the stopped lane"
    );

    sim.backend().push(finish_after(Duration::ZERO));
    sim.api
        .send_message(&UserMessage {
            project: None,
            text: "Go on with the page.".to_owned(),
            images: Vec::new(),
            client_message_id: "go-on".to_owned(),
            task: Some(task.id),
        })
        .await
        .unwrap();
    assert_eq!(
        sim.work_until_idle().await.unwrap(),
        2,
        "the task resumes and the lead reports"
    );
    assert_eq!(sim.event(events[0].id).await.status, EventStatus::Done);
    assert_eq!(sim.event(events[0].id).await.attempts, 0);
    assert_invariants(&sim, "stop and resume a task").await;
    assert_quiet(&sim, "stop and resume a task").await;
}

fn reply_to_folded(prompt: &str) -> Json {
    let response = if prompt.contains(MID_RUN) {
        "The appendix is included."
    } else {
        "Only the original request."
    };
    json!({ "text": response })
}

fn fold_script() -> Script {
    Script::new()
        .wait(TURN)
        .then(ScriptStep::TurnBoundary)
        .call_for("answer", SEND_MESSAGE, reply_to_folded)
        .finish("Answered.")
}

#[tokio::test]
async fn a_mid_run_message_gets_an_answer_in_that_run_and_never_in_the_next() {
    let sim = Sim::start().await;
    sim.backend().push(fold_script());
    let first = sim.send("first", "Prepare the report.").await;
    let mut work = sim.spawn_work_once();
    work.wait_in_debounce(&sim).await;
    sim.clock().advance(sim.world.runner_config.debounce);
    waiting(&sim, 1, 2).await;
    let folded = sim.send("folded", MID_RUN).await;
    sim.clock().advance(TURN);
    assert!(work.finish(&sim).await.unwrap());
    assert_eq!(sim.backend().received().len(), 1);
    let run = sim.runs().await.remove(0);
    assert_eq!(run.status, RunStatus::Committed);
    let events = sim.db().run_events(run.id).await.unwrap();
    assert_eq!(
        events.iter().map(|event| event.id).collect::<Vec<_>>(),
        [first, folded]
    );
    assert_eq!(sim.dispatch().await.unwrap(), 1);
    assert_eq!(
        sim.feed().await[0].message.body["text"],
        "The appendix is included."
    );

    sim.backend().push(Script::new().finish("Done."));
    let later = sim.send("later", "A separate question.").await;
    assert!(sim.work_once().await.unwrap());
    assert!(!sim.backend().received()[1].prompt.contains(MID_RUN));
    assert_eq!(sim.committed_runs_with(folded).await.len(), 1);
    assert_eq!(sim.committed_runs_with(later).await.len(), 1);
    assert_invariants(&sim, "folded message consumed once").await;
}

#[tokio::test]
async fn a_backend_without_a_turn_boundary_leaves_the_message_for_the_next_run() {
    let sim = Sim::start().await;
    sim.backend()
        .push(Script::new().wait(TURN).finish("First turn."));
    sim.backend().push(Script::new().finish("Second turn."));
    sim.send("first", "Prepare the report.").await;
    let mut work = sim.spawn_work_once();
    work.wait_in_debounce(&sim).await;
    sim.clock().advance(sim.world.runner_config.debounce);
    waiting(&sim, 1, 2).await;
    let pending = sim.send("later", MID_RUN).await;
    sim.clock().advance(TURN);
    assert!(work.finish(&sim).await.unwrap());
    let runs = sim.committed_runs_with(pending).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(sim.runs().await[1].id, runs[0].id);
    assert!(sim.backend().received()[1].prompt.contains(MID_RUN));
    assert_invariants(&sim, "next-run fallback").await;
}

#[tokio::test]
async fn lowering_the_mode_mid_run_changes_the_next_tool_call_and_next_run() {
    let sim = Sim::start().await;
    sim.backend().push(
        Script::new()
            .call("before", SEND_MESSAGE, json!({"text": "before"}))
            .wait(TURN)
            .call("after", SEND_MESSAGE, json!({"text": "after"}))
            .finish("Finished."),
    );
    sim.send("first", "Send a report.").await;
    let mut work = sim.spawn_work_once();
    work.wait_in_debounce(&sim).await;
    sim.clock().advance(sim.world.runner_config.debounce);
    waiting(&sim, 1, 2).await;
    assert_eq!(sim.outbox().await.len(), 1);
    let lane = sim.general_lane().await;
    let lead = sim
        .db()
        .lead_binding(lane.project_id)
        .await
        .unwrap()
        .unwrap();
    let lead = sim.db().agent(lead.agent_id).await.unwrap().unwrap().name;
    sim.api
        .org(OrgRequest::SetMode {
            project: None,
            agent: lead,
            mode: Some(Mode::Off),
        })
        .await
        .unwrap();
    sim.clock().advance(TURN);
    assert!(work.finish(&sim).await.unwrap());
    let run = sim.runs().await.remove(0);
    let journal = sim.journal(run.id).await;
    assert!(journal.iter().any(|row| row.entry["type"] == "tool_result"
        && row.entry["call_id"] == "after"
        && row.entry["is_error"] == true));
    assert_eq!(sim.outbox().await.len(), 1);

    let next = sim
        .db()
        .insert_event(&NewEvent {
            project_id: lane.project_id,
            lane_id: lane.id,
            source: "test".to_owned(),
            source_event_id: "next".to_owned(),
            kind: EventKind::OutsideEvent,
            payload: json!({"text": "Next request"}),
            trust: Trust::System,
            requested: true,
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: sim.clock().now(),
        })
        .await
        .unwrap()
        .id();
    sim.work_until_idle().await.unwrap();
    assert_eq!(
        sim.backend().received().len(),
        1,
        "Off prevents the next run"
    );
    let event = sim.event(next).await;
    assert_eq!(event.status, EventStatus::Skipped);
    assert_eq!(event.status_reason.as_deref(), Some(OFF_REASON));
    assert_invariants(&sim, "mode change at the gate").await;
}

#[tokio::test]
async fn stop_survives_a_crash_before_the_cancel_is_pushed() {
    let sim = World::new()
        .await
        .boot(CrashPlan::kill_at(stop::AFTER_RECORD))
        .await;
    let event = sim.send("first", "Start work.").await;
    let link = sim.core.link(sim.machine);
    let lease = link.request_work().await.unwrap().unwrap();
    assert!(matches!(
        link.begin_run(&lease).await.unwrap(),
        threadlane_core::link::Begin::Run(_)
    ));
    let target = LaneTarget::Lead { project: None };
    assert!(
        sim.api
            .org(OrgRequest::Stop {
                target: target.clone(),
                client_request_id: "stop-1".to_owned()
            })
            .await
            .is_err()
    );
    assert_eq!(sim.plan.killed(), Some(stop::AFTER_RECORD));
    let sim = sim.restart(CrashPlan::new()).await;
    sim.clock().advance(PAST_LEASE);
    sim.core.sweep_once().await.unwrap();
    assert_eq!(sim.runs().await[0].status, RunStatus::Stopped);
    assert_eq!(sim.event(event).await.attempts, 0);
    assert!(!sim.work_once().await.unwrap());
    sim.backend().push(Script::new().finish("Resumed."));
    sim.api
        .org(OrgRequest::ResumeLane { target })
        .await
        .unwrap();
    assert!(sim.work_once().await.unwrap());
    assert_eq!(sim.event(event).await.status, EventStatus::Done);
    assert_invariants(&sim, "stop persisted before notification").await;
}

#[tokio::test]
async fn a_crash_after_recording_the_stopped_run_keeps_it_stopped() {
    let sim = World::new()
        .await
        .boot(CrashPlan::kill_at(runner::AFTER_STOP))
        .await;
    let event = sim.send("first", "Start work.").await;
    sim.backend().push(Script::new().then(ScriptStep::Hang));
    let mut work = sim.spawn_work_once();
    work.wait_in_debounce(&sim).await;
    sim.clock().advance(sim.world.runner_config.debounce);
    waiting(&sim, 1, 1).await;
    request_stop(&sim, LaneTarget::Lead { project: None }).await;
    assert!(
        matches!(work.finish(&sim).await, Err(RunnerError::Killed(killed)) if killed.step == runner::AFTER_STOP)
    );
    let sim = sim.restart(CrashPlan::new()).await;
    sim.clock().advance(PAST_LEASE);
    sim.core.sweep_once().await.unwrap();
    assert_eq!(sim.runs().await[0].status, RunStatus::Stopped);
    assert_eq!(sim.event(event).await.status, EventStatus::Pending);
    assert!(!sim.work_once().await.unwrap());
}

#[tokio::test]
async fn folded_input_returns_to_pending_if_its_run_is_killed() {
    let sim = World::new()
        .await
        .boot(CrashPlan::kill_at(runner::AFTER_FOLD_IN))
        .await;
    sim.backend().push(fold_script());
    sim.send("first", "Prepare the report.").await;
    let mut work = sim.spawn_work_once();
    work.wait_in_debounce(&sim).await;
    sim.clock().advance(sim.world.runner_config.debounce);
    waiting(&sim, 1, 2).await;
    let folded = sim.send("folded", MID_RUN).await;
    sim.clock().advance(TURN);
    assert!(
        matches!(work.finish(&sim).await, Err(RunnerError::Killed(killed)) if killed.step == runner::AFTER_FOLD_IN)
    );
    assert_eq!(sim.event(folded).await.status, EventStatus::Claimed);
    let sim = sim.restart(CrashPlan::new()).await;
    sim.clock().advance(PAST_LEASE);
    sim.core.sweep_once().await.unwrap();
    assert_eq!(sim.event(folded).await.status, EventStatus::Pending);
    sim.backend().push(
        Script::new()
            .call_for("answer", SEND_MESSAGE, reply_to_folded)
            .finish("Answered."),
    );
    assert!(sim.work_once().await.unwrap());
    assert_eq!(sim.committed_runs_with(folded).await.len(), 1);
    assert_eq!(sim.dispatch().await.unwrap(), 1);
    assert_eq!(
        sim.feed().await[0].message.body["text"],
        "The appendix is included."
    );
    assert_invariants(&sim, "folded input recovered after crash").await;
}
