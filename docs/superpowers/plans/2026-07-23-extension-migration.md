# Plan and Subagent Extension Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Migrate plan mode and subagent functionality to the generic v2 WASI broker and remove their extension-specific harness paths.

**Architecture:** v2 extensions request generic `tools`, `agent`, and `session` capabilities. The host continues owning policy changes and child-agent execution through generic capability handlers, but no longer interprets plan/subagent effect variants or checks extension names. Existing user-visible commands remain `/plan`, `/todos`, and `/subagent`.

**Tech Stack:** Rust/WASI `wasm32-wasip1`, serde JSON, existing `WasiExtensionManager`, `AgentWorkScheduler`.

## Constraints

- No `plan_mode_ext`, `subagent_ext`, `WasiSubagentTask`, or legacy effect-name checks in harness runtime after migration.
- Preserve command names, messages, persisted plan state, read-only behavior, and subagent output behavior.
- Use only generic capability/operation routing; extension identity is metadata, not behavior.
- Keep API v1 compatibility in the runtime, but migrated bundled extensions are v2.
- No GUI changes except adapting existing plan display if tests require it.

### Task 1: Migrate plan extension to v2 broker calls

**Files:** `extensions/plan_mode_ext/src/lib.rs`, `crates/threadlane-coding-agent/tests/wasi_tests.rs`.

- Change manifest to API v2 and declare `tools`, `agent`, `session`.
- Replace `SetToolPolicy` with broker `tools.set_policy`.
- Replace `RequestModelTurn` with broker `agent.request_turn`.
- Use broker `session.get_extension_state`/`set_extension_state` for durable state, or retain returned state only if the host contract proves equivalent; test the broker path explicitly.
- Keep `/plan` and `/todos` output and assistant-message plan parsing.
- Add a v2 WASM integration test that verifies plan invokes broker requests and receives asynchronous results/events.
- Build/test and commit.

### Task 2: Migrate subagent extension and generic child-agent operation

**Files:** `extensions/subagent_ext/src/lib.rs`, `crates/threadlane-coding-agent/src/coding_agent.rs`, `crates/threadlane-coding-agent/src/wasi_extension.rs`, tests.

- Change manifest to API v2 and declare `agent`.
- Replace `RunSubagents` with generic `agent.run` arguments containing validated task list and `parallel`.
- Add `agent.run` as a generic host capability operation that delegates to the existing child-agent runner without checking extension identity/name.
- Preserve sequential `{previous}` substitution, parallel execution, thinking relay, and formatted results.
- Add integration coverage proving `/subagent` works through v2 broker requests.
- Build/test and commit.

### Task 3: Remove legacy bundled-extension harness paths

**Files:** `crates/threadlane-coding-agent/src/wasi_extension.rs`, `crates/threadlane-coding-agent/src/coding_agent.rs`, public exports/tests/docs/scripts as needed.

- Delete legacy `WasiExtensionEffect` variants and `WasiSubagentTask` only after migrated extensions no longer use them.
- Remove `enable_plan_mode` startup interpretation, `restored_plan_mode` extension-name lookups, and `run_subagents` special command handling.
- Keep generic tool-policy state driven by `tools.set_policy`; restored state must be handled through generic session/state behavior.
- Remove now-unused imports/helpers and update docs to state bundled extensions are ordinary v2 modules.
- Run `./scripts/build_extensions.sh`, `cargo fmt --check`, and `cargo test --workspace`.
- Commit.

## Acceptance

- `/plan`, `/todos`, and `/subagent` work with only v2 broker requests.
- Removing or renaming the bundled extension does not change generic broker dispatch code.
- No harness code branches on `plan_mode_ext` or `subagent_ext`.
- Full workspace tests pass.
