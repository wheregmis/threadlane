//! Exit scenarios of docs/milestone-8.md §3: a check result on a task's pull
//! request wakes that task within about a minute, and a review request that
//! belongs to no task is recorded and not acted on.
//!
//! The real hub with its poller loop, the real runner, and the fake GitHub.

use std::time::Duration;

use threadlane_core::github::{GITHUB_SOURCE, GithubItem, GithubKind};
use threadlane_core::model::{EventStatus, TaskStatus};
use threadlane_core::runtime::steps::github::AFTER_WAKE;
use threadlane_core::time::Clock;
use threadlane_hub::SKIP_REASON;
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{
    REPLY, Sim, TASK_SUMMARY, World, add_engineer, count, finish_after, join, plan_tasks_in,
    reply_script, until,
};
use crate::invariants::{assert_invariants, assert_quiet};
use crate::loops::{sleepers_at_least, spawn_hub};
use crate::publish::{ask_script, drive};

const REMOTE: &str = "git@github.com:acme/app.git";
const BRANCH: &str = "threadlane/T-0001";

fn check_failed(at: &str) -> GithubItem {
    GithubItem {
        id: "check_run:31000000001".into(),
        kind: GithubKind::CheckResult,
        repository: "acme/app".into(),
        pull_request: Some(1),
        branch: Some(BRANCH.into()),
        author: Some("github-actions".into()),
        title: "test: failure".into(),
        body: "2 tests failed".into(),
        url: None,
        at: at.into(),
    }
}

async fn github_events(sim: &Sim) -> Vec<threadlane_core::model::Event> {
    sim.events()
        .await
        .into_iter()
        .filter(|event| event.source == GITHUB_SOURCE)
        .collect()
}

#[tokio::test]
async fn a_check_result_on_a_tasks_pull_request_wakes_that_task_within_about_a_minute() {
    let world = World::new().await.with_github();
    add_engineer(&world.db, 1).await;
    world.add_github_repository("app", REMOTE).await;
    // The lead plans one task in app; the task opens its pull request,
    // then waits for the checks; the failed check makes it finish.
    world
        .backend
        .route("outside_event", finish_after(Duration::ZERO));
    world.backend.route(
        "decision_answer",
        Script::new()
            .text("The pull request is open; the checks are running.")
            .finish("Waiting for the checks."),
    );
    world.backend.route("task_result", reply_script());
    world.backend.route("task_start", ask_script());
    world
        .backend
        .set_default(plan_tasks_in(&["app"], &[("page", &[])]));
    let github = world.github.clone().unwrap();
    let sim = world.boot(CrashPlan::new()).await;
    sim.send("m-1", "Put the page up and open a pull request")
        .await;
    drive(&sim, "approve", None).await.unwrap();
    assert_eq!(
        sim.world.forge.pull_requests(),
        ["https://forge.test/pull/1"]
    );
    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::Sleeping, "waits for its checks");

    let (stop_hub, hub) = spawn_hub(&sim);
    let plan = sim.plan.clone();
    until("the poller makes its first pass", || {
        std::future::ready(count(&plan, AFTER_WAKE) == 1)
    })
    .await;
    sleepers_at_least(&sim, "the sweep, dispatcher, scheduler and poller wait", 4).await;
    let finished_at = sim.clock().now();
    github.add_to("acme/app", BRANCH, check_failed("2026-11-17T09:00:00Z"));
    sim.clock().advance(Duration::from_secs(60));
    until("the poller makes a pass", || {
        std::future::ready(count(&plan, AFTER_WAKE) == 2)
    })
    .await;
    let _ = stop_hub.send(());
    join("the hub stops", hub).await.unwrap();

    let events = github_events(&sim).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].lane_id, task.lane_id, "the task's own lane");
    assert!(events[0].requested, "it continues the task");
    assert!(
        events[0].received_at <= finished_at + Duration::from_secs(60),
        "within a minute"
    );
    assert_eq!(sim.task("T-0001").await.status, TaskStatus::Queued, "woken");

    sim.work_until_idle().await.unwrap();
    sim.dispatch().await.unwrap();
    let woken = sim
        .backend()
        .received()
        .into_iter()
        .find(|run| run.prompt.contains("outside_event"))
        .expect("the task ran on the check result");
    for told in [
        "GitHub: a check result on acme/app, pull request #1, by github-actions",
        "test: failure",
        "<<<external data",
    ] {
        assert!(woken.prompt.contains(told), "{told:?} in {}", woken.prompt);
    }
    assert_eq!(sim.task("T-0001").await.status, TaskStatus::Done);
    assert_eq!(
        sim.task("T-0001").await.result.unwrap()["summary"],
        TASK_SUMMARY
    );
    assert_eq!(sim.event(events[0].id).await.status, EventStatus::Done);
    assert!(
        sim.feed()
            .await
            .iter()
            .any(|item| item.message.body["text"] == REPLY),
        "the lead reported the finished task"
    );
    assert_quiet(&sim, "check result").await;
    assert_invariants(&sim, "check result").await;
}

#[tokio::test]
async fn a_review_request_that_belongs_to_no_task_is_recorded_and_not_acted_on() {
    let world = World::new().await.with_github();
    world.add_github_repository("app", REMOTE).await;
    world.backend.set_default(reply_script());
    let github = world.github.clone().unwrap();
    let sim = world.boot(CrashPlan::new()).await;
    github.notify(GithubItem {
        id: "notification:12000000001:2026-11-17T08:58:12Z".into(),
        kind: GithubKind::ReviewRequest,
        repository: "acme/app".into(),
        pull_request: Some(9),
        branch: None,
        author: None,
        title: "Somebody else's change".into(),
        body: String::new(),
        url: None,
        at: "2026-11-17T08:58:12Z".into(),
    });
    assert_eq!(sim.core.poll_github_once().await.unwrap().inserted, 1);
    sim.work_until_idle().await.unwrap();
    sim.dispatch().await.unwrap();

    let events = github_events(&sim).await;
    assert_eq!(events.len(), 1, "recorded");
    let general = sim.general_lane().await;
    assert_eq!(events[0].lane_id, general.id, "the lead's lane");
    assert!(!events[0].requested);
    assert_eq!(events[0].status, EventStatus::Skipped);
    assert_eq!(events[0].status_reason.as_deref(), Some(SKIP_REASON));
    assert!(sim.backend().received().is_empty(), "no run started");
    assert!(sim.feed().await.is_empty(), "nothing said");
    assert_quiet(&sim, "review request").await;
    assert_invariants(&sim, "review request").await;
}
