# Task 4: Generic asynchronous capability dispatch

## Status

Implemented and committed as `feat: dispatch generic extension capabilities`.

## Changes

- Added capability/operation-based `CapabilityDispatcher` with ordered async dispatch, structured unknown-capability/operation errors, generic message/follow-up aggregation, and host-only extension identity envelopes.
- Added generic host handlers for tools, agent, session, filesystem, process, network, UI, and events.
- Wired command broker requests before legacy API v1 effects without changing plan/subagent branches.
- Added dispatcher routing, unknown operation, and unknown capability tests.
- Added the narrow persisted session-state mutation and event queue APIs required by host handlers.

## TDD evidence

1. Added dispatcher tests before implementation; the expected initial compile failure was due to the missing dispatcher.
2. Implemented the generic router and host handlers.
3. Re-ran focused and full WASI integration tests successfully.

## Acceptance report

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Capability/operation routing is generic; command integration dispatches broker requests before unchanged legacy effects, with no plan/subagent dispatch branches added."
    }
  ],
  "changedFiles": [
    "crates/mypi-coding-agent/src/coding_agent.rs",
    "crates/mypi-coding-agent/src/extension_broker.rs",
    "crates/mypi-coding-agent/src/lib.rs",
    "crates/mypi-coding-agent/src/wasi_extension.rs",
    "crates/mypi-coding-agent/tests/wasi_tests.rs",
    ".superpowers/sdd/task-4-report.md"
  ],
  "testsAddedOrUpdated": [
    "crates/mypi-coding-agent/tests/wasi_tests.rs"
  ],
  "commandsRun": [
    {
      "command": "cargo check -p mypi-coding-agent",
      "result": "passed",
      "summary": "Package checked successfully; pre-existing dead-code warning remains."
    },
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests broker_dispatch_",
      "result": "passed",
      "summary": "3 dispatcher tests passed."
    },
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests",
      "result": "passed",
      "summary": "20 WASI tests passed."
    },
    {
      "command": "git diff --check",
      "result": "passed",
      "summary": "No whitespace errors."
    }
  ],
  "validationOutput": [
    "Focused dispatcher tests pass, including ordered recording and structured unknown capability/operation errors.",
    "Full wasi_tests regression suite passes."
  ],
  "residualRisks": [
    "Network handler intentionally supports host-allowlisted plain HTTP only; HTTPS requires a compatible client dependency.",
    "Existing base_system_prompt dead-code warning remains unrelated to this change."
  ],
  "noStagedFiles": true,
  "diffSummary": "Added generic capability dispatch and host handlers, host-side identity envelopes, command wiring, and focused tests.",
  "reviewFindings": [
    "no blockers"
  ],
  "manualNotes": "Task 5 tool/hook middleware was not wired; only the Task 4 command path and request identity plumbing were added."
}
```

## Review follow-up

- Events now support `events.subscribe` and `events.publish`; `{topic,payload}` values are queued per subscribed extension and delivered in `WasiExtensionInvocation.events` on the next invocation.
- `process.run` uses Tokio child processes with kill-on-drop and a bounded timeout; `network.http` uses Tokio I/O with the same timeout. Filesystem containment, identity-scoped state, and structured timeout/denial tests were added.
- GUI files were intentionally left untouched.

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Only coding-agent broker/event/timeout implementation, focused tests, and this report were changed; GUI worktree files remain unstaged and unmodified."
    }
  ],
  "changedFiles": [
    "crates/mypi-coding-agent/src/coding_agent.rs",
    "crates/mypi-coding-agent/src/extension_broker.rs",
    "crates/mypi-coding-agent/src/lib.rs",
    "crates/mypi-coding-agent/src/wasi_extension.rs",
    "crates/mypi-coding-agent/tests/wasi_tests.rs",
    ".superpowers/sdd/task-4-report.md"
  ],
  "testsAddedOrUpdated": [
    "crates/mypi-coding-agent/src/coding_agent.rs",
    "crates/mypi-coding-agent/tests/wasi_tests.rs"
  ],
  "commandsRun": [
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests",
      "result": "passed",
      "summary": "22 tests passed."
    },
    {
      "command": "cargo test -p mypi-coding-agent coding_agent::tests",
      "result": "passed",
      "summary": "Filesystem containment and process/network error tests passed."
    },
    {
      "command": "git diff --check",
      "result": "passed",
      "summary": "No whitespace errors."
    }
  ],
  "validationOutput": [
    "Topic-filtered event queue delivers events through the next-invocation drain path.",
    "Process timeout returns timeout and network policy returns host_denied without blocking Tokio."
  ],
  "residualRisks": [
    "Network client remains intentionally plain HTTP and host-allowlisted; HTTPS is not added."
  ],
  "noStagedFiles": true,
  "diffSummary": "Review follow-up adds generic event subscriptions and queued delivery, Tokio-bounded process/network operations, and focused state/filesystem/error tests.",
  "reviewFindings": [
    "no blockers"
  ],
  "manualNotes": "Pre-existing unstaged GUI files were not touched."
}
```
