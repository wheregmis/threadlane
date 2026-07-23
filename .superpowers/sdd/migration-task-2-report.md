# Migration Task 2 Report

## Status

Implemented and validated subagent extension API v2 migration.

## Changes

- Migrated `subagent_ext` manifest to API v2 with the `agent` capability.
- Replaced the legacy extension `RunSubagents` response with a generic WASI broker `agent.run` request containing `tasks` and `parallel`.
- Added generic host-side `agent.run` dispatch through the existing child-agent runner callback. Dispatch is capability/operation based and does not inspect extension names.
- Preserved sequential `{previous}` substitution, parallel execution, formatted result sections, child thinking relay, and child UI event relay.
- Added WASM integration coverage asserting the v2 manifest and `agent.run` broker request arguments.
- Kept legacy effect handling in place for this migration task; GUI files were not changed.

## Validation

- `cargo test -p subagent-ext` — passed (2 tests).
- `cargo test -p mypi-coding-agent` — passed (all package tests, including `/subagent`).
- `cargo test -p mypi-coding-agent --test wasi_tests subagent_v2_command_uses_generic_agent_run_broker_request` — passed.
- `./scripts/build_extensions.sh` — passed; WASI modules built and deployed.
- `cargo test --workspace` — passed.
- `git diff --check` — passed.

## Concerns

- Existing `static mut` output-buffer warnings remain in bundled extensions.
- Legacy `WasiExtensionEffect::RunSubagents` and `WasiSubagentTask` remain intentionally for compatibility; removal is deferred to Task 3.
