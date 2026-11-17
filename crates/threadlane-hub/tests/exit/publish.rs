//! Exit scenarios of docs/milestone-3.md §4: a finished task asks before
//! publishing; after your answer its branch is pushed and a draft pull
//! request opened, exactly once, with the process killed at every step
//! boundary; a decline; a stored call that no longer matches its hash
//! (invariant 7); a backend that keeps going after asking; and nothing
//! token-shaped in the database or the logs (invariant 8).
//!
//! The real hub and the real runner, with `GitWorkspaces` on a local bare
//! repository and the fake forge, which records every push and pull request.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::client::{DecisionReply, DecisionRequest};
use threadlane_core::model::{
    DecisionKind, DecisionOption, DecisionStatus, NewDecision, OutboxKind, RunOutcome, RunStatus,
    TaskStatus,
};
use threadlane_core::runtime::Step;
use threadlane_core::store::{DecisionStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_hub::{FINISH_TASK, HubError, OPEN_PULL_REQUEST, derived_call_key};
use threadlane_runner::RunnerError;
use threadlane_testkit::fakes::ForgeCall;
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{
    REPLY, Sim, TASK_SUMMARY, World, add_engineer, finish_after, plan_tasks_in, reply_script,
};
use crate::invariants::{assert_invariants, assert_quiet};

/// The pull request the task agent asks for.
fn pull_request() -> Json {
    json!({ "repository": "app", "title": "Add the page", "body": "Adds the page." })
}

/// A task agent that asks to open its pull request and ends its turn.
pub fn ask_script() -> Script {
    Script::new()
        .text("The page is done; asking to open the pull request.")
        .call("toolu_pr", OPEN_PULL_REQUEST, pull_request())
        .finish("Waiting on the user.")
}

/// A world whose lead turns a message into one task in `app`, whose task
/// agent asks to open a pull request, and, once answered, finishes, and
/// whose lead then replies with [`REPLY`]. Scripts follow the prompt, so a
/// run repeated after a crash plays the same script.
pub async fn publish_world(world: World, task_script: Script) -> World {
    add_engineer(&world.db, 1).await;
    world.add_repository("app").await;
    world
        .backend
        .route("decision_answer", finish_after(Duration::ZERO));
    world.backend.route("task_result", reply_script());
    world.backend.route("task_start", task_script);
    world
        .backend
        .set_default(plan_tasks_in(&["app"], &[("page", &[])]));
    world
}

/// How a life ended.
#[derive(Debug)]
pub enum Death {
    Runner(RunnerError),
    Hub(HubError),
}

impl Death {
    pub fn killed_at(&self) -> Option<Step> {
        match self {
            Self::Runner(RunnerError::Killed(killed)) | Self::Hub(HubError::Killed(killed)) => {
                Some(killed.step)
            }
            _ => None,
        }
    }
}

/// One process lifetime: release what expired, work every lane, deliver.
async fn live(sim: &Sim) -> Result<(), Death> {
    sim.core.sweep_once().await.map_err(Death::Hub)?;
    sim.work_until_idle().await.map_err(Death::Runner)?;
    sim.dispatch().await.map_err(Death::Hub)?;
    Ok(())
}

/// Answers every pending decision with `option`; returns how many.
async fn answer_all(sim: &Sim, option: &str, note: Option<&str>) -> usize {
    let DecisionReply::Pending { decisions } =
        sim.api.decisions(DecisionRequest::Pending).await.unwrap()
    else {
        panic!("not a list");
    };
    for decision in &decisions {
        let answered = sim
            .api
            .decisions(DecisionRequest::Answer {
                decision: decision.id,
                option: option.to_owned(),
                note: note.map(str::to_owned),
            })
            .await
            .unwrap();
        assert!(
            matches!(answered, DecisionReply::Answered { recorded: true, .. }),
            "{answered:?}"
        );
    }
    decisions.len()
}

/// Lives, answering whatever waits on you with `option`, until nothing does.
pub async fn drive(sim: &Sim, option: &str, note: Option<&str>) -> Result<(), Death> {
    for _ in 0..5 {
        live(sim).await?;
        if answer_all(sim, option, note).await == 0 {
            return Ok(());
        }
    }
    panic!("decisions kept being asked");
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

/// The end state of an approved publish: one decision, performed once, one
/// draft pull request, the task done with its branch and pull request, and
/// the card and the lead's reply in the feed.
pub async fn assert_published(sim: &Sim, scenario: &str) {
    assert_invariants(sim, scenario).await;
    assert_quiet(sim, scenario).await;
    let forge = &sim.world.forge;
    let opened = forge.pull_requests();
    assert_eq!(opened.len(), 1, "{scenario}: exactly one pull request");
    for call in forge.calls() {
        match call {
            ForgeCall::Open {
                branch,
                base,
                title,
                body,
                ..
            } => assert_eq!(
                (
                    branch.as_str(),
                    base.as_str(),
                    title.as_str(),
                    body.as_str()
                ),
                (
                    "threadlane/T-0001",
                    "main",
                    "Add the page",
                    "Adds the page."
                ),
                "{scenario}"
            ),
            ForgeCall::Push { branch, worktree } => {
                assert_eq!(branch, "threadlane/T-0001", "{scenario}");
                assert_eq!(
                    worktree,
                    sim.world.workspaces().join("worktrees/T-0001/app"),
                    "{scenario}: in the task's own worktree"
                );
            }
            ForgeCall::Find { branch } => assert_eq!(branch, "threadlane/T-0001", "{scenario}"),
        }
    }
    let decisions = sim.decisions().await;
    assert_eq!(decisions.len(), 1, "{scenario}: one decision");
    let decision = &decisions[0];
    assert!(decision.approved(), "{scenario}");
    assert_eq!(
        decision
            .outcome
            .as_ref()
            .map(|outcome| &outcome["pull_request"]),
        Some(&json!(opened[0])),
        "{scenario}"
    );
    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::Done, "{scenario}");
    assert_eq!(
        task.result,
        Some(json!({
            "summary": TASK_SUMMARY,
            "branch": "threadlane/T-0001",
            "pull_request": opened[0],
        })),
        "{scenario}"
    );
    let feed = sim.feed().await;
    let kinds: Vec<OutboxKind> = feed.iter().map(|item| item.message.kind).collect();
    assert_eq!(
        kinds,
        [OutboxKind::Decision, OutboxKind::Message],
        "{scenario}: the card, then the reply"
    );
    assert_eq!(
        feed[0].message.body["decision"],
        json!("D-0001"),
        "{scenario}"
    );
    assert_eq!(feed[1].message.body, json!({ "text": REPLY }), "{scenario}");
}

#[tokio::test]
async fn a_finished_task_asks_before_publishing_and_after_the_answer_opens_one_draft() {
    let world = publish_world(World::new().await, ask_script()).await;
    let sim = world.boot(CrashPlan::new()).await;
    sim.send("m-1", "Put the page up and open a pull request")
        .await;
    live(&sim).await.unwrap();

    // The task asked and waits; nothing was published.
    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::WaitingOnYou);
    assert!(
        sim.world.forge.calls().is_empty(),
        "nothing before the answer"
    );
    let decisions = sim.decisions().await;
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].status, DecisionStatus::Pending);
    let asking_run = sim
        .runs()
        .await
        .into_iter()
        .find(|run| Some(run.id) == decisions[0].run_id)
        .unwrap();
    assert_eq!(asking_run.outcome, Some(RunOutcome::Deferred));
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1, "the decision card reached the feed");
    assert_eq!(feed[0].message.kind, OutboxKind::Decision);
    assert!(
        feed[0].message.body["text"]
            .as_str()
            .unwrap()
            .contains("threadlane answer D-0001"),
        "{}",
        feed[0].message.body
    );

    assert_eq!(answer_all(&sim, "approve", Some("Ship it.")).await, 1);
    live(&sim).await.unwrap();
    assert_published(&sim, "uninterrupted").await;
    assert_eq!(
        sim.world.forge.pushes().len(),
        1,
        "pushed as part of opening"
    );

    let prompts = task_prompts(&sim);
    assert_eq!(prompts.len(), 2, "one run to ask, one after the answer");
    let resumed = &prompts[1];
    for told in [
        "You approved D-0001",
        "Ship it.",
        "Outcome of D-0001: the system performed it",
        "https://forge.test/pull/1",
    ] {
        assert!(resumed.contains(told), "{told:?} in {resumed}");
    }
}

#[tokio::test]
async fn a_declined_call_is_not_performed_and_the_session_resumes_with_your_note() {
    let world = publish_world(World::new().await, ask_script()).await;
    let sim = world.boot(CrashPlan::new()).await;
    sim.send("m-1", "Put the page up").await;
    drive(&sim, "decline", Some("Not yet: add tests first."))
        .await
        .unwrap();

    assert_invariants(&sim, "decline").await;
    assert_quiet(&sim, "decline").await;
    assert!(sim.world.forge.calls().is_empty(), "nothing was published");
    let decision = &sim.decisions().await[0];
    assert_eq!(decision.status, DecisionStatus::Answered);
    assert!(!decision.approved());
    assert_eq!(decision.outcome, None);
    let prompts = task_prompts(&sim);
    let resumed = prompts.last().unwrap();
    assert!(
        resumed.contains("You declined D-0001") && resumed.contains("add tests first"),
        "{resumed}"
    );
    assert!(!resumed.contains("Outcome of"), "{resumed}");
    // Declining is information, not failure: no attempt is used.
    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::Done);
    assert_eq!(task.result, Some(json!({ "summary": TASK_SUMMARY })));
    for event in sim.events().await {
        assert_eq!(event.attempts, 0, "event {} used an attempt", event.id);
    }
    assert!(
        sim.runs()
            .await
            .iter()
            .all(|run| run.status == RunStatus::Committed)
    );
}

#[tokio::test]
async fn a_stored_call_that_no_longer_matches_its_hash_performs_nothing() {
    // The task agent ends its first run without asking anything.
    let world = publish_world(World::new().await, Script::new().finish("Working on it.")).await;
    let sim = world.boot(CrashPlan::new()).await;
    sim.send("m-1", "Put the page up").await;
    live(&sim).await.unwrap();
    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::Sleeping);
    let session = sim
        .db()
        .latest_open_session(task.lane_id)
        .await
        .unwrap()
        .unwrap();

    // A stored call whose arguments are not the ones its hash binds, as if
    // they had been changed after it was asked.
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
                tool: OPEN_PULL_REQUEST.to_owned(),
                arguments: json!({ "repository": "app", "title": "Something else" }),
                arguments_hash: derived_call_key(OPEN_PULL_REQUEST, &pull_request()),
                kind: DecisionKind::Approval,
                question: "Open a draft pull request?".to_owned(),
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
        sim.world.forge.calls().is_empty(),
        "invariant 7: nothing was performed"
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
    assert_eq!(sim.task("T-0001").await.status, TaskStatus::Done);
}

#[tokio::test]
async fn the_runner_stops_a_backend_that_keeps_working_after_it_asked() {
    let keeps_going = Script::new()
        .call("toolu_pr", OPEN_PULL_REQUEST, pull_request())
        .text("Meanwhile, I will just finish.")
        .call("toolu_f", FINISH_TASK, json!({ "summary": "Sneaked out." }))
        .finish("Done anyway.");
    let world = publish_world(World::new().await, keeps_going).await;
    let sim = world.boot(CrashPlan::new()).await;
    sim.send("m-1", "Put the page up").await;
    live(&sim).await.unwrap();

    let task = sim.task("T-0001").await;
    assert_eq!(task.status, TaskStatus::WaitingOnYou);
    assert_eq!(task.result, None, "finish_task never reached the hub");
    let decision = &sim.decisions().await[0];
    let run = sim
        .runs()
        .await
        .into_iter()
        .find(|run| Some(run.id) == decision.run_id)
        .unwrap();
    assert_eq!(run.status, RunStatus::Committed);
    assert_eq!(run.outcome, Some(RunOutcome::Deferred));
    assert!(
        run.rationale
            .as_deref()
            .unwrap_or_default()
            .starts_with("Stopped by the runner"),
        "{:?}",
        run.rationale
    );
    let journal = sim.journal_types(run.id).await;
    assert_eq!(
        journal.last().map(String::as_str),
        Some("tool_use"),
        "stopped at the next tool use: {journal:?}"
    );
    assert_invariants(&sim, "keeps going").await;
    assert_quiet(&sim, "keeps going").await;
}

/// Captured log output.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Something that looks like a credential: a known token prefix, or a long
/// run of letters and digits that is random in case too, as tokens are and
/// hashes and ids (lower-case hex, UUIDs), or words run together in a
/// database page, are not.
fn token_shaped(text: &str) -> Option<String> {
    const PREFIXES: &[&str] = &[
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "sk-ant-",
        "xoxb-",
        "xoxp-",
        "AKIA",
    ];
    if let Some(prefix) = PREFIXES.iter().find(|prefix| text.contains(**prefix)) {
        return Some((*prefix).to_owned());
    }
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|word| {
            let count = |class: fn(&char) -> bool| word.chars().filter(class).count();
            word.len() >= 32
                && count(char::is_ascii_uppercase) >= 6
                && count(char::is_ascii_lowercase) >= 6
                && count(char::is_ascii_digit) >= 4
        })
        .map(str::to_owned)
}

#[tokio::test]
async fn no_table_or_log_line_holds_a_token() {
    assert!(token_shaped("ghp_abc").is_some());
    assert!(token_shaped("x AbCdEfGhIjKlMnOpQrStUvWxYz0123456789 y").is_some());
    assert!(token_shaped(&derived_call_key("t", &json!({}))).is_none());
    let hash = derived_call_key("t", &json!({}));
    assert!(token_shaped(&format!("{}approvalOpen", &hash[2..])).is_none());
    assert!(token_shaped("5f0c2a8e-1b7d-4c39-8e0f-2d6a9b4c7e15").is_none());

    let logs = Captured::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::tempdir().unwrap();
    let world = publish_world(World::on_disk(dir.path()).await, ask_script()).await;
    let sim = world.boot(CrashPlan::new()).await;
    sim.send("m-1", "Put the page up and open a pull request")
        .await;
    drive(&sim, "approve", None).await.unwrap();
    assert_published(&sim, "on disk").await;
    tracing::info!("scenario finished");

    let logged = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    assert!(logged.contains("scenario finished"), "logs were captured");
    for line in logged.lines() {
        assert_eq!(token_shaped(line), None, "invariant 8: a log line: {line}");
    }
    let mut files = 0;
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        files += 1;
        assert_eq!(
            token_shaped(&String::from_utf8_lossy(&bytes)),
            None,
            "invariant 8: {}",
            path.display()
        );
    }
    assert!(files > 0, "the database is on disk");
    // That the real forge sets no credential and runs only `git` and `gh` is
    // the runner's forge tests.
}
