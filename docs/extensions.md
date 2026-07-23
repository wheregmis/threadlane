# WASI Extension APIs

`mypi` loads project-local WebAssembly extension packages from
`./.mypi/extensions/<extension-id>/extension.wasm` (and also accepts deployed
`.wasm` files in that directory). Extensions add slash commands and model tools
without linking into the agent binary. The API version in `extension_info`
selects the invocation protocol: v2 extensions use the capability broker below;
v1 extensions retain the effects protocol documented at the end of this page.

## API v2: capability broker

A v2 extension manifest has this shape:

```json
{
  "api_version": 2,
  "name": "example-extension",
  "version": "0.1.0",
  "description": "Example extension",
  "capabilities": ["tools", "agent"],
  "commands": [{"name":"example","description":"Run the example"}],
  "tools": [],
  "hooks": ["before_tool_call"]
}
```

`capabilities` is the extension's requested grant set. The host policy may grant
fewer capabilities; an extension must not assume that a declaration is a grant.
Commands, tools, and hooks are still declared in the manifest. The initial hook
names are `before_tool_call`, `after_tool_call`, and `assistant_message`.

### Invocation and broker import

The host invokes a command, tool, or hook with JSON containing the API version,
kind, contribution name, arguments, host-owned extension state, and any queued
events:

```json
{
  "api_version": 2,
  "kind": "command",
  "name": "example",
  "arguments": {"raw": ""},
  "state": {},
  "events": []
}
```

A v2 module may import the synchronous broker function exactly as follows:

```rust
#[link(wasm_import_module = "mypi_host")]
extern "C" {
    #[link_name = "request"]
    fn broker_request(
        request_ptr: i32,
        request_len: i32,
        response_ptr: i32,
        response_capacity: i32,
    ) -> i32;
}
```

The import is `mypi_host.request(i32, i32, i32, i32) -> i32`. The first two
arguments identify the request JSON in the extension's exported `memory`; the
last two identify the extension-provided response buffer and its capacity. A
request has this shape:

```json
{
  "api_version": 2,
  "capability": "tools",
  "operation": "set_policy",
  "arguments": {"policy": "read_only"}
}
```

The import returns an acknowledgement JSON response only:

```json
{"ok":true,"value":null}
```

This acknowledgement means only that the request was accepted into the host's
asynchronous queue; it is not the operation's output. The host dispatches queued
requests after the current extension invocation returns. A successful operation
result is delivered to the invoking extension on a future invocation in its
`events` array, with topic `broker_response` and this payload shape:

```json
{
  "api_version": 2,
  "capability": "agent",
  "operation": "request_turn",
  "ok": true,
  "value": {"...": "operation output"}
}
```

Operation outputs are never returned synchronously through the import or folded
into the command/tool/hook response. Extensions that need a result must retain
request context and consume the later `broker_response` event (or another host
future/event delivery) on a subsequent invocation.

or, for malformed requests, unsupported versions, and denied grants:

```json
{
  "ok": false,
  "error": {"code": "capability_denied", "message": "..."}
}
```

The return value is the number of response bytes written. Negative pointers,
lengths, or capacities return `-1`. If the serialized response is larger than
the supplied capacity, the host does not write the buffer and returns the
negative number of bytes required; the extension can retry with a larger
buffer. Invalid JSON and unsupported broker versions are JSON errors rather
than traps.

### Initial capabilities and operations

The host dispatches requests by capability and operation, not by extension
name or command name. Every operation receives JSON in `arguments` and returns
JSON in the broker response.

| Capability | Operations | Contract |
| --- | --- | --- |
| `tools` | `set_policy`, `get_policy` | Set policy to `read_only` or `full`, or read the current policy. |
| `agent` | `request_turn`, `queue_message` | `request_turn` schedules a non-empty prompt for a host-managed agent turn; `queue_message` queues a non-empty user message for the agent's follow-up queue. Both return an asynchronous acknowledgement/result event after scheduling. |
| `session` | `get_extension_state`, `set_extension_state` | Read or update only the invoking extension's persisted state. |
| `fs` | `read_text`, `write_text`, `list` | Read, write, or list UTF-8 workspace files. |
| `process` | `run` | Run a program with string arguments in the workspace directory; returns exit code, stdout, and stderr. |
| `network` | `http` | Make an HTTP request subject to the host's URL and host allow policy. |
| `ui` | `notify`, `set_status` | Emit a generic notification or status event for the host UI. |
| `events` | `subscribe`, `publish` | Subscribe the invoking extension to a topic, or publish a JSON topic and payload to subscribed extensions. |

Filesystem paths are resolved relative to the workspace and must remain within
that workspace after normalization; absolute paths and `..` escapes are
rejected. Process execution uses the workspace as its working directory.
Network access is subject to the host's configured host allow policy. The host
checks each requested capability against both the extension declaration and
host policy before import acknowledgement, and checks it again when placing the
request on the host queue. The default host policy grants declared v2
capabilities for compatibility; a restrictive host policy may grant a subset of
the declaration (for example, only `tools`). A missing effective grant returns
`capability_denied` and the request is not queued. Session state is identity-scoped and cannot be read or changed for a
different extension.

An extension response may contain a message, updated state, and broker-driven
host work. Hook responses may additionally contain typed middleware fields such
as `block`, `reason`, `arguments`, `result`, and `context`; unsupported fields
are inert for the current hook type.

## API v1 compatibility

v1 modules retain their current effects protocol until they are explicitly
migrated. They continue to receive JSON and return JSON, and the host still
validates and performs their requested effects. v1 manifests have no broker
grants (missing `capabilities` defaults to an empty list), and v1 state and
lifecycle behavior remain unchanged.

A v1 invocation receives JSON and returns JSON:

```json
{
  "api_version": 1,
  "kind": "command",
  "name": "plan",
  "arguments": { "raw": "" },
  "state": {}
}
```

```json
{
  "message": "🟢 WASI Plan Mode ENABLED",
  "state": { "enabled": true },
  "effects": [
    { "type": "set_tool_policy", "policy": "read_only" },
    { "type": "request_model_turn", "prompt": "Analyze the workspace..." }
  ]
}
```

`state` is host-managed, scoped to the active session and extension, and
persisted under the project's `.mypi/state/extensions/` directory in v1. A
fresh WASM instance receives the previously returned state on its next
invocation. `effects` are requests: the host validates and performs them,
rather than granting the extension direct model or tool-policy access. Supported
effects are `set_tool_policy` (`read_only` or `full`) and
`request_model_turn`; lifecycle hooks remain the documented v1 hooks.

### Required exports

Every extension must export:

- `memory`
- `alloc(size: i32) -> i32`
- `extension_info() -> u64`

The packed `u64` contains a pointer in its upper 32 bits and byte length in its
lower 32 bits. Command extensions additionally export
`execute_command(ptr: i32, len: i32) -> u64`; tool extensions export
`execute_tool(ptr: i32, len: i32) -> u64`.

### v1 manifest

`extension_info` returns this JSON manifest:

```json
{
  "api_version": 1,
  "name": "example-extension",
  "version": "0.1.0",
  "description": "Example extension",
  "commands": [{ "name": "example", "description": "Run the example" }],
  "tools": [],
  "hooks": ["assistant_message", "after_tool_call"]
}
```

Tool contributions are included with built-in tools in the model request. When
the model calls one, `AgentLoop` routes it to the owning extension; unknown
tools continue to use the built-in tool runtime.

### v1 lifecycle hooks

Extensions may declare `assistant_message` and/or `after_tool_call` in their
manifest. The host invokes the extension's
`handle_hook(ptr, len) -> u64` export with the same invocation/response protocol
used by commands. `assistant_message` receives `{ "content": "..." }`;
`after_tool_call` receives the tool name, arguments, result text, and error flag.
Hook responses can update extension state. Hook effects are currently ignored to
prevent background hooks from unexpectedly changing policy or triggering model
turns.

### v1 reference extension

[`extensions/plan_mode_ext`](../extensions/plan_mode_ext) is the v1 reference
implementation. It demonstrates host-managed state across fresh WASM instances
through `/plan` and `/todos`, then uses the `assistant_message` hook to parse the
model's `Plan:` block into extension-owned todos.

Build and deploy it:

```sh
cargo build --manifest-path extensions/plan_mode_ext/Cargo.toml --target wasm32-wasip1 --release
mkdir -p .mypi/extensions/plan_mode_ext
cp extensions/plan_mode_ext/target/wasm32-wasip1/release/plan_mode_ext.wasm .mypi/extensions/plan_mode_ext/extension.wasm
```

### Project runtime layout

```text
.mypi/
  extensions/
    plan_mode_ext/
      extension.wasm
      extension.json
  state/
    extensions/
      plan_mode_ext.json
  sessions/
    default.jsonl
```

The host persists returned extension state under the active session's
`.mypi/state/extensions/sessions/<session-id>/<extension>.json` path. State is
managed by the host and scoped to both the loaded extension and conversation;
`CodingAgent` also defaults its session history to `.mypi/sessions/default.jsonl`.
The legacy `./extensions/*.wasm` location remains a discovery fallback during
migration.

### v1 boundaries

Extensions cannot directly read files, run commands, call the model, modify
system prompts, or install arbitrary hooks. They can request a host model turn
or a global tool policy through the limited effect protocol, and may subscribe
to the two safe lifecycle hooks documented above. Filesystem, shell, richer UI,
session persistence, hook effects, and more lifecycle events are explicit host
capabilities in v2 rather than arbitrary access.
