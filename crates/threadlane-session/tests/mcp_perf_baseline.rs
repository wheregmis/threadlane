//! Measurement harness for MCP tool-call latency.
//!
//! Ignored by default: it spawns real subprocesses and reports timings rather
//! than asserting behavior. Run with
//! `cargo test -p threadlane-session --test mcp_perf_baseline -- --ignored --nocapture`.
//!
//! The stub server is a `/bin/sh` script, so this harness is Unix-only.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use threadlane_session::mcp::{
    McpManager, McpScope, McpServerConfig, McpSettings, McpToolExecutor, McpTransport,
};
use threadlane_session::ToolExecutor;

fn stub_server_script(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("stub_mcp.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
# Minimal MCP stdio server. Echoes the request id, as JSON-RPC requires.
# Uses parameter expansion only: forking per request would tax the per-call
# measurement and hide what is actually being compared.
while IFS= read -r line; do
  rest=${line#*\"id\":}
  id=${rest%%,*}
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"stub","version":"1"}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object","properties":{}}}]}}\n' "$id" ;;
    *'"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"ok"}]}}\n' "$id" ;;
  esac
done
"#,
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn write_config(global_dir: &std::path::Path, script: &std::path::Path) {
    let config = McpServerConfig {
        id: "stub".to_string(),
        name: "Stub".to_string(),
        transport: McpTransport::Stdio {
            command: script.to_string_lossy().to_string(),
            args: Vec::new(),
            env: HashMap::new(),
        },
        enabled: true,
        scope: McpScope::Global,
    };
    McpSettings::save_global(global_dir, &[config]).unwrap();
}

#[tokio::test]
#[ignore = "measurement harness, not an assertion"]
async fn mcp_tool_call_latency() {
    const CALLS: u32 = 20;

    let dir = tempfile::tempdir().unwrap();
    let script = stub_server_script(dir.path());
    write_config(dir.path(), &script);

    let manager = Arc::new(McpManager::new(Some(dir.path().to_path_buf()), None));

    // First exec of a freshly written script costs ~185ms on macOS (the
    // system's one-time check on a new executable). Without this warm-up that
    // cost lands inside the first discovery and reads as "MCP discovery is
    // slow", which it is not.
    let warm = Instant::now();
    let _ = tokio::process::Command::new(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .await;
    println!(
        "first-exec warm-up   : {:?} (harness cost, not MCP)",
        warm.elapsed()
    );

    let discover_start = Instant::now();
    let records = manager.discover_and_connect().await;
    let discover = discover_start.elapsed();
    assert_eq!(records.len(), 1, "stub server should be discovered");

    // A repeat refresh reconnects, so this shows the steady-state cost.
    let repeat_start = Instant::now();
    manager.discover_and_connect().await;
    let repeat = repeat_start.elapsed();

    // Exercise the same path the agent uses.
    let executor = McpToolExecutor::new(Arc::clone(&manager));
    let call_start = Instant::now();
    for _ in 0..CALLS {
        let result = executor.execute_tool("mcp__stub__echo", "{}").await;
        assert!(matches!(result, Some(Ok(_))), "stub call should succeed");
    }
    let calls = call_start.elapsed();

    println!("\n--- MCP latency ---");
    println!("discover_and_connect : {discover:?}");
    println!("discover (repeat)    : {repeat:?}");
    println!("{CALLS} tool calls      : {calls:?}");
    println!("per tool call        : {:?}", calls / CALLS);
    println!("-------------------\n");
}
