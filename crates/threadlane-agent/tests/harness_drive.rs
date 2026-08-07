use std::sync::{Arc, Mutex};
use threadlane_agent::harness::{
    AbortProcedure, AgentHarness, DeferredProcedure, DeferredResolution, GatedEffects, LaneStatus,
    MemoryStore, NoToolRun, OperationIntent, PromptProcedure, ProvisionedEntry, QueueKind,
    QueueProcedure, Record, Reducer, RetryPolicy, UsageCause,
};
use threadlane_agent::AgentMessage;
use threadlane_agent::TokenUsage;

struct RecordingTelemetry(Arc<Mutex<Vec<(String, std::collections::BTreeMap<String, String>)>>>);

impl threadlane_agent::harness::TelemetrySink for RecordingTelemetry {
    fn event(&self, name: &str, context: &threadlane_agent::harness::ExecutionContext) {
        self.0
            .lock()
            .unwrap()
            .push((name.into(), context.attributes().clone()));
    }
}

fn assistant(text: &str) -> AgentMessage {
    AgentMessage::Assistant {
        content: Some(text.into()),
        tool_calls: None,
        stop_reason: Some("stop".into()),
        deferred_handle: None,
    }
}

#[test]
fn no_tool_run_is_parked_without_writes_then_completes() {
    let mut store = MemoryStore::new("session-1");
    let mut effects = GatedEffects::new();
    NoToolRun::accept(&store, "run-1", "hello", assistant("hi"), &mut effects).unwrap();

    assert!(store.entries().is_empty());
    assert!(store.records().is_empty());
    effects.run_to_completion(&mut store).unwrap();

    let lane = Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .clone();
    assert_eq!(lane.status, LaneStatus::Completed);
    assert!(lane.open_operation.is_none());
    assert_eq!(store.entries().len(), 2);
}

#[test]
fn a_lane_rejects_a_second_prompt_while_the_first_mutation_is_parked() {
    let store = MemoryStore::new("session-1");
    let mut effects = GatedEffects::new();
    PromptProcedure::accept(
        &store,
        "run-1",
        AgentMessage::user("one", vec![]),
        &mut effects,
    )
    .unwrap();

    let error = PromptProcedure::accept(
        &store,
        "run-2",
        AgentMessage::user("two", vec![]),
        &mut effects,
    )
    .unwrap_err();
    assert!(format!("{error:?}").contains("uncommitted mutation"));
}

#[test]
fn a_new_child_lane_can_accept_a_prompt_through_the_harness() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness
        .accept_prompt_on_lane("child", "run-child", AgentMessage::user("task", vec![]))
        .unwrap();
    harness.drive_to_completion().unwrap();
    assert_eq!(
        Reducer::reduce(harness.store())
            .unwrap()
            .lane("child")
            .unwrap()
            .open_operation
            .as_deref(),
        Some("run-child")
    );
}

#[test]
fn retry_is_durable_with_capped_exponential_backoff() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness
        .start_operation("run-1", None, OperationIntent::Run)
        .unwrap();
    harness.drive_to_completion().unwrap();

    let policy = RetryPolicy {
        max_attempts: 3,
        base_delay: 10,
        max_delay: 15,
    };
    assert_eq!(
        harness
            .schedule_retry("run-1", "transient", policy)
            .unwrap(),
        1
    );
    harness.drive_to_completion().unwrap();
    let retry = harness
        .snapshot()
        .unwrap()
        .state
        .lane("main")
        .unwrap()
        .retry
        .clone()
        .unwrap();
    assert_eq!(retry.attempt, 1);
    assert_eq!(retry.retry_at, 12);

    assert!(harness
        .schedule_retry("run-1", "duplicate", policy)
        .is_err());
    assert_eq!(harness.begin_retry("run-1").unwrap(), 1);
    harness.drive_to_completion().unwrap();
    assert_eq!(
        harness
            .snapshot()
            .unwrap()
            .state
            .lane("main")
            .unwrap()
            .attempts,
        1
    );
    assert_eq!(
        harness
            .schedule_retry("run-1", "transient-again", policy)
            .unwrap(),
        2
    );
}

#[test]
fn failed_provider_usage_advances_the_next_retry_attempt() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness
        .start_operation("run-1", None, OperationIntent::Run)
        .unwrap();
    harness.drive_to_completion().unwrap();
    assert_eq!(
        harness
            .record_provider_usage("run-1", TokenUsage::default())
            .unwrap(),
        1
    );
    harness.drive_to_completion().unwrap();
    assert_eq!(
        harness
            .schedule_retry(
                "run-1",
                "timeout",
                RetryPolicy {
                    max_attempts: 3,
                    base_delay: 1,
                    max_delay: 2
                }
            )
            .unwrap(),
        2
    );
    harness.drive_to_completion().unwrap();
    assert_eq!(
        harness
            .snapshot()
            .unwrap()
            .state
            .lane("main")
            .unwrap()
            .retry
            .as_ref()
            .unwrap()
            .attempt,
        2
    );
    assert_eq!(
        harness
            .store()
            .records()
            .iter()
            .filter(|record| matches!(record, Record::Usage { .. }))
            .count(),
        1
    );
}

#[test]
fn usage_ledger_accepts_discarded_requests_and_adjustments() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness
        .start_operation("run-1", None, OperationIntent::Run)
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .record_discarded_usage(
            "run-1",
            TokenUsage {
                total_tokens: 3,
                ..TokenUsage::default()
            },
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .record_usage_adjustment(
            "run-1",
            TokenUsage {
                total_tokens: 2,
                ..TokenUsage::default()
            },
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    let usage = harness
        .snapshot()
        .unwrap()
        .state
        .lane("main")
        .unwrap()
        .usage
        .clone();
    assert_eq!(usage.total_tokens, 5);
    assert!(harness.store().records().iter().any(|record| matches!(
        record,
        Record::Usage {
            cause: UsageCause::Discarded,
            ..
        }
    )));
    assert!(harness.store().records().iter().any(|record| matches!(
        record,
        Record::Usage {
            cause: UsageCause::Adjustment,
            ..
        }
    )));
}

#[test]
fn provider_usage_commits_before_assistant_finalization_without_double_counting() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness
        .accept_prompt("run-1", AgentMessage::user("hello", vec![]))
        .unwrap();
    harness.drive_to_completion().unwrap();
    let seq = harness
        .store()
        .entries()
        .iter()
        .map(|entry| entry.seq)
        .chain(harness.store().records().iter().map(Record::seq))
        .max()
        .unwrap_or(0)
        + 1;
    harness
        .append_entry_gated(threadlane_agent::harness::Entry {
            id: "entry-run-1-assistant-1".into(),
            parent_id: Some("entry-run-1-user".into()),
            lane: "main".into(),
            seq,
            timestamp: seq,
            message: assistant("done"),
            terminate: false,
        })
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .record_provider_usage(
            "run-1",
            TokenUsage {
                total_tokens: 7,
                ..TokenUsage::default()
            },
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .finish_assistant_attempt("run-1", "entry-run-1-assistant-1", TokenUsage::default())
        .unwrap();
    harness.drive_to_completion().unwrap();
    assert_eq!(
        harness
            .store()
            .records()
            .iter()
            .filter(|record| matches!(record, Record::Usage { .. }))
            .count(),
        1
    );
    assert_eq!(
        harness
            .store()
            .records()
            .iter()
            .filter(|record| matches!(record, Record::StepAttempt { .. }))
            .count(),
        1
    );
}

#[test]
fn provider_usage_records_each_request_in_one_attempt() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness
        .start_operation("run-1", None, OperationIntent::Run)
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .record_provider_usage(
            "run-1",
            TokenUsage {
                total_tokens: 2,
                ..TokenUsage::default()
            },
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .record_provider_usage(
            "run-1",
            TokenUsage {
                total_tokens: 3,
                ..TokenUsage::default()
            },
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    assert_eq!(
        harness
            .store()
            .records()
            .iter()
            .filter(|record| matches!(
                record,
                Record::Usage {
                    cause: UsageCause::Provider,
                    ..
                }
            ))
            .count(),
        2
    );
    assert_eq!(
        harness
            .snapshot()
            .unwrap()
            .state
            .lane("main")
            .unwrap()
            .usage
            .total_tokens,
        5
    );
}

#[test]
fn a_consumed_retry_finishes_with_the_consumed_attempt_number() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness
        .start_operation("run-1", None, OperationIntent::Run)
        .unwrap();
    harness.drive_to_completion().unwrap();
    let policy = RetryPolicy {
        max_attempts: 3,
        base_delay: 1,
        max_delay: 2,
    };
    assert_eq!(
        harness.schedule_retry("run-1", "timeout", policy).unwrap(),
        1
    );
    harness.drive_to_completion().unwrap();
    assert_eq!(harness.begin_retry("run-1").unwrap(), 1);
    harness.drive_to_completion().unwrap();
    let seq = harness
        .store()
        .entries()
        .iter()
        .map(|entry| entry.seq)
        .chain(harness.store().records().iter().map(Record::seq))
        .max()
        .unwrap_or(0)
        + 1;
    harness
        .append_entry_gated(threadlane_agent::harness::Entry {
            id: "assistant-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq,
            timestamp: seq,
            message: assistant("retried"),
            terminate: false,
        })
        .unwrap();
    harness.drive_to_completion().unwrap();
    harness
        .finish_assistant_attempt("run-1", "assistant-1", TokenUsage::default())
        .unwrap();
    harness.drive_to_completion().unwrap();
    assert_eq!(
        harness
            .snapshot()
            .unwrap()
            .state
            .lane("main")
            .unwrap()
            .attempts,
        1
    );
}

#[test]
fn lane_facts_are_durable_and_reduced_after_commit() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness.set_fact("main", "model", "gpt-test", None).unwrap();
    assert!(harness.store().records().is_empty());
    harness.drive_to_completion().unwrap();
    assert_eq!(
        harness
            .snapshot()
            .unwrap()
            .state
            .lane("main")
            .unwrap()
            .facts["model"],
        "gpt-test"
    );
}

#[test]
fn queued_facts_reserve_distinct_sequences() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness.set_fact("main", "model", "gpt-test", None).unwrap();
    harness.set_fact("main", "effort", "high", None).unwrap();
    harness.drive_to_completion().unwrap();
    let records = harness.store().records();
    assert_eq!(records.len(), 2);
    assert!(records[0].seq() < records[1].seq());
}

#[test]
fn agent_harness_owns_manual_drive_and_snapshot() {
    let mut harness = AgentHarness::new(MemoryStore::new("session-1"));
    harness
        .accept_no_tool_run("run-1", "hello", assistant("hi"))
        .unwrap();
    assert!(harness.peek_action().is_some());
    let mut subscription = harness.subscribe().unwrap();
    harness.drive_to_completion().unwrap();
    assert_eq!(harness.snapshot().unwrap().entries.len(), 2);
    assert!(!harness.events().poll(&mut subscription).unwrap().is_empty());
}

#[test]
fn telemetry_runs_only_after_effect_commit_and_contains_safe_identity() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let telemetry = Arc::new(RecordingTelemetry(events.clone()));
    let mut harness = AgentHarness::with_telemetry(
        MemoryStore::new("session-1"),
        threadlane_agent::harness::HarnessEventHub::new(8),
        telemetry,
    );
    harness
        .accept_no_tool_run("run-1", "secret prompt", assistant("secret response"))
        .unwrap();
    assert!(events.lock().unwrap().is_empty());
    harness.drive_to_completion().unwrap();

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 5);
    assert!(events.iter().all(|(name, attrs)| {
        name == "effect_committed"
            && attrs.contains_key("effect")
            && attrs.contains_key("effect_id")
            && !attrs.values().any(|value| value.contains("secret"))
    }));
}

#[test]
fn telemetry_context_drops_sensitive_default_attributes() {
    let mut context = threadlane_agent::harness::ExecutionContext::default();
    context.set_attribute("prompt", "private");
    context.set_attribute("tool_arguments", "private");
    context.set_attribute("effect_id", "safe");
    assert_eq!(context.attributes().len(), 1);
    assert_eq!(context.attributes().get("effect_id"), Some(&"safe".into()));
}

#[test]
fn no_tool_run_resume_completes_a_partial_prefix() {
    let mut store = MemoryStore::new("session-1");
    let mut parked = GatedEffects::new();
    NoToolRun::accept(&store, "run-1", "hello", assistant("hi"), &mut parked).unwrap();
    for _ in 0..2 {
        let id = parked.peek_action().unwrap().id().to_owned();
        parked.execute_action(&mut store, &id).unwrap();
    }

    let mut resumed = GatedEffects::new();
    NoToolRun::resume(&store, "run-1", "hello", assistant("hi"), &mut resumed).unwrap();
    resumed.run_to_completion(&mut store).unwrap();
    assert_eq!(
        Reducer::reduce(&store)
            .unwrap()
            .lane("main")
            .unwrap()
            .status,
        LaneStatus::Completed
    );
    assert_eq!(store.entries().len(), 2);
}

#[test]
fn manual_and_automatic_no_tool_drive_have_identical_durable_state() {
    let mut automatic = MemoryStore::new("automatic");
    let mut automatic_effects = GatedEffects::new();
    NoToolRun::accept(
        &automatic,
        "run-1",
        "hello",
        assistant("hi"),
        &mut automatic_effects,
    )
    .unwrap();
    automatic_effects.run_to_completion(&mut automatic).unwrap();

    let mut manual = MemoryStore::new("manual");
    let mut manual_effects = GatedEffects::new();
    NoToolRun::accept(
        &manual,
        "run-1",
        "hello",
        assistant("hi"),
        &mut manual_effects,
    )
    .unwrap();
    while let Some(id) = manual_effects
        .peek_action()
        .map(|action| action.id().to_owned())
    {
        manual_effects.execute_action(&mut manual, &id).unwrap();
    }

    assert_eq!(automatic.entries(), manual.entries());
    assert_eq!(automatic.records(), manual.records());
}

#[test]
fn failed_entry_append_does_not_reserve_identity_or_mutate_store() {
    let mut store = MemoryStore::new("session-1");
    let invalid = threadlane_agent::harness::Entry {
        id: "entry-1".into(),
        parent_id: Some("missing".into()),
        lane: "main".into(),
        seq: 1,
        timestamp: 1,
        message: AgentMessage::user("bad", vec![]),
        terminate: false,
    };
    assert!(store.try_append_entry(invalid).is_err());
    assert!(store.entries().is_empty());
    let valid = threadlane_agent::harness::Entry {
        id: "entry-1".into(),
        parent_id: None,
        lane: "main".into(),
        seq: 1,
        timestamp: 1,
        message: AgentMessage::user("good", vec![]),
        terminate: false,
    };
    store.try_append_entry(valid).unwrap();
    assert_eq!(store.entries().len(), 1);
}

#[test]
fn queue_acceptance_and_cancellation_are_durable_actions() {
    let mut store = MemoryStore::new("session-1");
    let mut effects = GatedEffects::new();
    let target = ProvisionedEntry {
        id: "entry-follow-up".into(),
        parent_id: None,
        message: AgentMessage::user("later", vec![]),
    };
    QueueProcedure::enqueue(&store, "run-1", QueueKind::FollowUp, target, &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();
    assert_eq!(
        Reducer::reduce(&store)
            .unwrap()
            .lane("main")
            .unwrap()
            .queued
            .len(),
        1
    );

    let mut cancel_effects = GatedEffects::new();
    assert!(
        QueueProcedure::cancel(&store, "wrong-run", "entry-follow-up", &mut cancel_effects)
            .is_err()
    );
    QueueProcedure::cancel(&store, "run-1", "entry-follow-up", &mut cancel_effects).unwrap();
    cancel_effects.run_to_completion(&mut store).unwrap();
    assert!(Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .queued
        .is_empty());
}

#[test]
fn queued_input_consumption_is_distinct_and_durable() {
    let mut store = MemoryStore::new("session-1");
    let target = ProvisionedEntry {
        id: "entry-follow-up".into(),
        parent_id: None,
        message: AgentMessage::user("later", vec![]),
    };
    let mut effects = GatedEffects::new();
    QueueProcedure::enqueue(&store, "run-1", QueueKind::FollowUp, target, &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();
    let mut consume = GatedEffects::new();
    QueueProcedure::consume(&store, "run-1", "entry-follow-up", &mut consume).unwrap();
    consume.run_to_completion(&mut store).unwrap();
    assert!(Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .queued
        .is_empty());
    assert!(matches!(
        store.records().last(),
        Some(Record::QueueConsumed { .. })
    ));
}

#[test]
fn abort_request_commits_before_reconciliation() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    let mut effects = GatedEffects::new();
    AbortProcedure::request(&store, "run-1", &mut effects).unwrap();
    assert_eq!(store.records().len(), 1);
    effects.run_to_completion(&mut store).unwrap();
    let lane = Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .clone();
    assert!(lane.abort_requested);
    assert_eq!(lane.open_operation.as_deref(), Some("run-1"));
}

#[test]
fn deferred_write_is_accepted_with_its_provisioned_entry() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    let mut effects = GatedEffects::new();
    DeferredProcedure::enqueue(
        &store,
        "run-1",
        ProvisionedEntry {
            id: "entry-deferred-1".into(),
            parent_id: None,
            message: AgentMessage::user("deferred", vec![]),
        },
        &mut effects,
    )
    .unwrap();
    effects.run_to_completion(&mut store).unwrap();
    let reduced = Reducer::reduce(&store).unwrap();
    assert_eq!(
        reduced.lane("main").unwrap().deferred_writes[0].id,
        "entry-deferred-1"
    );
}

#[test]
fn deferred_write_materializes_once_and_is_marked_applied() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    let target = ProvisionedEntry {
        id: "entry-deferred-1".into(),
        parent_id: None,
        message: AgentMessage::user("deferred", vec![]),
    };
    let mut effects = GatedEffects::new();
    DeferredProcedure::enqueue(&store, "run-1", target, &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();

    let mut effects = GatedEffects::new();
    DeferredProcedure::apply_pending(&store, "run-1", &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();
    let reduced = Reducer::reduce(&store).unwrap();
    assert!(reduced.lane("main").unwrap().deferred_writes.is_empty());
    assert_eq!(
        store
            .entries()
            .iter()
            .filter(|entry| entry.id == "entry-deferred-1")
            .count(),
        1
    );

    let mut effects = GatedEffects::new();
    DeferredProcedure::apply_pending(&store, "run-1", &mut effects).unwrap();
    assert!(effects.peek_action().is_none());
}

#[test]
fn deferred_write_uses_the_open_lane_leaf_not_a_sibling_tail() {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "main-root".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("main", vec![]),
            terminate: false,
        })
        .unwrap();
    store
        .try_append_entry(threadlane_agent::harness::Entry {
            id: "child-leaf".into(),
            parent_id: Some("main-root".into()),
            lane: "child".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("child", vec![]),
            terminate: false,
        })
        .unwrap();
    store.append_record(Record::OperationStarted {
        id: "child-run".into(),
        seq: 3,
        lane: "child".into(),
        timestamp: 3,
        source_leaf_id: Some("child-leaf".into()),
        intent: OperationIntent::Run,
    });

    let mut effects = GatedEffects::new();
    DeferredProcedure::enqueue(
        &store,
        "child-run",
        ProvisionedEntry {
            id: "child-deferred".into(),
            parent_id: None,
            message: AgentMessage::user("deferred", vec![]),
        },
        &mut effects,
    )
    .unwrap();
    effects.run_to_completion(&mut store).unwrap();

    let mut effects = GatedEffects::new();
    DeferredProcedure::apply_pending(&store, "child-run", &mut effects).unwrap();
    effects.run_to_completion(&mut store).unwrap();
    assert_eq!(
        store
            .entries()
            .iter()
            .find(|entry| entry.id == "child-deferred")
            .unwrap()
            .parent_id
            .as_deref(),
        Some("child-leaf")
    );
}

#[test]
fn deferred_assistant_entry_suspends_the_lane_without_finishing_it() {
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    let mut effects = GatedEffects::new();
    DeferredProcedure::suspend(
        &store,
        "run-1",
        threadlane_agent::harness::Entry {
            id: "deferred-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: None,
                stop_reason: None,
                deferred_handle: Some(threadlane_agent::DeferredHandle {
                    handle_id: "h-1".into(),
                    provider: "test".into(),
                    model: "test-model".into(),
                }),
            },
            terminate: false,
        },
        &mut effects,
    )
    .unwrap();
    effects.run_to_completion(&mut store).unwrap();
    let lane = Reducer::reduce(&store)
        .unwrap()
        .lane("main")
        .unwrap()
        .clone();
    assert_eq!(lane.status, LaneStatus::SuspendedDeferred);
    assert_eq!(lane.open_operation.as_deref(), Some("run-1"));
}

#[test]
fn deferred_redemption_requires_the_same_handle_and_persists_terminal_result() {
    let handle = threadlane_agent::DeferredHandle {
        handle_id: "h-1".into(),
        provider: "test".into(),
        model: "test-model".into(),
    };
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    let mut suspend = GatedEffects::new();
    DeferredProcedure::suspend(
        &store,
        "run-1",
        threadlane_agent::harness::Entry {
            id: "deferred-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: None,
                stop_reason: None,
                deferred_handle: Some(handle.clone()),
            },
            terminate: false,
        },
        &mut suspend,
    )
    .unwrap();
    suspend.run_to_completion(&mut store).unwrap();

    let mut pending = GatedEffects::new();
    assert!(!DeferredProcedure::redeem(
        &store,
        "run-1",
        DeferredResolution::Pending(handle.clone()),
        &mut pending,
    )
    .unwrap());
    assert!(pending.peek_action().is_some());
    pending.run_to_completion(&mut store).unwrap();

    let mut ready = GatedEffects::new();
    assert!(DeferredProcedure::redeem(
        &store,
        "run-1",
        DeferredResolution::Ready(assistant("ready")),
        &mut ready,
    )
    .unwrap());
    ready.run_to_completion(&mut store).unwrap();
    assert_eq!(
        Reducer::reduce(&store)
            .unwrap()
            .lane("main")
            .unwrap()
            .status,
        LaneStatus::SuspendedCrash
    );
    let finish_seq = store
        .entries()
        .iter()
        .map(|entry| entry.seq)
        .chain(store.records().iter().map(Record::seq))
        .max()
        .unwrap()
        + 1;
    store.append_record(Record::OperationFinished {
        id: "finish-run-1".into(),
        seq: finish_seq,
        lane: "main".into(),
        timestamp: finish_seq,
        run_id: "run-1".into(),
        outcome: threadlane_agent::harness::OperationOutcome::Completed,
        error: None,
    });
    assert_eq!(
        Reducer::reduce(&store)
            .unwrap()
            .lane("main")
            .unwrap()
            .status,
        LaneStatus::Completed
    );
}
