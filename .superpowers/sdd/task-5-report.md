# Task 5: Broker-backed extension middleware

## Status

Implemented and committed as `feat: support broker-backed extension middleware`.

## Changes

- Added typed, serde-defaulted `WasiHookMiddleware` response fields and exported it.
- Added `execute_tool_with_broker_requests` and `execute_hook_with_broker_requests`, retaining the existing `*_with_effects` APIs as compatibility aliases.
- Before-tool hooks dispatch broker requests before interpreting structured middleware; v2 `block`/`reason` are applied without message matching while v1 `message` behavior remains unchanged.
- Before-hook invocation and broker failures block the tool; after-hook failures are logged and never replace a completed tool result.
- Added v2 and v1 WASM hook fixtures and focused regression tests. Plan/subagent paths were not changed.

## Acceptance report

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Only extension middleware/request execution APIs, tool-hook integration, focused WASI tests, the public type export, and this report were changed; existing read-only and v1 handling remain in place and plan/subagent branches are untouched."
    }
  ],
  "changedFiles": [
    "crates/mypi-coding-agent/src/coding_agent.rs",
    "crates/mypi-coding-agent/src/lib.rs",
    "crates/mypi-coding-agent/src/wasi_extension.rs",
    "crates/mypi-coding-agent/tests/wasi_tests.rs",
    ".superpowers/sdd/task-5-report.md"
  ],
  "testsAddedOrUpdated": [
    "crates/mypi-coding-agent/tests/wasi_tests.rs"
  ],
  "commandsRun": [
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests structured_hook_",
      "result": "passed",
      "summary": "2 structured v2/v1 hook tests passed."
    },
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests test_extension_command_state_is_host_managed",
      "result": "passed",
      "summary": "Existing v1 plan extension regression passed."
    },
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests",
      "result": "passed",
      "summary": "25 WASI tests passed."
    },
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests structured_hook_ test_extension_command_state_is_host_managed",
      "result": "failed",
      "summary": "Cargo accepts one positional test filter; the two filters were rerun separately and passed."
    },
    {
      "command": "git diff --check",
      "result": "passed",
      "summary": "No whitespace errors."
    }
  ],
  "validationOutput": [
    "Structured v2 middleware blocks with reason Protected path despite an empty message.",
    "v1 message containing blocked still blocks with the original message reason.",
    "Full wasi_tests regression suite passes."
  ],
  "residualRisks": [
    "After-hook diagnostics are emitted through stderr because the agent after-hook result has no diagnostics field."
  ],
  "noStagedFiles": true,
  "diffSummary": "Added typed hook middleware parsing, explicit tool/hook broker-request APIs, broker error isolation, and v1/v2 fixture coverage.",
  "reviewFindings": [
    "no blockers"
  ],
  "manualNotes": "Pre-existing GUI worktree changes remain unstaged and are unrelated."
}
```
