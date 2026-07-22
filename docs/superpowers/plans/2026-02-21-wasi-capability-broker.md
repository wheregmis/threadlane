# WASI Capability Broker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a versioned, grant-checked WASI capability broker so future extensions can use host services without extension-specific harness code.

**Architecture:** API v2 extensions import one synchronous `mypi_host.request` ABI. The import validates JSON requests and declared capability grants, writes a JSON response into caller-provided memory, and queues accepted requests on the invocation result. `CodingAgent` owns one generic asynchronous dispatcher that executes queued requests through capability adapters; individual extensions do not appear in that dispatcher. API v1 command, tool, state, and hook behavior remains unchanged.

**Tech Stack:** Rust 2021, `wasmi` 0.36, `serde`/`serde_json`, Tokio, existing `mypi-agent` and `mypi-tools` crates.

## Global Constraints

- Keep extensions compiled for `wasm32-wasip1`; do not introduce native/full-trust extensions.
- Preserve API v1 manifests and the existing plan/subagent behavior in this change.
- Do not add plan- or subagent-specific conditions to the broker or dispatcher.
- Capability declarations are necessary but not sufficient: host policy grants must be checked for every request.
- All broker responses are JSON data; malformed memory/ABI use and WASM traps are invocation errors.
- Keep request execution generic and capability/operation based.

---

## File structure

- Modify: `crates/mypi-coding-agent/src/wasi_extension.rs` — v2 manifest/request/response types, WASM store data, host import, grant validation, pending-request extraction, v1 compatibility.
- Create: `crates/mypi-coding-agent/src/extension_broker.rs` — public broker request/result types, capability policy, capability handler trait, and operation routing independent of individual extensions.
- Modify: `crates/mypi-coding-agent/src/coding_agent.rs` — generic async broker dispatcher and host adapters for agent/tools/session/fs/process/network/UI/events.
- Modify: `crates/mypi-coding-agent/src/lib.rs` — expose stable public broker API types.
- Modify: `crates/mypi-coding-agent/tests/wasi_tests.rs` — manifest, ABI, grant, error, state, and v1 regression coverage.
- Create: `extensions/broker_smoke_ext/` — minimal v2 WASI fixture used only to prove the imported ABI and broker envelope.
- Modify: `docs/extensions.md` — document API v2 manifest, ABI, grants, response/error semantics, and compatibility.

## ABI contract used by every task

Use an output buffer supplied by the extension, rather than attempting to invoke extension `alloc` re-entrantly from a host import:

```rust
#[link(wasm_import_module = "mypi_host")]
extern "C" {
    // Returns the byte length written. Negative values are ABI errors.
    fn request(request_ptr: i32, request_len: i32, response_ptr: i32, response_capacity: i32) -> i32;
}
```

The extension allocates the response buffer with its existing `alloc`. If the JSON response does not fit, the host writes no partial JSON and returns `-(required_len as i32)`. The extension retries with `-status` bytes. This replaces the originally proposed packed return only for the host import; exported extension functions keep their packed `u64` response convention.

The JSON request and success/failure response are:

```json
{"api_version":2,"capability":"tools","operation":"set_policy","arguments":{"policy":"read_only"}}
```

```json
{"ok":true,"value":null}
```

```json
{"ok":false,"error":{"code":"capability_denied","message":"Extension did not declare capability `tools`"}}
```

### Task 1: Define the broker’s versioned public contract

**Files:**
- Create: `crates/mypi-coding-agent/src/extension_broker.rs`
- Modify: `crates/mypi-coding-agent/src/lib.rs`
- Test: `crates/mypi-coding-agent/tests/wasi_tests.rs`

**Interfaces:**
- Produces `BrokerRequest { api_version: u32, capability: String, operation: String, arguments: Value }`.
- Produces `BrokerResponse { ok: bool, value: Option<Value>, error: Option<BrokerError> }`.
- Produces `BrokerError { code: String, message: String }` and `CapabilityPolicy`.
- Produces `CapabilityHandler::handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError>` for synchronous host-safe operations.

- [ ] **Step 1: Write failing serde and policy tests**

Add to `crates/mypi-coding-agent/tests/wasi_tests.rs`:

```rust
use mypi_coding_agent::{BrokerRequest, BrokerResponse, CapabilityPolicy};

#[test]
fn broker_request_round_trips_and_requires_v2() {
    let request: BrokerRequest = serde_json::from_str(
        r#"{"api_version":2,"capability":"tools","operation":"set_policy","arguments":{"policy":"read_only"}}"#,
    ).unwrap();
    assert_eq!(request.api_version, 2);
    assert_eq!(request.capability, "tools");
    assert_eq!(request.operation, "set_policy");
}

#[test]
fn capability_policy_rejects_undeclared_capabilities() {
    let policy = CapabilityPolicy::new(["agent"]);
    let response = policy.denied_response("tools");
    assert!(!response.ok);
    assert_eq!(response.error.unwrap().code, "capability_denied");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p mypi-coding-agent --test wasi_tests broker_`

Expected: compilation fails because `BrokerRequest` and `CapabilityPolicy` do not exist.

- [ ] **Step 3: Implement only the serializable contract and grant check**

Create `crates/mypi-coding-agent/src/extension_broker.rs` with these exact core definitions:

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const BROKER_API_VERSION: u32 = 2;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BrokerRequest {
    pub api_version: u32,
    pub capability: String,
    pub operation: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BrokerError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BrokerResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BrokerError>,
}

impl BrokerResponse {
    pub fn ok(value: Value) -> Self { Self { ok: true, value: Some(value), error: None } }
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { ok: false, value: None, error: Some(BrokerError { code: code.into(), message: message.into() }) }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CapabilityPolicy { granted: BTreeSet<String> }

impl CapabilityPolicy {
    pub fn new(granted: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { granted: granted.into_iter().map(Into::into).collect() }
    }
    pub fn allows(&self, capability: &str) -> bool { self.granted.contains(capability) }
    pub fn denied_response(&self, capability: &str) -> BrokerResponse {
        BrokerResponse::error("capability_denied", format!("Extension did not declare capability `{capability}`"))
    }
}
```

Export the types and `BROKER_API_VERSION` from `crates/mypi-coding-agent/src/lib.rs`.

- [ ] **Step 4: Run the focused tests**

Run: `cargo test -p mypi-coding-agent --test wasi_tests broker_`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/mypi-coding-agent/src/extension_broker.rs crates/mypi-coding-agent/src/lib.rs crates/mypi-coding-agent/tests/wasi_tests.rs
git commit -m "feat: define WASI broker contract"
```

### Task 2: Add API v2 manifests and strict declared-grant validation

**Files:**
- Modify: `crates/mypi-coding-agent/src/wasi_extension.rs`
- Test: `crates/mypi-coding-agent/tests/wasi_tests.rs`

**Interfaces:**
- Consumes `CapabilityPolicy` and `BROKER_API_VERSION` from `extension_broker`.
- Extends `WasiExtensionManifest` with `capabilities: Vec<String>` defaulting to empty.
- Produces `WasiExtension::capability_policy() -> CapabilityPolicy`.
- API v1 manifests load with no broker grants; API v2 manifests may declare grants.

- [ ] **Step 1: Write failing manifest compatibility tests**

Add tests:

```rust
#[test]
fn v1_manifest_defaults_to_no_capabilities() {
    let manifest: WasiExtensionManifest = serde_json::from_str(
        r#"{"api_version":1,"name":"old","version":"1","description":"old"}"#,
    ).unwrap();
    assert!(manifest.capabilities.is_empty());
}

#[test]
fn v2_manifest_preserves_declared_capabilities() {
    let manifest: WasiExtensionManifest = serde_json::from_str(
        r#"{"api_version":2,"name":"new","version":"1","description":"new","capabilities":["tools","agent"]}"#,
    ).unwrap();
    assert_eq!(manifest.capabilities, vec!["tools", "agent"]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mypi-coding-agent --test wasi_tests manifest_`

Expected: compilation fails because `capabilities` is not a field.

- [ ] **Step 3: Add the field and version rule**

In `WasiExtensionManifest`, add:

```rust
#[serde(default)]
pub capabilities: Vec<String>,
```

In `WasiExtension::load_from_bytes`, accept API versions `1` and `BROKER_API_VERSION`, reject every other version, and add:

```rust
pub fn capability_policy(&self) -> CapabilityPolicy {
    if self.manifest.api_version < BROKER_API_VERSION {
        CapabilityPolicy::default()
    } else {
        CapabilityPolicy::new(self.manifest.capabilities.clone())
    }
}
```

Update all in-repository struct literals for `WasiExtensionManifest` with `capabilities: vec![]`.

- [ ] **Step 4: Run focused tests and existing v1 tests**

Run: `cargo test -p mypi-coding-agent --test wasi_tests manifest_ test_extension_command_state_is_host_managed`

Expected: PASS; existing v1 plan extension test remains green.

- [ ] **Step 5: Commit**

```bash
git add crates/mypi-coding-agent/src/wasi_extension.rs crates/mypi-coding-agent/tests/wasi_tests.rs
git commit -m "feat: add WASI v2 capability declarations"
```

### Task 3: Wire the synchronous WASM broker import and queue accepted requests

**Files:**
- Modify: `crates/mypi-coding-agent/src/wasi_extension.rs`
- Create: `extensions/broker_smoke_ext/Cargo.toml`
- Create: `extensions/broker_smoke_ext/src/lib.rs`
- Test: `crates/mypi-coding-agent/tests/wasi_tests.rs`

**Interfaces:**
- Consumes `BrokerRequest`, `BrokerResponse`, and `CapabilityPolicy`.
- `WasiExtension::call_*` returns the extension response plus `Vec<BrokerRequest>` captured during the invocation.
- Produces `WasiExtensionManager::take_broker_requests()` only as an internal manager helper; public command/tool methods return a result object containing the captured requests.
- The import is `mypi_host.request(i32, i32, i32, i32) -> i32` and follows the ABI contract above.

- [ ] **Step 1: Write the failing smoke-extension test**

Add a test that builds or reads `extensions/broker_smoke_ext` for `wasm32-wasip1`, loads it, invokes `/broker-smoke`, and asserts both:

```rust
assert!(result.message.contains("broker accepted tools.set_policy"));
assert_eq!(result.broker_requests.len(), 1);
assert_eq!(result.broker_requests[0].capability, "tools");
assert_eq!(result.broker_requests[0].operation, "set_policy");
```

Also add a denied-grant variant that loads a fixture declaring only `agent`; assert the extension receives JSON `capability_denied` and `broker_requests` is empty.

- [ ] **Step 2: Run it to verify failure**

Run: `cargo test -p mypi-coding-agent --test wasi_tests broker_import_ -- --nocapture`

Expected: FAIL because the fixture/import and broker request result are not implemented.

- [ ] **Step 3: Make WASM store data invocation-local**

Replace `Store<()>` with a private `WasiStoreData` containing:

```rust
struct WasiStoreData {
    policy: CapabilityPolicy,
    requests: Vec<BrokerRequest>,
}
```

Pass `CapabilityPolicy::default()` while loading a manifest. During command/tool/hook calls, create the store with `extension.capability_policy()`.

- [ ] **Step 4: Define `mypi_host.request` in the linker**

Change `create_linker` to accept `Store<WasiStoreData>`. Define `mypi_host.request` with `Func::wrap` and `Caller<WasiStoreData>`. Its implementation must:

1. Reject negative pointers/lengths/capacity with `-1`.
2. Read exactly `request_len` bytes from the caller’s exported `memory`.
3. Deserialize `BrokerRequest`; on JSON failure create `BrokerResponse::error("invalid_request", ...)`.
4. Require `api_version == BROKER_API_VERSION`; otherwise return `invalid_request` JSON.
5. Check `caller.data().policy.allows(&request.capability)`; on failure return `denied_response`.
6. On success, append a clone to `caller.data_mut().requests` and return `BrokerResponse::ok(Value::Null)`.
7. Serialize the response and write it only if it fits `response_capacity`; otherwise return the negative byte length without writing.
8. Return the number of bytes written.

Factor memory reading/writing and response serialization into private helpers so all bounds checks have one implementation.

- [ ] **Step 5: Return queued requests without breaking v1 callers**

Introduce:

```rust
#[derive(Debug, Clone, Default)]
pub struct WasiExtensionInvocationResult {
    pub response: WasiExtensionResponse,
    pub broker_requests: Vec<BrokerRequest>,
}
```

Make private `WasiExtension::call` return it. Extend `WasiExtensionCommandResult` with `broker_requests: Vec<BrokerRequest>`. Keep existing `execute_command()` returning only the message and preserve `execute_tool()`’s string API until Task 5 adds a richer generic result.

- [ ] **Step 6: Create the smallest v2 fixture**

Create `extensions/broker_smoke_ext/Cargo.toml` as a `cdylib` using only `serde` and `serde_json`. Its manifest declares:

```rust
api_version: 2,
capabilities: vec!["tools".into()],
commands: vec![WasiCommandDefinition { name: "broker-smoke".into(), description: "Broker ABI smoke test".into() }],
```

Its command allocates a request and a 1024-byte response buffer, calls `mypi_host::request` for `tools.set_policy`, decodes the returned JSON, and returns `"broker accepted tools.set_policy"` only when `ok` is true. Include the existing `memory`, `alloc`, `extension_info`, and `execute_command` exports.

- [ ] **Step 7: Run focused tests**

Run: `cargo test -p mypi-coding-agent --test wasi_tests broker_import_`

Expected: PASS for accepted, denied, malformed-request, and too-small-output-buffer tests.

- [ ] **Step 8: Commit**

```bash
git add crates/mypi-coding-agent/src/wasi_extension.rs crates/mypi-coding-agent/tests/wasi_tests.rs extensions/broker_smoke_ext
git commit -m "feat: add WASI capability broker import"
```

### Task 4: Build generic asynchronous capability dispatch

**Files:**
- Modify: `crates/mypi-coding-agent/src/extension_broker.rs`
- Modify: `crates/mypi-coding-agent/src/coding_agent.rs`
- Test: `crates/mypi-coding-agent/tests/wasi_tests.rs`

**Interfaces:**
- Consumes broker requests returned by commands, tools, and hooks.
- Produces `BrokerDispatchResult { message: Option<String>, follow_up_prompt: Option<String> }`.
- Produces `CapabilityDispatcher::dispatch(&mut self, requests: Vec<BrokerRequest>) -> Future<Result<BrokerDispatchResult, BrokerError>>`.
- No dispatch branch may inspect an extension name, command name, plan state, or subagent task type.

- [ ] **Step 1: Write failing dispatcher tests for generic operations**

Add unit tests for a recording `CapabilityHandler` that assert:

```rust
let requests = vec![BrokerRequest {
    api_version: 2,
    capability: "tools".into(),
    operation: "set_policy".into(),
    arguments: serde_json::json!({"policy":"read_only"}),
}];
let result = dispatcher.dispatch(requests).await.unwrap();
assert_eq!(recorded, vec![("tools", "set_policy")]);
assert!(result.message.is_none());
```

Add an unknown-operation test asserting error code `unknown_operation`, not a panic.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p mypi-coding-agent --test wasi_tests broker_dispatch_`

Expected: compilation fails because `CapabilityDispatcher` does not exist.

- [ ] **Step 3: Add a capability/operation router, not an extension router**

In `extension_broker.rs`, add a dispatcher owning a `HashMap<String, Arc<dyn CapabilityHandler>>`. Its `dispatch` iterates requests in order, sends each request to the handler registered for `request.capability`, and maps an absent handler to:

```rust
BrokerError { code: "unknown_capability".into(), message: format!("Host does not implement capability `{}`", request.capability) }
```

Each handler rejects unsupported operations with `unknown_operation`. Aggregate only documented generic outputs (`message`, queued agent prompt, and UI notification); do not create a new enum variant per future extension.

- [ ] **Step 4: Add initial generic host handlers in `CodingAgent`**

Implement handlers for these operations:

| Capability | Operations | Host behavior |
| --- | --- | --- |
| `tools` | `set_policy`, `get_policy` | Validate `read_only`/`full`; mutate/read `ToolPolicy`. |
| `agent` | `request_turn`, `queue_message` | Validate non-empty prompt/content; return queued work for `handle_input` to await through `self.agent.prompt`. |
| `session` | `get_extension_state`, `set_extension_state` | Read/update the invoking extension’s persisted manager state; never expose another extension’s state. |
| `fs` | `read_text`, `write_text`, `list` | Resolve relative paths under `work_dir`; reject path escape; use UTF-8 text only. |
| `process` | `run` | Require `program: String` and `args: Vec<String>`; run with `work_dir`; return exit code/stdout/stderr. |
| `network` | `http` | Require URL/method/body; use the project’s existing HTTP client or add the smallest already-compatible client; enforce host allow policy. |
| `ui` | `notify`, `set_status` | Emit generic UI events; no direct Makepad dependency in the broker. |
| `events` | `publish` | Publish JSON `{topic, payload}` through a manager event queue for subscribed extensions. |

Use an invoking-extension identity in `BrokerRequest`’s host-side wrapper (not extension-supplied JSON) for `session` ownership and audit messages.

- [ ] **Step 5: Wire command dispatch without changing plan/subagent paths**

After `execute_command_with_effects` succeeds in `CodingAgent::handle_input`, dispatch `result.broker_requests` before the legacy `result.effects` loop. Apply generic dispatch outputs in order: notify/message, then queued model work. Leave the existing `SetToolPolicy`, `RequestModelTurn`, and `RunSubagents` branches intact for API v1 compatibility.

- [ ] **Step 6: Run focused tests**

Run: `cargo test -p mypi-coding-agent --test wasi_tests broker_dispatch_`

Expected: PASS; unknown capability/operation returns a structured error.

- [ ] **Step 7: Commit**

```bash
git add crates/mypi-coding-agent/src/extension_broker.rs crates/mypi-coding-agent/src/coding_agent.rs crates/mypi-coding-agent/tests/wasi_tests.rs
git commit -m "feat: dispatch generic extension capabilities"
```

### Task 5: Apply broker execution consistently to tools and hooks

**Files:**
- Modify: `crates/mypi-coding-agent/src/wasi_extension.rs`
- Modify: `crates/mypi-coding-agent/src/coding_agent.rs`
- Test: `crates/mypi-coding-agent/tests/wasi_tests.rs`

**Interfaces:**
- Produces `WasiExtensionManager::execute_tool_with_broker_requests(...)` and `execute_hook_with_broker_requests(...)`.
- Hook responses use structured middleware fields, not message substring matching.
- Existing v1 hooks and the existing read-only policy keep working unchanged.

- [ ] **Step 1: Write failing structured-hook tests**

Add a v2 fixture hook returning:

```json
{"message":"","state":{},"middleware":{"block":true,"reason":"Protected path"}}
```

Test that `before_tool_call` becomes `BeforeToolCallResult { block: true, reason: Some("Protected path"), .. }` without relying on the word `blocked` in `message`. Add a v1 hook returning message text and verify its old behavior is unchanged.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p mypi-coding-agent --test wasi_tests structured_hook_`

Expected: FAIL because `middleware` is not parsed or applied.

- [ ] **Step 3: Add typed middleware response fields**

Extend `WasiExtensionResponse` with an optional, serde-defaulted:

```rust
pub middleware: Option<WasiHookMiddleware>
```

where `WasiHookMiddleware` has optional `block`, `reason`, `arguments`, `result`, and `context` JSON fields. Do not apply fields that do not match the current hook type. For `before_tool_call`, consume `block`/`reason`; for `after_tool_call`, use a typed result replacement only after the underlying agent hook exposes it. Leave unsupported fields visible in the response but inert.

- [ ] **Step 4: Propagate queued broker calls from tool and hook invocation**

Make `ExtensionBeforeToolHook` and `ExtensionAfterToolHook` receive a dispatcher handle. Dispatch their broker requests before interpreting middleware. Ensure errors become a blocked tool result only for `before_tool_call`; after-tool hook failures are logged/returned as extension diagnostics and do not overwrite a completed tool result.

- [ ] **Step 5: Run hook and v1 regression tests**

Run: `cargo test -p mypi-coding-agent --test wasi_tests structured_hook_ test_extension_command_state_is_host_managed`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/mypi-coding-agent/src/wasi_extension.rs crates/mypi-coding-agent/src/coding_agent.rs crates/mypi-coding-agent/tests/wasi_tests.rs
git commit -m "feat: support broker-backed extension middleware"
```

### Task 6: Document API v2 and verify workspace compatibility

**Files:**
- Modify: `docs/extensions.md`
- Modify: `scripts/build_extensions.sh`
- Test: `crates/mypi-coding-agent/tests/wasi_tests.rs`

**Interfaces:**
- Documents the exact v2 manifest, imported ABI, request/response shapes, capabilities/operations, grants, and v1 compatibility.
- `scripts/build_extensions.sh` builds `broker_smoke_ext` with the existing extensions and deploys it as an ordinary WASI module.

- [ ] **Step 1: Write a failing documentation-derived smoke assertion**

Add a test that reads the compiled broker smoke extension and verifies its v2 manifest declares exactly `tools` and exposes the `broker-smoke` command. This is the executable counterpart to the documentation example.

- [ ] **Step 2: Run it to verify failure if the fixture is not deployed/built**

Run: `cargo test -p mypi-coding-agent --test wasi_tests broker_smoke_manifest_`

Expected: PASS after Task 3; if it fails because the WASM target is absent, report that environmental prerequisite rather than weakening the assertion.

- [ ] **Step 3: Replace v1-only documentation with versioned API documentation**

Update `docs/extensions.md` to include:

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

Document the four-argument import exactly as defined above, negative required-length retry, JSON error responses, all initial operation names, workspace path containment, host policy grants, and the rule that v1 modules retain their current effects protocol until migrated.

- [ ] **Step 4: Build all extensions and run targeted tests**

Run:

```bash
./scripts/build_extensions.sh
cargo test -p mypi-coding-agent --test wasi_tests
```

Expected: WASI fixtures compile and all `wasi_tests` pass.

- [ ] **Step 5: Run full verification**

Run:

```bash
cargo fmt --check
cargo test --workspace
```

Expected: both commands exit 0.

- [ ] **Step 6: Commit**

```bash
git add docs/extensions.md scripts/build_extensions.sh crates/mypi-coding-agent/tests/wasi_tests.rs
git commit -m "docs: document WASI capability broker"
```

## Spec coverage review

- Versioned generic broker import: Tasks 1–3.
- Declared/granted capability validation: Tasks 1–3.
- Initial capability families and generic dispatch: Task 4.
- Hook middleware and command/tool/hook coverage: Task 5.
- Persisted state and session ownership: Task 4.
- API v1 compatibility: Tasks 2, 5, and 6.
- Test coverage and workspace verification: Tasks 3–6.
- Plan/subagent migration deliberately excluded: preserved by the global constraints and legacy paths in Task 4.

## Plan self-review

- No placeholder or future-work steps remain; each capability and operation is named.
- Broker type names are consistent across all tasks.
- The import uses extension-provided output memory to avoid unsafe/re-entrant guest allocation.
- The only behavior deferred is the separately approved migration of plan/subagents, not implementation of the generic runtime.
