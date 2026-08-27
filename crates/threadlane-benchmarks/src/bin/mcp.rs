//! Steady-state MCP discovery and tool-call benchmark.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use threadlane_mcp::{
    McpManager, McpScope, McpServerConfig, McpSettings, McpToolExecutor, McpTransport,
};
use threadlane_runtime::ToolExecutor;

const SAMPLES: usize = 10;

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
    McpSettings::save_global(
        global_dir,
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
}

#[hotpath::measure]
async fn discover_repeat(manager: &McpManager) {
    std::hint::black_box(manager.discover_and_connect().await);
}

#[hotpath::measure]
async fn tool_calls(executor: &McpToolExecutor) {
    for _ in 0..20 {
        std::hint::black_box(
            executor
                .execute_tool("mcp__stub__echo", "{}")
                .await
                .unwrap()
                .unwrap(),
        );
    }
}

#[hotpath::main]
fn main() {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async_main());
}

async fn async_main() {
    let dir = tempfile::tempdir().unwrap();
    let script = stub_server_script(dir.path());
    write_config(dir.path(), &script);

    let manager = Arc::new(McpManager::new(Some(dir.path().to_path_buf()), None));

    let _ = tokio::process::Command::new(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .await;

    manager.discover_and_connect().await;
    for _ in 0..SAMPLES {
        discover_repeat(&manager).await;
    }

    let executor = McpToolExecutor::new(Arc::clone(&manager));
    for _ in 0..SAMPLES {
        tool_calls(&executor).await;
    }
}
