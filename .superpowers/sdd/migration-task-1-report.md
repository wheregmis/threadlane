# Migration Task 1 Report

## Status

Implemented and committed the `plan_mode_ext` API v2 migration.

## Changes

- Changed `plan_mode_ext` manifest to API v2 with `tools`, `agent`, and `session` capabilities.
- Replaced legacy `SetToolPolicy` and `RequestModelTurn` effects with generic broker requests (`tools.set_policy` and `agent.request_turn`).
- Added generic session broker reads/writes (`session.get_extension_state` and `session.set_extension_state`) while retaining the host-provided returned state for immediate invocation compatibility.
- Preserved `/plan`, `/todos`, plan state transitions, and assistant-message plan parsing.
- Added a WASM integration test covering broker request routing and asynchronous `broker_response` delivery.
- Reworked existing plan persistence tests to build the extension from the repository instead of relying on an absolute local path.

## Validation

- `cargo build --manifest-path extensions/plan_mode_ext/Cargo.toml --target wasm32-wasip1 --target-dir target/plan-mode-integration` — passed.
- `cargo test -p mypi-coding-agent --test wasi_tests plan_v2_requests_generic_brokers_and_receives_async_events -- --nocapture` — passed.
- `cargo test -p mypi-coding-agent --test wasi_tests -- --nocapture` — passed (30 tests).
- `./scripts/build_extensions.sh` — passed.
- `git diff --check` — passed.

Known warnings are pre-existing workspace warnings plus the extension's existing `static_mut_refs` warning; no warning caused a failure.

## Scope and concerns

Only `extensions/plan_mode_ext/src/lib.rs`, `crates/mypi-coding-agent/tests/wasi_tests.rs`, and this report were changed. Legacy harness effect definitions and handling remain untouched for compatibility. GUI code was not changed. Session broker responses are delivered asynchronously; the existing host-injected invocation state remains the immediate source of truth, with broker state writes queued for host dispatch.
