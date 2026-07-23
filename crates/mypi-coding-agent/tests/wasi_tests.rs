use mypi_coding_agent::{
    BrokerError, BrokerRequest, CapabilityDispatcher, CapabilityHandler, CapabilityPolicy,
    WasiExtension, WasiExtensionEffect, WasiExtensionManager, WasiExtensionManifest,
    WasiToolDefinition,
};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

fn build_broker_smoke_extension(agent_only: bool) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target_dir = root.join(if agent_only {
        "target/broker-smoke-agent"
    } else {
        "target/broker-smoke-tools"
    });
    let mut command = Command::new("cargo");
    command
        .current_dir(&root)
        .args([
            "build",
            "--manifest-path",
            "extensions/broker_smoke_ext/Cargo.toml",
            "--target",
            "wasm32-wasip1",
            "--target-dir",
        ])
        .arg(&target_dir);
    if agent_only {
        command.args(["--features", "agent-only"]);
    }
    assert!(command.status().unwrap().success());
    target_dir.join("wasm32-wasip1/debug/broker_smoke_ext.wasm")
}

#[test]
fn broker_import_queues_accepted_requests_and_returns_denials_to_the_extension() {
    let extension = WasiExtension::load_from_file(&build_broker_smoke_extension(false)).unwrap();
    let mut manager = WasiExtensionManager::new();
    manager
        .extensions
        .insert(extension.manifest.name.clone(), extension);

    let result = manager
        .execute_command_with_effects("broker-smoke", "")
        .unwrap()
        .unwrap();
    assert!(result.message.contains("broker accepted tools.set_policy"));
    assert_eq!(result.broker_requests.len(), 1);
    assert_eq!(result.broker_requests[0].capability, "tools");
    assert_eq!(result.broker_requests[0].operation, "set_policy");

    let malformed = manager
        .execute_command_with_effects("broker-smoke", r#"{"mode":"malformed"}"#)
        .unwrap()
        .unwrap();
    assert!(malformed.message.contains("invalid_request"));
    assert!(malformed.broker_requests.is_empty());

    let too_small = manager
        .execute_command_with_effects("broker-smoke", r#"{"mode":"small-output"}"#)
        .unwrap()
        .unwrap();
    assert!(too_small.message.contains("broker response too large"));
    assert_eq!(too_small.broker_requests.len(), 1);

    let denied = WasiExtension::load_from_file(&build_broker_smoke_extension(true)).unwrap();
    let mut manager = WasiExtensionManager::new();
    manager
        .extensions
        .insert(denied.manifest.name.clone(), denied);
    let result = manager
        .execute_command_with_effects("broker-smoke", "")
        .unwrap()
        .unwrap();
    assert!(result.message.contains("capability_denied"));
    assert!(result.broker_requests.is_empty());
}

fn push_unsigned_leb(mut value: u32, bytes: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn push_signed_leb(mut value: i64, bytes: &mut Vec<u8>) {
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        bytes.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

fn push_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
    wasm.push(id);
    push_unsigned_leb(payload.len() as u32, wasm);
    wasm.extend_from_slice(payload);
}

fn manifest_wasm(json: &str) -> Vec<u8> {
    let mut wasm = b"\0asm\x01\0\0\0".to_vec();

    push_section(&mut wasm, 1, &[1, 0x60, 0, 1, 0x7e]);
    push_section(&mut wasm, 3, &[1, 0]);
    push_section(&mut wasm, 5, &[1, 0, 1]);

    let mut exports = vec![2];
    push_unsigned_leb("extension_info".len() as u32, &mut exports);
    exports.extend_from_slice(b"extension_info");
    exports.extend_from_slice(&[0, 0]);
    push_unsigned_leb("memory".len() as u32, &mut exports);
    exports.extend_from_slice(b"memory");
    exports.extend_from_slice(&[2, 0]);
    push_section(&mut wasm, 7, &exports);

    let mut body = vec![0, 0x42];
    push_signed_leb(json.len() as i64, &mut body);
    body.push(0x0b);
    let mut code = vec![1];
    push_unsigned_leb(body.len() as u32, &mut code);
    code.extend_from_slice(&body);
    push_section(&mut wasm, 10, &code);

    let mut data = vec![1, 0, 0x41, 0, 0x0b];
    push_unsigned_leb(json.len() as u32, &mut data);
    data.extend_from_slice(json.as_bytes());
    push_section(&mut wasm, 11, &data);
    wasm
}

#[test]
fn broker_import_rejects_out_of_bounds_huge_request_without_allocating() {
    let extension = WasiExtension::load_from_file(&build_broker_smoke_extension(false)).unwrap();
    let mut manager = WasiExtensionManager::new();
    manager
        .extensions
        .insert(extension.manifest.name.clone(), extension);

    let result = manager
        .execute_command_with_effects("broker-smoke", r#"{"mode":"huge-length"}"#)
        .unwrap()
        .unwrap();
    assert_eq!(result.message, "broker invalid range");
    assert!(result.broker_requests.is_empty());
}

struct RecordingCapabilityHandler {
    recorded: Arc<Mutex<Vec<(String, String)>>>,
}

impl CapabilityHandler for RecordingCapabilityHandler {
    fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        self.recorded
            .lock()
            .unwrap()
            .push((request.capability.clone(), request.operation.clone()));
        if request.operation == "unsupported" {
            return Err(BrokerError {
                code: "unknown_operation".into(),
                message: "unsupported operation".into(),
            });
        }
        Ok(Value::Null)
    }
}

#[tokio::test]
async fn broker_dispatch_routes_capability_operations_in_order() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut dispatcher = CapabilityDispatcher::new();
    dispatcher.register(
        "tools",
        Arc::new(RecordingCapabilityHandler {
            recorded: recorded.clone(),
        }),
    );
    let result = dispatcher
        .dispatch(vec![BrokerRequest {
            api_version: 2,
            capability: "tools".into(),
            operation: "set_policy".into(),
            arguments: serde_json::json!({"policy":"read_only"}),
        }])
        .await
        .unwrap();
    assert_eq!(
        *recorded.lock().unwrap(),
        vec![("tools".into(), "set_policy".into())]
    );
    assert!(result.message.is_none());
}

#[tokio::test]
async fn broker_dispatch_returns_unknown_operation_without_panicking() {
    let mut dispatcher = CapabilityDispatcher::new();
    dispatcher.register(
        "tools",
        Arc::new(RecordingCapabilityHandler {
            recorded: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    let error = dispatcher
        .dispatch(vec![BrokerRequest {
            api_version: 2,
            capability: "tools".into(),
            operation: "unsupported".into(),
            arguments: Value::Null,
        }])
        .await
        .unwrap_err();
    assert_eq!(error.code, "unknown_operation");
}

#[tokio::test]
async fn broker_dispatch_returns_unknown_capability_structured_error() {
    let mut dispatcher = CapabilityDispatcher::new();
    let error = dispatcher
        .dispatch(vec![BrokerRequest {
            api_version: 2,
            capability: "missing".into(),
            operation: "anything".into(),
            arguments: Value::Null,
        }])
        .await
        .unwrap_err();
    assert_eq!(error.code, "unknown_capability");
}

#[test]
fn broker_request_round_trips_and_requires_v2() {
    let request: BrokerRequest = serde_json::from_str(
        r#"{"api_version":2,"capability":"tools","operation":"set_policy","arguments":{"policy":"read_only"}}"#,
    )
    .unwrap();
    assert_eq!(request.api_version, 2);
    assert_eq!(request.capability, "tools");
    assert_eq!(request.operation, "set_policy");
}

#[test]
fn capability_policy_rejects_undeclared_capabilities() {
    let policy = CapabilityPolicy::new(["agent"]);
    assert!(policy.allows("agent"));
    assert!(!policy.allows("tools"));
    let response = policy.denied_response("tools");
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "capability_denied");
}

#[test]
fn test_wasi_extension_manager_discovery() {
    let dir = tempdir().unwrap();
    let ext_dir = dir.path().join("extensions");
    std::fs::create_dir_all(&ext_dir).unwrap();

    let mut manager = WasiExtensionManager::new();
    let count = manager.discover_and_load(dir.path());
    assert_eq!(count, 0);
}

#[test]
fn v1_manifest_defaults_to_no_capabilities() {
    let manifest: WasiExtensionManifest =
        serde_json::from_str(r#"{"api_version":1,"name":"old","version":"1","description":"old"}"#)
            .unwrap();
    assert!(manifest.capabilities.is_empty());
}

#[test]
fn v2_manifest_preserves_declared_capabilities() {
    let manifest: WasiExtensionManifest = serde_json::from_str(
        r#"{"api_version":2,"name":"new","version":"1","description":"new","capabilities":["tools","agent"]}"#,
    )
    .unwrap();
    assert_eq!(manifest.capabilities, vec!["tools", "agent"]);
}

#[test]
fn load_from_bytes_rejects_unknown_manifest_api_version() {
    let result = WasiExtension::load_from_bytes(manifest_wasm(
        r#"{"api_version":99,"name":"unknown","version":"1","description":"unknown"}"#,
    ));
    let error = match result {
        Ok(_) => panic!("unknown API version was accepted"),
        Err(error) => error,
    };
    assert_eq!(error, "Unsupported extension API version: 99");
}

#[test]
fn loaded_v1_extension_has_no_capability_grants() {
    let extension = WasiExtension::load_from_bytes(manifest_wasm(
        r#"{"api_version":1,"name":"old","version":"1","description":"old","capabilities":["tools"]}"#,
    ))
    .unwrap();
    let policy = extension.capability_policy();
    assert!(!policy.allows("tools"));
    assert!(!policy.allows("agent"));
}

#[test]
fn loaded_v2_extension_has_declared_capability_grants() {
    let extension = WasiExtension::load_from_bytes(manifest_wasm(
        r#"{"api_version":2,"name":"new","version":"1","description":"new","capabilities":["tools","agent"]}"#,
    ))
    .unwrap();
    let policy = extension.capability_policy();
    assert!(policy.allows("tools"));
    assert!(policy.allows("agent"));
    assert!(!policy.allows("filesystem"));
}

#[test]
fn test_wasi_manifest_serde() {
    let manifest = WasiExtensionManifest {
        api_version: 1,
        name: "test_ext".into(),
        version: "1.0.0".into(),
        description: "Test extension".into(),
        capabilities: vec![],
        tools: vec![WasiToolDefinition {
            name: "test_tool".into(),
            description: "A test tool".into(),
            parameters: serde_json::json!({"type": "object"}),
        }],
        commands: vec![],
        hooks: vec![],
    };

    let serialized = serde_json::to_string(&manifest).unwrap();
    let deserialized: WasiExtensionManifest = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.name, "test_ext");
    assert_eq!(deserialized.tools.len(), 1);
}

#[test]
fn test_wasi_minimal_wasm_bytes() {
    let wasm_bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    let ext = WasiExtension::load_from_bytes(wasm_bytes).unwrap();
    assert_eq!(ext.manifest.name, "unnamed_wasi_ext");
}

#[test]
fn test_load_actual_plan_mode_wasm() {
    let wasm_path = PathBuf::from(
        "/Users/wheregmis/Documents/exploration/mypi/.mypi/extensions/plan_mode_ext/extension.wasm",
    );
    if wasm_path.exists() {
        let ext = WasiExtension::load_from_file(&wasm_path);
        println!("Load result: {:?}", ext.as_ref().map(|e| &e.manifest));
        assert!(ext.is_ok(), "Failed to load WASM: {:?}", ext.err());
    }
}

#[test]
fn test_extension_command_state_is_host_managed() {
    let wasm_path = PathBuf::from(
        "/Users/wheregmis/Documents/exploration/mypi/.mypi/extensions/plan_mode_ext/extension.wasm",
    );
    if !wasm_path.exists() {
        return;
    }

    let extension = WasiExtension::load_from_file(&wasm_path).unwrap();
    let mut manager = WasiExtensionManager::new();
    manager
        .extensions
        .insert(extension.manifest.name.clone(), extension);

    let enabled = manager
        .execute_command_with_effects("plan", "improve extension support")
        .unwrap()
        .unwrap();
    assert!(enabled.message.contains("ENABLED"));
    assert!(enabled.effects.iter().any(|effect| matches!(
        effect,
        WasiExtensionEffect::SetToolPolicy { policy } if policy == "read_only"
    )));
    assert!(enabled
        .effects
        .iter()
        .any(|effect| matches!(effect, WasiExtensionEffect::RequestModelTurn { .. })));

    let hook_results = manager.execute_hook(
        "assistant_message",
        r#"{"content":"Plan:\n1. Inspect the extension boundary\n2. Add hook tests"}"#,
    );
    assert_eq!(hook_results.len(), 1);
    assert!(hook_results.into_iter().all(|result| result.is_ok()));

    let status = manager.execute_command("todos", "").unwrap().unwrap();
    assert!(status.contains("1. Inspect the extension boundary"));
    assert!(status.contains("2. Add hook tests"));

    let disabled = manager.execute_command("plan", "").unwrap().unwrap();
    assert!(disabled.contains("DISABLED"));
}

#[test]
fn test_session_state_paths_are_isolated_and_filesystem_safe() {
    let project = tempdir().unwrap();
    let first =
        WasiExtensionManager::session_state_path(project.path(), "session/one", "plan_mode_ext");
    let second =
        WasiExtensionManager::session_state_path(project.path(), "session/two", "plan_mode_ext");

    assert_ne!(first, second);
    assert!(first.ends_with("plan_mode_ext.json"));
    assert!(first.starts_with(project.path().join(".mypi/state/extensions/sessions")));
}

#[test]
fn test_extension_state_persists_in_project_mypi_directory() {
    let source = PathBuf::from(
        "/Users/wheregmis/Documents/exploration/mypi/.mypi/extensions/plan_mode_ext/extension.wasm",
    );
    if !source.exists() {
        return;
    }

    let project = tempdir().unwrap();
    let package_dir = project.path().join(".mypi/extensions/plan_mode_ext");
    std::fs::create_dir_all(&package_dir).unwrap();
    std::fs::copy(source, package_dir.join("extension.wasm")).unwrap();

    let mut first = WasiExtensionManager::for_project(project.path());
    assert_eq!(first.discover_and_load(project.path()), 1);
    first.execute_command("plan", "").unwrap().unwrap();

    let mut reloaded = WasiExtensionManager::for_project(project.path());
    assert_eq!(reloaded.discover_and_load(project.path()), 1);
    let status = reloaded.execute_command("todos", "").unwrap().unwrap();
    assert!(status.contains("No plan items yet"));
    assert!(project
        .path()
        .join(".mypi/state/extensions/plan_mode_ext.json")
        .exists());
}

#[test]
fn test_extension_state_is_scoped_to_a_session() {
    let source = PathBuf::from(
        "/Users/wheregmis/Documents/exploration/mypi/.mypi/extensions/plan_mode_ext/extension.wasm",
    );
    if !source.exists() {
        return;
    }

    let project = tempdir().unwrap();
    let package_dir = project.path().join(".mypi/extensions/plan_mode_ext");
    std::fs::create_dir_all(&package_dir).unwrap();
    std::fs::copy(source, package_dir.join("extension.wasm")).unwrap();

    let mut first = WasiExtensionManager::for_project_session(project.path(), "session_a");
    assert_eq!(first.discover_and_load(project.path()), 1);
    first.execute_command("plan", "").unwrap().unwrap();

    let mut second = WasiExtensionManager::for_project_session(project.path(), "session_b");
    assert_eq!(second.discover_and_load(project.path()), 1);
    let second_status = second.execute_command("todos", "").unwrap().unwrap();
    assert!(second_status.contains("disabled"));

    let mut restored = WasiExtensionManager::for_project_session(project.path(), "session_a");
    assert_eq!(restored.discover_and_load(project.path()), 1);
    let restored_status = restored.execute_command("todos", "").unwrap().unwrap();
    assert!(restored_status.contains("No plan items yet"));
}
