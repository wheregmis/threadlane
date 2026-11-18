//! The conformance suite (Plan.md §16) against a fake `claude` that answers
//! its prompts as the real one would, given how the driver started it: it
//! keeps sessions, calls the system's tool over the MCP config it was given
//! (with `curl`), writes a file only with `bypassPermissions`, and refuses a
//! command only when a `--disallowedTools` rule covers it. The same suite
//! runs against the real `claude` in `live.rs`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use threadlane_backend_claude::{ClaudeBackend, ClaudeConfig};
use threadlane_testkit::conformance::Conformance;

const FAKE: &str = r#"#!/bin/sh
IFS= read -r prompt
dir=@DIR@
session=""; flag=""; mcp=""; mode=""; denied=""; in_denied=0; prev=""
for arg in "$@"; do
  case "$prev" in
    --session-id|--resume) session="$arg"; flag="$prev" ;;
    --mcp-config) mcp="$arg" ;;
    --permission-mode) mode="$arg" ;;
  esac
  case "$arg" in
    --disallowedTools) in_denied=1 ;;
    --*) in_denied=0 ;;
    *) if [ "$in_denied" = 1 ]; then denied="$denied|$arg"; fi ;;
  esac
  prev="$arg"
done
touch "$dir/sessions"
if [ "$flag" = "--session-id" ]; then
  if grep -qxF "$session" "$dir/sessions"; then
    echo "Error: Session ID $session is already in use." >&2; exit 1
  fi
  echo "$session" >> "$dir/sessions"
elif ! grep -qxF "$session" "$dir/sessions"; then
  echo "{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"errors\":[\"No conversation found with session ID: $session\"]}"
  exit 1
fi
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$session\",\"mcp_servers\":[{\"name\":\"threadlane\",\"status\":\"connected\"}]}"
case "$prompt" in *send_message*)
  url=$(sed -n 's/.*"url":"\([^"]*\)".*/\1/p' "$mcp")
  auth=$(sed -n 's/.*"Authorization":"\([^"]*\)".*/\1/p' "$mcp")
  echo '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"mcp__threadlane__send_message","input":{"text":"hi"}}]}}'
  reply=$(curl -s -X POST "$url" -H "Authorization: $auth" -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_message","arguments":{"text":"hi"},"_meta":{"claudecode/toolUseId":"toolu_1"}}}')
  case "$reply" in *'"isError":false'*) failed=false ;; *) failed=true ;; esac
  echo "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_1\",\"content\":\"sent\",\"is_error\":$failed}]}}"
;; esac
case "$prompt" in *conformance.txt*)
  echo '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_2","name":"Bash","input":{"command":"echo ok > conformance.txt"}}]}}'
  if [ "$mode" = bypassPermissions ]; then
    echo ok > conformance.txt
    echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","content":""}]}}'
  else
    echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","content":"This command requires approval","is_error":true}]}}'
  fi
;; esac
case "$prompt" in *'`'*)
  cmd=$(printf '%s' "$prompt" | sed -n 's/^[^`]*`\([^`]*\)`.*$/\1/p')
  refused=false
  IFS='|'
  for rule in $denied; do
    prefix=${rule#Bash(}; prefix=${prefix%:\*)}
    if [ -n "$prefix" ]; then
      case "$cmd" in "$prefix"|"$prefix "*) refused=true ;; esac
    fi
  done
  unset IFS
  echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_3\",\"name\":\"Bash\",\"input\":{\"command\":\"$cmd\"}}]}}"
  if [ "$refused" = true ]; then
    echo "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_3\",\"content\":\"Permission to use Bash with command $cmd has been denied.\",\"is_error\":true}]}}"
  else
    echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_3","content":"USAGE [new branch]"}]}}'
  fi
;; esac
case "$prompt" in *"usage limit"*)
  echo '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1794892800}}'
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["usage limit"]}'
  exit 1
;; esac
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"session_id\":\"$session\",\"num_turns\":1,\"usage\":{}}"
"#;

/// The suite on a fake `claude` in its own directory, and a working directory.
fn suite() -> (Conformance, TempDir, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let program = dir.path().join("claude");
    std::fs::write(
        &program,
        FAKE.replace("@DIR@", &dir.path().display().to_string()),
    )
    .expect("writes fake claude");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("makes fake claude executable");
    let backend = ClaudeBackend::new(ClaudeConfig {
        program,
        config_dir: None,
    });
    let suite = Conformance {
        backend: Arc::new(backend),
        model: None,
        new_session_id: || uuid::Uuid::new_v4().to_string(),
        limit: Duration::from_secs(30),
    };
    (suite, dir, tempfile::tempdir().expect("temp dir"))
}

#[tokio::test]
async fn an_unattended_run_reaches_the_system_s_tools_and_resumes_by_id() {
    let (suite, _fake, work) = suite();
    suite.run_with_tools_then_resume(work.path()).await;
}

#[tokio::test]
async fn each_deny_rule_refuses_its_command() {
    let (suite, _fake, work) = suite();
    suite.each_deny_rule_refuses_its_command(work.path()).await;
}

#[tokio::test]
async fn a_usage_limit_is_reported_as_the_runner_expects() {
    let (suite, _fake, work) = suite();
    suite.usage_limit_is_reported(work.path()).await;
}
