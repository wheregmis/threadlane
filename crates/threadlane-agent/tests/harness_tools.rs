use threadlane_agent::harness::{
    GatedEffects, MemoryStore, OperationIntent, Record, Reducer, ToolBatchProcedure, ToolRecovery,
    ToolReplaySafety, ToolResult, ToolSpec, UsageCause,
};
use threadlane_agent::AgentMessage;
use threadlane_agent::TokenUsage;
use threadlane_provider::openai::{ToolCall, ToolCallFunction};

fn open_run_with_assistant() -> MemoryStore {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "assistant-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            terminate: false,
        })
        .unwrap();
    store
}

fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            index: 0,
            call_id: "call-1".into(),
            name: "read_file".into(),
            effective_args: serde_json::json!({"path":"a"}),
            result_entry_id: "result-1".into(),
            replay: ToolReplaySafety::Safe,
        },
        ToolSpec {
            index: 1,
            call_id: "call-2".into(),
            name: "read_file".into(),
            effective_args: serde_json::json!({"path":"b"}),
            result_entry_id: "result-2".into(),
            replay: ToolReplaySafety::Never,
        },
    ]
}

#[test]
fn tool_intents_commit_in_source_order_before_results() {
    let mut store = open_run_with_assistant();
    let mut effects = GatedEffects::new();
    ToolBatchProcedure::start(&store, "run-1", "assistant-1", &specs(), &mut effects).unwrap();
    assert!(store
        .records()
        .iter()
        .all(|record| !matches!(record, Record::ToolStarted { .. })));
    effects.run_to_completion(&mut store).unwrap();
    let tool_calls: Vec<_> = store
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::ToolStarted { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_calls, ["call-1", "call-2"]);
}

#[test]
fn completed_tool_is_not_restarted_when_batch_resumes() {
    let mut store = open_run_with_assistant();
    let mut effects = GatedEffects::new();
    ToolBatchProcedure::start(&store, "run-1", "assistant-1", &specs(), &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();
    ToolBatchProcedure::finish(
        &store,
        "run-1",
        ToolResult {
            call_id: "call-1".into(),
            name: "read_file".into(),
            content: "a".into(),
            is_error: false,
            terminate: false,
        },
        &mut effects,
    )
    .unwrap();
    effects.run_to_completion(&mut store).unwrap();

    let record_count = store.records().len();
    let mut resume_effects = GatedEffects::new();
    ToolBatchProcedure::start(
        &store,
        "run-1",
        "assistant-1",
        &specs(),
        &mut resume_effects,
    )
    .unwrap();
    assert!(resume_effects.peek_action().is_none());
    ToolBatchProcedure::finish(
        &store,
        "run-1",
        ToolResult {
            call_id: "call-2".into(),
            name: "read_file".into(),
            content: "b".into(),
            is_error: false,
            terminate: true,
        },
        &mut resume_effects,
    )
    .unwrap();
    resume_effects.run_to_completion(&mut store).unwrap();
    assert_eq!(
        store
            .records()
            .iter()
            .filter(|record| matches!(record, Record::ToolStarted { .. }))
            .count(),
        2
    );
    assert!(store.records().len() > record_count);
}

#[test]
fn reducer_rejects_a_tool_intent_with_the_wrong_source_ordinal() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "assistant-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![
                    ToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        thought_signature: None,
                        function: ToolCallFunction {
                            name: "read_file".into(),
                            arguments: "{}".into(),
                        },
                    },
                    ToolCall {
                        id: "call-2".into(),
                        r#type: "function".into(),
                        thought_signature: None,
                        function: ToolCallFunction {
                            name: "read_file".into(),
                            arguments: "{}".into(),
                        },
                    },
                ]),
                stop_reason: None,
                deferred_handle: None,
            },
            terminate: false,
        })
        .unwrap();
    store.append_record_unchecked(Record::ToolStarted {
        id: "tool-1".into(),
        seq: 3,
        lane: "main".into(),
        timestamp: 3,
        run_id: "run-1".into(),
        assistant_entry_id: "assistant-1".into(),
        tool_index: 1,
        tool_call_id: "call-1".into(),
        tool_name: "read_file".into(),
        effective_args: serde_json::json!({}),
        result_entry_id: "result-1".into(),
        replay: ToolReplaySafety::Never,
    });
    assert!(Reducer::reduce(&store).is_err());
}

#[test]
fn unsafe_unfinished_tool_is_synthesized_and_safe_tool_is_replayed() {
    let mut store = open_run_with_assistant();
    let mut effects = GatedEffects::new();
    ToolBatchProcedure::start(&store, "run-1", "assistant-1", &specs(), &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();

    let recoveries =
        ToolBatchProcedure::resume(&store, "run-1", "assistant-1", &specs(), &mut effects).unwrap();
    assert!(matches!(
        recoveries[0],
        ToolRecovery::Replay(ref spec) if spec.call_id == "call-1"
    ));
    assert!(matches!(
        recoveries[1],
        ToolRecovery::Synthesized(ref result) if result.call_id == "call-2"
    ));
    effects.run_to_completion(&mut store).unwrap();
    assert!(store.records().iter().any(|record| matches!(
        record,
        Record::Usage {
            cause: UsageCause::Replay,
            tool_call_id: Some(tool_call_id),
            ..
        } if tool_call_id == "call-1"
    )));
    let lane = threadlane_agent::harness::Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .clone();
    assert!(!lane.tools[0].completed);
    assert!(lane.tools[1].completed);
}

#[test]
fn tool_completion_can_persist_usage_for_the_physical_execution() {
    let mut store = open_run_with_assistant();
    let mut effects = GatedEffects::new();
    ToolBatchProcedure::start(&store, "run-1", "assistant-1", &specs(), &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();
    ToolBatchProcedure::finish_with_usage(
        &store,
        "run-1",
        ToolResult {
            call_id: "call-1".into(),
            name: "read_file".into(),
            content: "a".into(),
            is_error: false,
            terminate: false,
        },
        TokenUsage {
            input_tokens: 2,
            output_tokens: 3,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 5,
        },
        &mut effects,
    )
    .unwrap();
    effects.run_to_completion(&mut store).unwrap();
    assert_eq!(
        Reducer::reduce(&store)
            .unwrap()
            .lane("main")
            .unwrap()
            .usage
            .total_tokens,
        5
    );
}

#[test]
fn batch_finalization_reserves_source_order_sequences_before_drive() {
    let mut store = open_run_with_assistant();
    let mut effects = GatedEffects::new();
    ToolBatchProcedure::start(&store, "run-1", "assistant-1", &specs(), &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();
    ToolBatchProcedure::finish_batch(
        &store,
        "run-1",
        &[
            ToolResult {
                call_id: "call-1".into(),
                name: "read_file".into(),
                content: "a".into(),
                is_error: false,
                terminate: false,
            },
            ToolResult {
                call_id: "call-2".into(),
                name: "read_file".into(),
                content: "b".into(),
                is_error: false,
                terminate: true,
            },
        ],
        TokenUsage::default(),
        &mut effects,
    )
    .unwrap();
    effects.run_to_completion(&mut store).unwrap();
    let results: Vec<_> = store
        .entries()
        .iter()
        .filter_map(|entry| match &entry.message {
            AgentMessage::Tool { tool_call_id, .. } => Some((tool_call_id.clone(), entry.seq)),
            _ => None,
        })
        .collect();
    assert_eq!(results, [("call-1".into(), 5), ("call-2".into(), 8)]);
}
