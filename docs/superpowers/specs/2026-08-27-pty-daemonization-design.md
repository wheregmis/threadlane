# PTY Daemonization Design

## Goal

Move shell process ownership from `threadlane-gpui` to `threadlane-daemon` so
terminal tabs execute in the daemon host environment while retaining the
existing GPUI terminal emulator, scrollback, selection, and keyboard behavior.

PTY sessions are ephemeral. A tab creates one daemon terminal session; closing
or restarting the tab closes that session. A daemon disconnect stops the tab's
attachment and the tab reports the loss rather than attempting to recreate a
shell silently; sessions are not reattached after reconnect.

## Architecture

`TerminalService` remains the single owner of `portable-pty` pairs. It stores
the PTY writer/master/child by terminal id, reads each master on a worker
thread, and publishes ordered `TerminalOutputEvent` values through its existing
broadcast channel. EOF is represented by the existing event with an exit code;
read failures publish an explicit terminal error event.

The line-delimited RPC connection exposes one `terminal/subscribe` stream per
client connection. The subscription forwards daemon terminal events as JSON-RPC
notifications. `DaemonClient` makes this subscription idempotent, so multiple
GPUI terminal tabs share one notification pump without duplicate forwarding.

## Protocol and client API

Keep the existing spawn/input/resize/close request and event shapes. Add typed
client helpers for resize and close, plus an explicit `subscribe_terminal`
helper that can be safely called by every tab. The client continues to expose a
broadcast receiver for terminal events; events are filtered by terminal id at
the GPUI tab boundary.

The event payload remains UTF-8 text for compatibility with the current
protocol. PTY output is decoded lossily at the daemon boundary, matching the
existing daemon behavior. Backpressure is bounded by the client and daemon
channels; a disconnected subscriber is dropped without affecting the PTY
reader.

## GPUI terminal adapter

`TerminalView` retains only terminal presentation state and a daemon terminal
id. The local `PtySession`, local shell spawn, and local PTY reader are removed.
On construction it asynchronously obtains the shared daemon client, ensures
the terminal subscription, spawns a terminal with the current project and
dimensions, and sends matching output events to the existing parser worker.

Keyboard input and programmatic commands call the daemon input RPC. Resize
calls update the local parser immediately and send the daemon resize RPC. Clear,
selection, scrollback, and rendering remain local because they are emulator
state, not host process state. Restart closes the old terminal before spawning
a new one; close/drop sends a best-effort close for the current terminal id.

Daemon errors are surfaced through the existing status banner. A terminal event
with an exit code stops input and reports that the shell exited. Stale spawn or
close responses are ignored when a restart has already advanced the tab's
generation. Tab close and restart send best-effort close requests; daemon
shutdown reaps any remaining PTYs.

## Data flow

```text
GPUI TerminalView
  ├─ terminal/spawn ───────────────▶ daemon TerminalService ─▶ portable-pty
  ├─ terminal/input / resize / close ▶ daemon PTY handles
  └─ shared terminal/subscribe ◀──── JSON-RPC terminal/event notifications
                                      │
                                      └─ terminal-id filter
                                          └─ existing vt100 parser worker
```

## Testing

1. Protocol tests cover serialization of resize/close and terminal events.
2. Daemon service tests use a temporary project and a real shell command to
   prove spawn, output delivery, input round-trip, resize, and close behavior.
3. Client/daemon UDS integration covers subscription notifications and ensures
   events for one terminal do not get applied to another terminal id.
4. GPUI tests retain the parser, wake/coalescing, and input encoding coverage;
   new adapter tests cover event filtering and daemon failure state transitions.

## Non-goals

- Persisting or reattaching terminal sessions after daemon/client disconnect.
- Moving `vt100` parsing or selection/rendering into the daemon.
- Adding a second streaming transport or a new terminal protocol.
