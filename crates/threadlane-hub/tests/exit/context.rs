//! The exit scenario of milestone 5 (Plan.md §15, docs/milestone-5.md §4),
//! end to end in one test on the scripted backend: each step is one
//! sentence of the Exit paragraph, and invariant 12 is asserted over every
//! memory version the scenario wrote.

use std::time::Duration;

use serde_json::json;
use threadlane_core::model::{
    EventKind, LaneKind, Mode, NewBinding, NewEvent, NewProject, TaskStatus, Trust,
};
use threadlane_core::store::{InboxStore, MemoryStore, OrgStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_hub::{FINISH_TASK, MEMORY_CREATE};
use threadlane_testkit::{CrashPlan, ReceivedRun, Script, SessionOutcome};

use crate::harness::{Sim, World, add_engineer, plan_tasks, reply_script};
use crate::invariants::{assert_invariants, assert_quiet};

/// What you tell the lead about General.
const PROJECT_RULE: &str = "In General, answer in French.";
/// What you tell the lead about every project.
const SPACE_RULE: &str = "Sign every reply as the team.";
/// What the task agent's first session leaves in its progress file.
const LEFT: &str = "The form is done; the tests are left.";
/// Longer than the lead's idle window, so the next message starts a session.
const PAST_IDLE: Duration = Duration::from_secs(31 * 60);

/// General's lead writes each rule when you give it, plans the page task
/// when asked, and otherwise replies. The task agent's first session writes
/// its progress and stops; a fresh session given that progress finishes.
async fn world() -> World {
    let world = World::new().await;
    add_engineer(&world.db, 1).await;
    let backend = &world.backend;
    backend.route(
        LEFT,
        Script::new()
            .text("Going on from my progress file: writing the tests.")
            .call(
                "toolu_p2",
                MEMORY_CREATE,
                json!({ "path": "project/tasks/T-0001.md", "content": "# T-0001\nAll done." }),
            )
            .call(
                "toolu_f",
                FINISH_TASK,
                json!({ "summary": "The page and its tests." }),
            )
            .finish("Finished the task."),
    );
    backend.route(
        "task_start",
        Script::new()
            .call(
                "toolu_p1",
                MEMORY_CREATE,
                json!({ "path": "project/tasks/T-0001.md", "content": format!("# T-0001\n{LEFT}") }),
            )
            .finish("Stopping for now."),
    );
    backend.route(
        "Remember for General",
        Script::new()
            .call(
                "toolu_r",
                MEMORY_CREATE,
                json!({ "path": "project/instructions.md", "content": PROJECT_RULE }),
            )
            .finish("Recorded the rule."),
    );
    backend.route(
        "Remember everywhere",
        Script::new()
            .call(
                "toolu_r",
                MEMORY_CREATE,
                json!({ "path": "space/instructions.md", "content": SPACE_RULE }),
            )
            .finish("Recorded the rule."),
    );
    backend.route("Build the page", plan_tasks(&[("page", &[])]));
    backend.set_default(reply_script());
    world
}

/// Binds General's lead as the lead of a second project, Alpha.
async fn add_alpha(sim: &Sim) {
    let alpha = sim
        .db()
        .create_project(&NewProject {
            name: "Alpha".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    let general = sim.db().general_project().await.unwrap();
    let lead = sim.db().lead_binding(general.id).await.unwrap().unwrap();
    sim.db()
        .bind(&NewBinding {
            project_id: alpha.id,
            agent_id: lead.agent_id,
            mode: Mode::OnRequest,
            is_lead: true,
            max_parallel: 1,
        })
        .await
        .unwrap();
}

/// Sends `text` to `project` (General if `None`), works every lane, and
/// returns the backend runs it caused.
async fn say(sim: &Sim, project: Option<&str>, text: &str) -> Vec<ReceivedRun> {
    let before = sim.backend().received().len();
    sim.clock().advance(PAST_IDLE);
    sim.send_to(project, &format!("m-{before}-{text}"), text)
        .await;
    work(sim).await;
    sim.backend().received().split_off(before)
}

async fn work(sim: &Sim) {
    sim.work_until_idle().await.unwrap();
    sim.dispatch().await.unwrap();
}

/// The one run whose prompt holds `text`.
fn run_with<'a>(runs: &'a [ReceivedRun], text: &str) -> &'a ReceivedRun {
    let found: Vec<&ReceivedRun> = runs
        .iter()
        .filter(|run| run.prompt.contains(text))
        .collect();
    assert_eq!(found.len(), 1, "one run's prompt holds {text:?}");
    found[0]
}

/// Invariant 12 over every version written: each was written by a run whose
/// own project or the space's notes hold it, or, for a lead, the space's
/// instructions. Returns how many versions were checked.
async fn assert_memory_in_scope(sim: &Sim) -> usize {
    let db = sim.db();
    let mut checked = 0;
    for file in db.list("").await.unwrap() {
        for version in db.history(&file.path).await.unwrap() {
            let run_id = version.written_by_run.expect("every write carries its run");
            let run = db.run(run_id).await.unwrap().unwrap();
            let lane = db.lane(run.lane_id).await.unwrap().unwrap();
            let path = &version.path;
            let own = path.starts_with(&format!("projects/{}/", lane.project_id.get()));
            let space = path.starts_with("space/notes/")
                || (path == "space/instructions.md" && lane.kind == LaneKind::Lead);
            assert!(
                own || space,
                "invariant 12: {path} v{} was written by {run_id} in project {} ({} lane)",
                version.version,
                lane.project_id,
                lane.kind
            );
            checked += 1;
        }
    }
    checked
}

#[tokio::test]
async fn milestone_5_exit_memory_and_context() {
    let sim = world().await.boot(CrashPlan::new()).await;
    add_alpha(&sim).await;

    // A project instruction given in one session changes what a later
    // session in that project is given, and has no effect in another project.
    let before = say(&sim, None, "Hello").await;
    let given = say(&sim, None, "Remember for General: answer in French").await;
    assert_eq!(
        sim.db()
            .read("projects/1/instructions.md")
            .await
            .unwrap()
            .unwrap()
            .content,
        PROJECT_RULE,
        "a run you triggered changes the instructions at once"
    );
    let later = say(&sim, None, "Hello again").await;
    let elsewhere = say(&sim, Some("Alpha"), "Hello from Alpha").await;
    assert!(!before[0].appended_instructions.contains(PROJECT_RULE));
    assert!(!given[0].appended_instructions.contains(PROJECT_RULE));
    assert_ne!(
        later[0].session_id(),
        given[0].session_id(),
        "a later session"
    );
    assert!(later[0].appended_instructions.contains(PROJECT_RULE));
    assert!(!elsewhere[0].appended_instructions.contains(PROJECT_RULE));

    // A space instruction applies in every project of the hub.
    say(&sim, Some("Alpha"), "Remember everywhere: sign replies").await;
    // Alpha first: its lead's reply is in General's space summary.
    let alpha = say(&sim, Some("Alpha"), "Status in Alpha?").await;
    let general = say(&sim, None, "Status?").await;
    assert!(general[0].appended_instructions.contains(SPACE_RULE));
    assert!(alpha[0].appended_instructions.contains(SPACE_RULE));

    // Appended instructions are byte-identical across runs when memory has
    // not changed, and differ when it has.
    let again = say(&sim, None, "Status once more?").await;
    assert_eq!(
        again[0].appended_instructions, general[0].appended_instructions,
        "memory did not change"
    );
    assert_ne!(
        general[0].appended_instructions,
        later[0].appended_instructions
    );

    // A task continues from its progress file after its native session is
    // deleted.
    let first = say(&sim, None, "Build the page").await;
    let task = sim.task("T-0001").await;
    assert_eq!(
        task.status,
        TaskStatus::Sleeping,
        "it stopped without finishing"
    );
    let started = run_with(&first, "task_start");
    assert!(started.appended_instructions.contains(SPACE_RULE));
    assert!(started.appended_instructions.contains(PROJECT_RULE));
    let first_received = first
        .iter()
        .filter(|run| run.prompt.contains("Your task"))
        .count();
    assert_eq!(
        first_received, 1,
        "it wrote its progress, so no final progress turn"
    );
    assert!(sim.backend().forget_session(started.session_id()));

    let before = sim.backend().received().len();
    sim.db()
        .insert_event(&NewEvent {
            project_id: task.project_id,
            lane_id: task.lane_id,
            source: "client".into(),
            source_event_id: "carry-on".into(),
            kind: EventKind::UserMessage,
            payload: json!({ "text": "Carry on with the page" }),
            trust: Trust::User,
            requested: true,
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: sim.clock().now(),
        })
        .await
        .unwrap();
    work(&sim).await;
    let resumed = sim.backend().received().split_off(before);
    let task_runs: Vec<&ReceivedRun> = resumed
        .iter()
        .filter(|run| run.prompt.contains("Your task"))
        .collect();
    assert_eq!(
        task_runs.len(),
        2,
        "a resume that finds nothing, then a fresh session"
    );
    assert_eq!(task_runs[0].session_outcome, SessionOutcome::Missing);
    assert_eq!(task_runs[1].session_outcome, SessionOutcome::Created);
    assert_ne!(task_runs[1].session_id(), started.session_id());
    assert!(
        task_runs[1].prompt.contains(&format!(
            "Your progress file, project/tasks/T-0001.md, as an earlier session left it:\n# T-0001\n{LEFT}"
        )),
        "{}",
        task_runs[1].prompt
    );
    assert!(task_runs[1].prompt.contains("Carry on with the page"));
    let done = sim.task("T-0001").await;
    assert_eq!(done.status, TaskStatus::Done);
    let event = sim
        .events()
        .await
        .into_iter()
        .find(|event| event.source_event_id == "carry-on")
        .unwrap();
    assert_eq!(
        event.attempts, 0,
        "a missing session is not a failed attempt"
    );

    // Invariant 12, over every memory version written in the scenario.
    let checked = assert_memory_in_scope(&sim).await;
    assert!(checked >= 6, "{checked} versions checked");
    assert_invariants(&sim, "milestone 5").await;
    assert_quiet(&sim, "milestone 5").await;
}
