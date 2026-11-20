//! Plan.md §13 invariants as assertions on the stores, called at the end of
//! every scenario. Invariant 10 has its own scenarios (`fencing`); 9 is
//! invariant 1 plus the feed checks of each scenario.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::Value as Json;
use threadlane_core::ids::{EventId, LaneId, RunId, SessionId, TaskId};
use threadlane_core::model::{EventKind, EventStatus, OutboxKind, RunStatus, TaskStatus};
use threadlane_core::store::SessionStore;
use threadlane_hub::{CREATE_TASK, SEND_MESSAGE};

use crate::harness::Sim;

/// Every invariant this milestone covers that can be read from the stores.
pub async fn assert_invariants(sim: &Sim, scenario: &str) {
    assert_every_event_finished(sim, scenario).await;
    assert_one_active_run_per_lane(sim, scenario).await;
    assert_no_event_in_two_committed_runs(sim, scenario).await;
    assert_one_outbox_row_per_send_message_call(sim, scenario).await;
    assert_one_task_per_create_task_call(sim, scenario).await;
    assert_one_result_per_ended_task(sim, scenario).await;
    assert_no_two_tasks_share_a_worktree(sim, scenario);
    assert_one_card_per_decision(sim, scenario).await;
}

/// Every decision that was asked has exactly one card in the outbox
/// (Plan.md §10.1), however often a crash interrupted asking. One a
/// standing rule, the classifier or the built-in default answered was never
/// asked, and has none.
pub async fn assert_one_card_per_decision(sim: &Sim, scenario: &str) {
    let outbox = sim.outbox().await;
    let events = sim.events().await;
    for decision in sim.decisions().await {
        let by_rule = events.iter().any(|event| {
            event.decision_ref == Some(decision.id)
                && event.payload["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Standing rule"))
        });
        let cards = outbox
            .iter()
            .filter(|row| row.kind == OutboxKind::Decision && row.decision_id == Some(decision.id))
            .count();
        assert_eq!(
            cards,
            usize::from(
                !by_rule
                    && !decision
                        .answer
                        .as_ref()
                        .is_some_and(|a| a["classifier"] == "allow" || a["by_default"] == true)
            ),
            "{scenario}: decision {} has {cards} cards",
            decision.id
        );
    }
}

/// The end of a scenario: nothing holds a lease and no run is active.
pub async fn assert_quiet(sim: &Sim, scenario: &str) {
    for lane in sim.lanes().await {
        assert_eq!(
            lane.lease_owner, None,
            "{scenario}: lane {} still holds a lease",
            lane.id
        );
    }
    let active: Vec<_> = sim
        .runs()
        .await
        .into_iter()
        .filter(|run| run.status == RunStatus::Active)
        .map(|run| run.id)
        .collect();
    assert!(
        active.is_empty(),
        "{scenario}: runs left active: {active:?}"
    );
    for task in sim.tasks().await {
        assert_ne!(
            task.status,
            TaskStatus::Running,
            "{scenario}: task {} is running without a run",
            task.id
        );
    }
}

/// Invariant 1: every accepted event became `done`, `skipped`, or `dead`.
pub async fn assert_every_event_finished(sim: &Sim, scenario: &str) {
    for event in sim.events().await {
        assert!(
            matches!(
                event.status,
                EventStatus::Done | EventStatus::Skipped | EventStatus::Dead
            ),
            "{scenario}: invariant 1: event {} is {}",
            event.id,
            event.status
        );
    }
}

/// Invariant 2: at most one run is active per lane.
pub async fn assert_one_active_run_per_lane(sim: &Sim, scenario: &str) {
    let mut active: HashMap<LaneId, u32> = HashMap::new();
    for run in sim.runs().await {
        if run.status == RunStatus::Active {
            *active.entry(run.lane_id).or_default() += 1;
        }
    }
    for (lane, runs) in active {
        assert!(
            runs <= 1,
            "{scenario}: invariant 2: lane {lane} has {runs} active runs"
        );
    }
}

/// Invariant 3: no event is consumed by more than one committed run.
pub async fn assert_no_event_in_two_committed_runs(sim: &Sim, scenario: &str) {
    let mut consumed: HashMap<EventId, u32> = HashMap::new();
    for run in sim.runs().await {
        if run.status != RunStatus::Committed {
            continue;
        }
        for event in sim.db().run_events(run.id).await.unwrap() {
            *consumed.entry(event.id).or_default() += 1;
        }
    }
    for (event, runs) in consumed {
        assert_eq!(
            runs, 1,
            "{scenario}: invariant 3: event {event} was consumed by {runs} committed runs"
        );
    }
}

/// The calls to `tool` the mirrored journals show, per session: a call id
/// within a backend session (a resumed session repeats its ids), or,
/// without an id, each `tool_use` entry on its own.
async fn calls_per_session(sim: &Sim, tool: &str) -> HashMap<SessionId, u32> {
    let mut ids: HashSet<(SessionId, String)> = HashSet::new();
    let mut calls: HashMap<SessionId, u32> = HashMap::new();
    for run in sim.runs().await {
        for entry in sim.journal(run.id).await {
            let entry = &entry.entry;
            if entry.get("type").and_then(Json::as_str) != Some("tool_use")
                || entry.get("name").and_then(Json::as_str) != Some(tool)
            {
                continue;
            }
            let new_call = match entry.get("call_id").and_then(Json::as_str) {
                Some(id) => ids.insert((run.session_id, id.to_owned())),
                None => true,
            };
            if new_call {
                *calls.entry(run.session_id).or_default() += 1;
            }
        }
    }
    calls
}

/// Invariant 5 for `create_task`: one call created at most one task,
/// however often a crash made the backend repeat it. Creation is keyed on
/// the call, so a session's tasks must not outnumber its calls.
pub async fn assert_one_task_per_create_task_call(sim: &Sim, scenario: &str) {
    let calls = calls_per_session(sim, CREATE_TASK).await;
    let mut tasks: HashMap<SessionId, u32> = HashMap::new();
    for task in sim.tasks().await {
        let key = task
            .created_by_call
            .clone()
            .unwrap_or_else(|| panic!("{scenario}: task {} names no call", task.id));
        if task.origin == threadlane_core::model::TaskOrigin::User && key.starts_with("client:") {
            continue;
        }
        // `s<session>:<call id>`, or `r<run>:<derived key>` without a call id.
        let (scope, id) = key
            .split_once(':')
            .and_then(|(scope, _)| Some((scope.get(..1)?, scope.get(1..)?.parse::<i64>().ok()?)))
            .unwrap_or_else(|| panic!("{scenario}: task {} has key {key}", task.id));
        let session = match scope {
            "s" => SessionId(id),
            "r" => sim.db().run(RunId(id)).await.unwrap().unwrap().session_id,
            _ => panic!("{scenario}: task {} has key {key}", task.id),
        };
        *tasks.entry(session).or_default() += 1;
    }
    for (session, tasks) in tasks {
        let calls = calls.get(&session).copied().unwrap_or(0);
        assert!(
            tasks <= calls,
            "{scenario}: invariant 5: session {session} has {tasks} tasks from {calls} create_task calls"
        );
    }
}

/// A task that ended was reported to its lead lane exactly once, and one
/// that has not ended was not reported (docs/milestone-3.md §2 "Events").
/// The notice of a task left with nothing to wake it is not a result.
pub async fn assert_one_result_per_ended_task(sim: &Sim, scenario: &str) {
    let mut results: HashMap<TaskId, u32> = HashMap::new();
    for event in sim.events().await {
        if event.kind == EventKind::TaskResult && event.source_event_id.starts_with("result:") {
            let task: TaskId = serde_json::from_value(event.payload["task_id"].clone()).unwrap();
            *results.entry(task).or_default() += 1;
        }
    }
    for task in sim.tasks().await {
        let expected = u32::from(task.status.is_terminal());
        assert_eq!(
            results.get(&task.id).copied().unwrap_or(0),
            expected,
            "{scenario}: task {} is {} and was reported that many times",
            task.id,
            task.status
        );
    }
}

/// Invariants 5 and 6: one `send_message` call produced at most one outbox
/// row, however often a crash made the backend repeat it.
///
/// A call is what the mirrored journals show: a call id within a backend
/// session (a resumed session repeats its ids), or, without an id, each
/// `tool_use` entry on its own. So a session's message rows must not
/// outnumber its calls, and no two rows may share an idempotency key. A
/// lead's final text, delivered when the run sent nothing, is not a call.
pub async fn assert_one_outbox_row_per_send_message_call(sim: &Sim, scenario: &str) {
    let calls = calls_per_session(sim, SEND_MESSAGE).await;
    let mut rows: HashMap<SessionId, u32> = HashMap::new();
    let mut keys: HashSet<String> = HashSet::new();
    for row in sim.outbox().await {
        if row.kind != OutboxKind::Message {
            continue;
        }
        let key = row
            .tool_call_id
            .clone()
            .unwrap_or_else(|| panic!("{scenario}: message {} names no call", row.id));
        assert!(
            keys.insert(key.clone()),
            "{scenario}: invariant 6: two rows under key {key}"
        );
        // A lead's delivered final text is one per run, not a call.
        if row.run_id.is_some() && !key.starts_with("final_text:") {
            let session = row.session_id.expect("a message from a run has a session");
            *rows.entry(session).or_default() += 1;
        }
    }
    for (session, rows) in rows {
        let calls = calls.get(&session).copied().unwrap_or(0);
        assert!(
            rows <= calls,
            "{scenario}: invariant 6: session {session} has {rows} messages from {calls} send_message calls"
        );
    }
}

/// Invariant 15: two tasks never share a worktree. Every run of one task
/// works in one directory, and no other task's run works in it.
pub fn assert_no_two_tasks_share_a_worktree(sim: &Sim, scenario: &str) {
    let mut dirs: HashMap<String, PathBuf> = HashMap::new();
    let codex = sim.world.codex.iter().flat_map(|codex| codex.received());
    for run in sim.backend().received().into_iter().chain(codex) {
        let Some(task) = run
            .prompt
            .split_once("Your task, ")
            .and_then(|(_, rest)| rest.split_once(':'))
            .map(|(task, _)| task.to_owned())
        else {
            continue;
        };
        let dir = dirs.entry(task.clone()).or_insert(run.working_dir.clone());
        assert_eq!(
            *dir, run.working_dir,
            "{scenario}: task {task} ran in two directories"
        );
    }
    let distinct: HashSet<&PathBuf> = dirs.values().collect();
    assert_eq!(
        distinct.len(),
        dirs.len(),
        "{scenario}: invariant 15: tasks share a worktree: {dirs:?}"
    );
}
