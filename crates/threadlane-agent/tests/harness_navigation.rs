use threadlane_agent::harness::{
    Entry, GatedEffects, LaneStatus, MemoryStore, NavigationProcedure, Record, Reducer,
};
use threadlane_agent::AgentMessage;

fn store_with_two_entries() -> MemoryStore {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(Entry {
            id: "entry-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("one", vec![]),
            terminate: false,
        })
        .unwrap();
    store
        .try_append_entry(Entry {
            id: "entry-2".into(),
            parent_id: Some("entry-1".into()),
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("two", vec![]),
            terminate: false,
        })
        .unwrap();
    store
}

#[test]
fn navigation_moves_the_lane_before_appending_its_summary() {
    let mut store = store_with_two_entries();
    let mut effects = GatedEffects::new();
    NavigationProcedure::accept(
        &store,
        "run-navigation-1",
        "entry-1",
        Some("Returned to the first branch.".into()),
        &mut effects,
    )
    .unwrap();
    assert_eq!(store.entries().len(), 2);
    effects.run_to_completion(&mut store).unwrap();

    let reduced = Reducer::reduce(&store).unwrap();
    let lane = reduced.lane("main").unwrap();
    assert_eq!(lane.status, LaneStatus::Completed);
    assert!(lane.open_operation.is_none());
    assert_eq!(
        lane.leaf_id.as_deref(),
        Some("navigation-run-navigation-1-summary")
    );
    assert!(store.records().iter().any(|record| matches!(
        record,
        Record::LaneMoved { target_leaf_id, .. } if target_leaf_id == "entry-1"
    )));
}

#[test]
fn navigation_resume_after_move_only_appends_missing_summary_and_finish() {
    let mut store = store_with_two_entries();
    let mut first_effects = GatedEffects::new();
    NavigationProcedure::accept(
        &store,
        "run-navigation-1",
        "entry-1",
        Some("Returned to the first branch.".into()),
        &mut first_effects,
    )
    .unwrap();
    for _ in 0..2 {
        let id = first_effects.peek_action().unwrap().id().to_owned();
        first_effects.execute_action(&mut store, &id).unwrap();
    }
    assert_eq!(store.records().len(), 2);

    let mut resumed_effects = GatedEffects::new();
    NavigationProcedure::resume(
        &store,
        "run-navigation-1",
        "entry-1",
        Some("Returned to the first branch.".into()),
        &mut resumed_effects,
    )
    .unwrap();
    resumed_effects.run_to_completion(&mut store).unwrap();
    assert_eq!(
        store
            .records()
            .iter()
            .filter(|record| matches!(record, Record::LaneMoved { .. }))
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
