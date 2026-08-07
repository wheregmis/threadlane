use threadlane_agent::harness::{
    AgentHarness, LaneStatus, MemoryStore, OperationIntent, ProvisionedEntry, QueueKind, Record,
    ReduceError, Reducer, SessionStore, ToolReplaySafety, ToolResult, ToolSpec,
};
use threadlane_agent::AgentMessage;

#[test]
fn reduces_an_open_run_to_suspended_crash_without_effects() {
    let mut store = MemoryStore::new("session-1");
    let entry = store.append_message(None, AgentMessage::user("hello", vec![]));
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        source_leaf_id: Some(entry.clone()),
        intent: OperationIntent::Run,
    });

    let state = Reducer::reduce(&store).unwrap();
    assert_eq!(
        state.lane("main").unwrap().status,
        LaneStatus::SuspendedCrash
    );
    assert_eq!(
        state.lane("main").unwrap().open_operation.as_deref(),
        Some("run-1")
    );
    assert_eq!(store.effect_count(), 0);
}

#[test]
fn reducer_rejects_run_records_after_finish() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    store.append_record(Record::OperationFinished {
        id: "finish-1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        run_id: "run-1".into(),
        outcome: threadlane_agent::harness::OperationOutcome::Completed,
        error: None,
    });
    store.append_record_unchecked(Record::StepAttempt {
        id: "attempt-1".into(),
        seq: 3,
        lane: "main".into(),
        timestamp: 3,
        run_id: "run-1".into(),
        attempt: 1,
        result_entry_id: "missing-result".into(),
        compaction_reason: None,
    });
    assert!(matches!(
        Reducer::reduce(&store),
        Err(ReduceError::UnknownOperation(run_id)) if run_id == "run-1"
    ));
}

#[test]
fn child_lane_entries_do_not_advance_main_leaf() {
    let mut store = MemoryStore::new("session-1");
    let root = store.append_message(None, AgentMessage::user("parent", vec![]));
    store.append_record(Record::OperationStarted {
        id: "child-run".into(),
        seq: 2,
        lane: "child@1".into(),
        timestamp: 2,
        source_leaf_id: Some(root.clone()),
        intent: OperationIntent::Run,
    });
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "child-entry".into(),
            parent_id: Some(root.clone()),
            lane: "child".into(),
            seq: 3,
            timestamp: 3,
            message: AgentMessage::Assistant {
                content: Some("child".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            terminate: false,
        })
        .unwrap();
    store.append_record(Record::LaneMoved {
        id: "child-move".into(),
        seq: 4,
        lane: "child@1".into(),
        timestamp: 4,
        run_id: "child-run".into(),
        target_leaf_id: "child-entry".into(),
    });

    let state = Reducer::reduce(&store).unwrap();
    assert_eq!(
        state.lane("main").unwrap().leaf_id.as_deref(),
        Some(root.as_str())
    );
    assert_eq!(
        state.lane("child@1").unwrap().leaf_id.as_deref(),
        Some("child-entry")
    );
}

#[test]
fn operation_acceptance_can_target_a_child_lane() {
    let mut store = MemoryStore::new("session-1");
    let root = store.append_message(None, AgentMessage::user("parent", vec![]));
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "child-leaf".into(),
            parent_id: Some(root.clone()),
            lane: "child".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("child", vec![]),
            terminate: false,
        })
        .unwrap();

    let mut harness = AgentHarness::new(store);
    harness
        .start_operation_on_lane(
            "child",
            "child-run",
            Some("child-leaf".into()),
            OperationIntent::Run,
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    assert_eq!(
        Reducer::reduce(harness.store())
            .unwrap()
            .lane("child")
            .unwrap()
            .open_operation
            .as_deref(),
        Some("child-run")
    );
}

#[test]
fn prompt_acceptance_commits_input_and_provisions_the_first_attempt() {
    let store = MemoryStore::new("session-1");
    let mut harness = AgentHarness::new(store);
    let result_id = harness
        .accept_prompt("run-1", AgentMessage::user("hello", vec![]))
        .unwrap();

    assert_eq!(result_id, "entry-run-1-assistant-1");
    assert!(harness.store().entries().is_empty());
    harness.drive_to_completion().unwrap();
    assert_eq!(harness.store().entries().len(), 1);
    assert!(harness.store().records().iter().any(|record| matches!(
        record,
        Record::StepAttempt { run_id, result_entry_id, attempt: 1, .. }
            if run_id == "run-1" && result_entry_id == "entry-run-1-assistant-1"
    )));
    assert_eq!(
        Reducer::reduce(harness.store())
            .unwrap()
            .lane("main")
            .unwrap()
            .open_operation
            .as_deref(),
        Some("run-1")
    );
}

#[test]
fn no_tool_acceptance_writes_prompt_and_result_on_a_child_lane() {
    let mut store = MemoryStore::new("session-1");
    let root = store.append_message(None, AgentMessage::user("parent", vec![]));
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "child-leaf".into(),
            parent_id: Some(root),
            lane: "child".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("child", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut harness = AgentHarness::new(store);
    harness
        .accept_no_tool_run_on_lane(
            "child",
            "child-run",
            "continue",
            AgentMessage::Assistant {
                content: Some("done".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            },
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    let state = Reducer::reduce(harness.store()).unwrap();
    assert_eq!(state.lane("child").unwrap().status, LaneStatus::Completed);
    assert!(
        harness
            .store()
            .entries()
            .iter()
            .filter(|entry| entry.lane == "child")
            .count()
            >= 2
    );
}

#[test]
fn queued_input_is_scoped_to_the_target_lane() {
    let mut store = MemoryStore::new("session-1");
    let root = store.append_message(None, AgentMessage::user("parent", vec![]));
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "child-leaf".into(),
            parent_id: Some(root),
            lane: "child".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("child", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut harness = AgentHarness::new(store);
    harness
        .start_operation_on_lane(
            "child",
            "child-run",
            Some("child-leaf".into()),
            OperationIntent::Run,
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .enqueue_on_lane(
            "child",
            "child-run",
            QueueKind::FollowUp,
            ProvisionedEntry {
                id: "queued-child".into(),
                parent_id: Some("child-leaf".into()),
                message: AgentMessage::user("follow up", vec![]),
            },
        )
        .unwrap();
    harness.drive_to_completion().unwrap();

    let state = Reducer::reduce(harness.store()).unwrap();
    assert_eq!(state.lane("main").unwrap().queued.len(), 0);
    assert_eq!(state.lane("child").unwrap().queued.len(), 1);
}

#[test]
fn tool_intent_and_result_recovery_stay_on_a_child_lane() {
    let mut store = MemoryStore::new("session-1");
    let root = store.append_message(None, AgentMessage::user("parent", vec![]));
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "child-leaf".into(),
            parent_id: Some(root),
            lane: "child".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("child", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut harness = AgentHarness::new(store);
    harness
        .start_operation_on_lane(
            "child",
            "child-run",
            Some("child-leaf".into()),
            OperationIntent::Run,
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .append_entry_gated(threadlane_agent::harness::Entry {
            id: "child-assistant".into(),
            parent_id: Some("child-leaf".into()),
            lane: "child".into(),
            seq: 4,
            timestamp: 4,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            terminate: false,
        })
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .start_tool_batch(
            "child-run",
            "child-assistant",
            &[ToolSpec {
                index: 0,
                call_id: "call-child".into(),
                name: "read_file".into(),
                effective_args: serde_json::json!({"path":"README.md"}),
                result_entry_id: "child-tool-result".into(),
                replay: ToolReplaySafety::Safe,
            }],
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .finish_tool(
            "child-run",
            ToolResult {
                call_id: "call-child".into(),
                name: "read_file".into(),
                content: "ok".into(),
                is_error: false,
                terminate: false,
            },
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    let state = Reducer::reduce(harness.store()).unwrap();
    assert!(state
        .lane("child")
        .unwrap()
        .tools
        .iter()
        .any(|tool| tool.completed));
    assert_eq!(
        harness.store().entry("child-tool-result").unwrap().lane,
        "child"
    );

    harness
        .append_entry_gated(threadlane_agent::harness::Entry {
            id: "child-assistant-2".into(),
            parent_id: Some("child-tool-result".into()),
            lane: "child".into(),
            seq: 9,
            timestamp: 9,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            terminate: false,
        })
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .start_tool_batch(
            "child-run",
            "child-assistant-2",
            &[ToolSpec {
                index: 0,
                call_id: "call-child-2".into(),
                name: "read_file".into(),
                effective_args: serde_json::json!({"path":"Cargo.toml"}),
                result_entry_id: "child-tool-result-2".into(),
                replay: ToolReplaySafety::Safe,
            }],
        )
        .unwrap();
}

#[test]
fn compaction_acceptance_targets_the_requested_lane() {
    let mut store = MemoryStore::new("session-1");
    let root = store.append_message(None, AgentMessage::user("parent", vec![]));
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "child-leaf".into(),
            parent_id: Some(root),
            lane: "child".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("child", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut harness = AgentHarness::new(store);
    harness
        .accept_compaction_on_lane("child", "compact-child", "retained context")
        .unwrap();
    harness.drive_to_completion().unwrap();
    let state = Reducer::reduce(harness.store()).unwrap();
    assert_eq!(state.lane("child").unwrap().status, LaneStatus::Completed);
    assert!(harness.store().entries().iter().any(|entry| {
        entry.lane == "child"
            && matches!(
                &entry.message,
                AgentMessage::Custom { custom_type, .. } if custom_type == "compaction_summary"
            )
    }));
}

#[test]
fn navigation_acceptance_targets_the_requested_lane() {
    let mut store = MemoryStore::new("session-1");
    let root = store.append_message(None, AgentMessage::user("parent", vec![]));
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "child-leaf".into(),
            parent_id: Some(root.clone()),
            lane: "child".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("child", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut harness = AgentHarness::new(store);
    harness
        .accept_navigation_on_lane(
            "child",
            "navigate-child",
            &root,
            Some("returned to parent".into()),
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    let state = Reducer::reduce(harness.store()).unwrap();
    assert_eq!(state.lane("child").unwrap().status, LaneStatus::Completed);
    assert_eq!(
        state.lane("child").unwrap().leaf_id.as_deref(),
        Some("navigation-navigate-child-summary")
    );
}

#[test]
fn reduction_is_a_fixed_point_and_rejects_duplicate_record_ids() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    let first = Reducer::reduce(&store).unwrap();
    assert_eq!(first, Reducer::reduce(&store).unwrap());

    let error = store
        .try_append_record(Record::OperationStarted {
            id: "run-1".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 2,
            source_leaf_id: None,
            intent: OperationIntent::Run,
        })
        .unwrap_err();
    assert!(matches!(error, ReduceError::DuplicateId(_)));
}

#[test]
fn reducer_matches_tool_completion_by_provisioned_result_id() {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "assistant-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            terminate: false,
        })
        .unwrap();
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        source_leaf_id: Some("assistant-1".into()),
        intent: OperationIntent::Run,
    });
    store.append_record(Record::ToolStarted {
        id: "tool-1".into(),
        seq: 3,
        lane: "main".into(),
        timestamp: 3,
        run_id: "run-1".into(),
        assistant_entry_id: "assistant-1".into(),
        tool_index: 0,
        tool_call_id: "call-1".into(),
        tool_name: "list_dir".into(),
        effective_args: serde_json::json!({}),
        result_entry_id: "result-1".into(),
        replay: ToolReplaySafety::Never,
    });
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "result-1".into(),
            parent_id: Some("assistant-1".into()),
            lane: "main".into(),
            seq: 4,
            timestamp: 4,
            message: AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "list_dir".into(),
                content: "done".into(),
                is_error: false,
                terminate: true,
            },
            terminate: true,
        })
        .unwrap();
    store.append_record_unchecked(Record::ToolFinished {
        id: "tool-finished-1".into(),
        seq: 5,
        lane: "main".into(),
        timestamp: 5,
        run_id: "run-1".into(),
        tool_call_id: "call-1".into(),
        result_entry_id: "result-1".into(),
        terminate: true,
    });

    let lane = Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .clone();
    assert_eq!(lane.tools.len(), 1);
    assert!(lane.tools[0].completed);
    assert!(lane.tools[0].terminate);
}

#[test]
fn queued_input_is_reduced_by_its_provisioned_entry_identity() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::QueueEnqueued {
        id: "queue-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: Some("run-1".into()),
        queue: QueueKind::FollowUp,
        priority: None,
        target: ProvisionedEntry {
            id: "entry-queued-1".into(),
            parent_id: None,
            message: AgentMessage::user("follow up", vec![]),
        },
    });
    let reduced = Reducer::reduce(&store).unwrap();
    let lane = reduced.lane("main").unwrap();
    assert_eq!(lane.queued[0].target.id, "entry-queued-1");
}

#[test]
fn queue_cancellation_of_unknown_entry_is_corruption() {
    let mut store = MemoryStore::new("session-1");
    store.append_record_unchecked(Record::QueueCancelled {
        id: "cancel-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        entry_id: "missing-entry".into(),
    });
    assert!(matches!(
        Reducer::reduce(&store),
        Err(ReduceError::InvalidRecord(_))
    ));
}

#[test]
fn tool_finish_without_persisted_result_is_corruption() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    store.append_record_unchecked(Record::ToolStarted {
        id: "tool-1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        run_id: "run-1".into(),
        assistant_entry_id: "assistant-1".into(),
        tool_index: 0,
        tool_call_id: "call-1".into(),
        tool_name: "read_file".into(),
        effective_args: serde_json::json!({}),
        result_entry_id: "result-1".into(),
        replay: ToolReplaySafety::Safe,
    });
    store.append_record_unchecked(Record::ToolFinished {
        id: "tool-finished-1".into(),
        seq: 3,
        lane: "main".into(),
        timestamp: 3,
        run_id: "run-1".into(),
        tool_call_id: "call-1".into(),
        result_entry_id: "result-1".into(),
        terminate: false,
    });
    assert!(matches!(
        Reducer::reduce(&store),
        Err(ReduceError::InvalidRecord(_))
    ));
}
