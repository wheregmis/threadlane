use super::{BrokerError, BrokerOperationResult, BrokerRequest, WasiExtensionManager};
use super::{CapabilityDispatcher, CapabilityHandler, HostBrokerRequest};
use serde_json::{json, Value};
use std::{fs, path::Path};
use std::{
    future::Future,
    io::Write,
    task::{Context, Poll, Waker},
};

const NAME: &str = "checkpoint_probe";

fn reply_identity() -> threadlane_protocol::ToolExecutionIdentity {
    threadlane_protocol::ToolExecutionIdentity {
        session_id: "a".into(),
        lane: "main".into(),
        run_id: "run".into(),
        assistant_entry_id: "assistant".into(),
        tool_call_id: "call".into(),
        tool_name: "probe".into(),
        result_entry_id: "result".into(),
    }
}

#[test]
fn terminal_reply_commits_with_state_and_broker_acknowledgment() {
    let project = tempfile::tempdir().unwrap();
    let manager = emitting_manager(project.path());
    let identity = reply_identity();
    let mut operation = manager.begin_tool_operation("probe").unwrap().unwrap();
    let mut wrong_scope = identity.clone();
    wrong_scope.session_id = "b".into();
    assert!(operation.invoke_for_execution("{}", &wrong_scope).is_err());
    assert!(!project.path().join("effects.log").exists());
    let pending = operation.invoke_for_execution("{}", &identity).unwrap();
    manager
        .enqueue_broker_results(dispatch_once(
            project.path(),
            &manager,
            pending.host_broker_requests,
        ))
        .unwrap();
    let done = operation.invoke_for_execution("{}", &identity).unwrap();
    assert_eq!(done.response.message.as_deref(), Some("done"));
    let checkpoint: Value =
        serde_json::from_slice(&fs::read(manager.state_path(NAME).unwrap()).unwrap()).unwrap();
    assert_eq!(checkpoint["state"]["phase"], "ready");
    assert!(checkpoint["broker_events"].as_array().unwrap().is_empty());
    assert_eq!(
        checkpoint["terminal_reply"]["identity"],
        serde_json::to_value(&identity).unwrap()
    );
    assert_eq!(checkpoint["terminal_reply"]["result"], json!({"Ok":"done"}));
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
    drop(operation);
    let path = manager.state_path(NAME).unwrap();
    let saved = fs::read(&path).unwrap();
    assert!(manager
        .set_extension_state(NAME, json!({"overwrite":true}))
        .is_err());
    assert!(manager
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), saved);
    drop(manager);
    fs::remove_file(project.path().join(".threadlane/extensions/probe.wasm")).unwrap();
    let recovered = WasiExtensionManager::for_project_session(project.path(), "a");
    assert_eq!(
        recovered
            .reload_from_roots(None, Some(project.path()))
            .unwrap(),
        0
    );
    assert!(recovered.set_session_scope("b").is_err());
    assert_eq!(
        recovered
            .recover_tool_reply(&identity, "probe", "{}")
            .unwrap(),
        Some(Ok("done".into()))
    );
    assert!(recovered
        .recover_tool_reply(&identity, "probe", r#"{"changed":true}"#)
        .is_err());
    let mut wrong_run = identity.clone();
    wrong_run.run_id = "later-run".into();
    assert_eq!(
        recovered
            .recover_tool_reply(&wrong_run, "probe", "{}")
            .unwrap(),
        None
    );
    recovered.acknowledge_tool_reply(&wrong_run).unwrap();
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert!(recovered.set_session_scope("b").is_err());
    recovered.acknowledge_tool_reply(&identity).unwrap();
    assert!(recovered
        .pending_tool_reply_identities()
        .unwrap()
        .is_empty());
    let checkpoint: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(checkpoint.get("terminal_reply").is_none());
    assert_eq!(checkpoint["state"]["phase"], "ready");
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[test]
fn canonical_reply_lookup_restores_a_cold_removed_extension_checkpoint() {
    let project = tempfile::tempdir().unwrap();
    let manager = emitting_manager(project.path());
    let identity = reply_identity();
    let mut operation = manager.begin_tool_operation("probe").unwrap().unwrap();
    let pending = operation.invoke_for_execution("{}", &identity).unwrap();
    manager
        .enqueue_broker_results(dispatch_once(
            project.path(),
            &manager,
            pending.host_broker_requests,
        ))
        .unwrap();
    operation.invoke_for_execution("{}", &identity).unwrap();
    drop(operation);
    let mut prepared =
        threadlane_protocol::AgentToolResult::external("call", "probe", "done plus hook", false);
    prepared.terminate = true;
    prepared.images.push(threadlane_protocol::ImageAttachment {
        display_name: "fixture.png".into(),
        data_url: "data:image/png;base64,AA==".into(),
    });
    manager.prepare_tool_reply(&identity, &prepared).unwrap();
    let checkpoint = manager.state_path(NAME).unwrap();
    let before = fs::read(&checkpoint).unwrap();
    let competing = WasiExtensionManager::for_project_session(project.path(), "a");
    assert!(competing
        .recovered_canonical_reply(&identity)
        .unwrap_err()
        .contains("owned"));
    drop(competing);
    drop(manager);
    fs::remove_file(project.path().join(".threadlane/extensions/probe.wasm")).unwrap();
    let recovered = WasiExtensionManager::for_project_session(project.path(), "a");
    assert_eq!(
        recovered.recovered_canonical_reply(&identity).unwrap(),
        Some(prepared.clone())
    );
    assert_eq!(
        recovered.recovered_canonical_reply(&identity).unwrap(),
        Some(prepared)
    );
    let mut other = identity.clone();
    other.run_id = "another-run".into();
    assert_eq!(recovered.recovered_canonical_reply(&other).unwrap(), None);
    assert_eq!(fs::read(&checkpoint).unwrap(), before);
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[cfg(unix)]
#[test]
fn terminal_reply_sync_uncertainty_recovers_without_redelivery_or_effect_replay() {
    let project = tempfile::tempdir().unwrap();
    let manager = emitting_manager(project.path());
    let identity = reply_identity();
    let mut operation = manager.begin_tool_operation("probe").unwrap().unwrap();
    let pending = operation.invoke_for_execution("{}", &identity).unwrap();
    manager
        .enqueue_broker_results(dispatch_once(
            project.path(),
            &manager,
            pending.host_broker_requests,
        ))
        .unwrap();
    fail_next_directory_sync();
    assert!(operation
        .invoke_for_execution("{}", &identity)
        .unwrap_err()
        .contains("recover_state_commit"));
    drop(operation);
    assert!(manager
        .recover_tool_reply(&identity, "probe", "{}")
        .is_err());
    manager.recover_state_commit().unwrap();
    assert_eq!(
        manager
            .recover_tool_reply(&identity, "probe", "{}")
            .unwrap(),
        Some(Ok("done".into()))
    );
    let mut result =
        threadlane_protocol::AgentToolResult::external("call", "probe", "done plus hook", false);
    result.terminate = true;
    result.images.push(threadlane_protocol::ImageAttachment {
        display_name: "fixture.png".into(),
        data_url: "data:image/png;base64,AA==".into(),
    });
    manager.prepare_tool_reply(&identity, &result).unwrap();
    assert_eq!(
        manager.recovered_canonical_reply(&identity).unwrap(),
        Some(result.clone())
    );
    let before = fs::read(manager.state_path(NAME).unwrap()).unwrap();
    manager.prepare_tool_reply(&identity, &result).unwrap();
    let mut changed = result.clone();
    changed.content.push_str("changed");
    assert!(manager.prepare_tool_reply(&identity, &changed).is_err());
    assert_eq!(fs::read(manager.state_path(NAME).unwrap()).unwrap(), before);
    fail_next_directory_sync();
    assert!(manager
        .acknowledge_tool_reply(&identity)
        .unwrap_err()
        .contains("recover_state_commit"));
    manager.recover_state_commit().unwrap();
    assert!(manager.pending_tool_reply_identities().unwrap().is_empty());
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[test]
fn terminal_broker_reply_waits_for_known_outcomes_and_preserves_success_or_failure() {
    for failed in [false, true] {
        let project = tempfile::tempdir().unwrap();
        let manager = emitting_manager_with_continuation(project.path(), false);
        let identity = reply_identity();
        let mut operation = manager.begin_tool_operation("probe").unwrap().unwrap();
        let terminal = operation.invoke_for_execution("{}", &identity).unwrap();
        assert!(!terminal.response.continue_after_broker);
        drop(operation);
        let path = manager.state_path(NAME).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(manager
            .recover_tool_reply(&identity, "probe", "{}")
            .is_err());
        assert!(manager.acknowledge_tool_reply(&identity).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut outcomes = dispatch_once(project.path(), &manager, terminal.host_broker_requests);
        if failed {
            outcomes[0].error = Some(BrokerError {
                code: "fixture".into(),
                message: "known broker failure".into(),
            });
        } else {
            outcomes[0].value = json!({"output":"known broker reply"});
        }
        manager.enqueue_broker_results(outcomes).unwrap();
        drop(manager);
        fs::remove_file(project.path().join(".threadlane/extensions/probe.wasm")).unwrap();
        let recovered = WasiExtensionManager::for_project_session(project.path(), "a");
        assert_eq!(
            recovered
                .recover_tool_reply(&identity, "probe", "{}")
                .unwrap(),
            Some(if failed {
                Err("known broker failure".into())
            } else {
                Ok("known broker reply".into())
            })
        );
        recovered.acknowledge_tool_reply(&identity).unwrap();
        assert!(recovered
            .pending_tool_reply_identities()
            .unwrap()
            .is_empty());
        assert_eq!(
            fs::read(project.path().join("effects.log")).unwrap(),
            b"effect\n"
        );
    }
}

#[cfg(unix)]
fn fail_next_directory_sync() {
    super::FAIL_NEXT_STATE_DIRECTORY_SYNC.with(|fault| fault.set(true));
}

#[cfg(unix)]
#[test]
fn unconfirmed_intent_cannot_be_overwritten_and_recovers_as_not_dispatched() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    initial
        .set_extension_state(NAME, json!({"phase":"ready"}))
        .unwrap();
    fail_next_directory_sync();
    let error = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap_err();
    assert!(error.contains("recover_state_commit"), "{error}");
    let path = initial.state_path(NAME).unwrap();
    let saved = fs::read(&path).unwrap();
    let checkpoint: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(checkpoint["unsettled"].as_array().unwrap().len(), 1);
    assert!(initial.extension_state(NAME).is_none());
    assert!(initial.set_extension_state(NAME, json!({})).is_err());
    assert!(initial.begin_tool_operation("probe").unwrap().is_err());
    assert!(initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .is_err());
    assert!(initial.set_session_scope("b").is_err());
    assert_eq!(fs::read(&path).unwrap(), saved);

    fail_next_directory_sync();
    assert!(initial
        .recover_state_commit()
        .unwrap_err()
        .contains("confirmation still failed"));
    assert_eq!(fs::read(&path).unwrap(), saved);
    initial.recover_state_commit().unwrap();
    let checkpoint: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(checkpoint["unsettled"].as_array().unwrap().is_empty());
    assert_eq!(
        checkpoint["broker_events"][0]["payload"]["error"]["code"],
        json!("not_dispatched")
    );
    let done = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    assert!(done.host_broker_requests.is_empty());
    assert!(values(&initial).is_empty());
    assert!(!project.path().join("effects.log").exists());
    assert_eq!(
        initial
            .last_broker_id
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[cfg(unix)]
#[test]
fn outcome_sync_failure_recovers_once_without_repeating_the_physical_effect() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    let invocation = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    let outcomes = dispatch_once(project.path(), &initial, invocation.host_broker_requests);
    fail_next_directory_sync();
    assert!(initial
        .enqueue_broker_results(outcomes)
        .unwrap_err()
        .contains("retained in memory"));
    assert_eq!(values(&initial), vec![json!("executed once")]);
    initial.recover_state_commit().unwrap();
    assert_eq!(values(&initial), vec![json!("executed once")]);
    assert!(initial.unsettled_broker.lock().unwrap()[NAME].is_empty());
    let done = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    assert!(done.host_broker_requests.is_empty());
    assert!(values(&initial).is_empty());
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[cfg(unix)]
#[test]
fn acknowledgment_sync_failure_does_not_redeliver_after_confirmation() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    let invocation = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    initial
        .enqueue_broker_results(dispatch_once(
            project.path(),
            &initial,
            invocation.host_broker_requests,
        ))
        .unwrap();
    initial.subscribe_event(NAME, "notice".into()).unwrap();
    initial
        .publish_event("notice".into(), json!("delivered notification"))
        .unwrap();
    fail_next_directory_sync();
    assert!(initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .is_err());
    assert_eq!(values(&initial).len(), 2);
    initial.recover_state_commit().unwrap();
    assert!(values(&initial).is_empty());
    assert_eq!(
        initial.extension_state(NAME),
        Some(json!({"phase":"ready"}))
    );
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
    drop(initial);
    let restored = emitting_manager(project.path());
    assert!(values(&restored).is_empty());
    assert_eq!(
        restored.extension_state(NAME),
        Some(json!({"phase":"ready"}))
    );
}

#[cfg(unix)]
#[test]
fn results_arriving_during_an_unconfirmed_state_edit_are_retained() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    let mut operation = initial.begin_tool_operation("probe").unwrap().unwrap();
    let invocation = operation.invoke("{}").unwrap();
    fail_next_directory_sync();
    assert!(initial
        .set_extension_state(NAME, json!({"phase":"waiting", "edited":true}))
        .is_err());
    let path = initial.state_path(NAME).unwrap();
    let saved = fs::read(&path).unwrap();
    let outcomes = dispatch_once(project.path(), &initial, invocation.host_broker_requests);
    assert!(initial
        .enqueue_broker_results(outcomes)
        .unwrap_err()
        .contains("retained in memory"));
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert!(initial
        .recover_state_commit()
        .unwrap_err()
        .contains("active"));
    drop(operation);
    initial.recover_state_commit().unwrap();
    assert_eq!(
        initial.extension_state(NAME),
        Some(json!({"phase":"waiting", "edited":true}))
    );
    assert_eq!(values(&initial), vec![json!("executed once")]);
    let done = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    assert!(done.host_broker_requests.is_empty());
    assert!(values(&initial).is_empty());
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[cfg(unix)]
#[test]
fn cached_host_policy_is_hidden_until_its_replacement_is_confirmed() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    initial
        .set_host_state("tools.policy", json!("full"))
        .unwrap();
    fail_next_directory_sync();
    assert!(initial
        .set_host_state("tools.policy", json!("read_only"))
        .is_err());
    assert!(initial.host_state("tools.policy").is_err());
    let path = initial.host_state_path("tools.policy").unwrap();
    let saved = fs::read(&path).unwrap();
    fs::write(&path, br#""full""#).unwrap();
    let error = initial.recover_state_commit().unwrap_err();
    assert!(error.contains("changed"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), br#""full""#);
    assert!(initial.host_state("tools.policy").is_err());
    fs::write(&path, saved).unwrap();
    initial.recover_state_commit().unwrap();
    assert_eq!(
        initial.host_state("tools.policy").unwrap(),
        Some(json!("read_only"))
    );
}

/// Measures serialization plus the complete atomic/synced checkpoint write.
/// Run separately from other tests with `--run-ignored only --no-capture`.
#[test]
#[ignore]
fn checkpoint_write_profile() {
    use std::{hint::black_box, sync::Arc, time::Instant};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("checkpoint.json");
    let state = json!({"documents": (0..128).map(|index| json!({
        "path": format!("src/file-{index}.rs"),
        "text": "source 🦀\n".repeat(1_000),
        "version": index,
    })).collect::<Vec<_>>()});
    let events = vec![Arc::new(super::WasiExtensionEvent {
        topic: "broker_response".into(),
        payload: json!({"api_version": 2, "capability": "process", "operation": "recv",
            "arguments": {}, "ok": true, "value": "reply"}),
    })];
    let mut samples = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        for _ in 0..25 {
            super::checkpoint::persist_checkpoint(black_box(&path), &state, &events, 0, &[], None)
                .unwrap();
        }
        samples.push(start.elapsed().as_nanos() / 25);
    }
    samples.sort_unstable();
    let bytes = fs::read(&path).unwrap();
    let decoded =
        super::checkpoint::ExtensionCheckpoint::decode(serde_json::from_slice(&bytes).unwrap())
            .unwrap();
    assert_eq!(decoded.state, state);
    assert_eq!(decoded.broker_events[0].payload, events[0].payload);
    eprintln!(
        "durable checkpoint: median {} ns/write; {} bytes",
        samples[3],
        bytes.len()
    );
}

fn manager(project: &Path, trap: bool) -> WasiExtensionManager {
    let manifest = json!({"api_version":2,"name":NAME,"version":"1","description":"test","tools":[{"name":"probe","description":"test","parameters":{}}]}).to_string();
    let response = r#"{"message":"done","state":{"phase":"ready"}}"#;
    let body = if trap {
        "unreachable".into()
    } else {
        format!("(i64.const {})", (1024u64 << 32) | response.len() as u64)
    };
    let wasm = format!(
        r#"(module
        (memory (export "memory") 1)
        (data (i32.const 0) "{}")
        (data (i32.const 1024) "{}")
        (func (export "extension_info") (result i64) (i64.const {}))
        (func (export "alloc") (param i32) (result i32) (i32.const 4096))
        (func (export "execute_tool") (param i32 i32) (result i64) {}))"#,
        manifest.replace('"', "\\\""),
        response.replace('"', "\\\""),
        manifest.len(),
        body
    );
    let path = project.join(".threadlane/extensions/probe.wasm");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, wasm).unwrap();
    let manager = WasiExtensionManager::for_project_session(project, "a");
    assert_eq!(manager.reload_from_roots(None, Some(project)).unwrap(), 1);
    manager
}

fn outcome(value: &str) -> BrokerOperationResult {
    BrokerOperationResult {
        receipt: None,
        invoking_extension: NAME.into(),
        request: BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "recv".into(),
            arguments: json!({"name":"server"}),
        },
        value: json!(value),
        error: None,
    }
}

fn values(manager: &WasiExtensionManager) -> Vec<Value> {
    manager
        .pending_events
        .lock()
        .unwrap()
        .get(&Some("a".into()))
        .and_then(|queues| queues.get(NAME))
        .into_iter()
        .flatten()
        .map(|event| event.payload["value"].clone())
        .collect()
}

#[test]
fn outcomes_and_state_survive_reload_and_are_acknowledged_together() {
    let project = tempfile::tempdir().unwrap();
    let initial = manager(project.path(), false);
    initial
        .set_extension_state(NAME, json!({"phase":"waiting"}))
        .unwrap();
    initial
        .enqueue_broker_results(vec![outcome("once")])
        .unwrap();
    drop(initial);
    let restored = manager(project.path(), false);
    assert_eq!(
        restored.extension_state(NAME),
        Some(json!({"phase":"waiting"}))
    );
    assert_eq!(values(&restored), vec![json!("once")]);
    restored
        .reload_from_roots(None, Some(project.path()))
        .unwrap();
    assert_eq!(values(&restored), vec![json!("once")]);
    let result = restored
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    assert_eq!(result.response.message.as_deref(), Some("done"));
    drop(restored);
    let acknowledged = manager(project.path(), false);
    assert_eq!(
        acknowledged.extension_state(NAME),
        Some(json!({"phase":"ready"}))
    );
    assert!(values(&acknowledged).is_empty());
}

#[test]
fn traps_keep_the_durable_delivery_unacknowledged() {
    let project = tempfile::tempdir().unwrap();
    let initial = manager(project.path(), true);
    initial
        .set_extension_state(NAME, json!({"phase":"waiting"}))
        .unwrap();
    initial
        .enqueue_broker_results(vec![outcome("once")])
        .unwrap();
    assert!(initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .is_err());
    drop(initial);
    let restored = manager(project.path(), false);
    assert_eq!(
        restored.extension_state(NAME),
        Some(json!({"phase":"waiting"}))
    );
    assert_eq!(values(&restored), vec![json!("once")]);
    assert!(restored
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .is_ok());
    drop(restored);
    assert!(values(&manager(project.path(), false)).is_empty());
}

#[test]
fn writes_during_delivery_keep_in_flight_outcomes_in_the_checkpoint() {
    let project = tempfile::tempdir().unwrap();
    let initial = manager(project.path(), false);
    initial
        .set_extension_state(NAME, json!({"phase":"waiting"}))
        .unwrap();
    initial
        .enqueue_broker_results(vec![outcome("first")])
        .unwrap();
    let (_, _, batch) = initial.begin_delivery(NAME, false, None).unwrap();
    assert_eq!(batch.len(), 1);
    initial
        .enqueue_broker_results(vec![outcome("second")])
        .unwrap();
    initial
        .set_extension_state(NAME, json!({"phase":"waiting"}))
        .unwrap();
    let error = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap_err();
    assert!(error.contains("already executing"), "{error}");
    assert!(initial.set_session_scope("b").is_err());
    assert!(initial
        .reload_from_roots(None, Some(project.path()))
        .is_err());
    drop(initial); // Cut after delivery began, before its state could commit.
    let restored = manager(project.path(), false);
    assert_eq!(values(&restored), vec![json!("first"), json!("second")]);
}

#[test]
fn outcomes_follow_the_session_after_scope_eviction() {
    let project = tempfile::tempdir().unwrap();
    let initial = manager(project.path(), false);
    initial
        .set_extension_state(NAME, json!({"phase":"waiting"}))
        .unwrap();
    initial.enqueue_broker_results(vec![outcome("a")]).unwrap();
    initial.set_session_scope("b").unwrap();
    assert!(values(&initial).is_empty());
    initial.set_session_scope("a").unwrap();
    assert_eq!(values(&initial), vec![json!("a")]);
}

#[test]
fn failed_checkpoint_commits_keep_outcomes_and_report_no_replay() {
    let project = tempfile::tempdir().unwrap();
    let mut initial = manager(project.path(), false);
    initial
        .set_extension_state(NAME, json!({"phase":"waiting"}))
        .unwrap();
    initial
        .enqueue_broker_results(vec![outcome("first")])
        .unwrap();
    let path = initial.state_path(NAME).unwrap();
    let before = fs::read(&path).unwrap();
    let original_root = initial.state_dir.clone();
    let blocked = project.path().join("blocked");
    fs::write(&blocked, b"not a directory").unwrap();
    initial.state_dir = Some(blocked.join("state"));
    let error = initial
        .enqueue_broker_results(vec![outcome("second")])
        .unwrap_err();
    assert!(error.contains("do not repeat"), "{error}");
    assert!(initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .is_err());
    assert_eq!(
        initial.extension_state(NAME),
        Some(json!({"phase":"waiting"}))
    );
    assert_eq!(values(&initial), vec![json!("first"), json!("second")]);
    assert_eq!(fs::read(path).unwrap(), before);
    initial.state_dir = original_root;
    assert!(initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .is_ok());
    drop(initial);
    assert!(values(&manager(project.path(), false)).is_empty());
}

#[test]
fn legacy_state_migrates_and_unknown_checkpoint_versions_preserve_live_state() {
    let project = tempfile::tempdir().unwrap();
    let initial = manager(project.path(), false);
    let path = initial.state_path(NAME).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, br#"{"phase":"legacy"}"#).unwrap();
    initial
        .reload_from_roots(None, Some(project.path()))
        .unwrap();
    assert_eq!(
        initial.extension_state(NAME),
        Some(json!({"phase":"legacy"}))
    );
    initial
        .enqueue_broker_results(vec![outcome("migrated")])
        .unwrap();
    let mut checkpoint: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(checkpoint["version"], json!(2));
    checkpoint["version"] = json!(999);
    fs::write(&path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let error = initial
        .reload_from_roots(None, Some(project.path()))
        .unwrap_err();
    assert!(error.contains("Unsupported"), "{error}");
    assert_eq!(
        initial.extension_state(NAME),
        Some(json!({"phase":"legacy"}))
    );
    assert_eq!(values(&initial), vec![json!("migrated")]);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
        checkpoint
    );
}

#[test]
fn checkpoints_validate_outcome_shapes_and_accept_successful_null_results() {
    let project = tempfile::tempdir().unwrap();
    let initial = manager(project.path(), false);
    let mut success = outcome("unused");
    success.value = Value::Null;
    let mut failure = outcome("unused");
    failure.error = Some(BrokerError {
        code: "process_missing".into(),
        message: "server ended".into(),
    });
    initial
        .enqueue_broker_results(vec![success, failure])
        .unwrap();
    drop(initial);
    let restored = manager(project.path(), false);
    assert_eq!(values(&restored).len(), 2);
    let path = restored.state_path(NAME).unwrap();
    let mut checkpoint: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    checkpoint["broker_events"][0]["payload"]
        .as_object_mut()
        .unwrap()
        .remove("value");
    fs::write(&path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    assert!(restored
        .reload_from_roots(None, Some(project.path()))
        .unwrap_err()
        .contains("broker event"));
    assert_eq!(values(&restored).len(), 2);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
        checkpoint
    );
}

fn emitting_manager(project: &Path) -> WasiExtensionManager {
    emitting_manager_with_continuation(project, true)
}

fn emitting_manager_with_continuation(project: &Path, continuation: bool) -> WasiExtensionManager {
    let manifest = json!({"api_version":2,"name":NAME,"version":"1","description":"test","capabilities":["process"],"tools":[{"name":"probe","description":"test","parameters":{}}],"commands":[{"name":"probe_command","description":"test"}],"hooks":["before_tool_call"]}).to_string();
    let pending =
        json!({"state":{"phase":"waiting"},"continue_after_broker":continuation}).to_string();
    let done = r#"{"state":{"phase":"ready"},"message":"done"}"#;
    let request = r#"{"api_version":2,"capability":"process","operation":"run","arguments":{"command":"effect"}}"#;
    let escape = |text: &str| text.replace('"', "\\\"");
    let wasm = format!(
        r#"(module
      (import "threadlane_host" "request" (func $request (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 0) "{}")
      (data (i32.const 1024) "{}")
      (data (i32.const 2048) "{}")
      (data (i32.const 3072) "{}")
      (func (export "extension_info") (result i64) (i64.const {}))
      (func (export "alloc") (param i32) (result i32) (i32.const 8192))
      (func (export "execute_tool") (export "execute_command") (export "handle_hook") (param $ptr i32) (param $len i32) (result i64)
        (local $end i32)
        (local.set $end (i32.sub (i32.add (local.get $ptr) (local.get $len)) (i32.const 8)))
        (block $scanned (loop $scan
          (br_if $scanned (i32.gt_u (local.get $ptr) (local.get $end)))
          (if (i64.eq (i64.load align=1 (local.get $ptr)) (i64.const 0x725f72656b6f7262))
            (then (return (i64.const {}))))
          (local.set $ptr (i32.add (local.get $ptr) (i32.const 1)))
          (br $scan)))
        (drop (call $request (i32.const 3072) (i32.const {}) (i32.const 4096) (i32.const 1024)))
        (i64.const {})))"#,
        escape(&manifest),
        escape(&pending),
        escape(done),
        escape(request),
        manifest.len(),
        (2048u64 << 32) | done.len() as u64,
        request.len(),
        (1024u64 << 32) | pending.len() as u64
    );
    let path = project.join(".threadlane/extensions/probe.wasm");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, wasm).unwrap();
    let manager = WasiExtensionManager::for_project_session(project, "a");
    manager.reload_from_roots(None, Some(project)).unwrap();
    manager
}

struct PhysicalEffect {
    checkpoint: std::path::PathBuf,
    effect: std::path::PathBuf,
}

fn install_other_module(project: &Path) {
    let directory = project.join(".threadlane/extensions");
    // Equal-length names retain the fixture's embedded manifest length.
    let other = fs::read_to_string(directory.join("probe.wasm"))
        .unwrap()
        .replace("probe", "other");
    fs::write(directory.join("other.wasm"), other).unwrap();
}

#[async_trait::async_trait]
impl CapabilityHandler for PhysicalEffect {
    fn handle(&self, _: &BrokerRequest) -> Result<Value, BrokerError> {
        let checkpoint: Value =
            serde_json::from_slice(&fs::read(&self.checkpoint).unwrap()).unwrap();
        assert_eq!(checkpoint["state"]["phase"], json!("waiting"));
        assert_eq!(checkpoint["unsettled"].as_array().unwrap().len(), 1);
        assert!(
            checkpoint["unsettled"][0]["receipt"]["id"]
                .as_u64()
                .unwrap()
                > 0
        );
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.effect)
            .unwrap()
            .write_all(b"effect\n")
            .unwrap();
        Ok(json!("executed once"))
    }
}

fn dispatch_once(
    project: &Path,
    manager: &WasiExtensionManager,
    requests: Vec<HostBrokerRequest>,
) -> Vec<BrokerOperationResult> {
    let mut dispatcher = CapabilityDispatcher::new();
    dispatcher.register(
        "process",
        std::sync::Arc::new(PhysicalEffect {
            checkpoint: manager.state_path(NAME).unwrap(),
            effect: project.join("effects.log"),
        }),
    );
    // This handler finishes synchronously; the real dispatcher needs no reactor.
    let mut future = std::pin::pin!(dispatcher.dispatch_envelopes(requests));
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => result.unwrap().operation_results,
        Poll::Pending => panic!("synchronous handler unexpectedly yielded"),
    }
}

#[test]
fn receipt_numbers_survive_restart_after_disable_or_removal() {
    for remove in [false, true] {
        let project = tempfile::tempdir().unwrap();
        let initial = emitting_manager(project.path());
        install_other_module(project.path());
        initial
            .reload_from_roots(None, Some(project.path()))
            .unwrap();
        let invocation = initial
            .execute_tool_with_broker_requests("probe", "{}")
            .unwrap()
            .unwrap();
        let previous_id = invocation.host_broker_requests[0]
            .receipt
            .as_ref()
            .unwrap()
            .id;
        initial
            .enqueue_broker_results(dispatch_once(
                project.path(),
                &initial,
                invocation.host_broker_requests,
            ))
            .unwrap();
        initial
            .execute_tool_with_broker_requests("probe", "{}")
            .unwrap()
            .unwrap();
        let saved_path = initial.state_path(NAME).unwrap();
        let saved_checkpoint = fs::read(&saved_path).unwrap();
        let inventory =
            super::packages::ExtensionManager::new(None, Some(project.path().to_owned()));
        let record = inventory
            .discover_checked()
            .unwrap()
            .into_iter()
            .find(|record| record.name() == NAME)
            .unwrap();
        if remove {
            inventory.remove(&record).unwrap();
        } else {
            inventory.set_enabled(&record, false).unwrap();
        }
        drop(initial);
        let restored = WasiExtensionManager::for_project_session(project.path(), "a");
        assert_eq!(
            restored
                .reload_from_roots(None, Some(project.path()))
                .unwrap(),
            1
        );
        let next = restored
            .execute_tool_with_broker_requests("other", "{}")
            .unwrap()
            .unwrap();
        let next_id = next.host_broker_requests[0].receipt.as_ref().unwrap().id;
        assert!(
            next_id > previous_id,
            "reused receipt {next_id}, previous {previous_id}, remove={remove}"
        );
        assert_eq!(fs::read(&saved_path).unwrap(), saved_checkpoint);
        assert_eq!(
            fs::read(project.path().join("effects.log")).unwrap(),
            b"effect\n"
        );
    }
}

#[test]
fn receipt_recovery_includes_encoded_names_and_excludes_host_and_other_scopes() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    initial.set_extension_state("old/name", json!({})).unwrap();
    let path = initial.state_path("old/name").unwrap();
    let mut checkpoint: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    checkpoint["last_broker_id"] = json!(73);
    fs::write(&path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    initial
        .set_host_state(
            "fixture",
            json!({"$threadlane":"threadlane.extension-checkpoint","version":999}),
        )
        .unwrap();
    let other_scope = WasiExtensionManager::for_project_session(project.path(), "b");
    other_scope
        .set_extension_state("inactive", json!({}))
        .unwrap();
    let other_path = other_scope.state_path("inactive").unwrap();
    checkpoint["last_broker_id"] = json!(u64::MAX);
    fs::write(other_path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let result = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    assert_eq!(
        result.host_broker_requests[0].receipt.as_ref().unwrap().id,
        74
    );
    assert_eq!(
        initial
            .last_broker_id
            .load(std::sync::atomic::Ordering::SeqCst),
        74
    );
}

#[test]
fn malformed_inactive_checkpoint_blocks_allocation_without_publishing_work() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    initial
        .set_extension_state(NAME, json!({"phase":"ready"}))
        .unwrap();
    initial
        .set_host_state("tools.policy", json!("full"))
        .unwrap();
    let current = initial.state_path(NAME).unwrap();
    let before = fs::read(&current).unwrap();
    let inactive = initial.state_path("disabled").unwrap();
    let mut checkpoint: Value = serde_json::from_slice(&before).unwrap();
    checkpoint["version"] = json!(999);
    let malformed = serde_json::to_vec(&checkpoint).unwrap();
    fs::write(&inactive, &malformed).unwrap();
    assert_eq!(
        initial.host_state("tools.policy").unwrap(),
        Some(json!("full"))
    );
    let error = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap_err();
    assert!(error.contains("Cannot allocate broker receipts"), "{error}");
    assert!(error.contains("disabled.json"), "{error}");
    assert!(error.contains("do not retry unchanged"), "{error}");
    assert_eq!(fs::read(&current).unwrap(), before);
    assert_eq!(fs::read(&inactive).unwrap(), malformed);
    assert_eq!(
        initial.extension_state(NAME),
        Some(json!({"phase":"ready"}))
    );
    assert_eq!(
        initial
            .last_broker_id
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    checkpoint["version"] = json!(2);
    checkpoint["last_broker_id"] = json!(99);
    fs::write(&inactive, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let result = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    assert_eq!(
        result.host_broker_requests[0].receipt.as_ref().unwrap().id,
        100
    );
    assert!(!project.path().join("effects.log").exists());
}

#[test]
fn receipt_batch_allocation_is_contiguous_and_exhaustion_commits_no_partial_batch() {
    for exhausted in [false, true] {
        let project = tempfile::tempdir().unwrap();
        let initial = emitting_manager(project.path());
        let module = project.path().join(".threadlane/extensions/probe.wasm");
        let wasm = fs::read_to_string(&module).unwrap();
        let request_line = wasm
            .lines()
            .find(|line| line.contains("(drop (call $request "))
            .unwrap();
        fs::write(
            &module,
            wasm.replace(
                request_line,
                &format!("{request_line}\n{request_line}\n{request_line}"),
            ),
        )
        .unwrap();
        initial
            .reload_from_roots(None, Some(project.path()))
            .unwrap();
        initial
            .set_extension_state(NAME, json!({"phase":"ready"}))
            .unwrap();
        let path = initial.state_path(NAME).unwrap();
        let mut checkpoint: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let floor = if exhausted { u64::MAX - 1 } else { 10 };
        checkpoint["last_broker_id"] = json!(floor);
        let before = serde_json::to_vec(&checkpoint).unwrap();
        fs::write(&path, &before).unwrap();
        let result = initial
            .execute_tool_with_broker_requests("probe", "{}")
            .unwrap();
        if exhausted {
            assert!(result.unwrap_err().contains("ID exhausted"));
            assert_eq!(fs::read(&path).unwrap(), before);
            assert_eq!(
                initial.extension_state(NAME),
                Some(json!({"phase":"ready"}))
            );
            assert_eq!(
                initial
                    .last_broker_id
                    .load(std::sync::atomic::Ordering::SeqCst),
                floor
            );
        } else {
            let result = result.unwrap();
            let ids: Vec<_> = result
                .host_broker_requests
                .iter()
                .map(|request| request.receipt.as_ref().unwrap().id)
                .collect();
            assert_eq!(ids, vec![11, 12, 13]);
            let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["last_broker_id"], json!(13));
            assert_eq!(saved["unsettled"].as_array().unwrap().len(), 3);
            initial
                .enqueue_broker_results(
                    result
                        .host_broker_requests
                        .into_iter()
                        .map(|request| {
                            request.not_dispatched(BrokerError {
                                code: "test".into(),
                                message: "No dispatch".into(),
                            })
                        })
                        .collect(),
                )
                .unwrap();
            assert!(initial
                .execute_tool_with_broker_requests("probe", "{}")
                .unwrap()
                .unwrap()
                .host_broker_requests
                .is_empty());
        }
        assert!(!project.path().join("effects.log").exists());
    }
}

#[test]
fn competing_managers_cannot_erase_committed_broker_intent() {
    let project = tempfile::tempdir().unwrap();
    let first = emitting_manager(project.path());
    let invocation = first
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    let outcomes = dispatch_once(project.path(), &first, invocation.host_broker_requests);
    let path = first.state_path(NAME).unwrap();
    let before = fs::read(&path).unwrap();
    let second = WasiExtensionManager::for_project_session(project.path(), "a");
    let error = second
        .set_extension_state(NAME, json!({"phase":"ready"}))
        .unwrap_err();
    assert!(error.contains("owned"), "{error}");
    assert!(second
        .set_host_state("tools.policy", json!("full"))
        .is_err());
    assert!(second
        .reload_from_roots(None, Some(project.path()))
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
    drop(first);
    // The previous manager's owner claim is gone, but its intent must be
    // hydrated before any write, even if the successor never reloaded modules.
    second
        .set_extension_state(NAME, json!({"phase":"waiting"}))
        .unwrap();
    let checkpoint: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(checkpoint["unsettled"].as_array().unwrap().len(), 1);
    second.enqueue_broker_results(outcomes).unwrap();
    drop(second);
    let settled = emitting_manager(project.path());
    assert!(settled
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap()
        .host_broker_requests
        .is_empty());
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[test]
fn state_owner_blocks_other_processes_and_releases_after_process_death() {
    const PROBE: &str = "THREADLANE_TEST_STATE_OWNER_PROJECT";
    if let Some(project) = std::env::var_os(PROBE) {
        let project = Path::new(&project);
        let owner = emitting_manager(project);
        if std::env::var_os("THREADLANE_TEST_TERMINAL_REPLY").is_some() {
            let identity = reply_identity();
            let mut operation = owner.begin_tool_operation("probe").unwrap().unwrap();
            let invocation = operation.invoke_for_execution("{}", &identity).unwrap();
            owner
                .enqueue_broker_results(dispatch_once(
                    project,
                    &owner,
                    invocation.host_broker_requests,
                ))
                .unwrap();
            operation.invoke_for_execution("{}", &identity).unwrap();
            drop(operation);
            owner
                .prepare_tool_reply(
                    &identity,
                    &threadlane_protocol::AgentToolResult::external(
                        "call",
                        "probe",
                        "done plus hook",
                        false,
                    ),
                )
                .unwrap();
            println!("owner-ready");
            std::io::stdout().flush().unwrap();
            let mut byte = [0];
            std::io::Read::read(&mut std::io::stdin(), &mut byte).unwrap();
            return;
        }
        let invocation = owner
            .execute_tool_with_broker_requests("probe", "{}")
            .unwrap()
            .unwrap();
        let _outcomes = dispatch_once(project, &owner, invocation.host_broker_requests);
        // Cut after physical execution and before persisting its outcome.
        println!("owner-ready");
        std::io::stdout().flush().unwrap();
        let mut byte = [0];
        std::io::Read::read(&mut std::io::stdin(), &mut byte).unwrap();
        drop(owner);
        return;
    }
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let project = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "checkpoint_tests::state_owner_blocks_other_processes_and_releases_after_process_death",
            "--nocapture",
        ])
        .env(PROBE, project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "owner child stopped before committing intent"
        );
        if line.trim() == "owner-ready" {
            break;
        }
    }
    let alias = project.path().join("project-alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(project.path(), &alias).unwrap();
    #[cfg(not(unix))]
    let alias = project.path().join(".");
    let successor = WasiExtensionManager::for_project_session(&alias, "a");
    let error = successor
        .set_host_state("tools.policy", json!("full"))
        .unwrap_err();
    assert!(error.contains("owned"), "{error}");
    let independent = WasiExtensionManager::for_project_session(project.path(), "b");
    independent
        .set_extension_state(NAME, json!({"kept":"b"}))
        .unwrap();
    independent
        .set_host_state("tools.policy", json!("read_only"))
        .unwrap();
    assert!(independent
        .set_session_scope("a")
        .unwrap_err()
        .contains("owned"));
    assert_eq!(
        independent.active_session_scope().unwrap(),
        Some("b".into())
    );
    assert_eq!(independent.extension_state(NAME), Some(json!({"kept":"b"})));
    assert_eq!(
        independent.host_state("tools.policy").unwrap(),
        Some(json!("read_only"))
    );
    let blocked_b = WasiExtensionManager::for_project_session(project.path(), "b");
    assert!(blocked_b
        .set_host_state("tools.policy", json!("full"))
        .is_err());
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    successor.reload_from_roots(None, Some(&alias)).unwrap();
    let error = successor
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap_err();
    assert!(error.contains("may already have executed"), "{error}");
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
    drop(independent);
    blocked_b
        .set_host_state("tools.policy", json!("read_only"))
        .unwrap();
}

#[test]
fn terminal_reply_survives_process_death_and_module_removal() {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let project = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "checkpoint_tests::state_owner_blocks_other_processes_and_releases_after_process_death",
            "--nocapture",
        ])
        .env("THREADLANE_TEST_STATE_OWNER_PROJECT", project.path())
        .env("THREADLANE_TEST_TERMINAL_REPLY", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "child stopped before terminal reply committed"
        );
        if line.trim() == "owner-ready" {
            break;
        }
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    fs::remove_file(project.path().join(".threadlane/extensions/probe.wasm")).unwrap();
    let recovered = WasiExtensionManager::for_project_session(project.path(), "a");
    assert_eq!(
        recovered
            .recover_tool_reply(&reply_identity(), "probe", "{}")
            .unwrap(),
        Some(Ok("done".into()))
    );
    assert_eq!(
        recovered
            .recovered_canonical_reply(&reply_identity())
            .unwrap()
            .unwrap()
            .content,
        "done plus hook"
    );
    recovered.acknowledge_tool_reply(&reply_identity()).unwrap();
    assert!(recovered
        .pending_tool_reply_identities()
        .unwrap()
        .is_empty());
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[test]
fn first_write_hydrates_queued_outcomes_and_rejects_corrupt_checkpoints() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    let invocation = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    let outcomes = dispatch_once(project.path(), &initial, invocation.host_broker_requests);
    initial.enqueue_broker_results(outcomes).unwrap();
    let path = initial.state_path(NAME).unwrap();
    drop(initial);
    let successor = WasiExtensionManager::for_project_session(project.path(), "a");
    successor
        .set_extension_state(NAME, json!({"phase":"waiting"}))
        .unwrap();
    assert_eq!(values(&successor), vec![json!("executed once")]);
    successor
        .reload_from_roots(None, Some(project.path()))
        .unwrap();
    assert_eq!(
        successor
            .execute_tool_with_broker_requests("probe", "{}")
            .unwrap()
            .unwrap()
            .response
            .message
            .as_deref(),
        Some("done")
    );
    drop(successor);
    let mut checkpoint: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    checkpoint["version"] = json!(999);
    let corrupt = serde_json::to_vec(&checkpoint).unwrap();
    fs::write(&path, &corrupt).unwrap();
    let cold = WasiExtensionManager::for_project_session(project.path(), "a");
    let error = cold.set_extension_state(NAME, json!({})).unwrap_err();
    assert!(error.contains("Unsupported"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), corrupt);
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[cfg(unix)]
#[test]
fn owner_lock_symlinks_are_rejected_without_touching_the_target() {
    let project = tempfile::tempdir().unwrap();
    let manager = WasiExtensionManager::for_project_session(project.path(), "a");
    let lock = manager
        .state_path(NAME)
        .unwrap()
        .parent()
        .unwrap()
        .join(".owner.lock");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let external = project.path().join("external");
    fs::write(&external, b"preserved").unwrap();
    std::os::unix::fs::symlink(&external, &lock).unwrap();
    let error = manager
        .set_host_state("tools.policy", json!("full"))
        .unwrap_err();
    assert!(error.contains("owner lock"), "{error}");
    assert_eq!(fs::read(&external).unwrap(), b"preserved");
    assert!(fs::symlink_metadata(&lock)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn operations_keep_call_ownership_after_outcomes_commit() {
    for kind in ["tool", "command", "hook"] {
        let project = tempfile::tempdir().unwrap();
        let initial = emitting_manager(project.path());
        let mut operation = match kind {
            "tool" => initial.begin_tool_operation("probe").unwrap().unwrap(),
            "command" => initial
                .begin_command_operation("probe_command")
                .unwrap()
                .unwrap(),
            _ => initial
                .begin_hook_operations("before_tool_call")
                .next()
                .unwrap()
                .unwrap(),
        };
        let invocation = operation.invoke("{}").unwrap();
        let outcomes = dispatch_once(project.path(), &initial, invocation.host_broker_requests);
        initial.enqueue_broker_results(outcomes).unwrap();
        // This is the gap between result persistence and the owner's next VM.
        // Neither event delivery nor unsettled receipts still own the slot.
        assert!(initial.in_flight_events.lock().unwrap().is_empty());
        assert!(initial.unsettled_broker.lock().unwrap()[NAME].is_empty());
        let path = initial.state_path(NAME).unwrap();
        let checkpoint = fs::read(&path).unwrap();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    assert!(initial
                        .begin_tool_operation("probe")
                        .unwrap()
                        .err()
                        .unwrap()
                        .contains("active call"));
                    assert!(initial
                        .begin_command_operation("probe_command")
                        .unwrap()
                        .is_err());
                    assert!(initial
                        .begin_hook_operations("before_tool_call")
                        .next()
                        .unwrap()
                        .is_err());
                    assert!(initial
                        .execute_tool_with_broker_requests("probe", "{}")
                        .unwrap()
                        .unwrap_err()
                        .contains("active call"));
                    assert!(initial
                        .execute_command_with_effects("probe_command", "{}")
                        .unwrap()
                        .is_err());
                    assert!(
                        initial.execute_hook_with_broker_requests("before_tool_call", "{}")[0]
                            .is_err()
                    );
                    assert!(initial
                        .reload_from_roots(None, Some(project.path()))
                        .is_err());
                    assert!(initial.set_session_scope("b").is_err());
                })
                .join()
                .unwrap();
        });
        assert_eq!(fs::read(&path).unwrap(), checkpoint);
        assert_eq!(values(&initial), vec![json!("executed once")]);
        let done = operation.invoke("{}").unwrap();
        assert_eq!(done.response.message.as_deref(), Some("done"));
        assert!(done.host_broker_requests.is_empty());
        assert!(initial.begin_tool_operation("probe").unwrap().is_err());
        drop(operation);
        drop(initial.begin_tool_operation("probe").unwrap().unwrap());
        initial
            .reload_from_roots(None, Some(project.path()))
            .unwrap();
        assert_eq!(
            initial.extension_state(NAME),
            Some(json!({"phase":"ready"}))
        );
        assert!(values(&initial).is_empty());
        initial.set_session_scope("b").unwrap();
        assert_eq!(
            fs::read(project.path().join("effects.log")).unwrap(),
            b"effect\n"
        );
    }
}

#[test]
fn call_ownership_is_per_extension_and_hook_claims_are_lazy() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    install_other_module(project.path());
    assert_eq!(
        initial
            .reload_from_roots(None, Some(project.path()))
            .unwrap(),
        2
    );
    let hooks = initial.begin_hook_operations("before_tool_call");
    assert!(initial.active_operations.lock().unwrap().is_empty());
    let first_hook = hooks.into_iter().next().unwrap().unwrap();
    assert_eq!(first_hook.extension.manifest.name, "checkpoint_other");
    let owner = initial.begin_tool_operation("probe").unwrap().unwrap();
    assert_eq!(initial.active_operations.lock().unwrap().len(), 2);
    drop(first_hook);
    let invocation = initial
        .execute_tool_with_broker_requests("other", "{}")
        .unwrap()
        .unwrap();
    initial
        .enqueue_broker_results(
            invocation
                .host_broker_requests
                .into_iter()
                .map(|request| {
                    request.not_dispatched(BrokerError {
                        code: "test".into(),
                        message: "No physical dispatch".into(),
                    })
                })
                .collect(),
        )
        .unwrap();
    assert_eq!(
        initial
            .execute_tool_with_broker_requests("other", "{}")
            .unwrap()
            .unwrap()
            .response
            .message
            .as_deref(),
        Some("done")
    );
    assert_eq!(initial.active_operations.lock().unwrap().len(), 1);
    drop(owner);
    initial
        .reload_from_roots(None, Some(project.path()))
        .unwrap();
}

#[test]
fn failed_invocation_restores_delivery_and_releases_ownership_when_dropped() {
    let project = tempfile::tempdir().unwrap();
    let initial = manager(project.path(), true);
    initial
        .enqueue_broker_results(vec![outcome("preserved")])
        .unwrap();
    let checkpoint = fs::read(initial.state_path(NAME).unwrap()).unwrap();
    let mut operation = initial.begin_tool_operation("probe").unwrap().unwrap();
    assert!(operation.invoke("{}").is_err());
    assert_eq!(values(&initial), vec![json!("preserved")]);
    assert!(initial.in_flight_events.lock().unwrap().is_empty());
    assert!(initial.begin_tool_operation("probe").unwrap().is_err());
    drop(operation);
    assert!(initial.active_operations.lock().unwrap().is_empty());
    initial
        .reload_from_roots(None, Some(project.path()))
        .unwrap();
    assert_eq!(
        fs::read(initial.state_path(NAME).unwrap()).unwrap(),
        checkpoint
    );
    assert_eq!(values(&initial), vec![json!("preserved")]);
}

#[test]
fn dropping_an_operation_keeps_unknown_receipts_blocked() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    let mut operation = initial.begin_tool_operation("probe").unwrap().unwrap();
    let invocation = operation.invoke("{}").unwrap();
    drop(operation);
    assert!(initial.active_operations.lock().unwrap().is_empty());
    let mut recovery = initial.begin_tool_operation("probe").unwrap().unwrap();
    assert!(recovery
        .invoke("{}")
        .unwrap_err()
        .contains("may already have executed"));
    let outcomes = invocation
        .host_broker_requests
        .into_iter()
        .map(|request| {
            request.not_dispatched(BrokerError {
                code: "cancelled".into(),
                message: "Owner knows dispatch never began".into(),
            })
        })
        .collect();
    initial.enqueue_broker_results(outcomes).unwrap();
    assert!(recovery
        .invoke("{}")
        .unwrap()
        .host_broker_requests
        .is_empty());
    drop(recovery);
    initial
        .reload_from_roots(None, Some(project.path()))
        .unwrap();
    assert!(!project.path().join("effects.log").exists());
}

#[test]
fn intent_precedes_physical_execution_and_unknown_outcomes_never_replay() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    let invocation = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    assert_eq!(invocation.host_broker_requests.len(), 1);
    let id = invocation.host_broker_requests[0]
        .receipt
        .as_ref()
        .unwrap()
        .id;
    let outcomes = dispatch_once(project.path(), &initial, invocation.host_broker_requests);
    drop(initial); // Cut after the side effect, before its outcome commit.
    let recovered = emitting_manager(project.path());
    let error = recovered
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap_err();
    assert!(error.contains("may already have executed"), "{error}");
    assert!(error.contains("do not retry or replay"), "{error}");
    assert!(error.contains(".json"), "{error}");
    assert!(recovered.set_session_scope("b").is_err());
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
    // Reconcile the surviving original result, rather than dispatching again.
    recovered.enqueue_broker_results(outcomes).unwrap();
    assert!(recovered
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap()
        .host_broker_requests
        .is_empty());
    drop(recovered);
    let settled = emitting_manager(project.path());
    assert_eq!(
        settled.extension_state(NAME),
        Some(json!({"phase":"ready"}))
    );
    let next = settled
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    assert!(next.host_broker_requests[0].receipt.as_ref().unwrap().id > id);
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[test]
fn commands_keep_receipts_and_duplicate_or_mismatched_results_are_rejected() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    let invocation = initial
        .execute_command_with_effects("probe_command", "{}")
        .unwrap()
        .unwrap();
    assert!(invocation.host_broker_requests[0].receipt.is_some());
    let outcomes = dispatch_once(project.path(), &initial, invocation.host_broker_requests);
    let mut wrong = outcomes.clone();
    wrong[0].request.arguments = json!({"command":"another effect"});
    assert!(initial.enqueue_broker_results(wrong).is_err());
    assert!(values(&initial).is_empty());
    let duplicate = outcomes.clone();
    initial.enqueue_broker_results(outcomes).unwrap();
    assert!(initial.enqueue_broker_results(duplicate).is_err());
    assert_eq!(values(&initial), vec![json!("executed once")]);
    initial
        .execute_command_with_effects("probe_command", "{}")
        .unwrap()
        .unwrap();
    drop(initial);
    assert!(values(&emitting_manager(project.path())).is_empty());
}

#[test]
fn known_outcomes_recommit_after_storage_failure_without_rerunning_effects() {
    let project = tempfile::tempdir().unwrap();
    let mut initial = emitting_manager(project.path());
    let invocation = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    let outcomes = dispatch_once(project.path(), &initial, invocation.host_broker_requests);
    let original_root = initial.state_dir.clone();
    let blocked = project.path().join("blocked");
    fs::write(&blocked, b"not a directory").unwrap();
    initial.state_dir = Some(blocked.join("state"));
    assert!(initial
        .enqueue_broker_results(outcomes)
        .unwrap_err()
        .contains("do not repeat"));
    initial.state_dir = original_root;
    assert!(initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap()
        .host_broker_requests
        .is_empty());
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
    drop(initial);
    assert!(values(&emitting_manager(project.path())).is_empty());
}

#[test]
fn duplicate_outcome_after_failed_commit_is_rejected_before_recovery() {
    let project = tempfile::tempdir().unwrap();
    let mut initial = emitting_manager(project.path());
    let invocation = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    let outcomes = dispatch_once(project.path(), &initial, invocation.host_broker_requests);
    let duplicate = outcomes.clone();
    let original_root = initial.state_dir.clone();
    let blocked = project.path().join("blocked");
    fs::write(&blocked, b"not a directory").unwrap();
    initial.state_dir = Some(blocked.join("state"));
    assert!(initial.enqueue_broker_results(outcomes).is_err());
    initial.state_dir = original_root;
    let error = initial.enqueue_broker_results(duplicate).unwrap_err();
    assert!(error.contains("duplicate"), "{error}");
    assert_eq!(values(&initial), vec![json!("executed once")]);
    {
        let _commit = initial.state_commit.lock().unwrap();
        initial
            .commit_retained_outcomes(NAME, &initial.extension_state(NAME).unwrap(), true)
            .unwrap();
    }
    drop(initial);
    let recovered = emitting_manager(project.path());
    assert_eq!(values(&recovered), vec![json!("executed once")]);
    assert!(recovered
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap()
        .host_broker_requests
        .is_empty());
    assert_eq!(
        fs::read(project.path().join("effects.log")).unwrap(),
        b"effect\n"
    );
}

#[test]
fn a_cut_before_dispatch_is_also_uncertain_until_the_owner_resolves_it() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    let invocation = initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    drop(initial);
    let recovered = emitting_manager(project.path());
    assert!(recovered
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap_err()
        .contains("do not retry or replay"));
    assert!(!project.path().join("effects.log").exists());
    // The original owner still holds the envelope and knows dispatch never began.
    let cancelled = invocation
        .host_broker_requests
        .into_iter()
        .map(|request| {
            request.not_dispatched(BrokerError {
                code: "cancelled_before_dispatch".into(),
                message: "Owner cancelled before starting execution".into(),
            })
        })
        .collect();
    recovered.enqueue_broker_results(cancelled).unwrap();
    assert!(recovered
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap()
        .host_broker_requests
        .is_empty());
    assert!(!project.path().join("effects.log").exists());
}

#[test]
fn corrupt_or_cross_session_receipts_are_rejected_without_loading_state() {
    let project = tempfile::tempdir().unwrap();
    let initial = emitting_manager(project.path());
    initial
        .execute_tool_with_broker_requests("probe", "{}")
        .unwrap()
        .unwrap();
    let path = initial.state_path(NAME).unwrap();
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for mutation in 0..3 {
        let mut value = original.clone();
        match mutation {
            0 => value["unsettled"][0]["receipt"]["id"] = json!(0),
            1 => value["unsettled"][0]["receipt"]["scope"] = json!("other-session"),
            _ => {
                let duplicate = value["unsettled"][0].clone();
                value["unsettled"].as_array_mut().unwrap().push(duplicate);
            }
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let candidate = WasiExtensionManager::for_project_session(project.path(), "a");
        assert!(candidate
            .reload_from_roots(None, Some(project.path()))
            .is_err());
        assert!(candidate.extension_state(NAME).is_none());
        assert!(candidate.extension_manifest(NAME).is_none());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
            value
        );
    }
}

#[test]
fn version_one_checkpoint_files_remain_readable() {
    let project = tempfile::tempdir().unwrap();
    let initial = manager(project.path(), false);
    initial
        .set_extension_state(NAME, json!({"phase":"waiting"}))
        .unwrap();
    initial.enqueue_broker_results(vec![outcome("v1")]).unwrap();
    let path = initial.state_path(NAME).unwrap();
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["version"] = json!(1);
    value.as_object_mut().unwrap().remove("unsettled");
    value.as_object_mut().unwrap().remove("last_broker_id");
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    drop(initial);
    let recovered = manager(project.path(), false);
    assert_eq!(
        recovered.extension_state(NAME),
        Some(json!({"phase":"waiting"}))
    );
    assert_eq!(values(&recovered), vec![json!("v1")]);
}
