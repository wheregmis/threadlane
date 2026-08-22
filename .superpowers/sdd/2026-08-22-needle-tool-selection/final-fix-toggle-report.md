# Busy-session Needle toggle fix

## Root cause

`AppState::set_needle_enabled` persisted the preference and updated its UI state,
but ignored `false` from `SessionController::try_set_needle_enabled`.  A chat
turn holds the controller's agent mutex for its full provider/tool loop, so a
toggle during that period was dropped for that retained runtime.

## Fix

`SessionController::set_needle_enabled` now awaits the agent mutex.  AppState
queues that setter on Threadlane's process-lifetime shared Tokio runtime for
every live session runtime.  New sessions continue to initialize from the
existing persisted preference; the active turn intentionally keeps its current
configuration and the queued change applies after it releases the mutex.

No additional configuration state or session runtime is created.

## TDD evidence

- RED: `cargo test -p threadlane-session queued_needle_toggle_applies_after_busy_agent_releases_lock`
  failed with `E0599`: only `try_set_needle_enabled` existed.
- GREEN: the same focused test passed after adding the awaited setter and queue.

## Verification

- `cargo test -p threadlane-session queued_needle_toggle_applies_after_busy_agent_releases_lock` — 1 passed.
- `cargo check -p threadlane-gpui` — 0 errors (six pre-existing dead-code warnings).
- `cargo test -p threadlane-runtime needle` — 2 passed.
- `git diff --check -- crates/threadlane-session/src/controller.rs crates/threadlane-gpui/src/state/app_state.rs` — clean.

## Scope

Committed files: `crates/threadlane-session/src/controller.rs`,
`crates/threadlane-gpui/src/state/app_state.rs`, and this report.  Unrelated
unstaged formatting changes in `crates/threadlane-runtime/src/tool_executor.rs`
and `crates/threadlane-runtime/src/turn_driver.rs` were preserved.
