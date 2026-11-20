//! The extra turn of a task run that would end with nothing to wake its
//! task: one more turn in the same session with [`QUIET_END_PROMPT`], shared
//! with the final progress turn when both apply, and none for a run that
//! failed, was stopped, hit a usage limit, or waits on a decision. Whether
//! anything would wake the task (a follow-up, an open decision, a pending
//! event, `finish_task`) is the hub's answer to `left_unwoken`.

use serde_json::json;
use threadlane_core::backend::NativeSession;
use threadlane_core::ids::TaskId;
use threadlane_core::link::{LinkError, PROGRESS_WRITTEN};
use threadlane_core::model::LaneKind;
use threadlane_core::tools::ToolError;
use threadlane_runner::{PROGRESS_PROMPT, QUIET_END_PROMPT};
use threadlane_testkit::{Script, ScriptStep};

use crate::support::{Harness, LANE, assignment, is_fail, lease, run};

/// A granted task lane whose one run plays `script`, with a turn queued for
/// an extra turn to play, and the hub's answer to `left_unwoken`.
fn task_run(h: &Harness, script: Script, unwoken: Result<bool, LinkError>) {
    h.link.grant(lease(LANE));
    let mut assignment = assignment(11);
    assignment.lane_kind = LaneKind::Task;
    assignment.task_id = Some(TaskId(1));
    h.link.push_begin(Ok(run(assignment)));
    h.link.push_left_unwoken(unwoken);
    h.backend.scripted.push(script);
    h.backend
        .scripted
        .push(Script::new().text("setting a follow-up").finish("extra"));
}

/// A run that writes its progress file, so only the quiet end can apply.
fn wrote_progress(h: &Harness) -> Script {
    h.link.push_tool(Ok(
        json!({ "path": "project/tasks/T-0001.md", PROGRESS_WRITTEN: true }),
    ));
    Script::new()
        .call("toolu_p", "memory_create", json!({}))
        .finish("Waiting for clippy and fmt to finish.")
}

#[tokio::test]
async fn a_run_with_nothing_to_wake_its_task_takes_exactly_one_extra_turn() {
    let h = Harness::new();
    let script = wrote_progress(&h);
    task_run(&h, script, Ok(true));
    h.backend
        .scripted
        .push(Script::new().finish("a third turn"));

    h.runner.work_once().await.expect("work_once");

    let received = h.backend.scripted.received();
    assert_eq!(received.len(), 2, "one extra turn, never more");
    assert_eq!(received[1].prompt, QUIET_END_PROMPT);
    assert_eq!(
        received[1].session,
        NativeSession::Resume {
            id: "native-11".into()
        }
    );
    assert_eq!(h.link.unwoken_asked(), 1);
    assert!(!h.calls().iter().any(is_fail));
}

#[tokio::test]
async fn a_run_whose_task_has_something_to_wake_it_takes_none() {
    let h = Harness::new();
    let script = wrote_progress(&h);
    task_run(&h, script, Ok(false));

    h.runner.work_once().await.expect("work_once");

    assert_eq!(h.backend.scripted.received().len(), 1);
    assert_eq!(h.link.unwoken_asked(), 1);
}

#[tokio::test]
async fn with_the_progress_file_missing_too_both_share_one_turn() {
    let h = Harness::new();
    task_run(&h, Script::new().finish("done"), Ok(true));
    h.backend
        .scripted
        .push(Script::new().finish("a third turn"));

    h.runner.work_once().await.expect("work_once");

    let received = h.backend.scripted.received();
    assert_eq!(received.len(), 2, "one combined extra turn");
    assert_eq!(
        received[1].prompt,
        format!("{PROGRESS_PROMPT}\n\n{QUIET_END_PROMPT}")
    );
}

#[tokio::test]
async fn a_hub_that_cannot_answer_costs_only_the_extra_turn() {
    let h = Harness::new();
    let script = wrote_progress(&h);
    task_run(&h, script, Err(LinkError::Unreachable));

    h.runner.work_once().await.expect("work_once");

    assert_eq!(h.backend.scripted.received().len(), 1);
    assert!(!h.calls().iter().any(is_fail));
}

#[tokio::test]
async fn failed_stopped_usage_limited_and_deferred_runs_take_none() {
    let endings = [
        (Script::new().fail("boom"), None),
        (
            Script::new()
                .call("toolu_x", "memory_view", json!({}))
                .finish("done"),
            Some(Err(ToolError::LeaseLost)),
        ),
        (Script::new().then(ScriptStep::UsageLimit(None)), None),
        (
            Script::new()
                .call("toolu_a", "publish_branch", json!({}))
                .finish("waiting on the user"),
            Some(Err(ToolError::WaitingOnYou)),
        ),
        (
            Script::new()
                .call("toolu_a", "publish_branch", json!({}))
                .finish("the push is authorized"),
            Some(Err(ToolError::AuthorizedAndQueued)),
        ),
    ];
    for (script, tool) in endings {
        let h = Harness::new();
        if let Some(tool) = tool {
            h.link.push_tool(tool);
        }
        task_run(&h, script, Ok(true));

        h.runner.work_once().await.expect("work_once");

        assert_eq!(h.backend.scripted.received().len(), 1);
        assert_eq!(h.link.unwoken_asked(), 0, "the hub is not even asked");
    }
}
