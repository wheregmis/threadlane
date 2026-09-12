//! Guards that MCP servers are started once and reused across tool calls.
//!
//! Asserts on the number of processes actually started rather than on timing,
//! so it fails for the right reason on a loaded CI machine.
//!
//! The stubs are `/bin/sh` scripts, so the file is Unix-only. The behaviour
//! under test is platform-independent; only the way the fake server is written
//! is not.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use threadlane_session::mcp::{
    McpManager, McpScope, McpServerConfig, McpSettings, McpToolExecutor, McpTransport,
};
use threadlane_session::ToolExecutor;

/// Writes a stub server that appends one line to `spawn_log` each time it starts.
fn stub_server(dir: &Path, spawn_log: &Path) -> PathBuf {
    let path = dir.join("stub_mcp.sh");
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
echo started >> "{log}"
while IFS= read -r line; do
  rest=${{line#*\"id\":}}
  id=${{rest%%,*}}
  case "$line" in
    *'"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":"2024-11-05","capabilities":{{}}}}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"tools":[{{"name":"echo","description":"echo","inputSchema":{{"type":"object"}}}}]}}}}\n' "$id" ;;
    *'"tools/call"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[{{"type":"text","text":"ok"}}]}}}}\n' "$id" ;;
  esac
done
"#,
            log = spawn_log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn spawn_count(spawn_log: &Path) -> usize {
    std::fs::read_to_string(spawn_log)
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0)
}

#[tokio::test]
async fn tool_calls_reuse_one_server_process() {
    let dir = tempfile::tempdir().unwrap();
    let spawn_log = dir.path().join("spawns.log");
    let script = stub_server(dir.path(), &spawn_log);

    McpSettings::save_global(
        dir.path(),
        &[McpServerConfig {
            id: "stub".to_string(),
            name: "Stub".to_string(),
            transport: McpTransport::Stdio {
                command: script.to_string_lossy().to_string(),
                args: Vec::new(),
                env: HashMap::new(),
            },
            enabled: true,
            scope: McpScope::Global,
        }],
    )
    .unwrap();

    let manager = Arc::new(McpManager::new(Some(dir.path().to_path_buf()), None));
    let records = manager.discover_and_connect().await;
    assert_eq!(records.len(), 1);
    assert_eq!(
        spawn_count(&spawn_log),
        1,
        "discovery should start the server exactly once"
    );
    manager.discover_and_connect().await;
    assert_eq!(
        spawn_count(&spawn_log),
        1,
        "unchanged discovery should reuse the live server"
    );

    let executor = McpToolExecutor::new(Arc::clone(&manager));
    for call in 0..5 {
        let result = executor.execute_tool("mcp__stub__echo", "{}").await;
        assert!(
            matches!(result, Some(Ok(ref text)) if text == "ok"),
            "call {call} should succeed, got {result:?}"
        );
    }

    // The whole point of the change: five calls, still one process.
    assert_eq!(
        spawn_count(&spawn_log),
        1,
        "tool calls must reuse the running server rather than respawning it"
    );

    manager.shutdown().await;
}

#[tokio::test]
async fn changed_config_restarts_server_process() {
    let dir = tempfile::tempdir().unwrap();
    let spawn_log = dir.path().join("spawns.log");
    let script = stub_server(dir.path(), &spawn_log);
    let config = |name: &str| McpServerConfig {
        id: "stub".to_string(),
        name: name.to_string(),
        transport: McpTransport::Stdio {
            command: script.to_string_lossy().to_string(),
            args: Vec::new(),
            env: HashMap::new(),
        },
        enabled: true,
        scope: McpScope::Global,
    };
    McpSettings::save_global(dir.path(), &[config("Before")]).unwrap();
    let manager = McpManager::new(Some(dir.path().to_path_buf()), None);
    manager.discover_and_connect().await;

    McpSettings::save_global(dir.path(), &[config("After")]).unwrap();
    manager.discover_and_connect().await;

    assert_eq!(spawn_count(&spawn_log), 2);
    manager.shutdown().await;
}

#[tokio::test]
async fn a_dead_server_is_restarted_on_the_next_call() {
    let dir = tempfile::tempdir().unwrap();
    let spawn_log = dir.path().join("spawns.log");
    // This stub exits after answering the handshake and one listing, so the
    // first tool call finds a dead pipe.
    let script = dir.path().join("dying_mcp.sh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
echo started >> "{log}"
count=0
while IFS= read -r line; do
  rest=${{line#*\"id\":}}
  id=${{rest%%,*}}
  case "$line" in
    *'"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"tools":[{{"name":"echo","description":"e","inputSchema":{{"type":"object"}}}}]}}}}\n' "$id" ;;
    *'"tools/call"'*)
      count=$((count+1))
      if [ "$count" -ge 1 ]; then exit 0; fi ;;
  esac
done
"#,
            log = spawn_log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    McpSettings::save_global(
        dir.path(),
        &[McpServerConfig {
            id: "stub".to_string(),
            name: "Stub".to_string(),
            transport: McpTransport::Stdio {
                command: script.to_string_lossy().to_string(),
                args: Vec::new(),
                env: HashMap::new(),
            },
            enabled: true,
            scope: McpScope::Global,
        }],
    )
    .unwrap();

    let manager = Arc::new(McpManager::new(Some(dir.path().to_path_buf()), None));
    manager.discover_and_connect().await;
    let executor = McpToolExecutor::new(Arc::clone(&manager));

    // First call loses the server and must report an error rather than hanging.
    let first = executor.execute_tool("mcp__stub__echo", "{}").await;
    assert!(matches!(first, Some(Err(_))), "got {first:?}");

    // The broken session must have been retired, so the next call starts a new
    // process instead of reusing a dead pipe forever.
    let before = spawn_count(&spawn_log);
    let _ = executor.execute_tool("mcp__stub__echo", "{}").await;
    assert!(
        spawn_count(&spawn_log) > before,
        "a dead server should be restarted on the next call"
    );

    manager.shutdown().await;
}
