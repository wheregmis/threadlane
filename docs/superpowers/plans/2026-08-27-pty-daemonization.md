# PTY Daemonization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move terminal shell ownership to `threadlane-daemon` while keeping GPUI's existing `vt100` emulator and terminal interactions.

**Architecture:** The daemon owns one `portable-pty` session per terminal id and publishes ordered output/error/exit events. The shared `DaemonClient` subscribes once per connection; each GPUI `TerminalView` filters events by its daemon id, feeds output into the existing parser worker, and sends input/resize/close requests to the daemon. Sessions are ephemeral and are not reattached after reconnect.

**Tech Stack:** Rust workspace, `portable-pty`, Tokio UDS/WebSocket JSON-RPC, GPUI `Context::spawn`, `vt100`, `tokio::sync::broadcast`.

**Spec:** `docs/superpowers/specs/2026-08-27-pty-daemonization-design.md`

## Global Constraints

- Keep `TerminalView`'s existing `vt100` rendering, selection, scrollback, and parser-worker behavior.
- PTY reads stay off the UI thread; GPUI entity updates happen through foreground GPUI tasks.
- Use the existing daemon RPC/event transport; do not add a second streaming protocol.
- Keep terminal sessions ephemeral; do not add persistence or reconnect reattachment.
- Bound output buffering using the existing parser channel and daemon/client broadcast channels.
- Remove `portable-pty` from GPUI once no GPUI source uses it; retain it in the daemon.
- Preserve existing terminal key encoding and current project-scoped terminal groups.

## File Map

- Modify `crates/threadlane-protocol/src/terminal.rs`: add optional terminal error data without breaking existing event payloads.
- Modify `crates/threadlane-protocol/src/client.rs`: add typed resize/close/subscription helpers and idempotent subscription state.
- Modify `crates/threadlane-daemon/src/services/terminal_service.rs`: harden PTY ownership, non-blocking lock usage, duplicate-id handling, and reader error events.
- Modify `crates/threadlane-daemon/src/connection.rs`: forward terminal subscriptions for both UDS and WebSocket connections and cleanly stop forwarding on disconnect.
- Modify `crates/threadlane-gpui/src/screens/terminal/mod.rs`: replace local PTY state/spawn/read/write/resize with daemon session state while retaining the parser and renderer.
- Modify `crates/threadlane-gpui/src/screens/workspace/view.rs`: explicitly close daemon terminal ids when tabs are closed or replaced.
- Modify `crates/threadlane-gpui/Cargo.toml`: remove the now-unused `portable-pty` dependency.
- Modify `crates/threadlane-daemon/tests/client_daemon_integration_test.rs`: cover subscription, output, input round-trip, resize, event filtering, and close over UDS.
- Modify `crates/threadlane-gpui/src/screens/terminal/mod.rs` tests: cover terminal-id filtering and daemon event state transitions with pure helpers.

### Task 1: Extend the terminal protocol and client subscription

**Files:**
- Modify: `crates/threadlane-protocol/src/terminal.rs`
- Modify: `crates/threadlane-protocol/src/client.rs`
- Test: `crates/threadlane-protocol/src/lib.rs`

**Interfaces:**
- Produces `DaemonClient::subscribe_terminal() -> Future<Output = Result<(), String>>`.
- Produces `DaemonClient::resize_terminal(ResizeTerminalRequest) -> Future<Output = Result<(), String>>`.
- Produces `DaemonClient::close_terminal(CloseTerminalRequest) -> Future<Output = Result<TerminalClosedResponse, String>>`.
- Extends `TerminalOutputEvent` with `error: Option<String>` using `#[serde(default, skip_serializing_if = "Option::is_none")]`.

- [ ] **Step 1: Write the failing serialization test.**

Add a `TerminalOutputEvent` case with `error: Some("reader failed".into())`, round-trip it through JSON, and assert the error survives. Also deserialize the legacy payload without `error` and assert it becomes `None`.

- [ ] **Step 2: Run the protocol test to verify it fails.**

Run: `cargo test -p threadlane-protocol test_terminal_event_serialization`

Expected: compile failure because `TerminalOutputEvent` has no `error` field.

- [ ] **Step 3: Add the optional event field and typed client methods.**

Add the serde-defaulted field, add `terminal_subscribed: AtomicBool` to `DaemonClient`, and implement:

```rust
pub async fn subscribe_terminal(&self) -> Result<(), String> {
    if self.terminal_subscribed.load(Ordering::Acquire)
        || self.terminal_subscribed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return Ok(());
    }
    match self.request::<_, bool>("terminal/subscribe", Value::Null).await {
        Ok(_) => Ok(()),
        Err(error) => {
            self.terminal_subscribed.store(false, Ordering::Release);
            Err(error)
        }
    }
}
```

The resize and close helpers should call the existing RPC methods with their existing request/response types.

- [ ] **Step 4: Run the protocol tests.**

Run: `cargo test -p threadlane-protocol`

Expected: all protocol tests pass.

### Task 2: Harden daemon PTY ownership and notification forwarding

**Files:**
- Modify: `crates/threadlane-daemon/src/services/terminal_service.rs`
- Modify: `crates/threadlane-daemon/src/connection.rs`
- Test: `crates/threadlane-daemon/tests/client_daemon_integration_test.rs`

**Interfaces:**
- Consumes the protocol types from Task 1.
- Keeps `TerminalService::{spawn_terminal, write_input, resize_terminal, close_terminal}` as the dispatcher-facing API.
- Emits `TerminalOutputEvent { terminal_id, data, exit_code, error }` through the existing broadcast channel.

- [ ] **Step 1: Add the failing UDS round-trip test.**

Start a daemon and client against a temporary project, call `subscribe_terminal`, spawn two terminals with explicit ids, send `printf 'pty-a\n'` and `printf 'pty-b\n'` to the corresponding ids, and wait with a timeout for each matching event. Assert that each id only delivers its own marker. Then call typed resize and close for both ids and assert input to a closed id returns an error.

Use a helper shaped like:

```rust
async fn wait_for_marker(
    events: &mut broadcast::Receiver<TerminalOutputEvent>,
    terminal_id: &str,
    marker: &str,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.expect("terminal event");
            if event.terminal_id == terminal_id && event.data.contains(marker) {
                break;
            }
        }
    })
    .await
    .expect("terminal marker");
}
```

- [ ] **Step 2: Run the integration test to verify it fails.**

Run: `cargo test -p threadlane-daemon --test client_daemon_integration_test terminal`

Expected: compile failure because typed resize/close/subscription helpers are missing, or timeout because GPUI-independent subscription forwarding is incomplete.

- [ ] **Step 3: Implement daemon-side ownership and forwarding.**

Keep PTY output reads on the existing reader thread. Before spawning, reject a duplicate terminal id. Store the writer behind its own mutex, clone it before performing blocking writes, and never hold the terminal-map mutex while writing. On reader errors publish an event with `error: Some(...)`; on EOF publish the exit event. Remove terminal entries before dropping the child on close.

Add the same `terminal/subscribe` branch to the WebSocket loop that already exists in the UDS loop. The forwarding task exits when its connection output channel closes; no per-terminal global stream is added.

- [ ] **Step 4: Run the daemon integration tests.**

Run: `cargo test -p threadlane-daemon --test client_daemon_integration_test`

Expected: all existing and new tests pass.

### Task 3: Replace GPUI-local PTY state with daemon terminal sessions

**Files:**
- Modify: `crates/threadlane-gpui/src/screens/terminal/mod.rs`
- Modify: `crates/threadlane-gpui/Cargo.toml`
- Test: `crates/threadlane-gpui/src/screens/terminal/mod.rs`

**Interfaces:**
- Consumes `DaemonClient` typed terminal methods and `TerminalOutputEvent` from Tasks 1–2.
- Keeps `TerminalView::{new, send_input, restart, clear, resize, select_all, paste_from_clipboard}` public behavior unchanged.
- Adds a pure `terminal_event_matches(current_id: Option<&str>, event_id: &str) -> bool` helper for filtering tests.

- [ ] **Step 1: Write the failing event-filter test.**

Add tests asserting that an event is accepted only when its terminal id equals the current id, and that `None` (before spawn or after close) rejects all output events.

- [ ] **Step 2: Run the GPUI terminal tests to verify the new test fails.**

Run: `cargo test -p threadlane-gpui terminal_event_matches`

Expected: compile failure because the helper does not exist.

- [ ] **Step 3: Implement the daemon-backed adapter.**

Remove `PtySession`, `spawn_shell`, `native_pty_system`, `CommandBuilder`, and GPUI `portable-pty` imports. Keep `start_parser_worker`; store its bounded output sender in `TerminalView`.

Add daemon state:

```rust
daemon_client: Option<Arc<DaemonClient>>,
terminal_id: Option<String>,
terminal_generation: u64,
parser_output_tx: Option<mpsc::SyncSender<Vec<u8>>>,
daemon_event_task: Option<Task<()>>,
```

In `new`, start the parser worker, start one stored GPUI async event task that calls `get_daemon_client`, `subscribe_terminal`, and filters `TerminalOutputEvent` by `terminal_event_matches` before sending bytes to the parser. On exit/error, update the status through the existing `PtyEvent` path and clear the active id.

Make `start` spawn a daemon terminal asynchronously with the current project and dimensions. Capture a generation number; apply the response only if that generation is still current, otherwise best-effort close the stale daemon id. `send` and `send_input` dispatch `write_terminal_input` on the shared runtime using the current id. `resize` updates the parser immediately and dispatches `resize_terminal` without blocking the UI. `restart` increments the generation, best-effort closes the old id, resets the emulator, and starts again. Add a `close` method for tab removal and a best-effort `Drop` close for app/entity teardown.

Use `error` and `exit_code` events to set the existing status banner and stop accepting input. Keep all selection, scrollback, rendering, and key encoding code unchanged.

- [ ] **Step 4: Update workspace tab lifecycle and remove the dependency.**

Call `TerminalView::close` before removing/replacing tabs in `close_terminal_tab` and `close_other_terminal_tabs`. Remove `portable-pty = "0.9"` from `crates/threadlane-gpui/Cargo.toml` after `rg` confirms no GPUI source references remain.

- [ ] **Step 5: Run GPUI terminal tests and compile checks.**

Run: `cargo test -p threadlane-gpui screens::terminal`

Expected: parser, wake/coalescing, filtering, and terminal rendering tests pass.

Run: `cargo check -p threadlane-gpui`

Expected: exit 0 with only the repository's existing unused-code warnings.

### Task 4: Full verification and completion audit

**Files:**
- No new production files.
- Review: all files from Tasks 1–3.

- [ ] **Step 1: Run focused daemon and protocol suites.**

Run: `cargo test -p threadlane-protocol && cargo test -p threadlane-daemon`

Expected: zero failures, including the two-terminal event filtering test.

- [ ] **Step 2: Run the full GPUI suite.**

Run: `cargo test -p threadlane-gpui`

Expected: zero failures.

- [ ] **Step 3: Run formatting and whitespace checks.**

Run: `cargo fmt --all -- --check`

Expected: no formatting changes in the PTY files; if unrelated pre-existing formatting diffs remain, verify the changed PTY files individually with `rustfmt --check`.

Run: `git -c core.fsmonitor=false diff --check`

Expected: no whitespace errors.

- [ ] **Step 4: Audit the final architecture.**

Run: `rg -n "portable_pty|native_pty_system|CommandBuilder|spawn_shell|PtySession" crates/threadlane-gpui crates/threadlane-daemon crates/threadlane-protocol`

Expected: PTY ownership symbols appear only in `threadlane-daemon`; GPUI retains only `vt100` parser/rendering code and daemon RPC calls.

- [ ] **Step 5: Confirm requirement-by-requirement evidence.**

Verify that daemon spawn/input/resize/close mutate host PTYs, output/error/exit events cross the client subscription, two terminal ids remain isolated, GPUI tabs no longer spawn local shells, and all focused/full tests pass before marking the goal complete.
