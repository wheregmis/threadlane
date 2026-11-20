//! Killing the process at every step boundary, then restarting it (Plan.md
//! §15 milestone 1 exit, invariants 1, 3, 5, 6, 9; docs/milestone-1.md §10
//! item 3). The runner and the hub are one process here, as in the hub
//! binary: one crash plan stops both, and a restart builds both afresh on the
//! same database, clock, and backend.

use std::time::Duration;

use serde_json::json;
use threadlane_core::model::{EventKind, EventStatus, RunOutcome, TaskStatus};
use threadlane_core::runtime::steps::{
    begin_run, dispatcher, perform, placement, runner, tool_call,
};
use threadlane_core::runtime::{Killed, Step};
use threadlane_core::store::MemoryStore;
use threadlane_hub::{FINISH_TASK, HubError, MEMORY_CREATE, SET_FOLLOW_UP};
use threadlane_runner::{PROGRESS_PROMPT, QUIET_END_PROMPT, RunnerError};
use threadlane_store::UNWOKEN_REASON;
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{
    PAST_LEASE, REPLY, Sim, TASK_SUMMARY, World, add_engineer, finish_after, plan_tasks_in,
    progress_content, progress_turn, reply_script, write_progress,
};
use crate::invariants::{assert_invariants, assert_quiet};
use crate::permissions::asks_before_sharing;
use crate::publish::{self, ask_script, assert_published, drive, publish_world};

/// Every boundary one message passes through on its way to the feed.
fn kill_points() -> Vec<Step> {
    [
        runner::LEAD,
        begin_run::ALL,
        tool_call::ALL,
        dispatcher::ALL,
        placement::ALL,
    ]
    .concat()
}

/// Boundaries a second process never reaches for the same message, because
/// the first one got past them durably: the run was committed, or the reply
/// was marked sent.
const ONCE_ONLY: &[Step] = &[
    runner::AFTER_COMMIT,
    runner::AFTER_RECHECK,
    runner::AFTER_RELEASE,
    dispatcher::AFTER_MARK_SENT,
];

/// How many backend starts it takes to deliver the reply when the process is
/// killed at `step` in each of `kills` lives in a row, and why.
fn expected_starts(step: Step, kills: usize) -> usize {
    match step {
        // The backend was started and created its session: every life
        // resumes it, and the last one finishes.
        s if s == runner::AFTER_BACKEND_START
            || s == runner::AFTER_BACKEND_RUN
            || tool_call::ALL.contains(&s) =>
        {
            1 + kills
        }
        // The hub opened the run, and recorded the session id it chose,
        // but the backend never ran: the next life's resume finds no
        // session (one start that ends `SessionMissing`), the hub closes
        // the session, and a fresh one runs (a second start). A life
        // killed here again starts nothing.
        s if s == begin_run::AFTER_START_RUN
            || s == begin_run::AFTER_ASSEMBLE_CONTEXT
            || s == runner::AFTER_START_RUN =>
        {
            2
        }
        // Before any run was opened, or after the run was committed.
        _ => 1,
    }
}

/// How a life ended.
#[derive(Debug)]
enum Death {
    Runner(RunnerError),
    Hub(HubError),
}

impl Death {
    fn killed(&self) -> Option<Killed> {
        match self {
            Self::Runner(RunnerError::Killed(killed)) | Self::Hub(HubError::Killed(killed)) => {
                Some(*killed)
            }
            _ => None,
        }
    }
}

/// One process lifetime: release what expired, work every lane, deliver.
async fn live(sim: &Sim) -> Result<(), Death> {
    sim.core.sweep_once().await.map_err(Death::Hub)?;
    sim.work_until_idle().await.map_err(Death::Runner)?;
    sim.dispatch().await.map_err(Death::Hub)?;
    Ok(())
}

/// Sends one message, kills the process at `step` in each of `kills` lives,
/// restarts after each, and checks the end state from the stores. Returns
/// how many backend starts it took.
async fn kill_and_recover(step: Step, kills: usize) -> usize {
    let scenario = format!("{step} x{kills}");
    let mut sim = Sim::start().await;
    sim.backend().set_default(reply_script());
    let event = sim.send("m-1", "Are you there?").await;

    for life in 1..=kills {
        let plan = CrashPlan::kill_at(step);
        sim = sim.restart(plan.clone()).await;
        let died = live(&sim).await;
        let killed = died.as_ref().err().and_then(Death::killed);
        assert_eq!(
            killed.map(|killed| killed.step),
            Some(step),
            "{scenario}: life {life} was killed at the step, got {died:?}"
        );
        assert_eq!(plan.killed(), Some(step), "{scenario}: life {life}");
        // The process is gone; its lease runs out before the next one starts.
        sim.clock().advance(PAST_LEASE);
    }

    sim = sim.restart(CrashPlan::new()).await;
    live(&sim)
        .await
        .unwrap_or_else(|death| panic!("{scenario}: recovery failed: {death:?}"));

    assert_invariants(&sim, &scenario).await;
    assert_eq!(
        sim.event(event).await.status,
        EventStatus::Done,
        "{scenario}: the event is done"
    );
    let committed = sim.committed_runs_with(event).await;
    assert_eq!(
        committed.len(),
        1,
        "{scenario}: one committed run consumed it"
    );
    assert_eq!(
        committed[0].outcome,
        Some(RunOutcome::Sent),
        "{scenario}: the run that consumed it sent the reply"
    );
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1, "{scenario}: exactly one reply in the feed");
    assert_eq!(feed[0].message.body, json!({ "text": REPLY }), "{scenario}");
    assert_quiet(&sim, &scenario).await;

    let starts = sim.backend().received().len();
    assert_eq!(
        starts,
        expected_starts(step, kills),
        "{scenario}: backend starts: {:?}",
        sim.backend()
            .received()
            .iter()
            .map(|run| run.session_outcome)
            .collect::<Vec<_>>()
    );
    starts
}

#[tokio::test]
async fn killing_the_process_at_any_step_still_yields_exactly_one_reply() {
    let steps = kill_points();
    assert_eq!(steps.len(), 20);
    let mut starts = Vec::new();
    for step in steps {
        starts.push((step, kill_and_recover(step, 1).await));
    }
    // Shown with --nocapture: the starts each recovery took.
    for (step, starts) in starts {
        println!("{step}: {starts} backend start(s)");
    }
}

#[tokio::test]
async fn killing_the_process_twice_at_the_same_step_still_yields_exactly_one_reply() {
    for step in kill_points() {
        if ONCE_ONLY.contains(&step) {
            continue;
        }
        let starts = kill_and_recover(step, 2).await;
        println!("{step} twice: {starts} backend start(s)");
    }
}

/// The steps left out of the double kill are not reached by a second
/// process, so a second kill there is impossible rather than skipped.
#[tokio::test]
async fn a_second_process_does_not_pass_a_step_the_first_got_past_durably() {
    for &step in ONCE_ONLY {
        let sim = Sim::start().await;
        sim.backend().set_default(reply_script());
        sim.send("m-1", "Are you there?").await;
        let first = sim.restart(CrashPlan::kill_at(step)).await;
        let died = live(&first).await;
        assert_eq!(
            died.as_ref().err().and_then(Death::killed).map(|k| k.step),
            Some(step),
            "{step}: {died:?}"
        );
        sim.clock().advance(PAST_LEASE);

        let second_plan = CrashPlan::kill_at(step);
        let second = sim.restart(second_plan.clone()).await;
        live(&second)
            .await
            .unwrap_or_else(|death| panic!("{step}: the second life failed: {death:?}"));
        assert_eq!(second_plan.killed(), None, "{step} is not reached again");
        assert!(!second_plan.reached().contains(&step), "{step}");
        assert_eq!(second.feed().await.len(), 1, "{step}");
        assert_quiet(&second, step.0).await;
        assert_invariants(&second, step.0).await;
    }
}

/// A world whose lead turns a message into one task in a repository, whose
/// task agent finishes it in its worktree, and whose lead then replies with
/// [`REPLY`]. Each kind of run gets its script from its prompt, so a run
/// repeated after a crash plays the same script whichever lane runs first.
async fn task_world() -> World {
    task_world_with(finish_after(Duration::ZERO), progress_turn()).await
}

/// [`task_world`] whose task agent plays `task` and, in a final progress
/// turn, `progress`.
async fn task_world_with(task: Script, progress: Script) -> World {
    let world = World::new().await;
    add_engineer(&world.db, 1).await;
    world.add_repository("app").await;
    world.backend.route(PROGRESS_PROMPT, progress);
    world.backend.route("task_result", reply_script());
    world.backend.route("task_start", task);
    world
        .backend
        .set_default(plan_tasks_in(&["app"], &[("page", &[])]));
    world
}

/// A task world whose task agent ends its run without writing its progress
/// file, so the runner gives it the final progress turn, in which it writes
/// the file and finishes the task.
async fn progress_turn_world() -> World {
    task_world_with(
        Script::new()
            .text("Working on it.")
            .finish("Stopping here."),
        // The progress prompt names no task; the session knows it is T-0001.
        Script::new()
            .call(
                "toolu_p",
                MEMORY_CREATE,
                json!({ "path": "project/tasks/T-0001.md", "content": progress_content("T-0001") }),
            )
            .call("toolu_f", FINISH_TASK, json!({ "summary": TASK_SUMMARY }))
            .finish("Saved my progress and finished."),
    )
    .await
}

/// Sends the message through a task, kills the process the `nth` time it
/// reaches `step`, restarts, and checks the end state from the stores.
async fn kill_task_cycle_and_recover(step: Step, nth: u32) {
    kill_cycle_and_recover(task_world().await, "task cycle", step, nth).await;
}

/// [`kill_task_cycle_and_recover`] in `world`.
async fn kill_cycle_and_recover(world: World, name: &str, step: Step, nth: u32) {
    let scenario = format!("{name}, {step} #{nth}");
    let plan = CrashPlan::kill_at_nth(step, nth);
    let sim = world.boot(plan.clone()).await;
    let event = sim.send("m-1", "Put the page up").await;
    let died = live(&sim).await;
    assert_eq!(
        died.as_ref()
            .err()
            .and_then(Death::killed)
            .map(|killed| killed.step),
        Some(step),
        "{scenario}: killed at the step, got {died:?}"
    );
    sim.clock().advance(PAST_LEASE);

    let sim = sim.restart(CrashPlan::new()).await;
    live(&sim)
        .await
        .unwrap_or_else(|death| panic!("{scenario}: recovery failed: {death:?}"));

    assert_invariants(&sim, &scenario).await;
    assert_quiet(&sim, &scenario).await;
    assert_eq!(
        sim.event(event).await.status,
        EventStatus::Done,
        "{scenario}"
    );
    let tasks = sim.tasks().await;
    assert_eq!(tasks.len(), 1, "{scenario}: one task");
    assert_eq!(tasks[0].status, TaskStatus::Done, "{scenario}");
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1, "{scenario}: exactly one reply in the feed");
    assert_eq!(feed[0].message.body, json!({ "text": REPLY }), "{scenario}");
    let progress = sim
        .db()
        .history("projects/1/tasks/T-0001.md")
        .await
        .unwrap();
    assert_eq!(
        progress.len(),
        1,
        "{scenario}: the progress file is written once"
    );
}

/// The task cycle with a final progress turn, killed the `nth` time it
/// reaches each boundary it passes, inside the extra turn included: one
/// progress file version, one task done, one reply.
#[tokio::test]
async fn killing_the_process_at_any_step_of_a_task_that_takes_its_final_progress_turn() {
    let plan = CrashPlan::new();
    let sim = progress_turn_world().await.boot(plan.clone()).await;
    sim.send("m-1", "Put the page up").await;
    live(&sim).await.unwrap();
    assert_eq!(sim.feed().await.len(), 1);
    let task_runs: Vec<_> = sim
        .backend()
        .received()
        .into_iter()
        .filter(|run| run.tools.iter().any(|tool| tool == FINISH_TASK))
        .collect();
    assert_eq!(task_runs.len(), 2, "the task's run and its progress turn");
    // Nothing is set to wake the task either: one turn asks for both.
    assert_eq!(
        task_runs[1].prompt,
        format!("{PROGRESS_PROMPT}\n\n{QUIET_END_PROMPT}")
    );
    assert_eq!(task_runs[1].session_id(), task_runs[0].session_id());
    let reached = plan.reached();
    for (index, &step) in reached.iter().enumerate() {
        let nth = reached[..=index]
            .iter()
            .filter(|seen| **seen == step)
            .count();
        let world = progress_turn_world().await;
        kill_cycle_and_recover(world, "progress turn", step, u32::try_from(nth).unwrap()).await;
    }
    println!(
        "{} kill points in a task with a progress turn",
        reached.len()
    );
}

#[tokio::test]
async fn killing_the_process_at_any_step_of_a_task_still_yields_one_task_one_result_and_one_reply()
{
    let plan = CrashPlan::new();
    let sim = task_world().await.boot(plan.clone()).await;
    sim.send("m-1", "Put the page up").await;
    live(&sim).await.unwrap();
    assert_eq!(sim.feed().await.len(), 1);
    let reached = plan.reached();
    // The task lane passes its own boundaries, besides the lead's.
    for step in [
        runner::TASK,
        begin_run::ALL,
        tool_call::ALL,
        placement::ALL,
        dispatcher::ALL,
    ]
    .concat()
    {
        assert!(reached.contains(&step), "the cycle reaches {step}");
    }
    // Every time any boundary is passed, in either lane.
    for (index, &step) in reached.iter().enumerate() {
        let nth = reached[..=index]
            .iter()
            .filter(|seen| **seen == step)
            .count();
        kill_task_cycle_and_recover(step, u32::try_from(nth).unwrap()).await;
    }
    println!("{} kill points in a task cycle", reached.len());
}

/// The publish scenario of docs/milestone-3.md §4, killed the `nth` time it
/// reaches each boundary it passes, the new ones of asking and performing
/// included: whatever is interrupted, exactly one draft pull request.
#[tokio::test]
async fn killing_the_process_at_any_step_still_opens_exactly_one_pull_request() {
    // Asked and approved, and allowed by default: performed the same way.
    for asks in [true, false] {
        let world = || async {
            let world = publish_world(World::new().await, ask_script()).await;
            if asks {
                asks_before_sharing(&world).await;
            }
            world
        };
        let plan = CrashPlan::new();
        let sim = world().await.boot(plan.clone()).await;
        sim.send("m-1", "Put the page up and open a pull request")
            .await;
        drive(&sim, "approve", None).await.unwrap();
        assert_published(&sim, "reference").await;
        let reached = plan.reached();
        for step in [runner::TASK, tool_call::ALL, perform::ALL].concat() {
            assert!(reached.contains(&step), "the scenario reaches {step}");
        }

        for (index, &step) in reached.iter().enumerate() {
            let nth = reached[..=index]
                .iter()
                .filter(|seen| **seen == step)
                .count();
            let scenario = format!("publish (asks {asks}), {step} #{nth}");
            let plan = CrashPlan::kill_at_nth(step, u32::try_from(nth).unwrap());
            let sim = world().await.boot(plan.clone()).await;
            sim.send("m-1", "Put the page up and open a pull request")
                .await;
            let died = drive(&sim, "approve", None).await;
            assert_eq!(
                died.as_ref().err().and_then(publish::Death::killed_at),
                Some(step),
                "{scenario}: killed at the step, got {died:?}"
            );
            sim.clock().advance(PAST_LEASE);
            let sim = sim.restart(CrashPlan::new()).await;
            drive(&sim, "approve", None)
                .await
                .unwrap_or_else(|death| panic!("{scenario}: recovery failed: {death:?}"));
            assert_published(&sim, &scenario).await;
        }
        println!("{} kill points in a publish (asks {asks})", reached.len());
    }
}

/// A task world whose task agent, as in the real run, writes its progress
/// file and ends its turn saying it waits for checks it left running, and
/// in the extra turn of a quiet end plays `quiet`.
async fn quiet_world(quiet: Script) -> World {
    let world = task_world_with(
        Script::new()
            .text("Running the checks.")
            .call_for("toolu_p", MEMORY_CREATE, write_progress)
            .finish("Waiting for clippy and fmt to finish."),
        progress_turn(),
    )
    .await;
    world.backend.route(QUIET_END_PROMPT, quiet);
    world
}

/// The runs of the task agent, which alone is offered `finish_task`.
fn task_runs(sim: &Sim) -> Vec<threadlane_testkit::ReceivedRun> {
    sim.backend()
        .received()
        .into_iter()
        .filter(|run| run.tools.iter().any(|tool| tool == FINISH_TASK))
        .collect()
}

/// How many notices of a task left with nothing to wake it reached the lead.
async fn unwoken_notices(sim: &Sim) -> usize {
    sim.events()
        .await
        .into_iter()
        .filter(|event| {
            event.kind == EventKind::TaskResult && event.payload["reason"] == UNWOKEN_REASON
        })
        .count()
}

/// The end state of [`quiet_world`] after an extra turn that did nothing:
/// the task sleeps saying why, and the lead was told once and told you once.
async fn assert_left_unwoken(sim: &Sim, scenario: &str) {
    let tasks = sim.tasks().await;
    assert_eq!(tasks.len(), 1, "{scenario}: one task");
    assert_eq!(tasks[0].status, TaskStatus::Sleeping, "{scenario}");
    assert_eq!(
        tasks[0].waiting_reason.as_deref(),
        Some(UNWOKEN_REASON),
        "{scenario}"
    );
    assert_eq!(unwoken_notices(sim).await, 1, "{scenario}: one notice");
    assert_eq!(sim.feed().await.len(), 1, "{scenario}: one reply");
}

/// The real run's defect: a task ends its run with nothing to wake it. It
/// gets one extra turn; when that turn changes nothing either, the task is
/// marked and the lead told exactly once, whatever step the process is
/// killed at.
#[tokio::test]
async fn a_task_left_with_nothing_to_wake_it_says_so_once_at_any_kill() {
    let plan = CrashPlan::new();
    let sim = quiet_world(progress_turn()).await.boot(plan.clone()).await;
    sim.send("m-1", "Put the page up").await;
    live(&sim).await.unwrap();
    let runs = task_runs(&sim);
    assert_eq!(runs.len(), 2, "the task's run and exactly one extra turn");
    assert_eq!(runs[1].prompt, QUIET_END_PROMPT);
    assert_eq!(runs[1].session_id(), runs[0].session_id());
    assert_left_unwoken(&sim, "no kill").await;

    let reached = plan.reached();
    for (index, &step) in reached.iter().enumerate() {
        let nth = reached[..=index]
            .iter()
            .filter(|seen| **seen == step)
            .count();
        let scenario = format!("quiet end, {step} #{nth}");
        let plan = CrashPlan::kill_at_nth(step, u32::try_from(nth).unwrap());
        let sim = quiet_world(progress_turn()).await.boot(plan).await;
        sim.send("m-1", "Put the page up").await;
        let died = live(&sim).await;
        assert_eq!(
            died.as_ref().err().and_then(Death::killed).map(|k| k.step),
            Some(step),
            "{scenario}: {died:?}"
        );
        sim.clock().advance(PAST_LEASE);
        let sim = sim.restart(CrashPlan::new()).await;
        live(&sim)
            .await
            .unwrap_or_else(|death| panic!("{scenario}: recovery failed: {death:?}"));
        assert_invariants(&sim, &scenario).await;
        assert_quiet(&sim, &scenario).await;
        assert_left_unwoken(&sim, &scenario).await;
    }
}

/// Finishing in the extra turn ends the task as finishing ever does.
#[tokio::test]
async fn finishing_in_the_extra_turn_ends_the_task() {
    let sim = quiet_world(
        Script::new()
            .call("toolu_f", FINISH_TASK, json!({ "summary": TASK_SUMMARY }))
            .finish("Finished after all."),
    )
    .await
    .boot(CrashPlan::new())
    .await;
    sim.send("m-1", "Put the page up").await;
    live(&sim).await.unwrap();

    assert_eq!(task_runs(&sim).len(), 2);
    let tasks = sim.tasks().await;
    assert_eq!(tasks[0].status, TaskStatus::Done);
    assert_eq!(tasks[0].waiting_reason, None);
    assert_eq!(unwoken_notices(&sim).await, 0);
    assert_eq!(sim.feed().await.len(), 1);
}

/// A follow-up set in the run is something to wake the task: no extra turn,
/// no mark, no notice.
#[tokio::test]
async fn a_task_that_set_a_follow_up_gets_no_extra_turn() {
    let world = quiet_world(progress_turn()).await;
    world.backend.route(
        "task_start",
        Script::new()
            .call_for("toolu_p", MEMORY_CREATE, write_progress)
            .call(
                "toolu_s",
                SET_FOLLOW_UP,
                json!({ "instruction": "Look at the checks again.", "in_minutes": 30 }),
            )
            .finish("Set a follow-up for the checks."),
    );
    let sim = world.boot(CrashPlan::new()).await;
    sim.send("m-1", "Put the page up").await;
    live(&sim).await.unwrap();

    assert_eq!(task_runs(&sim).len(), 1);
    let tasks = sim.tasks().await;
    assert_eq!(tasks[0].status, TaskStatus::Sleeping);
    assert_eq!(tasks[0].waiting_reason, None);
    assert_eq!(unwoken_notices(&sim).await, 0);
}
