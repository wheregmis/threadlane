//! The exit scenario of milestone 3 (Plan.md §15), end to end in one test:
//! the real hub and the real runner, `GitWorkspaces` on a local bare
//! repository, the scripted backend, and the fake forge. Each step is one
//! sentence of the Exit paragraph; the per-stage scenarios (`tasks`,
//! `accounts`, `publish`) cover each part in more depth, with crashes.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::json;
use threadlane_core::backend::DENY_RULES;
use threadlane_core::client::{DecisionReply, DecisionRequest};
use threadlane_core::model::{EventStatus, OutboxKind, RunOutcome, RunStatus, TaskStatus};
use threadlane_core::time::Clock;
use threadlane_hub::{FINISH_TASK, MEMORY_CREATE, OPEN_PULL_REQUEST, SEND_MESSAGE};
use threadlane_store::BLOCKED_REASON;
use threadlane_testkit::{CrashPlan, Script, ScriptStep};

use crate::harness::{
    Sim, World, add_engineer, finish_after, join, plan_tasks_in, until, write_progress,
};
use crate::invariants::{assert_invariants, assert_quiet};
use crate::loops::{sleepers_at_least, spawn_hub, spawn_runner};

/// How long the first two tasks work, on the fake clock.
const WORK: Duration = Duration::from_secs(10);
/// What the page task reports once its pull request is open.
const PUBLISHED: &str = "The page is published.";
/// The lead's one message: every task's result at once.
const COMBINED: &str = "The report page is live: data, docs, and the page's draft pull request.";

/// One goal, "Ship the report page", becomes three tasks in `app`: `page`,
/// which works then asks to open its pull request; `data`, which works;
/// and `docs`, which depends on `data`. The lead stays silent until the
/// page's result, the last to come, and then reports everything at once.
async fn world() -> World {
    let mut world = World::new().await;
    world.runner_config.debounce = Duration::ZERO;
    add_engineer(&world.db, 2).await;
    world.add_repository("app").await;
    let backend = &world.backend;
    backend.route(
        PUBLISHED,
        Script::new()
            .call("toolu_s", SEND_MESSAGE, json!({ "text": COMBINED }))
            .finish("Told the user everything at once."),
    );
    backend.route(
        "task_result",
        Script::new().finish("No response requested."),
    );
    backend.route(
        "decision_answer",
        Script::new()
            .call_for("toolu_p", MEMORY_CREATE, write_progress)
            .call("toolu_f", FINISH_TASK, json!({ "summary": PUBLISHED }))
            .finish("Finished the task."),
    );
    backend.route(
        "Your task, T-0001:",
        Script::new()
            .wait(WORK)
            .text("The page is done; asking to open the pull request.")
            .call(
                "toolu_pr",
                OPEN_PULL_REQUEST,
                json!({ "repository": "app", "title": "Add the page", "body": "Adds the page." }),
            )
            .finish("Waiting on the user."),
    );
    backend.route("Your task, T-0002:", finish_after(WORK));
    backend.route("Your task", finish_after(Duration::ZERO));
    backend.set_default(plan_tasks_in(
        &["app"],
        &[("page", &[]), ("data", &[]), ("docs", &["data"])],
    ));
    world
}

fn notices(feed: &[threadlane_core::client::FeedItem]) -> Vec<String> {
    feed.iter()
        .filter(|item| item.message.kind == OutboxKind::SystemNotice)
        .map(|item| item.message.body["text"].as_str().unwrap().to_owned())
        .collect()
}

/// Waits, yielding, until `check` holds of the simulation.
async fn until_sim<F, Fut>(sim: &Sim, what: &str, check: F)
where
    F: Fn(Sim) -> Fut,
    Fut: Future<Output = bool>,
{
    until(what, || check(sim.clone())).await;
}

#[tokio::test]
async fn milestone_3_exit() {
    let sim = world().await.boot(CrashPlan::new()).await;
    let backend = sim.backend().clone();

    // "One goal becomes several tasks"
    sim.send("m-1", "Ship the report page").await;
    assert!(sim.work_once().await.unwrap(), "the lead plans");
    assert_eq!(sim.tasks().await.len(), 3);

    // "When no account is available, tasks show as blocked, none fails or
    // uses an attempt, you receive one system notice"
    let resets_at = sim.clock().now() + Duration::from_secs(10);
    backend.push(Script::new().then(ScriptStep::UsageLimit(Some(resets_at))));
    assert!(sim.work_once().await.unwrap(), "a task runs into the limit");
    assert!(!sim.work_once().await.unwrap(), "nothing may run");
    for id in ["T-0001", "T-0002"] {
        let task = sim.task(id).await;
        assert_eq!(task.status, TaskStatus::Queued, "{id}");
        assert_eq!(task.waiting_reason.as_deref(), Some(BLOCKED_REASON), "{id}");
    }
    assert_eq!(sim.dispatch().await.unwrap(), 1);
    let feed = sim.feed().await;
    assert_eq!(notices(&feed).len(), 1, "one notice: {feed:?}");
    assert_eq!(feed.len(), 1);

    // "and work resumes by itself when a limit resets": the long-running
    // loops wait, and the reset alone wakes them.
    let (stop_hub, hub) = spawn_hub(&sim);
    let (stop_runner, runner) = spawn_runner(&sim);
    sleepers_at_least(&sim, "every loop waits on the clock", 5).await;
    sim.clock().set(resets_at);

    // "that run in parallel on one machine in separate worktrees of the
    // same repository", and "A dependent task waits."
    until_sim(&sim, "page and data run at once", |sim| async move {
        let tasks = sim.tasks().await;
        tasks.len() == 3
            && tasks[..2]
                .iter()
                .all(|task| task.status == TaskStatus::Running)
            && sim.backend().received().len() == 4
            && sim.clock().sleepers() >= 9
    })
    .await;
    let active: BTreeSet<_> = sim
        .runs()
        .await
        .iter()
        .filter(|run| run.status == RunStatus::Active)
        .map(|run| run.lane_id)
        .collect();
    assert_eq!(active.len(), 2, "two task runs at the same time");
    let worktree = |task: &str| {
        sim.world
            .workspaces()
            .join("worktrees")
            .join(task)
            .join("app")
    };
    let received = backend.received();
    let dirs: BTreeSet<_> = received[2..]
        .iter()
        .map(|run| run.working_dir.clone())
        .collect();
    assert_eq!(
        dirs,
        BTreeSet::from([worktree("T-0001"), worktree("T-0002")])
    );
    let dependent = sim.task("T-0003").await;
    assert_eq!(dependent.status, TaskStatus::Queued);
    assert_eq!(
        dependent.waiting_reason.as_deref(),
        Some("waiting for T-0002 to be done")
    );

    // "A finished task asks before publishing": data, then docs, finish;
    // the page asks and waits on you, and nothing is published.
    sim.clock().advance(WORK);
    until_sim(
        &sim,
        "data and docs are done and the page waits on you",
        |sim| async move {
            let tasks = sim.tasks().await;
            tasks[0].status == TaskStatus::WaitingOnYou
                && tasks[1..]
                    .iter()
                    .all(|task| task.status == TaskStatus::Done)
                && sim.feed().await.len() == 3
                && sim
                    .lanes()
                    .await
                    .iter()
                    .all(|lane| lane.lease_owner.is_none())
        },
    )
    .await;
    assert!(
        sim.world.forge.calls().is_empty(),
        "nothing before the answer"
    );
    let feed = sim.feed().await;
    let kinds: Vec<_> = feed.iter().map(|item| item.message.kind).collect();
    assert_eq!(
        kinds,
        [
            OutboxKind::SystemNotice,
            OutboxKind::SystemNotice,
            OutboxKind::Decision
        ],
        "blocked, resumed, and the page's question; the lead said nothing yet"
    );
    assert!(notices(&feed)[1].contains("work has resumed"), "{feed:?}");

    // "after your answer its branch is pushed and a draft pull request
    // opened, exactly once"
    let DecisionReply::Pending { decisions } =
        sim.api.decisions(DecisionRequest::Pending).await.unwrap()
    else {
        panic!("not a list");
    };
    assert_eq!(decisions.len(), 1);
    let answered = sim
        .api
        .decisions(DecisionRequest::Answer {
            decision: decisions[0].id,
            option: "approve".to_owned(),
            note: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        answered,
        DecisionReply::Answered { recorded: true, .. }
    ));
    // The runner also finds the answer on its next poll.
    sim.clock().advance(sim.world.runner_config.poll);

    // "and you receive one combined result"
    until_sim(
        &sim,
        "the page is done and the lead has reported",
        |sim| async move {
            sim.task("T-0001").await.status == TaskStatus::Done
                && sim.feed().await.len() == 4
                && sim
                    .lanes()
                    .await
                    .iter()
                    .all(|lane| lane.lease_owner.is_none())
        },
    )
    .await;
    stop_runner.send(()).unwrap();
    stop_hub.send(()).unwrap();
    join("the runner stops", runner).await.unwrap();
    join("the hub stops", hub).await.unwrap();

    let forge = &sim.world.forge;
    let opened = forge.pull_requests();
    assert_eq!(opened.len(), 1, "exactly one draft pull request");
    assert_eq!(
        forge.pushes(),
        ["threadlane/T-0001"],
        "pushed once, as part of opening"
    );
    let decision = &sim.decisions().await[0];
    assert!(decision.approved());
    assert_eq!(
        decision
            .outcome
            .as_ref()
            .map(|outcome| &outcome["pull_request"]),
        Some(&json!(opened[0]))
    );
    assert_eq!(
        crate::harness::merged_cleanly(sim.task("T-0001").await.result, "app"),
        Some(json!({
            "summary": PUBLISHED,
            "branch": "threadlane/T-0001",
            "pull_request": opened[0],
        }))
    );
    let messages: Vec<_> = sim
        .feed()
        .await
        .into_iter()
        .filter(|item| item.message.kind == OutboxKind::Message)
        .map(|item| item.message.body)
        .collect();
    assert_eq!(
        messages,
        [json!({ "text": COMBINED })],
        "one combined result"
    );

    // "A direct push by the agent is refused by the deny rules": every task
    // run was started with them. That each backend refuses each one is the
    // conformance suite, `threadlane_testkit::conformance`, run against the
    // fake and the real `claude` and `codex` by each driver's tests.
    let deny: Vec<String> = DENY_RULES.iter().map(|rule| (*rule).to_owned()).collect();
    for run in backend.received() {
        if run.tools.iter().any(|tool| tool == FINISH_TASK) {
            assert_eq!(run.deny_rules, deny, "{}", run.prompt);
            assert!(!run.remove_builtin_tools);
        } else {
            assert!(run.deny_rules.is_empty() && run.remove_builtin_tools);
        }
    }

    // None failed or used an attempt.
    for task in sim.tasks().await {
        assert_eq!(task.status, TaskStatus::Done, "{}", task.id);
    }
    for event in sim.events().await {
        assert_eq!(event.attempts, 0, "event {} used an attempt", event.id);
        assert_ne!(event.status, EventStatus::Dead);
    }
    let runs = sim.runs().await;
    let failed: Vec<_> = runs
        .iter()
        .filter(|run| run.status == RunStatus::Failed)
        .collect();
    assert_eq!(failed.len(), 1, "only the run that met the limit");
    assert_eq!(
        failed[0].error.as_deref(),
        Some("the account reached its usage limit")
    );
    assert!(
        runs.iter()
            .any(|run| run.outcome == Some(RunOutcome::Deferred)),
        "the page's run ended waiting on you"
    );

    // Invariants 7 and 8 have their own scenarios in `publish`; 15 and the
    // rest are asserted here.
    assert_quiet(&sim, "milestone 3 exit").await;
    assert_invariants(&sim, "milestone 3 exit").await;
}
