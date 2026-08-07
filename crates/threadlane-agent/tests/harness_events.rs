use threadlane_agent::harness::{
    AgentHarness, EffectAction, Entry, EventPayload, GatedEffects, HarnessEventHub, MemoryStore,
    NoToolRun, Record, SessionStore, Snapshot, StreamingState,
};
use threadlane_agent::AgentMessage;

#[test]
fn subscription_starts_with_snapshot_and_has_no_live_event_gap() {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(Entry {
            id: "entry-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("before", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut hub = HarnessEventHub::new(8);
    hub.publish(EventPayload::EntryCommitted(store.entries()[0].clone()));
    let mut subscription = hub.subscribe(&store).unwrap();
    assert_eq!(subscription.snapshot.entries.len(), 1);

    let mut effects = GatedEffects::new();
    NoToolRun::accept(
        &store,
        "run-1",
        "after",
        AgentMessage::Assistant {
            content: Some("done".into()),
            tool_calls: None,
            stop_reason: Some("stop".into()),
            deferred_handle: None,
        },
        &mut effects,
    )
    .unwrap();
    let _ = effects;
    hub.publish(EventPayload::Fault("test event".into()));
    let events = hub.poll(&mut subscription).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].payload, EventPayload::Fault(_)));
}

#[test]
fn snapshot_reduces_from_the_same_store_prefix() {
    let mut store = MemoryStore::new("session-1");
    store.append_message(None, AgentMessage::user("hello", vec![]));
    let snapshot = Snapshot::from_store(&store).unwrap();
    assert_eq!(snapshot.session_id, "session-1");
    assert_eq!(snapshot.entries, store.entries());
    assert_eq!(
        snapshot.state.lane("main").unwrap().leaf_id.as_deref(),
        Some("entry_1")
    );
}

#[test]
fn streaming_state_is_in_snapshot_and_buffered_live_events() {
    let store = MemoryStore::new("session-1");
    let hub = HarnessEventHub::new(8);
    hub.publish_streaming(Some(StreamingState {
        lane: "main".into(),
        run_id: Some("run-1".into()),
        assistant_text: "partial".into(),
        ..Default::default()
    }));
    let mut subscription = hub.subscribe(&store).unwrap();
    assert_eq!(
        subscription
            .snapshot
            .streaming
            .as_ref()
            .map(|state| state.assistant_text.as_str()),
        Some("partial")
    );
    hub.publish_streaming(None);
    let events = hub.poll(&mut subscription).unwrap();
    assert!(matches!(events[0].payload, EventPayload::Streaming(None)));
    assert!(hub.subscribe(&store).unwrap().snapshot.streaming.is_none());
}

#[test]
fn harness_snapshot_includes_current_streaming_state() {
    let hub = HarnessEventHub::new(8);
    let harness = AgentHarness::with_events(MemoryStore::new("session-1"), hub.clone());
    hub.publish_streaming(Some(StreamingState {
        lane: "main".into(),
        run_id: Some("run-1".into()),
        assistant_text: "partial".into(),
        ..Default::default()
    }));

    assert_eq!(
        harness
            .snapshot()
            .unwrap()
            .streaming
            .as_ref()
            .map(|state| state.assistant_text.as_str()),
        Some("partial")
    );
}

#[test]
fn slow_subscriber_receives_an_explicit_gap_instead_of_silent_loss() {
    let store = MemoryStore::new("session-1");
    let mut hub = HarnessEventHub::new(2);
    let mut subscription = hub.subscribe(&store).unwrap();
    hub.publish(EventPayload::Fault("one".into()));
    hub.publish(EventPayload::Fault("two".into()));
    hub.publish(EventPayload::Fault("three".into()));
    assert!(matches!(
        hub.poll(&mut subscription),
        Err(threadlane_agent::harness::EventError::Gap { .. })
    ));
}

#[test]
fn identified_events_preserve_lane_run_and_recovery_identity() {
    let mut hub = HarnessEventHub::new(2);
    let event = hub.publish_identified(
        EventPayload::Fault("paused".into()),
        Some("main".into()),
        Some("run-1".into()),
        Some("recovery-1".into()),
    );
    assert_eq!(event.lane.as_deref(), Some("main"));
    assert_eq!(event.run_id.as_deref(), Some("run-1"));
    assert_eq!(event.turn, None);
    assert_eq!(event.recovery_id.as_deref(), Some("recovery-1"));
}

#[test]
fn committed_attempt_events_include_turn_identity() {
    let hub = HarnessEventHub::new(8);
    let mut harness = AgentHarness::with_events(MemoryStore::new("session-1"), hub.clone());
    harness
        .start_operation(
            "run-1",
            None,
            threadlane_agent::harness::OperationIntent::Run,
        )
        .unwrap();
    harness.drive_to_completion().unwrap();
    let mut subscription = hub.subscribe(harness.store()).unwrap();
    harness
        .append_record_gated(Record::StepAttempt {
            id: "attempt-1".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 2,
            run_id: "run-1".into(),
            attempt: 1,
            result_entry_id: "entry-1".into(),
            compaction_reason: None,
        })
        .unwrap();
    harness.drive_to_completion().unwrap();
    let event = hub.poll(&mut subscription).unwrap().last().unwrap().clone();
    assert_eq!(event.turn, Some(1));
}

#[test]
fn direct_entry_commit_events_preserve_the_entry_lane() {
    let hub = HarnessEventHub::new(8);
    let mut harness = AgentHarness::with_events(MemoryStore::new("session-1"), hub.clone());
    let store = MemoryStore::new("session-1");
    let mut subscription = hub.subscribe(&store).unwrap();

    harness
        .append_entry(Entry {
            id: "child-entry".into(),
            parent_id: None,
            lane: "child@1".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("child", vec![]),
            terminate: false,
        })
        .unwrap();

    let event = hub.poll(&mut subscription).unwrap().pop().unwrap();
    assert_eq!(event.lane.as_deref(), Some("child@1"));
}

#[test]
fn effects_publish_commit_events_only_after_store_success() {
    let mut store = MemoryStore::new("session-1");
    let mut effects = GatedEffects::new();
    effects
        .park(EffectAction::AppendEntry {
            entry: Entry {
                id: "entry-1".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::user("committed", vec![]),
                terminate: false,
            },
        })
        .unwrap();
    let mut hub = HarnessEventHub::new(8);
    let mut subscription = hub.subscribe(&store).unwrap();
    effects
        .run_to_completion_with_events(&mut store, &mut hub)
        .unwrap();
    assert_eq!(store.entries().len(), 1);
    let events = hub.poll(&mut subscription).unwrap();
    assert!(matches!(events[0].payload, EventPayload::EntryCommitted(_)));
}

#[test]
fn cloned_event_hubs_share_commits_across_harness_adapters() {
    let hub = HarnessEventHub::new(8);
    let mut first = AgentHarness::with_events(MemoryStore::new("session-1"), hub.clone());
    let mut second = AgentHarness::with_events(MemoryStore::new("session-1"), hub.clone());
    let store = MemoryStore::new("session-1");
    let mut subscription = hub.subscribe(&store).unwrap();

    first
        .append_entry_gated(Entry {
            id: "entry-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("first", vec![]),
            terminate: false,
        })
        .unwrap();
    first.drive_to_completion().unwrap();
    second
        .append_record_gated(threadlane_agent::harness::Record::FactSet {
            id: "fact-1".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            run_id: None,
            key: "model".into(),
            value: "test".into(),
        })
        .unwrap();
    second.drive_to_completion().unwrap();

    let events = hub.poll(&mut subscription).unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0].payload, EventPayload::EntryCommitted(_)));
    assert!(matches!(
        events[1].payload,
        EventPayload::RecordCommitted(_)
    ));
}
