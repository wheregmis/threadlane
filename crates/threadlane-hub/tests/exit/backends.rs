//! Backend choice (Plan.md §5.3 "use whatever is available", §5.7,
//! docs/milestone-3.md §5): a task runs on the first of its agent's backends
//! that is installed, logged in, and on a free account; when that account
//! reaches its usage limit the task goes on, on the other backend, in a
//! fresh session; with neither free it is blocked, not failed.

use std::time::Duration;

use threadlane_core::backend::{NativeSession, Probe};
use threadlane_core::model::{
    BackendKind, EventStatus, Mode, NewAgent, NewBinding, Run, RunStatus, SessionStatus, TaskStatus,
};
use threadlane_core::store::{OrgStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_store::BLOCKED_REASON;
use threadlane_testkit::{CrashPlan, Script, ScriptStep, ScriptedBackend};

use crate::harness::{ENGINEER, Sim, World, finish_after, plan_tasks, reply_script};
use crate::invariants::{assert_invariants, assert_quiet};

/// A world with a scripted Claude and a scripted Codex, each on its own
/// account, and an engineer that prefers Codex. Each backend's task agent
/// finishes at once; the lead, on Claude while it can, plans one task and
/// replies to its result.
async fn world() -> World {
    let mut world = World::new().await.with_codex();
    world.runner_config.debounce = Duration::ZERO;
    let agent = world
        .db
        .create_agent(&NewAgent {
            backends: vec![BackendKind::Codex, BackendKind::Claude],
            models: serde_json::json!({"codex": "preferred-model", "claude": "fallback-model"}),
            ..NewAgent::named(ENGINEER, "engineer")
        })
        .await
        .unwrap();
    world
        .db
        .bind(&NewBinding {
            project_id: world.db.general_project().await.unwrap().id,
            agent_id: agent.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    for backend in [&world.backend, codex(&world)] {
        backend.route("task_result", reply_script());
        backend.route("Your task", finish_after(Duration::ZERO));
        backend.set_default(plan_tasks(&[("page", &[])]));
    }
    world
}

fn codex(world: &World) -> &ScriptedBackend {
    world.codex.as_ref().unwrap()
}

/// The backend of each run of the task's lane, oldest first.
async fn task_backends(sim: &Sim) -> Vec<Option<BackendKind>> {
    let lane = sim.task("T-0001").await.lane_id;
    sim.runs()
        .await
        .iter()
        .filter(|run| run.lane_id == lane)
        .map(|run| run.backend)
        .collect()
}

#[tokio::test]
async fn a_task_runs_on_the_first_of_its_agent_s_backends_that_is_installed_and_logged_in() {
    for (codex_probe, expected) in [
        (None, BackendKind::Codex),
        (Some((true, false)), BackendKind::Claude),
        (Some((false, false)), BackendKind::Claude),
    ] {
        let world = world().await;
        if let Some((installed, logged_in)) = codex_probe {
            codex(&world).set_probe(Probe {
                installed,
                version: None,
                logged_in,
                account_label: None,
            });
        }
        let sim = world.boot(CrashPlan::new()).await;
        sim.send("m-1", "Build the page").await;
        sim.work_until_idle().await.unwrap();
        sim.dispatch().await.unwrap();
        let scenario = format!("codex {codex_probe:?}");
        assert_eq!(task_backends(&sim).await, [Some(expected)], "{scenario}");
        assert_eq!(
            sim.task("T-0001").await.status,
            TaskStatus::Done,
            "{scenario}"
        );
        assert_quiet(&sim, &scenario).await;
        assert_invariants(&sim, &scenario).await;
    }
}

#[tokio::test]
async fn at_a_usage_limit_the_task_goes_on_on_the_other_backend_in_a_fresh_session_and_with_none_free_it_is_blocked()
 {
    let world = world().await;
    let sim = world.boot(CrashPlan::new()).await;
    let codex = codex(&sim.world).clone();
    let claude = sim.backend().clone();
    sim.send("m-1", "Build the page").await;
    assert!(sim.work_once().await.unwrap(), "the lead plans, on Claude");

    // Codex comes first and reaches its account's limit.
    let codex_resets = sim.clock().now() + Duration::from_secs(600);
    let claude_resets = codex_resets + Duration::from_secs(600);
    codex.push(Script::new().then(ScriptStep::UsageLimit(Some(codex_resets))));
    assert!(sim.work_once().await.unwrap());
    // The task goes on on Claude at once, in a fresh session, and Claude's
    // account reaches its limit too.
    claude.push(Script::new().then(ScriptStep::UsageLimit(Some(claude_resets))));
    assert!(sim.work_once().await.unwrap());
    assert_eq!(
        task_backends(&sim).await,
        [Some(BackendKind::Codex), Some(BackendKind::Claude)]
    );
    assert_eq!(
        codex.received().last().unwrap().model.as_deref(),
        Some("preferred-model")
    );
    let task_run = claude.received().pop().unwrap();
    assert_eq!(task_run.model.as_deref(), Some("fallback-model"));
    assert!(
        matches!(task_run.session, NativeSession::Start { .. }),
        "a fresh session: {:?}",
        task_run.session
    );
    assert!(
        task_run.prompt.contains("Your task, T-0001:"),
        "{}",
        task_run.prompt
    );
    let runs = sim.runs().await;
    let lane = sim.task("T-0001").await.lane_id;
    let sessions: Vec<_> = runs
        .iter()
        .filter(|run| run.lane_id == lane)
        .map(|run| run.session_id)
        .collect();
    assert_ne!(sessions[0], sessions[1], "the Codex session is not reused");
    let first = sim.db().session(sessions[0]).await.unwrap().unwrap();
    assert_eq!(first.backend, Some(BackendKind::Codex));
    assert_ne!(first.status, SessionStatus::Open);

    // Neither account is free: blocked, nothing failed, no attempt used.
    assert!(!sim.work_once().await.unwrap(), "nothing may run");
    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::Queued);
    assert_eq!(task.waiting_reason.as_deref(), Some(BLOCKED_REASON));
    for event in sim.events().await {
        assert_eq!(event.attempts, 0, "event {} used an attempt", event.id);
        assert_ne!(event.status, EventStatus::Dead);
    }
    assert!(
        runs.iter()
            .filter(|run| run.status == RunStatus::Failed)
            .all(|run| run.error.as_deref() == Some("the account reached its usage limit"))
    );

    // Codex resets first. Both the task and its lead can continue there.
    sim.clock().set(codex_resets);
    sim.core.sweep_once().await.unwrap();
    sim.work_until_idle().await.unwrap();
    sim.dispatch().await.unwrap();
    assert_eq!(
        task_backends(&sim).await,
        [
            Some(BackendKind::Codex),
            Some(BackendKind::Claude),
            Some(BackendKind::Codex)
        ]
    );
    assert_eq!(sim.task("T-0001").await.status, TaskStatus::Done);
    let lead = sim.general_lane().await.id;
    let last = |runs: Vec<Run>| runs.last().map(|run| (run.lane_id, run.backend));
    assert_eq!(
        last(sim.runs().await),
        Some((lead, Some(BackendKind::Codex)))
    );
    assert_quiet(&sim, "usage limit on each backend").await;
    assert_invariants(&sim, "usage limit on each backend").await;
}
