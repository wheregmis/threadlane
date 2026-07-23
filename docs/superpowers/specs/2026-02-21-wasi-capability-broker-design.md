# WASI Capability Broker Design

## Goal

Make WASI extensions broadly extensible without adding extension-specific logic to the Rust harness. Plan mode and subagents remain unchanged until this runtime is complete and tested.

## Boundary

Extensions remain WebAssembly modules compiled for `wasm32-wasip1`. The host exposes one generic broker import:

```text
threadlane_host.request(ptr: i32, len: i32) -> u64
```

The module sends JSON and receives packed pointer/length JSON using the existing `alloc` and memory contract.

Request:

```json
{
  "api_version": 2,
  "capability": "agent",
  "operation": "request_turn",
  "arguments": {"prompt": "..."}
}
```

Response:

```json
{"ok": true, "value": {}, "error": null}
```

Errors are returned as data; malformed requests, missing imports, memory faults, and traps remain host invocation errors.

## Manifest and grants

API v2 manifests add declared capabilities. The host grants only capabilities declared by the extension and allowed by host policy. Unknown capabilities and operations are denied. API v1 extensions continue to load through the existing compatibility path.

Initial capability families:

- `agent`: request or queue model turns and send messages.
- `tools`: set/read tool policy and active tools.
- `session`: read/append session data and extension state.
- `fs`: controlled workspace file access.
- `process`: controlled command execution.
- `network`: HTTP requests subject to host policy.
- `ui`: notifications, status, widgets, and input requests.
- `events`: publish/subscribe to generic lifecycle events.

The broker is intentionally operation-based rather than adding one host import per feature. Capability validation and operation dispatch are centralized in the manager.

## Runtime behavior

- Each invocation receives the extension's current state as today.
- Broker calls use the same extension context and state scope as the parent invocation.
- Command, tool, and hook responses can return message, state, and generic effects/results.
- Hook dispatch preserves extension registration order and supports middleware fields such as `block`, `reason`, `arguments`, `result`, and `context`.
- Host-side failures are isolated to the calling extension and surfaced as extension errors; they must not crash the agent.
- Capability requests must not silently bypass grants.

## Migration and compatibility

First implement the broker, manifest v2 types, WASI import wiring, capability dispatch, and tests. Do not alter plan/subagent harness paths in this phase. Then migrate plan and subagents to broker calls and delete their special-case handling in a separate change.

## Verification

Tests must cover:

1. v2 manifest parsing and capability declarations.
2. Broker request/response memory exchange.
3. granted operations and denied/unknown operations.
4. malformed JSON and extension traps.
5. state/session scope preservation through broker calls.
6. command, tool, and hook compatibility with existing v1 extensions.
7. no regression in `cargo test --workspace`.
