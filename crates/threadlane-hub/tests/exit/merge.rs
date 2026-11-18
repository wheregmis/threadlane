//! docs/merge-help.md: a task finds out at `finish_task` that its branch
//! conflicts with its base, and a run start brings the base in. The real
//! hub and the real runner, with `GitWorkspaces` on a local bare repository.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use threadlane_core::client::{LaneTarget, OrgRequest, UserMessage};
use threadlane_core::ids::RunId;
use threadlane_core::model::TaskStatus;
use threadlane_hub::{FINISH_TASK, MEMORY_CREATE};
use threadlane_testkit::git::git;
use threadlane_testkit::{CrashPlan, Script, ScriptStep};

use crate::harness::{
    Sim, TASK_SUMMARY, World, add_engineer, finish_after, plan_tasks_in, reply_script, until,
    write_progress,
};

const TURN: Duration = Duration::from_secs(10);
const TASK_README: &str = "# app, by the task\n";

/// Lands a commit writing `README.md` on the remote's `main`, as another
/// task's merged branch would; returns that commit.
fn land_on_main(sim: &Sim, remote: &str, content: &str) -> String {
    let other = sim.world.lanes.path().join("other");
    if !other.exists() {
        git(
            sim.world.lanes.path(),
            &["clone", "--quiet", remote, "other"],
        );
    }
    git(&other, &["pull", "--quiet", "--ff-only"]);
    std::fs::write(other.join("README.md"), content).unwrap();
    git(&other, &["commit", "--quiet", "--no-verify", "-am", "Land"]);
    git(&other, &["push", "--quiet", "origin", "main"]);
    git(&other, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Until the backend has started `starts` runs and the latest waits on the
/// clock (the runner's heartbeat is the other sleeper).
async fn waiting(sim: &Sim, starts: usize) {
    until("the task run waits on the clock", || {
        std::future::ready(sim.backend().received().len() == starts && sim.clock().sleepers() >= 2)
    })
    .await;
}

/// The run's journal as text, to look for what a tool said.
async fn journal_text(sim: &Sim, run: RunId) -> String {
    sim.journal(run)
        .await
        .iter()
        .map(|entry| entry.entry.to_string())
        .collect()
}

async fn world() -> (World, String) {
    let world = World::new().await;
    add_engineer(&world.db, 1).await;
    let repository = world.add_repository("app").await;
    world.backend.route("task_result", reply_script());
    (world, repository.remote_url)
}

#[tokio::test]
async fn a_task_branch_that_merges_cleanly_finishes_and_says_so() {
    let (world, _) = world().await;
    let sim = world.boot(CrashPlan::new()).await;
    sim.backend()
        .push(plan_tasks_in(&["app"], &[("page", &[])]));
    sim.backend().push(finish_after(Duration::ZERO));
    sim.send("goal", "Build the page").await;
    sim.work_until_idle().await.unwrap();

    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::Done);
    let merge = &task.result.as_ref().unwrap()["merge"];
    assert_eq!(merge[0]["repository"], "app");
    assert_eq!(merge[0]["branch"], "main");
    assert_eq!(merge[0]["conflicts"], json!([]));
}

#[tokio::test]
async fn a_conflict_is_refused_with_its_files_and_finishes_after_the_agent_rebases() {
    let (world, remote) = world().await;
    let sim = world.boot(CrashPlan::new()).await;
    sim.backend()
        .push(plan_tasks_in(&["app"], &[("page", &[])]));
    sim.backend().push(Script::new().then(ScriptStep::Hang));
    sim.send("goal", "Build the page").await;
    assert!(sim.work_once().await.unwrap());
    let task = sim.task("T-0001").await;

    // The task changes README.md while another task's change to it lands.
    let run = sim.spawn_work_once();
    until("the task run started", || {
        std::future::ready(sim.backend().received().len() == 2)
    })
    .await;
    let worktree = sim.backend().received()[1].working_dir.clone();
    std::fs::write(worktree.join("README.md"), TASK_README).unwrap();
    let base = land_on_main(&sim, &remote, "# app, by another task\n");
    sim.api
        .org(OrgRequest::Stop {
            target: LaneTarget::Task { task: task.id },
            client_request_id: "stop-1".to_owned(),
        })
        .await
        .unwrap();
    assert!(run.finish(&sim).await.unwrap());

    // The next run starts with the branch left as it was, and says so.
    sim.backend().push(
        Script::new()
            .call_for("toolu_p", MEMORY_CREATE, write_progress)
            .call("toolu_f1", FINISH_TASK, json!({ "summary": TASK_SUMMARY }))
            .wait(TURN)
            .call("toolu_f2", FINISH_TASK, json!({ "summary": TASK_SUMMARY }))
            .finish("Finished the task."),
    );
    sim.api
        .send_message(&UserMessage {
            project: None,
            text: "Go on.".to_owned(),
            images: Vec::new(),
            client_message_id: "go-on".to_owned(),
            task: Some(task.id),
        })
        .await
        .unwrap();
    let mut work = sim.spawn_work_once();
    work.wait_in_debounce(&sim).await;
    sim.clock().advance(sim.world.runner_config.debounce);
    waiting(&sim, 3).await;
    let prompt = sim.backend().received()[2].prompt.clone();
    assert!(
        prompt.contains(&format!("conflicts with main at {base}, in: README.md")),
        "{prompt}"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        TASK_README,
        "a conflicting branch is not rebased"
    );

    // Refused; the agent rebases and resolves, then finishes.
    git(&worktree, &["reset", "--quiet", "--hard", "origin/main"]);
    std::fs::write(worktree.join("README.md"), "# app, by both\n").unwrap();
    git(
        &worktree,
        &["commit", "--quiet", "--no-verify", "-am", "Resolve"],
    );
    sim.clock().advance(TURN);
    assert!(work.finish(&sim).await.unwrap());
    sim.work_until_idle().await.unwrap();

    let runs = sim.runs().await;
    let journal = journal_text(&sim, runs[2].id).await;
    assert!(journal.contains("finish_task refused"), "{journal}");
    assert!(journal.contains(&format!("conflicts with main at {base} in: README.md")));
    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::Done);
    let merge = &task.result.as_ref().unwrap()["merge"][0];
    assert_eq!(merge["base"], json!(base));
    assert_eq!(merge["conflicts"], json!([]));
}

#[tokio::test]
async fn the_third_conflict_is_recorded_not_refused() {
    let (world, remote) = world().await;
    let sim = world.boot(CrashPlan::new()).await;
    sim.backend()
        .push(plan_tasks_in(&["app"], &[("page", &[])]));
    let mut script = Script::new()
        .wait(TURN)
        .call_for("toolu_p", MEMORY_CREATE, write_progress);
    for n in 1..=3 {
        script = script.call(
            format!("toolu_f{n}"),
            FINISH_TASK,
            json!({ "summary": TASK_SUMMARY }),
        );
    }
    sim.backend().push(script.finish("Finished the task."));
    sim.send("goal", "Build the page").await;
    assert!(sim.work_once().await.unwrap());

    let work = sim.spawn_work_once();
    waiting(&sim, 2).await;
    let worktree = sim.backend().received()[1].working_dir.clone();
    std::fs::write(worktree.join("README.md"), TASK_README).unwrap();
    let base = land_on_main(&sim, &remote, "# app, by another task\n");
    sim.clock().advance(TURN);
    assert!(work.finish(&sim).await.unwrap());
    sim.work_until_idle().await.unwrap();

    let runs = sim.runs().await;
    let journal = journal_text(&sim, runs[1].id).await;
    assert_eq!(
        journal.matches("finish_task refused").count(),
        2,
        "{journal}"
    );
    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::Done, "a task never loops");
    let merge = &task.result.as_ref().unwrap()["merge"][0];
    assert_eq!(merge["base"], json!(base));
    assert_eq!(merge["conflicts"], json!(["README.md"]));
    assert!(Path::new(&worktree).is_dir());
}
