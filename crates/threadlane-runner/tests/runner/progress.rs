//! The final progress turn of a task run (docs/milestone-5.md §1 decision
//! 7): one more turn in the same session when the run would end without its
//! progress file written, and none when it was written, or the run failed,
//! was stopped, hit a usage limit, or waits on a decision.

use serde_json::json;
use threadlane_core::backend::{BackendEvent, NativeSession};
use threadlane_core::ids::{RunId, TaskId};
use threadlane_core::link::PROGRESS_WRITTEN;
use threadlane_core::model::LaneKind;
use threadlane_core::tools::ToolError;
use threadlane_runner::PROGRESS_PROMPT;
use threadlane_testkit::{Script, ScriptStep};

use crate::support::{Call, Harness, LANE, assignment, is_fail, journal_events, lease, run};

/// A granted task lane whose one run plays `script`; the next script queued
/// is what an extra turn would play.
fn task_run(h: &Harness, script: Script) {
    h.link.grant(lease(LANE));
    let mut assignment = assignment(11);
    assignment.lane_kind = LaneKind::Task;
    assignment.task_id = Some(TaskId(1));
    h.link.push_begin(Ok(run(assignment)));
    h.backend.scripted.push(script);
    h.backend
        .scripted
        .push(Script::new().text("saving progress").finish("saved"));
}

fn completed(h: &Harness) -> Vec<Option<String>> {
    h.calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Complete { final_text, .. } => Some(final_text),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_run_that_wrote_its_progress_file_ends_with_no_extra_turn() {
    let h = Harness::new();
    task_run(
        &h,
        Script::new()
            .call("toolu_p", "memory_create", json!({}))
            .finish("done"),
    );
    h.link.push_tool(Ok(
        json!({ "path": "project/tasks/T-0001.md", PROGRESS_WRITTEN: true }),
    ));

    h.runner.work_once().await.expect("work_once");

    assert_eq!(h.backend.scripted.received().len(), 1);
    assert_eq!(
        h.backend.scripted.remaining(),
        1,
        "no extra turn was played"
    );
    assert_eq!(completed(&h), vec![Some("done".to_owned())]);
}

#[tokio::test]
async fn a_run_that_did_not_write_it_takes_exactly_one_extra_turn_in_the_same_session() {
    let h = Harness::new();
    // A memory write that is not the progress file does not count.
    task_run(
        &h,
        Script::new()
            .call("toolu_n", "memory_create", json!({}))
            .finish("done"),
    );
    h.link
        .push_tool(Ok(json!({ "path": "project/notes/a.md", "version": 1 })));
    h.backend
        .scripted
        .push(Script::new().finish("a third turn"));

    h.runner.work_once().await.expect("work_once");

    let received = h.backend.scripted.received();
    assert_eq!(received.len(), 2, "one extra turn, never more");
    assert_eq!(h.backend.scripted.remaining(), 1);
    assert_eq!(received[1].prompt, PROGRESS_PROMPT);
    assert_eq!(
        received[1].session,
        NativeSession::Resume {
            id: "native-11".into()
        }
    );
    assert_eq!(received[1].working_dir, received[0].working_dir);
    // The run ends as it would have, with its own rationale, and the extra
    // turn is in its journal.
    assert_eq!(completed(&h), vec![Some("done".to_owned())]);
    let journal = journal_events(&h.link);
    assert!(
        journal.contains(&BackendEvent::Text {
            text: "saving progress".into()
        }),
        "{journal:?}"
    );
    assert!(!h.calls().iter().any(is_fail));
}

#[tokio::test]
async fn a_lead_run_takes_no_extra_turn() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.backend.scripted.push(Script::new().finish("done"));
    h.backend.scripted.push(Script::new().finish("extra"));

    h.runner.work_once().await.expect("work_once");

    assert_eq!(h.backend.scripted.received().len(), 1);
}

#[tokio::test]
async fn a_failed_run_takes_no_extra_turn() {
    let h = Harness::new();
    task_run(&h, Script::new().fail("boom"));

    h.runner.work_once().await.expect("work_once");

    assert_eq!(h.backend.scripted.received().len(), 1);
    assert!(h.calls().iter().any(is_fail));
    assert!(completed(&h).is_empty());
}

#[tokio::test]
async fn a_stopped_run_takes_no_extra_turn() {
    let h = Harness::new();
    task_run(
        &h,
        Script::new()
            .call("toolu_x", "memory_view", json!({}))
            .finish("done"),
    );
    h.link.push_tool(Err(ToolError::LeaseLost));

    h.runner.work_once().await.expect("work_once");

    assert_eq!(h.backend.scripted.received().len(), 1);
    assert!(completed(&h).is_empty());
}

#[tokio::test]
async fn a_run_at_its_usage_limit_takes_no_extra_turn() {
    let h = Harness::new();
    task_run(&h, Script::new().then(ScriptStep::UsageLimit(None)));

    h.runner.work_once().await.expect("work_once");

    assert_eq!(h.backend.scripted.received().len(), 1);
    assert!(h.calls().iter().any(is_fail));
}

#[tokio::test]
async fn a_run_waiting_on_a_decision_takes_no_extra_turn() {
    let h = Harness::new();
    task_run(
        &h,
        Script::new()
            .call("toolu_a", "publish_branch", json!({}))
            .finish("waiting on the user"),
    );
    h.link.push_tool(Err(ToolError::WaitingOnYou));

    h.runner.work_once().await.expect("work_once");

    assert_eq!(h.backend.scripted.received().len(), 1);
    assert_eq!(completed(&h), vec![Some("waiting on the user".to_owned())]);
    assert!(h.calls().iter().any(|call| *call
        == Call::Complete {
            run: RunId(11),
            final_text: Some("waiting on the user".into()),
        }));
}
