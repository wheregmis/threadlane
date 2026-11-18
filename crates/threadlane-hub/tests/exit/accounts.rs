//! Milestone 3 stage B exit scenario: no model account is free, so tasks
//! show blocked, none fails or uses an attempt, you get one notice, and work
//! resumes by itself when the limit resets (Plan.md §5.3, §10.2,
//! docs/milestone-3.md §3).

use std::time::Duration;

use serde_json::json;
use threadlane_core::model::{EventStatus, NewProject, OutboxKind, RunStatus, TaskStatus};
use threadlane_core::runtime::Step;
use threadlane_core::runtime::steps::account;
use threadlane_core::store::OrgStore;
use threadlane_core::time::{Clock, Timestamp};
use threadlane_hub::format_time;
use threadlane_store::BLOCKED_REASON;
use threadlane_testkit::{CrashPlan, Script, ScriptStep};

use crate::harness::{
    PAST_LEASE, REPLY, Sim, World, add_engineer, finish_after, join, plan_tasks, reply_script,
    until,
};
use crate::invariants::{assert_invariants, assert_quiet};
use crate::loops::{sleepers_at_least, spawn_hub, spawn_runner};

/// What the lead tells you about the tasks' results.
const REPORT: &str = "Both tasks are done.";

/// A world whose General lead plans two tasks for an engineer that runs one
/// at a time, whose task agent finishes at once, and whose lead in Alpha
/// answers "Still there?". No debounce, so only a limit resetting moves
/// time in the scenario.
async fn world() -> World {
    let mut world = World::new().await;
    world.runner_config.debounce = Duration::ZERO;
    add_engineer(&world.db, 1).await;
    world
        .db
        .create_project(&NewProject {
            name: "Alpha".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let backend = &world.backend;
    backend.route("Still there?", reply_script());
    backend.route(
        "task_result",
        Script::new()
            .call(
                "toolu_r",
                threadlane_hub::SEND_MESSAGE,
                json!({ "text": REPORT }),
            )
            .finish("Reported."),
    );
    backend.route("Your task", finish_after(Duration::ZERO));
    backend.set_default(plan_tasks(&[("data", &[]), ("page", &[])]));
    world
}

/// Plans the tasks, then runs the first into the account's usage limit,
/// resetting at `resets_at`.
async fn hit_the_limit(
    sim: &Sim,
    resets_at: Timestamp,
) -> Result<(), threadlane_runner::RunnerError> {
    sim.send("m-1", "Build the data and the page").await;
    assert!(sim.work_once().await?, "the lead plans");
    assert_eq!(sim.tasks().await.len(), 2);
    sim.backend()
        .push(Script::new().then(ScriptStep::UsageLimit(Some(resets_at))));
    assert!(sim.work_once().await?, "the first task runs into the limit");
    Ok(())
}

async fn notices(sim: &Sim) -> Vec<String> {
    sim.outbox()
        .await
        .into_iter()
        .filter(|message| message.kind == OutboxKind::SystemNotice)
        .map(|message| message.body["text"].as_str().unwrap().to_owned())
        .collect()
}

/// Nothing failed or used an attempt; every task waits, blocked.
async fn assert_blocked(sim: &Sim, scenario: &str) {
    for task in sim.tasks().await {
        assert_eq!(task.status, TaskStatus::Queued, "{scenario}: {}", task.id);
        assert_eq!(
            task.waiting_reason.as_deref(),
            Some(BLOCKED_REASON),
            "{scenario}: {}",
            task.id
        );
    }
    for event in sim.events().await {
        assert_eq!(
            event.attempts, 0,
            "{scenario}: event {} used an attempt",
            event.id
        );
        assert_ne!(event.status, EventStatus::Dead, "{scenario}");
    }
}

#[tokio::test]
async fn with_no_account_free_tasks_show_blocked_you_get_one_notice_and_work_resumes_at_the_reset()
{
    let sim = world().await.boot(CrashPlan::new()).await;
    // Sooner than any poll, so only the reset itself can wake the work.
    let resets_at = sim.clock().now() + Duration::from_secs(10);
    hit_the_limit(&sim, resets_at).await.unwrap();

    let limited: Vec<_> = sim
        .runs()
        .await
        .into_iter()
        .filter(|run| run.status == RunStatus::Failed)
        .collect();
    assert_eq!(limited.len(), 1);
    assert_eq!(
        limited[0].error.as_deref(),
        Some("the account reached its usage limit")
    );
    let accounts = threadlane_core::store::MachineStore::accounts(sim.db())
        .await
        .unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].label, "claude-scripted");
    assert_eq!(accounts[0].cooldown_until, Some(resets_at));
    assert!(!sim.work_once().await.unwrap(), "nothing may run");
    assert_blocked(&sim, "blocked").await;

    assert_eq!(sim.dispatch().await.unwrap(), 1);
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1, "one notice");
    assert_eq!(feed[0].message.kind, OutboxKind::SystemNotice);
    let text = feed[0].message.body["text"].as_str().unwrap();
    assert!(text.contains("no agent can run"), "{text}");
    assert!(
        text.contains(&format!("at about {}", format_time(resets_at))),
        "says when it expects to resume: {text}"
    );

    // A message sent meanwhile is accepted and held.
    let held = sim.send_to(Some("Alpha"), "a-1", "Still there?").await;
    assert!(!sim.work_once().await.unwrap());
    sim.core.sweep_once().await.unwrap();
    assert_eq!(sim.event(held).await.status, EventStatus::Pending);
    assert_eq!(notices(&sim).await.len(), 1, "still one notice");

    // The long-running loops wait; the reset alone wakes them.
    let (stop_hub, hub) = spawn_hub(&sim);
    let (stop_runner, runner) = spawn_runner(&sim);
    sleepers_at_least(&sim, "every loop waits on the clock", 5).await;
    sim.clock().set(resets_at);
    let probe = sim.clone();
    until("work resumes and everything is answered", || {
        let sim = probe.clone();
        async move {
            sim.event(held).await.status == EventStatus::Done
                && sim
                    .tasks()
                    .await
                    .iter()
                    .all(|task| task.status == TaskStatus::Done)
                && sim.feed().await.len() >= 4
                && sim
                    .lanes()
                    .await
                    .iter()
                    .all(|lane| lane.lease_owner.is_none())
        }
    })
    .await;
    assert_eq!(sim.clock().now(), resets_at, "resumed at the reset");
    stop_runner.send(()).unwrap();
    stop_hub.send(()).unwrap();
    join("the runner stops", runner).await.unwrap();
    join("the hub stops", hub).await.unwrap();

    let notices = notices(&sim).await;
    assert_eq!(
        notices.len(),
        2,
        "one when blocked, one when resumed: {notices:?}"
    );
    assert!(notices[1].contains("work has resumed"), "{notices:?}");
    let messages: Vec<_> = sim
        .feed()
        .await
        .into_iter()
        .filter(|item| item.message.kind == OutboxKind::Message)
        .map(|item| item.message.body["text"].as_str().unwrap().to_owned())
        .collect();
    assert!(messages.contains(&REPLY.to_owned()), "{messages:?}");
    assert!(messages.contains(&REPORT.to_owned()), "{messages:?}");
    for event in sim.events().await {
        assert_eq!(event.attempts, 0, "event {} used an attempt", event.id);
    }
    assert_quiet(&sim, "no account").await;
    assert_invariants(&sim, "no account").await;
}

/// Kills the process at `step` of the usage limit, restarts, and checks that
/// you still get exactly one notice for the block and one for the resume.
async fn kill_at_and_recover(step: Step) {
    let scenario = format!("usage limit, killed at {step}");
    let world = world().await;
    let sim = world.boot(CrashPlan::kill_at(step)).await;
    let resets_at = sim.clock().now() + Duration::from_secs(3600);
    let died = hit_the_limit(&sim, resets_at).await;
    assert!(
        matches!(&died, Err(threadlane_runner::RunnerError::Killed(killed)) if killed.step == step),
        "{scenario}: {died:?}"
    );
    sim.clock().advance(PAST_LEASE);

    let sim = sim.restart(CrashPlan::new()).await;
    sim.core.sweep_once().await.unwrap();
    assert!(
        !sim.work_once().await.unwrap(),
        "{scenario}: nothing may run"
    );
    sim.core.sweep_once().await.unwrap();
    sim.dispatch().await.unwrap();
    assert_eq!(notices(&sim).await.len(), 1, "{scenario}: one notice");
    assert_eq!(sim.feed().await.len(), 1, "{scenario}");
    assert_blocked(&sim, &scenario).await;

    sim.clock().set(resets_at);
    sim.core.sweep_once().await.unwrap();
    sim.work_until_idle().await.unwrap();
    sim.core.sweep_once().await.unwrap();
    sim.dispatch().await.unwrap();
    let notices = notices(&sim).await;
    assert_eq!(notices.len(), 2, "{scenario}: {notices:?}");
    assert!(
        sim.tasks()
            .await
            .iter()
            .all(|task| task.status == TaskStatus::Done),
        "{scenario}"
    );
    assert_quiet(&sim, &scenario).await;
    assert_invariants(&sim, &scenario).await;
}

#[tokio::test]
async fn killing_the_process_around_a_usage_limit_still_sends_each_notice_once() {
    for &step in account::ALL {
        kill_at_and_recover(step).await;
    }
}
