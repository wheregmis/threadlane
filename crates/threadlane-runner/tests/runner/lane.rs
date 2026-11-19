//! One lane worked through `Runner::work_once` (docs/milestone-1.md §3, §8).

use std::time::Duration;

use serde_json::json;
use threadlane_core::backend::{BackendEvent, NativeSession};
use threadlane_core::ids::{LaneId, RunId, SessionId};
use threadlane_core::link::{LinkError, RunFailure};
use threadlane_core::model::{BackendKind, EventKind, LaneKind};
use threadlane_core::runtime::steps::{runner, tool_call};
use threadlane_core::runtime::{Killed, Step};
use threadlane_core::tools::ToolError;
use threadlane_runner::RunnerError;
use threadlane_testkit::{CrashPlan, Script, ScriptStep, SessionOutcome};

use crate::support::{
    Call, CallCounter, FakeLink, Harness, LANE, WALL_TIME, assignment, event, eventually, is_fail,
    is_release, journal_events, lease, run, settle,
};

const LANE_ID: LaneId = LaneId(LANE);

/// One granted lane, one run that says something and finishes, then idle.
fn script_one_run(h: &Harness) {
    script_one_run_in(h, LaneKind::Lead);
}

/// [`script_one_run`] in a lane of `kind`.
fn script_one_run_in(h: &Harness, kind: LaneKind) {
    h.link.grant(lease(LANE));
    let mut assignment = assignment(11);
    assignment.lane_kind = kind;
    h.link.push_begin(Ok(run(assignment)));
    h.backend
        .scripted
        .push(Script::new().text("thinking").finish("done"));
}

fn assert_killed_at(result: Result<bool, RunnerError>, step: Step) {
    match result {
        Err(RunnerError::Killed(Killed { step: killed })) => assert_eq!(killed, step),
        other => panic!("expected a kill at {step}, got {other:?}"),
    }
}

fn assert_abandoned(h: &Harness) {
    let calls = h.calls();
    assert!(
        !calls.iter().any(is_fail),
        "no fail after losing the lease: {calls:?}"
    );
    assert!(
        !calls.iter().any(is_release),
        "no release after losing the lease: {calls:?}"
    );
    assert_eq!(h.backend.stops(), 1, "the backend is stopped");
}

fn fail_of(h: &Harness) -> RunFailure {
    h.calls()
        .into_iter()
        .find_map(|call| match call {
            Call::Fail { failure, .. } => Some(failure),
            _ => None,
        })
        .expect("the run was failed")
}

#[tokio::test]
async fn no_lane_granted_does_nothing() {
    let h = Harness::new();
    assert!(!h.runner.work_once().await.expect("work_once"));
    assert_eq!(h.calls(), vec![Call::RequestWork]);
}

#[tokio::test]
async fn asking_for_work_on_an_unreachable_hub_is_an_error() {
    let h = Harness::new();
    h.link.push_request_work(Err(LinkError::Unreachable));
    let result = h.runner.work_once().await;
    assert!(
        matches!(result, Err(RunnerError::Link(LinkError::Unreachable))),
        "{result:?}"
    );
}

#[tokio::test]
async fn one_run_follows_the_cycle_exactly() {
    let h = Harness::new();
    script_one_run(&h);

    assert!(h.runner.work_once().await.expect("work_once"));

    assert_eq!(
        h.calls(),
        vec![
            Call::RequestWork,
            Call::Pending { lane: LANE_ID },
            Call::BeginRun { lane: LANE_ID },
            Call::Append { seqs: vec![1] },
            Call::Append { seqs: vec![2, 3] },
            Call::Complete {
                run: RunId(11),
                final_text: Some("done".into()),
            },
            Call::BeginRun { lane: LANE_ID },
            Call::Release { lease: lease(LANE) },
        ]
    );
    assert_eq!(h.plan.reached(), runner::LEAD);
    assert_eq!(h.backend.stops(), 0);
}

#[tokio::test]
async fn the_request_matches_the_assignment_field_for_field() {
    let h = Harness::new();
    script_one_run(&h);
    let lane_dir = h.config.lanes_dir.join(LANE.to_string());
    assert!(!lane_dir.exists());

    h.runner.work_once().await.expect("work_once");

    let expected = assignment(11);
    let received = h.backend.scripted.received();
    assert_eq!(received.len(), 1);
    let request = &received[0];
    assert_eq!(request.session, expected.native_session);
    assert_eq!(request.session_outcome, SessionOutcome::Created);
    assert_eq!(request.prompt, expected.prompt);
    assert_eq!(
        request.appended_instructions,
        expected.appended_instructions
    );
    assert_eq!(request.model, expected.model);
    assert_eq!(request.deny_rules, expected.deny_rules);
    assert_eq!(request.remove_builtin_tools, expected.remove_builtin_tools);
    assert_eq!(request.wall_time_limit, expected.wall_time_limit);
    assert_eq!(request.max_turns, expected.max_turns);
    assert_eq!(request.tools, vec!["send_message".to_owned()]);
    assert_eq!(request.working_dir, lane_dir);
    assert!(lane_dir.is_dir(), "the lane directory was created");
}

#[tokio::test]
async fn no_debounce_without_a_pending_user_message() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_pending(Ok(vec![
        event(EventKind::System),
        event(EventKind::OutsideEvent),
    ]));
    let start = h.now();

    assert!(h.runner.work_once().await.expect("work_once"));

    assert_eq!(h.now(), start, "the clock never had to move");
    assert_eq!(
        h.link.count(|call| matches!(call, Call::BeginRun { .. })),
        1
    );
}

#[tokio::test]
async fn a_pending_user_message_debounces_for_exactly_the_configured_time() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_pending(Ok(vec![
        event(EventKind::System),
        event(EventKind::UserMessage),
    ]));
    let start = h.now();

    let task = h.spawn_work_once();
    eventually("the debounce sleeps", || h.clock.sleepers() == 1).await;
    let before = vec![Call::RequestWork, Call::Pending { lane: LANE_ID }];
    assert_eq!(h.calls(), before);

    h.clock
        .advance(h.config.debounce - Duration::from_millis(1));
    settle().await;
    assert_eq!(h.calls(), before, "still debouncing 1 ms before the end");
    assert_eq!(h.plan.reached(), vec![runner::AFTER_CLAIM]);

    h.clock.advance(Duration::from_millis(1));
    assert!(task.await.expect("task").expect("work_once"));
    assert_eq!(h.now(), start + h.config.debounce);
    assert_eq!(
        h.calls()[2..],
        [
            Call::BeginRun { lane: LANE_ID },
            Call::Release { lease: lease(LANE) },
        ]
    );
}

#[tokio::test]
async fn runs_back_to_back_until_the_hub_is_idle() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.link.push_begin(Ok(run(assignment(12))));
    h.backend.scripted.push(Script::new().finish("first"));
    h.backend.scripted.push(Script::new().finish("second"));

    assert!(h.runner.work_once().await.expect("work_once"));

    let calls: Vec<Call> = h
        .calls()
        .into_iter()
        .filter(|call| !matches!(call, Call::Append { .. }))
        .collect();
    assert_eq!(
        calls,
        vec![
            Call::RequestWork,
            Call::Pending { lane: LANE_ID },
            Call::BeginRun { lane: LANE_ID },
            Call::Complete {
                run: RunId(11),
                final_text: Some("first".into()),
            },
            Call::BeginRun { lane: LANE_ID },
            Call::Complete {
                run: RunId(12),
                final_text: Some("second".into()),
            },
            Call::BeginRun { lane: LANE_ID },
            Call::Release { lease: lease(LANE) },
        ]
    );
    let per_run = [
        runner::AFTER_START_RUN,
        runner::AFTER_BACKEND_START,
        runner::AFTER_BACKEND_RUN,
        runner::AFTER_COMMIT,
    ];
    let mut expected = vec![runner::AFTER_CLAIM, runner::AFTER_DEBOUNCE];
    expected.extend(per_run);
    expected.extend(per_run);
    expected.extend([runner::AFTER_RECHECK, runner::AFTER_RELEASE]);
    assert_eq!(h.plan.reached(), expected);

    let received = h.backend.scripted.received();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].working_dir, received[1].working_dir);
    assert_eq!(received[1].prompt, "prompt for run 12");
}

#[tokio::test]
async fn a_tool_call_reaches_the_hub_and_its_result_is_mirrored() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.link.push_tool(Ok(json!({ "outbox_id": 5 })));
    let arguments = json!({ "text": "hello" });
    h.backend.scripted.push(
        Script::new()
            .call("toolu_1", "send_message", arguments.clone())
            .finish("sent"),
    );

    h.runner.work_once().await.expect("work_once");

    let tool_calls: Vec<Call> = h
        .calls()
        .into_iter()
        .filter(|call| matches!(call, Call::Tool { .. }))
        .collect();
    assert_eq!(
        tool_calls,
        vec![Call::Tool {
            run: RunId(11),
            call_id: Some("toolu_1".into()),
            tool: "send_message".into(),
            arguments: arguments.clone(),
        }]
    );
    assert_eq!(
        journal_events(&h.link),
        vec![
            BackendEvent::SessionStarted {
                native_session_id: "native-11".into(),
            },
            BackendEvent::ToolUse {
                call_id: Some("toolu_1".into()),
                name: "send_message".into(),
                input: arguments,
            },
            BackendEvent::ToolResult {
                call_id: Some("toolu_1".into()),
                output: json!({ "outbox_id": 5 }),
                is_error: false,
            },
            BackendEvent::Finished {
                final_text: Some("sent".into()),
                usage: json!({}),
            },
        ]
    );
}

#[tokio::test]
async fn journal_entries_are_numbered_from_one_and_all_appended_before_complete() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.backend.scripted.push(
        Script::new()
            .text("one")
            .call("toolu_1", "send_message", json!({ "text": "hi" }))
            .text("two")
            .finish("done"),
    );

    h.runner.work_once().await.expect("work_once");

    let journal = h.link.journal();
    let seqs: Vec<i64> = journal.iter().map(|entry| entry.seq).collect();
    assert_eq!(seqs, (1..=6).collect::<Vec<_>>());
    assert!(journal.iter().all(|entry| entry.run_id == RunId(11)));
    assert!(journal.iter().all(|entry| entry.at == h.now()));

    let calls = h.calls();
    let batches: Vec<_> = calls
        .iter()
        .filter_map(|call| match call {
            Call::Append { seqs } => Some(seqs.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(batches, [&[1][..], &[2, 3], &[4, 5, 6]]);
    let complete = calls
        .iter()
        .position(|call| matches!(call, Call::Complete { .. }))
        .expect("completed");
    let last_append = calls
        .iter()
        .rposition(|call| matches!(call, Call::Append { .. }))
        .expect("appended");
    assert!(last_append < complete, "{calls:?}");
}

#[tokio::test]
async fn an_unreachable_append_is_retried_with_the_next_event() {
    let h = Harness::new();
    script_one_run(&h);
    h.link.push_append(Err(LinkError::Unreachable));

    h.runner.work_once().await.expect("work_once");

    let appends: Vec<Call> = h
        .calls()
        .into_iter()
        .filter(|call| matches!(call, Call::Append { .. }))
        .collect();
    assert_eq!(
        appends,
        vec![
            Call::Append { seqs: vec![1] },
            Call::Append {
                seqs: vec![1, 2, 3]
            },
        ]
    );
    let seqs: Vec<i64> = h.link.journal().iter().map(|entry| entry.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    assert_eq!(
        journal_events(&h.link)[1],
        BackendEvent::Text {
            text: "thinking".into()
        }
    );
    assert_eq!(
        h.link.count(|call| matches!(call, Call::Complete { .. })),
        1
    );
}

#[tokio::test]
async fn a_run_is_never_completed_with_entries_left_unsent() {
    let h = Harness::new();
    script_one_run(&h);
    h.link.push_append(Ok(()));
    h.link.push_append(Err(LinkError::Unreachable));

    h.runner.work_once().await.expect("work_once");

    let calls = h.calls();
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::Complete { .. }))
    );
    assert!(matches!(fail_of(&h), RunFailure::Error { .. }));
    assert_eq!(calls.last(), Some(&Call::Release { lease: lease(LANE) }));
    assert_eq!(h.backend.stops(), 1);
}

#[tokio::test]
async fn a_failed_backend_run_fails_with_error() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.backend.scripted.push(Script::new().fail("boom"));

    h.runner.work_once().await.expect("work_once");

    let calls = h.calls();
    let tail = &calls[calls.len() - 2..];
    assert_eq!(
        tail,
        [
            Call::Fail {
                run: RunId(11),
                failure: RunFailure::Error {
                    message: "boom".into()
                },
            },
            Call::Release { lease: lease(LANE) },
        ]
    );
    assert!(!h.plan.reached().contains(&runner::AFTER_BACKEND_RUN));
}

#[tokio::test]
async fn a_backend_that_cannot_start_fails_the_run() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));

    h.runner.work_once().await.expect("work_once");

    match fail_of(&h) {
        RunFailure::Error { message } => assert!(message.contains("no script"), "{message}"),
        other => panic!("expected an error failure, got {other:?}"),
    }
    assert_eq!(
        h.calls().last(),
        Some(&Call::Release { lease: lease(LANE) })
    );
}

#[tokio::test]
async fn a_run_past_its_wall_time_is_stopped_and_fails_with_limit_exceeded() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.backend
        .scripted
        .push(Script::new().text("thinking").then(ScriptStep::Hang));

    let task = h.spawn_work_once();
    tokio::time::timeout(Duration::from_secs(2), async {
        while h.link.journal().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the pending journal batch flushes even while the backend waits");
    eventually("the run hangs", || {
        h.link.journal().len() == 2 && h.clock.sleepers() == 1
    })
    .await;

    h.clock.advance(WALL_TIME - Duration::from_secs(1));
    settle().await;
    assert!(!task.is_finished(), "still within the limit");
    assert_eq!(h.backend.stops(), 0);

    h.clock.advance(Duration::from_secs(1));
    assert!(task.await.expect("task").expect("work_once"));
    assert_eq!(h.backend.stops(), 1);
    assert!(
        matches!(fail_of(&h), RunFailure::LimitExceeded { .. }),
        "{:?}",
        fail_of(&h)
    );
    assert_eq!(
        h.calls().last(),
        Some(&Call::Release { lease: lease(LANE) })
    );
}

#[tokio::test]
async fn a_usage_limit_is_reported_then_fails_with_usage_limit() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    let resets_at = h.now() + Duration::from_secs(3_600);
    h.backend
        .scripted
        .push(Script::new().then(ScriptStep::UsageLimit(Some(resets_at))));

    h.runner.work_once().await.expect("work_once");

    let account = h
        .calls()
        .iter()
        .position(|call| {
            *call
                == Call::Account {
                    backend: BackendKind::Claude,
                    label: Some("claude-scripted".into()),
                    resets_at: Some(resets_at),
                }
        })
        .expect("the usage limit is reported with the probe's label");
    let fail = h.calls().iter().position(is_fail).expect("the run fails");
    assert!(account < fail, "reported before the run fails");
    assert_eq!(fail_of(&h), RunFailure::UsageLimit);
}

#[tokio::test]
async fn resuming_an_unknown_session_fails_with_session_missing_and_rechecks() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    let mut resume = assignment(11);
    resume.native_session = NativeSession::Resume { id: "gone".into() };
    h.link.push_begin(Ok(run(resume)));

    h.runner.work_once().await.expect("work_once");

    assert_eq!(
        h.calls()[3..],
        [
            Call::Append { seqs: vec![1] },
            Call::Fail {
                run: RunId(11),
                failure: RunFailure::SessionMissing,
            },
            Call::BeginRun { lane: LANE_ID },
            Call::Release { lease: lease(LANE) },
        ]
    );
    let received = h.backend.scripted.received();
    assert_eq!(received[0].session_outcome, SessionOutcome::Missing);
}

#[tokio::test]
async fn a_different_native_session_id_is_recorded() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.backend.scripted.push(
        Script::new()
            .then(ScriptStep::Emit(BackendEvent::SessionStarted {
                native_session_id: "codex-42".into(),
            }))
            .finish("done"),
    );

    h.runner.work_once().await.expect("work_once");

    let calls = h.calls();
    let recorded: Vec<&Call> = calls
        .iter()
        .filter(|call| matches!(call, Call::NativeSession { .. }))
        .collect();
    assert_eq!(
        recorded,
        vec![&Call::NativeSession {
            session: SessionId(111),
            native: "codex-42".into(),
        }],
        "the assigned id is not recorded again"
    );
}

#[tokio::test]
async fn a_task_lane_runs_in_its_working_directory_like_a_lead_lane() {
    let h = Harness::new();
    script_one_run_in(&h, LaneKind::Task);

    assert!(h.runner.work_once().await.expect("work_once"));

    let received = h.backend.scripted.received();
    assert_eq!(received.len(), 1, "the backend ran");
    assert_eq!(
        received[0].working_dir,
        h.config.lanes_dir.join(LANE.to_string())
    );
    assert!(
        h.calls()
            .iter()
            .any(|call| matches!(call, Call::Complete { .. }))
    );
    assert!(!h.calls().iter().any(is_fail));
    assert_eq!(h.plan.reached(), runner::TASK);
}

#[tokio::test]
async fn a_kill_at_every_step_of_either_lane_kind_stops_everything() {
    for (kind, steps) in [
        (LaneKind::Lead, runner::LEAD),
        (LaneKind::Task, runner::TASK),
    ] {
        a_kill_at_every_step_stops_everything(kind, steps).await;
    }
}

async fn a_kill_at_every_step_stops_everything(kind: LaneKind, steps: &[Step]) {
    let link = std::sync::Arc::new(FakeLink::default());
    let counter = std::sync::Arc::new(CallCounter::new(std::sync::Arc::clone(&link)));
    let happy = Harness::with_hook(link, counter.clone());
    script_one_run_in(&happy, kind);
    happy.runner.work_once().await.expect("work_once");
    let happy_calls = happy.calls();

    for &step in steps {
        let calls_before_kill = counter
            .reached()
            .into_iter()
            .find(|(reached, _)| *reached == step)
            .map(|(_, calls)| calls)
            .expect("the happy run reaches every step of its lane kind");
        let h = Harness::with_plan(CrashPlan::kill_at(step));
        script_one_run_in(&h, kind);

        assert_killed_at(h.runner.work_once().await, step);

        assert_eq!(
            h.calls(),
            happy_calls[..calls_before_kill],
            "no link call after a kill at {step}"
        );
        assert!(
            !h.calls().iter().any(is_fail),
            "no fail after a kill at {step}"
        );
        assert_eq!(
            h.backend.stops(),
            0,
            "nothing is stopped after a kill at {step}"
        );
    }
}

#[tokio::test]
async fn killed_hub_calls_are_process_death() {
    let hub_step = Step("hub.test");
    for kill_append in [true, false] {
        let h = Harness::new();
        script_one_run(&h);
        if kill_append {
            h.link
                .push_append(Err(LinkError::Killed(Killed { step: hub_step })));
        } else {
            h.link
                .push_complete(Err(LinkError::Killed(Killed { step: hub_step })));
        }

        assert_killed_at(h.runner.work_once().await, hub_step);

        let calls = h.calls();
        let last = calls.last().expect("calls were made");
        assert!(
            matches!(last, Call::Append { .. } | Call::Complete { .. }),
            "nothing after the killed call: {calls:?}"
        );
        assert_eq!(h.backend.stops(), 0);
    }
}

#[tokio::test]
async fn a_killed_tool_call_stops_the_lane() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.link.push_tool(Err(ToolError::Killed(Killed {
        step: tool_call::AFTER_EFFECT,
    })));
    h.backend.scripted.push(
        Script::new()
            .call("toolu_1", "send_message", json!({ "text": "hi" }))
            .finish("done"),
    );

    assert_killed_at(h.runner.work_once().await, tool_call::AFTER_EFFECT);

    let calls = h.calls();
    assert!(
        matches!(calls.last(), Some(Call::Tool { .. })),
        "nothing after the killed tool call: {calls:?}"
    );
    assert_eq!(h.backend.stops(), 0);
}

#[tokio::test]
async fn a_stale_lease_on_append_abandons_the_lane() {
    let h = Harness::new();
    script_one_run(&h);
    h.link
        .push_append(Err(LinkError::StaleLease { lane_id: LANE_ID }));

    assert!(h.runner.work_once().await.expect("work_once"));

    assert_abandoned(&h);
    assert_eq!(h.calls().last(), Some(&Call::Append { seqs: vec![1] }));
}

#[tokio::test]
async fn a_stale_lease_on_complete_abandons_the_lane() {
    let h = Harness::new();
    script_one_run(&h);
    h.link
        .push_complete(Err(LinkError::StaleLease { lane_id: LANE_ID }));

    assert!(h.runner.work_once().await.expect("work_once"));

    assert_abandoned(&h);
    assert!(matches!(h.calls().last(), Some(Call::Complete { .. })));
}

#[tokio::test]
async fn a_tool_call_that_lost_the_lease_abandons_the_lane() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.link.push_tool(Err(ToolError::LeaseLost));
    h.backend.scripted.push(
        Script::new()
            .call("toolu_1", "send_message", json!({ "text": "hi" }))
            .finish("done"),
    );

    assert!(h.runner.work_once().await.expect("work_once"));

    assert_abandoned(&h);
    assert!(matches!(h.calls().last(), Some(Call::Tool { .. })));
}

#[tokio::test]
async fn a_stale_lease_before_the_run_abandons_the_lane() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link
        .push_pending(Err(LinkError::StaleLease { lane_id: LANE_ID }));

    assert!(h.runner.work_once().await.expect("work_once"));

    assert_eq!(
        h.calls(),
        vec![Call::RequestWork, Call::Pending { lane: LANE_ID }]
    );
}

#[tokio::test]
async fn a_rejected_complete_stops_the_backend_fails_and_releases() {
    let h = Harness::new();
    script_one_run(&h);
    h.link
        .push_complete(Err(LinkError::Rejected("run is closed".into())));

    assert!(h.runner.work_once().await.expect("work_once"));

    assert_eq!(h.backend.stops(), 1);
    assert!(matches!(fail_of(&h), RunFailure::Error { .. }));
    assert_eq!(
        h.calls().last(),
        Some(&Call::Release { lease: lease(LANE) })
    );
}

#[tokio::test]
async fn a_rejected_begin_run_releases_without_failing() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link
        .push_begin(Err(LinkError::Rejected("no usable backend".into())));

    assert!(h.runner.work_once().await.expect("work_once"));

    assert_eq!(
        h.calls(),
        vec![
            Call::RequestWork,
            Call::Pending { lane: LANE_ID },
            Call::BeginRun { lane: LANE_ID },
            Call::Release { lease: lease(LANE) },
        ]
    );
}

#[tokio::test]
async fn a_rejected_fail_still_releases_once() {
    let h = Harness::new();
    h.link.grant(lease(LANE));
    h.link.push_begin(Ok(run(assignment(11))));
    h.link
        .push_fail(Err(LinkError::Rejected("run is closed".into())));
    h.backend.scripted.push(Script::new().fail("boom"));

    assert!(h.runner.work_once().await.expect("work_once"));

    assert_eq!(
        h.link.count(is_fail),
        1,
        "the failure is not reported twice"
    );
    assert_eq!(h.link.count(is_release), 1);
    assert_eq!(
        h.calls().last(),
        Some(&Call::Release { lease: lease(LANE) })
    );
}
