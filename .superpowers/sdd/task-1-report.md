# Task 1 Report: WASI Broker Contract

## Implemented

Defined and exported the version-2 WASI broker contract:

- `BROKER_API_VERSION = 2`
- Serializable `BrokerRequest`, `BrokerResponse`, and `BrokerError`
- `BrokerResponse::ok` and `BrokerResponse::error` constructors
- `CapabilityPolicy` backed by `BTreeSet`, including `allows` and the required `capability_denied` response
- Regression coverage for request deserialization and undeclared-capability denial

No broker dispatch, handler trait, host integration, or policy enforcement wiring was added; those are outside Task 1.

## Test-first evidence

1. Added the two requested tests before implementation.
2. Ran `cargo test -p mypi-coding-agent --test wasi_tests broker_`; it failed to compile because the three requested public imports did not exist.
3. Implemented the serializable contract and re-exports.
4. Re-ran the required focused command successfully. The `broker_` filter selected the request test; the capability-policy test was also run with its own focused filter.

## Validation

| Command | Result |
| --- | --- |
| `cargo test -p mypi-coding-agent --test wasi_tests broker_` (before implementation) | Expected compile failure: unresolved `BrokerRequest`, `BrokerResponse`, and `CapabilityPolicy` imports |
| `cargo test -p mypi-coding-agent --test wasi_tests broker_` | Passed: 1 passed, 0 failed |
| `cargo test -p mypi-coding-agent --test wasi_tests capability_policy_` | Passed: 1 passed, 0 failed |
| `rustfmt --edition 2021 --check crates/mypi-coding-agent/src/extension_broker.rs crates/mypi-coding-agent/tests/wasi_tests.rs` | Passed |
| `git diff --check` | Passed |
| `git diff --cached --check` | Passed before commit |

The focused test commands emit existing workspace duplicate-package warnings and an existing `base_system_prompt` dead-code warning. The requested test import of `BrokerResponse` is unused in the supplied test snippet, producing an unused-import warning.

## Self-review

Reviewed the staged diff before committing. It changes only the three files named in the task brief, exposes exactly the requested public contract, defaults omitted `arguments`, omits absent response fields during serialization, and returns the exact specified denial code/message. No blockers found.

## Commit

`044f405 feat: define WASI broker contract`

## Residual risks

Task 1 intentionally only defines the contract and local grant check. It does not validate `api_version` or wire policy checks into a broker dispatcher; those are future-task work. The requested test name says "requires v2", but its supplied assertions only verify round-trip parsing of a version-2 request, consistent with the task's explicit scope.

## Review-fix evidence

- Added and publicly re-exported the synchronous `CapabilityHandler` trait with the required signature: `handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError>`.
- Strengthened `capability_policy_rejects_undeclared_capabilities` to assert `allows("agent")` and `!allows("tools")`, while retaining the denial response assertion.
- Required focused command: `cargo test -p mypi-coding-agent --test wasi_tests broker_` — passed; 1 test passed, 0 failed, 9 filtered out. The selected test was `broker_request_round_trips_and_requires_v2`.
- Policy regression command: `cargo test -p mypi-coding-agent --test wasi_tests capability_policy_` — passed; 1 test passed, 0 failed, 9 filtered out. This directly exercised both policy assertions and the `capability_denied` response.
- Formatting/checks: targeted `rustfmt --edition 2021 --check crates/mypi-coding-agent/src/extension_broker.rs crates/mypi-coding-agent/tests/wasi_tests.rs` and `git diff --check` passed. Workspace-wide `cargo fmt --all -- --check` remains failing on pre-existing formatting in unrelated files; no unrelated files were changed.
- Both Cargo test runs emitted existing duplicate-package warnings and the existing `base_system_prompt` dead-code warning; the supplied test's unused `BrokerResponse` import warning also remains unchanged.
