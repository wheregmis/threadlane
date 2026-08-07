use threadlane_agent::harness::{
    Entry, MemoryStore, OperationIntent, OperationOutcome, Record, Reducer, SessionStore,
    SqliteStore,
};
use threadlane_agent::AgentMessage;

fn exercise<S: SessionStore>(store: &mut S) {
    store
        .append_entry(Entry {
            id: "user".into(),
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
            id: "run".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 2,
            source_leaf_id: Some("user".into()),
            intent: OperationIntent::Run,
        })
        .unwrap();
    store
        .append_entry(Entry {
            id: "assistant".into(),
            parent_id: Some("user".into()),
            lane: "main".into(),
            seq: 3,
            timestamp: 3,
            message: AgentMessage::Assistant {
                content: Some("done".into()),
                tool_calls: None,
                stop_reason: Some("stop".into()),
                deferred_handle: None,
            },
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::StepAttempt {
            id: "attempt".into(),
            seq: 4,
            lane: "main".into(),
            timestamp: 4,
            run_id: "run".into(),
            attempt: 1,
            result_entry_id: "assistant".into(),
            compaction_reason: None,
        })
        .unwrap();
    store
        .append_record(Record::OperationFinished {
            id: "finish".into(),
            seq: 5,
            lane: "main".into(),
            timestamp: 5,
            run_id: "run".into(),
            outcome: OperationOutcome::Completed,
            error: None,
        })
        .unwrap();
    store
        .append_record(Record::FactSet {
            id: "fact-model".into(),
            seq: 6,
            lane: "main".into(),
            timestamp: 6,
            run_id: None,
            key: "model".into(),
            value: "gpt-test".into(),
        })
        .unwrap();
}

#[test]
fn memory_jsonl_and_sqlite_have_identical_logical_state() {
    let mut memory = MemoryStore::new("session");
    exercise(&mut memory);

    let dir = tempfile::tempdir().unwrap();
    let jsonl_path = dir.path().join("session.jsonl");
    std::fs::File::create(&jsonl_path).unwrap();
    let mut jsonl = threadlane_agent::harness::JsonlStore::open(&jsonl_path).unwrap();
    exercise(&mut jsonl);

    let sqlite_path = dir.path().join("session.sqlite");
    let mut sqlite = SqliteStore::open(&sqlite_path, "session").unwrap();
    exercise(&mut sqlite);

    assert_eq!(memory.entries(), jsonl.entries());
    assert_eq!(memory.entries(), sqlite.entries());
    assert_eq!(memory.records(), jsonl.records());
    assert_eq!(memory.records(), sqlite.records());
    assert_eq!(memory.facts(), jsonl.facts());
    assert_eq!(memory.facts(), sqlite.facts());
    assert_eq!(Reducer::reduce(&memory), Reducer::reduce(&jsonl));
    assert_eq!(Reducer::reduce(&memory), Reducer::reduce(&sqlite));
}

fn invalid_append_cases<S: SessionStore>(
    store: &mut S,
) -> Vec<threadlane_agent::harness::ReduceError> {
    store
        .append_entry(Entry {
            id: "root".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("root", vec![]),
            terminate: false,
        })
        .unwrap();
    [
        store
            .append_entry(Entry {
                id: "root".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::user("duplicate", vec![]),
                terminate: false,
            })
            .unwrap_err(),
        store
            .append_entry(Entry {
                id: "missing-parent".into(),
                parent_id: Some("missing".into()),
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::user("missing", vec![]),
                terminate: false,
            })
            .unwrap_err(),
        store
            .append_record(Record::OperationStarted {
                id: "bad-lane".into(),
                seq: 2,
                lane: "".into(),
                timestamp: 2,
                source_leaf_id: Some("root".into()),
                intent: OperationIntent::Run,
            })
            .unwrap_err(),
    ]
    .into_iter()
    .collect()
}

#[test]
fn memory_jsonl_and_sqlite_return_identical_validation_errors_and_preserve_prefix() {
    let mut memory = MemoryStore::new("session");
    let memory_errors = invalid_append_cases(&mut memory);

    let dir = tempfile::tempdir().unwrap();
    let jsonl_path = dir.path().join("session.jsonl");
    std::fs::File::create(&jsonl_path).unwrap();
    let mut jsonl = threadlane_agent::harness::JsonlStore::open(&jsonl_path).unwrap();
    let jsonl_errors = invalid_append_cases(&mut jsonl);

    let sqlite_path = dir.path().join("session.sqlite");
    let mut sqlite = SqliteStore::open(&sqlite_path, "session").unwrap();
    let sqlite_errors = invalid_append_cases(&mut sqlite);

    assert_eq!(memory_errors, jsonl_errors);
    assert_eq!(memory_errors, sqlite_errors);
    assert_eq!(memory.entries(), jsonl.entries());
    assert_eq!(memory.entries(), sqlite.entries());
    assert_eq!(memory.records(), jsonl.records());
    assert_eq!(memory.records(), sqlite.records());
}
