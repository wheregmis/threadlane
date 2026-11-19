//! Milestone 3 stage A exit scenarios: one goal becomes several tasks that
//! run in parallel, a dependent task waits, and the lead sends one combined
//! result (Plan.md §15 milestone 3, docs/milestone-3.md §2).

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::json;
use threadlane_core::model::{EventKind, RunStatus, TaskStatus};
use threadlane_core::store::{InboxStore, SessionStore};
use threadlane_hub::{ASK_USER, CANCEL_TIMER, CREATE_SCHEDULE, LIST_TIMERS, SET_FOLLOW_UP};
use threadlane_hub::{
    CREATE_TASK, FINISH_TASK, MEMORY_CREATE, MEMORY_DELETE, MEMORY_EDIT, MEMORY_RENAME,
    MEMORY_VIEW, OPEN_PULL_REQUEST, PUBLISH_BRANCH, SEND_MESSAGE,
};
use threadlane_testkit::git::git;
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{Sim, World, add_engineer, finish_after, join, plan_tasks_in, until};
use crate::invariants::{assert_invariants, assert_one_active_run_per_lane, assert_quiet};
use crate::loops::{sleepers_at_least, spawn_hub, spawn_runner};

/// How long the slow tasks work, on the fake clock.
const WORK: Duration = Duration::from_secs(10);

/// What the lead tells you once every task is done.
const COMBINED: &str = "The report page is ready: data, page, and docs are done.";

/// A lead that reports every result it is shown in one message.
fn report_once() -> Script {
    Script::new()
        .call("toolu_s", SEND_MESSAGE, json!({ "text": COMBINED }))
        .finish("Told the user everything at once.")
}

/// A hub with an engineer that may run two tasks at once in General, whose
/// lead plans `tasks` for a goal and reports their results. The tasks
/// `slow` work for [`WORK`]; the rest finish at once.
async fn start(tasks: &[(&str, &[&str])], slow: &[&str]) -> Sim {
    start_in(&[], tasks, slow).await
}

/// [`start`], with the tasks working in `repositories`, registered in
/// General as local bare repositories.
async fn start_in(repositories: &[&str], tasks: &[(&str, &[&str])], slow: &[&str]) -> Sim {
    let world = World::new().await;
    add_engineer(&world.db, 2).await;
    for name in repositories {
        world.add_repository(name).await;
    }
    let sim = world.boot(CrashPlan::new()).await;
    let backend = sim.backend();
    backend.route("task_result", report_once());
    for id in slow {
        backend.route(format!("Your task, {id}:"), finish_after(WORK));
    }
    backend.route("task_start", finish_after(Duration::ZERO));
    backend.set_default(plan_tasks_in(repositories, tasks));
    sim
}

/// Waits until `runs` backend runs have started and wait on the clock.
async fn until_running(sim: &Sim, runs: usize) {
    let backend = sim.backend().clone();
    let clock = sim.clock().clone();
    // Each waiting run sleeps in its script and on its wall-time limit.
    until(&format!("{runs} backend runs wait"), || {
        std::future::ready(backend.received().len() == runs && clock.sleepers() >= 2 * (runs - 1))
    })
    .await;
}

#[tokio::test]
async fn one_goal_becomes_three_tasks_two_run_at_once_and_the_lead_sends_one_combined_result() {
    let sim = start_in(
        &["app"],
        &[("data", &[]), ("page", &[]), ("docs", &[])],
        &["T-0001", "T-0002"],
    )
    .await;
    let goal = sim.send("m-1", "Ship the report page").await;
    assert!(sim.work_once().await.unwrap(), "the lead plans");
    let tasks = sim.tasks().await;
    assert_eq!(tasks.len(), 3, "one goal, three tasks");
    assert!(tasks.iter().all(|task| task.status == TaskStatus::Queued));

    // Two tasks run at the same time; the third waits for a slot.
    let first = sim.spawn_work_once();
    until_running(&sim, 2).await;
    let second = sim.spawn_work_once();
    until_running(&sim, 3).await;
    let active: BTreeSet<_> = sim
        .runs()
        .await
        .iter()
        .filter(|run| run.status == RunStatus::Active)
        .map(|run| run.lane_id)
        .collect();
    let running: BTreeSet<_> = [sim.task("T-0001").await, sim.task("T-0002").await]
        .iter()
        .map(|task| {
            assert_eq!(task.status, TaskStatus::Running);
            task.lane_id
        })
        .collect();
    assert_eq!(active, running, "two task lanes, one active run each");
    assert_one_active_run_per_lane(&sim, "two tasks in progress").await;
    assert!(!sim.work_once().await.unwrap(), "nothing more may start");
    // The first task changes its worktree while it works.
    let worktree = |task: &str| {
        sim.world
            .workspaces()
            .join("worktrees")
            .join(task)
            .join("app")
    };
    std::fs::write(worktree("T-0001").join("data.txt"), "the data\n").unwrap();
    let third = sim.task("T-0003").await;
    assert_eq!(third.status, TaskStatus::Queued);
    assert_eq!(
        third.waiting_reason.as_deref(),
        Some("the project already runs its limit of 2 tasks at once")
    );

    sim.clock().advance(WORK);
    assert!(first.finish(&sim).await.unwrap());
    assert!(second.finish(&sim).await.unwrap());
    // The third task takes a slot, then the lead sees all three results.
    assert_eq!(sim.work_until_idle().await.unwrap(), 2);
    assert_eq!(sim.dispatch().await.unwrap(), 1);

    for task in sim.tasks().await {
        assert_eq!(task.status, TaskStatus::Done, "{}", task.id);
        assert_eq!(task.waiting_reason, None);
    }
    let lead_lane = sim.general_lane().await.id;
    let lead_runs: Vec<_> = sim
        .runs()
        .await
        .into_iter()
        .filter(|run| run.lane_id == lead_lane)
        .collect();
    assert_eq!(lead_runs.len(), 2, "one run to plan, one to report");
    let reported = sim.db().run_events(lead_runs[1].id).await.unwrap();
    assert_eq!(reported.len(), 3, "the three results in one run");
    assert!(
        reported
            .iter()
            .all(|event| event.kind == EventKind::TaskResult)
    );
    assert_eq!(sim.event(goal).await.claimed_by_run, Some(lead_runs[0].id));
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1, "one combined result");
    assert_eq!(feed[0].message.body, json!({ "text": COMBINED }));

    let received = sim.backend().received();
    assert_eq!(received.len(), 5);
    for run in &received {
        if run.tools.iter().any(|tool| tool == FINISH_TASK) {
            assert_eq!(
                run.tools,
                [
                    CREATE_TASK,
                    FINISH_TASK,
                    PUBLISH_BRANCH,
                    OPEN_PULL_REQUEST,
                    threadlane_hub::COMMENT,
                    threadlane_hub::CREATE_ISSUE,
                    threadlane_hub::MERGE,
                    threadlane_hub::MARK_READY,
                    MEMORY_VIEW,
                    MEMORY_CREATE,
                    MEMORY_EDIT,
                    MEMORY_DELETE,
                    MEMORY_RENAME,
                    SET_FOLLOW_UP,
                    CREATE_SCHEDULE,
                    LIST_TIMERS,
                    CANCEL_TIMER,
                    ASK_USER,
                ]
            );
            assert!(
                !run.remove_builtin_tools,
                "a task agent keeps its own tools"
            );
            assert!(run.prompt.contains("Your task, T-000"), "{}", run.prompt);
        } else {
            assert!(
                run.remove_builtin_tools,
                "a lead has no file or shell tools"
            );
            assert!(run.tools.iter().any(|tool| tool == SEND_MESSAGE));
        }
    }
    // Each task works in its own worktree of the one repository, on its own
    // branch, and what it left uncommitted was committed there.
    for run in &received[1..4] {
        let task = &run.prompt[run.prompt.find("T-").unwrap()..][..6];
        assert_eq!(run.working_dir, worktree(task), "{task}");
        assert_eq!(
            git(&run.working_dir, &["symbolic-ref", "--short", "HEAD"]).trim(),
            format!("threadlane/{task}")
        );
        assert_eq!(
            git(&run.working_dir, &["status", "--porcelain"]),
            "",
            "{task}"
        );
        assert!(
            run.deny_rules.iter().any(|rule| rule == "git push"),
            "{task}"
        );
    }
    let log = git(
        &worktree("T-0001"),
        &["log", "--format=%s", "--name-only", "-1"],
    );
    assert!(log.starts_with("Threadlane: work of T-0001"), "{log}");
    assert!(log.contains("data.txt"), "{log}");
    assert_eq!(
        git(&worktree("T-0002"), &["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "a task that changed nothing made no commit"
    );

    assert_quiet(&sim, "three tasks").await;
    assert_invariants(&sim, "three tasks").await;
}

#[tokio::test]
async fn a_dependent_task_waits_until_its_dependency_is_done() {
    let sim = start(&[("schema", &[]), ("migration", &["schema"])], &["T-0001"]).await;
    sim.send("m-1", "Change the schema and migrate").await;
    assert!(sim.work_once().await.unwrap());
    let dependent = sim.task("T-0002").await;
    assert_eq!(dependent.depends_on, vec![sim.task("T-0001").await.id]);

    let first = sim.spawn_work_once();
    until_running(&sim, 2).await;
    assert!(
        !sim.work_once().await.unwrap(),
        "the dependent task may not start"
    );
    let waiting = sim.task("T-0002").await;
    assert_eq!(waiting.status, TaskStatus::Queued);
    assert_eq!(
        waiting.waiting_reason.as_deref(),
        Some("waiting for T-0001 to be done")
    );
    assert!(
        sim.db()
            .latest_open_session(waiting.lane_id)
            .await
            .unwrap()
            .is_none()
    );

    sim.clock().advance(WORK);
    assert!(first.finish(&sim).await.unwrap());
    assert_eq!(sim.task("T-0001").await.status, TaskStatus::Done);
    assert!(sim.work_until_idle().await.unwrap() >= 2);
    sim.dispatch().await.unwrap();

    let done = sim.task("T-0002").await;
    assert_eq!(done.status, TaskStatus::Done);
    let runs = sim.runs().await;
    let ended = |lane| {
        runs.iter()
            .find(|run| run.lane_id == lane)
            .map(|run| (run.started_at, run.ended_at))
            .unwrap()
    };
    let (_, dependency_ended) = ended(sim.task("T-0001").await.lane_id);
    let (dependent_started, _) = ended(done.lane_id);
    assert!(
        Some(dependent_started) >= dependency_ended,
        "it started after its dependency ended"
    );
    assert!(
        runs.iter()
            .filter(|run| run.lane_id == done.lane_id)
            .all(|run| run.status == RunStatus::Committed),
        "it never ran before, and failed no run"
    );
    assert_eq!(sim.feed().await.len(), 1);

    assert_quiet(&sim, "dependent task").await;
    assert_invariants(&sim, "dependent task").await;
}

#[tokio::test]
async fn with_the_long_running_loops_results_that_end_close_together_reach_the_lead_in_one_run() {
    let sim = start_in(
        &["app"],
        &[("data", &[]), ("page", &[]), ("docs", &[])],
        &["T-0001", "T-0002"],
    )
    .await;
    let (stop_hub, hub) = spawn_hub(&sim);
    let (stop_runner, runner) = spawn_runner(&sim);
    // The sweep, dispatcher and scheduler loops, the runner's heartbeat and
    // poll.
    sleepers_at_least(&sim, "every loop waits on the clock", 5).await;
    sim.send("m-1", "Ship the report page").await;
    sleepers_at_least(&sim, "the lead debounces the goal", 6).await;
    sim.clock().advance(sim.world.runner_config.debounce);

    // The lead plans; two tasks run, each waiting on the clock.
    let probe = sim.clone();
    until("two tasks run", || {
        let sim = probe.clone();
        async move {
            let tasks = sim.tasks().await;
            tasks.len() == 3
                && tasks
                    .iter()
                    .filter(|task| task.status == TaskStatus::Running)
                    .count()
                    == 2
        }
    })
    .await;
    until_running(&sim, 3).await;
    sim.clock().advance(WORK);

    // Both finish at once and the third takes a slot and finishes; the lead
    // lane, claimed on the first result, waits out its debounce window.
    let probe = sim.clone();
    until("every task is done and the lead debounces", || {
        let sim = probe.clone();
        async move {
            let lead = sim.general_lane().await;
            sim.tasks()
                .await
                .iter()
                .all(|task| task.status == TaskStatus::Done)
                && lead.lease_owner.is_some()
                && sim.db().pending_events(lead.id).await.unwrap().len() == 3
                && sim.clock().sleepers() >= 6
        }
    })
    .await;
    sim.clock().advance(sim.world.runner_config.debounce);

    let probe = sim.clone();
    until(
        "the combined result is delivered and every lane released",
        || {
            let sim = probe.clone();
            async move {
                sim.feed().await.len() == 1
                    && sim
                        .lanes()
                        .await
                        .iter()
                        .all(|lane| lane.lease_owner.is_none())
            }
        },
    )
    .await;
    stop_runner.send(()).unwrap();
    stop_hub.send(()).unwrap();
    join("the runner stops", runner).await.unwrap();
    join("the hub stops", hub).await.unwrap();

    let lead_lane = sim.general_lane().await.id;
    let lead_runs: Vec<_> = sim
        .runs()
        .await
        .into_iter()
        .filter(|run| run.lane_id == lead_lane)
        .collect();
    assert_eq!(lead_runs.len(), 2, "one run to plan, one to report");
    let reported = sim.db().run_events(lead_runs[1].id).await.unwrap();
    assert_eq!(reported.len(), 3, "the three results in one run");
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1, "one combined result");
    assert_eq!(feed[0].message.body, json!({ "text": COMBINED }));
    assert_quiet(&sim, "three tasks, long-running loops").await;
    assert_invariants(&sim, "three tasks, long-running loops").await;
}
