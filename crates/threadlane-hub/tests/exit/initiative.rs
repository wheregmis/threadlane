//! Exit scenario of milestone 9 stage B (docs/milestone-9.md §3, Plan.md
//! §15 milestone 9): "in Assisted mode a failing build on a registered
//! repository produces a proposal, and in Autonomous mode it produces a
//! task", and in On request it is recorded and not acted on.
//!
//! The real hub and runner, the scripted backend, and the fake GitHub.

use serde_json::json;
use threadlane_core::client::DecisionRequest;
use threadlane_core::github::{GITHUB_SOURCE, GithubItem, GithubKind};
use threadlane_core::model::{
    DecisionKind, DecisionStatus, EventStatus, Mode, TaskOrigin, TaskStatus,
};
use threadlane_core::store::OrgStore;
use threadlane_hub::{CREATE_TASK, SKIP_REASON};
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{ENGINEER, REPLY, Sim, World, add_engineer, finish_after, reply_script};
use crate::invariants::{assert_invariants, assert_quiet};

const REMOTE: &str = "git@github.com:acme/app.git";

/// A failed check on `main` of the registered repository: no task's work.
fn failing_build() -> GithubItem {
    GithubItem {
        id: "notification:13000000001:2026-11-17T09:00:00Z".into(),
        kind: GithubKind::CheckResult,
        repository: "acme/app".into(),
        pull_request: None,
        branch: Some("main".into()),
        author: Some("github-actions".into()),
        title: "build: failure".into(),
        body: "cargo build failed on main".into(),
        url: None,
        at: "2026-11-17T09:00:00Z".into(),
    }
}

/// The lead's answer to the failing build: a task for the engineer.
fn fix_it() -> Script {
    Script::new()
        .text("The build of main is failing; that needs a fix.")
        .call(
            "toolu_fix",
            CREATE_TASK,
            json!({
                "title": "Fix the build",
                "goal": "main builds again",
                "agent": ENGINEER,
                "repositories": ["app"],
            }),
        )
        .finish("Asked for a fix of the build.")
}

/// Lets General go up to Autonomous and puts the lead and the engineer at
/// `mode`.
async fn both_at(world: &World, mode: Mode) {
    let db = &world.db;
    let mut space = db.space().await.unwrap();
    space.mode_ceiling = Mode::Autonomous;
    db.update_space(&space).await.unwrap();
    let general = db.general_project().await.unwrap();
    for binding in db.bindings(general.id).await.unwrap() {
        db.set_binding_mode(binding.id, mode).await.unwrap();
    }
}

async fn live(sim: &Sim) {
    sim.work_until_idle().await.unwrap();
    sim.dispatch().await.unwrap();
}

#[tokio::test]
async fn a_failing_build_is_recorded_proposed_or_tasked_by_mode() {
    for mode in [Mode::OnRequest, Mode::Assisted, Mode::Autonomous] {
        let world = World::new().await.with_github();
        add_engineer(&world.db, 1).await;
        world.add_github_repository("app", REMOTE).await;
        world
            .backend
            .route("task_start", finish_after(std::time::Duration::ZERO));
        world.backend.route("decision_answer", reply_script());
        world.backend.route("task_result", reply_script());
        world.backend.route("outside_event", fix_it());
        let github = world.github.clone().unwrap();
        let sim = world.boot(CrashPlan::new()).await;
        // After boot, which makes the lead's binding.
        both_at(&world, mode).await;
        world.backend.push_answer(Ok(
            json!({"verdict":"clean","reason":"An ordinary build result."}),
        ));
        github.notify(failing_build());
        assert_eq!(sim.core.poll_github_once().await.unwrap().inserted, 1);
        live(&sim).await;

        let events: Vec<_> = sim
            .events()
            .await
            .into_iter()
            .filter(|event| event.source == GITHUB_SOURCE)
            .collect();
        assert_eq!(events.len(), 1, "{mode}");
        assert_eq!(events[0].lane_id, sim.general_lane().await.id, "{mode}");
        assert!(!events[0].requested, "{mode}: nobody asked for it");

        match mode {
            Mode::OnRequest => {
                assert_eq!(events[0].status, EventStatus::Skipped, "{mode}");
                assert_eq!(events[0].status_reason.as_deref(), Some(SKIP_REASON));
                assert!(sim.backend().received().is_empty(), "{mode}: no run");
                assert!(sim.decisions().await.is_empty(), "{mode}");
                assert!(sim.tasks().await.is_empty(), "{mode}");
            }
            Mode::Assisted => {
                // A proposal, and no task until you start it.
                assert!(sim.tasks().await.is_empty(), "{mode}");
                let decisions = sim.decisions().await;
                assert_eq!(decisions.len(), 1, "{mode}");
                let proposal = &decisions[0];
                assert_eq!(proposal.kind, DecisionKind::Proposal);
                assert_eq!(proposal.tool, CREATE_TASK);
                assert_eq!(proposal.status, DecisionStatus::Pending);
                assert_eq!(
                    proposal.question,
                    "Start a task for Engineer: Fix the build?"
                );
                sim.api
                    .decisions(DecisionRequest::Answer {
                        decision: proposal.id,
                        option: "start".into(),
                        note: None,
                    })
                    .await
                    .unwrap();
                for _ in 0..3 {
                    live(&sim).await;
                }
                let tasks = sim.tasks().await;
                assert_eq!(tasks.len(), 1, "{mode}: created once");
                assert_eq!(tasks[0].origin, TaskOrigin::User, "{mode}: you started it");
                assert_eq!(tasks[0].status, TaskStatus::Done, "{mode}");
                assert_eq!(
                    sim.decisions().await[0].outcome.as_ref().unwrap()["performed"],
                    json!(true)
                );
            }
            _ => {
                // A task, without asking.
                assert!(sim.decisions().await.is_empty(), "{mode}: nothing asked");
                live(&sim).await;
                let tasks = sim.tasks().await;
                assert_eq!(tasks.len(), 1, "{mode}");
                assert_eq!(tasks[0].title, "Fix the build");
                assert_eq!(tasks[0].origin, TaskOrigin::Agent, "{mode}: the lead's own");
                assert_eq!(tasks[0].status, TaskStatus::Done, "{mode}");
            }
        }
        if mode != Mode::OnRequest {
            assert_eq!(
                sim.event(events[0].id).await.status,
                EventStatus::Done,
                "{mode}"
            );
            assert!(
                sim.feed()
                    .await
                    .iter()
                    .any(|item| item.message.body["text"] == REPLY),
                "{mode}: the lead reported the finished task"
            );
        }
        assert_quiet(&sim, &format!("{mode}")).await;
        assert_invariants(&sim, &format!("{mode}")).await;
    }
}
