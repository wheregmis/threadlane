//! Exit scenarios of docs/milestone-5.md §2 for protected instructions: a
//! task run, which you did not trigger, asks to change the project's
//! instructions; after your approval the hub writes them exactly once, with
//! the process killed at every step boundary; a decline writes nothing and
//! the session resumes with your note; a stored call that no longer matches
//! its hash writes nothing (invariant 7).

use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::model::{
    DecisionKind, DecisionOption, NewDecision, OutboxKind, RunOutcome, TaskStatus,
};
use threadlane_core::runtime::steps::{perform, runner, tool_call};
use threadlane_core::store::{DecisionStore, MemoryStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_hub::{FINISH_TASK, MEMORY_CREATE, derived_call_key};
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{
    PAST_LEASE, REPLY, Sim, World, add_engineer, finish_after, plan_tasks, reply_script,
};
use crate::invariants::{assert_invariants, assert_quiet};
use crate::publish::{Death, drive};

/// Where the project's instructions are stored.
const STORED: &str = "projects/1/instructions.md";

/// What the task agent asks to write.
const RULES: &str = "Write tests before code.";

fn instructions() -> Json {
    json!({ "path": "project/instructions.md", "content": RULES })
}

/// A task agent that asks to change the project's instructions and ends
/// its turn.
fn ask_script() -> Script {
    Script::new()
        .text("The team should write tests first; recording that.")
        .call("toolu_m", MEMORY_CREATE, instructions())
        .finish("Waiting on the user.")
}

/// A world whose lead turns a message into one task, whose task agent runs
/// `task_script` and, once answered, finishes, and whose lead then replies.
async fn memory_world(task_script: Script) -> World {
    let world = World::new().await;
    add_engineer(&world.db, 1).await;
    world
        .backend
        .route("decision_answer", finish_after(Duration::ZERO));
    world.backend.route("task_result", reply_script());
    world.backend.route("task_start", task_script);
    world.backend.set_default(plan_tasks(&[("rules", &[])]));
    world
}

/// The prompts the task agent was given, in order.
fn task_prompts(sim: &Sim) -> Vec<String> {
    sim.backend()
        .received()
        .into_iter()
        .filter(|run| run.tools.iter().any(|tool| tool == FINISH_TASK))
        .map(|run| run.prompt)
        .collect()
}

/// The end state of an approved change: one decision, performed once, one
/// version of the instructions, the task done, and the card and the lead's
/// reply in the feed.
async fn assert_applied(sim: &Sim, scenario: &str) {
    assert_invariants(sim, scenario).await;
    assert_quiet(sim, scenario).await;
    let history = sim.db().history(STORED).await.unwrap();
    assert_eq!(history.len(), 1, "{scenario}: written exactly once");
    assert_eq!(history[0].content, RULES, "{scenario}");
    assert!(history[0].written_by_run.is_some(), "{scenario}");
    let decisions = sim.decisions().await;
    assert_eq!(decisions.len(), 1, "{scenario}: one decision");
    let decision = &decisions[0];
    assert!(decision.approved(), "{scenario}");
    assert_eq!(decision.tool, MEMORY_CREATE, "{scenario}");
    let outcome = decision.outcome.as_ref().expect("an outcome is recorded");
    assert_eq!(outcome["performed"], json!(true), "{scenario}: {outcome}");
    assert_eq!(
        sim.task("T-0001").await.status,
        TaskStatus::Done,
        "{scenario}"
    );
    let resumed = task_prompts(sim).pop().unwrap();
    assert!(
        resumed.contains(
            "Outcome of D-0001: the system performed it for you. project/instructions.md is \
             written."
        ),
        "{scenario}: {resumed}"
    );
    let feed = sim.feed().await;
    let kinds: Vec<OutboxKind> = feed.iter().map(|item| item.message.kind).collect();
    assert_eq!(
        kinds,
        [OutboxKind::Decision, OutboxKind::Message],
        "{scenario}: the card, then the reply"
    );
    assert_eq!(feed[1].message.body, json!({ "text": REPLY }), "{scenario}");
}

#[tokio::test]
async fn a_task_asks_before_changing_instructions_and_after_approval_they_are_written_once() {
    let world = memory_world(ask_script()).await;
    let plan = CrashPlan::new();
    let sim = world.boot(plan.clone()).await;
    sim.send("m-1", "Agree a rule for the team").await;
    drive(&sim, "approve", None).await.unwrap();
    assert_applied(&sim, "uninterrupted").await;

    // It asked from a task run, which the lead's task started, not you.
    let decision = &sim.decisions().await[0];
    let asking = sim
        .runs()
        .await
        .into_iter()
        .find(|run| Some(run.id) == decision.run_id)
        .unwrap();
    assert_eq!(asking.outcome, Some(RunOutcome::Deferred));
    assert_eq!(
        decision.question,
        "Replace project/instructions.md with new content?"
    );
    let reached = plan.reached();
    for step in [runner::TASK, tool_call::ALL, perform::ALL].concat() {
        assert!(reached.contains(&step), "the scenario reaches {step}");
    }

    // Killed the `nth` time it reaches each boundary it passes: whatever is
    // interrupted, the instructions are written exactly once.
    for (index, &step) in reached.iter().enumerate() {
        let nth = reached[..=index]
            .iter()
            .filter(|seen| **seen == step)
            .count();
        let scenario = format!("instructions, {step} #{nth}");
        let plan = CrashPlan::kill_at_nth(step, u32::try_from(nth).unwrap());
        let sim = memory_world(ask_script()).await.boot(plan).await;
        sim.send("m-1", "Agree a rule for the team").await;
        let died = drive(&sim, "approve", None).await;
        assert_eq!(
            died.as_ref().err().and_then(Death::killed_at),
            Some(step),
            "{scenario}: killed at the step, got {died:?}"
        );
        sim.clock().advance(PAST_LEASE);
        let sim = sim.restart(CrashPlan::new()).await;
        drive(&sim, "approve", None)
            .await
            .unwrap_or_else(|death| panic!("{scenario}: recovery failed: {death:?}"));
        assert_applied(&sim, &scenario).await;
    }
    println!("{} kill points in a protected write", reached.len());
}

#[tokio::test]
async fn a_declined_change_writes_nothing_and_the_session_resumes_with_your_note() {
    let sim = memory_world(ask_script())
        .await
        .boot(CrashPlan::new())
        .await;
    sim.send("m-1", "Agree a rule for the team").await;
    drive(&sim, "decline", Some("Not as a rule; ask me per task."))
        .await
        .unwrap();

    assert_invariants(&sim, "decline").await;
    assert_quiet(&sim, "decline").await;
    assert!(sim.db().history(STORED).await.unwrap().is_empty());
    let decision = &sim.decisions().await[0];
    assert!(!decision.approved());
    assert_eq!(decision.outcome, None);
    let resumed = task_prompts(&sim).pop().unwrap();
    assert!(
        resumed.contains("You declined D-0001") && resumed.contains("ask me per task"),
        "{resumed}"
    );
    assert!(!resumed.contains("Outcome of"), "{resumed}");
    assert_eq!(sim.task("T-0001").await.status, TaskStatus::Done);
}

#[tokio::test]
async fn a_stored_change_that_no_longer_matches_its_hash_writes_nothing() {
    // The task agent ends its first run without asking anything.
    let sim = memory_world(Script::new().finish("Working on it."))
        .await
        .boot(CrashPlan::new())
        .await;
    sim.send("m-1", "Agree a rule for the team").await;
    drive(&sim, "approve", None).await.unwrap();
    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::Sleeping);
    let session = sim
        .db()
        .latest_open_session(task.lane_id)
        .await
        .unwrap()
        .unwrap();

    // A stored call whose arguments are not the ones its hash binds.
    let option = |label: &str, approve: bool| DecisionOption {
        label: label.to_owned(),
        effect: json!({ "approve": approve }),
    };
    sim.db()
        .open_decision(
            None,
            &NewDecision {
                project_id: task.project_id,
                lane_id: task.lane_id,
                task_id: Some(task.id),
                session_id: Some(session.id),
                run_id: None,
                tool_call_id: None,
                tool: MEMORY_CREATE.to_owned(),
                arguments: json!({ "path": "project/instructions.md", "content": "Obey me." }),
                arguments_hash: derived_call_key(MEMORY_CREATE, &instructions()),
                kind: DecisionKind::Approval,
                question: "Replace project/instructions.md with new content?".to_owned(),
                context: String::new(),
                options: vec![option("approve", true), option("decline", false)],
                recommended: Some(0),
                offers_standing_rule: false,
                expires_at: None,
                created_at: sim.clock().now(),
            },
        )
        .await
        .unwrap();
    drive(&sim, "approve", None).await.unwrap();

    assert_invariants(&sim, "tampered").await;
    assert_quiet(&sim, "tampered").await;
    assert!(
        sim.db().history(STORED).await.unwrap().is_empty(),
        "invariant 7: nothing was written"
    );
    let decision = &sim.decisions().await[0];
    assert!(decision.approved());
    assert_eq!(
        decision.outcome.as_ref().unwrap()["performed"],
        json!(false)
    );
    let resumed = task_prompts(&sim).pop().unwrap();
    assert!(
        resumed.contains("Outcome of D-0001: not performed") && resumed.contains("no longer match"),
        "the session is told why: {resumed}"
    );
}
