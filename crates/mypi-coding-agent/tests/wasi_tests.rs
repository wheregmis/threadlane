use mypi_coding_agent::{
    WasiExtension, WasiExtensionEffect, WasiExtensionManager, WasiExtensionManifest,
    WasiToolDefinition,
};
use std::path::PathBuf;
use tempfile::tempdir;

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
fn test_wasi_manifest_serde() {
    let manifest = WasiExtensionManifest {
        api_version: 1,
        name: "test_ext".into(),
        version: "1.0.0".into(),
        description: "Test extension".into(),
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
