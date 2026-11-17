use super::{CodingAgent, CodingAgentOptions};
use std::path::Path;
use std::sync::{Arc, Mutex};
use threadlane_prompt::SystemPromptConfig;
use threadlane_protocol::browser::BrowserBridge;

pub(crate) fn install_fixture(project: &Path) {
    let manifest = serde_json::json!({"api_version":2,"name":"receipt_probe","version":"1","description":"test","capabilities":["tools"],"commands":[{"name":"receipt_probe","description":"test"}],"hooks":["before_tool_call"]}).to_string();
    let pending = r#"{"state":{"phase":"waiting"},"message":"waiting"}"#;
    let done = r#"{"state":{"phase":"ready"},"message":"done"}"#;
    let request =
        r#"{"api_version":2,"capability":"tools","operation":"get_policy","arguments":{}}"#;
    let escape = |value: &str| value.replace('"', "\\\"");
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
        (func (export "execute_command") (export "handle_hook") (param $ptr i32) (param $len i32) (result i64)
            (local $end i32)
            (local.set $end (i32.sub (i32.add (local.get $ptr) (local.get $len)) (i32.const 8)))
            (block $scanned (loop $scan
                (br_if $scanned (i32.gt_u (local.get $ptr) (local.get $end)))
                (if (i64.eq (i64.load align=1 (local.get $ptr)) (i64.const 0x725f72656b6f7262))
                    (then (return (i64.const {}))))
                (local.set $ptr (i32.add (local.get $ptr) (i32.const 1)))
                (br $scan)))
            (drop (call $request (i32.const 3072) (i32.const {}) (i32.const 4096) (i32.const 1024)))
            (drop (call $request (i32.const 3072) (i32.const {}) (i32.const 4096) (i32.const 1024)))
            (i64.const {})))"#,
        escape(&manifest),
        escape(pending),
        escape(done),
        escape(request),
        manifest.len(),
        (2048u64 << 32) | done.len() as u64,
        request.len(),
        request.len(),
        (1024u64 << 32) | pending.len() as u64,
    );
    let path = project.join(".threadlane/extensions/receipt_probe.wasm");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, wasm).unwrap();
}

#[tokio::test]
async fn command_startup_failure_does_not_invoke_or_leave_broker_intents() {
    let directory = tempfile::tempdir().unwrap();
    install_fixture(directory.path());
    let mut agent = CodingAgent::new(CodingAgentOptions {
        api_key: "test".into(),
        account_id: None,
        model: "test-model".into(),
        work_dir: directory.path().to_owned(),
        session_file: Some(directory.path().join("session.jsonl")),
        system_prompt: SystemPromptConfig::default(),
        agent_config: None,
        coding_config: None,
        browser: BrowserBridge::unavailable(),
    });
    let before = agent.wasi_extensions.extension_state("receipt_probe");
    // Fail precisely at harness startup, after the command has been selected.
    let run_state = agent.harness_run_id.clone();
    let _ = std::panic::catch_unwind(move || {
        let _guard = run_state.lock().unwrap();
        panic!("simulate unavailable harness run state");
    });
    let error = agent
        .handle_input_with_images("/receipt_probe", vec![])
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        error.contains("Harness run state is unavailable"),
        "{error}"
    );
    assert_eq!(
        agent.wasi_extensions.extension_state("receipt_probe"),
        before
    );
    agent.harness_run_id = Arc::new(Mutex::new(None));
    // Neither an active extension claim nor an unsettled receipt survives.
    agent
        .wasi_extensions
        .reload_from_roots(None, Some(directory.path()))
        .unwrap();
    agent
        .wasi_extensions
        .set_session_scope("another-session")
        .unwrap();
}
