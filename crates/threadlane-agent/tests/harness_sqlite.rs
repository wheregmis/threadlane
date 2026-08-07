use threadlane_agent::harness::{
    Entry, LaneStatus, OperationIntent, Record, Reducer, SessionStore, SqliteStore,
};
use threadlane_agent::AgentMessage;

#[test]
fn sqlite_store_round_trips_interleaved_entries_and_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.sqlite");
    let mut store = SqliteStore::open(&path, "session-1").unwrap();
    store
        .append_entry(Entry {
            id: "user-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("hello", vec![]),
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-1".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 2,
            source_leaf_id: Some("user-1".into()),
            intent: OperationIntent::Run,
        })
        .unwrap();
    store
        .append_entry(Entry {
            id: "assistant-1".into(),
            parent_id: Some("user-1".into()),
            lane: "main".into(),
            seq: 3,
            timestamp: 3,
            message: AgentMessage::Assistant {
                content: Some("hi".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::OperationFinished {
            id: "finish-1".into(),
            seq: 4,
            lane: "main".into(),
            timestamp: 4,
            run_id: "run-1".into(),
            outcome: threadlane_agent::harness::OperationOutcome::Completed,
            error: None,
        })
        .unwrap();

    let reopened = SqliteStore::open(&path, "session-1").unwrap();
    assert_eq!(reopened.entries().len(), 2);
    assert_eq!(reopened.records().len(), 2);
    assert_eq!(
        Reducer::reduce(&reopened)
            .unwrap()
            .lane("main")
            .unwrap()
            .status,
        LaneStatus::Completed
    );
}

#[test]
fn sqlite_store_rejects_invalid_appends_without_mutating() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.sqlite");
    let mut store = SqliteStore::open(&path, "session-1").unwrap();
    assert!(store
        .append_entry(Entry {
            id: "bad".into(),
            parent_id: Some("missing".into()),
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("bad", vec![]),
            terminate: false,
        })
        .is_err());
    assert!(store.entries().is_empty());
    let reopened = SqliteStore::open(&path, "session-1").unwrap();
    assert!(reopened.entries().is_empty());
}

#[test]
fn sqlite_forks_copy_entries_but_not_operation_records() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.sqlite");
    let mut source = SqliteStore::open(&source_path, "source").unwrap();
    for (id, parent, seq) in [("root", None, 1), ("child", Some("root"), 2)] {
        source
            .append_entry(Entry {
                id: id.into(),
                parent_id: parent.map(str::to_owned),
                lane: "main".into(),
                seq,
                timestamp: seq,
                message: AgentMessage::user(id, vec![]),
                terminate: false,
            })
            .unwrap();
    }
    source
        .append_record(Record::OperationStarted {
            id: "run".into(),
            seq: 3,
            lane: "main".into(),
            timestamp: 3,
            source_leaf_id: Some("child".into()),
            intent: OperationIntent::Run,
        })
        .unwrap();
    source
        .append_record(Record::FactSet {
            id: "fact-model".into(),
            seq: 4,
            lane: "main".into(),
            timestamp: 4,
            run_id: None,
            key: "model".into(),
            value: "gpt-test".into(),
        })
        .unwrap();
    let branch = source
        .fork_branch(dir.path().join("branch.sqlite"), "branch", "child")
        .unwrap();
    assert_eq!(branch.entries().len(), 2);
    assert_eq!(
        branch
            .records()
            .iter()
            .filter(|record| matches!(record, Record::FactSet { .. }))
            .count(),
        1
    );
    assert!(!branch
        .records()
        .iter()
        .any(|record| matches!(record, Record::OperationStarted { .. })));
    assert_eq!(branch.session_id(), "branch");
    assert_eq!(branch.parent_session_id(), Some("source"));
}

#[test]
fn sqlite_serializes_concurrent_writers_with_stale_sequence_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.sqlite");
    let mut root = SqliteStore::open(&path, "session").unwrap();
    root.append_entry(Entry {
        id: "root".into(),
        parent_id: None,
        lane: "main".into(),
        seq: 1,
        timestamp: 1,
        message: AgentMessage::user("root", vec![]),
        terminate: false,
    })
    .unwrap();
    drop(root);

    let first_path = path.clone();
    let first = std::thread::spawn(move || {
        let mut store = SqliteStore::open(&first_path, "session")?;
        store.append_entry(Entry {
            id: "first".into(),
            parent_id: Some("root".into()),
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("first", vec![]),
            terminate: false,
        })
    });
    let second_path = path.clone();
    let second = std::thread::spawn(move || {
        let mut store = SqliteStore::open(&second_path, "session")?;
        store.append_entry(Entry {
            id: "second".into(),
            parent_id: Some("root".into()),
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("second", vec![]),
            terminate: false,
        })
    });
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    first.unwrap();
    second.unwrap();

    let reopened = SqliteStore::open(&path, "session").unwrap();
    assert_eq!(reopened.entries().len(), 3);
    assert_eq!(
        reopened
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}
