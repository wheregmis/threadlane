//! Exit scenarios of milestone 9 stage A (docs/milestone-9.md §2): the same
//! scenario under each mode does what its row permits, as far as authority
//! goes; a standing rule made by answering "always"; a repository of
//! another project refused before any ask (invariant 19); and `ask_user`.

use serde_json::json;
use threadlane_core::client::{DecisionReply, DecisionRequest};
use threadlane_core::model::{
    DecisionKind, DecisionStatus, EventStatus, Mode, NewProject, NewRepository, OutboxKind,
    TaskStatus,
};
use threadlane_core::store::OrgStore;
use threadlane_hub::{ASK_USER, FINISH_TASK, OFF_REASON, OPEN_PULL_REQUEST};
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{ENGINEER, Sim, World, plan_tasks_in, reply_script};
use crate::invariants::{assert_invariants, assert_quiet};
use crate::publish::{ask_script, publish_world};

/// One process lifetime: release what expired, work every lane, deliver.
async fn live(sim: &Sim) {
    sim.core.sweep_once().await.unwrap();
    sim.work_until_idle().await.unwrap();
    sim.dispatch().await.unwrap();
}

/// Lets General go up to Autonomous and gives the engineer `mode` there.
pub(super) async fn engineer_in(world: &World, mode: Mode) {
    let db = &world.db;
    let mut space = db.space().await.unwrap();
    space.mode_ceiling = Mode::Autonomous;
    db.update_space(&space).await.unwrap();
    let general = db.general_project().await.unwrap();
    let engineer = db
        .agents()
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.name == ENGINEER)
        .unwrap();
    let binding = db
        .bindings(general.id)
        .await
        .unwrap()
        .into_iter()
        .find(|binding| binding.agent_id == engineer.id)
        .unwrap();
    db.set_binding_mode(binding.id, mode).await.unwrap();
}

/// Turns off the default that opens draft pull requests and pushes the task
/// branch without asking, with an override for tier 2, for tests about asking.
pub(super) async fn asks_before_sharing(world: &World) {
    let db = &world.db;
    let general = db.general_project().await.unwrap();
    let engineer = db
        .agents()
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.name == ENGINEER)
        .unwrap();
    let binding = db
        .bindings(general.id)
        .await
        .unwrap()
        .into_iter()
        .find(|binding| binding.agent_id == engineer.id)
        .unwrap();
    db.set_binding_overrides(binding.id, &serde_json::json!({"shared_reversible": "ask"}))
        .await
        .unwrap();
}

/// How many task runs the backend was given.
fn task_runs(sim: &Sim) -> usize {
    sim.backend()
        .received()
        .iter()
        .filter(|run| run.tools.iter().any(|tool| tool == FINISH_TASK))
        .count()
}

#[tokio::test]
async fn the_same_scenario_under_each_mode_does_exactly_what_its_row_permits() {
    for mode in [Mode::Off, Mode::OnRequest, Mode::Assisted, Mode::Autonomous] {
        // Two tasks in `app`, the second after the first; each opens a pull
        // request, a tier 2 call.
        let world = publish_world(World::new().await, ask_script()).await;
        // The scenario is about asking: drafts are opened without asking
        // unless an override for tier 2 says otherwise.
        asks_before_sharing(&world).await;
        world.backend.set_default(plan_tasks_in(
            &["app"],
            &[("page", &[]), ("docs", &["page"])],
        ));
        engineer_in(&world, mode).await;
        let sim = world.boot(CrashPlan::new()).await;
        sim.send(
            "m-1",
            "Put up the page and the docs, each with a pull request",
        )
        .await;
        live(&sim).await;

        if mode == Mode::Off {
            // Off: the engineer never runs, so nothing is asked or done.
            assert_eq!(task_runs(&sim), 0, "{mode}");
            assert!(sim.decisions().await.is_empty(), "{mode}");
            assert!(sim.world.forge.calls().is_empty(), "{mode}");
            let first = sim.task("T-0001").await;
            let skipped = sim
                .events()
                .await
                .into_iter()
                .find(|event| event.lane_id == first.lane_id)
                .unwrap();
            assert_eq!(skipped.status, EventStatus::Skipped, "{mode}");
            assert_eq!(skipped.status_reason.as_deref(), Some(OFF_REASON));
            continue;
        }

        // Above Off a tier 2 call asks: On request by the matrix, Assisted
        // and Autonomous by review, with no scripted answer, so review falls back to a card.
        let decisions = sim.decisions().await;
        assert_eq!(decisions.len(), 1, "{mode}");
        let asked = &decisions[0];
        assert_eq!(asked.status, DecisionStatus::Pending, "{mode}");
        assert_eq!(asked.tool, OPEN_PULL_REQUEST, "{mode}");
        assert!(asked.offers_standing_rule, "{mode}");
        assert!(asked.expires_at.is_some(), "{mode}");
        assert!(sim.world.forge.calls().is_empty(), "{mode}");

        // "Always": the second task's identical call is allowed by the rule,
        // in every mode, and performed without asking you.
        let answered = sim
            .api
            .decisions(DecisionRequest::AnswerAlways {
                decision: asked.id,
                option: "approve".into(),
                note: None,
            })
            .await
            .unwrap();
        assert!(
            matches!(answered, DecisionReply::Answered { recorded: true, .. }),
            "{mode}"
        );
        for _ in 0..4 {
            live(&sim).await;
        }
        let decisions = sim.decisions().await;
        assert_eq!(decisions.len(), 2, "{mode}");
        assert!(
            decisions.iter().all(|decision| decision.approved()),
            "{mode}: {decisions:?}"
        );
        assert_eq!(sim.world.forge.pull_requests().len(), 2, "{mode}");
        let cards = sim
            .feed()
            .await
            .iter()
            .filter(|item| item.message.kind == OutboxKind::Decision)
            .count();
        assert_eq!(cards, 1, "{mode}: only the first was asked");
        let DecisionReply::StandingRules { rules } = sim
            .api
            .decisions(DecisionRequest::StandingRules { project: None })
            .await
            .unwrap()
        else {
            panic!("not rules");
        };
        assert_eq!(rules.len(), 1, "{mode}");
        assert_eq!(rules[0].agent, ENGINEER);
        assert_eq!(rules[0].rule.pattern, json!({ "repository": "app" }));
        assert_eq!(rules[0].rule.uses, 1, "{mode}");
        for task in ["T-0001", "T-0002"] {
            assert_eq!(
                sim.task(task).await.status,
                TaskStatus::Done,
                "{mode} {task}"
            );
        }
        assert_invariants(&sim, &format!("{mode}")).await;
        assert_quiet(&sim, &format!("{mode}")).await;
    }
}

#[tokio::test]
async fn a_repository_of_another_project_is_refused_before_any_ask() {
    let elsewhere = Script::new()
        .call(
            "toolu_pr",
            OPEN_PULL_REQUEST,
            json!({ "repository": "elsewhere", "title": "Sneak in" }),
        )
        .finish("Tried.");
    let world = publish_world(World::new().await, elsewhere).await;
    let other = world
        .db
        .create_project(&NewProject {
            name: "Other".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    world
        .db
        .create_repository(&NewRepository {
            project_id: other.id,
            name: "elsewhere".into(),
            remote_url: "https://forge.test/other/elsewhere.git".into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        })
        .await
        .unwrap();
    let sim = world.boot(CrashPlan::new()).await;
    sim.send("m-1", "Put the page up").await;
    live(&sim).await;

    assert!(sim.decisions().await.is_empty(), "nothing was asked");
    assert!(sim.world.forge.calls().is_empty(), "nothing was done");
    let lane = sim.task("T-0001").await.lane_id;
    let run = sim
        .runs()
        .await
        .into_iter()
        .find(|run| run.lane_id == lane)
        .unwrap();
    let journal = sim.journal(run.id).await;
    assert!(
        journal
            .iter()
            .any(|entry| entry.entry.to_string().contains("elsewhere")
                && entry.entry.to_string().contains("no repository")),
        "the agent is told: {journal:?}"
    );
}

#[tokio::test]
async fn ask_user_returns_your_answer_to_the_session_and_performs_nothing() {
    let world = World::new().await;
    world.backend.route("decision_answer", reply_script());
    world.backend.set_default(
        Script::new()
            .call(
                "toolu_q",
                ASK_USER,
                json!({ "question": "Which colour?", "options": ["blue", "green"] }),
            )
            .finish("Asked."),
    );
    let sim = world.boot(CrashPlan::new()).await;
    sim.send("m-1", "Paint the page").await;
    live(&sim).await;

    let decisions = sim.decisions().await;
    assert_eq!(decisions.len(), 1);
    let asked = &decisions[0];
    assert_eq!(asked.kind, DecisionKind::Input);
    assert_eq!(asked.question, "Which colour?");
    let labels: Vec<&str> = asked.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, ["blue", "green"]);
    assert!(!asked.offers_standing_rule);

    let answered = sim
        .api
        .decisions(DecisionRequest::Answer {
            decision: asked.id,
            option: "GREEN".into(),
            note: Some("Like the logo.".into()),
        })
        .await
        .unwrap();
    assert!(matches!(
        answered,
        DecisionReply::Answered { recorded: true, .. }
    ));
    live(&sim).await;

    let received = sim.backend().received();
    assert_eq!(received.len(), 2, "one run to ask, one with the answer");
    let resumed = &received[1].prompt;
    assert!(
        resumed.contains("You answered D-0001 (\"Which colour?\"): \"green\".")
            && resumed.contains("Like the logo."),
        "{resumed}"
    );
    assert!(
        !resumed.contains("Outcome of"),
        "nothing performed: {resumed}"
    );
    let decision = &sim.decisions().await[0];
    assert_eq!(decision.status, DecisionStatus::Answered);
    assert_eq!(decision.outcome, None);
    assert_invariants(&sim, "ask_user").await;
    assert_quiet(&sim, "ask_user").await;
}
