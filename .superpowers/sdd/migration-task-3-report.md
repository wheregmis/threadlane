# Migration Task 3 Report

## Status

Implemented and committed as `71994b7 refactor: remove legacy extension harness paths`.

## Changes

- Removed `WasiExtensionEffect::RunSubagents` and `WasiSubagentTask`; generic `agent.run` now uses an internal validated task representation.
- Removed plan-specific startup policy interpretation, `restored_plan_mode` lookups, and command-time legacy subagent special handling.
- Kept API v1 `set_tool_policy` and `request_model_turn` response effects under `WasiLegacyEffect`; v2 bundled modules use broker requests only.
- Kept generic `tools.set_policy` dispatch and session extension-state persistence behavior.
- Removed `enable_plan_mode` from options and updated its workspace call sites.
- Updated extension documentation and plan-mode guide to describe bundled modules as ordinary v2 extensions.

## Changed files

- `crates/mypi-coding-agent/src/coding_agent.rs`
- `crates/mypi-coding-agent/src/lib.rs`
- `crates/mypi-coding-agent/src/wasi_extension.rs`
- `crates/mypi-coding-agent/tests/supervisor_tests.rs`
- `crates/mypi-gui/src/app/mod.rs` (option-shape update only)
- `docs/extensions.md`
- `docs/plan_mode_extension.md`

## Validation

- `./scripts/build_extensions.sh` — passed; all bundled WASI modules built and deployed.
- `cargo test -p mypi-coding-agent --lib` — passed, 9 tests.
- `cargo test -p mypi-coding-agent --test wasi_tests` — passed, 31 tests.
- `cargo test --workspace` — passed.
- `cargo check --workspace` — passed.
- `git diff --check` — passed.
- `cargo fmt --all -- --check` — reports existing formatting differences in unrelated pre-existing files; changed code was formatted with `cargo fmt --all` before unrelated formatting was restored to keep scope narrow.

## Concerns

- Existing `static_mut_refs` warnings remain in bundled extension crates.
- Existing workspace dead-code/GUI warnings remain.
- Tool policy starts as `FullAccess` rather than inspecting a plan extension's persisted state; policy changes are exclusively driven by generic v2 `tools.set_policy` requests.

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Legacy bundled harness paths were removed without changing generic capability dispatch; only related option call sites and extension documentation were updated."
    },
    {
      "id": "criterion-2",
      "status": "satisfied",
      "evidence": "Build, workspace tests, focused coding-agent tests, WASI integration tests, and diff checks passed; the report records the one repository-wide pre-existing fmt-check failure."
    }
  ],
  "changedFiles": [
    "crates/mypi-coding-agent/src/coding_agent.rs",
    "crates/mypi-coding-agent/src/lib.rs",
    "crates/mypi-coding-agent/src/wasi_extension.rs",
    "crates/mypi-coding-agent/tests/supervisor_tests.rs",
    "crates/mypi-gui/src/app/mod.rs",
    "docs/extensions.md",
    "docs/plan_mode_extension.md"
  ],
  "testsAddedOrUpdated": [
    "crates/mypi-coding-agent/tests/supervisor_tests.rs"
  ],
  "commandsRun": [
    {
      "command": "./scripts/build_extensions.sh",
      "result": "passed",
      "summary": "Built and deployed all bundled WASI extensions."
    },
    {
      "command": "cargo test -p mypi-coding-agent --lib",
      "result": "passed",
      "summary": "9 tests passed."
    },
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests",
      "result": "passed",
      "summary": "31 WASI integration tests passed."
    },
    {
      "command": "cargo test --workspace",
      "result": "passed",
      "summary": "All workspace tests passed."
    },
    {
      "command": "cargo fmt --all -- --check",
      "result": "failed",
      "summary": "Pre-existing formatting differences remain in unrelated files; no unrelated formatting was committed."
    },
    {
      "command": "git diff --check",
      "result": "passed",
      "summary": "No whitespace errors."
    }
  ],
  "validationOutput": [
    "No runtime references remain to WasiExtensionEffect, WasiSubagentTask, enable_plan_mode, restored_plan_mode, or extension-name dispatch checks.",
    "Workspace test result: all tests passed."
  ],
  "residualRisks": [
    "Repository-wide cargo fmt --check is blocked by pre-existing unrelated formatting differences.",
    "Bundled extension static_mut_refs warnings remain."
  ],
  "noStagedFiles": true,
  "diffSummary": "Removed legacy plan/subagent effect harness paths, retained only required v1 effects, and documented bundled v2 modules.",
  "reviewFindings": [
    "no blockers found"
  ],
  "manualNotes": "Committed as 71994b7; GUI change only removes the deleted options field."
}
```
