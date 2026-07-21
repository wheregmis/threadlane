# WASI Extension API v1

`mypi` loads project-local WebAssembly extension packages from `./.mypi/extensions/<extension-id>/extension.wasm`. Extensions add slash commands and model tools without linking into the agent binary.

## Host-owned execution and state

The host owns model access, tool execution, and extension state. Every extension invocation receives JSON and returns JSON. An extension may be recreated for every call; durable state must be returned in the response, then is supplied by the host to its next invocation.

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

`state` is scoped to the loaded extension for the lifetime of the agent process in v1. `effects` are requests: the host validates and performs them, rather than granting the extension direct model or tool-policy access. Supported effects are `set_tool_policy` (`read_only` or `full`) and `request_model_turn`. Session-file persistence and lifecycle hooks are intentionally deferred to the next API revision.

## Required exports

Every extension must export:

- `memory`
- `alloc(size: i32) -> i32`
- `extension_info() -> u64`

The packed `u64` contains a pointer in its upper 32 bits and byte length in its lower 32 bits. Command extensions additionally export `execute_command(ptr: i32, len: i32) -> u64`; tool extensions export `execute_tool(ptr: i32, len: i32) -> u64`.

## Manifest

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

Tool contributions are included with built-in tools in the model request. When the model calls one, `AgentLoop` routes it to the owning extension; unknown tools continue to use the built-in tool runtime.

## Lifecycle hooks

Extensions may declare `assistant_message` and/or `after_tool_call` in their manifest. The host invokes the extension's `handle_hook(ptr, len) -> u64` export with the same invocation/response protocol used by commands. `assistant_message` receives `{ "content": "..." }`; `after_tool_call` receives the tool name, arguments, result text, and error flag. Hook responses can update extension state. Hook effects are currently ignored to prevent background hooks from unexpectedly changing policy or triggering model turns.

## Reference extension

[`extensions/plan_mode_ext`](../extensions/plan_mode_ext) is the v1 reference implementation. It demonstrates host-managed state across fresh WASM instances through `/plan` and `/todos`, then uses the `assistant_message` hook to parse the model's `Plan:` block into extension-owned todos.

Build and deploy it:

```sh
cargo build --manifest-path extensions/plan_mode_ext/Cargo.toml --target wasm32-wasip1 --release
mkdir -p .mypi/extensions/plan_mode_ext
cp extensions/plan_mode_ext/target/wasm32-wasip1/release/plan_mode_ext.wasm .mypi/extensions/plan_mode_ext/extension.wasm
```

## Project runtime layout

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

The extension manager persists returned extension state under `.mypi/state/extensions/`. `CodingAgent` also defaults its session history to `.mypi/sessions/default.jsonl`. The legacy `./extensions/*.wasm` location remains a discovery fallback during migration.

## v1 boundaries

Extensions cannot directly read files, run commands, call the model, modify system prompts, or install arbitrary hooks. They can request a host model turn or a global tool policy through the limited effect protocol, and may subscribe to the two safe lifecycle hooks documented above. Filesystem, shell, richer UI, session persistence, hook effects, and more lifecycle events will be added as explicit host capabilities rather than granting arbitrary access.
