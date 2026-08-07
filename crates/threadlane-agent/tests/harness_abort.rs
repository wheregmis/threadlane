use threadlane_agent::harness::{
    AbortProcedure, Entry, GatedEffects, LaneStatus, MemoryStore, OperationIntent,
    ProvisionedEntry, QueueKind, Record, Reducer, ToolReplaySafety,
};
use threadlane_agent::AgentMessage;

fn store_with_interrupted_tool() -> MemoryStore {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(Entry {
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
        tool_name: "read_file".into(),
        effective_args: serde_json::json!({"path":"a"}),
        result_entry_id: "result-1".into(),
        replay: ToolReplaySafety::Never,
    });
    store.append_record(Record::AbortRequested {
        id: "abort-1".into(),
        seq: 4,
        lane: "main".into(),
        timestamp: 4,
        run_id: "run-1".into(),
    });
    store
}

#[test]
fn abort_reconciliation_is_parked_and_closes_the_run_after_commit() {
    let mut store = store_with_interrupted_tool();
    let mut effects = GatedEffects::new();
    AbortProcedure::reconcile(&store, "run-1", "assistant-1", &mut effects).unwrap();
    assert_eq!(store.entries().len(), 1);
    effects.run_to_completion(&mut store).unwrap();

    let reduced = Reducer::reduce(&store).unwrap();
    let lane = reduced.lane("main").unwrap();
    assert_eq!(lane.status, LaneStatus::Idle);
    assert!(lane.open_operation.is_none());
    assert!(!lane.abort_requested);
    assert!(lane.tools[0].completed);
    assert_eq!(store.entries().len(), 3);
    assert!(matches!(
        store.entries()[1].message,
        AgentMessage::Tool { is_error: true, .. }
    ));
    assert!(matches!(
        store.entries()[2].message,
        AgentMessage::Assistant { .. }
    ));
}

#[test]
fn abort_reconciliation_keeps_tool_results_on_their_lane() {
    let mut store = store_with_interrupted_tool();
    store
        .try_append_entry(Entry {
            id: "sibling-tail".into(),
            parent_id: None,
            lane: "sibling".into(),
            seq: 5,
            timestamp: 5,
            message: AgentMessage::user("other lane", Vec::new()),
            terminate: false,
        })
        .unwrap();
    let mut effects = GatedEffects::new();
    AbortProcedure::reconcile(&store, "run-1", "assistant-1", &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();
    assert_eq!(
        store
            .entries()
            .iter()
            .find(|entry| entry.id == "result-1")
            .and_then(|entry| entry.parent_id.as_deref()),
        Some("assistant-1")
    );
}

#[test]
fn abort_reconciliation_resumes_after_a_persisted_result_prefix() {
    let mut store = store_with_interrupted_tool();
    let mut first_effects = GatedEffects::new();
    AbortProcedure::reconcile(&store, "run-1", "assistant-1", &mut first_effects).unwrap();
    let first_id = first_effects.peek_action().unwrap().id().to_owned();
    first_effects.execute_action(&mut store, &first_id).unwrap();
    assert_eq!(store.entries().len(), 2);

    let mut resumed_effects = GatedEffects::new();
    AbortProcedure::reconcile(&store, "run-1", "assistant-1", &mut resumed_effects).unwrap();
    resumed_effects.run_to_completion(&mut store).unwrap();
    assert_eq!(
        store
            .entries()
            .iter()
            .filter(|entry| entry.id == "result-1")
            .count(),
        1
    );
    assert!(Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .open_operation
        .is_none());
}

#[test]
fn abort_reconciliation_can_materialize_a_missing_provisioned_result() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    store.append_record(Record::StepAttempt {
        id: "attempt-1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        run_id: "run-1".into(),
        attempt: 1,
        result_entry_id: "assistant-result-1".into(),
        compaction_reason: None,
    });
    store.append_record(Record::AbortRequested {
        id: "abort-1".into(),
        seq: 3,
        lane: "main".into(),
        timestamp: 3,
        run_id: "run-1".into(),
    });
    let mut effects = GatedEffects::new();
    AbortProcedure::reconcile(&store, "run-1", "assistant-result-1", &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();
    assert!(store.entries().iter().any(|entry| {
        entry.id == "assistant-result-1"
            && matches!(entry.message, AgentMessage::Assistant { stop_reason: Some(ref reason), .. } if reason == "aborted")
    }));
    assert!(Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .open_operation
        .is_none());
}

#[test]
fn abort_drains_steer_and_follow_up_but_preserves_next_run() {
    let mut store = store_with_interrupted_tool();
    for (id, seq, queue, run_id) in [
        ("steer-1", 5, QueueKind::Steer, Some("run-1")),
        ("follow-1", 6, QueueKind::FollowUp, Some("run-1")),
        ("next-1", 7, QueueKind::NextRun, None),
    ] {
        store.append_record(Record::QueueEnqueued {
            id: format!("queue-{id}"),
            seq,
            lane: "main".into(),
            timestamp: seq,
            run_id: run_id.map(str::to_owned),
            queue,
            priority: None,
            target: ProvisionedEntry {
                id: id.into(),
                parent_id: Some("assistant-1".into()),
                message: AgentMessage::user(id, Vec::new()),
            },
        });
    }
    let mut effects = GatedEffects::new();
    AbortProcedure::reconcile(&store, "run-1", "assistant-1", &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();

    let lane = Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .clone();
    assert_eq!(lane.queued.len(), 1);
    assert_eq!(lane.queued[0].target.id, "next-1");
}
