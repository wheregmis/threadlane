use threadlane_agent::harness::{
    CompactionProcedure, Entry, GatedEffects, LaneStatus, MemoryStore, Record, Reducer,
};
use threadlane_agent::AgentMessage;

#[test]
fn compaction_appends_a_summary_without_rewriting_history() {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(Entry {
            id: "entry-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("old context", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut effects = GatedEffects::new();
    CompactionProcedure::accept(
        &store,
        "run-compaction-1",
        "architecture and verification notes",
        &mut effects,
    )
    .unwrap();
    assert_eq!(store.entries().len(), 1);
    effects.run_to_completion(&mut store).unwrap();

    let reduced = Reducer::reduce(&store).unwrap();
    assert_eq!(reduced.lane("main").unwrap().status, LaneStatus::Completed);
    assert_eq!(store.entries().len(), 2);
    assert_eq!(
        reduced.lane("main").unwrap().leaf_id.as_deref(),
        Some("compaction-run-compaction-1-summary")
    );
    assert!(store.entries()[1].parent_id.is_none());
    assert!(matches!(
        &store.entries()[1].message,
        AgentMessage::Custom { custom_type, .. } if custom_type == "compaction_summary"
    ));
    assert!(store.records().iter().any(|record| matches!(
        record,
        Record::StepAttempt { compaction_reason: Some(reason), .. } if reason == "manual"
    )));
}
