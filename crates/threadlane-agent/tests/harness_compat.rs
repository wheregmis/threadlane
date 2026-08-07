use std::fs;
use tempfile::tempdir;
use threadlane_agent::harness::{
    Entry, JsonlStore, LaneStatus, OperationIntent, Record, SessionStore,
};
use threadlane_agent::{AgentMessage, SessionTree};

#[test]
fn legacy_global_facts_are_present_in_the_reduced_main_lane() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chat.jsonl");
    let mut tree = SessionTree::new("chat");
    tree.file_path = Some(path.clone());
    tree.set_fact("model", "gpt-test").unwrap();

    let store = JsonlStore::open(&path).unwrap();
    assert_eq!(
        threadlane_agent::harness::Reducer::reduce(&store)
            .unwrap()
            .lane("main")
            .unwrap()
            .facts["model"],
        "gpt-test"
    );
}

#[test]
fn jsonl_fork_copies_a_branch_and_facts_without_operation_history() {
    let dir = tempdir().unwrap();
    let source_path = dir.path().join("source.jsonl");
    let mut tree = SessionTree::new("source");
    tree.file_path = Some(source_path.clone());
    tree.set_model("antigravity/test-model".into()).unwrap();
    let root = tree.add_message(AgentMessage::user("root", vec![]));
    let child = tree.add_message(AgentMessage::user("child", vec![]));
    tree.set_fact("model", "gpt-test").unwrap();

    let mut source = JsonlStore::open(&source_path).unwrap();
    source
        .append_record(Record::OperationStarted {
            id: "run".into(),
            seq: source.next_sequence(),
            lane: "main".into(),
            timestamp: 10,
            source_leaf_id: Some(child.clone()),
            intent: OperationIntent::Run,
        })
        .unwrap();
    let branch = source
        .fork_branch(dir.path().join("branch.jsonl"), "branch", &child)
        .unwrap();

    assert_eq!(branch.session_id(), "branch");
    assert_eq!(branch.parent_session_id(), Some("source"));
    assert_eq!(branch.tree().model.as_deref(), Some("gpt-test"));
    assert_eq!(branch.entries().len(), 2);
    assert_eq!(branch.entries()[0].id, root);
    assert_eq!(branch.entries()[0].parent_id, None);
    assert_eq!(
        branch.entries()[1].parent_id,
        Some(branch.entries()[0].id.clone())
    );
    assert!(branch.records().iter().any(|record| {
        matches!(record, Record::FactSet { key, value, .. } if key == "model" && value == "gpt-test")
    }));
    assert!(!branch
        .records()
        .iter()
        .any(|record| matches!(record, Record::OperationStarted { .. })));
}

#[test]
fn legacy_model_images_and_passive_branches_survive_harness_open() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chat.jsonl");
    let mut tree = SessionTree::new("chat");
    tree.file_path = Some(path.clone());
    tree.set_model("antigravity/test-model".into()).unwrap();
    tree.set_fact("provider", "antigravity").unwrap();
    let root = tree.add_message(AgentMessage::user(
        "inspect this",
        vec![threadlane_agent::ImageAttachment {
            display_name: "diagram.png".into(),
            data_url: "data:image/png;base64,AA==".into(),
        }],
    ));
    tree.append_passive_branch(
        Some(&root),
        vec![AgentMessage::Assistant {
            content: Some("background result".into()),
            tool_calls: None,
            stop_reason: Some("stop".into()),
            deferred_handle: None,
        }],
    )
    .unwrap();

    let store = JsonlStore::open(&path).unwrap();
    assert_eq!(
        store.tree().model.as_deref(),
        Some("antigravity/test-model")
    );
    assert_eq!(
        store
            .tree()
            .global_facts
            .get("provider")
            .map(String::as_str),
        Some("antigravity")
    );
    let active = store.tree().get_active_branch_messages();
    assert!(matches!(
        &active[0],
        AgentMessage::UserWithImages { images, .. } if images[0].display_name == "diagram.png"
    ));
    assert_eq!(store.tree().nodes.len(), 2);
}

#[test]
fn checked_in_legacy_fixture_opens_idle_without_rewriting() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("legacy.jsonl");
    let fixture = include_str!("fixtures/sessions/legacy_full.jsonl");
    fs::write(&path, fixture).unwrap();
    let before = fs::read_to_string(&path).unwrap();
    let store = JsonlStore::open(&path).unwrap();
    let state = threadlane_agent::harness::Reducer::reduce(&store).unwrap();
    assert_eq!(state.lane("main").unwrap().status, LaneStatus::Idle);
    assert_eq!(
        state.lane("main").unwrap().leaf_id.as_deref(),
        Some("node_2")
    );
    assert_eq!(
        store.tree().model.as_deref(),
        Some("antigravity/test-model")
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn malformed_complete_lines_fail_but_a_torn_final_line_is_ignored() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chat.jsonl");
    fs::write(&path, "{\"id\":\"node_1\",\"parent_id\":null,\"timestamp\":1,\"message\":{\"role\":\"user\",\"content\":\"ok\"}}\nnot-json\n").unwrap();
    assert!(JsonlStore::open(&path).is_err());

    fs::write(&path, "{\"id\":\"node_1\",\"parent_id\":null,\"timestamp\":1,\"message\":{\"role\":\"user\",\"content\":\"ok\"}}\n{\"id\"").unwrap();
    assert!(JsonlStore::open(&path).is_ok());
}

#[test]
fn legacy_loader_does_not_silently_drop_malformed_complete_lines() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chat.jsonl");
    fs::write(&path, "not-json\n").unwrap();
    assert!(SessionTree::load_from_file(&path).is_err());
}

#[test]
fn jsonl_store_round_trips_v2_entries_and_records_without_rewriting_legacy_data() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chat.jsonl");
    fs::write(
        &path,
        "{\"id\":\"node_1\",\"parent_id\":null,\"timestamp\":1,\"message\":{\"role\":\"user\",\"content\":\"legacy\"}}\n",
    )
    .unwrap();
    let mut store = JsonlStore::open(&path).unwrap();
    let parent = store.entries()[0].id.clone();
    store
        .append_entry(Entry {
            id: "entry-v2".into(),
            parent_id: Some(parent),
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("v2", vec![]),
            terminate: false,
        })
        .unwrap();
    assert_eq!(store.tree().active_node_id(), Some("entry-v2"));
    store
        .append_record(Record::OperationStarted {
            id: "run-v2".into(),
            seq: 3,
            lane: "main".into(),
            timestamp: 3,
            source_leaf_id: Some("entry-v2".into()),
            intent: OperationIntent::Run,
        })
        .unwrap();
    let reopened = JsonlStore::open(&path).unwrap();
    assert_eq!(reopened.entries().len(), 2);
    assert_eq!(reopened.records().len(), 1);
    assert_eq!(reopened.entries()[1].id, "entry-v2");
    assert_eq!(reopened.records()[0].id(), "run-v2");
}

#[test]
fn jsonl_store_reads_v2_records_embedded_in_the_session_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chat.jsonl");
    let record = Record::OperationStarted {
        id: "embedded-run".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        source_leaf_id: Some("node_1".into()),
        intent: OperationIntent::Run,
    };
    fs::write(
        &path,
        format!(
            "{{\"id\":\"node_1\",\"parent_id\":null,\"timestamp\":1,\"message\":{{\"role\":\"user\",\"content\":\"legacy\"}}}}\n{}\n",
            serde_json::to_string(&record).unwrap()
        ),
    )
    .unwrap();

    let store = JsonlStore::open(&path).unwrap();
    assert_eq!(store.records().len(), 1);
    assert_eq!(store.records()[0].id(), "embedded-run");
}

#[test]
fn compatibility_tree_can_follow_v2_without_a_second_durable_append() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("chat.jsonl");
    let mut tree = SessionTree::new("chat");
    tree.file_path = Some(path.clone());
    tree.add_message(AgentMessage::user("before", vec![]));
    let before = fs::read_to_string(&path).unwrap();

    tree.add_message_in_memory(AgentMessage::Assistant {
        content: Some("already committed by V2".into()),
        tool_calls: None,
        stop_reason: Some("stop".into()),
        deferred_handle: None,
    });

    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    assert_eq!(tree.get_active_branch_messages().len(), 2);
}

#[cfg(unix)]
#[test]
fn jsonl_writer_claim_is_nonblocking_across_file_descriptors() {
    use std::os::fd::AsRawFd;

    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("chat.jsonl");
    fs::write(&path, "").unwrap();
    let _store = JsonlStore::open(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.with_extension("harness.lock"))
        .unwrap();

    assert_eq!(unsafe { flock(lock.as_raw_fd(), 2 | 4) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
