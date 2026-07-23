# Task 6: Document API v2 and verify workspace compatibility

## Status

Implemented Task 6 only. Commit is recorded as `docs: document WASI capability broker`.

## Changes

- Replaced the v1-only extension page with versioned API documentation covering the exact v2 manifest, invocation and broker request/response JSON, four-argument `mypi_host.request` ABI, negative required-length retry, structured JSON errors, all initial capability operations, grants, workspace containment, host policy, and v1 effects compatibility.
- Updated `scripts/build_extensions.sh` to deploy each built extension, including `broker_smoke_ext`, as an ordinary `.wasm` module while avoiding dependency artifacts.
- Added a deployed-fixture smoke assertion that checks the broker smoke manifest is API v2, declares exactly `tools`, and exposes `broker-smoke`.
- Plan/subagent paths and unrelated GUI worktree changes were not modified.

## Acceptance report

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Only docs/extensions.md, scripts/build_extensions.sh, the requested WASI smoke test, and this report were changed for Task 6; v1 compatibility and plan/subagent paths remain intact."
    },
    {
      "id": "criterion-2",
      "status": "satisfied",
      "evidence": "The deployed broker smoke manifest test passed, the complete wasi_tests suite passed, extension build/deployment passed, and the workspace test suite passed."
    }
  ],
  "changedFiles": [
    "docs/extensions.md",
    "scripts/build_extensions.sh",
    "crates/mypi-coding-agent/tests/wasi_tests.rs",
    ".superpowers/sdd/task-6-report.md"
  ],
  "testsAddedOrUpdated": [
    "crates/mypi-coding-agent/tests/wasi_tests.rs"
  ],
  "commandsRun": [
    {
      "command": "bash -n scripts/build_extensions.sh",
      "result": "passed",
      "summary": "Build/deploy script has valid shell syntax."
    },
    {
      "command": "./scripts/build_extensions.sh",
      "result": "passed",
      "summary": "Built broker_smoke_ext, plan_mode_ext, and subagent_ext for wasm32-wasip1 release and deployed all three modules."
    },
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests broker_smoke_manifest_",
      "result": "passed",
      "summary": "The deployed-fixture manifest assertion passed."
    },
    {
      "command": "cargo test -p mypi-coding-agent --test wasi_tests",
      "result": "passed",
      "summary": "27 WASI tests passed."
    },
    {
      "command": "cargo fmt --check",
      "result": "failed",
      "summary": "Pre-existing formatting differences in unrelated files; the changed Rust test passes standalone rustfmt checking."
    },
    {
      "command": "rustfmt --edition 2021 --check crates/mypi-coding-agent/tests/wasi_tests.rs",
      "result": "passed",
      "summary": "Changed Rust test is formatted."
    },
    {
      "command": "cargo test --workspace",
      "result": "passed",
      "summary": "All workspace tests and doc-tests passed."
    },
    {
      "command": "git diff --check",
      "result": "passed",
      "summary": "No whitespace errors."
    }
  ],
  "validationOutput": [
    "Deployed modules: broker_smoke_ext.wasm, plan_mode_ext.wasm, subagent_ext.wasm.",
    "WASI manifest smoke assertion passed with api_version 2, capabilities [tools], and command broker-smoke.",
    "Workspace tests passed; cargo fmt --check remains blocked by unrelated pre-existing formatting drift."
  ],
  "residualRisks": [
    "The repository-wide cargo fmt --check remains red because of pre-existing unrelated formatting differences.",
    "Build emits existing warnings for mutable static references and dead code."
  ],
  "noStagedFiles": true,
  "diffSummary": "Versioned v2 broker documentation, deterministic extension deployment including broker_smoke_ext, and a deployed manifest smoke assertion.",
  "reviewFindings": [
    "no blockers"
  ],
  "manualNotes": "Unrelated pre-existing GUI modifications remain unstaged and untouched."
}
```
